use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::graph::{ContextGraph, ContextNode, EdgeKind, NodeKind};
use crate::strategy::{CompressionBudget, RemovalKind, RemovalRecord};
use crate::tokens::counter_for;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

/// Runs all safety checks with an empty removal ledger.
///
/// This compatibility API accepts identity candidates. Any graph delta fails
/// `removals_enumerated`; compression engines must use
/// [`run_all_with_removals`].
pub fn run_all(
    original: &ContextGraph,
    candidate: &ContextGraph,
    budget: &CompressionBudget,
    elapsed_ms: u64,
) -> Vec<SafetyCheck> {
    run_all_with_removals(original, candidate, budget, elapsed_ms, &[])
}

/// Runs all safety checks with the cumulative typed removal ledger.
pub fn run_all_with_removals(
    original: &ContextGraph,
    candidate: &ContextGraph,
    budget: &CompressionBudget,
    elapsed_ms: u64,
    removals: &[RemovalRecord],
) -> Vec<SafetyCheck> {
    vec![
        protected_unchanged(original, candidate, budget),
        no_inflation(original, candidate),
        json_still_valid(original, candidate),
        fences_balanced(original, candidate),
        refs_valid(original, candidate, removals),
        provider_conversation_valid(original, candidate),
        minimum_output(original, candidate),
        token_counts_valid(candidate),
        latency_ok(budget, elapsed_ms),
        schema_valid(candidate),
        removals_enumerated(original, candidate, removals),
    ]
}

fn safety_check(name: &str, passed: bool, pass_detail: &str, fail_detail: &str) -> SafetyCheck {
    SafetyCheck {
        name: name.to_owned(),
        passed,
        detail: if passed { pass_detail } else { fail_detail }.to_owned(),
    }
}

fn protected_unchanged(
    original: &ContextGraph,
    candidate: &ContextGraph,
    budget: &CompressionBudget,
) -> SafetyCheck {
    let candidate_nodes = node_map(candidate);
    let protected = original.nodes.iter().filter(|node| {
        node.protected
            || node.kind.requires_protection()
            || budget.protected_kinds.contains(&node.kind)
    });
    let passed = protected.into_iter().all(|node| {
        candidate_nodes
            .get(node.id.as_str())
            .is_some_and(|value| *value == node)
    });

    safety_check(
        "protected_unchanged",
        passed,
        "protected nodes are byte-identical",
        "one or more protected nodes changed or were removed",
    )
}

fn no_inflation(original: &ContextGraph, candidate: &ContextGraph) -> SafetyCheck {
    let passed = candidate.total_tokens() <= original.total_tokens();
    safety_check(
        "no_inflation",
        passed,
        "candidate token count does not exceed input",
        "candidate token count exceeds input",
    )
}

fn json_still_valid(original: &ContextGraph, candidate: &ContextGraph) -> SafetyCheck {
    let candidate_nodes = node_map(candidate);
    let passed = original.nodes.iter().all(|node| {
        if serde_json::from_str::<Value>(&node.content).is_err() {
            return true;
        }
        candidate_nodes
            .get(node.id.as_str())
            .is_none_or(|candidate| serde_json::from_str::<Value>(&candidate.content).is_ok())
    });

    safety_check(
        "json_still_valid",
        passed,
        "original JSON values remain valid",
        "an original JSON value became invalid",
    )
}

fn fences_balanced(original: &ContextGraph, candidate: &ContextGraph) -> SafetyCheck {
    let original_nodes = node_map(original);
    let passed = candidate.nodes.iter().all(|node| {
        let candidate_balanced = content_fences_balanced(&node.content);
        original_nodes.get(node.id.as_str()).is_none_or(|original| {
            if content_fences_balanced(&original.content) {
                candidate_balanced
            } else {
                original.content == node.content
            }
        })
    });

    safety_check(
        "fences_balanced",
        passed,
        "fenced blocks remain balanced",
        "a fenced block is unbalanced or malformed input changed",
    )
}

fn content_fences_balanced(content: &str) -> bool {
    let mut open: Option<(u8, usize)> = None;
    for line in content.lines() {
        let Some((marker, length, remainder)) = fence_run(line) else {
            continue;
        };

        if let Some((open_marker, open_length)) = open {
            if marker == open_marker
                && length >= open_length
                && remainder.bytes().all(|byte| byte.is_ascii_whitespace())
            {
                open = None;
            }
        } else if marker != b'`' || !remainder.contains('`') {
            open = Some((marker, length));
        }
    }
    open.is_none()
}

fn fence_run(line: &str) -> Option<(u8, usize, &str)> {
    let bytes = line.as_bytes();
    let leading_spaces = bytes.iter().take_while(|byte| **byte == b' ').count();
    if leading_spaces > 3 {
        return None;
    }
    let marker = *bytes.get(leading_spaces)?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = bytes[leading_spaces..]
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    (length >= 3).then(|| (marker, length, &line[leading_spaces + length..]))
}

fn refs_valid(
    original: &ContextGraph,
    candidate: &ContextGraph,
    removals: &[RemovalRecord],
) -> SafetyCheck {
    let candidate_nodes = node_map(candidate);
    let original_nodes = node_map(original);
    let endpoints_valid = candidate.edges.iter().all(|edge| {
        candidate_nodes.contains_key(edge.from.as_str())
            && candidate_nodes.contains_key(edge.to.as_str())
    });

    let mut duplicate_edges = HashMap::<&str, Vec<&str>>::new();
    let duplicate_edges_valid = candidate
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::DuplicateOf)
        .all(|edge| {
            duplicate_edges
                .entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
            edge.from != edge.to
                && original_nodes
                    .get(edge.to.as_str())
                    .is_some_and(|original_target| {
                        candidate_nodes
                            .get(edge.to.as_str())
                            .is_some_and(|target| target.content == original_target.content)
                    })
        });
    let duplicate_edge_counts_valid = duplicate_edges.values().all(|targets| targets.len() == 1);

    let duplicate_records_valid = removals
        .iter()
        .filter(|record| record.kind == RemovalKind::DuplicateReference)
        .all(|record| {
            let Some(target_id) = record.locator.as_deref() else {
                return false;
            };
            if !candidate_nodes.contains_key(record.node_id.as_str()) {
                return true;
            }
            let target_preserved = original_nodes
                .get(target_id)
                .is_some_and(|original_target| {
                    candidate_nodes
                        .get(target_id)
                        .is_some_and(|target| target.content == original_target.content)
                });
            let edge_agrees = duplicate_edges
                .get(record.node_id.as_str())
                .is_none_or(|targets| targets.as_slice() == [target_id]);
            target_id != record.node_id && target_preserved && edge_agrees
        });

    let passed = endpoints_valid
        && duplicate_edges_valid
        && duplicate_edge_counts_valid
        && duplicate_records_valid;
    safety_check(
        "refs_valid",
        passed,
        "graph and dedup references resolve",
        "one or more graph or dedup references are invalid",
    )
}

