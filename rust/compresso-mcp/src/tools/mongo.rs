//! The operator-enabled `compresso_mongo` tool (E17-52, D117, ADR-0038).
//!
//! Advertised only when the server starts with `--mongo-env NAME`, never in
//! the lean profile, and never to the exploration agent. Every call goes
//! through [`compresso_sql::mongo::MongoService`], which checks the
//! arguments against its allow-list before connecting and checks the account
//! on every connection: MongoDB has no read-only session, so those two are
//! the control. Like `compresso_sql`, a call writes no ledger row (D113's
//! reason); a failure is remembered in memory for `compresso_report`.

use std::time::Duration;

use compresso_core::{TokenCounter, TokenProvider, counter_for};
use compresso_sql::mongo::{MongoAction, MongoOutcome, MongoRequest, MongoService};
use compresso_sql::{DEFAULT_MAX_ROWS, DEFAULT_TIMEOUT_MS, MAX_ROWS, MAX_TIMEOUT_MS, Truncation};
use rmcp::model::JsonObject;
use serde_json::{Map, Value, json};

use super::common::{ToolFailure, checked_arguments, optional_string, optional_usize};

pub(crate) const TOOL_NAME: &str = "compresso_mongo";
pub(crate) const DEFAULT_MAX_TOKENS: usize = super::sql::DEFAULT_MAX_TOKENS;
pub(crate) const MIN_MAX_TOKENS: usize = super::sql::MIN_MAX_TOKENS;
pub(crate) const MAX_MAX_TOKENS: usize = super::sql::MAX_MAX_TOKENS;
pub(crate) const MIN_TIMEOUT_MS: usize = super::sql::MIN_TIMEOUT_MS;
/// The longest JSON text accepted for a filter, projection, sort, or
/// pipeline (the same bound as a SQL statement).
pub(crate) const MAX_ARGUMENT_CHARS: usize = super::sql::MAX_QUERY_CHARS;
/// The largest `skip` accepted.
pub(crate) const MAX_SKIP: usize = compresso_sql::mongo::MAX_SKIP as usize;

pub(crate) fn handles(name: &str) -> bool {
    name == TOOL_NAME
}

/// What the advertised surface says about the MongoDB tool, fixed at startup.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MongoSurface {
    /// `--mongo-env` not given: the tool is not advertised.
    #[default]
    Off,
    /// `--mongo-env` given, but the variable is missing or unusable: the tool
    /// is advertised and every call returns `mongo_not_configured`.
    Unconfigured,
    /// One MongoDB server.
    On,
}

impl MongoSurface {
    /// The surface for a server process's service, if any.
    pub fn of(service: Option<&MongoService>) -> Self {
        match service.map(MongoService::is_configured) {
            None => Self::Off,
            Some(false) => Self::Unconfigured,
            Some(true) => Self::On,
        }
    }

