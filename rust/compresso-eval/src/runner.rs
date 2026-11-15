use std::fs;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use compresso_atlas::{IndexReport, Indexer};
use compresso_core::{
    CompressionBudget, CompressionResult, ContextGraph, EdgeKind as ContextEdgeKind, Engine,
    Lossiness, Mode, TokenProvider, counter_for,
};
use compresso_store::{EdgeKind, ResolvedEdgeRecord, Store};
use serde::{Deserialize, Serialize};

use crate::campaign::TaskState;
use crate::fixtures::{AtlasExpectations, Fixture, FixtureInput, FixtureSet};
use crate::recall::{EvalRunError, RecallReport, evaluate_corpus, validate_passthrough};

static TEMPORARY_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Runs committed, versioned fixtures through production compression and indexing paths.
#[derive(Clone, Copy, Debug, Default)]
pub struct EvalRunner;

/// One deterministic evaluation report. Timing fields are the only intentionally variable values.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct EvalReport {
    pub schema_version: u32,
    pub fixtures: Vec<FixtureOutcome>,
    pub aggregate: AggregateMetrics,
    pub recall: RecallReport,
}

/// Whether a row exercises compression, repository indexing, or one task of
/// an external benchmark campaign.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationKind {
    Compression,
    Atlas,
    Campaign,
}

/// The stable report row for one fixture.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct FixtureOutcome {
    pub id: String,
    pub kind: EvaluationKind,
    pub eligible: bool,
    pub mode: Mode,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub duration_ms: u64,
    pub reduction_pct: f64,
    pub fallback_used: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub protected_mutations: usize,
    pub malformed_json: usize,
    pub passed: bool,
    pub failures: Vec<ExpectationFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas: Option<AtlasMetrics>,
    /// Output is available to in-process evaluators without putting corpus content in JSON reports.
    #[serde(skip)]
    pub output: Option<ContextGraph>,
}

/// Atlas counters reported independently from compression metrics.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct AtlasMetrics {
    pub files_indexed: usize,
    pub symbols: usize,
    pub resolved_edges: usize,
}

/// Exact, automated fixture assertion failures. No model judge is used.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpectationFailure {
    MinimumReduction {
        expected_pct: f64,
        actual_pct: f64,
    },
    MissingSubstring {
        expected: String,
    },
    ForbiddenSubstring {
        forbidden: String,
    },
    ForbiddenTraceSubstring {
        forbidden: String,
    },
    FallbackMismatch {
        expected: bool,
        actual: bool,
    },
    FallbackChangedOutput,
    ProtectedMutation {
        node_id: String,
    },
    MalformedJson {
        node_id: String,
    },
    NondeterministicCompression,
    AtlasMinimumFiles {
        expected: usize,
        actual: usize,
    },
    AtlasMinimumSymbols {
        expected: usize,
        actual: usize,
    },
    AtlasMissingEdge {
        from_path: String,
        to_path: String,
        edge_kind: String,
    },
    RunnerError {
        stage: RunnerStage,
    },
    RunnerPanic,
    /// A benchmark campaign task that did not end in `passed`.
    CampaignTask {
        state: TaskState,
    },
}

/// Stable execution stages used in reports without leaking random temporary paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerStage {
    ValidateFixture,
    PrepareRepository,
    CopyRepository,
    OpenStore,
    IndexRepository,
    LoadRepository,
    ReadEdges,
    SerializeTrace,
}

/// Corpus metrics. `median_reduction_eligible` is a ratio in `[0, 1]` for the 0.25 gate.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AggregateMetrics {
    pub compression_fixtures: usize,
    pub eligible_fixtures: usize,
    pub median_reduction_eligible: f64,
    pub p95_latency_ms: u64,
    pub protected_mutations: usize,
    pub malformed_json: usize,
    pub fallback_rate: f64,
}

impl EvalRunner {
    /// Evaluates every fixture through the E7-02 compatibility path.
    ///
    /// This does not validate passthrough probes; canonical recall runs use [`Self::try_run`].
    pub fn run(&self, set: &FixtureSet) -> EvalReport {
        self.evaluate(set)
    }

