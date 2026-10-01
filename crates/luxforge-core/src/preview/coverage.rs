//! One mask's coverage over a whole evaluated stage on a small grid, and the identity of what that
//! grid depends on: the Masks panel's per-mask thumbnail.
//!
//! The grid is the one [`mask_overlay_for`] fills for the canvas's live overlay — the same compiled
//! mask, the same geometry tail and, for a mask that reads pixels, the same input of its first
//! bound layer — asked for over the whole output stage at the caller's cell count. It reads no pixel
//! of any rendered frame and allocates only the cells, so a thumbnail costs no render.
//!
//! **The key is what makes it cheap to keep.** A thumbnail is asked for every mask after every
//! settled frame, and almost every such frame leaves most masks exactly as they were. The key hashes
//! everything the grid is a function of — the source's identity, the geometry tail, the mask itself
//! and, only when the mask reads pixels, the stack before its first bound layer with the masks that
//! stack applies through — so a caller holding the key of the grid it already has learns in
//! `O(recipe)` and without filling a cell that nothing changed.
use super::PreviewSource;
use crate::{
    Cancel, Component, ComponentId, ComponentMode, Error, ErrorKind, Evaluation, Mask, MaskId,
    ModuleRegistry, Recipe, Region, Render, RenderContext,
    analysis::{MaskOverlay, MaskPixels},
    mask::CompiledMask,
    modules::Stage,
};
use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// One mask's coverage grid, or the reason it has none: what [`Evaluation::mask_overlay_coverage`]
/// and [`Evaluation::mask_coverage`] answer.
///
/// It reads no pixel of any rendered frame — only the geometry tail of the evaluation's exact
/// compilation, and for a mask that reads pixels, the input of its first bound layer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaskOverlayOutcome {
    /// The coverage grid of the mask asked for, over the evaluation's exact output stage or the
    /// rectangle of it that was asked for. `None` means the mask had nothing to describe or the
    /// grid was refused. It never means a mask whose coverage happens to be zero everywhere: that
    /// is a grid of zeros, and this is its absence.
    pub grid: Option<MaskOverlay>,
    /// Why the grid asked for is not in `grid`, in the host's own words.
    ///
    /// A client that asked for an overlay and waits for its texture has to be able to stop waiting:
    /// the grid is refused for reasons that belong to the mask rather than to the frame — a mask
    /// whose coverage depends on the pixel it reads has no grid at all
    /// ([proposal P16](../../../../docs/design/range-study.md#proposals)) — and an absence with no reason
    /// beside it is indistinguishable from a grid still on its way. `None` beside no grid means the
    /// mask had nothing to describe.
    pub absent: Option<String>,
}

/// One mask's coverage grid to fill: the mask, optionally one of its components alone, on a
/// `cells_w × cells_h` display grid.
///
/// **Why a component *identity* and not an index.** Hovering a row of the component list shows that
/// row's own contribution, so one component's grid has to be obtainable on its own. The host derives
/// a one-component mask and compiles it through the same [`CompiledMask`] the whole mask goes
/// through, so the row's overlay and the mask's overlay cannot disagree about that component's
/// field, and compiling one component is strictly cheaper than compiling all of them. It is a
/// [`ComponentId`] rather than a position because a position is not an identity: the component list
/// is reorderable, a hover and the grid that answers it are a request apart, and an index that
/// silently slid onto the neighbouring row would draw the wrong field with no way to tell. A
/// component the mask does not hold is refused by name instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MaskOverlayRequest {
    /// The mask to describe. It must be one the recipe holds.
    pub(super) mask: MaskId,
    /// One component of that mask, on its own, or `None` for the whole composed mask.
    pub(super) component: Option<ComponentId>,
    pub(super) cells_w: u32,
    pub(super) cells_h: u32,
}

/// The mask a component's own row describes: that one component alone.
///
/// Its mode is `add` because there is nothing before it to subtract from or intersect with, the
/// whole-mask amount and inversion are left out because they are the mask's modifiers and not the
/// row's, and the component's *own* inversion is kept because that is a control on the row. `None`
/// when the mask does not hold that component.
fn one_component(mask: &Mask, component: &ComponentId) -> Option<Mask> {
    let found = mask.components.iter().find(|held| &held.id == component)?;
    let alone = Component {
        mode: ComponentMode::Add,
        ..found.clone()
    };
    Some(Mask {
        id: mask.id.clone(),
        name: mask.name.clone(),
        amount: Mask::FULL_AMOUNT,
        invert: false,
        next_ordinal: BTreeMap::new(),
        components: vec![alone],
    })
}

