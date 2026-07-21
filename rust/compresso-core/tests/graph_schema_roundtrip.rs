use std::fs;
use std::path::Path;

use compresso_core::graph::{
    ContextEdge, ContextGraph, ContextNode, EdgeKind, GraphError, NodeKind, ProviderTurn, Source,
};
use compresso_core::tokens::{TokenCounter, TokenProvider};
use jsonschema::{Draft, Validator};
use serde_json::{Map, Value, json};

#[derive(Debug)]
struct WordCounter;

impl TokenCounter for WordCounter {
    fn provider(&self) -> TokenProvider {
        TokenProvider::Heuristic
    }

    fn count(&self, text: &str) -> usize {
        text.split_whitespace().count()
    }
}

fn context_graph_validator() -> Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("schemas/context-graph/v1.json");
    let schema: Value = serde_json::from_str(
        &fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display())),
    )
    .expect("context graph schema should be JSON");
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema)
        .expect("context graph schema should compile")
}

fn node(id: &str, kind: NodeKind, source: Source) -> ContextNode {
    ContextNode {
        id: id.to_owned(),
        kind,
        content: format!("content for {id}"),
        source,
        token_count: 3,
        token_provider: TokenProvider::OpenAiO200k,
        priority: 0.5,
        protected: kind.requires_protection(),
        recency: None,
        metadata: Map::new(),
        provider_turn: None,
    }
}

#[test]
fn test_roundtrip_all_kinds_validates_against_schema() {
    let kinds = [
        NodeKind::SystemInstruction,
        NodeKind::UserInstruction,
        NodeKind::AssistantResponse,
        NodeKind::AssistantReasoning,
        NodeKind::ToolDefinition,
        NodeKind::ToolRequest,
        NodeKind::ToolResponse,
        NodeKind::SourceFile,
        NodeKind::CodeSymbol,
        NodeKind::SearchResult,
        NodeKind::CompilerOutput,
        NodeKind::TestOutput,
        NodeKind::RepoMap,
        NodeKind::Summary,
        NodeKind::Decision,
        NodeKind::MemoryFact,
    ];
    let sources = [
        Source::Client,
        Source::Repository {
            path: "src/lib.rs".to_owned(),
        },
        Source::Tool {
            name: "compiler".to_owned(),
        },
        Source::Synthetic,
    ];
    let nodes = kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| {
            node(
                &format!("n{index}"),
                kind,
                sources[index % sources.len()].clone(),
            )
        })
        .collect::<Vec<_>>();
    let edge_kinds = [
        EdgeKind::Imports,
        EdgeKind::Calls,
        EdgeKind::Defines,
        EdgeKind::References,
        EdgeKind::Tests,
        EdgeKind::Supersedes,
        EdgeKind::ProducedByTool,
        EdgeKind::RespondsTo,
        EdgeKind::DependsOn,
        EdgeKind::SameErrorGroup,
        EdgeKind::DuplicateOf,
    ];
    let edges = edge_kinds
        .into_iter()
        .enumerate()
        .map(|(index, kind)| ContextEdge {
            from: format!("n{index}"),
            to: format!("n{}", index + 1),
            kind,
            weight: 0.75,
        })
        .collect();
    let graph = ContextGraph {
        schema_version: 1,
        nodes,
        edges,
    };

    graph.validate().expect("every-kind graph should be valid");
    let serialized = serde_json::to_value(&graph).expect("graph should serialize");
    context_graph_validator()
        .validate(&serialized)
        .expect("serialized graph should match context-graph/v1.json");
    let roundtripped: ContextGraph =
        serde_json::from_value(serialized).expect("graph should deserialize");

    assert_eq!(roundtripped, graph);
}

#[test]
fn test_token_provider_defaults_to_openai_o200k() {
    let serialized = json!({
        "schema_version": 1,
        "nodes": [{
            "id": "n1",
            "kind": "source_file",
            "content": "content",
            "source": { "type": "client" },
            "token_count": 1,
            "priority": 0.5,
            "protected": false
        }],
        "edges": []
    });

    let graph: ContextGraph = serde_json::from_value(serialized).expect("graph should deserialize");

    assert_eq!(graph.nodes[0].token_provider, TokenProvider::OpenAiO200k);
}

