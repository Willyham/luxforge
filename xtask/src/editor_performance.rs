use crate::*;
use luxforge_core::{
    BASIC_EFFECT, CROP_EFFECT, Cancel, CropPayload, CropStage, EditorService, Layer, LayerId,
    ModuleRegistry, Mutation, PRESENCE_EFFECT, PreviewSource, ProxyBounds, Raster, Recipe,
    RenderContext, RenderOptions, SnapshotId, Transform, analysis, render,
};
use std::time::Instant;

/// The Basic layer the colour rows measure: one exposure value, the real module's payload and the
/// real compiled unit, placed where the host would place a colour-stage commit.
fn basic_exposure_layer(ev: f64) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: 1,
        payload: json!({ "exposure": ev }),
        mask: None,
        artifacts: Vec::new(),
    }
}

/// The Basic layer the "exposure plus tone" colour row measures: the same exposure value alongside
/// every Contrast/Highlights/Shadows/Whites/Blacks field non-neutral, compiled by the real module
/// into two real pointwise units (Exposure, then Tone) run as one streamed colour pass.
fn basic_exposure_and_tone_layer(ev: f64) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: 1,
        payload: json!({
            "exposure": ev,
            "contrast": 30.0,
            "highlights": -20.0,
            "shadows": 20.0,
            "whites": -10.0,
            "blacks": 10.0,
        }),
        mask: None,
        artifacts: Vec::new(),
    }
}

/// The Basic layer the Vibrance/Saturation row measures: the real module's payload compiling to
/// its two colour units (vibrance then saturation, the frozen internal order), placed where the
/// host would place a colour-stage commit.
fn basic_vibrance_saturation_layer(vibrance: f64, saturation: f64) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: 1,
        payload: json!({ "vibrance": vibrance, "saturation": saturation }),
        mask: None,
        artifacts: Vec::new(),
    }
}

/// The Basic layer the white-balance row measures: one 3x3 linear-sRGB matrix per pixel, compiled
/// by the real module from the real payload, at the same position a colour-stage commit takes.
fn basic_white_balance_layer(temperature: f64, tint: f64) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: 1,
        payload: json!({ "temperature": temperature, "tint": tint }),
        mask: None,
        artifacts: Vec::new(),
    }
}

/// The Basic layer the full-stack rows measure: every one of the ten fields non-neutral, so the
/// compiled operation runs all four units (white balance, exposure, tone, colour) in one pass.
fn basic_full_layer() -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: 1,
        payload: json!({
            "exposure": 0.5,
            "contrast": 25.0,
            "highlights": -30.0,
            "shadows": 30.0,
            "whites": -15.0,
            "blacks": 15.0,
            "temperature": 20.0,
            "tint": -10.0,
            "vibrance": 30.0,
            "saturation": 15.0,
        }),
        mask: None,
        artifacts: Vec::new(),
    }
}

/// The Presence layer the small-display proxy rows measure: every field at full strength, so the
/// compiled spatial operation holds all three of its neighbourhood units, as `editor-latency
/// --presence` commits it.
fn presence_full_layer() -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: PRESENCE_EFFECT.into(),
        effect_format: 1,
        payload: json!({ "texture": 100.0, "clarity": 100.0, "dehaze": 100.0 }),
        mask: None,
        artifacts: Vec::new(),
    }
}

/// The display the proxy rows are sized for: the owner's 2880 × 1800 physical window, which is
/// also the upper bound of what a Fit preview can show on it.
const PROXY_DISPLAY: ProxyBounds = ProxyBounds {
    width: 2880,
    height: 1800,
};

/// A small window's display: a 1280 × 800 bound, which fits a 3:2 photograph's whole proxy in
/// 1200 × 800 (0.96 megapixels), so every pass of a Fit render there is below one megapixel.
const SMALL_PROXY_DISPLAY: ProxyBounds = ProxyBounds {
    width: 1280,
    height: 800,
};

