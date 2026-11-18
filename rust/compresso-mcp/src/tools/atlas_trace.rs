//! `compresso_trace`: callers, callees, and call paths in one bounded call (D90).

use std::path::Path;

use compresso_atlas::{
    AtlasError, QueryError, TraceCallSite, TraceHop, TraceNeighbor, TraceOptions, TraceReport,
    TraceResult, TraceSymbol, repository_stale, trace,
};
use compresso_core::{TokenCounter, operation_identity};
use compresso_store::Store;
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::Form;
use super::common::{
    REINDEX_HINT, ToolFailure, ToolResult, checked_arguments, map_atlas_error, navigation_counter,
    optional_string, optional_u32, optional_usize, resolve_repository, symbol_kind_name,
    token_budget, with_budget,
};
use super::text;

pub(crate) const TOOL_NAME: &str = "compresso_trace";
const TRACE_DEFAULT_TOKENS: usize = 4_000;
const MAX_SPEC_BYTES: usize = 512;

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
            "from",
            "to",
            "max_depth",
            "max_paths",
            "max_results",
            "max_tokens",
        ],
    )?;
    let root = optional_string(&mut arguments, "root", false)?;
    let from = optional_string(&mut arguments, "from", false)?;
    let to = optional_string(&mut arguments, "to", false)?;
    if from.is_none() && to.is_none() {
        return Err(ToolFailure::invalid(
            "name 'to' for callers, 'from' for callees, or both for call paths.",
        ));
    }
    if [&from, &to]
        .into_iter()
        .flatten()
        .any(|spec| spec.len() > MAX_SPEC_BYTES || spec.contains('\0'))
    {
        return Err(ToolFailure::invalid(
            "arguments 'from' and 'to' must be at most 512 bytes without NUL.",
        ));
    }
    let max_depth = optional_u32(&mut arguments, "max_depth", 1, 8)?
        .map(|depth| u8::try_from(depth).unwrap_or(8));
    let max_paths = optional_usize(&mut arguments, "max_paths", 1, 10)?.unwrap_or(3);
    let max_results = optional_usize(&mut arguments, "max_results", 1, 500)?.unwrap_or(50);
    let max_tokens = token_budget(&mut arguments, TRACE_DEFAULT_TOKENS)?;
    let repository = resolve_repository(store, repository_root, root.as_deref())?;

    let report = trace(
        store,
        repository.id,
        from.as_deref(),
        to.as_deref(),
        TraceOptions {
            max_depth,
            max_paths,
            max_results,
            ..TraceOptions::default()
        },
    )
    .map_err(map_trace_error)?;
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;
    let payload = render(
        report,
        from,
        to,
        stale,
        max_tokens,
        navigation_counter(),
        form,
    );
    Ok(with_budget(payload, max_tokens))
}

fn map_trace_error(error: AtlasError) -> ToolFailure {
    match error {
        AtlasError::Query(QueryError::InvalidTraceOptions { reason }) => {
            ToolFailure::invalid(format!("{reason}."))
        }
        error => map_atlas_error(error),
    }
}

