//! `compresso decisions`: summarize a session's ledger decisions as a pull
//! request comment draft.
//!
//! The draft is built only from the content-free operation ledger, is
//! secret-redacted, and is a preview unless the caller passes `--post --yes`.

use std::io::Write;
use std::path::PathBuf;

use clap::Args;
use compresso_core::Trace;
use compresso_recall::redact;
use compresso_store::{OperationRecord, SessionId, SessionRecord, StatsFilter, Store};
use serde_json::{Value, json};

use super::{CliExitCode, OutputArgs, write_error};

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;
/// Newest ledger rows inspected when no session is named.
const LATEST_SESSION_SCAN: usize = 200;
const MAX_INLINE_CHARS: usize = 120;
const COMPRESSION_TOOLS: [&str; 2] = ["compresso_compress", "engine_compress"];

#[derive(Args, Debug)]
pub struct DecisionsArgs {
    /// Session (ULID) to summarize; defaults to the newest recorded session.
    #[arg(long, value_name = "ULID")]
    pub session: Option<String>,
    /// Maximum newest operations summarized (1-200).
    #[arg(long, default_value_t = DEFAULT_LIMIT)]
    pub limit: usize,
    /// Pull request number the comment draft is written for.
    #[arg(long, value_name = "NUMBER")]
    pub pr: Option<u64>,
    /// Post the draft as a pull request comment; requires --pr and --yes.
    #[arg(long)]
    pub post: bool,
    /// Confirm posting. Without it, --post only previews.
    #[arg(long)]
    pub yes: bool,
    /// Never post, even with --post --yes.
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub output: OutputArgs,
}

/// Why a confirmed post did not happen.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PostError {
    #[error("the GitHub CLI `gh` was not found on PATH")]
    GhNotFound,
    #[error(
        "this build cannot start `gh`: the CLI crate's capability class forbids child processes"
    )]
    ExecutionUnavailable,
    #[error("`gh` did not accept the comment")]
    Failed,
}

impl PostError {
    const fn code(self) -> &'static str {
        match self {
            Self::GhNotFound => "gh_not_found",
            Self::ExecutionUnavailable => "post_unavailable",
            Self::Failed => "post_failed",
        }
    }
}

/// Boundary that delivers a confirmed draft to a pull request.
///
/// It is only ever called after `--post --yes`; tests inject a recording stub
/// so no test reaches the network.
pub trait PullRequestPoster {
    fn post_comment(&self, pull_request: u64, body: &str) -> Result<(), PostError>;
}

/// Production poster. It looks for `gh` on `PATH` and never starts a process.
///
/// `compresso-cli` is a `bounded-local` crate (01-stack §3), so it may not
/// execute child processes. Until a process-capable owner exists, a confirmed
/// post reports that honestly and the caller pipes the draft to `gh` itself.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemPullRequestPoster {
    path_entries: Vec<PathBuf>,
}

impl SystemPullRequestPoster {
    /// Builds a poster that searches the given `PATH` entries for `gh`.
    pub fn new(path_entries: Vec<PathBuf>) -> Self {
        Self { path_entries }
    }

    /// Builds a poster from the process `PATH`.
    pub fn from_environment() -> Self {
        Self::new(
            std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
        )
    }

    fn gh_is_present(&self) -> bool {
        let names: &[&str] = if cfg!(windows) {
            &["gh.exe", "gh.cmd"]
        } else {
            &["gh"]
        };
        self.path_entries
            .iter()
            .any(|entry| names.iter().any(|name| entry.join(name).is_file()))
    }
}

impl PullRequestPoster for SystemPullRequestPoster {
    fn post_comment(&self, _pull_request: u64, _body: &str) -> Result<(), PostError> {
        if !self.gh_is_present() {
            return Err(PostError::GhNotFound);
        }
        Err(PostError::ExecutionUnavailable)
    }
}

struct CommandError {
    code: &'static str,
    message: String,
}

impl CommandError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn internal() -> Self {
        Self::new("internal", "failed to read the local operation ledger")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Status {
    Preview,
    ConfirmationRequired,
    Posted,
}

impl Status {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::ConfirmationRequired => "confirmation_required",
            Self::Posted => "posted",
        }
    }

    const fn note(self) -> &'static str {
        match self {
            Self::Preview => "preview only; nothing was posted",
            Self::ConfirmationRequired => {
                "preview only; nothing was posted. Re-run with --post --yes to post this draft"
            }
            Self::Posted => "posted as a pull request comment",
        }
    }
}

