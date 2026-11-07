//! Host-local steward: session registration and bounded pid liveness.
//!
//! One steward per `(Vault, host)` owns `steward.lock`. A tick checks only the
//! registered harness pids; it never samples CPU/memory, scrapes transcripts or
//! controls an agent process. A missing pid or a changed process start identity
//! produces exactly one `session.lost` fact and seals that session writer —
//! except for a Codex thread, which outlives the process of each turn: that
//! registration becomes `resumable` (quiet) instead of lost.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::adapters::{Adapter, AdapterCapabilities, CodexAdapter, TransportResult};
use crate::error::{AgendarError, ErrorCode, Result};
use crate::model::{
    Agent, DeliveredOutcome, EventPayload, Harness, InboxPacket, LostReason, PacketKind,
    ProjectSlug, ProjectState, Pulse, Refs, SessionContinued, SessionId, SessionLost, UtcTime,
    WriterId, WriterRole,
};
use crate::runtime::acquire_lock;
use crate::runtime::sessions::{
    DeliveryAttempt, LocalSession, RuntimeState, new_event_id, new_session_id, process_identity,
    update_runtime,
};
use crate::service::Service;
use crate::vault::{
    Clock, EventDraft, SealReason, SystemClock, UlidIdSource, WriterKey, append_event_reserved,
    seal_writer,
};

/// How long a Claude Code session whose process exited stays resumable before
/// it is lost, counted from its last report. `claude --resume` keeps the
/// conversation id, so a restart within this window gets its session and its
/// steps back; a crash still reaches the human as a hand-off item afterwards.
/// Chosen by the user on 2026-11-07 ("Wait 15 minutes").
pub const CLAUDE_RESUME_GRACE_S: i64 = 15 * 60;

/// One session registration request from `init` or a harness hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRegistration {
    pub project: ProjectSlug,
    pub harness: Harness,
    pub harness_session_id: String,
    pub pid: u32,
    pub process_start: u64,
    pub repo_root: PathBuf,
}

/// One queue acceptance, reported honestly: queued is not continued.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuedReport {
    pub session: SessionId,
    pub packet: crate::model::Sha256,
    pub thread_id: String,
    pub message_id: String,
    /// `queued` or `queued_waiting_for_resume`; never "continued".
    pub status: String,
    /// Human-readable honest wording, including the recorded resume warning.
    pub detail: String,
}

/// One steward tick result (CLI `steward --once`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StewardReport {
    pub processed: u32,
    pub delivered: u32,
    pub lost: u32,
    /// Sessions whose turn process exited but which stay resumable by harness
    /// id (a Codex thread); newly observed in this tick, never lost.
    pub resumable: u32,
    pub capabilities: Vec<AdapterCapabilities>,
    pub queued: Vec<QueuedReport>,
}

