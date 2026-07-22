use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::graph::ContextGraph;
use crate::safety::{
    SafetyCheck, identity_checks, run_all, run_all_with_removals, run_preflight_after_recount,
};
use crate::strategy::{
    CompressionBudget, Mode, RemovalKind, RemovalRecord, StrategyExecution, StrategyRegistry,
};
use crate::tokens::{TokenCounter, TokenProvider, counter_for};
use crate::trace::{StrategyResult, Trace};

#[cfg(test)]
use crate::strategy::CompressionStrategy;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompressionResult {
    pub output: ContextGraph,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub strategy_results: Vec<StrategyResult>,
    pub safety_checks: Vec<SafetyCheck>,
    pub fallback_used: bool,
    pub fallback_reason: Option<String>,
    pub trace: Trace,
}

pub struct Engine {
    mode: Mode,
    counter: &'static dyn TokenCounter,
    registry: StrategyRegistry,
}

impl Engine {
    pub fn new(mode: Mode, counter: &'static dyn TokenCounter) -> Self {
        Self {
            mode,
            counter,
            registry: StrategyRegistry::safe_default(mode),
        }
    }

    /// Compresses `input` without exposing a fallible API.
    ///
    /// Every rejected candidate, strategy failure, and caught panic restores
    /// the exact caller-owned graph supplied to this method.
    pub fn compress(&self, input: ContextGraph, budget: &CompressionBudget) -> CompressionResult {
        let original = input;
        catch_unwind(AssertUnwindSafe(|| {
            self.compress_inner(original.clone(), budget)
        }))
        .unwrap_or_else(|_| {
            let before_tokens = recounted_total(&original);
            let safety_checks = run_all(&original, &original, budget, 0);
            self.fallback(
                original,
                before_tokens,
                Vec::new(),
                safety_checks,
                "engine_panic".to_owned(),
            )
        })
    }

