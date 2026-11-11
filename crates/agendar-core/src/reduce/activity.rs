//! Local activity feed: human sentences about one project's recent events,
//! newest first, for the desktop UI only.
//!
//! This is a non-canonical projection. It is never part of `State`, `state
//! export`, the event-set digest or convergence; the app host attaches it to
//! each project object of its snapshot. Consecutive events from one writer
//! within two minutes fold into one line. Questions, answers, acks,
//! withdrawals, session starts and ends, and project declarations always stand
//! alone.

use serde::Serialize;

use crate::model::event::{Event, EventPayload, WorkStatus};
use crate::model::ids::{EventId, Harness, NodePath, UtcTime, WriterRole};
use crate::model::state::{ProjectState, ReceiptSource};
use crate::model::{Msg, MsgArg};

/// How many of a project's newest valid events the feed considers.
pub const ACTIVITY_EVENT_WINDOW: usize = 400;

/// The most feed items returned for one project.
pub const ACTIVITY_MAX_ITEMS: usize = 40;

/// Two events from the same writer at most this far apart fold together.
const BURST_GAP_MS: i64 = 120_000;

/// The steward's session-start pulse message.
const SESSION_STARTED: &str = "Session started";

/// Who an activity item is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Agent,
    Human,
    System,
}

/// One feed line in the frontend wire shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivityItem {
    /// Timestamp of the newest event folded into this item.
    pub ts: UtcTime,
    pub actor: String,
    pub actor_kind: ActorKind,
    pub icon: &'static str,
    pub text: String,
    /// `text` as a catalog message, for the app's language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<Msg>,
    pub detail: Option<String>,
    /// The feature node this item is about, for single-subject items only.
    pub node: Option<NodePath>,
    /// How many events were folded into this item.
    pub count: u32,
    /// The report document a "published a report" line opens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<crate::model::BlobRef>,
}

/// Display label of one agent harness.
pub const fn harness_label(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "Claude Code",
        Harness::Codex => "Codex",
        Harness::Pi => "pi",
        Harness::Herdr => "Herdr",
        Harness::OpenCode => "opencode",
        Harness::Cursor => "Cursor",
        Harness::Dsh => "dsh",
        Harness::Agy => "Antigravity",
        Harness::Gemini => "Gemini CLI",
        Harness::Generic => "Agent",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Join,
    Project,
    Plan,
    Started,
    Done,
    Dropped,
    Pulse,
    Ask,
    Answer,
    Ack,
    Withdrawn,
    Lost,
    Report,
}

impl Key {
    const fn standalone(self) -> bool {
        !matches!(
            self,
            Self::Plan | Self::Started | Self::Done | Self::Dropped | Self::Pulse
        )
    }
}

/// One classified event.
struct Entry {
    key: Key,
    /// Writer identity: bursts only fold within one writer (one session).
    writer: String,
    actor: String,
    /// The actor as a message argument (`You` is translated, names are not).
    actor_arg: MsgArg,
    actor_kind: ActorKind,
    ts: UtcTime,
    text: String,
    msg: Option<Msg>,
    icon: &'static str,
    detail: Option<String>,
    node: Option<NodePath>,
    /// The subject step's title, used as the detail of a single-subject burst.
    title: Option<String>,
    /// The milestone a plan update targets.
    milestone: Option<String>,
    report: Option<crate::model::BlobRef>,
}

/// Build the activity feed of one project from its recent events.
///
/// `events` may arrive in any order; events of other projects and kinds the
/// feed does not describe are ignored. Titles, questions and option labels
/// come from the current reduced project state and fall back to node paths
/// or ids. The result is newest first and holds at most
/// [`ACTIVITY_MAX_ITEMS`] items.
pub fn build(project: &ProjectState, events: &[Event]) -> Vec<ActivityItem> {
    let mut ordered: Vec<&Event> = events
        .iter()
        .filter(|event| event.project == project.summary.slug)
        .collect();
    ordered.sort_by(|left, right| (left.ts, left.id.as_str()).cmp(&(right.ts, right.id.as_str())));
    let mut groups: Vec<Vec<Entry>> = Vec::new();
    for event in ordered {
        let Some(entry) = classify(project, event) else {
            continue;
        };
        let previous = groups.last_mut().filter(|group| {
            group.last().is_some_and(|last| {
                !last.key.standalone()
                    && !entry.key.standalone()
                    && last.writer == entry.writer
                    && entry.ts.millis() - last.ts.millis() <= BURST_GAP_MS
            })
        });
        match previous {
            Some(group) => group.push(entry),
            None => groups.push(vec![entry]),
        }
    }
    groups
        .iter()
        .rev()
        .take(ACTIVITY_MAX_ITEMS)
        .filter_map(|group| summarize(group))
        .collect()
}

