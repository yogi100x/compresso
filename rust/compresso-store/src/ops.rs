use std::fmt;
use std::process;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use compresso_core::Mode;
use jsonschema::{Draft, Validator};
use rusqlite::{OptionalExtension, Row, params};
use serde_json::Value;

use crate::files::{sqlite_integer_from_u64, sqlite_integer_from_usize, validate_nonempty};
use crate::{Store, StoreError, unix_timestamp};

const TRACE_SCHEMA: &str = include_str!("../../../schemas/compression-trace/v1.json");
const CROCKFORD_BASE32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const ULID_TIMESTAMP_MASK: u64 = (1_u64 << 48) - 1;
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);
static TRACE_VALIDATOR: OnceLock<Result<Validator, String>> = OnceLock::new();

/// Identifier supplied by the engine or MCP handler for one recorded operation.
pub type OperationId = String;

/// Locally generated ULID-compatible identifier for one client connection.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId(String);

impl SessionId {
    /// Returns the canonical 26-character identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the identifier and returns its string representation.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for SessionId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One operation-ledger row at the Store boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationRecord {
    /// Stable operation id; compression traces must carry the same value.
    pub id: OperationId,
    /// Owning client session, or `None` for local engine work outside MCP.
    pub session_id: Option<SessionId>,
    /// Exact known MCP tool name or `engine_compress`.
    pub tool: String,
    /// Compression mode active when the operation ran.
    pub mode: Mode,
    /// Tokens observed before the operation.
    pub observed_tokens: usize,
    /// Tokens delivered after the operation.
    pub applied_tokens: usize,
    /// Tokens added by Compresso itself.
    pub compression_overhead: usize,
    /// Whether compression returned the byte-identical fallback.
    pub fallback_used: bool,
    /// Retrieval payload tokens; forbidden for compression operations.
    pub tokens_served: Option<usize>,
    /// Root compression-trace v1 JSON; forbidden for retrieval operations.
    pub trace_json: Option<String>,
    /// Unix timestamp in seconds.
    pub created_at: i64,
}

/// Counterfactual savings estimate, always explicitly labelled as modeled.
#[derive(Clone, Debug, PartialEq)]
pub struct ModeledCostSaved {
    /// Counterfactual US-dollar estimate; zero until a committed price model exists.
    pub amount_usd: f64,
    /// Always the explicit label `"modeled"`.
    pub label: String,
}

/// Aggregate operation-ledger metrics in the fixed savings vocabulary.
#[derive(Clone, Debug, PartialEq)]
pub struct StatsSummary {
    /// Number of retained operations in the selected window.
    pub operations: u64,
    /// Total tokens entering Compresso.
    pub observed_tokens: u64,
    /// Total tokens delivered after each operation.
    pub applied_tokens: u64,
    /// Saturating token reduction from applied, non-fallback compression.
    pub removed_tokens: u64,
    /// Total tokens added by Compresso itself.
    pub compression_overhead: u64,
    /// Exact signed `removed_tokens - compression_overhead` value.
    pub net_tokens_saved: i64,
    /// Explicitly counterfactual cost estimate.
    pub modeled_cost_saved: ModeledCostSaved,
    /// Fraction of selected operations whose fallback flag is set.
    pub fallback_rate: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationKind {
    Compression,
    Retrieval,
}

struct PreparedStrategyResult<'a> {
    strategy_id: &'a str,
    applied: bool,
    before_tokens: i64,
    after_tokens: i64,
    duration_ms: f64,
}

type RawOperation = (
    String,
    Option<String>,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<String>,
    i64,
);

impl Store {
    /// Starts a validated client session with a locally generated ULID-compatible id.
    pub fn start_session(&self, client: &str, transport: &str) -> Result<SessionId, StoreError> {
        validate_nonempty("session client", client)?;
        if !matches!(transport, "stdio" | "http") {
            return Err(StoreError::InvalidField {
                field: "session transport",
                reason: "must be stdio or http",
            });
        }

        let (session_id, started_at) = generate_session_identity()?;
        self.connection()?.execute(
            "INSERT INTO sessions (id, client, transport, started_at) VALUES (?1, ?2, ?3, ?4)",
            params![session_id.as_str(), client, transport, started_at],
        )?;
        Ok(session_id)
    }

    /// Marks a session ended; this exists for the E5-05 disconnect lifecycle.
    pub fn close_session(&self, session: &SessionId) -> Result<(), StoreError> {
        let ended_at = unix_timestamp()?;
        let changed = self.connection()?.execute(
            "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
            params![ended_at, session.as_str()],
        )?;
        if changed == 0 {
            return Err(StoreError::SessionNotFound(session.as_str().to_owned()));
        }
        Ok(())
    }

