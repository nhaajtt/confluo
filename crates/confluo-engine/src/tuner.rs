//! Chooses how many connections one network should use.
//!
//! More connections only help when something limits each connection separately
//! (a per-connection cap on the server, or loss on a long path). The tuner
//! therefore doubles the count only while doing so buys at least 10% more speed,
//! steps back when it does not, and lowers a ceiling when the server refuses
//! connections or the disk cannot keep up.

use std::time::{Duration, Instant};

/// How long new connections get to show what they add before being judged.
const SETTLE: Duration = Duration::from_millis(1000);
/// How often the speed is looked at while waiting for the first steady reading,
/// and how soon after a step up a clear gain may be accepted.
const BASELINE_STEP: Duration = Duration::from_millis(250);
/// A doubling that brings at least this much more speed means each connection is
/// capped separately; the count then goes up four-fold instead of two-fold.
const STRONG_GAIN: f64 = 1.6;
const RETRY_GROWTH: Duration = Duration::from_secs(30);
/// After a refusal the ceiling is tested again this soon: a refusal says little
/// about where the limit is (several connections are usually refused at once).
/// Every test that is refused again doubles the wait.
const RECOVERY_FIRST: Duration = Duration::from_secs(1);
const RECOVERY_MAX: Duration = Duration::from_secs(60);
/// A server that counts requests is tested far less often: each test is a request.
const RECOVERY_RATE_LIMITED: Duration = Duration::from_secs(15);
const GAIN: f64 = 1.10;
const RISING: f64 = 1.03;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Grow,
    Hold,
}

#[derive(Debug, Clone)]
pub struct Tuner {
    max: usize,
    target: usize,
    prev_target: usize,
    ceiling: usize,
    phase: Phase,
    base_rate: f64,
    /// Speed at the previous evaluation, to tell a plateau from a climb.
    last_rate: f64,
    last_sample: f64,
    /// Evaluations spent waiting for new connections to get up to speed.
    waits: u32,
    /// A doubling of connections has (nearly) doubled the speed at least once.
    strong_gain: bool,
    changed_at: Instant,
    ceiling_at: Instant,
    /// Wait before the ceiling is tested again.
    recovery: Duration,
    /// The ceiling was raised by one as a test and nothing has been refused since.
    testing: bool,
    /// Growth steps that paid off.
    gains: u32,
    disk_limited_at: Option<Instant>,
}

impl Tuner {
    pub fn new(initial: usize, max: usize, now: Instant) -> Self {
        let max = max.max(1);
        let target = initial.clamp(1, max);
        Self {
            max,
            target,
            prev_target: target,
            ceiling: max,
            phase: Phase::Grow,
            base_rate: 0.0,
            last_rate: 0.0,
            last_sample: 0.0,
            waits: 0,
            strong_gain: false,
            changed_at: now,
            ceiling_at: now,
            recovery: RECOVERY_FIRST,
            testing: false,
            gains: 0,
            disk_limited_at: None,
        }
    }

    /// Connections this network should have open.
    pub fn target(&self) -> usize {
        self.target.min(self.ceiling).max(1)
    }

    pub fn refusing(&self) -> bool {
        self.ceiling < self.max
    }

    /// True when extra connections kept paying off: something caps each connection.
    pub fn per_connection_capped(&self) -> bool {
        self.gains >= 2 || self.strong_gain
    }

    pub fn disk_limited(&self, now: Instant) -> bool {
        self.disk_limited_at.is_some_and(|t| now.duration_since(t) < Duration::from_secs(20))
    }

