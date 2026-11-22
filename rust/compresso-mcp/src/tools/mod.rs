pub(crate) mod atlas_grep;
pub(crate) mod atlas_nav;
pub(crate) mod atlas_read;
pub(crate) mod atlas_trace;
pub(crate) mod common;
pub(crate) mod edit;
pub(crate) mod engine_tools;
pub(crate) mod recall;
pub(crate) mod report;
pub(crate) mod text;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use compresso_core::{Config, Engine, Mode, TokenCounter, TokenProvider, Trace, counter_for};
use compresso_store::{OperationRecord, RetrievalMetrics, Store};
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::Value;

use crate::profile::{McpResultFormat, McpToolProfile};
use crate::session::SessionRuntime;

/// Internal payload key carrying the call's effective `max_tokens`; removed
/// before any form is sent.
pub(crate) const BUDGET_KEY: &str = "_compresso_max_tokens";
/// Text renders re-run with a larger reserve at most this many times before
/// the text is cut at a line boundary.
const TEXT_FIT_ATTEMPTS: usize = 4;
const TEXT_CUT_NOTICE: &str = "[output cut to max_tokens]";

pub(crate) struct ToolSuccess {
    pub(crate) payload: serde_json::Value,
    pub(crate) operation: OperationRecord,
    pub(crate) compression_trace: Option<Trace>,
}

/// How one tool call fits and encodes its result.
#[derive(Clone, Copy)]
pub(crate) struct Form {
    text: bool,
    /// The lean profile's defaults apply (its schema states them).
    lean: bool,
    /// Tokens withheld from a tool's content budget so its text header and
    /// trailing notes still fit under `max_tokens`.
    reserve: usize,
}

impl Form {
    /// Whether the result is fitted and sent as compact text.
    pub(crate) const fn is_text(self) -> bool {
        self.text
    }

    /// Whether the call is served by the lean profile.
    pub(crate) const fn is_lean(self) -> bool {
        self.lean
    }

    /// The budget a tool spends on content whose size it measures alone.
    pub(crate) const fn content_budget(self, max_tokens: usize) -> usize {
        max_tokens.saturating_sub(self.reserve)
    }

    /// Keeps the longest prefix of `values` whose rendering in this form fits
    /// `max_tokens`: the JSON array, or one `row` per line in text.
    pub(crate) fn cap(
        self,
        values: Vec<Value>,
        max_tokens: usize,
        counter: &dyn TokenCounter,
        row: fn(&Value) -> String,
    ) -> (Vec<Value>, usize, bool) {
        if self.text {
            common::cap_rows(values, self.content_budget(max_tokens), counter, row)
        } else {
            common::cap_values(values, max_tokens, counter)
        }
    }
}

pub(crate) fn handles(name: &str) -> bool {
    atlas_nav::handles(name)
        || atlas_grep::handles(name)
        || atlas_read::handles(name)
        || atlas_trace::handles(name)
        || engine_tools::handles(name)
        || recall::handles(name)
        || edit::handles(name)
        || report::handles(name)
}

