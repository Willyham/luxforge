use super::{
    AssetRecord, EditorService, EditorState, LayerDescription, RecipeDescription,
    catalog::{ASSET_COLUMNS, asset_row, stored_revision},
};
use crate::{AssetId, EntryId, Error, HistoryEntry};
use serde_json::Map;

impl EditorService {
    /// The asset, its revision, its current entry with its strokes resolved, and what redo would
    /// return to. After the first read of an asset and its current entry this decodes and hashes
    /// nothing: it copies the cached head and entry, which every write that moves them updates.
    pub fn state(&self, asset_id: &AssetId) -> Result<EditorState, Error> {
        let head = self.head(asset_id)?;
        let entry = self.shared_entry(asset_id, &head.current)?;
        Ok(EditorState {
            asset: head.asset,
            revision: head.revision,
            current_entry: HistoryEntry::clone(&entry),
            redo: head.redo,
        })
    }

    /// The asset's current revision, which is all a draft's conflict check compares. It decodes
    /// nothing, whether or not the asset's head is cached.
    pub fn revision(&self, asset_id: &AssetId) -> Result<u64, Error> {
        match self.entries.borrow().revision(asset_id) {
            Some(revision) => Ok(revision),
            None => stored_revision(&self.connection, asset_id),
        }
    }

    /// Every referenced asset in import order.
    pub fn assets(&self) -> Result<Vec<AssetRecord>, Error> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT {ASSET_COLUMNS} FROM assets ORDER BY rowid"
        ))?;
        let rows = statement.query_map([], asset_row)?;
        rows.map(|row| row?.into_record()).collect()
    }

    /// One entry of this asset's history with its strokes resolved: decoded once, then copied
    /// from the cache.
    pub fn entry(&self, asset_id: &AssetId, entry_id: &EntryId) -> Result<HistoryEntry, Error> {
        self.shared_entry(asset_id, entry_id)
            .map(|entry| HistoryEntry::clone(&entry))
    }

    /// Describe one entry's stored layers for the recipe panel: `O(layers)` registry lookups and
    /// payload reads, with no decode, no render and no source access. A layer whose provider is
    /// missing or unavailable is listed with the reason, never omitted.
    pub fn describe_entry(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<RecipeDescription, Error> {
        let entry = match entry_id {
            Some(entry_id) => self.entry(asset_id, entry_id)?,
            None => self.state(asset_id)?.current_entry,
        };
        let mut layers = Vec::with_capacity(entry.snapshot.recipe.layers.len());
        for layer in &entry.snapshot.recipe.layers {
            let described = match self.registry.effect(&layer.effect_id) {
                None => LayerDescription {
                    id: layer.id.clone(),
                    effect: layer.effect_id.clone(),
                    module: None,
                    title: None,
                    summary: "no provider".into(),
                    values: Map::new(),
                    available: false,
                    mask: layer.mask.clone(),
                    artifacts: layer.artifacts.clone(),
                    neutral: false,
                },
                Some((module, _)) => {
                    let descriptor = module.descriptor();
                    let unreadable = |error: Error| {
                        (
                            format!("unreadable payload: {}", error.detail),
                            Map::new(),
                            false,
                        )
                    };
                    let (summary, values, available) = match &descriptor.availability {
                        crate::Availability::Unavailable { reason } => {
                            (format!("unavailable: {reason}"), Map::new(), false)
                        }
                        // A payload the provider cannot read is reported on its own row; the
                        // rest of the stack is still described.
                        crate::Availability::Available => match module.describe_layer(
                            &layer.effect_id,
                            layer.effect_format,
                            &layer.payload,
                        ) {
                            Ok(summary) => match module.values(
                                &layer.effect_id,
                                layer.effect_format,
                                &layer.payload,
                            ) {
                                Ok(values) => (summary, values, true),
                                Err(error) => unreadable(error),
                            },
                            Err(error) => unreadable(error),
                        },
                    };
                    LayerDescription {
                        id: layer.id.clone(),
                        effect: layer.effect_id.clone(),
                        module: Some(descriptor.id.clone()),
                        title: Some(descriptor.title.clone()),
                        summary,
                        values,
                        available,
                        mask: layer.mask.clone(),
                        artifacts: layer.artifacts.clone(),
                        neutral: self.registry.layer_neutral(layer),
                    }
                }
            };
            layers.push(described);
        }
        Ok(RecipeDescription {
            entry_id: entry.id,
            layers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::test_support::{fixture, mutation, temp};
    use serde_json::json;

    #[test]
    fn an_entrys_layers_are_described_in_order_with_their_provider() {
        let catalog = temp("describe.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.state(&asset).unwrap().current_entry.id;
        assert_eq!(
            service.describe_entry(&asset, None).unwrap(),
            RecipeDescription {
                entry_id: original.clone(),
                layers: Vec::new(),
            },
            "the original entry has no layers"
        );
        service
            .apply_action(
                &asset,
                mutation(0, "crop"),
                "crop",
                json!({"x":0.25,"y":0.25,"width":0.5,"height":0.5}),
            )
            .unwrap();
        service
            .apply_pixel(&asset, mutation(1, "pixel"), 1, 2, [4, 5, 6])
            .unwrap();
        // Two quarter turns after the crop: one orientation layer, ahead of the crop, whose row
        // names the orientation it holds rather than the two actions that reached it.
        for (revision, request) in [(2, "turn-a"), (3, "turn-b")] {
            service
                .apply_action(
                    &asset,
                    mutation(revision, request),
                    "transform",
                    json!({"transform":"rotate-right"}),
                )
                .unwrap();
        }
        let described = service.describe_entry(&asset, None).unwrap();
        // The pixel layer is in the content stage, so it sits before the geometry tail however
        // late it was committed, and the orientation goes ahead of the crop it carries.
        assert_eq!(
            described
                .layers
                .iter()
                .map(|layer| (
                    layer.module.as_deref(),
                    layer.title.as_deref(),
                    layer.summary.as_str(),
                    layer.available
                ))
                .collect::<Vec<_>>(),
            [
                (
                    Some("luxforge.pixel"),
                    Some("Pixel"),
                    "Pixel 1, 2 → 4,5,6",
                    true
                ),
                (
                    Some("luxforge.transform"),
                    Some("Transforms"),
                    "Rotate 180°",
                    true
                ),
                (
                    Some("luxforge.crop"),
                    Some("Crop and straighten"),
                    "50% × 50%",
                    true
                ),
            ]
        );
        let current = service.state(&asset).unwrap().current_entry;
        assert_eq!(described.entry_id, current.id);
        assert_eq!(
            described
                .layers
                .iter()
                .map(|layer| layer.id.clone())
                .collect::<Vec<_>>(),
            current
                .snapshot
                .recipe
                .layers
                .iter()
                .map(|layer| layer.id.clone())
                .collect::<Vec<_>>(),
            "the stored order and identities"
        );
        // An earlier entry describes its own stack.
        assert!(
            service
                .describe_entry(&asset, Some(&original))
                .unwrap()
                .layers
                .is_empty()
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
