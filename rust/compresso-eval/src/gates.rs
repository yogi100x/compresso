use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::EvalReport;

const GATES_SCHEMA_VERSION: u32 = 1;
// 06-fixtures.md §4.5: the eligible median needs a non-gameable population.
const MIN_ELIGIBLE_FIXTURES: usize = 5;

/// Versioned release-gate thresholds loaded from the committed TOML contract.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Gates {
    pub schema_version: u32,
    pub reduction: ReductionGates,
    pub safety: SafetyGates,
    pub latency: LatencyGates,
    pub recall: RecallGates,
}

/// Safe-mode reduction thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReductionGates {
    pub median_reduction_eligible_min: f64,
}

/// Content-integrity thresholds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyGates {
    pub protected_mutations_max: usize,
    pub malformed_json_max: usize,
}

/// Compression-latency thresholds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyGates {
    pub p95_ms_max: u64,
}

/// D40 recall thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecallGates {
    pub protected_min: f64,
    pub overall_min: f64,
}

/// Errors at the versioned release-gate configuration boundary.
#[derive(Debug, Error)]
pub enum GateError {
    #[error("failed to read gates file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid gates file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("unsupported gates schema version {actual}; expected {expected}")]
    UnsupportedSchema { expected: u32, actual: u32 },
    #[error("invalid gate threshold {name}: {reason}")]
    InvalidThreshold {
        name: &'static str,
        reason: &'static str,
    },
}

/// The D37 and D40 aggregate gate results in deterministic presentation order.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct GateReport {
    pub passed: bool,
    pub gates: Vec<GateOutcome>,
    pub failed_fixtures: Vec<String>,
}

/// One named threshold comparison.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct GateOutcome {
    pub name: GateName,
    pub operator: GateOperator,
    pub threshold: f64,
    pub observed: f64,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<GateFailureReason>,
}

/// Stable aggregate names shared by human and JSON reports.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateName {
    MedianReductionEligible,
    ProtectedMutations,
    MalformedJson,
    P95LatencyMs,
    RecallProbeCount,
    ProtectedProbeRecall,
    OverallProbeRecall,
}

/// Stable reasons for gate failures requiring more context than a comparison.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateFailureReason {
    InsufficientEligibleFixtures,
    NoProbes,
}

/// The strict comparison required by one release gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum GateOperator {
    #[serde(rename = ">")]
    GreaterThan,
    #[serde(rename = "==")]
    Equal,
    #[serde(rename = "<")]
    LessThan,
    #[serde(rename = ">=")]
    GreaterThanOrEqual,
}