fn provider_conversation_valid(original: &ContextGraph, candidate: &ContextGraph) -> SafetyCheck {
    let original_nodes = node_map(original);
    let candidate_nodes = node_map(candidate);

    let reasoning_immutable = original
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::AssistantReasoning)
        .all(|node| {
            candidate_nodes
                .get(node.id.as_str())
                .is_some_and(|value| *value == node)
        })
        && candidate
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::AssistantReasoning)
            .all(|node| original_nodes.contains_key(node.id.as_str()));

    let provider_nodes_valid = candidate.nodes.iter().all(|node| {
        let Some(turn) = &node.provider_turn else {
            return true;
        };
        !turn.provider.is_empty()
            && !turn.turn_id.is_empty()
            && !turn.tool_call_id.is_empty()
            && turn
                .opaque_state_hash
                .as_ref()
                .is_none_or(|hash| !hash.is_empty())
            && matches!(
                node.kind,
                NodeKind::AssistantResponse
                    | NodeKind::AssistantReasoning
                    | NodeKind::ToolRequest
                    | NodeKind::ToolResponse
            )
            && original_nodes
                .get(node.id.as_str())
                .is_some_and(|original| original.provider_turn == node.provider_turn)
    });

    let mut original_turns = BTreeMap::<(String, String), Vec<&ContextNode>>::new();
    for node in &original.nodes {
        if let Some(turn) = &node.provider_turn {
            original_turns
                .entry((turn.provider.clone(), turn.turn_id.clone()))
                .or_default()
                .push(node);
        }
    }
    let turns_atomic = original_turns.values().all(|nodes| {
        let retained = nodes
            .iter()
            .filter(|node| candidate_nodes.contains_key(node.id.as_str()))
            .count();
        let all_or_none = retained == 0 || retained == nodes.len();
        let opaque_immutable = retained == 0
            || !nodes.iter().any(|node| {
                node.provider_turn
                    .as_ref()
                    .is_some_and(|turn| turn.opaque_state_hash.is_some())
            })
            || nodes.iter().all(|node| {
                candidate_nodes
                    .get(node.id.as_str())
                    .is_some_and(|value| *value == *node)
            });
        all_or_none && opaque_immutable
    });

    let candidate_pairs_valid = provider_pairs_valid(candidate);
    let dependencies_valid = original.edges.iter().all(|edge| {
        if edge.kind != EdgeKind::DependsOn || !candidate_nodes.contains_key(edge.from.as_str()) {
            return true;
        }
        original_nodes.get(edge.to.as_str()).is_none_or(|target| {
            target.provider_turn.is_none() || candidate_nodes.contains_key(edge.to.as_str())
        })
    });

    let passed = reasoning_immutable
        && provider_nodes_valid
        && turns_atomic
        && candidate_pairs_valid
        && dependencies_valid;
    safety_check(
        "provider_conversation_valid",
        passed,
        "provider turns, tool pairs, and reasoning are valid",
        "provider turn atomicity, tool pairing, or reasoning immutability failed",
    )
}

fn provider_pairs_valid(graph: &ContextGraph) -> bool {
    let provider_shaped = graph.nodes.iter().any(|node| node.provider_turn.is_some());
    if !provider_shaped {
        return true;
    }

    let mut owners = BTreeMap::<(String, String), Vec<&str>>::new();
    let mut requests = BTreeMap::<(String, String, String), Vec<&str>>::new();
    let mut responses = BTreeMap::<(String, String, String), Vec<&str>>::new();
    for node in &graph.nodes {
        let Some(turn) = &node.provider_turn else {
            continue;
        };
        let turn_key = (turn.provider.clone(), turn.turn_id.clone());
        let call_key = (
            turn.provider.clone(),
            turn.turn_id.clone(),
            turn.tool_call_id.clone(),
        );
        match node.kind {
            NodeKind::AssistantResponse => owners.entry(turn_key).or_default().push(&node.id),
            NodeKind::ToolRequest => requests.entry(call_key).or_default().push(&node.id),
            NodeKind::ToolResponse => responses.entry(call_key).or_default().push(&node.id),
            NodeKind::AssistantReasoning => {}
            _ => return false,
        }
    }

    let turns = graph
        .nodes
        .iter()
        .filter_map(|node| node.provider_turn.as_ref())
        .map(|turn| (turn.provider.clone(), turn.turn_id.clone()))
        .collect::<BTreeSet<_>>();
    if !turns
        .iter()
        .all(|turn| owners.get(turn).is_some_and(|values| values.len() == 1))
    {
        return false;
    }

    let calls = requests
        .keys()
        .chain(responses.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut expected_edges = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(request) = requests.get(&call).filter(|values| values.len() == 1) else {
            return false;
        };
        let Some(response) = responses.get(&call).filter(|values| values.len() == 1) else {
            return false;
        };
        expected_edges.push((response[0], request[0]));
    }

    let actual_edges = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::RespondsTo)
        .collect::<Vec<_>>();
    actual_edges.len() == expected_edges.len()
        && expected_edges.iter().all(|(response, request)| {
            actual_edges
                .iter()
                .filter(|edge| edge.from == *response && edge.to == *request)
                .count()
                == 1
        })
}

fn minimum_output(original: &ContextGraph, candidate: &ContextGraph) -> SafetyCheck {
    let nodes_present = original.nodes.is_empty() || !candidate.nodes.is_empty();
    let meaningful_content = !original.nodes.iter().any(|node| !node.content.is_empty())
        || candidate.nodes.iter().any(|node| !node.content.is_empty());
    let passed = nodes_present && meaningful_content;
    safety_check(
        "minimum_output",
        passed,
        "non-empty input retains meaningful output",
        "non-empty input produced empty output",
    )
}

