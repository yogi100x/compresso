use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use super::adapter::{AdapterError, SuiteAdapter, TaskContext, TaskMeasurement, TaskSpec};
use super::checkpoint::{Checkpoint, CheckpointError};
use super::state::{AttemptFailure, AttemptFailureKind, TaskState, TransitionError};

/// Execution limits. They are not part of a checkpoint's identity, so a
/// campaign may be resumed with a different policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CampaignPolicy {
    /// Maximum tasks in flight at once.
    pub concurrency: usize,
    /// Attempts allowed per task, counting the first.
    pub max_attempts: u32,
    /// Wall-clock limit for one attempt.
    pub task_timeout: Duration,
}

impl Default for CampaignPolicy {
    fn default() -> Self {
        Self {
            concurrency: 1,
            max_attempts: 2,
            task_timeout: Duration::from_secs(600),
        }
    }
}

/// Whether a run creates its checkpoint or continues an existing one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartMode {
    /// Requires that no checkpoint exists at the configured path.
    Fresh,
    /// Requires an existing checkpoint for the same suite and task set.
    Resume,
}

#[derive(Debug, Error)]
pub enum CampaignError {
    #[error("invalid campaign policy: {message}")]
    InvalidPolicy { message: &'static str },
    #[error(transparent)]
    Adapter(AdapterError),
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
    #[error(transparent)]
    Transition(#[from] TransitionError),
    #[error("checkpoint {path} already exists; resume it or choose another path")]
    CheckpointExists { path: PathBuf },
    #[error("checkpoint {path} does not exist; start a fresh campaign instead of resuming")]
    CheckpointMissing { path: PathBuf },
    #[error(
        "adapter {adapter} needs network or container access and requires the explicit external opt-in"
    )]
    ExternalAdapterNotAllowed { adapter: &'static str },
    #[error(
        "adapter {adapter} is not built into compresso-eval; implement SuiteAdapter in a crate with the required capability class"
    )]
    ExternalAdapterUnavailable { adapter: &'static str },
}

impl CampaignError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidPolicy { .. } => "invalid_policy",
            Self::Adapter(_) => "adapter_failed",
            Self::Checkpoint(error) => error.code(),
            Self::Transition(_) => "invalid_transition",
            Self::CheckpointExists { .. } => "checkpoint_exists",
            Self::CheckpointMissing { .. } => "checkpoint_missing",
            Self::ExternalAdapterNotAllowed { .. } => "external_adapter_not_allowed",
            Self::ExternalAdapterUnavailable { .. } => "external_adapter_unavailable",
        }
    }
}

/// The checkpoint as it stood when the run returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CampaignOutcome {
    pub checkpoint: Checkpoint,
    /// True when a stop request left non-terminal tasks behind.
    pub interrupted: bool,
    /// Tasks found `running` in the checkpoint and reclaimed on resume.
    pub recovered: Vec<String>,
}

/// Drives a suite to completion, persisting every state transition.
pub struct CampaignRunner {
    adapter: Arc<dyn SuiteAdapter>,
    policy: CampaignPolicy,
    checkpoint_path: PathBuf,
    stop: Arc<AtomicBool>,
}

struct Shared {
    checkpoint: Checkpoint,
    queue: VecDeque<usize>,
    error: Option<CampaignError>,
}

enum AttemptResult {
    Verdict(TaskMeasurement),
    Failure(AttemptFailureKind, String),
}

