//! A draft's GPU preview, planned with its preview job on the catalog owner
//! (`docs/design/gpu-preview.md`, "A tick"): the plan the photo surface draws a tick from, and
//! the boundary it starts from, which the preview worker renders once per draft.
//!
//! Planning reads the job's own evaluation — the draft's effective recipe beside the entry it was
//! planned over, and the stack's one compilation — and compiles at the stage the job's proxy phase
//! draws at: `O(layers)`, no pixel read, nothing that scales with the image.
//!
//! - **From the source.** Every plan starts from the source itself, the first segment's input
//!   before its first operation, so a drag and the stack's picture at rest ([`plan_rest`]) share
//!   one boundary over one source and view, every stack has a plan, the empty one included, and
//!   the geometry is the plan's tail. Only a draft whose earliest change — the first layer that
//!   differs from the entry's own stack or reads a mask the draft changes, the layer a module
//!   action drafts, or for a `mask.*` gesture the first layer the drafted mask modulates — is a
//!   source layer, or a geometry layer before every content layer, starts from that layer's input,
//!   which names `boundary-stage`.
//! - **A stable shape.** A field-patch module compiles only its non-neutral units, so the drafted
//!   layer is planned in its GPU shape, every unit present and a neutral one as its identity
//!   ([`super::GpuPlanRequest::drafted`]); a drafted layer the stack does not hold yet, because
//!   its value is still neutral, is planned as the neutral layer its first commit would insert.
//!   A drag that leaves or returns to neutral therefore keeps one program sequence. The CPU
//!   compile is untouched: only this plan asks for the shape.
//! - **Stored estimates.** A spatial operation's global estimate, Dehaze's atmospheric light, is
//!   read from the estimate store the job's CPU frames fill, under the key a CPU frame of the same
//!   content at the same stage asks with: the proxy is named by its source and plan, never built. A plan whose key
//!   the store does not hold yet takes the estimate on the GPU and says it is approximate.
//! - **The boundary key** names everything the boundary's texels depend on: the source's identity
//!   (fingerprint, development and view), the layers before the boundary and the masks they read,
//!   the boundary's index, and the proxy plan with its bounds and window, or at the exact stage at
//!   Fit the window the output reads, or at a percentage zoom of 100% or more the region of the
//!   output stage the boundary is held for. Equal keys hold equal texels.
//! - **Where it is drawn** ([`GpuView`]): at Fit, and at a percentage zoom below 100%, the stage
//!   the job's proxy phase draws at its display bounds: Fit's, or below 100% the displayed size of
//!   the whole stage, the same proxy the CPU path draws there; at a percentage zoom of 100% or
//!   more, the exact stage, over the visible region at full scale, whose boundary is the window of
//!   the boundary layer's received stage that region reads.
//! - **What it holds.** Only the part of the boundary layer's received stage the drawn output
//!   reads: a windowed proxy's window at a proxy; at the exact stage at Fit, which a photograph
//!   that fits the display bounds is drawn at, the window the whole output stage reads through the
//!   windowed planner ([`crate::render::window::WindowPlan::of_rect`]) — what a crop, a
//!   straightening and a warp read, with their resample's taps and margin, clamped to the stage;
//!   at a percentage zoom of 100% or more, the window the visible region reads.
use super::{
    EstimateSource, GpuAnswer, GpuEstimates, GpuFallback, GpuPlan, GpuPlanRequest, gpu_plan_with,
    plan::{HeldPrefix, gpu_plan_holding},
};
use crate::{
    Draft, EFFECT_FORMAT, EffectStage, Error, Evaluation, Layer, LayerId, MaskId, ModuleRegistry,
    ProxyBounds, ProxyIdentity, ProxyPlan, Recipe,
    mask_field::MaskSampling,
    modules::{MAX_MASKED_SPATIAL_LAYERS, Region, Stage},
    render::{Compiled, spatial::prefix_hash, window::WindowPlan},
};
use serde_json::json;

/// What identifies a held boundary's texels: two equal keys hold equal texels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryKey {
    /// The source the boundary is rendered from: its fingerprint, and for a RAW the development
    /// and the view over its planes.
    source: ProxyIdentity,
    /// The hash of the layers before the boundary and of the masks they read.
    prefix: String,
    /// The boundary layer's index in the drafted stack.
    layer: usize,
    /// The proxy the boundary is rendered at, with the bounds it was fitted to and the window it
    /// holds; `None` at the exact stage, where a photograph that fits the display is drawn, and at
    /// a percentage zoom of 100% or more.
    plan: Option<ProxyPlan>,
    /// At a percentage zoom of 100% or more, the rectangle of the output stage the boundary is
    /// held for.
    region: Option<Region>,
    /// The rectangle of the boundary stage whose texels it holds, including spatial support
    /// margins. The output region alone does not identify a spatial input window.
    window: Option<Region>,
}

impl BoundaryKey {
    /// The source the boundary is derived from: its fingerprint, and for a RAW the development and
    /// the view over its planes.
    pub fn source(&self) -> &ProxyIdentity {
        &self.source
    }

    /// The proxy the boundary is rendered at, with its window; `None` at the exact stage.
    pub fn plan(&self) -> Option<ProxyPlan> {
        self.plan
    }

    /// The boundary layer's index in the drafted stack.
    pub fn layer(&self) -> usize {
        self.layer
    }

    /// At a percentage zoom of 100% or more, the rectangle of the output stage the boundary is held
    /// for, which the plan's frame covers; `None` for a whole frame, at Fit and below 100%.
    pub fn region(&self) -> Option<Region> {
        self.region
    }
}

/// Where a draft's GPU preview is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GpuView {
    /// A whole frame in these display bounds: the stage the job's proxy phase draws at. At Fit the
    /// bounds are the photo area's; at a percentage zoom below 100% they are the displayed size of
    /// the whole stage, so the plan addresses the proxy the CPU path draws at that zoom, and differs
    /// from Fit's only in its size.
    Fit(ProxyBounds),
    /// At a percentage zoom of 100% or more: `rect` of the output stage at full scale, the visible
    /// region, drawn at `magnification` physical pixels an output pixel.
    Region { rect: Region, magnification: f64 },
}

/// The boundary a GPU plan starts from: the source itself at the plan's stage, which the photo
/// surface derives from the prepared source it holds (`docs/design/gpu-preview.md`, "The GPU
/// source") — reduced to the proxy plan the key names at Fit and below 100%, and a window of it cut
/// at full scale at the exact stage, at Fit or over a region at 100% or more. Nothing renders it
/// on the CPU.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceBoundary {
    pub key: BoundaryKey,
    /// A lens warp's geometry tail, whose coordinate grid is computed off the interface thread
    /// once per key ([`Self::grid`]); `None` for an affine or projective tail, which the surface
    /// evaluates exactly.
    pub(crate) warp: Option<super::GpuGeometry>,
    /// How the boundary's texels are held: `f32` for a plan of the linear path, half floats
    /// otherwise.
    pub format: crate::BoundaryFormat,
    /// Physical pixels an output pixel is drawn at, which a warp's coordinate grid is made dense
    /// enough for: one for a whole frame, whose proxy is drawn at about its own size, the zoom
    /// over a region at 100% or more.
    pub(crate) magnification: f64,
    /// The window of the content stage the boundary holds, so what the boundary and a slot drawing
    /// over it take is known before it is derived: at a percentage zoom of 100% or more, the one
    /// the region reads, the region and every margin after it; at the exact stage at Fit, the one
    /// the whole output stage reads, when that is less than all of it. `None` at a proxy, at Fit or
    /// below 100%, whose plan names its window ([`ProxyPlan::held`]), and for a boundary of the
    /// whole exact stage.
    pub window: Option<Region>,
}

