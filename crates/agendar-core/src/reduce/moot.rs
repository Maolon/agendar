//! Questions on closed steps.
//!
//! An open question whose step was dropped, or is done with the owner's
//! accept, is still answerable, but nobody is expected to answer it any more.
//! For [`MOOT_FOLD_S`] after the step closed it stays in the active Needs-you
//! list, labelled, so its asker can withdraw it; after that, still unanswered
//! and not withdrawn, it folds into "On closed steps": never notified, and it
//! no longer holds its session's steps (`reduce::claim`). An answer closes it
//! as usual; a restart of the step reopens the step, which unfolds it.
//!
//! Only a drop or the owner's accept closes a step here: a step the agent only
//! says is done keeps its questions active, since a question often lands on
//! the current step and outlives it.
//!
//! A function of the events and `as_of`; nothing is written.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::event::WorkStatus;
use crate::model::ids::{EventId, Mode, NodePath, UtcTime};
use crate::model::state::{ClosedStep, DecisionLifecycle, DecisionState, MOOT_FOLD_S};
use crate::reduce::plan::FeatureFold;

/// When each closed step closed: its drop, or the accept (and the agent's own
/// done, when it came later) of a step whose live deliveries are all accepted.
pub fn closed_steps(
    features: &BTreeMap<NodePath, FeatureFold>,
    event_ts: &BTreeMap<EventId, UtcTime>,
) -> BTreeMap<NodePath, UtcTime> {
    let mut closed = BTreeMap::new();
    for (node, feature) in features {
        let since = match feature.current() {
            None => feature
                .unstarted_drop
                .as_ref()
                .and_then(|drop| event_ts.get(drop).copied()),
            Some(attempt) => match attempt.status {
                WorkStatus::Dropped => attempt.status_ts,
                WorkStatus::Started => attempt
                    .accepted_at()
                    .and(attempt.settle.accepted.as_ref())
                    .map(|verdict| verdict.at),
                WorkStatus::Done | WorkStatus::Verified => {
                    attempt.settle.accepted.as_ref().map(|verdict| {
                        attempt
                            .status_ts
                            .map_or(verdict.at, |done| done.max(verdict.at))
                    })
                }
            },
        };
        if let Some(since) = since {
            closed.insert(node.clone(), since);
        }
    }
    closed
}

/// The open question `decision` on a closed step, with whether it folded.
pub fn closed_step(
    decision: &DecisionState,
    closed: &BTreeMap<NodePath, UtcTime>,
    as_of: UtcTime,
) -> Option<ClosedStep> {
    if decision.lifecycle != DecisionLifecycle::Open || decision.mode != Mode::Ask {
        return None;
    }
    let since = *closed.get(&decision.node)?;
    Some(ClosedStep {
        since,
        folded: as_of >= since.add_seconds(MOOT_FOLD_S),
    })
}

/// The open questions that folded: they no longer hold their session.
pub fn folded(
    decisions: &BTreeMap<EventId, DecisionState>,
    closed: &BTreeMap<NodePath, UtcTime>,
    as_of: UtcTime,
) -> BTreeSet<EventId> {
    decisions
        .iter()
        .filter(|(_, decision)| {
            closed_step(decision, closed, as_of).is_some_and(|closed| closed.folded)
        })
        .map(|(ask, _)| ask.clone())
        .collect()
}
