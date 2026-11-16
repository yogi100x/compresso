use compresso_atlas::{
    SearchHit, SearchOptions, file_line_count, is_scanner_excluded, plan_context, read_bounded,
    repository_stale, search_with_diagnostics, validate_search_query,
};
use compresso_core::{TokenCounter, operation_identity};
use compresso_store::{Store, SymbolRecord};
use rmcp::model::JsonObject;
use serde_json::{Value, json};
use std::path::Path;

use super::common::{
    MAX_TOKENS, MIN_TOKENS, REINDEX_HINT, ToolFailure, ToolResult, cap_values, checked_arguments,
    map_atlas_error, navigation_counter, optional_bool, optional_string, optional_u32,
    optional_usize, required_string, resolve_repository, symbol_kind_name, token_budget,
    validate_repo_path,
};

const SEARCH_DEFAULT_LIMIT: usize = 8;
const SEARCH_DEFAULT_TOKENS: usize = 500;
/// Most dropped hits that are still reported by location.
const SEARCH_MAX_TRUNCATED_LOCATIONS: usize = 50;
const READ_DEFAULT_TOKENS: usize = 2_000;
const CONTEXT_INITIAL_TOKENS: usize = 800;
const CONTEXT_DEFAULT_CEILING: usize = 4_000;
const CONTEXT_MAX_TOKENS: usize = 4_000;
const CONTEXT_BUDGET_STEPS: [usize; 4] = [800, 1_600, 3_200, 4_000];
const CONTEXT_EXPANSION_REASON: &str = "insufficient_evidence_coverage_at_lower_budget";

pub(crate) fn handles(name: &str) -> bool {
    matches!(
        name,
        "compresso_search" | "compresso_read" | "compresso_context"
    )
}

pub(crate) fn dispatch(
    store: &Store,
    repository_root: Option<&Path>,
    name: &str,
    arguments: Option<JsonObject>,
) -> ToolResult {
    match name {
        "compresso_search" => search_tool(store, repository_root, arguments),
        "compresso_read" => read_tool(store, repository_root, arguments),
        "compresso_context" => context_tool(store, repository_root, arguments),
        _ => Err(ToolFailure::internal()),
    }
}

fn search_tool(
    store: &Store,
    repository_root: Option<&Path>,
    arguments: Option<JsonObject>,
) -> ToolResult {
    let mut arguments =
        checked_arguments(arguments, &["root", "query", "path", "limit", "max_tokens"])?;
    let root = optional_string(&mut arguments, "root", false)?;
    let query = optional_string(&mut arguments, "query", true)?
        .ok_or_else(|| ToolFailure::invalid("missing required string argument 'query'."))?;
    validate_search_query(&query).map_err(|error| map_atlas_error(error.into()))?;
    let path = optional_string(&mut arguments, "path", false)?;
    let limit = optional_usize(&mut arguments, "limit", 1, 100)?.unwrap_or(SEARCH_DEFAULT_LIMIT);
    let max_tokens = token_budget(&mut arguments, SEARCH_DEFAULT_TOKENS)?;
    let repository = resolve_repository(store, repository_root, root.as_deref())?;
    let report = search_with_diagnostics(
        store,
        repository.id,
        &query,
        SearchOptions {
            limit,
            path: path.as_deref(),
        },
    )
    .map_err(map_atlas_error)?;
    let hits_total = report.hits.len();
    let counter = navigation_counter();
    let capped = cap_search_hits(&report.hits, max_tokens, counter);
    let (hits, tokens_used, truncated) = (capped.hits, capped.tokens_used, capped.truncated);
    let diagnostics = report
        .diagnostics
        .iter()
        .map(|diagnostic| {
            json!({
                "code": diagnostic.code.as_str(),
                "severity": "info",
                "message": diagnostic.message
            })
        })
        .collect::<Vec<_>>();
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;
    let operation_id = operation_identity().id;

    Ok(json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "query": query,
        "path": path,
        "hits": hits,
        "hits_total": hits_total,
        "truncated_hits": capped.truncated_hits,
        "truncated_hits_omitted": capped.truncated_hits_omitted,
        "diagnostics": diagnostics,
        "suggestion": (hits_total == 0).then_some(if stale {
            "No indexed symbols matched. Reindex the repository, then retry or broaden the query."
        } else {
            "No indexed symbols matched. Broaden the query."
        }),
        "stale": stale,
        "reindex_hint": stale.then_some(REINDEX_HINT),
        "tokens_used": tokens_used,
        "token_provider": counter.provider(),
        "truncated": truncated
    }))
}