    fn compress_inner(
        &self,
        original: ContextGraph,
        budget: &CompressionBudget,
    ) -> CompressionResult {
        let started = Instant::now();

        if self.mode == Mode::Passthrough {
            let before_tokens = original.total_tokens();
            return self.complete(
                original,
                before_tokens,
                before_tokens,
                Vec::new(),
                identity_checks(),
                false,
                None,
                "Passthrough mode.".to_owned(),
                None,
            );
        }

        let mut ingress = original.clone();
        recount_graph(&mut ingress);
        force_protection(&mut ingress, budget);
        let before_tokens = ingress.total_tokens();
        let ingress_validation = ingress.validate();
        let preflight_checks =
            run_preflight_after_recount(&ingress, budget, elapsed_ms(started.elapsed()));

        if budget.max_tokens == 0 || budget.max_latency_ms == 0 {
            return self.fallback(
                original,
                before_tokens,
                Vec::new(),
                preflight_checks,
                "validation:invalid_budget".to_owned(),
            );
        }
        if ingress_validation.is_err() {
            return self.fallback(
                original,
                before_tokens,
                Vec::new(),
                preflight_checks,
                "validation:context_graph".to_owned(),
            );
        }
        if let Some(failed) = first_failed_check(&preflight_checks) {
            let reason = format!("safety_check_failed:{failed}");
            return self.fallback(
                original,
                before_tokens,
                Vec::new(),
                preflight_checks,
                reason,
            );
        }

        if matches!(self.mode, Mode::Balanced | Mode::Aggressive) {
            let reason = format!("unsupported_mode:{}", mode_name(self.mode));
            return self.fallback(
                original,
                before_tokens,
                Vec::new(),
                preflight_checks,
                reason,
            );
        }

        let mut current = ingress.clone();
        let mut strategy_results = Vec::new();
        let mut safety_ledger = Vec::new();

        for strategy in self.registry.permitted(budget.allowed_lossiness) {
            if elapsed_ms(started.elapsed()) > budget.max_latency_ms {
                let safety_checks = run_all_with_removals(
                    &ingress,
                    &current,
                    budget,
                    elapsed_ms(started.elapsed()),
                    &safety_ledger,
                );
                return self.fallback(
                    original,
                    before_tokens,
                    strategy_results,
                    safety_checks,
                    "latency_budget_exceeded".to_owned(),
                );
            }

            let strategy_id = match catch_unwind(AssertUnwindSafe(|| strategy.id())) {
                Ok(strategy_id) => strategy_id,
                Err(_) => {
                    let safety_checks = run_all_with_removals(
                        &ingress,
                        &current,
                        budget,
                        elapsed_ms(started.elapsed()),
                        &safety_ledger,
                    );
                    return self.fallback(
                        original,
                        before_tokens,
                        strategy_results,
                        safety_checks,
                        "strategy_panic:unknown_strategy".to_owned(),
                    );
                }
            };
            let strategy_started = Instant::now();
            let before = current.total_tokens();

            let combined = match catch_unwind(AssertUnwindSafe(|| {
                strategy.execute_if_supported(&current, budget)
            })) {
                Ok(combined) => combined,
                Err(_) => {
                    strategy_results.push(failed_strategy_result(
                        strategy_id,
                        before,
                        strategy_started.elapsed(),
                        None,
                        "strategy panicked",
                    ));
                    let safety_checks = run_all_with_removals(
                        &ingress,
                        &current,
                        budget,
                        elapsed_ms(started.elapsed()),
                        &safety_ledger,
                    );
                    return self.fallback(
                        original,
                        before_tokens,
                        strategy_results,
                        safety_checks,
                        format!("strategy_panic:{strategy_id}"),
                    );
                }
            };
            let (estimate, output) = match combined {
                StrategyExecution::Legacy => {
                    let supported =
                        match catch_unwind(AssertUnwindSafe(|| strategy.supports(&current))) {
                            Ok(supported) => supported,
                            Err(_) => {
                                strategy_results.push(failed_strategy_result(
                                    strategy_id,
                                    before,
                                    strategy_started.elapsed(),
                                    None,
                                    "strategy panicked",
                                ));
                                let safety_checks = run_all_with_removals(
                                    &ingress,
                                    &current,
                                    budget,
                                    elapsed_ms(started.elapsed()),
                                    &safety_ledger,
                                );
                                return self.fallback(
                                    original,
                                    before_tokens,
                                    strategy_results,
                                    safety_checks,
                                    format!("strategy_panic:{strategy_id}"),
                                );
                            }
                        };
                    if !supported {
                        strategy_results.push(not_supported_strategy_result(
                            strategy_id,
                            before,
                            strategy_started.elapsed(),
                        ));
                        continue;
                    }

                    let estimate =
                        match catch_unwind(AssertUnwindSafe(|| strategy.estimate(&current))) {
                            Ok(estimate) => estimate,
                            Err(_) => {
                                strategy_results.push(failed_strategy_result(
                                    strategy_id,
                                    before,
                                    strategy_started.elapsed(),
                                    None,
                                    "strategy panicked",
                                ));
                                let safety_checks = run_all_with_removals(
                                    &ingress,
                                    &current,
                                    budget,
                                    elapsed_ms(started.elapsed()),
                                    &safety_ledger,
                                );
                                return self.fallback(
                                    original,
                                    before_tokens,
                                    strategy_results,
                                    safety_checks,
                                    format!("strategy_panic:{strategy_id}"),
                                );
                            }
                        };

                    let output =
                        match catch_unwind(AssertUnwindSafe(|| strategy.execute(&current, budget)))
                        {
                            Ok(output) => output,
                            Err(_) => {
                                strategy_results.push(failed_strategy_result(
                                    strategy_id,
                                    before,
                                    strategy_started.elapsed(),
                                    Some(estimate.confidence),
                                    "strategy panicked",
                                ));
                                let safety_checks = run_all_with_removals(
                                    &ingress,
                                    &current,
                                    budget,
                                    elapsed_ms(started.elapsed()),
                                    &safety_ledger,
                                );
                                return self.fallback(
                                    original,
                                    before_tokens,
                                    strategy_results,
                                    safety_checks,
                                    format!("strategy_panic:{strategy_id}"),
                                );
                            }
                        };
                    (estimate, output)
                }
                StrategyExecution::NotSupported => {
                    strategy_results.push(not_supported_strategy_result(
                        strategy_id,
                        before,
                        strategy_started.elapsed(),
                    ));
                    continue;
                }
                StrategyExecution::Executed { estimate, output } => (estimate, output),
            };

            let output = match output {
                Ok(output) => output,
                Err(_) => {
                    strategy_results.push(failed_strategy_result(
                        strategy_id,
                        before,
                        strategy_started.elapsed(),
                        Some(estimate.confidence),
                        "strategy returned an error",
                    ));
                    let safety_checks = run_all_with_removals(
                        &ingress,
                        &current,
                        budget,
                        elapsed_ms(started.elapsed()),
                        &safety_ledger,
                    );
                    return self.fallback(
                        original,
                        before_tokens,
                        strategy_results,
                        safety_checks,
                        format!("strategy_error:{strategy_id}"),
                    );
                }
            };

            let after = output.graph.total_tokens();
            let applied = output.graph != current;
            let notes = redact_leaking_notes(output.notes, &ingress);
            let removals = output.removals;
            extend_safety_ledger(&mut safety_ledger, &removals, &ingress);
            strategy_results.push(StrategyResult {
                strategy_id: strategy_id.to_owned(),
                applied,
                before_tokens: before,
                after_tokens: after,
                duration_ms: duration_ms(strategy_started.elapsed()),
                confidence: Some(estimate.confidence),
                notes,
                removals,
                skipped_reason: (!applied).then(|| "no_change".to_owned()),
            });
            current = output.graph;
        }

        let elapsed = elapsed_ms(started.elapsed());
        let safety_checks =
            run_all_with_removals(&ingress, &current, budget, elapsed, &safety_ledger);
        if let Some(failed) = first_failed_check(&safety_checks) {
            let reason = format!("safety_check_failed:{failed}");
            return self.fallback(
                original,
                before_tokens,
                strategy_results,
                safety_checks,
                reason,
            );
        }

        let after_tokens = current.total_tokens();
        self.complete(
            current,
            before_tokens,
            after_tokens,
            strategy_results,
            safety_checks,
            false,
            None,
            "Compression done.".to_owned(),
            Some(&ingress),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn complete(
        &self,
        output: ContextGraph,
        before_tokens: usize,
        after_tokens: usize,
        strategy_results: Vec<StrategyResult>,
        safety_checks: Vec<SafetyCheck>,
        fallback_used: bool,
        fallback_reason: Option<String>,
        explanation: String,
        trace_source: Option<&ContextGraph>,
    ) -> CompressionResult {
        let (trace_strategy_results, trace_safety_checks, trace_fallback_reason, trace_explanation) =
            if let Some(trace_source) = trace_source {
                (
                    sanitize_strategy_results(&strategy_results, trace_source),
                    sanitize_safety_checks(&safety_checks, trace_source),
                    fallback_reason
                        .as_deref()
                        .map(|reason| sanitize_trace_text(reason, trace_source)),
                    sanitize_trace_text(&explanation, trace_source),
                )
            } else {
                (
                    strategy_results.clone(),
                    safety_checks.clone(),
                    fallback_reason.clone(),
                    explanation,
                )
            };
        let trace = Trace::new(
            self.mode,
            before_tokens,
            after_tokens,
            configured_provider(self.counter),
            fallback_used,
            trace_fallback_reason,
            trace_strategy_results,
            trace_safety_checks,
            trace_explanation,
        );
        CompressionResult {
            output,
            before_tokens,
            after_tokens,
            strategy_results,
            safety_checks,
            fallback_used,
            fallback_reason,
            trace,
        }
    }

    fn fallback(
        &self,
        original: ContextGraph,
        before_tokens: usize,
        strategy_results: Vec<StrategyResult>,
        safety_checks: Vec<SafetyCheck>,
        reason: String,
    ) -> CompressionResult {
        self.complete(
            original.clone(),
            before_tokens,
            before_tokens,
            strategy_results,
            safety_checks,
            true,
            Some(reason),
            "Original returned.".to_owned(),
            Some(&original),
        )
    }

    #[cfg(test)]
    fn with_strategies(
        mode: Mode,
        counter: &'static dyn TokenCounter,
        strategies: Vec<Box<dyn CompressionStrategy>>,
    ) -> Self {
        Self {
            mode,
            counter,
            registry: StrategyRegistry::from_strategies(strategies),
        }
    }
}

fn force_protection(graph: &mut ContextGraph, budget: &CompressionBudget) {
    for node in &mut graph.nodes {
        if node.kind.requires_protection() || budget.protected_kinds.contains(&node.kind) {
            node.protected = true;
        }
    }

    let protected_turns = graph
        .nodes
        .iter()
        .filter(|node| node.protected)
        .filter_map(|node| node.provider_turn.as_ref())
        .map(|turn| (turn.provider.clone(), turn.turn_id.clone()))
        .collect::<BTreeSet<_>>();
    for node in &mut graph.nodes {
        if node.provider_turn.as_ref().is_some_and(|turn| {
            protected_turns.contains(&(turn.provider.clone(), turn.turn_id.clone()))
        }) {
            node.protected = true;
        }
    }
}

fn recount_graph(graph: &mut ContextGraph) {
    for node in &mut graph.nodes {
        node.token_count = counter_for(node.token_provider).count(&node.content);
    }
}

fn recounted_total(graph: &ContextGraph) -> usize {
    graph.nodes.iter().fold(0_usize, |total, node| {
        total.saturating_add(counter_for(node.token_provider).count(&node.content))
    })
}

fn configured_provider(counter: &'static dyn TokenCounter) -> TokenProvider {
    catch_unwind(AssertUnwindSafe(|| counter.provider())).unwrap_or(TokenProvider::Heuristic)
}

fn first_failed_check(checks: &[SafetyCheck]) -> Option<&str> {
    checks
        .iter()
        .find(|check| !check.passed)
        .map(|check| check.name.as_str())
}

fn elapsed_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn not_supported_strategy_result(
    strategy_id: &str,
    tokens: usize,
    duration: Duration,
) -> StrategyResult {
    StrategyResult {
        strategy_id: strategy_id.to_owned(),
        applied: false,
        before_tokens: tokens,
        after_tokens: tokens,
        duration_ms: duration_ms(duration),
        confidence: None,
        notes: Vec::new(),
        removals: Vec::new(),
        skipped_reason: Some("not_supported".to_owned()),
    }
}

fn failed_strategy_result(
    strategy_id: &str,
    tokens: usize,
    duration: Duration,
    confidence: Option<f32>,
    skipped_reason: &str,
) -> StrategyResult {
    StrategyResult {
        strategy_id: strategy_id.to_owned(),
        applied: false,
        before_tokens: tokens,
        after_tokens: tokens,
        duration_ms: duration_ms(duration),
        confidence,
        notes: Vec::new(),
        removals: Vec::new(),
        skipped_reason: Some(skipped_reason.to_owned()),
    }
}

fn extend_safety_ledger(
    ledger: &mut Vec<RemovalRecord>,
    records: &[RemovalRecord],
    ingress: &ContextGraph,
) {
    for record in records {
        let mut record = record.clone();
        if record.kind == RemovalKind::DroppedNode
            && let Some(original) = ingress.nodes.iter().find(|node| node.id == record.node_id)
        {
            // A final whole-node drop supersedes earlier partial transformations
            // of that node. Rebase it to the ingress graph so the final ledger
            // exactly reconciles the graph delta without overlapping regions.
            ledger.retain(|existing| existing.node_id != record.node_id);
            record.original_hash = content_hash(&original.content);
            record.before_tokens = original.token_count;
        }
        ledger.push(record);
    }
}

fn redact_leaking_notes(notes: Vec<String>, ingress: &ContextGraph) -> Vec<String> {
    notes
        .into_iter()
        .map(|note| {
            if contains_input_fragment(&note, ingress, 20) {
                "[redacted strategy note]".to_owned()
            } else {
                note
            }
        })
        .collect()
}

fn sanitize_strategy_results(
    results: &[StrategyResult],
    trace_source: &ContextGraph,
) -> Vec<StrategyResult> {
    results
        .iter()
        .cloned()
        .map(|mut result| {
            result.strategy_id = sanitize_trace_text(&result.strategy_id, trace_source);
            result.notes = result
                .notes
                .into_iter()
                .map(|note| sanitize_trace_text(&note, trace_source))
                .collect();
            result.skipped_reason = result
                .skipped_reason
                .as_deref()
                .map(|reason| sanitize_trace_text(reason, trace_source));
            for removal in &mut result.removals {
                removal.strategy_id = sanitize_trace_text(&removal.strategy_id, trace_source);
                removal.node_id = sanitize_trace_text(&removal.node_id, trace_source);
                removal.locator = removal
                    .locator
                    .as_deref()
                    .map(|locator| sanitize_trace_text(locator, trace_source));
                removal.original_hash = sanitize_trace_text(&removal.original_hash, trace_source);
                removal.replacement_hash = removal
                    .replacement_hash
                    .as_deref()
                    .map(|hash| sanitize_trace_text(hash, trace_source));
            }
            result
        })
        .collect()
}

fn sanitize_safety_checks(checks: &[SafetyCheck], trace_source: &ContextGraph) -> Vec<SafetyCheck> {
    checks
        .iter()
        .cloned()
        .map(|mut check| {
            check.name = sanitize_trace_text(&check.name, trace_source);
            check.detail = sanitize_trace_text(&check.detail, trace_source);
            check
        })
        .collect()
}

fn sanitize_trace_text(value: &str, trace_source: &ContextGraph) -> String {
    if contains_input_fragment(value, trace_source, 20) {
        "[redacted]".to_owned()
    } else {
        value.to_owned()
    }
}

fn contains_input_fragment(value: &str, ingress: &ContextGraph, minimum_chars: usize) -> bool {
    let boundaries = value
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(value.len()))
        .collect::<Vec<_>>();
    if boundaries.len() <= minimum_chars {
        return false;
    }
    boundaries.windows(minimum_chars + 1).any(|window| {
        let fragment = &value[window[0]..window[minimum_chars]];
        ingress
            .nodes
            .iter()
            .any(|node| node.content.contains(fragment))
    })
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Safe => "safe",
        Mode::Balanced => "balanced",
        Mode::Aggressive => "aggressive",
        Mode::Passthrough => "passthrough",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value};

