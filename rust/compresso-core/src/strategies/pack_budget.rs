use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::graph::{ContextGraph, EdgeKind, GraphError, NodeKind};
use crate::safety::duplicate_references_well_formed;
use crate::strategy::{
    CompressionBudget, CompressionStrategy, Estimate, RemovalKind, RemovalRecord, StrategyError,
    StrategyOutput,
};

pub struct PackBudget;

impl CompressionStrategy for PackBudget {
    fn id(&self) -> &'static str {
        "pack_budget"
    }

    fn supports(&self, input: &ContextGraph) -> bool {
        packable_structure(input) && input.total_tokens() > 0
    }

    fn estimate(&self, input: &ContextGraph) -> Estimate {
        if !packable_structure(input) {
            return Estimate::default();
        }
        let tokens_saved = maximum_droppable_tokens(input);
        if tokens_saved == 0 {
            Estimate::default()
        } else {
            Estimate {
                tokens_saved,
                confidence: 0.5,
            }
        }
    }

    fn execute(
        &self,
        input: &ContextGraph,
        budget: &CompressionBudget,
    ) -> Result<StrategyOutput, StrategyError> {
        if !packable_structure(input) {
            return Ok(StrategyOutput {
                graph: input.clone(),
                changed_node_ids: BTreeSet::new(),
                notes: vec!["skipped_reason: invalid graph structure".to_owned()],
                removals: Vec::new(),
            });
        }
        if input.total_tokens() <= budget.max_tokens {
            return Ok(StrategyOutput {
                graph: input.clone(),
                changed_node_ids: BTreeSet::new(),
                notes: vec!["skipped_reason: graph already satisfies budget".to_owned()],
                removals: Vec::new(),
            });
        }

        let mut groups = atomic_groups(input, &budget.protected_kinds);
        let protected_tokens = groups
            .iter()
            .filter(|group| group.protected)
            .fold(0_usize, |total, group| total.saturating_add(group.tokens));
        let unsatisfiable = protected_tokens > budget.max_tokens;

        groups.retain(|group| !group.protected);
        groups.sort_by(compare_value_density);

        let mut remaining_tokens = input.total_tokens();
        let mut dropped_indices = Vec::new();
        for group in groups {
            if !unsatisfiable && remaining_tokens <= budget.max_tokens {
                break;
            }
            remaining_tokens = remaining_tokens.saturating_sub(group.tokens);
            dropped_indices.extend(group.members);
        }

        let dropped_ids = dropped_indices
            .iter()
            .map(|index| input.nodes[*index].id.as_str())
            .collect::<HashSet<_>>();
        let changed_node_ids = dropped_indices
            .iter()
            .map(|index| input.nodes[*index].id.clone())
            .collect::<BTreeSet<_>>();

        let mut notes = Vec::new();
        if unsatisfiable {
            notes.push(format!(
                "unsatisfiable budget: protected set uses {protected_tokens} tokens, exceeding max {}",
                budget.max_tokens
            ));
        }
        let removals = dropped_indices
            .iter()
            .map(|index| {
                let node = &input.nodes[*index];
                notes.push(format!("removed node:{}", node.id));
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
            })
            .collect();

        let graph = ContextGraph {
            schema_version: input.schema_version,
            nodes: input
                .nodes
                .iter()
                .filter(|node| !dropped_ids.contains(node.id.as_str()))
                .cloned()
                .collect(),
            edges: input
                .edges
                .iter()
                .filter(|edge| {
                    !dropped_ids.contains(edge.from.as_str())
                        && !dropped_ids.contains(edge.to.as_str())
                })
                .cloned()
                .collect(),
        };

        Ok(StrategyOutput {
            graph,
            changed_node_ids,
            notes,
            removals,
        })
    }
}

struct AtomicGroup {
    members: Vec<usize>,
    tokens: usize,
    priority: f32,
    id: String,
    protected: bool,
}

// Drop the least protected value per token first. Cross multiplication avoids
// division and gives exact tie handling for the finite f32 priorities promoted
// to f64. Zero-token groups cannot help satisfy a budget, so they sort last.
fn compare_value_density(left: &AtomicGroup, right: &AtomicGroup) -> Ordering {
    match (left.tokens == 0, right.tokens == 0) {
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (true, true) => left.id.cmp(&right.id),
        (false, false) => {
            let left_product = f64::from(left.priority) * right.tokens as f64;
            let right_product = f64::from(right.priority) * left.tokens as f64;
            left_product
                .total_cmp(&right_product)
                .then_with(|| left.id.cmp(&right.id))
        }
    }
}