fn classify(project: &ProjectState, event: &Event) -> Option<Entry> {
    let (actor, actor_kind) = actor_of(event);
    let actor_arg = actor_arg_of(event, &actor);
    let node = event.refs.node.clone();
    let title = node.as_ref().map(|node| step_title(project, node));
    let step = title.clone().unwrap_or_else(|| "a step".to_owned());
    let step_arg = title.clone().map_or_else(
        || MsgArg::Msg(Box::new(Msg::new("act.a_step"))),
        MsgArg::Text,
    );
    let with_actor = |key: &str| Msg::new(key).arg("actor", actor_arg.clone());
    let mut detail = None;
    let mut milestone = None;
    let mut report = None;
    let (key, text, msg, icon) = match &event.payload {
        EventPayload::Pulse(body) if is_session_start(&body.message) => (
            Key::Join,
            format!("{actor} started a session"),
            Some(with_actor("act.joined")),
            "bot",
        ),
        EventPayload::Pulse(body) => {
            detail.clone_from(&title);
            (
                Key::Pulse,
                format!("{actor}: {}", body.message.trim()),
                Some(with_actor("act.pulse").text("message", body.message.trim())),
                "radio",
            )
        }
        EventPayload::ProjectDeclared(_) => (
            Key::Project,
            format!("{actor} described the project"),
            Some(with_actor("act.project")),
            "target",
        ),
        EventPayload::PlanFeature(body) => {
            milestone = body
                .milestone
                .clone()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| {
                    node.as_ref()
                        .and_then(|node| project.features.get(node))
                        .and_then(|feature| feature.milestone.clone())
                });
            (Key::Plan, String::new(), None, "git-branch")
        }
        EventPayload::WorkProgress(body) => match body.status {
            WorkStatus::Started => (
                Key::Started,
                format!("{actor} started {step}"),
                Some(with_actor("act.started").arg("step", step_arg.clone())),
                "clock",
            ),
            WorkStatus::Done | WorkStatus::Verified => (
                Key::Done,
                format!("{actor} finished {step}"),
                Some(with_actor("act.finished").arg("step", step_arg.clone())),
                "check",
            ),
            WorkStatus::Dropped => (
                Key::Dropped,
                format!("{actor} dropped {step}"),
                Some(with_actor("act.dropped").arg("step", step_arg.clone())),
                "x",
            ),
        },
        EventPayload::PlanDropped(_) => (
            Key::Dropped,
            format!("{actor} dropped {step}"),
            Some(with_actor("act.dropped").arg("step", step_arg.clone())),
            "x",
        ),
        EventPayload::DecisionAsk(body) => (
            Key::Ask,
            format!("{actor} asked you: {}", body.card.question),
            Some(with_actor("act.asked").text("question", body.card.question.clone())),
            "help-circle",
        ),
        EventPayload::DecisionAnswer(body) => {
            let question = question(project, event);
            let label = project
                .receipts
                .get(&event.id)
                .map(|receipt| receipt.chose.label.clone())
                .or_else(|| {
                    decision(project, event.refs.ask.as_ref())
                        .and_then(|decision| {
                            decision
                                .card
                                .options
                                .iter()
                                .find(|option| option.id == body.option)
                        })
                        .map(|option| option.label.clone())
                })
                .unwrap_or_else(|| body.option.as_str().to_owned());
            if let Some(text) = &body.own_answer {
                detail = Some(text.lines().next().unwrap_or_default().to_owned());
            }
            let label_arg = if body.option.as_str() == crate::model::cards::OWN_OPTION {
                MsgArg::Msg(Box::new(Msg::new("fmt.own")))
            } else {
                MsgArg::Text(label.clone())
            };
            (
                Key::Answer,
                format!("{actor} answered \u{201c}{question}\u{201d} \u{2192} {label}"),
                Some(
                    with_actor("act.answered")
                        .text("question", question.clone())
                        .arg("label", label_arg),
                ),
                "user",
            )
        }
        EventPayload::DecisionAck(_) => {
            detail = Some(question(project, event));
            (
                Key::Ack,
                format!("{actor} applied your answer"),
                Some(with_actor("act.applied")),
                "badge-check",
            )
        }
        EventPayload::ReportPublished(body) => {
            detail = Some(body.summary.lines().next().unwrap_or_default().to_owned());
            report = Some(body.report.clone());
            (
                Key::Report,
                format!("{actor} published a report: {}", body.title),
                Some(with_actor("act.report").text("title", body.title.clone())),
                "file-text",
            )
        }
        EventPayload::DecisionWithdrawn(_) => {
            let question = question(project, event);
            (
                Key::Withdrawn,
                format!("{actor} withdrew: {question}"),
                Some(with_actor("act.withdrew").text("question", question)),
                "x",
            )
        }
        EventPayload::SessionContinued(body) => {
            let harness = project
                .sessions
                .get(&body.session)
                .map(|session| harness_label(session.harness));
            let session = harness.unwrap_or("An agent");
            let who = harness.map_or_else(
                || MsgArg::Msg(Box::new(Msg::new("act.an_agent"))),
                |label| MsgArg::Text(label.to_owned()),
            );
            (
                Key::Join,
                format!("{session} session came back and continued its steps"),
                Some(Msg::new("act.session_continued").arg("actor", who)),
                "refresh-cw",
            )
        }
        EventPayload::SessionLost(body) => {
            let harness = project
                .sessions
                .get(&body.session)
                .map(|session| harness_label(session.harness));
            let session = harness.unwrap_or("An agent");
            let who = harness.map_or_else(
                || MsgArg::Msg(Box::new(Msg::new("act.an_agent"))),
                |label| MsgArg::Text(label.to_owned()),
            );
            (
                Key::Lost,
                format!("{session} session ended"),
                Some(Msg::new("act.session_ended").arg("actor", who)),
                "pause-circle",
            )
        }
        EventPayload::DecisionAssumed(_)
        | EventPayload::DecisionAuto(_)
        | EventPayload::Delivery(_)
        | EventPayload::EvidenceObserved(_)
        | EventPayload::ReviewResult(_)
        | EventPayload::RuleSet(_)
        | EventPayload::PolicySet(_)
        | EventPayload::NeedSnoozed(_)
        | EventPayload::Delivered(_) => return None,
    };
    let actor_kind = match &event.payload {
        // An answer is the human's own choice unless its receipt says otherwise.
        EventPayload::DecisionAnswer(_) => match project.receipts.get(&event.id) {
            Some(receipt) if receipt.source != ReceiptSource::Human => ActorKind::System,
            _ => ActorKind::Human,
        },
        _ => actor_kind,
    };
    let node = match key {
        Key::Join | Key::Project => None,
        _ => node,
    };
    Some(Entry {
        key,
        writer: event.writer.to_string(),
        actor,
        actor_arg,
        actor_kind,
        ts: event.ts,
        text,
        msg,
        icon,
        detail,
        node,
        title,
        milestone,
        report,
    })
}

