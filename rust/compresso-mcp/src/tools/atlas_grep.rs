//! `compresso_grep`: bounded text search over the bound repository's indexed
//! files (D87).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use compresso_atlas::{
    AtlasError, GrepFile, GrepLine, GrepOptions, GrepReport, grep, is_test_path, repository_stale,
    validate_grep,
};
use compresso_core::{TokenCounter, operation_identity};
use compresso_store::{RepoId, Store, SymbolKind};
use rmcp::model::JsonObject;
use serde_json::{Map, Value, json};

use super::Form;
use super::common::{
    REINDEX_HINT, ToolFailure, ToolResult, checked_arguments, map_atlas_error, navigation_counter,
    optional_bool, optional_string, optional_usize, resolve_repository, with_budget,
};
use super::text;

pub(crate) const TOOL_NAME: &str = "compresso_grep";
const DEFAULT_MAX_FILES: usize = 20;
const DEFAULT_MAX_MATCHES_PER_FILE: usize = 5;
const DEFAULT_TOKENS: usize = 1_500;
/// Room for the envelope and every pattern header at the pattern cap.
const MIN_TOKENS: usize = 300;
const MAX_TOKENS: usize = 10_000;
/// Lean-profile defaults (E17-33, D98), stated in the lean schema: fewer
/// rows per file and a smaller budget unless the call asks for more.
pub(crate) const LEAN_DEFAULT_MAX_MATCHES_PER_FILE: usize = 3;
pub(crate) const LEAN_DEFAULT_TOKENS: usize = 1_000;
/// Text form only (E17-34, D99): context lines are served around this many
/// listed match rows per call; later rows are listed without context.
const TEXT_CONTEXT_MATCH_ROWS: usize = 6;

pub(crate) fn handles(name: &str) -> bool {
    name == TOOL_NAME
}

pub(crate) fn dispatch(
    store: &Store,
    repository_root: Option<&Path>,
    arguments: Option<JsonObject>,
    form: Form,
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
    .unwrap_or(if form.is_lean() {
        LEAN_DEFAULT_MAX_MATCHES_PER_FILE
    } else {
        DEFAULT_MAX_MATCHES_PER_FILE
    });
    let context_lines = optional_usize(
        &mut arguments,
        "context_lines",
        0,
        compresso_atlas::GREP_MAX_CONTEXT_LINES,
    )?
    .unwrap_or(0);
    let max_tokens = optional_usize(&mut arguments, "max_tokens", MIN_TOKENS, MAX_TOKENS)?
        .unwrap_or(if form.is_lean() {
            LEAN_DEFAULT_TOKENS
        } else {
            DEFAULT_TOKENS
        });
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
    let tests = if form.is_text() {
        TestCode::of(store, repository.id, &report)
    } else {
        TestCode::default()
    };

    let mut budget = max_tokens.saturating_mul(4) / 5;
    loop {
        let fitted = report.fit_to_budget(budget, counter);
        let mut payload = payload(&fitted, &options, &operation_id, stale, counter);
        // Each form is held to max_tokens as it is sent.
        let tokens = if form.is_text() {
            compact_rows_of(&mut payload, &fitted, &tests);
            text::render(TOOL_NAME, &payload).map_or(0, |text| counter.count(&text))
        } else {
            stabilize_tokens(&mut payload, counter)?
        };
        if tokens <= max_tokens {
            return Ok(with_budget(payload, max_tokens));
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

/// Where test code sits in the files a grep report lists (E17-34, D99): a
/// test file by path, a `tests` or `test` module by its indexed line range,
/// or a function whose name starts with `test_`.
#[derive(Default)]
struct TestCode {
    modules: HashMap<String, Vec<(u32, u32)>>,
}

impl TestCode {
    /// The test module ranges of the listed files that are not test files.
    /// A file whose symbols cannot be read, or that changed since it was
    /// indexed, is judged by path and function name only.
    fn of(store: &Store, repo: RepoId, report: &GrepReport) -> Self {
        let mut modules = HashMap::new();
        for file in report.results.iter().flat_map(|result| &result.files) {
            if file.stale || is_test_path(&file.path) || modules.contains_key(&file.path) {
                continue;
            }
            let ranges = store
                .symbols_in_file(repo, &file.path)
                .unwrap_or_default()
                .into_iter()
                .filter(|symbol| {
                    symbol.kind == SymbolKind::Module
                        && matches!(symbol.name.as_str(), "tests" | "test")
                })
                .map(|symbol| (symbol.start_line, symbol.end_line))
                .collect::<Vec<_>>();
            modules.insert(file.path.clone(), ranges);
        }
        Self { modules }
    }

    fn contains(&self, path: &str, line: &GrepLine) -> bool {
        is_test_path(path)
            || line.symbol.as_deref().is_some_and(|symbol| {
                symbol
                    .rsplit("::")
                    .next()
                    .is_some_and(|name| name.starts_with("test_"))
            })
            || self.modules.get(path).is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|&(start, end)| start <= line.line && line.line <= end)
            })
    }
}

