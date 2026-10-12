//! The merge providers this build registers, by kind (`docs/design/panorama-merge.md`,
//! "Generalizing the merge boundary"): which [`MergeKind`]s can merge, the module and algorithm
//! version each result records, and the refusal of a kind with none.
//!
//! HDR's provider is `luxforge.hdr` version 1. No panorama provider is registered, so a panorama
//! request is `not-ready` naming `luxforge.panorama`, and schemas list `hdr` alone.
use crate::{
    Error,
    catalog_types::{MergeKind, MergeProvider},
};
use serde_json::json;

/// The algorithm version of the HDR provider: every result it writes records it, and a rerun must
/// match it.
pub(crate) const HDR_VERSION: u32 = 1;

/// The provider this build registers for `kind`, if any.
pub(crate) fn provider(kind: MergeKind) -> Option<MergeProvider> {
    match kind {
        MergeKind::Hdr => Some(MergeProvider {
            module: kind.module().to_owned(),
            version: HDR_VERSION,
        }),
        MergeKind::Panorama => None,
    }
}

/// The kinds with a registered provider, in [`MergeKind::ALL`]'s order: what a schema's `kind`
/// enumerates.
pub(crate) fn registered() -> impl Iterator<Item = MergeKind> {
    MergeKind::ALL
        .into_iter()
        .filter(|kind| provider(*kind).is_some())
}

/// Why `kind` has no provider in this build.
pub(crate) fn missing(kind: MergeKind) -> String {
    format!("this build has no {} provider", kind.module())
}

/// The refusal of a merge of `kind` while it has no provider: `not-ready`, naming the kind, the
/// module that would provide it and why there is none.
pub(crate) fn not_ready(kind: MergeKind, reason: &str) -> Error {
    Error::not_ready(format!(
        "{} merging is not available: {reason}",
        kind.noun()
    ))
    .with_data(json!({"kind": kind, "provider": kind.module(), "reason": reason}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    #[test]
    fn merge_kind_registry_names_hdr_alone_and_refuses_panorama_by_its_module() {
        assert_eq!(registered().collect::<Vec<_>>(), [MergeKind::Hdr]);
        assert_eq!(
            provider(MergeKind::Hdr),
            Some(MergeProvider {
                module: "luxforge.hdr".into(),
                version: 1
            })
        );
        assert_eq!(provider(MergeKind::Panorama), None);
        let error = not_ready(MergeKind::Panorama, &missing(MergeKind::Panorama));
        assert_eq!(error.kind, ErrorKind::NotReady);
        assert_eq!(
            error.detail,
            "panorama merging is not available: this build has no luxforge.panorama provider"
        );
        let data = error.data.unwrap();
        assert_eq!(data["kind"], "panorama");
        assert_eq!(data["provider"], "luxforge.panorama");
    }

    #[test]
    fn merge_kind_bounds_limits_and_jobs_are_per_kind() {
        assert_eq!(MergeKind::Hdr.inputs(), 2..=9);
        assert_eq!(MergeKind::Panorama.inputs(), 2..=32);
        assert_eq!(MergeKind::Hdr.temp_ceiling(), 16 << 30);
        assert_eq!(MergeKind::Panorama.temp_ceiling(), 64 << 30);
        assert_eq!(MergeKind::Hdr.scratch_limit(), 256 << 20);
        assert_eq!(MergeKind::Panorama.scratch_limit(), 512 << 20);
        assert_eq!(
            MergeKind::Hdr.job().map(|job| job.job_kind),
            Some("hdr-merge")
        );
        assert!(MergeKind::Panorama.job().is_none());
        for kind in MergeKind::ALL {
            assert_eq!(MergeKind::parse(kind.as_str()), Some(kind));
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(kind.as_str())
            );
        }
        assert_eq!(MergeKind::parse("Panorama"), None);
    }
}
