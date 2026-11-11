//! What one agent session still owes the board, for `agendar settle` and the
//! one-line debt on its boundary commands.
//!
//! Only what this session can act on, one line each with the command and,
//! where a time rule applies, its default and deadline:
//!
//! - the steps it holds (doing or sent back), past their ETA or with the
//!   instant the claim ends if no word comes;
//! - its deliveries in review, as a count (the owner holds them);
//! - steps it may take: free (abandoned) or handed off by the owner, with the
//!   answers it must apply when the step's question was already answered;
//! - its questions on closed steps, until they fold;
//! - the steps it was on that the owner dropped, for a day: nothing is owed,
//!   it only has to stop;
//! - planned steps: always the count, and a few candidates to look at (leave,
//!   patch or drop with a reason), each shown once a week per project, in the
//!   order of the signals that a step may not be needed: every step that
//!   depends on it is finished, the rest of its milestone is closed, only
//!   agents wrote it; then the oldest last touch.
//!
//! A pure function of the projection, the ordered events, `as_of` and the
//! local shown-cursor. Nothing here writes.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::model::event::Event;
use crate::model::ids::{EventId, Mode, NodePath, SessionId, UtcTime};
use crate::model::state::{
    CLAIM_EXPIRY_S, DecisionLifecycle, FeatureState, Lease, MOOT_FOLD_S, NeedsKind, Phase,
    Progress, ProjectState,
};
use crate::reduce::attention::{free_for, runnable_applying, runnable_nodes};
use crate::reduce::nudges::short_duration;
use crate::reduce::plan::{Touch, human_decided};

/// Planned-step candidates per `settle`.
pub const CANDIDATES: usize = 5;
/// Steps a `settle` may offer to take, at most.
pub const TAKE_LINES: usize = 5;
/// A candidate shown by `settle` is not shown again in this project for this
/// long.
pub const SHOWN_FOR_S: i64 = 7 * 24 * 60 * 60;

/// One step this session holds.
#[derive(Debug, Clone, Serialize)]
pub struct Yours {
    pub node: NodePath,
    pub attempt: EventId,
    /// Past its ETA bound, with no hold by a question or a pause.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub past_eta: bool,
    pub say: String,
}

/// One step this session may take.
#[derive(Debug, Clone, Serialize)]
pub struct Take {
    pub node: NodePath,
    /// `free` (its claim ended), `granted` (the owner answered this agent's
    /// takeover question) or `handed_off` (a hand-off recorded by an older
    /// version of the app).
    pub why: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub applies: Vec<EventId>,
    pub say: String,
}

/// One of this session's questions on a closed step, before it folds.
#[derive(Debug, Clone, Serialize)]
pub struct Moot {
    pub ask: EventId,
    pub node: NodePath,
    pub say: String,
}

/// A step this session was on that the owner dropped.
#[derive(Debug, Clone, Serialize)]
pub struct Dropped {
    pub node: NodePath,
    pub say: String,
}

/// One planned step to look at.
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub node: NodePath,
    pub title: String,
    /// The owner wrote or decided something on it: dropping it is theirs.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub owners: bool,
    pub say: String,
}

/// Everything this session can act on.
#[derive(Debug, Clone, Default)]
pub struct Owed {
    pub yours: Vec<Yours>,
    /// Its deliveries waiting for the owner's review: nothing to do.
    pub in_review: usize,
    /// Every step it may take (the output keeps the first [`TAKE_LINES`]).
    pub take: Vec<Take>,
    pub questions: Vec<Moot>,
    /// Its steps the owner dropped in the last [`MOOT_FOLD_S`]: nothing to
    /// do, so never part of the debt.
    pub dropped: Vec<Dropped>,
    /// Planned steps in the project.
    pub planned: usize,
    /// Planned steps not shown in the last week, in the order to show them.
    pub candidates: Vec<Candidate>,
}

