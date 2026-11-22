//! Bounding one result: cells, rows, and serialized bytes.

use serde_json::Value;

use crate::{QueryResult, Truncation};

/// Characters of one text cell kept before it is cut.
pub const MAX_CELL_CHARS: usize = 1_000;
/// Serialized bytes of one result (column names and rows), before any token
/// fit.
pub const MAX_RESULT_BYTES: usize = 256 * 1024;
/// Columns one result keeps; the rest are counted in `columns_omitted`.
pub const MAX_COLUMNS: usize = 100;
/// Characters of one column, table, or other name kept before it is cut.
pub const MAX_NAME_CHARS: usize = 64;
/// Characters of one metadata value (a type, a default expression) kept.
pub const MAX_METADATA_CHARS: usize = 256;

/// `value` cut to `limit` characters, with the number left out, and C1
/// control and bidirectional-formatting characters written as `\uXXXX`
/// text: they can reorder or hide what a reader sees, and JSON escapes
/// only C0 controls.
pub(crate) fn cut(value: &str, limit: usize) -> String {
    let cut = match value.char_indices().nth(limit) {
        None => value.to_owned(),
        Some((index, _)) => {
            let omitted = value[index..].chars().count();
            format!("{}... [+{omitted} chars]", &value[..index])
        }
    };
    neutralise(&cut)
}

/// Whether a character is a C1 control or a bidirectional formatting
/// character.
fn is_hidden_control(character: char) -> bool {
    matches!(
        character,
        '\u{80}'..='\u{9f}'
            | '\u{61c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn neutralise(value: &str) -> String {
    if !value.chars().any(is_hidden_control) {
        return value.to_owned();
    }
    let mut output = String::with_capacity(value.len() + 8);
    for character in value.chars() {
        if is_hidden_control(character) {
            output.push_str(&format!("\\u{:04X}", u32::from(character)));
        } else {
            output.push(character);
        }
    }
    output
}

/// A text cell, cut to [`MAX_CELL_CHARS`] with the number of characters left
/// out.
pub(crate) fn text(value: &str) -> Value {
    Value::from(cut(value, MAX_CELL_CHARS))
}

/// A binary cell: its size, never its bytes.
pub(crate) fn binary(length: usize) -> Value {
    Value::from(format!("<binary {length} bytes>"))
}

/// Collects rows under the row, column, and byte caps.
pub(crate) struct RowCollector {
    max_rows: usize,
    bytes: usize,
    columns: Vec<String>,
    columns_omitted: usize,
    rows: Vec<Vec<Value>>,
    truncation: Option<Truncation>,
}

impl RowCollector {
    /// Starts a result with its column names: at most [`MAX_COLUMNS`], each
    /// cut to [`MAX_NAME_CHARS`]; their size counts against the byte cap.
    pub(crate) fn new(max_rows: usize, mut columns: Vec<String>) -> Self {
        let columns_omitted = columns.len().saturating_sub(MAX_COLUMNS);
        columns.truncate(MAX_COLUMNS);
        let columns = columns
            .iter()
            .map(|name| cut(name, MAX_NAME_CHARS))
            .collect::<Vec<_>>();
        let bytes = serde_json::to_string(&columns).map_or(0, |text| text.len());
        Self {
            max_rows,
            bytes,
            columns,
            columns_omitted,
            rows: Vec::new(),
            truncation: (columns_omitted > 0).then_some(Truncation::MaxColumns),
        }
    }

    /// Adds one row; returns `false` once no more rows are wanted. A row past
    /// a cap is not kept: its existence is what marks the result truncated.
    pub(crate) fn push(&mut self, mut row: Vec<Value>) -> bool {
        if self
            .truncation
            .is_some_and(|reason| reason != Truncation::MaxColumns)
        {
            return false;
        }
        row.truncate(MAX_COLUMNS);
        if self.rows.len() == self.max_rows {
            self.truncation = Some(Truncation::MaxRows);
            return false;
        }
        let size = serde_json::to_string(&row).map_or(usize::MAX, |text| text.len() + 1);
        if self.bytes.saturating_add(size) > MAX_RESULT_BYTES {
            self.truncation = Some(Truncation::MaxBytes);
            return false;
        }
        self.bytes += size;
        self.rows.push(row);
        true
    }

    /// Whether rows were left unread (the statement had more rows).
    pub(crate) fn is_truncated(&self) -> bool {
        self.truncation
            .is_some_and(|reason| reason != Truncation::MaxColumns)
    }

    pub(crate) fn finish(self, elapsed_ms: u64) -> QueryResult {
        QueryResult {
            columns: self.columns,
            columns_omitted: self.columns_omitted,
            row_count: self.rows.len(),
            rows: self.rows,
            truncated: self.truncation.is_some(),
            truncation_reason: self.truncation,
            elapsed_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_cells_are_cut_on_a_character_boundary() {
        assert_eq!(text("short"), Value::from("short"));
        let long = "é".repeat(MAX_CELL_CHARS + 5);
        let Value::String(cut) = text(&long) else {
            panic!("string cell");
        };
        assert!(cut.ends_with("... [+5 chars]"), "{cut}");
        assert_eq!(cut.chars().filter(|c| *c == 'é').count(), MAX_CELL_CHARS);
        assert_eq!(binary(42), Value::from("<binary 42 bytes>"));
    }

    #[test]
    fn test_rows_stop_at_the_row_cap_and_the_byte_cap() {
        let mut rows = RowCollector::new(2, vec!["n".to_owned()]);
        assert!(rows.push(vec![Value::from(1)]));
        assert!(rows.push(vec![Value::from(2)]));
        assert!(!rows.push(vec![Value::from(3)]));
        let result = rows.finish(1);
        assert_eq!(result.row_count, 2);
        assert_eq!(result.truncation_reason, Some(Truncation::MaxRows));

        let mut rows = RowCollector::new(1_000, Vec::new());
        let wide = Value::from("x".repeat(MAX_CELL_CHARS));
        let mut kept = 0;
        while rows.push(vec![wide.clone(); 10]) {
            kept += 1;
        }
        let result = rows.finish(1);
        assert_eq!(result.truncation_reason, Some(Truncation::MaxBytes));
        assert_eq!(result.row_count, kept);
        assert!(serde_json::to_string(&result.rows).unwrap().len() <= MAX_RESULT_BYTES);

        let rows = RowCollector::new(5, Vec::new());
        let result = rows.finish(0);
        assert!(!result.truncated);
        assert_eq!(result.truncation_reason, None);
    }
}
