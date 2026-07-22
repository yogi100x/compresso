use std::hint::black_box;
use std::time::{Duration, Instant};

use compresso_core::{
    CompressionBudget, ContextGraph, Engine, Lossiness, Mode, NodeKind, TokenProvider, counter_for,
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

const LOG_500: &str = include_str!("../../../fixtures/logs/repeated-npm-install/input.txt");
const JSON_2000: &str = include_str!("../../../fixtures/benchmarks/json-2000-elements/input.json");
const CONVERSATION_40: &str =
    include_str!("../../../fixtures/conversations/long-session-prune/graph.json");
const SOURCE_200KB: &str = include_str!("../../../fixtures/benchmarks/source-200kb/input.ts");
const DISTRIBUTION_SAMPLES: usize = 100;

struct FixtureCase {
    benchmark_name: &'static str,
    graph: ContextGraph,
    budget: CompressionBudget,
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

    // The corpus models forty conversation-turn nodes: archived request
    // summaries, the current user request, and assistant responses. The system
    // instruction and current tool request bring the canonical total to 42.
    assert_eq!(conversation.nodes.len(), 42);
    assert_eq!(
        conversation
            .nodes
            .iter()
            .filter(|node| matches!(
                node.kind,
                NodeKind::Summary | NodeKind::UserInstruction | NodeKind::AssistantResponse
            ))
            .count(),
        40
    );

    vec![
        FixtureCase {
            benchmark_name: "compress_log_500",
            graph: ContextGraph::from_text(LOG_500, NodeKind::ToolResponse, counter),
            budget: budget(2_000),
        },
        FixtureCase {
            benchmark_name: "compress_json_2000",
            graph: ContextGraph::from_text(JSON_2000, NodeKind::ToolResponse, counter),
            budget: budget(4_000),
        },
        FixtureCase {
            benchmark_name: "compress_conversation_40",
            graph: conversation,
            budget: budget(6_000),
        },
        FixtureCase {
            benchmark_name: "compress_source_200kb",
            graph: ContextGraph::from_text(SOURCE_200KB, NodeKind::SourceFile, counter),
            budget: budget(1_500),
        },
    ]
}

fn measured_percentiles(engine: &Engine, fixture: &FixtureCase) -> (Duration, Duration) {
    let _ = black_box(engine.compress(fixture.graph.clone(), &fixture.budget));
    let mut durations = Vec::with_capacity(DISTRIBUTION_SAMPLES);
    for _ in 0..DISTRIBUTION_SAMPLES {
        let input = fixture.graph.clone();
        let started = Instant::now();
        let _ = black_box(engine.compress(input, &fixture.budget));
        durations.push(started.elapsed());
    }
    durations.sort_unstable();
    (durations[49], durations[94])
}

fn benchmark_compression(c: &mut Criterion) {
    for fixture in fixture_cases() {
        let engine = Engine::new(Mode::Safe, counter_for(TokenProvider::OpenAiO200k));
        if !cfg!(debug_assertions) {
            let (p50, p95) = measured_percentiles(&engine, &fixture);
            println!(
                "{} measured distribution: p50={:.3}ms p95={:.3}ms samples={DISTRIBUTION_SAMPLES}",
                fixture.benchmark_name,
                p50.as_secs_f64() * 1_000.0,
                p95.as_secs_f64() * 1_000.0,
            );
        }

        c.bench_function(fixture.benchmark_name, |bencher| {
            bencher.iter_batched(
                || fixture.graph.clone(),
                |input| {
                    let _ = black_box(engine.compress(black_box(input), &fixture.budget));
                },
                BatchSize::SmallInput,
            );
        });
    }
}

criterion_group! {
    name = compression_latency;
    config = Criterion::default()
        .sample_size(100)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = benchmark_compression
}
criterion_main!(compression_latency);
