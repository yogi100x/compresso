use std::hint::black_box;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use compresso_core::{
    CompressionBudget, ContextGraph, Engine, Lossiness, Mode, NodeKind, TokenProvider, counter_for,
};
use serde_json::Value;

const LOG_500: &str = include_str!("../../../fixtures/logs/repeated-npm-install/input.txt");
const JSON_2000: &str = include_str!("../../../fixtures/benchmarks/json-2000-elements/input.json");
const CONVERSATION_40: &str =
    include_str!("../../../fixtures/conversations/long-session-prune/graph.json");
const SOURCE_200KB: &str = include_str!("../../../fixtures/benchmarks/source-200kb/input.ts");
const SAMPLES: usize = 100;
const SAFE_P95_LIMIT: Duration = Duration::from_millis(200);
const PASSTHROUGH_P95_LIMIT: Duration = Duration::from_millis(5);

static LATENCY_GATE: Mutex<()> = Mutex::new(());

struct FixtureCase {
    name: &'static str,
    graph: ContextGraph,
    budget: CompressionBudget,
}

struct Distribution {
    p50: Duration,
    p95: Duration,
}

fn budget(max_tokens: usize) -> CompressionBudget {
    CompressionBudget {
        max_tokens,
        max_latency_ms: 200,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    }
}

fn fixture_cases() -> Vec<FixtureCase> {
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let conversation = serde_json::from_str::<ContextGraph>(CONVERSATION_40)
        .expect("the committed forty-turn conversation graph should deserialize");

    vec![
        FixtureCase {
            name: "compress_log_500",
            graph: ContextGraph::from_text(LOG_500, NodeKind::ToolResponse, counter),
            budget: budget(2_000),
        },
        FixtureCase {
            name: "compress_json_2000",
            graph: ContextGraph::from_text(JSON_2000, NodeKind::ToolResponse, counter),
            budget: budget(4_000),
        },
        FixtureCase {
            name: "compress_conversation_40",
            graph: conversation,
            budget: budget(6_000),
        },
        FixtureCase {
            name: "compress_source_200kb",
            graph: ContextGraph::from_text(SOURCE_200KB, NodeKind::SourceFile, counter),
            budget: budget(1_500),
        },
    ]
}

fn measure(engine: &Engine, fixture: &FixtureCase) -> Distribution {
    // Tokenizer initialization is a separate startup concern. Warm the exact
    // engine/fixture path once before collecting the release latency samples.
    let _ = black_box(engine.compress(fixture.graph.clone(), &fixture.budget));

    let mut durations = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let input = fixture.graph.clone();
        let started = Instant::now();
        let _ = black_box(engine.compress(input, &fixture.budget));
        durations.push(started.elapsed());
    }
    durations.sort_unstable();
    Distribution {
        p50: durations[49],
        p95: durations[94],
    }
}

fn print_distribution(name: &str, distribution: &Distribution) {
    eprintln!(
        "{name}: p50={:.3}ms p95={:.3}ms samples={SAMPLES}",
        distribution.p50.as_secs_f64() * 1_000.0,
        distribution.p95.as_secs_f64() * 1_000.0,
    );
}

#[test]
fn test_latency_fixtures_are_canonical() {
    assert_eq!(LOG_500.lines().count(), 500);
    let json = serde_json::from_str::<Value>(JSON_2000)
        .expect("the committed JSON benchmark should parse");
    assert_eq!(json.as_array().map(Vec::len), Some(2_000));
    assert_eq!(SOURCE_200KB.len(), 204_800);

    let conversation = serde_json::from_str::<ContextGraph>(CONVERSATION_40)
        .expect("the committed conversation should parse");
    assert_eq!(conversation.nodes.len(), 42);
    assert_eq!(
        conversation
            .nodes
            .iter()
            .filter(|node| matches!(
                node.kind,
                NodeKind::UserInstruction | NodeKind::AssistantResponse
            ))
            .count(),
        40
    );
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release-only latency gate")]
fn test_p95_under_200ms_all_fixtures() {
    let _guard = LATENCY_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    for fixture in fixture_cases() {
        let engine = Engine::new(Mode::Safe, counter_for(TokenProvider::OpenAiO200k));
        let distribution = measure(&engine, &fixture);
        print_distribution(fixture.name, &distribution);
        assert!(
            distribution.p95 < SAFE_P95_LIMIT,
            "{} p95 {:.3}ms exceeded the 200ms release gate (p50 {:.3}ms)",
            fixture.name,
            distribution.p95.as_secs_f64() * 1_000.0,
            distribution.p50.as_secs_f64() * 1_000.0,
        );
    }
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release-only latency gate")]
fn test_passthrough_under_5ms() {
    let _guard = LATENCY_GATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let fixture = FixtureCase {
        name: "passthrough_source_200kb",
        graph: ContextGraph::from_text(
            SOURCE_200KB,
            NodeKind::SourceFile,
            counter_for(TokenProvider::OpenAiO200k),
        ),
        budget: budget(1_500),
    };
    let engine = Engine::new(Mode::Passthrough, counter_for(TokenProvider::OpenAiO200k));

    let distribution = measure(&engine, &fixture);
    print_distribution(fixture.name, &distribution);
    assert!(
        distribution.p95 < PASSTHROUGH_P95_LIMIT,
        "passthrough p95 {:.3}ms exceeded the 5ms release gate (p50 {:.3}ms)",
        distribution.p95.as_secs_f64() * 1_000.0,
        distribution.p50.as_secs_f64() * 1_000.0,
    );
}