/// Register one harness process as a local session.
///
/// A matching unsealed live registration reuses its session ID and emits no
/// duplicate start pulse. A Codex registration with the same durable thread id
/// also reuses its session ID across the separate processes of its turns and
/// only updates the recorded process identity. A changed process identity for
/// any other harness, a new harness session or a lost/sealed registration
/// allocates a new session ID; previous mapping history is retained.
pub fn register_session(service: &Service, registration: SessionRegistration) -> Result<SessionId> {
    let repo_root = registration.repo_root.canonicalize().map_err(|_| {
        AgendarError::new(ErrorCode::Context, "The harness repository was not found.")
            .with_detail("missing_repo")
            .with_hint("Pass an existing local repository path with --repo.")
    })?;
    if !repo_root.is_dir() {
        return Err(AgendarError::validation_at(
            "/repo",
            "not_a_directory",
            "The harness repository must be a directory.",
        ));
    }
    let observed = process_identity(registration.pid).ok_or_else(|| {
        AgendarError::new(
            ErrorCode::Context,
            "The harness process was not found; no session was registered.",
        )
        .with_detail("missing_harness_process")
        .with_hint("The harness process must be running on this machine.")
    })?;
    if observed != registration.process_start {
        return Err(AgendarError::new(
            ErrorCode::Context,
            "The harness process start identity changed while registering.",
        )
        .with_detail("process_identity_changed")
        .with_hint("Retry from the running harness session."));
    }

    let (session_id, needs_pulse) = update_runtime(service.local(), service.vault(), |runtime| {
        if let Some((id, _)) = runtime.sessions.iter().find(|(_, session)| {
            session.matches_identity(
                &registration.project,
                registration.harness,
                &registration.harness_session_id,
                registration.pid,
                registration.process_start,
            )
        }) {
            return Ok((id.clone(), !runtime.sessions[id].started));
        }
        // A Codex thread is one durable session across the processes of its
        // turns: each `codex exec` turn is a new process with the same thread
        // id, so re-registration reuses the Agendar session and updates only
        // the recorded process identity. Pending packets and acks keep working
        // for that same session.
        let reusable_thread = runtime
            .sessions
            .iter()
            .filter(|(_, session)| {
                registration.harness == Harness::Codex
                    && session.harness == Harness::Codex
                    && session.project == registration.project
                    && session.harness_session_id == registration.harness_session_id
                    && !session.lost
                    && !session.sealed
            })
            .max_by(|left, right| {
                (left.1.registered_at.canonical(), left.0.as_str())
                    .cmp(&(right.1.registered_at.canonical(), right.0.as_str()))
            })
            .map(|(id, _)| id.clone());
        // A resumed Claude Code conversation (`claude --resume`) keeps its id.
        // When the process recorded for it is gone and the session is not lost
        // yet (the grace window), the new process takes the same Agendar
        // session back with its attempts. A live process keeps its own.
        let resumed_conversation = (registration.harness == Harness::ClaudeCode)
            .then(|| {
                runtime
                    .sessions
                    .iter()
                    .filter(|(_, session)| {
                        session.harness == Harness::ClaudeCode
                            && session.project == registration.project
                            && session.harness_session_id == registration.harness_session_id
                            && !session.lost
                            && !session.sealed
                            && process_identity(session.pid) != Some(session.process_start)
                    })
                    .max_by(|left, right| {
                        (left.1.registered_at.canonical(), left.0.as_str())
                            .cmp(&(right.1.registered_at.canonical(), right.0.as_str()))
                    })
                    .map(|(id, _)| id.clone())
            })
            .flatten();
        if let Some(id) = reusable_thread.or(resumed_conversation) {
            let record = runtime.sessions.get_mut(&id).expect("found above");
            record.pid = registration.pid;
            record.process_start = registration.process_start;
            record.resumable = false;
            return Ok((id, !record.started));
        }
        let id = new_session_id()?;
        let reserved = new_event_id()?;
        runtime.sessions.insert(
            id.clone(),
            LocalSession {
                project: registration.project.clone(),
                harness: registration.harness,
                harness_session_id: registration.harness_session_id.clone(),
                pid: registration.pid,
                process_start: registration.process_start,
                repo_root: repo_root.to_string_lossy().into_owned(),
                registered_at: SystemClock.now(),
                started: false,
                start_event: None,
                start_reserved: Some(reserved),
                lost: false,
                sealed: false,
                resumable: false,
                lost_event: None,
                lost_reserved: None,
                agenda_seen: None,
                reminded: Vec::new(),
            },
        );
        Ok((id, true))
    })?
    .0;

    if needs_pulse {
        let ((reserved, already_started), _) =
            update_runtime(service.local(), service.vault(), |runtime| {
                let session = runtime.sessions.get(&session_id).ok_or_else(|| {
                    AgendarError::new(ErrorCode::Context, "The registered session disappeared.")
                        .with_detail("session_registration_lost")
                })?;
                Ok((session.start_reserved.clone(), session.started))
            })?;
        if already_started {
            continue_lost_predecessor(service, &registration, &session_id)?;
            return Ok(session_id);
        }
        let Some(reserved) = reserved else {
            return Err(AgendarError::new(
                ErrorCode::Io,
                "Cannot register the session: its start reservation is missing.",
            )
            .with_detail("session_start_reserved"));
        };
        let draft = EventDraft {
            refs: Refs::default(),
            payload: EventPayload::Pulse(Pulse {
                agent: Agent {
                    session: session_id.clone(),
                    harness: registration.harness,
                },
                message: "Session started".to_owned(),
                boundary: true,
                eta: None,
            }),
        };
        let key = WriterKey {
            project: registration.project.clone(),
            writer: WriterId::new(
                service.host().clone(),
                WriterRole::Agent(session_id.clone()),
            ),
        };
        let mut clock = SystemClock;
        let mut ids = UlidIdSource;
        let appended = append_event_reserved(
            service.vault(),
            service.local(),
            &key,
            draft,
            Some(reserved),
            &clock,
            &mut ids,
        )?;
        let _ = &mut clock;
        update_runtime(service.local(), service.vault(), |runtime| {
            if let Some(session) = runtime.sessions.get_mut(&session_id) {
                session.started = true;
                session.start_event = Some(appended.event.id.clone());
            }
            Ok(())
        })?;
    }
    continue_lost_predecessor(service, &registration, &session_id)?;
    Ok(session_id)
}

