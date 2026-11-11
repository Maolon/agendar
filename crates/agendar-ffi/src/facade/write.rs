//! Contract v1 writes: `answer_checked` and `rescue`, and the `receipt` they
//! lead to. `review` (the verdict on a delivery) shares their refresh, their
//! one-at-a-time lock and their refusals from `review.rs`.
//!
//! Both writes refresh first, inside [`WRITE_REFRESH_BOUND`], check the
//! request against that read with the guard every host shares
//! (`agendar_core::guard`), and only then write through the service calls the
//! desktop uses. A request that does not pass is refused with a typed code and
//! nothing is written. A mobile client writes human events only.
#![forbid(unsafe_code)]

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use agendar_core::error::details;
use agendar_core::guard::{self, AnswerDenial, AnswerRequest, Refusal};
use agendar_core::index::ingest::SOURCE_WAITING;
use agendar_core::index::query::{Query, QueryResult, query};
use agendar_core::model::{
    DecisionLifecycle, DecisionState, Event, EventId, EventPayload, NeedsKind, OWN_OPTION,
    OptionId, Progress, ProjectSlug, ProjectState, ReceiptSource, SourcePath, WriterId, WriterRole,
};
use agendar_core::reduce::attention::is_active_need;
use agendar_core::reduce::ordering::need_key;
use agendar_core::service::Fresh;
use agendar_core::vault::{Clock, SystemClock};
use agendar_core::{AgendarError, ErrorCode};

use super::shared::{event_id, option_id, project_slug, revision};
use super::{Choice, FfiError, Receipt, ReceiptStep, RefusalCode, RescueAction, Session, Surface};

/// How long `answer_checked` and `rescue` wait for the refresh that precedes
/// their checks. Past it the request is refused with `refresh_timeout`.
pub const WRITE_REFRESH_BOUND: Duration = Duration::from_millis(2_000);

/// Row id prefix of the one rescue row a hand-off still resolves: two agents
/// on one step ("Who should continue?").
const CONTESTED_ROW: &str = "rescue:start-conflict:";

/// One checked write at a time in this process, so a repeated call sees the
/// first one's answer instead of racing it.
pub(super) static WRITES: Mutex<()> = Mutex::new(());

impl Session {
    /// Answer an ask after a bounded refresh and the shared rechecks:
    /// lifecycle, `expected_revision`, guard x surface, the choice, and the
    /// echoed label of the chosen option (on every guard, as on the desktop).
    ///
    /// A repeat of an answer this device already gave returns that answer's
    /// receipt and writes nothing.
    pub fn answer_checked(
        &self,
        ask_id: String,
        choice: Choice,
        expected_revision: String,
        surface: Surface,
        echo_label: String,
    ) -> Result<Receipt, FfiError> {
        let ask = event_id("/ask_id", &ask_id)?;
        let expected = revision(&expected_revision)?;
        let option = match &choice {
            Choice::Option { id } => Some(option_id("/choice", id)?),
            Choice::Own { .. } => None,
        };
        let _writes = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
        let fresh = self.write_refresh()?;
        if let Some(answer) = self.repeated_answer(&fresh, &ask, &choice) {
            return self.build_receipt(&fresh, &answer);
        }
        let request = AnswerRequest {
            ask: &ask,
            choice: match (&choice, &option) {
                (Choice::Own { text }, _) => guard::Choice::Own(text),
                (Choice::Option { .. }, Some(option)) => guard::Choice::Option(option),
                (Choice::Option { .. }, None) => unreachable!("an option choice was parsed"),
            },
            expected_revision: &expected,
            surface: match surface {
                Surface::App => guard::Surface::App,
                Surface::Notification => guard::Surface::Notification,
                Surface::Island => guard::Surface::Island,
            },
            echo_label: Some(&echo_label),
        };
        let checked = guard::check_answer(&fresh.state, &request).map_err(denied)?;
        self.refuse_while_waiting(&fresh, &checked.project)?;
        let own = OptionId::parse(OWN_OPTION)?;
        let written = self
            .service
            .answer_decision_event(
                &checked.project,
                &ask,
                option.as_ref().unwrap_or(&own),
                None,
                None,
                checked.own_answer,
            )
            .map_err(write_error)?;
        let after = self.service.refresh(SystemClock.now(), false)?;
        self.build_receipt(&after, &written.id)
    }