impl Gates {
    /// Loads and validates the strict, versioned TOML contract at `path`.
    pub fn load(path: &Path) -> Result<Self, GateError> {
        let source = fs::read_to_string(path).map_err(|source| GateError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let gates = toml::from_str::<Self>(&source).map_err(|source| GateError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        gates.validate()?;
        Ok(gates)
    }

    /// Applies the D37 and D40 gates. Fixture failures remain report data and
    /// also fail the overall gate result without becoming threshold rows.
    pub fn evaluate(&self, report: &EvalReport) -> GateReport {
        let aggregate = &report.aggregate;
        let probe_count = report
            .recall
            .fixtures
            .iter()
            .map(|fixture| fixture.probes.len())
            .sum::<usize>();
        let mut reduction = GateOutcome::new(
            GateName::MedianReductionEligible,
            GateOperator::GreaterThan,
            self.reduction.median_reduction_eligible_min,
            aggregate.median_reduction_eligible,
        );
        if aggregate.eligible_fixtures < MIN_ELIGIBLE_FIXTURES {
            reduction = reduction.fail_with_reason(GateFailureReason::InsufficientEligibleFixtures);
        }
        let gates = vec![
            reduction,
            GateOutcome::new(
                GateName::ProtectedMutations,
                GateOperator::Equal,
                self.safety.protected_mutations_max as f64,
                aggregate.protected_mutations as f64,
            ),
            GateOutcome::new(
                GateName::MalformedJson,
                GateOperator::Equal,
                self.safety.malformed_json_max as f64,
                aggregate.malformed_json as f64,
            ),
            GateOutcome::new(
                GateName::P95LatencyMs,
                GateOperator::LessThan,
                self.latency.p95_ms_max as f64,
                aggregate.p95_latency_ms as f64,
            ),
            GateOutcome::new(
                GateName::RecallProbeCount,
                GateOperator::GreaterThan,
                0.0,
                probe_count as f64,
            )
            .with_failure_reason(GateFailureReason::NoProbes),
            GateOutcome::new(
                GateName::ProtectedProbeRecall,
                GateOperator::Equal,
                self.recall.protected_min,
                report.recall.protected_probe_recall,
            ),
            GateOutcome::new(
                GateName::OverallProbeRecall,
                GateOperator::GreaterThanOrEqual,
                self.recall.overall_min,
                report.recall.overall_probe_recall,
            ),
        ];
        let mut failed_fixtures = report
            .fixtures
            .iter()
            .filter(|fixture| !fixture.passed)
            .map(|fixture| fixture.id.clone())
            .collect::<Vec<_>>();
        failed_fixtures.sort();
        failed_fixtures.dedup();
        GateReport {
            passed: gates.iter().all(|gate| gate.passed) && failed_fixtures.is_empty(),
            gates,
            failed_fixtures,
        }
    }

    fn validate(&self) -> Result<(), GateError> {
        if self.schema_version != GATES_SCHEMA_VERSION {
            return Err(GateError::UnsupportedSchema {
                expected: GATES_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        let reduction = self.reduction.median_reduction_eligible_min;
        if !reduction.is_finite() || !(0.0..1.0).contains(&reduction) {
            return Err(GateError::InvalidThreshold {
                name: "median_reduction_eligible_min",
                reason: "must be finite and strictly between zero and one",
            });
        }
        if self.latency.p95_ms_max == 0 {
            return Err(GateError::InvalidThreshold {
                name: "p95_ms_max",
                reason: "must be greater than zero",
            });
        }
        if self.safety.protected_mutations_max != 0 {
            return Err(GateError::InvalidThreshold {
                name: "protected_mutations_max",
                reason: "D37 requires zero tolerance",
            });
        }
        if self.safety.malformed_json_max != 0 {
            return Err(GateError::InvalidThreshold {
                name: "malformed_json_max",
                reason: "D37 requires zero tolerance",
            });
        }
        if self.recall.protected_min != 1.0 {
            return Err(GateError::InvalidThreshold {
                name: "protected_min",
                reason: "D40 requires exactly 1.0",
            });
        }
        if !self.recall.overall_min.is_finite() || !(0.99..=1.0).contains(&self.recall.overall_min)
        {
            return Err(GateError::InvalidThreshold {
                name: "overall_min",
                reason: "D40 requires a finite threshold between 0.99 and 1.0",
            });
        }
        Ok(())
    }
}

impl GateOutcome {
    fn new(name: GateName, operator: GateOperator, threshold: f64, observed: f64) -> Self {
        Self {
            name,
            operator,
            threshold,
            observed,
            passed: operator.evaluate(observed, threshold),
            reason: None,
        }
    }

    fn fail_with_reason(mut self, reason: GateFailureReason) -> Self {
        self.passed = false;
        self.reason = Some(reason);
        self
    }

    fn with_failure_reason(mut self, reason: GateFailureReason) -> Self {
        if !self.passed {
            self.reason = Some(reason);
        }
        self
    }

    /// Renders a deterministic line containing the name, threshold, observation, and status.
    pub fn line(&self) -> String {
        let reason = self
            .reason
            .map(|reason| format!(", reason {}", reason.as_str()))
            .unwrap_or_default();
        format!(
            "{}: threshold {} {}, observed {}{}, {}",
            self.name.as_str(),
            self.operator.as_str(),
            format_number(self.threshold),
            format_number(self.observed),
            reason,
            if self.passed { "PASS" } else { "FAIL" },
        )
    }
}

impl GateReport {
    /// Finds a gate by its stable aggregate name.
    pub fn gate(&self, name: GateName) -> Option<&GateOutcome> {
        self.gates.iter().find(|gate| gate.name == name)
    }

    /// Renders exactly one deterministic human-readable line per gate.
    pub fn lines(&self) -> Vec<String> {
        self.gates.iter().map(GateOutcome::line).collect()
    }

    /// Returns a comma-separated stable list suitable for a non-zero CLI error.
    pub fn failed_names(&self) -> String {
        let mut names = self
            .gates
            .iter()
            .filter(|gate| !gate.passed)
            .map(|gate| gate.name.as_str().to_owned())
            .collect::<Vec<_>>();
        names.extend(
            self.failed_fixtures
                .iter()
                .map(|fixture| format!("fixture:{fixture}")),
        );
        names.join(", ")
    }
}

impl GateName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MedianReductionEligible => "median_reduction_eligible",
            Self::ProtectedMutations => "protected_mutations",
            Self::MalformedJson => "malformed_json",
            Self::P95LatencyMs => "p95_latency_ms",
            Self::RecallProbeCount => "recall_probe_count",
            Self::ProtectedProbeRecall => "protected_probe_recall",
            Self::OverallProbeRecall => "overall_probe_recall",
        }
    }
}

impl GateFailureReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InsufficientEligibleFixtures => "insufficient_eligible_fixtures",
            Self::NoProbes => "no_probes",
        }
    }
}

