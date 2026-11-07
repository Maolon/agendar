//! Decision lifecycle, receipt creation and answer/ack resolution
//!.
//!
//! Answers are compared by causality first and only then by the documented
//! `(ts, writer, seq, id)` tuple, so an independent machine's later wall clock
//! never overwrites a causally later answer. Receipts are immutable snapshots of
//! the ask card at the moment the answer was authored.

use std::collections::BTreeMap;

use crate::model::cards::{Criterion, DecisionCard, DecisionOption, OnTimeout};
use crate::model::event::{
    ChoiceReason, DecisionAck, DecisionAnswer, DecisionWithdrawn, Event, EventPayload,
    PolicyEvaluation, PolicySet,
};
use crate::model::ids::{EventId, Mode, OptionId, SessionId, UtcTime, WriterId};
use crate::model::state::{DecisionLifecycle, DecisionState, Receipt, ReceiptSource, TimeRange};
use crate::reduce::plan::FeatureFold;
use crate::reduce::policy::risk_scan;
use crate::reduce::{CausalView, FoldNote};

/// One answer/assumed/auto choice event on an ask.
#[derive(Debug, Clone)]
pub struct AnswerFold {
    pub id: EventId,
    pub ts: UtcTime,
    pub writer: WriterId,
    pub seq: u64,
    pub option: OptionId,
    pub order: usize,
}

/// One acknowledgement event.
#[derive(Debug, Clone)]
pub struct AckFold {
    pub id: EventId,
    pub ask: EventId,
    pub answer: EventId,
    pub order: usize,
    pub session: SessionId,
}

/// Fold state of one ask.
#[derive(Debug, Clone)]
pub struct AskFold {
    pub id: EventId,
    pub node: crate::model::ids::NodePath,
    pub card: DecisionCard,
    pub mode: Mode,
    /// True when the ask's own causal policy evaluation required a human. An
    /// ask that was auto/assume from the start never forms a human-wait interval.
    pub awaiting_human: bool,
    /// The declared causal policy's timeout setting.
    pub allow_timeouts: bool,
    pub session: SessionId,
    pub ts: UtcTime,
    pub order: usize,
    pub default_at: Option<UtcTime>,
    /// First event in `O` that clears this ask's human gate.
    pub close: Option<(usize, UtcTime)>,
    pub answers: Vec<AnswerFold>,
    pub acks: Vec<EventId>,
    pub applied: Option<EventId>,
    pub assumed_at: Option<UtcTime>,
    pub withdrawn_by: Option<EventId>,
    pub superseded_by: Option<EventId>,
    pub reason: BTreeMap<EventId, ChoiceReason>,
}

impl AskFold {
    pub fn answer_order(&self) -> Vec<EventId> {
        let mut answers = self.answers.clone();
        answers.sort_by_key(|answer| answer.order);
        answers.into_iter().map(|answer| answer.id).collect()
    }

    pub fn is_unresolved(&self) -> bool {
        self.withdrawn_by.is_none()
            && self.superseded_by.is_none()
            && self.applied.is_none()
            && self.answers.is_empty()
    }

    /// Close the human gate at the first clearing event in `O`.
    pub fn close_gate(&mut self, order: usize, ts: UtcTime) {
        if self.close.is_none_or(|(current, _)| order < current) {
            self.close = Some((order, ts));
        }
    }

    /// The interval during which this ask awaited a human, if it ever did.
    pub fn human_wait(&self) -> Option<(UtcTime, Option<UtcTime>)> {
        if !self.awaiting_human {
            return None;
        }
        Some((self.ts, self.close.map(|(_, ts)| ts)))
    }
}

/// Create a receipt from a human answer.
pub fn receipt_from_answer(
    ask: &AskFold,
    event: &Event,
    body: &DecisionAnswer,
) -> std::result::Result<Receipt, String> {
    if body.option.as_str() == crate::model::cards::OWN_OPTION {
        let text = body
            .own_answer
            .as_deref()
            .ok_or_else(|| "an own answer needs its text".to_owned())?;
        let mut receipt = receipt_for(
            ask,
            event,
            ask.card.own_option(text),
            body.done_when.clone(),
            body.expected_eta,
            ReceiptSource::Human,
        );
        receipt.own_answer = Some(text.to_owned());
        return Ok(receipt);
    }
    let option = ask
        .card
        .options
        .iter()
        .find(|option| option.id == body.option)
        .cloned()
        .ok_or_else(|| "the chosen option is not part of the ask card".to_owned())?;
    Ok(receipt_for(
        ask,
        event,
        option,
        body.done_when.clone(),
        body.expected_eta,
        ReceiptSource::Human,
    ))
}

