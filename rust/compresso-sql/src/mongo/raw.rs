//! Rendering raw BSON straight to bounded JSON (E17-52, D117 confirmation
//! round).
//!
//! The server's reply bytes are walked in place by the small parser here;
//! no `Document` or `Bson` tree of a result document is ever built (every
//! function takes `&[u8]`). Each document gets an output budget
//! ([`MAX_RESULT_BYTES`]): strings and names are cut as they are read, and
//! the walk stops once the budget is spent, so a 15 MB stored document costs
//! at most the budget in output, whatever its shape. A document whose raw
//! size is over [`MAX_DOCUMENT_BYTES`] is not walked at all; it becomes a
//! marker row with its size. Keys and strings that are not valid UTF-8 are
//! shown lossily (U+FFFD) instead of failing the call.
//!
//! Output form: JSON numbers, strings, booleans, null, arrays, and objects
//! for the JSON-like BSON types; every other type is an object whose only
//! key starts with `$` (`$oid`, `$date`, `$timestamp`, `$regex`/`$options`,
//! `$numberDecimal`, `$numberDouble`, `$binary`, `$code`, `$symbol`,
//! `$dbPointer`, `$undefined`, `$minKey`, `$maxKey`), and the tool's own
//! markers are `$omitted`, `$truncated`, `$document`, and `$array`. A stored
//! field name that starts with `$` or `\` is shown with a leading `\`, so a
//! `$` key in a result is never stored data.

use serde_json::{Map, Value};

use crate::cells::{MAX_CELL_CHARS, MAX_COLUMNS, MAX_NAME_CHARS, MAX_RESULT_BYTES, neutralise};

/// Raw size above which a document is shown only as its size.
pub const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
/// Nesting kept in a returned document; deeper values show as a marker.
pub const MAX_OUTPUT_DEPTH: usize = 8;

/// Bytes that are not BSON the parser can read.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Malformed;

fn read_i32(bytes: &[u8], at: usize) -> Result<i32, Malformed> {
    bytes
        .get(at..at + 4)
        .and_then(|slice| slice.try_into().ok())
        .map(i32::from_le_bytes)
        .ok_or(Malformed)
}

fn read_len(bytes: &[u8], at: usize) -> Result<usize, Malformed> {
    usize::try_from(read_i32(bytes, at)?).map_err(|_| Malformed)
}

/// One element of a document: its key bytes, type byte, and value bytes.
#[derive(Clone, Copy)]
pub(crate) struct Element<'a> {
    pub(crate) key: &'a [u8],
    pub(crate) kind: u8,
    pub(crate) value: &'a [u8],
}

/// The elements of one document, read in place.
pub(crate) struct Elements<'a> {
    bytes: &'a [u8],
    at: usize,
    end: usize,
    failed: bool,
}

/// Starts reading `doc`, whose first four bytes must give its length.
pub(crate) fn elements(doc: &[u8]) -> Result<Elements<'_>, Malformed> {
    let length = read_len(doc, 0)?;
    if length < 5 || length > doc.len() || doc[length - 1] != 0 {
        return Err(Malformed);
    }
    Ok(Elements {
        bytes: &doc[..length],
        at: 4,
        end: length - 1,
        failed: false,
    })
}

fn value_len(bytes: &[u8], at: usize, kind: u8) -> Result<usize, Malformed> {
    let string = |at: usize| -> Result<usize, Malformed> {
        let length = read_len(bytes, at)?;
        if length == 0 {
            return Err(Malformed);
        }
        Ok(4 + length)
    };
    let cstring = |at: usize| -> Result<usize, Malformed> {
        bytes
            .get(at..)
            .and_then(|rest| rest.iter().position(|byte| *byte == 0))
            .map(|end| end + 1)
            .ok_or(Malformed)
    };
    Ok(match kind {
        0x01 | 0x09 | 0x11 | 0x12 => 8,
        0x02 | 0x0D | 0x0E => string(at)?,
        0x03 | 0x04 | 0x0F => read_len(bytes, at)?,
        0x05 => 4 + 1 + read_len(bytes, at)?,
        0x06 | 0x0A | 0xFF | 0x7F => 0,
        0x07 => 12,
        0x08 => 1,
        0x0B => {
            let pattern = cstring(at)?;
            pattern + cstring(at + pattern)?
        }
        0x0C => string(at)? + 12,
        0x10 => 4,
        0x13 => 16,
        _ => return Err(Malformed),
    })
}

