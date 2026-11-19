use std::time::{Duration, Instant};

/// A worker's exclusive claim on one job for a bounded time.
pub struct JobLease {
    pub job_id: u64,
    pub worker_id: u32,
    pub expires_at: Instant,
}

impl JobLease {
    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
}

/// Extends a live lease; an expired lease cannot be renewed.
pub fn renew_lease(lease: &mut JobLease, now: Instant, extension: Duration) -> bool {
    if lease.is_expired(now) {
        return false;
    }
    lease.expires_at = now + extension;
    true
}

/// Returns the job ids whose lease ran out so they can be handed to another worker.
pub fn expire_stale_leases(leases: &mut Vec<JobLease>, now: Instant) -> Vec<u64> {
    let mut released = Vec::new();
    leases.retain(|lease| {
        if lease.is_expired(now) {
            released.push(lease.job_id);
            false
        } else {
            true
        }
    });
    released
}
