use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use compresso_cli::{Cli, CliExitCode, UnavailableAccountClient, execute_with_store};
use compresso_store::Store;
use serde_json::Value;
use tempfile::TempDir;

const FIRST_SOURCE: &str = "pub struct Queue;\nimpl Queue { pub fn push(&self) {} }\n";
const SECOND_SOURCE: &str = "export function drain(): number { return 1; }\n";

#[test]
fn test_fresh_index_approves_repository_and_reports_progress() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run_json(&["index", fixture.root_str(), "--json"]);

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["data"]["action"], "index");
    assert_eq!(output["data"]["dry_run"], false);
    assert_eq!(output["data"]["report"]["files_indexed"], 2);
    assert_eq!(output["data"]["report"]["files_removed"], 0);
    assert_eq!(
        output["data"]["progress"]["parsed"],
        serde_json::json!(["src/lib.rs", "src/worker.ts"])
    );
    assert_eq!(output["data"]["progress"]["removed"], serde_json::json!([]));

    let canonical = fs::canonicalize(&fixture.root).expect("repository should canonicalize");
    let repository = fixture
        .store
        .repo_by_root(&canonical)
        .expect("repository lookup should succeed")
        .expect("repository should be approved");
    assert!(repository.last_indexed_at.is_some());
    assert_eq!(
        fixture
            .store
            .indexed_files(repository.id)
            .expect("indexed files should load")
            .len(),
        2
    );
}

#[test]
fn test_noop_reindex_parses_zero_files() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run_json(&["index", "--json"]).0,
        CliExitCode::Success
    );

    let (exit, output, errors) = fixture.run_json(&["index", "--json"]);

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["data"]["report"]["files_indexed"], 0);
    assert_eq!(output["data"]["report"]["files_removed"], 0);
    assert_eq!(output["data"]["progress"]["parsed"], serde_json::json!([]));
    assert_eq!(output["data"]["progress"]["removed"], serde_json::json!([]));
}

#[test]
fn test_changed_file_reindexes_only_that_file() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.run_json(&["index", "--json"]).0,
        CliExitCode::Success
    );
    fs::write(
        fixture.root.join("src/worker.ts"),
        "export function drain(): number { return 2; }\n",
    )
    .expect("fixture source should change");

    let (exit, output, errors) = fixture.run_json(&["index", "--json"]);

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["data"]["report"]["files_indexed"], 1);
    assert_eq!(
        output["data"]["progress"]["parsed"],
        serde_json::json!(["src/worker.ts"])
    );
}

#[test]
fn test_default_path_uses_injected_working_directory() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run_json(&["index", "--json"]);

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(
        output["data"]["root"],
        fs::canonicalize(&fixture.root)
            .expect("repository should canonicalize")
            .to_string_lossy()
            .as_ref()
    );
}

#[test]
fn test_dry_run_previews_without_store_or_repository_writes() {
    let fixture = Fixture::new();
    let changes_before = fixture
        .store
        .total_changes()
        .expect("change count should load");
    let files_before = snapshot(&fixture.root);

    let (exit, output, errors) = fixture.run_json(&["index", "--dry-run", "--json"]);

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["data"]["action"], "index");
    assert_eq!(output["data"]["dry_run"], true);
    assert!(output["data"]["report"].is_null());
    assert_eq!(output["data"]["progress"]["parsed"], serde_json::json!([]));
    assert_eq!(
        fixture
            .store
            .total_changes()
            .expect("change count should load"),
        changes_before
    );
    assert!(
        fixture
            .store
            .approved_repositories()
            .expect("repositories should load")
            .is_empty()
    );
    assert_eq!(snapshot(&fixture.root), files_before);
}

#[test]
fn test_missing_and_non_directory_paths_return_stable_json_errors() {
    let fixture = Fixture::new();
    let missing = fixture.root.join("missing");
    let file = fixture.root.join("README.md");
    fs::write(&file, "not a directory\n").expect("fixture file should be written");

    for (path, expected_message) in [
        (missing, "index path does not exist or cannot be resolved"),
        (file, "index path is not a directory"),
    ] {
        let (exit, output, errors) = fixture.run_json(&[
            "index",
            path.to_str().expect("temporary path should be UTF-8"),
            "--json",
        ]);
        assert_eq!(exit, CliExitCode::Failure);
        assert!(output.is_null());
        assert_eq!(errors["error"]["code"], "invalid_arguments");
        assert_eq!(errors["error"]["message"], expected_message);
    }
}

#[test]
fn test_index_json_shape_is_versioned_and_stable() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run_json(&["index", "--json"]);

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    let data = output["data"]
        .as_object()
        .expect("data should be an object");
    assert_eq!(
        data.keys().map(String::as_str).collect::<Vec<_>>(),
        ["action", "dry_run", "progress", "report", "root"]
    );
    let report = data["report"]
        .as_object()
        .expect("report should be an object");
    assert_eq!(
        report.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "call_site_files_rejected",
            "call_site_files_truncated",
            "call_sites",
            "duration_ms",
            "files_indexed",
            "files_removed",
            "files_skipped",
            "git_exclude_unusable",
            "imports_resolved",
            "imports_unresolved",
            "stale",
            "symbols",
        ]
    );
}

struct Fixture {
    _directory: TempDir,
    root: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let root = directory.path().join("repo");
        fs::create_dir_all(root.join("src")).expect("fixture repository should be created");
        fs::write(root.join("src/lib.rs"), FIRST_SOURCE).expect("Rust source should be written");
        fs::write(root.join("src/worker.ts"), SECOND_SOURCE)
            .expect("TypeScript source should be written");
        let store =
            Store::open(&directory.path().join("state/compresso.db")).expect("store should open");
        Self {
            _directory: directory,
            root,
            store,
        }
    }

    fn root_str(&self) -> &str {
        self.root
            .to_str()
            .expect("temporary path should be valid UTF-8")
    }

    fn run_json(&self, arguments: &[&str]) -> (CliExitCode, Value, Value) {
        let argv = std::iter::once(OsString::from("compresso"))
            .chain(arguments.iter().map(OsString::from))
            .collect::<Vec<_>>();
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
        (exit, parse_or_null(&output), parse_or_null(&errors))
    }
}

fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    collect_snapshot(root, root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn collect_snapshot(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
    let mut entries = fs::read_dir(directory)
        .expect("fixture directory should be readable")
        .map(|entry| entry.expect("fixture entry should be readable").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_snapshot(root, &path, files);
        } else {
            files.push((
                path.strip_prefix(root)
                    .expect("fixture file should remain under root")
                    .to_path_buf(),
                fs::read(path).expect("fixture file should be readable"),
            ));
        }
    }
}

fn parse_or_null(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(bytes).expect("command output should be JSON")
    }
}
