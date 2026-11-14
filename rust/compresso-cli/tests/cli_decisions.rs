//! `compresso decisions`: ledger decisions to a pull request comment draft.
//!
//! Every test injects a recording poster stub; nothing here reaches a network
//! or starts a process.

use std::ffi::OsString;
use std::sync::Mutex;

use clap::Parser;
use compresso_cli::{
    Cli, CliExitCode, PostError, PullRequestPoster, RootCommand, UnavailableAccountClient,
    execute_decisions, execute_with_store,
};
use compresso_core::{
    CompressionBudget, ContextGraph, Engine, Lossiness, Mode, NodeKind, TokenProvider, counter_for,
    operation_identity,
};
use compresso_store::{OperationRecord, RetrievalMetrics, SessionId, Store};
use serde_json::Value;
use tempfile::TempDir;

const LOG_FIXTURE: &str = include_str!("../../../fixtures/logs/repeated-npm-install/input.txt");
const SECRET_CANARY: &str = "ghp_decisioncanary0123456789abcdefghij";

#[derive(Default)]
struct RecordingPoster {
    calls: Mutex<Vec<(u64, String)>>,
    failure: Option<PostError>,
}

impl RecordingPoster {
    fn failing(failure: PostError) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            failure: Some(failure),
        }
    }

    fn calls(&self) -> Vec<(u64, String)> {
        self.calls.lock().expect("poster lock should hold").clone()
    }
}

impl PullRequestPoster for RecordingPoster {
    fn post_comment(&self, pull_request: u64, body: &str) -> Result<(), PostError> {
        self.calls
            .lock()
            .expect("poster lock should hold")
            .push((pull_request, body.to_owned()));
        self.failure.map_or(Ok(()), Err)
    }
}

struct Fixture {
    temp: TempDir,
    store: Store,
    older: SessionId,
    newer: SessionId,
}

impl Fixture {
    fn empty() -> (TempDir, Store) {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let store = Store::open(&temp.path().join("compresso.db")).expect("store should open");
        (temp, store)
    }

    fn new() -> Self {
        let (temp, store) = Self::empty();
        let older = store
            .start_session("codex", "stdio")
            .expect("older session should start");
        record_retrieval(&store, &older, "compresso_search", 1_600_000_000);
        store
            .close_session(&older)
            .expect("older session should close");

        // The client name is supplied by another program: it carries markup,
        // a mention, and a credential that must never reach a pull request.
        let newer = store
            .start_session(
                &format!("claude-code @octocat `x` {SECRET_CANARY}"),
                "stdio",
            )
            .expect("newer session should start");
        record_compression(&store, &newer);
        let now = operation_identity().created_at;
        record_retrieval(&store, &newer, "compresso_context", now);
        record_retrieval(&store, &newer, "compresso_context", now);
        record_retrieval(&store, &newer, "compresso_read", now);
        Self {
            temp,
            store,
            older,
            newer,
        }
    }

    fn run(&self, poster: &RecordingPoster, arguments: &[&str]) -> (CliExitCode, String, String) {
        run(&self.store, poster, arguments)
    }

    fn run_json(&self, poster: &RecordingPoster, arguments: &[&str]) -> Value {
        let (exit, output, errors) = self.run(poster, arguments);
        assert_eq!(exit, CliExitCode::Success, "{errors}");
        assert!(errors.is_empty());
        serde_json::from_str(&output).expect("decisions output should be JSON")
    }
}

fn run(
    store: &Store,
    poster: &RecordingPoster,
    arguments: &[&str],
) -> (CliExitCode, String, String) {
    let mut argv = vec![OsString::from("compresso"), OsString::from("decisions")];
    argv.extend(arguments.iter().map(OsString::from));
    let cli = Cli::try_parse_from(argv).expect("decisions command should parse");
    let RootCommand::Decisions(arguments) = cli.command else {
        panic!("decisions should be selected");
    };
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let exit = execute_decisions(arguments, store, poster, &mut output, &mut errors);
    (
        exit,
        String::from_utf8(output).expect("output should be UTF-8"),
        String::from_utf8(errors).expect("errors should be UTF-8"),
    )
}

fn error_code(errors: &str) -> String {
    let error: Value = serde_json::from_str(errors).expect("error should be JSON");
    error["error"]["code"]
        .as_str()
        .expect("error code should be text")
        .to_owned()
}

fn record_retrieval(store: &Store, session: &SessionId, tool: &str, created_at: i64) {
    store
        .record_operation(&OperationRecord {
            id: operation_identity().id,
            session_id: Some(session.clone()),
            tool: tool.to_owned(),
            mode: Mode::Safe,
            observed_tokens: 120,
            applied_tokens: 120,
            compression_overhead: 0,
            fallback_used: false,
            tokens_served: Some(120),
            retrieval: Some(RetrievalMetrics {
                repository_id: None,
                estimated_raw_source_tokens: 900,
                modeled_follow_up_calls_avoided: 1,
            }),
            trace_json: None,
            created_at,
        })
        .expect("retrieval operation should record");
}

