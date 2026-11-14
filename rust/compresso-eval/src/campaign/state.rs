use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Lifecycle of one campaign task.
///
/// ```text
/// pending -> running -> passed | failed | exhausted
///               |
///               +-----> pending   (retryable failure with attempts remaining)
/// ```
///
/// `passed` and `failed` are verdicts from a task that ran to completion and
/// are never retried. `exhausted` means every permitted attempt ended in an
/// infrastructure failure (adapter error, timeout, panic, or interruption).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    Passed,
    Failed,
    Exhausted,
}

impl TaskState {
    /// Terminal states are never executed again, including after a resume.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Passed | Self::Failed | Self::Exhausted)
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Running)
                | (
                    Self::Running,
                    Self::Passed | Self::Failed | Self::Exhausted | Self::Pending
                )
        )
    }

    /// Returns `next` when the state machine permits the move.
    pub const fn transition(self, next: Self) -> Result<Self, TransitionError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(TransitionError {
                from: self,
                to: next,
            })
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Exhausted => "exhausted",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("campaign task cannot move from {} to {}", from.name(), to.name())]
pub struct TransitionError {
    pub from: TaskState,
    pub to: TaskState,
}

/// Why one attempt did not produce a verdict.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptFailureKind {
    /// The adapter reported an infrastructure error.
    Error,
    /// The attempt exceeded the per-task timeout.
    Timeout,
    /// The adapter panicked while running the task.
    Panic,
    /// The process stopped while the task was running; found on resume.
    Interrupted,
}

/// The most recent retryable failure recorded for a task.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptFailure {
    pub kind: AttemptFailureKind,
    pub attempt: u32,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [TaskState; 5] = [
        TaskState::Pending,
        TaskState::Running,
        TaskState::Passed,
        TaskState::Failed,
        TaskState::Exhausted,
    ];

    #[test]
    fn test_task_state_machine_permits_only_documented_transitions() {
        let permitted = [
            (TaskState::Pending, TaskState::Running),
            (TaskState::Running, TaskState::Passed),
            (TaskState::Running, TaskState::Failed),
            (TaskState::Running, TaskState::Exhausted),
            (TaskState::Running, TaskState::Pending),
        ];

        for from in ALL {
            for to in ALL {
                let expected = permitted.contains(&(from, to));
                assert_eq!(from.can_transition_to(to), expected, "{from:?} -> {to:?}");
                assert_eq!(from.transition(to).is_ok(), expected, "{from:?} -> {to:?}");
            }
        }
    }

    #[test]
    fn test_terminal_task_states_have_no_outgoing_transition() {
        for from in ALL.into_iter().filter(|state| state.is_terminal()) {
            for to in ALL {
                let error = from
                    .transition(to)
                    .expect_err("terminal state must not move");
                assert_eq!(error, TransitionError { from, to });
            }
        }
    }
}
