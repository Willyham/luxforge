//! UI-independent JPEG decoding, non-destructive editing state, rendering and the JSON owner API.
pub mod activity;
pub mod analysis;
mod api;
mod artifacts;
mod atomic_file;
mod cancel;
pub mod capabilities;
/// One home for the sRGB transfer function, Rec. 709 luminance, the Oklab conversion, small 3×3
/// linear algebra and the Planckian locus, shared by every renderer and colour module.
pub mod colour;
mod draft;
mod editor;
mod error;
/// JPEG export: capture metadata, the encoder and a publish that never replaces a file.
mod export;
pub mod flags;
pub mod jobs;
/// One persistent worker that runs the newest job, behind the preview, the analysis and the
/// desktop's clipping overlay.
pub mod latest;
/// The host's compiled mask and the component kinds this build can evaluate.
pub mod mask;
mod mask_field;
mod model;
mod modules;
/// The host's path primitives: the stored coordinate grid, decimation, the stroke a painting
/// action captures, and the content-addressed store those strokes live in.
pub mod path;
/// The person's preferences outside every catalog, and the launch read the desktop makes of them.
pub mod preferences;
mod presets;
mod preview;
mod profile;
mod proxy;
mod render;
pub mod resources;
mod source;

// The public surface: what the desktop, `luxforge-json`, `luxforge-net`, `luxforge-testkit`, xtask
// and this crate's integration tests name through the crate root, and every type a public item's
pub use activity::ActivitySnapshot;
pub use api::{
    ApiEvent, ApiFailure, ApiRequest, ApiResponse, ClientAuthority, ClientId, ClientSession,
    EventWake, EventsResult, LocalServer, MASK_MODE, MaskOverlayColour, MaskOverlayMode,
    OwnerHandle, POINTER_MODE, PreviewRequest, Renderer, RendererReason, RendererRecord,
    WorkspaceState, schemas, serve_json_lines_with,
};
pub use artifacts::{ArtifactId, ArtifactTable, PreparedArtifact};
pub use cancel::{Cancel, ProgressCounts};
pub use capabilities::context::ModuleContext;
pub use capabilities::host::HostConfig;
pub use capabilities::redact::redact_params;
pub use draft::{Draft, DraftTarget, declared_target};
pub use editor::{
    ActionResult, AssetPage, AssetRecord, AssetSummary, DraftStamp, EditorService, EditorState,
    Evaluation, FirstOpen, HistoryPage, LayerDescription, Lineage, LineageStep, MASK_FIELD,
    MutationOutcome, MutationResult, PixelInput, PixelSample, RawInterpretation, RecipeDescription,
    SkippedSetting, SourceKind, SourceTag, Version,
};
pub use error::{Error, ErrorKind, Preparation, PreparationNeeds};
pub use export::{CaptureInfo, CaptureMetadata};
pub use mask::MASK_GPU_PROGRAMS;
pub use model::{
    AssetId, COMPONENTS_PER_MASK, Component, ComponentId, ComponentMode, DraftId, EFFECT_FORMAT,
    EntryId, HistoryEntry, HistoryRow, JobId, Layer, LayerId, MASKS_PER_RECIPE, Mask, MaskId,
    Mutation, MutationRequest, Orientation, PresetId, RECIPE_FORMAT, Recipe, Snapshot, SnapshotId,
    Transform,
};
pub use modules::{
    ActionControl, ActionDescriptor, ActionInput, ActionPlan, ActionRef, ActionStyle, Availability,
    BASIC_EFFECT, BoxRect, CONTROLS_EFFECT, CROP_EFFECT, CURVE_EFFECT, CanvasInteraction,
    CapabilityModule, ChoiceControl, ChoiceStyle, ColorControl, ColorOperation, ColorStyle,
    CompileStage, Control, ControlVariant, Controls, ControlsModule, CropAspect, CropPayload,
    CropStage, CurveBackground, CurveChannel, CurveControl, DETAIL_EFFECT, Edge, EffectDescriptor,
    EffectStage, ExactGeometry, FieldPatch, FieldPatchModule, FitSettle, GPU_PROGRAMS,
    GroupControl, IdentityKind, LENS_EFFECT, LayerEdit, LayerReport, LayerUpdate, MAX_ANGLE,
    MAX_MASKED_SPATIAL_LAYERS, MIN_ANGLE, MIXER_EFFECT, ModuleDescriptor, ModuleLayout,
    ModuleRegistry, NewLayer, NumberControl, NumberStyle, ORIENTATION_EFFECT, OutputRect,
    PERSPECTIVE_EFFECT, PIXEL_EFFECT, PRESENCE_EFFECT, PROOF_GENERATE_PATH, PROOF_PALETTE,
    PROOF_PALETTE_PATH, ParameterDescriptor, ParameterKind, PickerControl, PointwiseColor,
    PresetsControl, Processing, Provider, QueryChoiceControl, QueryRef, RailDecoration,
    RangeControl, RawModule, RawPayload, Region, RegistryOptions, Resample, ResetAction,
    ResolvedControl, ResolvedReset, SamplingScale, SpatialOperation, Spec, Stage, StageContext,
    StageQuestions, TaskControl, ToggleControl, ToolModule, VIGNETTE_EFFECT, Values,
    WhiteBalanceMode, check_parameters, check_value, gains_from_temperature_tint, guide_angle,
    insertion_index_among, largest_with_ratio_inside, palette_bytes, resolve_control,
    resolve_group_reset, temperature_tint_from_gains,
};
pub use presets::{
    ImportReport, ImportedPreset, MAX_PRESET_BYTES, MappedSetting, PresetOrigin, PresetRecord,
    PresetSummary, ReportCounts, ReportedSetting, USER_PRESET_GROUP, inspect_preset,
};
pub use preview::{
    AssetSelection, BoundaryOutcome, ExactOutcome, HistorySelection, MAX_SELECTIONS, MaskCoverage,
    MaskCoverageTarget, MaskOverlayOutcome, PREVIEW_PROGRESS_QUIET, PhaseOutcome, PreviewIntent,
    PreviewJob, PreviewPhase, PreviewProgress, PreviewQueue, PreviewResult, PreviewSession,
    PreviewSource, ProxyOutcome, Queued, RegionOutcome, ViewState, Zoom,
};
pub use proxy::{ProxyApproximation, ProxyBounds, ProxyCoverage, ProxyIdentity, ProxyPlan};
pub use render::gpu::{
    BoundaryKey, BoundaryRequest, CoordinateGrid, EstimateSource, GPU_PASS_INPUTS, GPU_PLAN_LINKS,
    GPU_SHARED_VALUES, GPU_WARM_LINKS, GPU_WORKGROUP_LANES, GRID_MAX_NODES,
    GRID_SAMPLE_TOLERANCE_PX, GRID_TOLERANCE_PX, GpuAnswer, GpuApply, GpuBoundary, GpuChange,
    GpuClipping, GpuComponent, GpuDescription, GpuEstimates, GpuFallback, GpuGeometry, GpuMask,
    GpuOperation, GpuPass, GpuPassShape, GpuPlan, GpuPlanRequest, GpuPlane, GpuPlaneFormat,
    GpuPlaneSize, GpuPosition, GpuPreview, GpuProgram, GpuProgramKind, GpuRest, GpuSpatial,
    GpuSpatialUnit, GpuView, gpu_plan, gpu_plan_with,
};
pub use render::{BOUNDARY_MAX_BYTES, BoundaryFormat, BoundaryFrame};
pub use render::{
    ContentPoint, GeometryMap, INPUT_GRID_MAX_CELLS, InputGridCache, LinearSettings, MapError,
    MappingDescriptor, MappingShape, PrefixUse, Raster, RegionFrame, Render, RenderContext,
    RenderOptions, RenderSource, Sample, ScratchBudget, StageSize, WhiteBalanceApproximation,
    render, stage_transform,
};
pub use source::{LinearImage, OpticalIdentity, SourceImage, SourceOptics, open_source};

