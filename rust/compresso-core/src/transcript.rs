//! Savings derived from a client transcript of Compresso tool calls.
//!
//! A transcript is a JSON Lines stream in which assistant messages carry
//! `tool_use` blocks and user messages carry the matching `tool_result`
//! blocks. Only Compresso tool calls are inspected, only their numeric
//! accounting fields are read, and no prompt, source, or result text is kept.
//! Transcript text is untrusted: anything that does not match the expected
//! shape contributes zero savings and is counted as skipped.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use crate::pricing::{PriceTable, is_plain_model_id};

/// The Compresso MCP tools whose results carry accounting fields.
pub const COMPRESSO_TOOL_NAMES: [&str; 10] = [
    "compresso_repo_map",
    "compresso_search",
    "compresso_outline",
    "compresso_unfold",
    "compresso_read",
    "compresso_context",
    "compresso_impact",
    "compresso_compress",
    "compresso_stats",
    "compresso_explain",
];
/// Upper bound on distinct tool calls tracked from one transcript.
pub const MAX_TRANSCRIPT_TOOL_CALLS: usize = 100_000;
/// Model label used when the transcript does not name a real model.
pub const UNKNOWN_MODEL: &str = "unknown";

const COMPRESSION_TOOL: &str = "compresso_compress";
const MCP_TOOL_PREFIX: &str = "mcp__";
const SYNTHETIC_MODEL: &str = "<synthetic>";
const ULID_LENGTH: usize = 26;

/// Accounting fields read from one successful Compresso tool result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptCallKind {
    /// `compresso_compress`: exact before/after counts in the ledger's unit.
    Compression {
        before_tokens: u64,
        after_tokens: u64,
        fallback_used: bool,
    },
    /// Any retrieval tool: exact tokens of the returned pack.
    Retrieval { pack_tokens: u64 },
}

/// One distinct Compresso operation observed in a transcript.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptCall {
    pub operation_id: String,
    pub tool: String,
    /// Model named by the assistant message that issued the call.
    pub model: Option<String>,
    pub kind: TranscriptCallKind,
}

/// Content-free outcome of scanning a transcript.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptScan {
    pub calls: Vec<TranscriptCall>,
    /// Distinct Compresso `tool_use` blocks seen.
    pub tool_calls: u64,
    /// Results flagged as errors by the client.
    pub results_error: u64,
    /// Results whose payload was not a recognizable Compresso v1 result.
    pub results_unparsed: u64,
    /// Results repeating an already counted operation.
    pub results_duplicate: u64,
    /// Lines that were not JSON objects.
    pub lines_skipped: u64,
    /// Tool calls dropped after reaching [`MAX_TRANSCRIPT_TOOL_CALLS`].
    pub tool_calls_dropped: u64,
    /// Tool calls whose model identifier was not plain printable ASCII of
    /// bounded length. The identifier is discarded, never stored or echoed,
    /// and the call is attributed to the unknown model.
    pub model_ids_rejected: u64,
}

impl TranscriptScan {
    /// Tool calls for which no result appeared in the transcript.
    pub fn results_missing(&self) -> u64 {
        self.tool_calls.saturating_sub(
            (self.calls.len() as u64)
                .saturating_add(self.results_error)
                .saturating_add(self.results_unparsed)
                .saturating_add(self.results_duplicate),
        )
    }
}

struct PendingCall {
    tool: &'static str,
    model: Option<String>,
    resolved: bool,
}

/// Incremental line-by-line transcript scanner with bounded memory.
#[derive(Default)]
pub struct TranscriptScanner {
    pending: HashMap<String, PendingCall>,
    operations: HashSet<String>,
    scan: TranscriptScan,
}

