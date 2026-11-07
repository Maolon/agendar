//! Assumption consumption, confirmation and transitive taint
//!.
//!
//! One assumed receipt belongs to the ask it assumed and to the attempts that
//! consumed it: the choice's own attempt when the author had one, and any later
//! start that names the receipt in `refs.basis`. Consumption is a causal fact,
//! never a wall-clock or "current attempt" guess, so a concurrent attempt that
//! never cited the assumption stays firm.
//!
//! The human answer decides the fate of every consumed assumption:
//! matching the recommendation confirms it (basis only, progress untouched), a
//! different option taints it. A taint is sticky — an override that happened
//! after an attempt started survives a later answer that returns to the
//! recommendation — and only an explicit restart that cites the current
//! overriding answer repairs it.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::ids::{EventId, NodePath, SessionId};
use crate::model::state::{ProjectState, Receipt, ReceiptSource};

use super::decisions::AskFold;
use super::plan::AttemptFold;

/// When a confirm item appears: features built on the choice, or
/// the age of the assumption.
pub const CONFIRM_BUILT_ON: usize = 5;
/// Seconds after which an unconfirmed assumption is worth confirming.
pub const CONFIRM_AGE_S: i64 = 86_400;

/// How one attempt stands with the assumption receipts it consumed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Consumption {
    /// Consumed assumed receipts whose ask has no confirming human answer; a
    /// tainted receipt stays here because the choice is still unconfirmed.
    pub unconfirmed: Vec<EventId>,
    /// The asks (`d` in `tainted(d)`) whose assumption this attempt consumed and
    /// that the human overrode without this attempt repairing it.
    pub tainted: Vec<EventId>,
}

impl Consumption {
    pub fn is_firm(&self) -> bool {
        self.unconfirmed.is_empty()
    }
}

/// A human answer that matches the card recommendation confirms the assumption;
/// any other answer is an override.
pub fn is_confirmed(
    receipt: &EventId,
    decisions: &BTreeMap<EventId, crate::model::state::DecisionState>,
    receipts: &BTreeMap<EventId, Receipt>,
) -> bool {
    let Some(assumed) = receipts.get(receipt) else {
        return false;
    };
    if assumed.source != ReceiptSource::Assumed {
        return false;
    }
    let Some(decision) = decisions.get(&assumed.ask) else {
        return false;
    };
    let Some(effective) = &decision.effective_receipt else {
        return false;
    };
    let Some(chosen) = receipts.get(effective) else {
        return false;
    };
    chosen.source == ReceiptSource::Human && chosen.chose.id == decision.card.recommended.option
}

/// Assumed receipts of one attempt that no human answer has confirmed.
pub fn unconfirmed(
    assumptions: &[EventId],
    decisions: &BTreeMap<EventId, crate::model::state::DecisionState>,
    receipts: &BTreeMap<EventId, Receipt>,
) -> BTreeSet<EventId> {
    assumptions
        .iter()
        .filter(|receipt| !is_confirmed(receipt, decisions, receipts))
        .cloned()
        .collect()
}

/// Every assumption receipt one attempt consumed, through its own choice
/// binding or through an explicit causal citation.
fn consumed(
    attempt: &AttemptFold,
    consumed_choices: Option<&BTreeSet<EventId>>,
    receipts: &BTreeMap<EventId, Receipt>,
) -> BTreeSet<EventId> {
    let mut consumed: BTreeSet<EventId> = attempt
        .applied
        .iter()
        .filter(|id| {
            receipts
                .get(*id)
                .is_some_and(|receipt| receipt.source == ReceiptSource::Assumed)
        })
        .cloned()
        .collect();
    if let Some(extra) = consumed_choices {
        consumed.extend(extra.iter().cloned());
    }
    consumed
}

/// Fate of one consumed assumption receipt for one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// A human answer settled it (recommendation, or an override this attempt
    /// cited): the attempt is firm for this decision.
    Resolved,
    /// Still an open assumption.
    Assumed,
    /// The human chose differently and this attempt never repaired it.
    Tainted,
}

