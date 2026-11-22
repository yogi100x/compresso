//! `compresso_report`: local reports behind the in-session commands
//! (E17-48, D113).
//!
//! Five kinds, all built from facts this process already holds:
//!
//! - `status`: version and build, the server's options, the bound
//!   repository's index state, this session's calls, and recent failed calls;
//! - `update`: the running version and the documented manual update steps.
//!   Compresso never updates itself and never checks for a newer version;
//! - `share`: a short savings summary the user can copy;
//! - `bug` and `feedback`: a report with version, platform, server options,
//!   the last operation ids and the codes of recent failed calls, and the
//!   user's own description with secrets redacted and absolute paths removed.
//!   It is saved to a new file under `reports/` beside the Compresso database
//!   (never in the repository) and returned in full.
//!
//! Nothing is sent anywhere: the tool has no network or process code, and a
//! report leaves the machine only if the user copies it out. Like every tool
//! result it reaches the coding client, which may send it to its model
//! provider. No report holds repository content, file contents, the
//! repository's path, or any absolute path.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use compresso_atlas::repository_stale;
use compresso_store::{StatsFilter, StatsSummary, Store};
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::common::{ToolFailure, checked_arguments, optional_string, required_string};
use super::engine_tools::{format_timestamp, unix_timestamp};
use crate::profile::McpToolProfile;
use crate::session::SessionRuntime;

pub(crate) const TOOL_NAME: &str = "compresso_report";
/// Longest accepted `note`, in characters.
pub(crate) const MAX_NOTE_CHARS: usize = 2_000;
/// Operation ids listed in a bug or feedback report.
const RECENT_OPERATIONS: usize = 10;
/// Directory beside the database that holds saved reports.
const REPORTS_DIRECTORY: &str = "reports";
const PATH_MARKER: &str = "[path]";
/// Saved reports kept at most; past it a report is returned but not saved,
/// so a model calling the tool in a loop cannot fill the disk.
const MAX_SAVED_REPORTS: usize = 100;

const UPDATE_STEPS: &str = "Compresso does not update itself and does not check for newer versions: it makes no network request. To update by hand:
1. Get the newer release tarball for this platform with its SHA256SUMS and SHA256SUMS.sigstore.json from the Compresso release you were given.
2. Run `installers/install-local.sh <tarball or https URL>`. It checks the checksum and the release signature before it writes anything, and keeps the current binary as ~/.local/bin/compresso.previous.
3. Run `compresso --version` and `compresso doctor`.
4. Restart the coding client (or reconnect its MCP servers) so it starts the new binary, then run `compresso integrations status claude-code --project DIR`.
If you installed Compresso another way, update it the same way you installed it.";

const HOW_TO_SEND: &str = "Compresso sent this report nowhere. Read it first and remove anything you do not want to share; then, if you choose, paste it where your Compresso release asks for problem reports or feedback.";

/// What the report tool may read.
pub(crate) struct ReportContext<'a> {
    pub(crate) store: &'a Store,
    pub(crate) session: &'a SessionRuntime,
    pub(crate) repository_root: Option<&'a Path>,
    pub(crate) recall_enabled: bool,
    pub(crate) edits_enabled: bool,
    pub(crate) profile: McpToolProfile,
}

pub(crate) fn handles(name: &str) -> bool {
    name == TOOL_NAME
}

/// Builds the report. Unlike the other tools, a report call is not written
/// to the operation ledger: it saves nothing, and a binary released before
/// this tool would refuse to read a ledger row naming it.
pub(crate) fn run(
    context: &ReportContext<'_>,
    arguments: Option<JsonObject>,
) -> Result<Value, ToolFailure> {
    let mut arguments = checked_arguments(arguments, &["kind", "note"])?;
    let kind = required_string(&mut arguments, "kind")?;
    let note = optional_string(&mut arguments, "note", true)?;
    let reported = matches!(kind.as_str(), "bug" | "feedback");
    if note.is_some() && !reported {
        return Err(ToolFailure::invalid(
            "argument 'note' applies only to kind 'bug' or 'feedback'.",
        ));
    }
    if note
        .as_deref()
        .is_some_and(|note| note.chars().count() > MAX_NOTE_CHARS)
    {
        return Err(ToolFailure::invalid(format!(
            "argument 'note' must be at most {MAX_NOTE_CHARS} characters."
        )));
    }
    let now = unix_timestamp()?;
    let mut payload = json!({ "schema_version": 1, "kind": kind, "sent": false });
    let text = match kind.as_str() {
        "status" => status_text(&facts(context)?),
        "update" => format!("{}\n\n{UPDATE_STEPS}", build_line()),
        "share" => share_text(&all_history(context.store)?),
        "bug" | "feedback" => {
            let text = report_text(
                &kind,
                &format_timestamp(now)?,
                &facts(context)?,
                note.as_deref()
                    .map(|note| clean_note(note, context.repository_root)),
            );
            let saved = save_report(context.store, &kind, now, &text);
            payload["saved_to"] = saved
                .as_ref()
                .map_or(Value::Null, |path| Value::from(path.display().to_string()));
            if saved.is_none() {
                payload["save_note"] = Value::from(
                    "the report was not saved to a file (the reports directory may already hold 100 reports; delete old ones); copy it from this result",
                );
            }
            payload["how_to_send"] = Value::from(HOW_TO_SEND);
            text
        }
        _ => {
            return Err(ToolFailure::invalid(
                "argument 'kind' must be one of status, update, share, bug, feedback.",
            ));
        }
    };
    payload["text"] = Value::from(text);
    Ok(payload)
}