/// One mask's coverage grid over the exact output stage of `frame`, the one compilation of `recipe`
/// at the exact stage, or over the `region` of that stage when one is given.
///
/// It reads no pixel of the exact frame: only the geometry tail of `frame`, and for a mask that
/// reads pixels, the input of its first bound layer.
///
/// Every reason there is no grid is a reason there is none to draw, never a silently empty one, and
/// the reason travels in [`MaskOverlayOutcome::absent`] — the host's own words, for a client that
/// asked for an overlay and would otherwise wait for a texture nothing will fill. Two absences carry
/// **no** reason on purpose: a mask with nothing to describe, which
/// [`crate::analysis::coverage_grid`] decides in closed form and which a grid of zeros would
/// misreport, and a cancel, which each caller turns into [`ErrorKind::Cancelled`] itself.
pub(super) fn mask_overlay_for(
    registry: &ModuleRegistry,
    frame: &Render<'_>,
    recipe: &Recipe,
    request: &MaskOverlayRequest,
    region: Option<Region>,
    cancel: &Cancel,
    context: &RenderContext,
) -> MaskOverlayOutcome {
    let refused = |error: Error| MaskOverlayOutcome {
        grid: None,
        absent: match error.kind {
            ErrorKind::Cancelled => None,
            _ => Some(error.detail),
        },
    };
    let absent = |reason: String| MaskOverlayOutcome {
        grid: None,
        absent: Some(reason),
    };
    let Some(held) = recipe.masks.iter().find(|mask| mask.id == request.mask) else {
        return absent(format!(
            "mask {} is not in the stack this frame was rendered from",
            request.mask
        ));
    };
    let derived;
    let mask = match &request.component {
        None => held,
        Some(component) => match one_component(held, component) {
            Some(one) => {
                derived = one;
                &derived
            }
            None => {
                return absent(format!("mask {} holds no component {component}", held.name));
            }
        },
    };
    // `O(layers)`: it composes the geometry tail of the stack already compiled for this frame and
    // reads no pixel.
    let transform = match frame.transform() {
        Ok(transform) => transform,
        Err(error) => return refused(error),
    };
    let stage = Stage {
        width: transform.content.width,
        height: transform.content.height,
    };
    let compiled = match CompiledMask::new(mask, stage, &recipe.strokes) {
        Ok(compiled) => compiled,
        Err(error) => return refused(error),
    };
    // A value-based component is answered on the pixel the masked operation receives, which is the
    // input of the mask's **first bound layer** — the rule `mask::commands::input_layer_index`
    // states once for everything that reads a pixel through a mask, and which the colour-constrained
    // brush's seed and `mask.sample-input` already read, so the overlay and the seed cannot disagree
    // about which pixel a mask reads. The prefix is compiled once and asked once per cell.
    let input;
    let unavailable;
    let pixels = if !compiled.reads_pixels() {
        // Position-only: no operation is needed and none is looked for, so a geometric grid costs
        // exactly what it did before a value-based component existed.
        MaskPixels::Unavailable("this mask reads no pixel")
    } else {
        match crate::mask::commands::input_layer_index(recipe, &request.mask).and_then(|layer| {
            crate::render::layer_input(registry, frame.source(), recipe, layer, context)
        }) {
            // Two different stages would be two different coverage fields, and `coverage_grid`
            // refuses that mismatch for the frame; it is refused here for the operation, in the same
            // voice, rather than read at coordinates of another stage.
            Ok((received, _)) if received != stage => {
                unavailable = format!(
                    "the masked operation receives a {}x{} stage and this mask is compiled against \
                     {}x{}",
                    received.width, received.height, stage.width, stage.height
                );
                MaskPixels::Unavailable(&unavailable)
            }
            Ok((_, prefix)) => {
                input = prefix;
                MaskPixels::Input(&*input)
            }
            // No layer is bound to this mask, or its prefix holds a spatial layer, or it does not
            // compile: in every case there is no operation whose input this grid can read, and the
            // refusal's own sentence says which and what to do about it.
            Err(error) => {
                unavailable = error.detail;
                MaskPixels::Unavailable(&unavailable)
            }
        }
    };
    let (cells_w, cells_h) = (request.cells_w, request.cells_h);
    let region = region.unwrap_or(Region {
        x0: 0,
        y0: 0,
        width: transform.output.width,
        height: transform.output.height,
    });
    let coverage = match crate::analysis::coverage_grid_region(
        &compiled, &transform, region, cells_w, cells_h, pixels, cancel,
    ) {
        Ok(Some(coverage)) => coverage,
        Ok(None) => return MaskOverlayOutcome::default(),
        Err(error) => return refused(error),
    };
    MaskOverlayOutcome {
        grid: Some(MaskOverlay {
            mask: request.mask.clone(),
            component: request.component.clone(),
            cells_w,
            cells_h,
            coverage,
        }),
        absent: None,
    }
}

