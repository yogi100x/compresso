//! `compresso recall`: index-free search over local session transcripts.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use compresso_recall::{
    RecallError, RecallLimits, RecallReport, RecallRequest, RecallScope, RecallSource, recall,
};
use serde_json::{Value, json};

use super::{CliExitCode, OutputArgs, write_error};

const DEFAULT_LIMIT: usize = 8;

/// Session source accepted by `--source`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum RecallSourceArg {
    #[value(name = "claude-code")]
    ClaudeCode,
    Codex,
    /// Accepted so the refusal is explicit: the format is unverified and never read.
    Cursor,
}

impl From<RecallSourceArg> for RecallSource {
    fn from(source: RecallSourceArg) -> Self {
        match source {
            RecallSourceArg::ClaudeCode => Self::ClaudeCode,
            RecallSourceArg::Codex => Self::Codex,
            RecallSourceArg::Cursor => Self::Cursor,
        }
    }
}

#[derive(Args, Debug)]
pub struct RecallArgs {
    /// Words to look for in past local coding sessions.
    pub query: String,
    /// Session source to read; repeat to enable several. Nothing is read without it.
    #[arg(long = "source", value_enum, value_name = "SOURCE")]
    pub sources: Vec<RecallSourceArg>,
    /// Repository whose sessions are searched; defaults to the repository enclosing the cwd.
    #[arg(long, value_name = "PATH", conflicts_with = "all_repositories")]
    pub root: Option<PathBuf>,
    /// Search sessions from every repository instead of only this one.
    #[arg(long)]
    pub all_repositories: bool,
    /// Maximum number of excerpts returned (1-50).
    #[arg(long, default_value_t = DEFAULT_LIMIT)]
    pub limit: usize,
    #[command(flatten)]
    pub output: OutputArgs,
}

/// Injectable filesystem context for `compresso recall`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecallEnvironment {
    /// Home directory holding the clients' transcript folders.
    pub home_dir: PathBuf,
    /// Working directory used to find the default repository scope.
    pub cwd: PathBuf,
}

impl RecallEnvironment {
    pub(crate) fn system() -> Option<Self> {
        Some(Self {
            home_dir: system_home_dir()?,
            cwd: std::env::current_dir().ok()?,
        })
    }
}

pub(crate) fn system_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let value = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let value = std::env::var_os("HOME");
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// Runs `compresso recall` against an explicit home and working directory.
pub fn execute_recall(
    arguments: RecallArgs,
    environment: &RecallEnvironment,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> CliExitCode {
    let json = arguments.output.json;
    let sources = arguments
        .sources
        .iter()
        .copied()
        .map(RecallSource::from)
        .collect::<Vec<_>>();
    let scope = if arguments.all_repositories {
        RecallScope::AllRepositories
    } else {
        let root = arguments
            .root
            .clone()
            .unwrap_or_else(|| enclosing_repository(&environment.cwd));
        RecallScope::Repository(fs::canonicalize(&root).unwrap_or(root))
    };
    let report = recall(&RecallRequest {
        query: &arguments.query,
        sources: &sources,
        scope: scope.clone(),
        home_dir: &environment.home_dir,
        limits: RecallLimits {
            max_results: arguments.limit,
            ..RecallLimits::default()
        },
    });
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            let (code, message) = match error {
                RecallError::NoSourcesEnabled => (
                    "recall_disabled",
                    "recall reads nothing until a session source is enabled; pass --source claude-code and/or --source codex",
                ),
                RecallError::EmptyQuery => ("invalid_arguments", "query must not be empty"),
                RecallError::InvalidLimit => {
                    ("invalid_arguments", "limit must be between 1 and 50")
                }
            };
            let _ = write_error(errors, code, message, json);
            return CliExitCode::Failure;
        }
    };
    let result = if json {
        serde_json::to_writer(
            &mut *output,
            &json!({ "schema_version": 1, "data": report_json(&scope, &report) }),
        )
        .map_err(std::io::Error::other)
        .and_then(|()| writeln!(output))
    } else {
        output.write_all(report_human(&scope, &report).as_bytes())
    };
    if result.is_ok() {
        CliExitCode::Success
    } else {
        CliExitCode::Failure
    }
}

/// Nearest ancestor of `cwd` that holds a `.git` entry, or `cwd` itself.
fn enclosing_repository(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

fn report_json(scope: &RecallScope, report: &RecallReport) -> Value {
    let scope = match scope {
        RecallScope::Repository(root) => json!({ "kind": "repository", "root": root }),
        RecallScope::AllRepositories => json!({ "kind": "all_repositories" }),
    };
    json!({
        "scope": scope,
        "hits": report.hits.iter().map(|hit| json!({
            "source": hit.source.as_str(),
            "session_id": hit.session_id,
            "transcript": hit.transcript,
            "line": hit.line,
            "role": hit.role,
            "timestamp": hit.timestamp,
            "cwd": hit.cwd,
            "snippet": hit.snippet,
            "matched_terms": hit.matched_terms
        })).collect::<Vec<_>>(),
        "sources": report.sources.iter().map(|source| json!({
            "source": source.source.as_str(),
            "status": source.status.as_str(),
            "files_scanned": source.files_scanned,
            "files_unrecognized": source.files_unrecognized,
            "detail": source.detail
        })).collect::<Vec<_>>(),
        "truncated": report.truncated,
        "redactions": report.redactions
    })
}

fn report_human(scope: &RecallScope, report: &RecallReport) -> String {
    let scope = match scope {
        RecallScope::Repository(root) => format!("repository {}", root.display()),
        RecallScope::AllRepositories => "all repositories".to_owned(),
    };
    let mut human = format!("recall: {} hit(s) in {scope}\n", report.hits.len());
    for (index, hit) in report.hits.iter().enumerate() {
        human.push_str(&format!(
            "\n{}. [{}] {} {} {}:{}\n   {}\n",
            index + 1,
            hit.source.as_str(),
            hit.timestamp.as_deref().unwrap_or("unknown-time"),
            hit.role,
            hit.transcript,
            hit.line,
            hit.snippet
        ));
    }
    human.push_str("\nsources:\n");
    for source in &report.sources {
        human.push_str(&format!(
            "  {}: {} ({} file(s) scanned, {} unrecognized)",
            source.source.as_str(),
            source.status.as_str(),
            source.files_scanned,
            source.files_unrecognized
        ));
        if let Some(detail) = source.detail {
            human.push_str(&format!(" - {detail}"));
        }
        human.push('\n');
    }
    if report.redactions > 0 {
        human.push_str(&format!("redacted secrets: {}\n", report.redactions));
    }
    if report.truncated {
        human.push_str("note: a file, size, or time bound stopped the search early\n");
    }
    human
}