impl TranscriptScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes one transcript line. Malformed lines never fail the scan.
    pub fn push_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(line) else {
            self.scan.lines_skipped = self.scan.lines_skipped.saturating_add(1);
            return;
        };
        let Some(message) = entry.get("message").and_then(Value::as_object) else {
            return;
        };
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        let model = message.get("model").and_then(Value::as_str);
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => self.push_tool_use(block, model),
                Some("tool_result") => self.push_tool_result(block),
                _ => {}
            }
        }
    }

    fn push_tool_use(&mut self, block: &Value, model: Option<&str>) {
        let (Some(id), Some(tool)) = (
            block.get("id").and_then(Value::as_str),
            block
                .get("name")
                .and_then(Value::as_str)
                .and_then(canonical_tool),
        ) else {
            return;
        };
        if self.pending.contains_key(id) {
            return;
        }
        if self.pending.len() >= MAX_TRANSCRIPT_TOOL_CALLS {
            self.scan.tool_calls_dropped = self.scan.tool_calls_dropped.saturating_add(1);
            return;
        }
        self.scan.tool_calls = self.scan.tool_calls.saturating_add(1);
        let model = match model {
            None | Some("" | SYNTHETIC_MODEL) => None,
            Some(model) if is_plain_model_id(model) => Some(model.to_owned()),
            Some(_) => {
                self.scan.model_ids_rejected = self.scan.model_ids_rejected.saturating_add(1);
                None
            }
        };
        self.pending.insert(
            id.to_owned(),
            PendingCall {
                tool,
                model,
                resolved: false,
            },
        );
    }

    fn push_tool_result(&mut self, block: &Value) {
        let Some(call) = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .and_then(|id| self.pending.get_mut(id))
        else {
            return;
        };
        if call.resolved {
            return;
        }
        call.resolved = true;
        if block.get("is_error").and_then(Value::as_bool) == Some(true) {
            self.scan.results_error = self.scan.results_error.saturating_add(1);
            return;
        }
        let Some((operation_id, kind)) = result_payload(block.get("content"))
            .and_then(|payload| accounting_fields(call.tool, &payload))
        else {
            self.scan.results_unparsed = self.scan.results_unparsed.saturating_add(1);
            return;
        };
        if !self.operations.insert(operation_id.clone()) {
            self.scan.results_duplicate = self.scan.results_duplicate.saturating_add(1);
            return;
        }
        self.scan.calls.push(TranscriptCall {
            operation_id,
            tool: call.tool.to_owned(),
            model: call.model.clone(),
            kind,
        });
    }

    pub fn finish(self) -> TranscriptScan {
        self.scan
    }
}

/// Scans a whole in-memory transcript.
pub fn scan_transcript(text: &str) -> TranscriptScan {
    let mut scanner = TranscriptScanner::new();
    for line in text.lines() {
        scanner.push_line(line);
    }
    scanner.finish()
}

fn canonical_tool(name: &str) -> Option<&'static str> {
    let bare = match name.strip_prefix(MCP_TOOL_PREFIX) {
        Some(qualified) => qualified.rsplit_once("__")?.1,
        None => name,
    };
    COMPRESSO_TOOL_NAMES
        .iter()
        .copied()
        .find(|candidate| *candidate == bare)
}

fn result_payload(content: Option<&Value>) -> Option<Value> {
    let parse = |text: &str| {
        serde_json::from_str::<Value>(text)
            .ok()
            .filter(Value::is_object)
    };
    match content? {
        Value::String(text) => parse(text),
        Value::Array(blocks) => blocks.iter().find_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
                .and_then(parse)
        }),
        _ => None,
    }
}

fn accounting_fields(tool: &str, payload: &Value) -> Option<(String, TranscriptCallKind)> {
    if payload.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let operation_id = payload
        .get("operation_id")
        .and_then(Value::as_str)
        .filter(|id| is_ulid(id))?;
    let kind = if tool == COMPRESSION_TOOL {
        let count = |field: &str| {
            payload
                .get(field)
                .and_then(|counts| counts.get("openai_o200k"))
                .and_then(Value::as_u64)
        };
        TranscriptCallKind::Compression {
            before_tokens: count("before_tokens")?,
            after_tokens: count("after_tokens")?,
            fallback_used: payload.get("fallback_used").and_then(Value::as_bool)?,
        }
    } else {
        TranscriptCallKind::Retrieval {
            pack_tokens: payload.get("tokens_used").and_then(Value::as_u64)?,
        }
    };
    Some((operation_id.to_owned(), kind))
}

