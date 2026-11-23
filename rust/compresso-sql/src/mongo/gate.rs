//! The allow-list every `compresso_mongo` argument passes before any network
//! call (E17-52, D117, ADR-0038).
//!
//! MongoDB has no read-only session, so this allow-list and the account check
//! are the control, not defence in depth. The tool builds each driver command
//! itself from typed arguments; this module turns the JSON a model sends into
//! BSON by hand, accepting only:
//!
//! - field names that do not start with `$` and hold no NUL;
//! - `$` keys on the allow-list for the place they appear (query operators in
//!   a filter, expression and accumulator operators inside stages, the stage
//!   names at the top of a pipeline);
//! - the two literal forms `{"$oid": "<24 hex>"}` and `{"$date": "<RFC 3339>"
//!   | <milliseconds>}`, which become an object id and a date.
//!
//! Every other `$` key is refused at every depth, by name when it is a known
//! writer or JavaScript runner. No JSON form produces a JavaScript value: no
//! extended-JSON parser is used, and the finished BSON is walked once more to
//! refuse JavaScript, symbol, and database-pointer values. Depth and size are
//! bounded.

use std::collections::BTreeSet;

use bson::{Bson, DateTime, Document, oid::ObjectId};
use serde_json::{Map, Value};

use crate::SqlError;

/// Deepest nesting of objects and arrays accepted in one argument.
pub const MAX_DEPTH: usize = 24;
/// Most JSON values (objects, arrays, scalars) accepted in one argument.
pub const MAX_NODES: usize = 10_000;
/// Most stages in one pipeline, nested pipelines included.
pub const MAX_STAGES: usize = 50;
/// Longest collection name accepted.
pub const MAX_COLLECTION_CHARS: usize = 120;
/// Longest field name or path accepted.
pub const MAX_FIELD_CHARS: usize = 256;

/// Query operators accepted in a filter (and in `$match`).
pub const QUERY_OPERATORS: &[&str] = &[
    "$all",
    "$and",
    "$elemMatch",
    "$eq",
    "$exists",
    "$expr",
    "$gt",
    "$gte",
    "$in",
    "$lt",
    "$lte",
    "$mod",
    "$ne",
    "$nin",
    "$nor",
    "$not",
    "$options",
    "$or",
    "$regex",
    "$size",
    "$type",
];

/// Projection operators accepted in a `find` projection, beside inclusion and
/// exclusion.
pub const PROJECTION_OPERATORS: &[&str] = &["$elemMatch", "$slice"];

/// Aggregation expression operators accepted in `$expr` and inside stages.
pub const EXPRESSION_OPERATORS: &[&str] = &[
    "$abs",
    "$add",
    "$allElementsTrue",
    "$and",
    "$anyElementTrue",
    "$arrayElemAt",
    "$arrayToObject",
    "$ceil",
    "$cmp",
    "$concat",
    "$concatArrays",
    "$cond",
    "$convert",
    "$dateAdd",
    "$dateDiff",
    "$dateSubtract",
    "$dateToString",
    "$dateTrunc",
    "$dayOfMonth",
    "$dayOfWeek",
    "$dayOfYear",
    "$divide",
    "$eq",
    "$exp",
    "$filter",
    "$first",
    "$floor",
    "$gt",
    "$gte",
    "$hour",
    "$ifNull",
    "$in",
    "$indexOfArray",
    "$indexOfCP",
    "$isArray",
    "$isNumber",
    "$last",
    "$let",
    "$literal",
    "$ln",
    "$log",
    "$log10",
    "$lt",
    "$lte",
    "$ltrim",
    "$map",
    "$max",
    "$mergeObjects",
    "$millisecond",
    "$min",
    "$minute",
    "$mod",
    "$month",
    "$multiply",
    "$ne",
    "$not",
    "$objectToArray",
    "$or",
    "$pow",
    "$reduce",
    "$regexMatch",
    "$reverseArray",
    "$round",
    "$rtrim",
    "$second",
    "$setDifference",
    "$setIntersection",
    "$setIsSubset",
    "$setUnion",
    "$size",
    "$slice",
    "$split",
    "$sqrt",
    "$strLenCP",
    "$strcasecmp",
    "$substrCP",
    "$subtract",
    "$sum",
    "$avg",
    "$switch",
    "$toBool",
    "$toDate",
    "$toDecimal",
    "$toDouble",
    "$toInt",
    "$toLong",
    "$toLower",
    "$toObjectId",
    "$toString",
    "$toUpper",
    "$trim",
    "$trunc",
    "$type",
    "$week",
    "$year",
];

