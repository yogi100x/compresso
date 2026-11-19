//! `compresso_grep`: bounded text search over the bound repository's indexed
//! files (D87).

use std::path::Path;

use compresso_atlas::{
    AtlasError, GrepFile, GrepOptions, GrepReport, grep, repository_stale, validate_grep,
};
use compresso_core::{TokenCounter, operation_identity};
use compresso_store::Store;
use rmcp::model::JsonObject;
use serde_json::{Map, Value, json};

use super::common::{
    REINDEX_HINT, ToolFailure, ToolResult, checked_arguments, map_atlas_error, navigation_counter,
    optional_bool, optional_string, optional_usize, resolve_repository,
};

pub(crate) const TOOL_NAME: &str = "compresso_grep";
const DEFAULT_MAX_FILES: usize = 20;
const DEFAULT_MAX_MATCHES_PER_FILE: usize = 5;
const DEFAULT_TOKENS: usize = 1_500;
/// Room for the envelope and every pattern header at the pattern cap.
const MIN_TOKENS: usize = 300;
const MAX_TOKENS: usize = 10_000;

pub(crate) fn handles(name: &str) -> bool {
    name == TOOL_NAME
}

pub(crate) fn dispatch(
    store: &Store,
    repository_root: Option<&Path>,
    arguments: Option<JsonObject>,
) -> ToolResult {
    let mut arguments = checked_arguments(
        arguments,
        &[
            "root",
            "pattern",
            "patterns",
            "regex",
            "case_insensitive",
            "path",
            "max_files",
            "max_matches_per_file",
            "context_lines",
            "max_tokens",
        ],
    )?;
    let root = optional_string(&mut arguments, "root", false)?;
    let patterns = patterns_argument(&mut arguments)?;
    let regex = optional_bool(&mut arguments, "regex")?.unwrap_or(false);
    let case_insensitive = optional_bool(&mut arguments, "case_insensitive")?.unwrap_or(false);
    let path = optional_string(&mut arguments, "path", false)?;
    let max_files = optional_usize(
        &mut arguments,
        "max_files",
        1,
        compresso_atlas::GREP_MAX_FILES_LIMIT,
    )?
    .unwrap_or(DEFAULT_MAX_FILES);
    let max_matches_per_file = optional_usize(
        &mut arguments,
        "max_matches_per_file",
        1,
        compresso_atlas::GREP_MAX_MATCHES_PER_FILE_LIMIT,
    )?
    .unwrap_or(DEFAULT_MAX_MATCHES_PER_FILE);
    let context_lines = optional_usize(
        &mut arguments,
        "context_lines",
        0,
        compresso_atlas::GREP_MAX_CONTEXT_LINES,
    )?
    .unwrap_or(0);
    let max_tokens = optional_usize(&mut arguments, "max_tokens", MIN_TOKENS, MAX_TOKENS)?
        .unwrap_or(DEFAULT_TOKENS);
    let options = GrepOptions {
        patterns: &patterns,
        regex,
        case_insensitive,
        path: path.as_deref(),
        max_files,
        max_matches_per_file,
        context_lines,
    };
    // Reject a bad pattern before the repository is even resolved.
    validate_grep(&options).map_err(|error| map_grep_error(error.into()))?;
    let repository = resolve_repository(store, repository_root, root.as_deref())?;
    let report = grep(store, repository.id, &options).map_err(map_grep_error)?;
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;
    let operation_id = operation_identity().id;
    let counter = navigation_counter();

    let mut budget = max_tokens.saturating_mul(4) / 5;
    loop {
        let fitted = report.fit_to_budget(budget, counter);
        let mut payload = payload(&fitted, &options, &operation_id, stale, counter);
        let tokens = stabilize_tokens(&mut payload, counter)?;
        if tokens <= max_tokens {
            return Ok(payload);
        }
        if budget == 0 {
            // Even with no line listed, the per-pattern totals do not fit.
            return Err(ToolFailure::invalid(format!(
                "argument 'max_tokens' ({max_tokens}) cannot hold the totals of {} patterns; raise it or search fewer patterns per call.",
                patterns.len()
            )));
        }
        budget = budget.saturating_sub(tokens - max_tokens + 16);
    }
}