fn read_tool(
    store: &Store,
    repository_root: Option<&Path>,
    arguments: Option<JsonObject>,
) -> ToolResult {
    let mut arguments = checked_arguments(
        arguments,
        &[
            "root",
            "path",
            "start_line",
            "end_line",
            "max_tokens",
            "gate",
        ],
    )?;
    let root = optional_string(&mut arguments, "root", false)?;
    let path = required_string(&mut arguments, "path")?;
    let start_line = optional_u32(&mut arguments, "start_line", 1, u32::MAX)?;
    let end_line = optional_u32(&mut arguments, "end_line", 1, u32::MAX)?;
    let max_tokens = token_budget(&mut arguments, READ_DEFAULT_TOKENS)?;
    let gate = optional_bool(&mut arguments, "gate")?.unwrap_or(false);
    if end_line.is_some() && start_line.is_none() {
        return Err(ToolFailure::invalid(
            "argument 'end_line' requires 'start_line'.",
        ));
    }
    if let (Some(start_line), Some(end_line)) = (start_line, end_line)
        && end_line < start_line
    {
        return Err(ToolFailure::invalid(
            "argument 'end_line' must be greater than or equal to 'start_line'.",
        ));
    }

    let repository = resolve_repository(store, repository_root, root.as_deref())?;
    validate_repo_path(&repository.root, &path)?;
    // A direct read follows the scanner's exclusion policy: content the index
    // was never allowed to hold is not served either. An indexed file is
    // always readable, so an index built without ignore rules still works.
    if is_scanner_excluded(&repository.root, Path::new(&path))
        && store
            .indexed_file(repository.id, &path)
            .map_err(|_| ToolFailure::internal())?
            .is_none()
    {
        return Err(ToolFailure::new(
            "not_found",
            "The requested file was not found on disk.",
        ));
    }
    let counter = navigation_counter();
    let range = read_range(&repository.root, &path, start_line, end_line)?;
    if gate {
        return read_gate(
            store,
            repository.id,
            &repository.root,
            path,
            range,
            max_tokens,
            counter,
        );
    }

    let result = read_bounded(&repository.root, &path, range, max_tokens, counter)
        .map_err(map_atlas_error)?;
    let operation_id = operation_identity().id;
    let file_tokens_estimated = result.file_tokens_estimated;
    let mut payload = json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "path": result.path,
        "content": result.content,
        "range": {
            "start_line": result.start_line,
            "end_line": result.end_line
        },
        "resolved_range": {
            "start_line": result.start_line,
            "end_line": result.selected_end_line
        },
        "file_lines_total": result.file_lines_total,
        "range_clamped": result.range_clamped,
        "file_tokens_total": result.file_tokens_total,
        "tokens_used": result.tokens_used,
        "token_provider": result.token_provider,
        "truncated": result.truncated,
        "elision": result.elision
    });
    if file_tokens_estimated {
        payload["file_tokens_total_estimated"] = serde_json::Value::Bool(true);
    }
    Ok(payload)
}

fn read_range(
    root: &std::path::Path,
    path: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
) -> Result<Option<(u32, u32)>, ToolFailure> {
    match (start_line, end_line) {
        (None, None) => Ok(None),
        (Some(start_line), Some(end_line)) => Ok(Some((start_line, end_line))),
        (Some(start_line), None) => {
            let total_lines = file_line_count(root, path).map_err(map_atlas_error)?;
            Ok(Some((start_line, total_lines)))
        }
        (None, Some(_)) => Err(ToolFailure::invalid(
            "argument 'end_line' requires 'start_line'.",
        )),
    }
}