impl SourceBoundary {
    /// Whether the plan's tail is a lens warp, whose coordinate grid the surface reads
    /// ([`Self::grid`]).
    pub fn needs_grid(&self) -> bool {
        self.warp.is_some()
    }

    /// A lens warp's geometry tail and the magnification its coordinate grid is made dense enough
    /// for: what the whole output stage's grid, which every window of it takes its part of, is
    /// computed from ([`super::GpuGeometry::stage_grid`]). `None` for an affine or projective tail.
    pub fn warp(&self) -> Option<(&super::GpuGeometry, f64)> {
        self.warp.as_ref().map(|warp| (warp, self.magnification))
    }

    /// What of the output stage a lens warp's grid covers: the region at 100% or more, the whole
    /// stage at Fit and below 100%. `None` for a plan with no lens warp.
    pub fn grid_region(&self) -> Option<Region> {
        let output = self.warp.as_ref()?.output();
        Some(self.key.region().unwrap_or(Region {
            x0: 0,
            y0: 0,
            width: output.width,
            height: output.height,
        }))
    }

    /// A lens warp's coordinate grid over what the plan draws ([`Self::grid_region`]) at the plan's
    /// magnification, the part of the whole stage's grid there ([`super::GpuGeometry::grid`]), or
    /// why it cannot be built; `None` for an affine or projective tail. Frame work of up to
    /// [`super::GRID_MAX_NODES`] nodes: a caller runs it off the interface thread and the catalog
    /// owner.
    pub fn grid(&self) -> Option<Result<std::sync::Arc<super::CoordinateGrid>, Error>> {
        let warp = self.warp.as_ref()?;
        let region = self.grid_region()?;
        Some(warp.grid(region, self.magnification).and_then(|grid| {
            grid.map(std::sync::Arc::new)
                .ok_or_else(|| Error::internal("a warp tail with no grid"))
        }))
    }
}

/// A draft's GPU preview: the plan a tick is drawn from, or why the gesture takes the CPU path,
/// and the boundary the plan starts from.
#[derive(Clone, Debug)]
pub struct GpuPreview {
    pub answer: GpuAnswer,
    /// The boundary of [`Self::answer`]'s plan; `None` when there is no plan.
    pub boundary: Option<SourceBoundary>,
    /// Over a region at 100% or more, when [`Self::answer`]'s plan holds a drafted restoration or
    /// spatial layer in its GPU shape: the plan of that layer in the CPU's shape, the units its
    /// values need, from the same boundary. Over the region's window the GPU shape charges the
    /// planes of its units at zero too, so the desktop draws this one when only it fits the budget.
    pub cpu_shape: Option<Box<GpuPlan>>,
    /// The label the recipe list gives the layer [`Self::answer`]'s fallback names, when it names
    /// one ([`ModuleRegistry::layer_label`]). The reason's index is into the stack the plan was
    /// made from, which holds the neutral layer a drafted layer's first commit would add, so the
    /// label travels with the answer rather than being read back from the committed stack's rows.
    pub layer: Option<String>,
}

/// Whether a layer at `stage` has a GPU shape (`CompileStage::gpu_shape`): a colour or finish
/// layer's every unit, or a restoration or spatial layer's.
fn shaped(stage: EffectStage) -> bool {
    matches!(
        stage,
        EffectStage::Color | EffectStage::Finish | EffectStage::Restoration | EffectStage::Spatial
    )
}

/// The layer a module action drafts, and whether the stack holds it yet.
struct Drafted {
    index: usize,
    held: bool,
    effect: String,
    mask: Option<MaskId>,
}

/// The first layer of `recipe` a GPU plan can start from: past its source layers, whose
/// development the boundary already holds, and past any geometry layer before the first layer of
/// another stage, which only a stack holding nothing but geometry has; the stack's length when
/// there is none. Every gesture over the layers from it on is planned from it ([`plan_preview`]).
fn first_content_layer(registry: &ModuleRegistry, recipe: &Recipe) -> usize {
    recipe
        .layers
        .iter()
        .position(|layer| {
            !matches!(
                registry.effect_stage(&layer.effect_id),
                Some(EffectStage::Source | EffectStage::Geometry)
            )
        })
        .unwrap_or(recipe.layers.len())
}

/// The layer `draft`'s action drafts in `recipe`: the one layer of its field-patch module's colour,
/// finish, restoration or spatial effect for the draft's mask target, or where that layer's first
/// commit would join the stack. `None` for any other action, whose changes the stack comparison
/// finds.
fn drafted(registry: &ModuleRegistry, recipe: &Recipe, draft: &Draft) -> Option<Drafted> {
    let (provider, action) = registry.action(&draft.action)?;
    if !action.patch {
        return None;
    }
    let [effect] = provider.descriptor().effects.as_slice() else {
        return None;
    };
    if !shaped(effect.stage) {
        return None;
    }
    let mask = match draft.target.get("mask") {
        Some(mask) => Some(MaskId::parse(mask.clone()).ok()?),
        None => None,
    };
    let held = recipe
        .layers
        .iter()
        .position(|layer| layer.effect_id == effect.id && layer.mask == mask);
    Some(Drafted {
        index: held.unwrap_or_else(|| {
            registry.insertion_index_for_target(
                &recipe.layers,
                &effect.id,
                mask.as_ref(),
                &recipe.masks,
            )
        }),
        held: held.is_some(),
        effect: effect.id.clone(),
        mask,
    })
}

/// The first layer of `drafted` that differs from `entry`'s, or that reads a mask whose definition
/// differs between them.
fn first_change(entry: &Recipe, drafted: &Recipe) -> Option<usize> {
    let mask_changed = |id: &MaskId| {
        let find = |recipe: &Recipe| recipe.masks.iter().find(|mask| &mask.id == id).cloned();
        find(entry) != find(drafted)
    };
    drafted
        .layers
        .iter()
        .enumerate()
        .find_map(|(index, layer)| {
            let differs = entry.layers.get(index) != Some(layer)
                || layer.mask.as_ref().is_some_and(mask_changed);
            differs.then_some(index)
        })
}

