use crate::errors::{EngineError, error_status};
use crate::queue::JobQueue;
use crate::rate_limit::TokenBucket;

/// Routes API requests to the queue.
pub struct ApiServer {
    pub queue: JobQueue,
    pub bucket: TokenBucket,
}

pub struct Response {
    pub status: u16,
    pub body: String,
}

impl ApiServer {
    pub fn handle_enqueue(&mut self, kind: &str, body: Vec<u8>, now: u64) -> Response {
        if !self.bucket.try_acquire(now) {
            return error_response(&EngineError::Throttled);
        }
        match self.queue.enqueue(kind, body) {
            Ok(id) => Response {
                status: 202,
                body: format!("{{\"id\":{id}}}"),
            },
            Err(message) => Response {
                status: 400,
                body: message,
            },
        }
    }

    pub fn handle_status(&self) -> Response {
        Response {
            status: 200,
            body: format!("{{\"depth\":{}}}", self.queue.depth()),
        }
    }
}

fn error_response(error: &EngineError) -> Response {
    Response {
        status: error_status(error),
        body: error.to_string(),
    }
}