fn packable_structure(input: &ContextGraph) -> bool {
    duplicate_references_well_formed(input)
        && matches!(
            input.validate(),
            Ok(()) | Err(GraphError::ProtectionViolation { .. })
        )
}

// The estimate contract has no budget parameter, so this is the deterministic
// upper bound: every safely droppable group, excluding protected closures.
fn maximum_droppable_tokens(input: &ContextGraph) -> usize {
    atomic_groups(input, &[])
        .into_iter()
        .filter(|group| !group.protected)
        .fold(0_usize, |total, group| total.saturating_add(group.tokens))
}

fn atomic_groups(input: &ContextGraph, protected_kinds: &[NodeKind]) -> Vec<AtomicGroup> {
    let mut components = Components::new(input.nodes.len());
    let node_indices = input
        .nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut provider_turns = HashMap::<(&str, &str), usize>::new();

    for (index, node) in input.nodes.iter().enumerate() {
        if let Some(turn) = &node.provider_turn {
            let key = (turn.provider.as_str(), turn.turn_id.as_str());
            if let Some(existing) = provider_turns.insert(key, index) {
                components.union(existing, index);
            }
        }
    }

    for edge in &input.edges {
        if matches!(
            edge.kind,
            EdgeKind::DuplicateOf | EdgeKind::ProducedByTool | EdgeKind::RespondsTo
        ) && let (Some(&from), Some(&to)) = (
            node_indices.get(edge.from.as_str()),
            node_indices.get(edge.to.as_str()),
        ) {
            components.union(from, to);
        }
    }

    let mut grouped = BTreeMap::<usize, Vec<usize>>::new();
    for index in 0..input.nodes.len() {
        grouped
            .entry(components.find(index))
            .or_default()
            .push(index);
    }

    grouped
        .into_values()
        .map(|mut members| {
            members.sort_by(|left, right| input.nodes[*left].id.cmp(&input.nodes[*right].id));
            let tokens = members.iter().fold(0_usize, |total, index| {
                total.saturating_add(input.nodes[*index].token_count)
            });
            // An atomic group inherits its highest-priority member so a low-value
            // stub cannot make a valuable referent or provider turn cheap to drop.
            let priority = members
                .iter()
                .map(|index| input.nodes[*index].priority)
                .max_by(f32::total_cmp)
                .unwrap_or(0.0);
            let protected = members.iter().any(|index| {
                let node = &input.nodes[*index];
                node.protected
                    || node.kind.requires_protection()
                    || protected_kinds.contains(&node.kind)
            });
            let id = input.nodes[members[0]].id.clone();

            AtomicGroup {
                members,
                tokens,
                priority,
                id,
                protected,
            }
        })
        .collect()
}

struct Components {
    parents: Vec<usize>,
}

impl Components {
    fn new(length: usize) -> Self {
        Self {
            parents: (0..length).collect(),
        }
    }

    fn find(&mut self, index: usize) -> usize {
        let parent = self.parents[index];
        if parent == index {
            index
        } else {
            let root = self.find(parent);
            self.parents[index] = root;
            root
        }
    }

    fn union(&mut self, left: usize, right: usize) {
        let left_root = self.find(left);
        let right_root = self.find(right);
        if left_root != right_root {
            let (root, child) = if left_root < right_root {
                (left_root, right_root)
            } else {
                (right_root, left_root)
            };
            self.parents[child] = root;
        }
    }
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::graph::{
        ContextEdge, ContextNode, EdgeKind, NodeKind, ProviderId, ProviderTurn, ProviderTurnId,
        Source, ToolCallId,
    };
    use crate::strategy::{Lossiness, Mode, RemovalKind, StrategyRegistry};
    use crate::tokens::TokenProvider;

