use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Backoff {
    attempt: u32,
    cap: Duration,
    base: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Backoff {
    pub fn new() -> Self {
        Self { attempt: 0, cap: Duration::from_secs(60), base: Duration::from_secs(1) }
    }

    pub fn with_limits(base: Duration, cap: Duration) -> Self {
        Self { attempt: 0, cap, base }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Duration {
        let shift = self.attempt.min(31);
        self.attempt = self.attempt.saturating_add(1);
        let seconds = self.base.as_secs().saturating_mul(1u64 << shift);
        Duration::from_secs(seconds).min(self.cap)
    }

    pub fn with_jitter(&mut self, rng_factor: f64) -> Duration {
        let delay = self.next();
        let factor = rng_factor.clamp(0.0, 1.0);
        let scaled = 0.5 + factor * 0.5;
        delay.mul_f64(scaled)
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}