/// `patterns` (an array) or `pattern` (one string), exactly one of them.
fn patterns_argument(arguments: &mut Map<String, Value>) -> Result<Vec<String>, ToolFailure> {
    let single = optional_string(arguments, "pattern", true)?;
    let many = arguments.remove("patterns");
    match (single, many) {
        (Some(pattern), None) => Ok(vec![pattern]),
        (None, Some(Value::Array(values))) => values
            .into_iter()
            .map(|value| match value {
                Value::String(pattern) => Ok(pattern),
                _ => Err(ToolFailure::invalid(
                    "argument 'patterns' must be an array of strings.",
                )),
            })
            .collect(),
        (None, Some(_)) => Err(ToolFailure::invalid(
            "argument 'patterns' must be an array of strings.",
        )),
        (Some(_), Some(_)) => Err(ToolFailure::invalid(
            "pass either 'pattern' or 'patterns', not both.",
        )),
        (None, None) => Err(ToolFailure::invalid(
            "missing required argument 'patterns' (or a single 'pattern').",
        )),
    }
}

fn map_grep_error(error: AtlasError) -> ToolFailure {
    match error {
        AtlasError::Grep(error) => ToolFailure::invalid(format!("{error}.")),
        other => map_atlas_error(other),
    }
}

fn payload(
    report: &GrepReport,
    options: &GrepOptions<'_>,
    operation_id: &str,
    stale: bool,
    counter: &dyn TokenCounter,
) -> Value {
    let results = report
        .results
        .iter()
        .map(|result| {
            json!({
                "pattern": result.pattern,
                "matches_total": result.matches_total,
                "files_total": result.files_total,
                "lines_listed": result.lines_listed(),
                "files": result.files.iter().map(file_value).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let unlisted = report
        .results
        .iter()
        .map(|result| result.matches_total - result.lines_listed())
        .sum::<usize>();
    let note = if report.scan_truncated {
        "The scan stopped at its file or byte cap, so totals are lower bounds. Narrow with 'path' instead of repeating the search."
            .to_owned()
    } else if report.files_skipped() > 0 {
        format!(
            "{} files in scope were not searched (over the read size ceiling or unreadable; see scan), so totals may be low.{}",
            report.files_skipped(),
            if unlisted > 0 {
                format!(" {unlisted} matching lines are counted but not listed.")
            } else {
                String::new()
            }
        )
    } else if unlisted > 0 {
        format!(
            "matches_total is exact for every indexed file in scope; {unlisted} matching lines are counted but not listed. Narrow with 'path' or a more specific pattern rather than repeating the same search with larger limits."
        )
    } else {
        "Every match in the indexed files in scope is listed; larger limits would return nothing more."
            .to_owned()
    };
    json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "mode": if options.regex { "regex" } else { "literal" },
        "case_insensitive": options.case_insensitive,
        "path": options.path,
        "results": results,
        "scan": {
            "files_in_scope": report.files_in_scope,
            "files_scanned": report.files_scanned,
            "files_skipped_oversize": report.files_skipped_oversize,
            "files_skipped_unreadable": report.files_skipped_unreadable,
            "stale_files": report.stale_files,
            "scan_truncated": report.scan_truncated,
        },
        "complete": report.complete(),
        "note": note,
        "stale": stale,
        "reindex_hint": stale.then_some(REINDEX_HINT),
        "tokens_used": 0,
        "token_provider": counter.provider(),
        "truncated": !report.complete(),
    })
}

/// One file: its total and its listed lines as `line [symbol] snippet`.
fn file_value(file: &GrepFile) -> Value {
    // Context lines (`line- text`) sit between the matches they surround.
    let lines = file
        .lines
        .iter()
        .flat_map(compresso_atlas::GrepLine::rendered_rows)
        .collect::<Vec<_>>();
    let mut value = json!({
        "path": file.path,
        "matches": file.matches_total,
        "lines": lines,
    });
    if file.stale {
        value["stale"] = Value::Bool(true);
    }
    value
}

fn stabilize_tokens(payload: &mut Value, counter: &dyn TokenCounter) -> Result<usize, ToolFailure> {
    let mut previous = usize::MAX;
    for _ in 0..4 {
        let serialized = serde_json::to_string(payload).map_err(|_| ToolFailure::internal())?;
        let tokens = counter.count(&serialized);
        payload["tokens_used"] = json!(tokens);
        if tokens == previous {
            return Ok(tokens);
        }
        previous = tokens;
    }
    let serialized = serde_json::to_string(payload).map_err(|_| ToolFailure::internal())?;
    Ok(counter.count(&serialized))
}
