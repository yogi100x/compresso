pub(crate) mod atlas_nav;
pub(crate) mod atlas_read;
pub(crate) mod common;
pub(crate) mod engine_tools;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use compresso_core::{Config, Engine, Mode, TokenProvider, Trace, counter_for};
use compresso_store::{OperationRecord, RetrievalMetrics, Store};
use rmcp::model::{CallToolResult, JsonObject};

use crate::session::SessionRuntime;

pub(crate) struct ToolSuccess {
    pub(crate) payload: serde_json::Value,
    pub(crate) operation: OperationRecord,
    pub(crate) compression_trace: Option<Trace>,
}

pub(crate) fn handles(name: &str) -> bool {
    atlas_nav::handles(name) || atlas_read::handles(name) || engine_tools::handles(name)
}

pub(crate) fn dispatch(
    store: &Store,
    engine: &Engine,
    config: &Config,
    session: &SessionRuntime,
    repository_root: Option<&Path>,
    name: &str,
    arguments: Option<JsonObject>,
) -> CallToolResult {
    let Some(session_id) = session.current() else {
        return common::ToolFailure::internal().into_result();
    };
    let repository_id = repository_root
        .and_then(|root| store.repo_by_root(root).ok().flatten())
        .map(|repository| repository.id);
    let outcome = if atlas_nav::handles(name) {
        atlas_nav::dispatch(store, repository_root, name, arguments)
            .and_then(|payload| retrieval_outcome(payload, name, engine.mode(), repository_id))
    } else if atlas_read::handles(name) {
        atlas_read::dispatch(store, repository_root, name, arguments)
            .and_then(|payload| retrieval_outcome(payload, name, engine.mode(), repository_id))
    } else {
        engine_tools::dispatch(store, engine, config, repository_root, name, arguments)
    };
    match outcome {
        Ok(mut success) => {
            success.operation.session_id = Some(session_id.clone());
            if success.compression_trace.is_none() {
                let tokens_served = match serde_json::to_string(&success.payload) {
                    Ok(rendered) => counter_for(TokenProvider::OpenAiO200k).count(&rendered),
                    Err(_) => return common::ToolFailure::internal().into_result(),
                };
                success.operation.observed_tokens = tokens_served;
                success.operation.applied_tokens = tokens_served;
                success.operation.tokens_served = Some(tokens_served);
                if success.operation.retrieval.is_none()
                    && matches!(success.operation.tool.as_str(), "compresso_impact")
                {
                    success.operation.retrieval = Some(RetrievalMetrics {
                        repository_id,
                        estimated_raw_source_tokens: tokens_served,
                        modeled_follow_up_calls_avoided: 0,
                    });
                }
                if let Some(metrics) = &mut success.operation.retrieval
                    && metrics.estimated_raw_source_tokens == 0
                {
                    metrics.estimated_raw_source_tokens = tokens_served;
                }
            }
            if let Some(trace) = &mut success.compression_trace {
                trace.session_id = Some(session_id.into_string());
                success.operation.trace_json = match serde_json::to_string(trace) {
                    Ok(trace) => Some(trace),
                    Err(_) => return common::ToolFailure::internal().into_result(),
                };
            }
            session.record_operation(success.operation);
            common::success(success.payload)
        }
        Err(error) => error.into_result(),
    }
}

fn retrieval_outcome(
    mut payload: serde_json::Value,
    tool: &str,
    mode: Mode,
    repository_id: Option<i64>,
) -> Result<ToolSuccess, common::ToolFailure> {
    let telemetry = payload
        .as_object_mut()
        .and_then(|object| object.remove("_compresso_telemetry"));
    let estimated_raw_source_tokens = telemetry
        .as_ref()
        .and_then(|value| value.get("estimated_raw_source_tokens"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_default();
    let modeled_follow_up_calls_avoided = telemetry
        .as_ref()
        .and_then(|value| value.get("modeled_follow_up_calls_avoided"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_default();
    let id = payload
        .get("operation_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(common::ToolFailure::internal)?
        .to_owned();
    let created_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .ok_or_else(common::ToolFailure::internal)?;
    Ok(ToolSuccess {
        payload,
        operation: OperationRecord {
            id,
            session_id: None,
            tool: tool.to_owned(),
            mode,
            observed_tokens: 0,
            applied_tokens: 0,
            compression_overhead: 0,
            fallback_used: false,
            tokens_served: Some(0),
            retrieval: Some(RetrievalMetrics {
                repository_id,
                estimated_raw_source_tokens,
                modeled_follow_up_calls_avoided,
            }),
            trace_json: None,
            created_at,
        },
        compression_trace: None,
    })
}
