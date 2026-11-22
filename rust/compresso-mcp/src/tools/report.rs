//! `compresso_report`: local reports behind the in-session commands
//! (E17-48, D113).
//!
//! Five kinds:
//!
//! - `status`: version and build, the server's options, the bound
//!   repository's indexed-file count (one count query; staleness is not
//!   checked here), this session's calls, and recent failed calls;
//! - `update`: the running version and the documented manual update steps.
//!   Compresso never updates itself and never checks for a newer version;
//! - `share`: a short savings summary the user can copy;
//! - `bug` and `feedback`: what Compresso adds itself (version, platform,
//!   server options, index state without its path, the last operation ids,
//!   and the codes of recent failed calls), then the note as given by the
//!   caller, fenced, after control characters, secrets, and paths are removed
//!   on a best-effort basis (see [`clean_note`]). The note is written by the
//!   model from what the user or model typed and may hold anything, so the
//!   user must read the report before posting it. It is saved to a new file
//!   under `reports/` beside the Compresso database (never in the repository)
//!   and returned in full; the result names the file without the home
//!   directory or user name.
//!
//! Nothing is sent anywhere: the tool has no network or process code, and a
//! report leaves the machine only if the user copies it out. Like every tool
//! result it reaches the coding client, which may send it to its model
//! provider.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

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

/// Heads the note block of a bug or feedback report.
const NOTE_HEADER: &str = "note (written by the user or the model and included as given, with paths and secrets removed on a best-effort basis; read the whole report before posting it):\n";

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
            let home = home_directory();
            let store_directory = context
                .store
                .database_path()
                .and_then(|path| path.parent().map(Path::to_path_buf));
            let roots = [
                home.as_deref(),
                context.repository_root,
                store_directory.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            let text = report_text(
                &kind,
                &format_timestamp(now)?,
                &facts(context)?,
                note.as_deref().map(|note| clean_note(note, &roots)),
            );
            // Only saving can fail here: the report text is returned either way.
            let saved = match store_directory {
                Some(directory) => save_report(&directory, home.as_deref(), &kind, now, &text),
                None => Err(ToolFailure::new(
                    "report_store_has_no_file",
                    "The report was not saved: this server's store has no file. Copy it from this result.",
                )),
            };
            match saved {
                Ok(path) => {
                    payload["saved"] = Value::Bool(true);
                    payload["saved_to"] = Value::from(display_location(&path, home.as_deref()));
                }
                Err(failure) => {
                    payload["saved"] = Value::Bool(false);
                    payload["saved_to"] = Value::Null;
                    payload["save_error"] = json!({
                        "code": failure.code(),
                        "message": failure.message(),
                    });
                }
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
    Indexed { files: u64 },
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
                    .indexed_file_count(repository.id)
                    .unwrap_or_default(),
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
        IndexState::Indexed { files } => {
            format!("bound, indexed files: {files} (staleness not checked)")
        }
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
    text.push_str(NOTE_HEADER);
    match note.filter(|note| !note.trim().is_empty()) {
        Some(note) => {
            text.push_str("----- begin note -----\n");
            for line in note.trim_end().lines() {
                text.push_str("| ");
                text.push_str(line);
                text.push('\n');
            }
            text.push_str("----- end of note -----\n");
        }
        None => text.push_str("(no note given)\n"),
    }
    text
}

/// Marks where a known root was found, before the path scan; control
/// characters are stripped from the note first, so it cannot occur in it.
const ROOT_SENTINEL: char = '\u{1}';

/// The note made safer to show, on a best-effort basis (no guarantee):
///
/// 1. control characters other than newline and tab are removed (ANSI
///    escapes included), then secrets are redacted by the recall redactor;
/// 2. each known root (home, repository, store directory; as given and
///    canonical) is marked wherever it occurs, spaces included, when it ends
///    at a path boundary, so a sibling such as `/x/appendix` of `/x/app` is
///    left for step 3;
/// 3. a path starts at a known root, at `/` or `\` not preceded by a name
///    character, at `~/` or `~name/`, at a drive letter followed by `:\` or
///    `:/`, and at `../` or `..\` anywhere. Inside quotes or brackets it is
///    removed through the closing one, otherwise through the next
///    whitespace; then each following whitespace-separated word (on the same
///    line) that holds `/` or `\` is removed too. Each removal becomes
///    [`PATH_MARKER`].
///
/// A separator right after a letter, digit, `_`, `-`, or `.` is inside a
/// relative path such as `src/lib.rs`, which is kept unless it climbs.
fn clean_note(note: &str, roots: &[&Path]) -> String {
    let stripped = note
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect::<String>();
    let mut text = compresso_recall::redact(&stripped).text;
    let mut forms = roots
        .iter()
        .flat_map(|root| [Some(root.to_path_buf()), fs::canonicalize(root).ok()])
        .flatten()
        .filter_map(|root| {
            root.to_str()
                .map(|root| root.trim_end_matches(['/', '\\']).to_owned())
        })
        .filter(|root| root.chars().count() > 1)
        .collect::<Vec<_>>();
    forms.sort_by_key(|root| std::cmp::Reverse(root.len()));
    forms.dedup();
    for root in forms {
        text = mark_root(&text, &root);
    }
    scrub_paths(&text)
}

fn is_name_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '.')
}

