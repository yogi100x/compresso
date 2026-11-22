//! The operator-enabled `compresso_sql` tool (E17-50, D115, ADR-0036).
//!
//! Advertised only when the server starts with `--sql-env NAME`, never in the
//! lean profile, and never to the exploration agent. Every call goes through
//! [`compresso_sql::SqlService`], which owns the connection, the statement
//! gate, and the read-only session. A call writes no ledger row: a new tool
//! name in the ledger would break `stats` in an older binary sharing the
//! store (the same choice D113 made for `compresso_report`), so
//! `compresso_stats` does not count it and `compresso_explain` has no trace
//! for it. A failure is remembered in memory for `compresso_report`.

use std::time::Duration;

use compresso_core::{TokenCounter, TokenProvider, counter_for};
use compresso_sql::{
    DEFAULT_MAX_ROWS, DEFAULT_TIMEOUT_MS, MAX_ROWS, MAX_TIMEOUT_MS, SqlAction, SqlEngine,
    SqlOutcome, SqlRequest, SqlService, Truncation,
};
use rmcp::model::JsonObject;
use serde_json::{Value, json};

use super::common::{ToolFailure, checked_arguments, optional_string, optional_usize};

pub(crate) const TOOL_NAME: &str = "compresso_sql";
pub(crate) const DEFAULT_MAX_TOKENS: usize = 2_000;
pub(crate) const MIN_MAX_TOKENS: usize = 300;
pub(crate) const MAX_MAX_TOKENS: usize = 8_000;
pub(crate) const MIN_TIMEOUT_MS: usize = 100;
/// The longest statement text accepted.
pub(crate) const MAX_QUERY_CHARS: usize = 100_000;

pub(crate) fn handles(name: &str) -> bool {
    name == TOOL_NAME
}

/// What the advertised surface says about the SQL tool, fixed at startup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SqlSurface {
    /// `--sql-env` not given: the tool is not advertised.
    #[default]
    Off,
    /// `--sql-env` given, but the variable is missing or invalid: the tool is
    /// advertised and every call returns `sql_not_configured`.
    Unconfigured,
    /// A local SQLite file: no network at all.
    Sqlite,
    /// One network database of this engine.
    Network(SqlEngine),
}

impl SqlSurface {
    /// The surface for a server process's service, if any.
    pub fn of(service: Option<&SqlService>) -> Self {
        match service.map(SqlService::engine) {
            None => Self::Off,
            Some(None) => Self::Unconfigured,
            Some(Some(SqlEngine::Sqlite)) => Self::Sqlite,
            Some(Some(engine)) => Self::Network(engine),
        }
    }

    /// Whether the tool is advertised.
    pub const fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// The engine name the tool description and report state.
    pub const fn engine_name(self) -> Option<&'static str> {
        match self {
            Self::Sqlite => Some("sqlite"),
            Self::Network(engine) => Some(engine.name()),
            Self::Off | Self::Unconfigured => None,
        }
    }
}

/// The message for a call the process does not serve.
pub(crate) fn unavailable(enabled: bool) -> ToolFailure {
    if enabled {
        ToolFailure::new(
            "tool_unavailable",
            "Tool 'compresso_sql' is not served by the lean tool profile; the operator can serve it from a server started without `--tools lean`.",
        )
    } else {
        ToolFailure::new(
            "tool_unavailable",
            "Tool 'compresso_sql' is not enabled for this server process; the operator can start the server with `compresso mcp serve --sql-env COMPRESSO_SQL_NAME` to use it.",
        )
    }
}

