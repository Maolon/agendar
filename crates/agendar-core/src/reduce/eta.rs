//! ETA projection and human-wait interval arithmetic.
//!
//! The projection replays causal transitions instead of consulting final
//! progress: an ask on the attempt's own node holds it for its whole
//! human-wait interval, while an ask on an upstream feature holds it only while
//! the accepted dependency edge is live and that upstream attempt is still
//! pending. Restarts, completions, answers and later agenda edits therefore
//! close or keep open the intervals they actually caused; a historical wait is
//! never erased by a later projection. Late is measured against
//! the calibrated upper bound when the project has one, otherwise against the
//! agent's own bound.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::event::WorkStatus;
use crate::model::ids::{EventId, NodePath, UtcTime};
use crate::model::state::{EtaState, TimeRange};
use crate::reduce::graph;
use crate::reduce::plan::{AttemptFold, FeatureFold};

/// One human-wait interval contributed by a decision, replayed from the fold.
#[derive(Debug, Clone)]
pub struct WaitAsk {
    pub node: NodePath,
    pub source: EventId,
    pub start: UtcTime,
    /// `None` while the ask still awaits a human.
    pub end: Option<UtcTime>,
}

/// One accepted-dependency snapshot of one feature, effective from `at` until
/// the next step (or forever). `upstreams` is the transitive closure of the
/// accepted edges at that moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyStep {
    pub at: UtcTime,
    pub upstreams: BTreeSet<NodePath>,
}

/// Reconstruct the accepted dependency graph over causal time.
///
/// Each winning `depends_on` patch is one transition. Between transitions the
/// map holds the transitive upstream closure of every known node, so a wait can
/// ask which prerequisite edges were live when a human was asked. A dependency
/// that is added later never backdates a hold, and one that is removed later
/// closes an open hold at the removal instead of erasing it.
pub fn dependency_steps(
    features: &BTreeMap<NodePath, FeatureFold>,
    known: &BTreeSet<NodePath>,
) -> BTreeMap<NodePath, Vec<DependencyStep>> {
    struct Transition {
        ts: UtcTime,
        changes: Vec<(NodePath, Vec<NodePath>)>,
    }
    let mut updates: BTreeMap<usize, Transition> = BTreeMap::new();
    for (node, feature) in features {
        for change in &feature.depends_on_history {
            let entry = updates.entry(change.order).or_insert(Transition {
                ts: change.ts,
                changes: Vec::new(),
            });
            entry
                .changes
                .push((node.clone(), change.depends_on.clone()));
        }
    }
    let mut declared: BTreeMap<NodePath, Vec<NodePath>> = BTreeMap::new();
    let mut steps: BTreeMap<NodePath, Vec<DependencyStep>> = BTreeMap::new();
    let mut last_at: Option<UtcTime> = None;
    for (_, transition) in updates {
        for (node, depends_on) in transition.changes {
            declared.insert(node, depends_on);
        }
        // Keep the transition times monotonic even when replicated clocks skew:
        // an interval whose start is after its end would silently vanish.
        let at = last_at.map_or(transition.ts, |previous| transition.ts.max(previous));
        last_at = Some(at);
        for (node, upstreams) in accepted_closures(&declared, known) {
            steps
                .entry(node)
                .or_default()
                .push(DependencyStep { at, upstreams });
        }
    }
    steps
}

/// Accepted edges and their transitive upstream closure at one snapshot.
fn accepted_closures(
    declared: &BTreeMap<NodePath, Vec<NodePath>>,
    known: &BTreeSet<NodePath>,
) -> BTreeMap<NodePath, BTreeSet<NodePath>> {
    let mut adjacency: BTreeMap<NodePath, BTreeSet<NodePath>> = BTreeMap::new();
    for (downstream, upstreams) in declared {
        for upstream in upstreams {
            if upstream == downstream || !known.contains(upstream) {
                continue;
            }
            if graph::reachable(&adjacency, downstream, upstream) {
                continue;
            }
            adjacency
                .entry(upstream.clone())
                .or_default()
                .insert(downstream.clone());
        }
    }
    // One reversed view serves every node's ancestor walk.
    let mut reverse: BTreeMap<NodePath, BTreeSet<NodePath>> = BTreeMap::new();
    for (upstream, downstreams) in &adjacency {
        for downstream in downstreams {
            reverse
                .entry(downstream.clone())
                .or_default()
                .insert(upstream.clone());
        }
    }
    known
        .iter()
        .map(|node| {
            let upstreams = graph::descendants(&reverse, node).into_iter().collect();
            (node.clone(), upstreams)
        })
        .collect()
}

