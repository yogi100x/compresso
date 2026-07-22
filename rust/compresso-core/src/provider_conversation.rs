use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::graph::{
    ContextGraph, ContextNode, EdgeKind, NodeKind, ProviderId, ProviderTurnId, ToolCallId,
};

type TurnKey = (ProviderId, ProviderTurnId);
type CallKey = (ProviderId, ProviderTurnId, ToolCallId);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderConversationError {
    #[error("provider node {node_id} has an invalid kind or missing typed turn metadata")]
    InvalidProviderNode { node_id: String },
    #[error("provider turn has no unique assistant owner")]
    MissingOwner,
    #[error("provider tool request/result pairing is incomplete")]
    IncompleteToolPair,
    #[error("provider tool result has no exact responds_to edge")]
    InvalidResponseEdge,
    #[error("provider turn was only partially retained")]
    PartialTurn,
    #[error("protected assistant reasoning changed or was removed")]
    ChangedReasoning,
    #[error("provider state was changed, introduced, or replayed across providers")]
    ProviderStateReplay,
    #[error("retained provider turn depends on a removed provider turn")]
    RemovedDependency,
    #[error("opaque provider state is unknown to the active profile")]
    UnknownOpaqueState,
}

/// Profile hook layered on top of the provider-independent D53 structure.
///
/// Implementations may recognize provider-specific opaque state, but cannot
/// bypass the structural, atomicity, reasoning, or cross-provider checks.
pub trait ProviderConversationValidator: Send + Sync {
    fn validate_pre_compression(
        &self,
        _graph: &ContextGraph,
    ) -> Result<(), ProviderConversationError> {
        Ok(())
    }

    fn validate_post_compression(
        &self,
        _original: &ContextGraph,
        _candidate: &ContextGraph,
    ) -> Result<(), ProviderConversationError> {
        Ok(())
    }
}

/// Slice default: typed provider loops are accepted, opaque continuation state
/// requires an explicit profile validator and otherwise fails open.
#[derive(Debug, Default)]
pub struct DefaultProviderConversationValidator;

impl ProviderConversationValidator for DefaultProviderConversationValidator {
    fn validate_pre_compression(
        &self,
        graph: &ContextGraph,
    ) -> Result<(), ProviderConversationError> {
        reject_opaque_state(graph)
    }

    fn validate_post_compression(
        &self,
        _original: &ContextGraph,
        candidate: &ContextGraph,
    ) -> Result<(), ProviderConversationError> {
        reject_opaque_state(candidate)
    }
}

pub(crate) fn validate_pre_compression(
    graph: &ContextGraph,
    profile: &dyn ProviderConversationValidator,
) -> Result<(), ProviderConversationError> {
    validate_shape(graph)?;
    profile.validate_pre_compression(graph)
}

pub(crate) fn validate_post_compression(
    original: &ContextGraph,
    candidate: &ContextGraph,
    profile: &dyn ProviderConversationValidator,
) -> Result<(), ProviderConversationError> {
    validate_shape(candidate)?;
    validate_transition(original, candidate)?;
    profile.validate_post_compression(original, candidate)
}

