use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::graph::{ContextGraph, NodeKind};
use crate::strategies::{CollapseLogs, DedupExact};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Safe,
    Balanced,
    Aggressive,
    Passthrough,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lossiness {
    None,
    Structural,
    Summarizing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompressionBudget {
    pub max_tokens: usize,
    pub max_latency_ms: u64,
    pub allowed_lossiness: Lossiness,
    pub protected_kinds: Vec<NodeKind>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub tokens_saved: usize,
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalKind {
    DuplicateReference,
    CollapsedLines,
    JsonElision,
    DroppedNode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalRecord {
    pub strategy_id: String,
    pub node_id: String,
    pub kind: RemovalKind,
    pub locator: Option<String>,
    pub original_hash: String,
    pub replacement_hash: Option<String>,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub units_removed: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyOutput {
    pub graph: ContextGraph,
    pub notes: Vec<String>,
    pub removals: Vec<RemovalRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StrategyError {
    #[error("strategy {strategy_id} is not implemented")]
    NotImplemented { strategy_id: &'static str },
}

pub trait CompressionStrategy: Send + Sync {
    fn id(&self) -> &'static str;
    fn supports(&self, input: &ContextGraph) -> bool;
    fn estimate(&self, input: &ContextGraph) -> Estimate;
    fn execute(
        &self,
        input: &ContextGraph,
        budget: &CompressionBudget,
    ) -> Result<StrategyOutput, StrategyError>;
}

struct StubStrategy {
    id: &'static str,
}

impl StubStrategy {
    const fn new(id: &'static str) -> Self {
        Self { id }
    }
}

impl CompressionStrategy for StubStrategy {
    fn id(&self) -> &'static str {
        self.id
    }

    fn supports(&self, _input: &ContextGraph) -> bool {
        false
    }

    fn estimate(&self, _input: &ContextGraph) -> Estimate {
        Estimate::default()
    }

    fn execute(
        &self,
        _input: &ContextGraph,
        _budget: &CompressionBudget,
    ) -> Result<StrategyOutput, StrategyError> {
        Err(StrategyError::NotImplemented {
            strategy_id: self.id,
        })
    }
}

pub struct StrategyRegistry {
    strategies: Vec<Box<dyn CompressionStrategy>>,
}

impl StrategyRegistry {
    pub fn safe_default(mode: Mode) -> Self {
        let strategies: Vec<Box<dyn CompressionStrategy>> = match mode {
            Mode::Safe => vec![
                Box::new(DedupExact),
                Box::new(CollapseLogs),
                Box::new(StubStrategy::new("compact_json")),
                Box::new(StubStrategy::new("pack_budget")),
            ],
            Mode::Balanced | Mode::Aggressive | Mode::Passthrough => Vec::new(),
        };
        Self { strategies }
    }

    pub fn strategies(&self) -> &[Box<dyn CompressionStrategy>] {
        &self.strategies
    }

    pub fn permitted(&self, lossiness: Lossiness) -> Vec<&dyn CompressionStrategy> {
        self.strategies
            .iter()
            .map(Box::as_ref)
            .filter(|strategy| required_lossiness(strategy.id()) <= lossiness)
            .collect()
    }
}

fn required_lossiness(strategy_id: &str) -> Lossiness {
    match strategy_id {
        "dedup_exact" => Lossiness::None,
        "collapse_logs" | "compact_json" | "pack_budget" => Lossiness::Structural,
        _ => Lossiness::Summarizing,
    }
}

#[cfg(test)]
struct NoopStrategy;

#[cfg(test)]
impl CompressionStrategy for NoopStrategy {
    fn id(&self) -> &'static str {
        "noop"
    }

    fn supports(&self, _input: &ContextGraph) -> bool {
        true
    }

    fn estimate(&self, _input: &ContextGraph) -> Estimate {
        Estimate::default()
    }

    fn execute(
        &self,
        input: &ContextGraph,
        _budget: &CompressionBudget,
    ) -> Result<StrategyOutput, StrategyError> {
        Ok(StrategyOutput {
            graph: input.clone(),
            notes: Vec::new(),
            removals: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_graph() -> ContextGraph {
        ContextGraph {
            schema_version: 1,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    fn test_budget() -> CompressionBudget {
        CompressionBudget {
            max_tokens: 100,
            max_latency_ms: 200,
            allowed_lossiness: Lossiness::Structural,
            protected_kinds: Vec::new(),
        }
    }

    fn strategy_ids(strategies: Vec<&dyn CompressionStrategy>) -> Vec<&'static str> {
        strategies
            .into_iter()
            .map(CompressionStrategy::id)
            .collect()
    }

    #[test]
    fn test_safe_registry_order() {
        let registry = StrategyRegistry::safe_default(Mode::Safe);
        let ids = registry
            .strategies()
            .iter()
            .map(|strategy| strategy.id())
            .collect::<Vec<_>>();

        assert_eq!(
            ids,
            [
                "dedup_exact",
                "collapse_logs",
                "compact_json",
                "pack_budget"
            ]
        );
        assert!(
            registry
                .strategies()
                .iter()
                .all(|strategy| !strategy.supports(&empty_graph()))
        );
    }

    #[test]
    fn test_passthrough_registry_empty() {
        assert!(
            StrategyRegistry::safe_default(Mode::Passthrough)
                .strategies()
                .is_empty()
        );
    }

    #[test]
    fn test_unimplemented_modes_have_empty_registries() {
        for mode in [Mode::Balanced, Mode::Aggressive] {
            assert!(StrategyRegistry::safe_default(mode).strategies().is_empty());
        }
    }

    #[test]
    fn test_strategy_object_safety() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Box<dyn CompressionStrategy>>();

        let strategy: Box<dyn CompressionStrategy> = Box::new(NoopStrategy);
        assert_eq!(strategy.id(), "noop");
    }

    #[test]
    fn test_lossiness_none_permits_dedup_only() {
        let registry = StrategyRegistry::safe_default(Mode::Safe);

        assert_eq!(
            strategy_ids(registry.permitted(Lossiness::None)),
            ["dedup_exact"]
        );
        assert_eq!(
            strategy_ids(registry.permitted(Lossiness::Structural)),
            [
                "dedup_exact",
                "collapse_logs",
                "compact_json",
                "pack_budget"
            ]
        );
        assert_eq!(
            strategy_ids(registry.permitted(Lossiness::Summarizing)),
            [
                "dedup_exact",
                "collapse_logs",
                "compact_json",
                "pack_budget"
            ]
        );
    }

    #[test]
    fn test_remaining_stubs_return_typed_not_implemented_error() {
        let registry = StrategyRegistry::safe_default(Mode::Safe);
        for strategy in registry.strategies().iter().skip(2) {
            assert_eq!(
                strategy.execute(&empty_graph(), &test_budget()),
                Err(StrategyError::NotImplemented {
                    strategy_id: strategy.id(),
                })
            );
        }
    }

    #[test]
    fn test_noop_strategy_returns_unchanged_graph() {
        let graph = empty_graph();
        let output = NoopStrategy
            .execute(&graph, &test_budget())
            .expect("noop strategy should execute");

        assert_eq!(NoopStrategy.estimate(&graph), Estimate::default());
        assert_eq!(output.graph, graph);
        assert!(output.notes.is_empty());
        assert!(output.removals.is_empty());
    }

    #[test]
    fn test_strategy_domain_enum_tags_match_trace_schema() {
        let mode_tags = [
            (Mode::Safe, "\"safe\""),
            (Mode::Balanced, "\"balanced\""),
            (Mode::Aggressive, "\"aggressive\""),
            (Mode::Passthrough, "\"passthrough\""),
        ];
        let lossiness_tags = [
            (Lossiness::None, "\"none\""),
            (Lossiness::Structural, "\"structural\""),
            (Lossiness::Summarizing, "\"summarizing\""),
        ];
        let removal_tags = [
            (RemovalKind::DuplicateReference, "\"duplicate_reference\""),
            (RemovalKind::CollapsedLines, "\"collapsed_lines\""),
            (RemovalKind::JsonElision, "\"json_elision\""),
            (RemovalKind::DroppedNode, "\"dropped_node\""),
        ];

        for (value, expected) in mode_tags {
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        }
        for (value, expected) in lossiness_tags {
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        }
        for (value, expected) in removal_tags {
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        }
    }
}