/// Qualification only: CPU filters the desktop's GPU readback tests hold each GPU kernel to. Built
/// only with the `qualification` feature, which only a `[dev-dependencies]` table may turn on.
#[cfg(feature = "qualification")]
pub mod qualification {
    pub use crate::modules::detail_qualification as detail;
    pub use crate::modules::presence_qualification as presence;

    /// The proxy plan a Fit job's worker builds for `recipe` over `render`'s source within
    /// `bounds`, as the GPU preview's plan reads it, and the window of the whole proxy stage the
    /// proxy source holds (`[x, y, width, height]`) when the stack reads less than all of it.
    /// `None` when the stack takes no proxy, or none smaller than the source fits.
    pub fn fit_proxy(
        render: &crate::Render,
        registry: &crate::ModuleRegistry,
        recipe: &crate::Recipe,
        bounds: crate::ProxyBounds,
    ) -> Option<(crate::ProxyPlan, Option<[u32; 4]>)> {
        registry.proxy_eligible(recipe).ok()?;
        let plan = render.proxy_plan(bounds)?;
        let plan = render.proxy_window(registry, recipe, plan).plan();
        let window = plan
            .window
            .map(|window| [window.x, window.y, window.width, window.height]);
        Some((plan, window))
    }

    /// A Lens correction layer holding a frozen Poly3 profile of `k1`, resolved for a `stage`, as
    /// a detected profile's Apply commits one: a lens warp for the GPU tests, with no lens index.
    pub fn lens_layer(k1: f64, stage: (u32, u32)) -> crate::Layer {
        crate::Layer::new(
            crate::LENS_EFFECT,
            crate::modules::lens::payload::qualification(k1, stage),
        )
    }

