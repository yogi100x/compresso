//! Resumable, checkpointed benchmark campaigns over external task suites.
//!
//! A campaign drives every task of a suite through a small state machine,
//! persists each transition to a durable checkpoint, and aggregates the
//! finished tasks into the standard evaluation report shape. The suite itself
//! is reached only through [`SuiteAdapter`], so this crate stays free of
//! network and process capabilities.

mod adapter;
mod checkpoint;
pub mod cli;
mod fixture_adapter;
mod report;
mod runner;
mod state;

pub use adapter::{
    AdapterError, AdapterKind, SuiteAdapter, SuiteDescriptor, TaskContext, TaskMeasurement,
    TaskSpec, resolve_adapter,
};
pub use checkpoint::{CHECKPOINT_SCHEMA_VERSION, Checkpoint, CheckpointError, TaskRecord};
pub use fixture_adapter::{FixtureSuiteAdapter, SUITE_MANIFEST_FILE, SUITE_SCHEMA_VERSION};
pub use report::{CampaignReport, CampaignSummary, CampaignTaskSummary};
pub use runner::{CampaignError, CampaignOutcome, CampaignPolicy, CampaignRunner, StartMode};
pub use state::{AttemptFailure, AttemptFailureKind, TaskState, TransitionError};
