use std::ffi::OsString;
use std::fs;
use std::io::Cursor;

use clap::Parser;
use compresso_cli::{Cli, CliExitCode, UnavailableAccountClient, execute_with_store_and_input};
use compresso_core::Trace;
use compresso_store::Store;
use serde_json::{Value, json};
use tempfile::TempDir;

const LOG_FIXTURE: &str = include_str!("../../../fixtures/logs/repeated-npm-install/input.txt");
const FALLBACK_FIXTURE: &str = include_str!("../../../fixtures/adversarial/fence-bomb/input.txt");

#[test]
fn test_compress_log_fixture_via_stdin_records_valid_trace() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run_json(
        &["compress", "--target-tokens", "2000", "--json"],
        LOG_FIXTURE.as_bytes(),
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["schema_version"], 1);
    let content = output["data"]["content"]
        .as_str()
        .expect("compression result should be text");
    let before_tokens = output["data"]["before_tokens"]
        .as_u64()
        .expect("before_tokens should be an integer");
    let after_tokens = output["data"]["after_tokens"]
        .as_u64()
        .expect("after_tokens should be an integer");
    if output["data"]["fallback_used"] == true {
        assert_eq!(content, LOG_FIXTURE);
        assert_eq!(after_tokens, before_tokens);
        assert!(matches!(
            output["data"]["fallback_reason"].as_str(),
            Some("latency_budget_exceeded" | "safety_check_failed:latency_ok")
        ));
    } else {
        assert!(content.contains("[×500"));
        assert!(after_tokens < before_tokens);
        assert!(output["data"]["fallback_reason"].is_null());
    }

    let stats = fixture.store.stats(None).expect("stats should load");
    assert_eq!(stats.operations, 1);
    let operation_id = output["data"]["operation_id"]
        .as_str()
        .expect("operation id should be text");
    let operation = fixture
        .store
        .operation_by_id(operation_id)
        .expect("operation lookup should succeed")
        .expect("compression operation should exist");
    let trace: Trace = serde_json::from_str(
        operation
            .trace_json
            .as_deref()
            .expect("compression should store a trace"),
    )
    .expect("stored trace should match the typed root schema");
    assert_eq!(trace.operation_id, operation_id);
    assert!(
        !serde_json::to_string(&trace)
            .expect("trace should serialize")
            .contains("registry.npmjs.org")
    );
}

#[test]
fn test_passthrough_stdin_is_byte_exact() {
    let fixture = Fixture::new();
    let input = "arbitrary UTF-8\nβeta without a final newline";

    let (exit, output, errors) =
        fixture.run_raw(&["compress", "--mode", "passthrough"], input.as_bytes());

    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(output, input.as_bytes());
    assert!(errors.is_empty());
    let stats = fixture.store.stats(None).expect("stats should load");
    assert_eq!(stats.operations, 1);
    assert_eq!(stats.observed_tokens, stats.applied_tokens);
}

#[test]
fn test_compress_file_input_precedes_stdin() {
    let fixture = Fixture::new();
    let file = fixture.directory.path().join("input.txt");
    let file_content = "content from --file\nwith exact final newline\n";
    fs::write(&file, file_content).expect("file input should be written");
    let file = file.to_str().expect("temporary path should be UTF-8");

    let (exit, output, errors) = fixture.run_raw(
        &["compress", "--mode", "passthrough", "--file", file],
        b"ignored stdin",
    );

    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(output, file_content.as_bytes());
    assert!(errors.is_empty());
}

#[test]
fn test_compress_fail_open_surfaces_fallback_and_original_content() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run_json(
        &["compress", "--target-tokens", "500", "--json"],
        FALLBACK_FIXTURE.as_bytes(),
    );

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["data"]["fallback_used"], true);
    assert_eq!(output["data"]["content"], FALLBACK_FIXTURE);
    assert_eq!(
        output["data"]["before_tokens"],
        output["data"]["after_tokens"]
    );
}