/// Where layer `layer` of a stack compiled as `compiled` begins: its own position, or the end of
/// the last segment for a layer one past the stack.
pub(crate) fn position(compiled: &Compiled, layer: usize) -> Option<(usize, usize)> {
    match compiled.layers.get(layer) {
        Some(position) => Some(*position),
        None if layer == compiled.layers.len() => {
            let last = compiled.segments.len() - 1;
            Some((last, compiled.segments[last].operations.len()))
        }
        None => None,
    }
}

/// The stage a job's whole frame is drawn at, at Fit or at a percentage zoom below 100%, planned
/// exactly as the preview worker plans the job's proxy phase at the job's bounds: from the output
/// stage of the stack's one compilation, with the window of the proxy stage a crop reads. A stack
/// that fits the bounds, or has no proxy, is drawn at its exact stage.
struct FitStage {
    /// The proxy plan, with its window; `None` at the exact stage.
    plan: Option<ProxyPlan>,
    /// The stack compiled at that stage, uncut.
    compiled: Compiled,
    /// The stage the plan addresses, and the source's full content stage.
    stage: Stage,
    full: Stage,
    /// The source is a developed RAW, whose frames take the linear path.
    linear: bool,
    /// At a percentage zoom of 100% or more, the region of the output stage drawn, at this
    /// magnification.
    region: Option<(Region, f64)>,
}

impl FitStage {
    /// The stage `view` is drawn at: the proxy phase's at the view's bounds for [`GpuView::Fit`],
    /// the exact stage over the visible region for [`GpuView::Region`].
    fn of_view(evaluation: &Evaluation, view: GpuView) -> Result<Self, Error> {
        match view {
            GpuView::Fit(bounds) => Self::of(evaluation, bounds),
            GpuView::Region {
                rect,
                magnification,
            } => {
                let full = evaluation.source().dimensions();
                let full = Stage {
                    width: full.0,
                    height: full.1,
                };
                Ok(Self {
                    plan: None,
                    compiled: evaluation.compiled()?.clone(),
                    stage: full,
                    full,
                    linear: matches!(evaluation.source(), crate::PreviewSource::Raw { .. }),
                    region: Some((rect, magnification)),
                })
            }
        }
    }

    fn of(evaluation: &Evaluation, bounds: ProxyBounds) -> Result<Self, Error> {
        let registry = evaluation.registry();
        let recipe = evaluation.recipe();
        let exact = evaluation.exact(&crate::Cancel::never())?;
        let full = evaluation.source().dimensions();
        let full = Stage {
            width: full.0,
            height: full.1,
        };
        let proxy = registry
            .proxy_eligible(recipe)
            .ok()
            .and_then(|()| exact.proxy_plan(bounds))
            .map(|plan| exact.proxy_window(registry, recipe, plan));
        // A developed RAW's frames are the linear path's, unclamped and unquantized between
        // segments, where a JPEG's are clamped and quantized at every stage boundary.
        let linear = matches!(evaluation.source(), crate::PreviewSource::Raw { .. });
        Ok(match proxy {
            Some(stage) => {
                let plan = stage.plan();
                Self {
                    plan: Some(plan),
                    compiled: stage.compiled()?.clone(),
                    stage: Stage {
                        width: plan.width,
                        height: plan.height,
                    },
                    full,
                    linear,
                    region: None,
                }
            }
            None => Self {
                plan: None,
                compiled: evaluation.compiled()?.clone(),
                stage: full,
                full,
                linear,
                region: None,
            },
        })
    }

    /// A plan request from `boundary` at this stage, on the source's path: from layer `boundary`'s
    /// input, or from the source itself for `None` ([`GpuPlanRequest::from_source`]).
    fn request(&self, boundary: Option<usize>) -> GpuPlanRequest {
        let layer = boundary.unwrap_or(0);
        let request = match self.plan {
            Some(_) => GpuPlanRequest::fit(layer, self.stage, self.full),
            None => GpuPlanRequest::exact(layer, self.full),
        };
        let request = match boundary {
            Some(_) => request,
            None => request.from_source(),
        };
        if self.linear {
            request.linear()
        } else {
            request
        }
    }

    /// Where `evaluation`'s CPU frames at this stage keep their global estimates: its context's
    /// store, under its proxy at this plan, or under its source at the exact stage.
    ///
    /// A windowed proxy reduces no stage of its own: its spatial operations are handed the exact
    /// stage's estimates (`Render::render_proxy`), so its store is read under the exact stage's
    /// key, the source's own stage being where a spatial layer, which comes before any geometry,
    /// reads. A layer whose prefix holds a mask hashes it under the proxy's sampling, so such a
    /// lookup misses and the GPU takes the estimate from the window, approximate.
    fn estimates<'a>(&self, evaluation: &'a Evaluation) -> GpuEstimates<'a> {
        GpuEstimates {
            context: evaluation.context(),
            source: match self.plan {
                Some(plan) if plan.window.is_some() => EstimateSource::Whole {
                    source: evaluation.source().into(),
                    stage: self.full,
                },
                Some(plan) => EstimateSource::Proxy {
                    source: evaluation.source(),
                    plan,
                },
                None => EstimateSource::Render(evaluation.source().into()),
            },
        }
    }

    /// The masks a proxy compiles take the thin-feature supersample; the exact stage's do not.
    fn sampling(&self) -> MaskSampling {
        if self.plan.is_some() {
            MaskSampling::ThinFeature
        } else {
            MaskSampling::Point
        }
    }
}