    /// Validates and atomically records one operation and its trace strategy rows.
    pub fn record_operation(&self, operation: &OperationRecord) -> Result<(), StoreError> {
        validate_nonempty("operation id", &operation.id)?;
        let kind = classify_tool(&operation.tool)?;
        let trace = validate_operation_shape(operation, kind)?;

        let observed_tokens =
            sqlite_integer_from_usize("operation observed_tokens", operation.observed_tokens)?;
        let applied_tokens =
            sqlite_integer_from_usize("operation applied_tokens", operation.applied_tokens)?;
        let compression_overhead = sqlite_integer_from_usize(
            "operation compression_overhead",
            operation.compression_overhead,
        )?;
        let tokens_served = operation
            .tokens_served
            .map(|value| sqlite_integer_from_usize("operation tokens_served", value))
            .transpose()?;

        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        if let Some(session) = &operation.session_id {
            let exists = transaction
                .query_row(
                    "SELECT 1 FROM sessions WHERE id = ?1",
                    [session.as_str()],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !exists {
                return Err(StoreError::SessionNotFound(session.as_str().to_owned()));
            }
        }

        transaction.execute(
            "INSERT INTO operations (id, session_id, tool, mode, observed_tokens, applied_tokens, compression_overhead, fallback_used, tokens_served, trace_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                &operation.id,
                operation.session_id.as_ref().map(SessionId::as_str),
                &operation.tool,
                mode_name(operation.mode),
                observed_tokens,
                applied_tokens,
                compression_overhead,
                i64::from(operation.fallback_used),
                tokens_served,
                operation.trace_json.as_deref(),
                operation.created_at,
            ],
        )?;

        if let Some(trace) = trace {
            let results = trace
                .get("strategy_results")
                .and_then(Value::as_array)
                .ok_or_else(|| StoreError::InvalidTrace {
                    reason: "root strategy_results must be an array".to_owned(),
                })?;
            let mut statement = transaction.prepare(
                "INSERT INTO strategy_results (operation_id, strategy_id, applied, before_tokens, after_tokens, duration_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for result in results {
                let result = prepare_strategy_result(result)?;
                statement.execute(params![
                    &operation.id,
                    result.strategy_id,
                    i64::from(result.applied),
                    result.before_tokens,
                    result.after_tokens,
                    result.duration_ms,
                ])?;
            }
        }

        transaction.commit()?;
        Ok(())
    }

    /// Loads one validated operation ledger row by its stable identifier.
    pub fn operation_by_id(&self, id: &str) -> Result<Option<OperationRecord>, StoreError> {
        validate_nonempty("operation id", id)?;
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT id, session_id, tool, mode, observed_tokens, applied_tokens, compression_overhead, fallback_used, tokens_served, trace_json, created_at FROM operations WHERE id = ?1",
                [id],
                raw_operation_from_row,
            )
            .optional()?;
        raw.map(decode_operation).transpose()
    }

    /// Loads the newest validated local or MCP compression operation.
    pub fn latest_compression_operation(&self) -> Result<Option<OperationRecord>, StoreError> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT id, session_id, tool, mode, observed_tokens, applied_tokens, compression_overhead, fallback_used, tokens_served, trace_json, created_at FROM operations WHERE tool IN ('engine_compress', 'compresso_compress') ORDER BY created_at DESC, rowid DESC LIMIT 1",
                [],
                raw_operation_from_row,
            )
            .optional()?;
        raw.map(decode_operation).transpose()
    }

    /// Aggregates retained operation rows, optionally starting at a Unix timestamp.
    pub fn stats(&self, since: Option<i64>) -> Result<StatsSummary, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT tool, observed_tokens, applied_tokens, compression_overhead, fallback_used FROM operations WHERE (?1 IS NULL OR created_at >= ?1) ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;

        let mut operations = 0_u64;
        let mut observed_tokens = 0_u64;
        let mut applied_tokens = 0_u64;
        let mut removed_tokens = 0_u64;
        let mut compression_overhead = 0_u64;
        let mut fallbacks = 0_u64;
        for row in rows {
            let (tool, observed, applied, overhead, fallback) = row?;
            let kind = classify_stored_tool(&tool)?;
            let observed = decode_nonnegative("operations.observed_tokens", observed)?;
            let applied = decode_nonnegative("operations.applied_tokens", applied)?;
            let overhead = decode_nonnegative("operations.compression_overhead", overhead)?;
            let fallback = decode_stored_bool("operations.fallback_used", fallback)?;

            operations = operations.saturating_add(1);
            observed_tokens = observed_tokens.saturating_add(observed);
            applied_tokens = applied_tokens.saturating_add(applied);
            compression_overhead = compression_overhead.saturating_add(overhead);
            if kind == OperationKind::Compression && !fallback {
                removed_tokens = removed_tokens.saturating_add(observed.saturating_sub(applied));
            }
            if fallback {
                fallbacks = fallbacks.saturating_add(1);
            }
        }

        let fallback_rate = if operations == 0 {
            0.0
        } else {
            fallbacks as f64 / operations as f64
        };
        Ok(StatsSummary {
            operations,
            observed_tokens,
            applied_tokens,
            removed_tokens,
            compression_overhead,
            net_tokens_saved: signed_token_difference(removed_tokens, compression_overhead),
            modeled_cost_saved: ModeledCostSaved {
                amount_usd: 0.0,
                label: "modeled".to_owned(),
            },
            fallback_rate,
        })
    }
}