fn is_ulid(value: &str) -> bool {
    value.len() == ULID_LENGTH
        && value
            .bytes()
            .all(|byte| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&byte))
}

/// Ledger accounting for one operation, in the fixed savings vocabulary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LedgerOperationSavings {
    pub compression_savings: i64,
    pub retrieval_pack_tokens: u64,
    pub estimated_raw_source_tokens: u64,
    pub retrieval_tokens_avoided: u64,
    pub follow_up_calls_avoided: u64,
}

/// Per-model share of transcript-derived savings.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelSavings {
    pub model: String,
    pub tool_calls: u64,
    pub net_tokens_saved: i64,
    pub retrieval_tokens_avoided: u64,
    pub total_effective_savings: i64,
    /// `None` when the model is not in the price table: tokens only.
    pub estimated_cost_saved_usd: Option<f64>,
}

/// Transcript-derived savings, consistent with ledger semantics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TranscriptSavings {
    pub tool_calls: u64,
    pub operations: u64,
    /// Operations whose figures came from the local ledger row.
    pub ledger_matched: u64,
    /// Operations absent from the ledger, derived from the result payload.
    pub transcript_only: u64,
    /// Transcript-only retrieval operations: no baseline, zero avoidance.
    pub baseline_missing: u64,
    pub results_error: u64,
    pub results_unparsed: u64,
    pub results_duplicate: u64,
    pub results_missing: u64,
    pub lines_skipped: u64,
    pub tool_calls_dropped: u64,
    /// Tool calls whose unsafe model identifier was discarded.
    pub model_ids_rejected: u64,
    pub net_tokens_saved: i64,
    pub retrieval_pack_tokens: u64,
    pub estimated_raw_source_tokens: u64,
    pub retrieval_tokens_avoided: u64,
    pub follow_up_calls_avoided: u64,
    pub total_effective_savings: i64,
    pub by_model: Vec<ModelSavings>,
    /// Sum over priced models only.
    pub estimated_cost_saved_usd: f64,
    /// Positive effective savings attributed to unpriced models.
    pub unpriced_tokens_saved: u64,
}