/// `recipe` with a neutral layer of `effect` for `mask` inserted at `index`: what a drafted layer
/// the stack does not hold yet is planned as, the neutral layer its first commit would insert.
fn with_neutral(recipe: &Recipe, index: usize, effect: &str, mask: Option<MaskId>) -> Recipe {
    let mut planned = recipe.clone();
    planned.layers.insert(
        index,
        Layer {
            id: LayerId::new(),
            effect_id: effect.to_owned(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            mask,
            artifacts: Vec::new(),
        },
    );
    planned
}

/// For each spatial layer of `planned`, the drafted stack, whose input differs from the one it had
/// in `entry`'s stack only through restoration layers: the prefix its global estimates were stored
/// under in `entry` ([`HeldPrefix`]). A drag of Detail changes Dehaze's input only by its filters,
/// which barely move the block means the atmospheric light is chosen from, so it reads the light
/// `entry`'s settled frame stored, held for the drag (`docs/specs/performance.md`, "Dehaze behind
/// Detail at 100%"). A colour layer before a spatial one moves those means by its tone, as three
/// stops of exposure does far past the spatial limits, so a drag that changes one holds none.
/// `O(layers)`, hashing each such layer's prefix once in each stack.
fn held_prefixes(
    registry: &ModuleRegistry,
    entry: &crate::HistoryEntry,
    planned: &Recipe,
    sampling: MaskSampling,
) -> Result<Vec<HeldPrefix>, Error> {
    let committed = &entry.snapshot.recipe;
    let stage_of = |layer: &Layer| registry.effect_stage(&layer.effect_id);
    let mask_of = |recipe: &Recipe, layer: &Layer| {
        layer
            .mask
            .as_ref()
            .and_then(|id| recipe.masks.iter().find(|mask| &mask.id == id).cloned())
    };
    let mut held = Vec::new();
    for (layer, drafted) in planned.layers.iter().enumerate() {
        if stage_of(drafted) != Some(EffectStage::Spatial) {
            continue;
        }
        let Some(at) = committed
            .layers
            .iter()
            .position(|stored| stored.id == drafted.id)
        else {
            continue;
        };
        let (before, was) = (&planned.layers[..layer], &committed.layers[..at]);
        // Every layer before it the draft adds, removes or changes, its mask included, is a
        // restoration layer.
        let changed = |layer: &Layer, ours: &Recipe, other: (&[Layer], &Recipe)| {
            other
                .0
                .iter()
                .find(|stored| stored.id == layer.id)
                .is_none_or(|stored| {
                    stored != layer || mask_of(other.1, stored) != mask_of(ours, layer)
                })
        };
        let restoration_only = before
            .iter()
            .filter(|layer| changed(layer, planned, (was, committed)))
            .chain(
                was.iter()
                    .filter(|layer| changed(layer, committed, (before, planned))),
            )
            .all(|layer| stage_of(layer) == Some(EffectStage::Restoration));
        if !restoration_only {
            continue;
        }
        let prefix = prefix_hash(was, &committed.masks, sampling)?;
        if prefix != prefix_hash(before, &planned.masks, sampling)? {
            held.push(HeldPrefix {
                layer,
                prefix_hash: prefix,
            });
        }
    }
    Ok(held)
}

/// The window of the stage segment `segment` of `compiled`, a stack over a `full` source, receives
/// that the whole output stage reads, planned as the boundary's render plans it
/// (`Render::output_boundary`, [`WindowPlan::of_gpu_rect`]): `None` when the segment reads all of
/// it or the planner cannot cut the stack. A spatial operation before the segment whose estimate
/// lies behind an earlier one reads it from the store alone, which the caller checks
/// ([`Compiled::unheld_estimate`]). `O(segments)`, no pixel read.
pub(crate) fn output_window(compiled: &Compiled, full: Stage, segment: usize) -> Option<Region> {
    let full = (full.width, full.height);
    let output = Region::whole(compiled.stage());
    WindowPlan::of_gpu_rect(compiled, full, output, segment)
        .ok()
        .and_then(|windows| windows.received_cut(compiled, full, segment))
}

/// The GPU preview of `evaluation`, an open draft's preview job's evaluation, drawn as `view`
/// says: the plan from the earliest layer the draft changes, at the stage the job's proxy phase
/// draws at — at Fit, and at a percentage zoom below 100% — or the exact stage over the visible
/// region at 100% or more, and the boundary it starts from. `O(layers)` on the catalog owner: one
/// compile at the proxy stage for the window, one for the plan, a lookup in the estimate store for
/// each global estimate, and no pixel read.
pub(crate) fn plan_preview(
    evaluation: &Evaluation,
    draft: &Draft,
    view: GpuView,
) -> Result<GpuPreview, Error> {
    let registry = evaluation.registry();
    let recipe = evaluation.recipe();
    let drafted_layer = drafted(registry, recipe, draft);
    let modulated = draft
        .target
        .get("mask")
        .and_then(|mask| MaskId::parse(mask.clone()).ok())
        .and_then(|mask| {
            recipe
                .layers
                .iter()
                .position(|layer| layer.mask.as_ref() == Some(&mask))
        });
    let changed = first_change(&evaluation.entry().snapshot.recipe, recipe);
    let Some(earliest) = [
        changed,
        drafted_layer.as_ref().map(|drafted| drafted.index),
        modulated,
    ]
    .into_iter()
    .flatten()
    .min() else {
        return Ok(GpuPreview {
            answer: GpuAnswer::Fallback(GpuFallback::Unchanged),
            boundary: None,
            cpu_shape: None,
            layer: None,
        });
    };
    // Every gesture is planned from the source itself, whatever it changes, as the stack's picture
    // at rest is ([`plan_rest`]): the boundary is the (proxy) source, one key for every gesture over
    // the same source and view, which the photo surface derives from the source it holds, and the
    // surface keeps each spatial operation's output by content, so what a gesture leaves unchanged
    // runs once. A gesture that changes a source layer, or a geometry layer before the stack's
    // first content layer, which only a stack of nothing but geometry has, starts from that
    // layer's input, which names `boundary-stage`: the source the surface holds is not its input.
    let editable = first_content_layer(registry, recipe);
    let boundary = (earliest < editable).then_some(earliest);
    let fit = FitStage::of_view(evaluation, view)?;
    // The drafted layer in its GPU shape, inserted as the neutral layer its first commit would
    // add when the stack does not hold it yet.
    let request = fit.request(boundary);
    let (planned, request) = match &drafted_layer {
        Some(drafted) if !drafted.held => (
            with_neutral(recipe, drafted.index, &drafted.effect, drafted.mask.clone()),
            request.drafted(drafted.index),
        ),
        Some(drafted) => (recipe.clone(), request.drafted(drafted.index)),
        None => (recipe.clone(), request),
    };
    let spatial_drafted = drafted_layer.as_ref().and_then(|drafted| {
        matches!(
            registry.effect_stage(&drafted.effect),
            Some(EffectStage::Restoration | EffectStage::Spatial)
        )
        .then_some(drafted.index)
    });
    planned_preview(
        evaluation,
        &fit,
        &planned,
        boundary,
        request,
        spatial_drafted,
    )
}

/// A displayed stack's picture at rest on the GPU, planned with its preview job on the catalog
/// owner (`docs/design/gpu-first.md`, stage 2).
#[derive(Clone, Debug)]
pub struct GpuRest {
    /// The plan of the stack itself at the view, from the source, every layer in the CPU's shape,
    /// and the boundary it starts from — the one every gesture over the same source and view
    /// starts from, which the photo surface derives from the source it holds. At a percentage zoom
    /// of 100% or more it is the picture at rest, the visible region at full scale over the window
    /// cut from the source; at Fit and below 100% it is drawn over the source reduced to the
    /// view's size, as a drag's frame is, until the picture at rest lands, and kept behind it for
    /// the next gesture.
    pub view: GpuPreview,
    /// The stack at full resolution in tiles ([`RestTiles`]), which the histogram and clipping
    /// counts are reduced from at every view, and which at Fit and below 100%, where the view
    /// draws the output stage smaller than it is, are reduced to the view's size as the picture at
    /// rest process-first ([`RestTiles::reduction`]); or why they cannot be drawn so now: a global
    /// estimate the store does not hold yet for the stack, which its exact phase stores, after
    /// which the worker plans the tiles at Fit again ([`ExactOutcome::rest`]), or a stack the GPU
    /// cannot draw. Where the view draws the output stage at its own size or larger, the picture
    /// at rest is [`Self::view`]'s plan and the tiles are the counts' alone. `None` only from a
    /// caller that plans no tiles.
    ///
    /// [`ExactOutcome::rest`]: crate::ExactOutcome::rest
    pub tiles: Option<Result<Box<RestTiles>, GpuFallback>>,
}

/// The sides a picture at rest's tiles of the output stage take, longest first: the first whose
/// tile's slot the plan's own figures hold within [`REST_TILE_BYTES`].
pub const REST_TILE_SIDES: [u32; 4] = [2048, 1024, 512, 256];

/// What one tile's evaluation may take on the GPU by the plan's own figures — the boundary over the
/// tile's window, an intermediate for each link and a tail, the spatial planes and the tile's
/// output — a quarter of the photo surface's 2 GiB GPU-preview budget, so a tile's slot sits beside
/// a gesture's, the source and the picture at rest's own accumulator. A constant and the plan: never
/// the bytes in use.
pub const REST_TILE_BYTES: u64 = 512 << 20;

/// The stack at full resolution in tiles of the output stage, each the whole stack's plan over its
/// tile at full scale from its own window of the source (`docs/design/gpu-preview.md`, "The
/// picture at rest"; `docs/design/gpu-first.md`, stage 2): what the histogram and clipping counts
/// are reduced from at every view, and at Fit and below 100% the picture at rest process-first,
/// the tiles reduced to the view's size by an area-weighted average of their linear light — the
/// frame the reference is held to at those views.
#[derive(Clone, Debug, PartialEq)]
pub struct RestTiles {
    /// The plan of the whole stack at the exact stage, from the source, its global estimates read
    /// from the store: every tile's plan, which a tile's region and window place.
    pub plan: Box<GpuPlan>,
    /// The tiles, row by row: each its rectangle of the output stage and the window of the source
    /// it reads, anchored ([`GpuPlan::anchor`]). They cover every pixel of the output stage once.
    pub tiles: Vec<RestTile>,
    /// The output stage the tiles cover.
    pub output: Stage,
    /// Where the view draws the output stage smaller than it is, the reduction of the tiles to the
    /// view's size, the picture at rest; `None` where it draws the stage at its own size or larger,
    /// at 100% and above among them, whose tiles are the counts' alone.
    pub reduction: Option<RestReduction>,
    /// The source every tile's boundary is cut from, and how its texels are held.
    pub source: ProxyIdentity,
    pub format: crate::BoundaryFormat,
}

/// The picture at rest's reduction of the output stage to the view's size.
#[derive(Clone, Debug, PartialEq)]
pub struct RestReduction {
    /// The view's size the output stage is reduced to: the frame the CPU's proxy phase draws at the
    /// view's bounds, or, for a stack drawn at its exact stage, the output stage fitted to them.
    pub view: (u32, u32),
    /// The reduction's coverage of the output stage across and down.
    pub across: crate::ProxyCoverage,
    pub down: crate::ProxyCoverage,
}

/// One tile of a picture at rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestTile {
    /// Its rectangle of the output stage.
    pub rect: Region,
    /// The window of the source it reads, the rectangle and every margin after it, its origin a
    /// multiple of the plan's anchor.
    pub window: Region,
}