/// Replaces each occurrence of `root` that ends at a path boundary (not
/// followed by a letter, digit, `_`, or `-`) with [`ROOT_SENTINEL`].
fn mark_root(text: &str, root: &str) -> String {
    let mut marked = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(root) {
        let after = &rest[at + root.len()..];
        let boundary = after
            .chars()
            .next()
            .is_none_or(|next| !(next.is_alphanumeric() || matches!(next, '_' | '-')));
        marked.push_str(&rest[..at]);
        if boundary {
            marked.push(ROOT_SENTINEL);
        } else {
            marked.push_str(root);
        }
        rest = after;
    }
    marked.push_str(rest);
    marked
}

fn is_separator(character: char) -> bool {
    matches!(character, '/' | '\\')
}

/// Whether a path starts at `chars[index]` (see [`clean_note`]).
fn path_starts(chars: &[char], index: usize) -> bool {
    let at = |offset: usize| chars.get(index + offset).copied();
    let previous = index.checked_sub(1).map(|before| chars[before]);
    match chars[index] {
        ROOT_SENTINEL => true,
        '/' | '\\' => {
            !previous.is_some_and(is_name_character) || begins_with_top_directory(chars, index)
        }
        _ if previous.is_none_or(char::is_whitespace)
            && begins_with_user_directory(chars, index) =>
        {
            true
        }
        '~' => {
            let mut next = index + 1;
            while chars.get(next).is_some_and(|&c| is_name_character(c)) {
                next += 1;
            }
            chars.get(next).copied().is_some_and(is_separator)
        }
        '.' => at(1) == Some('.') && at(2).is_some_and(is_separator),
        letter if letter.is_ascii_alphabetic() => {
            at(1) == Some(':') && at(2).is_some_and(is_separator)
        }
        _ => false,
    }
}

/// Well-known top-level directories: a separator followed by one of these
/// starts a path even right after a name character (`-I/Users/...`).
const TOP_DIRECTORIES: [&str; 12] = [
    "Users", "home", "root", "var", "tmp", "private", "opt", "etc", "usr", "Volumes", "mnt", "srv",
];

/// Whether `chars[index..]` is `<separator><top directory><separator>`.
fn begins_with_top_directory(chars: &[char], index: usize) -> bool {
    let separator = chars[index];
    TOP_DIRECTORIES.iter().any(|name| {
        let end = index + 1 + name.chars().count();
        chars
            .get(index + 1..end)
            .is_some_and(|word| word.iter().copied().eq(name.chars()))
            && chars.get(end) == Some(&separator)
    })
}

