//! Offline Anthropic token-count calibration primitives.
//!
//! The transport is injected so parsing, validation, envelope subtraction, and
//! proposal construction remain deterministic and network-free in tests. The
//! feature-gated maintainer binary owns the actual HTTP transport.

use std::collections::BTreeSet;
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

pub const CALIBRATION_SCHEMA_VERSION: u32 = 1;
pub const CALIBRATION_SAMPLE_COUNT: usize = 10;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationFixture {
    pub schema_version: u32,
    pub anthropic_model: Option<String>,
    pub counted_at: Option<String>,
    pub calibration_factor: Option<f64>,
    pub samples: Vec<CalibrationSample>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationSample {
    pub id: String,
    pub text: String,
    pub chars: usize,
    pub o200k_tokens: usize,
    pub anthropic_tokens: Option<usize>,
}

impl CalibrationFixture {
    pub fn parse(input: &[u8]) -> Result<Self, CalibrationError> {
        let fixture = serde_json::from_slice(input).map_err(CalibrationError::FixtureJson)?;
        validate_fixture(&fixture)?;
        Ok(fixture)
    }

    pub fn load(path: &Path) -> Result<Self, CalibrationError> {
        let input = fs::read(path).map_err(|source| CalibrationError::Io {
            operation: "read",
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&input)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CountTokensRequest<'a> {
    pub model: &'a str,
    pub text: &'a str,
}

pub trait CountTokensTransport {
    fn count_tokens(&mut self, request: CountTokensRequest<'_>) -> Result<usize, CountTokensError>;
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct CountTokensError {
    message: String,
}

impl CountTokensError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CalibrationProposal {
    pub fixture: CalibrationFixture,
    pub empty_message_tokens: usize,
}

/// Build a complete replacement fixture without mutating or writing the input.
///
/// The empty-message request is issued once, followed by one request per sample.
/// A proposal is returned only after every response has been received and
/// validated.
pub fn propose_calibration(
    fixture: &CalibrationFixture,
    model: &str,
    counted_at: &str,
    transport: &mut impl CountTokensTransport,
) -> Result<CalibrationProposal, CalibrationError> {
    validate_fixture(fixture)?;
    validate_run_metadata(model, counted_at)?;

    let empty_message_tokens = transport
        .count_tokens(CountTokensRequest { model, text: "" })
        .map_err(|source| CalibrationError::CountTokens {
            sample_id: "empty-message-envelope".to_owned(),
            source,
        })?;

    let mut measured = Vec::with_capacity(fixture.samples.len());
    for sample in &fixture.samples {
        let raw_tokens = transport
            .count_tokens(CountTokensRequest {
                model,
                text: &sample.text,
            })
            .map_err(|source| CalibrationError::CountTokens {
                sample_id: sample.id.clone(),
                source,
            })?;
        let content_tokens = raw_tokens
            .checked_sub(empty_message_tokens)
            .ok_or_else(|| CalibrationError::EnvelopeExceedsCount {
                sample_id: sample.id.clone(),
                raw_tokens,
                empty_message_tokens,
            })?;
        if content_tokens == 0 {
            return Err(CalibrationError::ZeroMeasuredTokens {
                sample_id: sample.id.clone(),
            });
        }
        measured.push(content_tokens);
    }

    let factor = median_ratio(
        fixture
            .samples
            .iter()
            .zip(&measured)
            .map(|(sample, measured)| *measured as f64 / sample.o200k_tokens as f64),
    )?;

    let mut proposed = fixture.clone();
    proposed.anthropic_model = Some(model.to_owned());
    proposed.counted_at = Some(counted_at.to_owned());
    proposed.calibration_factor = Some(factor);
    for (sample, count) in proposed.samples.iter_mut().zip(measured) {
        sample.anthropic_tokens = Some(count);
    }
    validate_fixture(&proposed)?;

    Ok(CalibrationProposal {
        fixture: proposed,
        empty_message_tokens,
    })
}

/// Calibrate and atomically replace `path` only after all responses succeed.
///
/// Credential validation happens before the fixture or transport is touched,
/// making the no-key path side-effect free.
pub fn calibrate_file(
    path: &Path,
    model: &str,
    counted_at: &str,
    api_key: Option<&str>,
    transport: &mut impl CountTokensTransport,
) -> Result<CalibrationProposal, CalibrationError> {
    require_api_key(api_key)?;
    let fixture = CalibrationFixture::load(path)?;
    let proposal = propose_calibration(&fixture, model, counted_at, transport)?;
    write_fixture_atomic(path, &proposal.fixture)?;
    Ok(proposal)
}

pub fn require_api_key(api_key: Option<&str>) -> Result<(), CalibrationError> {
    match api_key.map(str::trim) {
        Some(key) if !key.is_empty() => Ok(()),
        _ => Err(CalibrationError::MissingApiKey),
    }
}

pub fn validate_fixture(fixture: &CalibrationFixture) -> Result<(), CalibrationError> {
    if fixture.schema_version != CALIBRATION_SCHEMA_VERSION {
        return Err(CalibrationError::UnsupportedSchemaVersion {
            actual: fixture.schema_version,
        });
    }
    if fixture.samples.len() != CALIBRATION_SAMPLE_COUNT {
        return Err(CalibrationError::WrongSampleCount {
            actual: fixture.samples.len(),
        });
    }

    let mut ids = BTreeSet::new();
    for sample in &fixture.samples {
        if sample.id.trim().is_empty() {
            return Err(CalibrationError::EmptySampleId);
        }
        if !ids.insert(sample.id.as_str()) {
            return Err(CalibrationError::DuplicateSampleId {
                sample_id: sample.id.clone(),
            });
        }
        if sample.text.is_empty() {
            return Err(CalibrationError::EmptySampleText {
                sample_id: sample.id.clone(),
            });
        }
        let actual_chars = sample.text.chars().count();
        if sample.chars != actual_chars {
            return Err(CalibrationError::CharacterCountMismatch {
                sample_id: sample.id.clone(),
                declared: sample.chars,
                actual: actual_chars,
            });
        }
        if sample.o200k_tokens == 0 {
            return Err(CalibrationError::ZeroO200kTokens {
                sample_id: sample.id.clone(),
            });
        }
        if sample.anthropic_tokens == Some(0) {
            return Err(CalibrationError::ZeroMeasuredTokens {
                sample_id: sample.id.clone(),
            });
        }
    }

    let populated_counts = fixture
        .samples
        .iter()
        .filter(|sample| sample.anthropic_tokens.is_some())
        .count();
    let metadata_fields = [
        fixture.anthropic_model.is_some(),
        fixture.counted_at.is_some(),
        fixture.calibration_factor.is_some(),
    ];
    let pending = populated_counts == 0 && metadata_fields.iter().all(|present| !present);
    let complete =
        populated_counts == fixture.samples.len() && metadata_fields.iter().all(|present| *present);
    if !pending && !complete {
        return Err(CalibrationError::PartialCalibration);
    }

    if complete {
        let model = fixture
            .anthropic_model
            .as_deref()
            .expect("complete metadata was checked");
        let counted_at = fixture
            .counted_at
            .as_deref()
            .expect("complete metadata was checked");
        validate_run_metadata(model, counted_at)?;
        let expected = median_ratio(fixture.samples.iter().map(|sample| {
            sample
                .anthropic_tokens
                .expect("complete counts were checked") as f64
                / sample.o200k_tokens as f64
        }))?;
        let actual = fixture
            .calibration_factor
            .expect("complete metadata was checked");
        if !actual.is_finite() || (actual - expected).abs() > f64::EPSILON {
            return Err(CalibrationError::CalibrationFactorMismatch { expected, actual });
        }
    }

    Ok(())
}

pub fn median_ratio(ratios: impl IntoIterator<Item = f64>) -> Result<f64, CalibrationError> {
    let mut ratios = ratios.into_iter().collect::<Vec<_>>();
    if ratios.is_empty()
        || ratios
            .iter()
            .any(|ratio| !ratio.is_finite() || *ratio <= 0.0)
    {
        return Err(CalibrationError::InvalidRatios);
    }
    ratios.sort_by(f64::total_cmp);
    let middle = ratios.len() / 2;
    let median = if ratios.len().is_multiple_of(2) {
        (ratios[middle - 1] + ratios[middle]) / 2.0
    } else {
        ratios[middle]
    };
    Ok(round_three_decimals(median))
}

pub fn write_fixture_atomic(
    path: &Path,
    fixture: &CalibrationFixture,
) -> Result<(), CalibrationError> {
    validate_fixture(fixture)?;
    let mut encoded = serde_json::to_vec_pretty(fixture).map_err(CalibrationError::FixtureJson)?;
    encoded.push(b'\n');

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CalibrationError::InvalidOutputPath {
            path: path.to_path_buf(),
        })?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{sequence}.tmp",
        std::process::id()
    ));

    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| CalibrationError::Io {
                operation: "create temporary calibration fixture",
                path: temporary.clone(),
                source,
            })?;
        if let Ok(metadata) = fs::metadata(path) {
            file.set_permissions(metadata.permissions())
                .map_err(|source| CalibrationError::Io {
                    operation: "preserve calibration fixture permissions",
                    path: temporary.clone(),
                    source,
                })?;
        }
        file.write_all(&encoded)
            .and_then(|()| file.sync_all())
            .map_err(|source| CalibrationError::Io {
                operation: "write temporary calibration fixture",
                path: temporary.clone(),
                source,
            })?;
        fs::rename(&temporary, path).map_err(|source| CalibrationError::Io {
            operation: "atomically replace calibration fixture",
            path: path.to_path_buf(),
            source,
        })?;
        sync_parent_directory(parent)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn validate_run_metadata(model: &str, counted_at: &str) -> Result<(), CalibrationError> {
    if model.trim().is_empty() {
        return Err(CalibrationError::EmptyModel);
    }
    if counted_at.trim().is_empty() {
        return Err(CalibrationError::EmptyCountedAt);
    }
    Ok(())
}

fn round_three_decimals(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<(), CalibrationError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CalibrationError::Io {
            operation: "sync calibration fixture directory",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<(), CalibrationError> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum CalibrationError {
    #[error("ANTHROPIC_API_KEY is required for calibration")]
    MissingApiKey,
    #[error("unsupported calibration schema version {actual}; expected 1")]
    UnsupportedSchemaVersion { actual: u32 },
    #[error("calibration fixture has {actual} samples; expected 10")]
    WrongSampleCount { actual: usize },
    #[error("calibration sample id must not be empty")]
    EmptySampleId,
    #[error("duplicate calibration sample id `{sample_id}`")]
    DuplicateSampleId { sample_id: String },
    #[error("calibration sample `{sample_id}` has empty text")]
    EmptySampleText { sample_id: String },
    #[error(
        "calibration sample `{sample_id}` declares {declared} characters but contains {actual}"
    )]
    CharacterCountMismatch {
        sample_id: String,
        declared: usize,
        actual: usize,
    },
    #[error("calibration sample `{sample_id}` has zero o200k tokens")]
    ZeroO200kTokens { sample_id: String },
    #[error("calibration sample `{sample_id}` has zero measured Anthropic tokens")]
    ZeroMeasuredTokens { sample_id: String },
    #[error("calibration fixture is partially populated")]
    PartialCalibration,
    #[error("calibration model must not be empty")]
    EmptyModel,
    #[error("calibration counted_at must not be empty")]
    EmptyCountedAt,
    #[error("calibration ratios must be finite, positive, and non-empty")]
    InvalidRatios,
    #[error("calibration factor is {actual}; expected rounded median {expected}")]
    CalibrationFactorMismatch { expected: f64, actual: f64 },
    #[error("Anthropic count_tokens failed for `{sample_id}`: {source}")]
    CountTokens {
        sample_id: String,
        #[source]
        source: CountTokensError,
    },
    #[error(
        "Anthropic count {raw_tokens} for `{sample_id}` is below empty-message overhead {empty_message_tokens}"
    )]
    EnvelopeExceedsCount {
        sample_id: String,
        raw_tokens: usize,
        empty_message_tokens: usize,
    },
    #[error("calibration fixture JSON is invalid: {0}")]
    FixtureJson(#[source] serde_json::Error),
    #[error("calibration output path is invalid: {path}", path = path.display())]
    InvalidOutputPath { path: PathBuf },
    #[error("failed to {operation} at {path}: {source}", path = path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use tempfile::TempDir;

    use super::*;

    const COUNTED_AT: &str = "2026-07-22T12:34:56Z";
    const MODEL: &str = "claude-test-model";

    fn pending_fixture() -> CalibrationFixture {
        CalibrationFixture {
            schema_version: 1,
            anthropic_model: None,
            counted_at: None,
            calibration_factor: None,
            samples: (1..=10)
                .map(|number| {
                    let text = format!("sample {number}");
                    CalibrationSample {
                        id: format!("s{number:02}"),
                        chars: text.chars().count(),
                        text,
                        o200k_tokens: number * 10,
                        anthropic_tokens: None,
                    }
                })
                .collect(),
        }
    }

    fn encoded_pending_fixture() -> Vec<u8> {
        let mut encoded =
            serde_json::to_vec_pretty(&pending_fixture()).expect("fixture serializes");
        encoded.push(b'\n');
        encoded
    }

    struct FakeTransport {
        responses: VecDeque<Result<usize, CountTokensError>>,
        requests: Vec<(String, String)>,
        unchanged_file: Option<(PathBuf, Vec<u8>)>,
    }

    impl FakeTransport {
        fn successful(adjusted_counts: &[usize], overhead: usize) -> Self {
            let responses = std::iter::once(Ok(overhead))
                .chain(adjusted_counts.iter().map(|count| Ok(count + overhead)))
                .collect();
            Self {
                responses,
                requests: Vec::new(),
                unchanged_file: None,
            }
        }
    }

    impl CountTokensTransport for FakeTransport {
        fn count_tokens(
            &mut self,
            request: CountTokensRequest<'_>,
        ) -> Result<usize, CountTokensError> {
            if let Some((path, original)) = &self.unchanged_file {
                assert_eq!(
                    fs::read(path).expect("fixture remains readable while counting"),
                    *original,
                    "the output changed before every response succeeded"
                );
            }
            self.requests
                .push((request.model.to_owned(), request.text.to_owned()));
            self.responses
                .pop_front()
                .expect("the test provided one response per expected request")
        }
    }

    #[test]
    fn test_parse_committed_pending_fixture() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/provider-payloads/anthropic-counts/anthropic-counts.json");

        let fixture = CalibrationFixture::load(&path).expect("committed fixture should validate");

        assert_eq!(fixture.samples.len(), 10);
        assert!(
            fixture
                .samples
                .iter()
                .all(|sample| sample.anthropic_tokens.is_none())
        );
    }

    #[test]
    fn test_calibration_subtracts_one_empty_envelope_and_rounds_median() {
        let fixture = pending_fixture();
        let adjusted = [11, 22, 33, 44, 55, 66, 77, 88, 99, 111];
        let mut transport = FakeTransport::successful(&adjusted, 7);

        let proposal = propose_calibration(&fixture, MODEL, COUNTED_AT, &mut transport)
            .expect("all deterministic responses should calibrate");

        assert_eq!(proposal.empty_message_tokens, 7);
        assert_eq!(transport.requests.len(), 11);
        assert_eq!(transport.requests[0], (MODEL.to_owned(), String::new()));
        assert_eq!(
            transport.requests[1..]
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>(),
            fixture
                .samples
                .iter()
                .map(|sample| sample.text.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            proposal
                .fixture
                .samples
                .iter()
                .map(|sample| sample.anthropic_tokens.expect("proposal is complete"))
                .collect::<Vec<_>>(),
            adjusted
        );
        assert_eq!(proposal.fixture.anthropic_model.as_deref(), Some(MODEL));
        assert_eq!(proposal.fixture.counted_at.as_deref(), Some(COUNTED_AT));
        assert_eq!(proposal.fixture.calibration_factor, Some(1.1));
        assert!(
            fixture.anthropic_model.is_none(),
            "the input was not mutated"
        );
    }

    #[test]
    fn test_calibration_factor_is_even_median_ratio_rounded_to_three_decimals() {
        let factor =
            median_ratio([1.0044, 1.0011, 1.0055, 1.0022]).expect("positive ratios have a median");

        assert_eq!(factor, 1.003);
    }

    #[test]
    fn test_failed_response_never_changes_fixture_file() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("anthropic-counts.json");
        let original = encoded_pending_fixture();
        fs::write(&path, &original).expect("write pending fixture");
        let mut responses = VecDeque::from([
            Ok(7),
            Ok(18),
            Ok(29),
            Err(CountTokensError::new("injected transport failure")),
        ]);
        let mut transport = FakeTransport {
            responses: std::mem::take(&mut responses),
            requests: Vec::new(),
            unchanged_file: Some((path.clone(), original.clone())),
        };

        let error = calibrate_file(&path, MODEL, COUNTED_AT, Some("test-key"), &mut transport)
            .expect_err("a failed sample response must abort the proposal");

        assert!(matches!(error, CalibrationError::CountTokens { .. }));
        assert_eq!(fs::read(&path).expect("read unchanged fixture"), original);
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("list output directory")
                .count(),
            1
        );
    }

    #[test]
    fn test_calibrate_requires_api_key_and_writes_nothing() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("anthropic-counts.json");
        let original = encoded_pending_fixture();
        fs::write(&path, &original).expect("write pending fixture");
        let mut transport = FakeTransport::successful(&[10; 10], 5);

        let error = calibrate_file(&path, MODEL, COUNTED_AT, None, &mut transport)
            .expect_err("a missing API key must fail before any work");

        assert!(matches!(error, CalibrationError::MissingApiKey));
        assert!(transport.requests.is_empty());
        assert_eq!(fs::read(&path).expect("read unchanged fixture"), original);
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("list output directory")
                .count(),
            1
        );
    }

    #[test]
    fn test_successful_file_calibration_atomically_writes_complete_fixture() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("anthropic-counts.json");
        let original = encoded_pending_fixture();
        fs::write(&path, &original).expect("write pending fixture");
        let adjusted = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        let mut transport = FakeTransport::successful(&adjusted, 4);
        transport.unchanged_file = Some((path.clone(), original));

        let proposal = calibrate_file(&path, MODEL, COUNTED_AT, Some("test-key"), &mut transport)
            .expect("all responses should produce one atomic replacement");

        let written = CalibrationFixture::load(&path).expect("written fixture validates");
        assert_eq!(written, proposal.fixture);
        assert!(
            fs::read(&path)
                .expect("read written fixture")
                .ends_with(b"\n"),
            "canonical output has a trailing newline"
        );
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("list output directory")
                .count(),
            1
        );
    }

    #[test]
    fn test_fixture_validation_rejects_partial_and_structurally_invalid_data() {
        let mut fixture = pending_fixture();
        fixture.samples[1].id = fixture.samples[0].id.clone();
        assert!(matches!(
            validate_fixture(&fixture),
            Err(CalibrationError::DuplicateSampleId { .. })
        ));

        let mut fixture = pending_fixture();
        fixture.samples[0].chars += 1;
        assert!(matches!(
            validate_fixture(&fixture),
            Err(CalibrationError::CharacterCountMismatch { .. })
        ));

        let mut fixture = pending_fixture();
        fixture.anthropic_model = Some(MODEL.to_owned());
        assert!(matches!(
            validate_fixture(&fixture),
            Err(CalibrationError::PartialCalibration)
        ));
    }

    #[test]
    fn test_envelope_larger_than_sample_aborts_without_proposal() {
        let fixture = pending_fixture();
        let mut transport = FakeTransport {
            responses: VecDeque::from([Ok(10), Ok(9)]),
            requests: Vec::new(),
            unchanged_file: None,
        };

        let error = propose_calibration(&fixture, MODEL, COUNTED_AT, &mut transport)
            .expect_err("underflowing envelope subtraction must fail");

        assert!(matches!(
            error,
            CalibrationError::EnvelopeExceedsCount {
                raw_tokens: 9,
                empty_message_tokens: 10,
                ..
            }
        ));
    }
}