impl RestTiles {
    /// A lens warp's geometry tail, whose whole output stage's grid at one display pixel an output
    /// pixel every tile takes its part of; `None` for an affine or projective tail.
    pub fn warp(&self) -> Option<&super::GpuGeometry> {
        self.plan
            .geometry
            .needs_grid()
            .then_some(&self.plan.geometry)
    }
}

/// `output` fitted to `bounds` when the bounds draw it smaller: its size at the scale the view
/// draws it at.
fn fitted(output: Stage, bounds: ProxyBounds) -> Option<(u32, u32)> {
    let bounds = bounds.clamped();
    let scale = (f64::from(bounds.width) / f64::from(output.width))
        .min(f64::from(bounds.height) / f64::from(output.height));
    (scale.is_finite() && scale < 1.0).then(|| {
        (
            ((f64::from(output.width) * scale).round() as u32).clamp(1, output.width),
            ((f64::from(output.height) * scale).round() as u32).clamp(1, output.height),
        )
    })
}

/// What a tile's evaluation over `window` of a `format` boundary takes by `plan`'s own figures: the
/// boundary, an intermediate for every link and a tail's, the spatial planes and a `side` square
/// output's codes.
fn tile_bytes(plan: &GpuPlan, window: Region, format: crate::BoundaryFormat, side: u32) -> u64 {
    let texels = u64::from(window.width) * u64::from(window.height);
    let links = plan.spatial.len() as u64 + 1;
    let planes: u64 = plan
        .spatial
        .iter()
        .map(|spatial| spatial.plane_bytes((window.x0, window.y0), (window.width, window.height)))
        .sum();
    texels * format.texel_bytes() as u64 * (links + 1) + planes + u64::from(side).pow(2) * 4
}

/// The picture at rest of `evaluation` at Fit bounds `bounds`, process-first ([`RestTiles`]):
/// `None` when the bounds draw the output stage at its own size; the reason when the stack cannot
/// be drawn so now — a global estimate the store does not hold yet, which names `region-estimate`,
/// or a stack the window planner cannot cut. `O(tiles × segments)` on the catalog owner: one plan,
/// a window per tile, no pixel read.
pub(crate) fn plan_rest_tiles(
    evaluation: &Evaluation,
    bounds: ProxyBounds,
    side: Option<u32>,
) -> Result<Option<Result<Box<RestTiles>, GpuFallback>>, Error> {
    let output = evaluation.compiled()?.stage();
    // The view's size: the CPU proxy frame's, which a gesture's frame draws at too, or the output
    // stage fitted to the bounds for a stack drawn at its exact stage.
    let fit = FitStage::of_view(evaluation, GpuView::Fit(bounds))?;
    let view = match fit.plan {
        Some(_) => {
            let stage = fit.compiled.stage();
            (stage.width, stage.height)
        }
        None => match fitted(output, bounds) {
            Some(view) => view,
            None => return Ok(None),
        },
    };
    plan_tiles(evaluation, Some(view), side).map(Some)
}

/// The stack of `evaluation` at full resolution in tiles with no reduction to a view
/// ([`RestTiles`]): the tiles the histogram and clipping counts are reduced from where the view
/// draws the output stage at its own size or larger. The reason when the stack cannot be drawn so
/// now, as [`plan_rest_tiles`] names it. `O(tiles × segments)`, no pixel read.
pub(crate) fn plan_count_tiles(
    evaluation: &Evaluation,
    side: Option<u32>,
) -> Result<Result<Box<RestTiles>, GpuFallback>, Error> {
    plan_tiles(evaluation, None, side)
}