/// The rows of one recipe rendered against one proxy source, and the SHA-256 of its frame, so two
/// builds' proxy frames can be compared.
fn proxy_recipe_samples(
    registry: &ModuleRegistry,
    source: &PreviewSource,
    recipe: &Recipe,
    samples: usize,
) -> Result<(Vec<f64>, (u32, u32), String)> {
    let (timings, stage) = recipe_render_samples(registry, source, recipe, samples)?;
    let sha256 = frame_sha256(
        &render(
            registry,
            source,
            recipe,
            RenderOptions::default(),
            &RenderContext::new(),
        )?
        .frame(SnapshotId::new())?,
    );
    Ok((timings, stage, sha256))
}

/// A proxy source for `recipe` over `source` fitted to `bounds`, built as the preview worker
/// builds a whole-stage proxy, and the time each of `samples` builds took.
fn small_proxy(
    registry: &ModuleRegistry,
    source: &PreviewSource,
    recipe: &Recipe,
    samples: usize,
) -> Result<(PreviewSource, (u32, u32), Vec<f64>)> {
    let plan = render(
        registry,
        source,
        recipe,
        RenderOptions::default(),
        &RenderContext::new(),
    )?
    .proxy_plan(SMALL_PROXY_DISPLAY)
    .ok_or("The photo-sized source needs no proxy for a 1280x800 display")?;
    let mut timings = Vec::with_capacity(samples);
    let mut built = None;
    for _ in 0..samples {
        let started = Instant::now();
        built = Some(source.proxy(plan)?);
        timings.push(milliseconds(started));
    }
    Ok((
        built.ok_or("No proxy was built")?,
        (plan.width, plan.height),
        timings,
    ))
}

/// Render one recipe repeatedly through a given registry, the way the preview worker does.
fn recipe_render_samples(
    registry: &ModuleRegistry,
    source: &PreviewSource,
    recipe: &Recipe,
    samples: usize,
) -> Result<(Vec<f64>, (u32, u32))> {
    let mut timings = Vec::with_capacity(samples);
    let mut stage = (0, 0);
    let context = RenderContext::new();
    for _ in 0..samples {
        let started = Instant::now();
        let raster = render(registry, source, recipe, RenderOptions::default(), &context)?
            .frame(SnapshotId::new())?;
        timings.push(milliseconds(started));
        ensure(!raster.rgba.is_empty(), "Colour render was empty")?;
        stage = (raster.width, raster.height);
    }
    Ok((timings, stage))
}

/// The SHA-256 of a rendered frame's bytes, so two builds' frames can be compared without keeping
/// either.
fn frame_sha256(raster: &Raster) -> String {
    format!("{:x}", Sha256::digest(&raster.rgba[..]))
}

fn mutation(revision: u64, request: impl Into<String>) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "xtask-performance".into(),
    }
}

fn milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn render_samples(
    service: &EditorService,
    asset: &luxforge_core::AssetId,
    samples: usize,
) -> Result<Vec<f64>> {
    let mut timings = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let raster = service.render_current(asset)?;
        ensure(!raster.rgba.is_empty(), "Performance render was empty")?;
        timings.push(milliseconds(started));
    }
    Ok(timings)
}

/// Reduction cost alone: `analysis::reduce` over an already-rendered raster, with no
/// decode or render work inside the timed section, so this measures the histogram reducer
/// separately from `render_samples` above.
fn reduce_samples(raster: &Raster, samples: usize) -> Result<Vec<f64>> {
    let mut timings = Vec::with_capacity(samples);
    let expected_pixels = u64::from(raster.width) * u64::from(raster.height);
    for _ in 0..samples {
        let started = Instant::now();
        let report = analysis::reduce(&raster.rgba, raster.width, raster.height, &Cancel::never())?;
        ensure(
            report.pixel_count() == expected_pixels,
            "Performance reduction pixel count mismatch",
        )?;
        timings.push(milliseconds(started));
    }
    Ok(timings)
}

