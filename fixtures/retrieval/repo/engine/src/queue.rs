use std::collections::VecDeque;

use crate::payload::ensure_fits;

/// One unit of work waiting for a worker.
pub struct Job {
    pub id: u64,
    pub kind: String,
    pub payload: Vec<u8>,
    pub attempts: u32,
}

/// In-memory first-in-first-out queue of pending jobs.
pub struct JobQueue {
    pending: VecDeque<Job>,
    next_id: u64,
    max_depth: usize,
}

impl JobQueue {
    pub fn new(max_depth: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            next_id: 1,
            max_depth,
        }
    }

    /// Adds a job to the back of the queue and returns its id.
    pub fn enqueue(&mut self, kind: &str, payload: Vec<u8>) -> Result<u64, String> {
        ensure_fits(&payload).map_err(|error| error.to_string())?;
        if self.pending.len() >= self.max_depth {
            return Err("queue is full".to_owned());
        }
        let id = self.next_id;
        self.next_id += 1;
        self.pending.push_back(Job {
            id,
            kind: kind.to_owned(),
            payload,
            attempts: 0,
        });
        Ok(id)
    }

    /// Removes and returns the oldest pending job.
    pub fn dequeue_next(&mut self) -> Option<Job> {
        self.pending.pop_front()
    }

    pub fn depth(&self) -> usize {
        self.pending.len()
    }
}
