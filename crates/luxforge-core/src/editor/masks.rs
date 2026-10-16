use super::{
    AreaInput, AreaRect, AreaStage, AssetRecord, EditorService, PixelInput,
    source::{Evaluated, validate_source_recipe},
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    AssetId, EntryId, Error, MaskId, ModuleRegistry, Recipe, Region, Stage, StageQuestions,
    mask::commands::{
        AREA_POINTS, HostRead, MaskCommand, MaskListing, MaskOutcome, MaskTarget, ModelStale,
        SAMPLE_INPUT, SAMPLE_INPUT_AREA,
    },
    tiles::{
        Answered,
        region::{MAX_REDUCTION, RegionRequest, RegionStage, aligned},
    },
};
use serde_json::{Map, Value, json};

impl EditorService {
    /// What one checked `mask.*` command does to `recipe`, the stack of `asset` it is planned
    /// against: the host's half of [`Self::plan_request`], the one planning step a commit and a
    /// drafted gesture share, so a draft equals its commit by construction.
    ///
    /// Unlike a module's plan it compiles nothing up front: it reads no pixel unless a stroke asks
    /// to be limited to a colour or the command is a pick. A limited stroke is seeded here, by the
    /// host, from the pixel the operation this mask modulates receives at the position the stroke
    /// began, and a pick reads that input at the point or over the area it names
    /// ([`Self::mask_pick_read`]). The request names a position and never a colour, so nothing a
    /// client sends can put a colour in a mask that the photograph does not have there, and the
    /// command family's planner stays pure — it is handed the pixels rather than reading them.
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
        let picked = self.mask_pick_read(asset, command, recipe, target, parameters)?;
        let read = match (&seed, &picked) {
            (Some(seed), _) => HostRead::Seed(*seed),
            (None, Some(picked)) => HostRead::Picked(picked.as_picked()),
            (None, None) => HostRead::Nothing,
        };
        let outcome =
            crate::mask::commands::plan(command, recipe, target, parameters, &self.registry, read)?;
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
    /// parameters already checked against the host descriptor's query: `mask.list`,
    /// `mask.sample-input` or `mask.sample-input-area`. It writes nothing, emits nothing and
    /// touches no session state.
    ///
    /// The two pixel reads are answered by [`input_read`], here over the host's own stage
    /// questions: off the catalog owner from the reference, and on it by deferring the read, which
    /// the dispatcher then asks of the tile service whole, where [`input_read`] answers it again
    /// over that service's reads.
    pub(super) fn host_query(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        query_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<Value, Error> {
        if query_id == crate::mask::commands::LIST {
            return self
                .mask_listing(asset_id, entry_id)
                .and_then(|listing| encode(&listing));
        }
        let state = self.state(asset_id)?;
        let entry = self.entry(asset_id, entry_id)?;
        let asset = &state.asset;
        let recipe = &entry.snapshot.recipe;
        validate_source_recipe(&self.registry, asset, recipe)?;
        let answered = self.bound(recipe).and_then(|bound| {
            self.with_stage_context(asset, &bound, None, |context| {
                input_read(
                    &self.registry,
                    &bound,
                    query_id,
                    parameters,
                    context.questions,
                    &|| None,
                )
            })
        });
        self.needing(Evaluated::exactly(asset, &entry.id, recipe), answered)
    }

    /// What one `mask.pick-<kind>` read at its mask's input site, or `None` for any other command.
    ///
    /// The **rule** — which component, which site, click or drag — is the command family's
    /// ([`crate::mask::commands::pick_request`]); the **pixels** are read here through the stage
    /// context's deferred query seam, as Corrections' automatic source is: on the catalog owner the
    /// read is parked and answered by the tile service, and the command is planned again with the
    /// answer once ([`super::pixels`]). A click reads `mask.sample-input` at the content pixel
    /// holding the point; a drag reads `mask.sample-input-area` over the rectangle, so the pick
    /// measures exactly what those reads answer a client.
    fn mask_pick_read(
        &self,
        asset: &AssetRecord,
        command: &MaskCommand,
        recipe: &Recipe,
        target: &MaskTarget,
        parameters: &Map<String, Value>,
    ) -> Result<Option<PickedPixels>, Error> {
        let Some(request) = crate::mask::commands::pick_request(
            command,
            recipe,
            target,
            parameters,
            &self.registry,
        )?
        else {
            return Ok(None);
        };
        let recipe = self.bound(recipe)?;
        self.with_stage_context(asset, &recipe, None, |context| {
            let questions = context.questions;
            let mask = request.mask.as_str();
            let Some((width, height)) = request.area else {
                let stage = questions.stage_before(request.site.index)?;
                let outside = || {
                    Error::validation(format!(
                        "outside the stage: a pick at ({:.4}, {:.4}) is not inside the {}x{} \
                         stage the masked layer receives",
                        request.x, request.y, stage.width, stage.height
                    ))
                };
                let x = content_pixel(request.x, stage.width).ok_or_else(outside)?;
                let y = content_pixel(request.y, stage.height).ok_or_else(outside)?;
                let answer = questions
                    .query(SAMPLE_INPUT, &object(json!({"mask": mask, "x": x, "y": y})))?;
                let point: PickedPoint = decode(answer)?;
                return Ok(Some(PickedPixels::Point([point.r, point.g, point.b])));
            };
            let answer = questions.query(
                SAMPLE_INPUT_AREA,
                &object(json!({"mask": mask, "x": request.x, "y": request.y,
                               "width": width, "height": height})),
            )?;
            let area: PickedArea = decode(answer)?;
            Ok(Some(PickedPixels::Area(area.points)))
        })
    }

    /// The pixel a limited stroke is seeded on, as the three sRGB codes the host sampled, or `None`
    /// when the command asks for no limit.
    ///
    /// The **rule** — which position's input, and what a mask no layer is bound to means — is the
    /// command family's, stated once in [`crate::mask::commands::input_site`]; the **pixel** is read
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
        let Some(request) = crate::mask::commands::colour_limit_request(
            command,
            recipe,
            target,
            parameters,
            &self.registry,
        )?
        else {
            return Ok(None);
        };
        // Reading the pixel compiles the stack, so its artifacts are bound first.
        let recipe = self.bound(recipe)?;
        self.with_stage_context(asset, &recipe, None, |context| {
            let stage = context.stage_before(request.site.index)?;
            // The stroke's positions are normalized against the stage its mask is compiled against,
            // which is the stage its site receives, so the pixel is that stage's own. A stroke that
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
            // Codes are the stage's bytes whichever way an operation receives the stage, so the
            // site's mode does not change them.
            let rgba = context
                .sample_before(request.site.index, x, y)?
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
    /// would be a different colour. On the catalog owner the pixel is read by its tile service;
    /// here, off it, with the reference renderer, which names no reason.
    pub fn mask_input_sample(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        mask: &MaskId,
        x: u32,
        y: u32,
    ) -> Result<PixelInput, Error> {
        let parameters = object(json!({"mask": mask.as_str(), "x": x, "y": y}));
        decode(self.host_query(asset_id, entry_id, SAMPLE_INPUT, &parameters)?)
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
        let mut listing = crate::mask::commands::listing(
            entry.id.clone(),
            &entry.snapshot.recipe,
            &self.registry,
        );
        let has_model = listing
            .masks
            .iter()
            .flat_map(|mask| &mask.components)
            .any(|component| component.model.is_some());
        if !has_model {
            return Ok(listing);
        }
        // What a selection computed now would record: unknown, rather than current, for a stack
        // that cannot be evaluated.
        let state = self.state(asset_id)?;
        let now = self
            .selection_input(&state.asset, &entry.snapshot.recipe)
            .ok();
        for component in listing
            .masks
            .iter_mut()
            .flat_map(|mask| &mut mask.components)
        {
            let stored = entry
                .snapshot
                .recipe
                .masks
                .iter()
                .flat_map(|mask| &mask.components)
                .find(|held| held.id == component.id)
                .and_then(crate::mask::model_selection);
            let (Some(report), Some(stored)) = (component.model.as_mut(), stored) else {
                continue;
            };
            report.computed_ms = self.artifact_created_ms(&report.artifact)?;
            report.stale = now.as_ref().map(|(fingerprint, prefix)| {
                let mut reasons = Vec::new();
                if &stored.input_fingerprint != fingerprint {
                    reasons.push(ModelStale::Source);
                }
                if &stored.prefix_hash != prefix {
                    reasons.push(ModelStale::RepairPrefix);
                }
                if self
                    .declared_model_version(&stored.model)
                    .is_some_and(|version| version != stored.model_version)
                {
                    reasons.push(ModelStale::ModelVersion);
                }
                reasons
            });
        }
        Ok(listing)
    }

    /// What a model selection of `recipe`, a stack of `asset`, computed now would record: the
    /// source identity and the repair stage's output identity
    /// ([`crate::modules::corrections_selection_prefix`]). What a selection task stores in
    /// `input_fingerprint` and `prefix_hash`, and what `mask.list` compares them with. Binds the
    /// stack's artifacts and compiles the prefix before the repair layer; reads no pixel.
    pub(crate) fn selection_input(
        &self,
        asset: &AssetRecord,
        recipe: &Recipe,
    ) -> Result<(String, String), Error> {
        let recipe = self.bound(recipe)?;
        let top = self.with_stage_context(asset, &recipe, None, |context| {
            crate::modules::corrections_top_input(context)
        })?;
        Ok((
            top.source.clone(),
            crate::modules::corrections_selection_prefix(&top),
        ))
    }

    /// The version a registered module now declares for the model resource `model`, when one
    /// declares it: what Recompute would run.
    pub(super) fn declared_model_version(&self, model: &str) -> Option<String> {
        self.registry
            .descriptors()
            .into_iter()
            .find_map(|descriptor| {
                descriptor
                    .resource(model)
                    .map(|resource| resource.version.clone())
            })
    }
}

/// What a pick read, owned: [`crate::mask::Picked`] borrows it for the planner.
pub(super) enum PickedPixels {
    Point([f64; 3]),
    Area(Vec<[f64; 3]>),
}

impl PickedPixels {
    fn as_picked(&self) -> crate::mask::Picked<'_> {
        match self {
            Self::Point(rgb) => crate::mask::Picked::Point(*rgb),
            Self::Area(points) => crate::mask::Picked::Area(points),
        }
    }
}

/// The host's two pixel reads about a mask — `mask.sample-input` at one content pixel and
/// `mask.sample-input-area` over one rectangle — answered over `questions` about `recipe`, a bound
/// stack. **One implementation for every reader**: the host's own questions off the catalog owner,
/// which read from the reference; the tile service's, which read from the GPU where it draws the
/// stage; and the analysis worker's, which hold the region the tile service read. `answered` names
/// the renderer of the latest point read where the questions record one; a region read carries its
/// own.
///
/// Both read at the mask's input site ([`crate::mask::commands::input_site`]), so a mask no layer is
/// bound to answers what a first adjustment through it would receive. A position outside the stage
/// is refused rather than clamped. `O(layers)` to plan; the area's read holds one band of the region
/// and at most [`AREA_POINTS`] reduced sums.
pub(crate) fn input_read(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    query: &str,
    parameters: &Map<String, Value>,
    questions: &dyn StageQuestions,
    answered: &dyn Fn() -> Option<Answered>,
) -> Result<Value, Error> {
    // The declared parameters, which the generic check has required and range-checked.
    #[derive(serde::Deserialize)]
    struct Point {
        mask: MaskId,
        x: u32,
        y: u32,
    }
    #[derive(serde::Deserialize)]
    struct Area {
        mask: MaskId,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }
    let parameters = Value::Object(parameters.clone());
    match query {
        SAMPLE_INPUT => {
            let Point { mask, x, y } = decode(parameters)?;
            let site = crate::mask::commands::input_site(registry, recipe, &mask)?;
            let stage = questions.stage_before(site.index)?;
            let outside = || {
                Error::validation(format!(
                    "outside the stage: ({x}, {y}) is not inside the {}x{} stage the masked layer \
                     receives",
                    stage.width, stage.height
                ))
            };
            if x >= stage.width || y >= stage.height {
                return Err(outside());
            }
            let [r, g, b] = questions.input_at(site, x, y)?.ok_or_else(outside)?;
            encode(&PixelInput {
                r,
                g,
                b,
                x,
                y,
                width: stage.width,
                height: stage.height,
                renderer: answered().map_or_else(crate::Renderer::headless, |answered| {
                    crate::Renderer::from(&answered)
                }),
            })
        }
        SAMPLE_INPUT_AREA => {
            let Area {
                mask,
                x,
                y,
                width,
                height,
            } = decode(parameters)?;
            let site = crate::mask::commands::input_site(registry, recipe, &mask)?;
            let stage = questions.stage_before(site.index)?;
            let rect = area_rect(stage, [x, y, width, height])?;
            let read = questions.region_before(&RegionRequest {
                stage: RegionStage::Before(site.index),
                rect,
                reduction: area_reduction(rect, stage),
                mode: Some(site.mode),
            })?;
            encode(&AreaInput {
                stage: AreaStage {
                    width: read.stage.width,
                    height: read.stage.height,
                },
                rect: AreaRect {
                    x0: read.rect.x0,
                    y0: read.rect.y0,
                    width: read.rect.width,
                    height: read.rect.height,
                },
                reduction: read.reduction,
                size: read.size,
                points: read.linear,
                renderer: crate::Renderer::from(&read.answered),
            })
        }
        other => Err(Error::internal(format!("{other} reads no pixel"))),
    }
}

/// The stage pixels a rectangle `[x, y, width, height]` of normalized content coordinates covers:
/// widened to whole pixels — at least the one pixel its corner falls in — and clipped to `stage`. A
/// rectangle that misses the stage is refused, not clamped onto its edge.
fn area_rect(stage: Stage, [x, y, width, height]: [f64; 4]) -> Result<Region, Error> {
    let span = |start: f64, extent: f64, side: u32| {
        let side = f64::from(side);
        let first = (start * side).floor();
        let last = ((start + extent) * side).ceil().max(first + 1.0);
        let (first, last) = (first.max(0.0), last.min(side));
        (first < last).then_some((first as u32, (last - first) as u32))
    };
    match (span(x, width, stage.width), span(y, height, stage.height)) {
        (Some((x0, width)), Some((y0, height))) => Ok(Region {
            x0,
            y0,
            width,
            height,
        }),
        _ => Err(Error::validation(format!(
            "outside the stage: the area at ({x:.4}, {y:.4}) sized {width:.4} x {height:.4} misses \
             the {}x{} stage the masked layer receives",
            stage.width, stage.height
        ))),
    }
}

/// The smallest power-of-two block side that reduces `rect`, aligned to whole blocks of the stage's
/// block grid as the region read aligns it, to at most [`AREA_POINTS`] points.
fn area_reduction(rect: Region, stage: Stage) -> u32 {
    let mut reduction = 1;
    while reduction < MAX_REDUCTION {
        let read = aligned(rect, reduction, stage);
        let points =
            u64::from(read.width.div_ceil(reduction)) * u64::from(read.height.div_ceil(reduction));
        if points <= AREA_POINTS as u64 {
            break;
        }
        reduction *= 2;
    }
    reduction
}

/// The content pixel holding normalized position `value` along a side of `side` pixels, or `None`
/// outside it.
fn content_pixel(value: f64, side: u32) -> Option<u32> {
    let index = (value * f64::from(side)).floor();
    (index >= 0.0 && index < f64::from(side)).then_some(index as u32)
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(object) => object,
        _ => Map::new(),
    }
}

fn encode(value: &impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|error| Error::internal(error.to_string()))
}

