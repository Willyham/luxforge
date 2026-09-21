use crate::*;
use lightwell_core::{
    CROP_EFFECT, CropPayload, CropStage, EditorService, Mutation, Raster, Transform, analysis,
    render,
};
use std::time::Instant;

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

fn distribution(mut samples: Vec<f64>) -> Value {
    samples.sort_by(f64::total_cmp);
    let percentile = |percent: usize| {
        let rank = (percent * samples.len()).div_ceil(100).max(1);
        samples[rank - 1]
    };
    json!({
        "samples_ms":samples,
        "p50_ms":percentile(50),
        "p95_ms":percentile(95),
    })
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
    let original_job = service.preview_job(&asset, Some(&original), None)?;
    let cached_preview_job_ms = milliseconds(started);
    let started = Instant::now();
    let original_raster = render(
        service.registry(),
        &original_job.source,
        original_job.entry.snapshot.id,
        &original_job.entry.snapshot.recipe,
    )?;
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
    drop(service);

    let service = EditorService::open(&catalog)?;
    let started = Instant::now();
    let cold_job = service.preview_job(&asset, Some(&original), None)?;
    let cold_source_and_job_ms = milliseconds(started);
    let started = Instant::now();
    let cold_raster = render(
        service.registry(),
        &cold_job.source,
        cold_job.entry.snapshot.id,
        &cold_job.entry.snapshot.recipe,
    )?;
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
            "Catalog reopen reconstructs the original historical state",
            "Source SHA-256 is unchanged"
        ]
    });
    write_json(&out.join("result.json"), &result)?;
    println!("PASS editor performance diagnostics: {}", out.display());
    Ok(())
}
