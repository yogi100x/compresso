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

/// Result of eagerly resolving the tokenizer providers needed by a process.
///
/// A provider appears in `fallback` when initialization failed and
/// [`counter_for`] returned the heuristic counter instead. Both collections
/// preserve first-request order and omit duplicate providers.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct TokenizerInitialization {
    pub initialized: Vec<TokenProvider>,
    pub fallback: Vec<TokenProvider>,
}

impl TokenizerInitialization {
    /// Returns true when every requested provider initialized exactly.
    pub fn all_ready(&self) -> bool {
        self.fallback.is_empty()
    }
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
        if let Some((line, repeats)) = exact_repeated_line(text)
            && let Some(total) = self.count_pieces(line).checked_mul(repeats)
        {
            return total;
        }
        self.count_pieces(text)
    }
}

impl TiktokenCounter {
    fn count_pieces(&self, text: &str) -> usize {
        bounded_pieces(text).fold(0_usize, |total, piece| {
            total.saturating_add(self.count_piece(piece))
        })
    }

    /// Counts one bounded piece. A tokenizer panic must never take the
    /// caller down (fail-open), so it degrades to the byte heuristic for
    /// that piece.
    fn count_piece(&self, piece: &str) -> usize {
        catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.tokenizer.encode_ordinary(piece).len()
        }))
        .unwrap_or_else(|_| piece.len().div_ceil(4))
    }
}

/// Longest run without whitespace handed to the tokenizer in one piece.
///
/// The tokenizer treats such a run as a single piece and merges it in
/// quadratic time; past roughly a megabyte its matcher gives up and the
/// library panics. Ordinary text and code never come near this length, so
/// they are counted in one call and stay exact.
pub(crate) const MAX_UNBROKEN_RUN_BYTES: usize = 1024;

/// Splits `text` so that no piece holds a longer unbroken run than
/// [`MAX_UNBROKEN_RUN_BYTES`] (plus the tail of one character). Text without
/// such a run comes back as a single piece. Cuts fall on character
/// boundaries, and each cut can add at most one token to the total.
fn bounded_pieces(text: &str) -> impl Iterator<Item = &str> {
    let mut piece_start = 0;
    let mut run_start = 0;
    let mut characters = text.char_indices();
    std::iter::from_fn(move || {
        if piece_start >= text.len() {
            return None;
        }
        for (index, character) in characters.by_ref() {
            if character.is_whitespace() {
                run_start = index + character.len_utf8();
            } else if index - run_start >= MAX_UNBROKEN_RUN_BYTES {
                let piece = &text[piece_start..index];
                piece_start = index;
                run_start = index;
                return Some(piece);
            }
        }
        let piece = &text[piece_start..];
        piece_start = text.len();
        Some(piece)
    })
}

/// Recognizes a byte-exact repeated nonblank line stream.
///
/// Tiktoken's ordinary regex treats a nonblank line ending as a stable piece
/// boundary, so an identical line has the same encoding at every repetition.
/// Blank-line runs are excluded because the regex may group adjacent newlines.
pub(crate) fn exact_repeated_line(text: &str) -> Option<(&str, usize)> {
    let line_end = text.as_bytes().iter().position(|byte| *byte == b'\n')? + 1;
    if line_end == text.len() || !text.len().is_multiple_of(line_end) {
        return None;
    }
    let line = &text[..line_end];
    if line.trim().is_empty() {
        return None;
    }
    let repeats = text.len() / line_end;
    text.as_bytes()
        .chunks_exact(line_end)
        .all(|chunk| chunk == line.as_bytes())
        .then_some((line, repeats))
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
        anthropic_calibrated_count(base_count)
    }
}

