//! The operator-enabled `compresso_edit` tool (E17-32, D97, ADR-0035).
//!
//! The server advertises and serves it only when started with `--edits`;
//! every other tool stays read-only. One call runs once: unlike the read
//! tools, whose text form may be re-fitted by running them again, an edit is
//! applied exactly once and only its report is fitted to `max_tokens`.

use std::fs;
use std::path::{Path, PathBuf};

use compresso_atlas::{
    EDIT_MAX_OPERATIONS, EditBound, EditError, EditLocation, EditOperation, EditReport,
    EditRequest, IndexState, InsertPosition, apply_edits,
};
use compresso_core::{Mode, TokenCounter, TokenProvider, counter_for, operation_identity};
use compresso_store::{OperationRecord, Store};
use rmcp::model::JsonObject;
use serde_json::{Map, Value, json};

use super::ToolSuccess;
use super::common::{
    MAX_TOKENS, ToolFailure, checked_arguments, optional_bool, optional_string, optional_usize,
    resolve_repository,
};

pub(crate) const TOOL_NAME: &str = "compresso_edit";
pub(crate) const DEFAULT_MAX_TOKENS: usize = 2_000;
/// Smallest report budget: the JSON envelope with its exact totals fits.
pub(crate) const MIN_MAX_TOKENS: usize = 300;
/// Halving steps tried before the report is cut to a minimal form.
const FIT_ATTEMPTS: usize = 40;

pub(crate) fn handles(name: &str) -> bool {
    name == TOOL_NAME
}

/// The message a server without `--edits` returns for this tool.
pub(crate) fn unavailable() -> ToolFailure {
    ToolFailure::new(
        "tool_unavailable",
        "Tool 'compresso_edit' is not enabled for this server process; the operator can start the server with `compresso mcp serve --edits` to use it.",
    )
}

/// Applies the call's edits once and returns the report fitted to its
/// `max_tokens` in the requested form, with the text actually sent.
pub(crate) fn run(
    store: &Store,
    repository_root: Option<&Path>,
    enabled: bool,
    mode: Mode,
    arguments: Option<JsonObject>,
    text: bool,
) -> Result<(ToolSuccess, String), ToolFailure> {
    if !enabled {
        return Err(unavailable());
    }
    let mut arguments = checked_arguments(arguments, &["operations", "dry_run", "max_tokens"])?;
    let max_tokens = optional_usize(&mut arguments, "max_tokens", MIN_MAX_TOKENS, MAX_TOKENS)?
        .unwrap_or(DEFAULT_MAX_TOKENS);
    let dry_run = optional_bool(&mut arguments, "dry_run")?.unwrap_or(false);
    let operations = parse_operations(arguments.remove("operations"))?;
    let repository = resolve_repository(store, repository_root, None)?;
    let request = EditRequest {
        operations,
        dry_run,
        protected: protected_paths(store, &repository.root),
    };
    let counter = counter_for(TokenProvider::OpenAiO200k);
    let report = apply_edits(store, &repository.root, &request, counter).map_err(map_edit_error)?;
    let identity = operation_identity();
    let payload = report_payload(&report, &request.operations, &identity.id);
    let (payload, sent) = fit(payload, max_tokens, text, counter);
    Ok((
        ToolSuccess {
            payload,
            operation: OperationRecord {
                id: identity.id,
                session_id: None,
                tool: TOOL_NAME.to_owned(),
                mode,
                observed_tokens: 0,
                applied_tokens: 0,
                compression_overhead: 0,
                fallback_used: false,
                tokens_served: Some(0),
                retrieval: None,
                trace_json: None,
                created_at: identity.created_at,
            },
            compression_trace: None,
        },
        sent,
    ))
}

/// Paths an edit never writes: the Compresso store's database files, and the
/// whole store directory (which also holds install receipts) when it lies
/// inside the repository being edited, for example when `root` is a home
/// directory.
pub fn protected_paths(store: &Store, root: &Path) -> Vec<PathBuf> {
    let Some(database) = store.database_path() else {
        return Vec::new();
    };
    let (Some(directory), Some(name)) = (database.parent(), database.file_name()) else {
        return Vec::new();
    };
    let Ok(directory) = fs::canonicalize(directory) else {
        return Vec::new();
    };
    let name = name.to_string_lossy();
    let mut protected = ["", "-wal", "-shm", "-journal"]
        .iter()
        .map(|suffix| directory.join(format!("{name}{suffix}")))
        .collect::<Vec<_>>();
    let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if directory.starts_with(&root) && directory != root {
        protected.push(directory);
    }
    protected
}

