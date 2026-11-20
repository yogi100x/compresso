//! `compresso_grep`: bounded text search over the bound repository's indexed
//! files (D87).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use compresso_atlas::{
    AtlasError, GrepError, GrepFile, GrepLine, GrepOptions, GrepReport, grep, repository_stale,
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
use super::{atlas_read, text};

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
            // Packed lookups (E17-36, D101): accepted in every profile,
            // advertised only by the lean schema.
            "ranges",
        ],
    )?;
    let root = optional_string(&mut arguments, "root", false)?;
    let mut clamped = Vec::new();
    let (patterns, scopes, failed) =
        patterns_argument(&mut arguments, form.is_lean().then_some(&mut clamped))?;
    let ranges = arguments
        .remove("ranges")
        .map(atlas_read::parse_ranges)
        .transpose()?;
    let range_count = ranges.as_ref().map_or(0, Vec::len);
    let regex = optional_bool(&mut arguments, "regex")?.unwrap_or(false);
    let case_insensitive = optional_bool(&mut arguments, "case_insensitive")?.unwrap_or(false);
    let path = optional_string(&mut arguments, "path", false)?;
    let max_files = clamped_usize(
        &mut arguments,
        form.is_lean().then_some(&mut clamped),
        "max_files",
        1,
        compresso_atlas::GREP_MAX_FILES_LIMIT,
    )?
    .unwrap_or(DEFAULT_MAX_FILES);
    let max_matches_per_file = clamped_usize(
        &mut arguments,
        form.is_lean().then_some(&mut clamped),
        "max_matches_per_file",
        1,
        compresso_atlas::GREP_MAX_MATCHES_PER_FILE_LIMIT,
    )?
    .unwrap_or(if form.is_lean() {
        LEAN_DEFAULT_MAX_MATCHES_PER_FILE
    } else {
        DEFAULT_MAX_MATCHES_PER_FILE
    });
    let context_lines = clamped_usize(
        &mut arguments,
        form.is_lean().then_some(&mut clamped),
        "context_lines",
        0,
        compresso_atlas::GREP_MAX_CONTEXT_LINES,
    )?
    .unwrap_or(0);
    let grep_default = if form.is_lean() {
        LEAN_DEFAULT_TOKENS
    } else {
        DEFAULT_TOKENS
    };
    let ranges_default = if range_count > 0 {
        RANGES_DEFAULT_TOKENS
    } else {
        0
    };
    let max_tokens = clamped_usize(
        &mut arguments,
        form.is_lean().then_some(&mut clamped),
        "max_tokens",
        MIN_TOKENS,
        MAX_TOKENS,
    )?
    .unwrap_or(grep_default + ranges_default);
    let options = GrepOptions {
        patterns: &patterns,
        regex,
        case_insensitive,
        path: path.as_deref(),
        max_files,
        max_matches_per_file,
        context_lines,
        scopes: &scopes,
        // Text form only (E17-39, D104): the JSON form keeps the first
        // lines in line order, byte for byte.
        select_rows: form.is_text(),
    };
    // Reject a bad pattern before the repository is even resolved.
    // Errors name the caller's pattern index, even after a pattern whose
    // scope failed was dropped.
    let caller_index = |index: usize| {
        let dropped = failed
            .iter()
            .filter_map(|entry| entry["index"].as_u64())
            .map(|index| index as usize)
            .collect::<Vec<_>>();
        (0..)
            .filter(|i| !dropped.contains(i))
            .nth(index)
            .unwrap_or(index)
    };
    validate_grep(&options)
        .map_err(|error| map_grep_error(remap_index(error, caller_index).into()))?;
    let repository = resolve_repository(store, repository_root, root.as_deref())?;
    let mut report = grep(store, repository.id, &options).map_err(map_grep_error)?;
    // The round-trip notes of E17-38 (D103) are lean only: the full
    // profile's behaviour, not only its schemas, is unchanged.
    let lean = form.is_lean();
    let alternations = if lean {
        search_literal_alternations(store, repository.id, &options, &mut report)
    } else {
        Vec::new()
    };
    let alternation_indexes = alternations
        .iter()
        .filter_map(|entry| entry["index"].as_u64())
        .map(|index| caller_index(usize::try_from(index).unwrap_or(usize::MAX)))
        .collect::<Vec<_>>();
    let alternations = alternations
        .into_iter()
        .map(|mut entry| {
            if let Some(object) = entry.as_object_mut() {
                object.remove("index");
            }
            entry
        })
        .collect::<Vec<_>>();
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;
    let operation_id = operation_identity().id;
    let counter = navigation_counter();
    let tests = if form.is_text() {
        TestCode::of(store, repository.id, &report)
    } else {
        TestCode::default()
    };

    // A packed call shares max_tokens in the proportion of the two
    // defaults, so a greedy pattern cannot starve the reads (the reads share
    // theirs evenly by range); the reads also get whatever the grep left.
    let grep_tokens = if range_count == 0 {
        max_tokens
    } else {
        // At most half of a small budget, so the ranges keep room too.
        (max_tokens * grep_default / (grep_default + ranges_default))
            .max(MIN_TOKENS.min(max_tokens / 2))
    };
    let mut budget = grep_tokens.saturating_mul(4) / 5;
    // The round-trip notes (E17-38, D103) never displace a listed row or
    // fail a call: the first time a result does not fit, the names of
    // unlisted files are dropped and the same budget is tried again; the
    // next time, the alternation notes shrink to one line of pattern
    // indexes and the unmatched-path notes are dropped.
    // Unlisted line numbers (E17-39, D104) are dropped next, before the
    // other notes shrink.
    let mut names = lean;
    let mut line_names = true;
    let mut full_notes = true;
    let (mut payload, tokens) = loop {
        let mut fitted = report.fit_to_budget(budget, counter);
        if !names {
            for result in &mut fitted.results {
                result.files_unlisted.clear();
            }
        }
        if !line_names {
            for file in fitted
                .results
                .iter_mut()
                .flat_map(|result| &mut result.files)
            {
                file.lines_unlisted.clear();
            }
        }
        let mut payload = payload(&fitted, &options, &operation_id, stale, counter);
        label_scopes(&mut payload, &scopes);
        if !failed.is_empty() {
            payload["patterns_failed"] = json!(failed);
        }
        if !clamped.is_empty() {
            payload["clamped"] = json!(clamped);
        }
        if !alternations.is_empty() {
            payload["literal_alternation"] = if full_notes {
                json!(alternations)
            } else {
                json!(
                    alternation_indexes
                        .iter()
                        .map(|index| json!({ "index": index }))
                        .collect::<Vec<_>>()
                )
            };
        }
        if lean && full_notes && !report.paths_unmatched.is_empty() {
            payload["paths_unmatched"] = report
                .paths_unmatched
                .iter()
                .map(|entry| match &entry.nearest {
                    Some(nearest) => json!({ "path": entry.path, "nearest": nearest }),
                    None => json!({ "path": entry.path }),
                })
                .collect();
        }
        // Each form is held to max_tokens as it is sent.
        let tokens = if form.is_text() {
            compact_rows_of(&mut payload, &fitted, &tests, context_lines);
            text::render(TOOL_NAME, &payload).map_or(0, |text| counter.count(&text))
        } else {
            stabilize_tokens(&mut payload, counter)?
        };
        if tokens <= grep_tokens {
            break (payload, tokens);
        }
        if names
            && fitted
                .results
                .iter()
                .any(|result| !result.files_unlisted.is_empty())
        {
            names = false;
            continue;
        }
        if line_names
            && form.is_text()
            && fitted
                .results
                .iter()
                .flat_map(|result| &result.files)
                .any(|file| !file.lines_unlisted.is_empty())
        {
            line_names = false;
            continue;
        }
        if full_notes && (!alternations.is_empty() || (lean && !report.paths_unmatched.is_empty()))
        {
            full_notes = false;
            continue;
        }
        if budget == 0 {
            // Even with no line listed, the per-pattern totals do not fit.
            return Err(ToolFailure::invalid(format!(
                "argument 'max_tokens' ({max_tokens}) cannot hold the totals of {} patterns; raise it or search fewer patterns per call.",
                patterns.len()
            )));
        }
        budget = budget.saturating_sub(tokens - grep_tokens + 16);
    };
    if let Some(ranges) = ranges {
        let mut left = max_tokens
            .saturating_sub(tokens)
            .saturating_sub(RANGE_HEADER_TOKENS * range_count);
        let truncated = payload["truncated"].as_bool().unwrap_or(false);
        // The text form is re-fitted as sent by the caller; the JSON form
        // is held to max_tokens here, envelope included, by withholding the
        // overshoot from the reads' content budget.
        for attempt in 1..=JSON_FIT_ATTEMPTS {
            let reads = atlas_read::read_ranges(store, &repository, &ranges, left, form)?;
            payload["ranges"] = json!(reads.values);
            payload["ranges_failed"] = json!(reads.failed);
            payload["truncated"] = Value::Bool(truncated || reads.truncated);
            if form.is_text() {
                break;
            }
            let sent = stabilize_tokens(&mut payload, counter)?;
            if sent <= max_tokens {
                break;
            }
            if left == 0 || attempt == JSON_FIT_ATTEMPTS {
                // Even with no content read, the envelope does not fit.
                return Err(ToolFailure::invalid(format!(
                    "argument 'max_tokens' ({max_tokens}) cannot hold the totals of {} patterns and the headers of {range_count} ranges; raise it or send fewer lookups per call.",
                    patterns.len()
                )));
            }
            left = left.saturating_sub(sent - max_tokens + 16);
        }
    }
    Ok(with_budget(payload, max_tokens))
}

