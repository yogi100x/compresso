//! Bounding one MongoDB result: rendered documents under the SQL tool's row
//! and byte caps. Rendering itself is in `raw.rs`, straight from raw BSON.

use serde_json::Value;

use crate::Truncation;
use crate::cells::MAX_RESULT_BYTES;

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

    /// Result bytes still free.
    pub(crate) fn remaining(&self) -> usize {
        MAX_RESULT_BYTES.saturating_sub(self.bytes)
    }

    pub(crate) fn finish(self) -> (Vec<Value>, Option<Truncation>) {
        (self.values, self.truncation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_values_stop_at_the_row_cap_and_the_byte_cap() {
        let mut collector = ValueCollector::new(2);
        assert!(collector.push(Value::from(1)));
        assert!(collector.push(Value::from(2)));
        assert!(!collector.push(Value::from(3)));
        assert_eq!(collector.finish().1, Some(Truncation::MaxRows));
        let mut collector = ValueCollector::new(1_000);
        let big = Value::from("x".repeat(1_000));
        while collector.push(big.clone()) {}
        let (values, reason) = collector.finish();
        assert_eq!(reason, Some(Truncation::MaxBytes));
        assert!(serde_json::to_string(&values).unwrap().len() <= MAX_RESULT_BYTES);
    }
}