struct Draft {
    session: SessionRecord,
    recorded_operations: u64,
    decisions: Vec<Value>,
    redactions: usize,
    markdown: String,
}

/// Runs `compresso decisions` against an injected store and poster.
pub fn execute_decisions(
    arguments: DecisionsArgs,
    store: &Store,
    poster: &dyn PullRequestPoster,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = arguments.output.json;
    let (status, draft) = match run(&arguments, store, poster) {
        Ok(result) => result,
        Err(error) => {
            let _ = write_error(errors, error.code, &error.message, json);
            return CliExitCode::Failure;
        }
    };
    let result = if json {
        serde_json::to_writer(
            &mut *output,
            &json!({
                "schema_version": 1,
                "data": {
                    "status": status.as_str(),
                    "posted": status == Status::Posted,
                    "pull_request": arguments.pr,
                    "session_id": draft.session.id.as_str(),
                    "operations": draft.recorded_operations,
                    "decisions": draft.decisions,
                    "redactions": draft.redactions,
                    "draft": draft.markdown
                }
            }),
        )
        .map_err(std::io::Error::other)
        .and_then(|()| writeln!(output))
    } else {
        write!(output, "status: {}\n\n{}", status.note(), draft.markdown)
    };
    if result.is_ok() {
        CliExitCode::Success
    } else {
        CliExitCode::Failure
    }
}

fn run(
    arguments: &DecisionsArgs,
    store: &Store,
    poster: &dyn PullRequestPoster,
) -> Result<(Status, Draft), CommandError> {
    if !(1..=MAX_LIMIT).contains(&arguments.limit) {
        return Err(CommandError::new(
            "invalid_arguments",
            "limit must be between 1 and 200",
        ));
    }
    if arguments.pr == Some(0) {
        return Err(CommandError::new(
            "invalid_arguments",
            "--pr must be a positive pull request number",
        ));
    }
    if arguments.post && arguments.pr.is_none() {
        return Err(CommandError::new(
            "invalid_arguments",
            "--post requires --pr NUMBER",
        ));
    }
    let session_id = match arguments.session.as_deref() {
        Some(session) => SessionId::parse(session).map_err(|_| {
            CommandError::new(
                "invalid_arguments",
                "--session must be a 26-character session ULID",
            )
        })?,
        None => latest_session(store)?,
    };
    let draft = build_draft(store, &session_id, arguments.limit)?;

    let status = match (arguments.post, arguments.yes, arguments.dry_run) {
        (true, true, false) => {
            let pull_request = arguments.pr.ok_or_else(CommandError::internal)?;
            poster
                .post_comment(pull_request, &draft.markdown)
                .map_err(|error| {
                    CommandError::new(
                        error.code(),
                        format!(
                            "{error}; nothing was posted. To post it yourself: compresso decisions --session {} --json | jq -r .data.draft | gh pr comment {pull_request} --body-file -",
                            draft.session.id
                        ),
                    )
                })?;
            Status::Posted
        }
        (true, false, false) => Status::ConfirmationRequired,
        _ => Status::Preview,
    };
    Ok((status, draft))
}

/// Finds the session that owns the newest session-bound ledger row.
fn latest_session(store: &Store) -> Result<SessionId, CommandError> {
    let stats = store
        .detailed_stats(&StatsFilter {
            since: None,
            session_id: None,
            operation_limit: Some(LATEST_SESSION_SCAN),
        })
        .map_err(|_| CommandError::internal())?;
    for row in &stats.by_operation {
        let operation = store
            .operation_by_id(&row.operation_id)
            .map_err(|_| CommandError::internal())?;
        if let Some(session_id) = operation.and_then(|operation| operation.session_id) {
            return Ok(session_id);
        }
    }
    Err(CommandError::new(
        "no_session",
        "no recorded client session was found in the operation ledger; pass --session ULID",
    ))
}

