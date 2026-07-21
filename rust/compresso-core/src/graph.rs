use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::tokens::{TokenCounter, TokenProvider};

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
    pub provider: String,
    pub turn_id: String,
    pub tool_call_id: String,
    #[serde(default)]
    pub opaque_state_hash: Option<String>,
}

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
    use super::*;

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
