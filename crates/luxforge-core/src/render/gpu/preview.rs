//! A draft's GPU preview, planned with its preview job on the catalog owner
//! (`docs/design/gpu-preview.md`, "A tick"): the plan the photo surface draws a tick from, and
//! the boundary it starts from, which the photo surface derives from the source it holds.
//!
//! Planning reads the job's own evaluation — the draft's effective recipe beside the entry it was
//! planned over, and the stack's one compilation — and compiles at the stage the view draws at:
//! `O(layers)`, no pixel read, nothing that scales with the image.
//!
//! - **From the source.** Every plan starts from the source itself, the first segment's input
//!   before its first operation, so a drag and the stack's picture at rest ([`plan_rest`]) share
//!   one boundary over one source and view, every stack has a plan, the empty one and one of
//!   geometry alone included, and the geometry is the plan's tail. A draft of a RAW's white
//!   balance, its source layer, is drawn over the source the surface holds, developed at the
//!   entry's white balance, with the change as a leading pointwise step
//!   ([`WhiteBalanceApproximation`], the matrix the drafted preview's evaluation carries); at rest
//!   after its release the picture is the redevelopment's.
//! - **A stable shape.** A field-patch module compiles only its non-neutral units, so the drafted
//!   layer is planned in its GPU shape, every unit present and a neutral one as its identity
//!   ([`super::GpuPlanRequest::drafted`]); a drafted layer the stack does not hold yet, because
//!   its value is still neutral, is planned as the neutral layer its first commit would insert.
//!   A drag that leaves or returns to neutral therefore keeps one program sequence. The CPU
//!   compile is untouched: only this plan asks for the shape.
//! - **Lights.** A spatial operation's global estimate, Dehaze's atmospheric light, is read from the
//!   light plane its plan's light link writes from the whole stage at full resolution, per frame
//!   ([`super::GpuLight`]), at every view: the one the picture at rest draws with, with every
//!   spatial layer before it included. Behind a spatial layer its input is that layer's exact
//!   output, which only the picture at rest's staged sweeps write ([`super::GpuLightInput::Stage`]):
//!   the view plan at rest reads the light they computed, kept under its input's key, and never its
//!   stand-in ([`super::GpuLight::stand_in`], none at rest). A drag that leaves a light's input
//!   unchanged reads that light; one that changes it only through restoration or spatial layers
//!   reads the light of the stack it started from; either computes the stand-in over the source,
//!   those layers left out, where the slot keeps no exact light. One that changes it through a
//!   colour layer computes it every tick from the source with every spatial layer before it left
//!   out ([`super::GpuLightRestoration::LeftOut`]), the owner's decision of 2026-10-06
//!   (`docs/decisions.md`, "GPU-first rendering").
//! - **The boundary key** names everything the boundary's texels depend on: the source's identity
//!   (fingerprint, development and view), the layers before the boundary and the masks they read,
//!   the boundary's index, and the reduced-stage plan with its bounds and window, or at the exact stage at
//!   Fit the window the output reads, or at a percentage zoom of 100% or more the region of the
//!   output stage the boundary is held for. Equal keys hold equal texels.
//! - **Where it is drawn** ([`GpuView`]): at Fit, and at a percentage zoom below 100%, the reduced
//!   stage at the job's display bounds ([`super::fit`]): Fit's, or below 100% the displayed size
//!   of the whole stage; at a percentage zoom of 100% or more, the exact stage, over the visible
//!   region at full scale, whose boundary is the window of the source that region reads. A region
//!   whose plan's own figures are large carries the draft planned at the reduced stage of the
//!   view's area too ([`GpuPreview::reduced`]), which the desktop draws scaled to the view when the
//!   region's slot would pass the budget.
//! - **What it holds.** Only the part of the source the drawn output reads, through the GPU's own
//!   window walk ([`crate::render::window::WindowPlan::of_gpu_rect`] from the source): at the
//!   reduced stage, the window of it a crop reads; at the exact stage at Fit, which a photograph
//!   that fits the display bounds is drawn at, the window the whole output stage reads — what a
//!   crop, a straightening and a warp read, with their resample's taps and margin, clamped to the
//!   stage; at a percentage zoom of 100% or more, the window the visible region reads.
use super::{
    GpuAnswer, GpuFallback, GpuLight, GpuLightRestoration, GpuPlan, GpuPlanRequest, gpu_lights,
    gpu_plan,
};
use crate::{
    Draft, EFFECT_FORMAT, EffectStage, Error, Evaluation, Layer, LayerId, MaskId, ModuleRegistry,
    ProxyBounds, ProxyIdentity, ProxyPlan, Recipe, SourceTag,
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
    /// A whole frame in these display bounds, at the reduced stage fitted to them
    /// ([`super::fit`]). At Fit the bounds are the photo area's; at a percentage zoom below 100%
    /// they are the displayed size of the whole stage, so the plan differs from Fit's only in its
    /// size.
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
    /// Over a region at 100% or more whose plan's own figures pass the figure the caller names,
    /// [`REDUCED_AFTER_BYTES`] by default ([`plan_preview_reducing`]): the
    /// same draft planned at the reduced stage of the view's area, the frame Fit draws at the
    /// view's size ([`GpuView::Fit`] at the region's displayed size). The desktop draws it scaled
    /// to the view, the softer drag frame, when the region's slot would pass the GPU-preview budget;
    /// the picture at rest after the release is the region's, sharp. `None` elsewhere.
    pub reduced: Option<Box<GpuPreview>>,
}

/// What a region plan's own figures pass before its draft is planned at the reduced stage too
/// ([`GpuPreview::reduced`]): a quarter of the photo surface's 2 GiB GPU-preview budget, below
/// which no region's slot, beside the largest source the budget holds, passes it. A constant and
/// the plan: never the bytes in use.
pub const REDUCED_AFTER_BYTES: u64 = 512 << 20;

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

/// The white-balance change a drafted RAW preview approximates on the planes its entry developed,
/// and the source layer it is drafted on: `None` for a JPEG and for a RAW whose planes hold the
/// recipe's white balance.
fn drafted_white_balance(
    evaluation: &Evaluation,
) -> Option<(crate::WhiteBalanceApproximation, usize)> {
    let crate::PreviewSource::Raw { settings, .. } = evaluation.source() else {
        return None;
    };
    let balance = settings.white_balance?;
    let registry = evaluation.registry();
    let layer = evaluation
        .recipe()
        .layers
        .iter()
        .position(|layer| registry.effect_stage(&layer.effect_id) == Some(EffectStage::Source))
        .unwrap_or(0);
    Some((balance, layer))
}

/// The leading pointwise step of a RAW white-balance draft: `balance`'s matrix, narrowed to `f32`,
/// over each source texel, reported under the source layer `layer`. It runs Basic's white-balance
/// program, the one linear-sRGB 3 × 3 multiply, unclamped.
fn white_balance_operation(
    balance: &crate::WhiteBalanceApproximation,
    layer: usize,
) -> super::GpuOperation {
    let words = balance
        .matrix()
        .iter()
        .flatten()
        .map(|value| (*value as f32).to_bits())
        .collect();
    super::GpuOperation {
        layer,
        units: vec![super::GpuDescription::new(
            &crate::modules::basic::WHITE_BALANCE_PROGRAM,
            words,
        )],
        position: super::GpuPosition::IDENTITY,
        mask: None,
    }
}