fn invalid(message: impl Into<String>) -> ToolFailure {
    ToolFailure::invalid(message)
}

/// Parses the `operations` argument into edit operations.
pub fn parse_operations_value(value: Option<Value>) -> Result<Vec<EditOperation>, String> {
    parse_operations(value).map_err(|failure| failure.message().to_owned())
}

fn parse_operations(value: Option<Value>) -> Result<Vec<EditOperation>, ToolFailure> {
    let Some(Value::Array(items)) = value else {
        return Err(invalid(format!(
            "argument 'operations' must be an array of 1 to {EDIT_MAX_OPERATIONS} operation objects."
        )));
    };
    if items.is_empty() {
        return Err(invalid(format!(
            "argument 'operations' must hold 1 to {EDIT_MAX_OPERATIONS} operations."
        )));
    }
    // Checked before any item is parsed, with the message the edit itself
    // gives (E17-47): the limit and the count sent.
    if items.len() > EDIT_MAX_OPERATIONS {
        return Err(map_edit_error(EditError::Bound {
            bound: EditBound::Operations,
            limit: EDIT_MAX_OPERATIONS as u64,
            actual: items.len() as u64,
        }));
    }
    items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let Value::Object(object) = item else {
                return Err(invalid(format!(
                    "operation {} must be an object.",
                    index + 1
                )));
            };
            parse_operation(object).map_err(|failure| {
                invalid(format!("operation {}: {}", index + 1, failure.message()))
            })
        })
        .collect()
}

fn parse_operation(mut object: Map<String, Value>) -> Result<EditOperation, ToolFailure> {
    let op = optional_string(&mut object, "op", false)?
        .ok_or_else(|| invalid("missing 'op' (replace, rename, insert, or create)."))?;
    let allowed: &[&str] = match op.as_str() {
        "replace" => &["path", "old", "new", "expected_count", "expected_hash"],
        "rename" => &["old", "new", "path", "expected_count", "allow_existing"],
        "insert" => &["path", "anchor", "text", "position", "expected_hash"],
        "create" => &["path", "content"],
        _ => return Err(invalid("'op' must be replace, rename, insert, or create.")),
    };
    if let Some(unknown) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(format!(
            "unknown field '{unknown}' for {op}; {op} takes {}.",
            allowed.join(", ")
        )));
    }
    let required = |object: &mut Map<String, Value>, name: &'static str, allow_empty| {
        optional_string(object, name, allow_empty)?
            .ok_or_else(|| invalid(format!("missing required string '{name}'.")))
    };
    let count = |object: &mut Map<String, Value>| {
        optional_usize(object, "expected_count", 1, compresso_atlas::EDIT_MAX_SITES)
    };
    Ok(match op.as_str() {
        "replace" => EditOperation::Replace {
            path: required(&mut object, "path", false)?,
            old: required(&mut object, "old", false)?,
            new: required(&mut object, "new", true)?,
            expected_count: count(&mut object)?.unwrap_or(1),
            expected_hash: optional_string(&mut object, "expected_hash", false)?,
        },
        "rename" => EditOperation::Rename {
            old: required(&mut object, "old", false)?,
            new: required(&mut object, "new", false)?,
            path: optional_string(&mut object, "path", false)?,
            expected_count: count(&mut object)?,
            allow_existing: optional_bool(&mut object, "allow_existing")?.unwrap_or(false),
        },
        "create" => EditOperation::Create {
            path: required(&mut object, "path", false)?,
            // An empty file may be created.
            content: required(&mut object, "content", true)?,
        },
        _ => EditOperation::Insert {
            path: required(&mut object, "path", false)?,
            anchor: required(&mut object, "anchor", false)?,
            text: required(&mut object, "text", false)?,
            position: match optional_string(&mut object, "position", false)?.as_deref() {
                None | Some("after") => InsertPosition::After,
                Some("before") => InsertPosition::Before,
                Some(_) => return Err(invalid("'position' must be before or after.")),
            },
            expected_hash: optional_string(&mut object, "expected_hash", false)?,
        },
    })
}

