//! Event-set resolution and the canonical causal order.
//!
//! `resolve` owns everything the wire decoder deliberately does not: duplicate
//! collapse, writer/sequence forks, missing-reference pending state, reference
//! kind/project/node/session matching, cyclic-reference quarantine, sequence-gap
//! diagnostics and the Kahn topological order `O`. It performs no filesystem,
//! clock or database work.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::model::event::{Event, EventPayload, Refs, WorkStatus};
use crate::model::ids::{EventId, HostId, ProjectSlug, SessionId, Sha256};
use crate::model::state::{Diagnostic, canonical_json};

/// Resolution status of one raw `(id, digest)` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariantStatus {
    Valid,
    Pending,
    Quarantined,
}

impl VariantStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Pending => "pending",
            Self::Quarantined => "quarantined",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantState {
    pub status: VariantStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// Eligible events in canonical causal order `O`.
    pub ordered: Vec<Event>,
    /// `O` index for every eligible event id.
    pub order_index: BTreeMap<EventId, usize>,
    /// Transitive causal ancestors within the eligible set.
    pub ancestors: BTreeMap<EventId, BTreeSet<EventId>>,
    /// Status of every raw variant keyed by `(id, digest)`.
    pub variants: BTreeMap<(EventId, Sha256), VariantState>,
    pub diagnostics: Vec<Diagnostic>,
    pub event_set_digest: Sha256,
}

impl Resolution {
    /// True when `earlier` is a transitive cause of `later` inside the eligible
    /// set. Mere `O` order (a concurrent timestamp tie-break) is not causality.
    pub fn causally_before(&self, earlier: &EventId, later: &EventId) -> bool {
        self.ancestors
            .get(later)
            .is_some_and(|set| set.contains(earlier))
    }
}

/// Canonical payload digest of one decoded event: SHA-256 of its canonical JSON.
pub fn event_digest(event: &Event) -> Sha256 {
    let value = serde_json::to_value(event).expect("events always serialize");
    Sha256::from_bytes(canonical_json(&value).as_bytes())
}

/// Resolve one decoded event multiset into the eligible event set and `O`.
pub fn resolve(events: &[Event]) -> Resolution {
    resolve_with_quarantine(events, &BTreeMap::new())
}

