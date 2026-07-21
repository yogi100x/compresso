use std::fmt;

use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use crate::graph::{ContextGraph, NodeKind};
use crate::strategy::{
    CompressionBudget, CompressionStrategy, Estimate, RemovalKind, RemovalRecord, StrategyError,
    StrategyOutput,
};
use crate::tokens::counter_for;

pub struct CompactJson;

struct CompactPlan {
    changes: Vec<NodeChange>,
    notes: Vec<String>,
    tokens_saved: usize,
}

struct NodeChange {
    index: usize,
    content: String,
    token_count: usize,
    removals: Vec<RemovalRecord>,
}

struct JsonOperation {
    pointer: String,
    replacement: Value,
    units_removed: usize,
    notes: Vec<String>,
}

impl CompressionStrategy for CompactJson {
    fn id(&self) -> &'static str {
        "compact_json"
    }

    fn supports(&self, input: &ContextGraph) -> bool {
        !compact_plan(input).changes.is_empty()
    }

    fn estimate(&self, input: &ContextGraph) -> Estimate {
        let plan = compact_plan(input);
        if plan.changes.is_empty() {
            Estimate::default()
        } else {
            Estimate {
                tokens_saved: plan.tokens_saved,
                confidence: 1.0,
            }
        }
    }

    fn execute(
        &self,
        input: &ContextGraph,
        _budget: &CompressionBudget,
    ) -> Result<StrategyOutput, StrategyError> {
        let plan = compact_plan(input);
        let mut graph = input.clone();
        let mut removals = Vec::new();

        for change in plan.changes {
            let node = &mut graph.nodes[change.index];
            node.content = change.content;
            node.token_count = change.token_count;
            removals.extend(change.removals);
        }

        Ok(StrategyOutput {
            graph,
            notes: plan.notes,
            removals,
        })
    }
}

fn compact_plan(input: &ContextGraph) -> CompactPlan {
    let mut changes = Vec::new();
    let mut notes = Vec::new();
    let mut tokens_saved = 0_usize;

    for (index, node) in input.nodes.iter().enumerate() {
        if node.protected || node.kind.requires_protection() {
            notes.push(format!("skipped_reason: node {} is protected", node.id));
            continue;
        }
        if node.kind != NodeKind::ToolResponse {
            notes.push(format!(
                "skipped_reason: node {} is not a tool response",
                node.id
            ));
            continue;
        }

        let Ok(parsed) = parse_unique_json(&node.content) else {
            notes.push(format!(
                "skipped_reason: node {} contains invalid JSON",
                node.id
            ));
            continue;
        };
        let Ok(compact) = serde_json::to_string(&parsed) else {
            notes.push(format!(
                "skipped_reason: node {} JSON could not be serialized",
                node.id
            ));
            continue;
        };
        let mut operations = Vec::new();
        plan_operations(&parsed, "", &mut operations);

        let counter = counter_for(node.token_provider);
        let mut current_value = parsed;
        let mut current_content = node.content.clone();
        let mut current_tokens = node.token_count;
        let mut node_removals = Vec::new();

        for operation in operations {
            let JsonOperation {
                pointer,
                replacement,
                units_removed,
                notes: operation_notes,
            } = operation;
            let mut candidate_value = current_value.clone();
            if !replace_pointer(&mut candidate_value, &pointer, replacement) {
                notes.push(format!(
                    "skipped_reason: node {} pointer {} did not resolve",
                    node.id,
                    display_pointer(&pointer)
                ));
                continue;
            }
            let Ok(candidate_content) = serde_json::to_string(&candidate_value) else {
                notes.push(format!(
                    "skipped_reason: node {} pointer {} could not be serialized",
                    node.id,
                    display_pointer(&pointer)
                ));
                continue;
            };
            let after_tokens = counter.count(&candidate_content);

            if after_tokens >= current_tokens {
                notes.push(format!(
                    "skipped_reason: node {} pointer {}; no token reduction",
                    node.id,
                    display_pointer(&pointer)
                ));
                continue;
            }

            node_removals.push(RemovalRecord {
                strategy_id: "compact_json".to_owned(),
                node_id: node.id.clone(),
                kind: RemovalKind::JsonElision,
                locator: Some(pointer),
                original_hash: content_hash(&current_content),
                replacement_hash: Some(content_hash(&candidate_content)),
                before_tokens: current_tokens,
                after_tokens,
                units_removed: Some(units_removed),
            });
            notes.extend(operation_notes);
            tokens_saved = tokens_saved.saturating_add(current_tokens - after_tokens);
            current_value = candidate_value;
            current_content = candidate_content;
            current_tokens = after_tokens;
        }

        if node_removals.is_empty() && compact != node.content {
            let after_tokens = counter.count(&compact);
            if after_tokens < current_tokens {
                node_removals.push(RemovalRecord {
                    strategy_id: "compact_json".to_owned(),
                    node_id: node.id.clone(),
                    kind: RemovalKind::JsonElision,
                    locator: Some(String::new()),
                    original_hash: content_hash(&current_content),
                    replacement_hash: Some(content_hash(&compact)),
                    before_tokens: current_tokens,
                    after_tokens,
                    units_removed: Some(
                        current_content
                            .chars()
                            .count()
                            .saturating_sub(compact.chars().count()),
                    ),
                });
                notes.push(format!("compacted node: {} JSON whitespace", node.id));
                tokens_saved = tokens_saved.saturating_add(current_tokens - after_tokens);
                current_content = compact;
                current_tokens = after_tokens;
            } else {
                notes.push(format!(
                    "skipped_reason: node {} compaction has no token reduction",
                    node.id
                ));
            }
        } else if node_removals.is_empty() && compact == node.content {
            notes.push(format!(
                "skipped_reason: node {} is already compact with no profitable elisions",
                node.id
            ));
        }

        if !node_removals.is_empty() {
            changes.push(NodeChange {
                index,
                content: current_content,
                token_count: current_tokens,
                removals: node_removals,
            });
        }
    }

    CompactPlan {
        changes,
        notes,
        tokens_saved,
    }
}