/// A new session of an agent conversation that was lost on this computer
/// (`claude --resume` after the window, a resumed Codex thread) continues it:
/// the steward records `session.continued`, and the lost session's running
/// steps move to the new one. Only harnesses whose session id is a durable
/// conversation id take part; another agent never inherits work this way.
fn continue_lost_predecessor(
    service: &Service,
    registration: &SessionRegistration,
    session_id: &SessionId,
) -> Result<()> {
    if !matches!(registration.harness, Harness::ClaudeCode | Harness::Codex) {
        return Ok(());
    }
    let runtime = RuntimeState::load(service.local(), service.vault())?;
    let Some(start_event) = runtime
        .sessions
        .get(session_id)
        .and_then(|session| session.start_event.clone())
    else {
        return Ok(());
    };
    let predecessor = runtime
        .sessions
        .iter()
        .filter(|(id, session)| {
            *id != session_id
                && session.lost
                && session.project == registration.project
                && session.harness == registration.harness
                && session.harness_session_id == registration.harness_session_id
                && session.lost_event.is_some()
        })
        .max_by(|left, right| {
            (left.1.registered_at.canonical(), left.0.as_str())
                .cmp(&(right.1.registered_at.canonical(), right.0.as_str()))
        })
        .map(|(id, session)| (id.clone(), session.lost_event.clone()));
    let Some((previous, Some(lost_event))) = predecessor else {
        return Ok(());
    };
    // Idempotent: continue only while the lost session still holds a running
    // step. Once its steps moved, every later registration finds nothing.
    let fresh = service.refresh(SystemClock.now(), false)?;
    let holds_work = fresh
        .state
        .projects
        .get(&registration.project)
        .is_some_and(|project| {
            project.features.values().any(|feature| {
                feature.session.as_ref() == Some(&previous)
                    && feature.progress == crate::model::Progress::Doing
            })
        });
    if !holds_work {
        return Ok(());
    }
    let draft = EventDraft {
        refs: Refs {
            causes: vec![lost_event, start_event],
            ..Refs::default()
        },
        payload: EventPayload::SessionContinued(SessionContinued {
            session: session_id.clone(),
            previous,
        }),
    };
    let key = WriterKey {
        project: registration.project.clone(),
        writer: WriterId::new(service.host().clone(), WriterRole::Steward),
    };
    let clock = SystemClock;
    let mut ids = UlidIdSource;
    append_event_reserved(
        service.vault(),
        service.local(),
        &key,
        draft,
        None,
        &clock,
        &mut ids,
    )?;
    Ok(())
}

/// Run one steward service tick: check registered pids, record losses, seal.
pub fn steward_tick(service: &Service) -> Result<StewardReport> {
    steward_tick_at(service, SystemClock.now())
}

