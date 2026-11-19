use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use clap::Parser;
use compresso_atlas::Indexer;
use compresso_cli::{Cli, CliExitCode, UnavailableAccountClient, execute_with_store};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
use serde_json::Value;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let root = temp.path().join("repo");
        fs::create_dir_all(root.join("src")).expect("source directory should be created");
        fs::write(
            root.join("src/lib.rs"),
            "mod store;\n\npub fn entry() {\n    store::save();\n}\n",
        )
        .expect("fixture should be written");
        fs::write(
            root.join("src/store.rs"),
            "pub fn save() {\n    write();\n}\n\nfn write() {}\n",
        )
        .expect("fixture should be written");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        Indexer::new(&store, counter_for(TokenProvider::Heuristic))
            .index(&root, |_| {})
            .expect("fixture should index");
        let root = fs::canonicalize(root).expect("root should canonicalize");
        Self {
            _temp: temp,
            root,
            store,
        }
    }

    fn run(&self, arguments: &[&str]) -> (CliExitCode, String, String) {
        let mut argv = vec![OsString::from("compresso")];
        argv.extend(arguments.iter().map(OsString::from));
        let cli = Cli::try_parse_from(argv).expect("command should parse");
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_with_store(
            cli,
            &UnavailableAccountClient,
            &self.store,
            &self.root,
            &mut output,
            &mut errors,
        );
        (
            exit,
            String::from_utf8(output).expect("utf-8 output"),
            String::from_utf8(errors).expect("utf-8 errors"),
        )
    }
}

#[test]
fn test_cli_trace_prints_a_call_path_as_json_and_text() {
    let fixture = Fixture::new();
    let (exit, output, errors) =
        fixture.run(&["trace", "--from", "entry", "--to", "write", "--json"]);
    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let output: Value = serde_json::from_str(&output).expect("JSON output");
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["data"]["status"], "paths");
    let hops = output["data"]["paths"][0].as_array().expect("one path");
    let symbols = hops
        .iter()
        .map(|hop| hop["symbol"].as_str().expect("symbol"))
        .collect::<Vec<_>>();
    assert_eq!(symbols, ["entry", "save", "write"]);
    assert_eq!(hops[1]["call_site"], "src/lib.rs:4");
    assert_eq!(hops[2]["defined_at"], "src/store.rs:5");
    assert_eq!(output["data"]["complete"], true);

    let (exit, output, _) = fixture.run(&["trace", "--to", "save"]);
    assert_eq!(exit, CliExitCode::Success);
    assert!(output.contains("entry (function) src/lib.rs:3"));
    assert!(output.contains("calls save at src/lib.rs:4"));
}

#[test]
fn test_cli_trace_requires_an_end_and_valid_bounds() {
    let fixture = Fixture::new();
    let (exit, _, errors) = fixture.run(&["trace", "--json"]);
    assert_eq!(exit, CliExitCode::Failure);
    assert!(errors.contains("invalid_arguments"));
    let (exit, _, errors) =
        fixture.run(&["trace", "--from", "entry", "--max-depth", "9", "--json"]);
    assert_eq!(exit, CliExitCode::Failure);
    assert!(errors.contains("invalid_arguments"));
}