/// The tokens the read ranges of a packed lean call add to the default
/// `max_tokens` (E17-36, D101): the `compresso_read` default, so packing a
/// read never serves less than the separate read call would.
pub(crate) const RANGES_DEFAULT_TOKENS: usize = atlas_read::READ_DEFAULT_TOKENS;
/// Tokens withheld per range for its `@@` header and notes.
const RANGE_HEADER_TOKENS: usize = 24;
/// Read passes a JSON-form packed call makes to fit its envelope.
const JSON_FIT_ATTEMPTS: usize = 4;

/// A limit argument. In the lean profile a value above `maximum` is clamped
/// to it and named in `clamped` (E17-36, D101), since a larger limit is a
/// request the server can still answer, and so is a `max_tokens` below its
/// minimum, which is raised to it (E17-38, D103); any other out-of-range
/// value, and every out-of-range value in the full profile, is rejected as
/// before.
fn clamped_usize(
    arguments: &mut Map<String, Value>,
    clamped: Option<&mut Vec<String>>,
    name: &'static str,
    minimum: usize,
    maximum: usize,
) -> Result<Option<usize>, ToolFailure> {
    if let Some(clamped) = clamped
        && let Some(value) = arguments.get(name).and_then(Value::as_u64)
    {
        let bound = if value > maximum as u64 {
            Some(maximum)
        } else if name == "max_tokens" && value < minimum as u64 {
            Some(minimum)
        } else {
            None
        };
        if let Some(bound) = bound {
            arguments.remove(name);
            clamped.push(format!("{name} {value} -> {bound}"));
            return Ok(Some(bound));
        }
    }
    optional_usize(arguments, name, minimum, maximum)
}

