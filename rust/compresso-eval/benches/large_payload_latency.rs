use std::hint::black_box;
use std::time::{Duration, Instant};

use compresso_core::{
    CompressionBudget, ContextGraph, Engine, Lossiness, MAX_COMPRESSION_INPUT_BYTES, Mode,
    NodeKind, TokenProvider, counter_for, initialize_tokenizers,
};
use compresso_eval::latency::measure_within_limit;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

const INPUT_1_MIB: &str =
    include_str!("../../../fixtures/benchmarks/mib-rebuild-stream/input-1mib.log");
const INPUT_4_MIB: &str =
    include_str!("../../../fixtures/benchmarks/mib-rebuild-stream/input-4mib.log");
const INPUT_8_MIB: &str =
    include_str!("../../../fixtures/benchmarks/mib-rebuild-stream/input-8mib.log");
const COLD_LIMIT: Duration = Duration::from_millis(500);
const WARM_P95_LIMIT: Duration = Duration::from_millis(200);
const GATE_SAMPLES: usize = 100;
/// A loaded runner only ever adds time, so a P95 over the limit is measured
/// again before it fails the gate. The limit itself does not move.
const GATE_ATTEMPTS: usize = 3;

struct ScaleFixture {
    name: &'static str,
    input: &'static str,
}

fn budget() -> CompressionBudget {
    CompressionBudget {
        max_tokens: usize::MAX,
        // The benchmark owns the wall-clock gate below. A larger engine
        // deadline lets it report the complete compression duration instead
        // of converting a slow sample into an early fail-open result.
        max_latency_ms: u64::MAX,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    }
}

fn fixtures() -> Vec<ScaleFixture> {
    assert_eq!(INPUT_8_MIB.len(), MAX_COMPRESSION_INPUT_BYTES);
    [
        ("compress_rebuild_stream_1_mib", INPUT_1_MIB),
        ("compress_rebuild_stream_4_mib", INPUT_4_MIB),
        ("compress_rebuild_stream_8_mib", INPUT_8_MIB),
    ]
    .into_iter()
    .map(|(name, input)| ScaleFixture { name, input })
    .collect()
}

fn assert_cold_startup_gate() {
    let started = Instant::now();
    let initialization = initialize_tokenizers(&[
        TokenProvider::OpenAiO200k,
        TokenProvider::AnthropicCalibrated,
    ]);
    let elapsed = started.elapsed();
    eprintln!("cold tokenizer startup={elapsed:?}");

    assert!(
        initialization.all_ready(),
        "cold tokenizer startup used a heuristic fallback"
    );
    assert!(
        elapsed < COLD_LIMIT,
        "cold tokenizer startup took {elapsed:?}, exceeding {COLD_LIMIT:?}"
    );
}

fn assert_warm_p95_gate(engine: &Engine, fixture: &ScaleFixture) {
    let compression_budget = budget();
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let warmup_started = Instant::now();
    let warmup_graph = ContextGraph::from_text(fixture.input, NodeKind::ToolResponse, counter);
    let warmup = engine.compress(warmup_graph, &compression_budget);
    let warmup_elapsed = warmup_started.elapsed();
    eprintln!("{} warmup={warmup_elapsed:?}", fixture.name);
    assert!(
        !warmup.fallback_used,
        "{}: {:?}",
        fixture.name, warmup.fallback_reason
    );
    assert!(warmup.after_tokens < warmup.before_tokens);

    let gate = measure_within_limit(WARM_P95_LIMIT, GATE_ATTEMPTS, || {
        let mut durations = Vec::with_capacity(GATE_SAMPLES);
        for _ in 0..GATE_SAMPLES {
            let started = Instant::now();
            let graph = ContextGraph::from_text(fixture.input, NodeKind::ToolResponse, counter);
            let result = black_box(engine.compress(black_box(graph), &compression_budget));
            durations.push(started.elapsed());
            assert!(!result.fallback_used, "{:?}", result.fallback_reason);
        }
        durations.sort_unstable();
        let p95 = durations[94];
        // Every attempt is printed, so drift toward the limit stays visible
        // even while the gate passes.
        eprintln!("{} warm p95={p95:?}", fixture.name);
        p95
    });
    assert!(
        gate.passed,
        "{} warm p95 exceeded {WARM_P95_LIMIT:?} on all {GATE_ATTEMPTS} attempts: {:?}",
        fixture.name, gate.attempts
    );
}

fn large_payload_latency(criterion: &mut Criterion) {
    assert_cold_startup_gate();
    let engine = Engine::new(Mode::Safe, counter_for(TokenProvider::OpenAiO200k));
    let compression_budget = budget();

    for fixture in fixtures() {
        assert_warm_p95_gate(&engine, &fixture);
        criterion.bench_function(fixture.name, |bencher| {
            bencher.iter_batched(
                || fixture.input,
                |input| {
                    let graph = ContextGraph::from_text(
                        black_box(input),
                        NodeKind::ToolResponse,
                        counter_for(TokenProvider::OpenAiO200k),
                    );
                    let result = engine.compress(black_box(graph), &compression_budget);
                    assert!(!black_box(result).fallback_used);
                },
                BatchSize::SmallInput,
            );
        });
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = large_payload_latency
}
criterion_main!(benches);
