use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use compresso_core::Mode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::fixture_adapter::FixtureSuiteAdapter;
use super::runner::CampaignError;

/// Identity of a suite. A checkpoint is bound to this value and is refused by
/// a campaign whose adapter reports a different one.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteDescriptor {
    pub id: String,
    pub version: String,
    /// Compression mode the suite was exercised with; reported per task.
    pub mode: Mode,
}

/// One schedulable unit of a suite.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskSpec {
    pub id: String,
}

/// The verdict and token accounting of one completed task.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskMeasurement {
    /// Whether the suite's own grader accepted the task result.
    pub resolved: bool,
    /// Tokens the task consumed without Compresso.
    pub baseline_tokens: usize,
    /// Tokens the task consumed with Compresso.
    pub compressed_tokens: usize,
}

/// Per-attempt execution context handed to an adapter.
#[derive(Clone, Debug)]
pub struct TaskContext {
    /// One-based attempt number, stable across process restarts.
    pub attempt: u32,
    /// The runner abandons the attempt after this long.
    pub timeout: Duration,
    cancel: Arc<AtomicBool>,
}

impl TaskContext {
    pub(crate) fn new(attempt: u32, timeout: Duration, cancel: Arc<AtomicBool>) -> Self {
        Self {
            attempt,
            timeout,
            cancel,
        }
    }

    /// True once the runner has given up on this attempt. Threads cannot be
    /// killed, so an adapter must poll this and release whatever it holds.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// Infrastructure failures. A task that ran and was graded as unresolved is
/// not an error; it is a [`TaskMeasurement`] with `resolved == false`.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AdapterError {
    #[error("suite could not be loaded: {message}")]
    Suite { message: String },
    #[error("task {task} could not be executed: {message}")]
    Task { task: String, message: String },
    #[error("task {task} observed cancellation")]
    Cancelled { task: String },
}

/// The seam between the campaign runner and a task suite.
///
/// Implementations must be deterministic about task identity: `tasks` returns
/// the same identifiers on every call for a given suite version.
pub trait SuiteAdapter: Send + Sync + 'static {
    fn descriptor(&self) -> SuiteDescriptor;

    fn tasks(&self) -> Result<Vec<TaskSpec>, AdapterError>;

    /// Runs one attempt. Called from a dedicated thread; may block up to
    /// `context.timeout` and must return promptly once
    /// `context.is_cancelled()` is true.
    fn run_task(
        &self,
        task: &TaskSpec,
        context: &TaskContext,
    ) -> Result<TaskMeasurement, AdapterError>;
}

/// Adapters selectable by name. External kinds need network or container
/// access and are never constructed by this crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdapterKind {
    /// Local, scripted suite manifest. No network, no processes.
    Fixture,
    SweBenchVerified,
    TerminalBench,
}

impl AdapterKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Fixture => "fixture",
            Self::SweBenchVerified => "swe-bench-verified",
            Self::TerminalBench => "terminal-bench",
        }
    }

    /// External adapters reach outside the local machine boundary.
    pub const fn is_external(self) -> bool {
        !matches!(self, Self::Fixture)
    }
}

/// Builds the adapter for `kind`.
///
/// External kinds fail with [`CampaignError::ExternalAdapterNotAllowed`]
/// unless `allow_external` is set, and with
/// [`CampaignError::ExternalAdapterUnavailable`] when it is, because this
/// crate's capability class forbids the network and process access they need.
pub fn resolve_adapter(
    kind: AdapterKind,
    suite: &Path,
    allow_external: bool,
) -> Result<Arc<dyn SuiteAdapter>, CampaignError> {
    match kind {
        AdapterKind::Fixture => Ok(Arc::new(
            FixtureSuiteAdapter::load(suite).map_err(CampaignError::Adapter)?,
        )),
        external if !allow_external => Err(CampaignError::ExternalAdapterNotAllowed {
            adapter: external.name(),
        }),
        external => Err(CampaignError::ExternalAdapterUnavailable {
            adapter: external.name(),
        }),
    }
}