/// Whether `chars[index..]` is `Users/<name>` or `home/<name>` (either
/// separator), read as absolute at the start of a word.
fn begins_with_user_directory(chars: &[char], index: usize) -> bool {
    ["Users", "home"].iter().any(|name| {
        let end = index + name.chars().count();
        chars
            .get(index..end)
            .is_some_and(|word| word.iter().copied().eq(name.chars()))
            && chars.get(end).copied().is_some_and(is_separator)
            && chars.get(end + 1).copied().is_some_and(is_name_character)
    })
}

/// Whether `segment` is a user directory that ends at the user-name
/// component (`/Users/jane`, `C:\Users\jane`, `home/jane`): the name may go on
/// after a space, so the path cannot be delimited.
fn ends_at_user_name(segment: &[char]) -> bool {
    let text = segment.iter().collect::<String>();
    let text = match text.as_bytes() {
        [drive, b':', ..] if drive.is_ascii_alphabetic() => text[2..].to_owned(),
        _ => text,
    };
    if text.ends_with(['/', '\\']) {
        return false;
    }
    let parts = text
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    parts.len() == 2 && matches!(parts[0], "Users" | "home")
}

/// The closing character for an opening quote or bracket.
fn closing_for(opening: char) -> Option<char> {
    Some(match opening {
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        '«' => '»',
        '\u{201c}' => '\u{201d}',
        '\u{2018}' => '\u{2019}',
        _ => return None,
    })
}

fn scrub_paths(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut cleaned = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let word_start = index == 0 || chars[index - 1].is_whitespace();
        if word_start && !chars[index].is_whitespace() {
            let mut word_end = index;
            while word_end < chars.len() && !chars[word_end].is_whitespace() {
                word_end += 1;
            }
            let word = chars[index..word_end]
                .iter()
                .collect::<String>()
                .to_lowercase();
            if word.contains("%2f") || word.contains("%5c") {
                // A percent-encoded separator: the whole word goes.
                cleaned.push_str(PATH_MARKER);
                index = word_end;
                continue;
            }
        }
        if !path_starts(&chars, index) {
            cleaned.push(chars[index]);
            index += 1;
            continue;
        }
        let closer = index
            .checked_sub(1)
            .and_then(|before| closing_for(chars[before]))
            .and_then(|closer| {
                chars[index + 1..]
                    .iter()
                    .position(|&c| c == closer || c == '\n')
                    .map(|offset| index + 1 + offset)
                    .filter(|&end| chars[end] == closer)
            });
        let mut end = closer.unwrap_or_else(|| {
            let mut end = index;
            while end < chars.len() && !chars[end].is_whitespace() {
                end += 1;
            }
            end
        });
        if let Some(closer) = closer {
            // The rest of the word after the closing quote, when it still
            // holds a separator (`'/Users/o'brien/x'`), goes too.
            let mut word_end = closer + 1;
            while word_end < chars.len() && !chars[word_end].is_whitespace() {
                word_end += 1;
            }
            if chars[closer + 1..word_end]
                .iter()
                .copied()
                .any(is_separator)
            {
                end = word_end;
            }
        } else if chars[index] != ROOT_SENTINEL && ends_at_user_name(&chars[index..end]) {
            // A user name may hold spaces: remove through the end of the line.
            while end < chars.len() && chars[end] != '\n' {
                end += 1;
            }
        } else {
            // Following words on the same line that hold a separator belong
            // to the path (a space inside it).
            loop {
                let mut word = end;
                while word < chars.len() && chars[word].is_whitespace() && chars[word] != '\n' {
                    word += 1;
                }
                let mut word_end = word;
                while word_end < chars.len() && !chars[word_end].is_whitespace() {
                    word_end += 1;
                }
                if word == end || !chars[word..word_end].iter().copied().any(is_separator) {
                    break;
                }
                end = word_end;
            }
        }
        cleaned.push_str(PATH_MARKER);
        index = end;
    }
    cleaned
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