/// Remembers the failed call's tool and code for `compresso_report`, then
/// returns the failure unchanged.
fn failed(session: &SessionRuntime, name: &str, error: common::ToolFailure) -> CallToolResult {
    session.record_failure(name, error.code());
    error.into_result()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch(
    store: &Store,
    engine: &Engine,
    config: &Config,
    session: &SessionRuntime,
    repository_root: Option<&Path>,
    recall_options: Option<&recall::McpRecallOptions>,
    edits_enabled: bool,
    name: &str,
    arguments: Option<JsonObject>,
    format: McpResultFormat,
    profile: McpToolProfile,
) -> CallToolResult {
    let Some(session_id) = session.current() else {
        return common::ToolFailure::internal().into_result();
    };
    let repository_id = repository_root
        .and_then(|root| store.repo_by_root(root).ok().flatten())
        .map(|repository| repository.id);
    let counter = counter_for(TokenProvider::OpenAiO200k);
    if edit::handles(name) {
        // An edit runs exactly once; only its report is fitted to the budget.
        return match edit::run(
            store,
            repository_root,
            edits_enabled,
            engine.mode(),
            arguments,
            format == McpResultFormat::Text,
        ) {
            Ok((success, sent)) => send(success, sent, session, session_id, repository_id, counter),
            Err(error) => failed(session, name, error),
        };
    }
    let run = |form: Form, arguments: Option<JsonObject>| {
        if atlas_nav::handles(name) {
            atlas_nav::dispatch(store, repository_root, name, arguments, form)
                .and_then(|payload| retrieval_outcome(payload, name, engine.mode(), repository_id))
        } else if atlas_grep::handles(name) {
            atlas_grep::dispatch(store, repository_root, arguments, form)
                .and_then(|payload| retrieval_outcome(payload, name, engine.mode(), repository_id))
        } else if atlas_read::handles(name) {
            atlas_read::dispatch(store, repository_root, name, arguments, form)
                .and_then(|payload| retrieval_outcome(payload, name, engine.mode(), repository_id))
        } else if atlas_trace::handles(name) {
            atlas_trace::dispatch(store, repository_root, arguments, form)
                .and_then(|payload| retrieval_outcome(payload, name, engine.mode(), repository_id))
        } else if recall::handles(name) {
            recall::dispatch(recall_options, repository_root, engine.mode(), arguments)
        } else if report::handles(name) {
            report::run(
                &report::ReportContext {
                    store,
                    session,
                    repository_root,
                    recall_enabled: recall_options.is_some_and(|options| options.is_enabled()),
                    edits_enabled,
                    profile,
                    mode: engine.mode(),
                },
                arguments,
            )
        } else {
            engine_tools::dispatch(store, engine, config, repository_root, name, arguments)
        }
    };
    let mut form = Form {
        text: format == McpResultFormat::Text && text::supports(name),
        lean: profile == McpToolProfile::Lean,
        reserve: 0,
    };
    let mut attempt = 1;
    let (success, sent) = loop {
        let mut success = match run(form, arguments.clone()) {
            Ok(success) => success,
            Err(error) => return failed(session, name, error),
        };
        let max_tokens = success
            .payload
            .as_object_mut()
            .and_then(|object| object.remove(BUDGET_KEY))
            .and_then(|value| value.as_u64())
            .and_then(|value| usize::try_from(value).ok());
        if !form.text {
            match serde_json::to_string(&success.payload) {
                Ok(sent) => break (success, sent),
                Err(_) => return common::ToolFailure::internal().into_result(),
            }
        }
        let Some(rendered) = text::render(name, &success.payload) else {
            return common::ToolFailure::internal().into_result();
        };
        let Some(max_tokens) = max_tokens else {
            break (success, rendered);
        };
        let tokens = counter.count(&rendered);
        if tokens <= max_tokens {
            break (success, rendered);
        }
        if attempt == TEXT_FIT_ATTEMPTS {
            break (success, cut_text(&rendered, max_tokens, counter));
        }
        // Withhold the overshoot (plus slack for tokenizer drift) from the
        // content budget and fit again.
        form.reserve += tokens - max_tokens + 8;
        attempt += 1;
    };
    send(success, sent, session, session_id, repository_id, counter)
}

/// Records the operation with what the client receives, then sends it.
fn send(
    mut success: ToolSuccess,
    sent: String,
    session: &SessionRuntime,
    session_id: compresso_store::SessionId,
    repository_id: Option<i64>,
    counter: &dyn TokenCounter,
) -> CallToolResult {
    success.operation.session_id = Some(session_id.clone());
    if success.compression_trace.is_none() {
        // The ledger records what the client received, in either form.
        let tokens_served = counter.count(&sent);
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
    common::success_text(sent)
}

/// The last resort when re-fitting could not bring a text result under
/// `max_tokens`: the longest whole-line prefix that fits with a notice.
fn cut_text(text: &str, max_tokens: usize, counter: &dyn TokenCounter) -> String {
    let ends = text
        .match_indices('\n')
        .map(|(index, _)| index + 1)
        .collect::<Vec<_>>();
    let fits =
        |end: usize| counter.count(&format!("{}{TEXT_CUT_NOTICE}", &text[..end])) <= max_tokens;
    let (mut low, mut high) = (0, ends.len());
    while low < high {
        let middle = (low + high).div_ceil(2);
        if fits(ends[middle - 1]) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let end = if low == 0 { 0 } else { ends[low - 1] };
    format!("{}{TEXT_CUT_NOTICE}", &text[..end])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cut_text_keeps_whole_lines_and_fits_the_budget() {
        let counter = counter_for(TokenProvider::OpenAiO200k);
        let text = (0..200)
            .map(|index| format!("line {index} of the result\n"))
            .collect::<String>();
        let cut = cut_text(&text, 100, counter);
        assert!(counter.count(&cut) <= 100);
        assert!(cut.ends_with(&format!("\n{TEXT_CUT_NOTICE}")));
        assert!(text.starts_with(cut.trim_end_matches(TEXT_CUT_NOTICE)));
        assert_eq!(cut_text("one long line", 5, counter), TEXT_CUT_NOTICE);
    }
}