/// One steward tick judged at `now` (the Claude Code resume window is counted
/// against it; tests move it forward).
pub fn steward_tick_at(service: &Service, now: UtcTime) -> Result<StewardReport> {
    let layout = service
        .local()
        .ensure_layout(service.vault().root(), service.vault().id())?;
    // The singleton lease is taken before any runtime/writer lock and is never
    // nested inside another lock wait.
    let _lease = acquire_lock(&layout.locks.join("steward.lock"))?;
    let runtime = RuntimeState::load(service.local(), service.vault())?;
    let candidates: Vec<(SessionId, LocalSession)> = runtime
        .sessions
        .iter()
        .filter(|(_, session)| !session.lost && !session.sealed)
        .map(|(id, session)| (id.clone(), session.clone()))
        .collect();
    let processed = candidates.len() as u32;
    let mut lost = 0_u32;
    let mut resumable = 0_u32;
    // Last report per session, read once and only when a Claude Code process
    // has exited: the resume window counts from it.
    let mut last_reports: Option<BTreeMap<SessionId, UtcTime>> = None;

    for (session_id, session) in candidates {
        let reason = match process_identity(session.pid) {
            Some(observed) if observed == session.process_start => continue,
            Some(_) => LostReason::PidReused,
            None => LostReason::Exited,
        };
        // A Codex thread outlives the process of each turn. An exited turn
        // process makes the session resumable (shown as quiet), not lost: no
        // `session.lost` fact, no rescue item and no sealed writer, so the next
        // turn re-registers the same Agendar session by thread id and its
        // pending packets and acks keep working.
        if session.harness == Harness::ClaudeCode {
            let reports = match &last_reports {
                Some(reports) => reports,
                None => last_reports.insert(last_reports_by_session(service, now)),
            };
            let last = reports
                .get(&session_id)
                .copied()
                .unwrap_or(session.registered_at)
                .max(session.registered_at);
            if now.millis() - last.millis() < CLAUDE_RESUME_GRACE_S * 1_000 {
                if mark_resumable(service, &session_id, &session)? {
                    resumable = resumable.saturating_add(1);
                }
                continue;
            }
        }
        if session.harness == Harness::Codex {
            let mut marked = false;
            update_runtime(service.local(), service.vault(), |runtime| {
                if let Some(record) = runtime.sessions.get_mut(&session_id)
                    && record.pid == session.pid
                    && record.process_start == session.process_start
                    && !record.resumable
                    && !record.lost
                {
                    record.resumable = true;
                    marked = true;
                }
                Ok(())
            })?;
            if marked {
                resumable = resumable.saturating_add(1);
            }
            continue;
        }
        let ((reserved, already_lost), _) =
            update_runtime(service.local(), service.vault(), |runtime| {
                let Some(record) = runtime.sessions.get_mut(&session_id) else {
                    return Err(AgendarError::new(
                        ErrorCode::Context,
                        "The registered session disappeared during the steward tick.",
                    )
                    .with_detail("session_registration_lost"));
                };
                if record.lost {
                    return Ok((None, true));
                }
                let reserved = match record.lost_reserved.clone() {
                    Some(reserved) => reserved,
                    None => {
                        let reserved = new_event_id()?;
                        record.lost_reserved = Some(reserved.clone());
                        reserved
                    }
                };
                Ok((Some(reserved), false))
            })?;
        if already_lost {
            continue;
        }
        let Some(reserved) = reserved else {
            continue;
        };

        let draft = EventDraft {
            refs: Refs::default(),
            payload: EventPayload::SessionLost(SessionLost {
                session: session_id.clone(),
                process_start: session.process_start,
                reason,
                exit_code: None,
            }),
        };
        let steward_writer = WriterId::new(service.host().clone(), WriterRole::Steward);
        let key = WriterKey {
            project: session.project.clone(),
            writer: steward_writer,
        };
        let mut clock = SystemClock;
        let mut ids = UlidIdSource;
        let appended = append_event_reserved(
            service.vault(),
            service.local(),
            &key,
            draft,
            Some(reserved),
            &clock,
            &mut ids,
        )?;
        let _ = &mut clock;

        let agent_key = WriterKey {
            project: session.project.clone(),
            writer: WriterId::new(
                service.host().clone(),
                WriterRole::Agent(session_id.clone()),
            ),
        };
        seal_writer(
            service.vault(),
            service.local(),
            &agent_key,
            SealReason::SessionEnded,
        )?;
        update_runtime(service.local(), service.vault(), |runtime| {
            if let Some(record) = runtime.sessions.get_mut(&session_id) {
                record.lost = true;
                record.sealed = true;
                record.resumable = false;
                record.lost_event = Some(appended.event.id.clone());
            }
            Ok(())
        })?;
        lost = lost.saturating_add(1);
    }

    let mut capabilities: Vec<AdapterCapabilities> = Vec::new();
    for (_, session) in runtime.sessions.iter() {
        let capability = service.capabilities_for(session.harness);
        if !capabilities
            .iter()
            .any(|existing| existing.harness == capability.harness)
        {
            capabilities.push(capability);
        }
    }
    capabilities.sort_by_key(|capability| capability.harness);

    let (delivered, queued) = deliver_pending(service)?;

    Ok(StewardReport {
        processed,
        delivered,
        lost,
        resumable,
        capabilities,
        queued,
    })
}