/// Accumulators accepted in `$group`, `$bucket`, and `$bucketAuto` outputs.
pub const ACCUMULATORS: &[&str] = &[
    "$addToSet",
    "$avg",
    "$count",
    "$first",
    "$last",
    "$max",
    "$min",
    "$push",
    "$stdDevPop",
    "$stdDevSamp",
    "$sum",
];

/// Pipeline stages accepted. `$lookup` only with `from` naming a collection
/// of the same database; `$facet` only one level deep.
pub const STAGES: &[&str] = &[
    "$addFields",
    "$bucket",
    "$bucketAuto",
    "$count",
    "$facet",
    "$group",
    "$limit",
    "$lookup",
    "$match",
    "$project",
    "$replaceRoot",
    "$replaceWith",
    "$sample",
    "$set",
    "$skip",
    "$sort",
    "$sortByCount",
    "$unset",
    "$unwind",
];

/// Keys refused by name because they run JavaScript on the server.
pub const JAVASCRIPT_OPERATORS: &[&str] = &["$where", "$function", "$accumulator"];
/// Stages refused by name because they write.
pub const WRITING_STAGES: &[&str] = &["$out", "$merge"];
/// Stages refused by name because they read beyond the configured database
/// or report on the server rather than the data.
pub const ADMIN_STAGES: &[&str] = &[
    "$changeStream",
    "$changeStreamSplitLargeEvent",
    "$collStats",
    "$currentOp",
    "$documents",
    "$graphLookup",
    "$indexStats",
    "$listLocalSessions",
    "$listSampledQueries",
    "$listSearchIndexes",
    "$listSessions",
    "$planCacheStats",
    "$querySettings",
    "$queryStats",
    "$shardedDataDistribution",
    "$unionWith",
];

/// A refusal from this gate, before any network call.
pub(crate) fn refused(message: impl Into<String>) -> SqlError {
    SqlError::new("mongo_refused", message)
}

/// Why a `$` key is refused, naming it only when it is on a known list.
fn refusal_for(key: &str, place: &str) -> SqlError {
    if JAVASCRIPT_OPERATORS.contains(&key) {
        return refused(format!(
            "{key} runs JavaScript on the server and is never sent."
        ));
    }
    if WRITING_STAGES.contains(&key) {
        return refused(format!("{key} writes to the database and is never sent."));
    }
    if ADMIN_STAGES.contains(&key) {
        return refused(format!(
            "{key} reads beyond the configured collections or reports on the server and is never sent."
        ));
    }
    let shown = if key.chars().count() <= 40
        && key
            .chars()
            .skip(1)
            .all(|character| character.is_ascii_alphanumeric())
    {
        key.to_owned()
    } else {
        "this key".to_owned()
    };
    refused(format!(
        "{shown} is not on the allow-list for {place}; rewrite it with the operators the tool description lists."
    ))
}

/// The operator sets one value may use.
#[derive(Clone, Copy)]
struct Allowed {
    sets: &'static [&'static [&'static str]],
    place: &'static str,
}

impl Allowed {
    fn contains(self, key: &str) -> bool {
        self.sets.iter().any(|set| set.contains(&key))
    }
}

const FILTER: Allowed = Allowed {
    sets: &[QUERY_OPERATORS, EXPRESSION_OPERATORS],
    place: "a filter",
};
const PROJECTION: Allowed = Allowed {
    sets: &[PROJECTION_OPERATORS, QUERY_OPERATORS],
    place: "a projection",
};
const STAGE_BODY: Allowed = Allowed {
    sets: &[QUERY_OPERATORS, EXPRESSION_OPERATORS, ACCUMULATORS],
    place: "a pipeline stage",
};
const LITERAL: Allowed = Allowed {
    sets: &[],
    place: "a value",
};

/// Counts what one argument has used against the size bounds.
struct Budget {
    nodes: usize,
    stages: usize,
}

impl Budget {
    fn new() -> Self {
        Self {
            nodes: 0,
            stages: 0,
        }
    }