    /// Evaluates every fixture after rejecting broken passthrough probes.
    pub fn try_run(&self, set: &FixtureSet) -> Result<EvalReport, EvalRunError> {
        for fixture in &set.fixtures {
            if matches!(
                fixture.input,
                FixtureInput::Graph { .. } | FixtureInput::Assembled
            ) {
                let provider = token_provider(fixture);
                let budget = compression_budget(fixture);
                validate_passthrough(fixture, &budget, provider)?;
            }
        }

        Ok(self.evaluate(set))
    }

    fn evaluate(&self, set: &FixtureSet) -> EvalReport {
        let fixtures = set
            .fixtures
            .iter()
            .map(|fixture| {
                catch_unwind(AssertUnwindSafe(|| evaluate_fixture(fixture)))
                    .unwrap_or_else(|_| panic_outcome(fixture))
            })
            .collect::<Vec<_>>();
        let aggregate = aggregate_metrics(&fixtures);
        let recall = evaluate_corpus(&set.fixtures, &fixtures);
        EvalReport {
            schema_version: 1,
            fixtures,
            aggregate,
            recall,
        }
    }
}

fn evaluate_fixture(fixture: &Fixture) -> FixtureOutcome {
    match &fixture.input {
        FixtureInput::Repository { path } => evaluate_atlas(fixture, path),
        FixtureInput::Graph { .. } | FixtureInput::Assembled => evaluate_compression(fixture),
    }
}

fn evaluate_compression(fixture: &Fixture) -> FixtureOutcome {
    let provider = token_provider(fixture);
    let engine = Engine::new(fixture.mode, counter_for(provider));
    let budget = measurement_budget(fixture);

    let first_input = fixture.graph.clone();
    let started = Instant::now();
    let first = engine.compress(first_input, &budget);
    let duration_ms = milliseconds(started.elapsed().as_millis());
    let second = engine.compress(fixture.graph.clone(), &budget);
    let reduction_pct = reduction_percent(first.before_tokens, first.after_tokens);
    let output_text = concatenated_output(&first.output);
    let mut failures = Vec::new();

    if stable_result(first.clone()) != stable_result(second) {
        failures.push(ExpectationFailure::NondeterministicCompression);
    }
    if reduction_pct + f64::EPSILON < fixture.expectations.min_reduction_pct {
        failures.push(ExpectationFailure::MinimumReduction {
            expected_pct: fixture.expectations.min_reduction_pct,
            actual_pct: reduction_pct,
        });
    }
    for expected in &fixture.expectations.must_preserve {
        if !output_text.contains(expected) {
            failures.push(ExpectationFailure::MissingSubstring {
                expected: expected.clone(),
            });
        }
    }
    for forbidden in &fixture.expectations.must_not_contain {
        if output_text.contains(forbidden) {
            failures.push(ExpectationFailure::ForbiddenSubstring {
                forbidden: forbidden.clone(),
            });
        }
    }
    match serde_json::to_string(&first.trace) {
        Ok(trace) => {
            for forbidden in &fixture.expectations.must_not_contain_in_trace {
                if trace.contains(forbidden) {
                    failures.push(ExpectationFailure::ForbiddenTraceSubstring {
                        forbidden: forbidden.clone(),
                    });
                }
            }
        }
        Err(_) => failures.push(ExpectationFailure::RunnerError {
            stage: RunnerStage::SerializeTrace,
        }),
    }
    if first.fallback_used != fixture.expectations.expect_fallback {
        failures.push(ExpectationFailure::FallbackMismatch {
            expected: fixture.expectations.expect_fallback,
            actual: first.fallback_used,
        });
    }
    if first.fallback_used && first.output != fixture.graph {
        failures.push(ExpectationFailure::FallbackChangedOutput);
    }

    let protected_nodes = protected_mutations(fixture, &first.output);
    for node_id in &protected_nodes {
        failures.push(ExpectationFailure::ProtectedMutation {
            node_id: node_id.clone(),
        });
    }
    let malformed_nodes = malformed_json_nodes(&fixture.graph, &first.output);
    for node_id in &malformed_nodes {
        failures.push(ExpectationFailure::MalformedJson {
            node_id: node_id.clone(),
        });
    }

    FixtureOutcome {
        id: fixture.name.clone(),
        kind: EvaluationKind::Compression,
        eligible: fixture.eligible,
        mode: fixture.mode,
        before_tokens: first.before_tokens,
        after_tokens: first.after_tokens,
        duration_ms,
        reduction_pct,
        fallback_used: first.fallback_used,
        fallback_reason: first.fallback_reason,
        protected_mutations: protected_nodes.len(),
        malformed_json: malformed_nodes.len(),
        passed: failures.is_empty(),
        failures,
        atlas: None,
        output: Some(first.output),
    }
}