#[test]
fn test_empty_compression_is_rejected_without_ledger_row() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run_json(&["compress", "--json"], b"");

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_null());
    assert_eq!(errors["error"]["code"], "invalid_arguments");
    assert_eq!(
        fixture
            .store
            .stats(None)
            .expect("stats should load")
            .operations,
        0
    );
}

#[test]
fn test_malformed_utf8_is_rejected_without_ledger_row() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run_json(&["compress", "--json"], &[0xff, 0xfe]);

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_null());
    assert_eq!(errors["error"]["code"], "invalid_arguments");
    assert_eq!(
        fixture
            .store
            .stats(None)
            .expect("stats should load")
            .operations,
        0
    );
}

#[test]
fn test_explain_last_and_id_return_typed_trace_without_raw_content() {
    let fixture = Fixture::new();
    let canary = "private-content-canary-57eac03b that must never enter a trace";
    let (_, compressed, _) = fixture.run_json(
        &["compress", "--mode", "passthrough", "--json"],
        canary.as_bytes(),
    );
    let operation_id = compressed["data"]["operation_id"]
        .as_str()
        .expect("operation id should be text");

    for selector in [
        vec!["explain", "--last", "--json"],
        vec!["explain", operation_id, "--json"],
    ] {
        let (exit, output, errors) = fixture.run_json(&selector, b"");
        assert_eq!(exit, CliExitCode::Success);
        assert!(errors.is_null());
        assert_eq!(output["schema_version"], 1);
        assert_eq!(output["data"]["operation_id"], operation_id);
        let trace: Trace = serde_json::from_value(output["data"]["trace"].clone())
            .expect("explain trace should match the typed root schema");
        assert_eq!(trace.operation_id, operation_id);
        assert!(!output.to_string().contains(canary));
    }
}

#[test]
fn test_stats_json_uses_exact_savings_vocabulary_and_human_labels_modeled() {
    let fixture = Fixture::new();
    let (compress_exit, _, compress_errors) =
        fixture.run_raw(&["compress", "--mode", "passthrough"], b"stats input");
    assert_eq!(compress_exit, CliExitCode::Success);
    assert!(compress_errors.is_empty());

    let (json_exit, output, errors) = fixture.run_json(&["stats", "--json"], b"");
    assert_eq!(json_exit, CliExitCode::Success);
    assert!(errors.is_null());
    let keys = output["data"]
        .as_object()
        .expect("stats data should be an object")
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from([
            "applied_tokens",
            "by_operation",
            "by_repository",
            "by_tool",
            "compression_overhead",
            "compression_savings",
            "cost_basis",
            "fallback_rate",
            "modeled_cost_saved",
            "net_tokens_saved",
            "observed_tokens",
            "operations",
            "removed_tokens",
            "retrieval_savings",
            "total_effective_savings",
            "transcript_savings",
        ])
    );
    assert!(output["data"]["transcript_savings"].is_null());
    assert_eq!(output["data"]["cost_basis"]["priced"], false);
    assert!(output["data"]["cost_basis"]["model"].is_null());
    assert_eq!(output["data"]["modeled_cost_saved"]["amount_usd"], 0.0);
    assert_eq!(output["data"]["modeled_cost_saved"]["label"], "modeled");
    assert_eq!(output["data"]["compression_savings"]["basis"], "applied");
    assert_eq!(output["data"]["retrieval_savings"]["basis"], "modeled");

    let (human_exit, human, human_errors) = fixture.run_raw(&["stats"], b"");
    assert_eq!(human_exit, CliExitCode::Success);
    assert!(human_errors.is_empty());
    assert!(
        String::from_utf8(human)
            .expect("human stats should be UTF-8")
            .contains("modeled")
    );
}

const TRANSCRIPT_OPERATION: &str = "01J2X3Y4Z5A6B7C8D9E0F1G2H3";
const TRANSCRIPT_RETRIEVAL: &str = "01J2X3Y4Z5A6B7C8D9E0F1G2H4";

fn transcript_call(id: &str, tool: &str, model: &str, payload: &Value) -> String {
    let call = json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "model": model,
            "content": [{ "type": "tool_use", "id": id, "name": tool, "input": {} }]
        }
    });
    let result = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": id,
                "content": [{ "type": "text", "text": payload.to_string() }]
            }]
        }
    });
    format!("{call}\n{result}\n")
}

