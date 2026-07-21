//! Deterministic context graph, token accounting, compression, and safety primitives.

pub mod graph;
pub mod safety;
pub mod strategies;
pub mod strategy;
pub mod tokens;

pub use graph::{
    ContextEdge, ContextGraph, ContextNode, EdgeKind, GraphError, NodeKind, ProviderTurn, Source,
};
pub use safety::{SafetyCheck, run_all, run_all_with_removals};
pub use strategies::{CollapseLogs, CompactJson, DedupExact, PackBudget};
pub use strategy::{
    CompressionBudget, CompressionStrategy, Estimate, Lossiness, Mode, RemovalKind, RemovalRecord,
    StrategyError, StrategyOutput, StrategyRegistry,
};
pub use tokens::{CALIBRATION, TokenCounter, TokenProvider, counter_for};