/// The user's home directory from the environment, if set.
fn home_directory() -> Option<PathBuf> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

/// Whether `name` is a saved report's file name:
/// `<bug|feedback>-<digits>-<26 base-32 characters>.txt`.
fn is_report_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("bug-")
        .or_else(|| name.strip_prefix("feedback-"))
    else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(".txt") else {
        return false;
    };
    let Some((seconds, id)) = rest.split_once('-') else {
        return false;
    };
    !seconds.is_empty()
        && seconds.bytes().all(|byte| byte.is_ascii_digit())
        && id.len() == 26
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
}

/// Saves `text` as a new report under `reports/` in `store_directory`.
///
/// The `reports` path must be a real directory (a symbolic link or another
/// file type is refused, never followed); it is created with mode 0700 and
/// files with 0600 on Unix. At most [`MAX_SAVED_REPORTS`] files with a report
/// name are kept: past that the call is refused and nothing is deleted. The
/// text goes to a temporary file in the same directory, which is then renamed,
/// so no partial report remains.
fn save_report(
    store_directory: &Path,
    home: Option<&Path>,
    kind: &str,
    now: i64,
    text: &str,
) -> Result<PathBuf, ToolFailure> {
    let directory = store_directory.join(REPORTS_DIRECTORY);
    let shown = display_directory(&directory, home);
    let unusable = || {
        ToolFailure::new(
            "report_directory_unusable",
            format!(
                "The report was not saved: {shown} is not a plain directory Compresso can use (a symbolic link or another file type is refused)."
            ),
        )
    };
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(unusable());
        }
        Ok(_) => {
            // A directory an earlier build created with a wider mode (0755)
            // is made private on use: group and other bits are removed
            // (0755 becomes 0700); owner bits are never added.
            #[cfg(unix)]
            if let Ok(metadata) = fs::metadata(&directory) {
                use std::os::unix::fs::PermissionsExt;
                let mode = metadata.permissions().mode();
                if mode & 0o077 != 0 {
                    let _ =
                        fs::set_permissions(&directory, fs::Permissions::from_mode(mode & 0o700));
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&directory).map_err(|_| unusable())?;
        }
        Err(_) => return Err(unusable()),
    }
    let saved = fs::read_dir(&directory)
        .map_err(|_| unusable())?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_str().is_some_and(is_report_name))
        .count();
    if saved >= MAX_SAVED_REPORTS {
        return Err(ToolFailure::new(
            "report_limit_reached",
            format!(
                "The report was not saved: {shown} already holds {MAX_SAVED_REPORTS} reports, the most Compresso keeps. Delete old reports there, then run the command again; Compresso deletes none by itself."
            ),
        ));
    }
    let identity = compresso_core::operation_identity();
    let name = format!("{kind}-{now}-{}.txt", identity.id);
    let path = directory.join(&name);
    let temporary = directory.join(format!(".{name}.partial"));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            ToolFailure::new(
                "report_directory_unwritable",
                format!("The report was not saved: Compresso may not write in {shown}."),
            )
        } else {
            ToolFailure::new(
                "report_write_failed",
                format!("The report was not saved: a file could not be created in {shown}."),
            )
        }
    })?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            fs::rename(&temporary, &path)
        });
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err(ToolFailure::new(
            "report_write_failed",
            format!(
                "The report was not saved: writing it in {shown} failed, and no partial file was kept."
            ),
        ));
    }
    Ok(path)
}

/// A directory for a message: `~/...` under home, else in words.
fn display_directory(directory: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| directory.strip_prefix(home).ok()) {
        Some(relative) => format!(
            "~/{}",
            relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        ),
        None => "the reports folder beside the Compresso database".to_owned(),
    }
}