/// Periods during which `upstream` was an accepted (transitive) prerequisite of
/// the feature described by `steps`.
fn upstream_spans(
    steps: &[DependencyStep],
    upstream: &NodePath,
) -> Vec<(UtcTime, Option<UtcTime>)> {
    let mut spans = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        if !step.upstreams.contains(upstream) {
            continue;
        }
        spans.push((step.at, steps.get(index + 1).map(|next| next.at)));
    }
    spans
}

/// Was the upstream feature satisfied at `at`?
///
/// The latest attempt started at or before `at` decides: a completed attempt
/// means the downstream was not blocked by this prerequisite, while a restart
/// that has not finished yet reopens the hold.
fn satisfied_at(feature: &FeatureFold, at: UtcTime) -> bool {
    let latest = feature
        .attempts
        .values()
        .filter(|attempt| attempt.started_at.millis() <= at.millis())
        .max_by_key(|attempt| (attempt.started_at.millis(), attempt.order));
    match latest {
        Some(attempt) => {
            attempt.status != WorkStatus::Dropped
                && attempt
                    .first_completion
                    .is_some_and(|(_, completed)| completed.millis() <= at.millis())
        }
        None => false,
    }
}

/// First successful completion of the upstream at or after `at`.
///
/// This is the transition that closes a prerequisite hold; completions before
/// the ask cannot resolve it, and a dropped attempt never does.
fn completion_after(feature: &FeatureFold, at: UtcTime) -> Option<UtcTime> {
    feature
        .attempts
        .values()
        .filter(|attempt| attempt.status != WorkStatus::Dropped)
        .filter_map(|attempt| attempt.first_completion.map(|(_, completed)| completed))
        .filter(|completed| completed.millis() >= at.millis())
        .min()
}

/// Reconstruct the human-wait intervals of one feature.
///
/// An ask on the feature's own node holds it until the ask is cleared. An ask on
/// a prerequisite holds it only while that dependency edge is live and the
/// prerequisite is still pending; the hold ends at the answer, at the edge
/// removal or at the prerequisite's completion transition, whichever is first.
/// Closed intervals stay in the list: a later restarted or re-planned upstream
/// must not rewrite time the human already cost.
pub fn waits(
    node: &NodePath,
    steps: &[DependencyStep],
    asks: &[WaitAsk],
    features: &BTreeMap<NodePath, FeatureFold>,
) -> Vec<(UtcTime, Option<UtcTime>)> {
    let mut intervals = Vec::new();
    for ask in asks {
        if &ask.node == node {
            intervals.push((ask.start, ask.end));
            continue;
        }
        let Some(feature) = features.get(&ask.node) else {
            continue;
        };
        for (span_start, span_end) in upstream_spans(steps, &ask.node) {
            let start = span_start.max(ask.start);
            let clipped = match (span_end, ask.end) {
                (Some(end), Some(ask_end)) => Some(end.min(ask_end)),
                (Some(end), None) => Some(end),
                (None, Some(ask_end)) => Some(ask_end),
                (None, None) => None,
            };
            if clipped.is_some_and(|end| end.millis() <= start.millis()) {
                continue;
            }
            if satisfied_at(feature, start) {
                continue;
            }
            let end = match (clipped, completion_after(feature, start)) {
                (Some(end), Some(resolved)) => Some(end.min(resolved)),
                (Some(end), None) => Some(end),
                (None, Some(resolved)) => Some(resolved),
                (None, None) => None,
            };
            if end.is_some_and(|end| end.millis() <= start.millis()) {
                continue;
            }
            intervals.push((start, end));
        }
    }
    intervals
}

