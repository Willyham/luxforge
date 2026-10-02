//! A draft's GPU preview, planned with its preview job on the catalog owner
//! (`docs/design/gpu-preview.md`, "A tick"): the plan the photo surface draws a tick from, and
//! the boundary it starts from, which the preview worker renders once per draft.
//!
//! Planning reads the job's own evaluation — the draft's effective recipe beside the entry it was
//! planned over, and the stack's one compilation — and compiles at the stage the Fit frame is
//! drawn at: `O(layers)`, no pixel read, nothing that scales with the image.
//!
//! - **The boundary layer** is the earliest layer the draft changes: the first that differs from
//!   the entry's own stack or reads a mask the draft changes, the layer a module action drafts
//!   (where it is, or where its first commit would put it), and for a `mask.*` gesture the first
//!   layer the drafted mask modulates.
//! - **A stable shape.** A field-patch module compiles only its non-neutral units, so the drafted
//!   layer is planned in its GPU shape, every unit present and a neutral one as its identity
//!   ([`super::GpuPlanRequest::drafted`]); a drafted layer the stack does not hold yet, because
//!   its value is still neutral, is planned as the neutral layer its first commit would insert.
//!   A drag that leaves or returns to neutral therefore keeps one program sequence. The CPU
//!   compile is untouched: only this plan asks for the shape.
//! - **Stored estimates.** A spatial operation's global estimate, Dehaze's atmospheric light, is
//!   read from the estimate store the job's CPU frames fill, under the key a Fit frame of the same
//!   content asks with: the proxy is named by its source and plan, never built. A plan whose key
//!   the store does not hold yet takes the estimate on the GPU and says it is approximate.
//! - **The boundary key** names everything the boundary's texels depend on: the source's identity
//!   (fingerprint, development and view), the layers before the boundary and the masks they read,
//!   the boundary's index, and the proxy plan with its window, or at a percentage zoom the region
//!   of the output stage the boundary is held for. Equal keys hold equal texels.
//! - **Where it is drawn** ([`GpuView`]): at Fit, the stage the job's Fit frame is drawn at; at a
//!   percentage zoom of 100% or more, the exact stage, over the visible region at full scale, whose
//!   boundary is the window of the boundary layer's received stage that region reads.
use super::{
    EstimateSource, GpuAnswer, GpuEstimates, GpuFallback, GpuPlan, GpuPlanRequest, gpu_plan_with,
};
use crate::{
    Draft, EFFECT_FORMAT, EffectStage, Error, Evaluation, Layer, LayerId, MaskId, ModuleRegistry,
    ProxyBounds, ProxyIdentity, ProxyPlan, Recipe,
    mask_field::MaskSampling,
    modules::{Region, Stage},
    render::{Compiled, spatial::prefix_hash},
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
    /// The proxy the boundary is rendered at, with the window it holds; `None` at the exact stage,
    /// where a photograph that fits the display is drawn, and at a percentage zoom.
    plan: Option<ProxyPlan>,
    /// At a percentage zoom, the rectangle of the output stage the boundary is held for.
    region: Option<Region>,
}

impl BoundaryKey {
    /// The proxy the boundary is rendered at, with its window; `None` at the exact stage.
    pub fn plan(&self) -> Option<ProxyPlan> {
        self.plan
    }

    /// The boundary layer's index in the drafted stack.
    pub fn layer(&self) -> usize {
        self.layer
    }

    /// At a percentage zoom, the rectangle of the output stage the boundary is held for, which the
    /// plan's frame covers; `None` at Fit.
    pub fn region(&self) -> Option<Region> {
        self.region
    }
}

/// Where a draft's GPU preview is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GpuView {
    /// At Fit, in these display bounds: the stage the job's Fit frame is drawn at.
    Fit(ProxyBounds),
    /// At a percentage zoom of 100% or more: `rect` of the output stage at full scale, the visible
    /// region, drawn at `magnification` physical pixels an output pixel.
    Region { rect: Region, magnification: f64 },
}

