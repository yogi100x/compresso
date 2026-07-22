use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use clap::Parser;
use compresso_atlas::Indexer;
use compresso_cli::{Cli, CliExitCode, UnavailableAccountClient, execute_with_store};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::{EdgeKind, EdgeRecord, EdgeResolution, Store};
use serde_json::{Value, json};
use tempfile::TempDir;

#[test]
fn test_unfold_ambiguity_json_snapshot() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run_json(&[
        "unfold",
        "src/session.rs",
        "duplicate",
        "--root",
        fixture.root_str(),
        "--json",
    ]);

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["data"]["status"], "ambiguous");
    assert_eq!(output["data"]["stale"], false);
    let candidates = output["data"]["candidates"]
        .as_array()
        .expect("candidates should be an array");
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0]["path"], "src/session.rs");
    assert_eq!(candidates[0]["qualified_name"], "duplicate");
    assert_eq!(candidates[1]["qualified_name"], "duplicate");
}

#[test]
fn test_unfold_stale_json_surfaces_current_body_and_hint() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("src/session.rs"),
        format!("// moved\n{}", SESSION_SOURCE),
    )
    .expect("fixture should become stale");

    let (exit, output, errors) = fixture.run_json(&[
        "unfold",
        "src/session.rs",
        "SessionManager",
        "--kind",
        "struct",
        "--root",
        fixture.root_str(),
        "--json",
    ]);

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(output["data"]["status"], "found");
    assert_eq!(output["data"]["stale"], true);
    assert!(
        output["data"]["content"]
            .as_str()
            .expect("content should be text")
            .contains("pub struct SessionManager")
    );
    assert!(
        output["data"]["reindex_hint"]
            .as_str()
            .expect("stale result should include a hint")
            .contains("compresso index")
    );
}

#[test]
fn test_read_root_escape_matches_stable_outside_root_error() {
    let fixture = Fixture::new();
    fs::write(fixture.root.parent().unwrap().join("outside.rs"), "secret")
        .expect("outside file should exist");

    let (exit, output, errors) = fixture.run_json(&[
        "read",
        "../outside.rs",
        "--root",
        fixture.root_str(),
        "--json",
    ]);

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_null());
    assert_eq!(errors["error"]["code"], "outside_root");
    assert_eq!(
        errors["error"]["message"],
        "path resolves outside the approved repository root"
    );
    assert!(!errors.to_string().contains("outside.rs"));
}

#[test]
fn test_impact_surfaces_conservative_unresolved_completeness() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run_json(&[
        "impact",
        "src/session.rs",
        "--root",
        fixture.root_str(),
        "--json",
    ]);

    assert_eq!(exit, CliExitCode::Success);
    assert!(errors.is_null());
    assert_eq!(
        output,
        json!({
            "schema_version": 1,
            "data": {
                "path": "src/session.rs",
                "dependents": [{
                    "path": "src/importer.rs",
                    "depth": 1,
                    "edge_kinds": ["imports"]
                }],
                "complete": false,
                "unresolved_or_ambiguous_sources": ["src/importer.rs"],
                "truncated": false
            }
        })
    );
}

#[test]
fn test_search_and_read_json_snapshots_are_versioned_and_bounded() {
    let fixture = Fixture::new();

    let (search_exit, search, search_errors) = fixture.run_json(&[
        "search",
        "SessionManager",
        "--root",
        fixture.root_str(),
        "--json",
    ]);
    assert_eq!(search_exit, CliExitCode::Success);
    assert!(search_errors.is_null());
    assert_eq!(search["schema_version"], 1);
    assert_eq!(search["data"]["query"], "SessionManager");
    assert_eq!(search["data"]["hits"][0]["path"], "src/session.rs");
    assert_eq!(
        search["data"]["hits"][0]["symbol"]["qualified_name"],
        "SessionManager"
    );

    let (read_exit, read, read_errors) = fixture.run_json(&[
        "read",
        "src/session.rs",
        "--start-line",
        "1",
        "--end-line",
        "3",
        "--max-tokens",
        "100",
        "--root",
        fixture.root_str(),
        "--json",
    ]);
    assert_eq!(read_exit, CliExitCode::Success);
    assert!(read_errors.is_null());
    assert_eq!(read["schema_version"], 1);
    assert_eq!(read["data"]["path"], "src/session.rs");
    assert_eq!(read["data"]["start_line"], 1);
    assert_eq!(read["data"]["end_line"], 3);
    assert_eq!(read["data"]["truncated"], false);
    assert!(read["data"]["tokens_used"].as_u64().unwrap() <= 100);
}

