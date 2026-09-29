use super::{
    AssetRecord, EditorService, EditorState, LayerDescription, RecipeDescription,
    catalog::{ASSET_COLUMNS, asset_row, stored_revision},
    entries::Head,
};
use crate::{
    AssetId, EntryId, Error, HistoryEntry, Layer, LayerReport, ModuleRegistry, ORIENTATION_EFFECT,
    Orientation, StageSize, modules::stored_orientation,
};

impl EditorState {
    /// The state a cached head and its current entry describe: a copy of both.
    pub(super) fn of(head: Head, current: &HistoryEntry) -> Self {
        Self {
            asset: head.asset,
            revision: head.revision,
            current_entry: current.clone(),
            redo: head.redo,
        }
    }
}

impl EditorService {
    /// The asset, its revision, its current entry with its strokes resolved, and what redo would
    /// return to. After the first read of an asset and its current entry this decodes and hashes
    /// nothing: it copies the cached head and entry, which every write that moves them updates.
    pub fn state(&self, asset_id: &AssetId) -> Result<EditorState, Error> {
        let head = self.head(asset_id)?;
        let entry = self.shared_entry(asset_id, &head.current)?;
        Ok(EditorState::of(head, &entry))
    }

    /// The id of the asset's current entry, and nothing else: what a caller that only names or
    /// compares the current entry reads, instead of [`Self::state`], which copies the whole entry.
    /// It copies the cached head and never touches an entry, cached or not.
    pub(crate) fn current_entry_id(&self, asset_id: &AssetId) -> Result<EntryId, Error> {
        Ok(self.head(asset_id)?.current)
    }

    /// The asset's current revision, which is all a draft's conflict check compares. It decodes
    /// nothing, whether or not the asset's head is cached.
    pub(crate) fn revision(&self, asset_id: &AssetId) -> Result<u64, Error> {
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

    /// Describe one entry's stored layers for the recipe panel against the asset's recorded
    /// extents ([`ModuleRegistry::describe_recipe`]). A layer whose provider is missing or
    /// unavailable is listed with the reason, never omitted.
    pub(crate) fn describe_entry(
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
        Ok(self
            .registry
            .describe_recipe(asset.width, asset.height, &entry))
    }
}

impl ModuleRegistry {
    /// Describe one entry's stored layers, for a source of these extents: `O(layers)` registry
    /// lookups, payload reads and payload compiles, with no decode, no render and no source access.
    /// Each row carries the stage its layer receives, folded once over the stack
    /// (`Self::stages`), and the orientation the orientation layers before it composed; the
    /// description ends with the stack's own output stage and orientation, which a layer appended
    /// to it would receive. A layer whose provider is missing or unavailable is listed with the
    /// reason, never omitted.
    pub fn describe_recipe(
        &self,
        source_width: u32,
        source_height: u32,
        entry: &HistoryEntry,
    ) -> RecipeDescription {
        let recipe = &entry.snapshot.recipe;
        let stages = self.stages(source_width, source_height, recipe);
        let size = |index: usize| {
            stages.get(index).map(|stage| StageSize {
                width: stage.width,
                height: stage.height,
            })
        };
        // The orientation ahead of each layer, composed in stack order; an orientation payload
        // that cannot be read leaves every later one unknown rather than guessed.
        let mut orientation = Some(Orientation::NEUTRAL);
        let mut layers = Vec::with_capacity(recipe.layers.len());
        for (index, layer) in recipe.layers.iter().enumerate() {
            let input_stage = size(index);
            let input_orientation = input_stage.and(orientation);
            if layer.effect_id == ORIENTATION_EFFECT {
                orientation = orientation.and_then(|ahead| {
                    stored_orientation(layer)
                        .ok()
                        .map(|next| ahead.followed_by(next))
                });
            }
            let descriptor = self
                .effect(&layer.effect_id)
                .map(|(provider, _)| provider.descriptor());
            // A layer the registry cannot describe is listed with the reason, never omitted, and
            // the rest of the stack is still described.
            let (report, available) = match self.layer_report(layer) {
                Ok(report) => (report, true),
                Err(reason) => (LayerReport::new(reason), false),
            };
            layers.push(LayerDescription {
                id: layer.id.clone(),
                effect: layer.effect_id.clone(),
                module: descriptor.map(|descriptor| descriptor.id.clone()),
                title: descriptor.map(|descriptor| descriptor.title.clone()),
                summary: report.summary,
                values: report.values,
                available,
                mask: layer.mask.clone(),
                artifacts: layer.artifacts.clone(),
                neutral: report.neutral,
                input_stage,
                input_orientation,
            });
        }
        let output_stage = size(recipe.layers.len());
        RecipeDescription {
            entry_id: entry.id.clone(),
            layers,
            output_stage,
            output_orientation: output_stage.and(orientation),
        }
    }

