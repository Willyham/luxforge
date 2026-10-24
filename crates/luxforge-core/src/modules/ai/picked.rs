//! The objects `task.select-object` published in this process, which `task.fill-generative`
//! removes by artifact (`object-artifacts`): each selection's photograph, the source and repair
//! prefix it was computed from, the prompt it was picked with, and its raster's size and non-zero
//! rectangle, so a fill is admitted on the owner from these alone and reads no artifact there.
//!
//! An artifact names only its bytes, so what it was computed from is known only to the task that
//! computed it. The record is kept in memory, at most [`MAX_PICKED_OBJECTS`] selections, oldest
//! first out, as the artifacts it describes are live only while this process published them
//! (`docs/design/module-capabilities.md`, retention). A fill naming an artifact the record does not
//! hold for this photograph as it is now, whether its original or its repairs have changed since,
//! it was picked on another photograph, or it was published by another process, is refused
//! `conflict` with `data.reason: "stale-selection"`; selecting the object again records it anew and
//! publishes the same artifact when nothing changed.
use super::object_region::ObjectRaster;
use crate::{ArtifactId, AssetId, Error, capabilities::read::StageIdentity, mask::ObjectPrompt};
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Mutex, PoisonError},
};

/// The most selections the record holds: far more than one fill names.
pub(crate) const MAX_PICKED_OBJECTS: usize = 256;

/// One selection `task.select-object` published.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PickedObject {
    pub(crate) artifact: ArtifactId,
    pub(crate) asset: AssetId,
    pub(crate) source: String,
    pub(crate) prefix: String,
    pub(crate) prompt: Option<ObjectPrompt>,
    pub(crate) raster: ObjectRaster,
}

/// The record. `O(entries)` lookups under one mutex, never held across a computation.
#[derive(Debug, Default)]
pub(crate) struct PickedObjects {
    entries: Mutex<VecDeque<PickedObject>>,
}

impl PickedObjects {
    /// Record a published selection, replacing an earlier record of the same artifact computed
    /// from the same photograph as it was then, newest last.
    pub(crate) fn record(&self, object: PickedObject) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|held| {
            !(held.artifact == object.artifact
                && held.asset == object.asset
                && held.source == object.source
                && held.prefix == object.prefix)
        });
        entries.push_back(object);
        while entries.len() > MAX_PICKED_OBJECTS {
            entries.pop_front();
        }
    }

    /// The selections `ids` name, in their order, each picked on `identity`'s photograph as it is
    /// now; `conflict` with `data: {reason: "stale-selection", artifact, asset_id}` naming the first
    /// that is not, and why.
    pub(crate) fn resolve(
        &self,
        ids: &[ArtifactId],
        identity: &StageIdentity,
    ) -> Result<Vec<PickedObject>, Error> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        ids.iter()
            .map(|id| {
                let mut of_asset = entries
                    .iter()
                    .rev()
                    .filter(|held| held.artifact == *id && held.asset == identity.asset);
                if let Some(current) = of_asset
                    .clone()
                    .find(|held| held.source == identity.source && held.prefix == identity.prefix)
                {
                    return Ok(current.clone());
                }
                let why = match of_asset.next() {
                    Some(held) if held.source != identity.source => {
                        "the photograph's original has changed since it was picked"
                    }
                    Some(_) => "the photograph was repaired since it was picked",
                    None if entries.iter().any(|held| held.artifact == *id) => {
                        "it was picked on another photograph"
                    }
                    None => "task.select-object did not publish it for this photograph in this session",
                };
                Err(Error::conflict(format!(
                    "artifact {id} is not a selection of photograph {} as it is now: {why}; select the object again",
                    identity.asset
                ))
                .with_data(json!({
                    "reason": "stale-selection",
                    "artifact": id,
                    "asset_id": identity.asset,
                })))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::Stage;

    fn identity(asset: &AssetId, source: &str, prefix: &str) -> StageIdentity {
        StageIdentity {
            asset: asset.clone(),
            source: source.into(),
            prefix: prefix.into(),
            size: Stage {
                width: 8,
                height: 8,
            },
        }
    }

    fn picked(artifact: &ArtifactId, at: &StageIdentity) -> PickedObject {
        PickedObject {
            artifact: artifact.clone(),
            asset: at.asset.clone(),
            source: at.source.clone(),
            prefix: at.prefix.clone(),
            prompt: None,
            raster: ObjectRaster {
                width: 8,
                height: 8,
                nonzero: Some([1, 1, 2, 2]),
            },
        }
    }

    fn reason(error: &Error) -> String {
        assert_eq!(error.kind.code(), "conflict", "{error:?}");
        assert_eq!(error.data.as_deref().unwrap()["reason"], "stale-selection");
        error.detail.clone()
    }

    #[test]
    fn a_selection_resolves_only_for_its_photograph_as_it_was_picked() {
        let record = PickedObjects::default();
        let (asset, other) = (AssetId::new(), AssetId::new());
        let artifact = ArtifactId::for_hash(&"a".repeat(64)).unwrap();
        let now = identity(&asset, "source", "prefix");
        record.record(picked(&artifact, &now));
        assert_eq!(
            record
                .resolve(std::slice::from_ref(&artifact), &now)
                .unwrap()
                .len(),
            1
        );
        let repaired = record
            .resolve(
                std::slice::from_ref(&artifact),
                &identity(&asset, "source", "later"),
            )
            .unwrap_err();
        assert!(reason(&repaired).contains("repaired"), "{repaired:?}");
        let replaced = record
            .resolve(
                std::slice::from_ref(&artifact),
                &identity(&asset, "another", "prefix"),
            )
            .unwrap_err();
        assert!(reason(&replaced).contains("original"), "{replaced:?}");
        let elsewhere = record
            .resolve(
                std::slice::from_ref(&artifact),
                &identity(&other, "source", "prefix"),
            )
            .unwrap_err();
        assert!(reason(&elsewhere).contains("another photograph"));
        let unknown = ArtifactId::for_hash(&"b".repeat(64)).unwrap();
        assert!(reason(&record.resolve(&[unknown], &now).unwrap_err()).contains("did not publish"));
    }

    #[test]
    fn the_record_keeps_the_newest_selections() {
        let record = PickedObjects::default();
        let asset = AssetId::new();
        let at = identity(&asset, "source", "prefix");
        let ids: Vec<ArtifactId> = (0..=MAX_PICKED_OBJECTS)
            .map(|n| ArtifactId::for_hash(&format!("{n:064x}")).unwrap())
            .collect();
        for id in &ids {
            record.record(picked(id, &at));
        }
        // Recording one again replaces it rather than holding it twice.
        record.record(picked(&ids[MAX_PICKED_OBJECTS], &at));
        assert!(record.resolve(&ids[..1], &at).is_err());
        assert_eq!(
            record.resolve(&ids[1..], &at).unwrap().len(),
            MAX_PICKED_OBJECTS
        );
    }
}