fn read_gate(
    store: &Store,
    repo: i64,
    root: &std::path::Path,
    path: String,
    range: Option<(u32, u32)>,
    max_tokens: usize,
    counter: &dyn TokenCounter,
) -> ToolResult {
    let read = read_bounded(root, &path, range, usize::MAX, counter).map_err(map_atlas_error)?;
    let symbols = if store
        .indexed_file(repo, &path)
        .map_err(|_| ToolFailure::internal())?
        .is_some()
    {
        store
            .symbols_in_file(repo, &path)
            .map_err(|_| ToolFailure::internal())?
    } else {
        Vec::new()
    };
    let fits_requested_budget = read.file_tokens_total <= max_tokens;
    let worth_reading = fits_requested_budget || !symbols.is_empty();
    let reason = if fits_requested_budget {
        format!(
            "The file fits the requested {max_tokens}-token budget ({} estimated tokens total).",
            read.file_tokens_total
        )
    } else if symbols.is_empty() {
        format!(
            "The file exceeds the requested budget and has no indexed symbol outline to establish structural relevance ({} estimated tokens total).",
            read.file_tokens_total
        )
    } else {
        format!(
            "The file defines {} indexed symbols and is structurally relevant, but its {} estimated tokens exceed the requested budget.",
            symbols.len(),
            read.file_tokens_total
        )
    };
    let recommended_budget = if read.file_tokens_total <= max_tokens {
        read.file_tokens_total.clamp(MIN_TOKENS, MAX_TOKENS)
    } else {
        max_tokens
            .saturating_add(max_tokens / 10)
            .min(read.file_tokens_total)
            .clamp(MIN_TOKENS, MAX_TOKENS)
    };
    let cheaper_alternative = symbols.first().map_or_else(
        || "compresso_read with a narrower start_line and end_line range".to_owned(),
        |symbol| {
            format!(
                "compresso_outline then compresso_unfold {}",
                symbol.qualified_name
            )
        },
    );
    let mut gate = json!({
        "worth_reading": worth_reading,
        "reason": reason,
        "file_tokens_total": read.file_tokens_total,
        "recommended_budget": recommended_budget,
        "cheaper_alternative": cheaper_alternative
    });
    if read.file_tokens_estimated {
        gate["file_tokens_total_estimated"] = serde_json::Value::Bool(true);
    }
    let tokens_used =
        counter.count(&serde_json::to_string(&gate).map_err(|_| ToolFailure::internal())?);
    let operation_id = operation_identity().id;
    Ok(json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "path": path,
        "gate": gate,
        "tokens_used": tokens_used,
        "token_provider": counter.provider()
    }))
}

fn context_tool(
    store: &Store,
    repository_root: Option<&Path>,
    arguments: Option<JsonObject>,
) -> ToolResult {
    let mut arguments = checked_arguments(arguments, &["root", "task", "max_tokens"])?;
    let root = optional_string(&mut arguments, "root", false)?;
    let task = required_string(&mut arguments, "task")?;
    if task.trim().chars().count() < 3 || task.contains('\0') {
        return Err(ToolFailure::invalid(
            "argument 'task' must contain at least three characters and no NUL byte.",
        ));
    }
    let requested_ceiling = token_budget(&mut arguments, CONTEXT_DEFAULT_CEILING)?;
    let budget_ceiling = requested_ceiling.min(CONTEXT_MAX_TOKENS);
    let budget_clamped = requested_ceiling > budget_ceiling;
    let repository = resolve_repository(store, repository_root, root.as_deref())?;
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;
    let operation_id = operation_identity().id;
    let counter = navigation_counter();
    let mut expanded = false;
    for budget_applied in context_budget_steps(budget_ceiling) {
        let payload = context_payload_for_budget(
            store,
            repository.id,
            &task,
            &operation_id,
            stale,
            budget_applied,
            budget_ceiling,
            budget_clamped,
            expanded,
            counter,
        )?;
        let targeted_follow_up_required =
            payload["targeted_follow_up_required"].as_bool() == Some(true);
        // A larger budget only helps when candidate files did not fit. A pack
        // whose evidence misses the task's concepts is not improved by size.
        let files_missing = payload["evidence_coverage"]["files_included"].as_u64()
            < payload["evidence_coverage"]["files_required"].as_u64();
        if !targeted_follow_up_required || !files_missing || budget_applied == budget_ceiling {
            return Ok(payload);
        }
        expanded = true;
    }
    Err(ToolFailure::internal())
}