/// The boundary a draft's GPU preview starts from: its key, and where its layer begins in the
/// compilation the preview worker renders the job's Fit frame from.
#[derive(Clone, Debug, PartialEq)]
pub struct BoundaryRequest {
    pub key: BoundaryKey,
    /// The segment and operation index the boundary layer begins at in the drafted stack's
    /// compilation at the boundary's stage.
    pub(crate) position: (usize, usize),
    /// A lens or perspective warp's geometry tail, whose coordinate grid the worker computes with
    /// the boundary, once per draft and off the interface thread; `None` for an affine tail.
    pub(crate) warp: Option<super::GpuGeometry>,
    /// How the boundary's texels are held: `f32` for a plan of the linear path, half floats
    /// otherwise.
    pub format: crate::BoundaryFormat,
    /// Physical pixels an output pixel is drawn at, which a warp's coordinate grid is made dense
    /// enough for: one at Fit, the zoom at a percentage.
    pub(crate) magnification: f64,
    /// At a percentage zoom, the window of the boundary layer's received stage the region reads,
    /// which the boundary holds: the region and every margin after it, so what the boundary and
    /// a slot drawing over it take is known before it is rendered.
    pub window: Option<Region>,
}

/// A draft's GPU preview: the plan a tick is drawn from, or why the gesture takes the CPU path,
/// and the boundary the plan starts from.
#[derive(Clone, Debug)]
pub struct GpuPreview {
    pub answer: GpuAnswer,
    /// The boundary of [`Self::answer`]'s plan; `None` when there is no plan.
    pub boundary: Option<BoundaryRequest>,
    /// At a percentage zoom, when [`Self::answer`]'s plan holds a drafted restoration or spatial
    /// layer in its GPU shape: the plan of that layer in the CPU's shape, the units its values
    /// need, from the same boundary. Over the region's window the GPU shape charges the planes of
    /// its units at zero too, so the desktop draws this one when only it fits the budget.
    pub cpu_shape: Option<Box<GpuPlan>>,
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

/// The stage a job's Fit frame is drawn at, planned exactly as the preview worker plans the job's
/// proxy phase: from the output stage of the stack's one compilation, with the window of the proxy
/// stage a crop reads. A stack that fits the bounds, or has no proxy, is drawn at its exact stage.
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
    /// At a percentage zoom, the region of the output stage drawn, at this magnification.
    region: Option<(Region, f64)>,
}

impl FitStage {
    /// The stage `view` is drawn at: the Fit frame's for [`GpuView::Fit`], the exact stage over
    /// the visible region for [`GpuView::Region`].
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

    /// A plan request from `boundary` at this stage, on the source's path.
    fn request(&self, boundary: usize) -> GpuPlanRequest {
        let request = match self.plan {
            Some(_) => GpuPlanRequest::fit(boundary, self.stage, self.full),
            None => GpuPlanRequest::exact(boundary, self.full),
        };
        if self.linear {
            request.linear()
        } else {
            request
        }
    }