/// The values of a click's [`PixelInput`] a pick derives from. Only these are read back: the
/// answer's renderer may name a reason no session carries, such as `refused` from a launch that
/// refused the GPU, which [`crate::Renderer`] does not read back.
#[derive(serde::Deserialize)]
struct PickedPoint {
    r: f64,
    g: f64,
    b: f64,
}

/// The points of a drag's [`AreaInput`], read back as [`PickedPoint`] is and for its reason.
#[derive(serde::Deserialize)]
struct PickedArea {
    points: Vec<[f64; 3]>,
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|error| Error::internal(error.to_string()))
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
    use luxforge_test_macros::slow;
    use rusqlite::{Connection, params};
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    /// A dragged area is widened to whole pixels and clipped to the stage, a rectangle that misses
    /// it is refused, and its reduction is the smallest power of two that leaves at most
    /// `AREA_POINTS` points of the block-aligned read.
    #[test]
    fn an_area_is_reduced_by_the_smallest_power_of_two_within_its_point_bound() {
        let stage = Stage {
            width: 6000,
            height: 4000,
        };
        let points = |rect: Region, reduction: u32| {
            let read = aligned(rect, reduction, stage);
            read.width.div_ceil(reduction) as usize * read.height.div_ceil(reduction) as usize
        };
        let whole = area_rect(stage, [0.0, 0.0, 1.0, 1.0]).unwrap();
        assert_eq!(
            whole,
            Region {
                x0: 0,
                y0: 0,
                width: 6000,
                height: 4000
            }
        );
        let side = |pixels: f64| pixels / 6000.0;
        let tall = |pixels: f64| pixels / 4000.0;
        for rect in [
            whole,
            area_rect(stage, [0.25, 0.25, 0.5, 0.5]).unwrap(),
            // 64 x 64 pixels is exactly the bound; one more column needs a reduction.
            area_rect(stage, [side(100.0), tall(100.0), side(64.0), tall(64.0)]).unwrap(),
            area_rect(stage, [side(100.0), tall(100.0), side(65.0), tall(64.0)]).unwrap(),
            // Off the block grid: a 64-pixel side starting at an odd pixel.
            area_rect(stage, [side(101.0), tall(3.0), side(64.0), tall(64.0)]).unwrap(),
        ] {
            let reduction = area_reduction(rect, stage);
            assert!(reduction.is_power_of_two(), "{rect:?}");
            assert!(
                points(rect, reduction) <= AREA_POINTS,
                "{rect:?} at {reduction}"
            );
            if reduction > 1 {
                assert!(
                    points(rect, reduction / 2) > AREA_POINTS,
                    "{rect:?} at {reduction}"
                );
            }
        }
        let exact = area_rect(stage, [side(100.0), tall(100.0), side(64.0), tall(64.0)]).unwrap();
        assert_eq!(
            (exact.width, exact.height, area_reduction(exact, stage)),
            (64, 64, 1)
        );
        assert_eq!(area_reduction(whole, stage), 128);
        // A zero-size area reads the one pixel its corner falls in.
        assert_eq!(
            area_rect(stage, [0.5, 0.5, 0.0, 0.0]).unwrap(),
            Region {
                x0: 3000,
                y0: 2000,
                width: 1,
                height: 1
            }
        );
        // A rectangle half off the frame is clipped; one wholly off it is refused.
        assert_eq!(
            area_rect(stage, [-0.5, 0.0, 0.75, 0.5]).unwrap(),
            Region {
                x0: 0,
                y0: 0,
                width: 1500,
                height: 2000
            }
        );
        let refused = area_rect(stage, [1.0, 0.0, 0.5, 0.5]).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Validation);
        assert!(refused.detail.starts_with("outside the stage"), "{refused}");
        assert!(area_rect(stage, [-0.6, -0.6, 0.5, 0.5]).is_err());
    }

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
            _ => Some("never asked".to_owned()),
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
            .apply_exposure(&asset, mutation(reopened.revision, "pixel"), 0.9)
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
    #[slow]
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
                .apply_exposure(&asset, mutation(revision, "pixel"), 0.1)
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

    /// A pick on a mask no layer is bound to reads where a first adjustment through it would go and
    /// commits one entry with the values its pixels derive: a luminance click centres the band on the
    /// tone `mask.sample-input` answers at the clicked pixel, and a colour drag samples a point of the
    /// area `mask.sample-input-area` answers, loosening Refine only.
    #[test]
    fn a_pick_on_an_unbound_mask_is_one_entry_set_from_the_pixels_it_read() {
        let mut session = Session::open("mask-pick.sqlite");
        let created = session
            .run(
                "mask.create-luminance-range",
                &MaskTarget::default(),
                json!({"low": 0.0, "low_feather": 5.0, "high": 100.0, "high_feather": 5.0}),
                "create",
            )
            .unwrap();
        let mask = MaskId::parse(created["mask"].as_str().unwrap()).unwrap();
        let band = MaskTarget {
            mask: Some(mask.clone()),
            component: Some(
                crate::ComponentId::parse(created["component"].as_str().unwrap()).unwrap(),
            ),
            ..MaskTarget::default()
        };
        let state = session.service.state(&session.asset).unwrap();
        let (width, height) = (state.asset.width, state.asset.height);
        let entry = state.current_entry.id.clone();
        let (x, y) = (0.3, 0.6);
        let input = session
            .service
            .mask_input_sample(
                &session.asset,
                &entry,
                &mask,
                (x * f64::from(width)).floor() as u32,
                (y * f64::from(height)).floor() as u32,
            )
            .unwrap();
        let before = session.labels().len();
        session
            .run(
                "mask.pick-luminance-range",
                &band,
                json!({"x": x, "y": y}),
                "pick-band",
            )
            .unwrap();
        let labels = session.labels();
        assert_eq!(labels.len(), before + 1, "one entry");
        assert_eq!(
            labels.last().unwrap(),
            &json!(["mask.pick-luminance-range", "Pick Luminance range 1"])
        );
        let tone = 100.0
            * crate::colour::srgb::encode(crate::colour::luma::rec709_f64([
                input.r, input.g, input.b,
            ]));
        let listing = session.list();
        let payload = &listing["masks"][0]["components"][0]["payload"];
        assert_eq!(
            payload,
            &json!({"low": (tone - 10.0).clamp(0.0, 100.0), "low_feather": 10.0,
                    "high": (tone + 10.0).clamp(0.0, 100.0), "high_feather": 10.0})
        );
        assert_eq!(listing["masks"][0]["layers"], json!([]), "still unbound");

        let added = session
            .run(
                "mask.add-colour-range",
                &MaskTarget {
                    mask: Some(mask.clone()),
                    ..MaskTarget::default()
                },
                json!({"mode": "intersect", "refine": 50.0}),
                "add-colour",
            )
            .unwrap();
        let colour = MaskTarget {
            component: Some(
                crate::ComponentId::parse(added["component"].as_str().unwrap()).unwrap(),
            ),
            ..band.clone()
        };
        let rect = json!({"x": 0.2, "y": 0.2, "width": 0.4, "height": 0.3});
        let entry = session
            .service
            .state(&session.asset)
            .unwrap()
            .current_entry
            .id;
        let mut query = rect.clone();
        query["mask"] = json!(mask.as_str());
        let area = session
            .service
            .run_query(&session.asset, &entry, SAMPLE_INPUT_AREA, query)
            .unwrap();
        let area: AreaInput = serde_json::from_value(area).unwrap();
        assert!(!area.points.is_empty() && area.points.len() <= AREA_POINTS);
        assert_eq!(area.points.len(), (area.size[0] * area.size[1]) as usize);
        let before = session.labels().len();
        session
            .run("mask.pick-colour-range", &colour, rect, "pick-colour")
            .unwrap();
        let labels = session.labels();
        assert_eq!(labels.len(), before + 1, "one entry");
        assert_eq!(
            labels.last().unwrap(),
            &json!(["mask.pick-colour-range", "Pick Colour range 1"])
        );
        let payload = session.list()["masks"][0]["components"][1]["payload"].clone();
        let samples: Vec<[f64; 3]> = serde_json::from_value(payload["samples"].clone()).unwrap();
        assert_eq!(samples.len(), 1);
        assert!(
            area.points.contains(&samples[0]),
            "the medoid is a point of the area"
        );
        assert!(payload["refine"].as_f64().unwrap() <= 50.0, "never raised");
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
    #[slow]
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
