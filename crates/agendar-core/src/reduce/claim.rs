//! The claim lease on a step in progress, per attempt.
//!
//! A step that is doing (or sent back) is held by the session of its current
//! attempt while that session is not lost and either the ETA bound has not
//! passed or the attempt's last word is inside the live window. Once the ETA
//! passed and the attempt is quiet it is stalled (a label). It is free at
//! `max(ETA bound, last word) + T`, or at once when the host reports the
//! session lost. The last word is the latest event *on that attempt* by its
//! session, so a session busy on another step does not keep a forgotten one.
//!
//! While the session waits on an open question of its own to the owner, on
//! any step, the clock does not run for any of its steps, and it restarts from
//! the answer. A step waiting on a decision (an open question on it or on an
//! upstream step) is the owner's and never expires. Paused, handed-off and
//! in-review steps carry no claim: the owner holds them.
//!
//! Everything here is a function of the events and `as_of`; a time rule never
//! writes an event, so a wrong clock gives a wrong label on that host, never a
//! wrong event.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::ids::Mode;
use crate::model::ids::{EventId, SessionId, UtcTime};
use crate::model::state::{
    CLAIM_EXPIRY_S, ClaimState, Correction, CorrectionRule, DecisionLifecycle, DecisionState,
    EtaState, Lease,
};
use crate::reduce::decisions::AskFold;
use crate::reduce::plan::AttemptFold;

/// The live window: an attempt heard from this recently is held.
pub const LIVE_WINDOW_MS: i64 = 15 * 60 * 1000;

/// Whether `ask` holds its session's claims while unanswered: a question to
/// the owner (mode ask, needing a human), still open (not answered, assumed,
/// withdrawn or superseded).
///
/// A question folded "on closed steps" (`reduce::moot`) does not hold: nobody
/// is expected to answer it, so it must not keep a dead session's steps.
pub fn holds_session(ask: &AskFold, decision: &DecisionState, folded: bool) -> bool {
    !folded
        && ask.human_wait().is_some_and(|(_, end)| end.is_none())
        && decision.lifecycle == DecisionLifecycle::Open
        && decision.mode == Mode::Ask
}

/// Per-session question holds of one project.
#[derive(Debug, Default)]
pub struct SessionHolds {
    /// The earliest open question of each session that holds it.
    pub open: BTreeMap<SessionId, EventId>,
    /// When each session's latest question hold ended: the clock restarts
    /// from the answer.
    pub ended: BTreeMap<SessionId, UtcTime>,
}

impl SessionHolds {
    pub fn build(
        asks: &BTreeMap<EventId, AskFold>,
        decisions: &BTreeMap<EventId, DecisionState>,
        folded: &BTreeSet<EventId>,
    ) -> Self {
        let mut holds = Self::default();
        let mut by_order: Vec<&AskFold> = asks.values().collect();
        by_order.sort_by_key(|ask| ask.order);
        for ask in by_order {
            let Some((_, end)) = ask.human_wait() else {
                continue;
            };
            let Some(decision) = decisions.get(&ask.id) else {
                continue;
            };
            let folded = folded.contains(&ask.id);
            if holds_session(ask, decision, folded) {
                holds
                    .open
                    .entry(ask.session.clone())
                    .or_insert_with(|| ask.id.clone());
            } else if !folded
                && let Some(end) = end
                && decision.mode == Mode::Ask
            {
                let entry = holds.ended.entry(ask.session.clone()).or_insert(end);
                *entry = (*entry).max(end);
            }
        }
        holds
    }
}

/// What the claim rule needs besides the attempt.
pub struct ClaimInputs<'a> {
    /// The current attempt's session was reported lost, at this time.
    pub lost: Option<(&'a EventId, UtcTime)>,
    pub eta: Option<&'a EtaState>,
    /// An open question on the step or on an accepted upstream step.
    pub decision: Option<EventId>,
    pub holds: &'a SessionHolds,
    pub as_of: UtcTime,
}

/// The claim and, when the board no longer shows the agent's claim, the
/// correction it rests on.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub claim: ClaimState,
    pub correction: Option<Correction>,
}

/// The claim on a started attempt the agent still holds open.
pub fn claim(attempt: &AttemptFold, inputs: &ClaimInputs<'_>) -> Outcome {
    let (last_word, last_word_event) = attempt.last_word.clone();
    let held = |since: UtcTime, waiting_on: Option<EventId>| Outcome {
        claim: ClaimState {
            state: Lease::Held,
            since,
            last_word,
            waiting_on,
        },
        correction: None,
    };
    if let Some((lost_event, lost_at)) = inputs.lost {
        return Outcome {
            claim: ClaimState {
                state: Lease::Free,
                since: lost_at,
                last_word,
                waiting_on: None,
            },
            correction: Some(Correction {
                rule: CorrectionRule::Lost,
                since: lost_at,
                evidence: vec![lost_event.clone()],
            }),
        };
    }
    // The owner holds it: a decision on the step or upstream, or the session
    // waits on its own question elsewhere.
    if let Some(ask) = &inputs.decision {
        return held(last_word, Some(ask.clone()));
    }
    if let Some(ask) = inputs.holds.open.get(&attempt.session) {
        return held(last_word, Some(ask.clone()));
    }
    if inputs.eta.is_some_and(|eta| eta.paused) {
        return held(last_word, None);
    }
    // The clock restarts from the latest answer to this session's question.
    let word = inputs
        .holds
        .ended
        .get(&attempt.session)
        .map_or(last_word, |ended| last_word.max(*ended));
    let as_of = inputs.as_of.millis();
    let bound = inputs.eta.map(|eta| {
        eta.likely_finish
            .as_ref()
            .map_or(eta.agent_finish.latest, |range| range.latest)
    });
    let inside_eta = bound.is_none_or(|bound| as_of <= bound.millis());
    let live = as_of - word.millis() <= LIVE_WINDOW_MS;
    if inside_eta || live {
        return held(word, None);
    }
    let bound = bound.unwrap_or(word);
    let expiry = bound.max(word).add_seconds(CLAIM_EXPIRY_S);
    if as_of >= expiry.millis() {
        return Outcome {
            claim: ClaimState {
                state: Lease::Free,
                since: expiry,
                last_word,
                waiting_on: None,
            },
            correction: Some(Correction {
                rule: CorrectionRule::Silent,
                since: expiry,
                evidence: vec![last_word_event],
            }),
        };
    }
    Outcome {
        claim: ClaimState {
            state: Lease::Stalled,
            since: bound,
            last_word,
            waiting_on: None,
        },
        correction: None,
    }
}