fn location_rows(locations: &[EditLocation]) -> String {
    locations
        .iter()
        .map(|location| format!("\n  {}:{}: {}", location.path, location.line, location.text))
        .collect()
}

/// Maps an edit failure to a stable, actionable tool error.
pub fn edit_failure_message(error: &EditError) -> (&'static str, String) {
    match error {
        EditError::Invalid { reason } => ("invalid_arguments", format!("{reason}.")),
        EditError::NotIndexed => (
            "not_indexed",
            "The repository is not indexed. Run `compresso index`, then retry.".to_owned(),
        ),
        EditError::OutsideRoot { path } => (
            "outside_root",
            format!(
                "The requested path '{path}' resolves outside the approved repository root; nothing was written."
            ),
        ),
        EditError::Refused { .. } => ("edit_refused", format!("{error}; nothing was written.")),
        EditError::CountMismatch { locations, .. } => (
            "count_mismatch",
            if locations.is_empty() {
                format!("{error}.")
            } else {
                format!("{error}. Matches:{}", location_rows(locations))
            },
        ),
        EditError::NoMatches {
            substring_total,
            substring,
            ..
        } => (
            "no_matches",
            if *substring_total == 0 {
                format!("{error}.")
            } else {
                format!(
                    "{error}. {substring_total} substring match(es) inside longer identifiers:{}",
                    location_rows(substring)
                )
            },
        ),
        EditError::NameExists { locations, .. } => (
            "name_exists",
            format!("{error}. Existing:{}", location_rows(locations)),
        ),
        EditError::HashMismatch { actual, .. } => {
            ("hash_mismatch", format!("{error}. Current hash: {actual}."))
        }
        EditError::Bound { .. } | EditError::CreateBound { .. } => {
            ("limit_exceeded", format!("{error}."))
        }
        EditError::Changed { .. } => ("file_changed", format!("{error}. Read it again and retry.")),
        EditError::WriteFailed {
            path,
            restored,
            unrestored,
            backups,
            ..
        } => (
            "write_failed",
            format!(
                "Writing '{path}' failed. Restored to their original content: [{}]. Left with the edit applied (restore failed): [{}]. Original copies of those kept at: [{}]. No other file was changed.",
                restored.join(", "),
                unrestored.join(", "),
                backups.join(", ")
            ),
        ),
        EditError::Read { .. } => ("read_failed", format!("{error}.")),
        EditError::Store(_) | EditError::Atlas(_) => (
            "internal",
            "Compresso could not complete the request because of an internal error.".to_owned(),
        ),
    }
}

fn map_edit_error(error: EditError) -> ToolFailure {
    let (code, message) = edit_failure_message(&error);
    ToolFailure::new(code, message)
}

fn locations_value(locations: &[EditLocation]) -> Value {
    Value::Array(
        locations
            .iter()
            .map(|location| {
                json!({ "path": location.path, "line": location.line, "text": location.text })
            })
            .collect(),
    )
}

