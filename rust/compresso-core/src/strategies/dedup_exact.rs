use std::collections::HashMap;

use crate::graph::{ContextEdge, ContextGraph, EdgeKind};
use crate::safety::{duplicate_group_node_ids, duplicate_references_well_formed, duplicate_stub};
use crate::strategy::{
    CompressionBudget, CompressionStrategy, Estimate, RemovalKind, RemovalRecord, StrategyError,
    StrategyOutput,
};
use crate::tokens::counter_for;

pub struct DedupExact;

struct Duplicate<'a> {
    index: usize,
    survivor_id: &'a str,
    stub: String,
    after_tokens: usize,
}

impl CompressionStrategy for DedupExact {
    fn id(&self) -> &'static str {
        "dedup_exact"
    }

    fn supports(&self, input: &ContextGraph) -> bool {
        !duplicate_plan(input).is_empty()
    }

    fn estimate(&self, input: &ContextGraph) -> Estimate {
        let duplicates = duplicate_plan(input);
        if duplicates.is_empty() {
            return Estimate::default();
        }

        let mut after_tokens = input.total_tokens();
        for duplicate in duplicates {
            let node = &input.nodes[duplicate.index];
            after_tokens = after_tokens
                .saturating_sub(node.token_count)
                .saturating_add(duplicate.after_tokens);
        }

        Estimate {
            tokens_saved: input.total_tokens().saturating_sub(after_tokens),
            confidence: 1.0,
        }
    }

    fn execute(
        &self,
        input: &ContextGraph,
        _budget: &CompressionBudget,
    ) -> Result<StrategyOutput, StrategyError> {
        let duplicates = duplicate_plan(input);
        if duplicates.is_empty() {
            return Ok(StrategyOutput {
                graph: input.clone(),
                notes: vec!["no profitable exact duplicates found".to_owned()],
                removals: Vec::new(),
            });
        }

        let mut graph = input.clone();
        let mut notes = Vec::with_capacity(duplicates.len());
        let mut removals = Vec::with_capacity(duplicates.len());
        let mut duplicate_edges = Vec::with_capacity(duplicates.len());

        for duplicate in duplicates {
            let node = &mut graph.nodes[duplicate.index];
            let original_content = node.content.clone();
            let before_tokens = node.token_count;
            let stub = duplicate.stub;
            let after_tokens = duplicate.after_tokens;

            node.content = stub.clone();
            node.token_count = after_tokens;

            notes.push(format!(
                "removed node: {} replaced by duplicate reference to {}",
                node.id, duplicate.survivor_id
            ));
            removals.push(RemovalRecord {
                strategy_id: self.id().to_owned(),
                node_id: node.id.clone(),
                kind: RemovalKind::DuplicateReference,
                locator: Some(duplicate.survivor_id.to_owned()),
                original_hash: content_hash(&original_content),
                replacement_hash: Some(content_hash(&stub)),
                before_tokens,
                after_tokens,
                units_removed: Some(1),
            });
            duplicate_edges.push(ContextEdge {
                from: node.id.clone(),
                to: duplicate.survivor_id.to_owned(),
                kind: EdgeKind::DuplicateOf,
                weight: 1.0,
            });
        }
        graph.edges.extend(duplicate_edges);

        Ok(StrategyOutput {
            graph,
            notes,
            removals,
        })
    }
}

fn duplicate_plan(input: &ContextGraph) -> Vec<Duplicate<'_>> {
    if !duplicate_references_well_formed(input) {
        return Vec::new();
    }

    let mut survivors = HashMap::<&str, &str>::new();
    let mut duplicates = Vec::new();
    let existing_duplicate_group = duplicate_group_node_ids(input);

    for (index, node) in input.nodes.iter().enumerate() {
        if node.protected
            || node.kind.requires_protection()
            || existing_duplicate_group.contains(node.id.as_str())
        {
            continue;
        }

        if let Some(&survivor_id) = survivors.get(node.content.as_str()) {
            let stub = duplicate_stub(survivor_id);
            let after_tokens = counter_for(node.token_provider).count(&stub);
            if after_tokens < node.token_count {
                duplicates.push(Duplicate {
                    index,
                    survivor_id,
                    stub,
                    after_tokens,
                });
            }
        } else {
            survivors.insert(node.content.as_str(), node.id.as_str());
        }
    }

    duplicates
}

