//! `compresso stats --transcript`: savings and cost derived from a client transcript.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use compresso_core::{
    CostBasis, LedgerOperationSavings, PriceTable, TranscriptSavings, TranscriptScanner,
    summarize_transcript,
};
use compresso_store::{StatsFilter, Store};
use serde_json::{Value, json};

use super::engine::EngineCommandError;

/// Largest transcript file the command will scan.
const MAX_TRANSCRIPT_BYTES: u64 = 256 * 1024 * 1024;

/// Scans a transcript file and reconciles it with the local ledger.
///
/// Only numeric accounting fields of Compresso tool results are read. The
/// file is never copied, persisted, or sent anywhere.
pub(crate) fn transcript_savings(
    store: &Store,
    path: &Path,
    prices: &PriceTable,
) -> Result<TranscriptSavings, EngineCommandError> {
    let file = File::open(path)
        .map_err(|_| EngineCommandError::invalid("transcript file could not be read"))?;
    let metadata = file
        .metadata()
        .map_err(|_| EngineCommandError::invalid("transcript file could not be read"))?;
    if !metadata.is_file() {
        return Err(EngineCommandError::invalid(
            "transcript path must be a regular file",
        ));
    }
    if metadata.len() > MAX_TRANSCRIPT_BYTES {
        return Err(EngineCommandError::invalid(
            "transcript file exceeds the 256 MiB limit",
        ));
    }
    let mut reader = BufReader::new(file);
    let mut scanner = TranscriptScanner::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|_| EngineCommandError::invalid("transcript file could not be read"))?;
        if read == 0 {
            break;
        }
        scanner.push_line(&String::from_utf8_lossy(&line));
    }
    let scan = scanner.finish();

    let ledger = if scan.calls.is_empty() {
        HashMap::new()
    } else {
        store
            .detailed_stats(&StatsFilter {
                since: None,
                session_id: None,
                operation_limit: None,
            })
            .map_err(|_| EngineCommandError::internal())?
            .by_operation
            .into_iter()
            .map(|row| {
                (
                    row.operation_id,
                    LedgerOperationSavings {
                        compression_savings: row.compression_savings,
                        retrieval_pack_tokens: row.retrieval_pack_tokens,
                        estimated_raw_source_tokens: row.estimated_raw_source_tokens,
                        retrieval_tokens_avoided: row.retrieval_tokens_avoided,
                        follow_up_calls_avoided: row.follow_up_calls_avoided,
                    },
                )
            })
            .collect::<HashMap<_, _>>()
    };
    Ok(summarize_transcript(
        &scan,
        |operation_id| ledger.get(operation_id).copied(),
        prices,
    ))
}

/// Renders untrusted text for a terminal or a JSON string.
///
/// Printable ASCII passes through. Every other character, including C0 and
/// C1 controls, DEL, escape-sequence introducers, and invisible or
/// bidirectional Unicode, becomes a visible `\u{..}` escape, so the result
/// can never drive the terminal. The scanner already discards such model
/// identifiers; this is the second, independent barrier at the output edge.
pub(crate) fn printable(text: &str) -> String {
    let mut rendered = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_graphic() || character == ' ' {
            rendered.push(character);
        } else {
            rendered.extend(character.escape_unicode());
        }
    }
    rendered
}

pub(crate) fn cost_basis_json(basis: &CostBasis) -> Value {
    json!({
        "model": basis.model.as_deref().map(printable),
        "priced": basis.priced,
        "rate": basis.rate,
        "price_table_version": basis.table_version,
        "effective_date": basis.effective_date,
    })
}

pub(crate) fn transcript_savings_json(savings: &TranscriptSavings) -> Value {
    json!({
        "basis": "transcript",
        "tool_calls": savings.tool_calls,
        "operations": savings.operations,
        "ledger_matched": savings.ledger_matched,
        "transcript_only": savings.transcript_only,
        "baseline_missing": savings.baseline_missing,
        "results_error": savings.results_error,
        "results_unparsed": savings.results_unparsed,
        "results_duplicate": savings.results_duplicate,
        "results_missing": savings.results_missing,
        "lines_skipped": savings.lines_skipped,
        "tool_calls_dropped": savings.tool_calls_dropped,
        "model_ids_rejected": savings.model_ids_rejected,
        "net_tokens_saved": savings.net_tokens_saved,
        "retrieval_pack_tokens": savings.retrieval_pack_tokens,
        "estimated_raw_source_tokens": savings.estimated_raw_source_tokens,
        "retrieval_tokens_avoided": savings.retrieval_tokens_avoided,
        "follow_up_calls_avoided": savings.follow_up_calls_avoided,
        "total_effective_savings": savings.total_effective_savings,
        "modeled_cost_saved": {
            "amount_usd": savings.estimated_cost_saved_usd,
            "label": "modeled",
        },
        "unpriced_tokens_saved": savings.unpriced_tokens_saved,
        "by_model": savings.by_model.iter().map(|row| json!({
            "model": printable(&row.model),
            "tool_calls": row.tool_calls,
            "net_tokens_saved": row.net_tokens_saved,
            "retrieval_tokens_avoided": row.retrieval_tokens_avoided,
            "total_effective_savings": row.total_effective_savings,
            "priced": row.estimated_cost_saved_usd.is_some(),
            "modeled_cost_saved": {
                "amount_usd": row.estimated_cost_saved_usd,
                "label": "modeled",
            },
        })).collect::<Vec<_>>(),
    })
}