fn token_provider(fixture: &Fixture) -> TokenProvider {
    fixture
        .graph
        .nodes
        .first()
        .map_or(TokenProvider::OpenAiO200k, |node| node.token_provider)
}

fn compression_budget(fixture: &Fixture) -> CompressionBudget {
    CompressionBudget {
        max_tokens: fixture.budget.max_tokens,
        max_latency_ms: fixture.budget.max_latency_ms,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    }
}

fn measurement_budget(fixture: &Fixture) -> CompressionBudget {
    CompressionBudget {
        // Eval owns the wall-clock measurement and release gate. Let the
        // algorithm finish so a slow sample is measured instead of being
        // censored into a timing-dependent fail-open result.
        max_latency_ms: u64::MAX,
        ..compression_budget(fixture)
    }
}

fn evaluate_atlas(fixture: &Fixture, source: &Path) -> FixtureOutcome {
    let Some(expectations) = fixture.expectations.atlas.as_ref() else {
        return atlas_outcome(
            fixture,
            0,
            None,
            vec![ExpectationFailure::RunnerError {
                stage: RunnerStage::ValidateFixture,
            }],
        );
    };
    let started = Instant::now();
    let evaluated = run_atlas(source);
    let duration_ms = milliseconds(started.elapsed().as_millis());
    let (atlas, failures) = match evaluated {
        Ok((report, edges)) => {
            let failures = evaluate_atlas_expectations(expectations, &report, &edges);
            (
                Some(AtlasMetrics {
                    files_indexed: report.files_indexed,
                    symbols: report.symbols,
                    resolved_edges: edges.len(),
                }),
                failures,
            )
        }
        Err(stage) => (None, vec![ExpectationFailure::RunnerError { stage }]),
    };

    atlas_outcome(fixture, duration_ms, atlas, failures)
}

fn atlas_outcome(
    fixture: &Fixture,
    duration_ms: u64,
    atlas: Option<AtlasMetrics>,
    failures: Vec<ExpectationFailure>,
) -> FixtureOutcome {
    FixtureOutcome {
        id: fixture.name.clone(),
        kind: EvaluationKind::Atlas,
        eligible: fixture.eligible,
        mode: fixture.mode,
        before_tokens: 0,
        after_tokens: 0,
        duration_ms,
        reduction_pct: 0.0,
        fallback_used: false,
        fallback_reason: None,
        protected_mutations: 0,
        malformed_json: 0,
        passed: failures.is_empty(),
        failures,
        atlas,
        output: None,
    }
}

fn run_atlas(source: &Path) -> Result<(IndexReport, Vec<ResolvedEdgeRecord>), RunnerStage> {
    let temporary = TemporaryDirectory::new().map_err(|_| RunnerStage::PrepareRepository)?;
    let repository = temporary.path.join("repository");
    copy_tree(source, &repository).map_err(|_| RunnerStage::CopyRepository)?;
    let store = Store::open(&temporary.path.join("eval.db")).map_err(|_| RunnerStage::OpenStore)?;
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let report = Indexer::new(&store, counter)
        .index(&repository, |_| {})
        .map_err(|_| RunnerStage::IndexRepository)?;
    let repo = store
        .approve_repo(&repository)
        .map_err(|_| RunnerStage::LoadRepository)?;
    let edges = store
        .resolved_edges(repo)
        .map_err(|_| RunnerStage::ReadEdges)?;
    Ok((report, edges))
}