/// Where a saved report is, for the tool result, without the home
/// directory or user name: `~/...` under home, else the file name and the
/// directory in words.
fn display_location(path: &Path, home: Option<&Path>) -> String {
    if let Some(relative) = home.and_then(|home| path.strip_prefix(home).ok()) {
        let parts = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        return format!("~/{}", parts.join("/"));
    }
    format!(
        "{} in the reports folder beside the Compresso database",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    )
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
            index: IndexState::Indexed { files: 412 },
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
                    see ~/notes.txt or file:///tmp/a; but src/lib.rs is fine; key ghp_0123456789abcdefghijABCDEFGHIJ012345 \
                    path=/home/someone/repo";
        let cleaned = clean_note(note, &[]);
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
            &[root],
        );
        assert_eq!(cleaned, "failed reading [path] twice");
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
                &[],
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
        assert!(text.ends_with("(no note given)\n"));
    }

    #[test]
    fn test_status_text_names_build_options_index_and_failures() {
        let text = status_text(&sample_facts());
        assert!(text.starts_with(&build_line()));
        assert!(text.contains("server: full profile; recall on; edits off"));

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

    /// Every input the review found leaking, and the decided rule's cases.
    #[test]
    fn test_clean_note_removes_paths_in_every_reviewed_form() {
        let leaks = [
            r#"path:"/Users/jane/secret/x""#,
            "path='/Users/jane/secret/x'",
            r#"{"path":"/Users/jane/secret/a.rs"}"#,
            "{/Users/jane/x}",
            "«/Users/jane/x»",
            "cwd=~/secret/x",
            "--file=~/x",
            r"/Users\jane\x",
            "../../Users/jane/.ssh",
            r"see C:\Users\jane\x and D:/jane/y",
            "~jane/notes",
        ];
        for leak in leaks {
            let cleaned = clean_note(&format!("before {leak} after"), &[]);
            assert!(!cleaned.contains("jane"), "{leak:?} -> {cleaned:?}");
            assert!(!cleaned.contains("secret"), "{leak:?} -> {cleaned:?}");
            assert!(cleaned.starts_with("before "), "{leak:?} -> {cleaned:?}");
            assert!(
                cleaned.ends_with(" after") || cleaned.ends_with("after"),
                "{leak:?} -> {cleaned:?}"
            );
            assert!(cleaned.contains(PATH_MARKER), "{leak:?} -> {cleaned:?}");
        }
    }

    #[test]
    fn test_clean_note_consumes_path_words_after_a_space() {
        for note in [
            // Not a user directory: a user directory ending at the name is
            // removed through the line end instead (final review rule C).
            "x /srv/Jane Doe/other/x.rs and more",
            "x /srv/Jane\u{a0}Doe/other/x.rs and more",
            "x \"/Users/Jane Doe/other x.rs\" and more",
        ] {
            let cleaned = clean_note(note, &[]);
            assert!(!cleaned.contains("Doe"), "{note:?} -> {cleaned:?}");
            assert!(!cleaned.contains("other"), "{note:?} -> {cleaned:?}");
            assert!(cleaned.ends_with("and more"), "{note:?} -> {cleaned:?}");
        }
    }

    #[test]
    fn test_known_roots_are_replaced_at_a_path_boundary_only() {
        let home = Path::new("/Users/Jane Doe");
        let root = Path::new("/x/app");
        let cleaned = clean_note(
            "home /Users/Jane Doe and /x/app/src plus /x/appendix/y end",
            &[home, root],
        );
        assert!(!cleaned.contains("Doe"), "{cleaned}");
        assert!(
            !cleaned.contains("endix"),
            "sibling not mangled into a tail: {cleaned}"
        );
        assert!(!cleaned.contains("/x/"), "{cleaned}");
        assert!(
            cleaned.starts_with("home [path] and [path] plus [path] end"),
            "{cleaned}"
        );
    }

    #[test]
    fn test_clean_note_strips_control_characters_but_keeps_lines_and_tabs() {
        let cleaned = clean_note("\u{1b}[31mred\u{1b}[0m\u{7} ok\tz\nnext\r\u{9b}2J", &[]);
        assert!(
            !cleaned
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t'),
            "{cleaned:?}"
        );
        assert!(cleaned.contains("ok\tz\nnext"), "{cleaned:?}");
    }

    #[test]
    fn test_note_is_fenced_so_it_cannot_pass_for_report_lines() {
        let text = report_text(
            "bug",
            "2026-11-22T10:00:01Z",
            &sample_facts(),
            Some("version: 9.9.9\n- compresso_read: forged\n----- end of note -----".to_owned()),
        );
        assert!(!text.lines().any(|line| line == "version: 9.9.9"), "{text}");
        assert!(
            !text.lines().any(|line| line == "- compresso_read: forged"),
            "{text}"
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("----- end of note"))
                .count(),
            1,
            "{text}"
        );
        assert!(text.contains("| version: 9.9.9"), "{text}");
        assert!(text.contains("best-effort"), "{text}");
        assert!(!text.contains("absolute paths removed"), "{text}");
    }

    /// Final review round: well-known top-level directories after a name
    /// character, bare user directories, quoted tails, percent-encoding.
    #[test]
    fn test_clean_note_final_round_forms() {
        for (note, kept) in [
            ("cc -I/Users/jane/inc -L/home/jane/lib ok", "ok"),
            ("a./Users/bob/x then ok", "ok"),
            ("foo/Users/bob/x then ok", "ok"),
            (r"foo\Users\bob\x then ok", "ok"),
            ("see Users/jane/x then ok", "ok"),
            ("see home/jane/x then ok", "ok"),
            ("x /private/var/jane/y ok", "ok"),
            ("'/Users/o'brien/secret/file' ok", "ok"),
            ("open %2FUsers%2Fjane%2Fx ok", "ok"),
            ("open ..%5cUsers%5Cjane ok", "ok"),
        ] {
            let cleaned = clean_note(note, &[]);
            for leak in ["jane", "bob", "brien", "secret", "Users"] {
                assert!(!cleaned.contains(leak), "{note:?} -> {cleaned:?}");
            }
            assert!(cleaned.ends_with(kept), "{note:?} -> {cleaned:?}");
        }
        // Relative paths are still kept.
        assert_eq!(clean_note("see src/lib.rs ok", &[]), "see src/lib.rs ok");
    }

    #[test]
    fn test_a_user_directory_without_a_further_separator_is_removed_to_the_line_end() {
        for note in [
            "a /Users/Jane Doe and it fails\nnext line",
            "a C:\\Users\\Jane Doe then\nnext line",
            "a /Users/Jane Doe\nnext line",
        ] {
            let cleaned = clean_note(note, &[]);
            assert!(!cleaned.contains("Doe"), "{note:?} -> {cleaned:?}");
            assert!(cleaned.ends_with("\nnext line"), "{note:?} -> {cleaned:?}");
            assert!(cleaned.starts_with("a [path]"), "{note:?} -> {cleaned:?}");
        }
    }

    #[test]
    fn test_saved_location_hides_home_and_user_name() {
        let home = Path::new("/Users/jane");
        let inside = Path::new("/Users/jane/.local/share/compresso/reports/bug-1-X.txt");
        assert_eq!(
            display_location(inside, Some(home)),
            "~/.local/share/compresso/reports/bug-1-X.txt"
        );
        let outside = Path::new("/srv/data/compresso/reports/bug-1-X.txt");
        let shown = display_location(outside, Some(home));
        assert!(!shown.contains("/srv"), "{shown}");
        assert!(shown.contains("bug-1-X.txt"), "{shown}");
    }

    #[test]
    fn test_status_says_staleness_is_not_checked() {
        let line = index_line(&IndexState::Indexed { files: 3 });
        assert_eq!(line, "bound, indexed files: 3 (staleness not checked)");
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
