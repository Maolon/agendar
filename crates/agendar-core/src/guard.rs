//! The answer guard every host shares: how much care one card needs, which
//! surface may answer it, the revision an answer is bound to, and what a
//! notice may offer.
//!
//! Everything here is pure: it reads reduced state and writes nothing. The
//! checks an answer must pass before it is written live in [`answer`], the
//! ones a review verdict must pass in [`review`].

mod answer;
mod review;

use serde_json::{Value, json};

use crate::model::{
    Confidence, DecisionCard, DecisionLifecycle, DecisionOption, DecisionState, NeedsKind,
    NeedsYouItem, OptionId, ProjectSlug, Reversibility, Sha256, canonical_json,
};
use crate::reduce::attention::is_active_need;
use crate::reduce::policy::risk_scan;

pub use answer::{
    AnswerDenial, AnswerRequest, CheckedAnswer, Choice, Refusal, check_answer, find_decision,
};
pub use review::{
    CheckedReview, ReviewDenial, check_review, current_receipt, delivery_revision, find_delivery,
};

/// How much care answering one card takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Guard {
    /// One tap or key on a labelled option.
    Easy,
    /// Pick an option, then press the answer button that names it.
    Choose,
    /// Pick an option, then confirm it deliberately.
    Confirm,
}

impl Guard {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Choose => "choose",
            Self::Confirm => "confirm",
        }
    }
}

/// Where an answer comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Surface {
    /// The app's own card.
    App,
    /// A button on a system notification.
    Notification,
    /// A button on the Dynamic Island or the Live Activity.
    Island,
}

impl Surface {
    pub const ALL: [Self; 3] = [Self::App, Self::Notification, Self::Island];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Notification => "notification",
            Self::Island => "island",
        }
    }
}

/// Whether a surface may answer a card with this guard. The app answers every
/// card; a notification or the island answers only easy cards, and offers
/// Open for the rest.
pub const fn surface_allows(guard: Guard, surface: Surface) -> bool {
    match surface {
        Surface::App => true,
        Surface::Notification | Surface::Island => matches!(guard, Guard::Easy),
    }
}

/// Worst-case effective risk of one card, including external effects and the
/// conservative heuristic floor.
pub fn effective_risk(card: &DecisionCard) -> Reversibility {
    let (risk, external, _) = risk_scan(card);
    if external {
        Reversibility::Irreversible
    } else {
        risk
    }
}

/// The guard enforced for one card.
///
/// Easy risk is only one-key when the recommendation is at least medium
/// confidence; an easy card with a low-confidence recommendation needs an
/// explicit choice, exactly like a costly card.
pub fn guard_for(card: &DecisionCard) -> Guard {
    let risk = effective_risk(card);
    if risk == Reversibility::Easy
        && matches!(
            card.recommended.confidence,
            Confidence::Med | Confidence::High
        )
    {
        Guard::Easy
    } else if risk == Reversibility::Irreversible {
        Guard::Confirm
    } else {
        Guard::Choose
    }
}

// ---------------------------------------------------------------------------
// Revisions
// ---------------------------------------------------------------------------

/// Lowercase SHA256 of compact canonical JSON.
pub fn revision(value: Value) -> Sha256 {
    Sha256::from_bytes(canonical_json(&value).as_bytes())
}

/// The revision an answer is bound to: card, policy and effective receipt.
pub fn decision_revision(
    project: &ProjectSlug,
    decision: &DecisionState,
    policy: &Sha256,
) -> Sha256 {
    let receipt = decision.effective_receipt.as_ref().map_or_else(
        || Value::String("absent".to_owned()),
        |id| Value::String(id.as_str().to_owned()),
    );
    let card = serde_json::to_value(&decision.card).unwrap_or(Value::Null);
    revision(json!([
        "decision",
        project.as_str(),
        decision.id.as_str(),
        card,
        policy,
        receipt
    ]))
}

/// A rescue/attention revision for the row a host displays.
pub fn need_revision(
    need: &NeedsYouItem,
    decision: Option<&DecisionState>,
    policy: &Sha256,
) -> Sha256 {
    match decision {
        Some(decision) => decision_revision(&need.project, decision, policy),
        None => revision(json!([
            "need",
            need.project.as_str(),
            need.id,
            need.kind.as_str(),
            need.source.as_str(),
            need.node.as_ref().map(|node| node.as_str()),
        ])),
    }
}

// ---------------------------------------------------------------------------
// Notice rules
// ---------------------------------------------------------------------------

/// Most answer buttons one notice offers next to Open.
pub const MAX_NOTICE_CHOICES: usize = 3;

/// Whether a question is still open with nothing answered, replaced or
/// withdrawn, and so may be answered from a notice at all.
fn open_and_unanswered(decision: &DecisionState) -> bool {
    decision.lifecycle == DecisionLifecycle::Open
        && decision.effective_receipt.is_none()
        && decision.superseded_by.is_none()
        && decision.withdrawn_by.is_none()
}

/// Easy, local and free of risky words: the only cards a notice answers.
fn plainly_easy(card: &DecisionCard) -> bool {
    let (risk, external, hits) = risk_scan(card);
    risk == Reversibility::Easy && !external && hits.is_empty()
}

/// The only approval a notification may offer: easy effective risk, current
/// recommendation with confidence med/high, still open and unanswered.
///
/// Returns the recommended option and its label.
pub fn approve_eligible(
    need: &NeedsYouItem,
    decision: Option<&DecisionState>,
) -> Option<(OptionId, String)> {
    if need.kind != NeedsKind::Decide {
        return None;
    }
    let decision = decision?;
    if !open_and_unanswered(decision) {
        return None;
    }
    if !matches!(
        decision.card.recommended.confidence,
        Confidence::Med | Confidence::High
    ) {
        return None;
    }
    if !plainly_easy(&decision.card) {
        return None;
    }
    let recommended = decision
        .card
        .options
        .iter()
        .find(|option| option.id == decision.card.recommended.option)?;
    Some((recommended.id.clone(), recommended.label.clone()))
}

/// Buttons a question's notice may offer: every option of an open, easy
/// (reversible, local, no risky words) question, recommended first, at most
/// [`MAX_NOTICE_CHOICES`]. Each button carries the option's own label, so
/// pressing one is an explicit choice. Costly and irreversible questions offer
/// Open only.
pub fn choice_options(
    need: &NeedsYouItem,
    decision: Option<&DecisionState>,
) -> Vec<(OptionId, String)> {
    if need.kind != NeedsKind::Decide {
        return Vec::new();
    }
    let Some(decision) = decision else {
        return Vec::new();
    };
    if !open_and_unanswered(decision) || !plainly_easy(&decision.card) {
        return Vec::new();
    }
    let recommended = &decision.card.recommended.option;
    let mut options: Vec<&DecisionOption> = decision.card.options.iter().collect();
    options.sort_by_key(|option| &option.id != recommended);
    options
        .into_iter()
        .take(MAX_NOTICE_CHOICES)
        .map(|option| (option.id.clone(), option.label.clone()))
        .collect()
}

/// Whether an item is announced at all: every open question (once per
/// revision), even while its agent still has other work, and every rescue row.
/// Confirm/review rows are in-app only.
pub fn notice_eligible(need: &NeedsYouItem) -> bool {
    matches!(need.kind, NeedsKind::Decide | NeedsKind::Rescue) && is_active_need(need)
}
