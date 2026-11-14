use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::adapter::{SuiteDescriptor, TaskMeasurement, TaskSpec};
use super::state::{AttemptFailure, AttemptFailureKind, TaskState};

pub const CHECKPOINT_SCHEMA_VERSION: u32 = 1;

/// Durable progress of one campaign. Tasks are kept in identifier order so the
/// file and every report derived from it are independent of completion order.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub schema_version: u32,
    pub suite: SuiteDescriptor,
    pub tasks: Vec<TaskRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRecord {
    pub id: String,
    pub state: TaskState,
    /// Attempts started, including one interrupted by a crash.
    pub attempts: u32,
    /// Wall time of the most recent finished attempt.
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement: Option<TaskMeasurement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<AttemptFailure>,
}

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("failed to read checkpoint {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("failed to write checkpoint {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("checkpoint {path} is not a valid campaign checkpoint: {message}")]
    Malformed { path: PathBuf, message: String },
    #[error("checkpoint schema version {found} is not supported (expected {expected})")]
    UnsupportedSchemaVersion { found: u64, expected: u32 },
    #[error(
        "checkpoint belongs to suite {found_id}@{found_version}, not {expected_id}@{expected_version}"
    )]
    SuiteMismatch {
        found_id: String,
        found_version: String,
        expected_id: String,
        expected_version: String,
    },
    #[error("checkpoint task set differs from the suite (first difference: {task})")]
    TaskSetMismatch { task: String },
    #[error("suite task list is invalid: {message}")]
    InvalidTaskList { message: String },
}

impl CheckpointError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Read { .. } => "checkpoint_read",
            Self::Write { .. } => "checkpoint_write",
            Self::Malformed { .. } => "checkpoint_malformed",
            Self::UnsupportedSchemaVersion { .. } => "checkpoint_schema_version",
            Self::SuiteMismatch { .. } => "checkpoint_suite_mismatch",
            Self::TaskSetMismatch { .. } => "checkpoint_task_set_mismatch",
            Self::InvalidTaskList { .. } => "invalid_task_list",
        }
    }
}

