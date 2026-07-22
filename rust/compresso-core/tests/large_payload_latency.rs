use std::hint::black_box;
use std::time::{Duration, Instant};

use compresso_core::{
    CompressionBudget, ContextGraph, Engine, Lossiness, MAX_COMPRESSION_INPUT_BYTES, Mode,
    NodeKind, TokenProvider, counter_for,
};

const INPUT_1_MIB: &str =
    include_str!("../../../fixtures/benchmarks/mib-rebuild-stream/input-1mib.log");
const INPUT_4_MIB: &str =
    include_str!("../../../fixtures/benchmarks/mib-rebuild-stream/input-4mib.log");
const INPUT_8_MIB: &str =
    include_str!("../../../fixtures/benchmarks/mib-rebuild-stream/input-8mib.log");
const REBUILD_LINE: &str = "2026-07-22 09:15:12 File change detected: src/components/Dashboard.tsx. Starting incremental compilation... status=rebuilding..\n";
const SAMPLES: usize = 100;
const WARM_P95_LIMIT: Duration = Duration::from_millis(200);

fn budget() -> CompressionBudget {
    CompressionBudget {
        max_tokens: usize::MAX,
        // This test owns the wall-clock gate below and needs complete samples
        // even when diagnosing a regression beyond the product threshold.
        max_latency_ms: u64::MAX,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    }
}

#[test]
fn test_scale_fixtures_are_exact_and_deterministic() {
    for (input, expected_bytes) in [
        (INPUT_1_MIB, 1_usize << 20),
        (INPUT_4_MIB, 4_usize << 20),
        (INPUT_8_MIB, MAX_COMPRESSION_INPUT_BYTES),
    ] {
        assert_eq!(input.len(), expected_bytes);
        assert!(
            input
                .bytes()
                .zip(REBUILD_LINE.bytes().cycle())
                .all(|(actual, expected)| actual == expected)
        );
    }
}

#[test]
#[cfg_attr(debug_assertions, ignore = "release-only large-payload gate")]
fn test_warm_large_payload_p95_under_two_hundred_ms_in_release() {
    let counter = counter_for(TokenProvider::OpenAiO200k);
    counter.count("prewarm");
    let engine = Engine::new(Mode::Safe, counter);

    for (name, input) in [
        ("1_mib", INPUT_1_MIB),
        ("4_mib", INPUT_4_MIB),
        ("8_mib", INPUT_8_MIB),
    ] {
        let graph = ContextGraph::from_text(input, NodeKind::ToolResponse, counter);
        let warmup = engine.compress(graph, &budget());
        assert!(!warmup.fallback_used, "{:?}", warmup.fallback_reason);
        assert!(warmup.after_tokens < warmup.before_tokens);

        let mut durations = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let started = Instant::now();
            let graph = ContextGraph::from_text(input, NodeKind::ToolResponse, counter);
            let result = black_box(engine.compress(graph, &budget()));
            durations.push(started.elapsed());
            assert!(!result.fallback_used, "{:?}", result.fallback_reason);
        }
        durations.sort_unstable();
        let p95 = durations[94];
        eprintln!("rebuild stream {name}: warm p95={p95:?}");
        assert!(
            p95 < WARM_P95_LIMIT,
            "rebuild stream {name} warm p95 {p95:?} exceeded {WARM_P95_LIMIT:?}"
        );
    }
}