    /// The input of layer `layer` of `render`'s stack over the window of its received stage that
    /// the output stage's `rect` (`[x, y, width, height]`) reads at full scale, held as `format`:
    /// the boundary a percentage zoom's GPU preview of a drag from that layer starts from, as the
    /// preview worker renders it for a job carrying its request.
    pub fn region_boundary(
        render: &crate::Render,
        layer: usize,
        rect: [u32; 4],
        format: crate::BoundaryFormat,
    ) -> Result<crate::BoundaryFrame, crate::Error> {
        let [x0, y0, width, height] = rect;
        render.layer_region_boundary(
            crate::modules::Region {
                x0,
                y0,
                width,
                height,
            },
            layer,
            format,
        )
    }

    /// `plan` over its whole proxy stage, with no window.
    pub fn whole_proxy(plan: crate::ProxyPlan) -> crate::ProxyPlan {
        plan.whole()
    }

    /// The boundary at the source of `render`'s stack — the first segment's input before its
    /// first operation, the content stage the source fills — over `window` (`[x, y, width,
    /// height]`) of it, held as `format`: what the GPU's cut of the source it holds is held to,
    /// bit for bit (`docs/design/gpu-preview.md`, "The GPU source").
    pub fn source_boundary(
        render: &crate::Render,
        window: [u32; 4],
        format: crate::BoundaryFormat,
    ) -> Result<crate::BoundaryFrame, crate::Error> {
        let [x0, y0, width, height] = window;
        render.source_boundary(
            crate::modules::Region {
                x0,
                y0,
                width,
                height,
            },
            format,
        )
    }

    /// `image` viewed through the crop window `[x, y, width, height]` of its base planes under EXIF
    /// `orientation`, sharing its planes, as a RAW adapter views its development.
    pub fn viewed(
        image: &crate::LinearImage,
        crop: [u32; 4],
        orientation: u8,
    ) -> Result<crate::LinearImage, crate::Error> {
        image.with_view(crop, orientation)
    }