fn receipt_for(
    ask: &AskFold,
    event: &Event,
    chose: DecisionOption,
    done_when: Vec<Criterion>,
    expected_eta: Option<crate::model::ids::EtaRange>,
    source: ReceiptSource,
) -> Receipt {
    let ruled_out: Vec<DecisionOption> = ask
        .card
        .options
        .iter()
        .filter(|option| option.id != chose.id)
        .cloned()
        .collect();
    let expected_eta = expected_eta.or(chose.eta);
    let expected_finish = expected_eta.map(|eta| TimeRange {
        earliest: event.ts.add_seconds(i64::from(eta.min_s.get())),
        latest: event.ts.add_seconds(i64::from(eta.max_s.get())),
    });
    Receipt {
        id: event.id.clone(),
        ask: ask.id.clone(),
        node: ask.node.clone(),
        source,
        chose,
        ruled_out,
        done_when,
        expected_eta,
        created_at: event.ts,
        expected_finish,
        own_answer: None,
    }
}

/// Apply `decision.ask`.
pub fn apply_ask(
    asks: &mut BTreeMap<EventId, AskFold>,
    event: &Event,
    order: usize,
    evaluated: &PolicyEvaluation,
    policy: &PolicySet,
) -> Option<FoldNote> {
    let EventPayload::DecisionAsk(body) = &event.payload else {
        return None;
    };
    let declared = &body.policy;
    // Replicated revalidation against the causally observed policy snapshot.
    // Budget counters are advisory evidence of the writer's admission view
    // and are deliberately not compared.
    if declared.requested != evaluated.requested
        || declared.effective != evaluated.effective
        || declared.reversibility != evaluated.reversibility
        || declared.external != evaluated.external
        || declared.heuristic_hits != evaluated.heuristic_hits
        || declared.policy_event != evaluated.policy_event
    {
        return Some(FoldNote::invalid(
            "the declared policy evaluation does not match the causal policy snapshot",
        ));
    }
    // The hard floor holds for imported asks too: irreversible/external choices
    // and a policy that forbids timeouts must declare `hold`.
    let (risk, external, _) = risk_scan(&body.card);
    let hold_required = risk == crate::model::ids::Reversibility::Irreversible
        || external
        || !policy.allow_timeouts;
    if hold_required && !matches!(body.card.on_timeout, OnTimeout::Hold) {
        return Some(FoldNote::invalid(
            "an irreversible, external or timeout-free policy requires on_timeout=hold",
        ));
    }
    if let Some(supersedes) = &event.refs.supersedes
        && let Some(previous) = asks.get_mut(supersedes)
    {
        let answered = !previous.answers.is_empty();
        if previous.applied.is_some() || answered {
            // Answered/acked decisions are amended, never superseded. The local
            // service rejects this before writing; a replicated attempt is
            // rejected so it cannot rewrite history.
            return Some(FoldNote::invalid(
                "a decision with an answer or ack cannot be superseded",
            ));
        }
        if previous.withdrawn_by.is_none() {
            previous.withdrawn_by = Some(event.id.clone());
            previous.superseded_by = Some(event.id.clone());
            previous.close_gate(order, event.ts);
        }
    }
    let default_at = match body.card.on_timeout {
        OnTimeout::Hold => None,
        OnTimeout::ProceedAsAssumption { after_s } => {
            Some(event.ts.add_seconds(i64::from(after_s.get())))
        }
    };
    asks.insert(
        event.id.clone(),
        AskFold {
            id: event.id.clone(),
            node: event.refs.node.clone().unwrap_or_else(empty_node),
            card: body.card.clone(),
            mode: evaluated.effective,
            awaiting_human: evaluated.effective == Mode::Ask,
            allow_timeouts: policy.allow_timeouts,
            session: body.agent.session.clone(),
            ts: event.ts,
            order,
            default_at,
            close: None,
            answers: Vec::new(),
            acks: Vec::new(),
            applied: None,
            assumed_at: None,
            withdrawn_by: None,
            superseded_by: None,
            reason: BTreeMap::new(),
        },
    );
    None
}

