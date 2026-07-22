use std::path::Path;

use compresso_core::{
    CompressionBudget, ContextGraph, Engine, Lossiness, Mode, NodeKind, counter_for,
};
use jsonschema::{Draft, Validator};
use serde_json::Value;

const CASES: [(&str, &str, usize); 4] = [
    (
        "anthropic_single",
        include_str!("fixtures/provider-tool-loop/anthropic-single.json"),
        1,
    ),
    (
        "anthropic_parallel",
        include_str!("fixtures/provider-tool-loop/anthropic-parallel.json"),
        2,
    ),
    (
        "openai_single",
        include_str!("fixtures/provider-tool-loop/openai-single.json"),
        1,
    ),
    (
        "openai_parallel",
        include_str!("fixtures/provider-tool-loop/openai-parallel.json"),
        2,
    ),
];

fn schema() -> Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas/context-graph/v1.json");
    let value = serde_json::from_str(
        &std::fs::read_to_string(path).expect("context graph schema should be readable"),
    )
    .expect("context graph schema should parse");
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&value)
        .expect("context graph schema should compile")
}

fn budget() -> CompressionBudget {
    CompressionBudget {
        max_tokens: 10_000,
        max_latency_ms: 2_000,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    }
}

#[test]
fn test_provider_loop_assets_roundtrip_validate_and_compress() {
    let validator = schema();
    for (name, source, expected_calls) in CASES {
        let json = serde_json::from_str::<Value>(source)
            .unwrap_or_else(|error| panic!("{name} JSON should parse: {error}"));
        validator
            .validate(&json)
            .unwrap_or_else(|error| panic!("{name} should match graph v1: {error}"));
        let graph = serde_json::from_value::<ContextGraph>(json.clone())
            .unwrap_or_else(|error| panic!("{name} should deserialize: {error}"));
        let serialized = serde_json::to_value(&graph).expect("graph should serialize");
        validator
            .validate(&serialized)
            .unwrap_or_else(|error| panic!("serialized {name} should match graph v1: {error}"));
        assert_eq!(
            serde_json::from_value::<ContextGraph>(serialized)
                .expect("serialized graph should deserialize"),
            graph,
            "{name} should roundtrip through the typed graph"
        );
        assert!(graph.validate().is_ok(), "{name} graph invariants");
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::ToolRequest)
                .count(),
            expected_calls
        );
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::ToolResponse)
                .count(),
            expected_calls
        );
        assert_eq!(
            graph
                .edges
                .iter()
                .filter(|edge| edge.kind == compresso_core::EdgeKind::RespondsTo)
                .count(),
            expected_calls
        );
        assert!(
            graph
                .nodes
                .iter()
                .any(|node| { node.kind == NodeKind::AssistantReasoning && node.protected })
        );

        let provider = graph.nodes[0].token_provider;
        let result = Engine::new(Mode::Safe, counter_for(provider)).compress(graph, &budget());
        assert!(
            !result.fallback_used,
            "{name}: {:?}",
            result.fallback_reason
        );
        assert!(result.safety_checks.iter().all(|check| check.passed));
    }
}

#[test]
fn test_typed_provider_identifiers_reject_empty_strings() {
    for field in ["provider", "turn_id", "tool_call_id"] {
        let mut value = serde_json::from_str::<Value>(CASES[0].1).expect("fixture parses");
        value["nodes"][0]["provider_turn"][field] = Value::String(String::new());
        assert!(
            serde_json::from_value::<ContextGraph>(value).is_err(),
            "{field}"
        );
    }
}
