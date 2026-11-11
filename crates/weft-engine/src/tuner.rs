//! Chooses how many connections one network should use.
//!
//! More connections only help when something limits each connection separately
//! (a per-connection cap on the server, or loss on a long path). The tuner
//! therefore doubles the count only while doing so buys at least 10% more speed,
//! steps back when it does not, and lowers a ceiling when the server refuses
//! connections or the disk cannot keep up.

use std::time::{Duration, Instant};

const SETTLE: Duration = Duration::from_millis(1000);
const RETRY_GROWTH: Duration = Duration::from_secs(30);
const CEILING_RECOVERY: Duration = Duration::from_secs(60);
const GAIN: f64 = 1.10;

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
    changed_at: Instant,
    ceiling_at: Instant,
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
            changed_at: now,
            ceiling_at: now,
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
        self.gains >= 2
    }

    pub fn disk_limited(&self, now: Instant) -> bool {
        self.disk_limited_at.is_some_and(|t| now.duration_since(t) < Duration::from_secs(20))
    }

    /// Feed the network's current speed. `active` is how many connections are open
    /// and `starved` whether fewer are open only because there is no more work.
    pub fn tick(&mut self, now: Instant, rate: f64, active: usize, starved: bool) {
        if self.ceiling < self.max && now.duration_since(self.ceiling_at) >= CEILING_RECOVERY {
            self.ceiling += 1;
            self.ceiling_at = now;
        }
        if now.duration_since(self.changed_at) < SETTLE || starved || active < self.target() {
            return;
        }
        match self.phase {
            Phase::Grow => {
                if self.base_rate == 0.0 || rate >= self.base_rate * GAIN {
                    if self.base_rate > 0.0 {
                        self.gains += 1;
                    }
                    self.base_rate = rate;
                    self.prev_target = self.target;
                    self.target = (self.target * 2).min(self.max).min(self.ceiling.max(1));
                    if self.target == self.prev_target {
                        self.phase = Phase::Hold;
                    }
                } else {
                    // The extra connections bought nothing: give them back.
                    self.target = self.prev_target;
                    self.phase = Phase::Hold;
                }
                self.changed_at = now;
            }
            Phase::Hold => {
                if now.duration_since(self.changed_at) >= RETRY_GROWTH
                    && self.target < self.max.min(self.ceiling)
                {
                    self.phase = Phase::Grow;
                    self.base_rate = rate;
                    self.prev_target = self.target;
                    self.target = (self.target * 2).min(self.max).min(self.ceiling);
                    self.changed_at = now;
                }
            }
        }
    }

    /// The server refused a connection or a request while `active` were open.
    pub fn refused(&mut self, now: Instant, active: usize) {
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
        *now += Duration::from_secs(2);
        let active = t.target();
        t.tick(*now, rate, active, false);
    }

    #[test]
    fn grows_while_it_pays_then_steps_back() {
        let mut now = Instant::now();
        let mut t = Tuner::new(4, 32, now);
        step(&mut t, &mut now, 4.0); // baseline -> 8
        assert_eq!(t.target(), 8);
        step(&mut t, &mut now, 8.0); // gain -> 16
        assert_eq!(t.target(), 16);
        step(&mut t, &mut now, 8.2); // no gain -> back to 8
        assert_eq!(t.target(), 8);
        step(&mut t, &mut now, 8.2);
        assert_eq!(t.target(), 8);
        assert!(!t.per_connection_capped());
    }

    #[test]
    fn capped_server_reaches_max() {
        let mut now = Instant::now();
        let mut t = Tuner::new(4, 32, now);
        for rate in [4.0, 8.0, 16.0, 32.0, 32.0] {
            step(&mut t, &mut now, rate);
        }
        assert_eq!(t.target(), 32);
        assert!(t.per_connection_capped());
    }

    #[test]
    fn refusal_lowers_ceiling_and_recovers_slowly() {
        let mut now = Instant::now();
        let mut t = Tuner::new(8, 32, now);
        t.refused(now, 6);
        assert_eq!(t.target(), 5);
        assert!(t.refusing());
        now += Duration::from_secs(61);
        t.tick(now, 1.0, 5, false);
        assert_eq!(t.ceiling, 6);
    }

    #[test]
    fn does_not_grow_when_starved() {
        let mut now = Instant::now();
        let mut t = Tuner::new(4, 32, now);
        now += Duration::from_secs(5);
        t.tick(now, 10.0, 2, true);
        assert_eq!(t.target(), 4);
    }
}