    /// Resolve a rescue row; returns the event id.
    ///
    /// `Pause` works on every rescue row. `Handoff` is accepted only on a
    /// contested step ("Who should continue?"); a free step is open to any
    /// agent, so there is nobody to hand it to. A repeat of an action that
    /// already holds returns the event that made it hold and writes nothing.
    pub fn rescue(
        &self,
        project: String,
        need_id: String,
        expected_revision: String,
        action: RescueAction,
    ) -> Result<String, FfiError> {
        let slug = project_slug("/project", &project)?;
        let expected = revision(&expected_revision)?;
        let _writes = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
        let fresh = self.write_refresh()?;
        let state = self.service.project(&fresh, &slug)?;
        let Some(need) = state.needs_you.iter().find(|need| need.id == need_id) else {
            // The row is gone. When this very action is why, say so with the
            // event that did it; otherwise the row moved on without us.
            let here = WriterId::new(self.service.host().clone(), WriterRole::Operator);
            return held_by(&fresh, state, &need_id, action, &here).ok_or_else(|| {
                FfiError::refused(
                    RefusalCode::StaleRevision,
                    "This rescue is no longer current.",
                )
            });
        };
        if need.kind != NeedsKind::Rescue {
            return Err(invalid(
                "/need_id",
                "not_a_rescue",
                "This row is not a rescue; answer it on its card.",
            ));
        }
        let revision = guard::need_revision(
            need,
            state.decisions.get(&need.source),
            &state.policy.revision,
        );
        if revision != expected {
            return Err(FfiError::refused(
                RefusalCode::StaleRevision,
                "This rescue changed after you opened it. Read it again.",
            ));
        }
        if action == RescueAction::Handoff && !need.id.starts_with(CONTESTED_ROW) {
            return Err(invalid(
                "/action",
                details::HANDOFF_NOT_OFFERED,
                "This step is open to any agent, so there is nobody to hand it to. Pause it, or \
                 let an agent pick it up.",
            ));
        }
        self.refuse_while_waiting(&fresh, &slug)?;
        let feature = need.node.as_ref().and_then(|node| state.features.get(node));
        let held = feature.and_then(|feature| match action {
            RescueAction::Pause if feature.paused => feature.field_sources.get("paused"),
            RescueAction::Handoff if feature.handoff_pending => feature.handoff_event.as_ref(),
            _ => None,
        });
        if let Some(event) = held {
            return Ok(event.as_str().to_owned());
        }
        let resolved = self.service.resolve_rescue(
            &slug,
            &need_id,
            match action {
                RescueAction::Handoff => agendar_core::model::RescueAction::Handoff,
                RescueAction::Pause => agendar_core::model::RescueAction::Pause,
            },
        )?;
        resolved["event_id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| {
                AgendarError::new(ErrorCode::Index, "The rescue returned no event id.").into()
            })
    }

    /// The receipt of one answer, with how far it got. A cached read: it sees
    /// what the last refresh or write of this session read.
    pub fn receipt(&self, answer_id: String) -> Result<Receipt, FfiError> {
        let answer = event_id("/answer_id", &answer_id)?;
        let fresh = self.service.refresh(SystemClock.now(), true)?;
        self.build_receipt(&fresh, &answer)
    }

    /// The refresh before a check. Only lock waits can be bounded: a refresh
    /// that is still waiting for another reader at the bound is a
    /// `refresh_timeout`; one that is ingesting runs to its end.
    pub(super) fn write_refresh(&self) -> Result<Fresh, FfiError> {
        let deadline = Instant::now() + WRITE_REFRESH_BOUND;
        self.service
            .refresh_until(SystemClock.now(), false, Some(deadline))
            .map_err(|error| match error.code() {
                ErrorCode::Busy | ErrorCode::Timeout => FfiError::refused(
                    RefusalCode::RefreshTimeout,
                    "Agendar could not read the Vault in time. Nothing was written; try again.",
                ),
                _ => error.into(),
            })
    }

    /// `not_synced` while a writer file of the project has not been
    /// downloaded: another host's answer or change may be in it.
    pub(super) fn refuse_while_waiting(
        &self,
        fresh: &Fresh,
        project: &ProjectSlug,
    ) -> Result<(), FfiError> {
        let QueryResult::Health { diagnostics, .. } =
            query(&fresh.index, Query::Health, fresh.now)?
        else {
            return Ok(());
        };
        let prefix = format!("projects/{}/", project.as_str());
        let waiting = diagnostics
            .iter()
            .filter(|note| note.code == SOURCE_WAITING && note.path.starts_with(&prefix))
            .count();
        if waiting == 0 {
            return Ok(());
        }
        Err(FfiError::refused(
            RefusalCode::NotSynced,
            format!(
                "{waiting} file(s) of this project have not downloaded yet, so another device's \
                 answer may be missing. Nothing was written; try again when the Vault has synced."
            ),
        ))
    }

    /// The answer this device already gave to `ask` with the same choice, if
    /// it is the one in effect.
    fn repeated_answer(&self, fresh: &Fresh, ask: &EventId, choice: &Choice) -> Option<EventId> {
        let (slug, decision) = guard::find_decision(&fresh.state, ask)?;
        if !matches!(
            decision.lifecycle,
            DecisionLifecycle::Answered | DecisionLifecycle::Acked | DecisionLifecycle::Amended
        ) {
            return None;
        }
        let effective = decision.effective_receipt.as_ref()?;
        let receipt = fresh.state.projects.get(slug)?.receipts.get(effective)?;
        let same = match choice {
            Choice::Option { id } => {
                receipt.own_answer.is_none() && receipt.chose.id.as_str() == id
            }
            Choice::Own { text } => receipt.own_answer.as_deref() == Some(text.trim()),
        };
        let here = WriterId::new(self.service.host().clone(), WriterRole::Operator);
        let ours = find_event(fresh, effective).is_some_and(|event| event.writer == here);
        (receipt.source == ReceiptSource::Human && same && ours).then(|| effective.clone())
    }

    fn build_receipt(&self, fresh: &Fresh, answer: &EventId) -> Result<Receipt, FfiError> {
        let missing = || {
            AgendarError::new(
                ErrorCode::NotFound,
                format!("Answer {} was not found.", answer.as_str()),
            )
            .with_detail("answer_not_found")
        };
        let (project, receipt) = fresh
            .state
            .projects
            .values()
            .find_map(|project| {
                project
                    .receipts
                    .get(answer)
                    .map(|receipt| (project, receipt))
            })
            .ok_or_else(missing)?;
        let event = find_event(fresh, answer).ok_or_else(missing)?;
        let decision = project.decisions.get(&receipt.ask);
        let effective = decision.is_some_and(|decision| {
            decision.effective_receipt.as_ref() == Some(answer)
                && !matches!(
                    decision.lifecycle,
                    DecisionLifecycle::Open | DecisionLifecycle::Withdrawn
                )
        });
        // The agent has the answer once it acknowledged it, or started work
        // that cites it.
        let acked_at = decision.and_then(|decision| {
            fresh
                .events
                .iter()
                .filter(|event| {
                    matches!(event.payload, EventPayload::DecisionAck(_))
                        && event.refs.answer.as_ref() == Some(answer)
                        && decision.ack_history.contains(&event.id)
                })
                .map(|event| event.ts)
                .min()
        });
        let applied =
            effective && decision.is_some_and(|decision| !decision.applied_sessions.is_empty());
        let agent_has_it = acked_at.is_some() || applied;
        let released = match decision {
            Some(decision) if effective && agent_has_it => released(project, decision),
            _ => Vec::new(),
        };
        Ok(Receipt {
            answer_id: answer.as_str().to_owned(),
            ask_id: receipt.ask.as_str().to_owned(),
            project: project.summary.slug.as_str().to_owned(),
            chose_label: receipt.chose.label.clone(),
            own_text: receipt.own_answer.clone(),
            written_at: event.ts.to_string(),
            writer_file: self.writer_file(fresh, event)?,
            step: if agent_has_it {
                ReceiptStep::AgentHasIt
            } else {
                ReceiptStep::Saved
            },
            acked_at: acked_at.map(|at| at.to_string()),
            released,
            next_need_id: next_need(fresh, &receipt.ask),
        })
    }

    /// The Vault-relative file `event` was appended to. A writer has one file
    /// until it seals a segment; only then are its files read to find the one.
    pub(super) fn writer_file(&self, fresh: &Fresh, event: &Event) -> Result<String, FfiError> {
        let QueryResult::Health { files, .. } = query(&fresh.index, Query::Health, fresh.now)?
        else {
            return Err(AgendarError::new(
                ErrorCode::Index,
                "The health query returned no health.",
            )
            .into());
        };
        let mut segments: Vec<SourcePath> = files
            .iter()
            .filter_map(|file| SourcePath::from_relative_path(&file.path).ok())
            .filter(|source| source.project == event.project && source.writer == event.writer)
            .collect();
        segments.sort_by_key(|source| std::cmp::Reverse(source.segment));
        let id = format!("\"{}\"", event.id.as_str());
        let holds = |source: &SourcePath| {
            std::fs::read_to_string(self.service.vault().root().join(&source.relative_path))
                .is_ok_and(|text| text.contains(&id))
        };
        let found = match segments.as_slice() {
            [only] => Some(only),
            many => many.iter().find(|source| holds(source)).or(many.first()),
        };
        match found {
            Some(source) => Ok(source.relative_path.clone()),
            None => Ok(agendar_core::vault::writer_relative_path(
                &event.project,
                &event.writer,
                1,
            )?),
        }
    }
}