fn raw_operation_from_row(row: &Row<'_>) -> rusqlite::Result<RawOperation> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

fn decode_operation(raw: RawOperation) -> Result<OperationRecord, StoreError> {
    let (
        id,
        session_id,
        tool,
        mode,
        observed_tokens,
        applied_tokens,
        compression_overhead,
        fallback_used,
        tokens_served,
        trace_json,
        created_at,
    ) = raw;
    validate_nonempty("stored operation id", &id)?;
    validate_nonempty("stored operation tool", &tool)?;
    let session_id = session_id
        .map(|session_id| {
            validate_nonempty("stored operation session id", &session_id)?;
            Ok::<SessionId, StoreError>(SessionId(session_id))
        })
        .transpose()?;
    let record = OperationRecord {
        id,
        session_id,
        tool,
        mode: decode_mode(&mode)?,
        observed_tokens: decode_usize("operations.observed_tokens", observed_tokens)?,
        applied_tokens: decode_usize("operations.applied_tokens", applied_tokens)?,
        compression_overhead: decode_usize(
            "operations.compression_overhead",
            compression_overhead,
        )?,
        fallback_used: decode_stored_bool("operations.fallback_used", fallback_used)?,
        tokens_served: tokens_served
            .map(|value| decode_usize("operations.tokens_served", value))
            .transpose()?,
        trace_json,
        created_at,
    };
    let kind = classify_stored_tool(&record.tool)?;
    let _validated_trace = validate_operation_shape(&record, kind)?;
    Ok(record)
}

fn classify_tool(tool: &str) -> Result<OperationKind, StoreError> {
    match tool {
        "compresso_compress" | "engine_compress" => Ok(OperationKind::Compression),
        "compresso_repo_map" | "compresso_search" | "compresso_outline" | "compresso_unfold"
        | "compresso_read" | "compresso_context" | "compresso_impact" | "compresso_stats"
        | "compresso_explain" => Ok(OperationKind::Retrieval),
        _ => Err(StoreError::UnknownOperationTool(tool.to_owned())),
    }
}

fn classify_stored_tool(tool: &str) -> Result<OperationKind, StoreError> {
    classify_tool(tool).map_err(|_| StoreError::InvalidStoredValue {
        field: "operations.tool",
        value: tool.to_owned(),
    })
}

fn validate_operation_shape(
    operation: &OperationRecord,
    kind: OperationKind,
) -> Result<Option<Value>, StoreError> {
    match kind {
        OperationKind::Compression => {
            if operation.tokens_served.is_some() {
                return Err(StoreError::UnexpectedTokensServed {
                    tool: operation.tool.clone(),
                });
            }
            let source =
                operation
                    .trace_json
                    .as_deref()
                    .ok_or_else(|| StoreError::MissingTrace {
                        tool: operation.tool.clone(),
                    })?;
            let trace = validate_trace(source)?;
            let trace_id = trace
                .get("operation_id")
                .and_then(Value::as_str)
                .ok_or_else(|| StoreError::InvalidTrace {
                    reason: "root operation_id must be a string".to_owned(),
                })?;
            if trace_id != operation.id {
                return Err(StoreError::OperationTraceIdMismatch {
                    operation_id: operation.id.clone(),
                    trace_operation_id: trace_id.to_owned(),
                });
            }
            validate_trace_accounting(&trace, operation)?;
            Ok(Some(trace))
        }
        OperationKind::Retrieval => {
            if operation.trace_json.is_some() {
                return Err(StoreError::UnexpectedTrace {
                    tool: operation.tool.clone(),
                });
            }
            if operation.tokens_served.is_none() {
                return Err(StoreError::MissingTokensServed {
                    tool: operation.tool.clone(),
                });
            }
            if operation.fallback_used {
                return Err(StoreError::InvalidField {
                    field: "retrieval fallback_used",
                    reason: "retrieval operations do not use compression fallback",
                });
            }
            Ok(None)
        }
    }
}

fn validate_trace(source: &str) -> Result<Value, StoreError> {
    let trace: Value = serde_json::from_str(source).map_err(|error| StoreError::InvalidTrace {
        reason: error.to_string(),
    })?;
    let validator = trace_validator()?;
    validator
        .validate(&trace)
        .map_err(|error| StoreError::InvalidTrace {
            reason: error.to_string(),
        })?;
    Ok(trace)
}

fn trace_validator() -> Result<&'static Validator, StoreError> {
    TRACE_VALIDATOR
        .get_or_init(|| {
            let schema: Value = serde_json::from_str(TRACE_SCHEMA)
                .map_err(|error| format!("embedded root schema is invalid: {error}"))?;
            jsonschema::options()
                .with_draft(Draft::Draft202012)
                .build(&schema)
                .map_err(|error| format!("embedded root schema did not compile: {error}"))
        })
        .as_ref()
        .map_err(|reason| StoreError::InvalidTrace {
            reason: reason.clone(),
        })
}