    /// Feed the network's current speed (`rate` smoothed, `sample` the latest raw
    /// reading). `active` is how many connections are open and `starved` whether
    /// fewer are open only because there is no more work.
    pub fn tick(&mut self, now: Instant, rate: f64, sample: f64, active: usize, starved: bool) {
        if self.ceiling < self.max && now.duration_since(self.ceiling_at) >= self.recovery {
            if self.testing {
                // The last extra connection was accepted: the limit is higher than thought.
                self.recovery = RECOVERY_FIRST;
            }
            // Try one more connection; if the server still refuses, the ceiling drops again.
            if self.target == self.ceiling {
                self.target += 1;
            }
            self.ceiling += 1;
            self.ceiling_at = now;
            self.testing = true;
        }
        if starved || active < self.target() {
            return;
        }
        let since = now.duration_since(self.changed_at);
        if self.base_rate == 0.0 {
            // No baseline yet. While speed still climbs (TCP ramp-up on a long path)
            // any number would be too low and every later step would look like a gain.
            // A baseline of nothing would do the same.
            if since < BASELINE_STEP || rate <= 0.0 {
                return;
            }
            // Smoothing would make a speed that is already level look like a climb
            // for a few more readings, so the first baseline compares raw ones.
            let last_sample = std::mem::replace(&mut self.last_sample, sample);
            if last_sample <= 0.0 || sample > last_sample * GAIN {
                self.last_rate = rate;
                return;
            }
            self.last_rate = rate;
            self.base_rate = rate.max(sample);
            self.grow(now, 2);
            return;
        } else if since < SETTLE {
            // A jump this large is not noise and needs no further waiting.
            if self.phase == Phase::Grow && since >= BASELINE_STEP && rate >= self.base_rate * STRONG_GAIN {
                self.gains += 1;
                self.strong_gain = true;
                self.base_rate = rate;
                self.last_rate = rate;
                self.grow(now, 4);
            }
            return;
        }
        let rising = rate > self.last_rate * RISING;
        self.last_rate = rate;
        match self.phase {
            Phase::Grow if rate >= self.base_rate * GAIN => {
                self.gains += 1;
                let strong = rate >= self.base_rate * STRONG_GAIN;
                self.strong_gain |= strong;
                self.base_rate = rate;
                self.grow(now, if strong { 4 } else { 2 });
            }
            Phase::Grow if rising && self.waits < 3 => {
                // The new connections are still getting up to speed: look again.
                self.waits += 1;
                self.changed_at = now;
            }
            Phase::Grow => {
                // The extra connections bought nothing: give them back.
                self.target = self.prev_target;
                self.phase = Phase::Hold;
                self.changed_at = now;
            }
            Phase::Hold => {
                if now.duration_since(self.changed_at) >= RETRY_GROWTH
                    && self.target < self.max.min(self.ceiling)
                {
                    self.phase = Phase::Grow;
                    self.base_rate = rate;
                    self.grow(now, 2);
                }
            }
        }
    }

    fn grow(&mut self, now: Instant, factor: usize) {
        self.prev_target = self.target;
        self.target = (self.target * factor).min(self.max).min(self.ceiling.max(1));
        self.waits = 0;
        self.changed_at = now;
        if self.target == self.prev_target {
            self.phase = Phase::Hold;
        }
    }

    /// The server refused a connection or a request while `active` were open.
    /// `rate_limited` says the refusal was about the number of requests.
    pub fn refused(&mut self, now: Instant, active: usize, rate_limited: bool) {
        let first = if rate_limited { RECOVERY_RATE_LIMITED } else { RECOVERY_FIRST };
        self.recovery = if self.testing { (self.recovery * 2).min(RECOVERY_MAX) } else { first }.max(first);
        self.testing = false;
        self.ceiling = active.saturating_sub(1).clamp(1, self.max);
        self.target = self.target.min(self.ceiling);
        self.prev_target = self.prev_target.min(self.ceiling);
        self.phase = Phase::Hold;
        self.changed_at = now;
        self.ceiling_at = now;
    }