    /// Where `evaluation`'s CPU frames at this stage keep their global estimates: its context's
    /// store, under its proxy at this plan, or under its source at the exact stage.
    fn estimates<'a>(&self, evaluation: &'a Evaluation) -> GpuEstimates<'a> {
        GpuEstimates {
            context: evaluation.context(),
            source: match self.plan {
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

/// The GPU preview of `evaluation`, an open draft's preview job's evaluation, drawn as `view`
/// says: the plan from the earliest layer the draft changes, at the stage the job's Fit frame is
/// drawn at or the exact stage at a percentage zoom, and the boundary it starts from. `O(layers)`
/// on the catalog owner: one compile at the proxy stage for the window, one for the plan, a lookup
/// in the estimate store for each global estimate, and no pixel read.
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
    let Some(boundary) = [
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
        });
    };
    let fit = FitStage::of_view(evaluation, view)?;
    // The drafted layer in its GPU shape, inserted as the neutral layer its first commit would
    // add when the stack does not hold it yet.
    let request = fit.request(boundary);
    let (planned, request) = match &drafted_layer {
        Some(drafted) if !drafted.held && drafted.index == boundary => (
            with_neutral(recipe, drafted.index, &drafted.effect, drafted.mask.clone()),
            request.drafted(drafted.index),
        ),
        Some(drafted) if drafted.held => (recipe.clone(), request.drafted(drafted.index)),
        _ => (recipe.clone(), request),
    };
    let mut answer = gpu_plan_with(registry, &planned, request, Some(fit.estimates(evaluation)))?;
    let position = position(&fit.compiled, boundary).ok_or_else(|| {
        Error::internal(format!(
            "the GPU preview's boundary layer {boundary} is past the stack"
        ))
    })?;
    // At a percentage zoom, the window of the received stage the boundary will hold, planned now
    // so what it takes is known before it is rendered; and a global estimate the store does not
    // hold yet, which the GPU would take from the region alone where the exact visible region
    // reads the whole stage's, keeps the drag on the CPU.
    let mut window = None;
    if let (GpuAnswer::Plan(plan), Some((rect, _))) = (&answer, fit.region) {
        let full = (fit.full.width, fit.full.height);
        answer = match plan.spatial.as_ref().filter(|spatial| spatial.estimated) {
            Some(spatial) => GpuAnswer::Fallback(GpuFallback::RegionEstimate {
                layer: spatial.layer,
            }),
            None => match crate::render::window::WindowPlan::of_rect(&fit.compiled, full, rect) {
                Ok(windows) => {
                    window = Some(windows.received(position.0));
                    answer
                }
                Err(reason) => GpuAnswer::Fallback(GpuFallback::Unplannable(format!(
                    "the region's boundary: {}",
                    reason.reason()
                ))),
            },
        };
    }
    // At a percentage zoom a drafted restoration or spatial layer's GPU shape charges the planes
    // of its units at zero over the window too: the plan of its CPU shape rides beside it, from
    // the same boundary, when it holds less.
    let spatial = |effect: &str| {
        matches!(
            registry.effect_stage(effect),
            Some(EffectStage::Restoration | EffectStage::Spatial)
        )
    };
    let mut cpu_shape = None;
    if let (GpuAnswer::Plan(plan), Some(_), Some(drafted)) = (&answer, fit.region, &drafted_layer)
        && request.drafted.is_some()
        && spatial(&drafted.effect)
    {
        let unshaped = GpuPlanRequest {
            drafted: None,
            ..request
        };
        let extent = |plan: &GpuPlan| {
            plan.spatial
                .as_ref()
                .map(|spatial| (spatial.passes.len(), spatial.applies.len()))
        };
        if let GpuAnswer::Plan(smaller) = gpu_plan_with(
            registry,
            &planned,
            unshaped,
            Some(fit.estimates(evaluation)),
        )? && extent(&smaller) != extent(plan)
            && !smaller.approximate()
        {
            cpu_shape = Some(smaller);
        }
    }
    let boundary_request = match &answer {
        GpuAnswer::Fallback(_) => None,
        GpuAnswer::Plan(plan) => Some(BoundaryRequest {
            key: BoundaryKey {
                source: evaluation.source().identity(),
                prefix: prefix_hash(&recipe.layers[..boundary], &recipe.masks, fit.sampling())?,
                layer: boundary,
                plan: fit.plan,
                region: fit.region.map(|(rect, _)| rect),
            },
            position,
            format: crate::BoundaryFormat::of(fit.linear),
            magnification: fit.region.map_or(1.0, |(_, magnification)| magnification),
            window,
            warp: plan
                .geometry
                .affine()
                .is_none()
                .then(|| plan.geometry.clone()),
        }),
    };
    Ok(GpuPreview {
        answer,
        boundary: boundary_request,
        cpu_shape,
    })
}

/// How many restoration and spatial layers a warm list warms a drag of, in stack order: one plan
/// each, the layer in its GPU shape, which is the one sequence every drag of it draws whatever its
/// values (every unit is held, a neutral one as its identity) and whatever the estimate store holds
/// (Dehaze takes the same passes for a stored light as for one it finds). A Presence or Detail
/// sequence is the slowest to compile (up to two seconds cold on the M4); with the colour
/// candidates — a handful of layers and modules — four keeps the list inside the surface's
/// 16-sequence pipeline cache, so warming never evicts what it has just warmed. A layer whose drag
/// takes the CPU path counts against none.
const WARM_SPATIAL_PLANS: usize = 4;

