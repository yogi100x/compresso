//! `compresso trace`: callers, callees, and call paths (D90).

use std::path::Path;

use clap::Args;
use compresso_atlas::{
    TraceCallSite, TraceHop, TraceNeighbor, TraceOptions, TraceResult, TraceSymbol,
    repository_stale, trace,
};
use compresso_store::Store;
use serde_json::{Value, json};

use super::{
    OutputArgs, Rendered, RepositoryArgs, RetrievalError, map_atlas_error, rendered,
    resolve_repository, symbol_kind_name,
};

#[derive(Args, Debug)]
pub struct TraceArgs {
    /// Calling symbol: `name`, `Type::name`, or `path/file.rs:Type::name`.
    /// Alone, lists what it calls.
    #[arg(long, value_name = "SYMBOL")]
    pub from: Option<String>,
    /// Called symbol, same forms. Alone, lists who calls it.
    #[arg(long, value_name = "SYMBOL")]
    pub to: Option<String>,
    #[command(flatten)]
    pub repository: RepositoryArgs,
    /// Call levels to follow (1-8). Default: 6 for a path, 1 otherwise.
    #[arg(long)]
    pub max_depth: Option<u8>,
    #[arg(long, default_value_t = 3)]
    pub max_paths: usize,
    #[arg(long, default_value_t = 50)]
    pub max_results: usize,
    #[command(flatten)]
    pub output: OutputArgs,
}

pub(super) fn execute_trace(
    store: &Store,
    cwd: &Path,
    arguments: TraceArgs,
) -> Result<Rendered, RetrievalError> {
    if arguments.from.is_none() && arguments.to.is_none() {
        return Err(RetrievalError::invalid(
            "name --to for callers, --from for callees, or both for call paths",
        ));
    }
    let repository = resolve_repository(store, arguments.repository.root.as_deref(), cwd)?;
    let report = trace(
        store,
        repository.id,
        arguments.from.as_deref(),
        arguments.to.as_deref(),
        TraceOptions {
            max_depth: arguments.max_depth,
            max_paths: arguments.max_paths,
            max_results: arguments.max_results,
            ..TraceOptions::default()
        },
    )
    .map_err(map_atlas_error)?;
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;

    let mut human = Vec::new();
    let (status, body) = match &report.result {
        TraceResult::Paths(paths) => {
            for (index, path) in paths.iter().enumerate() {
                human.push(format!("path {}:", index + 1));
                for hop in path {
                    human.push(format!("  {}", hop_line(hop)));
                }
            }
            (
                "paths",
                json!({ "paths": paths.iter().map(|path| path.iter().map(hop_json).collect::<Vec<_>>()).collect::<Vec<_>>() }),
            )
        }
        TraceResult::NoPath => {
            human.push("no call path found within the bounds".to_owned());
            ("no_path", json!({ "paths": [] }))
        }
        TraceResult::Callers(entries) | TraceResult::Callees(entries) => {
            let (status, key) = if matches!(report.result, TraceResult::Callers(_)) {
                ("callers", "callers")
            } else {
                ("callees", "callees")
            };
            for entry in entries {
                human.push(neighbor_line(entry, status == "callers"));
            }
            (
                status,
                json!({ key: entries.iter().map(neighbor_json).collect::<Vec<_>>() }),
            )
        }
        TraceResult::Ambiguous {
            end,
            candidates,
            total,
        } => {
            human.push(format!(
                "`{}` matches {total} symbols; qualify it as path:name:",
                end.as_str()
            ));
            for candidate in candidates {
                human.push(format!("  {}", symbol_line(candidate)));
            }
            (
                "ambiguous",
                json!({ "end": end.as_str(), "candidates_total": total, "candidates": candidates.iter().map(symbol_json).collect::<Vec<_>>() }),
            )
        }
        TraceResult::NotFound { end, name } => {
            human.push(format!("`{}` symbol not found: {name}", end.as_str()));
            ("not_found", json!({ "end": end.as_str(), "name": name }))
        }
        TraceResult::CallGraphMissing => {
            human.push("the index predates call sites; run `compresso index` and retry".to_owned());
            ("reindex_required", json!({}))
        }
    };
    for call in &report.unfollowed {
        human.push(format!(
            "  not followed ({}): {} in {} at {}",
            call.status,
            call.callee,
            call.caller,
            site(&call.call_site)
        ));
    }
    human.push(format!(
        "total {}; complete {}; truncated {}; depth_limited {}; stale {}",
        report.total, report.complete, report.truncated, report.depth_limited, stale
    ));

    let mut data = json!({
        "status": status,
        "from": arguments.from,
        "to": arguments.to,
        "total": report.total,
        "unfollowed": report.unfollowed.iter().map(|call| json!({
            "caller": call.caller,
            "call": call.callee,
            "call_site": site(&call.call_site),
            "status": call.status,
            "candidates": call.candidates,
        })).collect::<Vec<_>>(),
        "unfollowed_total": report.unfollowed_total,
        "nodes_visited": report.nodes_visited,
        "depth_reached": report.depth_reached,
        "max_depth": report.max_depth,
        "depth_limited": report.depth_limited,
        "truncated": report.truncated,
        "complete": report.complete,
        "stale": stale,
    });
    if let (Some(data), Value::Object(body)) = (data.as_object_mut(), body) {
        data.extend(body);
    }
    Ok(rendered(data, human.join("\n")))
}

