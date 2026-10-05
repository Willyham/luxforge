//! The per-frame light's reduction factor (`docs/specs/performance.md`, "The per-frame light's
//! reduction factor"): the measurement that chooses the one free parameter of stage 3's estimate
//! twin (`tasks/gpu-first.json`, TASK-005). The twin computes Dehaze's atmospheric light every
//! frame from the stack compiled at a block stage — the content stage reduced by a factor f per
//! side, f dividing 16 — the colour run over each f × f cell's mean, each 16 px block's cells
//! averaged, and the light selected as Dehaze prepares it. Where the colour run clips or crushes,
//! the colour of a cell's mean is not the mean of its pixels' colour, so the light misses the
//! reference's by that gap, which finer cells narrow.
//!
//! Each candidate light is emulated on the CPU (`luxforge_core::qualification::twin_lights`): the
//! twin's at f = 16, 8, 4 and 2; at f = 1, a control, where the cells are pixels and the colour run
//! is the CPU's over every pixel, so for a colour run alone the light is the reference's but for
//! the byte path's 16-bit hand-off; and, where the stage a light reads passes through Detail, the
//! same with Detail left out, which is what the twin at f = 16 nearly computes (Detail at a
//! sixteenth of its scale barely filters). Each is handed to the GPU plan's light planes in place
//! of the light its light links compute (`Qualifier::set_lights`), with the reference's own exact
//! light as the floor. The frame drawn with it is judged against the
//! reference frame of the view by the stack's class's limits, as the release gate judges the
//! picture at rest:
//!
//! - **100%**: the region plan from the stack's first pixel layer over the visible region of the
//!   largest window the owner's display holds, against that region of the reference frame;
//! - **Fit**: the process-first frame — the whole output stage drawn as 100% region plans over
//!   2048 px tiles, every tile reading the same light, stitched — and the reference frame, each
//!   reduced to the Fit frame's size by an area-weighted average of its linear light.
//!
//! The light's own error against the reference's exact light, per channel as a fraction of it, is
//! recorded beside the frame's figures. A measurement of pixels, not of time.
use super::{
    Drag, basic, basic_moderate, cropped_drags, detail_sharpen, drags, light, presence, step,
};
use crate::app::gpu_qualification::{
    CorpusSource, Opened, corpus_sources, figures, first_pixel_layer, fit_bounds, headless,
    largest_view,
};
use luxforge_core::{
    Cancel, EffectStage, Evaluation, GpuAnswer, GpuPlanRequest, ModuleRegistry, PRESENCE_EFFECT,
    PreviewRequest, Region, Render, RenderContext, RenderOptions, qualification, render,
};
use luxforge_reference::{
    preview_error::{self, Class, Rgb8, Statistics},
    tolerance,
};
use luxforge_ui::photo_surface::{
    GpuBoundary,
    gpu_preview::{GPU_PREVIEW_BUDGET, qualification::Qualifier},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The factors the twin is emulated at, coarsest first: the four the plan measures, then 1, the
/// control, whose cells are pixels.
const FACTORS: [u32; 5] = [16, 8, 4, 2, 1];

/// The side of the tiles a process-first frame is drawn in, as the release gate draws it.
const TILE: u32 = 2048;

/// Every stack measured here is held to the spatial limits: each holds a Presence layer.
const CLASS: Class = Class::Spatial;

/// The view a cell is judged at.
#[derive(Clone, Copy, Debug, PartialEq)]
enum View {
    /// The visible region of the largest window at 100%.
    Region,
    /// The process-first frame at the Fit frame's size.
    Fit,
}

impl View {
    fn name(self) -> &'static str {
        match self {
            Self::Region => "100%",
            Self::Fit => "fit",
        }
    }
}