/// The most urgent active need that is not about `settled`, to move on to.
pub(super) fn next_need(fresh: &Fresh, settled: &EventId) -> Option<String> {
    fresh
        .state
        .projects
        .values()
        .filter(|project| !project.summary.archived)
        .flat_map(|project| project.needs_you.iter())
        .filter(|need| is_active_need(need) && &need.source != settled)
        .min_by_key(|need| need_key(need))
        .map(|need| need.id.clone())
}

pub(super) fn find_event<'a>(fresh: &'a Fresh, id: &EventId) -> Option<&'a Event> {
    fresh.events.iter().find(|event| &event.id == id)
}

/// Titles of the steps an answer released: the question's own step and the
/// steps after it, without the ones already verified or dropped.
fn released(project: &ProjectState, decision: &DecisionState) -> Vec<String> {
    let Some(step) = project.features.get(&decision.node) else {
        return Vec::new();
    };
    std::iter::once(&decision.node)
        .chain(step.unblocks.iter())
        .filter_map(|node| project.features.get(node))
        .filter(|feature| !matches!(feature.progress, Progress::Verified | Progress::Dropped))
        .map(|feature| feature.title.clone())
        .collect()
}

/// The pause this device already wrote for a rescue row that is no longer
/// listed (a free step leaves the list once it is paused). The pause cites the
/// row's source event, and the row id carries that event's id.
fn held_by(
    fresh: &Fresh,
    state: &ProjectState,
    need_id: &str,
    action: RescueAction,
    here: &WriterId,
) -> Option<String> {
    if action != RescueAction::Pause {
        return None;
    }
    state
        .features
        .values()
        .filter(|feature| feature.paused)
        .filter_map(|feature| feature.field_sources.get("paused"))
        .filter_map(|pause| find_event(fresh, pause))
        .find(|pause| {
            &pause.writer == here
                && pause
                    .refs
                    .causes
                    .iter()
                    .any(|cause| need_id.contains(cause.as_str()))
        })
        .map(|pause| pause.id.as_str().to_owned())
}

