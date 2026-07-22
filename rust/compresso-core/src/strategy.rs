use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::graph::{ContextGraph, NodeKind};
use crate::strategies::{CollapseLogs, CompactJson, DedupExact, PackBudget};

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

/// Optional single-pass result for strategies whose planning is expensive.
///
/// The default [`CompressionStrategy::execute_if_supported`] implementation
/// returns `Legacy`, preserving the original `supports` → `estimate` →
/// `execute` engine contract for existing implementations. Strategies may
/// override the hook to plan once and return either `NotSupported` or the
/// exact estimate and execution result derived from that plan.
#[derive(Debug, Clone, PartialEq)]
pub enum StrategyExecution {
    Legacy,
    NotSupported,
    Executed {
        estimate: Estimate,
        output: Result<StrategyOutput, StrategyError>,
    },
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

    fn execute_if_supported(
        &self,
        _input: &ContextGraph,
        _budget: &CompressionBudget,
    ) -> StrategyExecution {
        StrategyExecution::Legacy
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
                Box::new(CompactJson),
                Box::new(PackBudget),
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

    #[cfg(test)]
    pub(crate) fn from_strategies(strategies: Vec<Box<dyn CompressionStrategy>>) -> Self {
        Self { strategies }
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
    fn test_noop_strategy_returns_unchanged_graph() {
        let graph = empty_graph();
        let output = NoopStrategy
            .execute(&graph, &test_budget())
            .expect("noop strategy should execute");

        assert_eq!(
            NoopStrategy.execute_if_supported(&graph, &test_budget()),
            StrategyExecution::Legacy
        );
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