/// The cells added to the drags measured at 100% behind Detail: the colour drags under Dehaze with
/// no Detail before them, where the twin's light is its colour run's alone; sharpen stress under
/// Dehaze at −100 alone; a Basic layer under Dehaze masked by a luminance range, whose coverage the
/// twin reads from a cell's mean; and a masked Dehaze layer after a global Presence layer (the
/// `viewport-fallback` stack's shape), whose light reads the global layer's output.
fn added_drags(raw: bool) -> Vec<Drag> {
    let plus3 = || basic(json!({"exposure": 3.0, "whites": 100.0, "blacks": 100.0}));
    let minus3 = || basic(json!({"exposure": -3.0, "whites": -100.0, "blacks": -100.0}));
    let range = || {
        step(
            "mask.create-luminance-range",
            json!({"low": 20.0, "low_feather": 10.0, "high": 80.0, "high_feather": 10.0}),
        )
    };
    let masked = |mut params: Value| {
        params["mask"] = json!({"name": "Mask 1"});
        basic(params)
    };
    let radial = || {
        step(
            "mask.create-radial",
            json!({"x": 0.5, "y": 0.5, "radius_x": 0.28, "radius_y": 0.22, "angle": 18.0,
                "feather": 45.0}),
        )
    };
    let masked_dehaze = |amount: i32| {
        step(
            "edit.set-presence",
            json!({"mask": {"name": "Mask 1"}, "dehaze": amount}),
        )
    };
    let drag = |id, committed: Vec<Value>, drafted| Drag {
        id,
        committed,
        drafted,
    };
    let chained = |amount| vec![presence(50, 50, 30), radial(), masked_dehaze(amount)];
    vec![
        drag(
            "basic-moderate-alone",
            vec![presence(50, 50, 30)],
            basic_moderate(raw),
        ),
        drag(
            "basic-plus3-strong-alone",
            vec![presence(100, 100, 100)],
            plus3(),
        ),
        drag(
            "basic-minus3-negative-alone",
            vec![presence(-100, -100, -100)],
            minus3(),
        ),
        drag(
            "detail-sharpen-dehaze-negative",
            vec![presence(0, 0, -100)],
            detail_sharpen(),
        ),
        drag(
            "range-masked-basic-under-dehaze",
            vec![presence(0, 0, 100), range()],
            masked(json!({"exposure": 0.8, "contrast": 20.0, "vibrance": 30.0})),
        ),
        drag(
            "range-masked-plus3-under-dehaze",
            vec![presence(0, 0, 100), range()],
            masked(json!({"exposure": 3.0, "whites": 100.0, "blacks": 100.0})),
        ),
        drag(
            "range-masked-minus3-under-negative",
            vec![presence(0, 0, -100), range()],
            masked(json!({"exposure": -3.0, "whites": -100.0, "blacks": -100.0})),
        ),
        drag(
            "masked-dehaze-after-presence",
            chained(100),
            basic_moderate(raw),
        ),
        drag("masked-dehaze-after-presence-plus3", chained(100), plus3()),
        drag(
            "masked-negative-dehaze-after-presence-minus3",
            chained(-100),
            minus3(),
        ),
    ]
}

/// One candidate: its name and the light it hands each estimating layer.
struct Candidate {
    name: String,
    lights: Vec<Option<Vec<f64>>>,
}

/// What one candidate's frame draws over its view.
struct Drawn {
    codes: Vec<u8>,
    charged: u64,
    shape: Option<&'static str>,
}

fn stage_of(registry: &ModuleRegistry, layer: &luxforge_core::Layer) -> Option<EffectStage> {
    registry
        .effect(&layer.effect_id)
        .map(|(_, effect)| effect.stage)
}

/// The four statistics and the largest difference, with where the worst block is.
fn statistics(s: &Statistics) -> Value {
    json!({
        "pixels": s.pixels,
        "mean": s.mean,
        "worst_block": s.worst_block,
        "worst_block_origin": s.worst_block_origin,
        "p99": s.p99,
        "mean_delta_l": s.mean_delta_l,
        "max": s.max,
    })
}

/// The rows of a frame of `rows` rows in one band per core.
fn bands(rows: u32) -> Vec<std::ops::Range<u32>> {
    let threads = std::thread::available_parallelism().map_or(8, std::num::NonZero::get) as u32;
    let size = rows.div_ceil(threads).max(1);
    (0..rows)
        .step_by(size as usize)
        .map(|start| start..(start + size).min(rows))
        .collect()
}