/// Classify one assumption receipt against the human answers on its ask.
fn classify(
    attempt: &AttemptFold,
    ask: &AskFold,
    effective: Option<&EventId>,
    receipts: &BTreeMap<EventId, Receipt>,
) -> Fate {
    let recommended = &ask.card.recommended.option;
    let is_override = |id: &EventId| {
        receipts.get(id).is_some_and(|receipt| {
            receipt.source == ReceiptSource::Human && &receipt.chose.id != recommended
        })
    };
    // Sticky taint: an override authored after this attempt started and not
    // cited by it. A later answer that returns to the recommendation never
    // erases a taint that was already incurred.
    let unapplied_override = ask.answers.iter().any(|answer| {
        is_override(&answer.id)
            && answer.order > attempt.order
            && !attempt.start_basis.contains(&answer.id)
    });
    if unapplied_override {
        return Fate::Tainted;
    }
    let Some(effective) = effective.and_then(|id| receipts.get(id)) else {
        return Fate::Assumed;
    };
    if effective.source != ReceiptSource::Human {
        return Fate::Assumed;
    }
    if &effective.chose.id == recommended {
        return Fate::Resolved;
    }
    // A restart that cites the current overriding answer in its basis is the
    // only repair; anything else stays tainted. A later
    // completion that mentions the answer is receipt application, not repair.
    if attempt.start_basis.contains(&effective.id) {
        Fate::Resolved
    } else {
        Fate::Tainted
    }
}

/// Per-receipt classification of one attempt's own consumption.
fn classify_attempt(
    attempt: &AttemptFold,
    consumed_choices: Option<&BTreeSet<EventId>>,
    asks: &BTreeMap<EventId, AskFold>,
    effective: &BTreeMap<EventId, Option<EventId>>,
    receipts: &BTreeMap<EventId, Receipt>,
) -> Vec<(EventId, EventId, Fate)> {
    let mut out = Vec::new();
    for assumption in consumed(attempt, consumed_choices, receipts) {
        let Some(receipt) = receipts.get(&assumption) else {
            continue;
        };
        let Some(ask) = asks.get(&receipt.ask) else {
            continue;
        };
        let choice = effective.get(&receipt.ask).and_then(Option::as_ref);
        out.push((
            assumption,
            receipt.ask.clone(),
            classify(attempt, ask, choice, receipts),
        ));
    }
    out
}

/// The chain of attempts this one replaces, oldest first.
fn lineage<'a>(
    current: &'a AttemptFold,
    feature: &'a super::plan::FeatureFold,
) -> Vec<&'a AttemptFold> {
    let mut chain: Vec<&AttemptFold> = Vec::new();
    let mut seen: BTreeSet<EventId> = BTreeSet::new();
    let mut cursor = Some(current.id.clone());
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) {
            break;
        }
        let Some(attempt) = feature.attempts.get(&id) else {
            break;
        };
        chain.push(attempt);
        cursor = attempt
            .causes
            .iter()
            .find(|cause| feature.attempts.contains_key(*cause))
            .cloned();
    }
    chain.reverse();
    chain
}

/// Did this attempt explicitly cite the current human answer of `ask`?
fn cites_current_answer(
    attempt: &AttemptFold,
    ask: &EventId,
    effective: &BTreeMap<EventId, Option<EventId>>,
    receipts: &BTreeMap<EventId, Receipt>,
) -> bool {
    let Some(receipt) = effective.get(ask).and_then(Option::as_ref) else {
        return false;
    };
    attempt.start_basis.contains(receipt)
        && receipts
            .get(receipt)
            .is_some_and(|receipt| receipt.source == ReceiptSource::Human)
}

