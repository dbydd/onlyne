use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Backoff {
    pub attempt: u32,
    pub cap: Duration,
    pub base: Duration,
    /// A per-instance salt, so two clients that redial after the same server
    /// restart do not land on the same second. Without it every role in a
    /// cluster came back together and the server met the whole cluster at
    /// once — the failure the jitter was added to stop.
    salt: u64,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Backoff {
    /// Create a backoff with a one second base and a sixty second cap.
    pub fn new() -> Self {
        Self {
            attempt: 0,
            cap: Duration::from_secs(60),
            base: Duration::from_secs(1),
            salt: Self::entropy(),
        }
    }

    pub fn with_limits(base: Duration, cap: Duration) -> Self {
        Self {
            attempt: 0,
            cap,
            base,
            salt: Self::entropy(),
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Duration {
        let shift = self.attempt.min(31);
        self.attempt = self.attempt.saturating_add(1);
        let seconds = self.base.as_secs().saturating_mul(1u64 << shift);
        Duration::from_secs(seconds).min(self.cap)
    }

    /// The next delay, spread over half to full of the ladder's value.
    ///
    /// This is the call a redial loop should make. `with_jitter` takes the
    /// factor from its caller, which is how it ended up with no caller at all:
    /// a decision the loop has to remember to make is a decision it eventually
    /// does not make.
    pub fn next_jittered(&mut self) -> Duration {
        let attempt = self.attempt;
        let factor = self.jitter_factor(attempt);
        self.with_jitter(factor)
    }

    /// Scale the next delay into half to full using a supplied factor.
    pub fn with_jitter(&mut self, rng_factor: f64) -> Duration {
        let delay = self.next();
        let factor = rng_factor.clamp(0.0, 1.0);
        let scaled = 0.5 + factor * 0.5;
        delay.mul_f64(scaled)
    }

    /// A factor in `[0, 1)`, decorrelated per instance and per call.
    ///
    /// A splitmix64 over three inputs: the instance salt, the attempt, and a
    /// per-process counter that advances on every call. The counter is what
    /// makes this correct rather than merely plausible — a clock read alone
    /// barely moves between two calls in a tight redial loop, and the factors
    /// came out identical for six rungs running, which is the exact failure
    /// the jitter exists to prevent. The salt is what decorrelates two
    /// processes, which a counter cannot.
    fn jitter_factor(&self, attempt: u32) -> f64 {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let tick = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut z = self.salt
            ^ (attempt as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ tick.wrapping_mul(0xD1B5_4A32_D192_ED03);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A cheap per-call entropy source: the clock, mixed.
    fn entropy() -> u64 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos() as u64)
            .unwrap_or(0);
        nanos.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ (nanos >> 31)
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }
}
