use std::time::{Duration, Instant};

use compresso_core::{TokenProvider, initialize_tokenizers};
use criterion::{Criterion, criterion_group, criterion_main};

const PROVIDERS: [TokenProvider; 2] = [
    TokenProvider::OpenAiO200k,
    TokenProvider::AnthropicCalibrated,
];

fn tokenizer_startup(criterion: &mut Criterion) {
    let started = Instant::now();
    let cold = initialize_tokenizers(&PROVIDERS);
    let cold_elapsed = started.elapsed();
    assert!(cold.all_ready(), "exact tokenizer startup used a fallback");
    if !cfg!(debug_assertions) {
        assert!(
            cold_elapsed < Duration::from_millis(500),
            "cold tokenizer startup took {cold_elapsed:?}"
        );
    }

    criterion.bench_function("tokenizer_startup_warm", |bencher| {
        bencher.iter(|| {
            let report = initialize_tokenizers(&PROVIDERS);
            assert!(report.all_ready());
        });
    });
}

criterion_group!(benches, tokenizer_startup);
criterion_main!(benches);