/// Assumption standing of one feature's current attempt.
///
/// Taint is a fact about work already done: a restart inherits it from the
/// attempt it replaces until it explicitly cites the current human answer. That
/// is why a feature cannot shed rework by silently starting again.
/// Assumptions, by contrast, are only ever acquired by citation.
pub fn feature_consumption(
    feature: &super::plan::FeatureFold,
    consumed_choices: &BTreeMap<EventId, BTreeSet<EventId>>,
    asks: &BTreeMap<EventId, AskFold>,
    effective: &BTreeMap<EventId, Option<EventId>>,
    receipts: &BTreeMap<EventId, Receipt>,
) -> Consumption {
    let Some(current) = feature.current() else {
        return Consumption::default();
    };
    let chain = lineage(current, feature);
    let mut carried: BTreeMap<EventId, EventId> = BTreeMap::new();
    let mut own: Vec<(EventId, EventId, Fate)> = Vec::new();
    for attempt in chain {
        let pairs = classify_attempt(
            attempt,
            consumed_choices.get(&attempt.id),
            asks,
            effective,
            receipts,
        );
        // An explicit repair drops the carried taint for that decision.
        carried.retain(|ask, _| !cites_current_answer(attempt, ask, effective, receipts));
        for (receipt, ask, fate) in &pairs {
            if *fate == Fate::Tainted {
                carried.insert(ask.clone(), receipt.clone());
            }
        }
        if attempt.id == current.id {
            own = pairs;
        }
    }
    let mut out = Consumption::default();
    for (receipt, ask, fate) in own {
        match fate {
            Fate::Resolved => {}
            Fate::Assumed => out.unconfirmed.push(receipt),
            Fate::Tainted => {
                out.tainted.push(ask);
                out.unconfirmed.push(receipt);
            }
        }
    }
    for (ask, receipt) in carried {
        out.tainted.push(ask);
        out.unconfirmed.push(receipt);
    }
    out.unconfirmed.sort();
    out.unconfirmed.dedup();
    out.tainted.sort();
    out.tainted.dedup();
    out
}

/// Distinct features whose current attempt consumed an assumption receipt of
/// one ask, sorted lexically and counted once per feature (`built_on`). A
/// tainted feature still counts: it consumed the choice.
pub fn built_on(
    ask: &EventId,
    consumed: &BTreeMap<crate::model::ids::NodePath, Consumption>,
    receipts: &BTreeMap<EventId, Receipt>,
) -> Vec<crate::model::ids::NodePath> {
    let mut nodes: Vec<crate::model::ids::NodePath> = consumed
        .iter()
        .filter(|(_, consumption)| {
            consumption.unconfirmed.iter().any(|receipt| {
                receipts
                    .get(receipt)
                    .is_some_and(|receipt| &receipt.ask == ask)
            })
        })
        .map(|(node, _)| node.clone())
        .collect();
    nodes.sort();
    nodes
}

/// Did a human answer this ask with something other than the recommendation?
/// That is an override: every `assumed(d)` becomes `tainted(d)`.
pub fn is_override(
    project: &ProjectState,
    ask: &EventId,
    receipts: &BTreeMap<EventId, Receipt>,
) -> bool {
    let Some(decision) = project.decisions.get(ask) else {
        return false;
    };
    let Some(effective) = &decision.effective_receipt else {
        return false;
    };
    receipts.get(effective).is_some_and(|receipt| {
        receipt.source == ReceiptSource::Human
            && receipt.chose.id != decision.card.recommended.option
    })
}

/// Sorted redo set of one overridden decision: the features still tainted by it.
pub fn pivot_redo(project: &ProjectState, ask: &EventId) -> Vec<NodePath> {
    let mut redo: Vec<NodePath> = project
        .features
        .values()
        .filter(|feature| feature.taints.iter().any(|taint| taint == ask))
        .map(|feature| feature.node.clone())
        .collect();
    redo.sort();
    redo.dedup();
    redo
}

/// Every non-lost session that owns a feature still tainted by this override.
/// The pivot packet goes to each of them, never only to the first.
pub fn pivot_recipients(project: &ProjectState, ask: &EventId) -> Vec<SessionId> {
    let mut sessions: BTreeSet<SessionId> = BTreeSet::new();
    for node in pivot_redo(project, ask) {
        let Some(feature) = project.features.get(&node) else {
            continue;
        };
        let Some(session) = &feature.session else {
            continue;
        };
        let lost = project
            .sessions
            .get(session)
            .is_some_and(|state| state.liveness == crate::model::state::Liveness::Lost);
        if !lost {
            sessions.insert(session.clone());
        }
    }
    sessions.into_iter().collect()
}
