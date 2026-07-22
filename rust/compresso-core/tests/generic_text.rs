use compresso_core::{
    CompressionBudget, ContextGraph, Engine, Lossiness, Mode, NodeKind, TokenProvider, counter_for,
    segment_graph,
};

const OVERSIZE_TEXT: &str = include_str!("fixtures/generic-text/single-oversize-text.txt");
const FENCE_BOMB: &str = include_str!("../../../fixtures/adversarial/fence-bomb/input.txt");

fn budget(max_tokens: usize) -> CompressionBudget {
    CompressionBudget {
        max_tokens,
        max_latency_ms: 2_000,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    }
}

#[test]
fn test_committed_generic_text_fixture_segments_losslessly() {
    let counter = counter_for(TokenProvider::Heuristic);
    let input = ContextGraph::from_text(OVERSIZE_TEXT, NodeKind::Summary, counter);

    let first = segment_graph(&input).expect("fixture should segment");
    let second = segment_graph(&input).expect("fixture should segment deterministically");

    assert_eq!(first, second);
    assert_eq!(
        first
            .nodes
            .iter()
            .map(|node| node.content.as_str())
            .collect::<String>(),
        OVERSIZE_TEXT
    );
    assert!(first.nodes.len() >= 5);
    assert!(first.nodes.iter().any(|node| {
        node.content
            .contains("{\"inside_fence\":\"this block stays structurally whole\"}")
            && node.content.starts_with("```json")
            && node.content.ends_with("```\n")
    }));
    assert!(first.nodes.last().expect("final segment").protected);
}

#[test]
fn test_single_oversize_text_keeps_anchor_or_falls_back_byte_identical() {
    let counter = counter_for(TokenProvider::Heuristic);
    let input = ContextGraph::from_text(OVERSIZE_TEXT, NodeKind::Summary, counter);
    let input_bytes = serde_json::to_vec(&input).expect("input should serialize");
    let anchor_tokens = segment_graph(&input)
        .expect("fixture should segment")
        .nodes
        .iter()
        .filter(|node| node.protected)
        .map(|node| node.token_count)
        .sum();

    let compressed =
        Engine::new(Mode::Safe, counter).compress(input.clone(), &budget(anchor_tokens));
    assert!(
        !compressed.fallback_used,
        "{:?}",
        compressed.fallback_reason
    );
    assert!(!compressed.output.nodes.is_empty());
    assert!(
        compressed
            .output
            .nodes
            .iter()
            .any(|node| !node.content.is_empty())
    );
    assert!(compressed.output.nodes.iter().any(|node| {
        node.content
            .contains("FINAL ANCHOR: retain this current non-empty instruction.")
    }));

    let fallback = Engine::new(Mode::Safe, counter).compress(input, &budget(1));
    assert!(fallback.fallback_used);
    assert_eq!(
        fallback.fallback_reason.as_deref(),
        Some("unsatisfiable_budget")
    );
    assert_eq!(
        serde_json::to_vec(&fallback.output).expect("fallback output should serialize"),
        input_bytes
    );
}

#[test]
fn test_nested_unbalanced_fence_fixture_fails_open_byte_identical() {
    let counter = counter_for(TokenProvider::Heuristic);
    let input = ContextGraph::from_text(FENCE_BOMB, NodeKind::AssistantResponse, counter);
    let bytes = serde_json::to_vec(&input).expect("input should serialize");

    let result = Engine::new(Mode::Safe, counter).compress(input, &budget(500));

    assert!(result.fallback_used);
    assert!(
        result
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("segmentation:"))
    );
    assert_eq!(
        serde_json::to_vec(&result.output).expect("fallback should serialize"),
        bytes
    );
}
