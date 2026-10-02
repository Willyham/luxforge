//! The GPU preview qualification harness the program tests share (`docs/design/gpu-preview.md`,
//! "Qualifying a program"): a deterministic synthetic grid of linear pixels, the report helpers,
//! and the corpus at Fit, which draws a recipe's CPU frame through the desktop's own Fit job and
//! the GPU frame of the same plan through the photo surface's own shader, and judges each pair by
//! its recipe's class. [`corpus_at_fit`] takes the families to run, so a test of any program class
//! runs the same corpus with its own.
use super::gpu_plan::surface_plan_at;
use luxforge_core::{CompileStage, GpuAnswer, GpuPlanRequest, Layer, Processing, Stage, gpu_plan};
use luxforge_reference::{
    preview_error::{self, Class, Rgb8, Statistics},
    srgb,
};
use luxforge_ui::photo_surface::{
    BoundaryFormat, GpuPlan, GpuProgram, GpuStep, MaskedColour, TexelMap,
    gpu_preview::qualification::{Qualifier, boundary, boundary_as, held},
};
use serde_json::{Value, json};

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

// ---- The synthetic grid -----------------------------------------------------------------------

/// A deterministic `splitmix64` stream: the grid is the same on every host and every run.
pub(crate) struct Stream(pub(crate) u64);