fn validate_trace_accounting(trace: &Value, operation: &OperationRecord) -> Result<(), StoreError> {
    let before_tokens = trace_u64(trace, "before_tokens")?;
    let after_tokens = trace_u64(trace, "after_tokens")?;
    let observed_tokens =
        u64::try_from(operation.observed_tokens).map_err(|_| StoreError::InvalidTrace {
            reason: "operation observed_tokens exceeds trace integer range".to_owned(),
        })?;
    let applied_tokens =
        u64::try_from(operation.applied_tokens).map_err(|_| StoreError::InvalidTrace {
            reason: "operation applied_tokens exceeds trace integer range".to_owned(),
        })?;
    if before_tokens != observed_tokens {
        return Err(StoreError::InvalidTrace {
            reason: "trace before_tokens does not match operation observed_tokens".to_owned(),
        });
    }
    if after_tokens != applied_tokens {
        return Err(StoreError::InvalidTrace {
            reason: "trace after_tokens does not match operation applied_tokens".to_owned(),
        });
    }
    let fallback_used = trace
        .get("fallback_used")
        .and_then(Value::as_bool)
        .ok_or_else(|| StoreError::InvalidTrace {
            reason: "root fallback_used must be boolean".to_owned(),
        })?;
    if fallback_used != operation.fallback_used {
        return Err(StoreError::InvalidTrace {
            reason: "trace fallback_used does not match operation fallback_used".to_owned(),
        });
    }
    if fallback_used && before_tokens != after_tokens {
        return Err(StoreError::InvalidTrace {
            reason: "fallback traces must preserve the observed token count".to_owned(),
        });
    }
    let mode =
        trace
            .get("mode")
            .and_then(Value::as_str)
            .ok_or_else(|| StoreError::InvalidTrace {
                reason: "root mode must be a string".to_owned(),
            })?;
    if mode != mode_name(operation.mode) {
        return Err(StoreError::InvalidTrace {
            reason: "trace mode does not match operation mode".to_owned(),
        });
    }
    Ok(())
}

fn trace_u64(trace: &Value, field: &'static str) -> Result<u64, StoreError> {
    trace
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| StoreError::InvalidTrace {
            reason: format!("root {field} must be a nonnegative integer"),
        })
}

fn prepare_strategy_result(value: &Value) -> Result<PreparedStrategyResult<'_>, StoreError> {
    let strategy_id = value
        .get("strategy_id")
        .and_then(Value::as_str)
        .ok_or_else(|| StoreError::InvalidTrace {
            reason: "strategy_id must be a string".to_owned(),
        })?;
    validate_nonempty("trace strategy_id", strategy_id)?;
    let applied = value
        .get("applied")
        .and_then(Value::as_bool)
        .ok_or_else(|| StoreError::InvalidTrace {
            reason: "strategy applied must be boolean".to_owned(),
        })?;
    let before_tokens =
        trace_nonnegative_integer(value.get("before_tokens"), "strategy_results.before_tokens")?;
    let after_tokens =
        trace_nonnegative_integer(value.get("after_tokens"), "strategy_results.after_tokens")?;
    let duration_ms = value
        .get("duration_ms")
        .and_then(Value::as_f64)
        .filter(|duration| duration.is_finite() && *duration >= 0.0)
        .ok_or(StoreError::InvalidField {
            field: "strategy_results.duration_ms",
            reason: "must be a finite nonnegative number",
        })?;
    Ok(PreparedStrategyResult {
        strategy_id,
        applied,
        before_tokens,
        after_tokens,
        duration_ms,
    })
}