/// The stack of `evaluation` at full resolution in tiles, reduced to `view` where it names one.
fn plan_tiles(
    evaluation: &Evaluation,
    view: Option<(u32, u32)>,
    side: Option<u32>,
) -> Result<Result<Box<RestTiles>, GpuFallback>, Error> {
    let registry = evaluation.registry();
    let recipe = evaluation.recipe();
    let compiled = evaluation.compiled()?;
    let output = compiled.stage();
    let full = evaluation.source().dimensions();
    let full = Stage {
        width: full.0,
        height: full.1,
    };
    let linear = matches!(evaluation.source(), crate::PreviewSource::Raw { .. });
    let request = GpuPlanRequest::exact(0, full).from_source();
    let request = if linear { request.linear() } else { request };
    let estimates = GpuEstimates {
        context: evaluation.context(),
        source: EstimateSource::Render(evaluation.source().into()),
    };
    let plan = match gpu_plan_holding(registry, recipe, request, Some(estimates), &[])? {
        GpuAnswer::Plan(plan) => plan,
        GpuAnswer::Fallback(reason) => return Ok(Err(reason)),
    };
    // Every global estimate is the exact stage's, read from the store: one the GPU would take from
    // a tile alone is not the whole stage's.
    if let Some(spatial) = plan.spatial.iter().find(|spatial| spatial.estimated) {
        return Ok(Err(GpuFallback::RegionEstimate {
            layer: spatial.layer,
        }));
    }
    if let Some(layer) = compiled.unheld_estimate(0, &estimates)? {
        return Ok(Err(GpuFallback::RegionEstimate { layer }));
    }
    let anchor = plan.anchor();
    let format = crate::BoundaryFormat::of(linear);
    let source = (full.width, full.height);
    let window_of = |rect: Region| {
        WindowPlan::of_gpu_rect(compiled, source, rect, 0)
            .map(|windows| super::plan::anchored(windows.reads(0), anchor))
    };
    // The longest side whose tile in the middle of the stage, its window grown on every side, the
    // plan's own figures hold; a test may name its own.
    let side = side.unwrap_or_else(|| {
        REST_TILE_SIDES
            .into_iter()
            .find(|side| {
                let (width, height) = ((*side).min(output.width), (*side).min(output.height));
                let middle = Region {
                    x0: (output.width - width) / 2,
                    y0: (output.height - height) / 2,
                    width,
                    height,
                };
                window_of(middle)
                    .is_ok_and(|window| tile_bytes(&plan, window, format, *side) <= REST_TILE_BYTES)
            })
            .unwrap_or(REST_TILE_SIDES[REST_TILE_SIDES.len() - 1])
    });
    let mut tiles = Vec::new();
    for y0 in (0..output.height).step_by(side as usize) {
        for x0 in (0..output.width).step_by(side as usize) {
            let rect = Region {
                x0,
                y0,
                width: side.min(output.width - x0),
                height: side.min(output.height - y0),
            };
            match window_of(rect) {
                Ok(window) => tiles.push(RestTile { rect, window }),
                Err(reason) => {
                    return Ok(Err(GpuFallback::Unplannable(format!(
                        "the picture at rest's tile at ({x0}, {y0}): {}",
                        reason.reason()
                    ))));
                }
            }
        }
    }
    Ok(Ok(Box::new(RestTiles {
        plan,
        tiles,
        output,
        reduction: view.map(|view| RestReduction {
            view,
            across: crate::area_coverage(output.width, view.0),
            down: crate::area_coverage(output.height, view.1),
        }),
        source: evaluation.source().identity(),
        format,
    })))
}

/// The GPU picture at rest of a committed stack, `evaluation` a job of no draft, drawn as `view`
/// says ([`GpuRest`]): every stack has one, the empty stack and a stack of geometry alone included,
/// since it is planned from the source; and its tiles at full resolution, which the histogram and
/// clipping counts are reduced from at every view and which at Fit and below 100% are the picture
/// at rest. `O(layers)` on the catalog owner and `O(tiles × segments)` for its tiles, no pixel
/// read.
pub(crate) fn plan_rest(evaluation: &Evaluation, view: GpuView) -> Result<GpuRest, Error> {
    let recipe = evaluation.recipe();
    let fit = FitStage::of_view(evaluation, view)?;
    let request = fit.request(None);
    let planned = planned_preview(evaluation, &fit, recipe, None, request, None)?;
    // Reduced to the view where it draws the stage smaller than it is; the counts' alone elsewhere.
    let reduced = match view {
        GpuView::Fit(bounds) => plan_rest_tiles(evaluation, bounds, None)?,
        GpuView::Region { .. } => None,
    };
    let tiles = Some(match reduced {
        Some(tiles) => tiles,
        None => plan_count_tiles(evaluation, None)?,
    });
    Ok(GpuRest {
        view: planned,
        tiles,
    })
}

