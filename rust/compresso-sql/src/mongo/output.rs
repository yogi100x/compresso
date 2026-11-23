//! Bounding one MongoDB result: documents as JSON, with the SQL tool's cell,
//! name, and byte caps and its escaping of hidden characters (`cells.rs`).

use bson::{Bson, Document};
use serde_json::{Map, Value};

use crate::Truncation;
use crate::cells::{self, MAX_COLUMNS, MAX_NAME_CHARS, MAX_RESULT_BYTES};

/// Nesting kept in a returned document; deeper values show as a marker.
pub const MAX_OUTPUT_DEPTH: usize = 8;

/// One BSON value as bounded JSON: text cut to [`MAX_CELL_CHARS`], names to
/// [`MAX_NAME_CHARS`], at most [`MAX_COLUMNS`] fields or items per level
/// (the rest counted), binary shown as its size, JavaScript as a marker.
pub(crate) fn render(value: &Bson) -> Value {
    render_at(value, 0)
}

fn render_at(value: &Bson, depth: usize) -> Value {
    match value {
        Bson::Double(number) => serde_json::Number::from_f64(*number)
            .map_or_else(|| Value::from(number.to_string()), Value::Number),
        Bson::String(text) | Bson::Symbol(text) => cells::text(text),
        Bson::Boolean(flag) => Value::Bool(*flag),
        Bson::Null => Value::Null,
        Bson::Undefined => Value::from("<undefined>"),
        Bson::Int32(number) => Value::from(*number),
        Bson::Int64(number) => Value::from(*number),
        Bson::Decimal128(number) => Value::from(number.to_string()),
        Bson::ObjectId(id) => object([("$oid", Value::from(id.to_hex()))]),
        Bson::DateTime(date) => object([(
            "$date",
            date.try_to_rfc3339_string()
                .map_or_else(|_| Value::from(date.timestamp_millis()), Value::from),
        )]),
        Bson::Timestamp(timestamp) => object([(
            "$timestamp",
            object([
                ("t", Value::from(timestamp.time)),
                ("i", Value::from(timestamp.increment)),
            ]),
        )]),
        Bson::Binary(binary) => cells::binary(binary.bytes.len()),
        Bson::RegularExpression(regex) => object([
            ("$regex", cells::text(regex.pattern.as_str())),
            ("$options", cells::text(regex.options.as_str())),
        ]),
        Bson::JavaScriptCode(_) | Bson::JavaScriptCodeWithScope(_) => Value::from("<javascript>"),
        Bson::DbPointer(_) => Value::from("<dbpointer>"),
        Bson::MaxKey => Value::from("<maxkey>"),
        Bson::MinKey => Value::from("<minkey>"),
        Bson::Array(items) => {
            if depth >= MAX_OUTPUT_DEPTH {
                return Value::from(format!("<array of {} items>", items.len()));
            }
            let mut rendered = items
                .iter()
                .take(MAX_COLUMNS)
                .map(|item| render_at(item, depth + 1))
                .collect::<Vec<_>>();
            if items.len() > MAX_COLUMNS {
                rendered.push(Value::from(format!(
                    "... [+{} items]",
                    items.len() - MAX_COLUMNS
                )));
            }
            Value::Array(rendered)
        }
        Bson::Document(document) => {
            if depth >= MAX_OUTPUT_DEPTH {
                return Value::from(format!("<document of {} fields>", document.len()));
            }
            render_document_at(document, depth)
        }
    }
}

fn object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

/// One document as a bounded JSON object.
pub(crate) fn render_document(document: &Document) -> Value {
    render_document_at(document, 0)
}

fn render_document_at(document: &Document, depth: usize) -> Value {
    let mut rendered = Map::new();
    for (key, value) in document.iter().take(MAX_COLUMNS) {
        let mut name = cells::cut(key.as_str(), MAX_NAME_CHARS);
        // Cut names can collide; keep every field.
        while rendered.contains_key(&name) {
            name.push('\'');
        }
        rendered.insert(name, render_at(value, depth + 1));
    }
    if document.len() > MAX_COLUMNS {
        rendered.insert(
            "...".to_owned(),
            Value::from(format!("[+{} fields]", document.len() - MAX_COLUMNS)),
        );
    }
    Value::Object(rendered)
}