    fn node(
        id: &str,
        kind: NodeKind,
        token_count: usize,
        priority: f32,
        protected: bool,
    ) -> ContextNode {
        ContextNode {
            id: id.to_owned(),
            kind,
            content: format!("content:{id}"),
            source: Source::Synthetic,
            token_count,
            token_provider: TokenProvider::OpenAiO200k,
            priority,
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

    fn budget(max_tokens: usize) -> CompressionBudget {
        CompressionBudget {
            max_tokens,
            max_latency_ms: 200,
            allowed_lossiness: Lossiness::Structural,
            protected_kinds: Vec::new(),
        }
    }

    fn execute(input: &ContextGraph, max_tokens: usize) -> StrategyOutput {
        PackBudget
            .execute(input, &budget(max_tokens))
            .expect("budget packing should execute")
    }

    fn ids(output: &StrategyOutput) -> Vec<&str> {
        output
            .graph
            .nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect()
    }

    #[test]
    fn test_drops_lowest_value_first() {
        let input = graph(vec![
            node("A", NodeKind::Summary, 100, 0.9, false),
            node("B", NodeKind::Summary, 100, 0.2, false),
            node("C", NodeKind::Summary, 100, 0.5, false),
        ]);
        let output = execute(&input, 250);

        assert_eq!(ids(&output), ["A", "C"]);
        assert_eq!(output.graph.total_tokens(), 200);
        assert!(output.notes.iter().any(|note| note == "removed node:B"));
        assert_eq!(output.changed_node_ids, BTreeSet::from(["B".to_owned()]));
    }

    #[test]
    fn test_protected_never_dropped() {
        let input = graph(vec![
            node("protected", NodeKind::Summary, 1_000, 0.0, true),
            node("drop-a", NodeKind::Summary, 100, 0.1, false),
            node("drop-b", NodeKind::Summary, 200, 0.2, false),
        ]);
        let output = execute(&input, 100);

        assert_eq!(ids(&output), ["protected"]);
        assert!(
            output
                .notes
                .iter()
                .any(|note| note.contains("unsatisfiable budget"))
        );
        assert_eq!(output.removals.len(), 2);
    }

    #[test]
    fn test_code_never_truncated() {
        let input = graph(vec![node("source", NodeKind::SourceFile, 400, 0.5, false)]);
        let original = input.nodes[0].content.clone();
        let output = execute(&input, 300);

        assert!(output.graph.nodes.is_empty());
        assert!(
            output
                .graph
                .nodes
                .iter()
                .all(|node| !original.starts_with(&node.content) || node.content == original)
        );
    }

    #[test]
    fn test_edges_pruned() {
        let mut input = graph(vec![
            node("A", NodeKind::Summary, 100, 0.9, false),
            node("B", NodeKind::Summary, 100, 0.1, false),
        ]);
        input.edges.push(ContextEdge {
            from: "A".to_owned(),
            to: "B".to_owned(),
            kind: EdgeKind::References,
            weight: 1.0,
        });
        let output = execute(&input, 100);

        assert_eq!(ids(&output), ["A"]);
        assert!(output.graph.edges.is_empty());
        assert_eq!(output.graph.validate(), Ok(()));
    }

    #[test]
    fn test_equal_ratio_tiebreaks_by_id() {
        let input = graph(vec![
            node("n_b", NodeKind::Summary, 200, 0.4, false),
            node("n_a", NodeKind::Summary, 100, 0.2, false),
        ]);
        let first = execute(&input, 200);
        let second = execute(&input, 200);

        assert_eq!(first, second);
        assert_eq!(ids(&first), ["n_b"]);
        assert_eq!(first.removals[0].node_id, "n_a");
    }

    #[test]
    fn test_lower_absolute_priority_with_higher_density_is_kept() {
        let input = graph(vec![
            node("lower-priority", NodeKind::Summary, 10, 0.2, false),
            node("lower-density", NodeKind::Summary, 100, 0.3, false),
        ]);
        let output = execute(&input, 20);

        assert_eq!(ids(&output), ["lower-priority"]);
        assert_eq!(output.removals.len(), 1);
        assert_eq!(output.removals[0].node_id, "lower-density");
    }

    #[test]
    fn test_equal_priority_drops_larger_token_group_first() {
        let input = graph(vec![
            node("large", NodeKind::Summary, 200, 0.5, false),
            node("small", NodeKind::Summary, 100, 0.5, false),
        ]);
        let output = execute(&input, 200);

        assert_eq!(ids(&output), ["small"]);
        assert_eq!(output.removals[0].node_id, "large");
    }

    #[test]
    fn test_zero_budget_drops_all_unprotected_nodes_and_incident_edges() {
        let mut input = graph(vec![
            node("A", NodeKind::Summary, 100, 0.1, false),
            node("B", NodeKind::Summary, 200, 0.2, false),
        ]);
        input.edges = vec![
            ContextEdge {
                from: "A".to_owned(),
                to: "B".to_owned(),
                kind: EdgeKind::DependsOn,
                weight: 1.0,
            },
            ContextEdge {
                from: "B".to_owned(),
                to: "B".to_owned(),
                kind: EdgeKind::References,
                weight: 1.0,
            },
        ];
        let output = execute(&input, 0);

        assert!(output.graph.nodes.is_empty());
        assert!(output.graph.edges.is_empty());
        assert_eq!(output.graph.total_tokens(), 0);
        assert_eq!(output.graph.validate(), Ok(()));
    }

    #[test]
    fn test_removal_records_reconcile_and_estimate_matches_zero_budget_execute() {
        let input = graph(vec![
            node("protected", NodeKind::Summary, 50, 0.0, true),
            node("A", NodeKind::Summary, 100, 0.1, false),
            node("B", NodeKind::Summary, 200, 0.2, false),
        ]);
        let output = execute(&input, 0);

        assert_eq!(PackBudget.estimate(&input).tokens_saved, 300);
        assert_eq!(input.total_tokens() - output.graph.total_tokens(), 300);
        assert_eq!(
            output
                .removals
                .iter()
                .map(|record| record.before_tokens - record.after_tokens)
                .sum::<usize>(),
            300
        );
        for record in &output.removals {
            let original = input
                .nodes
                .iter()
                .find(|node| node.id == record.node_id)
                .unwrap();
            assert_eq!(record.strategy_id, "pack_budget");
            assert_eq!(record.kind, RemovalKind::DroppedNode);
            assert_eq!(record.locator, None);
            assert_eq!(
                record.original_hash,
                blake3::hash(original.content.as_bytes())
                    .to_hex()
                    .to_string()
            );
            assert_eq!(record.replacement_hash, None);
            assert_eq!(record.before_tokens, original.token_count);
            assert_eq!(record.after_tokens, 0);
            assert_eq!(record.units_removed, Some(1));
        }
    }

    #[test]
    fn test_inherently_protected_kind_is_never_dropped_even_if_flag_is_false() {
        let input = graph(vec![
            node("instruction", NodeKind::SystemInstruction, 500, 0.0, false),
            node("drop", NodeKind::Summary, 100, 1.0, false),
        ]);
        let output = execute(&input, 0);

        assert_eq!(ids(&output), ["instruction"]);
        assert!(
            output
                .notes
                .iter()
                .any(|note| note.contains("unsatisfiable budget"))
        );
    }

    #[test]
    fn test_budget_protected_kinds_are_never_dropped() {
        let input = graph(vec![
            node("source", NodeKind::SourceFile, 400, 0.0, false),
            node("drop", NodeKind::Summary, 100, 1.0, false),
        ]);
        let mut protected_budget = budget(0);
        protected_budget.protected_kinds.push(NodeKind::SourceFile);
        let output = PackBudget.execute(&input, &protected_budget).unwrap();

        assert_eq!(ids(&output), ["source"]);
    }

    #[test]
    fn test_dedup_referent_and_stubs_are_an_atomic_group() {
        let mut stub = node("stub", NodeKind::ToolResponse, 20, 0.1, false);
        stub.content = "[duplicate of node referent]".to_owned();
        let mut input = graph(vec![
            node("referent", NodeKind::ToolResponse, 100, 0.4, false),
            stub,
            node("protected", NodeKind::Summary, 100, 1.0, true),
        ]);
        input.edges.push(ContextEdge {
            from: "stub".to_owned(),
            to: "referent".to_owned(),
            kind: EdgeKind::DuplicateOf,
            weight: 1.0,
        });
        let output = execute(&input, 100);

        assert_eq!(ids(&output), ["protected"]);
        assert_eq!(
            output
                .removals
                .iter()
                .map(|record| record.node_id.as_str())
                .collect::<Vec<_>>(),
            ["referent", "stub"]
        );
    }

    #[test]
    fn test_dedup_referent_and_stub_are_retained_together() {
        let mut stub = node("stub", NodeKind::ToolResponse, 20, 0.1, false);
        stub.content = "[duplicate of node referent]".to_owned();
        let mut input = graph(vec![
            node("referent", NodeKind::ToolResponse, 100, 0.9, false),
            stub,
            node("drop", NodeKind::Summary, 100, 0.1, false),
        ]);
        input.edges.push(ContextEdge {
            from: "stub".to_owned(),
            to: "referent".to_owned(),
            kind: EdgeKind::DuplicateOf,
            weight: 1.0,
        });

        let output = execute(&input, 120);

        assert_eq!(ids(&output), ["referent", "stub"]);
        assert_eq!(output.graph.edges, input.edges);
        assert_eq!(output.removals.len(), 1);
        assert_eq!(output.removals[0].node_id, "drop");
    }

    #[test]
    fn test_malformed_duplicate_cycle_is_not_packable() {
        let mut first = node("first", NodeKind::ToolResponse, 20, 0.1, false);
        first.content = "[duplicate of node second]".to_owned();
        let mut second = node("second", NodeKind::ToolResponse, 20, 0.1, false);
        second.content = "[duplicate of node first]".to_owned();
        let mut input = graph(vec![first, second]);
        input.edges = vec![
            ContextEdge {
                from: "first".to_owned(),
                to: "second".to_owned(),
                kind: EdgeKind::DuplicateOf,
                weight: 1.0,
            },
            ContextEdge {
                from: "second".to_owned(),
                to: "first".to_owned(),
                kind: EdgeKind::DuplicateOf,
                weight: 1.0,
            },
        ];

        assert!(!PackBudget.supports(&input));
        assert_eq!(execute(&input, 0).graph, input);
    }

    #[test]
    fn test_parallel_provider_turn_is_retained_as_one_atomic_group() {
        let first = ProviderTurn {
            provider: ProviderId::new("fixture").unwrap(),
            turn_id: ProviderTurnId::new("turn-1").unwrap(),
            tool_call_id: ToolCallId::new("call-1").unwrap(),
            opaque_state_hash: None,
        };
        let second = ProviderTurn {
            tool_call_id: ToolCallId::new("call-2").unwrap(),
            ..first.clone()
        };
        let mut owner = node("owner", NodeKind::AssistantResponse, 25, 0.0, false);
        owner.provider_turn = Some(first.clone());
        let mut reasoning = node("reasoning", NodeKind::AssistantReasoning, 25, 0.0, true);
        reasoning.provider_turn = Some(first.clone());
        let mut request_one = node("request-1", NodeKind::ToolRequest, 50, 0.0, false);
        request_one.provider_turn = Some(first.clone());
        let mut response_one = node("response-1", NodeKind::ToolResponse, 100, 0.0, false);
        response_one.provider_turn = Some(first);
        let mut request_two = node("request-2", NodeKind::ToolRequest, 50, 0.0, false);
        request_two.provider_turn = Some(second.clone());
        let mut response_two = node("response-2", NodeKind::ToolResponse, 100, 0.0, false);
        response_two.provider_turn = Some(second);
        let mut input = graph(vec![
            owner,
            reasoning,
            request_one,
            response_one,
            request_two,
            response_two,
            node("drop", NodeKind::Summary, 20, 1.0, false),
        ]);
        input.edges = vec![
            ContextEdge {
                from: "response-1".to_owned(),
                to: "request-1".to_owned(),
                kind: EdgeKind::RespondsTo,
                weight: 1.0,
            },
            ContextEdge {
                from: "response-2".to_owned(),
                to: "request-2".to_owned(),
                kind: EdgeKind::RespondsTo,
                weight: 1.0,
            },
        ];
        let output = execute(&input, 0);

        assert_eq!(
            ids(&output),
            [
                "owner",
                "reasoning",
                "request-1",
                "response-1",
                "request-2",
                "response-2"
            ]
        );
        assert_eq!(output.removals.len(), 1);
        assert_eq!(output.removals[0].node_id, "drop");
    }

    #[test]
    fn test_within_budget_is_byte_identical_noop() {
        let input = graph(vec![node("A", NodeKind::Summary, 100, 0.1, false)]);
        let output = execute(&input, 100);

        assert_eq!(output.graph, input);
        assert!(output.removals.is_empty());
    }

    #[test]
    fn test_invalid_graph_fails_open() {
        let input = graph(vec![
            node("duplicate", NodeKind::Summary, 100, 0.1, false),
            node("duplicate", NodeKind::Summary, 100, 0.2, false),
        ]);
        let output = execute(&input, 0);

        assert_eq!(output.graph, input);
        assert!(output.removals.is_empty());
        assert!(
            output
                .notes
                .iter()
                .any(|note| note.contains("skipped_reason"))
        );
        assert!(!PackBudget.supports(&input));
        assert_eq!(PackBudget.estimate(&input), Estimate::default());
    }

    #[test]
    fn test_safe_registry_uses_real_pack_budget_strategy() {
        let input = graph(vec![node("A", NodeKind::Summary, 100, 0.1, false)]);
        let registry = StrategyRegistry::safe_default(Mode::Safe);
        let strategy = &registry.strategies()[3];

        assert_eq!(strategy.id(), "pack_budget");
        assert!(strategy.supports(&input));
        assert!(strategy.execute(&input, &budget(0)).is_ok());
    }
}