#[test]
fn test_map_outline_and_context_use_approved_cwd_and_canonical_queries() {
    let fixture = Fixture::new();

    let (map_exit, map, map_errors) = fixture.run_json(&["map", "--max-tokens", "200", "--json"]);
    assert_eq!(map_exit, CliExitCode::Success);
    assert!(map_errors.is_null());
    assert_eq!(map["schema_version"], 1);
    assert_eq!(map["data"]["files_total"], 2);
    assert!(
        map["data"]["map"]
            .as_str()
            .expect("map should be text")
            .contains("session.rs")
    );
    assert!(map["data"]["tokens_used"].as_u64().unwrap() <= 200);

    let (outline_exit, outline, outline_errors) =
        fixture.run_json(&["outline", "src/session.rs", "--json"]);
    assert_eq!(outline_exit, CliExitCode::Success);
    assert!(outline_errors.is_null());
    assert_eq!(outline["schema_version"], 1);
    assert_eq!(outline["data"]["language"], "rust");
    assert_eq!(
        outline["data"]["symbols"][0]["qualified_name"],
        "SessionManager"
    );

    let (context_exit, context, context_errors) =
        fixture.run_json(&["context", "SessionManager", "--budget", "800", "--json"]);
    assert_eq!(context_exit, CliExitCode::Success);
    assert!(context_errors.is_null());
    assert_eq!(context["schema_version"], 1);
    assert_eq!(context["data"]["task"], "SessionManager");
    assert!(context["data"]["tokens_used"].as_u64().unwrap() <= 800);
    assert!(context["data"]["plan"]["outlines"].is_array());
    assert!(context["data"]["plan"]["bodies"].is_array());
}

#[test]
fn test_explicit_root_must_be_the_exact_approved_repository() {
    let fixture = Fixture::new();
    let unapproved = fixture.root.parent().unwrap().join("unapproved");
    fs::create_dir(&unapproved).expect("unapproved directory should be created");
    let unapproved = unapproved
        .to_str()
        .expect("unapproved path should be UTF-8");

    let (exit, output, errors) = fixture.run_json(&["map", "--root", unapproved, "--json"]);

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_null());
    assert_eq!(errors["error"]["code"], "not_indexed");
    assert!(
        errors["error"]["message"]
            .as_str()
            .expect("error message should be text")
            .contains("compresso index")
    );
    assert!(!errors.to_string().contains(unapproved));
}

#[test]
fn test_malformed_read_range_is_invalid_arguments() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run_json(&[
        "read",
        "src/session.rs",
        "--end-line",
        "2",
        "--root",
        fixture.root_str(),
        "--json",
    ]);

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_null());
    assert_eq!(errors["error"]["code"], "invalid_arguments");
}

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
        fs::write(root.join("src/session.rs"), SESSION_SOURCE)
            .expect("session source should be written");
        fs::write(root.join("src/importer.rs"), "pub fn importer() {}\n")
            .expect("importer source should be written");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        let counter = counter_for(TokenProvider::OpenAiO200k);
        Indexer::new(&store, counter)
            .index(&root, |_| {})
            .expect("fixture should index");
        let canonical = fs::canonicalize(&root).expect("root should canonicalize");
        let repo = store
            .repo_by_root(&canonical)
            .expect("repository lookup should work")
            .expect("repository should exist")
            .id;
        let importer = store
            .indexed_file(repo, "src/importer.rs")
            .expect("indexed file lookup should work")
            .expect("importer should exist");
        let symbols = store
            .symbols_in_file(repo, "src/importer.rs")
            .expect("importer symbols should load");
        let importer_id = store
            .upsert_file(repo, &importer)
            .expect("importer should reload");
        store
            .replace_symbols(
                importer_id,
                &symbols,
                &[
                    EdgeRecord {
                        to_path: "src/session.rs".to_owned(),
                        kind: EdgeKind::Imports,
                        resolution: EdgeResolution::Resolved,
                    },
                    EdgeRecord {
                        to_path: "external-package".to_owned(),
                        kind: EdgeKind::Imports,
                        resolution: EdgeResolution::Unresolved("not_indexed".to_owned()),
                    },
                ],
            )
            .expect("impact edges should replace");
        Self {
            _temp: temp,
            root: canonical,
            store,
        }
    }

    fn root_str(&self) -> &str {
        self.root.to_str().expect("fixture root should be UTF-8")
    }

    fn run_json(&self, arguments: &[&str]) -> (CliExitCode, Value, Value) {
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
            decode_optional_json(&output),
            decode_optional_json(&errors),
        )
    }
}

fn decode_optional_json(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(bytes).expect("command output should be JSON")
    }
}

const SESSION_SOURCE: &str = concat!(
    "pub struct SessionManager {\n",
    "    attempts: usize,\n",
    "}\n",
    "\n",
    "pub fn duplicate(value: i32) -> i32 { value }\n",
    "pub fn duplicate(value: i64) -> i64 { value }\n",
);
