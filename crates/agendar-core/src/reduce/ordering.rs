//! Explicit list ordering shared by reducer, index and app projections
//!.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use crate::model::ids::{ProjectSlug, UtcTime};
use crate::model::state::{NeedsYouItem, ProjectSummary, Variance};

/// Cost-of-delay key: highest score first, then the older row, then the ID.
pub fn need_key(row: &NeedsYouItem) -> (Reverse<u64>, UtcTime, String) {
    (Reverse(row.score_x2), row.since, row.id.clone())
}

/// Sort attention rows in place with the single documented order.
pub fn sort_needs(rows: &mut [NeedsYouItem]) {
    rows.sort_by_key(need_key);
}

/// Project order: variance first (rework, stopped, no demo, active, quiet),
/// then the earliest needs-you item, then the slug.
pub fn project_key(summary: &ProjectSummary, earliest_need: Option<UtcTime>) -> (u8, i64, String) {
    (
        summary.variance.rank(),
        earliest_need.map_or(i64::MAX, |need| need.millis()),
        summary.slug.as_str().to_owned(),
    )
}

/// Earliest attention timestamp per project, for the project sort tie-break.
pub fn earliest_needs(rows: &[NeedsYouItem]) -> BTreeMap<ProjectSlug, UtcTime> {
    let mut map: BTreeMap<ProjectSlug, UtcTime> = BTreeMap::new();
    for row in rows {
        map.entry(row.project.clone())
            .and_modify(|current| {
                if row.since < *current {
                    *current = row.since;
                }
            })
            .or_insert(row.since);
    }
    map
}

/// Sort project summaries by the documented order.
pub fn sort_projects(items: &mut [ProjectSummary], earliest: &BTreeMap<ProjectSlug, UtcTime>) {
    items.sort_by_key(|summary| project_key(summary, earliest.get(&summary.slug).copied()));
}

/// The variance ranks in human order, for callers that print them.
pub fn variance_rank(variance: Variance) -> u8 {
    variance.rank()
}