/// One mask's coverage grid, or the key alone when the caller already holds the grid it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskCoverage {
    /// The identity of everything the grid is a function of. Equal keys are equal grids within
    /// one process; it is never persisted.
    pub key: u64,
    /// The grid, or the host's reason there is none, exactly as the overlay would carry it. `None`
    /// when the caller's key matched and nothing was filled.
    pub outcome: Option<MaskOverlayOutcome>,
}

/// The mask to inspect in an evaluation. A creation is resolved against this evaluation's own
/// effective draft recipe, never against an identity minted by another planning call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaskCoverageTarget {
    Existing {
        mask: MaskId,
        component: Option<ComponentId>,
    },
    DraftCreated,
}

/// Serialized and formatted values written straight into a hasher, so a key allocates nothing.
struct Sink<'a>(&'a mut DefaultHasher);

impl std::io::Write for Sink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl std::fmt::Write for Sink<'_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0.write(text.as_bytes());
        Ok(())
    }
}

/// Hash one serializable value by its canonical JSON, the form a recipe is hashed in for its
/// analysis identity.
fn hash_json(hasher: &mut DefaultHasher, value: &impl serde::Serialize) -> Result<(), Error> {
    serde_json::to_writer(Sink(hasher), value).map_err(|error| {
        Error::internal(format!(
            "a mask's coverage key could not be serialized: {error}"
        ))
    })?;
    // A separator, so two adjacent values cannot hash as one.
    0xffu8.hash(hasher);
    Ok(())
}

