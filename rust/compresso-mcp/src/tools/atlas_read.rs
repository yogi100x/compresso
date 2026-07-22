use compresso_atlas::{plan_context, read_bounded, repository_stale, search};
use compresso_core::{TokenCounter, operation_identity};
use compresso_store::{Store, SymbolRecord};
use rmcp::model::JsonObject;
use serde_json::{Value, json};
use std::path::Path;

use super::common::{
    MAX_TOKENS, MIN_TOKENS, REINDEX_HINT, ToolFailure, ToolResult, cap_values, checked_arguments,
    language_name, map_atlas_error, navigation_counter, optional_bool, optional_string,
    optional_u32, optional_usize, required_string, resolve_repository, symbol_kind_name,
    symbol_value, token_budget, validate_repo_path,
};

const SEARCH_DEFAULT_LIMIT: usize = 20;
const SEARCH_DEFAULT_TOKENS: usize = 2_000;
const READ_DEFAULT_TOKENS: usize = 2_000;
const CONTEXT_DEFAULT_TOKENS: usize = 4_000;

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
    let mut arguments = checked_arguments(arguments, &["root", "query", "limit", "max_tokens"])?;
    let root = optional_string(&mut arguments, "root", false)?;
    let query = required_string(&mut arguments, "query")?;
    if query.trim().is_empty() || query.contains('\0') {
        return Err(ToolFailure::invalid(
            "argument 'query' must contain searchable text and no NUL byte.",
        ));
    }
    let limit = optional_usize(&mut arguments, "limit", 1, 100)?.unwrap_or(SEARCH_DEFAULT_LIMIT);
    let max_tokens = token_budget(&mut arguments, SEARCH_DEFAULT_TOKENS)?;
    let repository = resolve_repository(store, repository_root, root.as_deref())?;
    let hits = search(store, repository.id, &query, limit).map_err(map_atlas_error)?;
    let hits_total = hits.len();
    let counter = navigation_counter();
    let hit_values = hits
        .into_iter()
        .map(|hit| search_hit_value(&hit.path, &hit.symbol, hit.score))
        .collect::<Vec<_>>();
    let (hits, tokens_used, truncated) = cap_values(hit_values, max_tokens, counter);
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;
    let operation_id = operation_identity().id;

    Ok(json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "query": query,
        "hits": hits,
        "hits_total": hits_total,
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
    let counter = navigation_counter();
    let range = read_range(&repository.root, &path, start_line, end_line, counter)?;
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
    Ok(json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "path": result.path,
        "content": result.content,
        "range": {
            "start_line": result.start_line,
            "end_line": result.end_line
        },
        "file_tokens_total": result.file_tokens_total,
        "tokens_used": result.tokens_used,
        "token_provider": result.token_provider,
        "truncated": result.truncated,
        "elision": result.elision
    }))
}

fn read_range(
    root: &std::path::Path,
    path: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
    counter: &dyn TokenCounter,
) -> Result<Option<(u32, u32)>, ToolFailure> {
    match (start_line, end_line) {
        (None, None) => Ok(None),
        (Some(start_line), Some(end_line)) => Ok(Some((start_line, end_line))),
        (Some(start_line), None) => {
            let full =
                read_bounded(root, path, None, usize::MAX, counter).map_err(map_atlas_error)?;
            Ok(Some((start_line, full.end_line)))
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
    let gate = json!({
        "worth_reading": worth_reading,
        "reason": reason,
        "file_tokens_total": read.file_tokens_total,
        "recommended_budget": recommended_budget,
        "cheaper_alternative": cheaper_alternative
    });
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
    let max_tokens = token_budget(&mut arguments, CONTEXT_DEFAULT_TOKENS)?;
    let repository = resolve_repository(store, repository_root, root.as_deref())?;
    let plan = plan_context(store, repository.id, &task, max_tokens).map_err(map_atlas_error)?;
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;
    let map_excerpt = plan
        .repo_map
        .as_ref()
        .map(|map| map.rendered.clone())
        .unwrap_or_default();
    let outlines = plan
        .outlines
        .iter()
        .map(|outline| {
            json!({
                "path": outline.path,
                "language": outline.language.map(language_name),
                "symbols": outline.symbols.iter().map(symbol_value).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    let bodies = plan
        .symbol_bodies
        .iter()
        .map(|body| {
            json!({
                "path": body.path,
                "symbol": body.symbol.qualified_name,
                "kind": symbol_kind_name(body.symbol.kind),
                "start_line": body.symbol.start_line,
                "end_line": body.symbol.end_line,
                "content": body.content
            })
        })
        .collect::<Vec<_>>();
    let operation_id = operation_identity().id;
    Ok(json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "task": task,
        "plan": {
            "map_excerpt": map_excerpt,
            "outlines": outlines,
            "bodies": bodies,
            "omitted": []
        },
        "stale": stale,
        "reindex_hint": stale.then_some(REINDEX_HINT),
        "tokens_used": plan.tokens_used,
        "token_provider": plan.token_provider,
        "truncated": plan.truncated
    }))
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