impl Stream {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The values every grid draws its out-of-range extremes from: the largest half floats of both
/// signs, the smallest normal ones, zeros of both signs, and values past white and below black.
pub(crate) const EXTREMES: [f32; 10] = [
    65504.0, -65504.0, 6.104e-5, -6.104e-5, 0.0, -0.0, 2048.0, -16.0, 1.0e-7, 1.0,
];

/// One linear channel: in range (seventy percent, uniform in encoded sRGB so the shadows are as
/// dense as the highlights), past white up to 16, below black down to -0.5, or an extreme.
pub(crate) fn channel(stream: &mut Stream) -> f32 {
    let kind = stream.unit();
    let value = stream.unit();
    if kind < 0.70 {
        srgb::decode_encoded(value) as f32
    } else if kind < 0.82 {
        (1.0 + 15.0 * value * value) as f32
    } else if kind < 0.94 {
        (-0.5 * value) as f32
    } else {
        EXTREMES[(value * EXTREMES.len() as f64) as usize % EXTREMES.len()]
    }
}

/// A dense synthetic grid of `width × height` linear pixels, each channel as the boundary holds it
/// (the nearest half float): one pixel in ten an exact grey, the rest three independent channels.
pub(crate) fn grid(width: u32, height: u32, seed: u64) -> Vec<[f32; 3]> {
    let mut stream = Stream(seed);
    (0..width * height)
        .map(|_| {
            if stream.unit() < 0.1 {
                let grey = held(channel(&mut stream));
                [grey; 3]
            } else {
                [
                    held(channel(&mut stream)),
                    held(channel(&mut stream)),
                    held(channel(&mut stream)),
                ]
            }
        })
        .collect()
}

// ---- Reports ----------------------------------------------------------------------------------

/// `pixels` as 8-bit sRGB, three bytes a pixel, through the independent reference's quantizer.
pub(crate) fn codes(pixels: impl Iterator<Item = [f32; 3]>) -> Vec<u8> {
    pixels
        .flat_map(|rgb| rgb.map(|value| srgb::code(f64::from(value))))
        .collect()
}

/// The four statistics and the largest difference, as a report line.
pub(crate) fn figures(s: &Statistics) -> String {
    format!(
        "mean {:.4} worst block {:.4} p99 {:.4} mean dL* {:+.4} max {:.4}",
        s.mean, s.worst_block, s.p99, s.mean_delta_l, s.max
    )
}

/// The worst of each statistic over several cases, the signed ΔL* by its magnitude.
pub(crate) fn worst(statistics: &[Statistics]) -> Statistics {
    let largest = |of: fn(&Statistics) -> f64| statistics.iter().map(of).fold(0.0, f64::max);
    let delta_l = statistics
        .iter()
        .map(|s| s.mean_delta_l)
        .fold(0.0, |a: f64, b| if b.abs() > a.abs() { b } else { a });
    Statistics {
        pixels: statistics.iter().map(|s| s.pixels).sum(),
        mean: largest(|s| s.mean),
        worst_block: largest(|s| s.worst_block),
        worst_block_origin: [0, 0],
        p99: largest(|s| s.p99),
        mean_delta_l: delta_l,
        max: largest(|s| s.max),
    }
}

// ---- The corpus at Fit ------------------------------------------------------------------------

/// The Fit bounds of an evidence run's window, 1440 × 900 logical at 2× with both panels open,
/// as the desktop computes them ([`crate::app::Editor::proxy_bounds`]).
pub(crate) fn fit_bounds() -> luxforge_core::ProxyBounds {
    let surface = crate::layout::photo_surface((1440.0, 900.0), true, true);
    let inset = crate::layout::FIT_INSET;
    crate::app::preview::bounds_of(((surface.0 - inset.0) * 2.0, (surface.1 - inset.1) * 2.0))
        .expect("room for a photograph")
}

/// One corpus cell's photograph: its file and the corpus's id for it.
pub(crate) struct CorpusSource {
    pub(crate) id: String,
    pub(crate) path: std::path::PathBuf,
    pub(crate) raw: bool,
}

/// The corpus's sources this host has: the generated JPEGs under `generated`, and the RAWs the
/// private manifest at `manifest` resolves, when it is given. A source without a file here is
/// named and skipped.
pub(crate) fn corpus_sources(
    corpus: &Value,
    generated: &std::path::Path,
    manifest: Option<&Value>,
) -> Vec<CorpusSource> {
    let mut found = Vec::new();
    for source in corpus["sources"].as_array().expect("sources") {
        let id = source["id"].as_str().expect("an id").to_owned();
        let path = match source["kind"].as_str() {
            Some("generated-jpeg") => source["path"]
                .as_str()
                .and_then(|path| std::path::Path::new(path).file_name())
                .map(|name| generated.join(name)),
            Some("raw") => manifest.and_then(|manifest| {
                manifest["sources"]
                    .as_array()?
                    .iter()
                    .find(|entry| entry["id"] == source["manifest_id"])
                    .and_then(|entry| entry["path"].as_str())
                    .map(std::path::PathBuf::from)
            }),
            _ => None,
        };
        match path.filter(|path| path.is_file()) {
            Some(path) => found.push(CorpusSource {
                raw: source["kind"] == "raw",
                id,
                path,
            }),
            None => eprintln!("gap: {id} has no file on this host"),
        }
    }
    found
}

/// What one cell measured, or why it has no figures.
pub(crate) enum Cell {
    Measured {
        /// The stage the Fit frame was rendered at, and whether it was a proxy.
        stage: (u32, u32),
        proxy: bool,
        /// The codes the stage draws against the CPU's: the figures the limits judge.
        statistics: Statistics,
        /// The GPU's `f32` output through the reference quantizer against the CPU's, which leaves
        /// the hardware encoder's rounding out.
        program: Statistics,
        passed: bool,
        /// What the photo surface's slot drawing the plan charges the GPU-preview budget.
        charged: u64,
        /// For a stack whose Fit settles from the exact render, judged against the CPU's moving
        /// proxy it stands in for: the jump that settlement makes beside it.
        settled: Option<Box<Settled>>,
    },
    Gap(String),
}

/// What a Fit that settles from the exact render adds to a cell, as context the limits do not
/// judge (`docs/decisions.md`, "GPU previews"): the CPU's moving proxy against the exact-derived
/// frame it settles to, the jump the CPU path already makes, and the GPU frame against that frame.
pub(crate) struct Settled {
    pub(crate) proxy: Statistics,
    pub(crate) gpu: Statistics,
}

/// A step's `mask` and `component` parameters given as `{"name": ...}`, as the corpus's evidence
/// script names them, replaced by the identities the asset's recipe holds under those names.
fn resolve_names(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    asset: &Value,
    params: &mut Value,
) -> Result<(), String> {
    let named = |key: &str| params.get(key).and_then(|value| value["name"].as_str());
    if named("mask").is_none() && named("component").is_none() {
        return Ok(());
    }
    let recipe = luxforge_testkit::client::recipe(owner, client, asset)?;
    let mask = match named("mask") {
        Some(name) => Some(
            recipe
                .masks
                .iter()
                .find(|mask| mask.name == name)
                .ok_or_else(|| format!("no mask is named {name}"))?,
        ),
        None => None,
    };
    if let Some(name) = named("component") {
        let mask = mask.ok_or("a component named by name needs its mask named")?;
        let component = mask
            .components
            .iter()
            .find(|component| component.name == name)
            .ok_or_else(|| format!("{} holds no component named {name}", mask.name))?;
        params["component"] = json!(component.id.as_str());
    }
    if let Some(mask) = mask {
        params["mask"] = json!(mask.id.as_str());
    }
    Ok(())
}

/// `steps`, the corpus recipe's evidence-script steps, applied through the API to `asset`, a mask
/// or component named by name resolved to its identity. A `section` step only opens a panel and
/// changes no recipe, so it is passed over.
pub(crate) fn apply_steps(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    asset: &luxforge_core::AssetId,
    steps: &[Value],
) -> Result<(), String> {
    use luxforge_testkit::client::{call, mutation, request_id, revision};
    let id = json!(asset.as_str());
    for step in steps {
        if step.get("section").is_some() {
            continue;
        }
        let Some(api) = step.get("api") else {
            return Err(format!("{step} is not an API step"));
        };
        let mut params = api["params"].clone();
        resolve_names(owner, client, &id, &mut params)?;
        params["asset_id"] = id.clone();
        params["mutation"] = mutation(
            revision(owner, client, &id)?,
            &request_id("corpus"),
            "agent",
        );
        call(
            owner,
            client,
            api["method"].as_str().expect("a method"),
            params,
        )?;
    }
    Ok(())
}

/// The CPU Fit frame and the GPU frame of one recipe on one source, both written as PNGs in
/// `output` under `name`, and their figures held to `class`'s limits.
pub(crate) fn corpus_cell(
    qualifier: &Qualifier,
    source: &CorpusSource,
    steps: &[Value],
    output: &std::path::Path,
    name: &str,
    class: Class,
) -> Result<Cell, String> {
    use luxforge_core::{
        Cancel, EffectStage, OwnerHandle, PhaseOutcome, PreviewIntent, PreviewQueue,
        PreviewRequest, PreviewSource, RenderContext, RenderOptions, render,
    };
    let catalog = output.join(format!("{name}.sqlite"));
    let (owner, join) = OwnerHandle::start(&catalog).map_err(|error| error.to_string())?;
    let client = owner.register();
    let finish = |owner: OwnerHandle, join: std::thread::JoinHandle<()>| {
        owner.stop();
        let _ = join.join();
    };
    let asset = crate::app::testing::import_and_adopt(&owner, client, &source.path);
    let result = (|| -> Result<Cell, String> {
        apply_steps(&owner, client, &asset, steps)?;
        let bounds = fit_bounds();
        // The CPU frame: the desktop's own Fit job, through the preview worker's proxy phase. A
        // stack whose Fit settles from the exact render (Detail's) is judged against that proxy
        // too, the frame a gesture shows on the CPU path (owner, 2026-10-02); the exact phase's
        // reduction, which settlement presents, is kept beside it for the jump it makes.
        let mut job = crate::app::tasks::ready_preview_job(
            &owner,
            PreviewRequest::new(client, asset.clone()).proxy(bounds),
        )?;
        // An immediate job runs both phases: the moving proxy, then the exact frame with its
        // reduction.
        let settles = job.evaluation.settles_from_exact();
        job.intent = if settles {
            PreviewIntent::Immediate
        } else {
            PreviewIntent::Interactive
        };
        let evaluation = job.evaluation.clone();
        // The recipe the frame is rendered from, its painted strokes resolved.
        let recipe = evaluation.recipe().clone();
        let mut queue = PreviewQueue::default();
        let generation = queue.request(job);
        let outcome = luxforge_testbase::wait_for("the Fit frame", || {
            let result = queue.poll()?;
            (result.generation == generation).then_some(result.outcome)
        });
        let registry = evaluation.registry().clone();
        let full = evaluation.source().dimensions();
        // The Fit frame and the source it was rendered from: the proxy phase's frame over the
        // proxy the worker built — a window of the proxy stage when the stack reads less than all
        // of it, as a crop does — or, for a photograph that fits the bounds at its own size, the
        // exact phase's frame over the source itself. With the stage the plan addresses and where
        // in it the source's first texel is.
        let mut settled_frame = None;
        let (cpu, proxied, is_proxy, (stage_width, stage_height), origin) = match outcome {
            PhaseOutcome::Proxy(proxy) => {
                let context = RenderContext::new();
                let exact = render(
                    &registry,
                    evaluation.source(),
                    evaluation.recipe(),
                    RenderOptions::exact(&Cancel::never()),
                    &context,
                )
                .map_err(|error| error.to_string())?;
                let (plan, window) = luxforge_core::qualification::fit_proxy(
                    &exact,
                    &registry,
                    evaluation.recipe(),
                    bounds,
                )
                .ok_or("a proxy frame without a proxy plan")?;
                let proxied = evaluation
                    .source()
                    .proxy(plan)
                    .map_err(|error| error.to_string())?;
                if proxied.dimensions() != proxy.dimensions {
                    return Ok(Cell::Gap(format!(
                        "the worker's proxy is {:?}, not this {:?}",
                        proxy.dimensions,
                        proxied.dimensions()
                    )));
                }
                let origin = window.map_or((0, 0), |[x, y, _, _]| (x, y));
                let stage = (plan.width, plan.height);
                if settles {
                    let exact = luxforge_testbase::wait_for("the settled Fit frame", || {
                        let result = queue.poll()?;
                        match result.outcome {
                            PhaseOutcome::Exact(exact) if result.generation == generation => {
                                Some(exact)
                            }
                            _ => None,
                        }
                    });
                    exact.result.map_err(|error| error.to_string())?;
                    let Some(display) = exact.display else {
                        return Ok(Cell::Gap(
                            "the exact phase presented no settled Fit frame".to_owned(),
                        ));
                    };
                    settled_frame = Some(display);
                }
                (proxy.raster, proxied, true, stage, origin)
            }
            PhaseOutcome::Exact(exact) => (
                exact.result.map_err(|error| error.to_string())?,
                evaluation.source().clone(),
                false,
                full,
                (0, 0),
            ),
            PhaseOutcome::Region(_) => return Ok(Cell::Gap("a region at Fit".into())),
            PhaseOutcome::Boundary(_) => return Ok(Cell::Gap("a boundary for a Fit frame".into())),
        };
        let (width, height) = proxied.dimensions();
        // The boundary: the input of the first layer that processes pixels (restoration, colour,
        // spatial or finish), which is the source the frame was rendered from when every layer
        // before it compiles to the identity there, as a RAW development does. A geometry layer
        // before a content layer is part of the plan's geometry tail, which the CPU runs after the
        // content operations, so it does not change the boundary; before a finishing layer, which
        // runs after the tail, it does, and only the worker's boundary job holds that input.
        let stage_of = |layer: &Layer| registry.effect(&layer.effect_id).map(|(_, e)| e.stage);
        let boundary_layer = recipe
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
        let content = stage_of(&recipe.layers[boundary_layer]) != Some(EffectStage::Finish);
        for layer in &recipe.layers[..boundary_layer] {
            if content && stage_of(layer) == Some(EffectStage::Geometry) {
                continue;
            }
            let (module, _) = registry
                .effect(&layer.effect_id)
                .ok_or("an unknown effect")?;
            let compiled = module
                .compile(
                    &layer.effect_id,
                    layer.effect_format,
                    &layer.payload,
                    CompileStage::exact(stage(width, height)),
                )
                .map_err(|error| error.to_string())?;
            let identity = matches!(
                compiled,
                Processing::ExactGeometry(g)
                    if (g.a, g.b, g.c, g.d, g.tx, g.ty) == (1, 0, 0, 1, 0, 0)
                        && (g.output_width, g.output_height) == (width, height)
            );
            if !identity {
                return Ok(Cell::Gap(format!(
                    "{} before the boundary is not the identity",
                    layer.effect_id
                )));
            }
        }
        let texels: Vec<[f32; 3]> = match &proxied {
            PreviewSource::Jpeg(image) => {
                let table = luxforge_core::colour::srgb::decode_table();
                image
                    .rgba
                    .chunks_exact(4)
                    .map(|pixel| [0, 1, 2].map(|c| table[usize::from(pixel[c])]))
                    .collect()
            }
            PreviewSource::Raw { image, settings } => {
                if settings.white_balance.is_some() {
                    return Ok(Cell::Gap("an approximated white balance".into()));
                }
                (0..height)
                    .flat_map(|y| (0..width).map(move |x| (x, y)))
                    .map(|(x, y)| image.pixel(x, y).expect("a viewed pixel"))
                    .collect()
            }
        };
        let request = if is_proxy {
            GpuPlanRequest::fit(
                boundary_layer,
                stage(stage_width, stage_height),
                stage(full.0, full.1),
            )
        } else {
            GpuPlanRequest::exact(boundary_layer, stage(stage_width, stage_height))
        }
        .qualifying();
        // A RAW photograph's frames are the linear path's, which clamps no stage boundary.
        let request = match &proxied {
            PreviewSource::Raw { .. } => request.linear(),
            PreviewSource::Jpeg(_) => request,
        };
        let plan = match gpu_plan(&registry, &recipe, request).map_err(|e| e.to_string())? {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => {
                return Ok(Cell::Gap(format!("{}: {reason}", reason.code())));
            }
        };
        // A RAW's boundary holds its values as `f32`, as the worker's boundary job writes it.
        let format = match &proxied {
            PreviewSource::Raw { .. } => BoundaryFormat::Float,
            PreviewSource::Jpeg(_) => BoundaryFormat::Half,
        };
        let held = boundary_as(format, width, height, 1, &texels).ok_or("a boundary")?;
        // The frame is the plan's output stage: the boundary's, or its geometry tail's.
        let output_stage = plan.geometry.output();
        let (out_width, out_height) = (output_stage.width, output_stage.height);
        if (cpu.width, cpu.height) != (out_width, out_height) {
            return Ok(Cell::Gap(format!(
                "the frame is {}x{}, not the plan's output {out_width}x{out_height}",
                cpu.width, cpu.height
            )));
        }
        // A warp's coordinate grid over the whole output stage, as the boundary's job computes it.
        let grid = match plan.geometry.affine() {
            Some(_) => None,
            None => plan
                .geometry
                .grid(
                    luxforge_core::Region {
                        x0: 0,
                        y0: 0,
                        width: out_width,
                        height: out_height,
                    },
                    1.0,
                )
                .map_err(|error| error.to_string())?,
        };
        let converted = match surface_plan_at(&plan, held, origin, grid.as_ref()) {
            Ok(converted) => converted,
            Err(reason) => {
                return Ok(Cell::Gap(format!(
                    "{}: the surface cannot run {reason:?} yet",
                    reason.code()
                )));
            }
        };
        // A mask that selects nothing on this source measures nothing about its coverage: the
        // corpus names such a cell a gap, not a pass. Its first operation's mask is read back
        // over the boundary it reads.
        if let Some(GpuStep::Masked(masked)) = converted.steps.first()
            && selects_nothing(qualifier, &converted.boundary, masked)?
        {
            return Ok(Cell::Gap(
                "the mask selects nothing on this source".to_owned(),
            ));
        }
        let charged = qualifier
            .charged_bytes(&converted)
            .map_err(|reason| format!("{reason:?}"))?;
        let drawn = qualifier.evaluate_codes(&converted)?;
        let gpu: Vec<u8> = drawn
            .iter()
            .flat_map(|code| [code[0], code[1], code[2]])
            .collect();
        let program = codes(
            qualifier
                .evaluate(&converted)?
                .iter()
                .map(|texel| [texel[0], texel[1], texel[2]]),
        );
        let reference: Vec<u8> = cpu
            .rgba
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        let (width, height) = (out_width, out_height);
        for (suffix, bytes) in [("gpu", &gpu), ("cpu", &reference)] {
            image::RgbImage::from_raw(width, height, bytes.clone())
                .ok_or("a whole frame")?
                .save(output.join(format!("{name}-{suffix}.png")))
                .map_err(|error| error.to_string())?;
        }
        let frame = |bytes| Rgb8::new(width, height, bytes);
        let statistics =
            preview_error::compare(frame(&gpu)?, frame(&reference)?, [0, 0, width, height])?;
        let program =
            preview_error::compare(frame(&program)?, frame(&reference)?, [0, 0, width, height])?;
        // The exact-derived frame settlement presents, written beside the pair, against the CPU's
        // moving proxy and the GPU frame: context, not judged.
        let settled = match settled_frame {
            Some(settled) => {
                if (settled.width, settled.height) != (width, height) {
                    return Err(format!(
                        "the settled frame is {}x{}, not the proxy's {width}x{height}",
                        settled.width, settled.height
                    ));
                }
                let settled: Vec<u8> = settled
                    .rgba
                    .chunks_exact(4)
                    .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                    .collect();
                image::RgbImage::from_raw(width, height, settled.clone())
                    .ok_or("a whole frame")?
                    .save(output.join(format!("{name}-settled.png")))
                    .map_err(|error| error.to_string())?;
                let rect = [0, 0, width, height];
                Some(Box::new(Settled {
                    proxy: preview_error::compare(frame(&reference)?, frame(&settled)?, rect)?,
                    gpu: preview_error::compare(frame(&gpu)?, frame(&settled)?, rect)?,
                }))
            }
            None => None,
        };
        Ok(Cell::Measured {
            stage: (width, height),
            proxy: is_proxy,
            passed: preview_error::verdict(&statistics, class).passed(),
            statistics,
            program,
            charged,
            settled,
        })
    })();
    finish(owner, join);
    let _ = std::fs::remove_file(&catalog);
    result
}

/// Whether `masked`'s mask covers no pixel of `boundary`, its operation's input: its coverage read
/// back through a unit that adds one to every channel, so the output less the input is the
/// coverage.
fn selects_nothing(
    qualifier: &Qualifier,
    boundary: &luxforge_ui::photo_surface::GpuBoundary,
    masked: &MaskedColour,
) -> Result<bool, String> {
    let add_one = GpuProgram::new(
        "lf_test_add_one",
        "fn lf_test_add_one(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
         -> vec3<f32> {\n    return rgb + vec3<f32>(1.0);\n}\n",
    );
    let plan = GpuPlan {
        boundary: boundary.clone(),
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::Masked(MaskedColour {
            units: vec![add_one],
            ..masked.clone()
        })],
    };
    let input = GpuPlan {
        steps: Vec::new(),
        ..plan.clone()
    };
    let (covered, held) = (qualifier.evaluate(&plan)?, qualifier.evaluate(&input)?);
    Ok(covered
        .iter()
        .zip(&held)
        .all(|(out, input)| out[1] == input[1]))
}

