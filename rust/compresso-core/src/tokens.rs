use std::panic::{UnwindSafe, catch_unwind};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tiktoken_rs::CoreBPE;

/// Interim Anthropic calibration factor from
/// `fixtures/provider-payloads/anthropic-counts/anthropic-counts.json`, pending
/// the measured E2-02a value documented by ADR-0017.
pub const CALIBRATION: f64 = 1.15;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenProvider {
    #[default]
    #[serde(rename = "openai_o200k")]
    OpenAiO200k,
    #[serde(rename = "openai_cl100k")]
    OpenAiCl100k,
    AnthropicCalibrated,
    Heuristic,
}

pub trait TokenCounter: Send + Sync {
    fn provider(&self) -> TokenProvider;
    fn count(&self, text: &str) -> usize;
}

struct TiktokenCounter {
    provider: TokenProvider,
    tokenizer: CoreBPE,
}

impl TokenCounter for TiktokenCounter {
    fn provider(&self) -> TokenProvider {
        self.provider
    }

    fn count(&self, text: &str) -> usize {
        self.tokenizer.encode_ordinary(text).len()
    }
}

struct AnthropicCounter {
    _singleton_marker: u8,
}

impl TokenCounter for AnthropicCounter {
    fn provider(&self) -> TokenProvider {
        TokenProvider::AnthropicCalibrated
    }

    fn count(&self, text: &str) -> usize {
        let base_count = counter_for(TokenProvider::OpenAiO200k).count(text);
        ((base_count as f64) * CALIBRATION).ceil() as usize
    }
}

struct HeuristicCounter {
    _singleton_marker: u8,
}

impl TokenCounter for HeuristicCounter {
    fn provider(&self) -> TokenProvider {
        TokenProvider::Heuristic
    }

    fn count(&self, text: &str) -> usize {
        text.len().div_ceil(4)
    }
}

static O200K_COUNTER: OnceLock<Option<TiktokenCounter>> = OnceLock::new();
static CL100K_COUNTER: OnceLock<Option<TiktokenCounter>> = OnceLock::new();
static ANTHROPIC_COUNTER: AnthropicCounter = AnthropicCounter {
    _singleton_marker: 0,
};
static HEURISTIC_COUNTER: HeuristicCounter = HeuristicCounter {
    _singleton_marker: 0,
};

fn initialize_tiktoken<F, E>(provider: TokenProvider, initializer: F) -> Option<TiktokenCounter>
where
    F: FnOnce() -> Result<CoreBPE, E> + UnwindSafe,
{
    match catch_unwind(initializer) {
        Ok(Ok(tokenizer)) => Some(TiktokenCounter {
            provider,
            tokenizer,
        }),
        Ok(Err(_)) | Err(_) => None,
    }
}

fn exact_or_heuristic<F, E>(
    cell: &'static OnceLock<Option<TiktokenCounter>>,
    provider: TokenProvider,
    initializer: F,
) -> &'static dyn TokenCounter
where
    F: FnOnce() -> Result<CoreBPE, E> + UnwindSafe,
{
    match cell.get_or_init(|| initialize_tiktoken(provider, initializer)) {
        Some(counter) => counter,
        None => &HEURISTIC_COUNTER,
    }
}