/// Combines a scan with the local ledger and the price table.
///
/// A ledger row is authoritative when one exists. Otherwise compression is
/// taken from the result's exact before/after counts (zero on fallback), and
/// retrieval claims zero avoidance because the transcript carries no raw
/// baseline. Each model's positive effective savings are priced once at its
/// base input rate; unknown models report tokens only.
pub fn summarize_transcript(
    scan: &TranscriptScan,
    ledger: impl Fn(&str) -> Option<LedgerOperationSavings>,
    prices: &PriceTable,
) -> TranscriptSavings {
    let mut savings = TranscriptSavings {
        tool_calls: scan.tool_calls,
        operations: scan.calls.len() as u64,
        results_error: scan.results_error,
        results_unparsed: scan.results_unparsed,
        results_duplicate: scan.results_duplicate,
        results_missing: scan.results_missing(),
        lines_skipped: scan.lines_skipped,
        tool_calls_dropped: scan.tool_calls_dropped,
        model_ids_rejected: scan.model_ids_rejected,
        ..TranscriptSavings::default()
    };
    let mut by_model = BTreeMap::<String, (u64, i64, u64)>::new();
    for call in &scan.calls {
        let operation = match ledger(&call.operation_id) {
            Some(row) => {
                savings.ledger_matched = savings.ledger_matched.saturating_add(1);
                row
            }
            None => {
                savings.transcript_only = savings.transcript_only.saturating_add(1);
                match call.kind {
                    TranscriptCallKind::Compression {
                        before_tokens,
                        after_tokens,
                        fallback_used,
                    } => LedgerOperationSavings {
                        compression_savings: if fallback_used {
                            0
                        } else {
                            saturating_i64(before_tokens.saturating_sub(after_tokens))
                        },
                        ..LedgerOperationSavings::default()
                    },
                    TranscriptCallKind::Retrieval { pack_tokens } => {
                        savings.baseline_missing = savings.baseline_missing.saturating_add(1);
                        LedgerOperationSavings {
                            retrieval_pack_tokens: pack_tokens,
                            ..LedgerOperationSavings::default()
                        }
                    }
                }
            }
        };
        let effective = operation
            .compression_savings
            .saturating_add(saturating_i64(operation.retrieval_tokens_avoided));
        savings.net_tokens_saved = savings
            .net_tokens_saved
            .saturating_add(operation.compression_savings);
        savings.retrieval_pack_tokens = savings
            .retrieval_pack_tokens
            .saturating_add(operation.retrieval_pack_tokens);
        savings.estimated_raw_source_tokens = savings
            .estimated_raw_source_tokens
            .saturating_add(operation.estimated_raw_source_tokens);
        savings.retrieval_tokens_avoided = savings
            .retrieval_tokens_avoided
            .saturating_add(operation.retrieval_tokens_avoided);
        savings.follow_up_calls_avoided = savings
            .follow_up_calls_avoided
            .saturating_add(operation.follow_up_calls_avoided);
        savings.total_effective_savings = savings.total_effective_savings.saturating_add(effective);
        let entry = by_model
            .entry(
                call.model
                    .clone()
                    .unwrap_or_else(|| UNKNOWN_MODEL.to_owned()),
            )
            .or_default();
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(operation.compression_savings);
        entry.2 = entry.2.saturating_add(operation.retrieval_tokens_avoided);
    }
    for (model, (tool_calls, net_tokens_saved, retrieval_tokens_avoided)) in by_model {
        let total_effective_savings =
            net_tokens_saved.saturating_add(saturating_i64(retrieval_tokens_avoided));
        let billable = u64::try_from(total_effective_savings).unwrap_or(0);
        let estimated_cost_saved_usd = prices
            .lookup(&model)
            .map(|price| price.input_cost_usd(billable));
        match estimated_cost_saved_usd {
            Some(amount) => savings.estimated_cost_saved_usd += amount,
            None => {
                savings.unpriced_tokens_saved =
                    savings.unpriced_tokens_saved.saturating_add(billable);
            }
        }
        savings.by_model.push(ModelSavings {
            model,
            tool_calls,
            net_tokens_saved,
            retrieval_tokens_avoided,
            total_effective_savings,
            estimated_cost_saved_usd,
        });
    }
    savings
}

fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::pricing::MAX_MODEL_ID_BYTES;

    const OP_A: &str = "01J2X3Y4Z5A6B7C8D9E0F1G2H3";
    const OP_B: &str = "01J2X3Y4Z5A6B7C8D9E0F1G2H4";
    const OP_C: &str = "01J2X3Y4Z5A6B7C8D9E0F1G2H5";

    fn tool_use(id: &str, name: &str, model: &str) -> String {
        json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "model": model,
                "content": [{ "type": "tool_use", "id": id, "name": name, "input": {} }]
            }
        })
        .to_string()
    }

    fn tool_result(id: &str, payload: &Value) -> String {
        json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": [{ "type": "text", "text": payload.to_string() }]
                }]
            }
        })
        .to_string()
    }

    fn compress_payload(operation_id: &str, before: u64, after: u64, fallback: bool) -> Value {
        json!({
            "schema_version": 1,
            "operation_id": operation_id,
            "compressed": "synthetic",
            "before_tokens": { "openai_o200k": before, "anthropic_calibrated": before * 2 },
            "after_tokens": { "openai_o200k": after, "anthropic_calibrated": after * 2 },
            "fallback_used": fallback
        })
    }

    fn prices() -> PriceTable {
        PriceTable::parse(
            r#"{"schema_version":1,"table_version":"t1","effective_date":"2026-01-02","currency":"USD","unit":"per_million_tokens","source":"synthetic","models":[{"model":"synthetic-large","input_usd_per_mtok":2.0,"output_usd_per_mtok":10.0}]}"#,
        )
        .expect("synthetic table should parse")
    }

    #[test]
    fn test_headless_transcript_without_ledger_reports_exact_compression_savings() {
        let transcript = [
            tool_use(
                "t1",
                "mcp__compresso__compresso_compress",
                "synthetic-large",
            ),
            tool_result("t1", &compress_payload(OP_A, 1_000, 400, false)),
        ]
        .join("\n");
        let scan = scan_transcript(&transcript);
        let savings = summarize_transcript(&scan, |_| None, &prices());

        assert_eq!(savings.tool_calls, 1);
        assert_eq!(savings.operations, 1);
        assert_eq!(savings.transcript_only, 1);
        assert_eq!(savings.net_tokens_saved, 600);
        assert_eq!(savings.total_effective_savings, 600);
        assert_eq!(savings.by_model.len(), 1);
        assert_eq!(savings.by_model[0].model, "synthetic-large");
        assert_eq!(savings.by_model[0].estimated_cost_saved_usd, Some(0.0012));
        assert_eq!(savings.estimated_cost_saved_usd, 0.0012);
    }

    #[test]
    fn test_fallback_and_baseline_free_retrieval_claim_zero_savings() {
        let transcript = [
            tool_use("t1", "compresso_compress", "synthetic-large"),
            tool_result("t1", &compress_payload(OP_A, 1_000, 400, true)),
            tool_use("t2", "mcp__plugin_x__compresso_context", "synthetic-large"),
            tool_result(
                "t2",
                &json!({ "schema_version": 1, "operation_id": OP_B, "tokens_used": 750 }),
            ),
        ]
        .join("\n");
        let savings = summarize_transcript(&scan_transcript(&transcript), |_| None, &prices());

        assert_eq!(savings.operations, 2);
        assert_eq!(savings.baseline_missing, 1);
        assert_eq!(savings.retrieval_pack_tokens, 750);
        assert_eq!(savings.retrieval_tokens_avoided, 0);
        assert_eq!(savings.total_effective_savings, 0);
        assert_eq!(savings.estimated_cost_saved_usd, 0.0);
    }

    #[test]
    fn test_ledger_row_is_authoritative_over_transcript_numbers() {
        let transcript = [
            tool_use("t1", "mcp__compresso__compresso_context", "synthetic-large"),
            tool_result(
                "t1",
                &json!({ "schema_version": 1, "operation_id": OP_A, "tokens_used": 999_999 }),
            ),
        ]
        .join("\n");
        let savings = summarize_transcript(
            &scan_transcript(&transcript),
            |id| {
                (id == OP_A).then_some(LedgerOperationSavings {
                    compression_savings: 0,
                    retrieval_pack_tokens: 800,
                    estimated_raw_source_tokens: 4_200,
                    retrieval_tokens_avoided: 3_400,
                    follow_up_calls_avoided: 6,
                })
            },
            &prices(),
        );

        assert_eq!(savings.ledger_matched, 1);
        assert_eq!(savings.transcript_only, 0);
        assert_eq!(savings.retrieval_pack_tokens, 800);
        assert_eq!(savings.retrieval_tokens_avoided, 3_400);
        assert_eq!(savings.follow_up_calls_avoided, 6);
        assert_eq!(savings.total_effective_savings, 3_400);
        assert_eq!(savings.estimated_cost_saved_usd, 0.0068);
    }

    #[test]
    fn test_unknown_and_synthetic_models_report_tokens_without_price() {
        let transcript = [
            tool_use("t1", "compresso_compress", "synthetic-unlisted"),
            tool_result("t1", &compress_payload(OP_A, 500, 100, false)),
            tool_use("t2", "compresso_compress", "<synthetic>"),
            tool_result("t2", &compress_payload(OP_B, 300, 100, false)),
            tool_use("t3", "compresso_compress", "synthetic-large-20260102"),
            tool_result("t3", &compress_payload(OP_C, 1_000_100, 100, false)),
        ]
        .join("\n");
        let savings = summarize_transcript(&scan_transcript(&transcript), |_| None, &prices());

        let models = savings
            .by_model
            .iter()
            .map(|row| (row.model.as_str(), row.estimated_cost_saved_usd))
            .collect::<Vec<_>>();
        assert_eq!(
            models,
            vec![
                ("synthetic-large-20260102", Some(2.0)),
                ("synthetic-unlisted", None),
                ("unknown", None),
            ]
        );
        assert_eq!(savings.unpriced_tokens_saved, 600);
        assert_eq!(savings.estimated_cost_saved_usd, 2.0);
        assert_eq!(savings.total_effective_savings, 1_000_600);
    }

    #[test]
    fn test_duplicates_errors_foreign_tools_and_garbage_never_add_savings() {
        let error_result = json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "t3", "is_error": true,
                "content": compress_payload(OP_C, 9_000, 1, false).to_string()
            }]}
        })
        .to_string();
        let transcript = [
            "not json at all".to_owned(),
            "[1,2,3]".to_owned(),
            tool_use("t1", "compresso_compress", "synthetic-large"),
            tool_result("t1", &compress_payload(OP_A, 1_000, 400, false)),
            // A resumed session replays the same call and result.
            tool_use("t1", "compresso_compress", "synthetic-large"),
            tool_result("t1", &compress_payload(OP_A, 1_000, 400, false)),
            // A different call id repeating the same operation id.
            tool_use("t2", "compresso_compress", "synthetic-large"),
            tool_result("t2", &compress_payload(OP_A, 1_000, 400, false)),
            tool_use("t3", "compresso_compress", "synthetic-large"),
            error_result,
            // Another product's tool that happens to print similar fields.
            tool_use("t4", "mcp__other__compress", "synthetic-large"),
            tool_result("t4", &compress_payload(OP_B, 9_000, 1, false)),
            // Wrong schema version, bad operation id, truncated text, no result.
            tool_use("t5", "compresso_compress", "synthetic-large"),
            tool_result(
                "t5",
                &json!({ "schema_version": 2, "operation_id": OP_B, "before_tokens": {"openai_o200k": 9}, "after_tokens": {"openai_o200k": 1}, "fallback_used": false }),
            ),
            tool_use("t6", "compresso_read", "synthetic-large"),
            tool_result(
                "t6",
                &json!({ "schema_version": 1, "operation_id": "short", "tokens_used": 5 }),
            ),
            tool_use("t7", "compresso_read", "synthetic-large"),
            json!({ "type": "user", "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "t7", "content": "{\"schema_version\":1,\"operation_id\""
            }]}})
            .to_string(),
            tool_use("t8", "compresso_read", "synthetic-large"),
        ]
        .join("\n");
        let scan = scan_transcript(&transcript);
        let savings = summarize_transcript(&scan, |_| None, &prices());

        assert_eq!(scan.lines_skipped, 2);
        assert_eq!(savings.tool_calls, 7);
        assert_eq!(savings.operations, 1);
        assert_eq!(savings.results_duplicate, 1);
        assert_eq!(savings.results_error, 1);
        assert_eq!(savings.results_unparsed, 3);
        assert_eq!(savings.results_missing, 1);
        assert_eq!(savings.total_effective_savings, 600);
    }

    fn hostile_model_savings(model: &str) -> (TranscriptScan, TranscriptSavings) {
        let transcript = [
            tool_use("t1", "compresso_compress", model),
            tool_result("t1", &compress_payload(OP_A, 1_000_100, 100, false)),
        ]
        .join("\n");
        let scan = scan_transcript(&transcript);
        let savings = summarize_transcript(&scan, |_| None, &prices());
        (scan, savings)
    }

    fn assert_bucketed_as_unknown(model: &str) {
        let (scan, savings) = hostile_model_savings(model);
        let label = model.escape_debug();
        assert_eq!(scan.model_ids_rejected, 1, "{label}");
        assert_eq!(scan.calls[0].model, None, "{label}");
        assert_eq!(savings.model_ids_rejected, 1, "{label}");
        assert_eq!(savings.by_model.len(), 1, "{label}");
        assert_eq!(savings.by_model[0].model, UNKNOWN_MODEL, "{label}");
        assert_eq!(
            savings.by_model[0].estimated_cost_saved_usd, None,
            "{label}"
        );
        assert_eq!(savings.estimated_cost_saved_usd, 0.0, "{label}");
        assert_eq!(savings.unpriced_tokens_saved, 1_000_000, "{label}");
        let rendered = format!("{scan:?}{savings:?}");
        assert!(
            rendered
                .chars()
                .all(|character| character.is_ascii_graphic() || character == ' '),
            "{label}"
        );
    }

    #[test]
    fn test_every_control_byte_in_model_id_is_rejected_and_unpriced() {
        let control_bytes = (0x00_u32..=0x1f).chain([0x7f]).chain(0x80..=0x9f);
        for byte in control_bytes {
            let control = char::from_u32(byte).expect("control code points are scalar values");
            // Appending to a priced id proves the price is not applied.
            for model in [
                format!("synthetic-large{control}"),
                format!("{control}synthetic-large"),
                format!("synthetic{control}-large"),
                control.to_string(),
            ] {
                assert_bucketed_as_unknown(&model);
            }
        }
    }

    #[test]
    fn test_escape_sequences_oversized_and_invisible_model_ids_are_rejected() {
        let oversized = "m".repeat(200);
        let padded_priced = format!("synthetic-large{}", "-".repeat(MAX_MODEL_ID_BYTES));
        for model in [
            "\u{1b}]52;c;c3ludGhldGlj\u{07}",
            "\u{1b}[2Jsynthetic-large",
            "synthetic-large\u{1b}]8;;https://example.invalid\u{1b}\\link\u{1b}]8;;\u{1b}\\",
            "\u{9b}2Jsynthetic-large",
            "synthetic-large\0",
            "synthetic-large\u{fffd}",
            "synthetic-large\u{202e}",
            "synthetic-large\u{200b}",
            "synthetic large",
            "synthétic-large",
            oversized.as_str(),
            padded_priced.as_str(),
        ] {
            assert_bucketed_as_unknown(model);
        }
    }

    #[test]
    fn test_plain_model_id_at_length_limit_is_kept() {
        let longest = "m".repeat(MAX_MODEL_ID_BYTES);
        let (scan, savings) = hostile_model_savings(&longest);
        assert_eq!(scan.model_ids_rejected, 0);
        assert_eq!(savings.by_model[0].model, longest);

        let (scan, savings) = hostile_model_savings("synthetic-large");
        assert_eq!(scan.model_ids_rejected, 0);
        assert_eq!(savings.by_model[0].estimated_cost_saved_usd, Some(2.0));
    }

    #[test]
    fn test_scan_retains_no_transcript_text() {
        let canary = "CANARY-SECRET-TEXT";
        let mut payload = compress_payload(OP_A, 10, 5, false);
        payload["compressed"] = json!(canary);
        let transcript = [
            json!({ "type": "user", "message": { "role": "user", "content": canary } }).to_string(),
            tool_use("t1", "compresso_compress", "synthetic-large"),
            tool_result("t1", &payload),
        ]
        .join("\n");
        let scan = scan_transcript(&transcript);
        let savings = summarize_transcript(&scan, |_| None, &prices());

        assert!(!format!("{scan:?}{savings:?}").contains(canary));
    }
}
