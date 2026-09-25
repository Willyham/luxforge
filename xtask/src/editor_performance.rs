use crate::*;
use lightwell_core::{
    BASIC_EFFECT, CROP_EFFECT, CropPayload, CropStage, EditorService, Layer, LayerId,
    ModuleRegistry, Mutation, PreviewSource, ProxyBounds, Raster, Recipe, RenderContext,
    RenderOptions, SnapshotId, Transform, analysis, render,
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

/// The display the proxy rows are sized for: the owner's 2880 × 1800 physical window, which is
/// also the upper bound of what a Fit preview can show on it.
const PROXY_DISPLAY: ProxyBounds = ProxyBounds {
    width: 2880,
    height: 1800,
};

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

/// The one [`stats::Distribution`] shape every timing tool now writes, in place of this tool's own
/// `samples_ms`/`p50_ms`/`p95_ms`.
fn distribution(samples: Vec<f64>) -> Value {
    stats::distribution_json(samples)
}

fn render_samples(
    service: &EditorService,
    asset: &lightwell_core::AssetId,
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

/// Reduction cost alone: `analysis::reduce_raster` over an already-rendered raster, with no
/// decode or render work inside the timed section, so this measures the histogram reducer
/// separately from `render_samples` above.
fn reduce_samples(raster: &Raster, samples: usize) -> Result<Vec<f64>> {
    let mut timings = Vec::with_capacity(samples);
    let expected_pixels = u64::from(raster.width) * u64::from(raster.height);
    for _ in 0..samples {
        let started = Instant::now();
        let report = analysis::reduce_raster(raster)?;
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
    let original_raster = match &original_job.source {
        PreviewSource::Jpeg(image) => render(
            service.registry(),
            image,
            &original_job.entry.snapshot.recipe,
            RenderOptions::default(),
            service.render_context(),
        )?
        .frame(original_job.entry.snapshot.id)?,
        PreviewSource::Raw { .. } => return Err("JPEG performance input expected".into()),
    };
    let original_render_ms = milliseconds(started);
    ensure(
        (original_raster.width, original_raster.height) == (state.asset.width, state.asset.height),
        "Original performance render has wrong dimensions",
    )?;
    // Reduction alone, over the identity raster just rendered above: no decode or render work is
    // inside this timed section, so this isolates `analysis::reduce` from rasterizing cost.
    let histogram_reduce = distribution(reduce_samples(&original_raster, samples)?);

    service.apply_transform(
        &asset,
        mutation(0, "performance-rotate"),
        Transform::RotateRight,
    )?;
    let one_transform = distribution(render_samples(&service, &asset, samples)?);

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
    let two_hundred_transform_actions = distribution(render_samples(&service, &asset, samples)?);

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
    let angled_crop = distribution(render_samples(&service, &asset, samples)?);
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
    let stack = colour_job.entry.snapshot.recipe.clone();
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
    let (identity_samples, identity_stage) =
        recipe_render_samples(&colour_registry, &colour_job.source, &identity, samples)?;
    let (stack_samples, stack_stage) =
        recipe_render_samples(&colour_registry, &colour_job.source, &stack, samples)?;
    let (colour_samples, colour_stage) =
        recipe_render_samples(&colour_registry, &colour_job.source, &coloured, samples)?;
    let (toned_samples, toned_stage) =
        recipe_render_samples(&colour_registry, &colour_job.source, &toned, samples)?;
    let (vibrance_saturation_samples, vibrance_saturation_stage) = recipe_render_samples(
        &colour_registry,
        &colour_job.source,
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
    let (white_balance_samples, white_balance_stage) =
        recipe_render_samples(&colour_registry, &colour_job.source, &balanced, samples)?;
    ensure(
        stack_stage == white_balance_stage,
        "A white-balance operation changed the output stage",
    )?;
    let white_balance_render = distribution(white_balance_samples);

    // The proxy rows: the same crop stack with every Basic field non-neutral, rendered at full
    // resolution and against the display-bounded proxy the preview worker builds for a 2880 × 1800
    // window. The proxy build is timed on its own (a cache miss, once per gesture or resize) and
    // the proxy renders are what a slider tick costs at Fit.
    let mut full_basic = stack.clone();
    full_basic.layers.insert(index, basic_full_layer());
    let (full_basic_samples, full_basic_stage) =
        recipe_render_samples(&colour_registry, &colour_job.source, &full_basic, samples)?;
    ensure(
        stack_stage == full_basic_stage,
        "The full Basic layer changed the output stage",
    )?;
    let full_basic_render = distribution(full_basic_samples);
    let plan = colour_job
        .source
        .proxy_plan(&colour_registry, &full_basic, PROXY_DISPLAY)?
        .ok_or("The photo-sized source needs no proxy for a 2880x1800 display")?;
    let mut proxy_build = Vec::with_capacity(samples);
    let mut proxy_source = None;
    for _ in 0..samples {
        let started = Instant::now();
        let built = colour_job.source.proxy(plan)?;
        proxy_build.push(milliseconds(started));
        proxy_source = Some(built);
    }
    let proxy_source = proxy_source.ok_or("No proxy was built")?;
    let proxy_build = distribution(proxy_build);
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
    let proxy_identity_render = distribution(proxy_identity_samples);
    let proxy_stack_render = distribution(proxy_stack_samples);
    let proxy_exposure_render = distribution(proxy_exposure_samples);
    let proxy_full_basic_render = distribution(proxy_full_basic_samples);

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
    let neutral_picker = distribution(picker);
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
    let identity_render = distribution(identity_samples);
    let stack_render = distribution(stack_samples);
    let colour_render = distribution(colour_samples);
    let toned_render = distribution(toned_samples);
    let vibrance_saturation_render = distribution(vibrance_saturation_samples);
    drop(service);

    let service = EditorService::open(&catalog)?;
    let started = Instant::now();
    let cold_job = service.preview_job(&asset, Some(&original), None, None, None)?;
    let cold_source_and_job_ms = milliseconds(started);
    let started = Instant::now();
    let cold_raster = match &cold_job.source {
        PreviewSource::Jpeg(image) => render(
            service.registry(),
            image,
            &cold_job.entry.snapshot.recipe,
            RenderOptions::default(),
            service.render_context(),
        )?
        .frame(cold_job.entry.snapshot.id)?,
        PreviewSource::Raw { .. } => return Err("JPEG performance input expected".into()),
    };
    let cold_original_render_ms = milliseconds(started);
    ensure(
        (cold_raster.width, cold_raster.height) == (state.asset.width, state.asset.height),
        "Cold original performance render has wrong dimensions",
    )?;
    ensure(hash(&source)? == source_hash, "Performance source changed")?;

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
        "samples_per_recipe":samples,
        "method":"Core request-to-render diagnostics with a warm filesystem cache; excludes desktop scheduling, GPU upload and presentation.",
        "timings_ms":{
            "import":import_ms,
            "cached_preview_job":cached_preview_job_ms,
            "original_render":original_render_ms,
            "histogram_reduce":histogram_reduce,
            "one_transform":one_transform,
            "two_hundred_transform_actions_in_one_orientation_layer":two_hundred_transform_actions,
            "crop_fit_commit":crop_fit_commit_ms,
            "two_hundred_transform_actions_and_a_10_degree_crop":angled_crop,
            "colour_identity_render":identity_render,
            "colour_baseline_same_stack_without_colour":stack_render,
            "colour_same_stack_with_one_1ev_basic_layer":colour_render,
            "colour_same_stack_with_exposure_and_five_tone_fields":toned_render,
            "colour_same_stack_with_vibrance_50_saturation_20_basic_layer":vibrance_saturation_render,
            "colour_same_stack_with_one_white_balance_basic_layer":white_balance_render,
            "colour_same_stack_with_full_basic_layer":full_basic_render,
            "proxy_build_for_2880x1800":proxy_build,
            "proxy_identity_render":proxy_identity_render,
            "proxy_same_stack_without_colour":proxy_stack_render,
            "proxy_same_stack_with_one_1ev_basic_layer":proxy_exposure_render,
            "proxy_same_stack_with_full_basic_layer":proxy_full_basic_render,
            "neutral_picker_query_25_point_samples":neutral_picker,
            "reopen_source_and_preview_job":cold_source_and_job_ms,
            "reopen_original_render":cold_original_render_ms,
            "total":milliseconds(total),
        },
        "checks":[
            "Decoded source is cached after import",
            "Original render dimensions are exact",
            "analysis::reduce_raster's pixel_count matches the rendered raster on every sample",
            "One and 200 exact transform actions, composed into one orientation layer, render from the same immutable source",
            "A 10 degree crop-fit adds one resample stage boundary and renders its declared stage",
            "One +1 EV Basic exposure layer, compiled by the real lightwell.basic module, renders the same stage as the stack without it; the difference against that baseline is the streamed colour pass",
            "One Basic layer with +1 EV exposure and all five Contrast/Highlights/Shadows/Whites/Blacks fields non-neutral, compiled into two real pointwise units by the real lightwell.basic module, renders the same stage as the stack without it",
            "One Basic layer with vibrance 50 and saturation 20, compiled to two real Oklab colour units, renders the same stage as the stack without it",
            "One temperature 30 / tint -10 Basic layer renders the same stage as the stack without it; its unit is one composite 3x3 linear-sRGB multiply per pixel",
            "One Basic layer with all ten fields non-neutral renders the same stage as the stack without it, at full resolution and against the display-bounded proxy",
            "The proxy source built for a 2880x1800 display renders the crop stack into an output stage that fits that display",
            "The neutral picker query evaluates 25 point samples of the stage the Basic layer receives at O(layers) each and allocates no frame",
            "Catalog reopen reconstructs the original historical state",
            "Source SHA-256 is unchanged"
        ]
    });
    write_json(&out.join("result.json"), &result)?;
    println!("PASS editor performance diagnostics: {}", out.display());
    Ok(())
}
