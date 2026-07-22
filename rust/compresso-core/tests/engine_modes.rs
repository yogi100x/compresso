use compresso_core::{
    CompressionBudget, ContextGraph, Engine, Lossiness, Mode, NodeKind, TokenProvider, counter_for,
};

fn budget() -> CompressionBudget {
    CompressionBudget {
        max_tokens: 10_000,
        max_latency_ms: 200,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: vec![],
    }
}

#[test]
fn test_compress_in_mode_honors_request_without_mutating_engine_mode() {
    let counter = counter_for(TokenProvider::Heuristic);
    let engine = Engine::new(Mode::Safe, counter);
    let input = ContextGraph::from_text("line one\nline two\n", NodeKind::ToolResponse, counter);

    let passthrough = engine.compress_in_mode(Mode::Passthrough, input.clone(), &budget());

    assert_eq!(engine.mode(), Mode::Safe);
    assert_eq!(passthrough.output, input);
    assert_eq!(passthrough.trace.mode, Mode::Passthrough);
    assert!(!passthrough.fallback_used);
}

#[test]
fn test_controlled_fail_open_returns_valid_identity_trace() {
    let counter = counter_for(TokenProvider::Heuristic);
    let engine = Engine::new(Mode::Safe, counter);
    let input = ContextGraph::from_text("oversized input", NodeKind::ToolResponse, counter);

    let result = engine.fail_open(input.clone(), "input_too_large");

    assert_eq!(result.output, input);
    assert_eq!(result.before_tokens, result.after_tokens);
    assert!(result.fallback_used);
    assert_eq!(result.fallback_reason.as_deref(), Some("input_too_large"));
    assert_eq!(
        result.trace.fallback_reason.as_deref(),
        Some("input_too_large")
    );
    assert_eq!(result.trace.operation_id.len(), 26);
}