impl<'a> Iterator for Elements<'a> {
    type Item = Result<Element<'a>, Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.at >= self.end {
            return None;
        }
        let mut step = || -> Result<Element<'a>, Malformed> {
            let kind = self.bytes[self.at];
            let key_start = self.at + 1;
            let key_len = self.bytes[key_start..self.end]
                .iter()
                .position(|byte| *byte == 0)
                .ok_or(Malformed)?;
            let value_start = key_start + key_len + 1;
            let length = value_len(self.bytes, value_start, kind)?;
            let value_end = value_start.checked_add(length).ok_or(Malformed)?;
            if value_end > self.end {
                return Err(Malformed);
            }
            self.at = value_end;
            Ok(Element {
                key: &self.bytes[key_start..key_start + key_len],
                kind,
                value: &self.bytes[value_start..value_end],
            })
        };
        let result = step();
        if result.is_err() {
            self.failed = true;
        }
        Some(result)
    }
}

/// Characters a byte slice holds as UTF-8 (lossy: a byte that does not
/// continue a character counts as one).
fn char_count(bytes: &[u8]) -> usize {
    bytes.iter().filter(|byte| (**byte & 0xC0) != 0x80).count()
}

/// `bytes` decoded lossily and cut to `limit` characters, with the number
/// left out, hidden characters escaped. Only a bounded prefix is decoded.
pub(crate) fn cut_bytes(bytes: &[u8], limit: usize) -> String {
    let window = &bytes[..bytes.len().min(limit.saturating_mul(4).max(4))];
    let decoded = String::from_utf8_lossy(window);
    let mut kept = decoded.chars().take(limit).collect::<String>();
    let kept_chars = kept.chars().count();
    let total = decoded.chars().count() + char_count(&bytes[window.len()..]);
    if total > kept_chars {
        kept.push_str(&format!("... [+{} chars]", total - kept_chars));
    }
    neutralise(&kept)
}

/// A stored field name as shown: cut, escaped, and with a leading `\` when
/// it starts with `$` or `\`.
pub(crate) fn stored_name(name: String) -> String {
    if name.starts_with('$') || name.starts_with('\\') {
        format!("\\{name}")
    } else {
        name
    }
}

/// A stored key as shown.
pub(crate) fn key_name(key: &[u8]) -> String {
    stored_name(cut_bytes(key, MAX_NAME_CHARS))
}

/// The bytes of a BSON string value, without its length and NUL.
fn string_bytes(value: &[u8]) -> &[u8] {
    value
        .get(4..value.len().saturating_sub(1))
        .unwrap_or_default()
}

fn marker(key: &str, value: Value) -> Value {
    let mut object = Map::new();
    object.insert(key.to_owned(), value);
    Value::Object(object)
}

fn size_marker(key: &str, bytes: usize) -> Value {
    marker(key, marker("bytes", Value::from(bytes)))
}

/// Output bytes one document may still produce.
pub(crate) struct Budget {
    remaining: usize,
}

impl Budget {
    pub(crate) fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    fn spend(&mut self, bytes: usize) -> bool {
        if bytes > self.remaining {
            self.remaining = 0;
            return false;
        }
        self.remaining -= bytes;
        true
    }

    fn spent(&self) -> bool {
        self.remaining == 0
    }
}

/// One stored document as bounded JSON, in at most about `limit` output
/// bytes (the walk stops and marks `$truncated` when they are spent). A
/// document over
/// [`MAX_DOCUMENT_BYTES`] is a size marker; bytes the parser cannot read are
/// a `$malformed` marker.
pub(crate) fn render_document(doc: &[u8], limit: usize) -> Value {
    if doc.len() > MAX_DOCUMENT_BYTES {
        return size_marker("$document", doc.len());
    }
    let mut budget = Budget::new(limit.min(MAX_RESULT_BYTES));
    render_object(doc, 0, &mut budget, false)
}

/// The value of one top-level field of `doc`, rendered (for `distinct`,
/// whose values arrive as `_id` of grouped documents).
pub(crate) fn render_field(doc: &[u8], name: &[u8], limit: usize) -> Option<Value> {
    if doc.len() > MAX_DOCUMENT_BYTES {
        return Some(size_marker("$document", doc.len()));
    }
    let mut budget = Budget::new(limit.min(MAX_RESULT_BYTES));
    let found = elements(doc)
        .ok()?
        .flatten()
        .find(|element| element.key == name)?;
    Some(render_value(found, 1, &mut budget))
}