fn content_hash(content: &str) -> String {
    blake3::hash(content.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::graph::{ContextEdge, ContextNode, EdgeKind, NodeKind, Source};
    use crate::strategy::{CompressionBudget, Lossiness, Mode, RemovalKind, StrategyRegistry};
    use crate::tokens::{TokenProvider, counter_for};

    const DUPLICATE_CONTENT: &str = "SELECT * FROM users LIMIT 50;\n...200 identical rows...";

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

    fn three_duplicate_graph() -> ContextGraph {
        ContextGraph {
            schema_version: 1,
            nodes: ["n1", "n2", "n3"]
                .map(|id| node(id, NodeKind::ToolResponse, DUPLICATE_CONTENT, false))
                .into(),
            edges: vec![
                ContextEdge {
                    from: "n1".to_owned(),
                    to: "n2".to_owned(),
                    kind: EdgeKind::References,
                    weight: 1.0,
                },
                ContextEdge {
                    from: "n2".to_owned(),
                    to: "n3".to_owned(),
                    kind: EdgeKind::DependsOn,
                    weight: 0.5,
                },
            ],
        }
    }

    fn budget() -> CompressionBudget {
        CompressionBudget {
            max_tokens: 1_000,
            max_latency_ms: 200,
            allowed_lossiness: Lossiness::None,
            protected_kinds: Vec::new(),
        }
    }

    #[test]
    fn test_dedup_three_identical_tool_responses() {
        let graph = three_duplicate_graph();
        let original_edges = graph.edges.clone();
        let output = DedupExact
            .execute(&graph, &budget())
            .expect("exact dedup should execute");

        assert_eq!(output.graph.nodes[0], graph.nodes[0]);
        for duplicate_index in [1, 2] {
            let duplicate = &output.graph.nodes[duplicate_index];
            let expected_stub = "[duplicate of node n1]";
            assert_eq!(duplicate.content, expected_stub);
            assert_eq!(
                duplicate.token_count,
                counter_for(duplicate.token_provider).count(expected_stub)
            );
        }
        assert_eq!(&output.graph.edges[..original_edges.len()], original_edges);
        assert_eq!(
            &output.graph.edges[original_edges.len()..],
            [
                ContextEdge {
                    from: "n2".to_owned(),
                    to: "n1".to_owned(),
                    kind: EdgeKind::DuplicateOf,
                    weight: 1.0,
                },
                ContextEdge {
                    from: "n3".to_owned(),
                    to: "n1".to_owned(),
                    kind: EdgeKind::DuplicateOf,
                    weight: 1.0,
                },
            ]
        );
        assert!(output.graph.total_tokens() < graph.total_tokens());
        assert_eq!(output.graph.validate(), Ok(()));

        assert_eq!(output.notes.len(), 2);
        assert!(
            output
                .notes
                .iter()
                .any(|note| note.contains("removed node: n2") && note.contains("n1"))
        );
        assert!(
            output
                .notes
                .iter()
                .any(|note| note.contains("removed node: n3") && note.contains("n1"))
        );

        assert_eq!(output.removals.len(), 2);
        for (record, duplicate_id) in output.removals.iter().zip(["n2", "n3"]) {
            let replacement = "[duplicate of node n1]".to_owned();
            let replacement_hash = blake3::hash(replacement.as_bytes()).to_hex().to_string();
            assert_eq!(record.strategy_id, "dedup_exact");
            assert_eq!(record.node_id, duplicate_id);
            assert_eq!(record.kind, RemovalKind::DuplicateReference);
            assert_eq!(record.locator.as_deref(), Some("n1"));
            assert_eq!(
                record.original_hash,
                blake3::hash(DUPLICATE_CONTENT.as_bytes())
                    .to_hex()
                    .to_string()
            );
            assert_eq!(
                record.replacement_hash.as_deref(),
                Some(replacement_hash.as_str())
            );
            assert_eq!(record.before_tokens, graph.nodes[1].token_count);
            assert_eq!(
                record.after_tokens,
                counter_for(TokenProvider::OpenAiO200k).count(&replacement)
            );
            assert_eq!(record.units_removed, Some(1));
        }
    }

    #[test]
    fn test_protected_duplicate_untouched() {
        let graph = ContextGraph {
            schema_version: 1,
            nodes: vec![
                node(
                    "protected",
                    NodeKind::SystemInstruction,
                    DUPLICATE_CONTENT,
                    true,
                ),
                node(
                    "unprotected",
                    NodeKind::ToolResponse,
                    DUPLICATE_CONTENT,
                    false,
                ),
            ],
            edges: Vec::new(),
        };

        assert!(!DedupExact.supports(&graph));
        let output = DedupExact
            .execute(&graph, &budget())
            .expect("unsupported exact dedup should be a no-op");
        assert_eq!(output.graph, graph);
        assert!(output.removals.is_empty());
    }

    #[test]
    fn test_inherently_protected_kind_ignored_when_flag_is_false() {
        let graph = ContextGraph {
            schema_version: 1,
            nodes: vec![
                node(
                    "lying-system-1",
                    NodeKind::SystemInstruction,
                    DUPLICATE_CONTENT,
                    false,
                ),
                node(
                    "lying-system-2",
                    NodeKind::SystemInstruction,
                    DUPLICATE_CONTENT,
                    false,
                ),
            ],
            edges: Vec::new(),
        };

        assert!(!DedupExact.supports(&graph));
        let output = DedupExact
            .execute(&graph, &budget())
            .expect("inherently protected kinds should be ignored");
        assert_eq!(output.graph, graph);
        assert!(output.removals.is_empty());
    }

    #[test]
    fn test_protected_node_is_not_used_as_survivor() {
        let graph = ContextGraph {
            schema_version: 1,
            nodes: vec![
                node(
                    "protected",
                    NodeKind::SystemInstruction,
                    DUPLICATE_CONTENT,
                    true,
                ),
                node(
                    "first-unprotected",
                    NodeKind::ToolResponse,
                    DUPLICATE_CONTENT,
                    false,
                ),
                node(
                    "later-unprotected",
                    NodeKind::ToolResponse,
                    DUPLICATE_CONTENT,
                    false,
                ),
            ],
            edges: Vec::new(),
        };

        let output = DedupExact
            .execute(&graph, &budget())
            .expect("unprotected duplicate should be replaced");

        assert_eq!(output.graph.nodes[0], graph.nodes[0]);
        assert_eq!(output.graph.nodes[1], graph.nodes[1]);
        assert_eq!(
            output.graph.nodes[2].content,
            "[duplicate of node first-unprotected]"
        );
        assert_eq!(
            output.removals[0].locator.as_deref(),
            Some("first-unprotected")
        );
    }

    #[test]
    fn test_no_duplicates_not_supported() {
        let graph = ContextGraph {
            schema_version: 1,
            nodes: vec![
                node("n1", NodeKind::ToolResponse, "first", false),
                node("n2", NodeKind::ToolResponse, "second", false),
                node("n3", NodeKind::ToolResponse, "third", false),
            ],
            edges: Vec::new(),
        };

        assert!(!DedupExact.supports(&graph));
        let output = DedupExact
            .execute(&graph, &budget())
            .expect("unsupported exact dedup should be a no-op");
        assert_eq!(output.graph, graph);
        assert!(output.removals.is_empty());
        assert_eq!(output.notes, ["no profitable exact duplicates found"]);
    }

    #[test]
    fn test_existing_duplicate_group_is_not_rededuplicated_into_a_chain() {
        let referent_content = "a sufficiently long existing duplicate referent payload ".repeat(8);
        let mut graph = ContextGraph {
            schema_version: 1,
            nodes: vec![
                node("earlier", NodeKind::ToolResponse, &referent_content, false),
                node("referent", NodeKind::ToolResponse, &referent_content, false),
                node(
                    "stub",
                    NodeKind::ToolResponse,
                    "[duplicate of node referent]",
                    false,
                ),
            ],
            edges: Vec::new(),
        };
        graph.edges.push(ContextEdge {
            from: "stub".to_owned(),
            to: "referent".to_owned(),
            kind: EdgeKind::DuplicateOf,
            weight: 1.0,
        });

        assert!(!DedupExact.supports(&graph));
        let output = DedupExact.execute(&graph, &budget()).unwrap();
        assert_eq!(output.graph, graph);
        assert!(output.removals.is_empty());
    }

    #[test]
    fn test_short_duplicate_that_would_inflate_is_not_supported() {
        let graph = ContextGraph {
            schema_version: 1,
            nodes: vec![
                node("n1", NodeKind::ToolResponse, "ok", false),
                node("n2", NodeKind::ToolResponse, "ok", false),
            ],
            edges: Vec::new(),
        };
        let stub_tokens =
            counter_for(graph.nodes[1].token_provider).count("[duplicate of node n1]");
        assert!(stub_tokens >= graph.nodes[1].token_count);

        assert!(!DedupExact.supports(&graph));
        assert_eq!(DedupExact.estimate(&graph), Estimate::default());
        let output = DedupExact
            .execute(&graph, &budget())
            .expect("unprofitable exact dedup should be a no-op");
        assert_eq!(output.graph, graph);
        assert!(output.removals.is_empty());
    }

    #[test]
    fn test_estimate_matches_execute() {
        let graph = three_duplicate_graph();
        let estimate = DedupExact.estimate(&graph);
        let output = DedupExact
            .execute(&graph, &budget())
            .expect("exact dedup should execute");

        assert_eq!(
            estimate.tokens_saved,
            graph.total_tokens() - output.graph.total_tokens()
        );
        assert_eq!(estimate.confidence, 1.0);
    }

    #[test]
    fn test_safe_registry_uses_real_dedup_strategy() {
        let graph = three_duplicate_graph();
        let registry = StrategyRegistry::safe_default(Mode::Safe);
        let dedup = &registry.strategies()[0];

        assert_eq!(dedup.id(), "dedup_exact");
        assert!(dedup.supports(&graph));
        assert!(dedup.execute(&graph, &budget()).is_ok());
    }
}