    fn visit(&mut self, depth: usize) -> Result<(), SqlError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(refused(format!(
                "the argument holds more than {MAX_NODES} values; send a smaller one."
            )));
        }
        if depth > MAX_DEPTH {
            return Err(refused(format!(
                "the argument nests deeper than {MAX_DEPTH} levels; flatten it."
            )));
        }
        Ok(())
    }
}

/// A field name or path: not empty, no leading `$`, no NUL, bounded.
pub(crate) fn check_field(name: &str) -> Result<(), SqlError> {
    if name.is_empty() || name.chars().count() > MAX_FIELD_CHARS {
        return Err(refused(format!(
            "a field name must have 1 to {MAX_FIELD_CHARS} characters."
        )));
    }
    if name.starts_with('$') {
        return Err(refusal_for(name, "a field name"));
    }
    if name.contains('\0') {
        return Err(refused("a field name may not contain NUL."));
    }
    Ok(())
}

/// A collection of the configured database: 1 to 120 characters, no `$`, no
/// NUL, and not a `system.` collection.
pub(crate) fn check_collection(name: &str) -> Result<(), SqlError> {
    if name.is_empty() || name.chars().count() > MAX_COLLECTION_CHARS {
        return Err(refused(format!(
            "collection must name one collection in 1 to {MAX_COLLECTION_CHARS} characters."
        )));
    }
    if name.contains('$') || name.contains('\0') || name.chars().any(char::is_control) {
        return Err(refused(
            "collection names with $, NUL, or control characters are refused.",
        ));
    }
    if name.starts_with("system.") || name.starts_with('.') {
        return Err(refused(
            "system collections are refused; name a data collection.",
        ));
    }
    Ok(())
}

/// A `find`, `count`, or `distinct` filter (an object; `{}` matches all).
pub(crate) fn filter(value: &Value) -> Result<Document, SqlError> {
    let mut budget = Budget::new();
    let Value::Object(object) = value else {
        return Err(refused("filter must be a JSON object."));
    };
    document(object, FILTER, 0, &mut budget)
}

/// A `find` projection: fields with 0, 1, true, or false, `$slice`, or
/// `$elemMatch`.
pub(crate) fn projection(value: &Value) -> Result<Document, SqlError> {
    let mut budget = Budget::new();
    let Value::Object(object) = value else {
        return Err(refused("projection must be a JSON object."));
    };
    for (key, field) in object {
        check_field(key)?;
        match field {
            Value::Bool(_) => {}
            Value::Number(number) if matches!(number.as_i64(), Some(0 | 1)) => {}
            Value::Object(_) => {}
            _ => {
                return Err(refused(
                    "a projection value must be 0, 1, true, false, $slice, or $elemMatch.",
                ));
            }
        }
    }
    document(object, PROJECTION, 0, &mut budget)
}

/// A sort: fields with 1 or -1 only.
pub(crate) fn sort(value: &Value) -> Result<Document, SqlError> {
    let Value::Object(object) = value else {
        return Err(refused("sort must be a JSON object."));
    };
    if object.len() > 32 {
        return Err(refused("sort may name at most 32 fields."));
    }
    let mut sorted = Document::new();
    for (key, direction) in object {
        check_field(key)?;
        match direction.as_i64() {
            Some(direction @ (1 | -1)) => {
                sorted.insert(key.clone(), Bson::Int32(direction as i32));
            }
            _ => return Err(refused("a sort direction must be 1 or -1.")),
        }
    }
    Ok(sorted)
}

/// An aggregation pipeline: an array of stages from [`STAGES`].
pub(crate) fn pipeline(value: &Value) -> Result<Vec<Document>, SqlError> {
    let mut budget = Budget::new();
    let stages = stages(value, 0, true, &mut budget)?;
    Ok(stages)
}