impl GateOperator {
    const fn as_str(self) -> &'static str {
        match self {
            Self::GreaterThan => ">",
            Self::Equal => "==",
            Self::LessThan => "<",
            Self::GreaterThanOrEqual => ">=",
        }
    }

    fn evaluate(self, observed: f64, threshold: f64) -> bool {
        match self {
            Self::GreaterThan => observed > threshold,
            Self::Equal => observed == threshold,
            Self::LessThan => observed < threshold,
            Self::GreaterThanOrEqual => observed >= threshold,
        }
    }
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        return format!("{value:.0}");
    }
    format!("{value:.6}").trim_end_matches('0').to_owned()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use compresso_core::Mode;

    use crate::{
        AggregateMetrics, EvalReport, EvalRunner, EvaluationKind, ExpectationFailure,
        FixtureOutcome, FixtureRecallOutcome, FixtureSet, RecallProbeOutcome, RecallReport,
    };

    fn gates_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/plan/gates.toml")
    }

    fn report(
        median_reduction_eligible: f64,
        protected_mutations: usize,
        malformed_json: usize,
        p95_latency_ms: u64,
    ) -> EvalReport {
        EvalReport {
            schema_version: 1,
            fixtures: Vec::new(),
            aggregate: AggregateMetrics {
                compression_fixtures: 14,
                eligible_fixtures: 9,
                median_reduction_eligible,
                p95_latency_ms,
                protected_mutations,
                malformed_json,
                fallback_rate: 0.0,
            },
            recall: RecallReport {
                fixtures: vec![FixtureRecallOutcome {
                    fixture: "synthetic-recall".to_owned(),
                    probes: vec![RecallProbeOutcome {
                        fixture: "synthetic-recall".to_owned(),
                        probe_index: 0,
                        protected: true,
                        passed: true,
                    }],
                }],
                protected_probe_recall: 1.0,
                overall_probe_recall: 1.0,
            },
        }
    }

    fn report_with_recall(
        protected_probe_recall: f64,
        overall_probe_recall: f64,
        probe_count: usize,
    ) -> EvalReport {
        let mut report = report(0.50, 0, 0, 100);
        report.recall = RecallReport {
            fixtures: if probe_count == 0 {
                Vec::new()
            } else {
                vec![FixtureRecallOutcome {
                    fixture: "synthetic-recall".to_owned(),
                    probes: (0..probe_count)
                        .map(|probe_index| RecallProbeOutcome {
                            fixture: "synthetic-recall".to_owned(),
                            probe_index,
                            protected: probe_index == 0,
                            passed: true,
                        })
                        .collect(),
                }]
            },
            protected_probe_recall,
            overall_probe_recall,
        };
        report
    }

    #[test]
    fn test_gates_toml_parses_and_matches_d37() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");

        assert_eq!(gates.schema_version, 1);
        assert_eq!(gates.reduction.median_reduction_eligible_min, 0.25);
        assert_eq!(gates.safety.protected_mutations_max, 0);
        assert_eq!(gates.safety.malformed_json_max, 0);
        assert_eq!(gates.latency.p95_ms_max, 200);
    }

    #[test]
    fn test_recall_gates_toml_matches_d40() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");

        assert_eq!(gates.recall.protected_min, 1.0);
        assert_eq!(gates.recall.overall_min, 0.99);
    }

    #[test]
    fn test_reduction_gate_fails_below_threshold() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let evaluated = gates.evaluate(&report(0.10, 0, 0, 100));
        let reduction = evaluated
            .gate(GateName::MedianReductionEligible)
            .expect("reduction gate should be present");

        assert!(!evaluated.passed);
        assert!(!reduction.passed);
        assert_eq!(reduction.threshold, 0.25);
        assert_eq!(reduction.observed, 0.10);
        assert!(
            evaluated
                .failed_names()
                .contains("median_reduction_eligible")
        );
        assert!(reduction.line().ends_with("FAIL"));
    }

    #[test]
    fn test_protected_gate_zero_tolerance() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let evaluated = gates.evaluate(&report(0.50, 1, 0, 100));

        assert!(!evaluated.passed);
        assert!(
            !evaluated
                .gate(GateName::ProtectedMutations)
                .expect("protected gate should be present")
                .passed
        );
        for name in [
            GateName::MedianReductionEligible,
            GateName::MalformedJson,
            GateName::P95LatencyMs,
        ] {
            assert!(
                evaluated
                    .gate(name)
                    .expect("other gate should be present")
                    .passed
            );
        }
    }

    #[test]
    fn test_strict_gate_boundaries() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");

        let reduction_boundary = gates.evaluate(&report(0.25, 0, 0, 100));
        assert!(
            !reduction_boundary
                .gate(GateName::MedianReductionEligible)
                .expect("reduction gate should be present")
                .passed
        );
        let latency_boundary = gates.evaluate(&report(0.50, 0, 0, 200));
        assert!(
            !latency_boundary
                .gate(GateName::P95LatencyMs)
                .expect("latency gate should be present")
                .passed
        );
    }

    #[test]
    fn test_gate_lines_and_json_have_equivalent_shape() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let evaluated = gates.evaluate(&report(0.50, 0, 0, 100));
        let lines = evaluated.lines();
        let json = serde_json::to_value(&evaluated).expect("gate report should serialize");

        assert!(evaluated.passed);
        assert_eq!(lines.len(), 7);
        assert_eq!(json["gates"].as_array().map(Vec::len), Some(7));
        for (line, gate) in lines.iter().zip(&evaluated.gates) {
            assert!(line.contains(gate.name.as_str()));
            assert!(line.contains("threshold"));
            assert!(line.contains("observed"));
            assert!(line.ends_with("PASS"));
        }
    }

    #[test]
    fn test_fixture_expectation_failures_fail_gate_report_and_name_fixtures() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let mut report = report(0.50, 0, 0, 100);
        report.fixtures = ["z-regression", "a-regression"]
            .map(|id| FixtureOutcome {
                id: id.to_owned(),
                kind: EvaluationKind::Compression,
                eligible: true,
                mode: Mode::Safe,
                before_tokens: 100,
                after_tokens: 50,
                duration_ms: 100,
                reduction_pct: 50.0,
                fallback_used: false,
                fallback_reason: None,
                protected_mutations: 0,
                malformed_json: 0,
                passed: false,
                failures: vec![ExpectationFailure::MissingSubstring {
                    expected: "ground truth".to_owned(),
                }],
                atlas: None,
                output: None,
            })
            .into();

        let evaluated = gates.evaluate(&report);

        assert!(!evaluated.passed);
        assert!(evaluated.gates.iter().all(|gate| gate.passed));
        assert_eq!(evaluated.failed_fixtures, ["a-regression", "z-regression"]);
        assert_eq!(
            evaluated.failed_names(),
            "fixture:a-regression, fixture:z-regression"
        );
    }

    #[test]
    fn test_fewer_than_five_eligible_fixtures_fails_median_gate() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let mut sparse = report(0.50, 0, 0, 100);
        sparse.aggregate.eligible_fixtures = 4;

        let evaluated = gates.evaluate(&sparse);
        let reduction = evaluated
            .gate(GateName::MedianReductionEligible)
            .expect("median reduction gate should be present");

        assert!(!evaluated.passed);
        assert!(!reduction.passed);
        assert_eq!(
            reduction.reason,
            Some(GateFailureReason::InsufficientEligibleFixtures)
        );
        assert!(
            reduction
                .line()
                .contains("reason insufficient_eligible_fixtures")
        );
    }

    #[test]
    fn test_gate_validation_rejects_wrong_version_and_invalid_bounds() {
        let mut gates = Gates::load(&gates_path()).expect("committed gates should load");
        gates.schema_version = 2;
        assert!(matches!(
            gates.validate(),
            Err(GateError::UnsupportedSchema {
                expected: 1,
                actual: 2
            })
        ));

        gates.schema_version = 1;
        gates.reduction.median_reduction_eligible_min = f64::NAN;
        assert!(matches!(
            gates.validate(),
            Err(GateError::InvalidThreshold {
                name: "median_reduction_eligible_min",
                ..
            })
        ));

        gates.reduction.median_reduction_eligible_min = 0.25;
        gates.safety.protected_mutations_max = 1;
        assert!(matches!(
            gates.validate(),
            Err(GateError::InvalidThreshold {
                name: "protected_mutations_max",
                ..
            })
        ));
    }

    #[test]
    fn test_recall_gate_validation_rejects_weakened_thresholds() {
        let mut gates = Gates::load(&gates_path()).expect("committed gates should load");
        gates.recall.protected_min = 0.99;
        assert!(matches!(
            gates.validate(),
            Err(GateError::InvalidThreshold {
                name: "protected_min",
                ..
            })
        ));

        gates.recall.protected_min = 1.0;
        gates.recall.overall_min = 0.989;
        assert!(matches!(
            gates.validate(),
            Err(GateError::InvalidThreshold {
                name: "overall_min",
                ..
            })
        ));
    }

    #[test]
    fn test_protected_recall_zero_tolerance() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let evaluated = gates.evaluate(&report_with_recall(0.99, 1.0, 100));

        assert!(
            !evaluated
                .gate(GateName::ProtectedProbeRecall)
                .expect("protected recall gate should be present")
                .passed
        );
        assert!(
            evaluated
                .gate(GateName::OverallProbeRecall)
                .expect("overall recall gate should be present")
                .passed
        );
        assert!(evaluated.failed_names().contains("protected_probe_recall"));
    }

    #[test]
    fn test_overall_recall_boundary() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let below = gates.evaluate(&report_with_recall(1.0, 0.989, 100));
        let exact = gates.evaluate(&report_with_recall(1.0, 0.990, 100));

        assert!(
            !below
                .gate(GateName::OverallProbeRecall)
                .expect("overall recall gate should be present")
                .passed
        );
        assert!(
            exact
                .gate(GateName::OverallProbeRecall)
                .expect("overall recall gate should be present")
                .passed
        );
    }

    #[test]
    fn test_probeless_corpus_fails_gate() {
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let evaluated = gates.evaluate(&report_with_recall(1.0, 1.0, 0));
        let population = evaluated
            .gate(GateName::RecallProbeCount)
            .expect("recall population gate should be present");

        assert!(!evaluated.passed);
        assert!(!population.passed);
        assert_eq!(population.reason, Some(GateFailureReason::NoProbes));
        assert!(population.line().contains("reason no_probes"));
    }

    #[test]
    fn test_recall_gates_pass_on_committed_corpus() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let set = FixtureSet::load(&fixtures).expect("committed fixtures should load");
        let report = EvalRunner
            .try_run(&set)
            .expect("committed recall probes should pass the passthrough baseline");
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let evaluated = gates.evaluate(&report);

        let population = evaluated
            .gate(GateName::RecallProbeCount)
            .expect("recall population gate should be present");
        let protected = evaluated
            .gate(GateName::ProtectedProbeRecall)
            .expect("protected recall gate should be present");
        let overall = evaluated
            .gate(GateName::OverallProbeRecall)
            .expect("overall recall gate should be present");
        assert_eq!(population.observed, 10.0);
        assert!(population.passed);
        assert_eq!(protected.observed, 1.0);
        assert!(protected.passed);
        assert_eq!(overall.observed, 1.0);
        assert!(overall.passed);
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "release/dist corpus gate")]
    fn test_gates_pass_on_committed_corpus() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let set = FixtureSet::load(&fixtures).expect("committed fixtures should load");
        let report = EvalRunner.run(&set);
        let gates = Gates::load(&gates_path()).expect("committed gates should load");
        let evaluated = gates.evaluate(&report);

        assert!(
            evaluated.passed,
            "committed corpus gates failed:\n{}",
            evaluated.lines().join("\n")
        );
    }
}