/// Runs one call and returns the JSON text sent to the client.
pub(crate) fn run(
    service: Option<&SqlService>,
    arguments: Option<JsonObject>,
) -> Result<String, ToolFailure> {
    let Some(service) = service else {
        return Err(unavailable(false));
    };
    let mut arguments = checked_arguments(
        arguments,
        &[
            "action",
            "query",
            "table",
            "max_rows",
            "max_tokens",
            "timeout_ms",
        ],
    )?;
    let action = optional_string(&mut arguments, "action", false)?.ok_or_else(|| {
        ToolFailure::invalid("argument 'action' is required: query, tables, or describe.")
    })?;
    let query = optional_string(&mut arguments, "query", false)?;
    let table = optional_string(&mut arguments, "table", false)?;
    let max_rows =
        optional_usize(&mut arguments, "max_rows", 1, MAX_ROWS)?.unwrap_or(DEFAULT_MAX_ROWS);
    let max_tokens = optional_usize(&mut arguments, "max_tokens", MIN_MAX_TOKENS, MAX_MAX_TOKENS)?
        .unwrap_or(DEFAULT_MAX_TOKENS);
    let timeout_limit = usize::try_from(MAX_TIMEOUT_MS).unwrap_or(usize::MAX);
    let timeout_ms = optional_usize(&mut arguments, "timeout_ms", MIN_TIMEOUT_MS, timeout_limit)?
        .map_or(DEFAULT_TIMEOUT_MS, |value| {
            u64::try_from(value).unwrap_or(MAX_TIMEOUT_MS)
        });
    let action = match (action.as_str(), query, table) {
        ("query", Some(query), None) => {
            if query.chars().count() > MAX_QUERY_CHARS {
                return Err(ToolFailure::invalid(format!(
                    "argument 'query' is over {MAX_QUERY_CHARS} characters."
                )));
            }
            SqlAction::Query(query)
        }
        ("tables", None, None) => SqlAction::Tables,
        ("describe", None, Some(table)) => SqlAction::Describe(table),
        ("query", _, _) => {
            return Err(ToolFailure::invalid(
                "action query needs 'query' (one SQL statement) and no 'table'.",
            ));
        }
        ("tables", _, _) => {
            return Err(ToolFailure::invalid(
                "action tables takes no 'query' or 'table'.",
            ));
        }
        ("describe", _, _) => {
            return Err(ToolFailure::invalid(
                "action describe needs 'table' (table or schema.table) and no 'query'.",
            ));
        }
        _ => {
            return Err(ToolFailure::invalid(
                "argument 'action' must be one of query, tables, describe.",
            ));
        }
    };
    let request = SqlRequest {
        action,
        max_rows,
        timeout: Duration::from_millis(timeout_ms),
    };
    let outcome = service
        .execute(&request)
        .map_err(|error| ToolFailure::new(error.code(), error.message().to_owned()))?;
    let engine = service.engine().map_or("unknown", SqlEngine::name);
    Ok(fit(
        outcome,
        engine,
        max_tokens,
        counter_for(TokenProvider::OpenAiO200k),
    ))
}

fn payload(outcome: &SqlOutcome, engine: &str) -> Value {
    let (action, mut body) = match outcome {
        SqlOutcome::Query(result) => ("query", json!(result)),
        SqlOutcome::Tables(result) => ("tables", json!(result)),
        SqlOutcome::Describe(result) => ("describe", json!(result)),
    };
    if let Value::Object(object) = &mut body {
        object.insert("engine".to_owned(), Value::from(engine));
        object.insert("action".to_owned(), Value::from(action));
        if object.get("truncation_reason") == Some(&Value::Null) {
            object.remove("truncation_reason");
        }
    }
    body
}

/// The result's JSON, with rows (or tables, or described columns) dropped
/// from the end until it fits `max_tokens`.
fn fit(
    mut outcome: SqlOutcome,
    engine: &str,
    max_tokens: usize,
    counter: &dyn TokenCounter,
) -> String {
    let render = |outcome: &SqlOutcome| payload(outcome, engine).to_string();
    let fits = |outcome: &SqlOutcome| counter.count(&render(outcome)) <= max_tokens;
    if fits(&outcome) {
        return render(&outcome);
    }
    let length = match &outcome {
        SqlOutcome::Query(result) => result.rows.len(),
        SqlOutcome::Tables(result) => result.tables.len(),
        SqlOutcome::Describe(result) => result.columns.len(),
    };
    let keep = |outcome: &mut SqlOutcome, count: usize| match outcome {
        SqlOutcome::Query(result) => result.truncate_to(count, Truncation::MaxTokens),
        SqlOutcome::Tables(result) => {
            result.tables.truncate(count);
            result.truncated = true;
        }
        SqlOutcome::Describe(result) => {
            result.columns.truncate(count);
            result.indexes.clear();
            result.foreign_keys.clear();
        }
    };
    // The largest prefix that fits, by bisection over the row count.
    let (mut low, mut high) = (0, length);
    while low < high {
        let middle = (low + high).div_ceil(2);
        let mut candidate = outcome.clone();
        keep(&mut candidate, middle);
        if fits(&candidate) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    keep(&mut outcome, low);
    let mut value = payload(&outcome, engine);
    if let (SqlOutcome::Describe(_), Value::Object(object)) = (&outcome, &mut value) {
        object.insert("truncated".to_owned(), Value::Bool(true));
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use compresso_sql::QueryResult;

    #[test]
    fn test_fit_drops_trailing_rows_and_says_why() {
        let counter = counter_for(TokenProvider::OpenAiO200k);
        let result = QueryResult {
            columns: vec!["body".to_owned()],
            rows: (0..200)
                .map(|index| vec![Value::from(format!("row {index} with some words in it"))])
                .collect(),
            row_count: 200,
            truncated: false,
            truncation_reason: None,
            elapsed_ms: 3,
        };
        let text = fit(SqlOutcome::Query(result), "sqlite", 400, counter);
        assert!(counter.count(&text) <= 400, "{text}");
        let value: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(value["truncation_reason"], "max_tokens");
        assert_eq!(value["truncated"], true);
        assert_eq!(value["engine"], "sqlite");
        let rows = value["rows"].as_array().expect("rows").len();
        assert!(rows > 0 && rows < 200);
        assert_eq!(value["row_count"], rows);
    }
}