impl Checkpoint {
    /// A checkpoint with every task pending.
    pub fn fresh(suite: SuiteDescriptor, tasks: &[TaskSpec]) -> Result<Self, CheckpointError> {
        let mut ids = BTreeSet::new();
        for task in tasks {
            if task.id.is_empty() {
                return Err(CheckpointError::InvalidTaskList {
                    message: "task identifier is empty".to_owned(),
                });
            }
            if !ids.insert(task.id.as_str()) {
                return Err(CheckpointError::InvalidTaskList {
                    message: format!("duplicate task identifier {}", task.id),
                });
            }
        }
        if ids.is_empty() {
            return Err(CheckpointError::InvalidTaskList {
                message: "suite has no tasks".to_owned(),
            });
        }
        Ok(Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            suite,
            tasks: ids
                .into_iter()
                .map(|id| TaskRecord {
                    id: id.to_owned(),
                    state: TaskState::Pending,
                    attempts: 0,
                    duration_ms: 0,
                    measurement: None,
                    last_failure: None,
                })
                .collect(),
        })
    }

    /// Reads and validates a checkpoint without binding it to a suite.
    pub fn load(path: &Path) -> Result<Self, CheckpointError> {
        let bytes = fs::read(path).map_err(|source| CheckpointError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let malformed = |message: String| CheckpointError::Malformed {
            path: path.to_path_buf(),
            message,
        };
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|error| malformed(error.to_string()))?;
        let version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| malformed("schema_version is missing".to_owned()))?;
        if version != u64::from(CHECKPOINT_SCHEMA_VERSION) {
            return Err(CheckpointError::UnsupportedSchemaVersion {
                found: version,
                expected: CHECKPOINT_SCHEMA_VERSION,
            });
        }
        let checkpoint: Self =
            serde_json::from_value(value).map_err(|error| malformed(error.to_string()))?;
        checkpoint.validate().map_err(malformed)?;
        Ok(checkpoint)
    }

    fn validate(&self) -> Result<(), String> {
        if self.tasks.is_empty() {
            return Err("checkpoint has no tasks".to_owned());
        }
        for pair in self.tasks.windows(2) {
            if pair[0].id >= pair[1].id {
                return Err(format!(
                    "tasks are not in strictly ascending identifier order at {}",
                    pair[1].id
                ));
            }
        }
        for task in &self.tasks {
            let verdict = matches!(task.state, TaskState::Passed | TaskState::Failed);
            match (&task.measurement, verdict) {
                (None, true) => {
                    return Err(format!(
                        "task {} is {} without a measurement",
                        task.id,
                        task.state.name()
                    ));
                }
                (Some(_), false) => {
                    return Err(format!(
                        "task {} is {} but carries a measurement",
                        task.id,
                        task.state.name()
                    ));
                }
                (Some(measurement), true)
                    if measurement.resolved != (task.state == TaskState::Passed) =>
                {
                    return Err(format!(
                        "task {} state disagrees with its measurement",
                        task.id
                    ));
                }
                _ => {}
            }
            if task.state != TaskState::Pending && task.attempts == 0 {
                return Err(format!(
                    "task {} is {} with zero attempts",
                    task.id,
                    task.state.name()
                ));
            }
        }
        Ok(())
    }

    /// Refuses a checkpoint written for another suite, version, or task set.
    pub fn ensure_matches(
        &self,
        suite: &SuiteDescriptor,
        tasks: &[TaskSpec],
    ) -> Result<(), CheckpointError> {
        if self.suite.id != suite.id || self.suite.version != suite.version {
            return Err(CheckpointError::SuiteMismatch {
                found_id: self.suite.id.clone(),
                found_version: self.suite.version.clone(),
                expected_id: suite.id.clone(),
                expected_version: suite.version.clone(),
            });
        }
        let expected = Self::fresh(suite.clone(), tasks)?;
        let recorded = self.tasks.iter().map(|task| task.id.as_str());
        let current = expected.tasks.iter().map(|task| task.id.as_str());
        let recorded = recorded.collect::<BTreeSet<_>>();
        let current = current.collect::<BTreeSet<_>>();
        if let Some(task) = recorded.symmetric_difference(&current).next() {
            return Err(CheckpointError::TaskSetMismatch {
                task: (*task).to_owned(),
            });
        }
        Ok(())
    }

    /// Reclaims tasks a previous process left `running`.
    ///
    /// The interrupted attempt stays counted, so a task that crashes the
    /// process on every attempt is exhausted rather than retried forever.
    /// Returns the identifiers that were reclaimed.
    ///
    /// The attempt budget is also re-applied to `pending` tasks, because a
    /// resume may lower it: a task awaiting a retry it is no longer entitled
    /// to is exhausted instead of receiving one attempt past the bound.
    pub fn recover_interrupted(&mut self, max_attempts: u32) -> Vec<String> {
        let mut recovered = Vec::new();
        for task in &mut self.tasks {
            if task.state == TaskState::Pending && task.attempts >= max_attempts {
                task.state = TaskState::Exhausted;
                continue;
            }
            if task.state != TaskState::Running {
                continue;
            }
            task.last_failure = Some(AttemptFailure {
                kind: AttemptFailureKind::Interrupted,
                attempt: task.attempts,
                message: "process stopped while the task was running".to_owned(),
            });
            task.state = if task.attempts >= max_attempts {
                TaskState::Exhausted
            } else {
                TaskState::Pending
            };
            recovered.push(task.id.clone());
        }
        recovered
    }

    pub fn is_complete(&self) -> bool {
        self.tasks.iter().all(|task| task.state.is_terminal())
    }

    /// Replaces the file atomically: a reader sees the previous or the new
    /// checkpoint in full, never a partial write.
    pub fn save(&self, path: &Path) -> Result<(), CheckpointError> {
        let write_error = |source: io::Error| CheckpointError::Write {
            path: path.to_path_buf(),
            source,
        };
        let mut encoded = serde_json::to_vec_pretty(self)
            .map_err(|error| write_error(io::Error::other(error)))?;
        encoded.push(b'\n');

        let temporary = temporary_path(path);
        let result = (|| {
            let mut file = File::create(&temporary)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)?;
            sync_parent(path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(write_error)
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}