/// A literal pattern holding an unescaped `|` that matched nothing as
/// written is searched again as an alternation of its literal parts, in one
/// more scan over the same files and limits, and its result replaces the
/// empty one (E17-38, D103). Each part is escaped, so no regex syntax is
/// added. A pattern whose escaped alternation would exceed the pattern
/// limit is not retried, and a failed retry keeps the empty results: the
/// retry never fails a call. Returns `{index, pattern, parts}` for every
/// pattern searched that way.
fn search_literal_alternations(
    store: &Store,
    repo: RepoId,
    options: &GrepOptions<'_>,
    report: &mut GrepReport,
) -> Vec<Value> {
    if options.regex {
        return Vec::new();
    }
    let retried = report
        .results
        .iter()
        .enumerate()
        .filter(|(_, result)| result.matches_total == 0)
        .filter_map(|(index, _)| {
            compresso_atlas::literal_alternatives(&options.patterns[index])
                .filter(|parts| {
                    compresso_atlas::literal_alternation_source(parts).len()
                        <= compresso_atlas::GREP_MAX_PATTERN_BYTES
                })
                .map(|parts| (index, parts))
        })
        .collect::<Vec<_>>();
    if retried.is_empty() {
        return Vec::new();
    }
    let patterns = retried
        .iter()
        .map(|(_, parts)| compresso_atlas::literal_alternation_source(parts))
        .collect::<Vec<_>>();
    let scopes = if options.scopes.is_empty() {
        Vec::new()
    } else {
        retried
            .iter()
            .map(|(index, _)| options.scopes[*index].clone())
            .collect()
    };
    let Ok(second) = grep(
        store,
        repo,
        &GrepOptions {
            patterns: &patterns,
            regex: true,
            scopes: &scopes,
            ..*options
        },
    ) else {
        return Vec::new();
    };
    report.files_scanned += second.files_scanned;
    report.bytes_scanned = report.bytes_scanned.saturating_add(second.bytes_scanned);
    report.scan_truncated |= second.scan_truncated;
    report.output_truncated |= second.output_truncated;
    let mut notes = Vec::with_capacity(retried.len());
    for ((index, parts), mut result) in retried.into_iter().zip(second.results) {
        result.pattern.clone_from(&options.patterns[index]);
        report.results[index] = result;
        notes.push(json!({ "index": index, "pattern": options.patterns[index], "parts": parts }));
    }
    notes
}

