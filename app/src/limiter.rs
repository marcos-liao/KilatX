//! Token-bucket speed limiter shared by all connections it applies to.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Limiter {
    /// Bytes per second; 0 means unlimited.
    rate: AtomicU64,
    /// (available tokens, last refill). Tokens may go negative: that debt is
    /// what the caller sleeps off.
    bucket: Mutex<(f64, Instant)>,
}

impl Limiter {
    pub fn new(rate: u64) -> Self {
        Self { rate: AtomicU64::new(rate), bucket: Mutex::new((0.0, Instant::now())) }
    }

    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    pub fn set_rate(&self, rate: u64) {
        self.rate.store(rate, Ordering::Relaxed);
        *self.bucket.lock().unwrap() = (0.0, Instant::now());
    }

    /// Waits until `n` bytes may pass. Returns early when `cancel` is set or the
    /// limit is switched off, so pausing never hangs behind a slow limit.
    pub async fn acquire(&self, n: usize, cancel: &AtomicBool) {
        let rate = self.rate();
        if rate == 0 {
            return;
        }
        let wait = {
            let mut b = self.bucket.lock().unwrap();
            let now = Instant::now();
            let refill = now.duration_since(b.1).as_secs_f64() * rate as f64;
            b.1 = now;
            // Allow a burst of at most a quarter second.
            b.0 = (b.0 + refill).min(rate as f64 / 4.0) - n as f64;
            if b.0 < 0.0 { -b.0 / rate as f64 } else { 0.0 }
        };
        let deadline = Instant::now() + Duration::from_secs_f64(wait);
        while !cancel.load(Ordering::SeqCst) && self.rate() != 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            tokio::time::sleep(left.min(Duration::from_millis(100))).await;
        }
    }
}
