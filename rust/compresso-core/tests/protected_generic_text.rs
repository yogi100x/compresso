use compresso_core::{
    CompressionBudget, ContextGraph, ContextNode, Engine, Lossiness, Mode, NodeKind, Source,
    TokenProvider, counter_for,
};
use serde_json::Map;

const ALL_KINDS: [NodeKind; 16] = [
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

// Every content here is split by generic-text segmentation when unprotected,
// or (blank-only, unbalanced fence) exercises a segmentation edge.
const CONTENTS: [(&str, &str); 12] = [
    ("regression seed", "a\n\nN"),
    ("crlf blank line", "a\r\n\r\nN"),
    ("mixed line endings", "a\r\n\nN\n\r\nz"),
    ("leading blank line", "\n\na\n\nN"),
    ("trailing blank line", "a\n\nN\n\n"),
    ("only blank lines", "\n\n\n"),
    ("only crlf blank lines", "\r\n\r\n"),
    ("whitespace blank line", "a\n \t\nN"),
    (
        "fenced block",
        "before\n\n```rust\nfn main() {}\n```\n\nafter",
    ),
    ("unbalanced fence", "before\n\n```rust\nfn main() {}\n"),
    ("adjacent json values", "{\"a\":1}\n{\"b\":2}\n"),
    (
        "log error groups",
        "ERROR first failure\n  detail\nERROR second failure\n  detail\n",
    ),
];

fn single_node(kind: NodeKind, content: &str, protected: bool) -> ContextGraph {
    ContextGraph {
        schema_version: 1,
        nodes: vec![ContextNode {
            id: "node-0".to_owned(),
            kind,
            content: content.to_owned(),
            source: Source::Tool {
                name: "generated-tool-1".to_owned(),
            },
            token_count: counter_for(TokenProvider::Heuristic).count(content),
            token_provider: TokenProvider::Heuristic,
            priority: 0.989,
            protected,
            recency: None,
            metadata: Map::new(),
            provider_turn: None,
        }],
        edges: Vec::new(),
    }
}

fn budget(max_tokens: usize, protected_kinds: Vec<NodeKind>) -> CompressionBudget {
    CompressionBudget {
        max_tokens,
        max_latency_ms: 2_000,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds,
    }
}

fn assert_node_survives(input: &ContextGraph, budget: &CompressionBudget, case: &str) {
    let result = Engine::new(Mode::Safe, counter_for(TokenProvider::Heuristic))
        .compress(input.clone(), budget);
    let expected = &input.nodes[0];

    assert_eq!(result.output.nodes.len(), 1, "{case}");
    let kept = &result.output.nodes[0];
    assert_eq!(kept.id, expected.id, "{case}");
    assert_eq!(kept.content, expected.content, "{case}");
    assert_eq!(kept.kind, expected.kind, "{case}");
    assert_eq!(kept.source, expected.source, "{case}");
    assert!(result.output.edges.is_empty(), "{case}");
}

#[test]
fn test_protected_blank_line_regression_seed_keeps_id_and_bytes() {
    let input = single_node(NodeKind::SystemInstruction, "a\n\nN", true);
    let budget = budget(59_777, vec![NodeKind::ToolResponse]);

    let result = Engine::new(Mode::Safe, counter_for(TokenProvider::Heuristic))
        .compress(input.clone(), &budget);

    assert!(!result.fallback_used, "{:?}", result.fallback_reason);
    assert_eq!(result.output, input);
}

#[test]
fn test_protected_flag_keeps_id_and_bytes_for_every_kind_and_blank_line_shape() {
    for kind in ALL_KINDS {
        for (name, content) in CONTENTS {
            for max_tokens in [1, 59_777] {
                let case = format!("{kind:?} / {name} / max_tokens={max_tokens}");
                let input = single_node(kind, content, true);

                assert_node_survives(&input, &budget(max_tokens, Vec::new()), &case);
            }
        }
    }
}

#[test]
fn test_budget_protected_kind_keeps_id_and_bytes_when_flag_is_false() {
    for kind in ALL_KINDS {
        for (name, content) in CONTENTS {
            for max_tokens in [1, 59_777] {
                let case = format!("{kind:?} / {name} / max_tokens={max_tokens}");
                let input = single_node(kind, content, false);

                assert_node_survives(&input, &budget(max_tokens, vec![kind]), &case);
            }
        }
    }
}

#[test]
fn test_protected_large_source_is_not_chunked() {
    let content = "fn generated() {}\n".repeat(4_000);
    let input = single_node(NodeKind::SourceFile, &content, true);

    assert_node_survives(&input, &budget(usize::MAX, Vec::new()), "large source");
}

#[test]
fn test_unprotected_generic_text_is_still_segmented_and_packed() {
    let content = "discard this historical paragraph\n\ndiscard this second one\n\nFINAL ANCHOR";
    let input = single_node(NodeKind::Summary, content, false);

    let result = Engine::new(Mode::Safe, counter_for(TokenProvider::Heuristic))
        .compress(input, &budget(12, Vec::new()));

    assert!(!result.fallback_used, "{:?}", result.fallback_reason);
    assert!(result.output.nodes.len() >= 2);
    assert!(
        result
            .output
            .nodes
            .iter()
            .all(|node| node.id.starts_with("node-0::segment-"))
    );
    assert_eq!(
        result.output.nodes.last().map(|node| node.content.as_str()),
        Some("FINAL ANCHOR")
    );
}