impl Owed {
    /// Whether anything is owed: a held step, a step to take, a question on a
    /// closed step, or planned steps not looked at this week. Deliveries in
    /// review owe nothing.
    pub fn is_zero(&self) -> bool {
        self.yours.is_empty()
            && self.take.is_empty()
            && self.questions.is_empty()
            && self.candidates.is_empty()
    }

    /// `open: 2 yours (1 past ETA), 1 free to take, 26 planned`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.yours.is_empty() {
            let late = self.yours.iter().filter(|step| step.past_eta).count();
            parts.push(if late > 0 {
                format!("{} yours ({late} past ETA)", self.yours.len())
            } else {
                format!("{} yours", self.yours.len())
            });
        }
        if !self.take.is_empty() {
            let free = self.take.iter().all(|step| step.why == "free");
            parts.push(format!(
                "{} {}to take",
                self.take.len(),
                if free { "free " } else { "" }
            ));
        }
        if !self.questions.is_empty() {
            let n = self.questions.len();
            parts.push(format!(
                "{n} question{} on a closed step",
                if n == 1 { "" } else { "s" }
            ));
        }
        if self.planned > 0 {
            parts.push(format!("{} planned", self.planned));
        }
        if parts.is_empty() {
            "open: nothing".to_owned()
        } else {
            format!("open: {}", parts.join(", "))
        }
    }

    /// The debt line for a boundary command, when something is owed.
    pub fn line(&self) -> Option<String> {
        (!self.is_zero()).then(|| format!("{} · agendar settle", self.summary()))
    }
}

/// The ETA bound of a step in progress: when the agent said it would finish.
fn eta_bound(feature: &FeatureState) -> Option<UtcTime> {
    feature.eta.as_ref().map(|eta| {
        eta.likely_finish
            .as_ref()
            .map_or(eta.agent_finish.latest, |range| range.latest)
    })
}

fn secs_between(from: UtcTime, to: UtcTime) -> u64 {
    u64::try_from((to.millis() - from.millis()).max(0) / 1000).unwrap_or(0)
}

/// A step this session holds open (doing or sent back), or `None`.
fn yours(feature: &FeatureState, session: &SessionId, as_of: UtcTime) -> Option<Yours> {
    if feature.session.as_ref() != Some(session) || feature.progress != Progress::Doing {
        return None;
    }
    // In review, paused or handed off: the owner holds it.
    let claim = feature.claim.as_ref()?;
    let attempt = feature.attempt.clone()?;
    let node = feature.node.as_str();
    let paused = feature.eta.as_ref().is_some_and(|eta| eta.paused);
    let bound = eta_bound(feature);
    let past = bound.filter(|bound| *bound <= as_of);
    let past_eta = claim.waiting_on.is_none() && !paused && past.is_some();
    let ids = format!("--node {node} --attempt {attempt}");
    let sent_back = if feature.phase == Some(Phase::SentBack) {
        "sent back; "
    } else {
        ""
    };
    let say = if let Some(ask) = &claim.waiting_on {
        format!("{node}: {sent_back}held while the owner answers {ask}")
    } else if paused {
        format!("{node}: {sent_back}held, its ETA is paused")
    } else if claim.state == Lease::Free {
        format!(
            "{node}: {sent_back}freed {} ago (no word); open to any agent. Keep it: pulse {ids} --eta <range>; or progress done|dropped {ids}",
            short_duration(secs_between(claim.since, as_of))
        )
    } else {
        let expiry = bound
            .unwrap_or(claim.last_word)
            .max(claim.last_word)
            .add_seconds(CLAIM_EXPIRY_S);
        let freed = format!(
            "freed in {} if no word",
            short_duration(secs_between(as_of, expiry))
        );
        match past {
            Some(bound) => format!(
                "{node}: {sent_back}{} past ETA, {freed}: pulse {ids} --eta <range>, deliver, or progress done|dropped {ids}",
                short_duration(secs_between(bound, as_of))
            ),
            None => format!(
                "{node}: {sent_back}ETA in {}, then {freed}",
                short_duration(bound.map_or(0, |bound| secs_between(as_of, bound)))
            ),
        }
    };
    Some(Yours {
        node: feature.node.clone(),
        attempt,
        past_eta,
        say,
    })
}