    /// Whether the tool is advertised.
    pub const fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// The message for a call the process does not serve.
pub(crate) fn unavailable(enabled: bool) -> ToolFailure {
    if enabled {
        ToolFailure::new(
            "tool_unavailable",
            "Tool 'compresso_mongo' is not served by the lean tool profile; the operator can serve it from a server started without `--tools lean`.",
        )
    } else {
        ToolFailure::new(
            "tool_unavailable",
            "Tool 'compresso_mongo' is not enabled for this server process; the operator can start the server with `compresso mcp serve --mongo-env COMPRESSO_MONGO_NAME` to use it.",
        )
    }
}

/// A JSON argument (object or array) checked for size.
fn json_argument(
    arguments: &mut Map<String, Value>,
    name: &'static str,
) -> Result<Option<Value>, ToolFailure> {
    let Some(value) = arguments.remove(name) else {
        return Ok(None);
    };
    if !(value.is_object() || value.is_array()) {
        return Err(ToolFailure::invalid(format!(
            "argument '{name}' must be a JSON object or array."
        )));
    }
    if value.to_string().chars().count() > MAX_ARGUMENT_CHARS {
        return Err(ToolFailure::invalid(format!(
            "argument '{name}' is over {MAX_ARGUMENT_CHARS} characters."
        )));
    }
    Ok(Some(value))
}

/// Runs one call and returns the JSON text sent to the client.
pub(crate) fn run(
    service: Option<&MongoService>,
    arguments: Option<JsonObject>,
) -> Result<String, ToolFailure> {
    let Some(service) = service else {
        return Err(unavailable(false));
    };
    let mut arguments = checked_arguments(
        arguments,
        &[
            "action",
            "collection",
            "filter",
            "projection",
            "sort",
            "skip",
            "pipeline",
            "field",
            "max_rows",
            "max_tokens",
            "timeout_ms",
        ],
    )?;
    let action = optional_string(&mut arguments, "action", false)?.ok_or_else(|| {
        ToolFailure::invalid(
            "argument 'action' is required: find, aggregate, count, distinct, collections, or describe.",
        )
    })?;
    let collection = optional_string(&mut arguments, "collection", false)?;
    let field = optional_string(&mut arguments, "field", false)?;
    let filter = json_argument(&mut arguments, "filter")?;
    let projection = json_argument(&mut arguments, "projection")?;
    let sort = json_argument(&mut arguments, "sort")?;
    let pipeline = json_argument(&mut arguments, "pipeline")?;
    let skip = optional_usize(&mut arguments, "skip", 0, MAX_SKIP)?;
    let max_rows =
        optional_usize(&mut arguments, "max_rows", 1, MAX_ROWS)?.unwrap_or(DEFAULT_MAX_ROWS);
    let max_tokens = optional_usize(&mut arguments, "max_tokens", MIN_MAX_TOKENS, MAX_MAX_TOKENS)?
        .unwrap_or(DEFAULT_MAX_TOKENS);
    let timeout_limit = usize::try_from(MAX_TIMEOUT_MS).unwrap_or(usize::MAX);
    let timeout_ms = optional_usize(&mut arguments, "timeout_ms", MIN_TIMEOUT_MS, timeout_limit)?
        .map_or(DEFAULT_TIMEOUT_MS, |value| {
            u64::try_from(value).unwrap_or(MAX_TIMEOUT_MS)
        });
    let empty = || json!({});
    let only = |allowed: &[&str], present: &[(&str, bool)]| -> Result<(), ToolFailure> {
        for (name, given) in present {
            if *given && !allowed.contains(name) {
                return Err(ToolFailure::invalid(format!(
                    "action {action} does not take '{name}'; it takes {}.",
                    if allowed.is_empty() {
                        "no other argument".to_owned()
                    } else {
                        allowed.join(", ")
                    }
                )));
            }
        }
        Ok(())
    };
    let present = [
        ("collection", collection.is_some()),
        ("filter", filter.is_some()),
        ("projection", projection.is_some()),
        ("sort", sort.is_some()),
        ("skip", skip.is_some()),
        ("pipeline", pipeline.is_some()),
        ("field", field.is_some()),
    ];
    let need_collection = || {
        collection
            .clone()
            .ok_or_else(|| ToolFailure::invalid(format!("action {action} needs 'collection'.")))
    };
    let action = match action.as_str() {
        "find" => {
            only(
                &["collection", "filter", "projection", "sort", "skip"],
                &present,
            )?;
            MongoAction::Find {
                collection: need_collection()?,
                filter: filter.unwrap_or_else(empty),
                projection,
                sort,
                skip: skip.map_or(0, |skip| u64::try_from(skip).unwrap_or(u64::MAX)),
            }
        }
        "aggregate" => {
            only(&["collection", "pipeline"], &present)?;
            MongoAction::Aggregate {
                collection: need_collection()?,
                pipeline: pipeline.ok_or_else(|| {
                    ToolFailure::invalid("action aggregate needs 'pipeline' (an array of stages).")
                })?,
            }
        }
        "count" => {
            only(&["collection", "filter"], &present)?;
            MongoAction::Count {
                collection: need_collection()?,
                filter: filter.unwrap_or_else(empty),
            }
        }
        "distinct" => {
            only(&["collection", "field", "filter"], &present)?;
            MongoAction::Distinct {
                collection: need_collection()?,
                field: field
                    .clone()
                    .ok_or_else(|| ToolFailure::invalid("action distinct needs 'field'."))?,
                filter: filter.unwrap_or_else(empty),
            }
        }
        "collections" => {
            only(&[], &present)?;
            MongoAction::Collections
        }
        "describe" => {
            only(&["collection"], &present)?;
            MongoAction::Describe {
                collection: need_collection()?,
            }
        }
        _ => {
            return Err(ToolFailure::invalid(
                "argument 'action' must be one of find, aggregate, count, distinct, collections, describe.",
            ));
        }
    };
    let request = MongoRequest {
        action,
        max_rows,
        timeout: Duration::from_millis(timeout_ms),
    };
    let outcome = service
        .execute(&request)
        .map_err(|error| ToolFailure::new(error.code(), error.message().to_owned()))?;
    let action = match &request.action {
        MongoAction::Find { .. } => "find",
        MongoAction::Aggregate { .. } => "aggregate",
        MongoAction::Count { .. } => "count",
        MongoAction::Distinct { .. } => "distinct",
        MongoAction::Collections => "collections",
        MongoAction::Describe { .. } => "describe",
    };
    Ok(fit(
        outcome,
        action,
        max_tokens,
        counter_for(TokenProvider::OpenAiO200k),
    ))
}

fn payload(outcome: &MongoOutcome, action: &str) -> Value {
    let mut body = match outcome {
        MongoOutcome::Documents(result) => json!(result),
        MongoOutcome::Count(result) => json!(result),
        MongoOutcome::Collections(result) => json!(result),
        MongoOutcome::Describe(result) => json!(result),
    };
    if let Value::Object(object) = &mut body {
        if action == "distinct"
            && let Some(values) = object.remove("documents")
        {
            object.insert("values".to_owned(), values);
        }
        object.insert("engine".to_owned(), Value::from("mongodb"));
        object.insert("action".to_owned(), Value::from(action));
        if object.get("truncation_reason") == Some(&Value::Null) {
            object.remove("truncation_reason");
        }
    }
    body
}

/// The result's JSON, fitted to `max_tokens`: documents (or collections, or
/// described fields) are dropped from the end, and then the first document
/// that does not fit is cut (strings shortened, the rest marked
/// `$truncated`) and kept, so a result with documents never comes back
/// empty. `rows_omitted` counts the documents left out for the budget.
fn fit(
    mut outcome: MongoOutcome,
    action: &str,
    max_tokens: usize,
    counter: &dyn TokenCounter,
) -> String {
    let render = |outcome: &MongoOutcome, omitted: usize| {
        let mut value = payload(outcome, action);
        if omitted > 0
            && let Value::Object(object) = &mut value
        {
            object.insert("rows_omitted".to_owned(), Value::from(omitted));
        }
        value.to_string()
    };
    let fits = |text: &str| counter.count(text) <= max_tokens;
    if fits(&render(&outcome, 0)) {
        return render(&outcome, 0);
    }
    let length = match &outcome {
        MongoOutcome::Documents(result) => result.documents.len(),
        MongoOutcome::Collections(result) => result.collections.len(),
        MongoOutcome::Describe(result) => result.fields.len(),
        MongoOutcome::Count(_) => 0,
    };
    let keep = |outcome: &mut MongoOutcome, count: usize| match outcome {
        MongoOutcome::Documents(result) => result.truncate_to(count, Truncation::MaxTokens),
        MongoOutcome::Collections(result) => {
            result.collections.truncate(count);
            result.truncated = true;
        }
        MongoOutcome::Describe(result) => {
            result.fields_omitted += result.fields.len().saturating_sub(count);
            result.fields.truncate(count);
            result.indexes.clear();
        }
        MongoOutcome::Count(_) => {}
    };
    let (mut low, mut high) = (0, length);
    while low < high {
        let middle = (low + high).div_ceil(2);
        let mut candidate = outcome.clone();
        keep(&mut candidate, middle);
        if fits(&render(&candidate, length - middle)) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let next = match &outcome {
        MongoOutcome::Documents(result) if low < length => Some(result.documents[low].clone()),
        _ => None,
    };
    keep(&mut outcome, low);
    let mut omitted = length - low;
    if let Some(document) = next {
        // The first document that did not fit, cut to what is left.
        let with = |budget: usize| {
            let mut candidate = outcome.clone();
            if let MongoOutcome::Documents(result) = &mut candidate {
                result
                    .documents
                    .push(super::shrink::shrink(&document, budget));
                result.count = result.documents.len();
            }
            candidate
        };
        let limit = document.to_string().len();
        if let Some(budget) =
            super::shrink::largest_budget(limit, |budget| fits(&render(&with(budget), omitted - 1)))
        {
            outcome = with(budget);
            omitted -= 1;
        }
    }
    let mut value: Value = serde_json::from_str(&render(&outcome, omitted)).unwrap_or(Value::Null);
    if let (MongoOutcome::Describe(_), Value::Object(object)) = (&outcome, &mut value) {
        object.insert("truncated".to_owned(), Value::Bool(true));
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use compresso_sql::mongo::DocumentsResult;

    #[test]
    fn test_fit_drops_trailing_documents_and_says_why() {
        let counter = counter_for(TokenProvider::OpenAiO200k);
        let result = DocumentsResult {
            documents: (0..200)
                .map(|index| json!({"body": format!("document {index} with some words in it")}))
                .collect(),
            count: 200,
            truncated: false,
            truncation_reason: None,
            elapsed_ms: 3,
        };
        let text = fit(MongoOutcome::Documents(result), "find", 400, counter);
        assert!(counter.count(&text) <= 400, "{text}");
        let value: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(value["truncation_reason"], "max_tokens");
        assert_eq!(value["truncated"], true);
        assert_eq!(value["engine"], "mongodb");
        let documents = value["documents"].as_array().expect("documents").len();
        assert!(documents > 0 && documents < 200);
        assert_eq!(value["count"], documents);
    }

    #[test]
    fn test_distinct_values_are_named_values() {
        let counter = counter_for(TokenProvider::OpenAiO200k);
        let result = DocumentsResult {
            documents: vec![json!("a"), json!("b")],
            count: 2,
            truncated: false,
            truncation_reason: None,
            elapsed_ms: 1,
        };
        let text = fit(MongoOutcome::Documents(result), "distinct", 2_000, counter);
        let value: Value = serde_json::from_str(&text).expect("json");
        assert_eq!(value["values"], json!(["a", "b"]));
        assert!(value.get("documents").is_none());
    }

    fn documents(values: Vec<Value>) -> MongoOutcome {
        MongoOutcome::Documents(DocumentsResult {
            count: values.len(),
            documents: values,
            truncated: false,
            truncation_reason: None,
            elapsed_ms: 1,
        })
    }

    fn big(id: i64) -> Value {
        json!({"_id": id, "s": "\\u0001".repeat(4_000), "list": (0..400).map(|n| format!("item {n} {}", "x".repeat(40))).collect::<Vec<_>>()})
    }

    /// The third review's shapes: one huge document; a huge one between
    /// small ones; a huge one last. A result never comes back empty, the
    /// cut document is marked, and the documents left out are counted.
    #[test]
    fn test_fit_cuts_a_huge_document_instead_of_dropping_it() {
        let counter = counter_for(TokenProvider::OpenAiO200k);
        for (values, max_tokens, kept_ids, omitted) in [
            (vec![big(2)], 8_000, vec![2], 0),
            (vec![big(2)], 2_000, vec![2], 0),
            (
                vec![json!({"_id": 1}), big(2), json!({"_id": 3})],
                2_000,
                vec![1, 2],
                1,
            ),
            (
                vec![json!({"_id": 1}), json!({"_id": 3}), big(2)],
                2_000,
                vec![1, 3, 2],
                0,
            ),
        ] {
            let text = fit(documents(values), "find", max_tokens, counter);
            assert!(
                counter.count(&text) <= max_tokens,
                "{}",
                counter.count(&text)
            );
            let value: Value = serde_json::from_str(&text).expect("valid JSON");
            let ids = value["documents"]
                .as_array()
                .expect("documents")
                .iter()
                .map(|document| document["_id"].as_i64().expect("id"))
                .collect::<Vec<_>>();
            assert_eq!(ids, kept_ids, "{text}");
            assert_eq!(value["count"], kept_ids.len());
            assert!(text.contains("\"$truncated\":true"), "{text}");
            assert_eq!(value["truncation_reason"], "max_tokens");
            assert_eq!(
                value
                    .get("rows_omitted")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                omitted
            );
        }
    }
}