/// Returns the process-wide counter for `provider`.
///
/// Tokenizers initialize at most once on first use. If an embedded tokenizer
/// cannot initialize, the returned singleton is honestly tagged as
/// [`TokenProvider::Heuristic`].
pub fn counter_for(provider: TokenProvider) -> &'static dyn TokenCounter {
    match provider {
        TokenProvider::OpenAiO200k => exact_or_heuristic(
            &O200K_COUNTER,
            TokenProvider::OpenAiO200k,
            tiktoken_rs::o200k_base,
        ),
        TokenProvider::OpenAiCl100k => exact_or_heuristic(
            &CL100K_COUNTER,
            TokenProvider::OpenAiCl100k,
            tiktoken_rs::cl100k_base,
        ),
        TokenProvider::AnthropicCalibrated => {
            let o200k = counter_for(TokenProvider::OpenAiO200k);
            if o200k.provider() == TokenProvider::OpenAiO200k {
                &ANTHROPIC_COUNTER
            } else {
                &HEURISTIC_COUNTER
            }
        }
        TokenProvider::Heuristic => &HEURISTIC_COUNTER,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde::Deserialize;

    use super::*;

    const CALIBRATION_FIXTURE: &str =
        include_str!("../../../fixtures/provider-payloads/anthropic-counts/anthropic-counts.json");

    #[derive(Deserialize)]
    struct CalibrationFixture {
        samples: Vec<CalibrationSample>,
    }

    #[derive(Deserialize)]
    struct CalibrationSample {
        text: String,
        o200k_tokens: usize,
        anthropic_tokens: Option<usize>,
    }

    fn calibration_fixture() -> CalibrationFixture {
        serde_json::from_str(CALIBRATION_FIXTURE)
            .expect("the committed Anthropic calibration fixture should be valid JSON")
    }

    #[test]
    fn test_o200k_hello_world() {
        assert_eq!(
            counter_for(TokenProvider::OpenAiO200k).count("hello world"),
            2
        );
    }

    #[test]
    fn test_exact_counters_match_tiktoken_on_representative_text() {
        let o200k = tiktoken_rs::o200k_base().expect("o200k should initialize from embedded data");
        let cl100k =
            tiktoken_rs::cl100k_base().expect("cl100k should initialize from embedded data");
        let samples = [
            "hello world",
            "fn main() { println!(\"hello\"); }",
            "naive cafe resume — ✅ 完了 · компрессия завершена",
            "{\"schema_version\":1,\"items\":[1,2,3]}",
        ];

        for sample in samples {
            assert_eq!(
                counter_for(TokenProvider::OpenAiO200k).count(sample),
                o200k.encode_ordinary(sample).len()
            );
            assert_eq!(
                counter_for(TokenProvider::OpenAiCl100k).count(sample),
                cl100k.encode_ordinary(sample).len()
            );
        }
    }

    #[test]
    fn test_empty_string_zero() {
        for provider in [
            TokenProvider::OpenAiO200k,
            TokenProvider::OpenAiCl100k,
            TokenProvider::AnthropicCalibrated,
            TokenProvider::Heuristic,
        ] {
            assert_eq!(counter_for(provider).count(""), 0);
        }
    }

    #[test]
    fn test_calibrated_is_scaled_o200k() {
        let fixture = calibration_fixture();
        let calibrated = counter_for(TokenProvider::AnthropicCalibrated);
        let o200k = counter_for(TokenProvider::OpenAiO200k);

        for sample in fixture.samples {
            assert_eq!(o200k.count(&sample.text), sample.o200k_tokens);
            assert_eq!(
                calibrated.count(&sample.text),
                ((sample.o200k_tokens as f64) * CALIBRATION).ceil() as usize
            );
        }
    }

    #[test]
    fn test_calibration_accuracy_gated_on_fixture() {
        let fixture = calibration_fixture();
        if fixture
            .samples
            .iter()
            .any(|sample| sample.anthropic_tokens.is_none())
        {
            eprintln!("calibration pending (E2-02a)");
            return;
        }

        let calibrated = counter_for(TokenProvider::AnthropicCalibrated);
        for sample in fixture.samples {
            let expected = sample
                .anthropic_tokens
                .expect("the fixture gate verified that every count is present");
            let estimated = calibrated.count(&sample.text);
            let relative_error = estimated.abs_diff(expected) as f64 / expected as f64;
            assert!(
                relative_error <= 0.10,
                "calibrated count {estimated} differs from Anthropic count {expected} by more than 10%"
            );
        }
    }

    #[test]
    fn test_counter_for_singleton() {
        for provider in [
            TokenProvider::OpenAiO200k,
            TokenProvider::OpenAiCl100k,
            TokenProvider::AnthropicCalibrated,
            TokenProvider::Heuristic,
        ] {
            assert!(std::ptr::eq(counter_for(provider), counter_for(provider)));
        }
    }

    #[test]
    fn test_provider_tag_roundtrip() {
        let expected = [
            (TokenProvider::OpenAiO200k, "\"openai_o200k\""),
            (TokenProvider::OpenAiCl100k, "\"openai_cl100k\""),
            (
                TokenProvider::AnthropicCalibrated,
                "\"anthropic_calibrated\"",
            ),
            (TokenProvider::Heuristic, "\"heuristic\""),
        ];

        for (provider, serialized) in expected {
            let counter = counter_for(provider);
            assert_eq!(counter.provider(), provider);
            assert_eq!(
                serde_json::to_string(&counter.provider()).expect("provider tag should serialize"),
                serialized
            );
            assert_eq!(
                serde_json::from_str::<TokenProvider>(serialized)
                    .expect("provider tag should deserialize"),
                provider
            );
        }
    }

    #[test]
    fn test_failed_tokenizer_init_returns_honest_heuristic() {
        static FAILED_COUNTER: OnceLock<Option<TiktokenCounter>> = OnceLock::new();

        let counter = exact_or_heuristic(&FAILED_COUNTER, TokenProvider::OpenAiO200k, || {
            Err::<CoreBPE, _>("injected initialization failure")
        });

        assert_eq!(counter.provider(), TokenProvider::Heuristic);
        assert_eq!(counter.count("abcde"), 2);
    }

    #[test]
    fn test_heuristic_uses_ceiling_bytes_per_four() {
        let heuristic = counter_for(TokenProvider::Heuristic);
        assert_eq!(heuristic.count(""), 0);
        assert_eq!(heuristic.count("a"), 1);
        assert_eq!(heuristic.count("abcd"), 1);
        assert_eq!(heuristic.count("abcde"), 2);
        assert_eq!(heuristic.count("✅"), 1);
        assert_eq!(heuristic.count("✅a"), 1);
        assert_eq!(heuristic.count("✅ab"), 2);
    }

    #[test]
    fn test_warm_o200k_one_mib_under_two_hundred_ms_in_release() {
        if cfg!(debug_assertions) {
            eprintln!("release-only token counter latency assertion skipped in debug mode");
            return;
        }

        let counter = counter_for(TokenProvider::OpenAiO200k);
        const LOG_LINE: &str = "hello world\n";
        let input = LOG_LINE.repeat((1 << 20) / LOG_LINE.len() + 1);
        counter.count("prewarm");

        let started = Instant::now();
        let count = counter.count(&input[..1 << 20]);
        let elapsed = started.elapsed();

        assert!(count > 0);
        assert!(
            elapsed < Duration::from_millis(200),
            "warm 1 MiB token count took {elapsed:?}"
        );
    }
}
