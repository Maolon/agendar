//! Delivery, Seen observation and review projections.
//!
//! Deliveries are immutable agent claims. A later `evidence.observed` event
//! overlays Seen results without rewriting the delivery; reviews are retained
//! with their own causal history/frontier and become pending again when a new
//! observation the review never saw arrives.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::cards::{CheckResult, Criterion, ReviewVerdict};
use crate::model::event::{Delivery, Event, EventPayload, EvidenceObserved};
use crate::model::ids::{BlobRef, CriterionId, EventId, NodePath, ProjectSlug, UtcTime, WriterId};
use crate::model::state::{
    DecisionState, DeliveryState, ProjectedCriterion, Provenance, Receipt, ReviewStatus,
    SentBackNote,
};
use crate::reduce::CausalView;

/// Raw delivery/observation/review events collected while folding one project.
#[derive(Debug, Default)]
pub struct EvidenceFold {
    pub deliveries: BTreeMap<EventId, Event>,
    pub observations: BTreeMap<EventId, Event>,
    pub reviews: BTreeMap<EventId, Event>,
}

impl EvidenceFold {
    pub fn apply(&mut self, event: &Event) {
        match &event.payload {
            EventPayload::Delivery(_) => {
                self.deliveries.insert(event.id.clone(), event.clone());
            }
            EventPayload::EvidenceObserved(_) => {
                self.observations.insert(event.id.clone(), event.clone());
            }
            EventPayload::ReviewResult(_) => {
                self.reviews.insert(event.id.clone(), event.clone());
            }
            _ => {}
        }
    }
}

struct Observation<'a> {
    event: &'a Event,
    body: &'a EvidenceObserved,
}

impl Observation<'_> {
    fn key(&self) -> (UtcTime, WriterId, u64, EventId) {
        (
            self.event.ts,
            self.event.writer.clone(),
            self.event.seq,
            self.event.id.clone(),
        )
    }
}

struct Review<'a> {
    event: &'a Event,
    verdict: ReviewVerdict,
    causes: &'a [EventId],
}

impl Review<'_> {
    fn key(&self) -> (UtcTime, WriterId, u64, EventId) {
        (
            self.event.ts,
            self.event.writer.clone(),
            self.event.seq,
            self.event.id.clone(),
        )
    }
}

fn order_of(order_index: &BTreeMap<EventId, usize>, id: &EventId) -> usize {
    order_index.get(id).copied().unwrap_or(usize::MAX)
}

/// True when `later` causally observes `earlier` or is the same writer's later
/// sequence. Timestamps alone never establish observation.
fn observed_after(causal: &CausalView<'_>, later: &Event, earlier: &Event) -> bool {
    if later.id == earlier.id {
        return false;
    }
    if later.writer == earlier.writer && later.seq > earlier.seq {
        return true;
    }
    causal.before(&earlier.id, &later.id)
}

fn causal_maximal<'a, T>(
    items: &'a [T],
    id: impl Fn(&'a T) -> &'a Event,
    causal: &CausalView<'_>,
) -> Vec<&'a T> {
    let mut maximal = Vec::new();
    for item in items {
        let dominated = items.iter().any(|other| {
            let other_event = id(other);
            let event = id(item);
            other_event.id != event.id && observed_after(causal, other_event, event)
        });
        if !dominated {
            maximal.push(item);
        }
    }
    maximal
}

fn effective_receipt<'a>(
    decisions: &'a BTreeMap<EventId, DecisionState>,
    receipt: Option<&Receipt>,
) -> Option<&'a EventId> {
    decisions
        .get(&receipt?.ask)
        .and_then(|decision| decision.effective_receipt.as_ref())
}

