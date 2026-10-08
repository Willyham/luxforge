//! The long-running catalog work, each kind named once.
//!
//! Every job expected to take more than a second publishes [`ActivityProgress`] on the owner's one
//! activity board under its activity kind, with a count once it knows its extent (an honest "48,210
//! of about 200,000" while a walk is still discovering it), and is read and cancelled through
//! `job.read` and `job.cancel` like every other job. Each has its [`JobKind`] in the catalog
//! family: the lane that runs it (the index, preview or library lane) schedules it on its own
//! workers, and a cancel stops it for everyone and tells that lane.
//!
//! [`ActivityProgress`]: crate::activity::ActivityProgress
use crate::jobs::JobKind;

/// One kind of long-running catalog work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogJob {
    pub kind: JobKind,
    /// What `job.read` reports as `kind`: the [`JobKind`] as it serializes.
    pub job_kind: &'static str,
    /// The activity board's `kind`, dotted as the board's other kinds are (`source.develop`).
    pub activity: &'static str,
    /// The board's present-participle label.
    pub label: &'static str,
}

/// Listing a card or folder and reading headers (`index.refresh`, a card's mount, a first visit, and
/// the index lane's own listings: a rescan, a folder listed again as it moved or as the catalog
/// opens).
pub const INDEX_REFRESH: CatalogJob = CatalogJob {
    kind: JobKind::IndexRefresh,
    job_kind: "index-refresh",
    activity: "index.refresh",
    label: "Indexing",
};
/// Extracting embedded previews into the grid and loupe tiers (`preview.read` of a file).
pub const PREVIEW_EXTRACT: CatalogJob = CatalogJob {
    kind: JobKind::PreviewExtract,
    job_kind: "preview-extract",
    activity: "preview.extract",
    label: "Reading previews",
};
/// A 100% region, from the embedded full-size preview or a neutral development (`preview.region`).
pub const PREVIEW_REGION: CatalogJob = CatalogJob {
    kind: JobKind::PreviewRegion,
    job_kind: "preview-region",
    activity: "preview.region",
    label: "Checking focus",
};
/// Rendering developed photographs' grid and large tiers (`preview.read` of a photograph).
pub const PREVIEW_RENDER: CatalogJob = CatalogJob {
    kind: JobKind::PreviewRender,
    job_kind: "preview-render",
    activity: "preview.photo",
    label: "Rendering previews",
};
/// Bringing picks into the catalog (`pick.develop`).
pub const DEVELOP_PICKS: CatalogJob = CatalogJob {
    kind: JobKind::DevelopPicks,
    job_kind: "develop-picks",
    activity: "pick.develop",
    label: "Developing picks",
};
/// Checking originals' availability (`source.check`).
pub const SOURCE_CHECK: CatalogJob = CatalogJob {
    kind: JobKind::SourceCheck,
    job_kind: "source-check",
    activity: "source.check",
    label: "Checking originals",
};
/// Searching a folder for missing originals (`source.find`).
pub const SOURCE_FIND: CatalogJob = CatalogJob {
    kind: JobKind::SourceFind,
    job_kind: "source-find",
    activity: "source.find",
    label: "Finding originals",
};
/// Verifying one chosen file's fingerprint before relinking (`source.locate`).
pub const SOURCE_LOCATE: CatalogJob = CatalogJob {
    kind: JobKind::SourceLocate,
    job_kind: "source-locate",
    activity: "source.locate",
    label: "Verifying original",
};
/// Applying a settings set, a preset's or pasted, to many photographs (`batch.apply-settings`).
pub const BATCH_SETTINGS: CatalogJob = CatalogJob {
    kind: JobKind::BatchSettings,
    job_kind: "batch-settings",
    activity: "batch.apply-settings",
    label: "Applying settings",
};
/// Exporting many photographs (`batch.export`).
pub const BATCH_EXPORT: CatalogJob = CatalogJob {
    kind: JobKind::BatchExport,
    job_kind: "batch-export",
    activity: "batch.export",
    label: "Exporting",
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
    BATCH_SETTINGS,
    BATCH_EXPORT,
];

/// The catalog job that publishes activity board entries of kind `activity`, if one does: how a
/// reader of the board tells long-running catalog work from the rest.
pub fn catalog_job(activity: &str) -> Option<CatalogJob> {
    CATALOG_JOBS
        .into_iter()
        .find(|job| job.activity == activity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_board_kind_names_its_catalog_job() {
        for job in CATALOG_JOBS {
            assert_eq!(catalog_job(job.activity), Some(job));
        }
        assert_eq!(catalog_job("preview.render"), None);
        assert_eq!(catalog_job("source.develop"), None);
    }

    /// Each kind is named once, apart from every kind the job table and the activity board already
    /// use, and its `job_kind` is its job kind as `job.read` reports it.
    #[test]
    fn every_catalog_job_has_its_own_names() {
        for job in CATALOG_JOBS {
            assert_eq!(
                serde_json::to_value(job.kind).unwrap(),
                serde_json::json!(job.job_kind)
            );
            assert_eq!(job.kind.family(), crate::jobs::Family::Catalog);
        }
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
