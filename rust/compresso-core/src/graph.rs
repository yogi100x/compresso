use std::collections::{BTreeSet, HashSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::tokens::{TokenCounter, TokenProvider, counter_for};

pub const CONTEXT_GRAPH_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextGraph {
    pub schema_version: u32,
    pub nodes: Vec<ContextNode>,
    pub edges: Vec<ContextEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextNode {
    pub id: String,
    pub kind: NodeKind,
    pub content: String,
    pub source: Source,
    pub token_count: usize,
    #[serde(default)]
    pub token_provider: TokenProvider,
    pub priority: f32,
    pub protected: bool,
    #[serde(default)]
    pub recency: Option<u64>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub provider_turn: Option<ProviderTurn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderTurn {
    pub provider: ProviderId,
    pub turn_id: ProviderTurnId,
    pub tool_call_id: ToolCallId,
    #[serde(default)]
    pub opaque_state_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{kind} must not be empty")]
pub struct ProviderIdentifierError {
    kind: &'static str,
}

macro_rules! provider_identifier {
    ($name:ident, $kind:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, ProviderIdentifierError> {
                let value = value.into();
                if value.is_empty() {
                    Err(ProviderIdentifierError { kind: $kind })
                } else {
                    Ok(Self(value))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

provider_identifier!(ProviderId, "provider id");
provider_identifier!(ProviderTurnId, "provider turn id");
provider_identifier!(ToolCallId, "tool call id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    SystemInstruction,
    UserInstruction,
    AssistantResponse,
    AssistantReasoning,
    ToolDefinition,
    ToolRequest,
    ToolResponse,
    SourceFile,
    CodeSymbol,
    SearchResult,
    CompilerOutput,
    TestOutput,
    RepoMap,
    Summary,
    Decision,
    MemoryFact,
}

impl NodeKind {
    pub const fn requires_protection(self) -> bool {
        matches!(
            self,
            Self::SystemInstruction
                | Self::UserInstruction
                | Self::AssistantReasoning
                | Self::ToolDefinition
                | Self::ToolRequest
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    Client,
    Repository { path: String },
    Tool { name: String },
    Synthetic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    pub weight: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Imports,
    Calls,
    Defines,
    References,
    Tests,
    Supersedes,
    ProducedByTool,
    RespondsTo,
    DependsOn,
    SameErrorGroup,
    DuplicateOf,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GraphError {
    #[error("unsupported context graph schema version: {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("duplicate context node id: {0}")]
    DuplicateId(String),
    #[error("edge {from}->{to} references unknown endpoint {endpoint}")]
    UnknownEdgeEndpoint {
        from: String,
        to: String,
        endpoint: String,
    },
    #[error("node {node_id} of kind {kind:?} must be protected")]
    ProtectionViolation { node_id: String, kind: NodeKind },
    #[error("node {node_id} has stale token count: stored {stored}, recounted {recounted}")]
    StaleTokenCount {
        node_id: String,
        stored: usize,
        recounted: usize,
    },
}

impl ContextGraph {
    pub fn validate(&self) -> Result<(), GraphError> {
        if self.schema_version != CONTEXT_GRAPH_SCHEMA_VERSION {
            return Err(GraphError::UnsupportedSchemaVersion(self.schema_version));
        }

        let mut ids = HashSet::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if !ids.insert(node.id.as_str()) {
                return Err(GraphError::DuplicateId(node.id.clone()));
            }
        }

        for edge in &self.edges {
            if !ids.contains(edge.from.as_str()) {
                return Err(GraphError::UnknownEdgeEndpoint {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    endpoint: edge.from.clone(),
                });
            }
            if !ids.contains(edge.to.as_str()) {
                return Err(GraphError::UnknownEdgeEndpoint {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    endpoint: edge.to.clone(),
                });
            }
        }

        for node in &self.nodes {
            if node.kind.requires_protection() && !node.protected {
                return Err(GraphError::ProtectionViolation {
                    node_id: node.id.clone(),
                    kind: node.kind,
                });
            }
        }

        Ok(())
    }

    pub fn force_ingress_protection(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        for node in &mut self.nodes {
            if node.kind.requires_protection() && !node.protected {
                node.protected = true;
                notes.push(format!("protection_forced:{}", node.id));
            }
        }
        notes
    }

    pub fn total_tokens(&self) -> usize {
        self.nodes
            .iter()
            .fold(0, |total, node| total.saturating_add(node.token_count))
    }

    /// Recounts only nodes named by a strategy's exact change proof.
    ///
    /// Removed node ids are intentionally ignored because they have no
    /// candidate content to tokenize. The caller reconciles their stored
    /// ingress counts through aggregate delta accounting.
    pub fn recount_changed(&mut self, changed: &BTreeSet<String>, counter: &dyn TokenCounter) {
        for node in &mut self.nodes {
            if changed.contains(&node.id) {
                let node_counter = if node.token_provider == counter.provider() {
                    counter
                } else {
                    counter_for(node.token_provider)
                };
                node.token_count = node_counter.count(&node.content);
            }
        }
    }

    /// Independently verifies every stored node count.
    ///
    /// This full-graph helper is intended for ingress validation and tests.
    /// The engine's post-ingress strategy path uses [`Self::recount_changed`]
    /// instead.
    pub fn validate_token_counts(&self, counter: &dyn TokenCounter) -> Result<(), GraphError> {
        for node in &self.nodes {
            let node_counter = if node.token_provider == counter.provider() {
                counter
            } else {
                counter_for(node.token_provider)
            };
            let recounted = node_counter.count(&node.content);
            if recounted != node.token_count {
                return Err(GraphError::StaleTokenCount {
                    node_id: node.id.clone(),
                    stored: node.token_count,
                    recounted,
                });
            }
        }
        Ok(())
    }

    pub fn from_text(content: &str, kind: NodeKind, counter: &dyn TokenCounter) -> Self {
        Self {
            schema_version: CONTEXT_GRAPH_SCHEMA_VERSION,
            nodes: vec![ContextNode {
                id: "text-1".to_owned(),
                kind,
                content: content.to_owned(),
                source: Source::Client,
                token_count: counter.count(content),
                token_provider: counter.provider(),
                priority: 1.0,
                protected: kind.requires_protection(),
                recency: None,
                metadata: Map::new(),
                provider_turn: None,
            }],
            edges: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct CountingCounter {
        calls: AtomicUsize,
    }

    impl TokenCounter for CountingCounter {
        fn provider(&self) -> TokenProvider {
            TokenProvider::Heuristic
        }

        fn count(&self, text: &str) -> usize {
            self.calls.fetch_add(1, Ordering::SeqCst);
            text.len()
        }
    }

    fn node(id: &str, kind: NodeKind, protected: bool) -> ContextNode {
        ContextNode {
            id: id.to_owned(),
            kind,
            content: String::new(),
            source: Source::Synthetic,
            token_count: 0,
            token_provider: TokenProvider::OpenAiO200k,
            priority: 0.5,
            protected,
            recency: None,
            metadata: Map::new(),
            provider_turn: None,
        }
    }

    #[test]
    fn test_validate_rejects_duplicate_ids() {
        let graph = ContextGraph {
            schema_version: 1,
            nodes: vec![
                node("n1", NodeKind::SourceFile, false),
                node("n1", NodeKind::Summary, false),
            ],
            edges: vec![],
        };

        assert_eq!(
            graph.validate(),
            Err(GraphError::DuplicateId("n1".to_owned()))
        );
    }

    #[test]
    fn test_recount_changed_only_visits_declared_nodes() {
        let mut unchanged = node("unchanged", NodeKind::Summary, false);
        unchanged.content = "unchanged content".to_owned();
        unchanged.token_provider = TokenProvider::Heuristic;
        unchanged.token_count = 17;
        let mut changed = node("changed", NodeKind::Summary, false);
        changed.content = "new".to_owned();
        changed.token_provider = TokenProvider::Heuristic;
        changed.token_count = 999;
        let mut graph = ContextGraph {
            schema_version: 1,
            nodes: vec![unchanged, changed],
            edges: vec![],
        };
        let counter = CountingCounter {
            calls: AtomicUsize::new(0),
        };

        graph.recount_changed(&BTreeSet::from(["changed".to_owned()]), &counter);

        assert_eq!(counter.calls.load(Ordering::SeqCst), 1);
        assert_eq!(graph.nodes[0].token_count, 17);
        assert_eq!(graph.nodes[1].token_count, 3);
    }

    #[test]
    fn test_validate_token_counts_reports_exact_stale_node() {
        let mut graph = ContextGraph {
            schema_version: 1,
            nodes: vec![node("stale", NodeKind::Summary, false)],
            edges: vec![],
        };
        graph.nodes[0].content = "four".to_owned();
        graph.nodes[0].token_provider = TokenProvider::Heuristic;
        graph.nodes[0].token_count = 99;
        let counter = CountingCounter {
            calls: AtomicUsize::new(0),
        };

        assert_eq!(
            graph.validate_token_counts(&counter),
            Err(GraphError::StaleTokenCount {
                node_id: "stale".to_owned(),
                stored: 99,
                recounted: 4,
            })
        );
        assert_eq!(counter.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_validate_rejects_zero_and_unknown_schema_versions_first() {
        for schema_version in [0, 2] {
            let graph = ContextGraph {
                schema_version,
                nodes: vec![
                    node("duplicate", NodeKind::SourceFile, false),
                    node("duplicate", NodeKind::Summary, false),
                ],
                edges: vec![],
            };

            assert_eq!(
                graph.validate(),
                Err(GraphError::UnsupportedSchemaVersion(schema_version))
            );
        }
    }

    #[test]
    fn test_validate_rejects_dangling_edge() {
        let graph = ContextGraph {
            schema_version: 1,
            nodes: vec![node("n1", NodeKind::SourceFile, false)],
            edges: vec![ContextEdge {
                from: "n1".to_owned(),
                to: "missing".to_owned(),
                kind: EdgeKind::References,
                weight: 1.0,
            }],
        };

        assert_eq!(
            graph.validate(),
            Err(GraphError::UnknownEdgeEndpoint {
                from: "n1".to_owned(),
                to: "missing".to_owned(),
                endpoint: "missing".to_owned(),
            })
        );
    }

    #[test]
    fn test_protected_kinds_must_be_protected() {
        for kind in [
            NodeKind::SystemInstruction,
            NodeKind::UserInstruction,
            NodeKind::AssistantReasoning,
            NodeKind::ToolDefinition,
            NodeKind::ToolRequest,
        ] {
            let graph = ContextGraph {
                schema_version: 1,
                nodes: vec![node("protected-1", kind, false)],
                edges: vec![],
            };

            assert_eq!(
                graph.validate(),
                Err(GraphError::ProtectionViolation {
                    node_id: "protected-1".to_owned(),
                    kind,
                })
            );
        }
    }

    #[test]
    fn test_system_instruction_must_be_protected() {
        let graph = ContextGraph {
            schema_version: 1,
            nodes: vec![node("sys-1", NodeKind::SystemInstruction, false)],
            edges: vec![],
        };

        assert_eq!(
            graph.validate(),
            Err(GraphError::ProtectionViolation {
                node_id: "sys-1".to_owned(),
                kind: NodeKind::SystemInstruction,
            })
        );
    }

    #[test]
    fn test_force_ingress_protection_for_all_protected_kinds() {
        let protected_kinds = [
            NodeKind::SystemInstruction,
            NodeKind::UserInstruction,
            NodeKind::AssistantReasoning,
            NodeKind::ToolDefinition,
            NodeKind::ToolRequest,
        ];
        let mut graph = ContextGraph {
            schema_version: 1,
            nodes: protected_kinds
                .into_iter()
                .enumerate()
                .map(|(index, kind)| node(&format!("n{index}"), kind, false))
                .collect(),
            edges: vec![],
        };

        let notes = graph.force_ingress_protection();

        assert!(graph.nodes.iter().all(|node| node.protected));
        assert_eq!(
            notes,
            [
                "protection_forced:n0",
                "protection_forced:n1",
                "protection_forced:n2",
                "protection_forced:n3",
                "protection_forced:n4",
            ]
        );
    }

    #[test]
    fn test_validate_ignores_metadata_protection_overrides_without_panicking() {
        let mut metadata = Map::new();
        metadata.insert("allow_unprotect".to_owned(), Value::Bool(true));
        metadata.insert("protected".to_owned(), Value::Bool(true));
        let graph = ContextGraph {
            schema_version: 1,
            nodes: vec![ContextNode {
                metadata,
                ..node("sys-1", NodeKind::SystemInstruction, false)
            }],
            edges: vec![],
        };

        let result = std::panic::catch_unwind(|| graph.validate());

        assert!(result.is_ok());
        assert!(matches!(
            result.expect("validation should not panic"),
            Err(GraphError::ProtectionViolation { .. })
        ));
    }
}
