use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, SecondsFormat, Utc};
use compresso_atlas::{ImpactOptions, impact_with_options, repository_stale};
use compresso_core::{
    CompressionBudget, Config, ContextGraph, Engine, Lossiness, Mode, NodeKind, TokenCounter,
    TokenProvider, counter_for, operation_identity,
};
use compresso_store::{EdgeKind, OperationRecord, SessionId, StatsFilter, Store};
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::ToolSuccess;
use super::common::{
    REINDEX_HINT, ToolFailure, checked_arguments, map_atlas_error, optional_string, optional_u32,
    optional_usize, required_string, resolve_repository, validate_repo_path,
};

const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_MAX_LATENCY_MS: usize = 200;

pub(crate) fn handles(name: &str) -> bool {
    matches!(
        name,
        "compresso_compress" | "compresso_impact" | "compresso_stats" | "compresso_explain"
    )
}

pub(crate) fn dispatch(
    store: &Store,
    engine: &Engine,
    config: &Config,
    name: &str,
    arguments: Option<JsonObject>,
) -> Result<ToolSuccess, ToolFailure> {
    match name {
        "compresso_compress" => compress_tool(engine, config, arguments),
        "compresso_impact" => impact_tool(store, engine.mode(), arguments),
        "compresso_stats" => stats_tool(store, engine.mode(), arguments),
        "compresso_explain" => explain_tool(store, engine.mode(), arguments),
        _ => Err(ToolFailure::internal()),
    }
}

fn compress_tool(
    engine: &Engine,
    config: &Config,
    arguments: Option<JsonObject>,
) -> Result<ToolSuccess, ToolFailure> {
    let mut arguments =
        checked_arguments(arguments, &["text", "mode", "max_tokens", "max_latency_ms"])?;
    let text = required_string(&mut arguments, "text")?;
    let requested_mode = optional_string(&mut arguments, "mode", false)?
        .map(|value| parse_mode(&value))
        .transpose()?
        .unwrap_or(Mode::Safe);
    let max_tokens = optional_usize(&mut arguments, "max_tokens", 100, 100_000)?
        .unwrap_or_else(|| config.compression.target_tokens.clamp(100, 100_000));
    let max_latency_ms = optional_usize(&mut arguments, "max_latency_ms", 1, 10_000)?
        .unwrap_or(DEFAULT_MAX_LATENCY_MS);
    let max_latency_ms = u64::try_from(max_latency_ms).unwrap_or(u64::MAX);
    let effective_mode = if engine.mode() == Mode::Passthrough {
        Mode::Passthrough
    } else {
        requested_mode
    };
    let o200k = counter_for(TokenProvider::OpenAiO200k);
    let anthropic = counter_for(TokenProvider::AnthropicCalibrated);
    let graph = ContextGraph::from_text(&text, NodeKind::ToolResponse, o200k);
    let budget = CompressionBudget {
        max_tokens,
        max_latency_ms,
        allowed_lossiness: Lossiness::Structural,
        protected_kinds: Vec::new(),
    };
    let started = Instant::now();
    let mut result = if text.len() > MAX_BODY_BYTES {
        engine.fail_open(graph, "input_too_large")
    } else if effective_mode == Mode::Safe
        && std::env::var("COMPRESSO_TEST_FAIL_STRATEGY").as_deref() == Ok("collapse_logs")
    {
        engine.fail_open(graph, "strategy_error:collapse_logs")
    } else {
        engine.compress_in_mode(effective_mode, graph, &budget)
    };
    let compressed = result
        .output
        .nodes
        .iter()
        .map(|node| node.content.as_str())
        .collect::<String>();
    let before_o200k = o200k.count(&text);
    let before_anthropic = anthropic.count(&text);
    let after_o200k = o200k.count(&compressed);
    let after_anthropic = anthropic.count(&compressed);
    result.before_tokens = before_o200k;
    result.after_tokens = after_o200k;
    result.trace.mode = effective_mode;
    result.trace.before_tokens = before_o200k;
    result.trace.after_tokens = after_o200k;
    result.trace.token_provider = o200k.provider();
    let operation_id = result.trace.operation_id.clone();
    let created_at = result.trace.created_at;
    let mode = result.trace.mode;
    let fallback_used = result.fallback_used;
    let strategies_applied = result
        .strategy_results
        .iter()
        .filter(|strategy| strategy.applied)
        .map(|strategy| strategy.strategy_id.clone())
        .collect::<Vec<_>>();
    let payload = json!({
        "schema_version": 1,
        "operation_id": operation_id,
        "compressed": compressed,
        "before_tokens": {
            "openai_o200k": before_o200k,
            "anthropic_calibrated": before_anthropic
        },
        "after_tokens": {
            "openai_o200k": after_o200k,
            "anthropic_calibrated": after_anthropic
        },
        "actual_token_providers": {
            "openai_o200k": o200k.provider(),
            "anthropic_calibrated": anthropic.provider()
        },
        "token_counting_degraded": o200k.provider() != TokenProvider::OpenAiO200k
            || anthropic.provider() != TokenProvider::AnthropicCalibrated,
        "net_tokens_saved": {
            "openai_o200k": before_o200k.saturating_sub(after_o200k),
            "anthropic_calibrated": before_anthropic.saturating_sub(after_anthropic)
        },
        "strategies_applied": strategies_applied,
        "fallback_used": result.fallback_used,
        "fallback_reason": result.fallback_reason,
        "duration_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
    });
    Ok(ToolSuccess {
        payload,
        operation: OperationRecord {
            id: operation_id,
            session_id: None,
            tool: "compresso_compress".to_owned(),
            mode,
            observed_tokens: result.before_tokens,
            applied_tokens: result.after_tokens,
            compression_overhead: 0,
            fallback_used,
            tokens_served: None,
            trace_json: None,
            created_at,
        },
        compression_trace: Some(result.trace),
    })
}