/// Project every delivery of one project at the current evaluation time.
pub fn finalize(
    fold: &EvidenceFold,
    decisions: &BTreeMap<EventId, DecisionState>,
    receipts: &BTreeMap<EventId, Receipt>,
    causal: &CausalView<'_>,
    order_index: &BTreeMap<EventId, usize>,
) -> BTreeMap<EventId, DeliveryState> {
    let mut superseded: BTreeMap<EventId, EventId> = BTreeMap::new();
    for (id, event) in &fold.deliveries {
        let Some(previous) = &event.refs.supersedes else {
            continue;
        };
        if !fold.deliveries.contains_key(previous) {
            continue;
        }
        let replace = superseded
            .get(previous)
            .is_none_or(|current| order_of(order_index, current) <= order_of(order_index, id));
        if replace {
            superseded.insert(previous.clone(), id.clone());
        }
    }

    let mut states = BTreeMap::new();
    for (delivery_id, event) in &fold.deliveries {
        let EventPayload::Delivery(body) = &event.payload else {
            continue;
        };
        // A delivery of finished work no decision asked for has no receipt.
        let receipt_id = event.refs.receipt.clone();
        let receipt = match &receipt_id {
            Some(id) => match receipts.get(id) {
                Some(receipt) => Some(receipt),
                // The resolver keeps a delivery pending until its receipt exists.
                None => continue,
            },
            None => None,
        };
        let Some(node) = event
            .refs
            .node
            .clone()
            .or_else(|| receipt.map(|receipt| receipt.node.clone()))
        else {
            continue;
        };
        let superseded_by = superseded.get(delivery_id).cloned();
        let criteria = project_criteria(fold, delivery_id, body, receipt, causal, order_index);
        let observation_frontier: Vec<EventId> = criteria
            .iter()
            .flat_map(|criterion| criterion.observation_frontier.iter().cloned())
            .collect();
        let (review, review_event, review_history, review_frontier, review_conflict) =
            project_reviews(
                fold,
                delivery_id,
                event,
                receipt,
                decisions,
                &observation_frontier,
                causal,
                order_index,
                superseded_by.is_some(),
            );
        states.insert(
            delivery_id.clone(),
            DeliveryState {
                id: delivery_id.clone(),
                node,
                attempt: event.refs.attempt.clone().unwrap_or_else(|| {
                    // The wire decoder requires an attempt reference; retain a
                    // deterministic placeholder rather than dropping the event.
                    event.id.clone()
                }),
                receipt: receipt_id,
                name: body.name.clone(),
                demos: body.demos.clone(),
                criteria,
                disclosure: body.disclosure.clone(),
                review,
                review_event,
                superseded_by,
                review_history,
                review_frontier,
                review_conflict,
                supersedes: supersedes_on_feature(fold, event),
                replies: body.replies.clone(),
                sent_back: latest_send_back(fold, delivery_id, order_index),
            },
        );
    }
    states
}

/// The delivery this one replaces, when it exists on the same feature.
fn supersedes_on_feature(fold: &EvidenceFold, event: &Event) -> Option<EventId> {
    let previous = event.refs.supersedes.as_ref()?;
    let target = fold.deliveries.get(previous)?;
    (target.refs.node.is_some() && target.refs.node == event.refs.node).then(|| previous.clone())
}

/// The delivery's latest send-back by order, whatever happened after it.
fn latest_send_back(
    fold: &EvidenceFold,
    delivery_id: &EventId,
    order_index: &BTreeMap<EventId, usize>,
) -> Option<SentBackNote> {
    fold.reviews
        .values()
        .filter(|event| event.refs.delivery.as_ref() == Some(delivery_id))
        .filter_map(|event| match &event.payload {
            EventPayload::ReviewResult(body) if body.verdict == ReviewVerdict::SendBack => {
                Some((event, body))
            }
            _ => None,
        })
        .max_by_key(|(event, _)| (order_of(order_index, &event.id), event.id.clone()))
        .map(|(event, body)| SentBackNote {
            review: event.id.clone(),
            at: event.ts,
            reason: body.reason.clone(),
            comments: body.comments.clone(),
        })
}

fn project_criteria(
    fold: &EvidenceFold,
    delivery_id: &EventId,
    body: &Delivery,
    receipt: Option<&Receipt>,
    causal: &CausalView<'_>,
    order_index: &BTreeMap<EventId, usize>,
) -> Vec<ProjectedCriterion> {
    body.criteria
        .iter()
        .map(|result| {
            let criterion = receipt
                .and_then(|receipt| {
                    receipt
                        .done_when
                        .iter()
                        .find(|criterion| criterion.id == result.criterion)
                })
                .cloned()
                .unwrap_or_else(|| Criterion {
                    id: result.criterion.clone(),
                    text: String::new(),
                });
            let mut history = observations_for(fold, delivery_id, &result.criterion);
            history.sort_by_key(|observation| order_of(order_index, &observation.event.id));
            let history_ids: Vec<EventId> = history
                .iter()
                .map(|observation| observation.event.id.clone())
                .collect();
            let maximal = causal_maximal(&history, |observation| observation.event, causal);
            let mut frontier: Vec<&Observation<'_>> = maximal;
            frontier.sort_by_key(|observation| observation.key());
            let selected = frontier.last().copied();
            let frontier_ids: Vec<EventId> = frontier
                .iter()
                .map(|observation| observation.event.id.clone())
                .collect();
            let results: BTreeSet<CheckResult> = frontier
                .iter()
                .map(|observation| observation.body.result)
                .collect();
            let seen_result = selected.map(|observation| observation.body.result);
            let conflict = results.len() > 1
                || selected.is_some_and(|observation| {
                    result.status != CheckResult::NotProven
                        && observation.body.result != result.status
                });
            let provenance = match seen_result {
                Some(CheckResult::Passed | CheckResult::Failed) => Provenance::Seen,
                Some(CheckResult::NotProven) => Provenance::NotProvenYet,
                None if result.status == CheckResult::NotProven => Provenance::NotProvenYet,
                None => Provenance::AgentSays,
            };
            ProjectedCriterion {
                criterion,
                agent_result: result.status,
                seen_result,
                observation: selected.map(|observation| observation.event.id.clone()),
                observation_history: history_ids,
                observation_frontier: frontier_ids,
                provenance,
                conflict,
            }
        })
        .collect()
}

