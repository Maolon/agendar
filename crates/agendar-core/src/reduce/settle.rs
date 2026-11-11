//! Derived settle of an attempt from its deliveries and their reviews.
//!
//! Nothing is recorded: the owner's review is already the fact. For a started
//! attempt, only its own live (not superseded) deliveries count:
//!
//! - no live delivery, or a work signal after the latest one: at work;
//! - a live delivery sent back: sent back (open again);
//! - a live delivery waiting for a review: in review (the owner holds it);
//! - every live delivery accepted: done.
//!
//! A work signal is a pulse that sets an ETA, or a new delivery; a message-only
//! pulse, an ack, a report or a plan patch is not one. A review of a
//! receipt-bound delivery counts only while that receipt is current
//! (`reduce::evidence`), so a re-answered decision reverts a derived done by
//! itself. A `dropped` ordered after the accepting review is history only:
//! the owner's verdict outranks the agent's later word (`reduce::plan`).
//!
//! The settle carries its reason with it: when the shown status differs from
//! the agent's last self-report (an accept closed a step the agent still had
//! open, or a send-back reopened it), [`Settle::correction`] names the rule and
//! the events it rests on. Nothing else derives that reason.

use std::collections::BTreeMap;

use crate::model::event::WorkStatus;
use crate::model::ids::{EventId, NodePath, UtcTime};
use crate::model::state::{
    Correction, CorrectionRule, DeliveryState, Phase, ProjectState, ReviewStatus,
};
use crate::reduce::plan::{AttemptFold, FeatureFold};

/// Where an attempt's latest delivery stands when it is the agent's last work
/// signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlePhase {
    InReview,
    SentBack,
    Accepted,
}

impl SettlePhase {
    /// The phase a step in progress shows; accepted is shown as done instead.
    pub fn open_phase(self) -> Option<Phase> {
        match self {
            Self::InReview => Some(Phase::InReview),
            Self::SentBack => Some(Phase::SentBack),
            Self::Accepted => None,
        }
    }
}

/// One owner verdict a settle rests on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub review: EventId,
    pub at: UtcTime,
    pub delivery: EventId,
}

/// The derived settle of one attempt.
#[derive(Debug, Clone, Default)]
pub struct Settle {
    /// `None` while the attempt has no live delivery or the agent signalled
    /// more work after the latest one.
    pub phase: Option<SettlePhase>,
    /// `O` index and timestamp of the latest live delivery.
    pub delivered: Option<(usize, UtcTime)>,
    /// `O` index of the latest accepting review, when every live delivery is
    /// accepted.
    pub accepted_order: Option<usize>,
    /// The latest accepting review, when every live delivery is accepted,
    /// whether or not the agent signalled more work after it (Rule 1b rests on
    /// it once the claim expires).
    pub accepted: Option<Verdict>,
    /// The rule and evidence when this settle shows something other than the
    /// agent's last self-report: `accepted` (done) or `sent_back` (open).
    pub correction: Option<Correction>,
    /// Review waits: from a delivery until its first verdict or the agent's
    /// next work signal, whichever comes first. A human wait like an open ask.
    pub waits: Vec<(UtcTime, Option<UtcTime>)>,
}

/// Settle every attempt of the project. Returns the drops that became history
/// only, with their node, for a diagnostic.
pub fn apply(
    features: &mut BTreeMap<NodePath, FeatureFold>,
    deliveries: &BTreeMap<EventId, DeliveryState>,
    order_index: &BTreeMap<EventId, usize>,
    event_ts: &BTreeMap<EventId, UtcTime>,
) -> Vec<(NodePath, EventId)> {
    let mut by_attempt: BTreeMap<&EventId, Vec<&DeliveryState>> = BTreeMap::new();
    for delivery in deliveries.values() {
        if order_index.contains_key(&delivery.id) {
            by_attempt
                .entry(&delivery.attempt)
                .or_default()
                .push(delivery);
        }
    }
    let order = |id: &EventId| order_index.get(id).copied().unwrap_or(usize::MAX);
    let mut undone = Vec::new();
    for (node, feature) in features.iter_mut() {
        for attempt in feature.attempts.values_mut() {
            let Some(own) = by_attempt.get(&attempt.id) else {
                continue;
            };
            let mut own: Vec<&DeliveryState> = own
                .iter()
                .copied()
                .filter(|delivery| &delivery.node == node)
                .collect();
            own.sort_by_key(|delivery| order(&delivery.id));
            attempt.settle = settle(attempt, &own, &order, event_ts);
            if let Some(dropped) = undo_drop(attempt) {
                // The board shows the accept, not the agent's later drop.
                let shown_done = attempt.status != WorkStatus::Started
                    || attempt.settle.phase == Some(SettlePhase::Accepted);
                attempt.settle.correction = attempt
                    .settle
                    .accepted
                    .as_ref()
                    .filter(|_| shown_done)
                    .map(|verdict| Correction {
                        rule: CorrectionRule::Accepted,
                        since: verdict.at,
                        evidence: vec![verdict.review.clone(), dropped.clone()],
                    });
                undone.push((node.clone(), dropped));
            }
        }
    }
    undone
}