fn actor_of(event: &Event) -> (String, ActorKind) {
    if let Some(agent) = event.payload.agent() {
        return (harness_label(agent.harness).to_owned(), ActorKind::Agent);
    }
    match event.writer.role() {
        WriterRole::Operator => ("You".to_owned(), ActorKind::Human),
        WriterRole::Steward | WriterRole::Agent(_) => ("Agendar".to_owned(), ActorKind::System),
    }
}

/// The actor as a message argument: the human is `actor.you`, names stay as written.
fn actor_arg_of(event: &Event, actor: &str) -> MsgArg {
    if event.payload.agent().is_none() && matches!(event.writer.role(), WriterRole::Operator) {
        MsgArg::Msg(Box::new(Msg::new("actor.you")))
    } else {
        MsgArg::Text(actor.to_owned())
    }
}

fn is_session_start(message: &str) -> bool {
    message.trim().trim_end_matches('.') == SESSION_STARTED
}

fn step_title(project: &ProjectState, node: &NodePath) -> String {
    project
        .features
        .get(node)
        .map(|feature| feature.title.trim())
        .filter(|title| !title.is_empty())
        .unwrap_or(node.as_str())
        .to_owned()
}

fn decision<'a>(
    project: &'a ProjectState,
    ask: Option<&EventId>,
) -> Option<&'a crate::model::state::DecisionState> {
    ask.and_then(|ask| project.decisions.get(ask))
}