fn record_compression(store: &Store, session: &SessionId) {
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let graph = ContextGraph::from_text(LOG_FIXTURE, NodeKind::ToolResponse, counter);
    let budget = CompressionBudget {
        max_tokens: 2_000,
        max_latency_ms: 10_000,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    };
    let result = Engine::new(Mode::Safe, counter).compress(graph, &budget);
    let mut trace = result.trace;
    trace.session_id = Some(session.as_str().to_owned());
    store
        .record_operation(&OperationRecord {
            id: trace.operation_id.clone(),
            session_id: Some(session.clone()),
            tool: "compresso_compress".to_owned(),
            mode: Mode::Safe,
            observed_tokens: result.before_tokens,
            applied_tokens: result.after_tokens,
            compression_overhead: 0,
            fallback_used: result.fallback_used,
            tokens_served: None,
            retrieval: None,
            trace_json: Some(serde_json::to_string(&trace).expect("trace should serialize")),
            created_at: trace.created_at,
        })
        .expect("compression operation should record");
}

#[test]
fn test_decisions_preview_is_the_default_and_never_calls_the_poster() {
    let fixture = Fixture::new();
    let poster = RecordingPoster::default();

    let preview = fixture.run_json(&poster, &["--json"]);
    let unconfirmed = fixture.run_json(&poster, &["--pr", "12", "--post", "--json"]);
    let dry_run = fixture.run_json(
        &poster,
        &["--pr", "12", "--post", "--yes", "--dry-run", "--json"],
    );
    let (human_exit, human, _) = fixture.run(&poster, &[]);

    assert_eq!(preview["schema_version"], 1);
    let data = &preview["data"];
    assert_eq!(data["status"], "preview");
    assert_eq!(data["posted"], false);
    assert_eq!(data["pull_request"], Value::Null);
    assert_eq!(data["session_id"], fixture.newer.as_str());
    assert_eq!(data["operations"], 4);
    let draft = data["draft"].as_str().expect("draft should be text");
    assert!(draft.starts_with("## Compresso session decisions\n"));
    assert!(draft.contains(&format!("- Session: `{}`", fixture.newer)));
    assert!(draft.contains("- Operations recorded: 4 (newest 4 summarized)"));
    assert!(draft.contains("`compresso_compress` in `safe` mode:"));
    assert!(draft.contains("| `compresso_context` | 2 |"));
    assert!(draft.contains("| `compresso_read` | 1 |"));
    assert!(!draft.contains("compresso_search"));
    assert!(draft.contains("content-free operation ledger"));
    assert!(!draft.contains("registry.npmjs.org"));
    let decisions = data["decisions"].as_array().expect("decisions should list");
    assert_eq!(decisions[0]["kind"], "compression");
    assert_eq!(decisions[0]["tool"], "compresso_compress");
    assert!(
        decisions[0]["before_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    );
    assert_eq!(decisions.len(), 3);

    assert_eq!(unconfirmed["data"]["status"], "confirmation_required");
    assert_eq!(unconfirmed["data"]["posted"], false);
    assert_eq!(unconfirmed["data"]["pull_request"], 12);
    assert_eq!(dry_run["data"]["status"], "preview");
    assert_eq!(dry_run["data"]["posted"], false);
    assert_eq!(human_exit, CliExitCode::Success);
    assert!(human.starts_with("status: preview only; nothing was posted\n\n## Compresso"));
    assert!(poster.calls().is_empty());
}

#[test]
fn test_decisions_post_only_with_pr_and_explicit_confirmation() {
    let fixture = Fixture::new();
    let poster = RecordingPoster::default();

    let (missing_pr_exit, _, missing_pr) = fixture.run(&poster, &["--post", "--yes", "--json"]);
    let (zero_pr_exit, _, zero_pr) =
        fixture.run(&poster, &["--pr", "0", "--post", "--yes", "--json"]);
    assert!(poster.calls().is_empty());
    let preview = fixture.run_json(&poster, &["--json"]);
    let posted = fixture.run_json(&poster, &["--pr", "12", "--post", "--yes", "--json"]);

    assert_eq!(missing_pr_exit, CliExitCode::Failure);
    assert_eq!(error_code(&missing_pr), "invalid_arguments");
    assert_eq!(zero_pr_exit, CliExitCode::Failure);
    assert_eq!(error_code(&zero_pr), "invalid_arguments");
    assert_eq!(posted["data"]["status"], "posted");
    assert_eq!(posted["data"]["posted"], true);
    assert_eq!(posted["data"]["pull_request"], 12);
    let calls = poster.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 12);
    assert_eq!(Some(calls[0].1.as_str()), preview["data"]["draft"].as_str());
}

#[test]
fn test_decisions_redact_secrets_and_neutralize_untrusted_ledger_text() {
    let fixture = Fixture::new();
    let poster = RecordingPoster::default();

    let (exit, output, errors) = fixture.run(&poster, &["--pr", "7", "--post", "--yes", "--json"]);

    assert_eq!(exit, CliExitCode::Success, "{errors}");
    let posted = &poster.calls()[0].1;
    for rendered in [output.as_str(), posted.as_str()] {
        assert!(!rendered.contains(SECRET_CANARY));
        assert!(!rendered.contains("@octocat"));
        assert!(rendered.contains("[REDACTED:token]"));
    }
    assert!(posted.contains("(client `claude-code octocat x [REDACTED:token]`, stdio)"));
    let output: Value = serde_json::from_str(&output).expect("output should be JSON");
    assert_eq!(output["data"]["redactions"], 1);
}

#[test]
fn test_decisions_report_poster_failures_and_the_system_poster_never_posts() {
    let fixture = Fixture::new();
    let missing = RecordingPoster::failing(PostError::GhNotFound);

    let (exit, output, errors) =
        fixture.run(&missing, &["--pr", "12", "--post", "--yes", "--json"]);

    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_empty());
    assert_eq!(error_code(&errors), "gh_not_found");
    assert!(errors.contains("nothing was posted"));
    assert!(errors.contains("gh pr comment 12 --body-file -"));
    assert!(!errors.contains(SECRET_CANARY));

    // The production wiring looks for `gh` but may not start it, so a
    // confirmed post fails closed whether or not `gh` is installed.
    let cli = Cli::try_parse_from([
        "compresso",
        "decisions",
        "--pr",
        "12",
        "--post",
        "--yes",
        "--json",
    ])
    .expect("decisions should parse");
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let exit = execute_with_store(
        cli,
        &UnavailableAccountClient,
        &fixture.store,
        fixture.temp.path(),
        &mut output,
        &mut errors,
    );
    assert_eq!(exit, CliExitCode::Failure);
    assert!(output.is_empty());
    let code = error_code(&String::from_utf8(errors).expect("errors should be UTF-8"));
    assert!(
        matches!(code.as_str(), "gh_not_found" | "post_unavailable"),
        "{code}"
    );
}