/// Collects rendered values under the row and byte caps.
pub(crate) struct ValueCollector {
    max_rows: usize,
    bytes: usize,
    values: Vec<Value>,
    truncation: Option<Truncation>,
}

impl ValueCollector {
    pub(crate) fn new(max_rows: usize) -> Self {
        Self {
            max_rows,
            bytes: 2,
            values: Vec::new(),
            truncation: None,
        }
    }

    /// Adds one value; `false` once no more are wanted. A value past a cap is
    /// not kept: its existence is what marks the result truncated.
    pub(crate) fn push(&mut self, value: Value) -> bool {
        if self.truncation.is_some() {
            return false;
        }
        if self.values.len() == self.max_rows {
            self.truncation = Some(Truncation::MaxRows);
            return false;
        }
        let size = serde_json::to_string(&value).map_or(usize::MAX, |text| text.len() + 1);
        if self.bytes.saturating_add(size) > MAX_RESULT_BYTES {
            self.truncation = Some(Truncation::MaxBytes);
            return false;
        }
        self.bytes += size;
        self.values.push(value);
        true
    }

    pub(crate) fn finish(self) -> (Vec<Value>, Option<Truncation>) {
        (self.values, self.truncation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cells::MAX_CELL_CHARS;
    use bson::{Binary, DateTime, doc, oid::ObjectId, spec::BinarySubtype};

    #[test]
    fn test_documents_render_bounded_and_escaped() {
        let id = ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap();
        let document = doc! {
            "_id": id,
            "name": "a\u{202e}b",
            "when": DateTime::from_millis(0),
            "blob": Binary { subtype: BinarySubtype::Generic, bytes: vec![0; 42] },
            "long": "x".repeat(MAX_CELL_CHARS + 3),
            "n": 5_i64,
            "f": f64::NAN,
            "code": Bson::JavaScriptCode("function(){}".to_owned()),
        };
        let value = render_document(&document);
        assert_eq!(value["_id"]["$oid"], "65a1b2c3d4e5f60718293a4b");
        assert_eq!(value["name"], "a\\u202Eb");
        assert_eq!(value["when"]["$date"], "1970-01-01T00:00:00Z");
        assert_eq!(value["blob"], "<binary 42 bytes>");
        assert!(value["long"].as_str().unwrap().ends_with("... [+3 chars]"));
        assert_eq!(value["n"], 5);
        assert_eq!(value["f"], "NaN");
        assert_eq!(value["code"], "<javascript>");
    }

    #[test]
    fn test_wide_deep_and_many_values_are_capped() {
        let mut wide = Document::new();
        for index in 0..(MAX_COLUMNS + 5) {
            wide.insert(format!("f{index}"), index as i64);
        }
        let value = render_document(&wide);
        assert_eq!(value.as_object().unwrap().len(), MAX_COLUMNS + 1);
        assert_eq!(value["..."], "[+5 fields]");

        let mut deep = Bson::Int32(1);
        for _ in 0..(MAX_OUTPUT_DEPTH + 3) {
            deep = Bson::Document(doc! {"a": deep});
        }
        let text = render(&deep).to_string();
        assert!(text.contains("<document of 1 fields>"), "{text}");

        let items = Bson::Array((0..(MAX_COLUMNS as i32 + 2)).map(Bson::Int32).collect());
        let value = render(&items);
        assert_eq!(value.as_array().unwrap().len(), MAX_COLUMNS + 1);

        let mut collector = ValueCollector::new(2);
        assert!(collector.push(Value::from(1)));
        assert!(collector.push(Value::from(2)));
        assert!(!collector.push(Value::from(3)));
        assert_eq!(collector.finish().1, Some(Truncation::MaxRows));
        let mut collector = ValueCollector::new(1_000);
        let big = Value::from("x".repeat(MAX_CELL_CHARS));
        while collector.push(big.clone()) {}
        let (values, reason) = collector.finish();
        assert_eq!(reason, Some(Truncation::MaxBytes));
        assert!(serde_json::to_string(&values).unwrap().len() <= MAX_RESULT_BYTES);
    }
}
