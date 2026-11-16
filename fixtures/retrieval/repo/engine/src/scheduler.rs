/// A job template that fires on a fixed interval.
pub struct RecurringJob {
    pub kind: String,
    pub interval_seconds: u64,
    pub last_run_at: Option<u64>,
}

pub struct Scheduler {
    pub recurring: Vec<RecurringJob>,
}

impl Scheduler {
    /// Kinds that are due at `now`, in declaration order.
    pub fn due(&self, now: u64) -> Vec<&str> {
        self.recurring
            .iter()
            .filter(|job| next_run_at(job, now) <= now)
            .map(|job| job.kind.as_str())
            .collect()
    }
}

/// The first instant at or after the previous run plus one interval.
pub fn next_run_at(job: &RecurringJob, now: u64) -> u64 {
    match job.last_run_at {
        Some(previous) => previous.saturating_add(job.interval_seconds),
        None => now,
    }
}
