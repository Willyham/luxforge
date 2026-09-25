use super::{
    AssetRecord, EditorService, PixelInput,
    source::{Evaluated, validate_source_recipe},
};
use crate::{
    AssetId, EntryId, Error, ErrorKind, MaskId, ModuleRegistry, Recipe,
    mask::commands::{MaskCommand, MaskListing, MaskOutcome, MaskTarget},
};
use serde_json::{Map, Value};

/// One 8-bit RGBA sample as the linear-sRGB triple a mask's value-based parts evaluate on, through the
/// delivered decode and nothing else, so one definition of "linear sRGB" serves the whole editor.
fn linear_triple(rgba: [u8; 4]) -> [f64; 3] {
    let linear = crate::colour::srgb::decode_pixel([rgba[0], rgba[1], rgba[2]]);
    [
        f64::from(linear[0]),
        f64::from(linear[1]),
        f64::from(linear[2]),
    ]
}

impl EditorService {
    /// What one checked `mask.*` command does to `recipe`, the stack of `asset` it is planned
    /// against: the host's half of [`Self::plan_request`], the one planning step a commit and a
    /// drafted gesture share, so a draft equals its commit by construction.
    ///
    /// Unlike a module's plan it compiles nothing up front: it reads no pixel unless a stroke asks
    /// to be limited to a colour, and that one is seeded here, by the host, from the pixel the
    /// operation this mask modulates receives at the position the stroke began. The request named
    /// the limit and never the colour, so nothing a client sends can put a colour in a stroke that
    /// the photograph does not have at that position, and the command family's planner stays pure —
    /// it is handed the pixel rather than reading one.
    pub(super) fn plan_mask_command(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        command: &MaskCommand,
        target: &MaskTarget,
        parameters: &Map<String, Value>,
    ) -> Result<MaskOutcome, Error> {
        validate_source_recipe(asset, recipe)?;
        let seed = self.mask_colour_seed(asset, command, recipe, target, parameters)?;
        crate::mask::commands::plan(command, recipe, target, parameters, &self.registry, seed)
    }