fn empty_node() -> crate::model::ids::NodePath {
    crate::model::ids::NodePath::parse("unknown").expect("fixed node")
}

/// Apply a human answer.
pub fn apply_answer(
    asks: &mut BTreeMap<EventId, AskFold>,
    receipts: &mut BTreeMap<EventId, Receipt>,
    event: &Event,
    order: usize,
) -> Option<FoldNote> {
    let EventPayload::DecisionAnswer(body) = &event.payload else {
        return None;
    };
    let Some(ask_id) = event.refs.ask.clone() else {
        return Some(FoldNote::invalid("an answer requires an ask reference"));
    };
    let Some(ask) = asks.get_mut(&ask_id) else {
        return Some(FoldNote::invalid("the referenced ask is unknown"));
    };
    let receipt = match receipt_from_answer(ask, event, body) {
        Ok(receipt) => receipt,
        Err(detail) => return Some(FoldNote::invalid(detail)),
    };
    ask.answers.push(AnswerFold {
        id: event.id.clone(),
        ts: event.ts,
        writer: event.writer.clone(),
        seq: event.seq,
        option: body.option.clone(),
        order,
    });
    receipts.insert(event.id.clone(), receipt);
    // Any answer lifts the human gate at its own point in `O`, even when a
    // causally later answer supersedes it ("answer sent, awaiting ack").
    ask.close_gate(order, event.ts);
    if ask.withdrawn_by.is_some() || ask.superseded_by.is_some() {
        return Some(FoldNote::history(
            "the decision is withdrawn or superseded; the answer is history only",
        ));
    }
    None
}

