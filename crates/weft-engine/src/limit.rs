//! Speed limiting and speed measurement.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A shared byte-rate limit. Callers wait in [`Limiter::take`] until their bytes fit.
#[derive(Debug)]
pub struct Limiter {
    /// Bytes per second; 0 means unlimited.
    rate: AtomicU64,
    next: Mutex<Instant>,
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Limiter {
    pub fn new(rate: u64) -> Self {
        Self { rate: AtomicU64::new(rate), next: Mutex::new(Instant::now()) }
    }

    pub fn set_rate(&self, rate: u64) {
        self.rate.store(rate, Ordering::Relaxed);
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Waits until `bytes` may pass.
    pub async fn take(&self, bytes: usize) {
        let rate = self.rate();
        if rate == 0 || bytes == 0 {
            return;
        }
        let cost = Duration::from_secs_f64(bytes as f64 / rate as f64);
        let until = {
            let mut next = self.next.lock().unwrap();
            let now = Instant::now();
            // Unused time older than 50 ms is forgotten, so an idle period cannot
            // be spent later as a burst far above the limit.
            let floor = now.checked_sub(Duration::from_millis(50)).unwrap_or(now);
            let start = (*next).max(floor);
            *next = start + cost;
            *next
        };
        if until > Instant::now() {
            tokio::time::sleep_until(until.into()).await;
        }
    }
}

/// Exponentially weighted speed estimate, fed with byte counts as they arrive.
#[derive(Debug, Clone)]
pub struct RateMeter {
    window: Duration,
    bucket_start: Instant,
    bucket_bytes: u64,
    rate: f64,
    primed: bool,
}

impl RateMeter {
    /// `window` is the sampling bucket; the estimate smooths over a few buckets.
    pub fn new(window: Duration, now: Instant) -> Self {
        Self { window, bucket_start: now, bucket_bytes: 0, rate: 0.0, primed: false }
    }

    pub fn add(&mut self, bytes: u64, now: Instant) {
        self.roll(now);
        self.bucket_bytes += bytes;
    }

    fn roll(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.bucket_start);
        if elapsed < self.window {
            return;
        }
        let sample = self.bucket_bytes as f64 / elapsed.as_secs_f64();
        self.rate = if self.primed { self.rate * 0.6 + sample * 0.4 } else { sample };
        self.primed = true;
        self.bucket_start = now;
        self.bucket_bytes = 0;
    }

    /// Bytes per second. Before the first full bucket this is the raw average so far.
    pub fn rate(&mut self, now: Instant) -> f64 {
        self.roll(now);
        if self.primed {
            return self.rate;
        }
        let elapsed = now.saturating_duration_since(self.bucket_start).as_secs_f64();
        if elapsed < 0.05 { 0.0 } else { self.bucket_bytes as f64 / elapsed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limiter_paces() {
        let l = Limiter::new(200_000);
        let t = Instant::now();
        for _ in 0..10 {
            l.take(10_000).await;
        }
        let secs = t.elapsed().as_secs_f64();
        assert!((0.35..0.9).contains(&secs), "{secs}");
    }

    #[test]
    fn meter_converges() {
        let t0 = Instant::now();
        let mut m = RateMeter::new(Duration::from_millis(500), t0);
        for i in 1..=40 {
            m.add(50_000, t0 + Duration::from_millis(i * 100));
        }
        let r = m.rate(t0 + Duration::from_millis(4000));
        assert!((450_000.0..550_000.0).contains(&r), "{r}");
    }
}
