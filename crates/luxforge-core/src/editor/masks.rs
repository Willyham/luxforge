use super::{
    AssetRecord, EditorService, PixelInput,
    source::{Evaluated, validate_source_recipe},
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    AssetId, EntryId, Error, MaskId, ModuleRegistry, Recipe,
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
    ///
    /// A painted stroke is checked against the occupancy cap here, where the asset gives the content
    /// stage its mask is drawn on: the component the stroke would leave behind is indexed at that
    /// stage and a densest cell over the cap refuses the stroke before anything commits, whether or
    /// not a layer draws the mask yet. A draft of the stroke plans through here too, so the refusal
    /// arrives while the stroke is being painted.
    pub(super) fn plan_mask_command(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
        command: &MaskCommand,
        target: &MaskTarget,
        parameters: &Map<String, Value>,
    ) -> Result<MaskOutcome, Error> {
        validate_source_recipe(&self.registry, asset, recipe)?;
        let seed = self.mask_colour_seed(asset, command, recipe, target, parameters)?;
        let outcome =
            crate::mask::commands::plan(command, recipe, target, parameters, &self.registry, seed)?;
        if command.op == crate::mask::commands::MaskOp::AddStroke
            && let MaskOutcome::Change(change) = &outcome
            && let (Some(mask), Some(component)) = (&change.mask, &change.component)
            && let Some(mask) = change.recipe.masks.iter().find(|held| &held.id == mask)
        {
            let content = crate::modules::Stage {
                width: asset.width,
                height: asset.height,
            };
            crate::mask::check_painted_occupancy(mask, component, content, &change.recipe.strokes)?;
        }
        Ok(outcome)
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
                serde_json::to_value(value).map_err(|error| Error::internal(error.to_string()))
            })
        }
        match query_id {
            crate::mask::commands::LIST => encode(self.mask_listing(asset_id, entry_id)),
            crate::mask::commands::SAMPLE_INPUT => {
                // Its declared parameters, which the generic check has required and checked.
                #[derive(serde::Deserialize)]
                struct SampleInput {
                    mask: MaskId,
                    x: u32,
                    y: u32,
                }
                let SampleInput { mask, x, y } =
                    serde_json::from_value(Value::Object(parameters.clone()))
                        .map_err(|error| Error::internal(error.to_string()))?;
                encode(self.mask_input_sample(asset_id, entry_id, &mask, x, y))
            }
            other => Err(Error::validation(format!("unknown query {other}"))),
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
                Error::validation(format!(
                    "a stroke limited to a colour must begin inside the picture, and \
                         ({:.4}, {:.4}) is outside the {}x{} stage the masked layer receives",
                    request.x, request.y, stage.width, stage.height
                ))
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
        validate_source_recipe(&self.registry, asset, recipe)?;
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
                return Err(Error::validation(format!(
                    "outside the stage: ({x}, {y}) is not inside the {}x{} stage the masked \
                         layer receives",
                    stage.width, stage.height
                )));
            }
            let rgba = context.sample_before(layer, x, y)?.ok_or_else(|| {
                Error::validation(format!(
                    "outside the stage: ({x}, {y}) has no pixel to read"
                ))
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
pub(crate) fn mask_target_parameter() -> crate::ParameterDescriptor {
    crate::ParameterDescriptor::identity(MASK_FIELD, crate::IdentityKind::Mask).notes(
        "the mask this edit applies through; omit it to edit the layer that applies everywhere. \
         The global layer and each mask are distinct targets, so this action commits or updates \
         one layer per target",
    )
}

/// What carries the host's optional `mask` target: an action or a query, by its identity. Those of
/// a module that declares a maskable effect accept it.
#[derive(Clone, Copy)]
pub(super) enum Targeted<'a> {
    Action(&'a str),
    Query(&'a str),
}

/// Take the `mask` target out of an action's or a query's request before its own parameters are
/// checked, so no module's `parse`, `plan`, `compile` or `query` ever sees it
/// (`docs/design/masking.md`, "How a mask reaches an effect"). The one target check: a commit, a
/// drafted gesture's beginning and a query all take their mask here.
///
/// Sending it to an action or query that does not accept one is a `validation` error naming it, not
/// a silently ignored field: an agent that believes it edited through a mask must be told it did
/// not.
pub(super) fn take_mask_target(
    registry: &ModuleRegistry,
    targeted: Targeted<'_>,
    parameters: &mut Value,
) -> Result<Option<MaskId>, Error> {
    let Some(field) = parameters
        .as_object_mut()
        .and_then(|object| object.remove(MASK_FIELD))
    else {
        return Ok(None);
    };
    let (accepts, what, id) = match targeted {
        Targeted::Action(id) => (registry.action_accepts_mask(id), "action", id),
        Targeted::Query(id) => (registry.query_accepts_mask(id), "query", id),
    };
    if !accepts {
        return Err(Error::validation(format!(
            "{what} {id} does not accept a mask target"
        )));
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
        .ok_or_else(|| Error::validation(format!("unknown mask {id} for this asset")))
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
/// maskable module, a composite's steps included, and for that module's own queries about the global
/// target, where the layers before the module's own layer are what is sampled and a masked layer of
/// the same effect is never among them. A query about a mask reads the whole stack instead, so the
/// stage before that mask's own layer holds the global layer's colour, as it renders.
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
    use std::sync::Arc;

    /// A draft's target is the action's declared identity parameters by name, whichever action it
    /// is: a host command's identities are checked as a patch's are, a module action takes the
    /// host's `mask` field through the one target check, and a name the action declares as a value
    /// rather than an identity is refused by name. The target a command's declaration builds
    /// (`crate::declared_target`) is the one the desktop sends.
    #[test]
    fn a_draft_target_is_the_actions_declared_identities() {
        let catalog = temp("draft-target.sqlite");
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let mask = MaskId::new();
        let component = crate::ComponentId::new();
        let target = |pairs: &[(&str, &str)]| -> crate::DraftTarget {
            pairs
                .iter()
                .map(|(name, identity)| ((*name).to_owned(), (*identity).to_owned()))
                .collect()
        };

        let stroke = crate::mask::commands::find(crate::mask::commands::ADD_STROKE).unwrap();
        let declared = crate::declared_target(&stroke.action, |kind| match kind {
            crate::IdentityKind::Mask => Some(mask.as_str().to_owned()),
            crate::IdentityKind::Component => Some(component.as_str().to_owned()),
            crate::IdentityKind::Stroke => Some("never asked".to_owned()),
        });
        assert_eq!(
            declared,
            target(&[("mask", mask.as_str()), ("component", component.as_str())]),
            "a stroke names the mask and component it paints on, the identities it declares"
        );
        assert_eq!(
            service
                .draft_target(&asset, stroke.method, declared.clone())
                .unwrap(),
            declared
        );
        assert_eq!(
            service
                .draft_target(&asset, "set-basic", target(&[("mask", mask.as_str())]))
                .unwrap(),
            target(&[("mask", mask.as_str())]),
            "a module action's target is the host's mask field"
        );
        assert_eq!(
            service
                .draft_target(&asset, "set-basic", target(&[("exposure", "0.5")]))
                .unwrap_err()
                .detail,
            "parameter exposure of action set-basic is not an identity; a draft's target names \
             only the objects its gesture edits"
        );
        let rename = crate::mask::commands::find("mask.rename").unwrap();
        assert_eq!(
            service
                .draft_target(
                    &asset,
                    rename.method,
                    target(&[("mask", mask.as_str()), ("name", "Sky")])
                )
                .unwrap_err()
                .detail,
            "parameter name of action mask.rename is not an identity; a draft's target names \
             only the objects its gesture edits"
        );
        assert_eq!(
            service
                .draft_target(&asset, stroke.method, target(&[("stroke", "not-an-id")]))
                .unwrap_err()
                .detail,
            "unknown parameter stroke for action mask.add-stroke"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// An action's and a query's `mask` target take the one target check: taken out of the request
    /// so the module never sees it, checked as an identity, and refused by name where the action or
    /// query does not accept one, a field left in place.
    #[test]
    fn an_action_and_a_query_take_their_mask_target_through_one_check() {
        let registry = ModuleRegistry::builtin();
        let mask = MaskId::new();
        for targeted in [
            Targeted::Action("set-basic"),
            Targeted::Query("neutral-sample"),
        ] {
            let mut request = json!({"mask": mask.as_str(), "x": 1});
            assert_eq!(
                take_mask_target(&registry, targeted, &mut request).unwrap(),
                Some(mask.clone())
            );
            assert_eq!(request, json!({"x": 1}), "the module never sees the target");
            let mut untargeted = json!({"x": 1});
            assert_eq!(
                take_mask_target(&registry, targeted, &mut untargeted).unwrap(),
                None
            );
            let error = take_mask_target(&registry, targeted, &mut json!({"mask": "not-a-mask"}))
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "{}", error.detail);
        }
        for (targeted, refusal) in [
            (
                Targeted::Action("crop"),
                "action crop does not accept a mask target",
            ),
            (
                Targeted::Query("no-such-query"),
                "query no-such-query does not accept a mask target",
            ),
        ] {
            let error = take_mask_target(&registry, targeted, &mut json!({"mask": mask.as_str()}))
                .unwrap_err();
            assert_eq!(
                (error.kind, error.detail.as_str()),
                (ErrorKind::Validation, refusal)
            );
        }
    }

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
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
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

        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
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
        service
            .prepare(&service.entry_needs(&asset, None).unwrap())
            .unwrap();
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

    /// What the `mask` request field does beyond what the field-patch conformance suite proves for
    /// every maskable module (a masked set commits one layer for its target and later ones update it
    /// in place, a global set leaves it alone, and the stack samples as it renders), through the one
    /// action path a GUI gesture and a JSON client share: a neutral first field through a mask
    /// commits nothing, each masked layer is placed after the global one and in its mask's order, a
    /// mask the stack does not hold is refused, and so is an action that has no target to give.
    #[test]
    fn the_mask_target_commits_updates_and_orders_one_layer_per_target() {
        let catalog = temp("mask-target.sqlite");
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
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
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();

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
        assert_eq!(committed[1].payload["exposure"], json!(0.8));

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
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
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

        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        service
            .prepare(&service.entry_needs(&asset, None).unwrap())
            .unwrap();
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
                    job.evaluation.source().render(
                        job.evaluation.registry(),
                        job.evaluation.entry().snapshot.id.clone(),
                        job.evaluation.recipe(),
                    )
                })
                .unwrap_err(),
            // Planning checks the stored stack's structure before it asks a module for a plan.
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
        // A stale revision is refused before anything is planned, naming both revisions.
        let stale = service
            .run_action(
                &asset,
                mutation(0, "stale"),
                "mask.set-invert",
                (MaskTarget {
                    mask: Some(mask.clone()),
                    ..MaskTarget::default()
                })
                .request(json!({"invert": true})),
            )
            .unwrap_err();
        assert_eq!(stale.kind, ErrorKind::Conflict);
        assert_eq!(stale.detail, "stale revision 0; current revision is 2");
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

    /// `mask.rename-component` writes exactly one entry, exactly as any other mutation, and undo
    /// restores the component's previous name — the ordinary history behaviour, proved on this
    /// command rather than assumed of it because it shares the mask family's one commit path.
    #[test]
    fn a_component_rename_writes_one_entry_and_undo_restores_the_name() {
        let catalog = temp("mask-rename-component.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let create = crate::mask::commands::find("mask.create-linear").unwrap();
        let created = service
            .run_action(
                &asset,
                mutation(0, "create"),
                create.method,
                (MaskTarget::default())
                    .request(json!({"x0": 0.0, "y0": 0.0, "x1": 0.0, "y1": 1.0})),
            )
            .unwrap();
        let mask = created.mask.clone().expect("a created mask");
        let component = created.component.clone().expect("its first component");
        let before = service.history(&asset, None, 10).unwrap().entries.len();

        let renamed = service
            .run_action(
                &asset,
                mutation(1, "rename-component"),
                "mask.rename-component",
                (MaskTarget {
                    mask: Some(mask.clone()),
                    component: Some(component.clone()),
                    name: Some("Sky edge".into()),
                    ..MaskTarget::default()
                })
                .request(Value::Null),
            )
            .unwrap();
        assert_eq!(
            renamed.label.as_deref(),
            Some("Rename Linear 1 to Sky edge")
        );
        assert_eq!(
            service.history(&asset, None, 10).unwrap().entries.len(),
            before + 1,
            "one entry for the rename, exactly as any other mutation"
        );
        let after_rename = service.state(&asset).unwrap();
        assert_eq!(
            after_rename.current_entry.snapshot.recipe.masks[0].components[0].name,
            "Sky edge"
        );

        service.undo(&asset, mutation(2, "undo")).unwrap();
        let after_undo = service.state(&asset).unwrap();
        assert_eq!(
            after_undo.current_entry.snapshot.recipe.masks[0].components[0].name, "Linear 1",
            "undo restores the component's previous name"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// One asset in a catalog of its own, driven through the one action path, answering the way a
    /// client reads each answer.
    struct Session {
        service: EditorService,
        asset: AssetId,
    }

    impl Session {
        fn open(name: &str) -> Self {
            let mut service = EditorService::open(&temp(name)).unwrap();
            let asset = service.import(&fixture()).unwrap().asset.id;
            Self { service, asset }
        }

        /// One command at the current revision; `Err` is its code and detail.
        fn run(
            &mut self,
            method: &str,
            target: &MaskTarget,
            parameters: Value,
            request: &str,
        ) -> Result<Value, Value> {
            let revision = self.service.state(&self.asset).unwrap().revision;
            self.service
                .run_action(
                    &self.asset,
                    mutation(revision, request),
                    method,
                    target.request(parameters),
                )
                .map(|result| serde_json::to_value(result).unwrap())
                .map_err(|error| json!({"code": error.kind.code(), "detail": error.detail}))
        }

        fn list(&self) -> Value {
            let entry = self.service.state(&self.asset).unwrap().current_entry.id;
            serde_json::to_value(self.service.mask_listing(&self.asset, &entry).unwrap()).unwrap()
        }

        /// Every entry as `[action_id, label]`, oldest first.
        fn labels(&self) -> Vec<Value> {
            let mut labels: Vec<Value> = self
                .service
                .history(&self.asset, None, 100)
                .unwrap()
                .entries
                .iter()
                .map(|entry| json!([entry.action_id, entry.label]))
                .collect();
            labels.reverse();
            labels
        }
    }

    /// The component of `mask` named `component`, resolved from the listing as a client resolves it.
    fn component_of(listing: &Value, mask: &str, component: &str) -> MaskTarget {
        let found = listing["masks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["name"] == json!(mask))
            .unwrap_or_else(|| panic!("no mask named {mask} in {listing}"));
        let id = found["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["name"] == json!(component))
            .unwrap_or_else(|| panic!("no component named {component} in {mask}"))["id"]
            .as_str()
            .unwrap();
        MaskTarget {
            mask: Some(MaskId::parse(found["id"].as_str().unwrap()).unwrap()),
            component: Some(crate::ComponentId::parse(id).unwrap()),
            ..MaskTarget::default()
        }
    }

    /// One painted stroke as a request: the path and the brush it was drawn with.
    fn stroke(points: Value, size: f64, feather: f64, flow: f64, erase: bool) -> Value {
        json!({"points": points, "size": size, "feather": feather, "flow": flow, "erase": erase})
    }

    /// The strokes one component holds, in stored order, by content address.
    fn strokes_of(listing: &Value, mask: &str, component: &str) -> Vec<String> {
        let target = component_of(listing, mask, component);
        listing["masks"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|mask| mask["components"].as_array().unwrap())
            .find(|candidate| candidate["id"] == json!(target.component.as_ref().unwrap().as_str()))
            .unwrap()["payload"]["strokes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|address| address.as_str().unwrap().to_owned())
            .collect()
    }

    /// One stroke's content address as a target's `stroke` field.
    fn at(target: &MaskTarget, address: &str) -> MaskTarget {
        MaskTarget {
            stroke: Some(crate::path::StrokeId::parse(address.to_owned()).unwrap()),
            ..target.clone()
        }
    }

    /// The brush's two commands end to end: the first stroke draws a mask, later strokes update the
    /// component it made, a second brush joins the same mask in the mode it was given, and one
    /// stroke is deleted from the middle as a forward edit — each one entry, named for what it did —
    /// and every refusal the design states for them, by name.
    #[test]
    fn painting_is_one_entry_a_stroke_and_a_delete_is_a_forward_edit() {
        use crate::mask::commands::{ADD_STROKE, DELETE_STROKE};
        let mut session = Session::open("mask-painting.sqlite");
        // The first stroke on nothing: a mask, a brush component and the stroke, as one entry.
        session
            .run(
                ADD_STROKE,
                &MaskTarget::default(),
                stroke(
                    json!([[0.2, 0.2], [0.4, 0.4], [0.6, 0.4]]),
                    0.1,
                    50.0,
                    100.0,
                    false,
                ),
                "paint-1",
            )
            .expect("the first stroke draws a mask");
        let brush = component_of(&session.list(), "Mask 1", "Brush 1");
        // Every later stroke on that component is one entry of its own, which is what makes undo
        // walk back one stroke at a time.
        for (index, path) in [json!([[0.3, 0.7], [0.5, 0.7]]), json!([[0.7, 0.2]])]
            .into_iter()
            .enumerate()
        {
            session
                .run(
                    ADD_STROKE,
                    &brush,
                    stroke(path, 0.05, 20.0, 60.0, false),
                    &format!("paint-more-{index}"),
                )
                .expect("a further stroke");
        }
        // A second brush on the same mask, in the mode the gesture chose before it started.
        let mask = MaskTarget {
            component: None,
            ..brush.clone()
        };
        let mut subtract = stroke(json!([[0.5, 0.5], [0.55, 0.55]]), 0.08, 0.0, 100.0, false);
        subtract["mode"] = json!("subtract");
        session
            .run(ADD_STROKE, &mask, subtract, "paint-subtract")
            .expect("a subtract brush");
        // An erase stroke inside the first brush: a property of the stroke, not of the component.
        session
            .run(
                ADD_STROKE,
                &brush,
                stroke(json!([[0.35, 0.35], [0.45, 0.4]]), 0.04, 30.0, 100.0, true),
                "paint-erase",
            )
            .expect("an erase stroke");
        let held = strokes_of(&session.list(), "Mask 1", "Brush 1");
        // A forward edit: one entry appended, the named stroke gone, every other stroke where it was.
        session
            .run(DELETE_STROKE, &at(&brush, &held[1]), Value::Null, "unpaint")
            .expect("a stroke is deleted");
        assert_eq!(
            strokes_of(&session.list(), "Mask 1", "Brush 1"),
            [held[0].clone(), held[2].clone(), held[3].clone()],
            "only the named stroke goes and the rest keep their order"
        );

        let mut refusals = Vec::new();
        // A stroke appended to a component that exists takes no mode: a component's mode is changed
        // by the command that changes one, and an ignored field is never an answer.
        let mut moded = stroke(json!([[0.1, 0.1]]), 0.05, 10.0, 50.0, false);
        moded["mode"] = json!("intersect");
        refusals.push(
            session
                .run(ADD_STROKE, &brush, moded, "refuse-mode")
                .expect_err("a mode on an appended stroke"),
        );
        // A gradient is not painted on: its geometry is declared, and the refusal names the method
        // that does edit it.
        session
            .run(
                "mask.create-linear",
                &MaskTarget::default(),
                json!({"x0": 0.0, "y0": 0.0, "x1": 0.0, "y1": 1.0}),
                "a-gradient",
            )
            .expect("a gradient mask");
        let gradient = component_of(&session.list(), "Mask 2", "Linear 1");
        refusals.push(
            session
                .run(
                    ADD_STROKE,
                    &gradient,
                    stroke(json!([[0.5, 0.5]]), 0.05, 10.0, 50.0, false),
                    "refuse-kind",
                )
                .expect_err("a stroke on a gradient"),
        );
        // A stroke the component does not hold, and the last stroke of a component, are both named
        // refusals rather than something approximate.
        refusals.push(
            session
                .run(
                    DELETE_STROKE,
                    &at(&brush, &"0".repeat(32)),
                    Value::Null,
                    "refuse-absent",
                )
                .expect_err("a stroke that is not there"),
        );
        let listing = session.list();
        let two = component_of(&listing, "Mask 1", "Brush 2");
        let only = strokes_of(&listing, "Mask 1", "Brush 2");
        refusals.push(
            session
                .run(
                    DELETE_STROKE,
                    &at(&two, &only[0]),
                    Value::Null,
                    "refuse-last",
                )
                .expect_err("a component's only stroke"),
        );
        assert_eq!(
            refusals,
            [
                json!({"code": "validation", "detail":
                    "a stroke appended to an existing component takes no mode; change a \
                     component's mode with mask.set-component-mode"}),
                json!({"code": "validation", "detail":
                    "component Linear 1 is a linear component, whose geometry is declared rather \
                     than drawn; patch it with mask.set-linear"}),
                json!({"code": "validation", "detail":
                    "component Brush 1 holds no stroke 00000000000000000000000000000000"}),
                json!({"code": "validation", "detail": format!(
                    "stroke {} is the only stroke of Brush 2; delete the component instead",
                    only[0]
                )}),
            ]
        );

        // The design's granularity table, in the order the journey painted it, each entry storing
        // the command that wrote it. A mask's name prefixes a label once the stack holds more than
        // one, which is the delivered rule and not the brush's.
        let painted: Vec<Value> = session
            .labels()
            .into_iter()
            .filter(|entry| entry[0].as_str().unwrap().starts_with("mask."))
            .collect();
        assert_eq!(
            painted,
            [
                json!([ADD_STROKE, "Add brush"]),
                json!([ADD_STROKE, "Update Brush 1"]),
                json!([ADD_STROKE, "Update Brush 1"]),
                json!([ADD_STROKE, "Add subtract brush"]),
                json!([ADD_STROKE, "Update Brush 1"]),
                json!([DELETE_STROKE, "Delete a stroke from Brush 1"]),
                json!(["mask.create-linear", "Mask 2 · Add linear"]),
            ],
            "one entry a stroke, named for what that stroke did"
        );
        // No coordinate is ever written into a component payload: a payload holds addresses and
        // nothing else, which is what keeps one entry a stroke from copying every earlier stroke.
        let payload = &session.list()["masks"][0]["components"][0]["payload"];
        assert_eq!(
            payload.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["strokes"],
            "a brush payload carries the reserved strokes field and nothing else"
        );
        assert!(
            payload["strokes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|address| address.as_str().is_some_and(|text| text.len() == 32)),
            "every stroke is a content address, never a position"
        );
    }

    /// Decimation happens where the store says it does, and it is idempotent, so a desktop that
    /// decimates before it posts and an agent that posts the path it captured reach the same stored
    /// stroke — the same address, and therefore the same coverage.
    #[test]
    fn a_decimated_path_and_the_path_it_came_from_are_the_same_stored_stroke() {
        // A path a pointer produces: many positions along a straight run, which decimation at the
        // stroke's own radius reduces to its two ends.
        let captured: Vec<[f64; 2]> = (0..=64)
            .map(|step| [0.2 + f64::from(step) * 0.005, 0.3])
            .collect();
        let decimated = crate::path::decimate(&captured, 0.1).unwrap();
        assert_eq!(decimated.len(), 2, "a straight run keeps its ends");
        let address = |path: &[[f64; 2]], request: &str| -> String {
            let mut session = Session::open(&format!("mask-decimation-{request}.sqlite"));
            session
                .run(
                    crate::mask::commands::ADD_STROKE,
                    &MaskTarget::default(),
                    stroke(json!(path), 0.1, 50.0, 100.0, false),
                    request,
                )
                .expect("a stroke");
            strokes_of(&session.list(), "Mask 1", "Brush 1")[0].clone()
        };
        assert_eq!(
            address(&captured, "raw"),
            address(&decimated, "decimated"),
            "the same path, however much of it the client posted"
        );
    }

    #[test]
    fn a_global_and_a_masked_layer_of_one_effect_coexist_in_mask_order() {
        let mut session = Session::open("mask-coexist.sqlite");
        let mask = session
            .run(
                "mask.create-linear",
                &MaskTarget::default(),
                json!({"x0": 0.0, "y0": 0.5, "x1": 1.0, "y1": 0.5}),
                "create",
            )
            .unwrap()["mask"]
            .clone();
        let mask = MaskId::parse(mask.as_str().unwrap()).unwrap();
        let mut edit = |parameters: Value, request: &str| {
            let revision = session.service.state(&session.asset).unwrap().revision;
            session
                .service
                .apply_action(
                    &session.asset,
                    mutation(revision, request),
                    "set-basic",
                    parameters,
                )
                .unwrap_or_else(|error| panic!("set-basic failed: {error}"));
        };
        // The masked edit first, then the global one: the global layer must still land before it.
        edit(json!({"mask": mask.as_str(), "exposure": 1.0}), "masked");
        edit(json!({"exposure": -0.5}), "global");

        let state = session.service.state(&session.asset).unwrap();
        let targets: Vec<_> = state
            .current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .filter(|l| l.effect_id == crate::BASIC_EFFECT)
            .map(|l| l.mask.clone())
            .collect();
        assert_eq!(
            targets,
            vec![None, Some(mask)],
            "one effect holds one global layer and one masked layer, the global one first"
        );

        // And the stack still renders, which "ambiguous Basic layers" would have prevented.
        session
            .service
            .prepare(&session.service.entry_needs(&session.asset, None).unwrap())
            .unwrap();
        session
            .service
            .render_current(&session.asset)
            .expect("a stack holding a global and a masked layer of one effect renders");
    }
}
