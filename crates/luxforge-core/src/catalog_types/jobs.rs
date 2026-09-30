//! The long-running catalog work, each kind named once.
//!
//! Every job expected to take more than a second publishes [`ActivityProgress`] on the owner's one
//! activity board under its activity kind, with a count once it knows its extent (an honest "48,210
//! of about 200,000" while a walk is still discovering it), and is read and cancelled through
//! `job.read` and `job.cancel` like every other job. Each lane adds its kinds to
//! [`JobKind`](crate::jobs::JobKind) when it lands its job, serialized as the `job_kind` named here,
//! with the scheduling family its work needs; until then these names are the contract the desktop's
//! long-running-work UI (lane D) and the lanes share.
//!
//! [`ActivityProgress`]: crate::activity::ActivityProgress
use super::CatalogLane;

/// One kind of long-running catalog work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogJob {
    /// What `job.read` reports as `kind` once the lane adds the [`JobKind`](crate::jobs::JobKind)
    /// variant.
    pub job_kind: &'static str,
    /// The activity board's `kind`, dotted as the board's other kinds are (`source.develop`).
    pub activity: &'static str,
    /// The board's present-participle label.
    pub label: &'static str,
    pub lane: CatalogLane,
}

/// Listing a card or folder and reading headers (`index.refresh`, a card's mount, a first visit).
pub const INDEX_REFRESH: CatalogJob = CatalogJob {
    job_kind: "index-refresh",
    activity: "index.refresh",
    label: "Indexing",
    lane: CatalogLane::Files,
};
/// Extracting embedded previews into the grid and loupe tiers (`preview.read` of a file).
pub const PREVIEW_EXTRACT: CatalogJob = CatalogJob {
    job_kind: "preview-extract",
    activity: "preview.extract",
    label: "Reading previews",
    lane: CatalogLane::Previews,
};
/// A 100% region, from the embedded full-size preview or a neutral development (`preview.region`).
pub const PREVIEW_REGION: CatalogJob = CatalogJob {
    job_kind: "preview-region",
    activity: "preview.region",
    label: "Checking focus",
    lane: CatalogLane::Previews,
};
/// Rendering developed photographs' grid and large tiers (`preview.read` of a photograph).
pub const PREVIEW_RENDER: CatalogJob = CatalogJob {
    job_kind: "preview-render",
    activity: "preview.photo",
    label: "Rendering previews",
    lane: CatalogLane::Previews,
};
/// Bringing picks into the catalog (`pick.develop`).
pub const DEVELOP_PICKS: CatalogJob = CatalogJob {
    job_kind: "develop-picks",
    activity: "pick.develop",
    label: "Developing picks",
    lane: CatalogLane::Catalog,
};
/// Checking originals' availability (`source.check`).
pub const SOURCE_CHECK: CatalogJob = CatalogJob {
    job_kind: "source-check",
    activity: "source.check",
    label: "Checking originals",
    lane: CatalogLane::Catalog,
};
/// Searching a folder for missing originals (`source.find`).
pub const SOURCE_FIND: CatalogJob = CatalogJob {
    job_kind: "source-find",
    activity: "source.find",
    label: "Finding originals",
    lane: CatalogLane::Catalog,
};
/// Verifying one chosen file's fingerprint before relinking (`source.locate`).
pub const SOURCE_LOCATE: CatalogJob = CatalogJob {
    job_kind: "source-locate",
    activity: "source.locate",
    label: "Verifying original",
    lane: CatalogLane::Catalog,
};
/// Applying a preset to many photographs (`batch.apply-preset`).
pub const BATCH_PRESET: CatalogJob = CatalogJob {
    job_kind: "batch-preset",
    activity: "batch.apply-preset",
    label: "Applying preset",
    lane: CatalogLane::Catalog,
};
/// Exporting many photographs (`batch.export`).
pub const BATCH_EXPORT: CatalogJob = CatalogJob {
    job_kind: "batch-export",
    activity: "batch.export",
    label: "Exporting",
    lane: CatalogLane::Catalog,
};

/// Every catalog job kind.
pub const CATALOG_JOBS: [CatalogJob; 10] = [
    INDEX_REFRESH,
    PREVIEW_EXTRACT,
    PREVIEW_REGION,
    PREVIEW_RENDER,
    DEVELOP_PICKS,
    SOURCE_CHECK,
    SOURCE_FIND,
    SOURCE_LOCATE,
    BATCH_PRESET,
    BATCH_EXPORT,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Each kind is named once, apart from every kind the job table and the activity board already
    /// use.
    #[test]
    fn every_catalog_job_has_its_own_names() {
        let kinds: HashSet<_> = CATALOG_JOBS.iter().map(|job| job.job_kind).collect();
        let activities: HashSet<_> = CATALOG_JOBS.iter().map(|job| job.activity).collect();
        assert_eq!(kinds.len(), CATALOG_JOBS.len());
        assert_eq!(activities.len(), CATALOG_JOBS.len());
        for existing in [
            "prepare",
            "develop",
            "artifacts",
            "collect",
            "analysis",
            "install",
            "remove",
            "task",
            "export",
        ] {
            assert!(!kinds.contains(existing), "{existing}");
        }
        for existing in [
            "source.prepare",
            "source.develop",
            "artifacts.read",
            "artifacts.collect",
            "analysis.histogram",
            "preview.render",
            "export",
        ] {
            assert!(!activities.contains(existing), "{existing}");
        }
    }
}