fn build_draft(store: &Store, session_id: &SessionId, limit: usize) -> Result<Draft, CommandError> {
    let session = store
        .session_by_id(session_id)
        .map_err(|_| CommandError::internal())?
        .ok_or_else(|| CommandError::new("not_found", "no retained session has that id"))?;
    let stats = store
        .detailed_stats(&StatsFilter {
            since: None,
            session_id: Some(session_id.clone()),
            operation_limit: Some(limit),
        })
        .map_err(|_| CommandError::internal())?;
    if stats.summary.operations == 0 {
        return Err(CommandError::new(
            "no_decisions",
            "that session recorded no operations, so there is nothing to draft",
        ));
    }

    let mut redactions = 0;
    let mut decisions = Vec::new();
    let mut compression_lines = Vec::new();
    let mut fallbacks = 0_u64;
    // The ledger lists newest first; a reviewer reads decisions in order.
    for row in stats.by_operation.iter().rev() {
        let Some(operation) = store
            .operation_by_id(&row.operation_id)
            .map_err(|_| CommandError::internal())?
        else {
            continue;
        };
        let Some(trace) = operation
            .trace_json
            .as_deref()
            .and_then(|trace| serde_json::from_str::<Trace>(trace).ok())
        else {
            continue;
        };
        if trace.fallback_used {
            fallbacks += 1;
        }
        let (line, decision) = compression_decision(&operation, &trace, &mut redactions);
        compression_lines.push(line);
        decisions.push(decision);
    }

    let client = inline(&session.client, &mut redactions);
    let transport = inline(&session.transport, &mut redactions);
    let mut markdown = String::from("## Compresso session decisions\n\n");
    markdown.push_str(&format!(
        "- Session: `{}` (client `{client}`, {transport})\n",
        session.id
    ));
    markdown.push_str(&format!(
        "- Window: {} to {}\n",
        format_utc(session.started_at),
        session
            .ended_at
            .map_or_else(|| "still open".to_owned(), format_utc)
    ));
    markdown.push_str(&format!(
        "- Operations recorded: {} (newest {} summarized)\n",
        stats.summary.operations,
        stats.by_operation.len()
    ));

    markdown.push_str("\n### Compression decisions\n\n");
    if compression_lines.is_empty() {
        markdown.push_str("_None recorded._\n");
    }
    for line in &compression_lines {
        markdown.push_str(line);
        markdown.push('\n');
    }

    markdown.push_str("\n### Retrieval decisions\n\n");
    let retrieval = stats
        .by_tool
        .iter()
        .filter(|row| !COMPRESSION_TOOLS.contains(&row.tool.as_str()))
        .collect::<Vec<_>>();
    if retrieval.is_empty() {
        markdown.push_str("_None recorded._\n");
    } else {
        markdown.push_str("| Tool | Calls | Modeled tokens avoided |\n| --- | ---: | ---: |\n");
        for row in &retrieval {
            let tool = inline(&row.tool, &mut redactions);
            markdown.push_str(&format!(
                "| `{tool}` | {} | {} |\n",
                row.operations, row.retrieval_tokens_avoided
            ));
            decisions.push(json!({
                "kind": "retrieval",
                "tool": tool,
                "calls": row.operations,
                "modeled_tokens_avoided": row.retrieval_tokens_avoided
            }));
        }
    }

    markdown.push_str("\n### Totals\n\n");
    markdown.push_str(&format!(
        "- Net compression savings (applied): {} tokens\n",
        stats.summary.net_tokens_saved
    ));
    markdown.push_str(&format!(
        "- Retrieval tokens avoided (modeled): {} tokens\n",
        stats.summary.retrieval_tokens_avoided
    ));
    markdown.push_str(&format!("- Compression fallbacks: {fallbacks}\n"));
    markdown.push_str(
        "\n_Draft generated locally from Compresso's content-free operation ledger. It contains no prompts, source text, or transcripts._\n",
    );

    Ok(Draft {
        session,
        recorded_operations: stats.summary.operations,
        decisions,
        redactions,
        markdown,
    })
}