fn render(
    report: TraceReport,
    from: Option<String>,
    to: Option<String>,
    stale: bool,
    max_tokens: usize,
    counter: &dyn TokenCounter,
    form: Form,
) -> Value {
    let (status, key, values, extra) = match &report.result {
        TraceResult::Paths(paths) => (
            "paths",
            "paths",
            paths
                .iter()
                .map(|path| Value::Array(path.iter().map(hop_value).collect()))
                .collect(),
            None,
        ),
        TraceResult::NoPath => ("no_path", "paths", Vec::new(), None),
        TraceResult::Callers(entries) => (
            "callers",
            "callers",
            entries.iter().map(neighbor_value).collect(),
            None,
        ),
        TraceResult::Callees(entries) => (
            "callees",
            "callees",
            entries.iter().map(neighbor_value).collect(),
            None,
        ),
        TraceResult::Ambiguous {
            end,
            candidates,
            total,
        } => (
            "ambiguous",
            "candidates",
            candidates.iter().map(symbol_value).collect(),
            Some(json!({ "end": end.as_str(), "candidates_total": total })),
        ),
        TraceResult::NotFound { end, name } => (
            "not_found",
            "candidates",
            Vec::new(),
            Some(json!({ "end": end.as_str(), "name": name })),
        ),
        TraceResult::CallGraphMissing => ("reindex_required", "paths", Vec::new(), None),
    };
    let row: fn(&Value) -> String = match key {
        "paths" => text::path_rows,
        "candidates" => text::trace_candidate_row,
        _ => text::neighbor_row,
    };
    let (values, list_tokens, budget_truncated) = form.cap(values, max_tokens, counter, row);
    let unfollowed = report
        .unfollowed
        .iter()
        .map(|call| {
            json!({
                "caller": call.caller,
                "call": call.callee,
                "call_site": site(&call.call_site),
                "status": call.status,
                "candidates": call.candidates,
            })
        })
        .collect::<Vec<_>>();
    // The unfollowed calls share the token budget with the main list; any
    // that do not fit are still counted in `unfollowed_total`.
    let (unfollowed, unfollowed_tokens, _) = if unfollowed.is_empty() {
        (unfollowed, 0, false)
    } else {
        form.cap(
            unfollowed,
            max_tokens.saturating_sub(list_tokens),
            counter,
            text::unfollowed_row,
        )
    };
    let tokens_used = list_tokens + unfollowed_tokens;
    let reindex_required = matches!(report.result, TraceResult::CallGraphMissing);
    let mut payload = json!({
        "schema_version": 1,
        "operation_id": operation_identity().id,
        "status": status,
        "from": from,
        "to": to,
        key: values,
        "total": report.total,
        "unfollowed": unfollowed,
        "unfollowed_total": report.unfollowed_total,
        "nodes_visited": report.nodes_visited,
        "depth_reached": report.depth_reached,
        "max_depth": report.max_depth,
        "depth_limited": report.depth_limited,
        "truncated": report.truncated || budget_truncated,
        "complete": report.complete && !budget_truncated,
        "stale": stale,
        "reindex_hint": (stale || reindex_required).then_some(REINDEX_HINT),
        "tokens_used": tokens_used,
        "token_provider": counter.provider()
    });
    if let (Some(extra), Some(object)) = (extra, payload.as_object_mut())
        && let Some(extra) = extra.as_object()
    {
        for (name, value) in extra {
            object.insert(name.clone(), value.clone());
        }
    }
    if let Some(object) = payload.as_object_mut() {
        let shown = object
            .get(key)
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        object.insert("shown".to_owned(), Value::from(shown));
    }
    payload
}

fn site(call_site: &TraceCallSite) -> String {
    format!("{}:{}", call_site.path, call_site.line)
}

fn symbol_value(symbol: &TraceSymbol) -> Value {
    json!({
        "symbol": symbol.qualified_name,
        "kind": symbol_kind_name(symbol.kind),
        "defined_at": format!("{}:{}", symbol.path, symbol.start_line),
        "signature": symbol.signature,
    })
}

fn hop_value(hop: &TraceHop) -> Value {
    let mut value = symbol_value(&hop.symbol);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "call_site".to_owned(),
            hop.call_site
                .as_ref()
                .map(site)
                .map_or(Value::Null, Value::from),
        );
        object.insert(
            "resolution".to_owned(),
            hop.resolution
                .map_or(Value::Null, |rule| Value::from(rule.as_str())),
        );
        if hop.reference {
            object.insert("reference".to_owned(), Value::Bool(true));
        }
    }
    value
}

fn neighbor_value(entry: &TraceNeighbor) -> Value {
    let mut value = symbol_value(&entry.symbol);
    if let Some(object) = value.as_object_mut() {
        object.insert("depth".to_owned(), Value::from(entry.depth));
        // At depth 1 the adjacent symbol is the one asked about.
        if entry.depth > 1 {
            object.insert("via".to_owned(), Value::from(entry.via.clone()));
        }
        object.insert("call_site".to_owned(), Value::from(site(&entry.call_site)));
        object.insert(
            "resolution".to_owned(),
            Value::from(entry.resolution.as_str()),
        );
        if entry.reference {
            object.insert("reference".to_owned(), Value::Bool(true));
        }
        if entry.cycle {
            object.insert("cycle".to_owned(), Value::Bool(true));
        }
    }
    value
}
