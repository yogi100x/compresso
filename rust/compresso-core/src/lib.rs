//! Deterministic context graph, token accounting, compression, and safety primitives.

pub mod graph;
pub mod strategies;
pub mod strategy;
pub mod tokens;

pub use graph::{
    ContextEdge, ContextGraph, ContextNode, EdgeKind, GraphError, NodeKind, ProviderTurn, Source,
};
pub use strategies::DedupExact;
pub use strategy::{
    CompressionBudget, CompressionStrategy, Estimate, Lossiness, Mode, RemovalKind, RemovalRecord,
    StrategyError, StrategyOutput, StrategyRegistry,
};
pub use tokens::{CALIBRATION, TokenCounter, TokenProvider, counter_for};