struct StrictValueSeed;

impl<'de> DeserializeSeed<'de> for StrictValueSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("JSON number must be finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
        while let Some(value) = sequence.next_element_seed(StrictValueSeed)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = serde_json::Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(A::Error::custom("duplicate object key"));
            }
            values.insert(key, object.next_value_seed(StrictValueSeed)?);
        }
        Ok(Value::Object(values))
    }
}

fn parse_unique_json(content: &str) -> Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(content);
    let value = StrictValueSeed.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

fn plan_operations(value: &Value, pointer: &str, operations: &mut Vec<JsonOperation>) {
    match value {
        Value::String(value) if value.chars().count() > 500 => {
            let (replacement, removed) = truncate_string(value);
            operations.push(JsonOperation {
                pointer: pointer.to_owned(),
                replacement: Value::String(replacement),
                units_removed: removed,
                notes: vec![format!(
                    "truncated {}: removed {removed} chars",
                    display_pointer(pointer)
                )],
            });
        }
        Value::Array(array) if array.len() > 50 => {
            operations.push(truncate_array(array, pointer));
        }
        Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                plan_operations(
                    child,
                    &join_pointer(pointer, &index.to_string()),
                    operations,
                );
            }
        }
        Value::Object(object) => {
            for (key, child) in object {
                plan_operations(
                    child,
                    &join_pointer(pointer, &escape_pointer(key)),
                    operations,
                );
            }
        }
        _ => {}
    }
}

fn truncate_array(array: &[Value], pointer: &str) -> JsonOperation {
    let removed = array.len() - 30;
    let mut notes = vec![format!(
        "truncated {}: removed {removed} items",
        display_pointer(pointer)
    )];
    let mut replacement = Vec::with_capacity(31);

    for (index, value) in array.iter().take(25).enumerate() {
        replacement.push(transform_nested(
            value,
            &join_pointer(pointer, &index.to_string()),
            &mut notes,
        ));
    }
    replacement.push(Value::String(format!("…[+{removed} items]")));
    for (index, value) in array.iter().enumerate().skip(array.len() - 5) {
        replacement.push(transform_nested(
            value,
            &join_pointer(pointer, &index.to_string()),
            &mut notes,
        ));
    }

    JsonOperation {
        pointer: pointer.to_owned(),
        replacement: Value::Array(replacement),
        units_removed: removed,
        notes,
    }
}