/// `Compresso 0.1.0 (macos-aarch64, release build)`.
fn build_line() -> String {
    format!(
        "Compresso {} ({}-{}, {} build)",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    )
}

/// Index state of the bound repository, without its path.
enum IndexState {
    Unbound,
    NotIndexed,
    Indexed { files: usize, stale: Option<bool> },
}

/// Content-free facts shared by `status`, `bug`, and `feedback`.
struct Facts {
    options: String,
    index: IndexState,
    session_operations: Option<u64>,
    session_savings: Option<i64>,
    /// Operation id, tool, and time, newest first.
    operations: Vec<(String, String, String)>,
    /// Tool and error code, newest first.
    failures: Vec<(String, &'static str)>,
}

fn facts(context: &ReportContext<'_>) -> Result<Facts, ToolFailure> {
    let on_off = |value: bool| if value { "on" } else { "off" };
    let options = format!(
        "{} profile; recall {}; edits {}",
        context.profile.name(),
        on_off(context.recall_enabled),
        on_off(context.edits_enabled)
    );
    let index = match context.repository_root {
        None => IndexState::Unbound,
        Some(root) => match context.store.repo_by_root(root).ok().flatten() {
            Some(repository) if repository.last_indexed_at.is_some() => IndexState::Indexed {
                files: context
                    .store
                    .indexed_files(repository.id)
                    .map(|files| files.len())
                    .unwrap_or_default(),
                stale: repository_stale(context.store, repository.id).ok(),
            },
            _ => IndexState::NotIndexed,
        },
    };
    let session = context.session.current();
    let stats = session.as_ref().and_then(|session_id| {
        context
            .store
            .detailed_stats(&StatsFilter {
                since: None,
                session_id: Some(session_id.clone()),
                operation_limit: Some(RECENT_OPERATIONS),
            })
            .ok()
    });
    let mut operations = Vec::new();
    if let Some(stats) = &stats {
        for row in &stats.by_operation {
            operations.push((
                row.operation_id.clone(),
                row.tool.clone(),
                format_timestamp(row.created_at)?,
            ));
        }
    }
    Ok(Facts {
        options,
        index,
        session_operations: stats.as_ref().map(|stats| stats.summary.operations),
        session_savings: stats
            .as_ref()
            .map(|stats| stats.summary.total_effective_savings),
        operations,
        failures: context.session.recent_failures(),
    })
}

fn index_line(index: &IndexState) -> String {
    match index {
        IndexState::Unbound => "none bound to this server".to_owned(),
        IndexState::NotIndexed => "bound, not indexed (run `compresso index`)".to_owned(),
        IndexState::Indexed { files, stale } => format!(
            "bound, indexed files: {files}, index {}",
            match stale {
                Some(false) => "current",
                Some(true) => "stale (run `compresso index`)",
                None => "state unknown",
            }
        ),
    }
}

fn failures_line(failures: &[(String, &'static str)]) -> String {
    if failures.is_empty() {
        return "none".to_owned();
    }
    failures
        .iter()
        .map(|(tool, code)| format!("{tool}: {code}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn status_text(facts: &Facts) -> String {
    let mut text = build_line();
    let _ = write!(
        text,
        "\nserver: {}\nrepository: {}\nthis session: {} Compresso calls, {} tokens saved\nfailed calls since this server started (newest first): {}",
        facts.options,
        index_line(&facts.index),
        facts
            .session_operations
            .map_or_else(|| "unknown".to_owned(), grouped),
        facts
            .session_savings
            .map_or_else(|| "unknown".to_owned(), grouped_signed),
        failures_line(&facts.failures),
    );
    text
}

fn report_text(kind: &str, created: &str, facts: &Facts, note: Option<String>) -> String {
    let title = if kind == "bug" {
        "Compresso bug report"
    } else {
        "Compresso feedback"
    };
    let mut text = format!(
        "{title}\ncreated: {created}\nversion: {}\nserver: {}\nrepository: {}\n",
        build_line().strip_prefix("Compresso ").unwrap_or_default(),
        facts.options,
        index_line(&facts.index),
    );
    text.push_str("recent operations this session (newest first):\n");
    if facts.operations.is_empty() {
        text.push_str("- none\n");
    }
    for (id, tool, time) in &facts.operations {
        let _ = writeln!(text, "- {id} {tool} {time}");
    }
    text.push_str("failed calls since this server started (newest first):\n");
    if facts.failures.is_empty() {
        text.push_str("- none\n");
    }
    for (tool, code) in &facts.failures {
        let _ = writeln!(text, "- {tool}: {code}");
    }
    text.push_str("description (secrets redacted, absolute paths removed):\n");
    match note.filter(|note| !note.trim().is_empty()) {
        Some(note) => {
            text.push_str(note.trim_end());
            text.push('\n');
        }
        None => text.push_str("(none given)\n"),
    }
    text
}

/// The user's description with secrets redacted by the recall redactor and
/// absolute paths replaced by [`PATH_MARKER`]: first every occurrence of the
/// bound repository's root (as given and canonical, so a root holding a
/// space is caught whole), then every whitespace-separated word that is an
/// absolute path. Repository-relative paths are kept. Limits: another
/// absolute path that holds a space keeps the part after the space, and a
/// one-segment path such as `/tmp` is kept.
fn clean_note(note: &str, repository_root: Option<&Path>) -> String {
    let mut redacted = compresso_recall::redact(note).text;
    if let Some(root) = repository_root {
        let canonical = fs::canonicalize(root).ok();
        let mut roots = [Some(root), canonical.as_deref()]
            .into_iter()
            .flatten()
            .filter_map(Path::to_str)
            .filter(|root| root.len() > 1)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        // Longest first, so a canonical form that extends the given one is
        // replaced whole.
        roots.sort_by_key(|root| std::cmp::Reverse(root.len()));
        for root in roots {
            redacted = redacted.replace(&root, PATH_MARKER);
        }
    }
    let mut cleaned = String::with_capacity(redacted.len());
    for piece in redacted.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        let space = &piece[word.len()..];
        let bare = word.trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ';'
            )
        });
        if is_absolute_path(bare) {
            cleaned.push_str(&word.replacen(bare, PATH_MARKER, 1));
        } else {
            cleaned.push_str(word);
        }
        cleaned.push_str(space);
    }
    cleaned
}

/// Whether `word` is (or ends in, after `=` or `:`) an absolute path of at
/// least two segments on any platform, a drive or network path, a home path,
/// or a file URL. A single segment such as `/compresso` is kept, so a
/// command a user mentions survives.
fn is_absolute_path(word: &str) -> bool {
    let candidate = word
        .rsplit_once(['=', ':'])
        .filter(|(head, tail)| {
            !head.is_empty() && (tail.starts_with('/') || tail.starts_with('\\'))
        })
        .map_or(word, |(_, tail)| tail);
    let bytes = word.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    drive
        || word.starts_with("file:")
        || word.starts_with("~/")
        || word.starts_with("~\\")
        || word.starts_with("\\\\")
        || (candidate.starts_with('/') && candidate[1..].contains('/'))
        || (candidate.starts_with('\\') && candidate[1..].contains('\\'))
}

fn all_history(store: &Store) -> Result<StatsSummary, ToolFailure> {
    store
        .detailed_stats(&StatsFilter {
            since: None,
            session_id: None,
            operation_limit: Some(1),
        })
        .map(|stats| stats.summary)
        .map_err(|_| ToolFailure::internal())
}

fn share_text(summary: &StatsSummary) -> String {
    if summary.operations == 0 {
        return "Compresso has no recorded savings on this machine yet.".to_owned();
    }
    format!(
        "Compresso saved {} tokens over {} tool calls on this machine (retained history): {} by compression and {} retrieval tokens avoided (modeled). Measured locally by Compresso.",
        grouped_signed(summary.total_effective_savings),
        grouped(summary.operations),
        grouped_signed(summary.net_tokens_saved),
        grouped(summary.retrieval_tokens_avoided),
    )
}

/// Saves `text` to a new file under `reports/` beside the database, private
/// to the user where the platform allows. `None` when the store has no file
/// (an in-memory store) or the write fails.
fn save_report(store: &Store, kind: &str, now: i64, text: &str) -> Option<PathBuf> {
    let directory = store.database_path()?.parent()?.join(REPORTS_DIRECTORY);
    fs::create_dir_all(&directory).ok()?;
    if fs::read_dir(&directory)
        .ok()?
        .take(MAX_SAVED_REPORTS)
        .count()
        >= MAX_SAVED_REPORTS
    {
        return None;
    }
    let identity = compresso_core::operation_identity();
    let path = directory.join(format!("{kind}-{now}-{}.txt", identity.id));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).ok()?;
    file.write_all(text.as_bytes()).ok()?;
    Some(path)
}

