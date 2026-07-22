use compresso_core::{
    CompressionBudget, ContextEdge, ContextGraph, ContextNode, EdgeKind, Engine, Lossiness, Mode,
    NodeKind, Source, TokenProvider, counter_for,
};
use serde_json::Map;

const TOOL_OUTPUT_DUPES: &str =
    include_str!("../../../fixtures/conversations/tool-output-dupes/graph.json");

fn node(id: &str, content: &str) -> ContextNode {
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

fn duplicate_edge(from: &str, to: &str) -> ContextEdge {
    ContextEdge {
        from: from.to_owned(),
        to: to.to_owned(),
        kind: EdgeKind::DuplicateOf,
        weight: 1.0,
    }
}

fn graph(nodes: Vec<ContextNode>, edges: Vec<ContextEdge>) -> ContextGraph {
    ContextGraph {
        schema_version: 1,
        nodes,
        edges,
    }
}

fn assert_refs_fallback_byte_identical(input: ContextGraph) {
    let input_bytes = serde_json::to_vec(&input).expect("input should serialize");
    let budget = CompressionBudget {
        max_tokens: 10_000,
        max_latency_ms: 2_000,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    };

    let result =
        Engine::new(Mode::Safe, counter_for(TokenProvider::Heuristic)).compress(input, &budget);

    assert!(result.fallback_used);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some("safety_check_failed:refs_valid")
    );
    assert_eq!(
        serde_json::to_vec(&result.output).expect("output should serialize"),
        input_bytes
    );
}

#[test]
fn test_orphan_duplicate_stub_forces_byte_identical_fallback() {
    assert_refs_fallback_byte_identical(graph(
        vec![
            node("referent", "original payload"),
            node("stub", "[duplicate of node referent]"),
        ],
        Vec::new(),
    ));
}

#[test]
fn test_duplicate_cycle_forces_byte_identical_fallback() {
    assert_refs_fallback_byte_identical(graph(
        vec![
            node("first", "[duplicate of node second]"),
            node("second", "[duplicate of node first]"),
        ],
        vec![
            duplicate_edge("first", "second"),
            duplicate_edge("second", "first"),
        ],
    ));
}

#[test]
fn test_multiply_referenced_stub_forces_byte_identical_fallback() {
    assert_refs_fallback_byte_identical(graph(
        vec![
            node("first", "first payload"),
            node("second", "second payload"),
            node("stub", "[duplicate of node first]"),
        ],
        vec![
            duplicate_edge("stub", "first"),
            duplicate_edge("stub", "second"),
        ],
    ));
}

#[test]
fn test_duplicate_edge_marker_mismatch_forces_byte_identical_fallback() {
    assert_refs_fallback_byte_identical(graph(
        vec![
            node("referent", "original payload"),
            node("stub", "ordinary content, not a typed duplicate stub"),
        ],
        vec![duplicate_edge("stub", "referent")],
    ));
}

#[test]
fn test_tool_output_dupes_pipeline_preserves_referent_and_resolves_stubs() {
    let input: ContextGraph =
        serde_json::from_str(TOOL_OUTPUT_DUPES).expect("committed fixture should deserialize");
    let original_referent = input
        .nodes
        .iter()
        .find(|node| node.id == "resp-1")
        .expect("fixture referent")
        .clone();
    let budget = CompressionBudget {
        max_tokens: 1_200,
        max_latency_ms: 2_000,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    };

    let result =
        Engine::new(Mode::Safe, counter_for(TokenProvider::Heuristic)).compress(input, &budget);

    assert!(
        !result.fallback_used,
        "fallback: {:?}; strategies: {:?}",
        result.fallback_reason, result.strategy_results
    );
    assert_eq!(
        result
            .output
            .nodes
            .iter()
            .find(|node| node.id == "resp-1")
            .expect("referent should survive")
            .content,
        original_referent.content
    );
    for stub_id in ["resp-2", "resp-3"] {
        assert!(result.output.edges.iter().any(|edge| {
            edge.from == stub_id && edge.to == "resp-1" && edge.kind == EdgeKind::DuplicateOf
        }));
    }
}
