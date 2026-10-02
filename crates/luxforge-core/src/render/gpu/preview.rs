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
//! - **The boundary key** names everything the boundary's texels depend on: the source's identity
//!   (fingerprint, development and view), the layers before the boundary and the masks they read,
//!   the boundary's index, and the proxy plan with its window. Equal keys hold equal texels.
use super::{GpuAnswer, GpuFallback, GpuPlan, GpuPlanRequest, gpu_plan};
use crate::{
    Draft, EFFECT_FORMAT, EffectStage, Error, Evaluation, Layer, LayerId, MaskId, ModuleRegistry,
    ProxyBounds, ProxyIdentity, ProxyPlan, Recipe,
    mask_field::MaskSampling,
    modules::Stage,
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
    /// where a photograph that fits the display is drawn.
    plan: Option<ProxyPlan>,
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
}

/// A draft's GPU preview: the plan a tick is drawn from, or why the gesture takes the CPU path,
/// and the boundary the plan starts from.
#[derive(Clone, Debug)]
pub struct GpuPreview {
    pub answer: GpuAnswer,
    /// The boundary of [`Self::answer`]'s plan; `None` when there is no plan.
    pub boundary: Option<BoundaryRequest>,
}

/// The layer a module action drafts, and whether the stack holds it yet.
struct Drafted {
    index: usize,
    held: bool,
    effect: String,
    mask: Option<MaskId>,
}

/// The layer `draft`'s action drafts in `recipe`: the one layer of its field-patch module's colour
/// or finish effect for the draft's mask target, or where that layer's first commit would join the
/// stack. `None` for any other action, whose changes the stack comparison finds.
fn drafted(registry: &ModuleRegistry, recipe: &Recipe, draft: &Draft) -> Option<Drafted> {
    let (provider, action) = registry.action(&draft.action)?;
    if !action.patch {
        return None;
    }
    let [effect] = provider.descriptor().effects.as_slice() else {
        return None;
    };
    if !matches!(effect.stage, EffectStage::Color | EffectStage::Finish) {
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
fn position(compiled: &Compiled, layer: usize) -> Option<(usize, usize)> {
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
}

impl FitStage {
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
                }
            }
            None => Self {
                plan: None,
                compiled: evaluation.compiled()?.clone(),
                stage: full,
                full,
                linear,
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

/// The GPU preview of `evaluation`, an open draft's preview job's evaluation, drawn in `bounds`:
/// the plan from the earliest layer the draft changes, at the stage the job's Fit frame is drawn
/// at, and the boundary it starts from. `O(layers)` on the catalog owner: one compile at the proxy
/// stage for the window, one for the plan, and no pixel read.
pub(crate) fn plan_preview(
    evaluation: &Evaluation,
    draft: &Draft,
    bounds: ProxyBounds,
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
        });
    };
    let fit = FitStage::of(evaluation, bounds)?;
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
    let answer = gpu_plan(registry, &planned, request)?;
    let boundary_request = match &answer {
        GpuAnswer::Fallback(_) => None,
        GpuAnswer::Plan(plan) => Some(BoundaryRequest {
            key: BoundaryKey {
                source: evaluation.source().identity(),
                prefix: prefix_hash(&recipe.layers[..boundary], &recipe.masks, fit.sampling())?,
                layer: boundary,
                plan: fit.plan,
            },
            position: position(&fit.compiled, boundary).ok_or_else(|| {
                Error::internal(format!(
                    "the GPU preview's boundary layer {boundary} is past the stack"
                ))
            })?,
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
    })
}

/// The plans a gesture on `evaluation`'s stack is likely to draw at `bounds`, for the desktop to
/// warm their program sequences before a drag begins: a drag of each colour or finish layer the
/// stack holds, and the first drag of each field-patch colour or finish module it does not hold
/// yet, each from that layer with the layer in its GPU shape, as a draft of it is planned
/// ([`plan_preview`]). One plan per program sequence, in stack order. `O(layers × modules)`
/// compiles on the catalog owner, with no pixel read.
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
    let mut candidates: Vec<(Recipe, usize)> = recipe
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
        candidates.push((with_neutral(recipe, index, &effect.id, None), index));
    }
    let mut plans: Vec<GpuPlan> = Vec::new();
    let mut seen: Vec<Vec<&'static str>> = Vec::new();
    for (planned, index) in candidates {
        let request = fit.request(index).drafted(index);
        let answer = gpu_plan(registry, &planned, request)?;
        if let GpuAnswer::Plan(plan) = answer {
            let sequence: Vec<&'static str> = plan
                .operations()
                .flat_map(|operation| operation.units.iter().map(|unit| unit.program.entry))
                .collect();
            if !seen.contains(&sequence) {
                seen.push(sequence);
                plans.push(*plan);
            }
        }
    }
    Ok(plans)
}