/// `1234567` as `1,234,567`.
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn grouped_signed(value: i64) -> String {
    if value < 0 {
        format!("-{}", grouped(value.unsigned_abs()))
    } else {
        grouped(value.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_facts() -> Facts {
        Facts {
            options: "full profile; recall on; edits off".to_owned(),
            index: IndexState::Indexed {
                files: 412,
                stale: Some(false),
            },
            session_operations: Some(3),
            session_savings: Some(1_500),
            operations: vec![(
                "01JAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
                "compresso_grep".to_owned(),
                "2026-11-22T10:00:00Z".to_owned(),
            )],
            failures: vec![("compresso_read".to_owned(), "outside_root")],
        }
    }

    #[test]
    fn test_clean_note_removes_absolute_paths_and_secrets_but_keeps_relative_paths() {
        let note = "crash reading /Users/someone/work/app/src/main.rs and C:\\Users\\someone\\app\\x.rs, \
                    see ~/notes.txt or file:///tmp/a; src/lib.rs is fine; key ghp_0123456789abcdefghijABCDEFGHIJ012345 \
                    path=/home/someone/repo";
        let cleaned = clean_note(note, None);
        assert!(!cleaned.contains("/Users/"), "{cleaned}");
        assert!(!cleaned.contains("C:\\Users"), "{cleaned}");
        assert!(!cleaned.contains("~/notes"), "{cleaned}");
        assert!(!cleaned.contains("file:///"), "{cleaned}");
        assert!(!cleaned.contains("/home/someone"), "{cleaned}");
        assert!(!cleaned.contains("ghp_0123456789"), "{cleaned}");
        assert!(cleaned.contains("src/lib.rs"), "{cleaned}");
        assert!(cleaned.contains(PATH_MARKER), "{cleaned}");
    }

    #[test]
    fn test_clean_note_removes_a_repository_root_that_holds_a_space() {
        let root = Path::new("/Users/Jane Doe/work/app");
        let cleaned = clean_note(
            "failed reading /Users/Jane Doe/work/app/src/x.rs twice",
            Some(root),
        );
        assert_eq!(cleaned, "failed reading [path]/src/x.rs twice");
        assert!(!cleaned.contains("Doe"), "{cleaned}");
    }

    #[test]
    fn test_bug_report_lists_ids_and_codes_and_holds_no_absolute_path() {
        let text = report_text(
            "bug",
            "2026-11-22T10:00:01Z",
            &sample_facts(),
            Some(clean_note(
                "grep failed in /private/var/repo after edit",
                None,
            )),
        );
        assert!(text.starts_with("Compresso bug report\n"));
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        assert!(text.contains(std::env::consts::OS));
        assert!(text.contains("- 01JAAAAAAAAAAAAAAAAAAAAAAA compresso_grep 2026-11-22T10:00:00Z"));
        assert!(text.contains("- compresso_read: outside_root"));
        assert!(text.contains("indexed files: 412"));
        assert!(!text.contains("/private/var"), "{text}");
        for line in text.lines() {
            assert!(!line.contains(" /"), "absolute path in {line:?}");
        }
    }

    #[test]
    fn test_feedback_report_without_note_says_none_given() {
        let text = report_text("feedback", "2026-11-22T10:00:01Z", &sample_facts(), None);
        assert!(text.starts_with("Compresso feedback\n"));
        assert!(text.ends_with("(none given)\n"));
    }

    #[test]
    fn test_status_text_names_build_options_index_and_failures() {
        let text = status_text(&sample_facts());
        assert!(text.starts_with(&build_line()));
        assert!(text.contains("server: full profile; recall on; edits off"));
        assert!(text.contains("index current"));
        assert!(text.contains("3 Compresso calls, 1,500 tokens saved"));
        assert!(text.contains("compresso_read: outside_root"));
        let unbound = Facts {
            index: IndexState::Unbound,
            failures: Vec::new(),
            ..sample_facts()
        };
        let text = status_text(&unbound);
        assert!(text.contains("repository: none bound"));
        assert!(text.ends_with("(newest first): none"));
    }

    #[test]
    fn test_grouped_numbers() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_000), "1,000");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(grouped_signed(-12_345), "-12,345");
    }

    #[test]
    fn test_update_steps_download_nothing_and_name_the_installer() {
        assert!(UPDATE_STEPS.contains("does not update itself"));
        assert!(UPDATE_STEPS.contains("installers/install-local.sh"));
        assert!(UPDATE_STEPS.contains("compresso.previous"));
    }
}