    use super::*;
    use crate::graph::{ContextNode, NodeKind, Source};
    use crate::strategy::{Estimate, Lossiness, StrategyError, StrategyOutput};

    const LOG_FIXTURE: &str = include_str!("../../../fixtures/logs/repeated-npm-install/input.txt");

    fn counter() -> &'static dyn TokenCounter {
        counter_for(TokenProvider::Heuristic)
    }

    fn node(id: &str, kind: NodeKind, content: &str, protected: bool) -> ContextNode {
        ContextNode {
            id: id.to_owned(),
            kind,
            content: content.to_owned(),
            source: Source::Synthetic,
            token_count: counter().count(content),
            token_provider: counter().provider(),
            priority: 0.5,
            protected,
            recency: None,
            metadata: Map::new(),
            provider_turn: None,
        }
    }

    fn graph(nodes: Vec<ContextNode>) -> ContextGraph {
        ContextGraph {
            schema_version: 1,
            nodes,
            edges: Vec::new(),
        }
    }

    fn budget() -> CompressionBudget {
        CompressionBudget {
            max_tokens: 2_000,
            max_latency_ms: 2_000,
            allowed_lossiness: crate::strategy::Lossiness::Summarizing,
            protected_kinds: Vec::new(),
        }
    }

    struct PanickingStrategy;

    impl CompressionStrategy for PanickingStrategy {
        fn id(&self) -> &'static str {
            "panicking_test_strategy"
        }

        fn supports(&self, _input: &ContextGraph) -> bool {
            true
        }

        fn estimate(&self, _input: &ContextGraph) -> Estimate {
            Estimate {
                tokens_saved: 1,
                confidence: 1.0,
            }
        }

        fn execute(
            &self,
            _input: &ContextGraph,
            _budget: &CompressionBudget,
        ) -> Result<StrategyOutput, StrategyError> {
            panic!("injected strategy panic")
        }
    }

    struct ProtectedMutation;

    impl CompressionStrategy for ProtectedMutation {
        fn id(&self) -> &'static str {
            "protected_mutation"
        }

        fn supports(&self, _input: &ContextGraph) -> bool {
            true
        }

        fn estimate(&self, _input: &ContextGraph) -> Estimate {
            Estimate {
                tokens_saved: 1,
                confidence: 1.0,
            }
        }

        fn execute(
            &self,
            input: &ContextGraph,
            _budget: &CompressionBudget,
        ) -> Result<StrategyOutput, StrategyError> {
            let mut graph = input.clone();
            graph.nodes[0].content = "changed".to_owned();
            graph.nodes[0].token_count = counter().count("changed");
            Ok(StrategyOutput {
                graph,
                notes: Vec::new(),
                removals: Vec::new(),
            })
        }
    }

    struct SilentDrop;

    impl CompressionStrategy for SilentDrop {
        fn id(&self) -> &'static str {
            "silent_drop"
        }

        fn supports(&self, _input: &ContextGraph) -> bool {
            true
        }

        fn estimate(&self, input: &ContextGraph) -> Estimate {
            Estimate {
                tokens_saved: input.total_tokens(),
                confidence: 1.0,
            }
        }

        fn execute(
            &self,
            input: &ContextGraph,
            _budget: &CompressionBudget,
        ) -> Result<StrategyOutput, StrategyError> {
            let mut graph = input.clone();
            graph.nodes.pop();
            Ok(StrategyOutput {
                graph,
                notes: Vec::new(),
                removals: Vec::new(),
            })
        }
    }

    struct CombinedHookStrategy;

    impl CompressionStrategy for CombinedHookStrategy {
        fn id(&self) -> &'static str {
            "combined_hook"
        }

        fn supports(&self, _input: &ContextGraph) -> bool {
            panic!("legacy supports must not run")
        }

        fn estimate(&self, _input: &ContextGraph) -> Estimate {
            panic!("legacy estimate must not run")
        }

        fn execute(
            &self,
            _input: &ContextGraph,
            _budget: &CompressionBudget,
        ) -> Result<StrategyOutput, StrategyError> {
            panic!("legacy execute must not run")
        }

        fn execute_if_supported(
            &self,
            input: &ContextGraph,
            _budget: &CompressionBudget,
        ) -> StrategyExecution {
            StrategyExecution::Executed {
                estimate: Estimate {
                    tokens_saved: 0,
                    confidence: 0.75,
                },
                output: Ok(StrategyOutput {
                    graph: input.clone(),
                    notes: vec!["combined execution used".to_owned()],
                    removals: Vec::new(),
                }),
            }
        }
    }

    #[test]
    fn test_engine_uses_combined_execution_hook_without_legacy_calls() {
        let input = graph(vec![node("n1", NodeKind::Summary, "keep this", false)]);
        let engine =
            Engine::with_strategies(Mode::Safe, counter(), vec![Box::new(CombinedHookStrategy)]);

        let result = engine.compress(input.clone(), &budget());

        assert_eq!(result.output, input);
        assert!(!result.fallback_used, "{:?}", result.fallback_reason);
        assert_eq!(result.strategy_results.len(), 1);
        assert_eq!(result.strategy_results[0].strategy_id, "combined_hook");
        assert_eq!(result.strategy_results[0].confidence, Some(0.75));
        assert_eq!(
            result.strategy_results[0].skipped_reason.as_deref(),
            Some("no_change")
        );
    }

    #[test]
    fn test_never_err_on_panicking_strategy() {
        let input = graph(vec![node("n1", NodeKind::Summary, "keep this", false)]);
        let original_json = serde_json::to_vec(&input).expect("input should serialize");
        let engine =
            Engine::with_strategies(Mode::Safe, counter(), vec![Box::new(PanickingStrategy)]);

        let result = engine.compress(input, &budget());

        assert!(result.fallback_used);
        assert!(
            result
                .fallback_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("panicking_test_strategy"))
        );
        assert_eq!(
            serde_json::to_vec(&result.output).expect("output should serialize"),
            original_json
        );
    }

    #[test]
    fn test_failed_safety_restores_original() {
        let input = graph(vec![node(
            "system",
            NodeKind::SystemInstruction,
            "protected instruction",
            true,
        )]);
        let engine =
            Engine::with_strategies(Mode::Safe, counter(), vec![Box::new(ProtectedMutation)]);

        let result = engine.compress(input.clone(), &budget());

        assert_eq!(result.output, input);
        assert!(result.fallback_used);
        assert!(
            result
                .safety_checks
                .iter()
                .any(|check| { check.name == "protected_unchanged" && !check.passed })
        );
    }

    #[test]
    fn test_removals_enumerated_failure_falls_back() {
        let input = graph(vec![
            node("keep", NodeKind::Summary, "keep this", false),
            node("drop", NodeKind::Summary, "drop this", false),
        ]);
        let engine = Engine::with_strategies(Mode::Safe, counter(), vec![Box::new(SilentDrop)]);

        let result = engine.compress(input.clone(), &budget());

        assert_eq!(result.output, input);
        assert!(result.fallback_used);
        assert!(
            result
                .fallback_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("removals_enumerated"))
        );
    }

    #[test]
    fn test_preflight_uses_fresh_ingress_recount_proof() {
        let content = "stale caller accounting is repaired before preflight";
        let mut input = graph(vec![node("summary", NodeKind::Summary, content, false)]);
        input.nodes[0].token_count = 0;
        let mut invalid_budget = budget();
        invalid_budget.max_tokens = 0;

        let result = Engine::new(Mode::Safe, counter()).compress(input.clone(), &invalid_budget);

        assert!(result.fallback_used);
        assert_eq!(result.output, input);
        assert_eq!(result.before_tokens, counter().count(content));
        let token_check = result
            .safety_checks
            .iter()
            .find(|check| check.name == "token_counts_valid")
            .expect("preflight should retain the canonical token-count check");
        assert!(token_check.passed);
        assert_eq!(
            token_check.detail,
            "ingress token counts verified by immediate provider recount"
        );
    }

    #[test]
    fn test_passthrough_identity() {
        let mut input = graph(vec![node(
            "summary",
            NodeKind::Summary,
            "caller bytes remain untouched",
            false,
        )]);
        // Passthrough deliberately trusts stored accounting. Safe mode would
        // reject this stale count during preflight, but identity needs neither
        // a recount nor a content scan.
        input.nodes[0].token_count = usize::MAX;
        let serialized_input = serde_json::to_vec(&input).expect("input serializes");
        assert!(
            run_all(&input, &input, &budget(), 0)
                .iter()
                .any(|check| check.name == "token_counts_valid" && !check.passed)
        );

        let result = Engine::new(Mode::Passthrough, counter()).compress(input.clone(), &budget());

        assert_eq!(result.output, input);
        assert_eq!(
            serde_json::to_vec(&result.output).expect("output serializes"),
            serialized_input
        );
        assert_eq!(result.before_tokens, usize::MAX);
        assert_eq!(result.after_tokens, usize::MAX);
        assert!(result.strategy_results.is_empty());
        assert!(!result.fallback_used);
        assert!(result.fallback_reason.is_none());
        let expected_names = [
            "protected_unchanged",
            "no_inflation",
            "json_still_valid",
            "fences_balanced",
            "refs_valid",
            "provider_conversation_valid",
            "minimum_output",
            "token_counts_valid",
            "latency_ok",
            "schema_valid",
            "removals_enumerated",
        ];
        assert_eq!(
            result
                .safety_checks
                .iter()
                .map(|check| check.name.as_str())
                .collect::<Vec<_>>(),
            expected_names
        );
        assert!(result.safety_checks.iter().all(|check| check.passed));
        assert_eq!(result.trace.safety_checks, result.safety_checks);
        assert_eq!(result.trace.mode, Mode::Passthrough);
    }

    #[test]
    fn test_safe_mode_on_log_fixture() {
        let input = graph(vec![node(
            "log-1",
            NodeKind::ToolResponse,
            LOG_FIXTURE,
            false,
        )]);
        let result = Engine::new(Mode::Safe, counter()).compress(input, &budget());

        assert!(!result.fallback_used, "{:?}", result.fallback_reason);
        assert!(result.after_tokens * 2 < result.before_tokens);
        assert!(
            result
                .strategy_results
                .iter()
                .any(|strategy| { strategy.strategy_id == "collapse_logs" && strategy.applied })
        );
        assert_eq!(result.safety_checks.len(), 11);
        assert!(result.safety_checks.iter().all(|check| check.passed));
    }

    #[test]
    fn test_trace_leaks_no_content() {
        let input = graph(vec![node(
            "log-1",
            NodeKind::ToolResponse,
            LOG_FIXTURE,
            false,
        )]);
        let result = Engine::new(Mode::Safe, counter()).compress(input.clone(), &budget());
        let trace = serde_json::to_string(&result.trace).expect("trace should serialize");

        for node in input.nodes {
            let boundaries = node
                .content
                .char_indices()
                .map(|(index, _)| index)
                .chain(std::iter::once(node.content.len()))
                .collect::<Vec<_>>();
            for window in boundaries.windows(21) {
                assert!(!trace.contains(&node.content[window[0]..window[20]]));
            }
        }
    }

    #[test]
    fn test_trace_redacts_content_derived_ids_locators_and_safety_detail() {
        const NODE_ID_CANARY: &str = "node-id-canary-7f4e2d9c8b1a";
        const JSON_KEY_CANARY: &str = "json-key-canary-4c8d2e7f9a1b";
        const CONTENT_CANARY: &str = "content-canary-3a7e9d2c8f1b";
        const SAFETY_DETAIL_CANARY: &str = "protected nodes are byte-identical";

        let mut object = Map::new();
        object.insert(
            "content_derived_id".to_owned(),
            Value::String(NODE_ID_CANARY.to_owned()),
        );
        object.insert(
            JSON_KEY_CANARY.to_owned(),
            Value::String(format!("{CONTENT_CANARY}{}", "x".repeat(600))),
        );
        object.insert(
            "safety_detail".to_owned(),
            Value::String(SAFETY_DETAIL_CANARY.to_owned()),
        );
        let content = serde_json::to_string(&Value::Object(object))
            .expect("adversarial content should serialize");
        let input = graph(vec![node(
            NODE_ID_CANARY,
            NodeKind::ToolResponse,
            &content,
            false,
        )]);

        let result = Engine::new(Mode::Safe, counter()).compress(input, &budget());
        assert!(!result.fallback_used, "{:?}", result.fallback_reason);
        let internal_removal = result
            .strategy_results
            .iter()
            .find(|strategy| strategy.strategy_id == "compact_json")
            .and_then(|strategy| strategy.removals.first())
            .expect("compact_json should emit an internal removal");
        assert_eq!(internal_removal.node_id, NODE_ID_CANARY);
        assert!(
            internal_removal
                .locator
                .as_deref()
                .is_some_and(|locator| locator.contains(JSON_KEY_CANARY))
        );

        let trace_json = serde_json::to_string(&result.trace).expect("trace should serialize");
        for canary in [
            NODE_ID_CANARY,
            JSON_KEY_CANARY,
            CONTENT_CANARY,
            SAFETY_DETAIL_CANARY,
        ] {
            assert!(!trace_json.contains(canary), "trace leaked {canary}");
        }
        let trace_removal = result
            .trace
            .strategy_results
            .iter()
            .find(|strategy| strategy.strategy_id == "compact_json")
            .and_then(|strategy| strategy.removals.first())
            .expect("trace should retain the typed removal");
        assert_eq!(trace_removal.kind, RemovalKind::JsonElision);
        assert_eq!(trace_removal.node_id, "[redacted]");
        assert_eq!(trace_removal.locator.as_deref(), Some("[redacted]"));
        assert!(
            result.trace.safety_checks.iter().any(|check| {
                check.name == "protected_unchanged" && check.detail == "[redacted]"
            })
        );
    }

    #[test]
    fn test_deterministic_output() {
        let input = graph(vec![node(
            "log-1",
            NodeKind::ToolResponse,
            LOG_FIXTURE,
            false,
        )]);
        let engine = Engine::new(Mode::Safe, counter());

        let first = engine.compress(input.clone(), &budget());
        let second = engine.compress(input, &budget());

        assert_eq!(
            serde_json::to_vec(&first.output).expect("first output should serialize"),
            serde_json::to_vec(&second.output).expect("second output should serialize")
        );
        assert_ne!(first.trace.operation_id, second.trace.operation_id);
    }

    #[test]
    fn test_lossiness_none_only_attempts_dedup() {
        let input = graph(vec![node(
            "log-1",
            NodeKind::ToolResponse,
            LOG_FIXTURE,
            false,
        )]);
        let mut budget = budget();
        budget.allowed_lossiness = Lossiness::None;

        let result = Engine::new(Mode::Safe, counter()).compress(input, &budget);

        assert_eq!(result.strategy_results.len(), 1);
        assert_eq!(result.strategy_results[0].strategy_id, "dedup_exact");
    }

    #[test]
    fn test_unsupported_modes_have_typed_fallback_reasons() {
        let input = graph(vec![node("n1", NodeKind::Summary, "content", false)]);
        for mode in [Mode::Balanced, Mode::Aggressive] {
            let result = Engine::new(mode, counter()).compress(input.clone(), &budget());
            assert_eq!(result.output, input);
            assert!(result.fallback_used);
            assert_eq!(
                result.fallback_reason.as_deref(),
                Some(match mode {
                    Mode::Balanced => "unsupported_mode:balanced",
                    Mode::Aggressive => "unsupported_mode:aggressive",
                    _ => unreachable!(),
                })
            );
        }
    }
}
