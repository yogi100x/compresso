use std::fmt;

/// Failures the engine reports to an API caller.
#[derive(Debug)]
pub enum EngineError {
    QueueFull,
    UnknownJob(u64),
    Throttled,
    ConfigFileUnreadable(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull => write!(formatter, "queue is full"),
            Self::UnknownJob(id) => write!(formatter, "job {id} does not exist"),
            Self::Throttled => write!(formatter, "too many requests"),
            Self::ConfigFileUnreadable(path) => write!(formatter, "cannot read {path}"),
        }
    }
}

/// Status code the API server sends for an error.
pub fn error_status(error: &EngineError) -> u16 {
    match error {
        EngineError::QueueFull => 503,
        EngineError::UnknownJob(_) => 404,
        EngineError::Throttled => 429,
        EngineError::ConfigFileUnreadable(_) => 500,
    }
}
