//! What the Vault holds per project and what a cleanup could free.
//!
//! A read-only projection: events, the folded state, a listing of the files
//! present and `now` go in, numbers and a blob list come out. Nothing here
//! reads a blob, touches the filesystem or writes anything. The host passes
//! the listing (`vault::listing`), so the CLI and the app share one result.
//!
//! The rule:
//! - Reclaimable: a blob of an archived project that only deliveries
//!   reference (demos, posters, criteria evidence).
//! - Superseded (shown, never counted as reclaimable): in a project that is
//!   not archived, a blob referenced only by deliveries that are all
//!   superseded and all older than [`SUPERSEDED_AFTER_S`].
//! - Unreferenced (shown, never counted as reclaimable): a file in a `blobs/`
//!   folder that no event of the project cites.
//! - Kept: everything else. One referrer that is not a delivery (a report, a
//!   decision image, an icon, evidence outside a delivery) keeps the blob.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::model::event::{Event, EventPayload};
use crate::model::ids::{BlobRef, EventId, HostId, ProjectSlug, UtcTime};
use crate::model::state::State;

/// A superseded delivery counts on the "superseded" line once it is older
/// than this (30 days of event time).
pub const SUPERSEDED_AFTER_S: i64 = 30 * 24 * 60 * 60;

/// How one event field uses a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobUse {
    /// A delivery demo: screenshot, transcript, diff, recording or its poster.
    DeliveryDemo,
    /// Evidence attached to a criterion of a delivery.
    DeliveryEvidence,
    /// The output of an `evidence.observed` check.
    Observation,
    /// Evidence attached to a `work.progress` event.
    ProgressEvidence,
    /// An option image of a decision card.
    OptionImage,
    /// The report attached to a decision card.
    AskReport,
    /// A published report.
    Report,
    /// The project icon.
    Icon,
}

impl BlobUse {
    pub const fn is_report(self) -> bool {
        matches!(self, Self::AskReport | Self::Report)
    }

    pub const fn is_delivery(self) -> bool {
        matches!(self, Self::DeliveryDemo | Self::DeliveryEvidence)
    }

    pub const fn category(self) -> BlobCategory {
        match self {
            Self::DeliveryDemo | Self::DeliveryEvidence => BlobCategory::Delivery,
            Self::Observation | Self::ProgressEvidence => BlobCategory::Evidence,
            Self::OptionImage => BlobCategory::DecisionImage,
            Self::AskReport | Self::Report => BlobCategory::Report,
            Self::Icon => BlobCategory::Icon,
        }
    }
}

/// Every blob one event references, with how it uses it. The one list of
/// blob-carrying event fields: `evidence::blob_referenced` and the usage
/// listing both read it, so they cannot disagree.
pub fn visit_event_blobs(event: &Event, visit: &mut dyn FnMut(BlobUse, &BlobRef)) {
    match &event.payload {
        EventPayload::Delivery(body) => {
            for demo in &body.demos {
                for blob in demo.blobs() {
                    visit(BlobUse::DeliveryDemo, &blob);
                }
            }
            for criterion in &body.criteria {
                if let Some(blob) = &criterion.evidence {
                    visit(BlobUse::DeliveryEvidence, blob);
                }
            }
        }
        EventPayload::EvidenceObserved(body) => visit(BlobUse::Observation, &body.output),
        EventPayload::WorkProgress(body) => {
            for criterion in &body.evidence {
                if let Some(blob) = &criterion.evidence {
                    visit(BlobUse::ProgressEvidence, blob);
                }
            }
        }
        EventPayload::DecisionAsk(body) => {
            for option in &body.card.options {
                if let Some(blob) = &option.image {
                    visit(BlobUse::OptionImage, blob);
                }
            }
            if let Some(blob) = &body.card.report {
                visit(BlobUse::AskReport, blob);
            }
        }
        EventPayload::ProjectDeclared(body) => {
            if let Some(blob) = &body.icon {
                visit(BlobUse::Icon, blob);
            }
        }
        EventPayload::ReportPublished(body) => visit(BlobUse::Report, &body.report),
        _ => {}
    }
}