/// `plan` with `balance` applied to each source texel first ([`white_balance_operation`]): before
/// its content operations, and before every light link's, its stand-in's included, whose input is
/// the same source. `O(lights)`.
fn with_white_balance(
    plan: &mut GpuPlan,
    balance: &crate::WhiteBalanceApproximation,
    layer: usize,
) {
    let operation = white_balance_operation(balance, layer);
    plan.content.insert(0, operation.clone());
    for light in &mut plan.lights {
        light.content.insert(0, operation.clone());
        if let Some(stand_in) = &mut light.stand_in {
            stand_in.content.insert(0, operation.clone());
        }
    }
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
#[cfg(feature = "qualification")]
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

/// The stage a job's whole frame is drawn at, at Fit or at a percentage zoom below 100%: the GPU's
/// reduced stage at the job's bounds ([`super::fit::reduced_stage`]), fitted from the output stage
/// of the stack's one compilation, with the window of the reduced stage a crop reads. A stack that
/// fits the bounds, or holds a layer that does not scale, is drawn at its exact stage.
struct FitStage {
    /// The reduced-stage plan, with its window; `None` at the exact stage.
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
    /// The stage `view` is drawn at: the reduced stage at the view's bounds for [`GpuView::Fit`],
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
        let compiled = evaluation.compiled()?;
        let full = evaluation.source().dimensions();
        let reduced = super::fit::reduced_stage(
            evaluation.registry(),
            evaluation.recipe(),
            full,
            compiled.stage(),
            bounds,
        )?;
        let full = Stage {
            width: full.0,
            height: full.1,
        };
        // A developed RAW's frames are the linear path's, unclamped and unquantized between
        // segments, where a JPEG's are clamped and quantized at every stage boundary.
        let linear = matches!(evaluation.source(), crate::PreviewSource::Raw { .. });
        Ok(match reduced {
            Some(reduced) => Self {
                plan: Some(reduced.plan),
                stage: Stage {
                    width: reduced.plan.width,
                    height: reduced.plan.height,
                },
                compiled: reduced.compiled,
                full,
                linear,
                region: None,
            },
            None => Self {
                plan: None,
                compiled: compiled.clone(),
                stage: full,
                full,
                linear,
                region: None,
            },
        })
    }

    /// A plan request from the source itself at this stage, on the source's path
    /// ([`GpuPlanRequest::from_source`]).
    fn request(&self) -> GpuPlanRequest {
        let request = match self.plan {
            Some(_) => GpuPlanRequest::fit(0, self.stage, self.full),
            None => GpuPlanRequest::exact(0, self.full),
        }
        .from_source();
        if self.linear {
            request.linear()
        } else {
            request
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
/// the stack does not hold yet is planned as, the neutral layer its first commit would insert,
/// spelled as its module spells it ([`crate::ToolModule::neutral_payload`]).
fn with_neutral(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    index: usize,
    effect: &str,
    mask: Option<MaskId>,
) -> Recipe {
    // The layer the first commit would insert: the module's neutral payload at its effect's
    // current format.
    let (payload, format) = registry.effect(effect).map_or_else(
        || (json!({}), EFFECT_FORMAT),
        |(provider, descriptor)| (provider.neutral_payload(effect), descriptor.format),
    );
    let mut planned = recipe.clone();
    planned.layers.insert(
        index,
        Layer {
            id: LayerId::new(),
            effect_id: effect.to_owned(),
            effect_format: format,
            payload,
            mask,
            artifacts: Vec::new(),
        },
    );
    planned
}

/// How a drag changes the input of an estimating layer's light from the stack it started from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LightInput {
    /// Every layer before it is the one the stack it started from holds: the light the picture at
    /// rest drew with, which the slot keeps.
    Unchanged,
    /// Only restoration or spatial layers before it change, or are drafted: the light of the stack
    /// the drag started from, kept at rest, since a spatial layer's exact output exists only at
    /// rest; the release's picture at rest computes the new one exactly.
    Spatial,
    /// A colour layer before it, or a mask such a layer reads, changes or is drafted: the light is
    /// computed every tick over the source with every spatial layer before it left out.
    Colour,
}

/// How the drag of `planned`, the drafted stack, changes the input of the light of its layer
/// `layer` from `committed`, the stack the draft was opened over: the layers before it that the
/// draft adds, removes or changes, their masks included, and the drafted layer `drafted` when it
/// lies before it. `O(layers)`.
fn light_input(
    registry: &ModuleRegistry,
    committed: &Recipe,
    planned: &Recipe,
    layer: usize,
    drafted: Option<usize>,
) -> LightInput {
    let Some(id) = planned.layers.get(layer).map(|layer| &layer.id) else {
        return LightInput::Colour;
    };
    // A layer the stack it started from does not hold has no light at rest to read: its own
    // light, planned over the drafted stack, is the one a tick computes.
    let Some(at) = committed.layers.iter().position(|stored| &stored.id == id) else {
        return LightInput::Unchanged;
    };
    let stage_of = |layer: &Layer| registry.effect_stage(&layer.effect_id);
    let mask_of = |recipe: &Recipe, layer: &Layer| {
        layer
            .mask
            .as_ref()
            .and_then(|id| recipe.masks.iter().find(|mask| &mask.id == id).cloned())
    };
    let (before, was) = (&planned.layers[..layer], &committed.layers[..at]);
    let changed = |layer: &Layer, ours: &Recipe, other: (&[Layer], &Recipe)| {
        other
            .0
            .iter()
            .find(|stored| stored.id == layer.id)
            .is_none_or(|stored| {
                stored != layer || mask_of(other.1, stored) != mask_of(ours, layer)
            })
    };
    let mut changes = before
        .iter()
        .filter(|layer| changed(layer, planned, (was, committed)))
        .chain(
            was.iter()
                .filter(|layer| changed(layer, committed, (before, planned))),
        )
        .chain(
            drafted
                .filter(|index| *index < layer)
                .map(|index| &planned.layers[index]),
        )
        .peekable();
    if changes.peek().is_none() {
        return LightInput::Unchanged;
    }
    if changes.all(|layer| {
        matches!(
            stage_of(layer),
            Some(EffectStage::Restoration | EffectStage::Spatial)
        )
    }) {
        LightInput::Spatial
    } else {
        LightInput::Colour
    }
}

/// `planned` with every layer before layer `layer` as `committed` holds it, its mask's definition
/// too, and one `committed` does not hold left neutral: the stack whose light the slot kept from
/// before the drag, in the drafted stack's own order, so its light is planned at that layer's
/// place.
fn held_before(committed: &Recipe, planned: &Recipe, layer: usize) -> Recipe {
    let mut held = planned.clone();
    for stored in held.layers.iter_mut().take(layer) {
        match committed.layers.iter().find(|was| was.id == stored.id) {
            Some(was) => {
                if let Some(id) = &was.mask
                    && let Some(mask) = committed.masks.iter().find(|mask| &mask.id == id)
                {
                    match held.masks.iter_mut().find(|held| &held.id == id) {
                        Some(held) => *held = mask.clone(),
                        None => held.masks.push(mask.clone()),
                    }
                }
                *stored = was.clone();
            }
            None => stored.payload = json!({}),
        }
    }
    held
}

/// The lights a drag of `planned`, the drafted stack, draws with, from `lights`, the ones its plan
/// for `request` reads: each kept where the drag leaves its input as `committed` held it, the light
/// of the stack the drag started from where it changes that input only through restoration or
/// spatial layers ([`held_before`]), and the light over the source with every spatial layer before
/// it left out where it changes it through a colour layer ([`LightInput`]), computed every tick.
/// `O(layers)` compiles for each changed light, no pixel read.
fn drag_lights(
    registry: &ModuleRegistry,
    committed: &Recipe,
    planned: &Recipe,
    request: GpuPlanRequest,
    lights: &mut [GpuLight],
) -> Result<(), Error> {
    let full = super::spatial::light_request(request);
    let mut left_out: Option<Vec<GpuLight>> = None;
    for light in lights.iter_mut() {
        match light_input(registry, committed, planned, light.layer, request.drafted) {
            LightInput::Unchanged => {}
            LightInput::Spatial => {
                let held = held_before(committed, planned, light.layer);
                let unshaped = GpuPlanRequest {
                    drafted: None,
                    ..full
                };
                if let Some(found) = super::spatial::lights_of(registry, &held, unshaped)?
                    .into_iter()
                    .find(|held| held.layer == light.layer)
                {
                    *light = found;
                }
            }
            LightInput::Colour => {
                let left_out = match &mut left_out {
                    Some(left_out) => left_out,
                    None => left_out.insert(gpu_lights(
                        registry,
                        planned,
                        full,
                        GpuLightRestoration::LeftOut,
                    )?),
                };
                if let Some(found) = left_out.iter().find(|found| found.layer == light.layer) {
                    *light = found.clone();
                }
            }
        }
    }
    Ok(())
}

/// The window of the stage segment `segment` of `compiled`, a stack over a `full` source, receives
/// that the whole output stage reads, planned by the GPU window walk
/// (`Render::output_boundary`, [`WindowPlan::of_gpu_rect`]): `None` when the segment reads all of
/// it or the planner cannot cut the stack. `O(segments)`, no pixel read.
pub(crate) fn output_window(compiled: &Compiled, full: Stage, segment: usize) -> Option<Region> {
    let full = (full.width, full.height);
    let output = Region::whole(compiled.stage());
    WindowPlan::of_gpu_rect(compiled, full, output)
        .ok()
        .and_then(|windows| windows.received_cut(compiled, full, segment))
}

/// The GPU preview of `evaluation`, an open draft's preview job's evaluation, drawn as `view`
/// says: the plan from the source, at the reduced stage of the view's bounds — at Fit, and at a
/// percentage zoom below 100% — or the exact stage over the visible region at 100% or more, and
/// the boundary it starts from, with the lights the drag draws with ([`drag_lights`]); a RAW
/// white-balance draft's change as its leading step ([`with_white_balance`]); and over a region
/// whose plan's own figures pass [`REDUCED_AFTER_BYTES`], the same draft at the reduced stage of
/// the view's area ([`GpuPreview::reduced`]). `O(layers)` on the catalog owner: one compile at the
/// reduced stage for the window, one for the plan, one at the whole stage for its lights and one
/// more for each light the drag changes, those again for a region's reduced plan, and no pixel
/// read.
#[cfg(test)]
pub(crate) fn plan_preview(
    evaluation: &Evaluation,
    draft: &Draft,
    view: GpuView,
) -> Result<GpuPreview, Error> {
    plan_preview_reducing(evaluation, draft, view, REDUCED_AFTER_BYTES)
}

/// [`plan_preview`], a region whose plan's own figures pass `reduce_after` bytes carrying its
/// draft at the reduced stage of the view's area: what the owner plans with the figure the job's
/// request names, [`REDUCED_AFTER_BYTES`] unless a test names a smaller one
/// ([`crate::PreviewRequest::reduce_regions_after`]).
pub(crate) fn plan_preview_reducing(
    evaluation: &Evaluation,
    draft: &Draft,
    view: GpuView,
    reduce_after: u64,
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
    // A draft that changes no layer, drafts none and modulates none draws the entry's frame.
    if changed.is_none() && drafted_layer.is_none() && modulated.is_none() {
        return Ok(GpuPreview {
            answer: GpuAnswer::Fallback(GpuFallback::Unchanged),
            boundary: None,
            cpu_shape: None,
            layer: None,
            reduced: None,
        });
    }
    // Every gesture is planned from the source itself, whatever it changes, as the stack's picture
    // at rest is ([`plan_rest`]): the boundary is the (reduced) source, one key for every gesture
    // over the same source and view, which the photo surface derives from the source it holds,
    // and the surface keeps each spatial operation's output by content, so what a gesture leaves
    // unchanged runs once. A geometry drag on a stack of geometry alone is the plan's tail; a RAW
    // white-balance drag is a leading step over the source the surface holds, developed at the
    // entry's white balance ([`with_white_balance`]).
    let fit = FitStage::of_view(evaluation, view)?;
    // The drafted layer in its GPU shape, inserted as the neutral layer its first commit would
    // add when the stack does not hold it yet.
    let request = fit.request();
    let (planned, request) = match &drafted_layer {
        Some(drafted) if !drafted.held => (
            with_neutral(
                registry,
                recipe,
                drafted.index,
                &drafted.effect,
                drafted.mask.clone(),
            ),
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
    let mut preview = planned_preview(
        evaluation,
        &fit,
        &planned,
        request,
        spatial_drafted,
        Some(&evaluation.entry().snapshot.recipe),
    )?;
    if let Some((balance, layer)) = drafted_white_balance(evaluation) {
        if let GpuAnswer::Plan(plan) = &mut preview.answer {
            with_white_balance(plan, &balance, layer);
        }
        if let Some(plan) = &mut preview.cpu_shape {
            with_white_balance(plan, &balance, layer);
        }
    }
    // A region whose slot may pass the budget carries the draft planned at the reduced stage of
    // the view's area, which the desktop draws in its place, scaled to the view.
    if let (
        GpuView::Region {
            rect,
            magnification,
        },
        Some(bytes),
    ) = (view, region_bytes(&preview))
        && bytes > reduce_after
    {
        let side = |pixels: u32| (f64::from(pixels) * magnification).round().max(1.0) as u32;
        let bounds = ProxyBounds {
            width: side(rect.width),
            height: side(rect.height),
        };
        preview.reduced = Some(Box::new(plan_preview_reducing(
            evaluation,
            draft,
            GpuView::Fit(bounds),
            reduce_after,
        )?));
    }
    Ok(preview)
}

/// What a region plan of `preview` takes by its own figures, the smaller of its two shapes: the
/// boundary over its window and an intermediate for every link and a tail, the spatial planes over
/// it, and the region's output codes. `None` for a preview with no region plan. `O(passes)`.
fn region_bytes(preview: &GpuPreview) -> Option<u64> {
    let (GpuAnswer::Plan(plan), Some(boundary)) = (&preview.answer, &preview.boundary) else {
        return None;
    };
    let (rect, window) = (boundary.key.region()?, boundary.window?);
    let bytes = |plan: &GpuPlan| {
        let texels = u64::from(window.width) * u64::from(window.height);
        let links = plan.spatial.len() as u64 + 1;
        let planes: u64 = plan
            .spatial
            .iter()
            .map(|spatial| {
                spatial.plane_bytes((window.x0, window.y0), (window.width, window.height))
            })
            .sum();
        texels * boundary.format.texel_bytes() as u64 * (links + 1)
            + planes
            + u64::from(rect.width) * u64::from(rect.height) * 4
    };
    Some(
        preview
            .cpu_shape
            .as_deref()
            .map_or(bytes(plan), |smaller| bytes(plan).min(bytes(smaller))),
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
    /// rest process-first ([`RestTiles::reduction`]); or why the GPU cannot draw them. Where the
    /// view draws the output stage at its own size or larger, the picture at rest is
    /// [`Self::view`]'s plan and the tiles are the counts' alone. `None` only from a caller that
    /// plans no tiles.
    pub tiles: Option<Result<Box<RestTiles>, GpuFallback>>,
}

/// The sides a picture at rest's tiles of the output stage take, longest first: the first whose
/// tile's slot the plan's own figures hold within the rest's share ([`RestTiles::share`]) and whose
/// window carries at most [`REST_TILE_WORK`].
pub const REST_TILE_SIDES: [u32; 4] = [2048, 1024, 512, 256];

/// The photo surface's GPU-preview budget, the widget crate's `GPU_PREVIEW_BUDGET`, which every
/// slot, the source and a picture at rest's own parts are charged to: what the rest's share is
/// planned within. A test holds the two equal.
pub const GPU_PREVIEW_BYTES: u64 = 2 << 30;

/// The most a picture at rest's tile slot may take by the plan's own figures, whatever the budget
/// leaves beside the source, the view plan and the accumulator ([`RestTiles::share`]): 1.25 GiB,
/// which the 60 MP drag stack's 2048 px tile fits, so the rest stays within five eighths of the
/// GPU-preview budget wherever that budget leaves more.
pub const REST_SHARE_MAX: u64 = 5 << 28;

/// The most work one tile of a picture at rest may carry: its window's texels times the plan's
/// spatial links, about 24 MP·links, estimated at 50 to 70 ms of the M4's GPU at 2 to 4 ms a
/// megapixel a link. A side whose window would carry more steps down, so a gesture started while
/// the tiles are drawn waits behind at most one such tile.
pub const REST_TILE_WORK: u64 = 24_000_000;

/// The largest texture side on the editor's device, Iced's limit, which an output's size bucket
/// and a light link's tile follow.
const DEVICE_TEXTURE_SIDE: u32 = 8192;

/// A light link's tile side before its block rounds it down, the widget crate's `LIGHT_TILE`.
const LIGHT_TILE_SIDE: u32 = 2048;

/// The most a region's output reserves past its own size, the widget crate's
/// `REGION_BUCKET_BUDGET`.
const REGION_BUCKET_BYTES: u64 = 32 << 20;

/// The largest whole frame's output the widget crate reserves a square for, its `FULL_BUDGET`.
const FULL_BUCKET_BYTES: u64 = 512 << 20;

/// A slot's placement uniform, six `vec4<f32>`.
const UNIFORM_BYTES: u64 = luxforge_gpu_types::layout::PLACEMENT_BYTES as u64;

/// What a picture at rest's accumulator and its rest output take a view pixel: an `f32` sum of
/// four channels and the quantized codes.
const ACCUMULATOR_PIXEL_BYTES: u64 = 20;

/// The stack at full resolution in tiles of the output stage, each the whole stack's plan over its
/// tile at full scale from its own window of the source (`docs/design/gpu-preview.md`, "The
/// picture at rest"; `docs/design/gpu-first.md`, stage 2): what the histogram and clipping counts
/// are reduced from at every view, and at Fit and below 100% the picture at rest process-first,
/// the tiles reduced to the view's size by an area-weighted average of their linear light — the
/// frame the reference is held to at those views.
#[derive(Clone, Debug, PartialEq)]
pub struct RestTiles {
    /// The plan of the whole stack at the exact stage, from the source, reading its lights: every
    /// tile's plan, which a tile's region and window place.
    pub plan: Box<GpuPlan>,
    /// The tiles, each its rectangle of the output stage and the window of the source it reads,
    /// anchored ([`GpuPlan::anchor`]). They cover every pixel of the output stage once, in the
    /// order they are drawn: by the shape of their slot — the window's size and the rectangle's —
    /// each shape's tiles together and row by row, the shapes in the order a row-by-row walk first
    /// meets them, so the slot is refitted once a shape rather than wherever an edge tile falls
    /// between two of the middle's.
    pub tiles: Vec<RestTile>,
    /// What a tile's slot may take by the plan's own figures ([`rest_slot_bytes`] with its light
    /// links, [`rest_light_bytes`]): the GPU-preview budget less the source the surface holds, the
    /// view plan's slot and the accumulator with its rest output, at most [`REST_SHARE_MAX`].
    /// `None` for tiles of a side their caller named.
    pub share: Option<u64>,
    /// The same stack in staged sweeps within the same share, the stage textures charged in it, or
    /// why it is drawn chained ([`super::GpuStaging`]): the tiles above are the chained drawing,
    /// kept as the fallback, and a staged picture's last sweep is drawn, reduced and counted as
    /// they are.
    pub staging: super::GpuStaging,
    /// Where it reads a light behind a spatial layer and is drawn chained, the stage textures not
    /// fitting the share, the light sweeps that compute each such light first with no stage texture
    /// ([`super::GpuLightSweep`]), the chained tiles reading it kept; empty otherwise.
    pub light_sweeps: Vec<super::GpuLightSweep>,
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
    /// The view's size the output stage is reduced to: the frame a drag draws at the view's bounds,
    /// at its reduced stage, or, for a stack drawn at its exact stage, the output stage fitted to
    /// them.
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

/// What the photo surface's slot drawing `plan` over a boundary of `window` in `format` takes of the
/// GPU-preview budget, its output `output` pixels — a region's when `region`, a whole frame's
/// otherwise — by the plan's own figures, as the slot charges it before it creates anything (the
/// widget crate's `texture_charge` and `chain_charge`), with no device:
///
/// - the boundary, and an intermediate of its size and format for each link before the last, a
///   link starting at every spatial operation and one more before the first for the colour steps
///   ahead of it;
/// - a geometry tail's intermediate of the boundary's size: 8-bit codes where the tail clamps, `f32`
///   on the linear path, else half floats;
/// - the output in its size bucket, 4 bytes a pixel, and its placement uniform;
/// - each link's kept planes, the ones an apply reads, and its passes' parameter slices;
/// - the pool of scratch planes every link takes in turn, counted once: for each texture format and
///   plane size, the most any one link holds, and a texel for each light plane.
///
/// What it leaves out: the links' words and blocks buffers, kilobytes the device's limits size, and
/// the light links ([`rest_light_bytes`]). `O(planes)`.
pub fn rest_slot_bytes(
    plan: &GpuPlan,
    window: Region,
    format: crate::BoundaryFormat,
    output: (u32, u32),
    region: bool,
) -> u64 {
    let links = Links {
        spatial: 0..plan.spatial.len(),
        first: true,
        last: true,
    };
    links_bytes(plan, &links, window, format, output, region)
}

/// The links of a plan a slot runs: the plan's spatial operations `spatial`, each with the colour
/// operations after it; with the content operations before them when `first`, and the geometry
/// tail and output operations after them when `last`. A slot that runs the whole plan runs both; a
/// staged sweep's runs a part of it ([`super::GpuSweep`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Links {
    pub(crate) spatial: std::ops::Range<usize>,
    pub(crate) first: bool,
    pub(crate) last: bool,
}

/// [`rest_slot_bytes`] for the slot running `links` of `plan`. A slot that is not the last's
/// writes its last link's output as an intermediate, which a staged sweep copies into its stage
/// texture, rather than codes, so it holds an intermediate for every link and no tail; it still
/// holds an output, charged as the last's is. `O(planes)`.
pub(crate) fn links_bytes(
    plan: &GpuPlan,
    links: &Links,
    window: Region,
    format: crate::BoundaryFormat,
    output: (u32, u32),
    region: bool,
) -> u64 {
    let texels = u64::from(window.width) * u64::from(window.height);
    let (origin, size) = ((window.x0, window.y0), (window.width, window.height));
    let spatial = &plan.spatial[links.spatial.clone()];
    // The links the surface splits the steps into: one at each spatial step, and one before the
    // first for the colour steps a content operation gives.
    let ahead = links.first
        && plan
            .content
            .iter()
            .any(|operation| operation.mask.is_some() || !operation.units.is_empty());
    let count = match spatial.len() as u64 {
        0 => 1,
        spatial => spatial + u64::from(ahead),
    };
    // The boundary and an intermediate a link before the last, or a link each for a slot that
    // writes its last link's output as an intermediate.
    let textures = if links.last { count } else { count + 1 };
    let tail = if links.last && has_tail(plan) {
        texels
            * luxforge_gpu_types::layout::TailFormat::of(plan.geometry.clamps, plan.linear)
                .texel_bytes()
    } else {
        0
    };
    let output = if region {
        region_capacity(output)
    } else {
        full_capacity(output)
    };
    let output = u64::from(output.0) * u64::from(output.1) * 4 + UNIFORM_BYTES;
    // Each link's kept planes and parameters, and the most scratch planes of each class any link
    // holds: a class is the plane's texture format and its size.
    use luxforge_gpu_types::layout::{LinkLayout, PlaneClass, PlaneRole, PoolLayout};
    let mut kept = 0;
    let mut pool = PoolLayout::default();
    for spatial in spatial {
        let mut layout = LinkLayout::default();
        for (number, plane) in spatial.planes.iter().enumerate() {
            let role = if spatial.light == Some(number) {
                let Some(k) = plan.light_of(spatial.layer) else {
                    continue;
                };
                PlaneRole::Light(k as u32)
            } else {
                let class = PlaneClass::new(plane.format, plane.size);
                if spatial
                    .applies
                    .iter()
                    .any(|apply| apply.planes.contains(&number))
                {
                    PlaneRole::Kept(class)
                } else {
                    PlaneRole::Scratch(class)
                }
            };
            layout.push(role);
        }
        kept += layout.kept_bytes(origin, size, spatial.passes.len());
        pool.include(&layout);
    }
    let pool = pool.bytes(origin, size);
    texels * format.texel_bytes() as u64 * textures + tail + output + kept + pool
}

/// What the light links of `plan` take beside a slot whose boundary is held in `format`, by the
/// plan's own figures, as the photo surface charges them (the widget crate's `lights_charge`) but
/// for their buffers: each link's block plane, a `rgba32float` texel for each of the whole stage's
/// blocks, and the one tile texture of the source in `format` they cut their tiles into in turn,
/// the largest any of them needs. A link the surface cannot run is charged nothing. `O(lights)`.
pub fn rest_light_bytes(plan: &GpuPlan, format: crate::BoundaryFormat) -> u64 {
    lights_bytes(plan, 0..plan.spatial.len(), format)
}

/// [`rest_light_bytes`] for the light links the spatial operations `spatial` of `plan` read alone,
/// which a slot running those links runs before them. `O(lights)`.
pub(crate) fn lights_bytes(
    plan: &GpuPlan,
    spatial: std::ops::Range<usize>,
    format: crate::BoundaryFormat,
) -> u64 {
    let read = |light: &GpuLight| {
        plan.spatial[spatial.clone()]
            .iter()
            .any(|operation| operation.light.is_some() && operation.layer == light.layer)
    };
    let mut blocks = 0;
    let mut tile = 0;
    for light in plan.lights.iter().filter(|light| read(light)) {
        // A staged light's link reduces a stage texture as a link over the source reduces the
        // source, and its stand-in the same stage: one block plane and one tile, whichever runs.
        let link = light;
        let block = link
            .light
            .passes
            .first()
            .and_then(|pass| link.light.planes.get(pass.output))
            .and_then(|plane| match plane.size {
                super::GpuPlaneSize::Reduced(s) => Some(s.max(1)),
                super::GpuPlaneSize::Fixed { .. } => None,
            });
        let (Some(block), true) = (block, link.over_source() || link.staged()) else {
            continue;
        };
        let stage = (link.stage.width, link.stage.height);
        blocks += u64::from(stage.0.div_ceil(block)) * u64::from(stage.1.div_ceil(block)) * 16;
        let side = (LIGHT_TILE_SIDE.min(DEVICE_TEXTURE_SIDE) / block * block).max(block);
        tile = tile.max(
            u64::from(side.min(stage.0))
                * u64::from(side.min(stage.1))
                * format.texel_bytes() as u64,
        );
    }
    blocks + tile
}

/// Whether the surface runs `plan`'s geometry as a tail of its own: anything but an affine identity
/// onto the boundary's stage that reads all of it, clamps nothing and has no output operation after
/// it.
fn has_tail(plan: &GpuPlan) -> bool {
    let (geometry, stage) = (&plan.geometry, plan.boundary.stage);
    let output = geometry.output();
    let reads = geometry.reads;
    let identity = (output.width, output.height) == (stage.width, stage.height)
        && geometry.affine() == Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0])
        && !geometry.clamps
        && (reads.x0, reads.y0, reads.width, reads.height) == (0, 0, stage.width, stage.height);
    !(identity && plan.output.is_empty())
}

/// The texture a region's output of `size` pixels takes: two more pixels each way, in steps of 64,
/// so a pan's one-pixel change reuses it, where that is within the region bucket and the device's
/// side; its own size otherwise.
fn region_capacity(size: (u32, u32)) -> (u32, u32) {
    luxforge_gpu_types::layout::region_capacity(size, DEVICE_TEXTURE_SIDE, REGION_BUCKET_BYTES)
}

fn full_capacity(size: (u32, u32)) -> (u32, u32) {
    luxforge_gpu_types::layout::full_capacity(size, DEVICE_TEXTURE_SIDE, FULL_BUCKET_BYTES)
}

/// What `preview`'s slot and its light links take by its plan's own figures ([`rest_slot_bytes`],
/// [`rest_light_bytes`]): the view plan a picture at rest's share is planned beside. Zero for a
/// preview the GPU does not draw.
fn view_bytes(preview: &GpuPreview) -> u64 {
    let (GpuAnswer::Plan(plan), Some(boundary)) = (&preview.answer, &preview.boundary) else {
        return 0;
    };
    let stage = plan.boundary.stage;
    let window = boundary
        .window
        .or_else(|| {
            boundary.key.plan().map(|proxy| {
                let [x0, y0, width, height] = proxy.held();
                Region {
                    x0,
                    y0,
                    width,
                    height,
                }
            })
        })
        .unwrap_or(Region {
            x0: 0,
            y0: 0,
            width: stage.width,
            height: stage.height,
        });
    let (output, region) = match boundary.key.region() {
        Some(rect) => ((rect.width, rect.height), true),
        None => {
            let output = plan.geometry.output();
            ((output.width, output.height), false)
        }
    };
    rest_slot_bytes(plan, window, boundary.format, output, region)
        + rest_light_bytes(plan, boundary.format)
}

/// What the photo surface holds of `source` on the GPU: a JPEG's codes, 4 bytes a pixel, or a
/// developed RAW's three `f32` planes, 12.
fn source_bytes(source: &crate::PreviewSource) -> u64 {
    let (width, height) = source.dimensions();
    let pixel = match source {
        crate::PreviewSource::Jpeg(_) => 4,
        crate::PreviewSource::Raw { .. } => 12,
    };
    u64::from(width) * u64::from(height) * pixel
}

/// How a picture at rest's tiles are sized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RestSizing {
    /// The longest of [`REST_TILE_SIDES`] within the rest's share beside a view plan that takes
    /// these bytes ([`RestTiles::share`]).
    Beside(u64),
    /// The side a test or the release gate's harness names.
    #[cfg(any(test, feature = "qualification"))]
    Side(u32),
    /// The side a test names, a stack reading a staged light planned with light sweeps and no
    /// stage texture whatever fits ([`super::GpuLightSweep`]).
    #[cfg(any(test, feature = "qualification"))]
    StageFree(u32),
}

/// `tiles`, given row by row, in the order a picture at rest draws them: by their slot's shape,
/// the window's size and the rectangle's, each shape's tiles together in their row-by-row order,
/// the shapes in the order that walk first meets them. `O(tiles)`.
pub(crate) fn by_shape(tiles: Vec<RestTile>) -> Vec<RestTile> {
    let mut shapes = std::collections::HashMap::new();
    let mut keyed: Vec<(usize, RestTile)> = tiles
        .into_iter()
        .map(|tile| {
            let shape = (
                tile.window.width,
                tile.window.height,
                tile.rect.width,
                tile.rect.height,
            );
            let next = shapes.len();
            (*shapes.entry(shape).or_insert(next), tile)
        })
        .collect();
    // Stable, so each shape's tiles keep their row-by-row order.
    keyed.sort_by_key(|(shape, _)| *shape);
    keyed.into_iter().map(|(_, tile)| tile).collect()
}

/// The picture at rest of `evaluation` at Fit bounds `bounds`, process-first ([`RestTiles`]), its
/// tiles sized as `sizing` says: `None` when the bounds draw the output stage at its own size; the
/// reason when the GPU cannot draw the stack so, a stack the window planner cannot cut among them.
/// `O(tiles × segments)` on the catalog owner: one plan, a window per tile, no pixel read.
pub(crate) fn plan_rest_tiles(
    evaluation: &Evaluation,
    bounds: ProxyBounds,
    sizing: RestSizing,
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
    plan_tiles(evaluation, Some(view), sizing).map(Some)
}

/// The stack of `evaluation` at full resolution in tiles with no reduction to a view
/// ([`RestTiles`]), sized as `sizing` says: the tiles the histogram and clipping counts are reduced
/// from where the view draws the output stage at its own size or larger. The reason when the stack
/// cannot be drawn so now, as [`plan_rest_tiles`] names it. `O(tiles × segments)`, no pixel read.
pub(crate) fn plan_count_tiles(
    evaluation: &Evaluation,
    sizing: RestSizing,
) -> Result<Result<Box<RestTiles>, GpuFallback>, Error> {
    plan_tiles(evaluation, None, sizing)
}

/// The stack of `evaluation` at full resolution in tiles, reduced to `view` where it names one.
fn plan_tiles(
    evaluation: &Evaluation,
    view: Option<(u32, u32)>,
    sizing: RestSizing,
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
    let plan = match gpu_plan(registry, recipe, request)? {
        GpuAnswer::Plan(plan) => plan,
        GpuAnswer::Fallback(reason) => return Ok(Err(reason)),
    };
    let anchor = plan.anchor();
    let format = crate::BoundaryFormat::of(linear);
    let source = (full.width, full.height);
    let window_of = |rect: Region| {
        WindowPlan::of_gpu_rect(compiled, source, rect)
            .map(|windows| super::plan::anchored(windows.reads(0), anchor))
    };
    // What a tile's slot may take: the budget less the source the surface holds, the view plan's
    // slot and the accumulator with its rest output, at most half the budget.
    let share =
        match sizing {
            #[cfg(any(test, feature = "qualification"))]
            RestSizing::Side(_) | RestSizing::StageFree(_) => None,
            RestSizing::Beside(view_bytes) => {
                let accumulator = view.map_or(0, |(width, height)| {
                    u64::from(width) * u64::from(height) * ACCUMULATOR_PIXEL_BYTES
                });
                Some(REST_SHARE_MAX.min(
                    GPU_PREVIEW_BYTES.saturating_sub(
                        source_bytes(evaluation.source()) + view_bytes + accumulator,
                    ),
                ))
            }
        };
    // The longest side whose tile in the middle of the stage, its window grown on every side, the
    // share holds by the plan's own figures, its light links with it, and whose window carries at
    // most the work a tile may; a caller may name its own.
    let side = match sizing {
        #[cfg(any(test, feature = "qualification"))]
        RestSizing::Side(side) | RestSizing::StageFree(side) => side,
        RestSizing::Beside(_) => {
            let share = share.unwrap_or(0);
            let lights = rest_light_bytes(&plan, format);
            let links = plan.spatial.len().max(1) as u64;
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
                    window_of(middle).is_ok_and(|window| {
                        let slot = rest_slot_bytes(&plan, window, format, (width, height), true);
                        slot + lights <= share && window.pixels() * links <= REST_TILE_WORK
                    })
                })
                .unwrap_or(REST_TILE_SIDES[REST_TILE_SIDES.len() - 1])
        }
    };
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
    // The same stack in staged sweeps, each sweep's side chosen within the same share, or at the
    // side a caller names.
    let sides: &[u32] = match share {
        Some(_) => &REST_TILE_SIDES,
        None => std::slice::from_ref(&side),
    };
    let request = super::sweeps::SweepRequest {
        compiled,
        source,
        plan: &plan,
        format,
        sides,
        budget: share,
        order: super::sweeps::TileOrder::ByShape,
    };
    #[cfg_attr(not(any(test, feature = "qualification")), allow(unused_mut))]
    let mut staging = super::sweeps::plan_sweeps(&request);
    #[cfg(any(test, feature = "qualification"))]
    if let RestSizing::StageFree(_) = sizing
        && plan.lights.iter().any(GpuLight::staged)
    {
        staging = super::GpuStaging::Chained(super::Chained::OverBudget {
            needed: 0,
            budget: 0,
        });
    }
    // A light behind a spatial layer is computed from a staged sweep's stage texture, or where
    // those do not fit by light sweeps that hold none, the chained tiles reading it kept; a stack
    // neither fits is drawn by the reference, never with a stand-in.
    let mut light_sweeps = Vec::new();
    if let super::GpuStaging::Chained(_) = &staging
        && plan.lights.iter().any(GpuLight::staged)
    {
        match super::sweeps::plan_light_sweeps(&request) {
            Ok(sweeps) => light_sweeps = sweeps,
            Err(chained) => {
                return Ok(Err(
                    staged_light_fallback(&plan, &chained).expect("a staged light")
                ));
            }
        }
    }
    let staging = staging;
    // At rest a staged light is the exact one, never its stand-in.
    let mut plan = plan;
    for light in plan.lights.iter_mut().filter(|light| light.staged()) {
        light.stand_in = None;
    }
    Ok(Ok(Box::new(RestTiles {
        plan,
        tiles: by_shape(tiles),
        share,
        staging,
        light_sweeps,
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

/// Why `plan`, drawn chained for `chained`, cannot be drawn by the GPU: it reads a light whose
/// input only a staged sweep computes ([`super::GpuLightInput::Stage`]). `None` for a plan that
/// reads none. `O(lights)`.
pub(crate) fn staged_light_fallback(
    plan: &GpuPlan,
    chained: &super::Chained,
) -> Option<GpuFallback> {
    let light = plan.lights.iter().find(|light| light.staged())?;
    let why = match chained {
        super::Chained::OneSweep => "its links make one sweep".to_owned(),
        super::Chained::OverBudget { needed, budget } => format!(
            "its sweeps need {needed} bytes beside their stage textures, past the {budget} \
             available"
        ),
        super::Chained::Unplannable(reason) => reason.clone(),
    };
    Some(GpuFallback::LightStage {
        layer: light.layer,
        why,
    })
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
    let request = fit.request();
    let planned = planned_preview(evaluation, &fit, recipe, request, None, None)?;
    // Reduced to the view where it draws the stage smaller than it is; the counts' alone elsewhere.
    // Within the rest's share beside the view plan, whose slot stays held while the tiles are drawn.
    let sizing = RestSizing::Beside(view_bytes(&planned));
    let reduced = match view {
        GpuView::Fit(bounds) => plan_rest_tiles(evaluation, bounds, sizing)?,
        GpuView::Region { .. } => None,
    };
    let tiles = Some(match reduced {
        Some(tiles) => tiles,
        None => plan_count_tiles(evaluation, sizing)?,
    });
    Ok(GpuRest {
        view: planned,
        tiles,
    })
}

/// The plan of `planned` from the source at `fit`'s stage, with `request`, and the boundary it
/// starts from; over a region at 100% or more also the window the boundary will hold and, for a
/// drafted restoration or spatial layer at `spatial_drafted`, the plan of its CPU shape. For a drag
/// of the stack `committed`, the lights the drag draws with ([`drag_lights`]); otherwise the ones
/// the picture at rest draws with.
fn planned_preview(
    evaluation: &Evaluation,
    fit: &FitStage,
    planned: &Recipe,
    request: GpuPlanRequest,
    spatial_drafted: Option<usize>,
    committed: Option<&Recipe>,
) -> Result<GpuPreview, Error> {
    let registry = evaluation.registry();
    let recipe = evaluation.recipe();
    // At rest a staged light is the exact one or none: the view plan waits for the picture at
    // rest's sweeps to compute it, and never draws with its stand-in.
    let relit = |answer: GpuAnswer, request: GpuPlanRequest| -> Result<GpuAnswer, Error> {
        match (answer, committed) {
            (GpuAnswer::Plan(mut plan), Some(committed)) => {
                drag_lights(registry, committed, planned, request, &mut plan.lights)?;
                Ok(GpuAnswer::Plan(plan))
            }
            (GpuAnswer::Plan(mut plan), None) => {
                for light in plan.lights.iter_mut().filter(|light| light.staged()) {
                    light.stand_in = None;
                }
                Ok(GpuAnswer::Plan(plan))
            }
            (answer, _) => Ok(answer),
        }
    };
    let mut answer = relit(gpu_plan(registry, planned, request)?, request)?;
    // From the source, the first segment's input before its first operation: no layer before the
    // boundary, whose texels are the source's.
    let position = (0, 0);
    let before = 0;
    // Over a region at 100% or more, the window of the received stage the boundary will hold,
    // planned now so what it takes is known before it is rendered. A light is the whole stage's,
    // whatever window the plan draws over, so it bears on no window.
    let mut window = None;
    // At the exact stage at Fit, the window the whole output stage reads, so a crop's boundary
    // holds what the crop reads rather than its whole source; a stack the planner cannot cut keeps
    // the whole stage. The window is planned as the boundary's render plans it
    // (`Render::output_boundary`).
    if let (GpuAnswer::Plan(_), None, None) = (&answer, fit.plan, fit.region) {
        window = output_window(&fit.compiled, fit.full, position.0);
    }
    if let (GpuAnswer::Plan(_), Some((rect, _))) = (&answer, fit.region) {
        let full = (fit.full.width, fit.full.height);
        answer = match WindowPlan::of_gpu_rect(&fit.compiled, full, rect) {
            Ok(windows) => {
                window = Some(windows.reads(position.0));
                answer
            }
            Err(reason) => GpuAnswer::Fallback(GpuFallback::Unplannable(format!(
                "the region's boundary: {}",
                reason.reason()
            ))),
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
        if let GpuAnswer::Plan(mut smaller) = gpu_plan(registry, planned, unshaped)?
            && extent(&smaller) != extent(plan)
        {
            // The drag's lights, whichever shape draws it.
            smaller.lights = plan.lights.clone();
            cpu_shape = Some(smaller);
        }
    }
    // Every window is anchored, its origin a multiple of the plan's anchor, so each texel it holds
    // is the whole stage's, bit for bit, whichever window holds it ([`GpuPlan::anchor`]).
    let mut reduced = fit.plan;
    if let GpuAnswer::Plan(plan) = &answer {
        let anchor =
            super::plan::common_anchor(std::iter::once(&**plan).chain(cpu_shape.as_deref()));
        window = window.map(|window| super::plan::anchored(window, anchor));
        reduced = reduced.map(|reduced| super::fit::anchored_plan(reduced, anchor));
    }
    // Every plan starts from the source.
    let boundary_request = match &answer {
        GpuAnswer::Fallback(_) => None,
        GpuAnswer::Plan(plan) => Some(SourceBoundary {
            key: BoundaryKey {
                source: evaluation.source().identity(),
                prefix: prefix_hash(&recipe.layers[..before], &recipe.masks, fit.sampling())?,
                layer: before,
                plan: reduced,
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
        reduced: None,
    })
}

/// The most links one plan's chain holds, and the light link it computes its light with: the colour
/// operations before its first spatial operation, then one link for each spatial operation — a
/// global Detail layer, a global Presence layer and each masked spatial layer
/// ([`MAX_MASKED_SPATIAL_LAYERS`]) — and the sequence of a light link over the source
/// ([`warm_links`]).
pub const GPU_PLAN_LINKS: usize = 4 + MAX_MASKED_SPATIAL_LAYERS;

/// The most link sequences a warm list holds ([`plan_warm_list`]): the surface's 64-sequence pipeline
/// cache (`PIPELINE_CACHE`, `crates/luxforge-ui/src/photo_surface/gpu_preview/compile.rs`) less
/// the largest plan's [`GPU_PLAN_LINKS`], so a whole warm list and every link of the plan a drag
/// draws are held together, and warming never evicts what a tick asks for or what it has just
/// warmed. The stack's own plan and a drag of every restoration and spatial layer take at most
/// `2 × GPU_PLAN_LINKS − 1` of them (each such drag adds its drafted link at most), so they always
/// fit; their light links, the colour drags and the first drags of the modules the stack does not
/// hold fill the rest.
pub const GPU_WARM_LINKS: usize = 44;

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
/// key compile to one sequence, wherever they sit in a chain and whichever plan holds them. Each
/// light link the slot computes itself is a sequence after them ([`light_links`]).
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
    for (_, link) in light_links(plan) {
        if !links.contains(&link) {
            links.push(link);
        }
    }
    links
}

/// Each light link of `plan` the slot computes itself — one over the source, or the stand-in of
/// one that is not — with the light plane `k` it writes and what its sequence is told apart by, as
/// [`warm_links`] tells a link apart: its colour operations and its light step, once however many
/// of the plan's lights compile alike.
fn light_links(plan: &GpuPlan) -> Vec<((usize, &GpuLight), Vec<String>)> {
    let mut links: Vec<((usize, &GpuLight), Vec<String>)> = Vec::new();
    for (k, light) in plan.lights.iter().enumerate() {
        let Some(light) = (match light.over_source() {
            true => Some(light),
            false => light.stand_in.as_deref(),
        }) else {
            continue;
        };
        let link = light_link(light);
        if !links.iter().any(|(_, held)| *held == link) {
            links.push(((k, light), link));
        }
    }
    links
}

/// What the sequence of light link `light` is told apart by ([`light_links`]): not the light plane
/// it writes, which the surface binds when it runs it.
pub(crate) fn light_link(light: &GpuLight) -> Vec<String> {
    let mut link = vec![format!("light link, clamps {}", light.light.clamps)];
    link.extend(light.content.iter().flat_map(|operation| {
        operation
            .units
            .iter()
            .map(|unit| unit.program.entry.to_owned())
            .chain(operation.mask.as_ref().map(|mask| {
                let components: Vec<&str> = mask
                    .components
                    .iter()
                    .map(|component| component.program.program.entry)
                    .collect();
                format!("masked by {components:?}")
            }))
    }));
    link.extend(light.light.passes.iter().map(|pass| {
        format!(
            "{} {:?} -> {}, {:?}",
            pass.kernel, pass.inputs, pass.output, pass.shape
        )
    }));
    link
}

/// The plans a gesture on a stack is likely to draw and the light links its ticks compute, for the
/// desktop to warm before a drag begins ([`plan_warm_list`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GpuWarmList {
    /// The plans whose chains' links the list warms, in the order they are to compile.
    pub plans: Vec<GpuPlan>,
    /// The light links the list warms, each with the slot's light plane `k` it writes: those of
    /// the open stack's plans that fit within [`GPU_WARM_LINKS`] beside every link of their
    /// chains.
    pub lights: Vec<(usize, GpuLight)>,
    /// How many of [`Self::plans`], from the first, are drags of the open stack itself; the rest
    /// are the first drags of the modules it does not hold, which the surface compiles after them
    /// and after the light links.
    pub open: usize,
}

/// The plans a gesture on `evaluation`'s stack is likely to draw at `view`, for the desktop to
/// warm their program sequences before a drag begins, in the order they are to compile, the light
/// links their ticks compute, and how many of the plans, from the first, are the open stack's
/// ([`GpuWarmList`]). The open stack's first: when a layer is masked, the stack's own plan, which a
/// stroke or a shape moved draws; a drag of each colour or finish layer the stack holds; then a
/// drag of each restoration or spatial layer it holds. Then the rest of the program set: the first
/// drag of each patch module that applies to the photo's kind and that the stack does not hold
/// yet, over the neutral layer its module names, colour and finish modules first,
/// then restoration and spatial ones, Detail and Presence, whose sequences take the longest to
/// compile on a cold shader cache. Each drag is planned with its layer in its GPU shape and with
/// the lights a draft of it draws with ([`plan_preview`], [`drag_lights`]): a colour drag computes
/// the light of every estimating layer after it over the source.
///
/// The surface compiles one sequence per link of a plan's chain, so a plan joins only when its
/// chain holds a link no plan before it holds ([`warm_links`]). A restoration or spatial layer's
/// drag draws the stack's own links but its drafted one, so the list holds one drag for each
/// distinct drafted shape, and the drag of every such layer finds each link it draws warmed.
/// Beside the chains, the light links the open stack's ticks compute ([`GpuWarmList::lights`]),
/// each while it fits. The list holds at most [`GPU_WARM_LINKS`] links: the stack's own plan and
/// those drags' chains always fit, then their light links, the colour drags of the stack's layers
/// join with theirs while they leave room for them, and the first drags of the modules it does not
/// hold fill what is left. `O(layers × modules)` compiles on the catalog owner, with no pixel
/// read.
pub(crate) fn plan_warm_list(evaluation: &Evaluation, view: GpuView) -> Result<GpuWarmList, Error> {
    let registry = evaluation.registry();
    let recipe = evaluation.recipe();
    let fit = FitStage::of_view(evaluation, view)?;
    let stage_is = |effect: &str, stages: &[EffectStage]| {
        registry
            .effect_stage(effect)
            .is_some_and(|stage| stages.contains(&stage))
    };
    const COLOUR: &[EffectStage] = &[EffectStage::Color, EffectStage::Finish];
    const SPATIAL: &[EffectStage] = &[EffectStage::Restoration, EffectStage::Spatial];
    // A module that does not apply to the photo's kind (the look on a JPEG) is never dragged.
    let kind = match evaluation.source() {
        crate::PreviewSource::Raw { .. } => SourceTag::Raw,
        crate::PreviewSource::Jpeg(_) => SourceTag::Jpeg,
    };
    // The first drag of each field-patch module of `stages` the stack does not hold unmasked: the
    // stack it is planned over, with the neutral layer its first commit would insert, and where.
    let firsts = |stages: &[EffectStage]| -> Vec<(Recipe, usize)> {
        let mut firsts = Vec::new();
        for provider in registry.providers() {
            let descriptor = provider.descriptor();
            let [effect] = descriptor.effects.as_slice() else {
                continue;
            };
            if descriptor.developer
                || !descriptor.applies_to(kind)
                || !stage_is(&effect.id, stages)
                || !descriptor.actions.iter().any(|action| action.patch)
                || recipe
                    .layers
                    .iter()
                    .any(|layer| layer.effect_id == effect.id && layer.mask.is_none())
            {
                continue;
            }
            let index = registry.insertion_index_for_target(
                &recipe.layers,
                &effect.id,
                None,
                &recipe.masks,
            );
            firsts.push((
                with_neutral(registry, recipe, index, &effect.id, None),
                index,
            ));
        }
        firsts
    };
    // Every link a plan the list holds compiles, and the links of `plan` it would add.
    let mut links: Vec<Vec<String>> = Vec::new();
    let fresh = |plan: &GpuPlan, held: &[Vec<String>]| {
        let chain = warm_links(plan).len() - light_links(plan).len();
        let mut fresh: Vec<Vec<String>> = Vec::new();
        for link in warm_links(plan).into_iter().take(chain) {
            if !held.contains(&link) && !fresh.contains(&link) {
                fresh.push(link);
            }
        }
        fresh
    };
    // `plan` joins `into` when it adds a link and the list has room for what it adds.
    let join = |plan: GpuPlan, into: &mut Vec<GpuPlan>, links: &mut Vec<Vec<String>>| {
        let added = fresh(&plan, links);
        if !added.is_empty() && links.len() + added.len() <= GPU_WARM_LINKS {
            links.extend(added);
            into.push(plan);
        }
    };
    let mut plans: Vec<GpuPlan> = Vec::new();
    // Every drag is planned from the source ([`plan_preview`]), and so is a gesture of a mask the
    // stack's layers already read: a stroke, or a shape moved, draws the committed stack itself,
    // every layer in the CPU's shape.
    if recipe.layers.iter().any(|layer| layer.mask.is_some())
        && let GpuAnswer::Plan(plan) = gpu_plan(registry, recipe, fit.request())?
    {
        links = fresh(&plan, &links);
        plans.push(*plan);
    }
    // A drag's plan with the lights it draws with.
    let dragged = |planned: &Recipe, request: GpuPlanRequest| -> Result<GpuAnswer, Error> {
        match gpu_plan(registry, planned, request)? {
            GpuAnswer::Plan(mut plan) => {
                drag_lights(registry, recipe, planned, request, &mut plan.lights)?;
                Ok(GpuAnswer::Plan(plan))
            }
            fallback => Ok(fallback),
        }
    };
    // The restoration and spatial layers' drags are chosen first, by their chains' links, so the
    // colour drags leave room for them, and join the list after the colour drags.
    let mut drags: Vec<GpuPlan> = Vec::new();
    for (index, _) in recipe
        .layers
        .iter()
        .enumerate()
        .filter(|(_, layer)| stage_is(&layer.effect_id, SPATIAL))
    {
        let request = fit.request().drafted(index);
        if let GpuAnswer::Plan(plan) = dragged(recipe, request)? {
            join(*plan, &mut drags, &mut links);
        }
    }
    // The light links of the stack's own plan and of those drags, each while the list has room: a
    // drag whose light link does not fit compiles it on its first tick.
    let mut lights: Vec<(usize, GpuLight)> = Vec::new();
    let mut admit = |plan: &GpuPlan, links: &mut Vec<Vec<String>>| {
        for ((k, light), link) in light_links(plan) {
            if !links.contains(&link) && links.len() < GPU_WARM_LINKS {
                links.push(link);
                lights.push((k, light.clone()));
            }
        }
    };
    for plan in plans.iter().chain(&drags) {
        admit(plan, &mut links);
    }
    // On a RAW, a white-balance drag: the stack's own plan with the change as a leading step over
    // the source, the identity standing for every value since a sequence is told apart by its
    // programs, and every light computed per tick over the source it changes.
    if let crate::PreviewSource::Raw { .. } = evaluation.source()
        && let Some(raw) = recipe
            .layers
            .iter()
            .position(|layer| registry.effect_stage(&layer.effect_id) == Some(EffectStage::Source))
        && let GpuAnswer::Plan(mut plan) = gpu_plan(registry, recipe, fit.request())?
    {
        if plan.reads_lights() {
            let left_out = gpu_lights(
                registry,
                recipe,
                super::spatial::light_request(fit.request()),
                GpuLightRestoration::LeftOut,
            )?;
            for light in &mut plan.lights {
                if let Some(found) = left_out.iter().find(|found| found.layer == light.layer) {
                    *light = found.clone();
                }
            }
        }
        let identity = crate::WhiteBalanceApproximation::from_matrix([
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ])?;
        with_white_balance(&mut plan, &identity, raw);
        let held = plans.len();
        join(*plan, &mut plans, &mut links);
        if plans.len() > held {
            admit(&plans[held], &mut links);
        }
    }
    // A drag of a colour layer changes the input of every estimating layer after it, so its ticks
    // compute those lights over the source: each joins with its light links while they fit.
    for (index, _) in recipe
        .layers
        .iter()
        .enumerate()
        .filter(|(_, layer)| stage_is(&layer.effect_id, COLOUR))
    {
        let request = fit.request().drafted(index);
        if let GpuAnswer::Plan(plan) = dragged(recipe, request)? {
            let held = plans.len();
            join(*plan, &mut plans, &mut links);
            if plans.len() > held {
                admit(&plans[held], &mut links);
            }
        }
    }
    plans.extend(drags);
    let open = plans.len();
    // The rest of the program set, each module's first drag planned as its draft is. Their light
    // links are left to their first ticks: the open stack's come first.
    for (planned, index) in firsts(COLOUR).into_iter().chain(firsts(SPATIAL)) {
        let request = fit.request().drafted(index);
        if let GpuAnswer::Plan(plan) = dragged(&planned, request)? {
            join(*plan, &mut plans, &mut links);
        }
    }
    Ok(GpuWarmList {
        plans,
        lights,
        open,
    })
}

/// [`plan_warm_list`]'s plans alone.
#[cfg(test)]
pub(crate) fn plan_warm(evaluation: &Evaluation, view: GpuView) -> Result<Vec<GpuPlan>, Error> {
    plan_warm_list(evaluation, view).map(|warm| warm.plans)
}