fn validate_shape(graph: &ContextGraph) -> Result<(), ProviderConversationError> {
    if !graph.nodes.iter().any(|node| node.provider_turn.is_some()) {
        return Ok(());
    }

    let nodes = graph
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    let mut owners = BTreeMap::<TurnKey, Vec<&str>>::new();
    let mut requests = BTreeMap::<CallKey, Vec<&str>>::new();
    let mut responses = BTreeMap::<CallKey, Vec<&str>>::new();
    let mut turns = BTreeSet::<TurnKey>::new();

    for node in &graph.nodes {
        if matches!(node.kind, NodeKind::ToolRequest | NodeKind::ToolResponse)
            && node.provider_turn.is_none()
        {
            return Err(ProviderConversationError::InvalidProviderNode {
                node_id: node.id.clone(),
            });
        }
        let Some(turn) = &node.provider_turn else {
            continue;
        };
        if !matches!(
            node.kind,
            NodeKind::AssistantResponse
                | NodeKind::AssistantReasoning
                | NodeKind::ToolRequest
                | NodeKind::ToolResponse
        ) || turn
            .opaque_state_hash
            .as_ref()
            .is_some_and(String::is_empty)
        {
            return Err(ProviderConversationError::InvalidProviderNode {
                node_id: node.id.clone(),
            });
        }

        let turn_key = (turn.provider.clone(), turn.turn_id.clone());
        let call_key = (
            turn.provider.clone(),
            turn.turn_id.clone(),
            turn.tool_call_id.clone(),
        );
        turns.insert(turn_key.clone());
        match node.kind {
            NodeKind::AssistantResponse => owners.entry(turn_key).or_default().push(&node.id),
            NodeKind::ToolRequest => requests.entry(call_key).or_default().push(&node.id),
            NodeKind::ToolResponse => responses.entry(call_key).or_default().push(&node.id),
            NodeKind::AssistantReasoning => {}
            _ => unreachable!("provider node kinds were checked above"),
        }
    }

    if !turns
        .iter()
        .all(|turn| owners.get(turn).is_some_and(|nodes| nodes.len() == 1))
    {
        return Err(ProviderConversationError::MissingOwner);
    }

    let calls = requests
        .keys()
        .chain(responses.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for call in calls {
        let Some(request) = requests.get(&call).filter(|nodes| nodes.len() == 1) else {
            return Err(ProviderConversationError::IncompleteToolPair);
        };
        let Some(response) = responses.get(&call).filter(|nodes| nodes.len() == 1) else {
            return Err(ProviderConversationError::IncompleteToolPair);
        };
        let matching_edges = graph
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == EdgeKind::RespondsTo
                    && edge.from == response[0]
                    && edge.to == request[0]
            })
            .count();
        if matching_edges != 1 {
            return Err(ProviderConversationError::InvalidResponseEdge);
        }
    }

    let provider_response_edges_valid = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::RespondsTo)
        .filter(|edge| {
            nodes
                .get(edge.from.as_str())
                .is_some_and(|node| node.provider_turn.is_some())
                || nodes
                    .get(edge.to.as_str())
                    .is_some_and(|node| node.provider_turn.is_some())
        })
        .all(|edge| {
            let Some(response) = nodes.get(edge.from.as_str()) else {
                return false;
            };
            let Some(request) = nodes.get(edge.to.as_str()) else {
                return false;
            };
            response.kind == NodeKind::ToolResponse
                && request.kind == NodeKind::ToolRequest
                && response.provider_turn == request.provider_turn
        });
    if !provider_response_edges_valid {
        return Err(ProviderConversationError::InvalidResponseEdge);
    }

    Ok(())
}

fn validate_transition(
    original: &ContextGraph,
    candidate: &ContextGraph,
) -> Result<(), ProviderConversationError> {
    let original_nodes = original
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();
    let candidate_nodes = candidate
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<BTreeMap<_, _>>();

    for node in candidate
        .nodes
        .iter()
        .filter(|node| node.provider_turn.is_some())
    {
        let Some(previous) = original_nodes.get(node.id.as_str()) else {
            return Err(ProviderConversationError::ProviderStateReplay);
        };
        if previous.provider_turn != node.provider_turn {
            return Err(ProviderConversationError::ProviderStateReplay);
        }
    }

    for reasoning in original
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::AssistantReasoning)
    {
        if !candidate_nodes
            .get(reasoning.id.as_str())
            .is_some_and(|candidate| *candidate == reasoning)
        {
            return Err(ProviderConversationError::ChangedReasoning);
        }
    }

    let mut turns = BTreeMap::<TurnKey, Vec<&ContextNode>>::new();
    for node in original
        .nodes
        .iter()
        .filter(|node| node.provider_turn.is_some())
    {
        let turn = node.provider_turn.as_ref().expect("filtered provider turn");
        turns
            .entry((turn.provider.clone(), turn.turn_id.clone()))
            .or_default()
            .push(node);
    }
    for nodes in turns.values() {
        let retained = nodes
            .iter()
            .filter(|node| candidate_nodes.contains_key(node.id.as_str()))
            .count();
        if retained != 0 && retained != nodes.len() {
            return Err(ProviderConversationError::PartialTurn);
        }
    }

    for edge in original
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::DependsOn)
    {
        if candidate_nodes.contains_key(edge.from.as_str())
            && original_nodes
                .get(edge.to.as_str())
                .is_some_and(|node| node.provider_turn.is_some())
            && !candidate_nodes.contains_key(edge.to.as_str())
        {
            return Err(ProviderConversationError::RemovedDependency);
        }
    }
    Ok(())
}

fn reject_opaque_state(graph: &ContextGraph) -> Result<(), ProviderConversationError> {
    if graph.nodes.iter().any(|node| {
        node.provider_turn
            .as_ref()
            .is_some_and(|turn| turn.opaque_state_hash.is_some())
    }) {
        Err(ProviderConversationError::UnknownOpaqueState)
    } else {
        Ok(())
    }
}
