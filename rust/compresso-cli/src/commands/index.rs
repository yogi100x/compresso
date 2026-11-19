use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use compresso_atlas::{IndexProgress, IndexReport, Indexer};
use compresso_core::{TokenProvider, counter_for};
use compresso_store::Store;
use serde_json::{Value, json};

use super::OutputArgs;

#[derive(Args, Debug)]
pub struct IndexArgs {
    /// Repository directory to index.
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,
    #[command(flatten)]
    pub output: OutputArgs,
    /// Preview the resolved repository without approving or indexing it.
    #[arg(long)]
    pub dry_run: bool,
}

/// Longest `index_failed` message, cause included.
const INDEX_FAILED_MESSAGE_MAX_BYTES: usize = 512;

pub(crate) struct Rendered {
    pub json: Value,
    pub human: String,
}

pub(crate) struct IndexCommandError {
    pub code: &'static str,
    pub message: String,
}

pub(crate) fn execute(
    arguments: IndexArgs,
    store: Option<&Store>,
    cwd: &Path,
) -> Result<Rendered, IndexCommandError> {
    let root = resolve_root(&arguments.path, cwd)?;

    if arguments.dry_run {
        return Ok(render(root, true, None, Vec::new(), Vec::new()));
    }

    let store = store.ok_or_else(IndexCommandError::store_unavailable)?;
    let mut parsed = Vec::new();
    let mut removed = Vec::new();
    let report = Indexer::new(store, counter_for(TokenProvider::OpenAiO200k))
        .index(&root, |event| match event {
            IndexProgress::Parsing { path } => parsed.push(path_text(&path)),
            IndexProgress::Removed { path } => removed.push(path),
        })
        .map_err(|error| IndexCommandError::index_failed(&error))?;
    parsed.sort();
    removed.sort();

    Ok(render(root, false, Some(report), parsed, removed))
}

pub(crate) fn resolve_root(path: &Path, cwd: &Path) -> Result<PathBuf, IndexCommandError> {
    let requested = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let root = fs::canonicalize(requested).map_err(|_| IndexCommandError::invalid_path())?;
    if !root.is_dir() {
        return Err(IndexCommandError::not_directory());
    }
    Ok(root)
}

fn render(
    root: PathBuf,
    dry_run: bool,
    report: Option<IndexReport>,
    parsed: Vec<String>,
    removed: Vec<String>,
) -> Rendered {
    let root = root.to_string_lossy().into_owned();
    let report_json = report.as_ref().map(|report| {
        json!({
            "call_site_files_rejected": report.call_site_files_rejected,
            "call_site_files_truncated": report.call_site_files_truncated,
            "call_sites": report.call_sites,
            "duration_ms": report.duration_ms,
            "files_indexed": report.files_indexed,
            "files_removed": report.files_removed,
            "files_skipped": report.files_skipped,
            "imports_resolved": report.imports_resolved,
            "imports_unresolved": report.imports_unresolved,
            "stale": report.stale,
            "symbols": report.symbols,
        })
    });
    let json = json!({
        "schema_version": 1,
        "data": {
            "action": "index",
            "dry_run": dry_run,
            "progress": {
                "parsed": parsed,
                "removed": removed,
            },
            "report": report_json,
            "root": root,
        },
    });
    let human = match report {
        Some(report) => format!(
            "Indexed {root}: {} files indexed, {} removed, {} skipped, {} symbols.\n",
            report.files_indexed, report.files_removed, report.files_skipped, report.symbols
        ),
        None => format!("Would index {root}.\n"),
    };
    Rendered { json, human }
}

fn path_text(path: &Path) -> String {
    path.iter()
        .map(|component| component.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

impl IndexCommandError {
    fn invalid_path() -> Self {
        Self {
            code: "invalid_arguments",
            message: "index path does not exist or cannot be resolved".to_owned(),
        }
    }

    fn not_directory() -> Self {
        Self {
            code: "invalid_arguments",
            message: "index path is not a directory".to_owned(),
        }
    }

    fn store_unavailable() -> Self {
        Self {
            code: "internal",
            message: "failed to open the local Compresso store".to_owned(),
        }
    }

    /// Names the underlying cause (and its sources) so a failure can be
    /// acted on, bounded so a long path cannot flood the output.
    fn index_failed(error: &dyn std::error::Error) -> Self {
        let mut cause = error.to_string();
        let mut source = error.source();
        while let Some(next) = source {
            cause.push_str(": ");
            cause.push_str(&next.to_string());
            source = next.source();
        }
        let cause = cause.replace(['\n', '\r'], " ");
        let mut message = format!("repository indexing failed: {cause}");
        if message.len() > INDEX_FAILED_MESSAGE_MAX_BYTES {
            let mut end = INDEX_FAILED_MESSAGE_MAX_BYTES;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push('…');
        }
        Self {
            code: "index_failed",
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("outer failure")]
    struct Outer(#[source] std::io::Error);

    #[test]
    fn test_index_failed_names_the_cause_chain_on_one_bounded_line() {
        let error = Outer(std::io::Error::other("disk\nfull"));
        let failure = IndexCommandError::index_failed(&error);
        assert_eq!(failure.code, "index_failed");
        assert_eq!(
            failure.message,
            "repository indexing failed: outer failure: disk full"
        );

        let long = Outer(std::io::Error::other("é".repeat(1_000)));
        let failure = IndexCommandError::index_failed(&long);
        assert!(failure.message.len() <= INDEX_FAILED_MESSAGE_MAX_BYTES + '…'.len_utf8());
        assert!(failure.message.ends_with('…'));
    }
}
