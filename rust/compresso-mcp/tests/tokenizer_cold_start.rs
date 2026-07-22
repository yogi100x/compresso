use std::time::{Duration, Instant};

use compresso_core::{TokenProvider, initialize_tokenizers};

#[test]
fn test_cold_tokenizer_startup_under_five_hundred_ms_in_release() {
    if cfg!(debug_assertions) {
        eprintln!("cold tokenizer startup gate is release-only");
        return;
    }

    let started = Instant::now();
    let report = initialize_tokenizers(&[
        TokenProvider::OpenAiO200k,
        TokenProvider::AnthropicCalibrated,
    ]);
    let elapsed = started.elapsed();

    assert!(
        report.all_ready(),
        "exact tokenizer startup used a fallback"
    );
    assert!(
        elapsed < Duration::from_millis(500),
        "cold tokenizer startup took {elapsed:?}"
    );
}
