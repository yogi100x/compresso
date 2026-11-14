//! Index-free, read-only recall over local coding-session transcripts.
//!
//! Recall is disabled until a caller names at least one source. Every search
//! reads transcript files directly from disk, keeps nothing between calls,
//! redacts secrets before matching, and bounds files, bytes, results, and time.

mod parse;
mod redact;
mod search;
mod sources;

pub use redact::{Redaction, redact};
pub use search::{
    RecallError, RecallHit, RecallLimits, RecallReport, RecallRequest, RecallScope, SourceReport,
    SourceStatus, recall,
};
pub use sources::{RecallSource, SourceSupport};
