//! When the whole project finishes, not just the step in progress. A
//! host-local projection beside the header statistics; never canonical.
//!
//! Steps in progress count with their own remaining ETA window (calibrated
//! when the harness is). Steps not started yet have no estimate of their own,
//! so each counts as one typical finished step: the 25th..75th percentile of
//! the project's recent timed steps. The work is split between the agents
//! working now. With nobody working there is no honest clock time, only the
//! amount of work left.

use serde::Serialize;

use crate::model::ids::{SessionId, UtcTime};
use crate::model::state::{Lease, Liveness, Phase, Progress, ProjectState, TimeRange};

/// Finished steps needed before a typical step duration means anything.
pub const MIN_BASIS_STEPS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Forecast {
    /// Steps not finished yet (in progress or not started; dropped left out).
    pub open_steps: u32,
    /// Of those: an agent's claim stands (held or stalled).
    pub in_progress: u32,
    /// Of those: the claim ended (session lost or silent past its lease).
    pub abandoned: u32,
    /// Of those: nobody started them.
    pub not_started: u32,
    /// Agents working now; the remaining work is split between them.
    pub agents: u32,
    /// Remaining agent work, low and high, in milliseconds.
    pub work_ms: [u64; 2],
    /// When the project finishes if the work goes on now. Only while an agent
    /// is working.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish: Option<TimeRange>,
    /// The duration one not-started step counts for, low and high (ms).
    pub step_ms: [u64; 2],
    /// Finished steps that duration comes from.
    pub basis_steps: u32,
}

/// The forecast for `project` at `now`, from the sorted durations (ms) of its
/// recent timed steps. `None` when nothing is open or too few steps finished.
pub fn forecast(
    project: &ProjectState,
    sorted_durations: &[u64],
    now: UtcTime,
) -> Option<Forecast> {
    if sorted_durations.len() < MIN_BASIS_STEPS {
        return None;
    }
    let low = percentile(sorted_durations, 25);
    let median = percentile(sorted_durations, 50);
    let high = percentile(sorted_durations, 75);
    let mut open = 0_u32;
    let mut split = Split::default();
    let mut work = [0_u64, 0_u64];
    let mut agents: Vec<&SessionId> = Vec::new();
    for feature in project.features.values() {
        if matches!(
            feature.progress,
            Progress::Done | Progress::Verified | Progress::Dropped
        ) || feature.phase == Some(Phase::InReview)
        {
            // A step in review waits for the owner, not for agent work.
            continue;
        }
        open += 1;
        split.count(feature);
        // A step whose claim ended has nobody on it, even if its session is
        // busy elsewhere.
        let running = feature.progress == Progress::Doing
            && feature.liveness == Liveness::Live
            && feature
                .claim
                .as_ref()
                .is_none_or(|claim| claim.state != Lease::Free);
        match (running, feature.eta.as_ref()) {
            (true, Some(eta)) => {
                let window = eta.likely_finish.as_ref().unwrap_or(&eta.agent_finish);
                let left = |at: UtcTime| u64::try_from(at.millis() - now.millis()).unwrap_or(0);
                if now.millis() > window.latest.millis() {
                    // Already past its window: count up to a typical step more.
                    work[1] = work[1].saturating_add(median);
                } else {
                    work[0] = work[0].saturating_add(left(window.earliest));
                    work[1] = work[1].saturating_add(left(window.latest));
                }
            }
            _ => {
                work[0] = work[0].saturating_add(low);
                work[1] = work[1].saturating_add(high);
            }
        }
        if running
            && let Some(session) = feature.session.as_ref()
            && !agents.contains(&session)
        {
            agents.push(session);
        }
    }
    if open == 0 {
        return None;
    }
    let working = u64::try_from(agents.len()).unwrap_or(1);
    let finish = (working > 0).then(|| TimeRange {
        earliest: now.add_millis(i64::try_from(work[0] / working).unwrap_or(i64::MAX)),
        latest: now.add_millis(i64::try_from(work[1] / working).unwrap_or(i64::MAX)),
    });
    Some(Forecast {
        open_steps: open,
        in_progress: split.in_progress,
        abandoned: split.abandoned,
        not_started: split.not_started,
        agents: u32::try_from(agents.len()).unwrap_or(u32::MAX),
        work_ms: work,
        finish,
        step_ms: [low, high],
        basis_steps: u32::try_from(sorted_durations.len()).unwrap_or(u32::MAX),
    })
}

/// Work left by who stands behind it.
#[derive(Debug, Default)]
struct Split {
    in_progress: u32,
    abandoned: u32,
    not_started: u32,
}

impl Split {
    fn count(&mut self, feature: &crate::model::state::FeatureState) {
        match (
            feature.progress,
            feature.claim.as_ref().map(|claim| claim.state),
        ) {
            (Progress::Planned, _) => self.not_started += 1,
            (_, Some(Lease::Free)) => self.abandoned += 1,
            _ => self.in_progress += 1,
        }
    }
}

/// Nearest-rank percentile of a sorted, non-empty slice.
fn percentile(sorted: &[u64], percent: usize) -> u64 {
    let rank = (percent * sorted.len()).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn nearest_rank_percentiles() {
        let sorted = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(percentile(&sorted, 25), 2);
        assert_eq!(percentile(&sorted, 50), 4);
        assert_eq!(percentile(&sorted, 75), 6);
        assert_eq!(percentile(&[9], 75), 9);
    }
}