/// Apply a `decision.assumed` or `decision.auto` choice.
///
/// Choices are replicated safety decisions, not admission checks: the declared
/// policy is revalidated against the ask's own card and causal snapshot, hard
/// risk/external floors hold, a timeout assumption must be legal and due on the
/// original asking session, and the attempt binding follows the author's active
/// causal attempt.
#[allow(clippy::too_many_arguments)]
pub fn apply_choice(
    asks: &mut BTreeMap<EventId, AskFold>,
    receipts: &mut BTreeMap<EventId, Receipt>,
    features: &mut BTreeMap<crate::model::ids::NodePath, FeatureFold>,
    assumed_by_attempt: &mut BTreeMap<EventId, std::collections::BTreeSet<EventId>>,
    event: &Event,
    order: usize,
    declared_policy: &PolicySet,
    policy_source: Option<EventId>,
    current_policy: &PolicySet,
    causal: &CausalView<'_>,
) -> Option<FoldNote> {
    let (body, source) = match &event.payload {
        EventPayload::DecisionAssumed(body) => (body, ReceiptSource::Assumed),
        EventPayload::DecisionAuto(body) => (body, ReceiptSource::Auto),
        _ => return None,
    };
    if source == ReceiptSource::Auto && body.reason == ChoiceReason::Timeout {
        return Some(FoldNote::invalid(
            "an auto choice cannot use the timeout reason",
        ));
    }
    let Some(ask_id) = event.refs.ask.clone() else {
        return Some(FoldNote::invalid("a choice requires an ask reference"));
    };
    let Some(ask) = asks.get_mut(&ask_id) else {
        return Some(FoldNote::invalid("the referenced ask is unknown"));
    };

    // 1. The declared evaluation must match the ask's card under the choice's
    //    own declared causal policy (never a concurrent policy).
    let expected = crate::reduce::policy::evaluate_with_policy(
        &ask.card,
        body.policy.requested,
        declared_policy,
        policy_source.clone(),
    );
    if body.policy.requested != expected.requested
        || body.policy.effective != expected.effective
        || body.policy.reversibility != expected.reversibility
        || body.policy.external != expected.external
        || body.policy.heuristic_hits != expected.heuristic_hits
        || body.policy.policy_event != expected.policy_event
    {
        return Some(FoldNote::invalid(
            "the declared policy evaluation does not match the causal policy snapshot",
        ));
    }

    // 2. The ask's original mode must permit this kind of choice. An ask the
    //    policy routed to `ask` can never be turned into an assumption by an
    //    imported packet; a policy assumption needs `assume`, an auto choice
    //    needs `auto`.
    match source {
        ReceiptSource::Auto => {
            if expected.effective != Mode::Auto || ask.mode != Mode::Auto {
                return Some(FoldNote::invalid(
                    "an auto choice requires an auto policy evaluation on the ask",
                ));
            }
        }
        ReceiptSource::Assumed if body.reason == ChoiceReason::Policy => {
            if expected.effective != Mode::Assume || ask.mode != Mode::Assume {
                return Some(FoldNote::invalid(
                    "a policy assumption requires an assume policy evaluation on the ask",
                ));
            }
        }
        ReceiptSource::Assumed => {}
        ReceiptSource::Human => {}
    }

    // 3. Hard floors and timeout legality.
    let (risk, external, _) = risk_scan(&ask.card);
    if risk == crate::model::ids::Reversibility::Irreversible || external {
        return Some(FoldNote::invalid(
            "an irreversible or external decision can never default",
        ));
    }
    if body.reason == ChoiceReason::Timeout {
        let OnTimeout::ProceedAsAssumption { after_s } = ask.card.on_timeout else {
            return Some(FoldNote::invalid(
                "a timeout assumption requires on_timeout=proceed_as_assumption",
            ));
        };
        let _ = after_s;
        if !ask.allow_timeouts || !current_policy.allow_timeouts || !declared_policy.allow_timeouts
        {
            return Some(FoldNote::invalid(
                "a timeout assumption requires allow_timeouts on the observed and current policy",
            ));
        }
        if body.agent.session != ask.session {
            return Some(FoldNote::invalid(
                "only the original asking session may author a timeout assumption",
            ));
        }
        let Some(deadline) = ask.default_at else {
            return Some(FoldNote::invalid(
                "a timeout assumption requires a timeout deadline",
            ));
        };
        if event.ts.millis() < deadline.millis() {
            return Some(FoldNote::invalid(
                "a timeout assumption before its deadline is premature",
            ));
        }
    }

    if ask.withdrawn_by.is_some() || ask.superseded_by.is_some() {
        ask.answers.push(AnswerFold {
            id: event.id.clone(),
            ts: event.ts,
            writer: event.writer.clone(),
            seq: event.seq,
            option: body.option.clone(),
            order,
        });
        return Some(FoldNote::history(
            "the decision is withdrawn; the choice is history only",
        ));
    }
    if body.option != ask.card.recommended.option {
        return Some(FoldNote::invalid(
            "an assumed or auto choice must equal the ask recommendation",
        ));
    }
    // Attempt binding: select the author's latest attempt in the causal view
    // first, then inspect whether it is still unfinished.
    // A finished/restarted historical attempt is never the active consumer.
    let author = &body.agent.session;
    let latest = features.get(&ask.node).and_then(|feature| {
        feature
            .attempts
            .values()
            .filter(|attempt| &attempt.session == author && causal.before(&attempt.id, &event.id))
            .max_by_key(|attempt| attempt.order)
            .map(|attempt| (attempt.id.clone(), attempt.status))
    });
    let active = latest
        .filter(|(_, status)| *status == crate::model::event::WorkStatus::Started)
        .map(|(id, _)| id);
    match (active, event.refs.attempt.clone()) {
        (Some(attempt), Some(referenced)) if attempt == referenced => {}
        (Some(_), _) => return Some(FoldNote::invalid("choice_attempt_required")),
        (None, Some(_)) => {
            return Some(FoldNote::invalid(
                "a choice without an active attempt must omit the attempt reference",
            ));
        }
        (None, None) => {}
    }
    let Some(option) = ask
        .card
        .options
        .iter()
        .find(|option| option.id == body.option)
        .cloned()
    else {
        return Some(FoldNote::invalid(
            "the chosen option is not part of the ask card",
        ));
    };
    let receipt = receipt_for(ask, event, option, ask.card.done_when.clone(), None, source);
    ask.answers.push(AnswerFold {
        id: event.id.clone(),
        ts: event.ts,
        writer: event.writer.clone(),
        seq: event.seq,
        option: body.option.clone(),
        order,
    });
    ask.close_gate(order, event.ts);
    ask.reason.insert(event.id.clone(), body.reason);
    if body.reason == ChoiceReason::Timeout
        || (ask.assumed_at.is_none() && source == ReceiptSource::Assumed)
    {
        ask.assumed_at = Some(event.ts);
    }
    ask.mode = body.policy.effective;
    if source == ReceiptSource::Assumed
        && let Some(attempt) = &event.refs.attempt
    {
        assumed_by_attempt
            .entry(attempt.clone())
            .or_default()
            .insert(event.id.clone());
    }
    receipts.insert(event.id.clone(), receipt);
    None
}