#[allow(clippy::too_many_arguments)]
fn context_payload_for_budget(
    store: &Store,
    repository_id: i64,
    task: &str,
    operation_id: &str,
    stale: bool,
    budget_applied: usize,
    budget_ceiling: usize,
    budget_clamped: bool,
    budget_expanded: bool,
    counter: &dyn TokenCounter,
) -> ToolResult {
    let mut planner_budget = budget_applied.saturating_mul(4) / 5;
    loop {
        let plan =
            plan_context(store, repository_id, task, planner_budget).map_err(map_atlas_error)?;
        let mut payload = compact_context_payload(
            &plan,
            operation_id,
            stale,
            budget_applied,
            budget_ceiling,
            budget_clamped,
            budget_expanded,
        );
        let telemetry = payload
            .as_object_mut()
            .and_then(|object| object.remove("_compresso_telemetry"));
        let payload_tokens = stabilize_payload_tokens(&mut payload, counter)?;
        if let (Some(object), Some(telemetry)) = (payload.as_object_mut(), telemetry) {
            object.insert("_compresso_telemetry".to_owned(), telemetry);
        }
        if payload_tokens <= budget_applied || planner_budget == 0 {
            return Ok(payload);
        }
        let excess = payload_tokens.saturating_sub(budget_applied);
        planner_budget = planner_budget.saturating_sub(excess.saturating_add(16));
    }
}

fn context_budget_steps(budget_ceiling: usize) -> Vec<usize> {
    let mut steps = vec![budget_ceiling.min(CONTEXT_INITIAL_TOKENS)];
    steps.extend(
        CONTEXT_BUDGET_STEPS
            .into_iter()
            .filter(|budget| *budget > CONTEXT_INITIAL_TOKENS && *budget <= budget_ceiling),
    );
    if steps.last().copied() != Some(budget_ceiling) {
        steps.push(budget_ceiling);
    }
    steps
}

fn compact_context_payload(
    plan: &compresso_atlas::ContextPlan,
    operation_id: &str,
    stale: bool,
    budget_applied: usize,
    budget_ceiling: usize,
    budget_clamped: bool,
    budget_expanded: bool,
) -> Value {
    let retrieval_status = if plan.matches_total == 0 {
        "no_matches"
    } else {
        "matched"
    };
    let evidence_files_required = plan.evidence_files_total;
    let files_ready =
        evidence_files_required > 0 && plan.evidence_files_included >= evidence_files_required;
    let concepts_ready = plan.concept_coverage_sufficient();
    let evidence_ready = files_ready && concepts_ready;
    let broad_follow_up_required = plan.matches_total == 0 || stale;
    let targeted_follow_up_required =
        !broad_follow_up_required && plan.matches_total > 0 && !evidence_ready;
    let task_pack_ready = !broad_follow_up_required && !targeted_follow_up_required;
    let context_sufficiency = if broad_follow_up_required {
        "follow_up_needed"
    } else if targeted_follow_up_required {
        "targeted_follow_up_needed"
    } else {
        "task_pack_ready"
    };
    let next_action = if stale {
        "reindex_then_retry"
    } else if plan.matches_total == 0 {
        "broaden_task"
    } else if targeted_follow_up_required {
        "targeted_follow_up"
    } else {
        "answer_from_pack"
    };
    let targeted_follow_up_limit = if targeted_follow_up_required { 2 } else { 0 };
    let retrieval_note = if plan.matches_total == 0 {
        Some(
            "No indexed symbols matched the derived task queries. This is not proof that the repository contains no relevant implementation.",
        )
    } else if targeted_follow_up_required && !concepts_ready {
        Some(
            "The evidence found does not mention most of the task's terms (see task_coverage.uncovered). Use at most two precise Compresso follow-ups naming exact identifiers; do not repeat broad retrieval.",
        )
    } else if targeted_follow_up_required {
        Some(
            "The pack could not fit enough distinct evidence files. Use at most two precise Compresso follow-ups; do not repeat broad retrieval.",
        )
    } else {
        None
    };
    json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "retrieval_status": retrieval_status,
        "retrieval_note": retrieval_note,
        "context_sufficiency": context_sufficiency,
        "next_action": next_action,
        "broad_follow_up_required": broad_follow_up_required,
        "targeted_follow_up_required": targeted_follow_up_required,
        "targeted_follow_up_limit": targeted_follow_up_limit,
        "evidence_coverage": {
            "files_included": plan.evidence_files_included,
            "files_required": evidence_files_required,
            "candidate_files": plan.evidence_files_total
        },
        "task_coverage": {
            "concepts_covered": plan.task_concepts_covered,
            "concepts_total": plan.task_concepts_total,
            "uncovered": &plan.uncovered_concepts
        },
        "matches_total": plan.matches_total,
        "content_pack": plan.rendered,
        "stale": stale,
        "reindex_hint": stale.then_some(REINDEX_HINT),
        "budget_applied": budget_applied,
        "budget_ceiling": budget_ceiling,
        "budget_expanded": budget_expanded,
        "budget_expansion_reason": budget_expanded.then_some(CONTEXT_EXPANSION_REASON),
        "budget_clamped": budget_clamped,
        "tokens_used": 0,
        "token_provider": plan.token_provider,
        "truncated": plan.truncated,
        "_compresso_telemetry": {
            "estimated_raw_source_tokens": if task_pack_ready {
                plan.estimated_raw_source_tokens
            } else {
                0
            },
            "modeled_follow_up_calls_avoided": if task_pack_ready {
                plan.evidence_files_included
            } else {
                0
            }
        }
    })
}