    /// One of the host's own reads about its own objects, from one entry's stack, with its
    /// parameters already checked against the host descriptor's query: `mask.list` or
    /// `mask.sample-input`. It writes nothing, emits nothing and touches no session state.
    pub(super) fn host_query(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        query_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<Value, Error> {
        fn encode(value: Result<impl serde::Serialize, Error>) -> Result<Value, Error> {
            value.and_then(|value| {
                serde_json::to_value(value)
                    .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
            })
        }
        match query_id {
            crate::mask::commands::LIST => encode(self.mask_listing(asset_id, entry_id)),
            crate::mask::commands::SAMPLE_INPUT => {
                let (target, values) = MaskTarget::split(parameters)?;
                let coordinate = |name: &str| {
                    values
                        .get(name)
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or_else(|| {
                            Error::new(
                                ErrorKind::Validation,
                                format!("missing required parameter {name} for {query_id}"),
                            )
                        })
                };
                let mask = target.mask.as_ref().ok_or_else(|| {
                    Error::new(
                        ErrorKind::Validation,
                        format!("missing required parameter mask for {query_id}"),
                    )
                })?;
                encode(self.mask_input_sample(
                    asset_id,
                    entry_id,
                    mask,
                    coordinate("x")?,
                    coordinate("y")?,
                ))
            }
            other => Err(Error::new(
                ErrorKind::Validation,
                format!("unknown query {other}"),
            )),
        }
    }

    /// The pixel a limited stroke is seeded on, as the three sRGB codes the host sampled, or `None`
    /// when the command asks for no limit.
    ///
    /// The **rule** — which layer's input, and what a mask no layer is bound to means — is the
    /// command family's, stated once in `mask::commands::input_layer_index`; the **pixel** is read
    /// here, because the editor is the only thing that can evaluate one. That split is what makes a
    /// stored seed a colour the photograph has: a request carries a flag and a path, never a colour,
    /// so no client can put anything else in a stroke.
    fn mask_colour_seed(
        &self,
        asset: &AssetRecord,
        command: &MaskCommand,
        recipe: &Recipe,
        target: &MaskTarget,
        parameters: &Map<String, Value>,
    ) -> Result<Option<[u8; 3]>, Error> {
        let Some(request) =
            crate::mask::commands::colour_limit_request(command, recipe, target, parameters)?
        else {
            return Ok(None);
        };
        // Reading the pixel compiles the stack, so its artifacts are bound first.
        let recipe = self.bound(recipe)?;
        self.with_stage_context(asset, &recipe, None, |context| {
            let stage = context.stage_before(request.layer)?;
            // The stroke's positions are normalized against the stage its mask is compiled against,
            // which is the stage this layer receives, so the pixel is that stage's own. A stroke that
            // began outside the picture — an ordinary gesture, which the stored range allows — has no
            // input pixel to read and is refused by name rather than clamped to an edge whose colour
            // nobody chose.
            let pixel = |value: f64, side: u32| -> Option<u32> {
                let index = (value * f64::from(side)).floor();
                (index >= 0.0 && index < f64::from(side)).then_some(index as u32)
            };
            let outside = || {
                Error::new(
                    ErrorKind::Validation,
                    format!(
                        "a stroke limited to a colour must begin inside the picture, and \
                         ({:.4}, {:.4}) is outside the {}x{} stage the masked layer receives",
                        request.x, request.y, stage.width, stage.height
                    ),
                )
            };
            let x = pixel(request.x, stage.width).ok_or_else(outside)?;
            let y = pixel(request.y, stage.height).ok_or_else(outside)?;
            let rgba = context
                .sample_before(request.layer, x, y)?
                .ok_or_else(outside)?;
            // The codes, not the decoded colour: a stroke is addressed by the hash of its bytes, and
            // an integer survives a JSON round trip exactly where an `f64` does not. Decoding is the
            // delivered one and happens where the stroke is compiled.
            Ok(Some([rgba[0], rgba[1], rgba[2]]))
        })
    }

    /// `mask.sample-input`: the pixel the operation one mask modulates receives, at one content
    /// position of the stage that operation's layer receives, in linear sRGB.
    ///
    /// Read-only: it reads one entry's snapshot, writes nothing, emits nothing and touches no session
    /// state. It is where a canvas pick gets the colour a colour range's swatch is, and it exists so
    /// a client never has to decode one: the frame a client can see holds the masked operation's
    /// *output*, and a range selection is evaluated on its *input*, so a colour read from the picture
    /// would be a different colour. Cost is one `O(layers)` point evaluation and no frame is
    /// allocated.
    pub fn mask_input_sample(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        mask: &MaskId,
        x: u32,
        y: u32,
    ) -> Result<PixelInput, Error> {
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        let asset = &state.asset;
        let recipe = &entry.snapshot.recipe;
        validate_source_recipe(asset, recipe)?;
        let layer = crate::mask::commands::input_layer_index(recipe, mask)?;
        let sampled = self
            .bound(recipe)
            .and_then(|bound| self.input_sample(asset, &bound, layer, x, y));
        self.needing(Evaluated::exactly(asset, &entry.id, recipe), sampled)
    }

    /// The pixel the layer at `layer` of a bound stack receives at `(x, y)`, in linear sRGB.
    fn input_sample(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        layer: usize,
        x: u32,
        y: u32,
    ) -> Result<PixelInput, Error> {
        self.with_stage_context(asset, recipe, None, |context| {
            let stage = context.stage_before(layer)?;
            if x >= stage.width || y >= stage.height {
                return Err(Error::new(
                    ErrorKind::Validation,
                    format!(
                        "outside the stage: ({x}, {y}) is not inside the {}x{} stage the masked \
                         layer receives",
                        stage.width, stage.height
                    ),
                ));
            }
            let rgba = context.sample_before(layer, x, y)?.ok_or_else(|| {
                Error::new(
                    ErrorKind::Validation,
                    format!("outside the stage: ({x}, {y}) has no pixel to read"),
                )
            })?;
            let [r, g, b] = linear_triple(rgba);
            Ok(PixelInput {
                r,
                g,
                b,
                x,
                y,
                width: stage.width,
                height: stage.height,
            })
        })
    }

    /// `mask.list`: every mask of one stored stack with its components, its values and the layers
    /// bound to it. Read-only in every sense — it reads one entry's snapshot and writes nothing,
    /// emits nothing and touches no session state.
    pub fn mask_listing(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
    ) -> Result<MaskListing, Error> {
        let entry = self.entry(asset_id, entry_id)?;
        Ok(crate::mask::commands::listing(
            entry.id.clone(),
            &entry.snapshot.recipe,
            &self.registry,
        ))
    }
}

/// The host's one optional top-level request field on every action of a maskable effect.
pub const MASK_FIELD: &str = "mask";

/// The host's `mask` field as the identity parameter it is: validated by the one generic check, and
/// published by `schema.list` as the `target` of every action that accepts it. It is declared by the
/// host rather than by the module, because no module parses, plans or compiles a mask.
pub fn mask_target_parameter() -> crate::ParameterDescriptor {
    crate::ParameterDescriptor::identity(MASK_FIELD, crate::IdentityKind::Mask).notes(
        "the mask this edit applies through; omit it to edit the layer that applies everywhere. \
         The global layer and each mask are distinct targets, so this action commits or updates \
         one layer per target",
    )
}

/// Take the `mask` target out of a request before the action's own parameters are checked, so no
/// module's `parse`, `plan` or `compile` ever sees it (`docs/design/masking.md`, "How a mask reaches
/// an effect").
///
/// Sending it to an action that does not accept one is a `validation` error naming the action, not a
/// silently ignored field: an agent that believes it edited through a mask must be told it did not.
pub(super) fn take_mask_target(
    registry: &ModuleRegistry,
    action_id: &str,
    parameters: &mut Value,
) -> Result<Option<MaskId>, Error> {
    let Some(object) = parameters.as_object_mut() else {
        return Ok(None);
    };
    let Some(field) = object.remove(MASK_FIELD) else {
        return Ok(None);
    };
    if !registry.action_accepts_mask(action_id) {
        return Err(Error::new(
            ErrorKind::Validation,
            format!("action {action_id} does not accept a mask target"),
        ));
    }
    // The generic check of the identity kind, the one every mask identity takes.
    crate::check_value(&mask_target_parameter(), &field)?;
    let text = field
        .as_str()
        .expect("an identity the check accepted is a string");
    Ok(Some(MaskId::parse(text)?))
}

/// The mask a request named, resolved against the stack it will edit. A target the recipe does not
/// hold is refused before anything is planned, so an edit never creates a layer bound to a mask that
/// does not exist.
pub(super) fn resolve_mask_target<'a>(
    recipe: &'a Recipe,
    mask: Option<&MaskId>,
) -> Result<Option<&'a crate::Mask>, Error> {
    let Some(id) = mask else {
        return Ok(None);
    };
    recipe
        .masks
        .iter()
        .find(|mask| &mask.id == id)
        .map(Some)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                format!("unknown mask {id} for this asset"),
            )
        })
}

