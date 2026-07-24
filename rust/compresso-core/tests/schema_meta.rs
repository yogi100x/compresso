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
    context_graph_schema_from_text(&contents)
}

fn context_graph_schema_from_text(contents: &str) -> Value {
    let contents = contents.replace("\r\n", "\n");
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
fn test_documented_schema_extraction_accepts_crlf() {
    let docs = "## 1. ContextGraph v1\r\n\r\n```json\r\n{\"type\":\"object\"}\r\n```\r\n";
    assert_eq!(
        context_graph_schema_from_text(docs),
        json!({ "type": "object" })
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

#[test]
fn test_beta_release_manifest_schema_compiles_and_rejects_extra_fields() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas/release-manifest/beta-v1.json");
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let schema: Value = serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
    let validator = compile_schema(&schema);
    let manifest = json!({
        "schema_version": 1,
        "kind": "compresso_beta_release",
        "channel": "beta",
        "package_name": "@compresso/cli",
        "package_version": "0.1.0-beta.0",
        "engine_version": "0.1.0",
        "immutable_release_identity": "release-id",
        "provenance": {
            "identity": "provenance-id",
            "source_repository": "yogi100x/compresso",
            "source_commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "workflow_ref": "yogi100x/compresso/.github/workflows/package-beta.yml@refs/tags/cli-v0.1.0-beta.0",
            "run_id": "1234",
            "run_attempt": 1
        },
        "artifacts": [
            release_artifact("aarch64-apple-darwin", "darwin-arm64", "apple_codesign"),
            release_artifact("x86_64-apple-darwin", "darwin-x64", "apple_codesign"),
            release_artifact("x86_64-unknown-linux-gnu", "linux-x64", "sigstore_manifest")
        ]
    });

    assert!(validator.is_valid(&manifest));

    let mut unexpected = manifest;
    unexpected
        .as_object_mut()
        .expect("manifest should be an object")
        .insert("private_source".to_owned(), json!("forbidden"));
    assert!(!validator.is_valid(&unexpected));
}

fn release_artifact(rust_target: &str, package_target: &str, signing_kind: &str) -> Value {
    json!({
        "schema_version": 1,
        "rust_target": rust_target,
        "package_target": package_target,
        "binary": {
            "path": "compresso",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "size": 1
        },
        "sbom": {
            "path": "compresso.cdx.json",
            "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "size": 1,
            "format": "cyclonedx-json"
        },
        "native_signing": {
            "kind": signing_kind,
            "verified": true,
            "identity": if signing_kind == "apple_codesign" {
                "Developer ID Application: Compresso Test (TESTTEAM)"
            } else {
                ""
            }
        },
        "immutable_identity": format!("artifact-{package_target}"),
        "provenance_identity": "provenance-id"
    })
}
