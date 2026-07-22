pub(crate) mod atlas_nav;
pub(crate) mod atlas_read;
pub(crate) mod common;
pub(crate) mod engine_tools;

use compresso_core::{Config, Engine, Trace};
use compresso_store::{OperationRecord, Store};
use rmcp::model::{CallToolResult, JsonObject};

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
    name: &str,
    arguments: Option<JsonObject>,
) -> CallToolResult {
    if atlas_nav::handles(name) {
        atlas_nav::dispatch(store, name, arguments)
    } else if atlas_read::handles(name) {
        atlas_read::dispatch(store, name, arguments)
    } else {
        match engine_tools::dispatch(store, engine, config, name, arguments) {
            Ok(mut success) => {
                if let Some(trace) = &success.compression_trace {
                    success.operation.trace_json = match serde_json::to_string(trace) {
                        Ok(trace) => Some(trace),
                        Err(_) => return common::ToolFailure::internal().into_result(),
                    };
                }
                if let Err(error) = store.record_operation(&success.operation) {
                    eprintln!("compresso MCP operation ledger warning for {name}: {error}");
                }
                common::success(success.payload)
            }
            Err(error) => error.into_result(),
        }
    }
}