/// Names each scoped pattern's own path on its result (E17-36, D101).
fn label_scopes(payload: &mut Value, scopes: &[Option<String>]) {
    let Some(results) = payload["results"].as_array_mut() else {
        return;
    };
    for (result, scope) in results.iter_mut().zip(scopes) {
        if let Some(scope) = scope {
            result["path"] = json!(scope);
        }
    }
}

/// The parsed `patterns`: the patterns searched, their scopes (empty when
/// no pattern has one), and the scoped patterns whose scope was invalid.
type Patterns = (Vec<String>, Vec<Option<String>>, Vec<Value>);

/// `patterns` (an array) or `pattern` (one string), exactly one of them. A
/// `patterns` entry may be `{pattern, path}`, giving that pattern its own
/// scope (E17-36, D101). A pattern whose scope is invalid fails alone and is
/// reported in its own section; when every pattern fails, the call fails.
/// With `clamped` (the lean profile), entries past the pattern cap are
/// dropped unparsed and named there instead of failing the call (E17-38,
/// D103).
fn patterns_argument(
    arguments: &mut Map<String, Value>,
    clamped: Option<&mut Vec<String>>,
) -> Result<Patterns, ToolFailure> {
    let single = optional_string(arguments, "pattern", true)?;
    let mut many = arguments.remove("patterns");
    if let (Some(clamped), Some(Value::Array(values))) = (clamped, many.as_mut()) {
        let cap = compresso_atlas::GREP_MAX_PATTERNS;
        if values.len() > cap {
            clamped.push(format!(
                "patterns {} -> {cap} (the last {} not searched)",
                values.len(),
                values.len() - cap
            ));
            values.truncate(cap);
        }
    }
    let invalid = || {
        ToolFailure::invalid(
            "argument 'patterns' must be an array of strings or {pattern, path} objects.",
        )
    };
    let entries = match (single, many) {
        (Some(pattern), None) => return Ok((vec![pattern], Vec::new(), Vec::new())),
        (None, Some(Value::Array(values))) => values
            .into_iter()
            .map(|value| match value {
                Value::String(pattern) => Ok((pattern, None)),
                Value::Object(entry) => {
                    let mut entry = checked_arguments(Some(entry), &["pattern", "path"])?;
                    let pattern =
                        optional_string(&mut entry, "pattern", true)?.ok_or_else(invalid)?;
                    let path = optional_string(&mut entry, "path", false)?;
                    Ok((pattern, path))
                }
                _ => Err(invalid()),
            })
            .collect::<Result<Vec<_>, _>>()?,
        (None, Some(_)) => return Err(invalid()),
        (Some(_), Some(_)) => {
            return Err(ToolFailure::invalid(
                "pass either 'pattern' or 'patterns', not both.",
            ));
        }
        (None, None) => {
            return Err(ToolFailure::invalid(
                "missing required argument 'patterns' (or a single 'pattern').",
            ));
        }
    };
    let scoped = entries.iter().any(|(_, scope)| scope.is_some());
    if !scoped || entries.len() > compresso_atlas::GREP_MAX_PATTERNS {
        // Unscoped, or a call error whatever the scopes: validated whole.
        let (patterns, scopes): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        return Ok((
            patterns,
            if scoped { scopes } else { Vec::new() },
            Vec::new(),
        ));
    }
    let (mut kept, mut failed, mut first_failure) = (Vec::new(), Vec::new(), None);
    for (index, (pattern, scope)) in entries.into_iter().enumerate() {
        match scope.as_deref().and_then(scope_failure) {
            Some(failure) => {
                failed.push(json!({
                    "index": index,
                    "pattern": pattern,
                    "path": scope,
                    "error": { "code": failure.code(), "message": failure.message() },
                }));
                first_failure.get_or_insert(failure);
            }
            None => kept.push((pattern, scope)),
        }
    }
    if kept.is_empty()
        && let Some(failure) = first_failure
    {
        return Err(failure);
    }
    let any_scope = kept.iter().any(|(_, scope)| scope.is_some());
    let (patterns, scopes): (Vec<_>, Vec<_>) = kept.into_iter().unzip();
    Ok((
        patterns,
        if any_scope { scopes } else { Vec::new() },
        failed,
    ))
}

