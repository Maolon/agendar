//! Dependency graph construction, cycle rejection and traversal helpers
//!.
//!
//! Edge selection is reproducible: candidate edges are sorted by the winning
//! dependency patch's `O` index, then downstream and upstream bytes. An edge is
//! rejected when it is a self-edge or when it would close a cycle; only the
//! rejected edge and its source event are surfaced as a rescue.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::model::ids::{EventId, NodePath};
use crate::model::state::{
    DecisionLifecycle, DecisionState, Graph, GraphEdge, GraphNode, GraphNodeId, GraphNodeKind,
    NeedsKind, ProjectState,
};
/// One candidate dependency edge that survived cycle rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedEdge {
    pub upstream: NodePath,
    pub downstream: NodePath,
    pub source_event: EventId,
}

/// One candidate dependency edge rejected because it would close a cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedEdge {
    pub upstream: NodePath,
    pub downstream: NodePath,
    pub source_event: EventId,
}

/// Declared dependencies of one feature and the winning patch source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredDeps {
    pub node: NodePath,
    pub depends_on: Vec<NodePath>,
    pub source: EventId,
}

/// Declared dependencies that name a feature which does not exist yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingEdge {
    pub upstream: NodePath,
    pub downstream: NodePath,
    pub source_event: EventId,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanEdges {
    pub accepted: Vec<AcceptedEdge>,
    pub rejected: Vec<RejectedEdge>,
    pub missing: Vec<MissingEdge>,
}

impl PlanEdges {
    pub fn accepted_map(&self) -> BTreeMap<NodePath, Vec<NodePath>> {
        let mut map: BTreeMap<NodePath, Vec<NodePath>> = BTreeMap::new();
        for edge in &self.accepted {
            map.entry(edge.upstream.clone())
                .or_default()
                .push(edge.downstream.clone());
        }
        for values in map.values_mut() {
            values.sort();
            values.dedup();
        }
        map
    }

    pub fn accepted_by_downstream(&self) -> BTreeMap<NodePath, Vec<NodePath>> {
        let mut map: BTreeMap<NodePath, Vec<NodePath>> = BTreeMap::new();
        for edge in &self.accepted {
            map.entry(edge.downstream.clone())
                .or_default()
                .push(edge.upstream.clone());
        }
        for values in map.values_mut() {
            values.sort();
            values.dedup();
        }
        map
    }
}