/// What a warmed plan's program sequence is told apart by, as the surface keys a sequence or
/// finer: each colour operation's programs and its mask's components; the spatial operation's
/// program, clamp, mask, planes, passes but for their words, and applies; and the geometry tail's
/// kind. Two plans of one key compile to one sequence, so a warm list holds one of them.
pub(crate) fn warm_sequence(plan: &GpuPlan) -> Vec<String> {
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
    let mut keys = colour(&plan.content);
    if let Some(spatial) = &plan.spatial {
        keys.push(format!(
            "{}, clamps {}",
            spatial.program.entry, spatial.clamps
        ));
        keys.extend(mask(&spatial.mask));
        keys.extend(spatial.planes.iter().map(|plane| format!("{plane:?}")));
        keys.extend(spatial.passes.iter().map(|pass| {
            format!(
                "{} {:?} -> {}, source {}, {:?}",
                pass.kernel, pass.inputs, pass.output, pass.source, pass.shape
            )
        }));
        keys.extend(spatial.applies.iter().map(|apply| format!("{apply:?}")));
    }
    keys.push(format!(
        "geometry affine {}, clamps {}",
        plan.geometry.affine().is_some(),
        plan.geometry.clamps
    ));
    keys.extend(colour(&plan.output));
    keys
}

/// The plans a gesture on `evaluation`'s stack is likely to draw at `bounds`, for the desktop to
/// warm their program sequences before a drag begins: a drag of each colour or finish layer the
/// stack holds, and the first drag of each field-patch colour or finish module it does not hold
/// yet; then a drag of each restoration or spatial layer the stack holds, at most
/// [`WARM_SPATIAL_PLANS`] of them, reading the estimates the store holds, which a drag of a colour
/// layer before them never does. Each is planned from that layer with the layer in its GPU shape,
/// as a draft of it is planned ([`plan_preview`]). One plan per program sequence. `O(layers ×
/// modules)` compiles on the catalog owner, with no pixel read.
pub(crate) fn plan_warm(
    evaluation: &Evaluation,
    bounds: ProxyBounds,
) -> Result<Vec<GpuPlan>, Error> {
    let registry = evaluation.registry();
    let recipe = evaluation.recipe();
    let fit = FitStage::of(evaluation, bounds)?;
    let colour = |effect: &str| {
        matches!(
            registry.effect_stage(effect),
            Some(EffectStage::Color | EffectStage::Finish)
        )
    };
    // Each candidate: the stack it is planned over, its boundary, and whether it is a drag of a
    // restoration or spatial layer.
    let mut candidates: Vec<(Recipe, usize, bool)> = recipe
        .layers
        .iter()
        .enumerate()
        .filter(|(_, layer)| colour(&layer.effect_id))
        .map(|(index, _)| (recipe.clone(), index, false))
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
        candidates.push((with_neutral(recipe, index, &effect.id, None), index, false));
    }
    candidates.extend(
        recipe
            .layers
            .iter()
            .enumerate()
            .filter(|(_, layer)| {
                matches!(
                    registry.effect_stage(&layer.effect_id),
                    Some(EffectStage::Restoration | EffectStage::Spatial)
                )
            })
            .map(|(index, _)| (recipe.clone(), index, true)),
    );
    let mut plans: Vec<GpuPlan> = Vec::new();
    let mut seen: Vec<Vec<String>> = Vec::new();
    let mut spatial = 0;
    for (planned, index, own) in candidates {
        if own && spatial == WARM_SPATIAL_PLANS {
            break;
        }
        // A drag of a colour layer changes the input of every spatial layer after it, so its
        // ticks take their estimates on the GPU; a spatial layer's own drag leaves the layers
        // before it alone, so its ticks read the store, as the warmed plan does.
        let estimates = own.then(|| fit.estimates(evaluation));
        let request = fit.request(index).drafted(index);
        if let GpuAnswer::Plan(plan) = gpu_plan_with(registry, &planned, request, estimates)? {
            spatial += usize::from(own);
            let sequence = warm_sequence(&plan);
            if !seen.contains(&sequence) {
                seen.push(sequence);
                plans.push(*plan);
            }
        }
    }
    Ok(plans)
}
