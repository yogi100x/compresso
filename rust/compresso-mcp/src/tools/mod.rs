pub(crate) mod atlas_nav;
pub(crate) mod atlas_read;
pub(crate) mod common;
pub(crate) mod engine_tools;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use compresso_core::{Config, Engine, Mode, TokenProvider, Trace, counter_for};
use compresso_store::{OperationRecord, Store};
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
    let outcome = if atlas_nav::handles(name) {
        atlas_nav::dispatch(store, repository_root, name, arguments)
            .and_then(|payload| retrieval_outcome(payload, name, engine.mode()))
    } else if atlas_read::handles(name) {
        atlas_read::dispatch(store, repository_root, name, arguments)
            .and_then(|payload| retrieval_outcome(payload, name, engine.mode()))
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
    payload: serde_json::Value,
    tool: &str,
    mode: Mode,
) -> Result<ToolSuccess, common::ToolFailure> {
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
            trace_json: None,
            created_at,
        },
        compression_trace: None,
    })
}
