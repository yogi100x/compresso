//! `compresso recall` over synthetic transcripts in a scratch home.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use compresso_cli::{
    Cli, CliExitCode, RecallEnvironment, RootCommand, UnavailableAccountClient, execute_recall,
    execute_with_store,
};
use compresso_store::Store;
use serde_json::{Value, json};
use tempfile::TempDir;

const SECRET_CANARY: &str = "sk-ant-api03-ZZclirecallcanary0123456789";

struct Fixture {
    _temp: TempDir,
    environment: RecallEnvironment,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let home = temp.path().join("home");
        let repo = temp.path().join("work/repo");
        let other = temp.path().join("work/other");
        fs::create_dir_all(repo.join(".git")).expect("repository should be created");
        fs::create_dir_all(repo.join("crates/core")).expect("nested directory should be created");
        fs::create_dir_all(&other).expect("other repository should be created");
        let repo = fs::canonicalize(repo).expect("repository should canonicalize");
        let other = fs::canonicalize(other).expect("other repository should canonicalize");
        write_jsonl(
            &home.join(".claude/projects/-work-repo/aaaa.jsonl"),
            &[
                claude_line(
                    &repo,
                    "2026-06-01T10:00:00.000Z",
                    "Decision: migrations run inside one transaction per file.",
                ),
                claude_line(
                    &repo,
                    "2026-06-01T10:02:00.000Z",
                    &format!("ran migrations with API key {SECRET_CANARY} exported"),
                ),
            ],
        );
        write_jsonl(
            &home.join(".claude/projects/-work-other/bbbb.jsonl"),
            &[claude_line(
                &other,
                "2026-06-02T10:00:00.000Z",
                "Other repository migrations are hand-applied.",
            )],
        );
        write_jsonl(
            &home.join(".codex/sessions/2026/06/03/rollout-2026-06-03T09-00-00-thread-c.jsonl"),
            &[
                json!({ "timestamp": "2026-06-03T09:00:00.000Z", "type": "session_meta",
                    "payload": { "id": "thread-c", "cwd": repo } }),
                json!({ "timestamp": "2026-06-03T09:00:05.000Z", "type": "response_item",
                    "payload": { "type": "message", "role": "assistant", "content": [
                        { "type": "output_text", "text": "Chose forward-only migrations." }
                    ] } }),
            ],
        );
        Self {
            environment: RecallEnvironment {
                home_dir: home,
                cwd: repo.join("crates/core"),
            },
            repo,
            _temp: temp,
        }
    }

    fn run(&self, arguments: &[&str]) -> (CliExitCode, String, String) {
        let mut argv = vec![OsString::from("compresso"), OsString::from("recall")];
        argv.extend(arguments.iter().map(OsString::from));
        let cli = Cli::try_parse_from(argv).expect("recall command should parse");
        let RootCommand::Recall(arguments) = cli.command else {
            panic!("recall should be selected");
        };
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let exit = execute_recall(arguments, &self.environment, &mut output, &mut errors);
        (
            exit,
            String::from_utf8(output).expect("output should be UTF-8"),
            String::from_utf8(errors).expect("errors should be UTF-8"),
        )
    }

    fn run_json(&self, arguments: &[&str]) -> Value {
        let (exit, output, errors) = self.run(arguments);
        assert_eq!(exit, CliExitCode::Success, "{errors}");
        assert!(errors.is_empty());
        serde_json::from_str(&output).expect("recall output should be JSON")
    }
}

fn claude_line(cwd: &Path, timestamp: &str, text: &str) -> Value {
    json!({
        "type": "assistant",
        "cwd": cwd,
        "sessionId": "session-a",
        "timestamp": timestamp,
        "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] }
    })
}

fn write_jsonl(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().expect("transcript parent should exist"))
        .expect("transcript directory should be created");
    let body = lines
        .iter()
        .map(|line| line.to_string() + "\n")
        .collect::<String>();
    fs::write(path, body).expect("transcript should be written");
}

