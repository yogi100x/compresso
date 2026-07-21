use std::fs;
use std::path::{Path, PathBuf};

use jsonschema::{Draft, Validator};
use serde_json::{Value, json};

fn schema_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas")
        .join(name)
        .join("v1.json")
}

fn load_schema(name: &str) -> Value {
    let path = schema_path(name);
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

fn compile_schema(schema: &Value) -> Validator {
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(schema)
        .expect("schema should compile as JSON Schema draft 2020-12")
}

fn context_graph_schema_from_docs() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("docs/plan/02-schemas.md");
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let section = contents
        .split_once("## 1. ContextGraph v1")
        .expect("schema docs should contain the ContextGraph v1 section")
        .1;
    let json = section
        .split_once("```json\n")
        .expect("ContextGraph section should contain a JSON block")
        .1
        .split_once("\n```")
        .expect("ContextGraph JSON block should be closed")
        .0;

    serde_json::from_str(json).expect("documented ContextGraph schema should be JSON")
}

#[test]
fn test_context_graph_schema_compiles() {
    let schema = load_schema("context-graph");

    compile_schema(&schema);
}

#[test]
fn test_context_graph_schema_matches_authoritative_docs() {
    assert_eq!(
        load_schema("context-graph"),
        context_graph_schema_from_docs()
    );
}

#[test]
fn test_minimal_graph_validates() {
    let schema = load_schema("context-graph");
    let validator = compile_schema(&schema);
    let graph = json!({"schema_version": 1, "nodes": [], "edges": []});

    assert!(validator.is_valid(&graph));
}

#[test]
fn test_bad_kind_rejected() {
    let schema = load_schema("context-graph");
    let validator = compile_schema(&schema);
    let graph = json!({
        "schema_version": 1,
        "nodes": [{
            "id": "node-1",
            "kind": "banana",
            "content": "content",
            "source": {"type": "client"},
            "token_count": 1,
            "priority": 1.0,
            "protected": false
        }],
        "edges": []
    });

    let error = validator
        .validate(&graph)
        .expect_err("an unknown node kind should be rejected");
    assert_eq!(error.instance_path.as_str(), "/nodes/0/kind");
}

#[test]
fn test_trace_requires_fallback_used() {
    let schema = load_schema("compression-trace");
    let validator = compile_schema(&schema);
    let trace = json!({
        "schema_version": 1,
        "operation_id": "01J00000000000000000000000",
        "mode": "safe",
        "before_tokens": 10,
        "after_tokens": 8,
        "strategy_results": [],
        "safety_checks": [],
        "explanation": "compressed"
    });

    assert!(!validator.is_valid(&trace));
}