/// What a blob is for. A blob with several kinds of referrer takes the last
/// that applies in this order, so each blob counts in exactly one category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobCategory {
    Delivery,
    Evidence,
    DecisionImage,
    Report,
    Icon,
    Unreferenced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobStatus {
    Reclaimable,
    Superseded,
    Unreferenced,
    Kept,
}

/// Why a blob has its status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobReason {
    /// Reclaimable: only deliveries of an archived project reference it.
    ArchivedProjectDelivery,
    /// Superseded line: every delivery that references it is superseded and
    /// older than 30 days.
    SupersededDelivery,
    /// Kept: a delivery that is not superseded references it.
    CurrentDelivery,
    /// Kept: superseded, but a referring delivery is 30 days old or younger.
    SupersededRecently,
    /// Kept: evidence outside a delivery references it.
    Evidence,
    /// Kept: a decision option image.
    DecisionImage,
    /// Kept: a report.
    Report,
    /// Kept: a project icon.
    Icon,
    /// No event of the project references this file.
    Unreferenced,
}

/// One file found in a `blobs/` folder. `bytes` is the size the filesystem
/// reports; `local` is false when the content is not on this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobFile {
    pub host: HostId,
    pub name: String,
    pub bytes: u64,
    pub local: bool,
}

/// The files of one project directory, as listed by the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFiles {
    /// Every file beside the `blobs/` folders (the writer segments).
    pub event_bytes: u64,
    /// The part of `event_bytes` whose content is on this computer.
    pub event_local_bytes: u64,
    pub blobs: Vec<BlobFile>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Tally {
    pub count: u32,
    /// Logical bytes: `BlobRef.bytes`, or the listed size of an unreferenced file.
    pub bytes: u64,
    /// Bytes of the files present on this computer.
    pub local_bytes: u64,
}

impl Tally {
    fn add(&mut self, bytes: u64, local_bytes: u64) {
        self.count += 1;
        self.bytes += bytes;
        self.local_bytes += local_bytes;
    }