/// The JSON payload of one edit report. `operations` is the request the
/// report answers; a rename's names and scope are repeated from it.
pub fn report_payload(
    report: &EditReport,
    operations: &[EditOperation],
    operation_id: &str,
) -> Value {
    let files = report
        .files
        .iter()
        .map(|file| {
            let mut value = json!({
                "path": file.path,
                "sites": file.sites,
                "lines": file.lines,
                "stale": file.stale,
                "line_ending": file.line_ending.as_str(),
                "bom": file.bom,
                "bytes_before": file.bytes_before,
                "bytes_after": file.bytes_after,
                "hash_before": file.hash_before,
                "hash_after": file.hash_after,
                "verified": file.verified
            });
            if file.created {
                value["created"] = json!(true);
            }
            value
        })
        .collect::<Vec<_>>();
    let operations = report
        .operations
        .iter()
        .zip(operations)
        .map(|(operation, requested)| {
            let mut value = json!({
                "op": operation.kind,
                "sites": operation.sites,
                "files": operation.files
            });
            let object = value.as_object_mut().expect("object literal");
            match requested {
                EditOperation::Rename { old, new, .. } => {
                    object.insert("old".to_owned(), json!(old));
                    object.insert("new".to_owned(), json!(new));
                }
                EditOperation::Replace { path, .. }
                | EditOperation::Insert { path, .. }
                | EditOperation::Create { path, .. } => {
                    object.insert("path".to_owned(), json!(path));
                }
            }
            // An identical replace (E17-47): its count held, nothing changed.
            if operation.unchanged {
                object.insert("unchanged".to_owned(), json!(true));
                object.insert("matched".to_owned(), json!(operation.matched));
            }
            if let Some(rename) = &operation.rename {
                object.insert("scope".to_owned(), json!(rename.scope));
                object.insert("files_searched".to_owned(), json!(rename.files_searched));
                object.insert(
                    "files_skipped".to_owned(),
                    Value::Array(
                        rename
                            .files_skipped
                            .iter()
                            .map(|skipped| {
                                json!({ "path": skipped.path, "reason": skipped.reason.as_str() })
                            })
                            .collect(),
                    ),
                );
                object.insert(
                    "remaining_whole_total".to_owned(),
                    json!(rename.remaining_whole_total),
                );
                object.insert(
                    "remaining_whole".to_owned(),
                    locations_value(&rename.remaining_whole),
                );
                object.insert(
                    "remaining_substring_total".to_owned(),
                    json!(rename.remaining_substring_total),
                );
                object.insert(
                    "remaining_substring".to_owned(),
                    locations_value(&rename.remaining_substring),
                );
                object.insert(
                    "remaining_adjacent_total".to_owned(),
                    json!(rename.remaining_adjacent_total),
                );
                object.insert(
                    "remaining_adjacent".to_owned(),
                    locations_value(&rename.remaining_adjacent),
                );
                object.insert(
                    "files_skipped_total".to_owned(),
                    json!(rename.files_skipped.len()),
                );
                object.insert(
                    "new_name_preexisting".to_owned(),
                    json!(rename.new_name_preexisting),
                );
                // A scoped rename also names what it left outside its
                // scope (E17-36, D101); an unscoped one is unchanged.
                if rename.scope.is_some() {
                    object.insert(
                        "outside_scope_whole_total".to_owned(),
                        json!(rename.outside_whole_total),
                    );
                    object.insert(
                        "outside_scope_whole".to_owned(),
                        locations_value(&rename.outside_whole),
                    );
                    object.insert(
                        "outside_scope_unsearched_total".to_owned(),
                        json!(rename.outside_unsearched_total),
                    );
                    object.insert(
                        "outside_scope_unsearched".to_owned(),
                        json!(rename.outside_unsearched),
                    );
                }
                object.insert("complete".to_owned(), json!(rename.complete));
            }
            value
        })
        .collect::<Vec<_>>();
    let renames = report
        .operations
        .iter()
        .filter_map(|operation| operation.rename.as_ref());
    let outside = renames
        .clone()
        .map(|rename| rename.outside_whole_total)
        .sum::<usize>();
    let mut payload = json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "dry_run": report.dry_run,
        "files_total": report.files.len(),
        "sites_total": report.files.iter().map(|file| file.sites).sum::<usize>(),
        // Call-level safety totals: never dropped when the report is cut.
        "incomplete_operations": renames.clone().filter(|rename| !rename.complete).count(),
        "remaining_whole_total": renames.clone().map(|rename| rename.remaining_whole_total).sum::<usize>(),
        "remaining_substring_total": renames.clone().map(|rename| rename.remaining_substring_total).sum::<usize>(),
        "remaining_adjacent_total": renames.clone().map(|rename| rename.remaining_adjacent_total).sum::<usize>(),
        "files_skipped_total": renames.clone().map(|rename| rename.files_skipped.len()).sum::<usize>(),
        "index_refreshed": report.index_refreshed,
        "unverified_files": if report.dry_run { 0 } else { report.files.iter().filter(|file| !file.verified).count() },
        "files": files,
        "operations": operations,
        "diff": report.diff,
        "diff_truncated": report.diff_truncated,
        "bytes_written": report.bytes_written,
        "index": match report.index {
            IndexState::NotRun => "not_run",
            IndexState::Fresh => "fresh",
            IndexState::Stale => "stale",
        },
        "truncated": false
    });
    if outside > 0 {
        payload["outside_scope_whole_total"] = json!(outside);
    }
    // Call-level totals (E17-47), present only when not zero so earlier
    // reports are unchanged; no report cut drops a top-level total.
    let created = report.files.iter().filter(|file| file.created).count();
    if created > 0 {
        payload["files_created"] = json!(created);
    }
    // E17-49: directories made for created files (for a dry run, the ones
    // it would make), present only when not empty; at most 32 per call.
    if !report.directories_created.is_empty() {
        payload["directories_created"] = json!(report.directories_created);
    }
    let unchanged = report
        .operations
        .iter()
        .filter(|operation| operation.unchanged)
        .count();
    if unchanged > 0 {
        payload["unchanged_operations"] = json!(unchanged);
    }
    payload
}

