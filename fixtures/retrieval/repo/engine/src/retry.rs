use std::time::Duration;

const BASE_DELAY_MS: u64 = 200;
const MAX_DELAY_MS: u64 = 60_000;

/// How often a failed job may be attempted again.
pub struct RetryPolicy {
    pub max_attempts: u32,
}

impl RetryPolicy {
    pub fn should_retry(&self, attempts: u32) -> bool {
        attempts < self.max_attempts
    }
}

/// Delay before the next attempt: doubles per failure and is capped at one minute.
pub fn retry_backoff(attempt: u32) -> Duration {
    let exponent = attempt.min(16);
    let delay = BASE_DELAY_MS.saturating_mul(1_u64 << exponent);
    Duration::from_millis(delay.min(MAX_DELAY_MS))
}