/// Derives the calibrated Anthropic estimate from an exact o200k count.
///
/// Callers that already counted the same bytes with o200k should use this
/// helper instead of tokenizing them a second time through
/// [`TokenProvider::AnthropicCalibrated`].
pub fn anthropic_calibrated_count(o200k_tokens: usize) -> usize {
    ((o200k_tokens as f64) * CALIBRATION).ceil() as usize
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

/// Eagerly initializes each requested process-wide tokenizer singleton.
///
/// Initialization is idempotent. Failures are represented in the returned
/// report and remain fail-open through the honestly tagged heuristic counter.
pub fn initialize_tokenizers(providers: &[TokenProvider]) -> TokenizerInitialization {
    initialization_report(providers, |provider| counter_for(provider).provider())
}

fn initialization_report(
    providers: &[TokenProvider],
    mut resolve: impl FnMut(TokenProvider) -> TokenProvider,
) -> TokenizerInitialization {
    let mut initialized = Vec::new();
    let mut fallback = Vec::new();
    for &provider in providers {
        if initialized.contains(&provider) || fallback.contains(&provider) {
            continue;
        }
        if resolve(provider) == provider {
            initialized.push(provider);
        } else {
            fallback.push(provider);
        }
    }
    TokenizerInitialization {
        initialized,
        fallback,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock as TestOnceLock;
    use std::time::{Duration, Instant};

    use proptest::prelude::*;
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

    fn raw_o200k() -> &'static CoreBPE {
        static RAW: TestOnceLock<CoreBPE> = TestOnceLock::new();
        RAW.get_or_init(|| {
            tiktoken_rs::o200k_base().expect("the embedded o200k tokenizer should initialize")
        })
    }

    fn raw_cl100k() -> &'static CoreBPE {
        static RAW: TestOnceLock<CoreBPE> = TestOnceLock::new();
        RAW.get_or_init(|| {
            tiktoken_rs::cl100k_base().expect("the embedded cl100k tokenizer should initialize")
        })
    }

    #[test]
    fn test_o200k_hello_world() {
        assert_eq!(
            counter_for(TokenProvider::OpenAiO200k).count("hello world"),
            2
        );
    }

    #[test]
    fn test_megabyte_unbroken_letter_run_is_counted_instead_of_panicking() {
        let run = "a".repeat(1024 * 1024);
        let started = Instant::now();

        let count = counter_for(TokenProvider::OpenAiO200k).count(&run);

        assert!(count > 0);
        assert!(count <= run.len());
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "counting took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn test_unbroken_cjk_run_is_counted_in_bounded_time() {
        let run = "完了圧縮".repeat(32 * 1024);
        let started = Instant::now();

        let count = counter_for(TokenProvider::OpenAiCl100k).count(&run);

        assert!(count > 0);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "counting took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn test_run_at_the_unbroken_limit_stays_exact() {
        let mut text = String::from("prefix ");
        text.push_str(&"k".repeat(MAX_UNBROKEN_RUN_BYTES));
        text.push_str(" suffix\n");

        assert_eq!(
            counter_for(TokenProvider::OpenAiO200k).count(&text),
            raw_o200k().encode_ordinary(&text).len()
        );
    }

    #[test]
    fn test_split_long_run_overcounts_by_at_most_one_token_per_cut() {
        let run = "résumé".repeat(1500);
        let text = format!("before {run} after");
        let exact = raw_o200k().encode_ordinary(&text).len();
        let cuts = run.len() / MAX_UNBROKEN_RUN_BYTES + 1;

        let counted = counter_for(TokenProvider::OpenAiO200k).count(&text);

        assert!(
            counted >= exact && counted <= exact + cuts,
            "counted {counted}, exact {exact}, cuts {cuts}"
        );
    }

    #[test]
    fn test_bounded_pieces_cover_the_text_and_never_split_a_character() {
        let text = format!("a b {} c\n{}", "é".repeat(3000), "z".repeat(10));
        let pieces: Vec<&str> = bounded_pieces(&text).collect();

        assert_eq!(pieces.concat(), text);
        assert!(pieces.len() > 1);
        for piece in &pieces {
            let longest_run = piece
                .split(char::is_whitespace)
                .map(str::len)
                .max()
                .unwrap_or(0);
            assert!(
                longest_run <= MAX_UNBROKEN_RUN_BYTES + 4,
                "run of {longest_run}"
            );
        }
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
    fn test_repeated_line_fast_path_matches_independent_full_encoding() {
        let lines = [
            "2026-07-22 09:15:12 rebuild complete status=ok\n",
            "  indented unicode ✅ 完了\r\n",
            "warning: repeated compiler diagnostic E0308\n",
        ];
        let providers = [
            (TokenProvider::OpenAiO200k, tiktoken_rs::o200k_base()),
            (TokenProvider::OpenAiCl100k, tiktoken_rs::cl100k_base()),
        ];

        for (provider, raw) in providers {
            let raw = raw.expect("the embedded tokenizer should initialize");
            for line in lines {
                for repeats in [2, 3, 257] {
                    let input = line.repeat(repeats);
                    assert_eq!(
                        counter_for(provider).count(&input),
                        raw.encode_ordinary(&input).len(),
                        "provider={provider:?} line={line:?} repeats={repeats}"
                    );
                }
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn prop_repeated_line_fast_path_matches_raw_tokenizers(
            pieces in proptest::collection::vec(
                prop_oneof![
                    Just("ascii"),
                    Just("123"),
                    Just(" "),
                    Just("-"),
                    Just("_"),
                    Just("✅"),
                    Just("完了"),
                    Just("é"),
                    Just("компрессия"),
                ],
                0..16,
            ),
            trailing_spaces in 0_usize..5,
            crlf in any::<bool>(),
            repeats in 2_usize..33,
        ) {
            let ending = if crlf { "\r\n" } else { "\n" };
            let line = format!(
                "required{}{}{}",
                pieces.concat(),
                " ".repeat(trailing_spaces),
                ending,
            );
            let input = line.repeat(repeats);
            prop_assert_eq!(
                counter_for(TokenProvider::OpenAiO200k).count(&input),
                raw_o200k().encode_ordinary(&input).len()
            );
            prop_assert_eq!(
                counter_for(TokenProvider::OpenAiCl100k).count(&input),
                raw_cl100k().encode_ordinary(&input).len()
            );
        }
    }

    #[test]
    fn test_repeated_line_fast_path_rejects_partial_and_blank_runs() {
        assert_eq!(exact_repeated_line("same\nsame\n"), Some(("same\n", 2)));
        assert_eq!(exact_repeated_line("same\nsame"), None);
        assert_eq!(exact_repeated_line("\n\n\n"), None);
        assert_eq!(exact_repeated_line("single\n"), None);
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
                anthropic_calibrated_count(sample.o200k_tokens)
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
    fn test_eager_initialization_reports_exact_providers_once() {
        let report = initialize_tokenizers(&[
            TokenProvider::OpenAiO200k,
            TokenProvider::AnthropicCalibrated,
            TokenProvider::OpenAiO200k,
        ]);

        assert!(report.all_ready());
        assert_eq!(
            report.initialized,
            vec![
                TokenProvider::OpenAiO200k,
                TokenProvider::AnthropicCalibrated
            ]
        );
        assert!(report.fallback.is_empty());
    }

    #[test]
    fn test_eager_initialization_reports_honest_fallback() {
        let report = initialization_report(
            &[
                TokenProvider::OpenAiO200k,
                TokenProvider::Heuristic,
                TokenProvider::OpenAiO200k,
            ],
            |provider| match provider {
                TokenProvider::OpenAiO200k => TokenProvider::Heuristic,
                provider => provider,
            },
        );

        assert!(!report.all_ready());
        assert_eq!(report.initialized, vec![TokenProvider::Heuristic]);
        assert_eq!(report.fallback, vec![TokenProvider::OpenAiO200k]);
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