fn trace_nonnegative_integer(
    value: Option<&Value>,
    field: &'static str,
) -> Result<i64, StoreError> {
    let value = value.ok_or_else(|| StoreError::InvalidTrace {
        reason: format!("{field} is missing"),
    })?;
    if let Some(value) = value.as_i64() {
        if value < 0 {
            return Err(StoreError::InvalidField {
                field,
                reason: "must be nonnegative",
            });
        }
        return Ok(value);
    }
    if let Some(value) = value.as_u64() {
        return sqlite_integer_from_u64(field, value);
    }
    Err(StoreError::InvalidTrace {
        reason: format!("{field} must be an integer"),
    })
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Safe => "safe",
        Mode::Balanced => "balanced",
        Mode::Aggressive => "aggressive",
        Mode::Passthrough => "passthrough",
    }
}

fn decode_mode(value: &str) -> Result<Mode, StoreError> {
    match value {
        "safe" => Ok(Mode::Safe),
        "balanced" => Ok(Mode::Balanced),
        "aggressive" => Ok(Mode::Aggressive),
        "passthrough" => Ok(Mode::Passthrough),
        _ => Err(StoreError::InvalidStoredValue {
            field: "operations.mode",
            value: value.to_owned(),
        }),
    }
}

fn generate_session_identity() -> Result<(SessionId, i64), StoreError> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let timestamp_ms = u64::try_from(elapsed.as_millis())
        .unwrap_or(u64::MAX)
        .min(ULID_TIMESTAMP_MASK);
    let counter = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&elapsed.as_nanos().to_be_bytes());
    hasher.update(&process::id().to_be_bytes());
    hasher.update(&counter.to_be_bytes());
    let digest = hasher.finalize();

    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&timestamp_ms.to_be_bytes()[2..]);
    bytes[6..].copy_from_slice(&digest.as_bytes()[..10]);
    let id = SessionId(encode_ulid(bytes));
    let started_at = i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX);
    Ok((id, started_at))
}

fn encode_ulid(bytes: [u8; 16]) -> String {
    let mut value = u128::from_be_bytes(bytes);
    let mut encoded = [b'0'; 26];
    for byte in encoded.iter_mut().rev() {
        *byte = CROCKFORD_BASE32[(value & 0x1f) as usize];
        value >>= 5;
    }
    encoded.into_iter().map(char::from).collect()
}

fn decode_nonnegative(field: &'static str, value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::InvalidStoredValue {
        field,
        value: value.to_string(),
    })
}

fn decode_usize(field: &'static str, value: i64) -> Result<usize, StoreError> {
    let value = decode_nonnegative(field, value)?;
    usize::try_from(value).map_err(|_| StoreError::NumericOutOfRange {
        field,
        value: u128::from(value),
    })
}

fn decode_stored_bool(field: &'static str, value: i64) -> Result<bool, StoreError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(StoreError::InvalidStoredValue {
            field,
            value: value.to_string(),
        }),
    }
}

