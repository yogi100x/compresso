//! End-to-end recall behavior over synthetic transcripts in a scratch home.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use compresso_recall::{
    RecallError, RecallLimits, RecallReport, RecallRequest, RecallScope, RecallSource,
    SourceStatus, recall,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const SECRET_CANARY: &str = "sk-ant-api03-ZZrecallcanary0123456789abcdef";
const PASSWORD_CANARY: &str = "hunter2-recall-canary";

struct ScratchHome {
    temp: TempDir,
    repo: PathBuf,
    other_repo: PathBuf,
}

impl ScratchHome {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("scratch home should be created");
        let repo = temp.path().join("work/repo");
        let other_repo = temp.path().join("work/other");
        fs::create_dir_all(&repo).expect("repository should be created");
        fs::create_dir_all(&other_repo).expect("other repository should be created");
        let home = Self {
            temp,
            repo,
            other_repo,
        };
        home.seed();
        home
    }

    fn home(&self) -> &Path {
        self.temp.path()
    }

    fn seed(&self) {
        let repo = self.repo.to_str().expect("repository path should be UTF-8");
        let other = self
            .other_repo
            .to_str()
            .expect("other repository path should be UTF-8");
        self.write_claude(
            "-work-repo/11111111-aaaa.jsonl",
            &[
                json!({ "type": "summary", "summary": "retry work", "leafUuid": "leaf" }),
                claude_line("user", repo, "session-claude-1", "2026-03-01T10:00:00.000Z",
                    json!("Why does the upload retry loop spin forever?")),
                claude_line("assistant", repo, "session-claude-1", "2026-03-01T10:00:09.000Z",
                    json!([
                        { "type": "text", "text": "Decision: cap the upload retry loop at five attempts with jittered backoff." },
                        { "type": "tool_use", "name": "Bash", "input": { "command": "cargo test -p uploader retry" } }
                    ])),
                claude_line("user", repo, "session-claude-1", "2026-03-01T10:01:00.000Z",
                    json!(format!("deploy with ANTHROPIC_API_KEY={SECRET_CANARY} and password = {PASSWORD_CANARY} for the retry worker"))),
            ],
        );
        self.write_claude(
            "-work-other/22222222-bbbb.jsonl",
            &[claude_line(
                "assistant",
                other,
                "session-claude-2",
                "2026-03-05T08:00:00.000Z",
                json!("Unrelated repository: the retry policy here is exponential."),
            )],
        );
        self.write_codex(
            "2026/03/02/rollout-2026-03-02T09-00-00-thread-codex-1.jsonl",
            &[
                json!({ "timestamp": "2026-03-02T09:00:00.000Z", "type": "session_meta",
                    "payload": { "id": "thread-codex-1", "cwd": repo, "originator": "codex" } }),
                codex_message(
                    "2026-03-02T09:00:05.000Z",
                    "user",
                    "input_text",
                    "Should the retry budget live in config?",
                ),
                codex_message(
                    "2026-03-02T09:00:30.000Z",
                    "assistant",
                    "output_text",
                    "Chose to keep the retry budget in config.toml so operators can tune it.",
                ),
                json!({ "timestamp": "2026-03-02T09:00:31.000Z", "type": "response_item",
                    "payload": { "type": "reasoning", "summary": [] } }),
            ],
        );
        self.write_codex(
            "2026/03/03/rollout-2026-03-03T09-00-00-thread-codex-2.jsonl",
            &[
                json!({ "timestamp": "2026-03-03T09:00:00.000Z", "type": "session_meta",
                    "payload": { "id": "thread-codex-2", "cwd": other } }),
                codex_message(
                    "2026-03-03T09:00:05.000Z",
                    "assistant",
                    "output_text",
                    "Other repository retry notes.",
                ),
            ],
        );
        // A format this crate has never verified: flat role/text records.
        let legacy = self
            .home()
            .join(".codex/sessions/2026/03/04/rollout-2026-03-04T09-00-00-legacy.jsonl");
        fs::create_dir_all(legacy.parent().expect("legacy parent should exist"))
            .expect("legacy directory should be created");
        fs::write(
            &legacy,
            "{\"role\":\"assistant\",\"text\":\"legacy retry guess\"}\nnot json at all\n",
        )
        .expect("legacy transcript should be written");
        // A plausible Cursor location that must never be opened.
        let cursor = self
            .home()
            .join(".cursor/projects/work-repo/agent-transcripts");
        fs::create_dir_all(&cursor).expect("cursor directory should be created");
        fs::write(
            cursor.join("chat.jsonl"),
            claude_line(
                "assistant",
                repo,
                "cursor-1",
                "2026-03-06T00:00:00.000Z",
                json!("cursor retry transcript"),
            )
            .to_string(),
        )
        .expect("cursor transcript should be written");
    }

    fn write_claude(&self, relative: &str, lines: &[Value]) {
        write_jsonl(&self.home().join(".claude/projects").join(relative), lines);
    }

    fn write_codex(&self, relative: &str, lines: &[Value]) {
        write_jsonl(&self.home().join(".codex/sessions").join(relative), lines);
    }

    fn recall(&self, query: &str, sources: &[RecallSource], scope: RecallScope) -> RecallReport {
        self.recall_with(query, sources, scope, RecallLimits::default())
            .expect("recall should succeed")
    }

    fn recall_with(
        &self,
        query: &str,
        sources: &[RecallSource],
        scope: RecallScope,
        limits: RecallLimits,
    ) -> Result<RecallReport, RecallError> {
        recall(&RecallRequest {
            query,
            sources,
            scope,
            home_dir: self.home(),
            limits,
        })
    }

    fn repo_scope(&self) -> RecallScope {
        RecallScope::Repository(self.repo.clone())
    }

    /// Path, size, modification time, and content of every file under home.
    fn snapshot(&self) -> BTreeMap<PathBuf, (u64, SystemTime, Vec<u8>)> {
        fn visit(directory: &Path, files: &mut BTreeMap<PathBuf, (u64, SystemTime, Vec<u8>)>) {
            for entry in fs::read_dir(directory).expect("directory should be readable") {
                let path = entry.expect("entry should be readable").path();
                if path.is_dir() {
                    visit(&path, files);
                } else {
                    let metadata = fs::metadata(&path).expect("metadata should load");
                    files.insert(
                        path.clone(),
                        (
                            metadata.len(),
                            metadata.modified().expect("mtime should load"),
                            fs::read(&path).expect("file should be readable"),
                        ),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(self.home(), &mut files);
        files
    }
}

fn claude_line(kind: &str, cwd: &str, session: &str, timestamp: &str, content: Value) -> Value {
    json!({
        "type": kind,
        "uuid": format!("{session}-{timestamp}"),
        "cwd": cwd,
        "sessionId": session,
        "timestamp": timestamp,
        "message": { "role": kind, "content": content }
    })
}

fn codex_message(timestamp: &str, role: &str, part: &str, text: &str) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "response_item",
        "payload": { "type": "message", "role": role, "content": [{ "type": part, "text": text }] }
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

const BOTH: [RecallSource; 2] = [RecallSource::ClaudeCode, RecallSource::Codex];

#[test]
fn test_recall_finds_decisions_across_claude_code_and_codex() {
    let home = ScratchHome::new();

    let report = home.recall("retry budget decision", &BOTH, home.repo_scope());

    let sources = report
        .hits
        .iter()
        .map(|hit| hit.source)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(sources.contains(&RecallSource::ClaudeCode));
    assert!(sources.contains(&RecallSource::Codex));
    let best = &report.hits[0];
    assert_eq!(best.source, RecallSource::Codex);
    assert_eq!(best.session_id.as_deref(), Some("thread-codex-1"));
    assert_eq!(
        best.transcript,
        "~/.codex/sessions/2026/03/02/rollout-2026-03-02T09-00-00-thread-codex-1.jsonl"
    );
    assert!(best.snippet.contains("retry budget"));
    let decision = report
        .hits
        .iter()
        .find(|hit| hit.snippet.contains("cap the upload retry loop"))
        .expect("the Claude Code decision should be recalled");
    assert_eq!(decision.role, "assistant");
    assert_eq!(decision.line, 3);
    assert_eq!(decision.session_id.as_deref(), Some("session-claude-1"));
    assert_eq!(
        decision.timestamp.as_deref(),
        Some("2026-03-01T10:00:09.000Z")
    );
    assert!(decision.snippet.contains("cargo test -p uploader retry"));
    assert!(!report.truncated);
}

#[test]
fn test_recall_is_repository_scoped_by_default_and_widens_only_explicitly() {
    let home = ScratchHome::new();
    let repo = home.repo.to_str().expect("repository path should be UTF-8");

    let scoped = home.recall("retry", &BOTH, home.repo_scope());
    let widened = home.recall("retry", &BOTH, RecallScope::AllRepositories);

    assert!(!scoped.hits.is_empty());
    for hit in &scoped.hits {
        assert_eq!(hit.cwd.as_deref(), Some(repo), "{hit:?}");
        assert!(!hit.snippet.contains("Unrelated repository"));
        assert!(!hit.snippet.contains("Other repository"));
    }
    assert!(
        widened
            .hits
            .iter()
            .any(|hit| hit.snippet.contains("Unrelated repository"))
    );
    assert!(
        widened
            .hits
            .iter()
            .any(|hit| hit.snippet.contains("Other repository"))
    );
}

#[test]
fn test_recall_requires_an_explicitly_enabled_source() {
    let home = ScratchHome::new();

    let disabled = home.recall_with("retry", &[], home.repo_scope(), RecallLimits::default());
    let only_codex = home.recall("retry", &[RecallSource::Codex], home.repo_scope());

    assert_eq!(disabled, Err(RecallError::NoSourcesEnabled));
    assert!(!only_codex.hits.is_empty());
    assert!(
        only_codex
            .hits
            .iter()
            .all(|hit| hit.source == RecallSource::Codex)
    );
    assert_eq!(only_codex.sources.len(), 1);
}

#[test]
fn test_recall_redacts_secrets_and_never_matches_a_redacted_value() {
    let home = ScratchHome::new();

    let report = home.recall("deploy retry worker", &BOTH, home.repo_scope());
    let probe = home.recall(SECRET_CANARY, &BOTH, RecallScope::AllRepositories);
    let password_probe = home.recall(PASSWORD_CANARY, &BOTH, RecallScope::AllRepositories);

    let hit = report
        .hits
        .iter()
        .find(|hit| hit.snippet.contains("deploy with"))
        .expect("the deploy message should be recalled");
    assert!(hit.snippet.contains("[REDACTED:token]"));
    assert!(hit.snippet.contains("[REDACTED:secret]"));
    assert_eq!(hit.redactions, 2);
    assert!(report.redactions >= 2);
    let rendered = format!("{report:?}");
    assert!(!rendered.contains(SECRET_CANARY));
    assert!(!rendered.contains(PASSWORD_CANARY));
    assert!(probe.hits.is_empty());
    assert!(password_probe.hits.is_empty());
}

#[test]
fn test_recall_reports_unverified_formats_instead_of_guessing() {
    let home = ScratchHome::new();

    let report = home.recall(
        "retry",
        &[RecallSource::Cursor, RecallSource::Codex],
        RecallScope::AllRepositories,
    );

    let cursor = report
        .sources
        .iter()
        .find(|source| source.source == RecallSource::Cursor)
        .expect("cursor should be reported");
    assert_eq!(cursor.status, SourceStatus::Unsupported);
    assert_eq!(cursor.files_scanned, 0);
    assert!(
        cursor
            .detail
            .is_some_and(|detail| detail.contains("not read"))
    );
    let codex = report
        .sources
        .iter()
        .find(|source| source.source == RecallSource::Codex)
        .expect("codex should be reported");
    assert_eq!(codex.status, SourceStatus::Searched);
    assert_eq!(codex.files_scanned, 2);
    assert_eq!(codex.files_unrecognized, 1);
    for hit in &report.hits {
        assert_ne!(hit.source, RecallSource::Cursor);
        assert!(!hit.snippet.contains("legacy retry guess"));
        assert!(!hit.snippet.contains("cursor retry transcript"));
    }
}

#[test]
fn test_recall_reports_a_missing_source_without_failing() {
    let temp = tempfile::tempdir().expect("empty home should be created");

    let report = recall(&RecallRequest {
        query: "retry",
        sources: &BOTH,
        scope: RecallScope::AllRepositories,
        home_dir: temp.path(),
        limits: RecallLimits::default(),
    })
    .expect("recall should succeed without transcripts");

    assert!(report.hits.is_empty());
    assert!(
        report
            .sources
            .iter()
            .all(|source| source.status == SourceStatus::Missing)
    );
    assert_eq!(
        fs::read_dir(temp.path()).expect("home should list").count(),
        0
    );
}

#[test]
fn test_recall_is_read_only_and_keeps_no_index() {
    let home = ScratchHome::new();
    let before = home.snapshot();

    for _ in 0..3 {
        home.recall(
            "retry budget",
            &RecallSource::ALL,
            RecallScope::AllRepositories,
        );
    }

    assert_eq!(home.snapshot(), before);
}

#[test]
fn test_recall_bounds_results_snippets_files_and_time() {
    let home = ScratchHome::new();
    let repo = home.repo.to_str().expect("repository path should be UTF-8");
    let filler = "retry ".repeat(400);
    let lines = (0..120)
        .map(|index| {
            claude_line(
                "assistant",
                repo,
                "session-bulk",
                &format!("2026-04-01T00:{:02}:{:02}.000Z", index / 60, index % 60),
                json!(format!("bulk {index} {filler}")),
            )
        })
        .collect::<Vec<_>>();
    home.write_claude("-work-repo/33333333-cccc.jsonl", &lines);

    let limited = home
        .recall_with(
            "retry",
            &BOTH,
            home.repo_scope(),
            RecallLimits {
                max_results: 3,
                ..RecallLimits::default()
            },
        )
        .expect("bounded recall should succeed");
    let one_file = home
        .recall_with(
            "retry",
            &[RecallSource::ClaudeCode],
            RecallScope::AllRepositories,
            RecallLimits {
                max_files: 1,
                ..RecallLimits::default()
            },
        )
        .expect("file-bounded recall should succeed");
    let no_time = home
        .recall_with(
            "retry",
            &BOTH,
            home.repo_scope(),
            RecallLimits {
                deadline: Duration::ZERO,
                ..RecallLimits::default()
            },
        )
        .expect("time-bounded recall should succeed");

    assert_eq!(limited.hits.len(), 3);
    for hit in &limited.hits {
        assert!(hit.snippet.chars().count() <= 322, "{}", hit.snippet.len());
    }
    // Newest first: the three latest bulk lines win the tie on matched terms.
    assert_eq!(
        limited.hits.iter().map(|hit| hit.line).collect::<Vec<_>>(),
        vec![120, 119, 118]
    );
    assert!(one_file.truncated);
    assert_eq!(one_file.sources[0].files_scanned, 1);
    assert!(no_time.truncated);
    assert!(no_time.hits.is_empty());
    for (limit, expected) in [
        (0, RecallError::InvalidLimit),
        (51, RecallError::InvalidLimit),
    ] {
        assert_eq!(
            home.recall_with(
                "retry",
                &BOTH,
                home.repo_scope(),
                RecallLimits {
                    max_results: limit,
                    ..RecallLimits::default()
                },
            ),
            Err(expected)
        );
    }
    assert_eq!(
        home.recall_with("   ", &BOTH, home.repo_scope(), RecallLimits::default()),
        Err(RecallError::EmptyQuery)
    );
}

#[test]
fn test_recall_results_are_deterministic() {
    let home = ScratchHome::new();

    let first = home.recall("retry", &BOTH, RecallScope::AllRepositories);
    let second = home.recall("retry", &BOTH, RecallScope::AllRepositories);

    assert_eq!(first, second);
}