/// The plan of `planned` from layer `boundary`'s input, or from the source for `None`, at `fit`'s
/// stage, with `request`, and the boundary it starts from; over a region at 100% or more also the
/// window the boundary will hold and, for a drafted restoration or spatial layer at
/// `spatial_drafted`, the plan of its CPU shape.
fn planned_preview(
    evaluation: &Evaluation,
    fit: &FitStage,
    planned: &Recipe,
    boundary: Option<usize>,
    request: GpuPlanRequest,
    spatial_drafted: Option<usize>,
) -> Result<GpuPreview, Error> {
    let registry = evaluation.registry();
    let recipe = evaluation.recipe();
    // Over a region at 100% or more, a spatial layer's estimates the store holds for the stack the
    // draft was opened over, held for the drag where it holds none for the drafted stack. A
    // windowed proxy never holds a restoration layer before an estimate: the CPU's planner keeps
    // such a proxy whole, since no window can prepare the estimate behind it.
    let held = match fit.region {
        Some(_) => held_prefixes(registry, evaluation.entry(), planned, fit.sampling())?,
        None => Vec::new(),
    };
    let estimates = fit.estimates(evaluation);
    let mut answer = gpu_plan_holding(registry, planned, request, Some(estimates), &held)?;
    // From the source, the first segment's input before its first operation.
    let position = match boundary {
        None => (0, 0),
        Some(boundary) => position(&fit.compiled, boundary).ok_or_else(|| {
            Error::internal(format!(
                "the GPU preview's boundary layer {boundary} is past the stack"
            ))
        })?,
    };
    // The layers before the boundary, which its texels hold: none from the source.
    let before = boundary.unwrap_or(0);
    // Over a region at 100% or more, the window of the received stage the boundary will hold,
    // planned now so what it takes is known before it is rendered. Every global estimate is held rather than
    // reduced over that window: the plan's own, read from the store, and one the boundary's render
    // reads behind an earlier spatial layer, which the store must hold. One the GPU would take
    // from the region alone, where the exact frame reads the whole stage's, or one the store does
    // not hold behind an earlier spatial layer, keeps the drag on the CPU.
    let mut window = None;
    // At the exact stage at Fit, the window the whole output stage reads, so a crop's boundary
    // holds what the crop reads rather than its whole source. A global estimate the GPU takes from
    // the stage it holds would see the window alone, so such a plan keeps the whole stage; so does
    // a stack the planner cannot cut.
    // The window is planned as the boundary's render plans it (`Render::output_boundary`), whose
    // estimate behind an earlier spatial layer the store must hold.
    if let (GpuAnswer::Plan(plan), None, None) = (&answer, fit.plan, fit.region)
        && !plan.spatial.iter().any(|spatial| spatial.estimated)
        && fit
            .compiled
            .unheld_estimate(position.0, &estimates)?
            .is_none()
    {
        window = output_window(&fit.compiled, fit.full, position.0);
    }
    // Behind a windowed proxy the CPU's light is the exact stage's, which the plan reads from the
    // store or holds for a Detail drag; one it would take on the GPU, over the proxy, would be the
    // proxy's, which misses the spatial limits on the corpus.
    if let (GpuAnswer::Plan(plan), Some(_)) =
        (&answer, fit.plan.filter(|plan| plan.window.is_some()))
        && let Some(spatial) = plan.spatial.iter().find(|spatial| spatial.estimated)
    {
        answer = GpuAnswer::Fallback(GpuFallback::WindowEstimate {
            layer: spatial.layer,
        });
    }
    if let (GpuAnswer::Plan(plan), Some((rect, _))) = (&answer, fit.region) {
        let full = (fit.full.width, fit.full.height);
        let unheld = match plan.spatial.iter().find(|spatial| spatial.estimated) {
            Some(spatial) => Some(spatial.layer),
            None => fit.compiled.unheld_estimate(position.0, &estimates)?,
        };
        answer = match unheld {
            Some(layer) => GpuAnswer::Fallback(GpuFallback::RegionEstimate { layer }),
            None => match WindowPlan::of_gpu_rect(&fit.compiled, full, rect, position.0) {
                Ok(windows) => {
                    window = Some(windows.reads(position.0));
                    answer
                }
                Err(reason) => GpuAnswer::Fallback(GpuFallback::Unplannable(format!(
                    "the region's boundary: {}",
                    reason.reason()
                ))),
            },
        };
    }
    // Over a region at 100% or more a drafted restoration or spatial layer's GPU shape charges the
    // planes of its units at zero over the window too: the plan of its CPU shape rides beside it,
    // from the same boundary, when it holds less.
    let mut cpu_shape = None;
    if let (GpuAnswer::Plan(plan), Some(_), Some(_)) = (&answer, fit.region, spatial_drafted)
        && request.drafted.is_some()
    {
        let unshaped = GpuPlanRequest {
            drafted: None,
            ..request
        };
        let extent = |plan: &GpuPlan| -> Vec<(usize, usize)> {
            plan.spatial
                .iter()
                .map(|spatial| (spatial.passes.len(), spatial.applies.len()))
                .collect()
        };
        if let GpuAnswer::Plan(smaller) =
            gpu_plan_holding(registry, planned, unshaped, Some(estimates), &held)?
            && extent(&smaller) != extent(plan)
            && !smaller.spatial.iter().any(|spatial| spatial.estimated)
        {
            cpu_shape = Some(smaller);
        }
    }
    // Every window is anchored, its origin a multiple of the plan's anchor, so each texel it holds
    // is the whole stage's, bit for bit, whichever window holds it ([`GpuPlan::anchor`]).
    if let GpuAnswer::Plan(plan) = &answer {
        let anchor =
            super::plan::common_anchor(std::iter::once(&**plan).chain(cpu_shape.as_deref()));
        window = window.map(|window| super::plan::anchored(window, anchor));
    }
    // Every plan starts from the source: a boundary at a source or geometry layer before the
    // stack's first content layer names `boundary-stage` and has no plan.
    let boundary_request = match &answer {
        GpuAnswer::Fallback(_) => None,
        GpuAnswer::Plan(_) if position != (0, 0) => {
            return Err(Error::internal(
                "a GPU plan starts from the source, at the first segment's first operation",
            ));
        }
        GpuAnswer::Plan(plan) => Some(SourceBoundary {
            key: BoundaryKey {
                source: evaluation.source().identity(),
                prefix: prefix_hash(&recipe.layers[..before], &recipe.masks, fit.sampling())?,
                layer: before,
                plan: fit.plan,
                region: fit.region.map(|(rect, _)| rect),
                window,
            },
            format: crate::BoundaryFormat::of(fit.linear),
            magnification: fit.region.map_or(1.0, |(_, magnification)| magnification),
            window,
            warp: plan.geometry.needs_grid().then(|| plan.geometry.clone()),
        }),
    };
    let layer = answer
        .fallback()
        .and_then(GpuFallback::layer)
        .and_then(|layer| planned.layers.get(layer))
        .map(|layer| registry.layer_label(&layer.effect_id));
    Ok(GpuPreview {
        answer,
        boundary: boundary_request,
        cpu_shape,
        layer,
    })
}

/// The most links one plan's chain holds: the colour operations before its first spatial
/// operation, then one link for each spatial operation — a global Detail layer, a global Presence
/// layer and each masked spatial layer ([`MAX_MASKED_SPATIAL_LAYERS`]).
pub const GPU_PLAN_LINKS: usize = 3 + MAX_MASKED_SPATIAL_LAYERS;

/// The most link sequences a warm list holds ([`plan_warm`]): the surface's 64-sequence pipeline
/// cache (`PIPELINE_CACHE`, `crates/luxforge-ui/src/photo_surface/gpu_preview/compile.rs`) less
/// the largest plan's [`GPU_PLAN_LINKS`], so a whole warm list and every link of the plan a drag
/// draws are held together, and warming never evicts what a tick asks for or what it has just
/// warmed. The stack's own plan and a drag of every restoration and spatial layer take at most
/// `2 × GPU_PLAN_LINKS − 1` of them (each such drag adds its drafted link at most), so they always
/// fit; the colour candidates fill the rest.
pub const GPU_WARM_LINKS: usize = 45;

/// What a warmed plan's program sequence is told apart by, as the surface keys a sequence or
/// finer: each link's key in chain order ([`warm_links`]). Two plans of one key compile to the same
/// sequences.
#[cfg(test)]
pub(crate) fn warm_sequence(plan: &GpuPlan) -> Vec<String> {
    warm_links(plan).concat()
}