/// Project the ETA of one attempt.
///
/// `ratio` is the project's calibrated median for the attempt's harness, if the
/// project is eligible. `None` keeps the agent's original range.
pub fn compute(
    attempt: &AttemptFold,
    pauses: &[(UtcTime, Option<UtcTime>)],
    as_of: UtcTime,
    ratio: Option<u32>,
) -> EtaState {
    let anchor = attempt.eta_ts;
    let end = attempt.status_ts.unwrap_or(as_of).max(anchor);
    let paused_s = union_seconds(pauses, anchor, end, as_of);
    let paused =
        attempt.status == WorkStatus::Started && pauses.iter().any(|(_, finish)| finish.is_none());
    let agent_finish = TimeRange {
        earliest: anchor.add_seconds(
            i64::from(attempt.eta.min_s.get()) + i64::try_from(paused_s).unwrap_or(i64::MAX),
        ),
        latest: anchor.add_seconds(
            i64::from(attempt.eta.max_s.get()) + i64::try_from(paused_s).unwrap_or(i64::MAX),
        ),
    };
    let likely_finish = ratio
        .filter(|_| attempt.status != WorkStatus::Dropped)
        .map(|ratio| TimeRange {
            earliest: anchor.add_seconds(
                i64::try_from(scaled_seconds(attempt.eta.min_s.get(), ratio)).unwrap_or(i64::MAX)
                    + i64::try_from(paused_s).unwrap_or(i64::MAX),
            ),
            latest: anchor.add_seconds(
                i64::try_from(scaled_seconds(attempt.eta.max_s.get(), ratio)).unwrap_or(i64::MAX)
                    + i64::try_from(paused_s).unwrap_or(i64::MAX),
            ),
        });
    let bound = likely_finish
        .as_ref()
        .map(|range| range.latest)
        .unwrap_or(agent_finish.latest);
    let late_by_s = if attempt.status_event.is_none() && !paused && as_of > bound {
        u64::try_from((as_of.millis() - bound.millis()) / 1000).unwrap_or(0)
    } else {
        0
    };
    EtaState {
        agent_range: attempt.eta,
        anchor,
        paused_s,
        agent_finish,
        likely_finish,
        ratio_milli: ratio,
        late_by_s,
        paused,
    }
}

/// Apply a milli ratio to a whole number of seconds, rounding half up.
pub fn scaled_seconds(seconds: u32, ratio_milli: u32) -> u64 {
    (u64::from(seconds) * u64::from(ratio_milli) + 500) / 1000
}

/// Measure of the union of wait intervals clipped to `[anchor, end]`.
pub fn union_seconds(
    pauses: &[(UtcTime, Option<UtcTime>)],
    anchor: UtcTime,
    end: UtcTime,
    as_of: UtcTime,
) -> u64 {
    union_millis(pauses, anchor, end, as_of) / 1000
}

/// Measure of the union of wait intervals in milliseconds.
pub fn union_millis(
    pauses: &[(UtcTime, Option<UtcTime>)],
    anchor: UtcTime,
    end: UtcTime,
    as_of: UtcTime,
) -> u64 {
    let mut intervals: Vec<(i64, i64)> = pauses
        .iter()
        .map(|(start, finish)| {
            let start = (*start).max(anchor).millis();
            let finish = finish.unwrap_or(as_of).min(end).millis();
            (start, finish)
        })
        .filter(|(start, finish)| finish > start)
        .collect();
    intervals.sort();
    let mut total_ms: i64 = 0;
    let mut current: Option<(i64, i64)> = None;
    for (start, finish) in intervals {
        match current {
            None => current = Some((start, finish)),
            Some((current_start, current_finish)) => {
                if start <= current_finish {
                    current = Some((current_start, current_finish.max(finish)));
                } else {
                    total_ms = total_ms.saturating_add(current_finish - current_start);
                    current = Some((start, finish));
                }
            }
        }
    }
    if let Some((start, finish)) = current {
        total_ms = total_ms.saturating_add(finish - start);
    }
    u64::try_from(total_ms.max(0)).unwrap_or(u64::MAX)
}
