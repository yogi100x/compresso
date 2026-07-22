use std::ffi::OsString;
use std::fs;
use std::io::Cursor;

use clap::Parser;
use compresso_cli::{Cli, CliExitCode, UnavailableAccountClient, execute_with_store_and_input};
use compresso_core::Trace;
use compresso_store::Store;
use serde_json::Value;
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
    assert!(
        output["data"]["content"]
            .as_str()
            .expect("compressed content should be text")
            .contains("[×500")
    );
    assert!(
        output["data"]["after_tokens"].as_u64().unwrap()
            < output["data"]["before_tokens"].as_u64().unwrap()
    );
    assert_eq!(output["data"]["fallback_used"], false);

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
            "compression_overhead",
            "fallback_rate",
            "modeled_cost_saved",
            "net_tokens_saved",
            "observed_tokens",
            "operations",
            "removed_tokens",
        ])
    );
    assert_eq!(output["data"]["modeled_cost_saved"]["label"], "modeled");

    let (human_exit, human, human_errors) = fixture.run_raw(&["stats"], b"");
    assert_eq!(human_exit, CliExitCode::Success);
    assert!(human_errors.is_empty());
    assert!(
        String::from_utf8(human)
            .expect("human stats should be UTF-8")
            .contains("modeled")
    );
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