fn stages(
    value: &Value,
    depth: usize,
    allow_facet: bool,
    budget: &mut Budget,
) -> Result<Vec<Document>, SqlError> {
    let Value::Array(items) = value else {
        return Err(refused("pipeline must be a JSON array of stages."));
    };
    budget.visit(depth)?;
    let mut built = Vec::with_capacity(items.len());
    for item in items {
        budget.stages += 1;
        if budget.stages > MAX_STAGES {
            return Err(refused(format!(
                "a pipeline may hold at most {MAX_STAGES} stages, nested ones included."
            )));
        }
        let Value::Object(stage) = item else {
            return Err(refused("each pipeline stage must be an object."));
        };
        let mut entries = stage.iter();
        let (Some((name, body)), None) = (entries.next(), entries.next()) else {
            return Err(refused("each pipeline stage must hold exactly one key."));
        };
        if !STAGES.contains(&name.as_str()) || (name == "$facet" && !allow_facet) {
            if name == "$facet" {
                return Err(refused("$facet may not be nested inside $facet."));
            }
            return Err(refusal_for(name, "a pipeline"));
        }
        let body = match name.as_str() {
            "$facet" => facet(body, depth + 1, budget)?,
            "$lookup" => lookup(body, depth + 1, allow_facet, budget)?,
            "$limit" | "$skip" => match body.as_i64() {
                Some(count) if count >= 0 => Bson::Int64(count),
                _ => {
                    return Err(refused(format!("{name} takes a non-negative integer.")));
                }
            },
            "$sort" => Bson::Document(sort(body)?),
            "$sample" => sample(body)?,
            _ => value_to_bson(body, STAGE_BODY, depth + 1, budget)?,
        };
        let mut document = Document::new();
        document.insert(name.clone(), body);
        built.push(document);
    }
    Ok(built)
}

fn facet(body: &Value, depth: usize, budget: &mut Budget) -> Result<Bson, SqlError> {
    let Value::Object(outputs) = body else {
        return Err(refused("$facet takes an object of named pipelines."));
    };
    budget.visit(depth)?;
    let mut built = Document::new();
    for (name, pipeline) in outputs {
        check_field(name)?;
        let stages = stages(pipeline, depth + 1, false, budget)?;
        built.insert(
            name.clone(),
            Bson::Array(stages.into_iter().map(Bson::Document).collect()),
        );
    }
    Ok(Bson::Document(built))
}

/// `$lookup` against a collection of the same database: `from` must be a
/// plain collection name (the object form can name another database).
fn lookup(
    body: &Value,
    depth: usize,
    allow_facet: bool,
    budget: &mut Budget,
) -> Result<Bson, SqlError> {
    let Value::Object(fields) = body else {
        return Err(refused("$lookup takes an object."));
    };
    budget.visit(depth)?;
    let mut built = Document::new();
    for (key, value) in fields {
        match key.as_str() {
            "from" => {
                let Value::String(from) = value else {
                    return Err(refused(
                        "$lookup from must name a collection of the configured database as a string.",
                    ));
                };
                check_collection(from)?;
                built.insert("from", from.clone());
            }
            "localField" | "foreignField" | "as" => {
                let Value::String(field) = value else {
                    return Err(refused(format!("$lookup {key} must be a string.")));
                };
                check_field(field)?;
                built.insert(key.clone(), field.clone());
            }
            "let" => {
                built.insert("let", value_to_bson(value, STAGE_BODY, depth + 1, budget)?);
            }
            "pipeline" => {
                let stages = stages(value, depth + 1, allow_facet, budget)?;
                built.insert(
                    "pipeline",
                    Bson::Array(stages.into_iter().map(Bson::Document).collect()),
                );
            }
            _ => {
                return Err(refused(
                    "$lookup accepts only from, localField, foreignField, as, let, and pipeline.",
                ));
            }
        }
    }
    if !built.contains_key("from") {
        return Err(refused(
            "$lookup needs from (a collection of the configured database).",
        ));
    }
    Ok(Bson::Document(built))
}

