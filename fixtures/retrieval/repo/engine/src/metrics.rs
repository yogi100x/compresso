/// Counters exposed on the status endpoint.
#[derive(Default)]
pub struct QueueMetrics {
    pub enqueued: u64,
    pub completed: u64,
    pub failed: u64,
    pub retried: u64,
}

impl QueueMetrics {
    pub fn record_enqueue(&mut self) {
        self.enqueued += 1;
    }

    pub fn record_completion(&mut self, succeeded: bool) {
        if succeeded {
            self.completed += 1;
        } else {
            self.failed += 1;
        }
    }

    pub fn record_retry(&mut self) {
        self.retried += 1;
    }

    /// Share of finished jobs that failed, between zero and one.
    pub fn failure_rate(&self) -> f64 {
        let finished = self.completed + self.failed;
        if finished == 0 {
            return 0.0;
        }
        self.failed as f64 / finished as f64
    }
}