/// Resolve with an extra replicated-semantic quarantine set.
///
/// The fold feeds events that failed replicated semantic validation back here so
/// they become ineligible and their causal descendants stay pending; the seed
/// set is empty on the first pass and only shrinks eligibility. A stream is one
/// `(project, writer)` pair: operators and stewards write independently in every
/// project.
pub fn resolve_with_quarantine(
    events: &[Event],
    quarantined: &BTreeMap<EventId, String>,
) -> Resolution {
    let mut diagnostics = Vec::new();

    // 1. Collapse exact duplicates; quarantine conflicting payloads per id.
    let mut by_id: BTreeMap<EventId, BTreeMap<Sha256, Event>> = BTreeMap::new();
    for event in events {
        let digest = event_digest(event);
        by_id
            .entry(event.id.clone())
            .or_default()
            .entry(digest)
            .or_insert_with(|| event.clone());
    }
    let mut variants: BTreeMap<(EventId, Sha256), VariantState> = BTreeMap::new();
    let mut candidates: BTreeMap<EventId, (Sha256, Event)> = BTreeMap::new();
    for (id, digests) in &by_id {
        if digests.len() > 1 {
            let project = digests.values().next().map(|event| event.project.clone());
            for digest in digests.keys() {
                variants.insert(
                    (id.clone(), digest.clone()),
                    VariantState {
                        status: VariantStatus::Quarantined,
                        reason: Some("duplicate_event_id".to_owned()),
                    },
                );
            }
            diagnostics.push(Diagnostic::new(
                format!("duplicate_event_id:{}", id.as_str()),
                "duplicate_event_id",
                project,
                Some(id.clone()),
                format!(
                    "Event {} was observed with {} different payloads; every variant is quarantined.",
                    id.as_str(),
                    digests.len()
                ),
            ));
        } else if let Some((digest, event)) = digests.iter().next() {
            candidates.insert(id.clone(), (digest.clone(), event.clone()));
            variants.insert(
                (id.clone(), digest.clone()),
                VariantState {
                    status: VariantStatus::Valid,
                    reason: None,
                },
            );
        }
    }

    // 1b. Replicated semantic quarantine carried in from the fold. These events
    //     are ineligible exactly like a structural defect, so their descendants
    //     stay pending instead of folding on top of a rejected fact.
    for (id, reason) in quarantined {
        let Some(digests) = by_id.get(id) else {
            continue;
        };
        for digest in digests.keys() {
            variants.insert(
                (id.clone(), digest.clone()),
                VariantState {
                    status: VariantStatus::Quarantined,
                    reason: Some("semantic_invalid".to_owned()),
                },
            );
        }
        let project = digests.values().next().map(|event| event.project.clone());
        diagnostics.push(Diagnostic::new(
            format!("semantic_invalid:{}", id.as_str()),
            "semantic_invalid",
            project,
            Some(id.clone()),
            format!(
                "Event {} was rejected by replicated semantic validation: {reason}.",
                id.as_str()
            ),
        ));
    }

    // 2. Stream/sequence forks: quarantine both sides and hold later events of
    //    that stream pending until an operator repairs it. A logical stream is
    //    `(project, writer)`; equal sequences in different projects are legal.
    let mut by_stream_seq: BTreeMap<(ProjectSlug, String, u64), BTreeSet<EventId>> =
        BTreeMap::new();
    for (id, (_, event)) in &candidates {
        by_stream_seq
            .entry((event.project.clone(), event.writer.to_string(), event.seq))
            .or_default()
            .insert(id.clone());
    }
    let mut forked_streams: BTreeMap<(ProjectSlug, String), BTreeSet<u64>> = BTreeMap::new();
    for ((project, writer, seq), ids) in &by_stream_seq {
        if ids.len() > 1 {
            forked_streams
                .entry((project.clone(), writer.clone()))
                .or_default()
                .insert(*seq);
            for id in ids {
                if let Some((digest, _)) = candidates.get(id) {
                    variants.insert(
                        (id.clone(), digest.clone()),
                        VariantState {
                            status: VariantStatus::Quarantined,
                            reason: Some("writer_seq_fork".to_owned()),
                        },
                    );
                }
            }
            diagnostics.push(Diagnostic::new(
                format!(
                    "writer_seq_fork:{}:{writer}:{seq}",
                    project.as_str()
                ),
                "writer_seq_fork",
                Some(project.clone()),
                None,
                format!(
                    "Writer {writer} has {} different events at seq {seq} in project {}; both sides are quarantined and later events of that stream stay pending.",
                    ids.len(),
                    project.as_str()
                ),
            ));
        }
    }
    for (id, (digest, event)) in &candidates {
        if variants
            .get(&(id.clone(), digest.clone()))
            .is_some_and(|state| state.status != VariantStatus::Valid)
        {
            continue;
        }
        if let Some(sequences) =
            forked_streams.get(&(event.project.clone(), event.writer.to_string()))
            && sequences.iter().any(|seq| event.seq > *seq)
        {
            variants.insert(
                (id.clone(), digest.clone()),
                VariantState {
                    status: VariantStatus::Pending,
                    reason: Some("writer_fork_pending".to_owned()),
                },
            );
        }
    }

    // 3. Reference presence, target kind/project/node/session matching.
    let lookup: BTreeMap<EventId, Event> = candidates
        .iter()
        .map(|(id, (_, event))| (id.clone(), event.clone()))
        .collect();
    // Host-attested continuations (previous -> continuing sessions): a
    // steward's `session.continued` whose two sessions both write from that
    // steward's host. A continuing session may reference the attempts of the
    // session it continues.
    let session_hosts: BTreeMap<SessionId, HostId> = candidates
        .values()
        .filter_map(|(_, event)| {
            event
                .writer
                .session()
                .map(|session| (session.clone(), event.writer.host().clone()))
        })
        .collect();
    let mut continued: BTreeMap<SessionId, BTreeSet<SessionId>> = BTreeMap::new();
    for (_, event) in candidates.values() {
        if let EventPayload::SessionContinued(body) = &event.payload
            && event.writer.is_steward()
            && session_hosts.get(&body.previous) == Some(event.writer.host())
            && session_hosts.get(&body.session) == Some(event.writer.host())
        {
            continued
                .entry(body.previous.clone())
                .or_default()
                .insert(body.session.clone());
        }
    }
    let quarantined_ids: BTreeSet<EventId> = variants
        .iter()
        .filter(|(_, state)| state.status == VariantStatus::Quarantined)
        .map(|((id, _), _)| id.clone())
        .collect();
    let mut invalid: BTreeMap<EventId, String> = BTreeMap::new();
    for (id, (digest, event)) in &candidates {
        if variants
            .get(&(id.clone(), digest.clone()))
            .is_some_and(|state| state.status != VariantStatus::Valid)
        {
            continue;
        }
        let references = references(event);
        let mut missing = references
            .iter()
            .any(|(target, _)| !lookup.contains_key(target) || quarantined_ids.contains(target));
        let mut reason: Option<String> = None;
        for (target, kind) in &references {
            let Some(found) = lookup.get(target) else {
                continue;
            };
            if quarantined_ids.contains(target) {
                continue;
            }
            // Every declared role is validated before any edge is deduplicated:
            // an observation/policy citation stays typed even when the same ID
            // also appears in `causes`.
            if let Err(detail) = check_reference(event, found, *kind, &lookup, &continued) {
                reason = Some(detail);
                break;
            }
        }
        // Both sessions of a continuation belong to the steward's own host:
        // only that computer can tell they are one agent conversation.
        if reason.is_none()
            && let EventPayload::SessionContinued(body) = &event.payload
        {
            for session in [&body.session, &body.previous] {
                let host = candidates.values().find_map(|(_, candidate)| {
                    (candidate.writer.session() == Some(session)).then(|| candidate.writer.host())
                });
                match host {
                    Some(host) if host == event.writer.host() => {}
                    Some(_) => {
                        reason = Some("a continued session belongs to another host".to_owned());
                        break;
                    }
                    None => {
                        missing = true;
                        break;
                    }
                }
            }
        }
        if reason.is_none()
            && let EventPayload::SessionLost(body) = &event.payload
        {
            let host = candidates.values().find_map(|(_, candidate)| {
                (candidate.writer.session() == Some(&body.session)).then(|| candidate.writer.host())
            });
            match host {
                Some(host) if host == event.writer.host() => {}
                Some(_) => reason = Some("the lost session belongs to another host".to_owned()),
                None => missing = true,
            }
        }
        if let Some(detail) = reason {
            invalid.insert(id.clone(), detail.clone());
            variants.insert(
                (id.clone(), digest.clone()),
                VariantState {
                    status: VariantStatus::Quarantined,
                    reason: Some("reference_mismatch".to_owned()),
                },
            );
            diagnostics.push(Diagnostic::new(
                format!("reference_mismatch:{}", id.as_str()),
                "reference_mismatch",
                Some(event.project.clone()),
                Some(id.clone()),
                format!("Event {} is quarantined: {detail}.", id.as_str()),
            ));
        } else if missing {
            variants.insert(
                (id.clone(), digest.clone()),
                VariantState {
                    status: VariantStatus::Pending,
                    reason: Some("missing_reference".to_owned()),
                },
            );
        }
    }

    // 4. Cyclic causal edges (references plus same-writer sequence edges)
    //    quarantine every member of the strongly connected component.
    let blocked_ids: BTreeSet<EventId> = variants
        .iter()
        .filter(|(_, state)| state.status != VariantStatus::Valid)
        .map(|((id, _), _)| id.clone())
        .collect();
    let live: BTreeSet<EventId> = candidates
        .keys()
        .filter(|id| !blocked_ids.contains(*id))
        .cloned()
        .collect();
    let graph = edge_graph(&live, &lookup);
    for component in strongly_connected(&live, &graph) {
        if component.len() < 2 {
            continue;
        }
        let project = component
            .iter()
            .find_map(|id| lookup.get(id).map(|event| event.project.clone()));
        let mut members = Vec::new();
        for id in &component {
            if let Some(event) = lookup.get(id) {
                variants.insert(
                    (id.clone(), event_digest(event)),
                    VariantState {
                        status: VariantStatus::Quarantined,
                        reason: Some("cyclic_reference".to_owned()),
                    },
                );
                members.push(id.as_str().to_owned());
            }
        }
        members.sort();
        let anchor = component
            .first()
            .map(|id| id.as_str().to_owned())
            .unwrap_or_default();
        diagnostics.push(Diagnostic::new(
            format!("cyclic_reference:{anchor}"),
            "cyclic_reference",
            project,
            component.first().cloned(),
            format!(
                "Events {} reference each other causally and are quarantined.",
                members.join(", ")
            ),
        ));
    }

    // 5. Propagate pending state to every event whose causally required target
    //    or available same-stream predecessor is not eligible. A later event of
    //    one stream must never skip a pending predecessor just because that
    //    predecessor was excluded from the sequence edges.
    loop {
        let mut changed = false;
        let blocked: BTreeSet<EventId> = variants
            .iter()
            .filter(|(_, state)| state.status != VariantStatus::Valid)
            .map(|((id, _), _)| id.clone())
            .collect();
        let mut streams: BTreeMap<(ProjectSlug, String), Vec<(u64, EventId)>> = BTreeMap::new();
        for (id, (digest, event)) in &candidates {
            if variants
                .get(&(id.clone(), digest.clone()))
                .is_some_and(|state| state.status == VariantStatus::Quarantined)
            {
                continue;
            }
            streams
                .entry((event.project.clone(), event.writer.to_string()))
                .or_default()
                .push((event.seq, id.clone()));
        }
        let mut predecessors: BTreeMap<EventId, EventId> = BTreeMap::new();
        for entries in streams.values_mut() {
            entries.sort();
            for window in entries.windows(2) {
                if let [(_, previous), (_, next)] = window {
                    predecessors.insert(next.clone(), previous.clone());
                }
            }
        }
        for (id, (digest, event)) in &candidates {
            let current = variants
                .get(&(id.clone(), digest.clone()))
                .map(|state| state.status)
                .unwrap_or(VariantStatus::Valid);
            if current != VariantStatus::Valid {
                continue;
            }
            let reference_blocked = references(event)
                .iter()
                .any(|(target, _)| blocked.contains(target));
            let predecessor_blocked = predecessors
                .get(id)
                .is_some_and(|previous| blocked.contains(previous));
            if reference_blocked || predecessor_blocked {
                changed = true;
                variants.insert(
                    (id.clone(), digest.clone()),
                    VariantState {
                        status: VariantStatus::Pending,
                        reason: Some(
                            if reference_blocked {
                                "pending_reference"
                            } else {
                                "pending_stream_predecessor"
                            }
                            .to_owned(),
                        ),
                    },
                );
            }
        }
        if !changed {
            break;
        }
    }

    // Sequence gaps are diagnostics, never blockers. A gap differs from an
    // available-but-ineligible predecessor, which is reported separately.
    let mut stream_sequences: BTreeMap<(ProjectSlug, String), BTreeMap<u64, EventId>> =
        BTreeMap::new();
    for (id, (digest, event)) in &candidates {
        if variants
            .get(&(id.clone(), digest.clone()))
            .is_some_and(|state| state.status == VariantStatus::Valid)
        {
            stream_sequences
                .entry((event.project.clone(), event.writer.to_string()))
                .or_default()
                .insert(event.seq, id.clone());
        }
    }
    for ((project, writer), sequences) in &stream_sequences {
        let numbers: Vec<u64> = sequences.keys().copied().collect();
        for window in numbers.windows(2) {
            if let [previous, next] = window
                && *next > previous + 1
            {
                diagnostics.push(Diagnostic::new(
                    format!(
                        "sequence_gap:{}:{writer}:{previous}:{next}",
                        project.as_str()
                    ),
                    "sequence_gap",
                    Some(project.clone()),
                    None,
                    format!(
                        "Writer {writer} jumps from seq {previous} to seq {next} in project {}; the missing sequence is only a diagnostic.",
                        project.as_str()
                    ),
                ));
            }
        }
    }

    // Missing-reference diagnostics (pending is not an error, but it must be
    // visible in convergent state).
    for ((id, _), state) in &variants {
        if state.status == VariantStatus::Pending
            && state.reason.as_deref() == Some("missing_reference")
        {
            let event = lookup.get(id);
            diagnostics.push(Diagnostic::new(
                format!("missing_reference:{}", id.as_str()),
                "missing_reference",
                event.map(|event| event.project.clone()),
                Some(id.clone()),
                format!(
                    "Event {} references a target that is not available yet; it stays pending.",
                    id.as_str()
                ),
            ));
        }
        if state.status == VariantStatus::Pending
            && state.reason.as_deref() == Some("pending_stream_predecessor")
        {
            let event = lookup.get(id);
            diagnostics.push(Diagnostic::new(
                format!("pending_stream_predecessor:{}", id.as_str()),
                "pending_stream_predecessor",
                event.map(|event| event.project.clone()),
                Some(id.clone()),
                format!(
                    "Event {} follows an available but ineligible sequence predecessor in its stream and stays pending.",
                    id.as_str()
                ),
            ));
        }
        if state.status == VariantStatus::Pending
            && state.reason.as_deref() == Some("writer_fork_pending")
        {
            let event = lookup.get(id);
            diagnostics.push(Diagnostic::new(
                format!("writer_fork_pending:{}", id.as_str()),
                "writer_fork_pending",
                event.map(|event| event.project.clone()),
                Some(id.clone()),
                format!(
                    "Event {} follows a quarantined writer/sequence fork and stays pending.",
                    id.as_str()
                ),
            ));
        }
    }

    // 6. Kahn topological order over the eligible set.
    let eligible: BTreeSet<EventId> = candidates
        .iter()
        .filter(|(id, (digest, _))| {
            variants
                .get(&((*id).clone(), (*digest).clone()))
                .is_some_and(|state| state.status == VariantStatus::Valid)
        })
        .map(|(id, _)| id.clone())
        .collect();
    let edges = edge_graph(&eligible, &lookup);
    let ordered = topological_order(&eligible, &lookup, &edges);

    let mut order_index = BTreeMap::new();
    let mut ancestors: BTreeMap<EventId, BTreeSet<EventId>> = BTreeMap::new();
    for (index, event) in ordered.iter().enumerate() {
        order_index.insert(event.id.clone(), index);
        let mut set = BTreeSet::new();
        if let Some(direct) = edges.get(&event.id) {
            for predecessor in direct {
                set.insert(predecessor.clone());
                if let Some(inherited) = ancestors.get(predecessor) {
                    set.extend(inherited.iter().cloned());
                }
            }
        }
        ancestors.insert(event.id.clone(), set);
    }

    // 7. Canonical event-set digest over validated/pending input variants.
    let mut digest_items: Vec<(EventId, Sha256)> = variants
        .iter()
        .filter(|(_, state)| matches!(state.status, VariantStatus::Valid | VariantStatus::Pending))
        .map(|((id, digest), _)| (id.clone(), digest.clone()))
        .collect();
    digest_items.sort();
    let payload = Value::Array(
        digest_items
            .iter()
            .map(|(id, digest)| {
                Value::Array(vec![
                    Value::String(id.as_str().to_owned()),
                    Value::String(digest.as_str().to_owned()),
                ])
            })
            .collect(),
    );

    diagnostics.sort_by(|left, right| left.id.cmp(&right.id));
    diagnostics.dedup_by(|left, right| left.id == right.id);
    let _ = &invalid;

    Resolution {
        ordered,
        order_index,
        ancestors,
        variants,
        diagnostics,
        event_set_digest: Sha256::from_bytes(canonical_json(&payload).as_bytes()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RefKind {
    Ask,
    Answer,
    Delivery,
    Receipt,
    Attempt,
    Supersedes,
    Cause,
    Basis,
    Observation,
    Policy,
}

/// Every declared reference role of one event, including body references.
/// Roles are never deduplicated by ID: the same target can be both a generic
/// cause and a typed citation, and the typed constraint still applies.
fn references(event: &Event) -> Vec<(EventId, RefKind)> {
    let mut refs = Vec::new();
    let Refs {
        ask,
        answer,
        delivery,
        receipt,
        attempt,
        supersedes,
        causes,
        basis,
        ..
    } = &event.refs;
    if let Some(id) = ask {
        refs.push((id.clone(), RefKind::Ask));
    }
    if let Some(id) = answer {
        refs.push((id.clone(), RefKind::Answer));
    }
    if let Some(id) = delivery {
        refs.push((id.clone(), RefKind::Delivery));
    }
    if let Some(id) = receipt {
        refs.push((id.clone(), RefKind::Receipt));
    }
    if let Some(id) = attempt {
        refs.push((id.clone(), RefKind::Attempt));
    }
    if let Some(id) = supersedes {
        refs.push((id.clone(), RefKind::Supersedes));
    }
    for id in causes {
        refs.push((id.clone(), RefKind::Cause));
    }
    for id in basis {
        refs.push((id.clone(), RefKind::Basis));
    }
    match &event.payload {
        EventPayload::DecisionAsk(body) => {
            if let Some(policy) = body.policy.policy_event.clone() {
                refs.push((policy, RefKind::Policy));
            }
            for fact in &body.card.already_checked {
                if let Some(observation) = fact.observation.clone() {
                    refs.push((observation, RefKind::Observation));
                }
            }
        }
        EventPayload::WorkProgress(body) => {
            for result in &body.evidence {
                if let Some(observation) = result.observation.clone() {
                    refs.push((observation, RefKind::Observation));
                }
            }
        }
        _ => {}
    }
    refs.sort_by(|left, right| (left.0.clone(), left.1).cmp(&(right.0.clone(), right.1)));
    refs.dedup();
    refs
}

/// Unique reference target IDs (graph edges do not care about the role).
fn reference_ids(event: &Event) -> BTreeSet<EventId> {
    references(event).into_iter().map(|(id, _)| id).collect()
}

/// Whether `session` is `owner` or continues it through attested continuations.
fn continues(
    session: &SessionId,
    owner: &SessionId,
    continued: &BTreeMap<SessionId, BTreeSet<SessionId>>,
) -> bool {
    let mut frontier = vec![owner];
    let mut seen: BTreeSet<&SessionId> = BTreeSet::new();
    while let Some(current) = frontier.pop() {
        if current == session {
            return true;
        }
        if !seen.insert(current) {
            continue;
        }
        if let Some(next) = continued.get(current) {
            frontier.extend(next.iter());
        }
    }
    false
}

fn check_reference(
    event: &Event,
    target: &Event,
    kind: RefKind,
    all: &BTreeMap<EventId, Event>,
    continued: &BTreeMap<SessionId, BTreeSet<SessionId>>,
) -> std::result::Result<(), String> {
    if target.project != event.project {
        return Err(format!(
            "a {} reference must target the same project",
            ref_kind_name(kind)
        ));
    }
    match kind {
        RefKind::Ask => {
            if !matches!(target.payload, EventPayload::DecisionAsk(_)) {
                return Err("an ask reference must target `decision.ask`".to_owned());
            }
            require_same_node(event, target, "ask")?;
        }
        RefKind::Answer => {
            if !matches!(
                target.payload,
                EventPayload::DecisionAnswer(_)
                    | EventPayload::DecisionAuto(_)
                    | EventPayload::DecisionAssumed(_)
            ) {
                return Err(
                    "an answer reference must target a decision answer, auto or assumed event"
                        .to_owned(),
                );
            }
            require_same_node(event, target, "answer")?;
            if target.refs.ask != event.refs.ask {
                return Err("an answer reference must belong to the referenced ask".to_owned());
            }
        }
        RefKind::Delivery => {
            if !matches!(target.payload, EventPayload::Delivery(_)) {
                return Err("a delivery reference must target `delivery`".to_owned());
            }
        }
        RefKind::Receipt => {
            if !matches!(
                target.payload,
                EventPayload::DecisionAnswer(_)
                    | EventPayload::DecisionAuto(_)
                    | EventPayload::DecisionAssumed(_)
            ) {
                return Err(
                    "a receipt reference must target a decision answer, auto or assumed event"
                        .to_owned(),
                );
            }
            require_same_node(event, target, "receipt")?;
        }
        RefKind::Attempt => {
            match &target.payload {
                EventPayload::WorkProgress(body) if body.status == WorkStatus::Started => {}
                _ => {
                    return Err(
                        "an attempt reference must target `work.progress` status `started`"
                            .to_owned(),
                    );
                }
            }
            require_same_node(event, target, "attempt")?;
            let target_session = target.payload.agent().map(|agent| &agent.session);
            let event_session = event.payload.agent().map(|agent| &agent.session);
            if let (Some(owner), Some(author)) = (target_session, event_session)
                && !continues(author, owner, continued)
            {
                return Err("an attempt reference must belong to the authoring session".to_owned());
            }
        }
        RefKind::Supersedes => match &event.payload {
            EventPayload::DecisionAsk(_) => {
                if !matches!(target.payload, EventPayload::DecisionAsk(_)) {
                    return Err("an ask can only supersede another `decision.ask`".to_owned());
                }
            }
            EventPayload::Delivery(_) => {
                if !matches!(target.payload, EventPayload::Delivery(_)) {
                    return Err("a delivery can only supersede another `delivery`".to_owned());
                }
            }
            _ => return Err("supersedes is only allowed on ask or delivery".to_owned()),
        },
        RefKind::Cause => {}
        RefKind::Basis => {
            if !matches!(
                target.payload,
                EventPayload::DecisionAssumed(_)
                    | EventPayload::DecisionAnswer(_)
                    | EventPayload::DecisionAuto(_)
            ) {
                return Err("a basis reference must target a decision receipt event".to_owned());
            }
        }
        RefKind::Observation => {
            let EventPayload::EvidenceObserved(observed) = &target.payload else {
                return Err("an observation reference must target `evidence.observed`".to_owned());
            };
            match &event.payload {
                EventPayload::WorkProgress(body) => {
                    for result in &body.evidence {
                        if result.observation.as_ref() != Some(&target.id) {
                            continue;
                        }
                        // Only a passed result can ever be evidence; a failed or
                        // not-proven observation must never verify work
                        //.
                        if observed.result != crate::model::cards::CheckResult::Passed {
                            return Err(
                                "work progress evidence must cite a passed observation".to_owned()
                            );
                        }
                        if observed.criterion != result.criterion {
                            return Err(
                                "a cited observation must prove the same criterion".to_owned()
                            );
                        }
                        if let Some(blob) = &result.evidence
                            && blob.sha256 != observed.digest
                        {
                            return Err(
                                "cited observation evidence must match the observation digest"
                                    .to_owned(),
                            );
                        }
                        // The observation must be bound to the delivery of the
                        // same node and attempt. The delivery's required receipt
                        // reference supplies the receipt binding available before
                        // the delivery projection.
                        let Some(delivery_id) = &target.refs.delivery else {
                            return Err("a cited observation must belong to a delivery".to_owned());
                        };
                        let Some(delivery) = all.get(delivery_id) else {
                            return Err("a cited observation's delivery is not in the causal view"
                                .to_owned());
                        };
                        if delivery.refs.node != event.refs.node {
                            return Err(
                                "a cited observation must belong to the same node".to_owned()
                            );
                        }
                        if delivery.refs.attempt != event.refs.attempt {
                            return Err(
                                "a cited observation must belong to the referenced attempt"
                                    .to_owned(),
                            );
                        }
                    }
                }
                EventPayload::DecisionAsk(body) => {
                    for fact in &body.card.already_checked {
                        if fact.observation.as_ref() != Some(&target.id) {
                            continue;
                        }
                        if observed.result != crate::model::cards::CheckResult::Passed {
                            return Err("a Seen fact must cite a passed observation".to_owned());
                        }
                    }
                }
                _ => {}
            }
        }
        RefKind::Policy => {
            if !matches!(target.payload, EventPayload::PolicySet(_)) {
                return Err("a policy reference must target `policy.set`".to_owned());
            }
        }
    }
    Ok(())
}

fn ref_kind_name(kind: RefKind) -> &'static str {
    match kind {
        RefKind::Ask => "ask",
        RefKind::Answer => "answer",
        RefKind::Delivery => "delivery",
        RefKind::Receipt => "receipt",
        RefKind::Attempt => "attempt",
        RefKind::Supersedes => "supersedes",
        RefKind::Cause => "causes",
        RefKind::Basis => "basis",
        RefKind::Observation => "observation",
        RefKind::Policy => "policy",
    }
}

fn require_same_node(event: &Event, target: &Event, what: &str) -> std::result::Result<(), String> {
    if event.refs.node != target.refs.node {
        return Err(format!("a {what} reference must target the same node"));
    }
    Ok(())
}

/// Direct predecessors per event: reference targets plus same-writer sequence.
fn edge_graph(
    eligible: &BTreeSet<EventId>,
    lookup: &BTreeMap<EventId, Event>,
) -> BTreeMap<EventId, BTreeSet<EventId>> {
    let mut edges: BTreeMap<EventId, BTreeSet<EventId>> = BTreeMap::new();
    for id in eligible {
        edges.insert(id.clone(), BTreeSet::new());
    }
    for id in eligible {
        let Some(event) = lookup.get(id) else {
            continue;
        };
        for target in reference_ids(event) {
            if eligible.contains(&target) && &target != id {
                edges.get_mut(id).expect("known event").insert(target);
            }
        }
    }
    let mut streams: BTreeMap<(ProjectSlug, String), Vec<(u64, EventId)>> = BTreeMap::new();
    for id in eligible {
        if let Some(event) = lookup.get(id) {
            streams
                .entry((event.project.clone(), event.writer.to_string()))
                .or_default()
                .push((event.seq, id.clone()));
        }
    }
    for sequences in streams.values_mut() {
        sequences.sort();
        for window in sequences.windows(2) {
            if let [(_, previous), (_, next)] = window {
                edges
                    .get_mut(next)
                    .expect("known event")
                    .insert(previous.clone());
            }
        }
    }
    edges
}

fn topological_order(
    eligible: &BTreeSet<EventId>,
    lookup: &BTreeMap<EventId, Event>,
    edges: &BTreeMap<EventId, BTreeSet<EventId>>,
) -> Vec<Event> {
    let mut indegree: BTreeMap<EventId, usize> =
        eligible.iter().map(|id| (id.clone(), 0_usize)).collect();
    let mut successors: BTreeMap<EventId, BTreeSet<EventId>> = BTreeMap::new();
    for (event, predecessors) in edges {
        indegree.insert(event.clone(), predecessors.len());
        for predecessor in predecessors {
            successors
                .entry(predecessor.clone())
                .or_default()
                .insert(event.clone());
        }
    }
    let mut ready: BTreeSet<(String, String, u64, String, EventId)> = BTreeSet::new();
    for (id, degree) in &indegree {
        if *degree == 0
            && let Some(event) = lookup.get(id)
        {
            ready.insert(ready_key(event));
        }
    }
    let mut ordered = Vec::with_capacity(eligible.len());
    while let Some(key) = ready.iter().next().cloned() {
        ready.remove(&key);
        let id = key.4;
        if let Some(event) = lookup.get(&id) {
            ordered.push(event.clone());
        }
        if let Some(next) = successors.get(&id) {
            for successor in next {
                if let Some(degree) = indegree.get_mut(successor) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0
                        && let Some(event) = lookup.get(successor)
                    {
                        ready.insert(ready_key(event));
                    }
                }
            }
        }
    }
    ordered
}

fn ready_key(event: &Event) -> (String, String, u64, String, EventId) {
    (
        event.ts.canonical(),
        event.writer.to_string(),
        event.seq,
        event.id.as_str().to_owned(),
        event.id.clone(),
    )
}

fn strongly_connected(
    nodes: &BTreeSet<EventId>,
    edges: &BTreeMap<EventId, BTreeSet<EventId>>,
) -> Vec<Vec<EventId>> {
    // Iterative Tarjan.
    struct Frame {
        node: EventId,
        next: usize,
        children: Vec<EventId>,
    }
    let mut index_of: BTreeMap<EventId, usize> = BTreeMap::new();
    let mut lowlink: BTreeMap<EventId, usize> = BTreeMap::new();
    let mut on_stack: BTreeSet<EventId> = BTreeSet::new();
    let mut stack: Vec<EventId> = Vec::new();
    let mut counter = 0_usize;
    let mut components = Vec::new();

    for root in nodes {
        if index_of.contains_key(root) {
            continue;
        }
        index_of.insert(root.clone(), counter);
        lowlink.insert(root.clone(), counter);
        counter += 1;
        stack.push(root.clone());
        on_stack.insert(root.clone());
        let mut work: Vec<Frame> = vec![Frame {
            node: root.clone(),
            next: 0,
            children: edges
                .get(root)
                .map(|values| values.iter().cloned().collect())
                .unwrap_or_default(),
        }];

        while let Some(frame) = work.last_mut() {
            if frame.next < frame.children.len() {
                let child = frame.children[frame.next].clone();
                frame.next += 1;
                if !index_of.contains_key(&child) {
                    index_of.insert(child.clone(), counter);
                    lowlink.insert(child.clone(), counter);
                    counter += 1;
                    stack.push(child.clone());
                    on_stack.insert(child.clone());
                    let children = edges
                        .get(&child)
                        .map(|values| values.iter().cloned().collect())
                        .unwrap_or_default();
                    work.push(Frame {
                        node: child,
                        next: 0,
                        children,
                    });
                } else if on_stack.contains(&child) {
                    let child_index = index_of[&child];
                    if let Some(frame) = work.last_mut() {
                        let entry = lowlink.get_mut(&frame.node).expect("known node");
                        *entry = (*entry).min(child_index);
                    }
                }
            } else {
                let node = frame.node.clone();
                let low = lowlink[&node];
                if low == index_of[&node] {
                    let mut component = Vec::new();
                    while let Some(top) = stack.pop() {
                        on_stack.remove(&top);
                        component.push(top.clone());
                        if top == node {
                            break;
                        }
                    }
                    if component.len() > 1 {
                        component.sort();
                        components.push(component);
                    }
                }
                work.pop();
                if let Some(parent) = work.last_mut() {
                    let entry = lowlink.get_mut(&parent.node).expect("known node");
                    *entry = (*entry).min(low);
                }
            }
        }
    }
    components
}
