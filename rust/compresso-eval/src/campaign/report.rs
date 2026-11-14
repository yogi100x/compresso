use serde::Serialize;

use super::checkpoint::{Checkpoint, TaskRecord};
use super::state::{AttemptFailure, TaskState};
use crate::recall::RecallReport;
use crate::runner::{
    EvalReport, EvaluationKind, ExpectationFailure, FixtureOutcome, aggregate_metrics_for,
    reduction_percent,
};

/// A campaign rendered in the standard evaluation report shape, with the
/// campaign-specific bookkeeping under one additional `campaign` key.
///
/// Each task is one `fixtures` row of kind `campaign`. `aggregate` is computed
/// over those rows exactly as it is for compression fixtures. `recall` is
/// empty: external suites carry no recall probes, so release gates that
/// require probes do not apply to campaign reports.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CampaignReport {
    #[serde(flatten)]
    pub report: EvalReport,
    pub campaign: CampaignSummary,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CampaignSummary {
    pub schema_version: u32,
    pub suite: String,
    pub version: String,
    /// False while any task is still pending or running.
    pub complete: bool,
    pub tasks_total: usize,
    pub passed: usize,
    pub failed: usize,
    pub exhausted: usize,
    pub pending: usize,
    pub running: usize,
    /// `passed / tasks_total`; unfinished and exhausted tasks count against it.
    pub resolved_rate: f64,
    pub attempts_total: u64,
    pub retried_tasks: usize,
    pub tasks: Vec<CampaignTaskSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CampaignTaskSummary {
    pub id: String,
    pub state: TaskState,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<AttemptFailure>,
}

impl CampaignReport {
    /// Aggregates in task-identifier order, so the report does not depend on
    /// scheduling, concurrency, or how many times the campaign was resumed.
    pub fn from_checkpoint(checkpoint: &Checkpoint) -> Self {
        let fixtures = checkpoint
            .tasks
            .iter()
            .map(|task| outcome(checkpoint, task))
            .collect::<Vec<_>>();
        let aggregate = aggregate_metrics_for(&fixtures, EvaluationKind::Campaign);
        let count = |state: TaskState| {
            checkpoint
                .tasks
                .iter()
                .filter(|task| task.state == state)
                .count()
        };
        let passed = count(TaskState::Passed);
        let tasks_total = checkpoint.tasks.len();

        Self {
            report: EvalReport {
                schema_version: 1,
                fixtures,
                aggregate,
                recall: RecallReport {
                    fixtures: Vec::new(),
                    protected_probe_recall: 1.0,
                    overall_probe_recall: 1.0,
                },
            },
            campaign: CampaignSummary {
                schema_version: 1,
                suite: checkpoint.suite.id.clone(),
                version: checkpoint.suite.version.clone(),
                complete: checkpoint.is_complete(),
                tasks_total,
                passed,
                failed: count(TaskState::Failed),
                exhausted: count(TaskState::Exhausted),
                pending: count(TaskState::Pending),
                running: count(TaskState::Running),
                resolved_rate: if tasks_total == 0 {
                    0.0
                } else {
                    passed as f64 / tasks_total as f64
                },
                attempts_total: checkpoint
                    .tasks
                    .iter()
                    .map(|task| u64::from(task.attempts))
                    .sum(),
                retried_tasks: checkpoint
                    .tasks
                    .iter()
                    .filter(|task| task.attempts > 1)
                    .count(),
                tasks: checkpoint
                    .tasks
                    .iter()
                    .map(|task| CampaignTaskSummary {
                        id: task.id.clone(),
                        state: task.state,
                        attempts: task.attempts,
                        last_failure: task.last_failure.clone(),
                    })
                    .collect(),
            },
        }
    }
}

fn outcome(checkpoint: &Checkpoint, task: &TaskRecord) -> FixtureOutcome {
    let (before_tokens, after_tokens) = task.measurement.as_ref().map_or((0, 0), |measurement| {
        (measurement.baseline_tokens, measurement.compressed_tokens)
    });
    let passed = task.state == TaskState::Passed;
    FixtureOutcome {
        id: format!("{}/{}", checkpoint.suite.id, task.id),
        kind: EvaluationKind::Campaign,
        // Only a measured task contributes to the reduction median.
        eligible: task.measurement.is_some(),
        mode: checkpoint.suite.mode,
        before_tokens,
        after_tokens,
        duration_ms: task.duration_ms,
        reduction_pct: reduction_percent(before_tokens, after_tokens),
        fallback_used: false,
        fallback_reason: None,
        protected_mutations: 0,
        malformed_json: 0,
        passed,
        failures: if passed {
            Vec::new()
        } else {
            vec![ExpectationFailure::CampaignTask { state: task.state }]
        },
        atlas: None,
        output: None,
    }
}