impl Evaluation {
    /// The mask and component `target` names in this exact evaluation. A create must add exactly
    /// one mask to its base entry. Missing, ambiguous or non-draft targets are explicit refusals.
    fn resolve_target<'a>(
        &'a self,
        target: &'a MaskCoverageTarget,
    ) -> Result<(&'a Mask, Option<&'a ComponentId>), Error> {
        let masks = &self.recipe().masks;
        match target {
            MaskCoverageTarget::Existing { mask, component } => {
                let held = masks.iter().find(|held| &held.id == mask).ok_or_else(|| {
                    Error::validation(format!(
                        "mask {mask} is not in the stack this evaluation holds"
                    ))
                })?;
                if let Some(component) = component
                    && !held.components.iter().any(|held| &held.id == component)
                {
                    return Err(Error::validation(format!(
                        "mask {} holds no component {component}",
                        held.name
                    )));
                }
                Ok((held, component.as_ref()))
            }
            MaskCoverageTarget::DraftCreated => {
                if self.draft().is_none() {
                    return Err(Error::validation(
                        "a candidate mask overlay needs an evaluated draft",
                    ));
                }
                let base = &self.entry().snapshot.recipe.masks;
                let mut created = masks
                    .iter()
                    .filter(|mask| !base.iter().any(|old| old.id == mask.id));
                let mask = created
                    .next()
                    .ok_or_else(|| Error::validation("the evaluated draft created no mask"))?;
                if created.next().is_some() {
                    return Err(Error::validation(
                        "the evaluated draft created more than one mask",
                    ));
                }
                Ok((mask, None))
            }
        }
    }

    /// The source's pixels, the recipe format, every layer and each mask `keep` selects, hashed.
    /// Reads no pixel and keeps no source alive; a stack that does not compile has no key.
    fn stack_key(&self, keep: impl Fn(&Mask) -> bool) -> Result<u64, Error> {
        self.compiled()?;
        let recipe = self.recipe();
        let mut hasher = DefaultHasher::new();
        std::fmt::write(
            &mut Sink(&mut hasher),
            format_args!("{:?}", self.source().identity()),
        )
        .map_err(|_| Error::internal("a source identity could not be formatted"))?;
        recipe.format.hash(&mut hasher);
        hash_json(&mut hasher, &recipe.layers)?;
        for mask in recipe.masks.iter().filter(|mask| keep(mask)) {
            hash_json(&mut hasher, mask)?;
        }
        Ok(hasher.finish())
    }

    /// A process-local identity of the photograph's pixels. Unbound masks and draft/history
    /// bookkeeping do not change pixels. Bound masks, source development and every layer do.
    pub fn pixel_content_key(&self) -> Result<u64, Error> {
        let layers = &self.recipe().layers;
        self.stack_key(|mask| {
            layers
                .iter()
                .any(|layer| layer.mask.as_ref() == Some(&mask.id))
        })
    }

    /// The exact dependencies that must stay fixed while accepted revisions of one edited mask
    /// provide progressive feedback. Only that target's values may differ: source development,
    /// all layers (including geometry and bindings), and every other mask remain in the key.
    /// The caller also fences the base entry, draft, target and display choices.
    pub fn mask_feedback_key(&self, target: &MaskCoverageTarget) -> Result<u64, Error> {
        let edited = &self.resolve_target(target)?.0.id;
        self.stack_key(|mask| &mask.id != edited)
    }

    /// Exact composed or component coverage, independently of photograph rasterization, over a
    /// whole stage or one visible rectangle. The grid and scratch are display-cell bounded.
    /// Value-based masks retain the first-bound-layer input and its explicit refusal rules.
    /// Unlike a thumbnail, a valid fully uncovered selection delivers a zero grid, so each
    /// presentation mode and each completion wait has an honest, settled result.
    pub fn mask_overlay_coverage(
        &self,
        target: &MaskCoverageTarget,
        cells: (u32, u32),
        region: Option<Region>,
        cached: Option<u64>,
        cancel: &Cancel,
    ) -> Result<MaskCoverage, Error> {
        cancel.check()?;
        let (held, component) = self.resolve_target(target)?;
        let (cells_w, cells_h) = cells;
        let limit = crate::analysis::MAX_OVERLAY_CELLS;
        if cells_w == 0 || cells_h == 0 {
            return Err(Error::validation(
                "a mask overlay needs a non-empty cell grid",
            ));
        }
        if cells_w > limit || cells_h > limit {
            return Err(Error::resource_limit(format!(
                "a mask overlay grid exceeds the {limit} cells a side the display overlay allows"
            )));
        }
        let request = MaskOverlayRequest {
            mask: held.id.clone(),
            component: component.cloned(),
            cells_w,
            cells_h,
        };
        let mut hasher = DefaultHasher::new();
        // Full recipe identity is intentionally conservative. No cached source/evaluation is kept;
        // one grid is cached and may be reused only under all its exact dependencies.
        self.identity()?.hash(&mut hasher);
        std::fmt::write(
            &mut Sink(&mut hasher),
            format_args!("{:?}", self.source().identity()),
        )
        .map_err(|_| Error::internal("a source identity could not be formatted"))?;
        request.component.hash(&mut hasher);
        request.mask.hash(&mut hasher);
        cells.hash(&mut hasher);
        region
            .map(|r| (r.x0, r.y0, r.width, r.height))
            .hash(&mut hasher);
        let key = hasher.finish();
        if cached == Some(key) {
            return Ok(MaskCoverage { key, outcome: None });
        }
        let frame = self.exact(cancel)?;
        if let Some(region) = region {
            let stage = frame.transform()?.output;
            if region.is_empty() || region.x1() > stage.width || region.y1() > stage.height {
                return Err(Error::validation(
                    "mask overlay rectangle is outside the output stage",
                ));
            }
        }
        let mut outcome = mask_overlay_for(
            self.registry(),
            &frame,
            self.recipe(),
            &request,
            region,
            cancel,
            self.context(),
        );
        cancel.check()?;
        if outcome.grid.is_none() && outcome.absent.is_none() {
            outcome.grid = Some(crate::analysis::MaskOverlay {
                mask: request.mask,
                component: request.component,
                cells_w,
                cells_h,
                coverage: vec![0; cells_w as usize * cells_h as usize],
            });
        }
        Ok(MaskCoverage {
            key,
            outcome: Some(outcome),
        })
    }

    /// `mask`'s composed coverage over this evaluation's whole output stage, on a `cells_w ×
    /// cells_h` grid, unless `cached` is already the key of that grid.
    ///
    /// The grid is the overlay's own ([`mask_overlay_for`]), so a thumbnail and the overlay cannot
    /// disagree about a cell. A mask that reads pixels is answered on the input of its first bound
    /// layer, and has no grid — with the host's reason in [`MaskOverlayOutcome::absent`] — when no
    /// layer is bound to it or the stack before that layer holds a spatial one.
    ///
    /// Cost: `O(recipe)` to key it, which composes the geometry tail and compiles the one mask but
    /// reads no pixel; on a miss, `O(cells × components)` plus one point query per cell through the
    /// prefix for a mask that reads pixels. A cancel ends it with [`crate::ErrorKind::Cancelled`],
    /// never with an absent grid a caller could mistake for an answer.
    pub fn mask_coverage(
        &self,
        mask: &MaskId,
        (cells_w, cells_h): (u32, u32),
        cached: Option<u64>,
        cancel: &Cancel,
    ) -> Result<MaskCoverage, Error> {
        let recipe = self.recipe();
        let held = recipe
            .masks
            .iter()
            .find(|held| &held.id == mask)
            .ok_or_else(|| {
                Error::validation(format!(
                    "mask {mask} is not in the stack this evaluation holds"
                ))
            })?;
        // Lazy: it borrows the one compilation and reads no pixel until asked for one.
        let frame = self.exact(cancel)?;
        let transform = frame.transform()?;
        let stage = Stage {
            width: transform.content.width,
            height: transform.content.height,
        };
        let compiled = CompiledMask::new(held, stage, &recipe.strokes)?;
        let mut hasher = DefaultHasher::new();
        std::fmt::write(
            &mut Sink(&mut hasher),
            format_args!("{:?}", self.source().identity()),
        )
        .map_err(|_| Error::internal("a source identity could not be formatted"))?;
        (
            transform.content.width,
            transform.content.height,
            transform.output.width,
            transform.output.height,
        )
            .hash(&mut hasher);
        transform.sha256().hash(&mut hasher);
        (cells_w, cells_h).hash(&mut hasher);
        hash_json(&mut hasher, held)?;
        // A position-only mask is a function of its own geometry and the stage alone. A mask that
        // reads pixels is also a function of the pixel its first bound layer receives: the stack
        // before that layer, the masks that stack applies through and the settings a RAW source is
        // evaluated under.
        compiled.reads_pixels().hash(&mut hasher);
        if compiled.reads_pixels() {
            match crate::mask::commands::input_layer_index(recipe, mask) {
                Ok(index) => {
                    index.hash(&mut hasher);
                    for layer in &recipe.layers[..index] {
                        hash_json(&mut hasher, layer)?;
                        if let Some(bound) = layer.mask.as_ref().and_then(|bound| {
                            recipe.masks.iter().find(|candidate| &candidate.id == bound)
                        }) {
                            hash_json(&mut hasher, bound)?;
                        }
                    }
                    if let PreviewSource::Raw { settings, .. } = self.source() {
                        std::fmt::write(&mut Sink(&mut hasher), format_args!("{settings:?}"))
                            .map_err(|_| Error::internal("RAW settings could not be formatted"))?;
                    }
                }
                Err(error) => error.detail.hash(&mut hasher),
            }
        }
        let key = hasher.finish();
        if cached == Some(key) {
            return Ok(MaskCoverage { key, outcome: None });
        }
        let request = MaskOverlayRequest {
            mask: mask.clone(),
            component: None,
            cells_w,
            cells_h,
        };
        let outcome = mask_overlay_for(
            self.registry(),
            &frame,
            recipe,
            &request,
            None,
            cancel,
            self.context(),
        );
        // The overlay reports a cancel as an absence with no reason, which is right for a frame a
        // newer one replaces and wrong for a grid a caller would keep: say it was cancelled.
        cancel.check()?;
        Ok(MaskCoverage {
            key,
            outcome: Some(outcome),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetId, BASIC_EFFECT, Component, ComponentMode, DraftId, DraftStamp, EFFECT_FORMAT,
        EntryId, HistoryEntry, Layer, LayerId, Mask, ModuleRegistry, Recipe, RenderContext,
        Snapshot, SnapshotId, SourceImage,
    };
    use serde_json::json;
    use std::sync::Arc;

    fn linear() -> Mask {
        let mut mask = Mask::new("Sky");
        mask.components.push(Component::new(
            "Linear 1",
            ComponentMode::Add,
            "linear",
            json!({"x0":0.5,"y0":0.1,"x1":0.5,"y1":0.9}),
        ));
        mask
    }

    fn evaluation(base: Recipe, recipe: Recipe, draft: Option<DraftStamp>) -> Evaluation {
        let asset = AssetId::new();
        let entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset.clone(),
            sequence: 0,
            action_id: "test".into(),
            label: "Test".into(),
            parameters: json!({}),
            actor: "test".into(),
            timestamp_ms: 0,
            request_id: None,
            base_revision: 0,
            result_revision: 0,
            snapshot: Snapshot {
                id: SnapshotId::new(),
                asset_id: asset,
                recipe: base,
            },
            undo_parent: None,
            restore_target: None,
        };
        Evaluation::new(
            Arc::new(ModuleRegistry::builtin()),
            RenderContext::new(),
            PreviewSource::Jpeg(SourceImage {
                width: 60,
                height: 40,
                rgba: vec![128; 60 * 40 * 4].into(),
                fingerprint: "sha256:mask-coverage-test".into(),
                orientation: 1,
                capture: Default::default(),
            }),
            entry,
            recipe,
            draft,
        )
    }

    fn changed(held: &Evaluation, recipe: Recipe) -> Evaluation {
        Evaluation::new(
            held.registry().clone(),
            held.context().clone(),
            held.source().clone(),
            held.entry().clone(),
            recipe,
            held.draft().cloned(),
        )
    }

    fn target(mask: &Mask) -> MaskCoverageTarget {
        MaskCoverageTarget::Existing {
            mask: mask.id.clone(),
            component: None,
        }
    }

    #[test]
    fn coverage_cache_key_is_the_mapping_hash() {
        let mask = linear();
        let recipe = Recipe {
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        let held = evaluation(recipe.clone(), recipe.clone(), None);
        let initial = held
            .mask_coverage(&mask.id, (12, 8), None, &Cancel::never())
            .unwrap();
        let perspective = Layer {
            id: LayerId::new(),
            effect_id: "luxforge.perspective".into(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({"horizontal":25,"vertical":0}),
            mask: None,
            artifacts: Vec::new(),
        };
        let mut warped = recipe;
        warped.layers.push(perspective);
        let moved = changed(&held, warped.clone())
            .mask_coverage(&mask.id, (12, 8), Some(initial.key), &Cancel::never())
            .unwrap();
        assert_ne!(moved.key, initial.key);
        assert!(moved.outcome.is_some());
        warped.layers.insert(
            0,
            Layer {
                id: LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload: json!({"exposure":1.0}),
                mask: None,
                artifacts: Vec::new(),
            },
        );
        let colour = changed(&held, warped)
            .mask_coverage(&mask.id, (12, 8), Some(moved.key), &Cancel::never())
            .unwrap();
        assert_eq!(colour.key, moved.key);
        assert!(colour.outcome.is_none());
    }

    #[test]
    fn mask_overlay_cache_misses_when_only_lens_changes() {
        let mask = linear();
        let recipe = Recipe {
            masks: vec![mask.clone()],
            layers: vec![crate::render::testing::frozen_lens(60, 40, 24.0)],
            ..Recipe::default()
        };
        let held = evaluation(recipe.clone(), recipe.clone(), None);
        let first = held
            .mask_coverage(&mask.id, (12, 8), None, &Cancel::never())
            .unwrap();
        let mut edited = recipe;
        let replacement = crate::render::testing::frozen_lens(60, 40, 35.0);
        edited.layers[0].payload = replacement.payload;
        let changed = changed(&held, edited);
        let next = changed
            .mask_coverage(&mask.id, (12, 8), Some(first.key), &Cancel::never())
            .unwrap();
        assert_ne!(next.key, first.key);
        assert!(
            next.outcome.is_some(),
            "the new map is evaluated even when a coarse grid quantises to the same bytes"
        );
        let hit = changed
            .mask_coverage(&mask.id, (12, 8), Some(next.key), &Cancel::never())
            .unwrap();
        assert!(hit.outcome.is_none());
        assert_eq!(changed.recipe().masks, held.recipe().masks);
    }

    #[test]
    fn a_candidate_grid_uses_the_mask_created_in_this_evaluation() {
        let base = Recipe::default();
        let mask = linear();
        let recipe = Recipe {
            masks: vec![mask.clone()],
            ..base.clone()
        };
        let draft = DraftStamp {
            draft_id: DraftId::new(),
            draft_revision: 3,
        };
        let held = evaluation(base, recipe, Some(draft));
        let candidate = held
            .mask_overlay_coverage(
                &MaskCoverageTarget::DraftCreated,
                (28, 19),
                None,
                None,
                &Cancel::never(),
            )
            .unwrap();
        let existing = held
            .mask_coverage(&mask.id, (28, 19), None, &Cancel::never())
            .unwrap();
        assert_eq!(candidate.outcome, existing.outcome);
        assert_eq!(candidate.outcome.unwrap().grid.unwrap().mask, mask.id);

        // Another plan of the same create mints another mask. Cached identities never escape
        // from one evaluation into the grid delivered for that other plan.
        let other = linear();
        let other = changed(
            &held,
            Recipe {
                masks: vec![other.clone()],
                ..Recipe::default()
            },
        );
        let answer = other
            .mask_overlay_coverage(
                &MaskCoverageTarget::DraftCreated,
                (28, 19),
                None,
                Some(candidate.key),
                &Cancel::never(),
            )
            .unwrap();
        assert_ne!(answer.key, candidate.key);
        assert_eq!(
            answer.outcome.unwrap().grid.unwrap().mask,
            other.recipe().masks[0].id
        );
    }

    #[test]
    fn candidate_target_refuses_missing_ambiguous_and_saved_creations() {
        let draft = Some(DraftStamp {
            draft_id: DraftId::new(),
            draft_revision: 1,
        });
        let empty = evaluation(Recipe::default(), Recipe::default(), draft.clone());
        assert!(
            empty
                .resolve_target(&MaskCoverageTarget::DraftCreated)
                .unwrap_err()
                .detail
                .contains("no mask")
        );
        let recipe = Recipe {
            masks: vec![linear(), linear()],
            ..Recipe::default()
        };
        let ambiguous = evaluation(Recipe::default(), recipe.clone(), draft);
        assert!(
            ambiguous
                .resolve_target(&MaskCoverageTarget::DraftCreated)
                .unwrap_err()
                .detail
                .contains("more than one")
        );
        let saved = evaluation(recipe.clone(), recipe, None);
        assert!(
            saved
                .resolve_target(&MaskCoverageTarget::DraftCreated)
                .unwrap_err()
                .detail
                .contains("evaluated draft")
        );
    }

    #[test]
    fn exact_overlay_reports_zero_component_region_and_bounded_refusals() {
        let mut mask = linear();
        mask.amount = 0.0;
        let recipe = Recipe {
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        let held = evaluation(recipe.clone(), recipe, None);
        let zero = held
            .mask_overlay_coverage(&target(&mask), (28, 19), None, None, &Cancel::never())
            .unwrap();
        let grid = zero
            .outcome
            .unwrap()
            .grid
            .expect("zero coverage is a settled grid");
        assert_eq!(grid.coverage, vec![0; 28 * 19]);
        let component = MaskCoverageTarget::Existing {
            mask: mask.id.clone(),
            component: Some(mask.components[0].id.clone()),
        };
        let region = Some(Region {
            x0: 10,
            y0: 5,
            width: 20,
            height: 15,
        });
        let first = held
            .mask_overlay_coverage(&component, (12, 8), region, None, &Cancel::never())
            .unwrap();
        let grid = first.outcome.unwrap().grid.unwrap();
        assert_eq!(grid.component, Some(mask.components[0].id.clone()));
        assert!(
            grid.coverage.iter().any(|cell| *cell > 0),
            "component excludes whole-mask amount"
        );
        let cached = held
            .mask_overlay_coverage(
                &component,
                (12, 8),
                region,
                Some(first.key),
                &Cancel::never(),
            )
            .unwrap();
        assert_eq!(cached.outcome, None);
        let moved = held
            .mask_overlay_coverage(&component, (12, 8), None, Some(first.key), &Cancel::never())
            .unwrap();
        assert_ne!(moved.key, first.key);
        assert!(
            held.mask_overlay_coverage(&component, (0, 8), None, None, &Cancel::never())
                .is_err()
        );
        assert_eq!(
            held.mask_overlay_coverage(
                &component,
                (crate::analysis::MAX_OVERLAY_CELLS + 1, 8),
                None,
                None,
                &Cancel::never()
            )
            .unwrap_err()
            .kind,
            crate::ErrorKind::ResourceLimit
        );
        assert!(
            held.mask_overlay_coverage(
                &component,
                (12, 8),
                Some(Region {
                    x0: 59,
                    y0: 0,
                    width: 2,
                    height: 1
                }),
                None,
                &Cancel::never()
            )
            .is_err()
        );
        let missing = MaskCoverageTarget::Existing {
            mask: mask.id.clone(),
            component: Some(ComponentId::new()),
        };
        assert!(
            held.mask_overlay_coverage(&missing, (12, 8), None, None, &Cancel::never())
                .is_err()
        );
        let cancel = Cancel::new();
        cancel.cancel();
        assert_eq!(
            held.mask_overlay_coverage(&component, (12, 8), region, Some(first.key), &cancel)
                .unwrap_err()
                .kind,
            crate::ErrorKind::Cancelled
        );
    }

    #[test]
    fn value_based_overlay_preserves_the_exact_input_and_unbound_refusal() {
        let mut mask = Mask::new("Shadows");
        mask.components.push(Component::new(
            "Range 1",
            ComponentMode::Add,
            "luminance-range",
            json!({"low":20.0,"low_feather":10.0,"high":80.0,"high_feather":10.0}),
        ));
        let recipe = Recipe {
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        let held = evaluation(recipe.clone(), recipe.clone(), None);
        let absent = held
            .mask_overlay_coverage(&target(&mask), (28, 19), None, None, &Cancel::never())
            .unwrap()
            .outcome
            .unwrap();
        assert!(absent.grid.is_none() && absent.absent.is_some());
        let mut recipe = recipe;
        recipe.layers.push(Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure":1.0}),
            mask: Some(mask.id.clone()),
            artifacts: Vec::new(),
        });
        let held = changed(&held, recipe);
        let overlay = held
            .mask_overlay_coverage(&target(&mask), (28, 19), None, None, &Cancel::never())
            .unwrap();
        let thumbnail = held
            .mask_coverage(&mask.id, (28, 19), None, &Cancel::never())
            .unwrap();
        assert_eq!(overlay.outcome, thumbnail.outcome);
        let mut spatial = held.recipe().clone();
        spatial.layers.insert(
            0,
            Layer {
                id: LayerId::new(),
                effect_id: crate::PRESENCE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"clarity":50.0}),
                mask: None,
                artifacts: Vec::new(),
            },
        );
        let refused = changed(&held, spatial)
            .mask_overlay_coverage(&target(&mask), (28, 19), None, None, &Cancel::never())
            .unwrap()
            .outcome
            .unwrap();
        assert!(refused.grid.is_none());
        assert!(
            refused.absent.unwrap().contains("spatial"),
            "value-based coverage never approximates a spatial prefix"
        );
    }

    #[test]
    fn progressive_feedback_fences_every_dependency_except_the_edited_target() {
        let mask = linear();
        let mut other = linear();
        other.name = "Other".into();
        let recipe = Recipe {
            masks: vec![mask.clone(), other],
            ..Recipe::default()
        };
        let held = evaluation(recipe.clone(), recipe.clone(), None);
        let initial = held.mask_feedback_key(&target(&mask)).unwrap();
        let mut edited = recipe.clone();
        edited.masks[0].amount = 25.0;
        edited.masks[0].components[0].payload = json!({"x0":0.1,"y0":0.2,"x1":0.8,"y1":0.9});
        assert_eq!(
            changed(&held, edited.clone())
                .mask_feedback_key(&target(&mask))
                .unwrap(),
            initial
        );
        edited.masks[1].amount = 50.0;
        assert_ne!(
            changed(&held, edited)
                .mask_feedback_key(&target(&mask))
                .unwrap(),
            initial
        );
        let mut layered = recipe.clone();
        layered.layers.push(Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure":1.0}),
            mask: Some(mask.id.clone()),
            artifacts: Vec::new(),
        });
        let bound = changed(&held, layered.clone())
            .mask_feedback_key(&target(&mask))
            .unwrap();
        assert_ne!(bound, initial);
        layered.layers[0].payload = json!({"exposure":2.0});
        assert_ne!(
            changed(&held, layered)
                .mask_feedback_key(&target(&mask))
                .unwrap(),
            bound
        );
        let mut source = held.source().clone();
        let PreviewSource::Jpeg(ref mut image) = source else {
            unreachable!()
        };
        image.fingerprint = "a different source".into();
        let other_source = Evaluation::new(
            held.registry().clone(),
            held.context().clone(),
            source,
            held.entry().clone(),
            recipe,
            None,
        );
        assert_ne!(
            other_source.mask_feedback_key(&target(&mask)).unwrap(),
            initial
        );
        assert!(held.mask_feedback_key(&target(&linear())).is_err());
    }

    #[test]
    fn photograph_content_ignores_unbound_masks_and_history_but_tracks_bound_edits() {
        let held = evaluation(Recipe::default(), Recipe::default(), None);
        let initial = held.pixel_content_key().unwrap();
        let mask = linear();
        let recipe = Recipe {
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        let unbound = changed(&held, recipe.clone());
        assert_eq!(unbound.pixel_content_key().unwrap(), initial);
        let mut moved = recipe.clone();
        moved.masks[0].components[0].payload = json!({"x0":0.1,"y0":0.1,"x1":0.9,"y1":0.9});
        assert_eq!(
            changed(&held, moved.clone()).pixel_content_key().unwrap(),
            initial
        );
        let mut saved_entry = unbound.entry().clone();
        saved_entry.id = EntryId::new();
        saved_entry.snapshot.id = SnapshotId::new();
        saved_entry.snapshot.recipe = recipe.clone();
        let saved = Evaluation::new(
            held.registry().clone(),
            held.context().clone(),
            held.source().clone(),
            saved_entry,
            recipe.clone(),
            None,
        );
        assert_eq!(saved.pixel_content_key().unwrap(), initial);
        let layer = Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure":1.0}),
            mask: Some(mask.id.clone()),
            artifacts: Vec::new(),
        };
        let mut bound = recipe;
        bound.layers.push(layer.clone());
        let key = changed(&held, bound.clone()).pixel_content_key().unwrap();
        assert_ne!(key, initial);
        moved.layers.push(layer);
        assert_ne!(changed(&held, moved).pixel_content_key().unwrap(), key);
        bound.layers[0].payload = json!({"exposure":2.0});
        assert_ne!(changed(&held, bound).pixel_content_key().unwrap(), key);
        let mut source = held.source().clone();
        let PreviewSource::Jpeg(ref mut image) = source else {
            unreachable!()
        };
        image.fingerprint = "another source".into();
        let changed_source = Evaluation::new(
            held.registry().clone(),
            held.context().clone(),
            source,
            held.entry().clone(),
            Recipe::default(),
            None,
        );
        assert_ne!(changed_source.pixel_content_key().unwrap(), initial);
    }
}