impl CampaignRunner {
    pub fn new(
        adapter: Arc<dyn SuiteAdapter>,
        policy: CampaignPolicy,
        checkpoint_path: impl Into<PathBuf>,
    ) -> Result<Self, CampaignError> {
        if policy.concurrency == 0 {
            return Err(CampaignError::InvalidPolicy {
                message: "concurrency must be at least 1",
            });
        }
        if policy.max_attempts == 0 {
            return Err(CampaignError::InvalidPolicy {
                message: "max_attempts must be at least 1",
            });
        }
        if policy.task_timeout.is_zero() {
            return Err(CampaignError::InvalidPolicy {
                message: "task_timeout must be greater than zero",
            });
        }
        Ok(Self {
            adapter,
            policy,
            checkpoint_path: checkpoint_path.into(),
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Setting the returned flag stops the campaign after in-flight attempts
    /// finish. No further task is started; the checkpoint stays resumable.
    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    pub fn checkpoint_path(&self) -> &Path {
        &self.checkpoint_path
    }

    pub fn run(&self, start: StartMode) -> Result<CampaignOutcome, CampaignError> {
        let suite = self.adapter.descriptor();
        let tasks = self.adapter.tasks().map_err(CampaignError::Adapter)?;
        let exists = self.checkpoint_path.exists();

        let (checkpoint, recovered) = match start {
            StartMode::Fresh if exists => {
                return Err(CampaignError::CheckpointExists {
                    path: self.checkpoint_path.clone(),
                });
            }
            StartMode::Resume if !exists => {
                return Err(CampaignError::CheckpointMissing {
                    path: self.checkpoint_path.clone(),
                });
            }
            StartMode::Fresh => (Checkpoint::fresh(suite, &tasks)?, Vec::new()),
            StartMode::Resume => {
                let mut checkpoint = Checkpoint::load(&self.checkpoint_path)?;
                checkpoint.ensure_matches(&suite, &tasks)?;
                let recovered = checkpoint.recover_interrupted(self.policy.max_attempts);
                (checkpoint, recovered)
            }
        };
        checkpoint.save(&self.checkpoint_path)?;

        let queue = checkpoint
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, task)| task.state == TaskState::Pending)
            .map(|(index, _)| index)
            .collect::<VecDeque<_>>();
        let workers = self.policy.concurrency.min(queue.len());
        let shared = Mutex::new(Shared {
            checkpoint,
            queue,
            error: None,
        });

        thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| self.work(&shared));
            }
        });

        let shared = shared.into_inner().unwrap_or_else(PoisonError::into_inner);
        if let Some(error) = shared.error {
            return Err(error);
        }
        let interrupted = !shared.checkpoint.is_complete();
        Ok(CampaignOutcome {
            checkpoint: shared.checkpoint,
            interrupted,
            recovered,
        })
    }

    fn work(&self, shared: &Mutex<Shared>) {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return;
            }
            let (index, spec, attempt) = {
                let mut guard = lock(shared);
                if guard.error.is_some() {
                    return;
                }
                let Some(index) = guard.queue.pop_front() else {
                    return;
                };
                match self.begin(&mut guard, index) {
                    Ok(started) => started,
                    Err(error) => {
                        guard.error = Some(error);
                        return;
                    }
                }
            };

            let started = Instant::now();
            let result = self.attempt(spec, attempt);
            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

            let mut guard = lock(shared);
            if let Err(error) = self.finish(&mut guard, index, attempt, duration_ms, result) {
                guard.error.get_or_insert(error);
                return;
            }
        }
    }

    /// Marks the task running and persists that before any work starts, so a
    /// crash during the attempt is visible to the next process.
    fn begin(
        &self,
        shared: &mut Shared,
        index: usize,
    ) -> Result<(usize, TaskSpec, u32), CampaignError> {
        let record = &mut shared.checkpoint.tasks[index];
        record.state = record.state.transition(TaskState::Running)?;
        record.attempts += 1;
        let started = (
            index,
            TaskSpec {
                id: record.id.clone(),
            },
            record.attempts,
        );
        shared.checkpoint.save(&self.checkpoint_path)?;
        Ok(started)
    }

    fn finish(
        &self,
        shared: &mut Shared,
        index: usize,
        attempt: u32,
        duration_ms: u64,
        result: AttemptResult,
    ) -> Result<(), CampaignError> {
        let record = &mut shared.checkpoint.tasks[index];
        record.duration_ms = duration_ms;
        let mut retry = false;
        match result {
            AttemptResult::Verdict(measurement) => {
                let next = if measurement.resolved {
                    TaskState::Passed
                } else {
                    TaskState::Failed
                };
                record.state = record.state.transition(next)?;
                record.measurement = Some(measurement);
            }
            AttemptResult::Failure(kind, message) => {
                retry = attempt < self.policy.max_attempts;
                let next = if retry {
                    TaskState::Pending
                } else {
                    TaskState::Exhausted
                };
                record.state = record.state.transition(next)?;
                record.last_failure = Some(AttemptFailure {
                    kind,
                    attempt,
                    message,
                });
            }
        }
        if retry {
            shared.queue.push_back(index);
        }
        shared.checkpoint.save(&self.checkpoint_path)?;
        Ok(())
    }

    /// Runs one attempt on its own thread and waits at most the task timeout.
    ///
    /// A thread cannot be killed. On timeout the cancellation flag is raised
    /// and the thread is detached; a well-behaved adapter observes the flag
    /// and unwinds on its own.
    fn attempt(&self, spec: TaskSpec, attempt: u32) -> AttemptResult {
        let cancel = Arc::new(AtomicBool::new(false));
        let context = TaskContext::new(attempt, self.policy.task_timeout, Arc::clone(&cancel));
        let adapter = Arc::clone(&self.adapter);
        let (sender, receiver) = mpsc::channel();

        let spawned = thread::Builder::new()
            .name(format!("campaign-task-{}", spec.id))
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| adapter.run_task(&spec, &context)));
                let _ = sender.send(result);
            });
        if let Err(error) = spawned {
            return AttemptResult::Failure(
                AttemptFailureKind::Error,
                format!("failed to start task thread: {error}"),
            );
        }

        match receiver.recv_timeout(self.policy.task_timeout) {
            Ok(Ok(Ok(measurement))) => AttemptResult::Verdict(measurement),
            Ok(Ok(Err(error))) => {
                AttemptResult::Failure(AttemptFailureKind::Error, error.to_string())
            }
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => AttemptResult::Failure(
                AttemptFailureKind::Panic,
                "adapter panicked while running the task".to_owned(),
            ),
            Err(RecvTimeoutError::Timeout) => {
                cancel.store(true, Ordering::SeqCst);
                AttemptResult::Failure(
                    AttemptFailureKind::Timeout,
                    format!(
                        "attempt exceeded the {} ms task timeout",
                        self.policy.task_timeout.as_millis()
                    ),
                )
            }
        }
    }
}

fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}
