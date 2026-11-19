//! E17-32: `compresso edit`, the operator-run form of the opt-in edit tool.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use clap::Parser;
use compresso_atlas::Indexer;
use compresso_cli::{Cli, CliExitCode, UnavailableAccountClient, execute_with_store_and_input};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{StatsFilter, Store};
use serde_json::Value;
use tempfile::TempDir;

const LIB_RS: &str = "pub fn format_utc() -> u8 {\n    1\n}\n\npub fn format_utc_now() -> u8 {\n    format_utc()\n}\n";

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src")).expect("src");
        fs::write(root.join("src/lib.rs"), LIB_RS).expect("lib");
        fs::write(root.join("notes.txt"), "alpha\nbeta\n").expect("notes");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store");
        Indexer::new(&store, counter_for(TokenProvider::OpenAiO200k))
            .index(&root, |_| {})
            .expect("index");
        let root = fs::canonicalize(root).expect("canonical root");
        Self {
            _temp: temp,
            root,
            store,
        }
    }

    fn run(&self, arguments: &[&str], stdin: &str) -> (CliExitCode, String, String) {
        let mut argv = vec![OsString::from("compresso")];
        argv.extend(arguments.iter().map(OsString::from));
        let cli = Cli::try_parse_from(argv).expect("command parses");
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_with_store_and_input(
            cli,
            &UnavailableAccountClient,
            &self.store,
            &self.root,
            &mut stdin.as_bytes(),
            &mut output,
            &mut errors,
        );
        (
            exit,
            String::from_utf8(output).expect("utf-8"),
            String::from_utf8(errors).expect("utf-8"),
        )
    }

    fn text(&self, path: &str) -> String {
        fs::read_to_string(self.root.join(path)).expect("file")
    }
}

#[test]
fn test_cli_rename_writes_reports_and_records_the_edit() {
    let fixture = Fixture::new();
    let (exit, output, errors) = fixture.run(
        &[
            "edit",
            "--rename",
            "format_utc",
            "format_iso",
            "--path",
            "src/lib.rs",
            "--expected-count",
            "2",
            "--json",
        ],
        "",
    );
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let output: Value = serde_json::from_str(&output).expect("JSON");
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["data"]["sites_total"], 2);
    assert_eq!(output["data"]["index"], "fresh");
    assert_eq!(output["data"]["operations"][0]["remaining_whole_total"], 0);
    assert_eq!(
        output["data"]["operations"][0]["remaining_substring_total"],
        1
    );
    assert_eq!(
        fixture.text("src/lib.rs"),
        LIB_RS.replace("format_utc()", "format_iso()")
    );
    let stats = fixture
        .store
        .detailed_stats(&StatsFilter {
            since: None,
            session_id: None,
            operation_limit: None,
        })
        .expect("stats");
    assert!(
        stats
            .by_tool
            .iter()
            .any(|tool| tool.tool == "compresso_edit")
    );
}

#[test]
fn test_cli_dry_run_prints_the_diff_and_writes_nothing() {
    let fixture = Fixture::new();
    let (exit, output, errors) = fixture.run(
        &[
            "edit",
            "--replace",
            "notes.txt",
            "--old",
            "beta",
            "--new",
            "gamma",
            "--dry-run",
        ],
        "",
    );
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert!(
        output.starts_with("would edit 1 site(s) in 1 file(s)\n"),
        "{output}"
    );
    assert!(output.contains("-beta\n+gamma\n"), "{output}");
    assert!(output.contains("[dry run: nothing written]"), "{output}");
    assert_eq!(fixture.text("notes.txt"), "alpha\nbeta\n");
}

#[test]
fn test_cli_operations_from_stdin_apply_all_or_nothing() {
    let fixture = Fixture::new();
    let operations = r#"[
        {"op": "replace", "path": "notes.txt", "old": "alpha", "new": "ALPHA"},
        {"op": "insert", "path": "notes.txt", "anchor": "beta\n", "text": "gamma\n"}
    ]"#;
    let (exit, _, errors) = fixture.run(&["edit", "--operations", "-"], operations);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    assert_eq!(fixture.text("notes.txt"), "ALPHA\nbeta\ngamma\n");

    let failing = r#"[
        {"op": "replace", "path": "notes.txt", "old": "ALPHA", "new": "x"},
        {"op": "replace", "path": "notes.txt", "old": "missing", "new": "y"}
    ]"#;
    let (exit, _, errors) = fixture.run(&["edit", "--operations", "-", "--json"], failing);
    assert_eq!(exit, CliExitCode::Failure);
    let error: Value = serde_json::from_str(&errors).expect("JSON error");
    assert_eq!(error["error"]["code"], "count_mismatch");
    assert_eq!(fixture.text("notes.txt"), "ALPHA\nbeta\ngamma\n");
}

#[test]
fn test_cli_edit_requires_exactly_one_operation_form() {
    assert!(Cli::try_parse_from(["compresso", "edit"]).is_err());
    assert!(
        Cli::try_parse_from(["compresso", "edit", "--rename", "a", "b", "--replace", "x"]).is_err()
    );
    assert!(Cli::try_parse_from(["compresso", "edit", "--old", "a"]).is_err());
    let fixture = Fixture::new();
    let (exit, _, errors) = fixture.run(&["edit", "--replace", "notes.txt", "--old", "a"], "");
    assert_eq!(exit, CliExitCode::Failure);
    assert!(
        errors.contains("--replace requires --old and --new"),
        "{errors}"
    );
    let (exit, _, errors) = fixture.run(
        &["edit", "--replace", ".env", "--old", "a", "--new", "b"],
        "",
    );
    assert_eq!(exit, CliExitCode::Failure);
    assert!(
        errors.contains("edit_refused") || errors.contains("secret"),
        "{errors}"
    );
}