fn evaluate_atlas_expectations(
    expectations: &AtlasExpectations,
    report: &IndexReport,
    edges: &[ResolvedEdgeRecord],
) -> Vec<ExpectationFailure> {
    let mut failures = Vec::new();
    if report.files_indexed < expectations.min_files {
        failures.push(ExpectationFailure::AtlasMinimumFiles {
            expected: expectations.min_files,
            actual: report.files_indexed,
        });
    }
    if report.symbols < expectations.min_symbols {
        failures.push(ExpectationFailure::AtlasMinimumSymbols {
            expected: expectations.min_symbols,
            actual: report.symbols,
        });
    }
    for required in &expectations.required_edges {
        let found = edges.iter().any(|edge| {
            edge.from_path == required.from_path
                && edge.to_path == required.to_path
                && edge_kind_name(edge.kind) == required.kind
        });
        if !found {
            failures.push(ExpectationFailure::AtlasMissingEdge {
                from_path: required.from_path.clone(),
                to_path: required.to_path.clone(),
                edge_kind: required.kind.clone(),
            });
        }
    }
    failures
}

fn edge_kind_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Imports => "imports",
        EdgeKind::References => "references",
        EdgeKind::Tests => "tests",
    }
}

fn protected_mutations(fixture: &Fixture, output: &ContextGraph) -> Vec<String> {
    fixture
        .graph
        .nodes
        .iter()
        .filter(|node| {
            node.protected
                || node.kind.requires_protection()
                || fixture.expectations.protected_node_ids.contains(&node.id)
        })
        .filter(|original| {
            output
                .nodes
                .iter()
                .find(|candidate| candidate.id == original.id)
                .is_none_or(|candidate| candidate.content != original.content)
        })
        .map(|node| node.id.clone())
        .collect()
}

fn malformed_json_nodes(original: &ContextGraph, output: &ContextGraph) -> Vec<String> {
    original
        .nodes
        .iter()
        .filter(|node| serde_json::from_str::<serde_json::Value>(&node.content).is_ok())
        .filter_map(|node| {
            output
                .nodes
                .iter()
                .find(|candidate| candidate.id == node.id)
                .filter(|candidate| {
                    serde_json::from_str::<serde_json::Value>(&candidate.content).is_err()
                        && !valid_json_duplicate_stub(original, output, node, candidate)
                })
                .map(|_| node.id.clone())
        })
        .collect()
}

fn valid_json_duplicate_stub(
    original: &ContextGraph,
    output: &ContextGraph,
    original_stub: &compresso_core::ContextNode,
    candidate_stub: &compresso_core::ContextNode,
) -> bool {
    let mut references = output
        .edges
        .iter()
        .filter(|edge| edge.kind == ContextEdgeKind::DuplicateOf && edge.from == candidate_stub.id);
    let Some(reference) = references.next() else {
        return false;
    };
    if references.next().is_some()
        || candidate_stub.content.trim_end_matches(['\r', '\n'])
            != format!("[duplicate of node {}]", reference.to)
    {
        return false;
    }

    let original_referent = original.nodes.iter().find(|node| node.id == reference.to);
    let output_referent = output.nodes.iter().find(|node| node.id == reference.to);
    original_referent
        .zip(output_referent)
        .is_some_and(|(original_referent, output_referent)| {
            original_stub.content == original_referent.content
                && output_referent.content == original_referent.content
        })
}

fn stable_result(mut result: CompressionResult) -> CompressionResult {
    for strategy in &mut result.strategy_results {
        strategy.duration_ms = 0.0;
    }
    result.trace.operation_id.clear();
    result.trace.created_at = 0;
    for strategy in &mut result.trace.strategy_results {
        strategy.duration_ms = 0.0;
    }
    result
}