/// Mark an exited session resumable once (shown as quiet). Returns whether
/// this tick marked it.
fn mark_resumable(
    service: &Service,
    session_id: &SessionId,
    session: &LocalSession,
) -> Result<bool> {
    let mut marked = false;
    update_runtime(service.local(), service.vault(), |runtime| {
        if let Some(record) = runtime.sessions.get_mut(session_id)
            && record.pid == session.pid
            && record.process_start == session.process_start
            && !record.resumable
            && !record.lost
        {
            record.resumable = true;
            marked = true;
        }
        Ok(())
    })?;
    Ok(marked)
}

/// The newest event time of every session, from a fresh read of the Vault.
fn last_reports_by_session(service: &Service, now: UtcTime) -> BTreeMap<SessionId, UtcTime> {
    service
        .refresh(now, false)
        .map(|fresh| {
            fresh
                .state
                .projects
                .values()
                .flat_map(|project| project.sessions.iter())
                .map(|(id, session)| (id.clone(), session.last_event_ts))
                .collect()
        })
        .unwrap_or_default()
}

/// Route every eligible unapplied answer to one local session.
///
/// Pull harnesses only become readable through `inbox`; the steward never
/// claims a delivery they did not acknowledge. Codex is enqueued by durable
/// thread id and the queue outcome is persisted before the `delivered` event.
fn deliver_pending(service: &Service) -> Result<(u32, Vec<QueuedReport>)> {
    let runtime = RuntimeState::load(service.local(), service.vault())?;
    let mut projects: BTreeSet<ProjectSlug> = BTreeSet::new();
    for session in runtime.sessions.values() {
        projects.insert(session.project.clone());
    }
    let now = SystemClock.now();
    let mut delivered = 0_u32;
    let mut queued_reports = Vec::new();
    for project in projects {
        let fresh = match service.refresh(now, false) {
            Ok(fresh) => fresh,
            Err(_) => continue,
        };
        let Ok(project_state) = service.project(&fresh, &project) else {
            continue;
        };
        for decision in project_state.decisions.values() {
            let Ok(Some(packet)) = service.decision_packet(&fresh, &project, &decision.id) else {
                continue;
            };
            if !matches!(packet.kind, PacketKind::Answer | PacketKind::Pivot) {
                continue;
            }
            let Some((session_id, session)) =
                recipient(&runtime, project_state, &packet, &decision.applied_sessions)
            else {
                continue;
            };
            // An override is rework for every affected session, not only the
            // first recipient.
            let targets: Vec<(SessionId, LocalSession)> = if packet.kind == PacketKind::Pivot {
                let affected =
                    crate::reduce::assumptions::pivot_recipients(project_state, &decision.id);
                if affected.is_empty() {
                    vec![(session_id, session)]
                } else {
                    affected
                        .into_iter()
                        .filter_map(|id| {
                            let record = runtime.sessions.get(&id)?;
                            addressable(record).then(|| (id, record.clone()))
                        })
                        .collect()
                }
            } else {
                vec![(session_id, session)]
            };
            for (session_id, session) in targets {
                deliver_one(
                    service,
                    &runtime,
                    &project,
                    &packet,
                    &decision.applied_sessions,
                    &session_id,
                    &session,
                    now,
                    &mut delivered,
                    &mut queued_reports,
                )?;
            }
        }
    }
    Ok((delivered, queued_reports))
}