fn render(payload: &Value, text: bool) -> String {
    if text {
        super::text::render(TOOL_NAME, payload).unwrap_or_default()
    } else {
        serde_json::to_string(payload).unwrap_or_default()
    }
}

/// The longest list in the report that can still shrink, as a JSON pointer.
fn longest_list(payload: &Value) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    let mut consider = |pointer: String, length: usize| {
        if length > 0 && best.as_ref().is_none_or(|(longest, _)| length > *longest) {
            best = Some((length, pointer));
        }
    };
    for (index, file) in payload["files"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        consider(
            format!("/files/{index}/lines"),
            file["lines"].as_array().map_or(0, Vec::len),
        );
    }
    for (index, operation) in payload["operations"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        for key in [
            "remaining_substring",
            "remaining_adjacent",
            "remaining_whole",
            "outside_scope_whole",
            "outside_scope_unsearched",
            "files_skipped",
            "files",
        ] {
            consider(
                format!("/operations/{index}/{key}"),
                operation[key].as_array().map_or(0, Vec::len),
            );
        }
    }
    best.map(|(_, pointer)| pointer)
}

/// Shrinks the report until its rendering fits `max_tokens`: the diff first
/// (whole lines), then the longest location list, halving each time. Totals
/// stay exact, and `truncated` says something was left out.
pub fn fit(
    mut payload: Value,
    max_tokens: usize,
    text: bool,
    counter: &dyn TokenCounter,
) -> (Value, String) {
    for _ in 0..FIT_ATTEMPTS {
        let rendered = render(&payload, text);
        if counter.count(&rendered) <= max_tokens {
            return (payload, rendered);
        }
        payload["truncated"] = Value::Bool(true);
        if let Some(diff) = payload["diff"].as_str().filter(|diff| !diff.is_empty()) {
            let lines = diff.split_inclusive('\n').collect::<Vec<_>>();
            let kept = lines[..lines.len() / 2].concat();
            payload["diff"] = Value::String(kept);
            payload["diff_truncated"] = Value::Bool(true);
            continue;
        }
        let Some(pointer) = longest_list(&payload) else {
            break;
        };
        if let Some(Value::Array(list)) = payload.pointer_mut(&pointer) {
            list.truncate(list.len() / 2);
        }
    }
    // Still too long (many files or operations): keep only the totals,
    // first per operation, then for the call as a whole.
    payload["truncated"] = Value::Bool(true);
    payload["diff"] = Value::Null;
    if let Some(files) = payload["files"].as_array_mut() {
        files.clear();
    }
    if let Some(operations) = payload["operations"].as_array_mut() {
        for operation in operations.iter_mut() {
            if let Some(object) = operation.as_object_mut() {
                object.retain(|key, _| {
                    matches!(
                        key.as_str(),
                        "op" | "sites"
                            | "unchanged"
                            | "matched"
                            | "old"
                            | "new"
                            | "path"
                            | "complete"
                            | "files_skipped_total"
                            | "remaining_whole_total"
                            | "remaining_substring_total"
                            | "remaining_adjacent_total"
                    )
                });
            }
        }
    }
    let rendered = render(&payload, text);
    if counter.count(&rendered) <= max_tokens {
        return (payload, rendered);
    }
    if let Some(operations) = payload["operations"].as_array_mut() {
        operations.clear();
    }
    let rendered = render(&payload, text);
    if !text || counter.count(&rendered) <= max_tokens {
        return (payload, rendered);
    }
    // Text only: the longest whole-line prefix that fits, with the notice.
    let notice = "[report cut to max_tokens; totals are exact]\n";
    let mut kept = String::new();
    for line in rendered.split_inclusive('\n') {
        if counter.count(&format!("{kept}{line}{notice}")) > max_tokens {
            break;
        }
        kept.push_str(line);
    }
    kept.push_str(notice);
    (payload, kept)
}
