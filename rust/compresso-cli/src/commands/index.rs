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
    let requested = if arguments.path.is_absolute() {
        arguments.path
    } else {
        cwd.join(arguments.path)
    };
    let root = fs::canonicalize(requested).map_err(|_| IndexCommandError::invalid_path())?;
    if !root.is_dir() {
        return Err(IndexCommandError::not_directory());
    }

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
        .map_err(|_| IndexCommandError::index_failed())?;
    parsed.sort();
    removed.sort();

    Ok(render(root, false, Some(report), parsed, removed))
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

    fn index_failed() -> Self {
        Self {
            code: "index_failed",
            message: "repository indexing failed".to_owned(),
        }
    }
}
