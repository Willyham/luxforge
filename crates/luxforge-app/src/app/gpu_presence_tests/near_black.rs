//! Near-black outliers on the linear (RAW) path, measured for a decision rather than held to a
//! limit: the Presence corpus recipes on the RAW sources, at Fit and on a full-resolution crop the
//! size of the 100% view, each through the boundary the slot holds today (`rgba16float`), through
//! an `rgba32float` boundary holding the same pixels unrounded, and through candidate guards in the
//! GPU apply's luminance-ratio reconstruction. The CPU frame is never changed.
//!
//! ```sh
//! LUXFORGE_GPU_CORPUS_OUTPUT=/tmp/new-dir \
//! LUXFORGE_RAW_MANIFEST=/path/to/raw-manifest.json \
//! cargo test -p luxforge-app gpu_presence_near_black -- --ignored --nocapture
//! ```
use super::super::gpu_plan::surface_plan;
use super::super::gpu_qualification::{apply_steps, codes, corpus_sources, figures, fit_bounds};
use luxforge_core::{
    Cancel, CompileStage, EffectStage, GpuAnswer, GpuPlanRequest, Layer, LinearImage,
    LinearSettings, ModuleRegistry, PhaseOutcome, PreviewIntent, PreviewQueue, PreviewRequest,
    PreviewSource, Processing, Recipe, RenderContext, RenderOptions, RenderSource, SnapshotId,
    Stage, gpu_plan, render,
};
use luxforge_reference::preview_error::{self, Rgb8, Statistics};
use luxforge_ui::photo_surface::{
    GpuPlan, GpuStep,
    gpu_preview::qualification::{Qualifier, boundary},
};
use serde_json::{Value, json};
use std::{borrow::Cow, sync::Arc};

/// A drawn pixel differing from the CPU's by more than this many codes in a channel is an outlier.
const OUTLIER_CODES: u8 = 8;

/// The condition of the near-black rule in `lf_presence_reconstruct`, which a guard replaces.
const NEAR_BLACK_RULE: &str = "    if abs(l_in) < lf_presence_near_black {\n";

/// One way of drawing the plan: its boundary's format and its reconstruction's near-black rule.
struct Variant {
    name: &'static str,
    float: bool,
    rule: Option<&'static str>,
}

const VARIANTS: [Variant; 7] = [
    Variant {
        name: "half",
        float: false,
        rule: None,
    },
    Variant {
        name: "float",
        float: true,
        rule: None,
    },
    Variant {
        name: "half, additive below 1e-5",
        float: false,
        rule: Some("    if abs(l_in) < 1e-5 {\n"),
    },
    Variant {
        name: "half, additive below 1e-4",
        float: false,
        rule: Some("    if abs(l_in) < 1e-4 {\n"),
    },
    Variant {
        name: "half, additive below 1e-3",
        float: false,
        rule: Some("    if abs(l_in) < 1e-3 {\n"),
    },
    // Additive where the luminance has cancelled to below 2^-8 of the channels' magnitudes, which
    // is where a half float's rounding of a channel (2^-11 of it) moves the ratio by an eighth.
    Variant {
        name: "half, additive below 2^-8 of |r|+|g|+|b|",
        float: false,
        rule: Some(
            "    if abs(l_in) < max(lf_presence_near_black, \
             0.00390625 * (abs(rgb.x) + abs(rgb.y) + abs(rgb.z))) {\n",
        ),
    },
    Variant {
        name: "float, additive below 2^-8 of |r|+|g|+|b|",
        float: true,
        rule: Some(
            "    if abs(l_in) < max(lf_presence_near_black, \
             0.00390625 * (abs(rgb.x) + abs(rgb.y) + abs(rgb.z))) {\n",
        ),
    },
];

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

/// `plan` with its spatial program's near-black rule replaced by `rule`.
fn guarded(plan: &GpuPlan, rule: Option<&str>) -> Result<GpuPlan, String> {
    let mut plan = plan.clone();
    let Some(rule) = rule else {
        return Ok(plan);
    };
    for step in &mut plan.steps {
        if let GpuStep::Spatial(spatial) = step {
            let source = spatial.program.source.as_ref();
            if source.matches(NEAR_BLACK_RULE).count() != 1 {
                return Err("the reconstruction's near-black rule has moved".into());
            }
            spatial.program.source = Cow::Owned(source.replace(NEAR_BLACK_RULE, rule));
        }
    }
    Ok(plan)
}

