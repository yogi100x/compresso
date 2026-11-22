//! Bounding one result: cells, rows, and serialized bytes.

use serde_json::Value;

use crate::{QueryResult, Truncation};

/// Characters of one text cell kept before it is cut.
pub const MAX_CELL_CHARS: usize = 1_000;
/// Serialized bytes of all rows of one result, before any token fit.
pub const MAX_RESULT_BYTES: usize = 256 * 1024;

/// A text cell, cut to [`MAX_CELL_CHARS`] with the number of characters left
/// out.
pub(crate) fn text(value: &str) -> Value {
    let mut chars = value.char_indices();
    match chars.nth(MAX_CELL_CHARS) {
        None => Value::from(value),
        Some((cut, _)) => {
            let omitted = value[cut..].chars().count();
            Value::from(format!("{}... [+{omitted} chars]", &value[..cut]))
        }
    }
}

/// A binary cell: its size, never its bytes.
pub(crate) fn binary(length: usize) -> Value {
    Value::from(format!("<binary {length} bytes>"))
}

/// Collects rows under the row and byte caps.
pub(crate) struct RowCollector {
    max_rows: usize,
    bytes: usize,
    rows: Vec<Vec<Value>>,
    truncation: Option<Truncation>,
}

impl RowCollector {
    pub(crate) fn new(max_rows: usize) -> Self {
        Self {
            max_rows,
            bytes: 0,
            rows: Vec::new(),
            truncation: None,
        }
    }

    /// Adds one row; returns `false` once no more rows are wanted. A row past
    /// a cap is not kept: its existence is what marks the result truncated.
    pub(crate) fn push(&mut self, row: Vec<Value>) -> bool {
        if self.truncation.is_some() {
            return false;
        }
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

    /// Whether a cap was reached (the statement had more rows).
    pub(crate) fn is_truncated(&self) -> bool {
        self.truncation.is_some()
    }

    pub(crate) fn finish(self, columns: Vec<String>, elapsed_ms: u64) -> QueryResult {
        QueryResult {
            columns,
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
        let mut rows = RowCollector::new(2);
        assert!(rows.push(vec![Value::from(1)]));
        assert!(rows.push(vec![Value::from(2)]));
        assert!(!rows.push(vec![Value::from(3)]));
        let result = rows.finish(vec!["n".to_owned()], 1);
        assert_eq!(result.row_count, 2);
        assert_eq!(result.truncation_reason, Some(Truncation::MaxRows));

        let mut rows = RowCollector::new(1_000);
        let wide = Value::from("x".repeat(MAX_CELL_CHARS));
        let mut kept = 0;
        while rows.push(vec![wide.clone(); 10]) {
            kept += 1;
        }
        let result = rows.finish(vec![], 1);
        assert_eq!(result.truncation_reason, Some(Truncation::MaxBytes));
        assert_eq!(result.row_count, kept);
        assert!(serde_json::to_string(&result.rows).unwrap().len() <= MAX_RESULT_BYTES);

        let rows = RowCollector::new(5);
        let result = rows.finish(vec![], 0);
        assert!(!result.truncated);
        assert_eq!(result.truncation_reason, None);
    }
}