/// Why one pattern's own scope is rejected, if it is.
fn scope_failure(scope: &str) -> Option<ToolFailure> {
    let probe = [String::from("x")];
    let scopes = [Some(scope.to_owned())];
    validate_grep(&GrepOptions {
        patterns: &probe,
        regex: false,
        case_insensitive: false,
        path: None,
        max_files: 1,
        max_matches_per_file: 1,
        context_lines: 0,
        scopes: &scopes,
        select_rows: false,
    })
    .err()
    .map(|error| map_grep_error(error.into()))
}

/// `error` with its pattern index mapped through `caller_index`.
fn remap_index(error: GrepError, caller_index: impl Fn(usize) -> usize) -> GrepError {
    match error {
        GrepError::EmptyPattern { index } => GrepError::EmptyPattern {
            index: caller_index(index),
        },
        GrepError::PatternTooLong { index, max } => GrepError::PatternTooLong {
            index: caller_index(index),
            max,
        },
        GrepError::PatternControlCharacter { index } => GrepError::PatternControlCharacter {
            index: caller_index(index),
        },
        GrepError::InvalidPattern { index, reason } => GrepError::InvalidPattern {
            index: caller_index(index),
            reason,
        },
        other => other,
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
            let mut value = json!({
                "pattern": result.pattern,
                "matches_total": result.matches_total,
                "files_total": result.files_total,
                "lines_listed": result.lines_listed(),
                "files": result.files.iter().map(file_value).collect::<Vec<_>>(),
            });
            // E17-38 (D103): only when a matched file is not listed.
            if !result.files_unlisted.is_empty() {
                value["files_unlisted"] = result
                    .files_unlisted
                    .iter()
                    .map(|(path, matches)| json!({ "path": path, "matches": matches }))
                    .collect();
            }
            value
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
/// file under a test directory (`test`, `tests`, `__tests__`, `testdata`
/// path components), a file whose language reserves its name for tests
/// (`_test.go`, E17-37, D102), or a line inside an indexed module named
/// `tests`. Other file names and function names never make a row test code,
/// so production code such as `src/test_utils.rs`, `test_utils.go`, or
/// `fn test_connection` is listed; an out-of-line `mod tests;` file is
/// listed too.
#[derive(Default)]
struct TestCode {
    modules: HashMap<String, Vec<(u32, u32)>>,
}

/// Whether `path` is test code as a whole file: under a test directory, or
/// named as a test file by its language (`db_test.go`).
fn whole_file_is_test(path: &str) -> bool {
    in_test_named_directory(path) || compresso_atlas::parsers::languages::is_test_file(path)
}

/// Whether a directory of `path` (not its file name) is a test directory.
fn in_test_named_directory(path: &str) -> bool {
    let mut parts = path.split('/').collect::<Vec<_>>();
    parts.pop();
    parts.into_iter().any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            "test" | "tests" | "__tests__" | "testdata"
        )
    })
}

