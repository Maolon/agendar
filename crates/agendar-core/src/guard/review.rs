//! The checks one review verdict passes before any host writes it.
//!
//! [`check_review`] reads reduced state and decides; the caller refreshes the
//! state first and writes the review afterwards
//! (`Service::review_delivery_events`). The desktop and mobile clients map a
//! denial to their own errors.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::model::{DeliveryState, EventId, ProjectSlug, ProjectState, Sha256, State};

use super::revision;

/// Delivery revision: identity, receipt binding, every projected Seen result and
/// the current review frontier. A new observation or review changes this token.
pub fn delivery_revision(
    project: &ProjectSlug,
    delivery: &DeliveryState,
    current_receipt: Option<&EventId>,
) -> Sha256 {
    let criteria: Vec<Value> = delivery
        .criteria
        .iter()
        .map(|criterion| {
            json!({
                "criterion": criterion.criterion.id,
                "agent": criterion.agent_result,
                "seen": criterion.seen_result,
                "observation": criterion.observation,
                "history": criterion.observation_history,
                "frontier": criterion.observation_frontier,
                "conflict": criterion.conflict,
            })
        })
        .collect();
    revision(json!([
        "delivery",
        project.as_str(),
        delivery.id.as_str(),
        delivery.node.as_str(),
        delivery.attempt.as_str(),
        delivery.receipt.as_ref().map(|id| id.as_str()),
        current_receipt.map(|id| id.as_str()),
        delivery.superseded_by.as_ref().map(|id| id.as_str()),
        delivery.review,
        delivery.review_history,
        delivery.review_frontier,
        criteria,
    ]))
}

/// The delivery `id` with its project, searched across every project.
pub fn find_delivery<'a>(
    state: &'a State,
    id: &EventId,
) -> Option<(&'a ProjectSlug, &'a ProjectState, &'a DeliveryState)> {
    state.projects.iter().find_map(|(slug, project)| {
        project
            .deliveries
            .get(id)
            .map(|delivery| (slug, project, delivery))
    })
}

/// The receipt in effect for the question a delivery answers; `None` for a
/// delivery no decision asked for.
pub fn current_receipt<'a>(
    project: &'a ProjectState,
    delivery: &DeliveryState,
) -> Option<&'a EventId> {
    project
        .receipts
        .get(delivery.receipt.as_ref()?)
        .and_then(|receipt| project.decisions.get(&receipt.ask))
        .and_then(|decision| decision.effective_receipt.as_ref())
}

/// Why a verdict is not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReviewDenial {
    /// This host has not read the delivery.
    NotFound,
    /// An observation or a review changed the delivery after the caller read
    /// it; a verdict already given is one such change.
    Changed,
    /// A newer delivery replaced this one.
    Superseded,
    /// The answer this delivery was made for is no longer the current one.
    ReceiptNotCurrent,
}

/// A verdict that passed every check, with what the write needs.
#[derive(Debug, Clone)]
pub struct CheckedReview<'a> {
    pub slug: &'a ProjectSlug,
    pub project: &'a ProjectState,
    pub delivery: &'a DeliveryState,
    /// What the review rests on: the receipts, the observations it saw and
    /// the review frontier it replaces.
    pub causes: Vec<EventId>,
}

/// Check one verdict against reduced state: the delivery is known, it is the
/// revision the caller rendered, it was not superseded and its receipt is
/// still the current one.
pub fn check_review<'a>(
    state: &'a State,
    delivery_id: &EventId,
    expected_revision: &Sha256,
) -> Result<CheckedReview<'a>, ReviewDenial> {
    let (slug, project, delivery) =
        find_delivery(state, delivery_id).ok_or(ReviewDenial::NotFound)?;
    let current = current_receipt(project, delivery);
    if &delivery_revision(slug, delivery, current) != expected_revision {
        return Err(ReviewDenial::Changed);
    }
    if delivery.superseded_by.is_some() {
        return Err(ReviewDenial::Superseded);
    }
    if delivery
        .receipt
        .as_ref()
        .is_some_and(|receipt| current != Some(receipt))
    {
        return Err(ReviewDenial::ReceiptNotCurrent);
    }
    let mut causes: BTreeSet<EventId> = BTreeSet::new();
    causes.extend(delivery.receipt.iter().cloned());
    causes.extend(current.cloned());
    for criterion in &delivery.criteria {
        causes.extend(criterion.observation.iter().cloned());
        causes.extend(criterion.observation_frontier.iter().cloned());
    }
    causes.extend(delivery.review_frontier.iter().cloned());
    Ok(CheckedReview {
        slug,
        project,
        delivery,
        causes: causes.into_iter().collect(),
    })
}