/// The question an answer, ack or withdrawal refers to, else its node or ask.
fn question(project: &ProjectState, event: &Event) -> String {
    decision(project, event.refs.ask.as_ref())
        .map(|decision| decision.card.question.clone())
        .or_else(|| {
            event
                .refs
                .node
                .as_ref()
                .map(|node| node.as_str().to_owned())
        })
        .or_else(|| event.refs.ask.as_ref().map(|ask| ask.as_str().to_owned()))
        .unwrap_or_else(|| "a question".to_owned())
}

fn summarize(group: &[Entry]) -> Option<ActivityItem> {
    let last = group.last()?;
    let count = u32::try_from(group.len()).unwrap_or(u32::MAX);
    if group.len() == 1 && last.key != Key::Plan {
        return Some(ActivityItem {
            ts: last.ts,
            actor: last.actor.clone(),
            actor_kind: last.actor_kind,
            icon: last.icon,
            text: last.text.clone(),
            msg: last.msg.clone(),
            detail: last.detail.clone(),
            node: last.node.clone(),
            count,
            report: last.report.clone(),
        });
    }
    let tally = |key: Key| group.iter().filter(|entry| entry.key == key).count();
    // Plan updates count distinct steps: a step declared and then patched in
    // one burst is one updated step, not two.
    let plan_entries = || group.iter().filter(|entry| entry.key == Key::Plan);
    let plan_steps: std::collections::BTreeSet<_> = plan_entries()
        .filter_map(|entry| entry.node.as_ref())
        .collect();
    let mut plan = plan_steps.len() + plan_entries().filter(|entry| entry.node.is_none()).count();
    let mut started = tally(Key::Started);
    let mut done = tally(Key::Done);
    let dropped = tally(Key::Dropped);
    let pulses = tally(Key::Pulse);
    let mut added_done = 0;
    if done > 0 {
        started = 0;
        if plan == done {
            added_done = done;
            plan = 0;
            done = 0;
        }
    }
    let counts = [
        (
            added_done,
            "added and finished",
            "step",
            "act.part.added_done",
        ),
        (done, "finished", "step", "act.part.finished"),
        (started, "started", "step", "act.part.started"),
        (dropped, "dropped", "step", "act.part.dropped"),
        (plan, "updated", "plan step", "act.part.updated"),
        (pulses, "sent", "progress update", "act.part.sent"),
    ];
    let shown = counts.iter().filter(|(count, ..)| *count > 0);
    let parts: Vec<String> = shown
        .clone()
        .map(|(count, verb, noun, _)| {
            let plural = if *count == 1 { "" } else { "s" };
            format!("{verb} {count} {noun}{plural}")
        })
        .collect();
    let part_msgs: Vec<Msg> = shown
        .map(|(count, _, _, key)| Msg::new(*key).count("n", *count))
        .collect();
    let milestone = one_milestone(group);
    let place = milestone.map_or_else(String::new, |name| format!(" in {name}"));
    let msg = match milestone {
        Some(name) => Msg::new("act.burst_in").text("milestone", name),
        None => Msg::new("act.burst"),
    }
    .arg("actor", last.actor_arg.clone())
    .list("parts", part_msgs);
    let icon = if added_done + done > 0 {
        "check"
    } else if started > 0 {
        "clock"
    } else if plan > 0 {
        "git-branch"
    } else if dropped > 0 {
        "x"
    } else {
        "radio"
    };
    let node = single_subject(group);
    let detail = node.as_ref().and_then(|_| last.title.clone());
    Some(ActivityItem {
        ts: last.ts,
        actor: last.actor.clone(),
        actor_kind: last.actor_kind,
        icon,
        text: format!("{} {}{place}", last.actor, parts.join(", ")),
        msg: Some(msg),
        detail,
        node,
        count,
        report: None,
    })
}

/// The milestone shared by every plan update of a burst, if there is one.
fn one_milestone(group: &[Entry]) -> Option<&str> {
    let mut milestones = group
        .iter()
        .filter(|entry| entry.key == Key::Plan)
        .map(|entry| entry.milestone.as_deref());
    let first = milestones.next()??;
    milestones
        .all(|milestone| milestone == Some(first))
        .then_some(first)
}