/// Select the canonical accepted edge set for one project's declared plans.
pub fn plan_edges(
    declared: &[DeclaredDeps],
    features: &BTreeSet<NodePath>,
    order_index: &BTreeMap<EventId, usize>,
) -> PlanEdges {
    struct Candidate {
        order: usize,
        source: EventId,
        upstream: NodePath,
        downstream: NodePath,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    for feature in declared {
        for upstream in &feature.depends_on {
            candidates.push(Candidate {
                order: order_index
                    .get(&feature.source)
                    .copied()
                    .unwrap_or(usize::MAX),
                source: feature.source.clone(),
                upstream: upstream.clone(),
                downstream: feature.node.clone(),
            });
        }
    }
    candidates.sort_by(|left, right| {
        (left.order, left.downstream.as_str(), left.upstream.as_str()).cmp(&(
            right.order,
            right.downstream.as_str(),
            right.upstream.as_str(),
        ))
    });

    let mut plan = PlanEdges::default();
    let mut adjacency: BTreeMap<NodePath, BTreeSet<NodePath>> = BTreeMap::new();
    for candidate in candidates {
        if !features.contains(&candidate.upstream) {
            plan.missing.push(MissingEdge {
                upstream: candidate.upstream,
                downstream: candidate.downstream,
                source_event: candidate.source,
            });
            continue;
        }
        let closes_cycle = candidate.upstream == candidate.downstream
            || reachable(&adjacency, &candidate.downstream, &candidate.upstream);
        if closes_cycle {
            plan.rejected.push(RejectedEdge {
                upstream: candidate.upstream,
                downstream: candidate.downstream,
                source_event: candidate.source,
            });
        } else {
            adjacency
                .entry(candidate.upstream.clone())
                .or_default()
                .insert(candidate.downstream.clone());
            plan.accepted.push(AcceptedEdge {
                upstream: candidate.upstream,
                downstream: candidate.downstream,
                source_event: candidate.source,
            });
        }
    }
    plan
}

/// Is `to` reachable from `from` following upstream → downstream edges?
pub fn reachable(
    adjacency: &BTreeMap<NodePath, BTreeSet<NodePath>>,
    from: &NodePath,
    to: &NodePath,
) -> bool {
    if from == to {
        return true;
    }
    let mut seen: BTreeSet<NodePath> = BTreeSet::new();
    let mut queue: VecDeque<NodePath> = VecDeque::new();
    queue.push_back(from.clone());
    while let Some(node) = queue.pop_front() {
        if !seen.insert(node.clone()) {
            continue;
        }
        if let Some(next) = adjacency.get(&node) {
            for candidate in next {
                if candidate == to {
                    return true;
                }
                queue.push_back(candidate.clone());
            }
        }
    }
    false
}

/// Sorted unique descendants of `start` (excluding `start`).
pub fn descendants(
    adjacency: &BTreeMap<NodePath, BTreeSet<NodePath>>,
    start: &NodePath,
) -> Vec<NodePath> {
    let mut seen: BTreeSet<NodePath> = BTreeSet::new();
    let mut queue: VecDeque<NodePath> = VecDeque::new();
    if let Some(next) = adjacency.get(start) {
        for node in next {
            queue.push_back(node.clone());
        }
    }
    while let Some(node) = queue.pop_front() {
        if !seen.insert(node.clone()) {
            continue;
        }
        if let Some(next) = adjacency.get(&node) {
            for candidate in next {
                if candidate != start {
                    queue.push_back(candidate.clone());
                }
            }
        }
    }
    seen.into_iter().collect()
}

/// Sorted unique ancestors of `start` (excluding `start`).
pub fn ancestors(
    adjacency: &BTreeMap<NodePath, BTreeSet<NodePath>>,
    start: &NodePath,
) -> Vec<NodePath> {
    let reverse = reverse_adjacency(adjacency);
    descendants(&reverse, start)
}

fn reverse_adjacency(
    adjacency: &BTreeMap<NodePath, BTreeSet<NodePath>>,
) -> BTreeMap<NodePath, BTreeSet<NodePath>> {
    let mut reverse: BTreeMap<NodePath, BTreeSet<NodePath>> = BTreeMap::new();
    for (upstream, downstreams) in adjacency {
        for downstream in downstreams {
            reverse
                .entry(downstream.clone())
                .or_default()
                .insert(upstream.clone());
        }
    }
    reverse
}

/// Build the semantic `Graph` projection from a finished project state.
///
/// The DTO is exactly `GraphNode`/`GraphEdge`: feature nodes,
/// one node per decision and per review, and the same accepted dependency edges
/// the gate projection uses plus the decision/review → held-feature edges.
/// Geometry and basis/ETA stay on the feature records; nothing is added here.
pub fn build_graph(project: &ProjectState) -> Graph {
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let need_for = |source: &EventId, kind: NeedsKind| -> Option<String> {
        project
            .needs_you
            .iter()
            .find(|need| &need.source == source && need.kind == kind)
            .map(|need| need.id.clone())
    };
    for (node, feature) in &project.features {
        nodes.push(GraphNode {
            id: GraphNodeId::feature(node),
            kind: GraphNodeKind::Feature,
            label: feature.title.clone(),
            feature: Some(node.clone()),
            need_id: None,
            state: feature.progress.as_str().to_owned(),
            assumed: feature.basis != crate::model::state::Basis::Firm,
        });
        let source = feature.field_sources.get("depends_on").cloned();
        if let Some(source) = source {
            for upstream in &feature.accepted_depends_on {
                edges.push(GraphEdge {
                    from: GraphNodeId::feature(upstream),
                    to: GraphNodeId::feature(node),
                    source_event: source.clone(),
                });
            }
        }
    }
    for (ask, decision) in &project.decisions {
        let need_id =
            need_for(ask, NeedsKind::Decide).or_else(|| need_for(ask, NeedsKind::Confirm));
        nodes.push(GraphNode {
            id: GraphNodeId::decision(ask),
            kind: GraphNodeKind::Decision,
            label: decision.card.question.clone(),
            feature: Some(decision.node.clone()),
            need_id,
            state: decision.lifecycle.as_str().to_owned(),
            assumed: decision.mode == crate::model::ids::Mode::Assume,
        });
        edges.push(GraphEdge {
            from: GraphNodeId::decision(ask),
            to: GraphNodeId::feature(&decision.node),
            source_event: ask.clone(),
        });
    }
    for (delivery, state) in &project.deliveries {
        nodes.push(GraphNode {
            id: GraphNodeId::review(delivery),
            kind: GraphNodeKind::Review,
            label: state.name.clone(),
            feature: Some(state.node.clone()),
            need_id: need_for(delivery, NeedsKind::Review),
            state: review_state(state.review).to_owned(),
            assumed: false,
        });
        edges.push(GraphEdge {
            from: GraphNodeId::review(delivery),
            to: GraphNodeId::feature(&state.node),
            source_event: delivery.clone(),
        });
    }
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    edges.sort_by(|left, right| {
        (
            left.from.as_str(),
            left.to.as_str(),
            left.source_event.as_str(),
        )
            .cmp(&(
                right.from.as_str(),
                right.to.as_str(),
                right.source_event.as_str(),
            ))
    });
    Graph { nodes, edges }
}

/// One review node state string (the wire vocabulary of `ReviewState`).
fn review_state(review: crate::model::state::ReviewStatus) -> &'static str {
    match review {
        crate::model::state::ReviewStatus::Pending => "pending",
        crate::model::state::ReviewStatus::Accepted => "accepted",
        crate::model::state::ReviewStatus::SentBack => "sent_back",
    }
}

/// Decision node ids for open mandatory decisions on one feature.
pub fn pending_decisions(
    decisions: &BTreeMap<EventId, DecisionState>,
    node: &NodePath,
) -> Vec<EventId> {
    decisions
        .iter()
        .filter(|(_, decision)| {
            &decision.node == node
                && decision.lifecycle == DecisionLifecycle::Open
                && decision.mode == crate::model::ids::Mode::Ask
        })
        .map(|(ask, _)| ask.clone())
        .collect()
}