    /// One stored layer as its available provider describes it ([`crate::ToolModule::describe`]),
    /// or, as the summary its recipe row shows instead, why nothing can: `no provider`,
    /// `unavailable: <reason>` or `unreadable payload: <detail>`. Such a layer is not neutral,
    /// since nothing can say it changes nothing. Reading the payload only.
    pub fn layer_report(&self, layer: &Layer) -> Result<LayerReport, String> {
        let (provider, _) = self
            .effect(&layer.effect_id)
            .ok_or_else(|| "no provider".to_owned())?;
        if let crate::Availability::Unavailable { reason } = &provider.descriptor().availability {
            return Err(format!("unavailable: {reason}"));
        }
        provider
            .describe(&layer.effect_id, layer.effect_format, &layer.payload)
            .map_err(|error| format!("unreadable payload: {}", error.detail))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::test_support::{
        SHRINK_ACTION, SHRINK_EFFECT, ShrinkModule, fixture, mutation, shrink, temp,
    };
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn an_entrys_layers_are_described_in_order_with_their_provider() {
        let catalog = temp("describe.sqlite");
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let original = service.state(&asset).unwrap().current_entry.id;
        let fixture_size = {
            let asset = service.state(&asset).unwrap().asset;
            StageSize {
                width: asset.width,
                height: asset.height,
            }
        };
        assert_eq!(
            service.describe_entry(&asset, None).unwrap(),
            RecipeDescription {
                entry_id: original.clone(),
                layers: Vec::new(),
                output_stage: Some(fixture_size),
                output_orientation: Some(Orientation::NEUTRAL),
            },
            "the original entry has no layers, and a first layer would receive the source"
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
        // A half turn swaps nothing, so every layer receives the fixture's own extents.
        assert!(
            described
                .layers
                .iter()
                .all(|layer| layer.input_stage == Some(fixture_size))
        );
        // The crop receives the half turn the orientation layer ahead of it holds, and so does
        // whatever would follow the crop.
        let half =
            Orientation::of(crate::Transform::RotateRight).then(crate::Transform::RotateRight);
        assert_eq!(
            described
                .layers
                .iter()
                .map(|layer| layer.input_orientation)
                .collect::<Vec<_>>(),
            [
                Some(Orientation::NEUTRAL),
                Some(Orientation::NEUTRAL),
                Some(half)
            ]
        );
        assert_eq!(described.output_orientation, Some(half));
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
        // The stack's own output is the whole stack's compiled stage, turned by the quarter turn
        // ahead of the crop, and only the crop receives that turn.
        let whole = service
            .registry
            .compile_layers(
                source.width,
                source.height,
                &recipe.layers,
                &recipe.masks,
                &recipe.strokes,
                &recipe.artifacts,
            )
            .unwrap()
            .stage();
        assert_eq!(described.output_stage, size(whole.width, whole.height));
        let right = Orientation::of(crate::Transform::RotateRight);
        assert_eq!(
            described
                .layers
                .iter()
                .map(|layer| layer.input_orientation)
                .collect::<Vec<_>>(),
            [
                Some(Orientation::NEUTRAL),
                Some(Orientation::NEUTRAL),
                Some(Orientation::NEUTRAL),
                Some(right)
            ]
        );
        assert_eq!(described.output_orientation, Some(right));
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
        assert_eq!(
            described
                .layers
                .iter()
                .map(|layer| layer.input_orientation)
                .collect::<Vec<_>>(),
            [
                Some(Orientation::NEUTRAL),
                Some(Orientation::NEUTRAL),
                None,
                None
            ],
            "no orientation is reported for a stage that is not known"
        );
        assert_eq!(
            (described.output_stage, described.output_orientation),
            (None, None),
            "nor is the output of a stack whose output cannot be known"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A stack with no crop layer describes the stage its output has, which is the stage a crop
    /// appended to it would receive, and the orientation that output was given: here a shrink and
    /// then a quarter turn, so a new crop would frame the turned, shrunk photograph. The rows alone
    /// could not say it, since no row follows the turn.
    #[test]
    fn the_output_stage_is_what_a_layer_appended_to_the_stack_receives() {
        let catalog = temp("describe-output.sqlite");
        let mut service = EditorService::open_with(&catalog, ShrinkModule::registry()).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let edits = [
            (SHRINK_ACTION, shrink(200, 120)),
            ("transform", json!({"transform": "rotate-right"})),
        ];
        for (revision, (action, parameters)) in edits.into_iter().enumerate() {
            service
                .apply_action(
                    &asset,
                    mutation(revision as u64, action),
                    action,
                    parameters,
                )
                .unwrap();
        }
        let described = service.describe_entry(&asset, None).unwrap();
        assert!(
            described
                .layers
                .iter()
                .all(|layer| layer.effect != crate::CROP_EFFECT),
            "no crop layer"
        );
        assert_eq!(
            described.output_stage,
            Some(StageSize {
                width: 120,
                height: 200
            }),
            "the shrunk stage, turned"
        );
        assert_eq!(
            described.output_orientation,
            Some(Orientation::of(crate::Transform::RotateRight))
        );
        let wire = serde_json::to_value(&described).unwrap();
        assert_eq!(wire["output_stage"], json!({"width": 120, "height": 200}));
        assert_eq!(
            wire["output_orientation"],
            json!({"mirror": false, "turns": 1})
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