fn stabilize_payload_tokens(
    payload: &mut Value,
    counter: &dyn TokenCounter,
) -> Result<usize, ToolFailure> {
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

/// Search hits fitted to a token budget, with locations for the hits that were cut.
struct CappedSearchHits {
    hits: Vec<Value>,
    /// `path` and line range of dropped hits, in rank order, within the budget.
    truncated_hits: Vec<Value>,
    /// Dropped hits whose location did not fit the budget either.
    truncated_hits_omitted: usize,
    tokens_used: usize,
    truncated: bool,
}

/// Keeps as many full hits as fit, then spends what is left of the budget on
/// the line ranges of the dropped hits so they can be read directly.
///
/// When hits were dropped and not even one location fits, the last full hit
/// is exchanged for locations, so truncation is never reported without them.
fn cap_search_hits(
    hits: &[SearchHit],
    max_tokens: usize,
    counter: &dyn TokenCounter,
) -> CappedSearchHits {
    let values = hits
        .iter()
        .map(|hit| search_hit_value(&hit.path, &hit.symbol, hit.score))
        .collect::<Vec<_>>();
    let (mut kept, mut hit_tokens, truncated) = cap_values(values, max_tokens, counter);
    if !truncated {
        return CappedSearchHits {
            hits: kept,
            truncated_hits: Vec::new(),
            truncated_hits_omitted: 0,
            tokens_used: hit_tokens,
            truncated,
        };
    }

    let mut locations = truncated_locations(&hits[kept.len()..], max_tokens - hit_tokens, counter);
    if locations.0.is_empty() && kept.len() > 1 {
        kept.pop();
        hit_tokens = counter.count(&serde_json::to_string(&kept).unwrap_or_default());
        locations = truncated_locations(&hits[kept.len()..], max_tokens - hit_tokens, counter);
    }
    let (truncated_hits, location_tokens) = locations;
    CappedSearchHits {
        truncated_hits_omitted: hits.len() - kept.len() - truncated_hits.len(),
        hits: kept,
        truncated_hits,
        tokens_used: hit_tokens + location_tokens,
        truncated,
    }
}

/// Renders dropped-hit locations in rank order until `budget` tokens are spent.
fn truncated_locations(
    dropped: &[SearchHit],
    budget: usize,
    counter: &dyn TokenCounter,
) -> (Vec<Value>, usize) {
    let mut locations = Vec::new();
    let mut tokens = 0;
    for hit in dropped.iter().take(SEARCH_MAX_TRUNCATED_LOCATIONS) {
        locations.push(json!({
            "path": hit.path,
            "start_line": hit.symbol.start_line,
            "end_line": hit.symbol.end_line
        }));
        let rendered = counter.count(&serde_json::to_string(&locations).unwrap_or_default());
        if rendered > budget {
            locations.pop();
            break;
        }
        tokens = rendered;
    }
    (locations, tokens)
}

fn search_hit_value(path: &str, symbol: &SymbolRecord, score: f64) -> Value {
    json!({
        "path": path,
        "symbol": symbol.qualified_name,
        "qualified_name": symbol.qualified_name,
        "kind": symbol_kind_name(symbol.kind),
        "signature": symbol.signature,
        "start_line": symbol.start_line,
        "end_line": symbol.end_line,
        "score": score,
        "token_estimate": symbol.token_estimate
    })
}