/// A headless qualifier, with the core's output encoding installed for its passes. `None`, having
/// printed that `test` was skipped, without an adapter.
pub(crate) fn headless(test: &str) -> Option<Qualifier> {
    assert!(
        super::gpu_plan::install_output_encoding(),
        "the surface holds the core's output encoding"
    );
    Qualifier::headless(test)
}

/// What a stack's first layer draws in its drafted GPU shape against its CPU shape's plan, over a
/// linear boundary of `pixels`.
#[derive(Debug)]
pub(crate) struct Drafted {
    /// The drafted shape's output values, bit for bit the boundary's.
    pub(crate) input: bool,
    /// The linear values that differ from the CPU shape's plan's, of how many, and the largest
    /// difference.
    pub(crate) values: usize,
    pub(crate) of: usize,
    pub(crate) largest: f32,
    /// The largest difference in the codes the two draw.
    pub(crate) code: u8,
}

/// [`Drafted`] for `recipe`'s first layer, planned for `request` (on the linear path), over a
/// `width × height` boundary of `pixels`.
pub(crate) fn drafted_against_cpu(
    qualifier: &Qualifier,
    recipe: &luxforge_core::Recipe,
    request: GpuPlanRequest,
    (width, height): (u32, u32),
    pixels: &[[f32; 3]],
) -> Drafted {
    let registry = luxforge_core::ModuleRegistry::builtin();
    let draw = |request: GpuPlanRequest| {
        let plan = match gpu_plan(&registry, recipe, request).expect("the stack compiles") {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
        let input = boundary(width, height, 1, pixels).expect("a boundary");
        let plan = super::gpu_plan::surface_plan(&plan, input).expect("a runnable plan");
        (
            qualifier.evaluate(&plan).expect("a readback"),
            qualifier.evaluate_codes(&plan).expect("a readback"),
        )
    };
    let (drafted, drafted_codes) = draw(request.drafted(0));
    let (cpu, cpu_codes) = draw(request);
    let values = |texels: &[[f32; 4]]| -> Vec<f32> {
        texels
            .iter()
            .flat_map(|texel| texel[..3].to_vec())
            .collect()
    };
    let (drafted, cpu) = (values(&drafted), values(&cpu));
    let pairs = || drafted.iter().zip(&cpu);
    Drafted {
        input: drafted
            .iter()
            .zip(pixels.iter().flatten())
            .all(|(drawn, given)| drawn.to_bits() == given.to_bits()),
        values: pairs().filter(|(a, b)| a.to_bits() != b.to_bits()).count(),
        of: drafted.len(),
        largest: pairs().map(|(a, b)| (a - b).abs()).fold(0.0, f32::max),
        code: drafted_codes
            .iter()
            .zip(&cpu_codes)
            .flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c])))
            .max()
            .unwrap_or(0),
    }
}