#[test]
fn test_recall_json_searches_enabled_sources_in_the_enclosing_repository() {
    let fixture = Fixture::new();

    let output = fixture.run_json(&[
        "migrations",
        "--source",
        "claude-code",
        "--source",
        "codex",
        "--json",
    ]);

    assert_eq!(output["schema_version"], 1);
    let data = &output["data"];
    assert_eq!(data["scope"]["kind"], "repository");
    assert_eq!(data["scope"]["root"], json!(fixture.repo));
    let hits = data["hits"].as_array().expect("hits should be an array");
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0]["source"], "codex");
    assert_eq!(hits[0]["session_id"], "thread-c");
    assert_eq!(
        hits[0]["transcript"],
        "~/.codex/sessions/2026/06/03/rollout-2026-06-03T09-00-00-thread-c.jsonl"
    );
    assert_eq!(hits[0]["line"], 2);
    assert_eq!(data["redactions"], 1);
    assert_eq!(data["truncated"], false);
    let rendered = output.to_string();
    assert!(!rendered.contains(SECRET_CANARY));
    assert!(rendered.contains("[REDACTED:token]"));
    assert!(!rendered.contains("hand-applied"));
}

#[test]
fn test_recall_is_disabled_without_a_source_and_reports_unsupported_cursor() {
    let fixture = Fixture::new();

    let (exit, output, errors) = fixture.run(&["migrations", "--json"]);
    let cursor = fixture.run_json(&["migrations", "--source", "cursor", "--json"]);

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_empty());
    let error: Value = serde_json::from_str(&errors).expect("error should be JSON");
    assert_eq!(error["error"]["code"], "recall_disabled");
    assert!(!errors.contains("transaction per file"));
    assert_eq!(cursor["data"]["hits"], json!([]));
    assert_eq!(cursor["data"]["sources"][0]["source"], "cursor");
    assert_eq!(cursor["data"]["sources"][0]["status"], "unsupported");
    assert_eq!(cursor["data"]["sources"][0]["files_scanned"], 0);
}

#[test]
fn test_recall_widens_scope_only_with_an_explicit_flag_and_validates_bounds() {
    let fixture = Fixture::new();

    let widened = fixture.run_json(&[
        "migrations",
        "--source",
        "claude-code",
        "--all-repositories",
        "--limit",
        "2",
        "--json",
    ]);
    let (human_exit, human, _) = fixture.run(&["migrations", "--source", "claude-code"]);

    assert_eq!(widened["data"]["scope"]["kind"], "all_repositories");
    let hits = widened["data"]["hits"]
        .as_array()
        .expect("hits should list");
    assert_eq!(hits.len(), 2);
    assert!(
        hits[0]["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains("hand-applied"))
    );
    assert_eq!(human_exit, CliExitCode::Success);
    assert!(human.starts_with("recall: 2 hit(s) in repository "));
    assert!(human.contains("claude-code: searched (2 file(s) scanned, 0 unrecognized)"));
    assert!(human.contains("redacted secrets: 1"));
    assert!(!human.contains(SECRET_CANARY));

    for arguments in [
        vec!["migrations", "--source", "codex", "--limit", "0", "--json"],
        vec!["migrations", "--source", "codex", "--limit", "51", "--json"],
        vec!["  ", "--source", "codex", "--json"],
    ] {
        let (exit, output, errors) = fixture.run(&arguments);
        assert_eq!(exit, CliExitCode::Failure, "{arguments:?}");
        assert!(output.is_empty());
        let error: Value = serde_json::from_str(&errors).expect("error should be JSON");
        assert_eq!(error["error"]["code"], "invalid_arguments");
    }
    Cli::try_parse_from([
        "compresso",
        "recall",
        "x",
        "--root",
        "/repo",
        "--all-repositories",
    ])
    .expect_err("--root and --all-repositories should conflict");
    Cli::try_parse_from(["compresso", "recall", "x", "--source", "claude-desktop"])
        .expect_err("Claude Desktop is not a recall source");
}

#[test]
fn test_recall_requires_an_explicit_environment_at_injected_boundaries() {
    let temp = tempfile::tempdir().expect("temporary directory should be created");
    let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
    let cli = Cli::try_parse_from(["compresso", "recall", "x", "--source", "codex", "--json"])
        .expect("recall should parse");
    let mut output = Vec::new();
    let mut errors = Vec::new();

    let exit = execute_with_store(
        cli,
        &UnavailableAccountClient,
        &store,
        temp.path(),
        &mut output,
        &mut errors,
    );

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_empty());
    let error: Value = serde_json::from_slice(&errors).expect("error should be JSON");
    assert_eq!(error["error"]["code"], "environment_required");
}