#[test]
fn test_provider_turn_json_rust_json_roundtrip() {
    let serialized = json!({
        "schema_version": 1,
        "nodes": [{
            "id": "request-1",
            "kind": "tool_request",
            "content": "{}",
            "source": { "type": "client" },
            "token_count": 1,
            "token_provider": "openai_o200k",
            "priority": 1.0,
            "protected": true,
            "recency": null,
            "metadata": {},
            "provider_turn": {
                "provider": "fixture-provider",
                "turn_id": "turn-1",
                "tool_call_id": "call-1",
                "opaque_state_hash": "blake3:abc"
            }
        }],
        "edges": []
    });

    let graph: ContextGraph =
        serde_json::from_value(serialized.clone()).expect("provider turn should deserialize");

    assert_eq!(
        graph.nodes[0].provider_turn,
        Some(ProviderTurn {
            provider: "fixture-provider".to_owned(),
            turn_id: "turn-1".to_owned(),
            tool_call_id: "call-1".to_owned(),
            opaque_state_hash: Some("blake3:abc".to_owned()),
        })
    );
    assert_eq!(
        serde_json::to_value(graph).expect("provider turn should serialize"),
        serialized
    );
}

#[test]
fn test_provider_turn_rejects_unknown_fields() {
    let provider_turn = json!({
        "provider": "fixture-provider",
        "turn_id": "turn-1",
        "tool_call_id": "call-1",
        "unexpected": true
    });

    assert!(serde_json::from_value::<ProviderTurn>(provider_turn).is_err());
}

#[test]
fn test_negative_recency_rejected_by_schema() {
    let serialized = json!({
        "schema_version": 1,
        "nodes": [{
            "id": "n1",
            "kind": "source_file",
            "content": "content",
            "source": { "type": "client" },
            "token_count": 1,
            "priority": 0.5,
            "protected": false,
            "recency": -1
        }],
        "edges": []
    });

    let error = context_graph_validator()
        .validate(&serialized)
        .expect_err("negative recency should violate the schema");

    assert_eq!(error.instance_path.as_str(), "/nodes/0/recency");
}

#[test]
fn test_ingress_forces_protection() {
    let mut metadata = Map::new();
    metadata.insert("allow_unprotect".to_owned(), Value::Bool(true));
    metadata.insert("protected".to_owned(), Value::Bool(false));
    metadata.insert("kind".to_owned(), Value::String("source_file".to_owned()));
    metadata.insert(
        "nested".to_owned(),
        json!({ "override": { "protected": false }, "values": [null, true, 42] }),
    );
    let mut graph = ContextGraph {
        schema_version: 1,
        nodes: vec![ContextNode {
            metadata,
            protected: false,
            ..node("sys-1", NodeKind::SystemInstruction, Source::Client)
        }],
        edges: vec![],
    };

    let notes = graph.force_ingress_protection();

    assert!(graph.nodes[0].protected);
    assert_eq!(notes, ["protection_forced:sys-1"]);
    assert_eq!(graph.force_ingress_protection(), Vec::<String>::new());
    graph.validate().expect("forced graph should validate");
}

#[test]
fn test_assistant_reasoning_cannot_be_unprotected() {
    let graph = ContextGraph {
        schema_version: 1,
        nodes: vec![ContextNode {
            protected: false,
            ..node("reasoning-1", NodeKind::AssistantReasoning, Source::Client)
        }],
        edges: vec![],
    };

    assert_eq!(
        graph.validate(),
        Err(GraphError::ProtectionViolation {
            node_id: "reasoning-1".to_owned(),
            kind: NodeKind::AssistantReasoning,
        })
    );
}

#[test]
fn test_from_text_counts_tokens() {
    let content = "fn main() {}";

    let graph = ContextGraph::from_text(content, NodeKind::SourceFile, &WordCounter);

    assert_eq!(graph.nodes.len(), 1);
    assert_eq!(graph.nodes[0].content, content);
    assert_eq!(graph.nodes[0].token_count, WordCounter.count(content));
    assert_eq!(graph.nodes[0].token_provider, TokenProvider::Heuristic);
    assert_eq!(graph.total_tokens(), graph.nodes[0].token_count);
}