fn observations_for<'a>(
    fold: &'a EvidenceFold,
    delivery_id: &EventId,
    criterion: &CriterionId,
) -> Vec<Observation<'a>> {
    fold.observations
        .values()
        .filter_map(|event| {
            let EventPayload::EvidenceObserved(body) = &event.payload else {
                return None;
            };
            (event.refs.delivery.as_ref() == Some(delivery_id) && &body.criterion == criterion)
                .then_some(Observation { event, body })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn project_reviews(
    fold: &EvidenceFold,
    delivery_id: &EventId,
    delivery: &Event,
    receipt: Option<&Receipt>,
    decisions: &BTreeMap<EventId, DecisionState>,
    observation_frontier: &[EventId],
    causal: &CausalView<'_>,
    order_index: &BTreeMap<EventId, usize>,
    superseded: bool,
) -> (
    ReviewStatus,
    Option<EventId>,
    Vec<EventId>,
    Vec<EventId>,
    bool,
) {
    let mut history: Vec<Review<'_>> = fold
        .reviews
        .values()
        .filter_map(|event| {
            let EventPayload::ReviewResult(body) = &event.payload else {
                return None;
            };
            (event.refs.delivery.as_ref() == Some(delivery_id)).then_some(Review {
                event,
                verdict: body.verdict,
                causes: &event.refs.causes,
            })
        })
        .collect();
    history.sort_by_key(|review| order_of(order_index, &review.event.id));
    let history_ids: Vec<EventId> = history
        .iter()
        .map(|review| review.event.id.clone())
        .collect();

    let current_receipt = effective_receipt(decisions, receipt).cloned();

    let eligible: Vec<&Review<'_>> = history
        .iter()
        .filter(|review| {
            // A delivery bound to a receipt is reviewed only while that
            // receipt is current; one without a receipt has nothing to go stale.
            let receipt_current = match delivery.refs.receipt.as_ref() {
                Some(bound) => {
                    current_receipt.as_ref() == Some(bound) && review.causes.contains(bound)
                }
                None => true,
            };
            !superseded
                && receipt_current
                && observation_frontier
                    .iter()
                    .all(|observation| causal.before(observation, &review.event.id))
        })
        .collect();
    let maximal: Vec<&Review<'_>> = causal_maximal(&eligible, |review| review.event, causal)
        .into_iter()
        .copied()
        .collect();
    let mut frontier: Vec<&Review<'_>> = maximal;
    frontier.sort_by_key(|review| review.key());
    let selected = frontier.last().copied();
    let frontier_ids: Vec<EventId> = frontier
        .iter()
        .map(|review| review.event.id.clone())
        .collect();
    let verdicts: BTreeSet<ReviewVerdict> = frontier.iter().map(|review| review.verdict).collect();
    let review_conflict = verdicts.len() > 1;
    let status = match selected.map(|review| review.verdict) {
        Some(ReviewVerdict::Accept) => ReviewStatus::Accepted,
        Some(ReviewVerdict::SendBack) => ReviewStatus::SentBack,
        None => ReviewStatus::Pending,
    };
    (
        status,
        selected.map(|review| review.event.id.clone()),
        history_ids,
        frontier_ids,
        review_conflict,
    )
}

/// True when this project's raw events reference the exact blob. The media
/// protocol and report reader only expose referenced bytes, never a guessed
/// digest or an arbitrary Vault path.
pub fn blob_referenced(
    events: &[Event],
    project: &ProjectSlug,
    blob: &BlobRef,
    include_reports: bool,
) -> bool {
    events.iter().any(|event| {
        if &event.project != project {
            return false;
        }
        let mut found = false;
        crate::reduce::usage::visit_event_blobs(event, &mut |usage, item| {
            found |= item == blob && (include_reports || !usage.is_report());
        });
        found
    })
}

/// Whether `blob` is (or was) the icon of `project`: the one path an SVG is
/// served on.
pub fn project_icon_referenced(events: &[Event], project: &ProjectSlug, blob: &BlobRef) -> bool {
    events.iter().any(|event| {
        &event.project == project
            && matches!(&event.payload, EventPayload::ProjectDeclared(body) if body.icon.as_ref() == Some(blob))
    })
}

/// Nodes whose active delivery still needs a human review before downstream work
/// may proceed, and nodes whose delivery has an accepted review.
pub fn review_gates(
    deliveries: &BTreeMap<EventId, DeliveryState>,
) -> (BTreeSet<NodePath>, BTreeSet<NodePath>) {
    let mut per_node: BTreeMap<NodePath, Vec<ReviewStatus>> = BTreeMap::new();
    for delivery in deliveries.values() {
        if delivery.superseded_by.is_some() {
            continue;
        }
        per_node
            .entry(delivery.node.clone())
            .or_default()
            .push(delivery.review);
    }
    let mut pending = BTreeSet::new();
    let mut accepted = BTreeSet::new();
    for (node, statuses) in per_node {
        if statuses
            .iter()
            .any(|status| *status != ReviewStatus::Accepted)
        {
            pending.insert(node);
        } else {
            accepted.insert(node);
        }
    }
    (pending, accepted)
}