/// What each link of a plan's chain is told apart by, as the surface keys the sequence it compiles
/// for a link or finer (`crates/luxforge-ui/src/photo_surface/gpu_preview/chain.rs`): the colour
/// operations before the first spatial operation, when there are any; then each spatial operation
/// with the colour operations after it, the last link also holding the geometry tail and the
/// output operations. A plan without a spatial operation is one link. A colour operation is told
/// apart by its programs and its mask's components; a spatial operation by its program, clamp,
/// mask, planes, passes but for their words, and applies; the tail by its kind. Two links of one
/// key compile to one sequence, wherever they sit in a chain and whichever plan holds them.
pub(crate) fn warm_links(plan: &GpuPlan) -> Vec<Vec<String>> {
    let mask = |mask: &Option<super::GpuMask>| {
        mask.as_ref().map(|mask| {
            let components: Vec<&str> = mask
                .components
                .iter()
                .map(|component| component.program.program.entry)
                .collect();
            format!("masked by {components:?}")
        })
    };
    let colour = |operations: &[super::GpuOperation]| -> Vec<String> {
        operations
            .iter()
            .flat_map(|operation| {
                operation
                    .units
                    .iter()
                    .map(|unit| unit.program.entry.to_owned())
                    .chain(mask(&operation.mask))
            })
            .collect()
    };
    let mut links = Vec::new();
    let content = colour(&plan.content);
    if !content.is_empty() || plan.spatial.is_empty() {
        links.push(content);
    }
    // Each chained spatial operation in order, the colour operations after it in its link.
    for spatial in &plan.spatial {
        let mut link = vec![format!(
            "{}, clamps {}",
            spatial.program.entry, spatial.clamps
        )];
        link.extend(mask(&spatial.mask));
        link.extend(spatial.planes.iter().map(|plane| format!("{plane:?}")));
        link.extend(spatial.passes.iter().map(|pass| {
            format!(
                "{} {:?} -> {}, source {}, {:?}",
                pass.kernel, pass.inputs, pass.output, pass.source, pass.shape
            )
        }));
        // Whether an apply is the identity is the tick's, as its words are: no part of the key.
        link.extend(spatial.applies.iter().map(|apply| {
            format!(
                "{} {:?}, words {}",
                apply.function, apply.planes, apply.words
            )
        }));
        link.extend(colour(&spatial.after));
        links.push(link);
    }
    let last = links.last_mut().expect("a plan is at least one link");
    last.push(format!(
        "geometry affine {}, projective {}, clamps {}",
        plan.geometry.affine().is_some(),
        plan.geometry.projective().is_some(),
        plan.geometry.clamps
    ));
    last.extend(colour(&plan.output));
    links
}

/// The plans a gesture on `evaluation`'s stack is likely to draw at `view`, for the desktop to
/// warm their program sequences before a drag begins, in the order they are to compile: when a
/// layer is masked, the stack's own plan, which a stroke or a shape moved draws; a drag of each
/// colour or finish layer the stack holds, and the first drag of each field-patch colour or finish
/// module it does not hold yet; then a drag of each restoration or spatial layer the stack holds,
/// reading the estimates the store holds, which a drag of a colour layer before them never does.
/// Each drag is planned with its layer in its GPU shape, as a draft of it is planned
/// ([`plan_preview`]).
///
/// The surface compiles one sequence per link of a plan's chain, so a plan joins only when it holds
/// a link no plan before it holds ([`warm_links`]). A restoration or spatial layer's drag draws the
/// stack's own links but its drafted one, so the list holds one drag for each distinct drafted
/// shape, and the drag of every such layer finds each link it draws warmed. The list holds at most
/// [`GPU_WARM_LINKS`] links: the stack's own plan and those drags always fit, and a colour
/// candidate joins only while it leaves room for them. `O(layers × modules)` compiles on the
/// catalog owner, with no pixel read.
pub(crate) fn plan_warm(evaluation: &Evaluation, view: GpuView) -> Result<Vec<GpuPlan>, Error> {
    let registry = evaluation.registry();
    let recipe = evaluation.recipe();
    let fit = FitStage::of_view(evaluation, view)?;
    let colour = |effect: &str| {
        matches!(
            registry.effect_stage(effect),
            Some(EffectStage::Color | EffectStage::Finish)
        )
    };
    // Each colour candidate: the stack it is planned over and its boundary.
    let mut colours: Vec<(Recipe, usize)> = recipe
        .layers
        .iter()
        .enumerate()
        .filter(|(_, layer)| colour(&layer.effect_id))
        .map(|(index, _)| (recipe.clone(), index))
        .collect();
    for provider in registry.providers() {
        let descriptor = provider.descriptor();
        let [effect] = descriptor.effects.as_slice() else {
            continue;
        };
        if descriptor.developer
            || !colour(&effect.id)
            || !descriptor.actions.iter().any(|action| action.patch)
            || recipe
                .layers
                .iter()
                .any(|layer| layer.effect_id == effect.id && layer.mask.is_none())
        {
            continue;
        }
        let index =
            registry.insertion_index_for_target(&recipe.layers, &effect.id, None, &recipe.masks);
        colours.push((with_neutral(recipe, index, &effect.id, None), index));
    }
    let spatial = recipe.layers.iter().enumerate().filter(|(_, layer)| {
        matches!(
            registry.effect_stage(&layer.effect_id),
            Some(EffectStage::Restoration | EffectStage::Spatial)
        )
    });
    // Every link a plan the list holds compiles, and the links of `plan` it would add.
    let mut links: Vec<Vec<String>> = Vec::new();
    let fresh = |plan: &GpuPlan, held: &[Vec<String>]| {
        let mut fresh: Vec<Vec<String>> = Vec::new();
        for link in warm_links(plan) {
            if !held.contains(&link) && !fresh.contains(&link) {
                fresh.push(link);
            }
        }
        fresh
    };
    let mut plans: Vec<GpuPlan> = Vec::new();
    // Every drag is planned from the source ([`plan_preview`]), and so is a gesture of a mask the
    // stack's layers already read: a stroke, or a shape moved, draws the committed stack itself,
    // every layer in the CPU's shape.
    if recipe.layers.iter().any(|layer| layer.mask.is_some())
        && let GpuAnswer::Plan(plan) = gpu_plan_with(
            registry,
            recipe,
            fit.request(None),
            Some(fit.estimates(evaluation)),
        )?
    {
        links = fresh(&plan, &links);
        plans.push(*plan);
    }
    // The restoration and spatial layers' drags are chosen first, so the colour candidates leave
    // room for them, and join the list after the colour candidates. A spatial layer's own drag
    // leaves the layers before it alone, so its ticks read the store, as the warmed plan does.
    let mut drags: Vec<GpuPlan> = Vec::new();
    for (index, _) in spatial {
        let request = fit.request(None).drafted(index);
        if let GpuAnswer::Plan(plan) =
            gpu_plan_with(registry, recipe, request, Some(fit.estimates(evaluation)))?
        {
            let added = fresh(&plan, &links);
            if !added.is_empty() && links.len() + added.len() <= GPU_WARM_LINKS {
                links.extend(added);
                drags.push(*plan);
            }
        }
    }
    // A drag of a colour layer changes the input of every spatial layer after it, so its ticks
    // take their estimates on the GPU.
    for (planned, index) in colours {
        let request = fit.request(None).drafted(index);
        if let GpuAnswer::Plan(plan) = gpu_plan_with(registry, &planned, request, None)? {
            let added = fresh(&plan, &links);
            if !added.is_empty() && links.len() + added.len() <= GPU_WARM_LINKS {
                links.extend(added);
                plans.push(*plan);
            }
        }
    }
    plans.extend(drags);
    Ok(plans)
}