/// Apply `decision.withdrawn`.
pub fn apply_withdrawn(
    asks: &mut BTreeMap<EventId, AskFold>,
    event: &Event,
    order: usize,
    causal: &CausalView<'_>,
) -> Option<FoldNote> {
    let EventPayload::DecisionWithdrawn(DecisionWithdrawn { agent, .. }) = &event.payload else {
        return None;
    };
    let Some(ask_id) = event.refs.ask.clone() else {
        return Some(FoldNote::invalid("a withdrawal requires an ask reference"));
    };
    let Some(ask) = asks.get_mut(&ask_id) else {
        return Some(FoldNote::invalid("the referenced ask is unknown"));
    };
    if agent.session != ask.session {
        return Some(FoldNote::invalid(
            "only the original asking session may withdraw a decision",
        ));
    }
    if ask.acks.iter().any(|ack| causal.before(ack, &event.id)) {
        return Some(FoldNote::invalid(
            "a withdrawal after an observed ack is invalid",
        ));
    }
    if ask.withdrawn_by.is_none() {
        ask.withdrawn_by = Some(event.id.clone());
    }
    ask.close_gate(order, event.ts);
    None
}

/// Apply `decision.ack`.
pub fn apply_ack(
    asks: &mut BTreeMap<EventId, AskFold>,
    acks: &mut BTreeMap<EventId, AckFold>,
    receipts: &BTreeMap<EventId, Receipt>,
    event: &Event,
    order: usize,
) -> Option<FoldNote> {
    let EventPayload::DecisionAck(DecisionAck {
        agent,
        applied_option,
        ..
    }) = &event.payload
    else {
        return None;
    };
    let (Some(ask_id), Some(answer_id)) = (event.refs.ask.clone(), event.refs.answer.clone())
    else {
        return Some(FoldNote::invalid(
            "an ack requires ask and answer references",
        ));
    };
    let Some(ask) = asks.get_mut(&ask_id) else {
        return Some(FoldNote::invalid("the referenced ask is unknown"));
    };
    let Some(receipt) = receipts.get(&answer_id) else {
        return Some(FoldNote::invalid("the referenced answer has no receipt"));
    };
    // only a human answer or an auto receipt may be acknowledged;
    // an assumed receipt is never an answer ack.
    if receipt.source == ReceiptSource::Assumed {
        return Some(FoldNote::invalid(
            "an assumption receipt cannot be acknowledged",
        ));
    }
    if &receipt.chose.id != applied_option {
        return Some(FoldNote::invalid(
            "applied_option must equal the referenced answer choice",
        ));
    }
    if !ask.answers.iter().any(|answer| answer.id == answer_id) {
        return Some(FoldNote::invalid(
            "the referenced answer does not belong to the ask",
        ));
    }
    ask.acks.push(event.id.clone());
    acks.insert(
        event.id.clone(),
        AckFold {
            id: event.id.clone(),
            ask: ask_id,
            answer: answer_id,
            order,
            session: agent.session.clone(),
        },
    );
    None
}

