//! The checks one answer passes before any host writes it.
//!
//! [`check_answer`] reads reduced state and decides; the caller refreshes the
//! state first and writes the answer afterwards. The desktop maps a denial to
//! its own errors; a mobile client returns the [`Refusal`] code as it is.

use crate::model::{
    DecisionLifecycle, DecisionOption, DecisionState, EventId, OWN_ANSWER_MAX, OptionId,
    ProjectSlug, Reversibility, Sha256, State,
};

use super::{Guard, Surface, decision_revision, effective_risk, guard_for, surface_allows};

/// Why an answer is not written although the request is well formed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Refusal {
    /// This host has not read the question yet.
    NotSynced,
    /// The card, the policy or the receipt changed after the caller read it.
    StaleRevision,
    /// The question has an answer already (a human's, or its default).
    AlreadyAnswered,
    /// The agent withdrew the question or replaced it with a new one.
    Withdrawn,
    /// This surface may not answer a card with this guard.
    SurfaceNotAllowed,
}

impl Refusal {
    pub const ALL: [Self; 5] = [
        Self::NotSynced,
        Self::StaleRevision,
        Self::AlreadyAnswered,
        Self::Withdrawn,
        Self::SurfaceNotAllowed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotSynced => "not_synced",
            Self::StaleRevision => "stale_revision",
            Self::AlreadyAnswered => "already_answered",
            Self::Withdrawn => "withdrawn",
            Self::SurfaceNotAllowed => "surface_not_allowed",
        }
    }
}

/// What the human chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice<'a> {
    /// One of the card's options.
    Option(&'a OptionId),
    /// An answer in the human's own words.
    Own(&'a str),
}

/// One answer as a surface submits it.
#[derive(Debug, Clone, Copy)]
pub struct AnswerRequest<'a> {
    pub ask: &'a EventId,
    pub choice: Choice<'a>,
    /// The [`decision_revision`] of the card the human looked at.
    pub expected_revision: &'a Sha256,
    pub surface: Surface,
    /// The label of the chosen option as the surface showed it. A choose or
    /// confirm card needs it; an easy card may leave it out, and a label that
    /// is given must match on every card.
    pub echo_label: Option<&'a str>,
}

/// An answer that passed every check, ready to be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedAnswer {
    pub project: ProjectSlug,
    /// The chosen option; for own words, the card's reserved own option.
    pub chosen: DecisionOption,
    /// The trimmed text of an answer in the human's own words.
    pub own_answer: Option<String>,
    pub guard: Guard,
    pub risk: Reversibility,
    pub revision: Sha256,
}

/// Why [`check_answer`] did not pass an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerDenial {
    /// A typed refusal. `revision_current` says whether the expected revision
    /// still matched the card, which is false whenever the card is unknown.
    Refused {
        refusal: Refusal,
        revision_current: bool,
    },
    /// The option is not one of the current card's.
    UnknownOption,
    /// An answer in the human's own words has no text.
    OwnAnswerMissing,
    /// An answer in the human's own words is longer than [`OWN_ANSWER_MAX`].
    OwnAnswerTooLong,
    /// The echoed label is not the chosen option's label.
    EchoMismatch,
    /// A choose or confirm card was answered without an echoed label.
    EchoMissing,
}

impl AnswerDenial {
    /// The typed refusal, or `None` when the request itself was invalid.
    pub const fn refusal(&self) -> Option<Refusal> {
        match self {
            Self::Refused { refusal, .. } => Some(*refusal),
            _ => None,
        }
    }
}

/// The project and state of one question.
pub fn find_decision<'a>(
    state: &'a State,
    ask: &EventId,
) -> Option<(&'a ProjectSlug, &'a DecisionState)> {
    state
        .projects
        .iter()
        .find_map(|(slug, project)| project.decisions.get(ask).map(|decision| (slug, decision)))
}

/// Checks one answer against current state, in this order: the question is
/// known, still open, unchanged since the caller read it, answerable from this
/// surface, and the choice and its echoed label belong to the card.
///
/// A closed question is reported as answered or withdrawn even when its
/// revision moved too, because that is the more useful thing to tell the
/// human.
pub fn check_answer(
    state: &State,
    request: &AnswerRequest<'_>,
) -> Result<CheckedAnswer, AnswerDenial> {
    let refused = |refusal, revision_current| AnswerDenial::Refused {
        refusal,
        revision_current,
    };
    let (project, decision) =
        find_decision(state, request.ask).ok_or(refused(Refusal::NotSynced, false))?;
    let policy = &state.projects[project].policy.revision;
    let revision = decision_revision(project, decision, policy);
    let current = &revision == request.expected_revision;
    match decision.lifecycle {
        DecisionLifecycle::Open => {}
        DecisionLifecycle::Withdrawn => return Err(refused(Refusal::Withdrawn, current)),
        DecisionLifecycle::Answered
        | DecisionLifecycle::Acked
        | DecisionLifecycle::Defaulted
        | DecisionLifecycle::Amended => return Err(refused(Refusal::AlreadyAnswered, current)),
    }
    if !current {
        return Err(refused(Refusal::StaleRevision, false));
    }
    let guard = guard_for(&decision.card);
    let own = matches!(request.choice, Choice::Own(_));
    // Own words are written in the app, never on a button surface.
    if !surface_allows(guard, request.surface) || (own && request.surface != Surface::App) {
        return Err(refused(Refusal::SurfaceNotAllowed, true));
    }
    let (chosen, own_answer) = match request.choice {
        Choice::Option(option) => {
            let chosen = decision
                .card
                .options
                .iter()
                .find(|candidate| &candidate.id == option)
                .cloned()
                .ok_or(AnswerDenial::UnknownOption)?;
            (chosen, None)
        }
        Choice::Own(text) => {
            let text = text.trim();
            if text.is_empty() {
                return Err(AnswerDenial::OwnAnswerMissing);
            }
            if text.chars().count() > OWN_ANSWER_MAX {
                return Err(AnswerDenial::OwnAnswerTooLong);
            }
            (decision.card.own_option(text), Some(text.to_owned()))
        }
    };
    match request.echo_label {
        Some(echo) => {
            if echo.trim() != chosen.label.trim() {
                return Err(AnswerDenial::EchoMismatch);
            }
            if guard != Guard::Easy && echo.trim().is_empty() {
                return Err(AnswerDenial::EchoMissing);
            }
        }
        None => {
            if guard != Guard::Easy {
                return Err(AnswerDenial::EchoMissing);
            }
        }
    }
    Ok(CheckedAnswer {
        project: project.clone(),
        chosen,
        own_answer,
        guard,
        risk: effective_risk(&decision.card),
        revision,
    })
}
