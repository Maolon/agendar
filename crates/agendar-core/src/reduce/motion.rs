//! Where a session stands for the owner's screens: moving, stalled, or
//! neither. A display rule only.
//!
//! Nothing here is read by an agent and nothing here changes the state:
//! `SessionState.stalled_on`, the `stalled_agents` of a need row and of the
//! summary, `active_agents`, `board`, `settle`, `inbox`, the nudges, what is
//! owed and the rescue rows are derived elsewhere and stay as they are.
//!
//! The rule, in the order it is applied to one session:
//!
//! 1. **Waiting on the owner.** A Needs-you row stalls the session
//!    (`stalled_on`): stalled, since the oldest such row. The review row of
//!    its own current step is not counted: a step delivered and waiting for
//!    review is the owner's, and the row under Needs you already says so.
//! 2. **Moving.** The session is live and holds a step in progress that is
//!    at work: not waiting for review, its claim not ended, and not a
//!    sent-back step nobody is on. It is moving on its current step when that
//!    is such a step, else on the one of them it started last. A live session
//!    with no such step (its last one accepted, nothing started since) is
//!    not moving; it is back with the next step it starts.
//! 3. **Sent back, nobody on it.** The session holds a sent-back step it is
//!    not at work on ([`sent_back_unattended`]): stalled, since the
//!    send-back.
//! 4. Otherwise neither.
//!
//! A sent-back step is unattended when its agent is not live or its claim
//! is no longer held. "Quiet inside its ETA" does not hold a sent-back step:
//! that ETA was the estimate of the delivery the owner already answered. The
//! desktop states the same predicate per step (`isMoving` / `isStalled` in
//! `app/src/state/moving.ts`) and words it "sent back, nobody on it".
//!
//! A send-back or a work signal ends the review phase (`reduce::settle`), and
//! an accepted step is finished, so neither is in review.

use crate::model::ids::UtcTime;
use crate::model::state::{
    FeatureState, Lease, Liveness, NeedsKind, Phase, Progress, ProjectState, SessionState,
};

/// Why a session is listed as stalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stall {
    /// It waits on a Needs-you row.
    NeedsYou,
    /// The owner sent its step back and nobody is on it.
    SentBack,
}

/// What the owner's screens say about one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion<'a> {
    /// At work on `step`, a step in progress it holds, since `since`: the
    /// send-back while the step is sent back, else the start of its ETA.
    Moving {
        step: &'a FeatureState,
        since: UtcTime,
    },
    /// Stalled for `why`: on a Needs-you row since the oldest such row
    /// (`step` is the session's current step, if any), or on a sent-back
    /// step since the send-back.
    Stalled {
        step: Option<&'a FeatureState>,
        since: UtcTime,
        why: Stall,
    },
    /// Not shown: nothing in progress that it holds, not live, or everything
    /// it has is with the owner for review.
    Neither,
}

/// A step delivered and waiting for the owner's review.
pub fn awaits_review(feature: &FeatureState) -> bool {
    feature.progress == Progress::Doing && feature.phase == Some(Phase::InReview)
}

/// A step the owner sent back that nobody is on: its agent is not live, or
/// its claim is no longer held. A step held because its agent waits on an
/// open question is under that question instead.
pub fn sent_back_unattended(feature: &FeatureState) -> bool {
    if feature.progress != Progress::Doing || feature.phase != Some(Phase::SentBack) {
        return false;
    }
    match &feature.claim {
        Some(claim) if claim.waiting_on.is_some() => false,
        Some(claim) => claim.state != Lease::Held || feature.liveness != Liveness::Live,
        None => feature.liveness != Liveness::Live,
    }
}

/// When the owner last sent back a delivery of the step's current attempt.
fn sent_back_at(project: &ProjectState, feature: &FeatureState) -> Option<UtcTime> {
    project
        .deliveries
        .values()
        .filter(|delivery| {
            delivery.node == feature.node && Some(&delivery.attempt) == feature.attempt.as_ref()
        })
        .filter_map(|delivery| delivery.sent_back.as_ref())
        .map(|note| note.at)
        .max()
}

fn holds(session: &SessionState, feature: &FeatureState) -> bool {
    feature.session.as_ref() == Some(&session.id)
}

/// A step in progress that `session` holds and is at work on.
fn at_work_on(session: &SessionState, feature: &FeatureState) -> bool {
    holds(session, feature)
        && feature.progress == Progress::Doing
        && !awaits_review(feature)
        && !sent_back_unattended(feature)
        && feature
            .claim
            .as_ref()
            .is_none_or(|claim| claim.state != Lease::Free)
}

/// Since when a session is moving on `step`.
fn moving_since(project: &ProjectState, session: &SessionState, step: &FeatureState) -> UtcTime {
    let sent_back = (step.phase == Some(Phase::SentBack))
        .then(|| sent_back_at(project, step))
        .flatten();
    sent_back
        .or_else(|| step.eta.as_ref().map(|eta| eta.anchor))
        .unwrap_or(session.last_event_ts)
}

/// Where `session` of `project` stands.
pub fn of_session<'a>(project: &'a ProjectState, session: &SessionState) -> Motion<'a> {
    let focus = session
        .current_node
        .as_ref()
        .and_then(|node| project.features.get(node));
    let parked = focus.filter(|feature| awaits_review(feature));
    // The review row of the parked step is the owner's turn, not a stall.
    let waiting_since = project
        .needs_you
        .iter()
        .filter(|need| session.stalled_on.contains(&need.id))
        .filter(|need| {
            !(need.kind == NeedsKind::Review
                && parked.is_some_and(|feature| need.node.as_ref() == Some(&feature.node)))
        })
        .map(|need| need.since)
        .min();
    if let Some(since) = waiting_since {
        return Motion::Stalled {
            step: focus,
            since,
            why: Stall::NeedsYou,
        };
    }
    if session.liveness == Liveness::Live {
        // Its current step, else the step at work it started last.
        let step = focus
            .filter(|feature| at_work_on(session, feature))
            .or_else(|| {
                project
                    .features
                    .values()
                    .filter(|feature| at_work_on(session, feature))
                    .max_by_key(|feature| feature.eta.as_ref().map(|eta| eta.anchor))
            });
        if let Some(step) = step {
            return Motion::Moving {
                step,
                since: moving_since(project, session, step),
            };
        }
    }
    // The sent-back step nobody is on, the one sent back last.
    let sent_back = project
        .features
        .values()
        .filter(|feature| holds(session, feature) && sent_back_unattended(feature))
        .map(|feature| (sent_back_at(project, feature), feature))
        .max_by_key(|(at, _)| *at);
    match sent_back {
        Some((at, step)) => Motion::Stalled {
            step: Some(step),
            // A send-back is an event with a time; the fallbacks never run.
            since: at
                .or_else(|| step.correction.as_ref().map(|correction| correction.since))
                .unwrap_or(session.last_event_ts),
            why: Stall::SentBack,
        },
        None => Motion::Neither,
    }
}

/// The header numbers of `project`, `(moving, stalled)`, by the same rule as
/// the lists: a session is counted where its row is, so a live session that
/// is stalled counts as stalled only. (The summary's `active_agents` and
/// `stalled_agents` still overlap; they are not these numbers.)
pub fn counts(project: &ProjectState) -> (u32, u32) {
    let (mut moving, mut stalled) = (0_u32, 0_u32);
    for session in project.sessions.values() {
        match of_session(project, session) {
            Motion::Moving { .. } => moving += 1,
            Motion::Stalled { .. } => stalled += 1,
            Motion::Neither => {}
        }
    }
    (moving, stalled)
}