/// The qualification corpus's recipes of `families` at Fit: for each source this host has, the CPU
/// frame the preview worker renders and the GPU frame of the same plan over the same source the
/// worker rendered from, written as `<recipe>--<source>-{cpu,gpu}.png` with the commands that run
/// `cargo xtask preview-error --class CLASS` over each pair, each recipe held to its own class's
/// limits. A RAW photograph's first open commits its lens profile, whose warp the geometry tail
/// draws through its coordinate grid. A cell the surface cannot run yet is a gap with its reason,
/// never a pass.
///
/// Reads `LUXFORGE_GPU_CORPUS_OUTPUT` (a new directory), `LUXFORGE_GENERATED_FIXTURES` (the
/// generated JPEGs, `fixtures/generated` by default) and, for the RAWs, `LUXFORGE_RAW_MANIFEST`.
/// Without an adapter it prints that `test` was skipped.
pub(crate) fn corpus_at_fit(test: &str, families: &[&str]) {
    let output = std::path::PathBuf::from(
        std::env::var("LUXFORGE_GPU_CORPUS_OUTPUT").expect("LUXFORGE_GPU_CORPUS_OUTPUT"),
    );
    assert!(
        !output.exists(),
        "{} exists: use a new directory",
        output.display()
    );
    let Some(qualifier) = headless(test) else {
        return;
    };
    std::fs::create_dir_all(&output).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &std::path::Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let corpus = read(&root.join("fixtures/preview/corpus.json"));
    let generated = std::env::var("LUXFORGE_GENERATED_FIXTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("fixtures/generated"));
    let manifest = std::env::var("LUXFORGE_RAW_MANIFEST")
        .ok()
        .map(|path| read(std::path::Path::new(&path)));
    let sources = corpus_sources(&corpus, &generated, manifest.as_ref());
    eprintln!(
        "{test}: adapter {}, Fit bounds {:?}",
        qualifier.adapter(),
        fit_bounds()
    );
    let (mut cells, mut commands, mut missed) = (Vec::new(), Vec::new(), Vec::new());
    for recipe in corpus["recipes"].as_array().expect("recipes") {
        let family = recipe["family"].as_str().unwrap_or_default();
        if !families.contains(&family) {
            continue;
        }
        let class =
            Class::parse(recipe["class"].as_str().unwrap_or_default()).expect("a recipe's class");
        let steps = recipe["steps"].as_array().expect("steps").clone();
        for source in &sources {
            if !recipe["sources"]
                .as_array()
                .expect("sources")
                .iter()
                .any(|id| id == source.id.as_str())
            {
                continue;
            }
            {
                let name = format!("{}--{}", recipe["id"].as_str().unwrap(), source.id);
                let cell = corpus_cell(&qualifier, source, &steps, &output, &name, class)
                    .unwrap_or_else(|error| Cell::Gap(format!("failed: {error}")));
                match &cell {
                    Cell::Measured {
                        stage: proxy,
                        proxy: is_proxy,
                        statistics,
                        program,
                        passed,
                        charged,
                        settled,
                    } => {
                        eprintln!(
                            "{name} at {}x{}{}: drawn {} | program {} | charged {charged} B{}",
                            proxy.0,
                            proxy.1,
                            if *is_proxy { "" } else { " (exact)" },
                            figures(statistics),
                            figures(program),
                            if *passed { "" } else { " MISS" }
                        );
                        if let Some(settled) = settled {
                            eprintln!(
                                "{name}: against the exact-derived frame it settles to: CPU proxy \
                                 {} | GPU {}",
                                figures(&settled.proxy),
                                figures(&settled.gpu)
                            );
                        }
                        commands.push(format!(
                            "cargo xtask preview-error --candidate {dir}/{name}-gpu.png \
                             --reference {dir}/{name}-cpu.png --photo-rect 0,0,{w},{h} \
                             --class {class} --output {dir}/{name}.json",
                            class = class.name(),
                            dir = output.display(),
                            w = proxy.0,
                            h = proxy.1
                        ));
                        if !passed {
                            missed.push(name.clone());
                        }
                        let stats = |s: &Statistics| {
                            json!({
                                "mean": s.mean, "worst_block": s.worst_block, "p99": s.p99,
                                "mean_delta_l": s.mean_delta_l, "max": s.max
                            })
                        };
                        let mut cell = json!({
                            "cell": name, "class": class.name(),
                            "stage": [proxy.0, proxy.1], "proxy": is_proxy,
                            "drawn": stats(statistics), "program": stats(program),
                            "passed": passed, "charged_bytes": charged
                        });
                        if let Some(settled) = settled {
                            cell["settled_from_exact"] = json!({
                                "cpu_proxy": stats(&settled.proxy),
                                "gpu": stats(&settled.gpu)
                            });
                        }
                        cells.push(cell);
                    }
                    Cell::Gap(reason) => {
                        eprintln!("{name}: gap: {reason}");
                        cells.push(json!({"cell": name, "gap": reason}));
                    }
                }
            }
        }
    }
    std::fs::write(
        output.join("cells.json"),
        serde_json::to_string_pretty(&json!({
            "adapter": qualifier.adapter(),
            "bounds": [fit_bounds().width, fit_bounds().height],
            "cells": cells
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(output.join("commands.sh"), commands.join("\n") + "\n").unwrap();
    eprintln!("{test}: {} cells in {}", cells.len(), output.display());
    assert!(
        missed.is_empty(),
        "cells missing their class's limits: {missed:?}"
    );
}