fn sample(body: &Value) -> Result<Bson, SqlError> {
    let size = body
        .as_object()
        .filter(|fields| fields.len() == 1)
        .and_then(|fields| fields.get("size"))
        .and_then(Value::as_i64)
        .filter(|size| *size > 0);
    match size {
        Some(size) => {
            let mut built = Document::new();
            built.insert("size", Bson::Int64(size));
            Ok(Bson::Document(built))
        }
        None => Err(refused("$sample takes {\"size\": <positive integer>}.")),
    }
}

fn document(
    object: &Map<String, Value>,
    allowed: Allowed,
    depth: usize,
    budget: &mut Budget,
) -> Result<Document, SqlError> {
    budget.visit(depth)?;
    let mut built = Document::new();
    for (key, value) in object {
        if key.starts_with('$') {
            if !allowed.contains(key) {
                return Err(refusal_for(key, allowed.place));
            }
        } else {
            check_field(key)?;
        }
        built.insert(
            key.clone(),
            value_to_bson(value, allowed, depth + 1, budget)?,
        );
    }
    Ok(built)
}

/// One JSON value as BSON. Objects whose only key is `$oid` or `$date` are
/// literals; every other object is checked key by key.
fn value_to_bson(
    value: &Value,
    allowed: Allowed,
    depth: usize,
    budget: &mut Budget,
) -> Result<Bson, SqlError> {
    match value {
        Value::Null => {
            budget.visit(depth)?;
            Ok(Bson::Null)
        }
        Value::Bool(flag) => {
            budget.visit(depth)?;
            Ok(Bson::Boolean(*flag))
        }
        Value::Number(number) => {
            budget.visit(depth)?;
            if let Some(integer) = number.as_i64() {
                Ok(i32::try_from(integer).map_or(Bson::Int64(integer), Bson::Int32))
            } else if number.is_u64() {
                Err(refused(
                    "an integer is larger than a 64-bit signed integer.",
                ))
            } else {
                number
                    .as_f64()
                    .map(Bson::Double)
                    .ok_or_else(|| refused("a number could not be read."))
            }
        }
        Value::String(text) => {
            budget.visit(depth)?;
            Ok(Bson::String(text.clone()))
        }
        Value::Array(items) => {
            budget.visit(depth)?;
            items
                .iter()
                .map(|item| value_to_bson(item, allowed, depth + 1, budget))
                .collect::<Result<Vec<_>, _>>()
                .map(Bson::Array)
        }
        Value::Object(object) => {
            if let Some(literal) = typed_literal(object)? {
                budget.visit(depth)?;
                return Ok(literal);
            }
            // `$literal` holds data, not operators: its content is checked as
            // a plain value, so it cannot smuggle a key past the list.
            if let Some(content) = object.get("$literal")
                && object.len() == 1
                && allowed.contains("$literal")
            {
                budget.visit(depth)?;
                let mut built = Document::new();
                built.insert(
                    "$literal",
                    value_to_bson(content, LITERAL, depth + 1, budget)?,
                );
                return Ok(Bson::Document(built));
            }
            document(object, allowed, depth, budget).map(Bson::Document)
        }
    }
}

/// `{"$oid": "<24 hex>"}` and `{"$date": "<RFC 3339>" | <milliseconds>}`.
fn typed_literal(object: &Map<String, Value>) -> Result<Option<Bson>, SqlError> {
    if object.len() != 1 {
        return Ok(None);
    }
    if let Some(value) = object.get("$oid") {
        return match value {
            Value::String(hex) => ObjectId::parse_str(hex)
                .map(|id| Some(Bson::ObjectId(id)))
                .map_err(|_| refused("$oid takes a 24-character hexadecimal string.")),
            _ => Err(refused("$oid takes a 24-character hexadecimal string.")),
        };
    }
    if let Some(value) = object.get("$date") {
        return match value {
            Value::String(text) => DateTime::parse_rfc3339_str(text)
                .map(|date| Some(Bson::DateTime(date)))
                .map_err(|_| refused("$date takes an RFC 3339 string or milliseconds.")),
            Value::Number(number) => number
                .as_i64()
                .map(|millis| Some(Bson::DateTime(DateTime::from_millis(millis))))
                .ok_or_else(|| refused("$date takes an RFC 3339 string or milliseconds.")),
            _ => Err(refused("$date takes an RFC 3339 string or milliseconds.")),
        };
    }
    Ok(None)
}

