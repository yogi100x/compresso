//! Latency gate decision shared by the large-payload benchmark.
//!
//! A shared CI runner can only make a measurement slower, never faster, so
//! one slow measurement says little while one fast measurement proves the
//! code can meet the limit. A real regression is slow on every attempt.

use std::time::Duration;

/// Outcome of a latency gate with every measurement it took.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LatencyGate {
    /// Measurements in the order taken. The last one is within the limit
    /// exactly when the gate passed.
    pub attempts: Vec<Duration>,
    pub passed: bool,
}

/// Measures until one result is below `limit`, at most `max_attempts` times.
///
/// The limit itself is not relaxed: the gate passes only if a measurement is
/// strictly below it. Every attempt is kept so a drift toward the limit stays
/// visible in the output even while the gate passes.
pub fn measure_within_limit(
    limit: Duration,
    max_attempts: usize,
    mut measure: impl FnMut() -> Duration,
) -> LatencyGate {
    let mut attempts = Vec::with_capacity(max_attempts);
    for _ in 0..max_attempts {
        let measured = measure();
        attempts.push(measured);
        if measured < limit {
            return LatencyGate {
                attempts,
                passed: true,
            };
        }
    }
    LatencyGate {
        attempts,
        passed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_millis(200);

    fn gate(measurements: &[u64]) -> LatencyGate {
        let mut remaining = measurements.iter().copied();
        measure_within_limit(LIMIT, 3, || {
            Duration::from_millis(remaining.next().expect("no measurement left to take"))
        })
    }

    fn millis(values: &[u64]) -> Vec<Duration> {
        values.iter().copied().map(Duration::from_millis).collect()
    }

    #[test]
    fn test_latency_gate_passes_on_the_first_measurement_within_the_limit() {
        let outcome = gate(&[120, 999, 999]);

        assert!(outcome.passed);
        assert_eq!(outcome.attempts, millis(&[120]), "no further measurement");
    }

    #[test]
    fn test_latency_gate_retries_a_noisy_measurement_and_keeps_every_attempt() {
        let outcome = gate(&[208, 230, 96]);

        assert!(outcome.passed);
        assert_eq!(outcome.attempts, millis(&[208, 230, 96]));
    }

    #[test]
    fn test_latency_gate_fails_when_every_attempt_exceeds_the_limit() {
        let outcome = gate(&[260, 245, 251]);

        assert!(!outcome.passed);
        assert_eq!(outcome.attempts, millis(&[260, 245, 251]));
    }

    #[test]
    fn test_latency_gate_does_not_relax_the_limit_itself() {
        // Exactly at the limit is not below it, on any attempt.
        let outcome = gate(&[200, 200, 200]);

        assert!(!outcome.passed);
    }
}
