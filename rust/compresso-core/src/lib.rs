//! Deterministic context graph, token accounting, compression, and safety primitives.

pub mod graph;
pub mod tokens;

pub use graph::{
    ContextEdge, ContextGraph, ContextNode, EdgeKind, GraphError, NodeKind, ProviderTurn, Source,
};
pub use tokens::{CALIBRATION, TokenCounter, TokenProvider, counter_for};
