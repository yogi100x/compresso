//! Cutting one rendered row so it fits what is left of a token budget
//! (E17-52, D117). The SQL and MongoDB tools drop trailing rows to fit
//! `max_tokens`; when even the first remaining row does not fit, it is cut
//! with these helpers instead of being dropped, so a result never comes back
//! empty while rows exist.

use serde_json::{Map, Value};

/// Marker text appended to a string cut here.
const CUT: &str = "... [cut to fit max_tokens]";

fn size(value: &Value) -> usize {
    serde_json::to_string(value).map_or(usize::MAX, |text| text.len())
}

/// `text` cut to at most about `budget` bytes of JSON, on a character
/// boundary, with a marker when anything was left out.
fn cut_text(text: &str, budget: usize) -> Value {
    let whole = Value::from(text);
    if size(&whole) <= budget {
        return whole;
    }
    // The longest prefix (in characters) that fits, by bisection.
    let ends = text
        .char_indices()
        .map(|(index, _)| index)
        .chain([text.len()])
        .collect::<Vec<_>>();
    let fits = |count: usize| size(&Value::from(&text[..ends[count]])) + CUT.len() <= budget;
    let (mut low, mut high) = (0, ends.len() - 1);
    while low < high {
        let middle = (low + high).div_ceil(2);
        if fits(middle) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    Value::from(format!("{}{CUT}", &text[..ends[low]]))
}

/// A JSON value cut to at most about `budget` bytes: strings shortened,
/// arrays and objects cut from the end with a `$truncated` marker.
pub(crate) fn shrink(value: &Value, budget: usize) -> Value {
    if size(value) <= budget {
        return value.clone();
    }
    match value {
        Value::String(text) => cut_text(text, budget),
        Value::Array(items) => {
            let mut kept = Vec::new();
            let mut used = 2 + 24;
            for item in items {
                let remaining = budget.saturating_sub(used);
                if remaining < 16 {
                    break;
                }
                let item = shrink(item, remaining);
                used += size(&item) + 1;
                if used > budget {
                    break;
                }
                kept.push(item);
            }
            let mut marker = Map::new();
            marker.insert("$truncated".to_owned(), Value::Bool(true));
            kept.push(Value::Object(marker));
            Value::Array(kept)
        }
        Value::Object(fields) => {
            let mut kept = Map::new();
            let mut used = 2 + 20;
            for (key, item) in fields {
                let key_size = size(&Value::from(key.as_str())) + 2;
                let remaining = budget.saturating_sub(used + key_size);
                if remaining < 16 {
                    break;
                }
                let item = shrink(item, remaining);
                used += key_size + size(&item);
                if used > budget {
                    break;
                }
                kept.insert(key.clone(), item);
            }
            kept.insert("$truncated".to_owned(), Value::Bool(true));
            Value::Object(kept)
        }
        other => other.clone(),
    }
}

/// A row of table cells cut so the row is at most about `budget` bytes:
/// every cell keeps its place (so columns stay aligned), and string cells
/// are shortened evenly.
pub(crate) fn shrink_cells(row: &[Value], budget: usize) -> Vec<Value> {
    let per_cell = budget / row.len().max(1);
    row.iter()
        .map(|cell| match cell {
            Value::String(text) => cut_text(text, per_cell.max(CUT.len() + 8)),
            other => other.clone(),
        })
        .collect()
}

/// The largest byte budget in `0..=limit` for which `fits` holds, by
/// bisection; `None` when not even budget 0 fits.
pub(crate) fn largest_budget(limit: usize, fits: impl Fn(usize) -> bool) -> Option<usize> {
    if !fits(0) {
        return None;
    }
    let (mut low, mut high) = (0, limit);
    while low < high {
        let middle = (low + high).div_ceil(2);
        if fits(middle) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    Some(low)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_shrink_keeps_shape_and_stays_within_budget() {
        let value = json!({"a": "x".repeat(10_000), "b": [1, 2, "y".repeat(5_000)], "c": 3});
        for budget in [60, 200, 2_000, 9_000] {
            let shrunk = shrink(&value, budget);
            assert!(size(&shrunk) <= budget + 64, "{budget}: {}", size(&shrunk));
            assert_eq!(shrunk["$truncated"], true);
        }
        assert_eq!(shrink(&value, 1_000_000), value);
        let row = vec![
            Value::from("z".repeat(9_000)),
            Value::from(5),
            Value::from("w"),
        ];
        let cut = shrink_cells(&row, 600);
        assert_eq!(cut.len(), 3);
        assert_eq!(cut[1], 5);
        assert!(cut[0].as_str().unwrap().ends_with(CUT));
        assert_eq!(largest_budget(100, |budget| budget <= 37), Some(37));
        assert_eq!(largest_budget(100, |_| false), None);
    }
}