fn concatenated_output(graph: &ContextGraph) -> String {
    graph
        .nodes
        .iter()
        .map(|node| node.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn reduction_percent(before_tokens: usize, after_tokens: usize) -> f64 {
    if before_tokens == 0 {
        return 0.0;
    }
    let saved = before_tokens.saturating_sub(after_tokens);
    saved as f64 / before_tokens as f64 * 100.0
}

fn aggregate_metrics(fixtures: &[FixtureOutcome]) -> AggregateMetrics {
    aggregate_metrics_for(fixtures, EvaluationKind::Compression)
}

/// Aggregates the rows of `kind`; safety counters always cover every row.
pub(crate) fn aggregate_metrics_for(
    fixtures: &[FixtureOutcome],
    kind: EvaluationKind,
) -> AggregateMetrics {
    let compression = fixtures
        .iter()
        .filter(|outcome| outcome.kind == kind)
        .collect::<Vec<_>>();
    let mut eligible_reductions = compression
        .iter()
        .filter(|outcome| outcome.eligible)
        .map(|outcome| outcome.reduction_pct / 100.0)
        .collect::<Vec<_>>();
    eligible_reductions.sort_by(f64::total_cmp);
    let median_reduction_eligible = median(&eligible_reductions);
    let mut latencies = compression
        .iter()
        .map(|outcome| outcome.duration_ms)
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    let p95_latency_ms = percentile_95(&latencies);
    let fallback_count = compression
        .iter()
        .filter(|outcome| outcome.fallback_used)
        .count();
    let fallback_rate = if compression.is_empty() {
        0.0
    } else {
        fallback_count as f64 / compression.len() as f64
    };

    AggregateMetrics {
        compression_fixtures: compression.len(),
        eligible_fixtures: eligible_reductions.len(),
        median_reduction_eligible,
        p95_latency_ms,
        protected_mutations: fixtures
            .iter()
            .map(|outcome| outcome.protected_mutations)
            .sum(),
        malformed_json: fixtures.iter().map(|outcome| outcome.malformed_json).sum(),
        fallback_rate,
    }
}

fn median(sorted: &[f64]) -> f64 {
    match sorted.len() {
        0 => 0.0,
        length if length % 2 == 1 => sorted[length / 2],
        length => (sorted[length / 2 - 1] + sorted[length / 2]) / 2.0,
    }
}

fn percentile_95(sorted: &[u64]) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * 95).div_ceil(100);
    sorted[rank.saturating_sub(1)]
}

