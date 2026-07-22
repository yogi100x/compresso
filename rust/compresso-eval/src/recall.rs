use compresso_core::{CompressionBudget, ContextGraph, Engine, Mode, TokenProvider, counter_for};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fixtures::Fixture;
use crate::runner::FixtureOutcome;

/// Corpus-wide recall-probe results for compressed output.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct RecallReport {
    pub fixtures: Vec<FixtureRecallOutcome>,
    pub protected_probe_recall: f64,
    pub overall_probe_recall: f64,
}

/// Recall-probe results for one compression fixture.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct FixtureRecallOutcome {
    pub fixture: String,
    pub probes: Vec<RecallProbeOutcome>,
}

/// One byte-exact probe result, identified without emitting its corpus content.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RecallProbeOutcome {
    pub fixture: String,
    pub probe_index: usize,
    pub protected: bool,
    pub passed: bool,
}

/// Fatal evaluation errors that must not be downgraded to release-gate failures.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum EvalRunError {
    #[error(
        "broken_probe: fixture {fixture} probe {probe_index} is absent from passthrough output"
    )]
    BrokenProbe { fixture: String, probe_index: usize },
}

impl EvalRunError {
    /// Stable machine-facing code for CLI error envelopes.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BrokenProbe { .. } => "broken_probe",
        }
    }
}

/// Runs the required passthrough baseline and rejects corpus probes absent from input.
pub(crate) fn validate_passthrough(
    fixture: &Fixture,
    budget: &CompressionBudget,
    provider: TokenProvider,
) -> Result<(), EvalRunError> {
    if fixture.recall_probes.is_empty() {
        return Ok(());
    }

    let baseline = Engine::new(Mode::Passthrough, counter_for(provider))
        .compress(fixture.graph.clone(), budget);
    let baseline_text = concatenated_output(&baseline.output);
    for (probe_index, probe) in fixture.recall_probes.iter().enumerate() {
        if !baseline_text.contains(&probe.answer_substring) {
            return Err(EvalRunError::BrokenProbe {
                fixture: fixture.name.clone(),
                probe_index,
            });
        }
    }
    Ok(())
}

/// Evaluates all compressed outputs in stable fixture and declaration order.
pub(crate) fn evaluate_corpus(fixtures: &[Fixture], outcomes: &[FixtureOutcome]) -> RecallReport {
    let fixture_results = fixtures
        .iter()
        .zip(outcomes)
        .filter(|(fixture, _)| {
            !fixture.recall_probes.is_empty()
                && !matches!(
                    fixture.input,
                    crate::fixtures::FixtureInput::Repository { .. }
                )
        })
        .map(|(fixture, outcome)| {
            let output_text = outcome
                .output
                .as_ref()
                .map(concatenated_output)
                .unwrap_or_default();
            let probes = fixture
                .recall_probes
                .iter()
                .enumerate()
                .map(|(probe_index, probe)| RecallProbeOutcome {
                    fixture: fixture.name.clone(),
                    probe_index,
                    protected: probe.protected,
                    passed: output_text.contains(&probe.answer_substring),
                })
                .collect();
            FixtureRecallOutcome {
                fixture: fixture.name.clone(),
                probes,
            }
        })
        .collect::<Vec<_>>();

    let probes = fixture_results
        .iter()
        .flat_map(|fixture| fixture.probes.iter())
        .collect::<Vec<_>>();
    let protected = probes
        .iter()
        .copied()
        .filter(|probe| probe.protected)
        .collect::<Vec<_>>();

    RecallReport {
        protected_probe_recall: recall_ratio(&protected),
        overall_probe_recall: recall_ratio(&probes),
        fixtures: fixture_results,
    }
}

fn recall_ratio(probes: &[&RecallProbeOutcome]) -> f64 {
    if probes.is_empty() {
        return 1.0;
    }
    probes.iter().filter(|probe| probe.passed).count() as f64 / probes.len() as f64
}

fn concatenated_output(graph: &ContextGraph) -> String {
    graph
        .nodes
        .iter()
        .map(|node| node.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