    /// The disk queue stayed full: fewer parallel writers make it more sequential.
    pub fn disk_slow(&mut self, now: Instant) {
        self.target = (self.target / 2).max(1);
        self.prev_target = self.target;
        self.phase = Phase::Hold;
        self.changed_at = now;
        self.disk_limited_at = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(t: &mut Tuner, now: &mut Instant, rate: f64) {
        *now += Duration::from_secs(1);
        let active = t.target();
        t.tick(*now, rate, rate, active, false);
    }

    #[test]
    fn waits_for_the_ramp_before_taking_a_baseline() {
        let mut now = Instant::now();
        let mut t = Tuner::new(4, 32, now);
        for rate in [1.0, 2.0, 4.0, 8.0] {
            step(&mut t, &mut now, rate); // still climbing on its own
            assert_eq!(t.target(), 4);
        }
        step(&mut t, &mut now, 8.1); // levelled off: baseline, try more
        assert_eq!(t.target(), 8);
        step(&mut t, &mut now, 8.2); // nothing gained
        assert_eq!(t.target(), 4);
        assert!(!t.per_connection_capped());
    }

    #[test]
    fn grows_while_it_pays_then_steps_back() {
        let mut now = Instant::now();
        let mut t = Tuner::new(4, 32, now);
        step(&mut t, &mut now, 4.0);
        step(&mut t, &mut now, 4.0); // baseline -> 8
        assert_eq!(t.target(), 8);
        step(&mut t, &mut now, 5.5); // a modest gain -> 16
        assert_eq!(t.target(), 16);
        step(&mut t, &mut now, 5.6); // no gain, not rising -> back to 8
        assert_eq!(t.target(), 8);
        step(&mut t, &mut now, 5.6);
        assert_eq!(t.target(), 8);
    }

    #[test]
    fn slow_starters_get_a_second_look() {
        let mut now = Instant::now();
        let mut t = Tuner::new(4, 32, now);
        step(&mut t, &mut now, 4.0);
        step(&mut t, &mut now, 4.0); // -> 8
        step(&mut t, &mut now, 4.2); // not yet a gain, but rising: wait
        assert_eq!(t.target(), 8);
        step(&mut t, &mut now, 6.0); // now it is a gain -> 16
        assert_eq!(t.target(), 16);
        step(&mut t, &mut now, 12.0); // doubling doubled the speed: capped per connection -> straight to 32
        assert_eq!(t.target(), 32);
    }

    #[test]
    fn capped_server_reaches_max() {
        let mut now = Instant::now();
        let mut t = Tuner::new(4, 32, now);
        for rate in [4.0, 4.0, 8.0, 16.0, 32.0, 32.0] {
            step(&mut t, &mut now, rate);
        }
        assert_eq!(t.target(), 32);
        assert!(t.per_connection_capped());
    }

    #[test]
    fn refusal_lowers_ceiling_and_tests_it_again() {
        let mut now = Instant::now();
        let mut t = Tuner::new(8, 32, now);
        t.refused(now, 6, false);
        assert_eq!(t.target(), 5);
        assert!(t.refusing());
        now += Duration::from_millis(1100);
        t.tick(now, 1.0, 1.0, 5, false);
        assert_eq!(t.target(), 6);
    }

    #[test]
    fn repeated_refusals_are_tested_less_and_less_often() {
        let mut now = Instant::now();
        let mut t = Tuner::new(8, 32, now);
        t.refused(now, 5, false); // four work
        let mut waits = Vec::new();
        for _ in 0..8 {
            let from = now;
            while t.target() == 4 {
                now += Duration::from_millis(250);
                t.tick(now, 1.0, 1.0, 4, false);
            }
            waits.push(now.duration_since(from).as_secs());
            t.refused(now, 5, false); // the fifth is refused every time
            assert_eq!(t.target(), 4);
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
    }

    #[test]
    fn an_accepted_test_keeps_looking_for_the_limit() {
        let mut now = Instant::now();
        let mut t = Tuner::new(8, 32, now);
        t.refused(now, 4, false); // undershoot: three left, the server takes five
        for want in [4, 5] {
            now += Duration::from_millis(1000);
            t.tick(now, 1.0, 1.0, want - 1, false);
            assert_eq!(t.target(), want);
        }
    }

    #[test]
    fn request_limits_are_tested_rarely() {
        let mut now = Instant::now();
        let mut t = Tuner::new(8, 32, now);
        t.refused(now, 4, true);
        now += Duration::from_secs(14);
        t.tick(now, 1.0, 1.0, 3, false);
        assert_eq!(t.target(), 3);
        now += Duration::from_secs(2);
        t.tick(now, 1.0, 1.0, 3, false);
        assert_eq!(t.target(), 4);
    }

    #[test]
    fn a_clear_gain_is_accepted_early() {
        let mut now = Instant::now();
        let mut t = Tuner::new(8, 32, now);
        let mut at = |t: &mut Tuner, rate: f64| {
            now += Duration::from_millis(250);
            let active = t.target();
            t.tick(now, rate, rate, active, false);
        };
        at(&mut t, 8.0);
        at(&mut t, 8.0); // level: baseline, try 16
        assert_eq!(t.target(), 16);
        at(&mut t, 13.6); // a quarter of a second later the speed is already 1.7 times
        assert_eq!(t.target(), 32);
        assert!(t.per_connection_capped());
    }

    #[test]
    fn no_baseline_before_data_arrives() {
        let mut now = Instant::now();
        let mut t = Tuner::new(8, 32, now);
        for _ in 0..8 {
            now += Duration::from_millis(250);
            t.tick(now, 0.0, 0.0, 8, false);
        }
        assert_eq!(t.target(), 8);
    }

    #[test]
    fn does_not_grow_when_starved() {
        let mut now = Instant::now();
        let mut t = Tuner::new(4, 32, now);
        now += Duration::from_secs(5);
        t.tick(now, 10.0, 10.0, 2, true);
        assert_eq!(t.target(), 4);
    }
}