/// The stack as one target sees it: the layers of every maskable effect that belong to some *other*
/// target are hidden, and everything else is exactly where it was.
///
/// This is what makes a target a target without a single line of module code. A module finds its own
/// layer through [`crate::StageContext::own_layer`], which answers for the context's target by the
/// same rule, [`ModuleRegistry::in_target`], so `edit.set-basic {mask, exposure}` commits and updates
/// the masked layer while `edit.set-basic {exposure}` keeps editing the global one; hiding the other
/// targets' layers is what keeps their colour out of what the module samples.
///
/// Every stage answer the context gives is unchanged by the hiding, because only a colour-, pixel-
/// or spatial-stage effect may be maskable and none of those changes the stage's dimensions: the
/// geometry tail is never hidden, so `stage`, `stage_before` and `insertion_index` answer exactly
/// what they answer for the whole stack. What a sampler reads does change — it no longer includes the
/// other targets' colour — and that is why the filtered view is used only for planning an action of a
/// maskable module, a composite's steps included, and for that module's own queries, where the layers
/// before the module's own layer are what is sampled and a masked layer of the same effect is never
/// among them.
///
/// A recipe with no masks is handed back as it is, so the ordinary path allocates nothing.
pub(super) fn recipe_for_target<'a>(
    registry: &ModuleRegistry,
    recipe: &'a Recipe,
    accepts_mask: bool,
    mask: Option<&MaskId>,
) -> std::borrow::Cow<'a, Recipe> {
    if !accepts_mask || recipe.masks.is_empty() {
        return std::borrow::Cow::Borrowed(recipe);
    }
    std::borrow::Cow::Owned(Recipe {
        format: recipe.format,
        layers: recipe
            .layers
            .iter()
            .filter(|layer| registry.in_target(layer, mask))
            .cloned()
            .collect(),
        masks: recipe.masks.clone(),
        strokes: recipe.strokes.clone(),
        artifacts: recipe.artifacts.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{
        ActionResult, EditorState, MutationOutcome, MutationResult,
        test_support::{fixture, mutation, stored_entry_json, temp},
    };
    use crate::{Component, ComponentMode, HistoryEntry, Mask, Snapshot, SnapshotId, Transform};
    use rusqlite::{Connection, params};
    use serde_json::json;
    use std::path::Path;

    /// One stored mask with a component the host can compile, which is what a persisted masked stack
    /// looks like. A component of a kind this build knows nothing about is its own case and is
    /// asserted where the kind table lives: the model proves the bytes survive a round trip, and the
    /// module registry proves every path that would have to draw the mask refuses it by name while
    /// reading, listing and validating still work.
    fn stored_mask() -> Mask {
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name("linear");
        mask.components.push(Component::new(
            name,
            ComponentMode::Add,
            "linear",
            json!({"x0": 0.25, "y0": 0.5, "x1": 0.75, "y1": 0.5}),
        ));
        mask
    }

    /// The entry a masked stack would commit, over the current one: the same layers, with the last
    /// one bound to `mask`, and `masks` as given so a dangling reference can be planted too.
    fn masked_entry(state: &EditorState, mask: &Mask, masks: Vec<Mask>) -> HistoryEntry {
        let mut layers = state.current_entry.snapshot.recipe.layers.clone();
        layers.last_mut().expect("a layer to mask").mask = Some(mask.id.clone());
        HistoryEntry {
            id: EntryId::new(),
            sequence: state.current_entry.sequence + 1,
            label: "Mask 1 exposure +0.50".into(),
            undo_parent: Some(state.current_entry.id.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot: Snapshot {
                id: SnapshotId::new(),
                asset_id: state.asset.id.clone(),
                recipe: Recipe {
                    format: crate::RECIPE_FORMAT,
                    layers,
                    masks,
                    ..Recipe::default()
                },
            },
            ..state.current_entry.clone()
        }
    }

    /// Write one entry and make it current without going through a mutation. The `mask.*` commands
    /// arrive later, so this is the only way to hold a stored masked stack against reopen now; a
    /// dangling reference could not be written through [`EditorService::mutate`] at all, which is its own
    /// guarantee and is asserted below.
    fn plant(catalog: &Path, entry: &HistoryEntry) {
        let connection = Connection::open(catalog).unwrap();
        connection
            .execute(
                "INSERT INTO entries (id,asset_id,sequence,action_id,label,actor,timestamp_ms,
                                      undo_parent_id,restore_target_id,entry_json)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    entry.id.as_str(),
                    entry.asset_id.as_str(),
                    entry.sequence as i64,
                    entry.action_id,
                    entry.label,
                    entry.actor,
                    entry.timestamp_ms,
                    entry.undo_parent.as_ref().map(EntryId::as_str),
                    entry.restore_target.as_ref().map(EntryId::as_str),
                    serde_json::to_string(entry).unwrap(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE asset_state SET current_entry_id=?1, revision=?2 WHERE asset_id=?3",
                params![
                    entry.id.as_str(),
                    entry.result_revision as i64,
                    entry.asset_id.as_str()
                ],
            )
            .unwrap();
    }

    /// Masks ride inside the snapshot every entry already stores, so reopen returns them unchanged
    /// and an ordinary later edit carries them with no second persistence path.
    #[test]
    fn masks_ride_in_the_stored_snapshot_and_reopen_unchanged() {
        let catalog = temp("masks.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_action(
                &asset,
                mutation(0, "basic"),
                "set-basic",
                json!({"exposure": 0.5}),
            )
            .unwrap();
        let state = service.state(&asset).unwrap();
        let mask = stored_mask();
        let entry = masked_entry(&state, &mask, vec![mask.clone()]);
        drop(service);
        plant(&catalog, &entry);

        let mut service = EditorService::open(&catalog).unwrap();
        let reopened = service.state(&asset).unwrap();
        assert_eq!(
            reopened.current_entry, entry,
            "every field of the entry, masks included, survives reopen"
        );
        let stored = &reopened.current_entry.snapshot.recipe.masks[0].components[0];
        assert_eq!(stored.kind, "linear", "the stored kind is kept as it is");
        assert_eq!(
            serde_json::to_string(&stored.payload).unwrap(),
            serde_json::to_string(&mask.components[0].payload).unwrap(),
            "its payload is retained byte for byte, unparsed"
        );
        assert_eq!(
            reopened
                .current_entry
                .snapshot
                .recipe
                .layers
                .last()
                .unwrap()
                .mask
                .as_ref(),
            Some(&mask.id)
        );
        assert_eq!(
            service.entry(&asset, &entry.id).unwrap(),
            entry,
            "the same entry reads the same by identity"
        );
        // The recipe panel still lists every layer, so a masked stack is never hidden from it.
        assert_eq!(
            service.describe_entry(&asset, None).unwrap().layers.len(),
            reopened.current_entry.snapshot.recipe.layers.len()
        );
        // A later mutation writes the mask table on in its own snapshot: one persistence path.
        let next = service
            .apply_pixel(
                &asset,
                mutation(reopened.revision, "pixel"),
                1,
                1,
                [9, 9, 9],
            )
            .unwrap()
            .current_entry_id;
        let committed = service.entry(&asset, &next).unwrap().snapshot.recipe;
        assert_eq!(committed.masks, vec![mask.clone()]);
        assert!(
            committed
                .layers
                .iter()
                .any(|layer| layer.mask.as_ref() == Some(&mask.id)),
            "the masked layer keeps its mask through an unrelated edit"
        );
        // History keeps the earlier unmasked snapshot as it was: nothing was rewritten.
        assert!(
            service
                .entry(&asset, &state.current_entry.id)
                .unwrap()
                .snapshot
                .recipe
                .masks
                .is_empty()
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// The same entry with a mask table and no layer bound to anything: the state a client is in
    /// after `mask.create`, which arrives with its own task, so the table is planted here instead.
    fn planted_masks(state: &EditorState, masks: Vec<Mask>) -> HistoryEntry {
        HistoryEntry {
            id: EntryId::new(),
            sequence: state.current_entry.sequence + 1,
            label: "Add linear".into(),
            undo_parent: Some(state.current_entry.id.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot: Snapshot {
                id: SnapshotId::new(),
                asset_id: state.asset.id.clone(),
                recipe: Recipe {
                    masks,
                    ..state.current_entry.snapshot.recipe.clone()
                },
            },
            ..state.current_entry.clone()
        }
    }

    /// The whole of what the `mask` request field does, through the one action path a GUI gesture and
    /// a JSON client share: it commits the masked layer on the first non-neutral field, updates that
    /// same layer in place afterwards, leaves the global layer alone, places each masked layer after
    /// the global one and in its mask's order, and refuses an action that has no target to give.
    #[test]
    fn the_mask_target_commits_updates_and_orders_one_layer_per_target() {
        let catalog = temp("mask-target.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        // One global Basic layer first, so the ordering rule has something to place a mask after.
        service
            .apply_action(
                &asset,
                mutation(0, "global"),
                "set-basic",
                json!({"exposure": 0.5}),
            )
            .unwrap();
        let state = service.state(&asset).unwrap();
        let first = stored_mask();
        let mut second = stored_mask();
        second.name = "Mask 2".into();
        let planted = planted_masks(&state, vec![first.clone(), second.clone()]);
        drop(service);
        plant(&catalog, &planted);
        let mut service = EditorService::open(&catalog).unwrap();

        fn revision(service: &EditorService, asset: &AssetId) -> u64 {
            service.state(asset).unwrap().revision
        }
        fn layers(service: &EditorService, asset: &AssetId) -> Vec<crate::Layer> {
            service
                .state(asset)
                .unwrap()
                .current_entry
                .snapshot
                .recipe
                .layers
                .clone()
        }
        let global_layer = layers(&service, &asset)[0].id.clone();
        assert_eq!(
            layers(&service, &asset).len(),
            1,
            "one global Basic layer to begin with"
        );

        // A neutral first field through a mask commits nothing at all: masking nothing is nothing.
        let quiet = service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "neutral-masked"),
                "set-basic",
                json!({"mask": first.id, "exposure": 0.0}),
            )
            .unwrap();
        assert_eq!(quiet.outcome, MutationOutcome::NoOp);
        assert_eq!(layers(&service, &asset).len(), 1);

        // The first non-neutral field commits the masked layer, after the global one.
        service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "mask-one"),
                "set-basic",
                json!({"mask": first.id, "exposure": 0.8}),
            )
            .unwrap();
        let committed = layers(&service, &asset);
        assert_eq!(committed.len(), 2);
        assert_eq!(committed[0].id, global_layer, "the global layer is first");
        assert_eq!(committed[0].mask, None);
        assert_eq!(committed[1].mask.as_ref(), Some(&first.id));
        let masked_layer = committed[1].id.clone();
        assert_eq!(committed[1].payload["exposure"], json!(0.8));

        // A later field updates that same layer in place, keeping its identity and position.
        service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "mask-one-again"),
                "set-basic",
                json!({"mask": first.id, "exposure": 0.9}),
            )
            .unwrap();
        let updated = layers(&service, &asset);
        assert_eq!(updated.len(), 2, "no second layer for the same target");
        assert_eq!(updated[1].id, masked_layer, "the masked layer's identity");
        assert_eq!(updated[1].payload["exposure"], json!(0.9));
        // And sending the same value again is the no-op it looks like.
        assert_eq!(
            service
                .apply_action(
                    &asset,
                    mutation(revision(&service, &asset), "mask-one-noop"),
                    "set-basic",
                    json!({"mask": first.id, "exposure": 0.9}),
                )
                .unwrap()
                .outcome,
            MutationOutcome::NoOp
        );

        // The global layer is still edited by the same action without the field, and the masked
        // layer is untouched by it.
        service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "global-again"),
                "set-basic",
                json!({"exposure": 0.25}),
            )
            .unwrap();
        let both = layers(&service, &asset);
        assert_eq!(both.len(), 2);
        assert_eq!(both[0].id, global_layer);
        assert_eq!(both[0].payload["exposure"], json!(0.25));
        assert_eq!(both[1].id, masked_layer);
        assert_eq!(both[1].payload["exposure"], json!(0.9));

        // A layer in the second mask is legal for the same single-layer effect and lands after the
        // first mask's, because that is the order the mask list shows.
        service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "mask-two"),
                "set-basic",
                json!({"mask": second.id, "exposure": -1.0}),
            )
            .unwrap();
        let three = layers(&service, &asset);
        assert_eq!(three.len(), 3);
        assert_eq!(
            three
                .iter()
                .map(|layer| layer.mask.clone())
                .collect::<Vec<_>>(),
            vec![None, Some(first.id.clone()), Some(second.id.clone())],
            "the global layer, then the masks in their own order"
        );
        // The stack renders and samples: a masked layer is evaluable the moment it is creatable.
        service.render_current(&asset).unwrap();

        // A target the stack does not hold is refused, and nothing is written.
        let absent = Mask::new("Mask 9");
        let error = service
            .apply_action(
                &asset,
                mutation(revision(&service, &asset), "absent-mask"),
                "set-basic",
                json!({"mask": absent.id, "exposure": 0.1}),
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        assert_eq!(
            error.detail,
            format!("unknown mask {} for this asset", absent.id)
        );

        // The field belongs to the actions of a maskable effect and nowhere else, and the refusal
        // names the action that was asked.
        for (action, parameters) in [
            (
                "set-pixel",
                json!({"mask": first.id, "x": 0, "y": 0, "rgb": [1, 2, 3]}),
            ),
            (
                "transform",
                json!({"mask": first.id, "transform": "rotate-left"}),
            ),
            ("set-vignette", json!({"mask": first.id, "amount": -40.0})),
        ] {
            let error = service
                .apply_action(
                    &asset,
                    mutation(revision(&service, &asset), action),
                    action,
                    parameters,
                )
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "{action}");
            assert_eq!(
                error.detail,
                format!("action {action} does not accept a mask target"),
                "{action}"
            );
        }
        assert_eq!(
            layers(&service, &asset).len(),
            3,
            "no refusal changed the stack"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A stored layer naming a mask its own snapshot does not carry is incompatible data: every
    /// evaluation path refuses it by name, the stack stays readable and its stored bytes do not
    /// change. The host cannot write such a stack in the first place, which is asserted here too.
    #[test]
    fn a_stored_layer_naming_a_missing_mask_is_refused_on_every_path_and_left_as_it_is() {
        let catalog = temp("dangling-mask.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_action(
                &asset,
                mutation(0, "basic"),
                "set-basic",
                json!({"exposure": 0.5}),
            )
            .unwrap();
        let state = service.state(&asset).unwrap();
        let mask = stored_mask();
        let dangling = masked_entry(&state, &mask, Vec::new());
        // Nothing valid can be written from it either: every write validates the whole recipe.
        assert_eq!(
            service
                .registry()
                .validate_recipe(&dangling.snapshot.recipe)
                .unwrap_err()
                .kind,
            ErrorKind::Incompatible,
            "every commit admits the same recipe before any row is written"
        );
        drop(service);
        plant(&catalog, &dangling);
        let before = stored_entry_json(&catalog, &dangling.id);

        let mut service = EditorService::open(&catalog).unwrap();
        let revision = service.state(&asset).unwrap().revision;
        let expected = format!(
            "layer {} references mask {}, which this recipe does not carry",
            dangling.snapshot.recipe.layers.last().unwrap().id,
            mask.id
        );
        let refusals = [
            service.render_current(&asset).unwrap_err(),
            service.render_entry(&asset, &dangling.id).unwrap_err(),
            service
                .sample_entry(&asset, &dangling.id, 0, 0)
                .unwrap_err(),
            service
                .locate_entry(&asset, &dangling.id, 0, 0)
                .unwrap_err(),
            // The preview worker's own render of the job the owner built for it.
            service
                .preview_job(&asset, None, None, None, None)
                .and_then(|job| {
                    job.source
                        .render(&job.registry, job.entry.snapshot.id.clone(), &job.recipe)
                })
                .unwrap_err(),
            // Planning compiles the stored stack before it asks a module for a plan.
            service
                .apply_pixel(&asset, mutation(revision, "pixel"), 0, 0, [1, 2, 3])
                .unwrap_err(),
            service
                .apply_transform(&asset, mutation(revision, "turn"), Transform::RotateLeft)
                .unwrap_err(),
        ];
        for error in refusals {
            assert_eq!(error.kind, ErrorKind::Incompatible);
            assert_eq!(error.detail, expected);
        }
        // Readable, unchanged and still listed in history: refusing is not discarding.
        let state = service.state(&asset).unwrap();
        assert_eq!(state.current_entry, dangling);
        assert_eq!(service.history(&asset, None, 10).unwrap().entries.len(), 3);
        drop(service);
        assert_eq!(
            stored_entry_json(&catalog, &dangling.id),
            before,
            "the refused entry's stored JSON is untouched"
        );
        std::fs::remove_file(catalog).unwrap();
    }

    /// A mask command's request stores its whole answer in the same transaction as the change, so a
    /// retry — in the same session or after a restart — answers with the mask and component the
    /// first attempt minted, read from the request table rather than reconstructed from history.
    #[test]
    fn a_retried_mask_command_answers_with_the_identities_it_minted() {
        let catalog = temp("mask-retry.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let command = crate::mask::commands::find("mask.create-linear").unwrap();
        let geometry = json!({"x0": 0.0, "y0": 0.0, "x1": 0.0, "y1": 1.0});
        let first = service
            .run_action(
                &asset,
                mutation(0, "create"),
                command.method,
                (MaskTarget::default()).request(geometry.clone()),
            )
            .unwrap();
        let mask = first.mask.clone().expect("a created mask");
        let component = first.component.clone().expect("its first component");
        // A later command moves the head and the history on; the retry still answers the first.
        service
            .run_action(
                &asset,
                mutation(1, "rename"),
                "mask.rename",
                (MaskTarget {
                    mask: Some(mask.clone()),
                    name: Some("Sky".into()),
                    ..MaskTarget::default()
                })
                .request(Value::Null),
            )
            .unwrap();
        let retry = |service: &mut EditorService| {
            service
                .run_action(
                    &asset,
                    mutation(0, "create"),
                    command.method,
                    (MaskTarget::default()).request(geometry.clone()),
                )
                .unwrap()
        };
        let again = retry(&mut service);
        assert!(again.mutation.deduplicated);
        assert_eq!(
            ActionResult {
                mutation: MutationResult {
                    deduplicated: false,
                    ..again.mutation.clone()
                },
                ..again.clone()
            },
            first,
            "the retry is the first answer, marked deduplicated"
        );
        drop(service);

        // The row holds the whole answer, identities included.
        let stored: String = Connection::open(&catalog)
            .unwrap()
            .query_row(
                "SELECT result_json FROM requests WHERE asset_id=?1 AND request_id='create'",
                params![asset.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        let stored: ActionResult = serde_json::from_str(&stored).unwrap();
        assert_eq!(stored.mask.as_ref(), Some(&mask));
        assert_eq!(stored.component.as_ref(), Some(&component));
        assert_eq!(stored.label.as_deref(), Some("Add linear"));

        // And after a restart.
        let mut service = EditorService::open(&catalog).unwrap();
        let reopened = retry(&mut service);
        assert_eq!(reopened.mask, Some(mask));
        assert_eq!(reopened.component, Some(component));
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