fn transform_nested(value: &Value, pointer: &str, notes: &mut Vec<String>) -> Value {
    match value {
        Value::String(value) if value.chars().count() > 500 => {
            let (replacement, removed) = truncate_string(value);
            notes.push(format!(
                "truncated {}: removed {removed} chars",
                display_pointer(pointer)
            ));
            Value::String(replacement)
        }
        Value::Array(array) if array.len() > 50 => {
            let operation = truncate_array(array, pointer);
            notes.extend(operation.notes);
            operation.replacement
        }
        Value::Array(array) => Value::Array(
            array
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    transform_nested(child, &join_pointer(pointer, &index.to_string()), notes)
                })
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, child)| {
                    (
                        key.clone(),
                        transform_nested(
                            child,
                            &join_pointer(pointer, &escape_pointer(key)),
                            notes,
                        ),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn truncate_string(value: &str) -> (String, usize) {
    let character_count = value.chars().count();
    let removed = character_count - 500;
    let mut replacement = value.chars().take(500).collect::<String>();
    replacement.push_str(&format!("…[+{removed} chars]"));
    (replacement, removed)
}

fn replace_pointer(value: &mut Value, pointer: &str, replacement: Value) -> bool {
    if pointer.is_empty() {
        *value = replacement;
        true
    } else if let Some(target) = value.pointer_mut(pointer) {
        *target = replacement;
        true
    } else {
        false
    }
}

fn join_pointer(parent: &str, segment: &str) -> String {
    format!("{parent}/{segment}")
}

fn escape_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn display_pointer(pointer: &str) -> &str {
    if pointer.is_empty() {
        "<root>"
    } else {
        pointer
    }
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::{Map, Value, json};

    use super::*;
    use crate::graph::{ContextEdge, ContextNode, EdgeKind, NodeKind, Source};
    use crate::strategy::{CompressionBudget, Lossiness, Mode, RemovalKind, StrategyRegistry};
    use crate::tokens::{TokenProvider, counter_for};

    fn node(id: &str, kind: NodeKind, content: &str, protected: bool) -> ContextNode {
        let counter = counter_for(TokenProvider::OpenAiO200k);
        ContextNode {
            id: id.to_owned(),
            kind,
            content: content.to_owned(),
            source: Source::Synthetic,
            token_count: counter.count(content),
            token_provider: counter.provider(),
            priority: 0.5,
            protected,
            recency: None,
            metadata: Map::new(),
            provider_turn: None,
        }
    }

    fn graph_with(node: ContextNode) -> ContextGraph {
        ContextGraph {
            schema_version: 1,
            nodes: vec![node],
            edges: Vec::new(),
        }
    }

    fn budget() -> CompressionBudget {
        CompressionBudget {
            max_tokens: 100_000,
            max_latency_ms: 200,
            allowed_lossiness: Lossiness::Structural,
            protected_kinds: Vec::new(),
        }
    }

    fn execute(content: &str) -> (ContextGraph, StrategyOutput) {
        let graph = graph_with(node("json", NodeKind::ToolResponse, content, false));
        let output = CompactJson
            .execute(&graph, &budget())
            .expect("JSON compaction should execute");
        (graph, output)
    }

    fn collect_key_sets(value: &Value, pointer: &str, sets: &mut Vec<(String, BTreeSet<String>)>) {
        match value {
            Value::Object(object) => {
                sets.push((pointer.to_owned(), object.keys().cloned().collect()));
                for (key, child) in object {
                    let escaped = key.replace('~', "~0").replace('/', "~1");
                    collect_key_sets(child, &format!("{pointer}/{escaped}"), sets);
                }
            }
            Value::Array(array) => {
                for (index, child) in array.iter().enumerate() {
                    collect_key_sets(child, &format!("{pointer}/{index}"), sets);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn test_minify_whitespace() {
        let content = "{\n  \"a\": 1,\n  \"b\": [1, 2, 3]\n}";
        let (graph, output) = execute(content);

        assert_eq!(output.graph.nodes[0].content, r#"{"a":1,"b":[1,2,3]}"#);
        assert!(output.graph.total_tokens() < graph.total_tokens());
        assert_eq!(output.graph.validate(), Ok(()));
    }

    #[test]
    fn test_truncate_2000_element_array() {
        let content = serde_json::to_string(&(0..2_000).collect::<Vec<_>>()).unwrap();
        let (_, output) = execute(&content);
        let parsed: Value = serde_json::from_str(&output.graph.nodes[0].content).unwrap();
        let array = parsed.as_array().unwrap();

        assert!(array.len() <= 31);
        assert!(array.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|value| value.contains("[+1970 items]"))
        }));
        assert_eq!(array[..25], (0..25).map(Value::from).collect::<Vec<_>>());
        assert_eq!(
            array[26..],
            (1_995..2_000).map(Value::from).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_long_string_elided() {
        let content = serde_json::to_string(&json!({ "value": "x".repeat(10_000) })).unwrap();
        let (_, output) = execute(&content);
        let parsed: Value = serde_json::from_str(&output.graph.nodes[0].content).unwrap();
        let value = parsed["value"].as_str().unwrap();

        assert!(value.chars().count() <= 520);
        assert!(value.ends_with("…[+9500 chars]"));
        assert_eq!(value.chars().take(500).collect::<String>(), "x".repeat(500));
    }

    #[test]
    fn test_long_string_counts_unicode_characters() {
        let content = serde_json::to_string(&json!({ "value": "🦀".repeat(600) })).unwrap();
        let (_, output) = execute(&content);
        let parsed: Value = serde_json::from_str(&output.graph.nodes[0].content).unwrap();
        let value = parsed["value"].as_str().unwrap();

        assert_eq!(
            value.chars().take(500).collect::<String>(),
            "🦀".repeat(500)
        );
        assert!(value.ends_with("…[+100 chars]"));
    }

    #[test]
    fn test_invalid_json_untouched() {
        let content = "{not json";
        let (graph, output) = execute(content);

        assert_eq!(output.graph, graph);
        assert!(output.removals.is_empty());
        assert!(
            output
                .notes
                .iter()
                .any(|note| note.contains("skipped_reason"))
        );
        assert!(!CompactJson.supports(&graph));
    }

    #[test]
    fn test_duplicate_object_keys_fail_open_at_root_and_nested_levels() {
        for content in [
            r#"{"duplicate":1,"duplicate":2}"#,
            r#"{"outer":{"duplicate":1,"duplicate":2}}"#,
        ] {
            let (graph, output) = execute(content);

            assert_eq!(output.graph, graph);
            assert!(output.removals.is_empty());
            assert!(
                output
                    .notes
                    .iter()
                    .any(|note| note.contains("skipped_reason"))
            );
            assert!(!CompactJson.supports(&graph));
        }
    }

    #[test]
    fn test_structure_preserved() {
        let content = serde_json::to_string_pretty(&json!({
            "level1": {
                "keep1": 1,
                "level2": {
                    "keep2": true,
                    "level3": {
                        "keep3": null,
                        "level4": { "keep4": "z".repeat(2_000) }
                    }
                }
            }
        }))
        .unwrap();
        let original: Value = serde_json::from_str(&content).unwrap();
        let (_, output) = execute(&content);
        let compacted: Value = serde_json::from_str(&output.graph.nodes[0].content).unwrap();
        let mut original_keys = Vec::new();
        let mut compacted_keys = Vec::new();

        collect_key_sets(&original, "", &mut original_keys);
        collect_key_sets(&compacted, "", &mut compacted_keys);
        assert_eq!(compacted_keys, original_keys);
    }

    #[test]
    fn test_nested_pointers_are_escaped_and_non_overlapping() {
        let content = serde_json::to_string(&json!({
            "a/b~c": "a".repeat(2_000),
            "nested": { "second": "b".repeat(2_000) }
        }))
        .unwrap();
        let (_, output) = execute(&content);
        let locators = output
            .removals
            .iter()
            .map(|record| record.locator.as_deref().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(locators, ["/a~1b~0c", "/nested/second"]);
        assert!(
            output
                .notes
                .iter()
                .any(|note| note.contains("truncated /a~1b~0c"))
        );
        assert!(
            output
                .notes
                .iter()
                .any(|note| note.contains("truncated /nested/second"))
        );
    }

    #[test]
    fn test_truncations_enumerated_in_notes() {
        let content = serde_json::to_string(&json!({
            "long": "x".repeat(4_000),
            "items": (0..100).collect::<Vec<_>>()
        }))
        .unwrap();
        let (_, output) = execute(&content);
        let parsed: Value = serde_json::from_str(&output.graph.nodes[0].content).unwrap();
        let marker_count = output.graph.nodes[0].content.matches("…[+").count();
        let truncated_notes = output
            .notes
            .iter()
            .filter(|note| note.starts_with("truncated "))
            .count();

        assert_eq!(marker_count, 2);
        assert_eq!(truncated_notes, marker_count);
        assert!(parsed["long"].as_str().unwrap().ends_with("[+3500 chars]"));
        assert!(parsed["items"].as_array().unwrap().iter().any(|item| {
            item.as_str()
                .is_some_and(|item| item.ends_with("[+70 items]"))
        }));

        let (_, no_elisions) = execute("{\n  \"small\": \"value\"\n}");
        assert!(
            !no_elisions
                .notes
                .iter()
                .any(|note| note.starts_with("truncated "))
        );
    }

    #[test]
    fn test_invalid_kind_protected_and_inherently_protected_are_untouched() {
        let content = format!(r#"{{"value":"{}"}}"#, "x".repeat(2_000));
        let graph = ContextGraph {
            schema_version: 1,
            nodes: vec![
                node("source", NodeKind::SourceFile, &content, false),
                node("protected", NodeKind::ToolResponse, &content, true),
                node("instruction", NodeKind::SystemInstruction, &content, false),
            ],
            edges: Vec::new(),
        };
        let output = CompactJson.execute(&graph, &budget()).unwrap();

        assert_eq!(output.graph, graph);
        assert!(output.removals.is_empty());
        assert_eq!(
            output
                .notes
                .iter()
                .filter(|note| note.contains("skipped_reason"))
                .count(),
            3
        );
    }

    #[test]
    fn test_minification_only_has_one_root_ledger_record() {
        let content = "{\n  \"a\": 1,\n  \"b\": [1, 2, 3]\n}";
        let (graph, output) = execute(content);
        let record = output
            .removals
            .first()
            .expect("minification must be recorded");
        let compacted = &output.graph.nodes[0];

        assert_eq!(output.removals.len(), 1);
        assert_eq!(record.kind, RemovalKind::JsonElision);
        assert_eq!(record.locator.as_deref(), Some(""));
        assert_eq!(
            record.original_hash,
            blake3::hash(content.as_bytes()).to_hex().to_string()
        );
        assert_eq!(
            record.replacement_hash.as_deref(),
            Some(blake3::hash(compacted.content.as_bytes()).to_hex().as_str())
        );
        assert_eq!(record.before_tokens, graph.nodes[0].token_count);
        assert_eq!(record.after_tokens, compacted.token_count);
        assert_eq!(
            record.before_tokens - record.after_tokens,
            graph.total_tokens() - output.graph.total_tokens()
        );
        assert!(
            !output
                .notes
                .iter()
                .any(|note| note.starts_with("truncated "))
        );
    }

    #[test]
    fn test_sequential_ledger_deltas_reconcile_exactly() {
        let content = format!(
            "{{\n  \"first\": {},\n  \"second\": {}\n}}",
            serde_json::to_string(&"a".repeat(4_000)).unwrap(),
            serde_json::to_string(&"b".repeat(5_000)).unwrap()
        );
        let (graph, output) = execute(&content);
        let records = &output.removals;

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].after_tokens, records[1].before_tokens);
        assert_eq!(
            records[0].replacement_hash,
            Some(records[1].original_hash.clone())
        );
        assert_eq!(
            records
                .iter()
                .map(|record| record.before_tokens - record.after_tokens)
                .sum::<usize>(),
            graph.total_tokens() - output.graph.total_tokens()
        );
        assert_eq!(
            records.last().unwrap().after_tokens,
            output.graph.nodes[0].token_count
        );
        assert_eq!(
            CompactJson.estimate(&graph).tokens_saved,
            graph.total_tokens() - output.graph.total_tokens()
        );
    }

    #[test]
    fn test_unprofitable_and_noop_json_are_untouched() {
        let unprofitable = serde_json::to_string(&json!({ "value": "a".repeat(501) })).unwrap();
        let (unprofitable_graph, unprofitable_output) = execute(&unprofitable);
        assert_eq!(unprofitable_output.graph, unprofitable_graph);
        assert!(unprofitable_output.removals.is_empty());
        assert!(
            !unprofitable_output
                .notes
                .iter()
                .any(|note| note.starts_with("truncated "))
        );

        let (noop_graph, noop_output) = execute(r#"{"a":1}"#);
        assert_eq!(noop_output.graph, noop_graph);
        assert!(noop_output.removals.is_empty());
        assert!(!CompactJson.supports(&noop_graph));
    }

    #[test]
    fn test_edges_and_provider_are_preserved() {
        let content = serde_json::to_string(&json!({ "value": "x".repeat(4_000) })).unwrap();
        let mut graph = graph_with(node("json", NodeKind::ToolResponse, &content, false));
        graph.edges.push(ContextEdge {
            from: "json".to_owned(),
            to: "json".to_owned(),
            kind: EdgeKind::References,
            weight: 1.0,
        });
        let provider = graph.nodes[0].token_provider;
        let edges = graph.edges.clone();
        let output = CompactJson.execute(&graph, &budget()).unwrap();

        assert_eq!(output.graph.edges, edges);
        assert_eq!(output.graph.nodes[0].token_provider, provider);
        assert_eq!(output.graph.validate(), Ok(()));
    }

    #[test]
    fn test_safe_registry_uses_real_compact_json_strategy() {
        let content = serde_json::to_string(&json!({ "value": "x".repeat(4_000) })).unwrap();
        let graph = graph_with(node("json", NodeKind::ToolResponse, &content, false));
        let registry = StrategyRegistry::safe_default(Mode::Safe);
        let strategy = &registry.strategies()[2];

        assert_eq!(strategy.id(), "compact_json");
        assert!(strategy.supports(&graph));
        assert!(strategy.execute(&graph, &budget()).is_ok());
    }
}