/// `pixels`, a `from` frame of `stride` bytes a pixel, reduced to `to` by the reference's
/// area-weighted average of linear light, in bands of output rows across the host's cores.
fn reduce(
    pixels: &[u8],
    stride: usize,
    from: (u32, u32),
    to: (u32, u32),
) -> Result<Vec<u8>, String> {
    let parts = std::thread::scope(|scope| {
        bands(to.1)
            .into_iter()
            .map(|rows| {
                scope.spawn(move || tolerance::reduce_srgb8_rows(pixels, stride, from, to, rows))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|band| band.join().expect("a band of the reduction"))
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(parts.concat())
}

/// `candidate` against `reference`, two `size` frames of three bytes a pixel, by the preview error
/// statistics over the whole frame, the differences taken in bands of rows across the host's cores.
fn compare(candidate: &[u8], reference: &[u8], size: (u32, u32)) -> Result<Statistics, String> {
    let (width, height) = size;
    let (candidate, reference) = (
        Rgb8::new(width, height, candidate)?,
        Rgb8::new(width, height, reference)?,
    );
    let photo = [0, 0, width, height];
    let parts = std::thread::scope(|scope| {
        bands(height)
            .into_iter()
            .map(|rows| {
                scope.spawn(move || preview_error::differences(candidate, reference, photo, rows))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|band| band.join().expect("a band of the comparison"))
            .collect::<Result<Vec<_>, _>>()
    })?;
    let pixels = width as usize * height as usize;
    let (mut delta_e, mut delta_l) = (Vec::with_capacity(pixels), Vec::with_capacity(pixels));
    for (e, l) in parts {
        delta_e.extend(e);
        delta_l.extend(l);
    }
    preview_error::statistics_of(width as usize, height as usize, &delta_e, &delta_l)
}

/// The size of the Fit frame of `opened`'s stack: the preview worker's moving frame at the Fit
/// bounds of an evidence run's window, its proxy, or its exact frame where no smaller proxy fits.
fn fit_size(opened: &Opened) -> Result<(u32, u32), String> {
    use luxforge_core::{PhaseOutcome, PreviewIntent, PreviewQueue};
    let mut job = crate::app::tasks::ready_preview_job(
        &opened.owner,
        PreviewRequest::new(opened.client, opened.asset.clone()).proxy(fit_bounds()),
    )?;
    job.intent = PreviewIntent::Interactive;
    let mut queue = PreviewQueue::default();
    let generation = queue.request(job);
    luxforge_testbase::wait_for("the Fit frame", || {
        let result = queue.poll()?;
        if result.generation != generation {
            return None;
        }
        match result.outcome {
            PhaseOutcome::Proxy(proxy) => Some(Ok((proxy.raster.width, proxy.raster.height))),
            PhaseOutcome::Exact(exact) => Some(
                exact
                    .result
                    .map(|raster| (raster.width, raster.height))
                    .map_err(|error| error.to_string()),
            ),
            _ => None,
        }
    })
}

/// `lights`, one for each estimating layer in recipe order, as a plan's light planes hold them:
/// what a plan drawn with them reads in place of the lights its light links compute.
fn planes_of(lights: &[Option<Vec<f64>>]) -> Result<Vec<[f32; 4]>, String> {
    lights
        .iter()
        .map(|light| match light.as_deref() {
            Some([r, g, b]) => Ok([*r as f32, *g as f32, *b as f32, 1.0]),
            _ => Err("the candidate prepared no light".to_owned()),
        })
        .collect()
}

/// The plan from `evaluation`'s first pixel layer over `rect` of its output stage at 100%, drawn on
/// the device over the boundary the worker renders for that region from `exact`, its light planes
/// holding `lights` — the release gate's region plan (`gpu_qualification::draw_region`), with the
/// lights handed rather than its light links' own. A restoration or spatial first layer
/// draws its GPU shape, every unit, while that slot fits the budget, else its CPU shape. `Err`
/// names why nothing was drawn: a gap, never a pass.
fn draw(
    qualifier: &Qualifier,
    evaluation: &Evaluation,
    exact: &Render<'_>,
    lights: &[[f32; 4]],
    raw: bool,
    rect: Region,
) -> Result<Drawn, String> {
    let (registry, recipe) = (evaluation.registry(), evaluation.recipe());
    let boundary_layer = first_pixel_layer(registry, recipe)?;
    let format = if raw {
        luxforge_core::BoundaryFormat::Float
    } else {
        luxforge_core::BoundaryFormat::Half
    };
    let frame = qualification::region_boundary(
        exact,
        boundary_layer,
        [rect.x0, rect.y0, rect.width, rect.height],
        format,
    )
    .map_err(|error| format!("no boundary: {error}"))?;
    let request = GpuPlanRequest::exact(boundary_layer, frame.stage).qualifying();
    let request = if raw { request.linear() } else { request };
    let spatial = matches!(
        stage_of(registry, &recipe.layers[boundary_layer]),
        Some(EffectStage::Restoration | EffectStage::Spatial)
    );
    let shapes = if spatial {
        vec![
            (request.drafted(boundary_layer), Some("gpu")),
            (request, Some("cpu")),
        ]
    } else {
        vec![(request, None)]
    };
    let mut over = Vec::new();
    for (request, shape) in shapes {
        let plan = match luxforge_core::gpu_plan(registry, recipe, request)
            .map_err(|error| error.to_string())?
        {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => return Err(format!("{}: {reason}", reason.code())),
        };
        if plan.lights.len() != lights.len() {
            return Err(format!(
                "the plan reads {} lights, not the {} handed",
                plan.lights.len(),
                lights.len()
            ));
        }
        let held = GpuBoundary::new(
            frame.texels.clone(),
            frame.width,
            frame.height,
            1,
            crate::app::gpu_plan::boundary_format(frame.format),
        )
        .ok_or("a boundary")?;
        let grid = plan
            .geometry
            .grid(rect, 1.0)
            .map_err(|error| error.to_string())?
            .map(|grid| crate::app::gpu_plan::WarpGrid::new(&grid));
        let converted = crate::app::gpu_plan::surface_plan_over(
            &plan,
            held,
            frame.origin,
            grid.as_ref(),
            Some(rect),
        )
        .map_err(|reason| format!("{}: the surface cannot run {reason:?}", reason.code()))?;
        let charged = qualifier
            .charged_bytes(&converted)
            .map_err(|reason| format!("{reason:?}"))?;
        if charged > GPU_PREVIEW_BUDGET {
            over.push(format!(
                "{charged} B in the {} shape",
                shape.unwrap_or("only")
            ));
            continue;
        }
        qualifier.set_lights(lights.to_vec());
        let codes = qualifier
            .evaluate_codes(&converted)?
            .iter()
            .flat_map(|code| [code[0], code[1], code[2]])
            .collect();
        return Ok(Drawn {
            codes,
            charged,
            shape,
        });
    }
    Err(format!(
        "budget-exceeded: {} of the {GPU_PREVIEW_BUDGET} B budget",
        over.join(" and ")
    ))
}

/// The process-first frame of `evaluation`'s whole output stage of `size`: [`draw`] over a grid
/// of [`TILE`]-sided tiles, every tile reading `lights`, stitched.
fn process_first(
    qualifier: &Qualifier,
    evaluation: &Evaluation,
    exact: &Render<'_>,
    lights: &[[f32; 4]],
    raw: bool,
    (width, height): (u32, u32),
) -> Result<Drawn, String> {
    let mut frame = Drawn {
        codes: vec![0; width as usize * height as usize * 3],
        charged: 0,
        shape: None,
    };
    for y0 in (0..height).step_by(TILE as usize) {
        for x0 in (0..width).step_by(TILE as usize) {
            let rect = Region {
                x0,
                y0,
                width: TILE.min(width - x0),
                height: TILE.min(height - y0),
            };
            let drawn = draw(qualifier, evaluation, exact, lights, raw, rect)
                .map_err(|error| format!("the tile at ({x0}, {y0}): {error}"))?;
            let row = rect.width as usize * 3;
            for (y, codes) in drawn.codes.chunks_exact(row).enumerate() {
                let start = ((y0 as usize + y) * width as usize + x0 as usize) * 3;
                frame.codes[start..start + row].copy_from_slice(codes);
            }
            frame.charged = frame.charged.max(drawn.charged);
            frame.shape = frame.shape.or(drawn.shape);
        }
    }
    Ok(frame)
}

/// Each layer's light against the exact one, per channel as a fraction of it, and the largest
/// magnitude among them.
fn light_errors(lights: &[Option<Vec<f64>>], exact: &[Option<Vec<f64>>]) -> (Value, f64) {
    let mut largest = 0.0_f64;
    let errors = lights
        .iter()
        .zip(exact)
        .map(|(light, exact)| match (light, exact) {
            (Some(light), Some(exact)) => {
                let error: Vec<f64> = (0..3)
                    .map(|channel| (light[channel] - exact[channel]) / exact[channel])
                    .collect();
                largest = error.iter().fold(largest, |a, b| a.max(b.abs()));
                json!(error)
            }
            _ => Value::Null,
        })
        .collect();
    (Value::Array(errors), largest)
}

/// One stack, `steps` applied to `source`, at `view`: every candidate light's frame against the
/// reference frame of the view, and each light's error against the reference's own.
fn cell(
    qualifier: &Qualifier,
    source: &CorpusSource,
    steps: &[Value],
    view: View,
    output: &Path,
    name: &str,
) -> Result<Value, String> {
    let started = std::time::Instant::now();
    let catalog = output.join("catalogs").join(format!("{name}.sqlite"));
    let opened = Opened::new(source, steps, &catalog)?;
    let job = crate::app::tasks::ready_preview_job(
        &opened.owner,
        PreviewRequest::new(opened.client, opened.asset.clone()),
    )?;
    let evaluation = job.evaluation;
    let (registry, recipe) = (evaluation.registry(), evaluation.recipe());
    // The reference frame: the stack's exact whole frame, whose render prepares each light from
    // the whole stage at full resolution.
    let context = RenderContext::new();
    let exact = render(
        registry,
        evaluation.source(),
        recipe,
        RenderOptions::exact(&Cancel::never()),
        &context,
    )
    .map_err(|error| error.to_string())?;
    let whole = exact
        .frame(evaluation.entry().snapshot.id.clone())
        .map_err(|error| error.to_string())?;
    let stage = (whole.width, whole.height);
    let mut estimating = Vec::new();
    let mut templates = Vec::new();
    for (layer, at) in recipe.layers.iter().enumerate() {
        if at.effect_id != PRESENCE_EFFECT {
            continue;
        }
        if let Ok(Some(estimates)) = qualification::held_estimates(&exact, layer)
            && light(&estimates).is_some()
        {
            estimating.push(layer);
            templates.push(estimates);
        }
    }
    let Some(&last) = estimating.last() else {
        return Err("no layer prepares a light".into());
    };
    let exact_lights: Vec<Option<Vec<f64>>> = templates
        .iter()
        .map(|estimates| light(estimates).map(|light| light.to_vec()))
        .collect();
    let restored = recipe.layers[..last]
        .iter()
        .any(|layer| stage_of(registry, layer) == Some(EffectStage::Restoration));
    let mut candidates = vec![Candidate {
        name: "exact".into(),
        lights: exact_lights.clone(),
    }];
    let twin = |skip| {
        qualification::twin_lights(
            registry,
            evaluation.source().into(),
            recipe,
            &estimating,
            &FACTORS,
            skip,
        )
        .map_err(|error| error.to_string())
    };
    for (factor, lights) in FACTORS.iter().zip(twin(false)?) {
        candidates.push(Candidate {
            name: format!("f{factor}"),
            lights,
        });
    }
    if restored {
        for (factor, lights) in FACTORS.iter().zip(twin(true)?) {
            candidates.push(Candidate {
                name: format!("detail-skipped-f{factor}"),
                lights,
            });
        }
    }
    // The reference frame at the view.
    let (size, region, reference) = match view {
        View::Region => {
            let rect = largest_view(stage, 100.0).ok_or("no visible region")?;
            let reference = tolerance::region_srgb8(
                &whole.rgba,
                4,
                whole.width,
                [rect.x0, rect.y0, rect.width, rect.height],
            )?;
            ((rect.width, rect.height), Some(rect), reference)
        }
        View::Fit => {
            let size = fit_size(&opened)?;
            (size, None, reduce(&whole.rgba, 4, stage, size)?)
        }
    };
    drop(whole);
    let frame_of = |lights: &[[f32; 4]]| -> Result<Drawn, String> {
        match region {
            Some(rect) => draw(qualifier, &evaluation, &exact, lights, opened.raw, rect),
            None => {
                let full =
                    process_first(qualifier, &evaluation, &exact, lights, opened.raw, stage)?;
                Ok(Drawn {
                    codes: reduce(&full.codes, 3, stage, size)?,
                    ..full
                })
            }
        }
    };
    let mut measured = Vec::new();
    let mut repeat = Value::Null;
    for candidate in &candidates {
        let (errors, largest) = light_errors(&candidate.lights, &exact_lights);
        let mut record = json!({
            "candidate": candidate.name,
            "lights": candidate.lights,
            "light_error": errors,
            "light_error_largest": largest,
        });
        let drawn = planes_of(&candidate.lights)
            .and_then(|lights| frame_of(&lights).map(|drawn| (drawn, lights)));
        match drawn {
            Ok((drawn, lights)) => {
                let against = compare(&drawn.codes, &reference, size)?;
                let passed = preview_error::verdict(&against, CLASS).passed();
                eprintln!(
                    "{name} at {} {}: {} | light error {largest:.5}{}",
                    view.name(),
                    candidate.name,
                    figures(&against),
                    if passed { "" } else { " PAST" }
                );
                // The same light drawn again draws the same frame.
                if candidate.name == "f16" {
                    let again = frame_of(&lights)?;
                    repeat = json!(again.codes == drawn.codes);
                }
                record["against_reference"] = statistics(&against);
                record["passed"] = json!(passed);
                record["charged_bytes"] = json!(drawn.charged);
                record["shape"] = json!(drawn.shape);
            }
            Err(reason) => {
                eprintln!(
                    "{name} at {} {}: gap: {reason}",
                    view.name(),
                    candidate.name
                );
                record["gap"] = json!(reason);
            }
        }
        measured.push(record);
    }
    let layers: Vec<Value> = recipe
        .layers
        .iter()
        .map(|layer| json!({"effect": layer.effect_id, "masked": layer.mask.is_some()}))
        .collect();
    Ok(json!({
        "cell": name,
        "view": view.name(),
        "source": source.id,
        "stage": [stage.0, stage.1],
        "frame": [size.0, size.1],
        "region": region.map(|rect| [rect.x0, rect.y0, rect.width, rect.height]),
        "layers": layers,
        "estimating": estimating,
        "detail_in_stage": restored,
        "repeat_identical": repeat,
        "candidates": measured,
        "clock_s": started.elapsed().as_secs_f64(),
    }))
}

/// The worst of each statistic over a candidate's cells, with the cell it is worst in, how many
/// cells are within the limits and which are past them or gaps.
#[derive(Default)]
struct Tally {
    within: usize,
    past: Vec<String>,
    gaps: Vec<String>,
    /// Mean, worst block, p99 and |signed mean ΔL*|, each with its cell.
    worst: [(f64, String); 4],
    light: (f64, String),
}

impl Tally {
    fn add(&mut self, cell: &str, candidate: &Value) {
        let largest = candidate["light_error_largest"].as_f64().unwrap_or(0.0);
        if largest > self.light.0 {
            self.light = (largest, cell.to_owned());
        }
        if candidate.get("gap").is_some() {
            self.gaps.push(cell.to_owned());
            return;
        }
        let s = &candidate["against_reference"];
        let values = [
            s["mean"].as_f64().unwrap_or(f64::NAN),
            s["worst_block"].as_f64().unwrap_or(f64::NAN),
            s["p99"].as_f64().unwrap_or(f64::NAN),
            s["mean_delta_l"].as_f64().unwrap_or(f64::NAN).abs(),
        ];
        for (worst, value) in self.worst.iter_mut().zip(values) {
            if value > worst.0 || worst.1.is_empty() {
                *worst = (value, cell.to_owned());
            }
        }
        if candidate["passed"] == true {
            self.within += 1;
        } else {
            self.past.push(cell.to_owned());
        }
    }

    fn json(&self) -> Value {
        let worst = |(value, cell): &(f64, String)| json!({"value": value, "cell": cell});
        json!({
            "within": self.within,
            "past": self.past,
            "gaps": self.gaps,
            "worst": {
                "mean": worst(&self.worst[0]),
                "worst_block": worst(&self.worst[1]),
                "p99": worst(&self.worst[2]),
                "abs_mean_delta_l": worst(&self.worst[3]),
            },
            "light_error_largest": worst(&self.light),
        })
    }
}

/// `document` written to `output/cells.json` through a temporary file, so a reader never sees half
/// of it.
fn write(output: &Path, document: &Value) {
    let staged = output.join("cells.json.partial");
    std::fs::write(&staged, serde_json::to_string_pretty(document).unwrap()).unwrap();
    std::fs::rename(&staged, output.join("cells.json")).unwrap();
}

/// Every candidate light for the per-frame estimate twin over every source this host has: at 100%
/// the drags [`drags`] measures behind Detail and [`added_drags`], over the sources the corpus
/// measures Detail beside Presence on; at Fit the drags behind the corpus's straightened crop
/// ([`cropped_drags`]), over every corpus source. Writes `cells.json` to
/// `LUXFORGE_GPU_CORPUS_OUTPUT`, rewritten after every cell; `LUXFORGE_GPU_CORPUS_SOURCES`,
/// `LUXFORGE_DEHAZE_DRAGS` and `LUXFORGE_DEHAZE_VIEWS` (`100%`, `fit`), comma-separated, run only
/// those.
#[test]
#[ignore = "a measurement: set LUXFORGE_GPU_CORPUS_OUTPUT to a new directory, \
            LUXFORGE_GENERATED_FIXTURES to the generated JPEGs and, for the RAWs, \
            LUXFORGE_RAW_MANIFEST"]
fn gpu_dehaze_reduction_factor_candidates() {
    let test = "gpu_dehaze_reduction_factor_candidates";
    let output = PathBuf::from(
        std::env::var("LUXFORGE_GPU_CORPUS_OUTPUT").expect("LUXFORGE_GPU_CORPUS_OUTPUT"),
    );
    assert!(
        !output.exists(),
        "{} exists: use a new directory",
        output.display()
    );
    std::fs::create_dir_all(output.join("catalogs")).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let corpus = read(&root.join("fixtures/preview/corpus.json"));
    let generated = std::env::var("LUXFORGE_GENERATED_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("fixtures/generated"));
    let manifest = std::env::var("LUXFORGE_RAW_MANIFEST")
        .ok()
        .map(|path| read(Path::new(&path)));
    let only = |name: &str| -> Option<Vec<String>> {
        std::env::var(name)
            .ok()
            .map(|ids| ids.split(',').map(|id| id.trim().to_owned()).collect())
    };
    let sources_only = only("LUXFORGE_GPU_CORPUS_SOURCES");
    let drags_only = only("LUXFORGE_DEHAZE_DRAGS");
    let views: Vec<View> = [View::Region, View::Fit]
        .into_iter()
        .filter(|view| {
            only("LUXFORGE_DEHAZE_VIEWS").is_none_or(|views| views.iter().any(|v| v == view.name()))
        })
        .collect();
    let mut document = json!({
        "test": test,
        "adapter": null,
        "skipped": null,
        "factors": FACTORS,
        "class": CLASS.name(),
        "views": views.iter().map(|view| view.name()).collect::<Vec<_>>(),
        "cells": [],
        "summary": {},
        "complete": false,
    });
    let Some(qualifier) = headless(test) else {
        document["skipped"] =
            json!("no GPU adapter on this host: nothing was drawn and this is not GPU evidence");
        write(&output, &document);
        return;
    };
    document["adapter"] = json!(qualifier.adapter());
    eprintln!("{test}: adapter {}", qualifier.adapter());
    // The sources the corpus measures Detail beside Presence on.
    let chained: Vec<String> = corpus["recipes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|recipe| recipe["id"] == "detail-presence")
        .expect("the corpus's detail-presence recipe")["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect();
    let sources = corpus_sources(&corpus, &generated, manifest.as_ref());
    let mut tallies: BTreeMap<(String, String), Tally> = BTreeMap::new();
    for view in views {
        for source in &sources {
            if sources_only
                .as_ref()
                .is_some_and(|ids| !ids.contains(&source.id))
                || (view == View::Region && !chained.contains(&source.id))
            {
                continue;
            }
            let stacks: Vec<(&'static str, Vec<Value>)> = match view {
                View::Region => drags(source.raw)
                    .into_iter()
                    .chain(added_drags(source.raw))
                    .map(|drag| {
                        let mut steps = drag.committed;
                        steps.push(drag.drafted);
                        (drag.id, steps)
                    })
                    .collect(),
                View::Fit => cropped_drags()
                    .into_iter()
                    .map(|drag| {
                        let mut steps = drag.committed;
                        steps.push(drag.drafted);
                        (drag.id, steps)
                    })
                    .collect(),
            };
            for (id, steps) in stacks {
                if drags_only
                    .as_ref()
                    .is_some_and(|ids| !ids.iter().any(|wanted| wanted == id))
                {
                    continue;
                }
                let name = format!("{id}--{}--{}", source.id, view.name().replace('%', ""));
                let record = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    cell(&qualifier, source, &steps, view, &output, &name)
                }))
                .unwrap_or_else(|panic| {
                    Err(format!(
                        "the harness panicked: {}",
                        panic
                            .downcast_ref::<String>()
                            .cloned()
                            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                            .unwrap_or_else(|| "a panic".to_owned())
                    ))
                });
                let record = match record {
                    Ok(record) => {
                        for candidate in record["candidates"].as_array().unwrap() {
                            tallies
                                .entry((
                                    view.name().to_owned(),
                                    candidate["candidate"].as_str().unwrap().to_owned(),
                                ))
                                .or_default()
                                .add(&name, candidate);
                        }
                        record
                    }
                    Err(error) => {
                        eprintln!("{name}: gap: {error}");
                        json!({"cell": name, "view": view.name(), "source": source.id,
                            "gap": error})
                    }
                };
                document["cells"].as_array_mut().unwrap().push(record);
                document["summary"] = tallies
                    .iter()
                    .map(|((view, candidate), tally)| (format!("{view} {candidate}"), tally.json()))
                    .collect::<serde_json::Map<_, _>>()
                    .into();
                write(&output, &document);
            }
        }
    }
    for ((view, candidate), tally) in &tallies {
        eprintln!(
            "{view} {candidate}: within {}, past {}, gaps {}; worst mean {:.4} ({}), block {:.4} \
             ({}), p99 {:.4} ({}), |dL*| {:.4} ({}); light error {:.5} ({}); past: {:?}",
            tally.within,
            tally.past.len(),
            tally.gaps.len(),
            tally.worst[0].0,
            tally.worst[0].1,
            tally.worst[1].0,
            tally.worst[1].1,
            tally.worst[2].0,
            tally.worst[2].1,
            tally.worst[3].0,
            tally.worst[3].1,
            tally.light.0,
            tally.light.1,
            tally.past,
        );
    }
    document["complete"] = json!(true);
    write(&output, &document);
}
