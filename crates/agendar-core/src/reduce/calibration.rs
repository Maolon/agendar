//! Project × harness ETA calibration.
//!
//! A project becomes eligible once five distinct features reached a valid
//! `done`/`verified` completion; one feature finished five times still counts as
//! one. Samples are the first completion of each completed attempt, measured as
//! active duration minus replayed human waits against the ETA the agent
//! declared when it started. Each harness has its own median ratio: a harness
//! with no samples never borrows another harness's history.

use std::collections::BTreeMap;

use crate::model::event::WorkStatus;
use crate::model::ids::{EtaRange, EventId, Harness, NodePath, UtcTime};
use crate::model::state::Calibration;
use crate::reduce::eta;
use crate::reduce::plan::FeatureFold;

/// Distinct finished features required before calibration is applied.
pub const MIN_FINISHED_FEATURES: usize = 5;

/// Timed samples a harness needs before its ratio is applied. Fewer are noise: one quick fix or one long umbrella step would
/// scale every later estimate of that harness.
pub const MIN_HARNESS_SAMPLES: usize = 5;

/// Most recent eligible samples per harness.
pub const MAX_SAMPLES: usize = 100;

/// Completions faster than this measure bookkeeping (an import, or a report of
/// work that happened elsewhere), not the work itself: the feature still counts
/// as finished for eligibility, but the attempt is not a duration sample.
pub const MIN_SAMPLE_MS: u64 = 60_000;

/// Lowest displayed ratio (0.1×).
pub const MIN_RATIO_MILLI: u32 = 100;

/// Highest displayed ratio (10×).
pub const MAX_RATIO_MILLI: u32 = 10_000;

/// One measured attempt: neutral evidence, not yet a project calibration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub feature: NodePath,
    pub attempt: EventId,
    pub harness: Harness,
    /// `O` index of the first completion; samples order by it.
    pub completion_order: usize,
    pub estimate_mid_ms: u64,
    pub actual_ms: u64,
    /// Unclamped actual/estimate ratio in milli.
    pub ratio_milli: u64,
}

/// The calibration evidence of one project at one `as_of`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Distinct node paths with at least one valid done/verified completion.
    pub finished_features: usize,
    /// Every eligible attempt sample, in completion `O` order.
    pub samples: Vec<Sample>,
    /// Per-harness medians, empty while the project is ineligible.
    pub calibration: Vec<Calibration>,
}

impl Report {
    pub fn eligible(&self) -> bool {
        self.finished_features >= MIN_FINISHED_FEATURES
    }

    /// The applied ratio for one harness, when the project is eligible and that
    /// harness has at least one sample.
    pub fn ratio_for(&self, harness: Harness) -> Option<u32> {
        self.calibration
            .iter()
            .find(|entry| entry.harness == harness)
            .map(|entry| entry.ratio_milli)
    }
}

/// Round half up: `numerator / denominator` for non-negative integers.
fn div_half_up(numerator: u64, denominator: u64) -> u64 {
    if denominator == 0 {
        return 0;
    }
    (numerator + denominator / 2) / denominator
}

/// Milliseconds of the midpoint of an ETA range, rounded half up.
pub fn estimate_mid_ms(range: EtaRange) -> u64 {
    let total = u64::from(range.min_s.get()) + u64::from(range.max_s.get());
    total * 1000 / 2
}

/// Per-sample ratio in milli, rounded half up; `None` when the estimate is 0.
pub fn sample_ratio_milli(actual_ms: u64, estimate_mid_ms: u64) -> Option<u64> {
    if estimate_mid_ms == 0 {
        return None;
    }
    Some(div_half_up(actual_ms.saturating_mul(1000), estimate_mid_ms))
}

/// Median of ratios (already sorted by the caller is not required).
pub fn median_milli(ratios: &[u64]) -> Option<u64> {
    if ratios.is_empty() {
        return None;
    }
    let mut sorted = ratios.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    Some(if sorted.len().is_multiple_of(2) {
        div_half_up(sorted[middle - 1].saturating_add(sorted[middle]), 2)
    } else {
        sorted[middle]
    })
}

/// Bound a raw ratio to the displayed range `[0.1×, 10×]`.
pub fn clamp_ratio(raw: u64) -> u32 {
    u32::try_from(raw)
        .unwrap_or(u32::MAX)
        .clamp(MIN_RATIO_MILLI, MAX_RATIO_MILLI)
}

/// Build the report for one project.
///
/// `waits` returns the replayed human-wait intervals of one attempt; the caller
/// owns the causal reconstruction (see `reduce::waits::by_node`).
pub fn report(
    features: &BTreeMap<NodePath, FeatureFold>,
    waits: &BTreeMap<EventId, Vec<(UtcTime, Option<UtcTime>)>>,
    as_of: UtcTime,
) -> Report {
    let mut finished_features = 0_usize;
    let mut samples: Vec<Sample> = Vec::new();
    for (node, feature) in features {
        let mut finished = false;
        for attempt in feature.attempts.values() {
            if attempt.status == WorkStatus::Dropped {
                // A dropped attempt is abandoned work, never a finished sample or
                // a finished-feature identity.
                continue;
            }
            let Some((order, completed_at)) = attempt.completion() else {
                continue;
            };
            finished = true;
            let end = completed_at.max(attempt.started_at);
            let pauses = waits.get(&attempt.id).map(Vec::as_slice).unwrap_or(&[]);
            let waited_ms = eta::union_millis(pauses, attempt.started_at, end, as_of);
            let actual_ms = u64::try_from(end.millis() - attempt.started_at.millis())
                .unwrap_or(0)
                .saturating_sub(waited_ms);
            if actual_ms < MIN_SAMPLE_MS {
                continue;
            }
            let estimate_mid = estimate_mid_ms(attempt.start_eta);
            let Some(ratio) = sample_ratio_milli(actual_ms, estimate_mid) else {
                continue;
            };
            samples.push(Sample {
                feature: node.clone(),
                attempt: attempt.id.clone(),
                harness: attempt.harness,
                completion_order: order,
                estimate_mid_ms: estimate_mid,
                actual_ms,
                ratio_milli: ratio,
            });
        }
        if finished {
            finished_features += 1;
        }
    }
    samples.sort_by(|left, right| {
        left.completion_order
            .cmp(&right.completion_order)
            .then_with(|| left.attempt.cmp(&right.attempt))
    });

    let mut calibration = Vec::new();
    if finished_features >= MIN_FINISHED_FEATURES {
        // Group by harness in first-sample order, so the list order is stable
        // across a rebuild and independent of enum declaration order.
        let mut per_harness: Vec<(Harness, Vec<u64>)> = Vec::new();
        for sample in &samples {
            match per_harness
                .iter_mut()
                .find(|(harness, _)| *harness == sample.harness)
            {
                Some((_, ratios)) => ratios.push(sample.ratio_milli),
                None => per_harness.push((sample.harness, vec![sample.ratio_milli])),
            }
        }
        for (harness, mut ratios) in per_harness {
            // Keep the most recent MAX_SAMPLES by completion order.
            if ratios.len() > MAX_SAMPLES {
                ratios.drain(..ratios.len() - MAX_SAMPLES);
            }
            if ratios.len() < MIN_HARNESS_SAMPLES {
                continue;
            }
            let Some(median) = median_milli(&ratios) else {
                continue;
            };
            calibration.push(Calibration {
                harness,
                samples: u32::try_from(ratios.len()).unwrap_or(u32::MAX),
                ratio_milli: clamp_ratio(median),
            });
        }
    }

    Report {
        finished_features,
        samples,
        calibration,
    }
}