fn milliseconds(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn panic_outcome(fixture: &Fixture) -> FixtureOutcome {
    let kind = if matches!(fixture.input, FixtureInput::Repository { .. }) {
        EvaluationKind::Atlas
    } else {
        EvaluationKind::Compression
    };
    FixtureOutcome {
        id: fixture.name.clone(),
        kind,
        eligible: fixture.eligible,
        mode: fixture.mode,
        before_tokens: 0,
        after_tokens: 0,
        duration_ms: 0,
        reduction_pct: 0.0,
        fallback_used: false,
        fallback_reason: None,
        protected_mutations: 0,
        malformed_json: 0,
        passed: false,
        failures: vec![ExpectationFailure::RunnerPanic],
        atlas: None,
        output: None,
    }
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn new() -> io::Result<Self> {
        for _ in 0..100 {
            let suffix = TEMPORARY_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("compresso-eval-{suffix}"));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve an evaluation directory",
        ))
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir(destination)?;
    let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), target)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "repository fixture contains a non-regular entry",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use compresso_core::{ContextEdge, ContextNode, NodeKind, Source};
    use serde_json::Map;

    use super::*;

    fn json_node(id: &str, content: &str) -> ContextNode {
        let counter = counter_for(TokenProvider::Heuristic);
        ContextNode {
            id: id.to_owned(),
            kind: NodeKind::ToolResponse,
            content: content.to_owned(),
            source: Source::Synthetic,
            token_count: counter.count(content),
            token_provider: counter.provider(),
            priority: 0.5,
            protected: false,
            recency: None,
            metadata: Map::new(),
            provider_turn: None,
        }
    }

    fn graph(nodes: Vec<ContextNode>, edges: Vec<ContextEdge>) -> ContextGraph {
        ContextGraph {
            schema_version: 1,
            nodes,
            edges,
        }
    }

    fn duplicate_edge(from: &str, to: &str) -> ContextEdge {
        ContextEdge {
            from: from.to_owned(),
            to: to.to_owned(),
            kind: ContextEdgeKind::DuplicateOf,
            weight: 1.0,
        }
    }

    #[test]
    fn test_malformed_json_detector_accepts_only_resolved_byte_preserved_duplicate_stub() {
        let json = r#"{"value":"original"}"#;
        let original = graph(
            vec![json_node("referent", json), json_node("stub", json)],
            Vec::new(),
        );
        let output = graph(
            vec![
                json_node("referent", json),
                json_node("stub", "[duplicate of node referent]"),
            ],
            vec![duplicate_edge("stub", "referent")],
        );

        assert!(malformed_json_nodes(&original, &output).is_empty());
    }

    #[test]
    fn test_malformed_json_detector_rejects_orphan_duplicate_stub() {
        let json = r#"{"value":"original"}"#;
        let original = graph(
            vec![json_node("referent", json), json_node("stub", json)],
            Vec::new(),
        );
        let output = graph(
            vec![
                json_node("referent", json),
                json_node("stub", "[duplicate of node referent]"),
            ],
            Vec::new(),
        );

        assert_eq!(malformed_json_nodes(&original, &output), ["stub"]);
    }

    #[test]
    fn test_malformed_json_detector_rejects_marker_edge_mismatch() {
        let json = r#"{"value":"original"}"#;
        let original = graph(
            vec![
                json_node("referent", json),
                json_node("other", json),
                json_node("stub", json),
            ],
            Vec::new(),
        );
        let output = graph(
            vec![
                json_node("referent", json),
                json_node("other", json),
                json_node("stub", "[duplicate of node referent]"),
            ],
            vec![duplicate_edge("stub", "other")],
        );

        assert_eq!(malformed_json_nodes(&original, &output), ["stub"]);
    }

    #[test]
    fn test_malformed_json_detector_rejects_changed_referent() {
        let json = r#"{"value":"original"}"#;
        let original = graph(
            vec![json_node("referent", json), json_node("stub", json)],
            Vec::new(),
        );
        let output = graph(
            vec![
                json_node("referent", r#"{"value":"changed"}"#),
                json_node("stub", "[duplicate of node referent]"),
            ],
            vec![duplicate_edge("stub", "referent")],
        );

        assert_eq!(malformed_json_nodes(&original, &output), ["stub"]);
    }

    #[test]
    fn test_median_is_ratio_and_handles_even_populations() {
        assert_eq!(median(&[]), 0.0);
        assert_eq!(median(&[0.2]), 0.2);
        assert!((median(&[0.2, 0.4]) - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn test_p95_uses_nearest_rank() {
        assert_eq!(percentile_95(&[]), 0);
        assert_eq!(percentile_95(&[7]), 7);
        assert_eq!(percentile_95(&(1..=20).collect::<Vec<_>>()), 19);
    }

    #[test]
    fn test_atlas_expectations_match_exact_resolved_edge_identity() {
        let expectations = AtlasExpectations {
            min_files: 1,
            min_symbols: 1,
            required_edges: vec![crate::fixtures::RequiredEdge {
                from_path: "src/main.rs".to_owned(),
                to_path: "src/session.rs".to_owned(),
                kind: "imports".to_owned(),
            }],
        };
        let report = IndexReport {
            files_indexed: 1,
            files_removed: 0,
            files_skipped: 0,
            symbols: 1,
            imports_resolved: 1,
            imports_unresolved: 0,
            duration_ms: 1,
            stale: false,
        };
        let edges = vec![ResolvedEdgeRecord {
            from_path: "src/main.rs".to_owned(),
            to_path: "src/session.rs".to_owned(),
            kind: EdgeKind::Imports,
        }];

        assert!(evaluate_atlas_expectations(&expectations, &report, &edges).is_empty());
        let wrong_target = vec![ResolvedEdgeRecord {
            from_path: "src/main.rs".to_owned(),
            to_path: "src/other.rs".to_owned(),
            kind: EdgeKind::Imports,
        }];
        assert!(matches!(
            evaluate_atlas_expectations(&expectations, &report, &wrong_target).as_slice(),
            [ExpectationFailure::AtlasMissingEdge { .. }]
        ));
    }
}