/// One frame to measure: the CPU's codes, the boundary's `f32` pixels and the plan over them.
struct Frame {
    size: (u32, u32),
    cpu: Vec<u8>,
    texels: Vec<[f32; 3]>,
    plan: GpuPlan,
}

/// What one variant drew against the CPU.
struct Measured {
    json: Value,
    outliers: u64,
    /// The worst drawn pixel's position.
    worst: (u32, u32),
}

fn measure(qualifier: &Qualifier, frame: &Frame, variant: &Variant) -> Result<Measured, String> {
    let (width, height) = frame.size;
    let plan = guarded(&frame.plan, variant.rule)?;
    let (drawn, values) = if variant.float {
        (
            qualifier.evaluate_codes_over(&plan, &frame.texels)?,
            qualifier.evaluate_over(&plan, &frame.texels)?,
        )
    } else {
        (qualifier.evaluate_codes(&plan)?, qualifier.evaluate(&plan)?)
    };
    let gpu: Vec<u8> = drawn
        .iter()
        .flat_map(|code| [code[0], code[1], code[2]])
        .collect();
    let program = codes(values.iter().map(|texel| [texel[0], texel[1], texel[2]]));
    let outliers = |codes: &[u8]| {
        codes
            .chunks_exact(3)
            .zip(frame.cpu.chunks_exact(3))
            .filter(|(a, b)| {
                a.iter()
                    .zip(*b)
                    .any(|(a, b)| a.abs_diff(*b) > OUTLIER_CODES)
            })
            .count() as u64
    };
    let worst = gpu
        .chunks_exact(3)
        .zip(frame.cpu.chunks_exact(3))
        .map(|(a, b)| {
            a.iter()
                .zip(b)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0)
        })
        .enumerate()
        .max_by_key(|(_, difference)| *difference)
        .map_or((0, 0), |(index, _)| {
            (index as u32 % width, index as u32 / width)
        });
    let rect = [0, 0, width, height];
    let rgb = |bytes| Rgb8::new(width, height, bytes);
    let drawn_statistics = preview_error::compare(rgb(&gpu)?, rgb(&frame.cpu)?, rect)?;
    let program_statistics = preview_error::compare(rgb(&program)?, rgb(&frame.cpu)?, rect)?;
    // The slot's charge holds a half-float boundary; a float one holds eight bytes a texel more.
    let charged = qualifier
        .charged_bytes(&plan)
        .map_err(|reason| format!("{reason:?}"))?
        + if variant.float {
            u64::from(width) * u64::from(height) * 8
        } else {
            0
        };
    let stats = |s: &Statistics| {
        json!({
            "mean": s.mean, "worst_block": s.worst_block, "p99": s.p99,
            "mean_delta_l": s.mean_delta_l, "max": s.max
        })
    };
    let drawn_outliers = outliers(&gpu);
    eprintln!(
        "    {:<44} outliers {:>5} (program {:>5}) | drawn {} | charged {charged} B",
        variant.name,
        drawn_outliers,
        outliers(&program),
        figures(&drawn_statistics),
    );
    Ok(Measured {
        json: json!({
            "variant": variant.name,
            "outliers": drawn_outliers,
            "program_outliers": outliers(&program),
            "drawn": stats(&drawn_statistics),
            "program": stats(&program_statistics),
            "charged_bytes": charged,
        }),
        outliers: drawn_outliers,
        worst,
    })
}

/// The index of the first layer that processes pixels, when every layer before it compiles to the
/// identity over a `size` stage, as a RAW development and a reset lens profile do.
fn boundary_layer(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    size: (u32, u32),
) -> Result<usize, String> {
    let stage_of = |layer: &Layer| registry.effect(&layer.effect_id).map(|(_, e)| e.stage);
    let index = recipe
        .layers
        .iter()
        .position(|layer| {
            matches!(
                stage_of(layer),
                Some(
                    EffectStage::Restoration
                        | EffectStage::Color
                        | EffectStage::Spatial
                        | EffectStage::Finish
                )
            )
        })
        .ok_or("no layer that processes pixels")?;
    for layer in &recipe.layers[..index] {
        let (module, _) = registry
            .effect(&layer.effect_id)
            .ok_or("an unknown effect")?;
        let compiled = module
            .compile(
                &layer.effect_id,
                layer.effect_format,
                &layer.payload,
                CompileStage::exact(stage(size.0, size.1)),
            )
            .map_err(|error| error.to_string())?;
        let identity = matches!(
            compiled,
            Processing::ExactGeometry(g)
                if (g.a, g.b, g.c, g.d, g.tx, g.ty) == (1, 0, 0, 1, 0, 0)
                    && (g.output_width, g.output_height) == size
        );
        if !identity {
            return Err(format!(
                "{} before the boundary is not the identity",
                layer.effect_id
            ));
        }
    }
    Ok(index)
}