    /// A light for a colour drag under Dehaze from a held reduced stage: the atmospheric light
    /// prepared from the reduction of the Presence input of `prefix` — the layers before the
    /// drag's first colour layer, then the Presence layer — over its whole exact stage, with the
    /// colour operations `colour` compiles to run over the reduced pixels, colour after the
    /// reduction standing in for the reduction after colour. Answers the light and how long the
    /// part a tick would repeat took: the colour over the reduction and the preparation, not the
    /// reduction, which a boundary job would hold.
    pub fn light_after_reduction(
        registry: &crate::ModuleRegistry,
        source: crate::RenderSource<'_>,
        prefix: &crate::Recipe,
        colour: &crate::Recipe,
    ) -> Result<(Option<Vec<f64>>, std::time::Duration), crate::Error> {
        let captured = || {
            crate::render::spatial::CAPTURED_REDUCTION
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        };
        let context = crate::RenderContext::new();
        let render = crate::render(
            registry,
            source,
            prefix,
            crate::RenderOptions::exact(&crate::Cancel::never()),
            &context,
        )?;
        captured();
        render.frame(crate::SnapshotId::new())?;
        let reduction = captured().ok_or_else(|| crate::Error::internal("no reduction"))?;
        let unit = render
            .estimating_unit()
            .ok_or_else(|| crate::Error::validation("no unit prepares an estimate"))?;
        let (width, height) = source.dimensions();
        let compiled = registry.compile_sampled(
            width,
            height,
            width,
            height,
            colour,
            crate::mask_field::MaskSampling::Point,
        )?;
        let units: Vec<std::sync::Arc<dyn crate::modules::PointwiseColor>> = compiled
            .segments
            .iter()
            .flat_map(|segment| &segment.operations)
            .filter_map(|operation| match operation {
                crate::modules::Processing::Color(colour) => Some(colour.units().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        // The byte path hands a spatial operation a quantized frame, so its colour is clamped to
        // [0, 1] before it is reduced; the linear path's is not.
        let clamps = matches!(source, crate::RenderSource::Byte(_));
        let started = std::time::Instant::now();
        let changed = reduction.with_rows(|y, row| {
            for unit in &units {
                unit.apply_row(y, 0, row);
            }
            if clamps {
                for pixel in row.iter_mut() {
                    *pixel = pixel.map(|value| value.clamp(0.0, 1.0));
                }
            }
        });
        let light = unit.prepare(&changed);
        let elapsed = started.elapsed();
        Ok((light.map(|light| light.values().to_vec()), elapsed))
    }

    /// The atmospheric light the per-frame estimate twin would prepare at each of `factors` (each a
    /// divisor of 16) for each estimating layer of `recipe` named in `estimating`, in recipe order,
    /// emulated on the CPU (`docs/specs/performance.md`, "The per-frame light's reduction factor").
    /// For each such layer: the stage its colour run reads — the output of every layer before it
    /// but the colour layers directly before it, the run, and with every restoration layer left
    /// out when `skip_restoration` — rendered at full resolution and reduced to its exact means
    /// over `factor × factor` cells; the run compiled at that reduced stage and run over the
    /// means, a masked layer blended by its coverage there ([`twin_light`]); clamped to `[0, 1]`
    /// where the byte path clamps; each 16-pixel block's cells averaged, unweighted, as the twin's
    /// light step averages its texels; and the light selected as Dehaze prepares it. An earlier
    /// layer of `estimating` that the stage passes through draws with its own light at the same
    /// factor; every other layer before the run is rendered exactly. Answers, for each factor,
    /// each layer's light in the order of `estimating`.
    pub fn twin_lights(
        registry: &crate::ModuleRegistry,
        source: crate::RenderSource<'_>,
        recipe: &crate::Recipe,
        estimating: &[usize],
        factors: &[u32],
        skip_restoration: bool,
    ) -> Result<Vec<Vec<Option<Vec<f64>>>>, crate::Error> {
        use crate::modules::EffectStage;
        use crate::render::spatial::cells;
        let stage_of = |layer: &crate::Layer| {
            registry
                .effect(&layer.effect_id)
                .map(|(_, effect)| effect.stage)
        };
        // The byte path hands a spatial operation a quantized frame, so its colour is clamped to
        // [0, 1] before it is reduced; the linear path's is not.
        let clamps = matches!(source, crate::RenderSource::Byte(_));
        let mut lights: Vec<Vec<Option<Vec<f64>>>> = vec![Vec::new(); factors.len()];
        for (k, &layer) in estimating.iter().enumerate() {
            if layer >= recipe.layers.len() {
                return Err(crate::Error::validation(format!(
                    "layer {layer} is past the stack"
                )));
            }
            let before: Vec<usize> = (0..layer)
                .filter(|&at| {
                    !(skip_restoration
                        && stage_of(&recipe.layers[at]) == Some(EffectStage::Restoration))
                })
                .collect();
            // The colour run: the colour layers directly before the estimating layer.
            let split = before
                .iter()
                .rposition(|&at| stage_of(&recipe.layers[at]) != Some(EffectStage::Color))
                .map_or(0, |at| at + 1);
            let (base, run) = before.split_at(split);
            let layers = |indices: &[usize]| -> Vec<crate::Layer> {
                indices
                    .iter()
                    .map(|&at| recipe.layers[at].clone())
                    .collect()
            };
            let mut stack = layers(base);
            stack.push(recipe.layers[layer].clone());
            let prefix = crate::Recipe {
                layers: stack,
                ..recipe.clone()
            };
            let run = crate::Recipe {
                layers: layers(run),
                ..recipe.clone()
            };
            // The earlier estimating layers the stage passes through: their places in `prefix`,
            // and in `estimating`.
            let earlier: Vec<(usize, usize)> = base
                .iter()
                .enumerate()
                .filter_map(|(place, at)| {
                    estimating[..k]
                        .iter()
                        .position(|other| other == at)
                        .map(|index| (place, index))
                })
                .collect();
            // One render reads the stage at every factor, unless an earlier light, which differs
            // by factor, is held in it: then one render a factor.
            let groups: Vec<Vec<usize>> = if earlier.is_empty() {
                vec![(0..factors.len()).collect()]
            } else {
                (0..factors.len()).map(|index| vec![index]).collect()
            };
            for group in groups {
                let asked: Vec<u32> = group.iter().map(|&index| factors[index]).collect();
                let context = crate::RenderContext::new();
                let render = crate::render(
                    registry,
                    source,
                    &prefix,
                    crate::RenderOptions::exact(&crate::Cancel::never()),
                    &context,
                )?;
                for &(place, index) in &earlier {
                    let light = lights[group[0]][index].clone().ok_or_else(|| {
                        crate::Error::validation("an earlier layer prepared no light")
                    })?;
                    // Every unit of the operation that declares the light's key is handed it;
                    // the others read nothing.
                    hold_estimates(&render, place, &vec![Some(light); HELD_UNITS])?;
                }
                cells::arm(&asked)?;
                let framed = render.frame(crate::SnapshotId::new());
                let captured = cells::take();
                framed?;
                let captured = captured.last().ok_or_else(|| {
                    crate::Error::internal("the estimating layer's input was not reduced")
                })?;
                let reduction = &captured.reduction;
                if let Some(blocks) = captured.cells.iter().find(|cells| cells.factor == 16) {
                    let held = (0..blocks.height).all(|y| {
                        (0..blocks.width).all(|x| {
                            reduction.pixel(x, y)
                                == Some(blocks.values[(y * blocks.width + x) as usize])
                        })
                    });
                    if !held
                        || (blocks.width, blocks.height) != (reduction.width(), reduction.height())
                    {
                        return Err(crate::Error::internal(
                            "the cells at 16 are not the reduction's own blocks",
                        ));
                    }
                }
                let unit = render
                    .estimating_unit()
                    .ok_or_else(|| crate::Error::validation("no unit prepares an estimate"))?;
                for (&index, means) in group.iter().zip(&captured.cells) {
                    lights[index].push(twin_light(registry, &run, means, clamps, unit.as_ref())?);
                }
            }
        }
        Ok(lights)
    }

    /// More units than a spatial operation holds: [`twin_lights`] hands an earlier layer's light
    /// to each of them, and only a unit that declares the light's key keeps it.
    const HELD_UNITS: usize = 16;

    /// The light the twin's light step selects from `means`, the exact cell means of an estimating
    /// layer's input before its colour run `run`: `run` compiled at the cells' stage, as the twin
    /// compiles a stack at its block stage (masks sampled by the proxy's thin-feature rule), and run
    /// over each row of means in order — a masked operation blended against its own input by the
    /// coverage at the cell, `(1 − M)·in + M·units(in)`, its input kept where the coverage is 0 —
    /// then clamped to `[0, 1]` when `clamps`; each 16-pixel block's cells averaged, unweighted, in
    /// `f64`; and the light `unit` prepares from those block means.
    fn twin_light(
        registry: &crate::ModuleRegistry,
        run: &crate::Recipe,
        means: &crate::render::spatial::cells::CellMeans,
        clamps: bool,
        unit: &dyn crate::modules::SpatialUnit,
    ) -> Result<Option<Vec<f64>>, crate::Error> {
        let (width, height) = (means.width, means.height);
        let compiled = registry.compile_shaped(
            width,
            height,
            means.stage.width,
            means.stage.height,
            run,
            crate::mask_field::MaskSampling::ThinFeature,
            None,
        )?;
        let mut operations = Vec::new();
        for operation in compiled
            .segments
            .iter()
            .flat_map(|segment| &segment.operations)
        {
            match operation {
                crate::modules::Processing::Color(colour) => operations.push(colour),
                other => {
                    return Err(crate::Error::validation(format!(
                        "a colour run compiled to {other:?}"
                    )));
                }
            }
        }
        let mut texels = means.values.clone();
        for (y, row) in texels.chunks_exact_mut(width as usize).enumerate() {
            let y = y as u32;
            for operation in &operations {
                let input = row.to_vec();
                for unit in operation.units() {
                    unit.apply_row(y, 0, row);
                }
                if let Some(mask) = operation.mask() {
                    for (x, (output, input)) in row.iter_mut().zip(&input).enumerate() {
                        let coverage = mask.evaluate(x as u32, y, *input);
                        *output = if coverage == 0.0 {
                            *input
                        } else {
                            std::array::from_fn(|channel| {
                                (1.0 - coverage) * input[channel] + coverage * output[channel]
                            })
                        };
                    }
                }
                if !row.as_flattened().iter().all(|value| value.is_finite()) {
                    return Err(crate::Error::resource_limit(
                        "the twin's colour run produced a value that is not finite",
                    ));
                }
            }
            if clamps {
                for pixel in row.iter_mut() {
                    *pixel = pixel.map(|value| value.clamp(0.0, 1.0));
                }
            }
        }
        let block = crate::modules::ESTIMATE_REDUCTION;
        let side = block / means.factor;
        let (blocks_x, blocks_y) = crate::modules::Reduction::dimensions(means.stage, block);
        let len = (blocks_x * blocks_y) as usize;
        let mut planes = vec![0.0_f32; 3 * len];
        for j in 0..blocks_y {
            for i in 0..blocks_x {
                let (x0, y0) = (i * side, j * side);
                let (x1, y1) = ((x0 + side).min(width), (y0 + side).min(height));
                let count = f64::from(x1 - x0) * f64::from(y1 - y0);
                for (channel, plane) in planes.chunks_exact_mut(len).enumerate() {
                    let mut sum = 0.0_f64;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            sum += f64::from(texels[(y * width + x) as usize][channel]);
                        }
                    }
                    plane[(j * blocks_x + i) as usize] = (sum / count) as f32;
                }
            }
        }
        let reduction = crate::modules::Reduction::new(means.stage, block, planes)?;
        Ok(unit
            .prepare(&reduction)
            .map(|light| light.values().to_vec()))
    }

    /// The proxy phase of `render`'s stack as a preview job's worker renders it at `bounds`: the
    /// stack compiled at the proxy stage of [`fit_proxy`]'s plan, cut to its window when it has
    /// one, over `proxied`, the proxy source that plan builds, in `context`, whose store a frame of
    /// it fills.
    pub fn proxy_render<'s>(
        render: &crate::Render,
        registry: &crate::ModuleRegistry,
        recipe: &crate::Recipe,
        bounds: crate::ProxyBounds,
        proxied: crate::RenderSource<'s>,
        context: &'s crate::RenderContext,
    ) -> Result<crate::Render<'s>, crate::Error> {
        let plan = render
            .proxy_plan(bounds)
            .ok_or_else(|| crate::Error::validation("no proxy fits these bounds"))?;
        let stage = render.proxy_window(registry, recipe, plan);
        render.render_proxy(proxied, stage, &crate::Cancel::never(), context)
    }

    /// The input of layer `layer` of `render`'s stack at the proxy stage a Fit job's worker plans
    /// at `bounds`, over `proxied`, the proxy source that plan builds, held as `format`: the
    /// boundary the worker's proxy phase renders for a GPU preview of a drag from that layer, cut
    /// as that phase cuts it and read from the proxy stage's own uncut compilation for where the
    /// layer begins, as the worker reads it. What a boundary after a geometry layer is, such as a
    /// vignette's after a lens warp, which the proxy source itself is not.
    pub fn proxy_boundary(
        render: &crate::Render,
        registry: &crate::ModuleRegistry,
        recipe: &crate::Recipe,
        bounds: crate::ProxyBounds,
        proxied: crate::RenderSource<'_>,
        layer: usize,
        format: crate::BoundaryFormat,
    ) -> Result<crate::BoundaryFrame, crate::Error> {
        let plan = render
            .proxy_plan(bounds)
            .ok_or_else(|| crate::Error::validation("no proxy fits these bounds"))?;
        let stage = render.proxy_window(registry, recipe, plan);
        let plan = stage.plan();
        let uncut = stage.compiled()?.clone();
        let position = crate::render::gpu::position(&uncut, layer)
            .ok_or_else(|| crate::Error::validation(format!("layer {layer} is past the stack")))?;
        let whole = crate::modules::Stage {
            width: plan.width,
            height: plan.height,
        };
        let window = plan
            .window
            .map_or(crate::modules::Region::whole(whole), |window| {
                crate::modules::Region {
                    x0: window.x,
                    y0: window.y,
                    width: window.width,
                    height: window.height,
                }
            });
        let context = crate::RenderContext::new();
        let proxy = render.render_proxy(proxied, stage, &crate::Cancel::never(), &context)?;
        proxy.boundary_reading(&uncut, whole, window, position, format, None)
    }

    /// The global estimates the spatial operation of layer `layer` reads in a frame of `render`,
    /// from its context's estimate store alone: each unit's values, `None` for a unit that
    /// prepares none. `None` when the store does not hold them, as before any frame of `render`.
    pub fn held_estimates(
        render: &crate::Render,
        layer: usize,
    ) -> Result<Option<Vec<Option<Vec<f64>>>>, crate::Error> {
        let index = render
            .spatial_segment_of(layer)
            .ok_or_else(|| crate::Error::validation(format!("layer {layer} is not spatial")))?;
        Ok(render.held_spatial_globals(index)?.map(|globals| {
            globals
                .into_iter()
                .map(|global| global.map(|global| global.values().to_vec()))
                .collect()
        }))
    }

    /// Hold `estimates`, each unit's values or `None`, in `render`'s estimate store for the
    /// spatial operation of layer `layer`, under the keys a frame of `render` asks with, as a
    /// frame that prepared them would: a GPU plan over `render`'s context then reads them.
    pub fn hold_estimates(
        render: &crate::Render,
        layer: usize,
        estimates: &[Option<Vec<f64>>],
    ) -> Result<(), crate::Error> {
        let index = render
            .spatial_segment_of(layer)
            .ok_or_else(|| crate::Error::validation(format!("layer {layer} is not spatial")))?;
        let globals = estimates
            .iter()
            .map(|values| values.clone().map(crate::modules::Global::new).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        render.hold_spatial_globals(index, &globals)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::{
            BASIC_EFFECT, Cancel, Layer, ModuleRegistry, PRESENCE_EFFECT, Recipe, RenderContext,
            RenderOptions, RenderSource, SnapshotId,
        };
        use serde_json::json;

        const FACTORS: [u32; 5] = [16, 8, 4, 2, 1];

        /// The light a render of `recipe` over `source` prepares for layer `layer`, exactly.
        fn exact_light(
            registry: &ModuleRegistry,
            source: RenderSource<'_>,
            recipe: &Recipe,
            layer: usize,
        ) -> Vec<f64> {
            let context = RenderContext::new();
            let render = crate::render(
                registry,
                source,
                recipe,
                RenderOptions::exact(&Cancel::never()),
                &context,
            )
            .unwrap();
            render.frame(SnapshotId::new()).unwrap();
            held_estimates(&render, layer)
                .unwrap()
                .unwrap()
                .into_iter()
                .flatten()
                .find(|values| values.len() == 3)
                .unwrap()
        }

        /// A stage whose width and height are multiples of 8, so no cell is partial at a factor
        /// below 16, and whose height is not one of 16, so the last row of blocks is.
        const STAGE: (u32, u32) = (208, 136);

        /// A gain commutes with a mean and stays inside `[0, 1]` here, so the twin's light at
        /// every factor is the exact light, but for the byte path's 16-bit hand-off and the order
        /// of the sums; and at 16 it is the light from the reduction itself, the colour over it.
        #[test]
        fn the_twins_light_through_a_gain_is_the_exact_light_at_every_factor() {
            let registry = ModuleRegistry::builtin();
            let source = crate::render::tests::gradient(STAGE.0, STAGE.1);
            let recipe = Recipe {
                layers: vec![
                    Layer::new(BASIC_EFFECT, json!({"exposure": -1.0})),
                    Layer::new(PRESENCE_EFFECT, json!({"dehaze": 60})),
                ],
                ..Recipe::default()
            };
            let exact = exact_light(&registry, (&source).into(), &recipe, 1);
            let twin =
                twin_lights(&registry, (&source).into(), &recipe, &[1], &FACTORS, false).unwrap();
            for (factor, lights) in FACTORS.iter().zip(&twin) {
                let light = lights[0].as_ref().expect("a light");
                assert!(
                    error(&exact, light) < 1.0e-4,
                    "at {factor}: {light:?} against {exact:?}"
                );
            }
            let prefix = Recipe {
                layers: vec![recipe.layers[1].clone()],
                ..recipe.clone()
            };
            let colour = Recipe {
                layers: vec![recipe.layers[0].clone()],
                ..recipe.clone()
            };
            let (reduced, _) =
                light_after_reduction(&registry, (&source).into(), &prefix, &colour).unwrap();
            assert_eq!(twin[0][0], reduced, "at 16, the colour over the reduction");
        }

        /// The largest of a light's three channels' errors against `exact`, as fractions of it.
        fn error(exact: &[f64], light: &[f64]) -> f64 {
            (0..3)
                .map(|channel| (light[channel] - exact[channel]).abs() / exact[channel])
                .fold(0.0, f64::max)
        }

        /// Grey stripes three pixels wide, dark ones darker down the frame and bright ones
        /// brighter across it.
        fn stripes(width: u32, height: u32) -> crate::SourceImage {
            let mut rgba = Vec::with_capacity((width * height * 4) as usize);
            for y in 0..height {
                for x in 0..width {
                    let code = if (x / 3) % 2 == 1 {
                        150 + x * 100 / width
                    } else {
                        20 + y * 60 / height
                    } as u8;
                    rgba.extend([code, code, code, 255]);
                }
            }
            crate::SourceImage {
                width,
                height,
                rgba: rgba.into(),
                fingerprint: "sha256:stripes".into(),
                orientation: 1,
                capture: Default::default(),
            }
        }

        /// Three stops clip the bright stripes and not the dark ones, so the colour over a cell's
        /// mean is not the mean of the colour over its pixels where a cell holds both: the twin's
        /// light misses the exact light by that gap, less over cells finer than a stripe, and not
        /// at all over cells of one pixel, where the colour is the CPU's over every pixel.
        #[test]
        fn the_twins_light_misses_by_the_colour_runs_gap_which_finer_cells_narrow() {
            let registry = ModuleRegistry::builtin();
            let source = stripes(STAGE.0, STAGE.1);
            let recipe = Recipe {
                layers: vec![
                    Layer::new(BASIC_EFFECT, json!({"exposure": 3.0})),
                    Layer::new(PRESENCE_EFFECT, json!({"dehaze": 40})),
                ],
                ..Recipe::default()
            };
            let exact = exact_light(&registry, (&source).into(), &recipe, 1);
            let twin =
                twin_lights(&registry, (&source).into(), &recipe, &[1], &FACTORS, false).unwrap();
            let gaps: Vec<f64> = twin
                .iter()
                .map(|lights| error(&exact, lights[0].as_ref().unwrap()))
                .collect();
            assert!(gaps[0] > 1.0e-3, "the colour run's gap at 16: {gaps:?}");
            assert!(gaps[3] < gaps[0], "finer cells narrow the gap: {gaps:?}");
            assert!(gaps[4] < 1.0e-4, "cells of one pixel have none: {gaps:?}");
        }

        /// A light whose stage passes through an earlier estimating layer reads that layer's
        /// output drawn with its own twin light at the same factor: through a gain, which the twin
        /// follows exactly, it is the exact light at every factor. And a restoration layer left
        /// out leaves the stack without it, bit for bit.
        #[test]
        fn a_twin_light_reads_the_earlier_twin_light_and_can_leave_restoration_out() {
            let registry = ModuleRegistry::builtin();
            let source = crate::render::tests::gradient(STAGE.0, STAGE.1);
            let mut mask = crate::Mask::new("Mask 1");
            mask.components = vec![crate::Component::new(
                "Linear 1",
                crate::ComponentMode::Add,
                "linear",
                json!({"x0": -0.5, "y0": 0.5, "x1": 1.5, "y1": 0.5}),
            )];
            let chained = Recipe {
                layers: vec![
                    Layer::new(BASIC_EFFECT, json!({"exposure": -1.0})),
                    Layer::new(PRESENCE_EFFECT, json!({"dehaze": 40})),
                    Layer {
                        mask: Some(mask.id.clone()),
                        ..Layer::new(PRESENCE_EFFECT, json!({"dehaze": -60}))
                    },
                ],
                masks: vec![mask],
                ..Recipe::default()
            };
            let second = exact_light(&registry, (&source).into(), &chained, 2);
            let twin = twin_lights(
                &registry,
                (&source).into(),
                &chained,
                &[1, 2],
                &FACTORS,
                false,
            )
            .unwrap();
            for (factor, lights) in FACTORS.iter().zip(&twin) {
                let light = lights[1].as_ref().expect("the second light");
                assert!(
                    error(&second, light) < 1.0e-3,
                    "at {factor}: {light:?} against {second:?}"
                );
            }
            let stack = |layers: Vec<Layer>| Recipe {
                layers,
                ..Recipe::default()
            };
            let gain = Layer::new(BASIC_EFFECT, json!({"exposure": -1.0}));
            let dehaze = Layer::new(PRESENCE_EFFECT, json!({"dehaze": 40}));
            let sharpened = stack(vec![
                Layer::new(
                    crate::DETAIL_EFFECT,
                    json!({"sharpening": 150, "radius": 3, "sharpen-detail": 100}),
                ),
                gain.clone(),
                dehaze.clone(),
            ]);
            let skipped = twin_lights(
                &registry,
                (&source).into(),
                &sharpened,
                &[2],
                &FACTORS,
                true,
            )
            .unwrap();
            let without = twin_lights(
                &registry,
                (&source).into(),
                &stack(vec![gain, dehaze]),
                &[1],
                &FACTORS,
                false,
            )
            .unwrap();
            assert_eq!(skipped, without);
            let kept = twin_lights(
                &registry,
                (&source).into(),
                &sharpened,
                &[2],
                &FACTORS,
                false,
            )
            .unwrap();
            assert_ne!(kept, without, "sharpening moves the cells it is kept in");
        }
    }
}

// The crate root paths the core itself uses.
pub(crate) use editor::{AnalysisPlan, AnalysisSelection};
pub(crate) use model::{JobStatus, MAX_MASK_NAME, POINTS_PER_MASK, PixelReplace};
pub(crate) use modules::{
    MAX_PRESET_NAME, MAX_SETTINGS_ACTIONS, MAX_SETTINGS_FIELDS, Superseded, lightroom_to_luxforge,
    valid_name,
};
pub(crate) use proxy::{ProxyCache, ProxyKey, ProxyWindow};
pub(crate) use render::RegionRenderOutcome;
pub(crate) use source::{open_source_bytes, read_bounded_file};

// The crate root paths only the core's unit tests use.
#[cfg(test)]
pub(crate) use activity::ActivityBoard;
#[cfg(test)]
pub(crate) use model::MASK_BYTES_PER_RECIPE;
#[cfg(test)]
pub(crate) use modules::{
    APPLY_PRESET, BasicModule, CapabilitiesProofModule, MAX_COLOR_UNITS, PROOF_PALETTE_GAINS,
    PresenceModule,
};
#[cfg(test)]
pub(crate) use render::ProxyRegionPlan;
