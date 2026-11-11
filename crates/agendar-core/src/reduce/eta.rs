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

use crate::model::event::WorkStatus;
use crate::model::ids::{EventId, NodePath, UtcTime};
use crate::model::state::{EtaState, TimeRange};
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

/// Was the upstream feature satisfied at `at`?
///
/// The latest attempt started at or before `at` decides: a completed attempt
/// means the downstream was not blocked by this prerequisite, while a restart
/// that has not finished yet reopens the hold.
pub(crate) fn satisfied_at(feature: &FeatureFold, at: UtcTime) -> bool {
    let latest = feature
        .attempts
        .values()
        .filter(|attempt| attempt.started_at.millis() <= at.millis())
        .max_by_key(|attempt| (attempt.started_at.millis(), attempt.order));
    match latest {
        Some(attempt) => {
            attempt.status != WorkStatus::Dropped
                && attempt
                    .completion()
                    .is_some_and(|(_, completed)| completed.millis() <= at.millis())
        }
        None => false,
    }
}

/// First successful completion of the upstream at or after `at`.
///
/// This is the transition that closes a prerequisite hold; completions before
/// the ask cannot resolve it, and a dropped attempt never does.
pub(crate) fn completion_after(feature: &FeatureFold, at: UtcTime) -> Option<UtcTime> {
    feature
        .attempts
        .values()
        .filter(|attempt| attempt.status != WorkStatus::Dropped)
        .filter_map(|attempt| attempt.completion().map(|(_, completed)| completed))
        .filter(|completed| completed.millis() >= at.millis())
        .min()
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
    // A started attempt the owner accepted ends at the delivery it rests on.
    let accepted = attempt
        .accepted_at()
        .filter(|_| attempt.status == WorkStatus::Started)
        .map(|(_, at)| at);
    let end = accepted.or(attempt.status_ts).unwrap_or(as_of).max(anchor);
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
    let late_by_s =
        if attempt.status_event.is_none() && accepted.is_none() && !paused && as_of > bound {
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
