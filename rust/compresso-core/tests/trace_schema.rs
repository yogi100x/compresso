use std::fs;
use std::path::Path;

use compresso_core::{
    CompressionBudget, ContextGraph, Engine, Lossiness, Mode, NodeKind, TokenProvider, counter_for,
};
use jsonschema::{Draft, Validator};
use serde_json::Value;

const LOG_FIXTURE: &str = include_str!("../../../fixtures/logs/repeated-npm-install/input.txt");
const CROCKFORD_BASE32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn trace_validator() -> Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas/compression-trace/v1.json");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let schema: Value = serde_json::from_str(&source)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema)
        .expect("the compression trace root schema should compile")
}

#[test]
fn test_trace_validates_against_schema() {
    let counter = counter_for(TokenProvider::Heuristic);
    let graph = ContextGraph::from_text(LOG_FIXTURE, NodeKind::ToolResponse, counter);
    let budget = CompressionBudget {
        max_tokens: 2_000,
        max_latency_ms: 2_000,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    };

    let result = Engine::new(Mode::Safe, counter).compress(graph, &budget);
    let trace = serde_json::to_value(&result.trace).expect("trace should serialize");

    trace_validator()
        .validate(&trace)
        .expect("trace should satisfy the root compression trace schema");
    assert_eq!(result.trace.operation_id.len(), 26);
    assert!(matches!(
        result.trace.operation_id.as_bytes()[0],
        b'0'..=b'7'
    ));
    assert!(
        result
            .trace
            .operation_id
            .bytes()
            .all(|byte| CROCKFORD_BASE32.contains(&byte))
    );
}