pub(crate) fn transcript_savings_human(savings: &TranscriptSavings, prices: &PriceTable) -> String {
    let mut human = format!(
        "\ntranscript savings (derived from client transcript; ledger rows take precedence):\n  compresso tool calls: {}\n  operations counted: {} (ledger {}, transcript only {})\n  not counted: {} errors, {} unparsed, {} duplicates, {} without a result\n  net compression savings: {}\n  retrieval pack tokens: {}\n  retrieval tokens avoided: {} (modeled; {} operations had no baseline and claim zero)\n  total effective savings: {}\n  modeled cost saved: ${:.6} (modeled; input rate, price table {} effective {})\n",
        savings.tool_calls,
        savings.operations,
        savings.ledger_matched,
        savings.transcript_only,
        savings.results_error,
        savings.results_unparsed,
        savings.results_duplicate,
        savings.results_missing,
        savings.net_tokens_saved,
        savings.retrieval_pack_tokens,
        savings.retrieval_tokens_avoided,
        savings.baseline_missing,
        savings.total_effective_savings,
        savings.estimated_cost_saved_usd,
        printable(&prices.table_version),
        printable(&prices.effective_date),
    );
    if savings.model_ids_rejected > 0 {
        human.push_str(&format!(
            "  model ids rejected as unsafe: {} (counted under unknown, never priced)\n",
            savings.model_ids_rejected
        ));
    }
    human.push_str("  per model:\n");
    if savings.by_model.is_empty() {
        human.push_str("    none recorded\n");
    }
    for row in &savings.by_model {
        let cost = match row.estimated_cost_saved_usd {
            Some(amount) => format!("${amount:.6} (modeled)"),
            None => "no price on file, tokens only".to_owned(),
        };
        human.push_str(&format!(
            "    {}: calls {}, effective {}, cost saved {}\n",
            printable(&row.model),
            row.tool_calls,
            row.total_effective_savings,
            cost
        ));
    }
    human
}

#[cfg(test)]
mod tests {
    use compresso_core::{ModelSavings, PriceTable, TranscriptSavings};

    use super::*;

    fn is_terminal_safe(text: &str) -> bool {
        text.chars()
            .all(|character| character.is_ascii_graphic() || matches!(character, ' ' | '\n'))
    }

    #[test]
    fn test_printable_escapes_every_control_byte_and_invisible_character() {
        let controls = (0x00_u32..=0x1f).chain([0x7f]).chain(0x80..=0x9f);
        for code in controls.chain([0x200b, 0x202e, 0xfffd, 0xe9]) {
            let character = char::from_u32(code).expect("code point should be a scalar value");
            let rendered = printable(&format!("a{character}b"));
            assert_eq!(rendered, format!("a\\u{{{code:x}}}b"));
            assert!(is_terminal_safe(&rendered));
        }
        assert_eq!(printable("claude-sonnet-5-5 [x]"), "claude-sonnet-5-5 [x]");
    }

    #[test]
    fn test_renderers_escape_unsafe_model_even_if_scanner_is_bypassed() {
        let hostile = "evil\u{1b}]52;c;AAAA\u{07}\u{9b}2J\0\u{7f}";
        let savings = TranscriptSavings {
            by_model: vec![ModelSavings {
                model: hostile.to_owned(),
                tool_calls: 1,
                net_tokens_saved: 1,
                retrieval_tokens_avoided: 0,
                total_effective_savings: 1,
                estimated_cost_saved_usd: None,
            }],
            ..TranscriptSavings::default()
        };
        let prices = PriceTable::embedded().expect("embedded table should validate");

        let human = transcript_savings_human(&savings, prices);
        assert!(is_terminal_safe(&human));
        assert!(human.contains("evil\\u{1b}]52;c;AAAA\\u{7}\\u{9b}2J\\u{0}\\u{7f}"));

        let json = transcript_savings_json(&savings).to_string();
        assert!(is_terminal_safe(&json));
        let basis = prices.cost_basis(1, Some(hostile));
        assert!(is_terminal_safe(&cost_basis_json(&basis).to_string()));
    }
}