fn render_object(doc: &[u8], depth: usize, budget: &mut Budget, array: bool) -> Value {
    let Ok(iter) = elements(doc) else {
        return size_marker("$malformed", doc.len());
    };
    let mut fields = Map::new();
    let mut items = Vec::new();
    let mut shown = 0;
    let mut rest = 0;
    let mut truncated = false;
    for element in iter {
        let Ok(element) = element else {
            truncated = true;
            break;
        };
        if shown == MAX_COLUMNS {
            rest += 1;
            continue;
        }
        if budget.spent()
            || !budget.spend(if array {
                2
            } else {
                element.key.len().min(MAX_NAME_CHARS * 4) + 4
            })
        {
            truncated = true;
            break;
        }
        let value = render_value(element, depth + 1, budget);
        if array {
            items.push(value);
        } else {
            let mut name = key_name(element.key);
            // Cut names can collide; keep every field.
            while fields.contains_key(&name) {
                name.push('\'');
            }
            fields.insert(name, value);
        }
        shown += 1;
    }
    if array {
        if rest > 0 {
            items.push(marker("$omitted", Value::from(rest)));
        }
        if truncated {
            items.push(marker("$truncated", Value::Bool(true)));
        }
        Value::Array(items)
    } else {
        if rest > 0 {
            fields.insert("$omitted".to_owned(), Value::from(rest));
        }
        if truncated {
            fields.insert("$truncated".to_owned(), Value::Bool(true));
        }
        Value::Object(fields)
    }
}

fn number<const N: usize>(value: &[u8]) -> [u8; N] {
    value
        .get(..N)
        .and_then(|slice| slice.try_into().ok())
        .unwrap_or([0; N])
}

fn render_value(element: Element<'_>, depth: usize, budget: &mut Budget) -> Value {
    let value = element.value;
    let text = |bytes: &[u8], budget: &mut Budget| {
        let cut = cut_bytes(bytes, MAX_CELL_CHARS);
        budget.spend(cut.len() + 2);
        Value::from(cut)
    };
    // Output bytes each kind costs beyond its text (markers are objects).
    budget.spend(match element.kind {
        0x02..=0x04 => 2,
        0x08 | 0x0A | 0x10 | 0x12 => 12,
        0x01 => 24,
        _ => 48,
    });
    match element.kind {
        0x01 => {
            let number = f64::from_le_bytes(number::<8>(value));
            serde_json::Number::from_f64(number).map_or_else(
                || {
                    let name = if number.is_nan() {
                        "NaN"
                    } else if number > 0.0 {
                        "Infinity"
                    } else {
                        "-Infinity"
                    };
                    marker("$numberDouble", Value::from(name))
                },
                Value::Number,
            )
        }
        0x02 => text(string_bytes(value), budget),
        0x03 | 0x04 => {
            if depth >= MAX_OUTPUT_DEPTH {
                size_marker(
                    if element.kind == 0x03 {
                        "$document"
                    } else {
                        "$array"
                    },
                    value.len(),
                )
            } else {
                render_object(value, depth, budget, element.kind == 0x04)
            }
        }
        0x05 => size_marker("$binary", value.len().saturating_sub(5)),
        0x06 => marker("$undefined", Value::Bool(true)),
        0x07 => marker(
            "$oid",
            Value::from(
                value
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
            ),
        ),
        0x08 => Value::Bool(value.first().is_some_and(|byte| *byte != 0)),
        0x09 => {
            let millis = i64::from_le_bytes(number::<8>(value));
            let date = bson::DateTime::from_millis(millis);
            marker(
                "$date",
                date.try_to_rfc3339_string()
                    .map_or_else(|_| Value::from(millis), Value::from),
            )
        }
        0x0A => Value::Null,
        0x0B => {
            let split = value
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(value.len());
            let pattern = &value[..split];
            let options = value
                .get(split + 1..value.len().saturating_sub(1))
                .unwrap_or_default();
            let mut object = Map::new();
            object.insert("$regex".to_owned(), text(pattern, budget));
            object.insert("$options".to_owned(), text(options, budget));
            Value::Object(object)
        }
        0x0C => size_marker("$dbPointer", value.len()),
        0x0D | 0x0F => size_marker("$code", value.len()),
        0x0E => marker("$symbol", text(string_bytes(value), budget)),
        0x10 => Value::from(i32::from_le_bytes(number::<4>(value))),
        0x11 => {
            let increment = u32::from_le_bytes(number::<4>(value));
            let time = u32::from_le_bytes(number::<4>(value.get(4..).unwrap_or_default()));
            let mut inner = Map::new();
            inner.insert("t".to_owned(), Value::from(time));
            inner.insert("i".to_owned(), Value::from(increment));
            marker("$timestamp", Value::Object(inner))
        }
        0x12 => Value::from(i64::from_le_bytes(number::<8>(value))),
        0x13 => marker(
            "$numberDecimal",
            Value::from(bson::Decimal128::from_bytes(number::<16>(value)).to_string()),
        ),
        0xFF => marker("$minKey", Value::from(1)),
        0x7F => marker("$maxKey", Value::from(1)),
        _ => marker("$malformed", Value::Bool(true)),
    }
}