    fn merge(&mut self, other: Self) {
        self.count += other.count;
        self.bytes += other.bytes;
        self.local_bytes += other.local_bytes;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlobReferrer {
    pub event: EventId,
    #[serde(rename = "use")]
    pub usage: BlobUse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlobUsage {
    /// Path below the Vault root.
    pub path: String,
    pub host: HostId,
    /// `<sha256>.<ext>`.
    pub file: String,
    pub bytes: u64,
    pub local_bytes: u64,
    pub category: BlobCategory,
    pub status: BlobStatus,
    pub reason: BlobReason,
    pub referrers: Vec<BlobReferrer>,
}

/// The numbers of one project, or of the whole Vault.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UsageNumbers {
    /// `event_bytes` plus every attachment.
    pub total_bytes: u64,
    pub total_local_bytes: u64,
    pub event_bytes: u64,
    pub event_local_bytes: u64,
    /// Every blob: referenced ones once each, plus unreferenced files.
    pub attachments: Tally,
    /// `attachments` split by category; the parts add up to it.
    pub categories: BTreeMap<BlobCategory, Tally>,
    /// What the default rule could free.
    pub reclaimable: Tally,
    /// Shown beside it; not part of `reclaimable`.
    pub superseded: Tally,
    /// Shown beside it; not part of `reclaimable`.
    pub unreferenced: Tally,
}

impl UsageNumbers {
    fn merge(&mut self, other: &Self) {
        self.total_bytes += other.total_bytes;
        self.total_local_bytes += other.total_local_bytes;
        self.event_bytes += other.event_bytes;
        self.event_local_bytes += other.event_local_bytes;
        self.attachments.merge(other.attachments);
        for (category, tally) in &other.categories {
            self.categories.entry(*category).or_default().merge(*tally);
        }
        self.reclaimable.merge(other.reclaimable);
        self.superseded.merge(other.superseded);
        self.unreferenced.merge(other.unreferenced);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectUsage {
    pub project: ProjectSlug,
    /// Absent for a project folder no valid event declares.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub archived: bool,
    #[serde(flatten)]
    pub numbers: UsageNumbers,
    /// Every blob with its status and reason: reclaimable first.
    pub blobs: Vec<BlobUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StorageUsage {
    pub evaluated_at: UtcTime,
    pub superseded_after_days: u32,
    pub total: UsageNumbers,
    /// Largest first.
    pub projects: Vec<ProjectUsage>,
}

/// One referenced blob while its referrers are collected.
struct Referenced {
    bytes: u64,
    referrers: Vec<(EventId, UtcTime, BlobUse)>,
}

/// Measure every project (or only `only`).
pub fn storage_usage(
    events: &[Event],
    state: &State,
    files: &BTreeMap<ProjectSlug, ProjectFiles>,
    now: UtcTime,
    only: Option<&ProjectSlug>,
) -> StorageUsage {
    let wanted = |project: &ProjectSlug| only.is_none_or(|slug| slug == project);
    let mut referenced: BTreeMap<ProjectSlug, BTreeMap<(HostId, String), Referenced>> =
        BTreeMap::new();
    for event in events {
        if !wanted(&event.project) {
            continue;
        }
        let blobs = referenced.entry(event.project.clone()).or_default();
        visit_event_blobs(event, &mut |usage, blob| {
            let key = (blob.host.clone(), blob_file_name(blob));
            let entry = blobs.entry(key).or_insert(Referenced {
                bytes: 0,
                referrers: Vec::new(),
            });
            entry.bytes = entry.bytes.max(blob.bytes);
            entry.referrers.push((event.id.clone(), event.ts, usage));
        });
    }

    let mut slugs: BTreeSet<&ProjectSlug> = state.projects.keys().collect();
    slugs.extend(referenced.keys());
    slugs.extend(files.keys());
    let empty_files = ProjectFiles::default();
    let empty_refs = BTreeMap::new();
    let mut projects: Vec<ProjectUsage> = slugs
        .into_iter()
        .filter(|slug| wanted(slug))
        .map(|slug| {
            project_usage(
                slug,
                state,
                referenced.get(slug).unwrap_or(&empty_refs),
                files.get(slug).unwrap_or(&empty_files),
                now,
            )
        })
        .collect();
    projects.sort_by(|a, b| {
        b.numbers
            .total_bytes
            .cmp(&a.numbers.total_bytes)
            .then_with(|| a.project.cmp(&b.project))
    });
    let mut total = UsageNumbers::default();
    for project in &projects {
        total.merge(&project.numbers);
    }
    StorageUsage {
        evaluated_at: now,
        superseded_after_days: u32::try_from(SUPERSEDED_AFTER_S / 86_400).unwrap_or(u32::MAX),
        total,
        projects,
    }
}

fn blob_file_name(blob: &BlobRef) -> String {
    format!("{}.{}", blob.sha256, blob.ext.as_str())
}

fn project_usage(
    slug: &ProjectSlug,
    state: &State,
    referenced: &BTreeMap<(HostId, String), Referenced>,
    files: &ProjectFiles,
    now: UtcTime,
) -> ProjectUsage {
    let project = state.projects.get(slug);
    let archived = project.is_some_and(|project| project.summary.archived);
    let present: BTreeMap<(&HostId, &str), &BlobFile> = files
        .blobs
        .iter()
        .map(|file| ((&file.host, file.name.as_str()), file))
        .collect();
    let cutoff_ms = now.millis() - SUPERSEDED_AFTER_S * 1000;

    let mut blobs: Vec<BlobUsage> = Vec::new();
    for ((host, file), entry) in referenced {
        let category = entry
            .referrers
            .iter()
            .map(|(_, _, usage)| usage.category())
            .max()
            .unwrap_or(BlobCategory::Delivery);
        let (status, reason) = match category {
            BlobCategory::Delivery if archived => {
                (BlobStatus::Reclaimable, BlobReason::ArchivedProjectDelivery)
            }
            BlobCategory::Delivery => {
                let superseded = |id: &EventId| {
                    project
                        .and_then(|project| project.deliveries.get(id))
                        .is_some_and(|delivery| delivery.superseded_by.is_some())
                };
                if !entry.referrers.iter().all(|(id, _, _)| superseded(id)) {
                    (BlobStatus::Kept, BlobReason::CurrentDelivery)
                } else if entry
                    .referrers
                    .iter()
                    .all(|(_, ts, _)| ts.millis() < cutoff_ms)
                {
                    (BlobStatus::Superseded, BlobReason::SupersededDelivery)
                } else {
                    (BlobStatus::Kept, BlobReason::SupersededRecently)
                }
            }
            BlobCategory::Evidence => (BlobStatus::Kept, BlobReason::Evidence),
            BlobCategory::DecisionImage => (BlobStatus::Kept, BlobReason::DecisionImage),
            BlobCategory::Report => (BlobStatus::Kept, BlobReason::Report),
            BlobCategory::Icon => (BlobStatus::Kept, BlobReason::Icon),
            BlobCategory::Unreferenced => (BlobStatus::Unreferenced, BlobReason::Unreferenced),
        };
        let local_bytes = present
            .get(&(host, file.as_str()))
            .filter(|found| found.local)
            .map_or(0, |found| found.bytes);
        blobs.push(BlobUsage {
            path: blob_path(slug, host, file),
            host: host.clone(),
            file: file.clone(),
            bytes: entry.bytes,
            local_bytes,
            category,
            status,
            reason,
            referrers: entry
                .referrers
                .iter()
                .map(|(event, _, usage)| BlobReferrer {
                    event: event.clone(),
                    usage: *usage,
                })
                .collect(),
        });
    }
    let mut seen: BTreeSet<(&HostId, &str)> = BTreeSet::new();
    for file in &files.blobs {
        let key = (&file.host, file.name.as_str());
        if referenced.contains_key(&(file.host.clone(), file.name.clone())) || !seen.insert(key) {
            continue;
        }
        blobs.push(BlobUsage {
            path: blob_path(slug, &file.host, &file.name),
            host: file.host.clone(),
            file: file.name.clone(),
            bytes: file.bytes,
            local_bytes: if file.local { file.bytes } else { 0 },
            category: BlobCategory::Unreferenced,
            status: BlobStatus::Unreferenced,
            reason: BlobReason::Unreferenced,
            referrers: Vec::new(),
        });
    }
    blobs.sort_by(|a, b| {
        a.status
            .cmp(&b.status)
            .then_with(|| b.bytes.cmp(&a.bytes))
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut numbers = UsageNumbers {
        event_bytes: files.event_bytes,
        event_local_bytes: files.event_local_bytes,
        ..UsageNumbers::default()
    };
    for blob in &blobs {
        numbers.attachments.add(blob.bytes, blob.local_bytes);
        numbers
            .categories
            .entry(blob.category)
            .or_default()
            .add(blob.bytes, blob.local_bytes);
        match blob.status {
            BlobStatus::Reclaimable => numbers.reclaimable.add(blob.bytes, blob.local_bytes),
            BlobStatus::Superseded => numbers.superseded.add(blob.bytes, blob.local_bytes),
            BlobStatus::Unreferenced => numbers.unreferenced.add(blob.bytes, blob.local_bytes),
            BlobStatus::Kept => {}
        }
    }
    numbers.total_bytes = numbers.event_bytes + numbers.attachments.bytes;
    numbers.total_local_bytes = numbers.event_local_bytes + numbers.attachments.local_bytes;
    ProjectUsage {
        project: slug.clone(),
        title: project.map(|project| project.summary.title.clone()),
        archived,
        numbers,
        blobs,
    }
}

fn blob_path(project: &ProjectSlug, host: &HostId, file: &str) -> String {
    format!("projects/{}/{host}/blobs/{file}", project.as_str())
}