/// Refuses any JavaScript, symbol, or database-pointer value, and any `$` key
/// off every list, anywhere in a finished document. The converter above
/// cannot produce them; this is the check on what is actually sent.
pub(crate) fn assert_sendable(document: &Document) -> Result<(), SqlError> {
    let known: BTreeSet<&str> = QUERY_OPERATORS
        .iter()
        .chain(PROJECTION_OPERATORS)
        .chain(EXPRESSION_OPERATORS)
        .chain(ACCUMULATORS)
        .chain(STAGES)
        .copied()
        .collect();
    fn walk(value: &Bson, known: &BTreeSet<&str>, depth: usize) -> Result<(), SqlError> {
        if depth > MAX_DEPTH * 2 + 4 {
            return Err(refused("the command nests too deeply."));
        }
        match value {
            Bson::JavaScriptCode(_)
            | Bson::JavaScriptCodeWithScope(_)
            | Bson::Symbol(_)
            | Bson::DbPointer(_) => Err(refused(
                "JavaScript, symbol, and database-pointer values are never sent.",
            )),
            Bson::Document(document) => {
                for (key, value) in document {
                    let key = key.as_str();
                    if key.starts_with('$') && !known.contains(key) {
                        return Err(refusal_for(key, "a command"));
                    }
                    walk(value, known, depth + 1)?;
                }
                Ok(())
            }
            Bson::Array(items) => items
                .iter()
                .try_for_each(|item| walk(item, known, depth + 1)),
            _ => Ok(()),
        }
    }
    walk(&Bson::Document(document.clone()), &known, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn code(result: Result<impl std::fmt::Debug, SqlError>) -> String {
        match result {
            Ok(value) => panic!("expected a refusal, got {value:?}"),
            Err(error) => format!("[{}] {}", error.code(), error.message()),
        }
    }

    #[test]
    fn test_plain_filters_become_bson() {
        let built = filter(&json!({
            "status": "open",
            "n": {"$gte": 2, "$lt": 5_000_000_000_i64},
            "$or": [{"a": 1}, {"b": {"$in": [1, 2.5, null, true]}}],
            "created": {"$gt": {"$date": "2024-01-02T03:04:05Z"}},
            "_id": {"$oid": "65a1b2c3d4e5f60718293a4b"},
            "$expr": {"$gt": ["$a", {"$add": ["$b", 1]}]}
        }))
        .expect("allowed");
        assert_eq!(built.get_str("status").unwrap(), "open");
        let n = built.get_document("n").unwrap();
        assert_eq!(n.get("$gte"), Some(&Bson::Int32(2)));
        assert_eq!(n.get("$lt"), Some(&Bson::Int64(5_000_000_000)));
        assert!(matches!(
            built.get_document("created").unwrap().get("$gt"),
            Some(Bson::DateTime(_))
        ));
        assert!(matches!(built.get("_id"), Some(Bson::ObjectId(_))));
        assert_sendable(&built).expect("sendable");
    }

    #[test]
    fn test_javascript_operators_are_refused_at_every_depth() {
        for value in [
            json!({"$where": "sleep(100)"}),
            json!({"a": {"$where": "1"}}),
            json!({"$or": [{"a": 1}, {"$where": "1"}]}),
            json!({"$expr": {"$function": {"body": "function(){}", "args": [], "lang": "js"}}}),
            json!({"$expr": {"$and": [{"$eq": [1, 1]}, {"$function": {"body": "x", "args": [], "lang": "js"}}]}}),
            json!({"a": {"$elemMatch": {"$where": "1"}}}),
        ] {
            let text = code(filter(&value));
            assert!(text.starts_with("[mongo_refused]"), "{text}");
            assert!(text.contains("JavaScript"), "{text}");
        }
        let text = code(pipeline(&json!([
            {"$group": {"_id": null, "x": {"$accumulator": {"init": "function(){}"}}}}
        ])));
        assert!(text.contains("$accumulator runs JavaScript"), "{text}");
        let text = code(pipeline(&json!([
            {"$facet": {"a": [{"$match": {"$where": "1"}}]}}
        ])));
        assert!(text.contains("$where runs JavaScript"), "{text}");
    }

    #[test]
    fn test_unknown_dollar_keys_are_refused_not_passed_through() {
        for value in [
            json!({"$text": {"$search": "x"}}),
            json!({"$jsonSchema": {}}),
            json!({"a": {"$near": [0, 0]}}),
            json!({"a": {"$bitsAllSet": 1}}),
            json!({"$comment": "x"}),
            json!({"a": {"$code": "function(){}"}}),
            json!({"a": {"$numberLong": "1"}}),
            json!({"a": {"$binary": {"base64": "", "subType": "00"}}}),
            json!({"a": {"$symbol": "x"}}),
            json!({"$expr": {"$rand": {}}}),
            json!({"a": {"$oid": "65a1b2c3d4e5f60718293a4b", "$x": 1}}),
        ] {
            let text = code(filter(&value));
            assert!(text.starts_with("[mongo_refused]"), "{value} {text}");
        }
        // A literal never hides an operator either.
        let text = code(pipeline(&json!([
            {"$project": {"x": {"$literal": {"$function": "x"}}}}
        ])));
        assert!(text.contains("$function runs JavaScript"), "{text}");
    }

    #[test]
    fn test_writing_admin_and_cross_database_stages_are_refused() {
        for stage in [
            json!({"$out": "copy"}),
            json!({"$out": {"db": "other", "coll": "copy"}}),
            json!({"$merge": {"into": "copy"}}),
            json!({"$currentOp": {}}),
            json!({"$listSessions": {}}),
            json!({"$listLocalSessions": {}}),
            json!({"$collStats": {"count": {}}}),
            json!({"$indexStats": {}}),
            json!({"$planCacheStats": {}}),
            json!({"$documents": [{"a": 1}]}),
            json!({"$changeStream": {}}),
            json!({"$graphLookup": {"from": "a"}}),
            json!({"$unionWith": "other"}),
            json!({"$geoNear": {}}),
            json!({"$search": {}}),
            json!({"$densify": {}}),
            json!({"$setWindowFields": {}}),
        ] {
            let text = code(pipeline(&json!([{"$match": {}}, stage.clone()])));
            assert!(text.starts_with("[mongo_refused]"), "{stage} {text}");
        }
        let text = code(pipeline(&json!([{"$out": "copy"}])));
        assert!(text.contains("$out writes"), "{text}");
        let text = code(pipeline(&json!([{"$merge": {"into": "copy"}}])));
        assert!(text.contains("$merge writes"), "{text}");
        // $out hidden in a nested pipeline.
        for nested in [
            json!([{"$facet": {"a": [{"$out": "copy"}]}}]),
            json!([{"$lookup": {"from": "b", "as": "c", "pipeline": [{"$merge": {"into": "x"}}]}}]),
            json!([{"$lookup": {"from": "b", "as": "c", "pipeline": [{"$lookup": {"from": "c", "as": "d", "pipeline": [{"$out": "x"}]}}]}}]),
        ] {
            let text = code(pipeline(&nested));
            assert!(text.contains("writes"), "{nested} {text}");
        }
        // $lookup to another database: `from` as an object is refused.
        let text = code(pipeline(&json!([
            {"$lookup": {"from": {"db": "admin", "coll": "system.users"}, "as": "u", "pipeline": []}}
        ])));
        assert!(text.contains("configured database"), "{text}");
        let text = code(pipeline(&json!([
            {"$lookup": {"from": "system.users", "as": "u", "localField": "a", "foreignField": "b"}}
        ])));
        assert!(text.contains("system collections"), "{text}");
        let text = code(pipeline(&json!([{"$facet": {"a": [{"$facet": {}}]}}])));
        assert!(text.contains("nested"), "{text}");
    }

    #[test]
    fn test_allowed_pipelines_are_built_in_order() {
        let stages = pipeline(&json!([
            {"$match": {"status": {"$in": ["a", "b"]}}},
            {"$lookup": {"from": "orders", "localField": "_id", "foreignField": "customer", "as": "orders"}},
            {"$unwind": {"path": "$orders", "preserveNullAndEmptyArrays": true}},
            {"$group": {"_id": "$status", "total": {"$sum": "$orders.amount"}, "n": {"$count": {}}}},
            {"$addFields": {"big": {"$cond": [{"$gt": ["$total", 10]}, true, false]}}},
            {"$sort": {"total": -1}},
            {"$skip": 0},
            {"$limit": 5},
            {"$facet": {"top": [{"$limit": 1}], "count": [{"$count": "n"}]}},
            {"$sample": {"size": 3}}
        ]))
        .expect("allowed");
        assert_eq!(stages.len(), 10);
        assert_eq!(stages[0].keys().next().unwrap(), "$match");
        assert_eq!(stages[7].get("$limit"), Some(&Bson::Int64(5)));
        for stage in &stages {
            assert_sendable(stage).expect("sendable");
        }
    }

    #[test]
    fn test_shapes_bounds_and_names_are_enforced() {
        for (value, needle) in [
            (json!({"$limit": -1}), "non-negative"),
            (json!({"$limit": "5"}), "non-negative"),
            (json!({"$sort": {"a": 2}}), "1 or -1"),
            (json!({"$sort": {"a": {"$meta": "textScore"}}}), "1 or -1"),
            (json!({"$sample": {"size": 0}}), "$sample"),
            (json!({"$match": {}, "$limit": 1}), "exactly one key"),
        ] {
            let text = code(pipeline(&json!([value])));
            assert!(text.contains(needle), "{text}");
        }
        assert!(code(pipeline(&json!({"$match": {}}))).contains("array"));
        assert!(code(filter(&json!([]))).contains("object"));
        assert!(code(sort(&json!({"$natural": 1}))).starts_with("[mongo_refused]"));
        assert!(code(projection(&json!({"a": 2}))).contains("projection value"));
        assert!(code(projection(&json!({"a": {"$function": "x"}}))).contains("JavaScript"));
        projection(&json!({"a": 1, "b": false, "c": {"$slice": [1, 2]}})).expect("allowed");

        let mut deep = json!(1);
        for _ in 0..(MAX_DEPTH + 2) {
            deep = json!({"a": deep});
        }
        assert!(code(filter(&deep)).contains("deeper"));
        let wide = Value::Array((0..MAX_NODES + 1).map(Value::from).collect());
        assert!(code(filter(&json!({"a": {"$in": wide}}))).contains("more than"));
        let many = Value::Array((0..=MAX_STAGES).map(|_| json!({"$limit": 1})).collect());
        assert!(code(pipeline(&many)).contains("at most"));
        assert!(code(filter(&json!({"a": u64::MAX}))).contains("64-bit"));
        assert!(code(filter(&json!({"a\u{0}b": 1}))).contains("NUL"));

        for bad in ["", "system.users", "system.js", "a$b", "a\u{0}", ".x"] {
            assert!(check_collection(bad).is_err(), "{bad:?}");
        }
        check_collection("orders.archive").expect("dots are allowed");
        assert!(check_field("$a").is_err());
        check_field("a.b").expect("paths are allowed");
    }

    #[test]
    fn test_the_final_walk_refuses_javascript_values() {
        let mut document = Document::new();
        document.insert("a", Bson::JavaScriptCode("function(){}".to_owned()));
        assert!(code(assert_sendable(&document)).contains("JavaScript"));
        let mut document = Document::new();
        document.insert("$where", Bson::String("1".to_owned()));
        assert!(code(assert_sendable(&document)).contains("JavaScript"));
        let mut nested = Document::new();
        nested.insert("$out", "x");
        let mut document = Document::new();
        document.insert("a", Bson::Array(vec![Bson::Document(nested)]));
        assert!(code(assert_sendable(&document)).contains("writes"));
    }
}