fn compress_result(operation_id: &str, before: u64, after: u64) -> Value {
    json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "compressed": "synthetic transcript body",
        "before_tokens": { "openai_o200k": before, "anthropic_calibrated": before },
        "after_tokens": { "openai_o200k": after, "anthropic_calibrated": after },
        "fallback_used": false
    })
}

#[test]
fn test_stats_transcript_reports_savings_and_cost_for_headless_run_without_ledger() {
    let fixture = Fixture::new();
    let transcript = [
        transcript_call(
            "toolu_1",
            "mcp__compresso__compresso_compress",
            "claude-sonnet-5-5",
            &compress_result(TRANSCRIPT_OPERATION, 1_000_500, 500),
        ),
        transcript_call(
            "toolu_2",
            "mcp__compresso__compresso_context",
            "model-without-a-price",
            &json!({ "schema_version": 1, "operation_id": TRANSCRIPT_RETRIEVAL, "tokens_used": 640 }),
        ),
    ]
    .concat();
    let path = fixture.directory.path().join("headless.jsonl");
    fs::write(&path, transcript).expect("synthetic transcript should be written");
    let path = path.to_str().expect("temporary path should be UTF-8");

    let (exit, output, errors) = fixture.run_json(&["stats", "--transcript", path, "--json"], b"");

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["data"]["operations"], 0);
    assert_eq!(output["data"]["total_effective_savings"], 0);
    let savings = &output["data"]["transcript_savings"];
    assert_eq!(savings["basis"], "transcript");
    assert_eq!(savings["tool_calls"], 2);
    assert_eq!(savings["operations"], 2);
    assert_eq!(savings["ledger_matched"], 0);
    assert_eq!(savings["transcript_only"], 2);
    assert_eq!(savings["baseline_missing"], 1);
    assert_eq!(savings["net_tokens_saved"], 1_000_000);
    assert_eq!(savings["retrieval_pack_tokens"], 640);
    assert_eq!(savings["retrieval_tokens_avoided"], 0);
    assert_eq!(savings["total_effective_savings"], 1_000_000);
    assert_eq!(savings["modeled_cost_saved"]["amount_usd"], 2.0);
    assert_eq!(savings["modeled_cost_saved"]["label"], "modeled");
    assert_eq!(savings["unpriced_tokens_saved"], 0);
    let by_model = savings["by_model"]
        .as_array()
        .expect("per-model rows should be an array");
    assert_eq!(by_model.len(), 2);
    assert_eq!(by_model[0]["model"], "claude-sonnet-5-5");
    assert_eq!(by_model[0]["priced"], true);
    assert_eq!(by_model[0]["modeled_cost_saved"]["amount_usd"], 2.0);
    assert_eq!(by_model[1]["model"], "model-without-a-price");
    assert_eq!(by_model[1]["priced"], false);
    assert!(by_model[1]["modeled_cost_saved"]["amount_usd"].is_null());
    assert!(!output.to_string().contains("synthetic transcript body"));
    assert_eq!(
        fixture
            .store
            .stats(None)
            .expect("stats should load")
            .operations,
        0,
        "scanning a transcript must not write ledger rows"
    );

    let (human_exit, human, _) = fixture.run_raw(&["stats", "--transcript", path], b"");
    assert_eq!(human_exit, CliExitCode::Success);
    let human = String::from_utf8(human).expect("human stats should be UTF-8");
    assert!(human.contains("transcript savings"));
    assert!(human.contains("no price on file, tokens only"));
    assert!(human.contains("effective 2026-"));
}