fn token_counts_valid(candidate: &ContextGraph) -> SafetyCheck {
    let passed = candidate
        .nodes
        .iter()
        .all(|node| counter_for(node.token_provider).count(&node.content) == node.token_count);
    safety_check(
        "token_counts_valid",
        passed,
        "candidate token counts match provider recounts",
        "one or more candidate token counts are stale",
    )
}

fn latency_ok(budget: &CompressionBudget, elapsed_ms: u64) -> SafetyCheck {
    let passed = elapsed_ms <= budget.max_latency_ms;
    safety_check(
        "latency_ok",
        passed,
        "elapsed time is within budget",
        "elapsed time exceeded the latency budget",
    )
}

fn schema_valid(candidate: &ContextGraph) -> SafetyCheck {
    let nodes_valid = candidate.nodes.iter().all(|node| {
        !node.id.is_empty()
            && node.priority.is_finite()
            && (0.0..=1.0).contains(&node.priority)
            && node.provider_turn.as_ref().is_none_or(|turn| {
                !turn.provider.is_empty()
                    && !turn.turn_id.is_empty()
                    && !turn.tool_call_id.is_empty()
            })
    });
    let edges_valid = candidate
        .edges
        .iter()
        .all(|edge| edge.weight.is_finite() && (0.0..=1.0).contains(&edge.weight));
    let passed = candidate.validate().is_ok() && nodes_valid && edges_valid;
    safety_check(
        "schema_valid",
        passed,
        "candidate satisfies the graph schema and invariants",
        "candidate violates the graph schema or invariants",
    )
}

#[derive(Clone, PartialEq, Eq)]
enum RemovalRegion {
    Whole,
    Lines { start: usize, end: usize },
    Json(Vec<String>),
}

fn removals_enumerated(
    original: &ContextGraph,
    candidate: &ContextGraph,
    removals: &[RemovalRecord],
) -> SafetyCheck {
    let passed = removal_ledger_valid(original, candidate, removals);
    safety_check(
        "removals_enumerated",
        passed,
        "typed removal ledger exactly reconciles the graph delta",
        "typed removal ledger is missing, overlapping, or inconsistent",
    )
}

