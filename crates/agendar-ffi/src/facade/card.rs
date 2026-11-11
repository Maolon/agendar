//! Contract v1: the decision card of one ask.
#![forbid(unsafe_code)]

use agendar_core::board::finished;
use agendar_core::guard::{
    Guard as CoreGuard, decision_revision, effective_risk, find_decision, guard_for,
};
use agendar_core::model::{
    Claim, Confidence as CoreConfidence, DecisionLifecycle, DecisionOption, DecisionState, Mode,
    NeedsYouItem, OWN_ANSWER_MAX, ProjectState, ReceiptSource, Reversibility,
};
use agendar_core::reduce::policy::risk_scan;
use agendar_core::service::Fresh;

use super::shared::{count, event_id, not_found, step_title};
use super::{
    AskMode, Card, CardFact, CardOption, CardStatus, Confidence, DoneCriterion, EtaWindow,
    FactClaim, FfiError, Guard, Risk, Session,
};

impl Session {
    /// The card of one ask, whatever its status, from the cached index.
    pub fn card(&self, ask_id: String) -> Result<Card, FfiError> {
        let ask = event_id("/ask_id", &ask_id)?;
        let fresh = self.cached()?;
        let Some((slug, decision)) = find_decision(&fresh.state, &ask) else {
            return Err(not_found("Decision", &ask_id));
        };
        let project = &fresh.state.projects[slug];
        let card = &decision.card;
        let need = project
            .needs_you
            .iter()
            .find(|need| need.source == decision.id);
        let (_, external, risk_words) = risk_scan(card);
        // As on the desktop: a default is shown only while the question is
        // open and the project lets questions time out; it is the
        // recommended option.
        let default_at = decision.default_at.filter(|_| {
            decision.lifecycle == DecisionLifecycle::Open && project.policy.value.allow_timeouts
        });
        let since = need.map(|need| need.since).or_else(|| {
            let asked = fresh.events.iter().find(|event| event.id == decision.id);
            asked.map(|event| event.ts)
        });
        Ok(Card {
            ask_id: decision.id.as_str().to_owned(),
            project: slug.as_str().to_owned(),
            project_title: project.summary.title.clone(),
            node: decision.node.as_str().to_owned(),
            step_title: step_title(project, &decision.node),
            revision: decision_revision(slug, decision, &project.policy.revision)
                .as_str()
                .to_owned(),
            guard: match guard_for(card) {
                CoreGuard::Easy => Guard::Easy,
                CoreGuard::Choose => Guard::Choose,
                CoreGuard::Confirm => Guard::Confirm,
            },
            risk: risk(effective_risk(card)),
            external,
            risk_words,
            mode: match decision.mode {
                Mode::Auto => AskMode::Auto,
                Mode::Assume => AskMode::Assume,
                Mode::Ask => AskMode::Ask,
            },
            question: card.question.clone(),
            why_now: card.why_now.clone(),
            status: self.status(&fresh, project, decision),
            options: card.options.iter().map(card_option).collect(),
            recommended_option: card.recommended.option.as_str().to_owned(),
            recommended_confidence: match card.recommended.confidence {
                CoreConfidence::Low => Confidence::Low,
                CoreConfidence::Med => Confidence::Med,
                CoreConfidence::High => Confidence::High,
            },
            recommended_because: card.recommended.because.clone(),
            own_max_chars: count(OWN_ANSWER_MAX),
            facts: card
                .already_checked
                .iter()
                .map(|fact| CardFact {
                    text: fact.text.clone(),
                    claim: match fact.claim {
                        Claim::AgentSays => FactClaim::AgentSays,
                        Claim::Seen => FactClaim::Seen,
                    },
                })
                .collect(),
            done_when: card
                .done_when
                .iter()
                .map(|criterion| DoneCriterion {
                    id: criterion.id.as_str().to_owned(),
                    text: criterion.text.clone(),
                })
                .collect(),
            unblocks: need.map_or_else(Vec::new, |need| released_titles(project, need)),
            stalled_agents: need.map_or(0, |need| need.stalled_agents),
            since: since.unwrap_or(fresh.state.evaluated_at).to_string(),
            default_at: default_at.as_ref().map(ToString::to_string),
            default_option: default_at.map(|_| card.recommended.option.as_str().to_owned()),
        })
    }

    /// What became of the ask. A replaced ask is `Superseded` (its lifecycle
    /// is withdrawn too). An ask whose effective choice an agent made on its
    /// own (timeout or policy) is `Defaulted`, also while its lifecycle is
    /// still open and the owner may confirm or override it. An ask whose
    /// effective answer a human wrote is answered here or elsewhere by the
    /// answer's writer host.
    fn status(
        &self,
        fresh: &Fresh,
        project: &ProjectState,
        decision: &DecisionState,
    ) -> CardStatus {
        if decision.superseded_by.is_some() {
            return CardStatus::Superseded;
        }
        let receipt = decision
            .effective_receipt
            .as_ref()
            .and_then(|id| project.receipts.get(id));
        match (decision.lifecycle, receipt) {
            (DecisionLifecycle::Withdrawn, _) => CardStatus::Withdrawn,
            // No effective choice yet.
            (_, None) => CardStatus::Open,
            (_, Some(receipt)) if receipt.source != ReceiptSource::Human => CardStatus::Defaulted {
                label: receipt.chose.label.clone(),
                at: receipt.created_at.to_string(),
            },
            (_, Some(receipt)) => {
                let label = receipt.chose.label.clone();
                let answer_id = receipt.id.as_str().to_owned();
                let answer = fresh.events.iter().find(|event| event.id == receipt.id);
                let host = answer.map(|event| event.writer.host());
                if host == Some(self.service.host()) {
                    CardStatus::AnsweredHere { label, answer_id }
                } else {
                    CardStatus::AnsweredElsewhere {
                        label,
                        host: host.map_or_else(String::new, |host| host.as_str().to_owned()),
                        answer_id,
                    }
                }
            }
        }
    }
}

const fn risk(reversibility: Reversibility) -> Risk {
    match reversibility {
        Reversibility::Easy => Risk::Easy,
        Reversibility::Costly => Risk::Costly,
        Reversibility::Irreversible => Risk::Irreversible,
    }
}

/// One option as declared: its own reversibility, with `external` beside it.
fn card_option(option: &DecisionOption) -> CardOption {
    CardOption {
        id: option.id.as_str().to_owned(),
        label: option.label.clone(),
        consequence: option.consequence.clone(),
        cost: option.cost.clone(),
        eta: option.eta.as_ref().map(|eta| EtaWindow {
            min_s: eta.min_s.get(),
            max_s: eta.max_s.get(),
        }),
        risk: risk(option.reversibility),
        external: option.external,
        has_image: option.image.is_some(),
    }
}

/// Titles of the unfinished steps a row holds up (the desktop's "holds").
fn released_titles(project: &ProjectState, need: &NeedsYouItem) -> Vec<String> {
    need.unblocks
        .iter()
        .filter_map(|node| project.features.get(node))
        .filter(|feature| !finished(feature.progress))
        .map(|feature| feature.title.clone())
        .collect()
}