#[test]
fn test_stats_transcript_prefers_ledger_row_over_transcript_numbers() {
    let fixture = Fixture::new();
    let (compress_exit, compressed, _) = fixture.run_json(
        &["compress", "--mode", "passthrough", "--json"],
        b"ledger backed input",
    );
    assert_eq!(compress_exit, CliExitCode::Success);
    let operation_id = compressed["data"]["operation_id"]
        .as_str()
        .expect("operation id should be text");
    let path = fixture.directory.path().join("inflated.jsonl");
    fs::write(
        &path,
        transcript_call(
            "toolu_1",
            "compresso_compress",
            "claude-sonnet-5-5",
            &compress_result(operation_id, 9_000_000, 1),
        ),
    )
    .expect("synthetic transcript should be written");

    let (exit, output, _) = fixture.run_json(
        &[
            "stats",
            "--transcript",
            path.to_str().expect("temporary path should be UTF-8"),
            "--json",
        ],
        b"",
    );

    assert_eq!(exit, CliExitCode::Success);
    let savings = &output["data"]["transcript_savings"];
    assert_eq!(savings["ledger_matched"], 1);
    assert_eq!(savings["transcript_only"], 0);
    assert_eq!(
        savings["net_tokens_saved"],
        output["data"]["net_tokens_saved"]
    );
    assert_eq!(savings["net_tokens_saved"], 0);
    assert_eq!(savings["modeled_cost_saved"]["amount_usd"], 0.0);
}

#[test]
fn test_stats_model_prices_ledger_only_when_model_is_in_price_table() {
    let fixture = Fixture::new();

    let (exit, known, _) =
        fixture.run_json(&["stats", "--model", "claude-sonnet-5-5", "--json"], b"");
    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(known["data"]["cost_basis"]["priced"], true);
    assert_eq!(known["data"]["cost_basis"]["rate"], "input");
    assert_eq!(known["data"]["cost_basis"]["model"], "claude-sonnet-5-5");
    assert!(known["data"]["cost_basis"]["effective_date"].is_string());
    assert!(known["data"]["cost_basis"]["price_table_version"].is_string());
    assert_eq!(known["data"]["modeled_cost_saved"]["amount_usd"], 0.0);

    let (exit, unknown, _) = fixture.run_json(
        &["stats", "--model", "model-without-a-price", "--json"],
        b"",
    );
    assert_eq!(exit, CliExitCode::Success);
    assert_eq!(unknown["data"]["cost_basis"]["priced"], false);
    assert_eq!(unknown["data"]["modeled_cost_saved"]["amount_usd"], 0.0);
}

#[test]
fn test_stats_rejects_unreadable_transcript_and_empty_model() {
    let fixture = Fixture::new();
    let missing = fixture.directory.path().join("missing.jsonl");
    let directory = fixture
        .directory
        .path()
        .to_str()
        .expect("path should be UTF-8");

    for arguments in [
        vec![
            "stats",
            "--transcript",
            missing.to_str().expect("path should be UTF-8"),
            "--json",
        ],
        vec!["stats", "--transcript", directory, "--json"],
        vec!["stats", "--model", "", "--json"],
    ] {
        let (exit, output, errors) = fixture.run_json(&arguments, b"");
        assert_eq!(exit, CliExitCode::Failure);
        assert!(output.is_null());
        assert_eq!(errors["error"]["code"], "invalid_arguments");
    }
}

struct Fixture {
    directory: TempDir,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&directory.path().join("compresso.db")).expect("store should open");
        Self { directory, store }
    }

    fn run_json(&self, arguments: &[&str], input: &[u8]) -> (CliExitCode, Value, Value) {
        let (exit, output, errors) = self.run_raw(arguments, input);
        (
            exit,
            decode_optional_json(&output),
            decode_optional_json(&errors),
        )
    }

    fn run_raw(&self, arguments: &[&str], input: &[u8]) -> (CliExitCode, Vec<u8>, Vec<u8>) {
        let mut argv = vec![OsString::from("compresso")];
        argv.extend(arguments.iter().map(OsString::from));
        let cli = Cli::try_parse_from(argv).expect("command should parse");
        let mut input = Cursor::new(input);
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_with_store_and_input(
            cli,
            &UnavailableAccountClient,
            &self.store,
            self.directory.path(),
            &mut input,
            &mut output,
            &mut errors,
        );
        (exit, output, errors)
    }
}

fn decode_optional_json(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(bytes).expect("command output should be JSON")
    }
}