impl TestCode {
    /// The `tests` module ranges of the listed files outside test
    /// directories. A file whose symbols cannot be read, or that changed
    /// since it was indexed, has none, so its rows are listed.
    fn of(store: &Store, repo: RepoId, report: &GrepReport) -> Self {
        let mut modules = HashMap::new();
        for file in report.results.iter().flat_map(|result| &result.files) {
            if file.stale || whole_file_is_test(&file.path) || modules.contains_key(&file.path) {
                continue;
            }
            let ranges = store
                .symbols_in_file(repo, &file.path)
                .unwrap_or_default()
                .into_iter()
                .filter(|symbol| symbol.kind == SymbolKind::Module && symbol.name == "tests")
                .map(|symbol| (symbol.start_line, symbol.end_line))
                .collect::<Vec<_>>();
            modules.insert(file.path.clone(), ranges);
        }
        Self { modules }
    }

    fn contains(&self, path: &str, line: &GrepLine) -> bool {
        whole_file_is_test(path)
            || self.modules.get(path).is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|&(start, end)| start <= line.line && line.line <= end)
            })
    }
}

/// How the text form shows one matching line.
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    Listed,
    /// Named in the file header: test code while the pattern lists code.
    Test,
    /// Named in the file header: an earlier pattern listed the same row.
    Above,
}