fn site(call_site: &TraceCallSite) -> String {
    format!("{}:{}", call_site.path, call_site.line)
}

fn symbol_line(symbol: &TraceSymbol) -> String {
    format!(
        "{} ({}) {}:{}{}",
        symbol.qualified_name,
        symbol_kind_name(symbol.kind),
        symbol.path,
        symbol.start_line,
        symbol
            .signature
            .as_deref()
            .map(|signature| format!("  {signature}"))
            .unwrap_or_default()
    )
}

fn symbol_json(symbol: &TraceSymbol) -> Value {
    json!({
        "symbol": symbol.qualified_name,
        "kind": symbol_kind_name(symbol.kind),
        "defined_at": format!("{}:{}", symbol.path, symbol.start_line),
        "signature": symbol.signature,
    })
}

fn hop_line(hop: &TraceHop) -> String {
    match (&hop.call_site, hop.resolution) {
        (Some(call_site), Some(rule)) => format!(
            "-> {}  [called at {}, {}]",
            symbol_line(&hop.symbol),
            site(call_site),
            rule.as_str()
        ),
        _ => symbol_line(&hop.symbol),
    }
}

fn hop_json(hop: &TraceHop) -> Value {
    let mut value = symbol_json(&hop.symbol);
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
    }
    value
}

fn neighbor_line(entry: &TraceNeighbor, callers: bool) -> String {
    format!(
        "{}{}  [{} {} at {}, {}{}]",
        "  ".repeat(usize::from(entry.depth.saturating_sub(1))),
        symbol_line(&entry.symbol),
        if callers { "calls" } else { "called by" },
        entry.via,
        site(&entry.call_site),
        entry.resolution.as_str(),
        if entry.cycle { ", cycle" } else { "" }
    )
}

fn neighbor_json(entry: &TraceNeighbor) -> Value {
    let mut value = symbol_json(&entry.symbol);
    if let Some(object) = value.as_object_mut() {
        object.insert("depth".to_owned(), Value::from(entry.depth));
        object.insert("via".to_owned(), Value::from(entry.via.clone()));
        object.insert("call_site".to_owned(), Value::from(site(&entry.call_site)));
        object.insert(
            "resolution".to_owned(),
            Value::from(entry.resolution.as_str()),
        );
        object.insert("cycle".to_owned(), Value::Bool(entry.cycle));
    }
    value
}