pub fn run(root: &Path, source: &Path, out: &Path, samples: usize) -> Result {
    ensure(!out.exists(), "Editor performance output must be new")?;
    ensure(samples > 0, "Editor performance samples must be positive")?;
    fs::create_dir_all(out)?;
    let source = source.canonicalize()?;
    let source_hash = hash(&source)?;
    let catalog = out.join("catalog.sqlite");
    let total = Instant::now();

    let mut service = EditorService::open(&catalog)?;
    let started = Instant::now();
    let state = service.import(&source)?;
    let import_ms = milliseconds(started);
    let asset = state.asset.id;
    let original = state.current_entry.id;

    let started = Instant::now();
    let original_job = service.preview_job(&asset, Some(&original), None, None, None)?;
    let cached_preview_job_ms = milliseconds(started);
    let started = Instant::now();
    let original_raster = match original_job.evaluation.source() {
        PreviewSource::Jpeg(image) => render(
            service.registry(),
            image,
            &original_job.evaluation.entry().snapshot.recipe,
            RenderOptions::default(),
            service.render_context(),
        )?
        .frame(original_job.evaluation.entry().snapshot.id.clone())?,
        PreviewSource::Raw { .. } => return Err("JPEG performance input expected".into()),
    };
    let original_render_ms = milliseconds(started);
    ensure(
        (original_raster.width, original_raster.height) == (state.asset.width, state.asset.height),
        "Original performance render has wrong dimensions",
    )?;
    // Reduction alone, over the identity raster just rendered above: no decode or render work is
    // inside this timed section, so this isolates `analysis::reduce` from rasterizing cost.
    let histogram_reduce = reduce_samples(&original_raster, samples)?;

    service.apply_transform(
        &asset,
        mutation(0, "performance-rotate"),
        Transform::RotateRight,
    )?;
    let one_transform = render_samples(&service, &asset, samples)?;
    let one_transform_sha256 = frame_sha256(&service.render_current(&asset)?);

    // Every further transform composes into the same orientation layer, so this measures 200
    // actions against one layer, not 200 layers: the render cost is the commit path's, not the
    // stack's.
    for index in 1..200u64 {
        service.apply_transform(
            &asset,
            mutation(index, format!("performance-action-{index:03}")),
            Transform::MirrorHorizontal,
        )?;
    }
    let two_hundred_transform_actions = render_samples(&service, &asset, samples)?;

    // One straightened crop on top of the exact stack: the resample is a stage boundary, so this
    // measures the interpolating pass on the photo-sized input as well as the exact pass before it.
    let crop_started = Instant::now();
    let crop_entry = service
        .apply_action(
            &asset,
            mutation(200, "performance-crop-fit"),
            "crop-fit",
            json!({"aspect":"16:9","angle":10.0}),
        )?
        .current_entry_id;
    let crop_fit_commit_ms = milliseconds(crop_started);
    let crop_layers = service.entry(&asset, &crop_entry)?.snapshot.recipe.layers;
    let crop_layer = crop_layers
        .iter()
        .find(|layer| layer.effect_id == CROP_EFFECT)
        .ok_or("The fit did not produce a crop layer")?;
    let crop_payload: CropPayload = serde_json::from_value(crop_layer.payload.clone())?;
    // A quarter turn and 199 reflections of a landscape source leave its dimensions swapped.
    let crop_input = CropStage {
        width: state.asset.height,
        height: state.asset.width,
        angle: crop_payload.angle,
    };
    let crop_rect = crop_payload.output_rect(&crop_input)?;
    let angled_crop = render_samples(&service, &asset, samples)?;
    let crop_raster = service.render_current(&asset)?;
    ensure(
        (crop_raster.width, crop_raster.height) == (crop_rect.width, crop_rect.height),
        format!(
            "Straightened crop renders {}x{}, its payload declares {}x{}",
            crop_raster.width, crop_raster.height, crop_rect.width, crop_rect.height
        ),
    )?;
    // The host's pointwise colour pass on the same stack and the same decoded source, through the
    // real Basic module's +1 EV exposure unit; the colour layer goes where the host would place a
    // colour-stage commit, before the geometry tail. The identity render shares the source buffer
    // and allocates no frame, so it is the floor; the same stack without the colour layer is the
    // honest baseline for the pass itself, because it materializes exactly the same frames.
    let colour_job = service.preview_job(&asset, Some(&crop_entry), None, None, None)?;
    let colour_registry = ModuleRegistry::builtin();
    let stack = colour_job.evaluation.entry().snapshot.recipe.clone();
    let identity = Recipe {
        format: stack.format,
        layers: Vec::new(),
        masks: Vec::new(),
        ..Recipe::default()
    };
    let mut coloured = stack.clone();
    let index = colour_registry.insertion_index_for(&coloured.layers, BASIC_EFFECT);
    coloured.layers.insert(index, basic_exposure_layer(1.0));
    let mut toned = stack.clone();
    toned
        .layers
        .insert(index, basic_exposure_and_tone_layer(1.0));
    // The Vibrance/Saturation row: the same crop stack with one Colour-group layer compiling to
    // two units (vibrance then saturation), instead of the one exposure unit above, so the
    // difference against the same `stack_render` baseline isolates the Oklab conversion's cost.
    let mut vibrance_saturation = stack.clone();
    vibrance_saturation
        .layers
        .insert(index, basic_vibrance_saturation_layer(50.0, 20.0));
    let (identity_samples, identity_stage) = recipe_render_samples(
        &colour_registry,
        colour_job.evaluation.source(),
        &identity,
        samples,
    )?;
    // One +1 EV Basic layer on the upright source and nothing else: one segment through the
    // identity, whose colour pass writes a frame the size of the source.
    let exposure_only = Recipe {
        layers: vec![basic_exposure_layer(1.0)],
        ..identity.clone()
    };
    let (exposure_only_samples, exposure_only_stage) = recipe_render_samples(
        &colour_registry,
        colour_job.evaluation.source(),
        &exposure_only,
        samples,
    )?;
    ensure(
        exposure_only_stage == (state.asset.width, state.asset.height),
        "The Exposure-only render has wrong dimensions",
    )?;
    let exposure_only_sha256 = frame_sha256(
        &render(
            &colour_registry,
            colour_job.evaluation.source(),
            &exposure_only,
            RenderOptions::default(),
            &RenderContext::new(),
        )?
        .frame(SnapshotId::new())?,
    );
    let (stack_samples, stack_stage) = recipe_render_samples(
        &colour_registry,
        colour_job.evaluation.source(),
        &stack,
        samples,
    )?;
    let (colour_samples, colour_stage) = recipe_render_samples(
        &colour_registry,
        colour_job.evaluation.source(),
        &coloured,
        samples,
    )?;
    let (toned_samples, toned_stage) = recipe_render_samples(
        &colour_registry,
        colour_job.evaluation.source(),
        &toned,
        samples,
    )?;
    let (vibrance_saturation_samples, vibrance_saturation_stage) = recipe_render_samples(
        &colour_registry,
        colour_job.evaluation.source(),
        &vibrance_saturation,
        samples,
    )?;
    // The same stack and the same source through the white-balance unit instead: one composite 3x3
    // linear-sRGB multiply per pixel against exposure's one scalar multiply, so the two rows are
    // directly comparable and the difference is the unit's own arithmetic.
    let mut balanced = stack.clone();
    balanced
        .layers
        .insert(index, basic_white_balance_layer(30.0, -10.0));
    let (white_balance_samples, white_balance_stage) = recipe_render_samples(
        &colour_registry,
        colour_job.evaluation.source(),
        &balanced,
        samples,
    )?;
    ensure(
        stack_stage == white_balance_stage,
        "A white-balance operation changed the output stage",
    )?;
    let white_balance_render = white_balance_samples;

    // The proxy rows: the same crop stack with every Basic field non-neutral, rendered at full
    // resolution and against the display-bounded proxy the preview worker builds for a 2880 × 1800
    // window. The proxy build is timed on its own (a cache miss, once per gesture or resize) and
    // the proxy renders are what a slider tick costs at Fit.
    let mut full_basic = stack.clone();
    full_basic.layers.insert(index, basic_full_layer());
    let (full_basic_samples, full_basic_stage) = recipe_render_samples(
        &colour_registry,
        colour_job.evaluation.source(),
        &full_basic,
        samples,
    )?;
    ensure(
        stack_stage == full_basic_stage,
        "The full Basic layer changed the output stage",
    )?;
    let full_basic_render = full_basic_samples;
    let plan = render(
        &colour_registry,
        colour_job.evaluation.source(),
        &full_basic,
        RenderOptions::default(),
        &RenderContext::new(),
    )?
    .proxy_plan(PROXY_DISPLAY)
    .ok_or("The photo-sized source needs no proxy for a 2880x1800 display")?;
    let mut proxy_build = Vec::with_capacity(samples);
    let mut proxy_source = None;
    for _ in 0..samples {
        let started = Instant::now();
        let built = colour_job.evaluation.source().proxy(plan)?;
        proxy_build.push(milliseconds(started));
        proxy_source = Some(built);
    }
    let proxy_source = proxy_source.ok_or("No proxy was built")?;
    let (proxy_identity_samples, _) =
        recipe_render_samples(&colour_registry, &proxy_source, &identity, samples)?;
    let (proxy_stack_samples, proxy_stack_stage) =
        recipe_render_samples(&colour_registry, &proxy_source, &stack, samples)?;
    let (proxy_exposure_samples, proxy_exposure_stage) =
        recipe_render_samples(&colour_registry, &proxy_source, &coloured, samples)?;
    let (proxy_full_basic_samples, proxy_full_basic_stage) =
        recipe_render_samples(&colour_registry, &proxy_source, &full_basic, samples)?;
    ensure(
        proxy_stack_stage == proxy_exposure_stage && proxy_stack_stage == proxy_full_basic_stage,
        "A colour operation changed the proxy output stage",
    )?;
    ensure(
        proxy_stack_stage.0 <= PROXY_DISPLAY.width && proxy_stack_stage.1 <= PROXY_DISPLAY.height,
        "The proxy output stage does not fit the display",
    )?;

    // The small-display rows: a 1280 × 800 bound, where a Fit render of the upright photograph is
    // below one megapixel in every pass, with a full Basic layer and with a full-strength Presence
    // layer; then the crop stack's proxy for the same bound, whose resample writes a sub-megapixel
    // output from a proxy stage above one megapixel.
    let with_layer = |recipe: &Recipe, layer: Layer| {
        let mut recipe = recipe.clone();
        let index = colour_registry.insertion_index_for(&recipe.layers, &layer.effect_id);
        recipe.layers.insert(index, layer);
        recipe
    };
    let upright_basic = with_layer(&identity, basic_full_layer());
    let upright_presence = with_layer(&identity, presence_full_layer());
    let stack_presence = with_layer(&stack, presence_full_layer());
    let (small_upright_source, small_upright_plan, small_upright_build) = small_proxy(
        &colour_registry,
        colour_job.evaluation.source(),
        &upright_basic,
        samples,
    )?;
    let (small_stack_source, small_stack_plan, small_stack_build) = small_proxy(
        &colour_registry,
        colour_job.evaluation.source(),
        &full_basic,
        samples,
    )?;
    let mut small_rows = Vec::new();
    let mut small_frames = serde_json::Map::new();
    let mut small_stages = serde_json::Map::new();
    for (metric, source, recipe) in [
        (
            "proxy_1280x800_upright_full_basic_layer",
            &small_upright_source,
            &upright_basic,
        ),
        (
            "proxy_1280x800_upright_presence_layer",
            &small_upright_source,
            &upright_presence,
        ),
        (
            "proxy_1280x800_same_stack_with_full_basic_layer",
            &small_stack_source,
            &full_basic,
        ),
        (
            "proxy_1280x800_same_stack_with_presence_layer",
            &small_stack_source,
            &stack_presence,
        ),
    ] {
        let (timings, stage, sha256) =
            proxy_recipe_samples(&colour_registry, source, recipe, samples)?;
        ensure(
            stage.0 <= SMALL_PROXY_DISPLAY.width && stage.1 <= SMALL_PROXY_DISPLAY.height,
            format!("{metric} does not fit a 1280x800 display"),
        )?;
        small_rows.push((metric, timings));
        small_frames.insert(metric.into(), json!(sha256));
        small_stages.insert(metric.into(), json!([stage.0, stage.1]));
    }

    let proxy_identity_render = proxy_identity_samples;
    let proxy_stack_render = proxy_stack_samples;
    let proxy_exposure_render = proxy_exposure_samples;
    let proxy_full_basic_render = proxy_full_basic_samples;

    // The neutral picker: 25 point samples of the stage the Basic layer receives, each evaluated
    // through the compiled stack at O(layers). No frame is allocated and nothing is written, so
    // this is the whole cost of a pick on the catalog owner.
    let (pick_x, pick_y) = (state.asset.width / 2, state.asset.height / 2);
    let mut picker = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let answered = service.run_query(
            &asset,
            &crop_entry,
            "neutral-sample",
            json!({"x": pick_x, "y": pick_y}),
        );
        picker.push(milliseconds(started));
        // A photograph's mid-frame patch may legitimately be clipped, near-black or beyond the
        // representable range; the measurement is of the sampling path either way, so only an
        // unexpected failure kind is a problem.
        if let Err(error) = &answered {
            ensure(
                error.detail.starts_with("clipped:")
                    || error.detail.starts_with("near-black:")
                    || error.detail.starts_with("out-of-range:"),
                format!("Neutral picker failed unexpectedly: {error}"),
            )?;
        }
    }
    let neutral_picker = picker;
    ensure(
        identity_stage == (state.asset.width, state.asset.height),
        "Identity colour baseline has wrong dimensions",
    )?;
    ensure(
        stack_stage == colour_stage
            && stack_stage == toned_stage
            && stack_stage == vibrance_saturation_stage,
        "A colour operation changed the output stage",
    )?;
    let identity_render = identity_samples;
    let stack_render = stack_samples;
    let colour_render = colour_samples;
    let toned_render = toned_samples;
    let vibrance_saturation_render = vibrance_saturation_samples;
    // A source without a calibrated camera remains a valid general workload: the query is timed,
    // and the profile-dependent rows are explicitly untested. Named lens workloads must select.
    let mut lens_queries = Vec::with_capacity(samples);
    let mut candidate = None;
    for _ in 0..samples {
        let started = Instant::now();
        let answer = service.run_query(
            &asset,
            &original,
            "lens-profiles",
            json!({"assume-uncorrected":true}),
        )?;
        lens_queries.push(milliseconds(started));
        candidate = answer["rows"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["eligible"] == true))
            .and_then(|row| row["key"].as_str())
            .map(str::to_owned);
    }
    let mut lens_selections = Vec::with_capacity(samples);
    let mut lens_render = Vec::with_capacity(samples);
    let lens_measurement = match candidate {
        Some(profile) => {
            for index in 0..samples {
                let revision = service.state(&asset)?.revision;
                service.apply_action(
                    &asset,
                    mutation(revision, format!("performance-lens-reset-{index}")),
                    "reset-lens-profile",
                    json!({}),
                )?;
                let revision = service.state(&asset)?.revision;
                let started = Instant::now();
                service.apply_action(
                    &asset,
                    mutation(revision, format!("performance-lens-select-{index}")),
                    "select-lens-profile",
                    json!({"profile":profile,"assume-uncorrected":true}),
                )?;
                lens_selections.push(milliseconds(started));
                let started = Instant::now();
                let raster = service.render_current(&asset)?;
                lens_render.push(milliseconds(started));
                ensure(
                    (raster.width, raster.height) == (crop_raster.width, crop_raster.height),
                    "A lens warp changed the existing crop output dimensions",
                )?;
            }
            json!({"status":"measured","profile":profile,"selection":"First eligible row returned by query.lens-profiles; reset between samples so each measured selection commits","recipe":"Existing exact orientation and 10-degree crop, with the lens warp fused into its geometry pass"})
        }
        None => {
            json!({"status":"untested","reason":"The source has no eligible offline profile; the query is measured, selection and warped render are untested"})
        }
    };
    drop(service);

    // The cold path is the catalog owner's own: the source job's read, hash and decode and the
    // owner's completion, run blocking on this thread, then the preview job.
    let mut service = EditorService::open(&catalog)?;
    let started = Instant::now();
    service.prepare(&service.entry_needs(&asset, Some(&original))?)?;
    let cold_job = service.preview_job(&asset, Some(&original), None, None, None)?;
    let cold_source_and_job_ms = milliseconds(started);
    let started = Instant::now();
    let cold_raster = match cold_job.evaluation.source() {
        PreviewSource::Jpeg(image) => render(
            service.registry(),
            image,
            &cold_job.evaluation.entry().snapshot.recipe,
            RenderOptions::default(),
            service.render_context(),
        )?
        .frame(cold_job.evaluation.entry().snapshot.id.clone())?,
        PreviewSource::Raw { .. } => return Err("JPEG performance input expected".into()),
    };
    let cold_original_render_ms = milliseconds(started);
    ensure(
        (cold_raster.width, cold_raster.height) == (state.asset.width, state.asset.height),
        "Cold original performance render has wrong dimensions",
    )?;
    ensure(hash(&source)? == source_hash, "Performance source changed")?;

    // `import` and the other one-shot core steps are single observations, each its own one-sample
    // row, beside the recipes' sampled distributions.
    let mut rows = Vec::new();
    for (metric, value) in [
        ("import", import_ms),
        ("cached_preview_job", cached_preview_job_ms),
        ("original_render", original_render_ms),
    ] {
        rows.push(stats::scalar(metric, "ms", Some(value)));
    }
    for (metric, samples) in [
        ("histogram_reduce", histogram_reduce),
        ("one_transform", one_transform),
        (
            "two_hundred_transform_actions_in_one_orientation_layer",
            two_hundred_transform_actions,
        ),
    ] {
        rows.push(stats::row(metric, "ms", samples));
    }
    rows.push(stats::row("lens_profiles_query", "ms", lens_queries));
    if !lens_selections.is_empty() {
        rows.push(stats::row(
            "lens_profile_selection_commit",
            "ms",
            lens_selections,
        ));
        rows.push(stats::row(
            "lens_and_straightened_crop_render",
            "ms",
            lens_render,
        ));
    }
    rows.push(stats::scalar(
        "crop_fit_commit",
        "ms",
        Some(crop_fit_commit_ms),
    ));
    for (metric, samples) in [
        (
            "two_hundred_transform_actions_and_a_10_degree_crop",
            angled_crop,
        ),
        ("colour_identity_render", identity_render),
        ("exposure_only_1ev_basic_layer", exposure_only_samples),
        ("colour_baseline_same_stack_without_colour", stack_render),
        ("colour_same_stack_with_one_1ev_basic_layer", colour_render),
        (
            "colour_same_stack_with_exposure_and_five_tone_fields",
            toned_render,
        ),
        (
            "colour_same_stack_with_vibrance_50_saturation_20_basic_layer",
            vibrance_saturation_render,
        ),
        (
            "colour_same_stack_with_one_white_balance_basic_layer",
            white_balance_render,
        ),
        ("colour_same_stack_with_full_basic_layer", full_basic_render),
        ("proxy_build_for_2880x1800", proxy_build),
        ("proxy_identity_render", proxy_identity_render),
        ("proxy_same_stack_without_colour", proxy_stack_render),
        (
            "proxy_same_stack_with_one_1ev_basic_layer",
            proxy_exposure_render,
        ),
        (
            "proxy_same_stack_with_full_basic_layer",
            proxy_full_basic_render,
        ),
        ("neutral_picker_query_25_point_samples", neutral_picker),
    ] {
        rows.push(stats::row(metric, "ms", samples));
    }
    rows.push(stats::row(
        "proxy_build_for_1280x800_upright",
        "ms",
        small_upright_build,
    ));
    rows.push(stats::row(
        "proxy_build_for_1280x800_same_stack",
        "ms",
        small_stack_build,
    ));
    for (metric, samples) in small_rows {
        rows.push(stats::row(metric, "ms", samples));
    }
    for (metric, value) in [
        ("reopen_source_and_preview_job", cold_source_and_job_ms),
        ("reopen_original_render", cold_original_render_ms),
        ("total", milliseconds(total)),
    ] {
        rows.push(stats::scalar(metric, "ms", Some(value)));
    }

    let result = json!({
        "status":"passed",
        "profile":if cfg!(debug_assertions) { "debug" } else { "release" },
        "platform":host(root)?,
        "source":source,
        "source_sha256":source_hash,
        "source_dimensions":[state.asset.width,state.asset.height],
        "crop_stage":{
            "input":[crop_input.width,crop_input.height],
            "angle_deg":crop_payload.angle,
            "output":[crop_rect.width,crop_rect.height],
        },
        "proxy":{
            "bounds":[PROXY_DISPLAY.width,PROXY_DISPLAY.height],
            "source":[plan.width,plan.height],
            "output":[proxy_stack_stage.0,proxy_stack_stage.1],
        },
        "small_proxy":{
            "bounds":[SMALL_PROXY_DISPLAY.width,SMALL_PROXY_DISPLAY.height],
            "upright_source":[small_upright_plan.0,small_upright_plan.1],
            "same_stack_source":[small_stack_plan.0,small_stack_plan.1],
            "outputs":small_stages,
            "frame_sha256":small_frames,
        },
        "samples_per_recipe":samples,
        "lens":lens_measurement,
        "frame_sha256":{
            "one_transform":one_transform_sha256,
            "exposure_only_1ev_basic_layer":exposure_only_sha256,
        },
        "method":"Core request-to-render diagnostics with a warm filesystem cache; excludes desktop scheduling, GPU upload and presentation.",
        "rows":rows,
        "checks":[
            "Decoded source is cached after import",
            "Original render dimensions are exact",
            "analysis::reduce's pixel_count matches the rendered raster on every sample",
            "One +1 EV Basic exposure layer alone renders the upright source's own dimensions",
            "One and 200 exact transform actions, composed into one orientation layer, render from the same immutable source",
            "A 10 degree crop-fit adds one resample stage boundary and renders its declared stage",
            "One +1 EV Basic exposure layer, compiled by the real luxforge.basic module, renders the same stage as the stack without it; the difference against that baseline is the streamed colour pass",
            "One Basic layer with +1 EV exposure and all five Contrast/Highlights/Shadows/Whites/Blacks fields non-neutral, compiled into two real pointwise units by the real luxforge.basic module, renders the same stage as the stack without it",
            "One Basic layer with vibrance 50 and saturation 20, compiled to two real Oklab colour units, renders the same stage as the stack without it",
            "One temperature 30 / tint -10 Basic layer renders the same stage as the stack without it; its unit is one composite 3x3 linear-sRGB multiply per pixel",
            "One Basic layer with all ten fields non-neutral renders the same stage as the stack without it, at full resolution and against the display-bounded proxy",
            "The proxy source built for a 2880x1800 display renders the crop stack into an output stage that fits that display",
            "Proxy sources built for a 1280x800 display render the upright photograph and the crop stack, each with a full Basic layer and with a full-strength Presence layer, into output stages that fit that display",
            "The neutral picker query evaluates 25 point samples of the stage the Basic layer receives at O(layers) each and allocates no frame",
            "Catalog reopen reconstructs the original historical state",
            "Source SHA-256 is unchanged"
        ]
    });
    write_json(&out.join("result.json"), &result)?;
    println!("PASS editor performance diagnostics: {}", out.display());
    Ok(())
}