/// Text form only: the rows of each listed file (E17-33, D98), plus the
/// lines named instead of listed (E17-34, D99). A line an earlier pattern
/// of the call listed with the same snippet is named under `above` (a long
/// line cut around another match is listed again). While a pattern lists a
/// new row outside test code and does not itself mention `test`, its rows
/// in test code are named under `tests`; otherwise they are listed. Every
/// listed row keeps the requested context that its block has not already
/// printed. Every match the JSON form lists is listed or named; the JSON
/// payload is left unchanged. The most useful unlisted lines of a file
/// (E17-39, D104) are named under `unlisted`, with `unlisted_more` counting
/// the rest.
fn compact_rows_of(
    payload: &mut Value,
    report: &GrepReport,
    tests: &TestCode,
    context_lines: usize,
) {
    let mut listed = HashSet::<(&str, u32, &str)>::new();
    for (result, value) in report.results.iter().zip(
        payload["results"]
            .as_array_mut()
            .map_or(&mut [][..], Vec::as_mut_slice),
    ) {
        let repeat = |path: &str, line: &GrepLine, listed: &HashSet<(&str, u32, &str)>| {
            listed.contains(&(path, line.line, line.snippet.as_str()))
        };
        let lists_code = result.files.iter().any(|file| {
            file.lines
                .iter()
                .any(|line| !repeat(&file.path, line, &listed) && !tests.contains(&file.path, line))
        });
        let names_tests = lists_code && !result.pattern.to_ascii_lowercase().contains("test");
        for (file, file_value) in result.files.iter().zip(
            value["files"]
                .as_array_mut()
                .map_or(&mut [][..], Vec::as_mut_slice),
        ) {
            let shown = file
                .lines
                .iter()
                .map(|line| {
                    if repeat(&file.path, line, &listed) {
                        Shown::Above
                    } else if names_tests && tests.contains(&file.path, line) {
                        Shown::Test
                    } else {
                        Shown::Listed
                    }
                })
                .collect::<Vec<_>>();
            let named = |kind: Shown| {
                file.lines
                    .iter()
                    .zip(&shown)
                    .filter(|&(_, &shown)| shown == kind)
                    .map(|(line, _)| line.line)
                    .collect::<Vec<_>>()
            };
            let (test_lines, above) = (named(Shown::Test), named(Shown::Above));
            for (line, _) in file
                .lines
                .iter()
                .zip(&shown)
                .filter(|&(_, &shown)| shown == Shown::Listed)
            {
                listed.insert((file.path.as_str(), line.line, line.snippet.as_str()));
            }
            file_value["lines"] = json!(compact_rows(file, &shown, context_lines));
            if !test_lines.is_empty() {
                file_value["tests"] = json!(test_lines);
            }
            if !above.is_empty() {
                file_value["above"] = json!(above);
            }
            // E17-39 (D104): unlisted lines named by number, ascending,
            // with a count of the rest.
            if !file.lines_unlisted.is_empty() {
                let mut named = file.lines_unlisted.clone();
                named.sort_unstable();
                file_value["unlisted"] = json!(named);
                let more = file
                    .matches_total
                    .saturating_sub(file.lines.len())
                    .saturating_sub(named.len());
                if more > 0 {
                    file_value["unlisted_more"] = json!(more);
                }
            }
        }
    }
}

/// One file's listed rows for the text form. A file block's first match row
/// is always tagged (`[-]` for no symbol); a later row is untagged exactly
/// when it is in the same symbol as the match row above it, is not a
/// definition, and its snippet does not start with `[`.
///
/// Context is re-merged over the listed rows only: atlas split it over every
/// kept match, so a row named instead of listed would otherwise take its
/// neighbours' context with it. Each listed row gets up to `context_lines`
/// lines on each side that its block has not printed, stopping before the
/// next listed row; a named line inside that span is shown as context.
fn compact_rows(file: &GrepFile, shown: &[Shown], context_lines: usize) -> Vec<String> {
    // Every line atlas returned for this file: context lines in full and
    // match lines as their snippets. Together they cover every line within
    // `context_lines` of a kept match.
    let mut text = BTreeMap::<u32, &str>::new();
    for line in &file.lines {
        for context in line.before.iter().chain(&line.after) {
            text.insert(context.line, context.text.as_str());
        }
    }
    for line in &file.lines {
        text.entry(line.line).or_insert(line.snippet.as_str());
    }
    let span = u32::try_from(context_lines).unwrap_or(u32::MAX);
    let visible = file
        .lines
        .iter()
        .zip(shown)
        .filter(|&(_, &shown)| shown == Shown::Listed)
        .map(|(line, _)| line)
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    let context_rows = |rows: &mut Vec<String>, from: u32, to: u32| {
        for (number, line) in text.range(from..=to) {
            rows.push(format!("{number}- {line}"));
        }
    };
    // The previous listed match row's symbol in this file block; `None`
    // before the first row, so a block's first row is always tagged.
    let mut previous: Option<Option<&str>> = None;
    let mut printed = 0_u32;
    for (index, line) in visible.iter().enumerate() {
        if span > 0 && line.line > 1 {
            let from = line.line.saturating_sub(span).max(printed + 1).max(1);
            if from < line.line {
                context_rows(&mut rows, from, line.line - 1);
            }
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
        printed = line.line;
        if span > 0 {
            let next = visible.get(index + 1).map_or(u32::MAX, |next| next.line);
            let to = line.line.saturating_add(span).min(next.saturating_sub(1));
            if to > line.line {
                context_rows(&mut rows, line.line + 1, to);
                printed = to;
            }
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