/// Who wrote each event, by id, for the few the lines need.
fn writers(ordered: &[Event]) -> BTreeMap<&EventId, &Event> {
    ordered.iter().map(|event| (&event.id, event)).collect()
}

/// What this session owes in `project` at `as_of`. `shown` is the local
/// cursor: when each planned step was last shown in this project.
pub fn owed(
    project: &ProjectState,
    ordered: &[Event],
    session: &SessionId,
    as_of: UtcTime,
    shown: &BTreeMap<NodePath, UtcTime>,
) -> Owed {
    let mut owed = Owed::default();
    for feature in project.features.values() {
        if let Some(step) = yours(feature, session, as_of) {
            owed.yours.push(step);
        } else if feature.session.as_ref() == Some(session)
            && feature.progress == Progress::Doing
            && feature.phase == Some(Phase::InReview)
        {
            owed.in_review += 1;
        }
    }

    // Its steps the owner dropped, for as long as a question on a closed
    // step stays in view.
    for feature in project.features.values() {
        let Some(drop) = &feature.owner_drop else {
            continue;
        };
        if feature.session.as_ref() != Some(session) || drop.at.add_seconds(MOOT_FOLD_S) <= as_of {
            continue;
        }
        let note = drop
            .note
            .as_deref()
            .map(|note| format!(" ({note})"))
            .unwrap_or_default();
        owed.dropped.push(Dropped {
            node: feature.node.clone(),
            say: format!(
                "{}: dropped by the owner {} ago{note}. Stop work on it; nothing to report",
                feature.node,
                short_duration(secs_between(drop.at, as_of))
            ),
        });
    }

    // Steps to take: free, or handed off by the owner; with the answers to
    // apply when the step's question was answered for someone else.
    let mut startable: BTreeMap<NodePath, Vec<EventId>> = runnable_nodes(session, project)
        .into_iter()
        .map(|node| (node, Vec::new()))
        .collect();
    startable.extend(runnable_applying(session, project));
    for (node, applies) in startable {
        let Some(feature) = project.features.get(&node) else {
            continue;
        };
        let theirs = feature
            .session
            .as_ref()
            .is_some_and(|owner| owner != session);
        let why = if feature.handoff_pending && (theirs || feature.session.is_none()) {
            "handed_off"
        } else if crate::reduce::attention::granted_to(session, feature).is_some() {
            "granted"
        } else if free_for(session, feature) {
            "free"
        } else {
            continue;
        };
        let mut command = format!("progress started --node {node} --eta <range>");
        for answer in &applies {
            command.push_str(&format!(" --applies {answer}"));
        }
        let since = if why == "free" {
            feature
                .claim
                .as_ref()
                .map(|claim| {
                    format!(
                        "abandoned {} ago",
                        short_duration(secs_between(claim.since, as_of))
                    )
                })
                .unwrap_or_else(|| "abandoned".to_owned())
        } else if why == "granted" {
            "the owner let you continue it".to_owned()
        } else {
            "handed off by the owner".to_owned()
        };
        let say = if applies.is_empty() {
            format!("you may take {node} ({since}): {command}")
        } else {
            format!("you may continue {node} ({since}); apply its answer: {command}")
        };
        owed.take.push(Take {
            node,
            why,
            applies,
            say,
        });
    }

    // Its questions on closed steps, inside the window before they fold.
    let by_id = writers(ordered);
    for row in &project.needs_you {
        let Some(closed) = row.closed_step.as_ref().filter(|closed| !closed.folded) else {
            continue;
        };
        if row.kind != NeedsKind::Decide {
            continue;
        }
        let asker = by_id
            .get(&row.source)
            .and_then(|event| event.writer.session());
        if asker != Some(session) {
            continue;
        }
        let Some(node) = row.node.clone() else {
            continue;
        };
        let folds_in = secs_between(as_of, closed.since.add_seconds(MOOT_FOLD_S));
        owed.questions.push(Moot {
            ask: row.source.clone(),
            say: format!(
                "your question {} on {node} is on a closed step: withdraw {} --reason <why>, or it folds in {}",
                row.source,
                row.source,
                short_duration(folds_in)
            ),
            node,
        });
    }

    // Planned steps: the count, and the candidates not shown this week.
    let mut touches: BTreeMap<NodePath, Touch> = BTreeMap::new();
    let mut touched: BTreeSet<&NodePath> = BTreeSet::new();
    for event in ordered {
        if event.project != project.summary.slug {
            continue;
        }
        Touch::record(&mut touches, event);
        if let Some(node) = event.refs.node.as_ref()
            && event.writer.session() == Some(session)
        {
            touched.insert(node);
        }
    }
    let decided = human_decided(project.receipts.values());
    let asked: BTreeSet<&NodePath> = project
        .decisions
        .values()
        .filter(|decision| {
            decision.lifecycle == DecisionLifecycle::Open && decision.mode == Mode::Ask
        })
        .map(|decision| &decision.node)
        .collect();
    let planned: Vec<&FeatureState> = project
        .features
        .values()
        .filter(|feature| feature.progress == Progress::Planned)
        .collect();
    owed.planned = planned.len();
    let finished = |node: &NodePath| {
        project
            .features
            .get(node)
            .is_some_and(|feature| matches!(feature.progress, Progress::Done | Progress::Verified))
    };
    // Signals that a planned step may no longer be needed, strongest
    // first: every step that depends on it is finished; the rest of its
    // milestone is closed; only agents wrote it. Then the oldest last touch.
    let mut candidates: Vec<([bool; 3], UtcTime, &FeatureState)> = planned
        .into_iter()
        .filter(|feature| !asked.contains(&feature.node))
        .filter(|feature| {
            shown
                .get(&feature.node)
                .is_none_or(|at| at.add_seconds(SHOWN_FOR_S) <= as_of)
        })
        .map(|feature| {
            let mut dependents = project
                .features
                .values()
                .filter(|other| other.accepted_depends_on.contains(&feature.node))
                .peekable();
            let dependents_done =
                dependents.peek().is_some() && dependents.all(|other| finished(&other.node));
            let mut siblings = project
                .features
                .values()
                .filter(|other| other.node != feature.node)
                .filter(|other| feature.milestone.is_some() && other.milestone == feature.milestone)
                .peekable();
            let milestone_done = siblings.peek().is_some()
                && siblings
                    .all(|other| finished(&other.node) || other.progress == Progress::Dropped);
            let owners = touches.get(&feature.node).is_some_and(|touch| touch.owner)
                || decided.contains(&feature.node);
            let touch = touches.get(&feature.node).map_or(as_of, |touch| touch.last);
            ([!dependents_done, !milestone_done, owners], touch, feature)
        })
        .collect();
    candidates.sort_by(|left, right| {
        (left.0, left.1, &left.2.node).cmp(&(right.0, right.1, &right.2.node))
    });
    owed.candidates = candidates
        .into_iter()
        .map(|(signals, touch, feature)| {
            let node = &feature.node;
            let owners = signals[2];
            let age = short_duration(secs_between(touch, as_of));
            let say = if owners {
                format!(
                    "{node} (untouched {age}): leave, or patch it; owner's step: ask via a card to drop it"
                )
            } else {
                format!(
                    "{node} (untouched {age}): leave, patch (feature declare), or progress dropped --node {node} --note <reason>"
                )
            };
            Candidate {
                node: node.clone(),
                title: feature.title.clone(),
                owners,
                say,
            }
        })
        .collect();
    owed
}