/// Deliver one packet to one session through its harness transport.
#[allow(clippy::too_many_arguments)]
fn deliver_one(
    service: &Service,
    runtime: &RuntimeState,
    project: &ProjectSlug,
    packet: &InboxPacket,
    applied: &[SessionId],
    session_id: &SessionId,
    session: &LocalSession,
    now: crate::model::UtcTime,
    delivered: &mut u32,
    queued_reports: &mut Vec<QueuedReport>,
) -> Result<()> {
    if session.harness != Harness::Codex {
        // Pull-only: the packet is already available through `inbox`,
        // which records the checkpoint only after a successful read.
        return Ok(());
    }
    let key = format!("{}:{}", packet.id.as_str(), session_id.as_str());
    if runtime.delivery_attempts.contains_key(&key) {
        // Known/unknown queue outcomes are never re-enqueued on a timer.
        return Ok(());
    }
    if applied.contains(session_id) {
        return Ok(());
    }
    // A known queue outcome for this packet and this durable thread is
    // never enqueued again, even after a same-thread re-registration
    // under a new Agendar session id.
    let already_queued = runtime.delivery_attempts.values().any(|attempt| {
        attempt.packet == packet.id
            && attempt
                .queued
                .as_ref()
                .is_some_and(|queued| queued.thread_id == session.harness_session_id)
    });
    if already_queued {
        return Ok(());
    }
    let adapter = CodexAdapter::new(
        service.codex_binary()?,
        session.harness_session_id.clone(),
        service.command_prefix(project, session_id),
    );
    match adapter.deliver(packet) {
        Ok(TransportResult::Queued(outcome)) => {
            service.mark_delivered_with(
                project,
                session_id,
                std::slice::from_ref(packet),
                DeliveredOutcome::Accepted,
                Some(&outcome),
            )?;
            *delivered = delivered.saturating_add(1);
            queued_reports.push(QueuedReport {
                session: session_id.clone(),
                packet: packet.id.clone(),
                thread_id: outcome.thread_id.clone(),
                message_id: outcome.message_id.clone(),
                status: match outcome.resume_status {
                    crate::adapters::ResumeStatus::WarningWaitingForResume => {
                        "queued_waiting_for_resume".to_owned()
                    }
                    crate::adapters::ResumeStatus::RequestedUnconfirmed => "queued".to_owned(),
                },
                detail: match outcome.resume_status {
                    crate::adapters::ResumeStatus::WarningWaitingForResume => {
                        "queued, applies when the session resumes".to_owned()
                    }
                    crate::adapters::ResumeStatus::RequestedUnconfirmed => {
                        "queued; continuation unconfirmed".to_owned()
                    }
                },
            });
        }
        Ok(TransportResult::Uncertain(_)) => {
            // Persist the attempt so an unknown outcome is reconciled,
            // never blindly retried as a possibly durable enqueue.
            persist_uncertain(service, project, session_id, packet, now)?;
        }
        Ok(_) => {}
        Err(_) => {}
    }
    Ok(())
}

/// Prefer the feature's assigned session; otherwise the newest live session in
/// the project. A lost Codex registration stays addressable by its thread id.
fn recipient(
    runtime: &RuntimeState,
    project: &ProjectState,
    packet: &InboxPacket,
    applied: &[SessionId],
) -> Option<(SessionId, LocalSession)> {
    let candidates: Vec<(&SessionId, &LocalSession)> = runtime
        .sessions
        .iter()
        .filter(|(id, session)| {
            session.project == packet.project && !applied.contains(id) && addressable(session)
        })
        .collect();
    if let Some(owner) = project
        .features
        .get(&packet.node)
        .and_then(|feature| feature.session.as_ref())
        && let Some((id, session)) = candidates.iter().find(|(id, _)| *id == owner)
    {
        return Some(((**id).clone(), (*session).clone()));
    }
    candidates
        .into_iter()
        .filter(|(_, session)| !session.lost && !session.sealed)
        .max_by(|left, right| {
            (left.1.registered_at.canonical(), left.0.as_str())
                .cmp(&(right.1.registered_at.canonical(), right.0.as_str()))
        })
        .map(|(id, session)| (id.clone(), session.clone()))
}

fn addressable(session: &LocalSession) -> bool {
    if !session.lost && !session.sealed {
        return true;
    }
    // Codex address eligibility is independent of process liveness; the
    // original durable thread can still accept a queued message.
    session.harness == Harness::Codex && !session.harness_session_id.trim().is_empty()
}

fn persist_uncertain(
    service: &Service,
    _project: &ProjectSlug,
    session: &SessionId,
    packet: &InboxPacket,
    now: crate::model::UtcTime,
) -> Result<()> {
    let key = format!("{}:{}", packet.id.as_str(), session.as_str());
    update_runtime(service.local(), service.vault(), |runtime| {
        runtime.delivery_attempts.insert(
            key.clone(),
            DeliveryAttempt {
                packet: packet.id.clone(),
                session: session.clone(),
                outcome: "uncertain".to_owned(),
                delivered_at: now,
                event: None,
                reserved: None,
                queued: None,
            },
        );
        Ok(())
    })?;
    Ok(())
}
