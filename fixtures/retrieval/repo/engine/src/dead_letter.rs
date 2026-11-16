use crate::queue::Job;
use crate::retry::RetryPolicy;

/// Jobs that exhausted their attempts and are parked for an operator.
#[derive(Default)]
pub struct DeadLetterQueue {
    parked: Vec<(Job, String)>,
}

impl DeadLetterQueue {
    pub fn len(&self) -> usize {
        self.parked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parked.is_empty()
    }
}

/// Parks a job once the policy refuses another attempt; otherwise hands it back.
pub fn move_to_dead_letter(
    dead: &mut DeadLetterQueue,
    policy: &RetryPolicy,
    job: Job,
    reason: &str,
) -> Option<Job> {
    if policy.should_retry(job.attempts) {
        return Some(job);
    }
    dead.parked.push((job, reason.to_owned()));
    None
}