fn removal_ledger_valid(
    original: &ContextGraph,
    candidate: &ContextGraph,
    removals: &[RemovalRecord],
) -> bool {
    if removals.is_empty() {
        return original == candidate;
    }
    if original.schema_version != candidate.schema_version {
        return false;
    }

    let original_nodes = node_map(original);
    let candidate_nodes = node_map(candidate);
    if original_nodes.len() != original.nodes.len()
        || candidate_nodes.len() != candidate.nodes.len()
        || candidate_nodes
            .keys()
            .any(|id| !original_nodes.contains_key(id))
    {
        return false;
    }

    let candidate_ids = candidate_nodes.keys().copied().collect::<HashSet<_>>();
    let expected_order = original
        .nodes
        .iter()
        .filter(|node| candidate_ids.contains(node.id.as_str()))
        .map(|node| node.id.as_str());
    if !expected_order.eq(candidate.nodes.iter().map(|node| node.id.as_str())) {
        return false;
    }

    let mut records_by_node = BTreeMap::<&str, Vec<&RemovalRecord>>::new();
    for record in removals {
        if record.strategy_id.is_empty()
            || !original_nodes.contains_key(record.node_id.as_str())
            || !valid_hash(&record.original_hash)
            || record
                .replacement_hash
                .as_ref()
                .is_some_and(|hash| !valid_hash(hash))
        {
            return false;
        }
        records_by_node
            .entry(record.node_id.as_str())
            .or_default()
            .push(record);
    }

    for node in &original.nodes {
        let records = records_by_node
            .get(node.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let candidate_node = candidate_nodes.get(node.id.as_str()).copied();
        if records.is_empty() {
            if candidate_node != Some(node) {
                return false;
            }
            continue;
        }
        if candidate_node.is_some_and(|candidate| !node_shape_unchanged(node, candidate))
            || !record_regions_do_not_overlap(records)
            || !record_chain_valid(node, candidate_node, records)
        {
            return false;
        }
    }

    if !edge_delta_valid(original, candidate, removals, &candidate_ids) {
        return false;
    }

    let Some(original_tokens) = checked_total_tokens(original) else {
        return false;
    };
    let Some(candidate_tokens) = checked_total_tokens(candidate) else {
        return false;
    };
    let Some(graph_delta) = original_tokens.checked_sub(candidate_tokens) else {
        return false;
    };
    let Some(record_delta) = removals.iter().try_fold(0_usize, |total, record| {
        record
            .before_tokens
            .checked_sub(record.after_tokens)
            .and_then(|delta| total.checked_add(delta))
    }) else {
        return false;
    };
    graph_delta == record_delta
}

fn record_chain_valid(
    original: &ContextNode,
    candidate: Option<&ContextNode>,
    records: &[&RemovalRecord],
) -> bool {
    let mut current_hash = content_hash(&original.content);
    let mut current_tokens = original.token_count;
    let mut dropped = false;

    for (index, record) in records.iter().enumerate() {
        if dropped
            || record.original_hash != current_hash
            || record.before_tokens != current_tokens
            || record.after_tokens > record.before_tokens
            || !record_shape_valid(record, original, candidate)
        {
            return false;
        }
        match &record.replacement_hash {
            Some(hash) => {
                if record.kind == RemovalKind::DroppedNode
                    || record.after_tokens == record.before_tokens
                {
                    return false;
                }
                current_hash.clone_from(hash);
                current_tokens = record.after_tokens;
            }
            None => {
                if record.kind != RemovalKind::DroppedNode
                    || record.after_tokens != 0
                    || index + 1 != records.len()
                {
                    return false;
                }
                dropped = true;
                current_tokens = 0;
            }
        }
    }

    match (candidate, dropped) {
        (None, true) => true,
        (Some(candidate), false) => {
            current_hash == content_hash(&candidate.content)
                && current_tokens == candidate.token_count
        }
        _ => false,
    }
}

fn record_shape_valid(
    record: &RemovalRecord,
    original: &ContextNode,
    candidate: Option<&ContextNode>,
) -> bool {
    let Some(units) = record.units_removed else {
        return false;
    };
    if units == 0 {
        return false;
    }
    match record.kind {
        RemovalKind::DuplicateReference => {
            record
                .locator
                .as_ref()
                .is_some_and(|value| !value.is_empty())
                && record.replacement_hash.is_some()
                && units == 1
        }
        RemovalKind::CollapsedLines => parse_line_locator(record)
            .is_some_and(|(_, end)| end <= logical_line_count(&original.content)),
        RemovalKind::JsonElision => {
            record.replacement_hash.is_some()
                && json_elision_semantically_valid(record, original, candidate)
        }
        RemovalKind::DroppedNode => {
            record.locator.is_none()
                && record.replacement_hash.is_none()
                && record.after_tokens == 0
                && units == 1
        }
    }
}

fn logical_line_count(content: &str) -> usize {
    content.lines().count()
}

fn json_elision_semantically_valid(
    record: &RemovalRecord,
    original: &ContextNode,
    candidate: Option<&ContextNode>,
) -> bool {
    let Some(pointer) = record.locator.as_deref() else {
        return false;
    };
    if parse_json_pointer(pointer).is_none() {
        return false;
    }
    let Ok(original_json) = serde_json::from_str::<Value>(&original.content) else {
        return false;
    };
    let Some(original_value) = original_json.pointer(pointer) else {
        return false;
    };
    let Some(candidate) = candidate else {
        return false;
    };
    let Ok(candidate_json) = serde_json::from_str::<Value>(&candidate.content) else {
        return false;
    };
    let Some(candidate_value) = candidate_json.pointer(pointer) else {
        return false;
    };

    // A root record whose parsed value is unchanged is the documented
    // minification-only case; it has no semantic sentinel to reconcile.
    if pointer.is_empty() && original_json == candidate_json {
        return true;
    }

    let Some(units) = record.units_removed else {
        return false;
    };
    match (original_value, candidate_value) {
        (Value::String(original), Value::String(candidate)) => {
            let marker = format!("…[+{units} chars]");
            candidate.strip_suffix(&marker).is_some_and(|prefix| {
                prefix.chars().count() == 500
                    && original.chars().count() == 500 + units
                    && prefix == original.chars().take(500).collect::<String>()
            })
        }
        (Value::Array(original), Value::Array(candidate)) => {
            let sentinel = format!("…[+{units} items]");
            original.len() > 50
                && units == original.len() - 30
                && candidate.len() == 31
                && candidate.get(25).and_then(Value::as_str) == Some(sentinel.as_str())
        }
        _ => false,
    }
}

fn record_regions_do_not_overlap(records: &[&RemovalRecord]) -> bool {
    let Some(regions) = records
        .iter()
        .map(|record| removal_region(record))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    for left in 0..regions.len() {
        for right in left + 1..regions.len() {
            if regions_overlap(&regions[left], &regions[right]) {
                return false;
            }
        }
    }
    true
}

fn removal_region(record: &RemovalRecord) -> Option<RemovalRegion> {
    match record.kind {
        RemovalKind::DuplicateReference | RemovalKind::DroppedNode => Some(RemovalRegion::Whole),
        RemovalKind::CollapsedLines => {
            parse_line_locator(record).map(|(start, end)| RemovalRegion::Lines { start, end })
        }
        RemovalKind::JsonElision => record
            .locator
            .as_deref()
            .and_then(parse_json_pointer)
            .map(RemovalRegion::Json),
    }
}

fn regions_overlap(left: &RemovalRegion, right: &RemovalRegion) -> bool {
    match (left, right) {
        (RemovalRegion::Whole, _) | (_, RemovalRegion::Whole) => true,
        (
            RemovalRegion::Lines {
                start: left_start,
                end: left_end,
            },
            RemovalRegion::Lines {
                start: right_start,
                end: right_end,
            },
        ) => left_start <= right_end && right_start <= left_end,
        (RemovalRegion::Json(left), RemovalRegion::Json(right)) => {
            pointer_is_prefix(left, right) || pointer_is_prefix(right, left)
        }
        _ => true,
    }
}

fn pointer_is_prefix(left: &[String], right: &[String]) -> bool {
    left.len() <= right.len() && left.iter().zip(right).all(|(left, right)| left == right)
}

fn parse_line_locator(record: &RemovalRecord) -> Option<(usize, usize)> {
    let locator = record.locator.as_deref()?.strip_prefix("lines:")?;
    let (start, end) = locator.split_once('-')?;
    let start = start.parse::<usize>().ok()?;
    let end = end.parse::<usize>().ok()?;
    (start >= 1 && end >= start && record.units_removed == Some(end.saturating_sub(start)))
        .then_some((start, end))
}

fn parse_json_pointer(pointer: &str) -> Option<Vec<String>> {
    if pointer.is_empty() {
        return Some(Vec::new());
    }
    let segments = pointer.strip_prefix('/')?.split('/');
    segments.map(unescape_pointer_segment).collect()
}

fn unescape_pointer_segment(segment: &str) -> Option<String> {
    let mut output = String::with_capacity(segment.len());
    let mut chars = segment.chars();
    while let Some(character) = chars.next() {
        if character != '~' {
            output.push(character);
            continue;
        }
        match chars.next()? {
            '0' => output.push('~'),
            '1' => output.push('/'),
            _ => return None,
        }
    }
    Some(output)
}

fn edge_delta_valid(
    original: &ContextGraph,
    candidate: &ContextGraph,
    removals: &[RemovalRecord],
    candidate_ids: &HashSet<&str>,
) -> bool {
    let expected_retained = original
        .edges
        .iter()
        .filter(|edge| {
            candidate_ids.contains(edge.from.as_str()) && candidate_ids.contains(edge.to.as_str())
        })
        .collect::<Vec<_>>();
    let existing_candidate = candidate
        .edges
        .iter()
        .filter(|edge| original.edges.contains(edge))
        .collect::<Vec<_>>();
    if expected_retained != existing_candidate {
        return false;
    }

    candidate
        .edges
        .iter()
        .filter(|edge| !original.edges.contains(edge))
        .all(|edge| {
            edge.kind == EdgeKind::DuplicateOf
                && removals.iter().any(|record| {
                    record.kind == RemovalKind::DuplicateReference
                        && record.node_id == edge.from
                        && record.locator.as_deref() == Some(edge.to.as_str())
                })
        })
}

fn node_shape_unchanged(original: &ContextNode, candidate: &ContextNode) -> bool {
    original.id == candidate.id
        && original.kind == candidate.kind
        && original.source == candidate.source
        && original.token_provider == candidate.token_provider
        && original.priority == candidate.priority
        && original.protected == candidate.protected
        && original.recency == candidate.recency
        && original.metadata == candidate.metadata
        && original.provider_turn == candidate.provider_turn
}

fn checked_total_tokens(graph: &ContextGraph) -> Option<usize> {
    graph
        .nodes
        .iter()
        .try_fold(0_usize, |total, node| total.checked_add(node.token_count))
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn node_map(graph: &ContextGraph) -> BTreeMap<&str, &ContextNode> {
    graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect()
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::*;
    use crate::graph::{ContextEdge, ContextNode, EdgeKind, NodeKind, ProviderTurn, Source};
    use crate::strategies::{CollapseLogs, CompactJson, DedupExact, PackBudget};
    use crate::strategy::CompressionStrategy;
    use crate::strategy::{Lossiness, RemovalKind};
    use crate::tokens::{TokenProvider, counter_for};

    const CHECK_NAMES: [&str; 11] = [
        "protected_unchanged",
        "no_inflation",
        "json_still_valid",
        "fences_balanced",
        "refs_valid",
        "provider_conversation_valid",
        "minimum_output",
        "token_counts_valid",
        "latency_ok",
        "schema_valid",
        "removals_enumerated",
    ];

    fn node(id: &str, kind: NodeKind, content: &str, protected: bool) -> ContextNode {
        let counter = counter_for(TokenProvider::Heuristic);
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

    fn graph(nodes: Vec<ContextNode>) -> ContextGraph {
        ContextGraph {
            schema_version: 1,
            nodes,
            edges: Vec::new(),
        }
    }

    fn budget() -> CompressionBudget {
        CompressionBudget {
            max_tokens: 10_000,
            max_latency_ms: 200,
            allowed_lossiness: Lossiness::Structural,
            protected_kinds: Vec::new(),
        }
    }

    fn check<'a>(checks: &'a [SafetyCheck], name: &str) -> &'a SafetyCheck {
        checks.iter().find(|check| check.name == name).unwrap()
    }

    fn drop_record(node: &ContextNode) -> RemovalRecord {
        RemovalRecord {
            strategy_id: "pack_budget".to_owned(),
            node_id: node.id.clone(),
            kind: RemovalKind::DroppedNode,
            locator: None,
            original_hash: content_hash(&node.content),
            replacement_hash: None,
            before_tokens: node.token_count,
            after_tokens: 0,
            units_removed: Some(1),
        }
    }

    #[test]
    fn test_all_pass_identity_and_exact_order() {
        let original = graph(vec![node("anchor", NodeKind::Summary, "stable", false)]);
        let checks = run_all(&original, &original, &budget(), 10);

        assert_eq!(
            checks
                .iter()
                .map(|check| check.name.as_str())
                .collect::<Vec<_>>(),
            CHECK_NAMES
        );
        assert!(checks.iter().all(|check| check.passed));
    }

    #[test]
    fn test_protected_unchanged_fails_on_mutation() {
        let original = graph(vec![node(
            "instruction",
            NodeKind::SystemInstruction,
            "stable",
            true,
        )]);
        let mut candidate = original.clone();
        candidate.nodes[0].content = "altered".to_owned();
        candidate.nodes[0].token_count = counter_for(TokenProvider::Heuristic).count("altered");

        assert!(
            !check(
                &run_all(&original, &candidate, &budget(), 1),
                "protected_unchanged"
            )
            .passed
        );
    }

    #[test]
    fn test_no_inflation_fails_when_tokens_increase() {
        let original = graph(vec![node("text", NodeKind::Summary, "short", false)]);
        let mut candidate = original.clone();
        candidate.nodes[0].content = "a much longer replacement value".to_owned();
        candidate.nodes[0].token_count =
            counter_for(TokenProvider::Heuristic).count(&candidate.nodes[0].content);

        assert!(
            !check(
                &run_all(&original, &candidate, &budget(), 1),
                "no_inflation"
            )
            .passed
        );
    }

    #[test]
    fn test_json_still_valid_fails_only_for_original_json() {
        let original = graph(vec![node(
            "json",
            NodeKind::ToolResponse,
            r#"{"a":1}"#,
            false,
        )]);
        let mut candidate = original.clone();
        candidate.nodes[0].content = r#"{"a":"#.to_owned();
        candidate.nodes[0].token_count =
            counter_for(TokenProvider::Heuristic).count(&candidate.nodes[0].content);

        assert!(
            !check(
                &run_all(&original, &candidate, &budget(), 1),
                "json_still_valid"
            )
            .passed
        );

        let invalid_original = graph(vec![node(
            "text",
            NodeKind::ToolResponse,
            "{not json",
            false,
        )]);
        assert!(
            check(
                &run_all(&invalid_original, &invalid_original, &budget(), 1),
                "json_still_valid"
            )
            .passed
        );
    }

    #[test]
    fn test_fences_balanced_fails_on_unclosed_candidate() {
        let original = graph(vec![node(
            "code",
            NodeKind::AssistantResponse,
            "```rust\nfn main() {}\n```\n",
            false,
        )]);
        let mut candidate = original.clone();
        candidate.nodes[0].content = "```rust\nfn main() {}\n".to_owned();
        candidate.nodes[0].token_count =
            counter_for(TokenProvider::Heuristic).count(&candidate.nodes[0].content);

        assert!(
            !check(
                &run_all(&original, &candidate, &budget(), 1),
                "fences_balanced"
            )
            .passed
        );
    }

    #[test]
    fn test_refs_valid_fails_on_dangling_endpoint() {
        let original = graph(vec![node("a", NodeKind::Summary, "a", false)]);
        let mut candidate = original.clone();
        candidate.edges.push(ContextEdge {
            from: "a".to_owned(),
            to: "missing".to_owned(),
            kind: EdgeKind::References,
            weight: 1.0,
        });

        assert!(!check(&run_all(&original, &candidate, &budget(), 1), "refs_valid").passed);
    }

    #[test]
    fn test_provider_conversation_valid_fails_on_orphan_response() {
        let mut response = node("response", NodeKind::ToolResponse, "result", false);
        response.provider_turn = Some(ProviderTurn {
            provider: "fixture".to_owned(),
            turn_id: "turn-1".to_owned(),
            tool_call_id: "call-1".to_owned(),
            opaque_state_hash: None,
        });
        let candidate = graph(vec![response]);

        assert!(
            !check(
                &run_all(&candidate, &candidate, &budget(), 1),
                "provider_conversation_valid"
            )
            .passed
        );
    }

    #[test]
    fn test_complete_provider_tool_turn_passes() {
        let turn = ProviderTurn {
            provider: "fixture".to_owned(),
            turn_id: "turn-1".to_owned(),
            tool_call_id: "call-1".to_owned(),
            opaque_state_hash: None,
        };
        let mut owner = node("owner", NodeKind::AssistantResponse, "calling tool", false);
        owner.provider_turn = Some(turn.clone());
        let mut request = node("request", NodeKind::ToolRequest, "request", true);
        request.provider_turn = Some(turn.clone());
        let mut response = node("response", NodeKind::ToolResponse, "response", false);
        response.provider_turn = Some(turn);
        let mut original = graph(vec![owner, request, response]);
        original.edges.push(ContextEdge {
            from: "response".to_owned(),
            to: "request".to_owned(),
            kind: EdgeKind::RespondsTo,
            weight: 1.0,
        });

        assert!(
            run_all(&original, &original, &budget(), 1)
                .iter()
                .all(|check| check.passed)
        );
    }

    #[test]
    fn test_provider_turn_rejects_wrong_or_extra_responds_to_edges() {
        let turn = ProviderTurn {
            provider: "fixture".to_owned(),
            turn_id: "turn-1".to_owned(),
            tool_call_id: "call-1".to_owned(),
            opaque_state_hash: None,
        };
        let mut owner = node("owner", NodeKind::AssistantResponse, "owner", false);
        owner.provider_turn = Some(turn.clone());
        let mut request = node("request", NodeKind::ToolRequest, "request", true);
        request.provider_turn = Some(turn.clone());
        let mut response = node("response", NodeKind::ToolResponse, "response", false);
        response.provider_turn = Some(turn);

        let mut wrong = graph(vec![owner.clone(), request.clone(), response.clone()]);
        wrong.edges.push(ContextEdge {
            from: "request".to_owned(),
            to: "response".to_owned(),
            kind: EdgeKind::RespondsTo,
            weight: 1.0,
        });
        assert!(
            !check(
                &run_all(&wrong, &wrong, &budget(), 1),
                "provider_conversation_valid"
            )
            .passed
        );

        let mut extra = graph(vec![owner, request, response]);
        extra.edges = vec![
            ContextEdge {
                from: "response".to_owned(),
                to: "request".to_owned(),
                kind: EdgeKind::RespondsTo,
                weight: 1.0,
            },
            ContextEdge {
                from: "request".to_owned(),
                to: "response".to_owned(),
                kind: EdgeKind::RespondsTo,
                weight: 1.0,
            },
        ];
        assert!(
            !check(
                &run_all(&extra, &extra, &budget(), 1),
                "provider_conversation_valid"
            )
            .passed
        );
    }

    #[test]
    fn test_minimum_output_fails_for_empty_candidate() {
        let original = graph(vec![
            node("anchor", NodeKind::Summary, "anchor", false),
            node("drop", NodeKind::Summary, "drop", false),
        ]);
        let candidate = graph(Vec::new());

        assert!(
            !check(
                &run_all_with_removals(
                    &original,
                    &candidate,
                    &budget(),
                    1,
                    &original.nodes.iter().map(drop_record).collect::<Vec<_>>()
                ),
                "minimum_output"
            )
            .passed
        );
    }

    #[test]
    fn test_token_counts_valid_fails_on_stale_count() {
        let original = graph(vec![node("text", NodeKind::Summary, "stable", false)]);
        let mut candidate = original.clone();
        candidate.nodes[0].token_count += 1;

        assert!(
            !check(
                &run_all(&original, &candidate, &budget(), 1),
                "token_counts_valid"
            )
            .passed
        );
    }

    #[test]
    fn test_latency_ok_fails_over_budget() {
        let original = graph(vec![node("text", NodeKind::Summary, "stable", false)]);

        assert!(!check(&run_all(&original, &original, &budget(), 201), "latency_ok").passed);
    }

    #[test]
    fn test_schema_valid_fails_on_unknown_version() {
        let original = graph(vec![node("text", NodeKind::Summary, "stable", false)]);
        let mut candidate = original.clone();
        candidate.schema_version = 2;

        assert!(
            !check(
                &run_all(&original, &candidate, &budget(), 1),
                "schema_valid"
            )
            .passed
        );
    }

    #[test]
    fn test_wrapper_changed_candidate_fails_removals_enumerated() {
        let original = graph(vec![
            node("anchor", NodeKind::Summary, "anchor", false),
            node("drop", NodeKind::Summary, "drop", false),
        ]);
        let candidate = graph(vec![original.nodes[0].clone()]);

        assert!(
            !check(
                &run_all(&original, &candidate, &budget(), 1),
                "removals_enumerated"
            )
            .passed
        );
    }

    #[test]
    fn test_ledger_aware_exact_removal_passes() {
        let original = graph(vec![
            node("anchor", NodeKind::Summary, "anchor", false),
            node("drop", NodeKind::Summary, "drop", false),
        ]);
        let candidate = graph(vec![original.nodes[0].clone()]);
        let removals = vec![drop_record(&original.nodes[1])];

        assert!(
            check(
                &run_all_with_removals(&original, &candidate, &budget(), 1, &removals),
                "removals_enumerated"
            )
            .passed
        );
    }

    #[test]
    fn test_ledger_mismatch_fails() {
        let original = graph(vec![
            node("anchor", NodeKind::Summary, "anchor", false),
            node("drop", NodeKind::Summary, "drop", false),
        ]);
        let candidate = graph(vec![original.nodes[0].clone()]);
        let mut removal = drop_record(&original.nodes[1]);
        removal.before_tokens += 1;

        assert!(
            !check(
                &run_all_with_removals(&original, &candidate, &budget(), 1, &[removal]),
                "removals_enumerated"
            )
            .passed
        );
    }

    #[test]
    fn test_invalid_removal_record_fails() {
        let original = graph(vec![
            node("anchor", NodeKind::Summary, "anchor", false),
            node("drop", NodeKind::Summary, "drop", false),
        ]);
        let candidate = graph(vec![original.nodes[0].clone()]);
        let mut removal = drop_record(&original.nodes[1]);
        removal.locator = Some("not-valid-for-a-dropped-node".to_owned());

        assert!(
            !check(
                &run_all_with_removals(&original, &candidate, &budget(), 1, &[removal]),
                "removals_enumerated"
            )
            .passed
        );
    }

    #[test]
    fn test_out_of_range_line_locator_fails() {
        let counter = counter_for(TokenProvider::Heuristic);
        let original_content = "aaaa\nbbbb\n";
        let candidate_content = "aaaa\n";
        let original = graph(vec![node(
            "log",
            NodeKind::ToolResponse,
            original_content,
            false,
        )]);
        let candidate = graph(vec![node(
            "log",
            NodeKind::ToolResponse,
            candidate_content,
            false,
        )]);
        let removal = RemovalRecord {
            strategy_id: "collapse_logs".to_owned(),
            node_id: "log".to_owned(),
            kind: RemovalKind::CollapsedLines,
            locator: Some("lines:5-6".to_owned()),
            original_hash: content_hash(original_content),
            replacement_hash: Some(content_hash(candidate_content)),
            before_tokens: counter.count(original_content),
            after_tokens: counter.count(candidate_content),
            units_removed: Some(1),
        };

        assert!(
            !check(
                &run_all_with_removals(&original, &candidate, &budget(), 1, &[removal]),
                "removals_enumerated"
            )
            .passed
        );
    }

    #[test]
    fn test_nonexistent_json_pointer_fails() {
        let original_value = "x".repeat(600);
        let candidate_value = format!("{}…[+100 chars]", "x".repeat(500));
        let original_content = serde_json::to_string(&json!({ "actual": original_value })).unwrap();
        let candidate_content =
            serde_json::to_string(&json!({ "actual": candidate_value })).unwrap();
        let original = graph(vec![node(
            "json",
            NodeKind::ToolResponse,
            &original_content,
            false,
        )]);
        let candidate = graph(vec![node(
            "json",
            NodeKind::ToolResponse,
            &candidate_content,
            false,
        )]);
        let removal = json_record(
            "json",
            "/missing",
            &original.nodes[0],
            &candidate.nodes[0],
            100,
        );

        assert!(
            !check(
                &run_all_with_removals(&original, &candidate, &budget(), 1, &[removal]),
                "removals_enumerated"
            )
            .passed
        );
    }

    #[test]
    fn test_json_marker_units_must_agree() {
        let original_value = "x".repeat(600);
        let candidate_value = format!("{}…[+100 chars]", "x".repeat(500));
        let original_content = serde_json::to_string(&json!({ "actual": original_value })).unwrap();
        let candidate_content =
            serde_json::to_string(&json!({ "actual": candidate_value })).unwrap();
        let original = graph(vec![node(
            "json",
            NodeKind::ToolResponse,
            &original_content,
            false,
        )]);
        let candidate = graph(vec![node(
            "json",
            NodeKind::ToolResponse,
            &candidate_content,
            false,
        )]);
        let removal = json_record(
            "json",
            "/actual",
            &original.nodes[0],
            &candidate.nodes[0],
            99,
        );

        assert!(
            !check(
                &run_all_with_removals(&original, &candidate, &budget(), 1, &[removal]),
                "removals_enumerated"
            )
            .passed
        );
    }

    #[test]
    fn test_overlapping_removal_locators_fail() {
        let counter = counter_for(TokenProvider::Heuristic);
        let original_content = "aaaa\naaaa\naaaa\naaaa\n";
        let intermediate_content = "aaaa\naaaa\naaaa\n";
        let candidate_content = "aaaa\naaaa\n";
        let original = graph(vec![node(
            "log",
            NodeKind::ToolResponse,
            original_content,
            false,
        )]);
        let candidate = graph(vec![node(
            "log",
            NodeKind::ToolResponse,
            candidate_content,
            false,
        )]);
        let removals = vec![
            RemovalRecord {
                strategy_id: "collapse_logs".to_owned(),
                node_id: "log".to_owned(),
                kind: RemovalKind::CollapsedLines,
                locator: Some("lines:1-3".to_owned()),
                original_hash: content_hash(original_content),
                replacement_hash: Some(content_hash(intermediate_content)),
                before_tokens: counter.count(original_content),
                after_tokens: counter.count(intermediate_content),
                units_removed: Some(2),
            },
            RemovalRecord {
                strategy_id: "collapse_logs".to_owned(),
                node_id: "log".to_owned(),
                kind: RemovalKind::CollapsedLines,
                locator: Some("lines:2-4".to_owned()),
                original_hash: content_hash(intermediate_content),
                replacement_hash: Some(content_hash(candidate_content)),
                before_tokens: counter.count(intermediate_content),
                after_tokens: counter.count(candidate_content),
                units_removed: Some(2),
            },
        ];

        assert!(
            !check(
                &run_all_with_removals(&original, &candidate, &budget(), 1, &removals),
                "removals_enumerated"
            )
            .passed
        );
    }

    #[test]
    fn test_all_eleven_return_under_multiple_failures() {
        let original = graph(vec![node(
            "instruction",
            NodeKind::SystemInstruction,
            "CANARY-CONTENT-DO-NOT-LEAK",
            true,
        )]);
        let candidate = graph(Vec::new());
        let checks = run_all(&original, &candidate, &budget(), 999);

        assert_eq!(checks.len(), 11);
        assert_eq!(
            checks
                .iter()
                .map(|check| check.name.as_str())
                .collect::<Vec<_>>(),
            CHECK_NAMES
        );
        assert!(checks.iter().filter(|check| !check.passed).count() >= 4);
        let serialized = serde_json::to_string(&checks).unwrap();
        assert!(!serialized.contains("CANARY-CONTENT-DO-NOT-LEAK"));
    }

    #[test]
    fn test_real_strategy_ledgers_pass_safety() {
        let structural_budget = budget();

        let duplicate_content = "duplicate payload with enough repeated material ".repeat(8);
        let duplicate_graph = graph(vec![
            node("kept", NodeKind::ToolResponse, &duplicate_content, false),
            node(
                "duplicate",
                NodeKind::ToolResponse,
                &duplicate_content,
                false,
            ),
        ]);
        let duplicate_output = DedupExact
            .execute(&duplicate_graph, &structural_budget)
            .unwrap();

        let repeated_line = "a sufficiently verbose repeated log line for token savings\n";
        let log_graph = graph(vec![node(
            "log",
            NodeKind::ToolResponse,
            &repeated_line.repeat(5),
            false,
        )]);
        let log_output = CollapseLogs
            .execute(&log_graph, &structural_budget)
            .unwrap();

        let json_graph = graph(vec![node(
            "json",
            NodeKind::ToolResponse,
            "{\n  \"long_name\": 1,\n  \"items\": [1, 2, 3]\n}",
            false,
        )]);
        let json_output = CompactJson
            .execute(&json_graph, &structural_budget)
            .unwrap();

        let pack_graph = graph(vec![
            node("anchor", NodeKind::Summary, "anchor", true),
            node(
                "drop",
                NodeKind::Summary,
                "a larger disposable payload that exceeds the target",
                false,
            ),
        ]);
        let mut pack_budget = budget();
        pack_budget.max_tokens = pack_graph.nodes[0].token_count;
        let pack_output = PackBudget.execute(&pack_graph, &pack_budget).unwrap();

        for (original, output, check_budget) in [
            (&duplicate_graph, &duplicate_output, &structural_budget),
            (&log_graph, &log_output, &structural_budget),
            (&json_graph, &json_output, &structural_budget),
            (&pack_graph, &pack_output, &pack_budget),
        ] {
            let checks =
                run_all_with_removals(original, &output.graph, check_budget, 1, &output.removals);
            assert!(
                checks.iter().all(|check| check.passed),
                "failed checks: {:?}",
                checks
                    .iter()
                    .filter(|check| !check.passed)
                    .map(|check| check.name.as_str())
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn test_optional_duplicate_edges_must_match_typed_locator_exactly() {
        let duplicate_content = "duplicate payload with enough repeated material ".repeat(8);
        let original = graph(vec![
            node("kept", NodeKind::ToolResponse, &duplicate_content, false),
            node(
                "duplicate",
                NodeKind::ToolResponse,
                &duplicate_content,
                false,
            ),
            node("wrong", NodeKind::ToolResponse, "different", false),
        ]);
        let output = DedupExact.execute(&original, &budget()).unwrap();
        assert!(
            check(
                &run_all_with_removals(&original, &output.graph, &budget(), 1, &output.removals),
                "refs_valid"
            )
            .passed
        );

        let mut wrong = output.graph.clone();
        wrong.edges.push(ContextEdge {
            from: "duplicate".to_owned(),
            to: "wrong".to_owned(),
            kind: EdgeKind::DuplicateOf,
            weight: 1.0,
        });
        assert!(
            !check(
                &run_all_with_removals(&original, &wrong, &budget(), 1, &output.removals),
                "refs_valid"
            )
            .passed
        );

        let mut extra = output.graph.clone();
        extra.edges = vec![
            ContextEdge {
                from: "duplicate".to_owned(),
                to: "kept".to_owned(),
                kind: EdgeKind::DuplicateOf,
                weight: 1.0,
            },
            ContextEdge {
                from: "duplicate".to_owned(),
                to: "kept".to_owned(),
                kind: EdgeKind::DuplicateOf,
                weight: 1.0,
            },
        ];
        assert!(
            !check(
                &run_all_with_removals(&original, &extra, &budget(), 1, &output.removals),
                "refs_valid"
            )
            .passed
        );
    }

    #[test]
    fn test_json_minification_and_nested_folded_elision_pass_semantic_validation() {
        let minified_original = graph(vec![node(
            "json",
            NodeKind::ToolResponse,
            "{\n  \"a\": 1,\n  \"b\": [1, 2, 3]\n}",
            false,
        )]);
        let minified = CompactJson.execute(&minified_original, &budget()).unwrap();
        assert!(
            check(
                &run_all_with_removals(
                    &minified_original,
                    &minified.graph,
                    &budget(),
                    1,
                    &minified.removals
                ),
                "removals_enumerated"
            )
            .passed
        );

        let mut items = (0..60).map(Value::from).collect::<Vec<_>>();
        items[0] = Value::String("x".repeat(800));
        let folded_content = serde_json::to_string(&json!({ "items": items })).unwrap();
        let folded_original = graph(vec![node(
            "json",
            NodeKind::ToolResponse,
            &folded_content,
            false,
        )]);
        let folded = CompactJson.execute(&folded_original, &budget()).unwrap();
        assert!(
            check(
                &run_all_with_removals(
                    &folded_original,
                    &folded.graph,
                    &budget(),
                    1,
                    &folded.removals
                ),
                "removals_enumerated"
            )
            .passed
        );
    }

    fn json_record(
        node_id: &str,
        pointer: &str,
        original: &ContextNode,
        candidate: &ContextNode,
        units_removed: usize,
    ) -> RemovalRecord {
        RemovalRecord {
            strategy_id: "compact_json".to_owned(),
            node_id: node_id.to_owned(),
            kind: RemovalKind::JsonElision,
            locator: Some(pointer.to_owned()),
            original_hash: content_hash(&original.content),
            replacement_hash: Some(content_hash(&candidate.content)),
            before_tokens: original.token_count,
            after_tokens: candidate.token_count,
            units_removed: Some(units_removed),
        }
    }

    fn content_hash(content: &str) -> String {
        blake3::hash(content.as_bytes()).to_hex().to_string()
    }
}
