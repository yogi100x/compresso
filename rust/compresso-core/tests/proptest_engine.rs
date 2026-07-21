use std::collections::HashMap;

use compresso_core::{
    CompressionBudget, ContextEdge, ContextGraph, ContextNode, EdgeKind, Engine, Lossiness, Mode,
    NodeKind, Source, TokenProvider, counter_for,
};
use proptest::prelude::*;
use serde_json::{Map, Value};

const CASES_PER_PROPERTY: u32 = 1_024;

#[derive(Clone, Debug)]
struct NodeSpec {
    kind: NodeKind,
    content: String,
    source: Source,
    priority: f32,
    protected: bool,
    recency: Option<u64>,
}

fn arb_node_kind() -> impl Strategy<Value = NodeKind> {
    prop::sample::select(vec![
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
    ])
}

fn arb_edge_kind() -> impl Strategy<Value = EdgeKind> {
    prop::sample::select(vec![
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
    ])
}

fn arb_source() -> impl Strategy<Value = Source> {
    prop_oneof![
        Just(Source::Client),
        Just(Source::Synthetic),
        (0_u8..8).prop_map(|index| Source::Repository {
            path: format!("generated/file-{index}.rs"),
        }),
        (0_u8..8).prop_map(|index| Source::Tool {
            name: format!("generated-tool-{index}"),
        }),
    ]
}

fn arb_unicode_text(max_chars: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..max_chars)
        .prop_map(|characters| characters.into_iter().collect())
}

fn arb_content() -> impl Strategy<Value = String> {
    prop_oneof![
        2 => Just(String::new()),
        2 => Just("\0".to_owned()),
        2 => Just("\u{0001}\u{001f}\u{007f}".to_owned()),
        8 => arb_unicode_text(96),
        5 => arb_unicode_text(64).prop_map(|text| format!("not-json::{text}")),
        5 => prop::collection::vec(any::<i32>(), 0..32)
            .prop_map(|values| serde_json::to_string(&values).expect("integer JSON serializes")),
        4 => (arb_unicode_text(32), any::<bool>()).prop_map(|(text, enabled)| {
            let value = serde_json::json!({"text": text, "enabled": enabled});
            serde_json::to_string(&value).expect("object JSON serializes")
        }),
        4 => (3_usize..20, 0_u16..1_000).prop_map(|(lines, code)| {
            (0..lines)
                .map(|index| format!("2026-01-01T00:00:{index:02}Z event code={code}\n"))
                .collect()
        }),
    ]
}

fn arb_node_spec() -> impl Strategy<Value = NodeSpec> {
    (
        arb_node_kind(),
        arb_content(),
        arb_source(),
        0_u16..=1_000,
        any::<bool>(),
        prop::option::of(any::<u64>()),
    )
        .prop_map(
            |(kind, content, source, priority, requested_protection, recency)| NodeSpec {
                kind,
                content,
                source,
                priority: f32::from(priority) / 1_000.0,
                protected: kind.requires_protection() || requested_protection,
                recency,
            },
        )
}

fn arb_graph() -> impl Strategy<Value = ContextGraph> {
    prop::collection::vec(arb_node_spec(), 0..51)
        .prop_flat_map(|specs| {
            let edge_strategy: BoxedStrategy<Vec<(usize, usize, EdgeKind, u16)>> =
                if specs.is_empty() {
                    Just(Vec::new()).boxed()
                } else {
                    let node_count = specs.len();
                    prop::collection::vec(
                        (0..node_count, 0..node_count, arb_edge_kind(), 0_u16..=1_000),
                        0..101,
                    )
                    .boxed()
                };
            (Just(specs), edge_strategy)
        })
        .prop_map(|(specs, edges)| {
            let counter = counter_for(TokenProvider::Heuristic);
            let nodes = specs
                .into_iter()
                .enumerate()
                .map(|(index, spec)| ContextNode {
                    id: format!("node-{index}"),
                    kind: spec.kind,
                    token_count: counter.count(&spec.content),
                    token_provider: counter.provider(),
                    content: spec.content,
                    source: spec.source,
                    priority: spec.priority,
                    protected: spec.protected,
                    recency: spec.recency,
                    metadata: Map::<String, Value>::new(),
                    provider_turn: None,
                })
                .collect::<Vec<_>>();
            let edges = edges
                .into_iter()
                .map(|(from, to, kind, weight)| ContextEdge {
                    from: format!("node-{from}"),
                    to: format!("node-{to}"),
                    kind,
                    weight: f32::from(weight) / 1_000.0,
                })
                .collect();
            ContextGraph {
                schema_version: 1,
                nodes,
                edges,
            }
        })
}

fn arb_lossiness() -> impl Strategy<Value = Lossiness> {
    prop::sample::select(vec![
        Lossiness::None,
        Lossiness::Structural,
        Lossiness::Summarizing,
    ])
}

fn arb_budget() -> impl Strategy<Value = CompressionBudget> {
    (
        1_usize..100_001,
        arb_lossiness(),
        prop::collection::vec(arb_node_kind(), 0..5),
    )
        .prop_map(
            |(max_tokens, allowed_lossiness, protected_kinds)| CompressionBudget {
                max_tokens,
                max_latency_ms: 2_000,
                allowed_lossiness,
                protected_kinds,
            },
        )
}

fn serialize_graph(graph: &ContextGraph) -> Vec<u8> {
    serde_json::to_vec(graph).expect("generated finite graph should serialize")
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: CASES_PER_PROPERTY,
        max_shrink_iters: 10_000,
        .. ProptestConfig::default()
    })]

    #[test]
    fn prop_compress_never_mutates_protected(input in arb_graph(), budget in arb_budget()) {
        prop_assert!(input.validate().is_ok());
        prop_assert!(input.nodes.len() <= 50);
        prop_assert!(input.edges.len() <= 100);
        let protected = input
            .nodes
            .iter()
            .filter(|node| node.protected || budget.protected_kinds.contains(&node.kind))
            .map(|node| (node.id.clone(), node.content.clone()))
            .collect::<HashMap<_, _>>();

        let result = Engine::new(Mode::Safe, counter_for(TokenProvider::Heuristic))
            .compress(input, &budget);
        let output = result
            .output
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.content.as_str()))
            .collect::<HashMap<_, _>>();

        for (id, content) in protected {
            prop_assert_eq!(output.get(id.as_str()).copied(), Some(content.as_str()));
        }
    }

    #[test]
    fn prop_safe_output_never_exceeds_input_tokens(input in arb_graph(), budget in arb_budget()) {
        prop_assert!(input.validate().is_ok());

        let result = Engine::new(Mode::Safe, counter_for(TokenProvider::Heuristic))
            .compress(input, &budget);

        prop_assert!(result.after_tokens <= result.before_tokens);
    }

    #[test]
    fn prop_output_always_validates(input in arb_graph(), budget in arb_budget()) {
        prop_assert!(input.validate().is_ok());

        let result = Engine::new(Mode::Safe, counter_for(TokenProvider::Heuristic))
            .compress(input, &budget);

        prop_assert!(result.output.validate().is_ok());
    }

    #[test]
    fn prop_fallback_implies_identity(input in arb_graph(), budget in arb_budget()) {
        prop_assert!(input.validate().is_ok());
        let input_json = serialize_graph(&input);

        let result = Engine::new(Mode::Safe, counter_for(TokenProvider::Heuristic))
            .compress(input, &budget);

        if result.fallback_used {
            prop_assert_eq!(serialize_graph(&result.output), input_json);
        }
    }
}