fn compression_decision(
    operation: &OperationRecord,
    trace: &Trace,
    redactions: &mut usize,
) -> (String, Value) {
    let tool = inline(&operation.tool, redactions);
    let mode = serde_json::to_value(trace.mode)
        .ok()
        .and_then(|mode| mode.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());
    let applied = trace
        .strategy_results
        .iter()
        .filter(|result| result.applied)
        .map(|result| inline(&result.strategy_id, redactions))
        .collect::<Vec<_>>();
    let skipped = trace
        .strategy_results
        .iter()
        .filter(|result| !result.applied)
        .map(|result| {
            (
                inline(&result.strategy_id, redactions),
                result
                    .skipped_reason
                    .as_deref()
                    .map(|reason| inline(reason, redactions)),
            )
        })
        .collect::<Vec<_>>();
    let fallback_reason = trace
        .fallback_reason
        .as_deref()
        .map(|reason| inline(reason, redactions));

    let mut line = format!(
        "- {} `{tool}` in `{mode}` mode: {} -> {} tokens.",
        format_utc(operation.created_at),
        trace.before_tokens,
        trace.after_tokens
    );
    if trace.fallback_used {
        line.push_str(&format!(
            " Fell back to the original content ({}).",
            fallback_reason.as_deref().unwrap_or("no reason recorded")
        ));
    }
    if !applied.is_empty() {
        line.push_str(&format!(" Applied: {}.", code_list(applied.iter())));
    }
    if !skipped.is_empty() {
        let rendered = skipped
            .iter()
            .map(|(strategy, reason)| match reason {
                Some(reason) => format!("`{strategy}` ({reason})"),
                None => format!("`{strategy}`"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        line.push_str(&format!(" Skipped: {rendered}."));
    }
    let decision = json!({
        "kind": "compression",
        "operation_id": operation.id,
        "tool": tool,
        "mode": mode,
        "created_at": operation.created_at,
        "before_tokens": trace.before_tokens,
        "after_tokens": trace.after_tokens,
        "applied": applied,
        "skipped": skipped.iter().map(|(strategy, reason)| json!({
            "strategy": strategy,
            "reason": reason
        })).collect::<Vec<_>>(),
        "fallback_used": trace.fallback_used,
        "fallback_reason": fallback_reason
    });
    (line, decision)
}

fn code_list<'a>(items: impl Iterator<Item = &'a String>) -> String {
    items
        .map(|item| format!("`{item}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Redacts and neutralizes one ledger string for inline Markdown.
///
/// Ledger strings such as the client name are supplied by other programs, so
/// anything that could form markup, a mention, or an issue reference is
/// removed before it reaches a pull request.
fn inline(value: &str, redactions: &mut usize) -> String {
    let redaction = redact(value);
    *redactions += redaction.count;
    let cleaned = redaction
        .text
        .chars()
        .map(|character| {
            if character.is_control() || matches!(character, '`' | '|' | '<' | '>' | '@' | '#') {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return "unknown".to_owned();
    }
    collapsed.chars().take(MAX_INLINE_CHARS).collect()
}

/// Formats Unix seconds as an RFC 3339 UTC timestamp.
fn format_utc(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    // Civil-from-days conversion for the proleptic Gregorian calendar.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3_600,
        second_of_day % 3_600 / 60,
        second_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_utc_matches_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_794_614_399), "2026-11-13T23:59:59Z");
        assert_eq!(format_utc(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn test_inline_neutralizes_markup_mentions_and_secrets() {
        let mut redactions = 0;

        let cleaned = inline(
            "@maintainer `x` | <b>#12</b>\nclient ghp_inlinecanary0123456789abcdefghij",
            &mut redactions,
        );

        assert_eq!(cleaned, "maintainer x b 12 /b client [REDACTED:token]");
        assert_eq!(redactions, 1);
        assert_eq!(inline(" \n ", &mut redactions), "unknown");
        assert_eq!(
            inline(&"x".repeat(500), &mut redactions).chars().count(),
            MAX_INLINE_CHARS
        );
    }

    #[test]
    fn test_system_poster_never_starts_a_process() {
        let temp = tempfile::tempdir().expect("probe directory should be created");
        let temp = temp.path().to_path_buf();
        let missing = SystemPullRequestPoster::new(vec![temp.clone()]);
        assert_eq!(missing.post_comment(1, "body"), Err(PostError::GhNotFound));

        let name = if cfg!(windows) { "gh.exe" } else { "gh" };
        // A file that would fail loudly if it were ever executed.
        std::fs::write(temp.join(name), "#!/bin/sh\nexit 97\n").expect("probe should be written");
        let present = SystemPullRequestPoster::new(vec![temp]);
        assert_eq!(
            present.post_comment(1, "body"),
            Err(PostError::ExecutionUnavailable)
        );
    }
}