fn invalid(pointer: &str, detail: &str, message: &str) -> FfiError {
    AgendarError::validation_at(pointer, detail, message).into()
}

/// What the shared check said, as the contract spells it: a typed refusal, or
/// `E_VALIDATION` for a request that was malformed for this card.
fn denied(denial: AnswerDenial) -> FfiError {
    let refusal = |code, message: &str| FfiError::refused(code, message);
    match denial {
        AnswerDenial::Refused { refusal: why, .. } => match why {
            Refusal::NotSynced => refusal(
                RefusalCode::NotSynced,
                "This device has not read this question yet. Try again when the Vault has synced.",
            ),
            Refusal::StaleRevision => refusal(
                RefusalCode::StaleRevision,
                "This card changed after you opened it. Read it again before answering.",
            ),
            Refusal::AlreadyAnswered => refusal(
                RefusalCode::AlreadyAnswered,
                "This question already has an answer.",
            ),
            Refusal::Withdrawn => refusal(RefusalCode::Withdrawn, WITHDRAWN),
            Refusal::SurfaceNotAllowed => refusal(
                RefusalCode::SurfaceNotAllowed,
                "Open Agendar to answer this question.",
            ),
        },
        AnswerDenial::UnknownOption => invalid(
            "/choice",
            "unknown_option",
            "The chosen option is not on this card.",
        ),
        AnswerDenial::OwnAnswerMissing => invalid(
            "/choice",
            "own_answer_missing",
            "An answer in your own words needs its text.",
        ),
        AnswerDenial::OwnAnswerTooLong => invalid(
            "/choice",
            details::OWN_ANSWER_TOO_LONG,
            "An answer in your own words is at most 600 characters.",
        ),
        AnswerDenial::EchoMismatch => invalid(
            "/echo_label",
            details::ECHO_MISMATCH,
            "The echoed label does not match the chosen option.",
        ),
        AnswerDenial::EchoMissing => invalid(
            "/echo_label",
            details::ECHO_MISSING,
            "Echo the chosen option's label to answer this card.",
        ),
    }
}

const WITHDRAWN: &str = "The agent withdrew this question or replaced it with a new one.";

/// A question withdrawn between the check and the write is the `withdrawn`
/// refusal, like one withdrawn before the check. Own words the core refuses
/// at the write (a NUL) are about `/choice`, as the checks before name it.
fn write_error(error: AgendarError) -> FfiError {
    match error.code() {
        ErrorCode::Withdrawn => FfiError::refused(RefusalCode::Withdrawn, WITHDRAWN),
        ErrorCode::Validation if error.field() == Some("/own_answer") => {
            error.with_field("/choice").into()
        }
        _ => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_withdrawn_during_the_write_is_the_withdrawn_refusal() {
        let withdrawn = AgendarError::new(ErrorCode::Withdrawn, "Decision was withdrawn.");
        assert_eq!(
            write_error(withdrawn),
            FfiError::refused(RefusalCode::Withdrawn, WITHDRAWN)
        );
        let other = write_error(AgendarError::new(ErrorCode::NotFound, "gone"));
        assert!(matches!(other, FfiError::Core { code, .. } if code == "E_NOT_FOUND"));
    }
}