fn signed_token_difference(removed_tokens: u64, compression_overhead: u64) -> i64 {
    if removed_tokens >= compression_overhead {
        i64::try_from(removed_tokens - compression_overhead).unwrap_or(i64::MAX)
    } else {
        let deficit = compression_overhead - removed_tokens;
        i64::try_from(deficit).map_or(i64::MIN, |value| -value)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::thread;

    use compresso_core::Mode;
    use serde_json::{Value, json};
    use tempfile::tempdir;

    use crate::{OperationRecord, Store, StoreError};

    fn setup() -> (tempfile::TempDir, Store) {
        let directory = tempdir().expect("temp directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        (directory, store)
    }

    fn valid_trace(id: &str, before: u64, after: u64, strategies: Vec<Value>) -> String {
        json!({
            "schema_version": 1,
            "operation_id": id,
            "mode": "safe",
            "before_tokens": before,
            "after_tokens": after,
            "fallback_used": before == after,
            "strategy_results": strategies,
            "safety_checks": [],
            "explanation": "fixture trace"
        })
        .to_string()
    }

    fn strategy(id: &str, before: u64, after: u64, duration_ms: f64) -> Value {
        json!({
            "strategy_id": id,
            "applied": before != after,
            "before_tokens": before,
            "after_tokens": after,
            "duration_ms": duration_ms,
            "removals": []
        })
    }

    fn operation(
        id: &str,
        tool: &str,
        token_counts: (usize, usize),
        fallback_used: bool,
        tokens_served: Option<usize>,
        trace_json: Option<String>,
        created_at: i64,
    ) -> OperationRecord {
        let (observed_tokens, applied_tokens) = token_counts;
        OperationRecord {
            id: id.to_owned(),
            session_id: None,
            tool: tool.to_owned(),
            mode: Mode::Safe,
            observed_tokens,
            applied_tokens,
            compression_overhead: 0,
            fallback_used,
            tokens_served,
            trace_json,
            created_at,
        }
    }

    #[test]
    fn test_session_ids_are_ulid_compatible_and_unique_concurrently() {
        let (_directory, store) = setup();
        let mut workers = Vec::new();
        for worker in 0..8 {
            let store = store.clone();
            workers.push(thread::spawn(move || {
                (0..25)
                    .map(|index| {
                        store
                            .start_session(&format!("test-{worker}-{index}"), "stdio")
                            .expect("session should start")
                    })
                    .collect::<Vec<_>>()
            }));
        }
        let ids = workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("worker should not panic"))
            .collect::<Vec<_>>();
        let distinct = ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<HashSet<_>>();
        assert_eq!(distinct.len(), 200);
        assert!(ids.iter().all(|id| {
            id.as_str().len() == 26
                && id
                    .as_str()
                    .bytes()
                    .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&byte))
                && id.as_str().as_bytes()[0] <= b'7'
        }));
    }

    #[test]
    fn test_session_validation_and_close() {
        let (_directory, store) = setup();
        assert!(matches!(
            store.start_session("", "stdio"),
            Err(StoreError::InvalidField {
                field: "session client",
                ..
            })
        ));
        assert!(matches!(
            store.start_session("codex", "socket"),
            Err(StoreError::InvalidField {
                field: "session transport",
                ..
            })
        ));

        let session = store
            .start_session("test-harness", "stdio")
            .expect("session should start");
        store.close_session(&session).expect("session should close");
        let ended_at: Option<i64> = store
            .connection()
            .expect("connection should lock")
            .query_row(
                "SELECT ended_at FROM sessions WHERE id = ?1",
                [session.as_str()],
                |row| row.get(0),
            )
            .expect("session should be queryable");
        assert!(ended_at.is_some());
    }

    #[test]
    fn test_record_operation_rejects_bad_trace() {
        let (_directory, store) = setup();
        let record = operation(
            "bad-trace",
            "compresso_compress",
            (10, 8),
            false,
            None,
            Some(r#"{"schema_version":1}"#.to_owned()),
            100,
        );
        assert!(matches!(
            store.record_operation(&record),
            Err(StoreError::InvalidTrace { .. })
        ));
        assert_eq!(operation_count(&store), 0);
    }

    #[test]
    fn test_d47_tool_classification_and_shape() {
        let (_directory, store) = setup();
        let retrieval_tools = [
            "compresso_repo_map",
            "compresso_search",
            "compresso_outline",
            "compresso_unfold",
            "compresso_read",
            "compresso_context",
            "compresso_impact",
            "compresso_stats",
            "compresso_explain",
        ];
        for (index, tool) in retrieval_tools.into_iter().enumerate() {
            store
                .record_operation(&operation(
                    &format!("retrieval-{index}"),
                    tool,
                    (10, 10),
                    false,
                    Some(7),
                    None,
                    100,
                ))
                .expect("known retrieval should record");
        }
        assert_eq!(operation_count(&store), 9);

        let missing_trace = operation(
            "missing-trace",
            "compresso_compress",
            (10, 8),
            false,
            None,
            None,
            100,
        );
        assert!(matches!(
            store.record_operation(&missing_trace),
            Err(StoreError::MissingTrace { .. })
        ));
        let unexpected_trace = operation(
            "retrieval-trace",
            "compresso_read",
            (10, 10),
            false,
            Some(7),
            Some(valid_trace("retrieval-trace", 10, 10, vec![])),
            100,
        );
        assert!(matches!(
            store.record_operation(&unexpected_trace),
            Err(StoreError::UnexpectedTrace { .. })
        ));
        let missing_served = operation(
            "missing-served",
            "compresso_read",
            (10, 10),
            false,
            None,
            None,
            100,
        );
        assert!(matches!(
            store.record_operation(&missing_served),
            Err(StoreError::MissingTokensServed { .. })
        ));
        let unknown = operation(
            "unknown",
            "compresso_future_tool",
            (10, 10),
            false,
            Some(1),
            None,
            100,
        );
        assert!(matches!(
            store.record_operation(&unknown),
            Err(StoreError::UnknownOperationTool(tool)) if tool == "compresso_future_tool"
        ));
        assert_eq!(operation_count(&store), 9);
    }

    #[test]
    fn test_retrieval_op_null_trace_tokens_served() {
        let (_directory, store) = setup();
        store
            .record_operation(&operation(
                "read-ledger-row",
                "compresso_read",
                (1_874, 1_874),
                false,
                Some(1_874),
                None,
                100,
            ))
            .expect("retrieval operation should record");

        let row: (Option<String>, i64) = store
            .connection()
            .expect("connection should lock")
            .query_row(
                "SELECT trace_json, tokens_served FROM operations WHERE id = ?1",
                ["read-ledger-row"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("retrieval ledger row should be queryable");
        assert_eq!(row, (None, 1_874));
    }

    #[test]
    fn test_engine_compress_is_supported_and_tokens_served_rejected() {
        let (_directory, store) = setup();
        let trace = valid_trace("engine-1", 20, 10, vec![]);
        store
            .record_operation(&operation(
                "engine-1",
                "engine_compress",
                (20, 10),
                false,
                None,
                Some(trace),
                100,
            ))
            .expect("local engine compression should record");
        let trace = valid_trace("compress-served", 20, 10, vec![]);
        let invalid = operation(
            "compress-served",
            "compresso_compress",
            (20, 10),
            false,
            Some(10),
            Some(trace),
            100,
        );
        assert!(matches!(
            store.record_operation(&invalid),
            Err(StoreError::UnexpectedTokensServed { .. })
        ));
    }

    #[test]
    fn test_operation_lookup_by_id_and_latest_compression() {
        let (_directory, store) = setup();
        let first = operation(
            "lookup-first",
            "engine_compress",
            (20, 10),
            false,
            None,
            Some(valid_trace("lookup-first", 20, 10, vec![])),
            100,
        );
        let retrieval = operation(
            "lookup-retrieval",
            "compresso_stats",
            (4, 4),
            false,
            Some(4),
            None,
            300,
        );
        let second = operation(
            "lookup-second",
            "compresso_compress",
            (30, 12),
            false,
            None,
            Some(valid_trace("lookup-second", 30, 12, vec![])),
            100,
        );
        for record in [&first, &retrieval, &second] {
            store
                .record_operation(record)
                .expect("fixture operation should record");
        }

        assert_eq!(
            store
                .operation_by_id("lookup-first")
                .expect("operation lookup should succeed"),
            Some(first)
        );
        assert_eq!(
            store
                .operation_by_id("missing")
                .expect("missing lookup should succeed"),
            None
        );
        assert_eq!(
            store
                .latest_compression_operation()
                .expect("latest compression lookup should succeed"),
            Some(second)
        );
    }

    #[test]
    fn test_operation_lookup_revalidates_persisted_trace() {
        let (_directory, store) = setup();
        store
            .connection()
            .expect("connection should lock")
            .execute(
                "INSERT INTO operations (id, tool, mode, observed_tokens, applied_tokens, compression_overhead, fallback_used, trace_json, created_at) VALUES (?1, 'engine_compress', 'safe', 10, 8, 0, 0, ?2, 100)",
                rusqlite::params!["corrupt-trace", r#"{"schema_version":1}"#],
            )
            .expect("corrupt fixture row should insert");

        assert!(matches!(
            store.operation_by_id("corrupt-trace"),
            Err(StoreError::InvalidTrace { .. })
        ));
    }

    #[test]
    fn test_operation_id_must_match_trace() {
        let (_directory, store) = setup();
        let record = operation(
            "operation-id",
            "compresso_compress",
            (10, 8),
            false,
            None,
            Some(valid_trace("different-id", 10, 8, vec![])),
            100,
        );
        assert!(matches!(
            store.record_operation(&record),
            Err(StoreError::OperationTraceIdMismatch { .. })
        ));
        assert_eq!(operation_count(&store), 0);
    }

    #[test]
    fn test_trace_accounting_must_match_operation() {
        let (_directory, store) = setup();
        for (index, field, replacement) in [
            (0, "before_tokens", json!(11)),
            (1, "after_tokens", json!(7)),
            (2, "fallback_used", json!(true)),
            (3, "mode", json!("passthrough")),
        ] {
            let id = format!("accounting-{index}");
            let mut trace: Value = serde_json::from_str(&valid_trace(&id, 10, 8, vec![]))
                .expect("fixture trace should parse");
            trace[field] = replacement;
            let record = operation(
                &id,
                "compresso_compress",
                (10, 8),
                false,
                None,
                Some(trace.to_string()),
                100,
            );
            assert!(matches!(
                store.record_operation(&record),
                Err(StoreError::InvalidTrace { .. })
            ));
        }
        let mut fallback_trace: Value =
            serde_json::from_str(&valid_trace("false-fallback-savings", 10, 8, vec![]))
                .expect("fixture trace should parse");
        fallback_trace["fallback_used"] = json!(true);
        let fallback_record = operation(
            "false-fallback-savings",
            "compresso_compress",
            (10, 8),
            true,
            None,
            Some(fallback_trace.to_string()),
            100,
        );
        assert!(matches!(
            store.record_operation(&fallback_record),
            Err(StoreError::InvalidTrace { .. })
        ));
        assert_eq!(operation_count(&store), 0);
    }

    #[test]
    fn test_record_operation_persists_strategy_results() {
        let (_directory, store) = setup();
        let trace = valid_trace(
            "with-strategies",
            100,
            60,
            vec![
                strategy("dedup_exact", 100, 80, 1.5),
                strategy("collapse_logs", 80, 60, 2.5),
            ],
        );
        store
            .record_operation(&operation(
                "with-strategies",
                "compresso_compress",
                (100, 60),
                false,
                None,
                Some(trace),
                100,
            ))
            .expect("compression should record");

        let connection = store.connection().expect("connection should lock");
        let children: Vec<(String, i64, i64, i64, f64)> = connection
            .prepare("SELECT strategy_id, applied, before_tokens, after_tokens, duration_ms FROM strategy_results WHERE operation_id = ?1 ORDER BY id")
            .expect("strategy query should prepare")
            .query_map(["with-strategies"], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })
            .expect("strategies should query")
            .collect::<Result<_, _>>()
            .expect("strategies should decode");
        assert_eq!(children.len(), 2);
        assert_eq!(children[0], ("dedup_exact".to_owned(), 1, 100, 80, 1.5));
        assert_eq!(children[1].0, "collapse_logs");
    }

    #[test]
    fn test_strategy_conversion_failure_rolls_back_operation_and_children() {
        let (_directory, store) = setup();
        let trace = valid_trace(
            "atomic-trace",
            100,
            80,
            vec![
                strategy("first", 100, 90, 1.0),
                json!({
                    "strategy_id": "too-large",
                    "applied": true,
                    "before_tokens": 9_223_372_036_854_775_808u64,
                    "after_tokens": 80,
                    "duration_ms": 1.0,
                    "removals": []
                }),
            ],
        );
        let error = store
            .record_operation(&operation(
                "atomic-trace",
                "compresso_compress",
                (100, 80),
                false,
                None,
                Some(trace),
                100,
            ))
            .expect_err("out-of-range child should fail");
        assert!(matches!(error, StoreError::NumericOutOfRange { .. }));
        let connection = store.connection().expect("connection should lock");
        let counts: (i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM operations), (SELECT COUNT(*) FROM strategy_results)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("counts should query");
        assert_eq!(counts, (0, 0));
    }

    #[test]
    fn test_stats_vocabulary_math_and_since_filter() {
        let (_directory, store) = setup();
        let mut first = operation(
            "stats-1",
            "compresso_compress",
            (1_000, 700),
            false,
            None,
            Some(valid_trace("stats-1", 1_000, 700, vec![])),
            100,
        );
        first.compression_overhead = 50;
        store
            .record_operation(&first)
            .expect("first operation should record");
        store
            .record_operation(&operation(
                "stats-2",
                "compresso_compress",
                (500, 500),
                true,
                None,
                Some(valid_trace("stats-2", 500, 500, vec![])),
                200,
            ))
            .expect("second operation should record");

        let all = store.stats(None).expect("stats should aggregate");
        assert_eq!(all.operations, 2);
        assert_eq!(all.observed_tokens, 1_500);
        assert_eq!(all.applied_tokens, 1_200);
        assert_eq!(all.removed_tokens, 300);
        assert_eq!(all.compression_overhead, 50);
        assert_eq!(all.net_tokens_saved, 250);
        assert_eq!(all.fallback_rate, 0.5);
        assert_eq!(all.modeled_cost_saved.amount_usd, 0.0);
        assert_eq!(all.modeled_cost_saved.label, "modeled");

        let recent = store.stats(Some(150)).expect("stats should filter");
        assert_eq!(recent.operations, 1);
        assert_eq!(recent.observed_tokens, 500);
        assert_eq!(recent.applied_tokens, 500);
        assert_eq!(recent.removed_tokens, 0);
        assert_eq!(recent.net_tokens_saved, 0);
        assert_eq!(recent.fallback_rate, 1.0);
    }

    #[test]
    fn test_stats_preserve_negative_net_savings_and_empty_store() {
        let (_directory, store) = setup();
        let empty = store.stats(None).expect("empty stats should succeed");
        assert_eq!(empty.operations, 0);
        assert_eq!(empty.fallback_rate, 0.0);

        let mut record = operation(
            "expanded",
            "compresso_compress",
            (100, 200),
            false,
            None,
            Some(valid_trace("expanded", 100, 200, vec![])),
            100,
        );
        record.compression_overhead = 10;
        store
            .record_operation(&record)
            .expect("expanded operation should record");
        let stats = store.stats(None).expect("stats should aggregate");
        assert_eq!(stats.removed_tokens, 0);
        assert_eq!(stats.net_tokens_saved, -10);
    }

    fn operation_count(store: &Store) -> i64 {
        store
            .connection()
            .expect("connection should lock")
            .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
            .expect("operation count should query")
    }
}