fn settle(
    attempt: &AttemptFold,
    own: &[&DeliveryState],
    order: &impl Fn(&EventId) -> usize,
    event_ts: &BTreeMap<EventId, UtcTime>,
) -> Settle {
    let ts = |id: &EventId| event_ts.get(id).copied();
    let live: Vec<&DeliveryState> = own
        .iter()
        .copied()
        .filter(|delivery| delivery.superseded_by.is_none())
        .collect();
    let Some(latest) = live.last() else {
        return Settle {
            waits: waits(attempt, own, order, event_ts),
            ..Settle::default()
        };
    };
    let latest_order = order(&latest.id);
    let delivered = ts(&latest.id).map(|at| (latest_order, at));
    let accepted_order = live
        .iter()
        .all(|delivery| delivery.review == ReviewStatus::Accepted)
        .then(|| {
            live.iter()
                .filter_map(|delivery| delivery.review_event.as_ref())
                .map(order)
                .max()
        })
        .flatten();
    let verdict = |delivery: &DeliveryState| {
        let review = delivery.review_event.clone()?;
        Some(Verdict {
            at: ts(&review)?,
            review,
            delivery: delivery.id.clone(),
        })
    };
    let accepted = accepted_order
        .and_then(|at| {
            live.iter()
                .find(|delivery| delivery.review_event.as_ref().map(order) == Some(at))
        })
        .and_then(|delivery| verdict(delivery));
    let phase = if attempt.eta_order > latest_order {
        None
    } else if live
        .iter()
        .any(|delivery| delivery.review == ReviewStatus::SentBack)
    {
        Some(SettlePhase::SentBack)
    } else if live
        .iter()
        .any(|delivery| delivery.review == ReviewStatus::Pending)
    {
        Some(SettlePhase::InReview)
    } else {
        Some(SettlePhase::Accepted)
    };
    // Only a step the agent still holds open is corrected: an agent that said
    // done itself already agrees with an accept.
    let correction = match phase {
        _ if attempt.status != WorkStatus::Started => None,
        Some(SettlePhase::Accepted) => accepted.as_ref().map(|verdict| Correction {
            rule: CorrectionRule::Accepted,
            since: verdict.at,
            evidence: vec![verdict.review.clone(), verdict.delivery.clone()],
        }),
        Some(SettlePhase::SentBack) => live
            .iter()
            .filter(|delivery| delivery.review == ReviewStatus::SentBack)
            .filter_map(|delivery| verdict(delivery))
            .max_by_key(|verdict| order(&verdict.review))
            .map(|verdict| Correction {
                rule: CorrectionRule::SentBack,
                since: verdict.at,
                evidence: vec![verdict.review, verdict.delivery],
            }),
        _ => None,
    };
    Settle {
        phase,
        delivered,
        accepted_order,
        accepted,
        correction,
        waits: waits(attempt, own, order, event_ts),
    }
}

/// One review wait per delivery of the attempt: from the delivery to its first
/// verdict or the agent's next work signal. A wait still open, or one that
/// ends after the agent's own done, is the agent's bookkeeping, not the
/// owner's hold, once the agent has finished.
fn waits(
    attempt: &AttemptFold,
    own: &[&DeliveryState],
    order: &impl Fn(&EventId) -> usize,
    event_ts: &BTreeMap<EventId, UtcTime>,
) -> Vec<(UtcTime, Option<UtcTime>)> {
    let ts = |id: &EventId| event_ts.get(id).copied();
    let mut intervals = Vec::new();
    for delivery in own {
        let Some(start) = ts(&delivery.id) else {
            continue;
        };
        let at = order(&delivery.id);
        let verdict = (delivery.review != ReviewStatus::Pending)
            .then(|| delivery.review_history.iter().filter_map(ts).min())
            .flatten();
        let next_signal = attempt
            .eta_signals
            .iter()
            .filter(|(signal, _)| *signal > at)
            .map(|(_, signal_ts)| *signal_ts)
            .chain(
                own.iter()
                    .filter(|other| order(&other.id) > at)
                    .filter_map(|other| ts(&other.id)),
            )
            .min();
        let end = match (verdict, next_signal) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        };
        let kept = match (attempt.first_completion, end) {
            (None, _) => true,
            (Some((_, done)), Some(end)) => end.millis() <= done.millis(),
            (Some(_), None) => false,
        };
        if kept {
            intervals.push((start, end.map(|end| end.max(start))));
        }
    }
    intervals
}

/// Rule 2c: a drop ordered after the review that accepted every live delivery
/// is history only. A drop ordered before the accept stays a drop.
fn undo_drop(attempt: &mut AttemptFold) -> Option<EventId> {
    if attempt.status != WorkStatus::Dropped {
        return None;
    }
    let accepted = attempt.settle.accepted_order?;
    let undo = attempt.before_drop.as_ref()?;
    if undo.order < accepted {
        return None;
    }
    let undo = attempt.before_drop.take()?;
    attempt.status = undo.status;
    attempt.status_event = undo.status_event;
    attempt.status_ts = undo.status_ts;
    Some(undo.event)
}

/// True when the owner accepted every live delivery of `attempt`: the agent
/// may confirm it, never drop it.
pub fn accepted_by_owner(project: &ProjectState, attempt: &EventId) -> bool {
    let mut live = project
        .deliveries
        .values()
        .filter(|delivery| &delivery.attempt == attempt && delivery.superseded_by.is_none())
        .peekable();
    live.peek().is_some() && live.all(|delivery| delivery.review == ReviewStatus::Accepted)
}