/// Resolve effective receipts, applied answers and lifecycles.
pub fn finalize(
    asks: &mut BTreeMap<EventId, AskFold>,
    acks: &BTreeMap<EventId, AckFold>,
    receipts: &BTreeMap<EventId, Receipt>,
    causal: &CausalView<'_>,
) -> BTreeMap<EventId, DecisionState> {
    let mut states = BTreeMap::new();
    for (id, ask) in asks.iter_mut() {
        let mut answers: Vec<&AnswerFold> = ask.answers.iter().collect();
        answers.sort_by_key(|answer| answer.order);
        let human: Vec<&AnswerFold> = answers
            .iter()
            .copied()
            .filter(|answer| {
                receipts
                    .get(&answer.id)
                    .is_some_and(|receipt| receipt.source == ReceiptSource::Human)
            })
            .collect();
        let candidates: Vec<&AnswerFold> = if human.is_empty() {
            answers.clone()
        } else {
            human
        };
        let effective = if ask.withdrawn_by.is_some() || ask.superseded_by.is_some() {
            None
        } else {
            choose_effective(&candidates, causal).map(|answer| answer.id.clone())
        };
        let applied = ask
            .acks
            .iter()
            .filter_map(|ack| acks.get(ack))
            .max_by_key(|ack| ack.order)
            .map(|ack| ack.answer.clone());
        ask.applied = applied.clone();

        let mut lifecycle = DecisionLifecycle::Open;
        let mut pending_pivot = false;
        if ask.withdrawn_by.is_some() || ask.superseded_by.is_some() {
            lifecycle = DecisionLifecycle::Withdrawn;
        } else if let Some(effective) = &effective {
            let source = receipts.get(effective).map(|receipt| receipt.source);
            if source == Some(ReceiptSource::Assumed) {
                let reason = ask.reason.get(effective);
                lifecycle = if reason == Some(&ChoiceReason::Timeout) {
                    DecisionLifecycle::Defaulted
                } else {
                    DecisionLifecycle::Open
                };
            }
            if let Some(applied) = &applied {
                let applied_option = receipts
                    .get(applied)
                    .map(|receipt| receipt.chose.id.clone());
                let effective_option = receipts
                    .get(effective)
                    .map(|receipt| receipt.chose.id.clone());
                if applied_option == effective_option {
                    lifecycle = DecisionLifecycle::Acked;
                } else {
                    lifecycle = DecisionLifecycle::Amended;
                    pending_pivot = true;
                }
            } else if lifecycle == DecisionLifecycle::Open && source != Some(ReceiptSource::Assumed)
            {
                lifecycle = DecisionLifecycle::Answered;
            }
        }

        let answer_history: Vec<EventId> = answers.iter().map(|answer| answer.id.clone()).collect();
        let ack_history: Vec<EventId> = ask
            .acks
            .iter()
            .filter_map(|ack| acks.get(ack))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|ack| ack.id.clone())
            .collect();
        let mut ack_history = ack_history;
        ack_history.sort_by_key(|ack| acks.get(ack).map(|ack| ack.order).unwrap_or(0));

        states.insert(
            id.clone(),
            DecisionState {
                id: id.clone(),
                node: ask.node.clone(),
                card: ask.card.clone(),
                mode: ask.mode,
                lifecycle,
                effective_receipt: effective,
                applied_answer: applied,
                answer_history,
                ack_history,
                applied_sessions: Vec::new(),
                default_at: ask.default_at,
                assumed_at: ask.assumed_at,
                built_on: Vec::new(),
                pending_pivot,
                superseded_by: ask.superseded_by.clone(),
                withdrawn_by: ask.withdrawn_by.clone(),
            },
        );
    }
    states
}

/// Maximal candidates under causality, then the documented tuple.
fn choose_effective<'a>(
    candidates: &[&'a AnswerFold],
    causal: &CausalView<'_>,
) -> Option<&'a AnswerFold> {
    let maximal: Vec<&&AnswerFold> = candidates
        .iter()
        .filter(|candidate| {
            !candidates
                .iter()
                .any(|other| other.id != candidate.id && causal.before(&candidate.id, &other.id))
        })
        .collect();
    maximal
        .into_iter()
        .max_by(|left, right| {
            (
                left.ts.canonical(),
                left.writer.to_string(),
                left.seq,
                left.id.as_str(),
            )
                .cmp(&(
                    right.ts.canonical(),
                    right.writer.to_string(),
                    right.seq,
                    right.id.as_str(),
                ))
        })
        .copied()
}

/// Did `session` apply the receipt `receipt_event` through an ack or an
/// explicit start/progress reference on its own attempt?
pub fn session_applied(
    receipt_event: &EventId,
    ask: &EventId,
    session: &SessionId,
    acks: &BTreeMap<EventId, AckFold>,
    feature: Option<&FeatureFold>,
) -> bool {
    if acks
        .values()
        .any(|ack| &ack.session == session && &ack.ask == ask && &ack.answer == receipt_event)
    {
        return true;
    }
    feature.is_some_and(|feature| {
        feature
            .attempts
            .values()
            .any(|attempt| &attempt.session == session && attempt.applied.contains(receipt_event))
    })
}