/// The node every event of a burst is about, if they share exactly one.
fn single_subject(group: &[Entry]) -> Option<NodePath> {
    let first = group.first()?.node.as_ref()?;
    group
        .iter()
        .all(|entry| entry.node.as_ref() == Some(first))
        .then(|| first.clone())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::cards::DecisionCard;
    use crate::model::event::{
        Agent, DecisionAck, DecisionAnswer, DecisionAsk, PlanFeature, ProjectDeclared, Pulse, Refs,
        SessionLost, WorkProgress,
    };
    use crate::model::ids::{OptionId, ProjectSlug, SessionId, WriterId};
    use crate::model::{DemoKind, LostReason};
    use crate::reduce::{ReduceContext, reduce};

    const HOST: &str = "h-00000000000000000000000001";
    const SESSION: &str = "00000000000000000000000101";
    const OTHER_SESSION: &str = "00000000000000000000000102";
    const ASK: u32 = 12;

    fn agent_writer() -> String {
        format!("{HOST}/s-{SESSION}")
    }

    fn other_writer() -> String {
        format!("{HOST}/s-{OTHER_SESSION}")
    }

    fn operator() -> String {
        format!("{HOST}/operator")
    }

    fn steward() -> String {
        format!("{HOST}/steward")
    }

    fn agent(session: &str, harness: Harness) -> Agent {
        Agent {
            session: SessionId::parse(session).expect("session"),
            harness,
        }
    }

    fn claude() -> Agent {
        agent(SESSION, Harness::ClaudeCode)
    }

    fn id(n: u32) -> EventId {
        EventId::parse(&format!("{n:026}")).expect("event id")
    }

    fn node(path: &str) -> NodePath {
        NodePath::parse(path).expect("node")
    }

    fn at(seconds: i64) -> UtcTime {
        UtcTime::parse("2026-01-01T00:00:00.000Z")
            .expect("time")
            .add_seconds(seconds)
    }

    fn event(n: u32, seconds: i64, writer: &str, refs: Refs, payload: EventPayload) -> Event {
        Event {
            v: 1,
            id: id(n),
            seq: u64::from(n),
            writer: WriterId::parse(writer).expect("writer"),
            ts: at(seconds),
            project: ProjectSlug::parse("example").expect("slug"),
            refs,
            payload,
        }
    }

    fn on(path: &str) -> Refs {
        Refs {
            node: Some(node(path)),
            ..Refs::default()
        }
    }

    fn on_ask(path: &str) -> Refs {
        Refs {
            node: Some(node(path)),
            ask: Some(id(ASK)),
            ..Refs::default()
        }
    }

    fn plan(title: &str, milestone: Option<&str>) -> EventPayload {
        EventPayload::PlanFeature(PlanFeature {
            agent: Some(claude()),
            title: Some(title.to_owned()),
            slice: Some("s1".to_owned()),
            demo_kind: Some(DemoKind::CliTranscript),
            milestone: milestone.map(str::to_owned),
            ..PlanFeature::default()
        })
    }

    fn progress(by: Agent, status: WorkStatus) -> EventPayload {
        let eta = (status == WorkStatus::Started).then(|| crate::model::ids::EtaRange {
            min_s: crate::model::ids::Seconds::new(60).expect("seconds"),
            max_s: crate::model::ids::Seconds::new(600).expect("seconds"),
        });
        EventPayload::WorkProgress(WorkProgress {
            agent: by,
            status,
            eta,
            evidence: Vec::new(),
            note: None,
        })
    }

    fn pulse(by: Agent, message: &str) -> EventPayload {
        EventPayload::Pulse(Pulse {
            agent: by,
            message: message.to_owned(),
            boundary: false,
            eta: None,
        })
    }

    fn card() -> DecisionCard {
        serde_json::from_value(json!({
            "why_now": "Wake delivery is blocked.",
            "question": "How should answers reach idle agents?",
            "reversibility": "easy",
            "reach": "local",
            "external": false,
            "domain": "arch",
            "options": [
                {"id": "a", "label": "Use harness adapters", "consequence": "Hooks.",
                 "reversibility": "easy", "external": false},
                {"id": "b", "label": "Use the inbox", "consequence": "Inbox.",
                 "reversibility": "easy", "external": false}
            ],
            "recommended": {"option": "a", "confidence": "med", "because": "Simpler."},
            "already_checked": [],
            "done_when": [{"id": "done", "text": "It is finished."}],
            "on_timeout": {"kind": "hold"}
        }))
        .expect("card")
    }

    fn ask() -> EventPayload {
        let card = card();
        let policy = crate::reduce::policy::evaluate_with_policy(
            &card,
            crate::model::ids::Mode::Ask,
            &crate::reduce::policy::default_policy(),
            None,
        );
        EventPayload::DecisionAsk(DecisionAsk {
            agent: claude(),
            card,
            policy,
        })
    }

    /// A project with two declared steps, used for title/milestone lookups.
    fn base_events() -> Vec<Event> {
        vec![
            event(
                1,
                0,
                &operator(),
                Refs::default(),
                EventPayload::ProjectDeclared(ProjectDeclared {
                    agent: None,
                    title: Some("Example".to_owned()),
                    brief: None,
                    goal: None,
                    glyph: None,
                    archived: None,
                    icon: None,
                }),
            ),
            event(
                2,
                1,
                &agent_writer(),
                on("core/wire"),
                plan("Wire model", Some("MVP build")),
            ),
            event(
                3,
                2,
                &agent_writer(),
                on("core/index"),
                plan("Local index", Some("MVP build")),
            ),
        ]
    }

    fn feed(extra: Vec<Event>) -> Vec<ActivityItem> {
        let mut events = base_events();
        events.extend(extra);
        let state = reduce(&events, &ReduceContext { as_of: at(100_000) });
        let project = state.projects.values().next().expect("project");
        // Skip the base setup by summarizing only the extra events.
        let extra: Vec<Event> = events.into_iter().skip(3).collect();
        build(project, &extra)
    }

    fn texts(items: &[ActivityItem]) -> Vec<&str> {
        items.iter().map(|item| item.text.as_str()).collect()
    }

    #[test]
    fn a_single_event_keeps_its_own_sentence() {
        let items = feed(vec![event(
            10,
            1_000,
            &agent_writer(),
            on("core/wire"),
            progress(claude(), WorkStatus::Done),
        )]);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.text, "Claude Code finished Wire model");
        assert_eq!(item.icon, "check");
        assert_eq!(item.actor, "Claude Code");
        assert_eq!(item.actor_kind, ActorKind::Agent);
        assert_eq!(item.node, Some(node("core/wire")));
        assert_eq!(item.count, 1);
        assert_eq!(item.ts, at(1_000));
    }

    #[test]
    fn the_wire_shape_matches_the_frontend_contract() {
        let items = feed(vec![event(
            10,
            1_000,
            &agent_writer(),
            on("core/wire"),
            progress(claude(), WorkStatus::Started),
        )]);
        let value = serde_json::to_value(&items[0]).expect("serialize");
        assert_eq!(
            value,
            json!({
                "ts": "2026-01-01T00:16:40.000Z",
                "actor": "Claude Code",
                "actor_kind": "agent",
                "icon": "clock",
                "text": "Claude Code started Wire model",
                "msg": {
                    "key": "act.started",
                    "args": {
                        "actor": {"t": "text", "v": "Claude Code"},
                        "step": {"t": "text", "v": "Wire model"}
                    }
                },
                "detail": null,
                "node": "core/wire",
                "count": 1
            })
        );
    }

    #[test]
    fn a_burst_that_adds_and_finishes_steps_folds_into_one_line() {
        let w = agent_writer();
        let items = feed(vec![
            event(
                10,
                1_000,
                &w,
                on("core/wire"),
                plan("Wire model", Some("MVP build")),
            ),
            event(
                11,
                1_010,
                &w,
                on("core/index"),
                plan("Local index", Some("MVP build")),
            ),
            event(
                12,
                1_020,
                &w,
                on("core/wire"),
                progress(claude(), WorkStatus::Started),
            ),
            event(
                13,
                1_030,
                &w,
                on("core/wire"),
                progress(claude(), WorkStatus::Done),
            ),
            event(
                14,
                1_040,
                &w,
                on("core/index"),
                progress(claude(), WorkStatus::Started),
            ),
            event(
                15,
                1_150,
                &w,
                on("core/index"),
                progress(claude(), WorkStatus::Verified),
            ),
        ]);
        assert_eq!(
            texts(&items),
            ["Claude Code added and finished 2 steps in MVP build"]
        );
        let item = &items[0];
        assert_eq!(item.icon, "check");
        assert_eq!(item.count, 6);
        assert_eq!(item.ts, at(1_150));
        assert_eq!(item.node, None, "a multi-subject burst has no node");
        assert_eq!(item.detail, None);
    }

    #[test]
    fn folded_counts_use_correct_plurals_and_order() {
        let w = agent_writer();
        let items = feed(vec![
            event(
                10,
                1_000,
                &w,
                on("core/wire"),
                progress(claude(), WorkStatus::Started),
            ),
            event(
                11,
                1_001,
                &w,
                on("core/index"),
                progress(claude(), WorkStatus::Started),
            ),
            event(12, 1_002, &w, on("ui/board"), plan("Board", None)),
            event(
                13,
                1_003,
                &w,
                Refs::default(),
                pulse(claude(), "Halfway there"),
            ),
        ]);
        assert_eq!(
            texts(&items),
            ["Claude Code started 2 steps, updated 1 plan step, sent 1 progress update"]
        );
        assert_eq!(items[0].icon, "clock");

        let finished = feed(vec![
            event(
                10,
                1_000,
                &w,
                on("core/wire"),
                progress(claude(), WorkStatus::Done),
            ),
            event(
                11,
                1_001,
                &w,
                on("core/index"),
                progress(claude(), WorkStatus::Done),
            ),
            event(
                12,
                1_002,
                &w,
                on("core/index"),
                plan("Local index", Some("MVP build")),
            ),
            event(13, 1_003, &w, Refs::default(), pulse(claude(), "One")),
            event(14, 1_004, &w, Refs::default(), pulse(claude(), "Two")),
        ]);
        assert_eq!(
            texts(&finished),
            [
                "Claude Code finished 2 steps, updated 1 plan step, sent 2 progress updates in MVP build"
            ]
        );
    }

    #[test]
    fn plan_updates_count_distinct_steps_not_declarations() {
        let w = agent_writer();
        let burst = feed(vec![
            event(10, 1_000, &w, on("core/wire"), plan("Wire model", None)),
            event(11, 1_001, &w, on("core/index"), plan("Local index", None)),
            event(12, 1_002, &w, on("core/wire"), plan("Wire model v2", None)),
            event(13, 1_003, &w, on("ui/board"), plan("Board", None)),
        ]);
        assert_eq!(texts(&burst), ["Claude Code updated 3 plan steps"]);
    }

    #[test]
    fn a_single_plan_update_is_counted_and_a_single_subject_burst_keeps_its_node() {
        let w = agent_writer();
        let single = feed(vec![event(
            10,
            1_000,
            &w,
            on("core/wire"),
            plan("Wire model", Some("MVP build")),
        )]);
        assert_eq!(
            texts(&single),
            ["Claude Code updated 1 plan step in MVP build"]
        );
        assert_eq!(single[0].icon, "git-branch");
        assert_eq!(single[0].node, Some(node("core/wire")));
        assert_eq!(single[0].detail.as_deref(), Some("Wire model"));

        let same = feed(vec![
            event(
                10,
                1_000,
                &w,
                on("core/wire"),
                progress(claude(), WorkStatus::Started),
            ),
            event(
                11,
                1_060,
                &w,
                on("core/wire"),
                progress(claude(), WorkStatus::Done),
            ),
        ]);
        assert_eq!(texts(&same), ["Claude Code finished 1 step"]);
        assert_eq!(same[0].node, Some(node("core/wire")));
        assert_eq!(same[0].detail.as_deref(), Some("Wire model"));
        assert_eq!(same[0].count, 2);
    }

    #[test]
    fn a_gap_or_another_writer_starts_a_new_item() {
        let codex = agent(OTHER_SESSION, Harness::Codex);
        let items = feed(vec![
            event(
                10,
                1_000,
                &agent_writer(),
                on("core/wire"),
                progress(claude(), WorkStatus::Started),
            ),
            // 121 s later: outside the burst window.
            event(
                11,
                1_121,
                &agent_writer(),
                on("core/wire"),
                progress(claude(), WorkStatus::Done),
            ),
            // Same second, different writer.
            event(
                12,
                1_121,
                &other_writer(),
                on("core/index"),
                progress(codex, WorkStatus::Started),
            ),
        ]);
        assert_eq!(
            texts(&items),
            [
                "Codex started Local index",
                "Claude Code finished Wire model",
                "Claude Code started Wire model",
            ]
        );
    }

    #[test]
    fn questions_answers_acks_and_sessions_always_stand_alone() {
        let w = agent_writer();
        let items = feed(vec![
            event(
                10,
                1_000,
                &w,
                Refs::default(),
                pulse(claude(), "Session started"),
            ),
            event(
                11,
                1_001,
                &w,
                on("core/wire"),
                progress(claude(), WorkStatus::Started),
            ),
            // The ask's id and seq sort before the events that reference it.
            event(ASK, 1_002, &w, on("core/wire"), ask()),
            event(
                20,
                1_003,
                &operator(),
                on_ask("core/wire"),
                EventPayload::DecisionAnswer(DecisionAnswer {
                    option: OptionId::parse("a").expect("option"),
                    done_when: Vec::new(),
                    expected_eta: None,
                    note: None,
                    own_answer: None,
                }),
            ),
            event(
                21,
                1_004,
                &w,
                Refs {
                    answer: Some(id(20)),
                    ..on_ask("core/wire")
                },
                EventPayload::DecisionAck(DecisionAck {
                    agent: claude(),
                    applied_option: OptionId::parse("a").expect("option"),
                    restarted: Vec::new(),
                }),
            ),
            event(
                22,
                1_005,
                &w,
                on("core/wire"),
                progress(claude(), WorkStatus::Done),
            ),
            event(
                23,
                1_006,
                &steward(),
                Refs::default(),
                EventPayload::SessionLost(SessionLost {
                    session: SessionId::parse(SESSION).expect("session"),
                    process_start: 1,
                    reason: LostReason::Exited,
                    exit_code: None,
                }),
            ),
        ]);
        assert_eq!(
            texts(&items),
            [
                "Claude Code session ended",
                "Claude Code finished Wire model",
                "Claude Code applied your answer",
                "You answered \u{201c}How should answers reach idle agents?\u{201d} \u{2192} Use harness adapters",
                "Claude Code asked you: How should answers reach idle agents?",
                "Claude Code started Wire model",
                "Claude Code started a session",
            ]
        );
        let icons: Vec<&str> = items.iter().map(|item| item.icon).collect();
        assert_eq!(
            icons,
            [
                "pause-circle",
                "check",
                "badge-check",
                "user",
                "help-circle",
                "clock",
                "bot"
            ]
        );
        let answer = &items[3];
        assert_eq!(answer.actor, "You");
        assert_eq!(answer.actor_kind, ActorKind::Human);
        let lost = &items[0];
        assert_eq!(lost.actor, "Agendar");
        assert_eq!(lost.actor_kind, ActorKind::System);
        assert_eq!(
            items[2].detail.as_deref(),
            Some("How should answers reach idle agents?")
        );
        assert_eq!(items[6].node, None);
    }

    #[test]
    fn a_pulse_on_a_step_carries_the_step_title_as_detail() {
        let items = feed(vec![event(
            10,
            1_000,
            &agent_writer(),
            Refs {
                attempt: Some(id(9)),
                ..on("core/index")
            },
            pulse(claude(), "Schema compiles"),
        )]);
        assert_eq!(texts(&items), ["Claude Code: Schema compiles"]);
        assert_eq!(items[0].icon, "radio");
        assert_eq!(items[0].detail.as_deref(), Some("Local index"));
        assert_eq!(items[0].node, Some(node("core/index")));
    }

    #[test]
    fn the_feed_is_newest_first_and_capped() {
        let w = agent_writer();
        // Unordered input; every ask stands alone, so each is one item.
        let mut extra: Vec<Event> = (0..50)
            .map(|n| {
                event(
                    100 + n,
                    i64::from(n) * 10 + 1_000,
                    &w,
                    on("core/wire"),
                    ask(),
                )
            })
            .collect();
        extra.reverse();
        let items = feed(extra);
        assert_eq!(items.len(), ACTIVITY_MAX_ITEMS);
        assert_eq!(items[0].ts, at(1_490));
        assert!(items.windows(2).all(|pair| pair[0].ts > pair[1].ts));
        assert_eq!(items[ACTIVITY_MAX_ITEMS - 1].ts, at(1_100));
    }

    #[test]
    fn titles_fall_back_to_the_node_path_and_harness_labels_are_fixed() {
        let pi = agent(SESSION, Harness::Pi);
        let items = feed(vec![event(
            10,
            1_000,
            &agent_writer(),
            on("core/unknown"),
            progress(pi, WorkStatus::Dropped),
        )]);
        assert_eq!(texts(&items), ["pi dropped core/unknown"]);
        assert_eq!(items[0].icon, "x");
        assert_eq!(harness_label(Harness::Generic), "Agent");
        assert_eq!(harness_label(Harness::Herdr), "Herdr");
        assert_eq!(harness_label(Harness::Codex), "Codex");
    }
}