/// Text form only: the rows of each listed file (E17-33, D98), plus the
/// lines named instead of listed (E17-34, D99). While a pattern matches
/// code, its matches in test code are named under `tests`; a line an
/// earlier pattern of the call listed with the same snippet is named under
/// `above` (a long line cut around another match is listed again); context is
/// served around the first [`TEXT_CONTEXT_MATCH_ROWS`] listed match rows,
/// and `context_cut_after` says when later rows went without it. Every match the
/// JSON form lists is listed or named. The JSON payload is left unchanged.
fn compact_rows_of(payload: &mut Value, report: &GrepReport, tests: &TestCode) {
    let mut listed = HashSet::<(&str, u32, &str)>::new();
    let mut rows_left = TEXT_CONTEXT_MATCH_ROWS;
    let mut context_cut = false;
    for (result, value) in report.results.iter().zip(
        payload["results"]
            .as_array_mut()
            .map_or(&mut [][..], Vec::as_mut_slice),
    ) {
        let matches_code = result.files.iter().any(|file| {
            file.lines
                .iter()
                .any(|line| !tests.contains(&file.path, line))
        });
        for (file, file_value) in result.files.iter().zip(
            value["files"]
                .as_array_mut()
                .map_or(&mut [][..], Vec::as_mut_slice),
        ) {
            let mut shown = Vec::new();
            let mut test_lines = Vec::new();
            let mut above = Vec::new();
            for line in &file.lines {
                if listed.contains(&(file.path.as_str(), line.line, line.snippet.as_str())) {
                    above.push(line.line);
                } else if matches_code && tests.contains(&file.path, line) {
                    test_lines.push(line.line);
                } else {
                    listed.insert((file.path.as_str(), line.line, line.snippet.as_str()));
                    let with_context = rows_left > 0;
                    rows_left = rows_left.saturating_sub(1);
                    let has_context = !line.before.is_empty() || !line.after.is_empty();
                    context_cut |= has_context && !with_context;
                    shown.push((line, with_context));
                }
            }
            file_value["lines"] = json!(compact_rows(&shown));
            if !test_lines.is_empty() {
                file_value["tests"] = json!(test_lines);
            }
            if !above.is_empty() {
                file_value["above"] = json!(above);
            }
        }
    }
    if context_cut {
        payload["context_cut_after"] = json!(TEXT_CONTEXT_MATCH_ROWS);
    }
}

/// One file's listed rows for the text form. A file block's first match row
/// is always tagged (`[-]` for no symbol); a later row is untagged exactly
/// when it is in the same symbol as the match row above it, is not a
/// definition, and its snippet does not start with `[`.
fn compact_rows(shown: &[(&GrepLine, bool)]) -> Vec<String> {
    let context = |line: &compresso_atlas::GrepContextLine| format!("{}- {}", line.line, line.text);
    let mut rows = Vec::new();
    // The previous listed match row's symbol in this file block; `None`
    // before the first row, so a block's first row is always tagged.
    let mut previous: Option<Option<&str>> = None;
    for &(line, with_context) in shown {
        if with_context {
            rows.extend(line.before.iter().map(context));
        }
        let symbol = line.symbol.as_deref();
        // A snippet starting with `[` keeps its tag, so it is never read
        // as one.
        let inherits =
            previous == Some(symbol) && !line.definition && !line.snippet.starts_with('[');
        let tag = match symbol {
            Some(symbol) if line.definition => format!("[def {symbol}] "),
            _ if inherits => String::new(),
            Some(symbol) => format!("[{symbol}] "),
            None => "[-] ".to_owned(),
        };
        rows.push(format!("{} {tag}{}", line.line, line.snippet));
        previous = Some(symbol);
        if with_context {
            rows.extend(line.after.iter().map(context));
        }
    }
    rows
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