fn rgb_of(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// A RAW cell's Fit frame through the desktop's own Fit job, and the full-resolution development,
/// its settings, the recipe and the registry it was rendered with.
fn fit(
    source: &std::path::Path,
    steps: &[Value],
    catalog: &std::path::Path,
) -> Result<
    (
        Frame,
        LinearImage,
        LinearSettings,
        Recipe,
        Arc<ModuleRegistry>,
    ),
    String,
> {
    use luxforge_core::OwnerHandle;
    let (owner, join) = OwnerHandle::start(catalog).map_err(|error| error.to_string())?;
    let client = owner.register();
    let asset = crate::app::testing::import_and_adopt(&owner, client, source);
    let result = (|| {
        apply_steps(&owner, client, &asset, steps)?;
        let bounds = fit_bounds();
        let mut job = crate::app::tasks::ready_preview_job(
            &owner,
            PreviewRequest::new(client, asset.clone()).proxy(bounds),
        )?;
        job.intent = PreviewIntent::Interactive;
        let evaluation = job.evaluation.clone();
        let recipe = evaluation.recipe().clone();
        let registry = evaluation.registry().clone();
        let mut queue = PreviewQueue::default();
        let generation = queue.request(job);
        let outcome = luxforge_testbase::wait_for("the Fit frame", || {
            let result = queue.poll()?;
            (result.generation == generation).then_some(result.outcome)
        });
        let PhaseOutcome::Proxy(proxy) = outcome else {
            return Err("a RAW at Fit is a proxy".to_owned());
        };
        let context = RenderContext::new();
        let plan = render(
            &registry,
            evaluation.source(),
            evaluation.recipe(),
            RenderOptions::exact(&Cancel::never()),
            &context,
        )
        .map_err(|error| error.to_string())?
        .proxy_plan(bounds)
        .ok_or("a proxy frame without a proxy plan")?;
        let proxied = evaluation
            .source()
            .proxy(plan)
            .map_err(|error| error.to_string())?;
        let (PreviewSource::Raw { image, settings }, PreviewSource::Raw { image: full, .. }) =
            (&proxied, evaluation.source())
        else {
            return Err("not a RAW".to_owned());
        };
        if settings.white_balance.is_some() {
            return Err("an approximated white balance".to_owned());
        }
        let size = proxied.dimensions();
        let full_size = evaluation.source().dimensions();
        let layer = boundary_layer(&registry, &recipe, size)?;
        let texels = pixels(image, (0, 0), size);
        let request = GpuPlanRequest::fit(
            layer,
            stage(size.0, size.1),
            stage(full_size.0, full_size.1),
        )
        .qualifying()
        .linear();
        let frame = frame_of(
            &registry,
            &recipe,
            request,
            size,
            texels,
            rgb_of(&proxy.raster.rgba),
        )?;
        Ok((frame, full.clone(), *settings, recipe, registry))
    })();
    owner.stop();
    let _ = join.join();
    result
}

/// The viewed pixels of `image` in the `size` rectangle at `origin`, row by row.
fn pixels(image: &LinearImage, origin: (u32, u32), size: (u32, u32)) -> Vec<[f32; 3]> {
    (0..size.1)
        .flat_map(|y| (0..size.0).map(move |x| (x, y)))
        .map(|(x, y)| {
            image
                .pixel(origin.0 + x, origin.1 + y)
                .expect("a viewed pixel")
        })
        .collect()
}

fn frame_of(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
    size: (u32, u32),
    texels: Vec<[f32; 3]>,
    cpu: Vec<u8>,
) -> Result<Frame, String> {
    let plan = match gpu_plan(registry, recipe, request).map_err(|e| e.to_string())? {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => return Err(format!("{}: {reason}", reason.code())),
    };
    let held = boundary(size.0, size.1, 1, &texels).ok_or("a boundary")?;
    let plan = surface_plan(&plan, held).map_err(|reason| format!("{reason:?}"))?;
    Ok(Frame {
        size,
        cpu,
        texels,
        plan,
    })
}

/// The `size` crop at `origin` of the full-resolution development, rendered exactly by the CPU as
/// a photograph of its own, and the exact plan over it: what a 100% view of that region draws,
/// but for the Dehaze light, which both take from the crop.
fn crop(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    full: &LinearImage,
    settings: &LinearSettings,
    origin: (u32, u32),
    size: (u32, u32),
) -> Result<Frame, String> {
    let texels = pixels(full, origin, size);
    let len = texels.len();
    let mut planes = vec![0.0_f32; 3 * len];
    for (index, pixel) in texels.iter().enumerate() {
        for channel in 0..3 {
            planes[channel * len + index] = pixel[channel];
        }
    }
    let image = LinearImage::with_fingerprint(
        size.0,
        size.1,
        planes,
        format!("sha256:near-black-{}-{}", origin.0, origin.1),
    )
    .map_err(|error| error.to_string())?;
    let context = RenderContext::new();
    let raster = render(
        registry,
        RenderSource::Linear {
            image: &image,
            settings: *settings,
        },
        recipe,
        RenderOptions::exact(&Cancel::never()),
        &context,
    )
    .and_then(|render| render.frame(SnapshotId::new()))
    .map_err(|error| error.to_string())?;
    let layer = boundary_layer(registry, recipe, size)?;
    let request = GpuPlanRequest::exact(layer, stage(size.0, size.1))
        .qualifying()
        .linear();
    frame_of(
        registry,
        recipe,
        request,
        size,
        texels,
        rgb_of(&raster.rgba),
    )
}

/// The Presence corpus recipes on this host's RAWs, at Fit and on two 100%-view crops of each (the
/// centre, and the region round the Fit frame's worst pixel), through every [`VARIANTS`] entry.
#[test]
#[ignore = "a measurement over the private RAWs: set LUXFORGE_GPU_CORPUS_OUTPUT to a new \
            directory and LUXFORGE_RAW_MANIFEST"]
fn gpu_presence_near_black_variants() {
    let test = "gpu_presence_near_black_variants";
    let output = std::path::PathBuf::from(
        std::env::var("LUXFORGE_GPU_CORPUS_OUTPUT").expect("LUXFORGE_GPU_CORPUS_OUTPUT"),
    );
    assert!(
        !output.exists(),
        "{} exists: use a new directory",
        output.display()
    );
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    std::fs::create_dir_all(&output).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &std::path::Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let corpus = read(&root.join("fixtures/preview/corpus.json"));
    let manifest = read(std::path::Path::new(
        &std::env::var("LUXFORGE_RAW_MANIFEST").expect("LUXFORGE_RAW_MANIFEST"),
    ));
    let sources: Vec<_> =
        corpus_sources(&corpus, &root.join("fixtures/generated"), Some(&manifest))
            .into_iter()
            .filter(|source| source.raw)
            .collect();
    // The 100% view: the photo surface of the evidence window, one source pixel a device pixel.
    let surface = crate::layout::photo_surface((1440.0, 900.0), true, true);
    let view = ((surface.0 * 2.0) as u32, (surface.1 * 2.0) as u32);
    eprintln!(
        "{test}: adapter {}, 100% view {view:?}",
        qualifier.adapter()
    );
    let reset_lens = json!({"api": {"method": "edit.reset-lens-profile", "params": {}}});
    let mut cells = Vec::new();
    let mut totals = vec![[0u64; 3]; VARIANTS.len()];
    for recipe in corpus["recipes"].as_array().expect("recipes") {
        if recipe["family"] != "presence" {
            continue;
        }
        let id = recipe["id"].as_str().unwrap();
        let mut steps = vec![reset_lens.clone()];
        steps.extend(recipe["steps"].as_array().expect("steps").iter().cloned());
        for source in &sources {
            let name = format!("{id}--{}--lens-reset", source.id);
            let catalog = output.join(format!("{name}.sqlite"));
            let prepared = fit(&source.path, &steps, &catalog);
            let _ = std::fs::remove_file(&catalog);
            let (fit_frame, full, settings, recipe, registry) = match prepared {
                Ok(prepared) => prepared,
                Err(reason) => {
                    eprintln!("{name}: gap: {reason}");
                    cells.push(json!({"cell": name, "gap": reason}));
                    continue;
                }
            };
            let full_size = (full.width(), full.height());
            let mut scales: Vec<Value> = Vec::new();
            eprintln!("{name}: Fit {}x{}", fit_frame.size.0, fit_frame.size.1);
            let mut worst = (0, 0);
            let mut measured = Vec::new();
            for (number, variant) in VARIANTS.iter().enumerate() {
                let result = measure(&qualifier, &fit_frame, variant).expect("a measurement");
                if number == 0 {
                    worst = result.worst;
                }
                totals[number][0] += result.outliers;
                measured.push(result.json);
            }
            scales.push(json!({"scale": "fit", "size": [fit_frame.size.0, fit_frame.size.1], "variants": measured}));
            drop(fit_frame);
            // The worst Fit pixel at full resolution, and the view round it inside the frame.
            let at = |fit: u32, fit_len: u32, full_len: u32, view_len: u32| {
                let centre =
                    (u64::from(fit) * u64::from(full_len) / u64::from(fit_len.max(1))) as u32;
                centre
                    .saturating_sub(view_len / 2)
                    .min(full_len.saturating_sub(view_len))
            };
            let size = (view.0.min(full_size.0), view.1.min(full_size.1));
            let fit_size = (
                scales[0]["size"][0].as_u64().unwrap() as u32,
                scales[0]["size"][1].as_u64().unwrap() as u32,
            );
            let crops = [
                (
                    "centre",
                    ((full_size.0 - size.0) / 2, (full_size.1 - size.1) / 2),
                ),
                (
                    "worst",
                    (
                        at(worst.0, fit_size.0, full_size.0, size.0),
                        at(worst.1, fit_size.1, full_size.1, size.1),
                    ),
                ),
            ];
            for (slot, (label, origin)) in crops.into_iter().enumerate() {
                eprintln!(
                    "{name}: 100% {label} crop at {origin:?}, {}x{}",
                    size.0, size.1
                );
                let frame = match crop(&registry, &recipe, &full, &settings, origin, size) {
                    Ok(frame) => frame,
                    Err(reason) => {
                        eprintln!("    gap: {reason}");
                        scales.push(json!({"scale": label, "gap": reason}));
                        continue;
                    }
                };
                let mut measured = Vec::new();
                for (number, variant) in VARIANTS.iter().enumerate() {
                    let result = measure(&qualifier, &frame, variant).expect("a measurement");
                    totals[number][slot + 1] += result.outliers;
                    measured.push(result.json);
                }
                scales.push(json!({
                    "scale": format!("100% {label}"), "origin": [origin.0, origin.1],
                    "size": [size.0, size.1], "variants": measured
                }));
            }
            cells.push(json!({"cell": name, "full": [full_size.0, full_size.1], "scales": scales}));
        }
    }
    eprintln!("{test}: outliers over every cell (Fit, 100% centre, 100% worst):");
    for (variant, totals) in VARIANTS.iter().zip(&totals) {
        eprintln!("    {:<44} {:?}", variant.name, totals);
    }
    std::fs::write(
        output.join("near-black.json"),
        serde_json::to_string_pretty(&json!({
            "adapter": qualifier.adapter(),
            "view": [view.0, view.1],
            "outlier_codes": OUTLIER_CODES,
            "cells": cells,
        }))
        .unwrap(),
    )
    .unwrap();
}

/// `plan` with Texture's and Clarity's applies returning their input: the Dehaze output those
/// units reconstruct from.
fn dehaze_only(plan: &GpuPlan) -> Result<GpuPlan, String> {
    const RETURN: &str = "return lf_presence_reconstruct(rgb, lf_presence_luma(rgb), \
                          lf_presence_decode(encoded + delta));";
    let mut plan = plan.clone();
    for step in &mut plan.steps {
        if let GpuStep::Spatial(spatial) = step {
            let source = spatial.program.source.as_ref();
            if source.matches(RETURN).count() != 2 {
                return Err("Texture's and Clarity's reconstructions have moved".into());
            }
            spatial.program.source = Cow::Owned(source.replace(RETURN, "return rgb;"));
        }
    }
    Ok(plan)
}

/// One cell's worst pixels at Fit, for the mechanism: the boundary's pixel as `f32` and as the half
/// float the slot holds, the Dehaze output the luminance units then read over each boundary, its
/// luminance, and the codes the CPU, the half-float boundary and the `f32` one draw.
#[test]
#[ignore = "a diagnosis over the private RAWs: set LUXFORGE_RAW_MANIFEST, \
            LUXFORGE_NEAR_BLACK_CELL to `<recipe>/<source>` and LUXFORGE_GPU_CORPUS_OUTPUT to a \
            new directory"]
fn gpu_presence_near_black_pixels() {
    let test = "gpu_presence_near_black_pixels";
    let output = std::path::PathBuf::from(
        std::env::var("LUXFORGE_GPU_CORPUS_OUTPUT").expect("LUXFORGE_GPU_CORPUS_OUTPUT"),
    );
    assert!(
        !output.exists(),
        "{} exists: use a new directory",
        output.display()
    );
    let cell = std::env::var("LUXFORGE_NEAR_BLACK_CELL").expect("LUXFORGE_NEAR_BLACK_CELL");
    let (recipe_id, source_id) = cell.split_once('/').expect("<recipe>/<source>");
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    std::fs::create_dir_all(&output).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &std::path::Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let corpus = read(&root.join("fixtures/preview/corpus.json"));
    let manifest = read(std::path::Path::new(
        &std::env::var("LUXFORGE_RAW_MANIFEST").expect("LUXFORGE_RAW_MANIFEST"),
    ));
    let source = corpus_sources(&corpus, &root.join("fixtures/generated"), Some(&manifest))
        .into_iter()
        .find(|source| source.id == source_id)
        .expect("the source on this host");
    let recipe = corpus["recipes"]
        .as_array()
        .expect("recipes")
        .iter()
        .find(|recipe| recipe["id"] == recipe_id)
        .expect("the recipe");
    let mut steps = vec![json!({"api": {"method": "edit.reset-lens-profile", "params": {}}})];
    steps.extend(recipe["steps"].as_array().expect("steps").iter().cloned());
    let (frame, ..) =
        fit(&source.path, &steps, &output.join("cell.sqlite")).expect("the Fit frame");
    let width = frame.size.0 as usize;
    let half_codes = qualifier.evaluate_codes(&frame.plan).expect("half codes");
    let float_codes = qualifier
        .evaluate_codes_over(&frame.plan, &frame.texels)
        .expect("float codes");
    let only = dehaze_only(&frame.plan).expect("the Dehaze output");
    let half_dehazed = qualifier.evaluate(&only).expect("half Dehaze");
    let float_dehazed = qualifier
        .evaluate_over(&only, &frame.texels)
        .expect("float Dehaze");
    let difference = |codes: &[[u8; 4]], index: usize| {
        (0..3)
            .map(|c| codes[index][c].abs_diff(frame.cpu[3 * index + c]))
            .max()
            .unwrap_or(0)
    };
    let mut worst: Vec<usize> = (0..frame.texels.len())
        .filter(|&index| {
            difference(&half_codes, index) > OUTLIER_CODES
                || difference(&float_codes, index) > OUTLIER_CODES
        })
        .collect();
    worst.sort_by_key(|&index| std::cmp::Reverse(difference(&half_codes, index)));
    let luma = |rgb: [f32; 4]| 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    eprintln!(
        "{test}: {cell}: {} outliers over either boundary",
        worst.len()
    );
    for &index in worst.iter().take(24) {
        let texel = frame.texels[index];
        let held = texel.map(luxforge_ui::photo_surface::gpu_preview::qualification::held);
        let (half, float) = (half_dehazed[index], float_dehazed[index]);
        eprintln!(
            "  ({}, {}): boundary {texel:?} held {held:?}\n    \
             Dehaze half {:?} (luma {:e}) float {:?} (luma {:e})\n    \
             codes cpu {:?} half {:?} float {:?}",
            index % width,
            index / width,
            &half[..3],
            luma(half),
            &float[..3],
            luma(float),
            &frame.cpu[3 * index..3 * index + 3],
            &half_codes[index][..3],
            &float_codes[index][..3],
        );
    }
}
