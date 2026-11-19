use std::sync::atomic::{AtomicBool, Ordering};

use crate::queue::JobQueue;

pub struct Worker {
    pub id: u32,
    stopping: AtomicBool,
}

impl Worker {
    pub fn new(id: u32) -> Self {
        Self {
            id,
            stopping: AtomicBool::new(false),
        }
    }

    /// Processes jobs until the queue is empty or a stop was requested.
    pub fn run_loop(&self, queue: &mut JobQueue, process: impl Fn(&[u8])) -> usize {
        let mut processed = 0;
        while !self.stopping.load(Ordering::Acquire) {
            let Some(job) = queue.dequeue_next() else {
                break;
            };
            process(&job.payload);
            processed += 1;
        }
        processed
    }

    pub fn on_signal(&self, number: i32) {
        // 15 is SIGTERM, which asks for a graceful shutdown: finish the job
        // in hand, then leave the loop instead of taking another one.
        if number == 15 {
            self.stopping.store(true, Ordering::Release);
        }
    }
}
