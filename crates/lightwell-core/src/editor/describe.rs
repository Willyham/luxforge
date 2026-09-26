use super::{
    AssetRecord, EditorService, EditorState, LayerDescription, RecipeDescription,
    catalog::{ASSET_COLUMNS, asset_row, stored_revision},
};
use crate::{AssetId, EntryId, Error, HistoryEntry, StageSize};
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

    /// Describe one entry's stored layers for the recipe panel: `O(layers)` registry lookups,
    /// payload reads and payload compiles, with no decode, no render and no source access. Each
    /// row carries the stage its layer receives, folded once over the stack from the asset's
    /// recorded extents ([`crate::ModuleRegistry::input_stages`]). A layer whose provider is missing
    /// or unavailable is listed with the reason, never omitted.
    pub fn describe_entry(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<RecipeDescription, Error> {
        let (asset, entry) = match entry_id {
            Some(entry_id) => (self.head(asset_id)?.asset, self.entry(asset_id, entry_id)?),
            None => {
                let state = self.state(asset_id)?;
                (state.asset, state.current_entry)
            }
        };
        let recipe = &entry.snapshot.recipe;
        let stages = self
            .registry
            .input_stages(asset.width, asset.height, recipe);
        let mut layers = Vec::with_capacity(recipe.layers.len());
        for (index, layer) in recipe.layers.iter().enumerate() {
            let input_stage = stages.get(index).map(|stage| StageSize {
                width: stage.width,
                height: stage.height,
            });
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
                    input_stage,
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
                        input_stage,
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
    use crate::editor::test_support::{
        SHRINK_ACTION, SHRINK_EFFECT, ShrinkModule, fixture, mutation, shrink, temp,
    };
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
                    Some("lightwell.pixel"),
                    Some("Pixel"),
                    "Pixel 1, 2 → 4,5,6",
                    true
                ),
                (
                    Some("lightwell.transform"),
                    Some("Transforms"),
                    "Rotate 180°",
                    true
                ),
                (
                    Some("lightwell.crop"),
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
        // A half turn swaps nothing, so every layer receives the fixture's own extents.
        let source = service.state(&asset).unwrap().asset;
        assert!(described.layers.iter().all(|layer| layer.input_stage
            == Some(StageSize {
                width: source.width,
                height: source.height
            })));
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

    /// Each row carries the stage its layer receives, by the compile's own fold: a colour layer
    /// the source's extents, and a crop behind another geometry layer and a quarter turn the stage
    /// both produced, its sides swapped by the turn. Every row is exactly the stage the
    /// host compiles for that prefix, which is what a module plans that layer against. Without a
    /// provider for a layer the fold stops there: that layer's own input is known and nothing
    /// after it is guessed.
    #[test]
    fn each_row_carries_the_stage_its_layer_receives() {
        let catalog = temp("describe-stages.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let source = service.state(&asset).unwrap().asset;
        let size = |width, height| Some(StageSize { width, height });
        // Straightened, so the crop resamples rather than maps exactly.
        let crop = json!({"angle": 10.0, "x": 0.2, "y": 0.2, "width": 0.5, "height": 0.5});
        let edits = [
            ("crop", crop),
            (SHRINK_ACTION, shrink(200, 120)),
            ("transform", json!({"transform": "rotate-right"})),
            ("set-basic", json!({"exposure": 0.5})),
        ];
        let entries = edits
            .into_iter()
            .enumerate()
            .map(|(revision, (action, parameters))| {
                service
                    .apply_action(
                        &asset,
                        mutation(revision as u64, action),
                        action,
                        parameters,
                    )
                    .unwrap()
                    .current_entry_id
            })
            .collect::<Vec<_>>();
        let described = service.describe_entry(&asset, None).unwrap();
        let recipe = service.state(&asset).unwrap().current_entry.snapshot.recipe;
        assert_eq!(
            described
                .layers
                .iter()
                .map(|layer| (layer.effect.as_str(), layer.input_stage))
                .collect::<Vec<_>>(),
            [
                (crate::BASIC_EFFECT, size(source.width, source.height)),
                (SHRINK_EFFECT, size(source.width, source.height)),
                (crate::ORIENTATION_EFFECT, size(200, 120)),
                (crate::CROP_EFFECT, size(120, 200)),
            ],
            "the stored order, each with the stage it receives"
        );
        for (index, layer) in described.layers.iter().enumerate() {
            let prefix = service
                .registry
                .compile_layers(
                    source.width,
                    source.height,
                    &recipe.layers[..index],
                    &recipe.masks,
                    &recipe.strokes,
                    &recipe.artifacts,
                )
                .unwrap()
                .stage();
            assert_eq!(
                layer.input_stage,
                size(prefix.width, prefix.height),
                "row {index} is the stage the host compiles for its prefix"
            );
        }
        // An earlier entry is described against its own stack: before the turn, the crop receives
        // the shrink's output unturned.
        let earlier = service.describe_entry(&asset, Some(&entries[1])).unwrap();
        assert_eq!(
            earlier
                .layers
                .iter()
                .map(|layer| layer.input_stage)
                .collect::<Vec<_>>(),
            [size(source.width, source.height), size(200, 120)]
        );
        drop(service);

        // Served without the shrink's provider, the same stack is still described: the shrink
        // row keeps the stage it receives, and the turn and the crop after it have none rather
        // than a guess.
        let service = EditorService::open(&catalog).unwrap();
        let described = service.describe_entry(&asset, None).unwrap();
        assert_eq!(
            described
                .layers
                .iter()
                .map(|layer| (layer.available, layer.input_stage))
                .collect::<Vec<_>>(),
            [
                (true, size(source.width, source.height)),
                (false, size(source.width, source.height)),
                (true, None),
                (true, None),
            ]
        );
        assert_eq!(
            serde_json::to_value(&described.layers[3]).unwrap()["input_stage"],
            serde_json::Value::Null,
            "an unknown stage is reported as null, never omitted"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