fn impact_tool(
    store: &Store,
    mode: Mode,
    arguments: Option<JsonObject>,
) -> Result<ToolSuccess, ToolFailure> {
    let mut arguments =
        checked_arguments(arguments, &["root", "path", "max_depth", "max_results"])?;
    let root = optional_string(&mut arguments, "root", false)?;
    let path = required_string(&mut arguments, "path")?;
    let max_depth = optional_u32(&mut arguments, "max_depth", 1, 2)?.unwrap_or(2);
    let max_results = optional_usize(&mut arguments, "max_results", 1, 500)?.unwrap_or(100);
    let repository = resolve_repository(store, root.as_deref())?;
    validate_repo_path(&repository.root, &path)?;
    let report = impact_with_options(
        store,
        repository.id,
        &path,
        ImpactOptions {
            max_depth: u8::try_from(max_depth).unwrap_or(2),
            max_results,
        },
    )
    .map_err(map_atlas_error)?;
    let stale = repository_stale(store, repository.id).map_err(map_atlas_error)?;
    let dependents = report
        .dependents
        .iter()
        .map(|entry| {
            json!({
                "path": entry.path,
                "depth": entry.depth,
                "edge_kinds": entry.edge_kinds.iter().map(|kind| edge_kind_name(*kind)).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    retrieval_success(
        json!({
            "schema_version": 1,
            "path": report.path,
            "dependents": dependents,
            "unresolved_edges": report.unresolved_edges,
            "ambiguous_edges": report.ambiguous_edges,
            "complete": report.complete,
            "truncated": report.truncated,
            "stale": stale,
            "reindex_hint": stale.then_some(REINDEX_HINT)
        }),
        "compresso_impact",
        mode,
    )
}

fn stats_tool(
    store: &Store,
    mode: Mode,
    arguments: Option<JsonObject>,
) -> Result<ToolSuccess, ToolFailure> {
    let mut arguments = checked_arguments(arguments, &["since", "session_id"])?;
    let since = optional_string(&mut arguments, "since", false)?;
    let session_id = optional_string(&mut arguments, "session_id", false)?
        .map(|value| {
            SessionId::parse(&value).map_err(|_| {
                ToolFailure::invalid("argument 'session_id' must be a canonical ULID.")
            })
        })
        .transpose()?;
    let now = unix_timestamp()?;
    let since_timestamp = since
        .as_deref()
        .map(|value| parse_since(value, now))
        .transpose()?;
    let stats = store
        .detailed_stats(&StatsFilter {
            since: since_timestamp,
            session_id,
        })
        .map_err(|_| ToolFailure::internal())?;
    let by_tool = stats
        .by_tool
        .iter()
        .map(|row| {
            json!({
                "tool": row.tool,
                "operations": row.operations,
                "net_tokens_saved": row.net_tokens_saved
            })
        })
        .collect::<Vec<_>>();
    retrieval_success(
        json!({
            "schema_version": 1,
            "window": {
                "since": since_timestamp.map(format_timestamp).transpose()?,
                "until": format_timestamp(now)?
            },
            "operations": stats.summary.operations,
            "observed_tokens": stats.summary.observed_tokens,
            "applied_tokens": stats.summary.applied_tokens,
            "removed_tokens": stats.summary.removed_tokens,
            "compression_overhead": stats.summary.compression_overhead,
            "net_tokens_saved": stats.summary.net_tokens_saved,
            "modeled_cost_saved": {
                "amount_usd": stats.summary.modeled_cost_saved.amount_usd,
                "label": stats.summary.modeled_cost_saved.label
            },
            "fallback_rate": stats.summary.fallback_rate,
            "by_tool": by_tool
        }),
        "compresso_stats",
        mode,
    )
}

fn explain_tool(
    store: &Store,
    mode: Mode,
    arguments: Option<JsonObject>,
) -> Result<ToolSuccess, ToolFailure> {
    let mut arguments = checked_arguments(arguments, &["operation_id"])?;
    let target_id = required_string(&mut arguments, "operation_id")?;
    let operation = store
        .operation_by_id(&target_id)
        .map_err(|_| ToolFailure::internal())?
        .filter(|operation| operation.trace_json.is_some())
        .ok_or_else(|| {
            ToolFailure::new(
                "not_found",
                "No retained compression trace was found for that operation (retention is 90 days).",
            )
        })?;
    let trace = serde_json::from_str::<Value>(
        operation
            .trace_json
            .as_deref()
            .ok_or_else(ToolFailure::internal)?,
    )
    .map_err(|_| ToolFailure::internal())?;
    retrieval_success(
        json!({
            "schema_version": 1,
            "explained_operation_id": target_id,
            "tool": operation.tool,
            "trace": trace
        }),
        "compresso_explain",
        mode,
    )
}

fn retrieval_success(
    mut payload: Value,
    tool: &'static str,
    mode: Mode,
) -> Result<ToolSuccess, ToolFailure> {
    let identity = operation_identity();
    let object = payload.as_object_mut().ok_or_else(ToolFailure::internal)?;
    object.insert(
        "operation_id".to_owned(),
        Value::String(identity.id.clone()),
    );
    object.insert(
        "token_provider".to_owned(),
        serde_json::to_value(TokenProvider::OpenAiO200k).map_err(|_| ToolFailure::internal())?,
    );
    object.insert("tokens_used".to_owned(), Value::from(0_u64));
    let tokens_used = fixed_point_tokens(&mut payload, counter_for(TokenProvider::OpenAiO200k))?;
    Ok(ToolSuccess {
        payload,
        operation: OperationRecord {
            id: identity.id,
            session_id: None,
            tool: tool.to_owned(),
            mode,
            observed_tokens: tokens_used,
            applied_tokens: tokens_used,
            compression_overhead: 0,
            fallback_used: false,
            tokens_served: Some(tokens_used),
            trace_json: None,
            created_at: identity.created_at,
        },
        compression_trace: None,
    })
}

fn fixed_point_tokens(
    payload: &mut Value,
    counter: &dyn TokenCounter,
) -> Result<usize, ToolFailure> {
    let mut tokens = 0_usize;
    for _ in 0..16 {
        payload
            .as_object_mut()
            .ok_or_else(ToolFailure::internal)?
            .insert("tokens_used".to_owned(), Value::from(tokens));
        let rendered = serde_json::to_string(payload).map_err(|_| ToolFailure::internal())?;
        let recounted = counter.count(&rendered);
        if recounted == tokens {
            return Ok(tokens);
        }
        tokens = recounted;
    }
    Err(ToolFailure::internal())
}

fn parse_mode(value: &str) -> Result<Mode, ToolFailure> {
    match value {
        "safe" => Ok(Mode::Safe),
        "passthrough" => Ok(Mode::Passthrough),
        _ => Err(ToolFailure::invalid(
            "argument 'mode' must be 'safe' or 'passthrough'.",
        )),
    }
}

fn parse_since(value: &str, now: i64) -> Result<i64, ToolFailure> {
    if let Some(hours) = value.strip_suffix('h') {
        return hours
            .parse::<i64>()
            .ok()
            .filter(|hours| *hours > 0)
            .and_then(|hours| hours.checked_mul(3_600))
            .and_then(|seconds| now.checked_sub(seconds))
            .ok_or_else(|| ToolFailure::invalid("argument 'since' is invalid."));
    }
    if let Some(days) = value.strip_suffix('d') {
        return days
            .parse::<i64>()
            .ok()
            .filter(|days| *days > 0)
            .and_then(|days| days.checked_mul(86_400))
            .and_then(|seconds| now.checked_sub(seconds))
            .ok_or_else(|| ToolFailure::invalid("argument 'since' is invalid."));
    }
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.timestamp())
        .map_err(|_| {
            ToolFailure::invalid(
                "argument 'since' must be RFC 3339 or a duration such as '24h', '7d', or '30d'.",
            )
        })
}

fn unix_timestamp() -> Result<i64, ToolFailure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .ok_or_else(ToolFailure::internal)
}

fn format_timestamp(timestamp: i64) -> Result<String, ToolFailure> {
    DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Secs, true))
        .ok_or_else(ToolFailure::internal)
}

const fn edge_kind_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Imports => "imports",
        EdgeKind::References => "references",
        EdgeKind::Tests => "tests",
    }
}