/// The BSON type name MongoDB's `$type` uses, by type byte.
pub(crate) fn type_name(kind: u8) -> &'static str {
    match kind {
        0x01 => "double",
        0x02 => "string",
        0x03 => "object",
        0x04 => "array",
        0x05 => "binData",
        0x06 => "undefined",
        0x07 => "objectId",
        0x08 => "bool",
        0x09 => "date",
        0x0A => "null",
        0x0B => "regex",
        0x0C => "dbPointer",
        0x0D => "javascript",
        0x0E => "symbol",
        0x0F => "javascriptWithScope",
        0x10 => "int",
        0x11 => "timestamp",
        0x12 => "long",
        0x13 => "decimal",
        0xFF => "minKey",
        0x7F => "maxKey",
        _ => "unknown",
    }
}

/// The documents of one cursor reply (`cursor.firstBatch` or
/// `cursor.nextBatch`), as slices of the reply.
pub(crate) fn batch_documents(reply: &[u8]) -> Result<Vec<&[u8]>, Malformed> {
    let cursor = elements(reply)?
        .flatten()
        .find(|element| element.key == b"cursor" && element.kind == 0x03)
        .ok_or(Malformed)?;
    let batch = elements(cursor.value)?
        .flatten()
        .find(|element| {
            (element.key == b"firstBatch" || element.key == b"nextBatch") && element.kind == 0x04
        })
        .ok_or(Malformed)?;
    let mut documents = Vec::new();
    for element in elements(batch.value)? {
        let element = element?;
        if element.kind != 0x03 {
            return Err(Malformed);
        }
        documents.push(element.value);
    }
    Ok(documents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::{Bson, Document, doc, oid::ObjectId};

    fn bytes(document: &Document) -> Vec<u8> {
        document.to_vec().expect("raw")
    }

    #[test]
    fn test_types_render_as_markers_and_stored_dollar_keys_are_escaped() {
        let id = ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap();
        let document = doc! {
            "real": id,
            "forged": {"$oid": "65a1b2c3d4e5f60718293a4b"},
            "$date": "x",
            "\\back": 1,
            "name\u{202e}x": 2,
            "$omitted": 3,
            "when": bson::DateTime::from_millis(0),
            "n": 5_i64,
            "f": f64::NAN,
            "d": Bson::Decimal128(bson::Decimal128::from_bytes([0; 16])),
            "code": Bson::JavaScriptCode("function(){}".to_owned()),
            "text": "a\u{200b}b",
        };
        let value = render_document(&bytes(&document), MAX_RESULT_BYTES);
        assert_eq!(
            value["real"],
            serde_json::json!({"$oid": "65a1b2c3d4e5f60718293a4b"})
        );
        assert_eq!(
            value["forged"],
            serde_json::json!({"\\$oid": "65a1b2c3d4e5f60718293a4b"})
        );
        assert_eq!(value["\\$date"], "x");
        assert_eq!(value["\\\\back"], 1);
        assert_eq!(value["name\\u202Ex"], 2);
        assert_eq!(value["\\$omitted"], 3);
        assert!(value.get("$date").is_none() && value.get("$omitted").is_none());
        assert_eq!(value["when"]["$date"], "1970-01-01T00:00:00Z");
        assert_eq!(value["n"], 5);
        assert_eq!(value["f"], serde_json::json!({"$numberDouble": "NaN"}));
        assert!(value["d"]["$numberDecimal"].is_string());
        assert!(value["code"]["$code"]["bytes"].is_number());
        assert_eq!(value["text"], "a\\u200Bb");
    }

    /// The review's case: every extended-JSON wrapper stored as data, at
    /// depth and in arrays, is shown escaped; only real typed values carry
    /// a `$` key.
    #[test]
    fn test_stored_wrappers_never_look_typed_at_any_depth() {
        let wrappers = [
            doc! {"$oid": "65a1b2c3d4e5f60718293a4b"},
            doc! {"$date": "2020-01-01T00:00:00Z"},
            doc! {"$date": {"$numberLong": "1"}},
            doc! {"$numberLong": "5"},
            doc! {"$timestamp": {"t": 1, "i": 2}},
            doc! {"$regularExpression": {"pattern": "a", "options": ""}},
            doc! {"$binary": {"base64": "AA==", "subType": "00"}},
            doc! {"$numberDecimal": "1.5"},
            doc! {"$minKey": 1},
            doc! {"$code": "f"},
        ];
        for wrapper in wrappers {
            let document = doc! {
                "v": wrapper.clone(),
                "deep": {"x": {"y": wrapper.clone()}},
                "list": [wrapper.clone(), [wrapper.clone()]],
            };
            let text = render_document(&bytes(&document), MAX_RESULT_BYTES).to_string();
            let unescaped = text.matches("{\"$").count();
            assert_eq!(unescaped, 0, "{text}");
        }
    }

    #[test]
    fn test_invalid_utf8_keys_and_strings_render_lossily() {
        // {"a\xff": "b\xfe"} built by hand.
        let mut doc = Vec::new();
        let body: Vec<u8> = [&[0x02][..], b"a\xff\0", &3_i32.to_le_bytes(), b"b\xfe\0"].concat();
        doc.extend_from_slice(&((body.len() + 5) as i32).to_le_bytes());
        doc.extend_from_slice(&body);
        doc.push(0);
        let value = render_document(&doc, MAX_RESULT_BYTES);
        assert_eq!(value["a\u{fffd}"], "b\u{fffd}");
    }

    #[test]
    fn test_a_huge_document_is_a_size_marker_and_a_large_one_is_cut() {
        let mut huge = Document::new();
        huge.insert("s", "x".repeat(15 * 1024 * 1024));
        let raw = bytes(&huge);
        let value = render_document(&raw, MAX_RESULT_BYTES);
        assert_eq!(value["$document"]["bytes"], raw.len());

        // Just under the per-document cap, with many long strings: the
        // output stays within the budget.
        let mut wide = Document::new();
        for index in 0..900 {
            wide.insert(format!("k{index}"), "y".repeat(1_100));
        }
        let raw = bytes(&wide);
        assert!(raw.len() <= MAX_DOCUMENT_BYTES);
        let text = render_document(&raw, MAX_RESULT_BYTES).to_string();
        assert!(text.len() <= MAX_RESULT_BYTES + 64 * 1024, "{}", text.len());

        // A deep document stops at the output depth.
        let mut deep = Bson::Int32(1);
        for _ in 0..(MAX_OUTPUT_DEPTH + 3) {
            deep = Bson::Document(doc! {"a": deep});
        }
        let text = render_document(&bytes(&doc! {"d": deep}), MAX_RESULT_BYTES).to_string();
        assert!(text.contains("\"$document\":{\"bytes\""), "{text}");
    }

    #[test]
    fn test_a_long_string_is_cut_without_decoding_it_whole() {
        let long = "é".repeat(MAX_CELL_CHARS + 5);
        let cut = cut_bytes(long.as_bytes(), MAX_CELL_CHARS);
        assert!(cut.ends_with("... [+5 chars]"), "{cut}");
        assert_eq!(cut.chars().filter(|c| *c == 'é').count(), MAX_CELL_CHARS);
    }

    #[test]
    fn test_batch_documents_are_read_in_place() {
        let reply =
            bytes(&doc! {"cursor": {"firstBatch": [{"a": 1}, {"b": 2}], "id": 0_i64}, "ok": 1});
        let documents = batch_documents(&reply).expect("batch");
        assert_eq!(documents.len(), 2);
        assert_eq!(render_document(documents[1], MAX_RESULT_BYTES)["b"], 2);
        assert_eq!(
            render_field(documents[0], b"a", MAX_RESULT_BYTES),
            Some(Value::from(1))
        );
        assert!(batch_documents(&bytes(&doc! {"ok": 1})).is_err());
        assert!(elements(&[1, 0, 0]).is_err());
    }
}