#[test]
fn test_decisions_select_the_newest_session_or_an_explicit_one() {
    let fixture = Fixture::new();
    let poster = RecordingPoster::default();
    let (_empty_temp, empty_store) = Fixture::empty();
    let idle = fixture
        .store
        .start_session("cursor", "stdio")
        .expect("idle session should start");

    let older = fixture.run_json(&poster, &["--session", fixture.older.as_str(), "--json"]);
    let limited = fixture.run_json(&poster, &["--limit", "1", "--json"]);

    assert_eq!(older["data"]["session_id"], fixture.older.as_str());
    let draft = older["data"]["draft"]
        .as_str()
        .expect("draft should be text");
    assert!(draft.contains("(client `codex`, stdio)"));
    assert!(draft.contains("| `compresso_search` | 1 |"));
    assert!(draft.contains("### Compression decisions\n\n_None recorded._"));
    assert!(!draft.contains("still open"));
    assert!(draft.contains("- Window: "));
    assert_eq!(limited["data"]["session_id"], fixture.newer.as_str());
    assert!(
        limited["data"]["draft"]
            .as_str()
            .is_some_and(|draft| draft.contains("- Operations recorded: 4 (newest 1 summarized)"))
    );

    for (arguments, store, expected) in [
        (
            vec!["--session", "not-a-ulid", "--json"],
            &fixture.store,
            "invalid_arguments",
        ),
        (
            vec!["--session", "01ARZ3NDEKTSV4RRFFQ69G5FAV", "--json"],
            &fixture.store,
            "not_found",
        ),
        (
            vec!["--session", idle.as_str(), "--json"],
            &fixture.store,
            "no_decisions",
        ),
        (
            vec!["--limit", "0", "--json"],
            &fixture.store,
            "invalid_arguments",
        ),
        (
            vec!["--limit", "201", "--json"],
            &fixture.store,
            "invalid_arguments",
        ),
        (vec!["--json"], &empty_store, "no_session"),
    ] {
        let (exit, output, errors) = run(store, &poster, &arguments);
        assert_eq!(exit, CliExitCode::Failure, "{arguments:?}");
        assert!(output.is_empty());
        assert_eq!(error_code(&errors), expected, "{arguments:?}");
    }
    assert!(poster.calls().is_empty());
}
