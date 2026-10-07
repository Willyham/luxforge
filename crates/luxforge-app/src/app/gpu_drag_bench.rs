//! The drag-tick benchmark (`docs/specs/performance.md`, "Software adapters"): what a drag costs on
//! the GPU of a named adapter, beside what the reference renderer's whole frame costs per tick, on
//! the 24 MP generated JPEG, so the software-adapter fallback (`docs/design/gpu-first.md`) is
//! decided on measured figures rather than estimates.
//!
//! For each stack — Basic alone, an Exposure drag; Basic, Tone curve and the colour mixer, an
//! Exposure drag; Detail and Presence, a Clarity drag and a Detail drag, which draws Presence again
//! over Detail's new output; and three masked Presence layers, a Clarity drag of the last — it
//! records:
//!
//! - **Ticks** at Fit (the evidence window's bounds, [`super::gpu_qualification::fit_bounds`]) and
//!   at 100% (that window's visible region at full scale): a draft of the stack's dragged slider,
//!   one value a tick, planned by the catalog owner as the desktop plans a gesture's tick, outside
//!   the timing, then drawn through one slot of the photo surface's own drawing on a headless
//!   device ([`HeadlessSurface::ticks`]) with the change measured since the previous tick, each
//!   tick timed from handing the source to the GPU having finished it. The first tick is drawn
//!   untimed: it waits out the compile and the source's upload, as a drag's first tick does. The
//!   last tick's frame is held to the same plan drawn fresh, so a tick that drew less than it was
//!   handed cannot pass for a fast one.
//! - **The picture at rest at Fit**: the stack's tiles at full resolution reduced to the view, as
//!   the editor draws it once a drag ends, its programs compiled by an untimed first draw.
//! - **Export**: the stack's output stage streamed through the desktop's GPU tile worker on this
//!   host's adapter, as the export lane streams it, once cold and once warm.
//! - **The reference renderer's whole frame per tick**: the drafted stack's exact frame at full
//!   resolution, the frame a session without a GPU draws per tick.
//!
//! It writes `drag-ticks.json` and `drag-ticks.md` to `LUXFORGE_DRAG_BENCH_OUTPUT` (a new
//! directory, an absolute path) and judges nothing. Reads `LUXFORGE_GENERATED_FIXTURES` (`fixtures/generated` by
//! default), `LUXFORGE_DRAG_BENCH_TICKS` (timed GPU ticks a view, 40 by default),
//! `LUXFORGE_DRAG_BENCH_REFERENCE_TICKS` (reference frames a stack, 8 by default),
//! `LUXFORGE_DRAG_BENCH_STACKS` (stack ids, comma-separated) and `LUXFORGE_GPU_ADAPTER=software`,
//! which asks wgpu for the platform's software adapter alone. Release builds only:
//!
//! `LUXFORGE_DRAG_BENCH_OUTPUT=NEW_DIR cargo test --release --locked -p luxforge-app --bin luxforge
//! app::gpu_drag_bench::drag_tick_benchmark -- --ignored --exact --nocapture`
use super::gpu_plan::surface_plan_over;
use super::gpu_preview::{derived_now, gpu_source_of, rest_now};
use super::gpu_qualification::{CorpusSource, Opened, fit_bounds, headless};
use super::gpu_tiles::GpuTiles;
use luxforge_core::{
    Cancel, GpuAnswer, GpuPreview, GpuView, PreviewRequest, ProxyBounds, Region, RenderOptions,
    tiles::TileService,
};
use luxforge_gpu::{GpuChange, GpuPlan};
use luxforge_testkit::client::call;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A picture at rest or an export that takes longer than this is measured once, its first run,
/// rather than again warm: on a software adapter one can take minutes.
const LONG: Duration = Duration::from_secs(20);

/// One stack the benchmark drags: its corpus-style steps, the action its drag drafts, through the
/// mask named, and the slider it moves from `from` to `to`.
struct Stack {
    id: &'static str,
    title: &'static str,
    steps: Value,
    action: &'static str,
    mask: Option<&'static str>,
    field: &'static str,
    from: f64,
    to: f64,
}

/// The four stacks, in the order the report lists them.
fn stacks() -> Vec<Stack> {
    let basic = json!({"api": {"method": "edit.set-basic", "params": {
        "exposure": 0.5, "contrast": 25.0, "highlights": -30.0, "shadows": 30.0, "whites": -15.0,
        "blacks": 15.0, "vibrance": 30.0, "saturation": 15.0, "temperature": 20.0, "tint": -10.0}}});
    let curve = json!({"api": {"method": "edit.set-curve", "params": {
        "luminance": [[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]]}}});
    let mixer = json!({"api": {"method": "edit.set-mixer", "params": {
        "red-hue": 30, "orange-saturation": -40, "green-saturation": 40, "aqua-hue": -25,
        "blue-luminance": -30, "magenta-saturation": 25}}});
    let detail = json!({"api": {"method": "edit.set-detail", "params": {
        "luminance": 40, "colour": 40, "sharpening": 50, "radius": 1.0}}});
    let presence = json!({"api": {"method": "edit.set-presence", "params": {
        "texture": 50, "clarity": 50, "dehaze": 30}}});
    let masked = |name: &str, x: f64, y: f64| {
        [
            json!({"api": {"method": "mask.create-radial", "params": {
                "x": x, "y": y, "radius_x": 0.22, "radius_y": 0.18, "angle": 18.0,
                "feather": 45.0}}}),
            json!({"api": {"method": "edit.set-presence", "params": {
                "mask": {"name": name}, "texture": 50, "clarity": 50, "dehaze": 30}}}),
        ]
    };
    vec![
        Stack {
            id: "basic",
            title: "(a) Basic",
            steps: json!([basic]),
            action: "set-basic",
            mask: None,
            field: "exposure",
            from: 0.0,
            to: 1.0,
        },
        Stack {
            id: "basic-curve-mixer",
            title: "(b) Basic, Tone curve, colour mixer",
            steps: json!([basic, curve, mixer]),
            action: "set-basic",
            mask: None,
            field: "exposure",
            from: 0.0,
            to: 1.0,
        },
        Stack {
            id: "detail-presence",
            title: "(c) Detail and Presence (Texture, Clarity, Dehaze)",
            steps: json!([detail, presence]),
            action: "set-presence",
            mask: None,
            field: "clarity",
            from: 10.0,
            to: 90.0,
        },
        Stack {
            id: "detail-presence-detail-drag",
            title: "(c′) Detail and Presence, a Detail drag",
            steps: json!([detail, presence]),
            action: "set-detail",
            mask: None,
            field: "luminance",
            from: 10.0,
            to: 90.0,
        },
        Stack {
            id: "masked-presence-3",
            title: "(d) Three masked Presence layers",
            steps: Value::Array(
                [
                    masked("Mask 1", 0.3, 0.35),
                    masked("Mask 2", 0.7, 0.4),
                    masked("Mask 3", 0.5, 0.7),
                ]
                .concat(),
            ),
            action: "set-presence",
            mask: Some("Mask 3"),
            field: "clarity",
            from: 10.0,
            to: 90.0,
        },
    ]
}

/// One view the ticks are drawn at.
#[derive(Clone, Copy)]
enum View {
    Fit,
    Full,
}

impl View {
    fn id(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::Full => "100%",
        }
    }
}

/// The evidence window's photo surface, 1440 × 900 logical at 2× with both panels open, at 100%
/// over an output stage of `stage`, scrolled to its centre: the visible region a 100% drag draws.
fn full_view(stage: (u32, u32)) -> Option<Region> {
    let surface = crate::layout::photo_surface((1440.0, 900.0), true, true, false);
    let pan = (
        ((stage.0 as f32 / 2.0 - surface.0) / 2.0).max(0.0),
        ((stage.1 as f32 / 2.0 - surface.1) / 2.0).max(0.0),
    );
    super::preview::viewport_rect(
        stage,
        &luxforge_core::Zoom::Percent { value: 100.0 },
        2.0,
        surface,
        pan,
    )
}

/// `values` evenly from `from` to `to` and back, as a slider dragged to and fro, `count` of them.
fn sweep(from: f64, to: f64, count: usize) -> Vec<f64> {
    let half = count.div_ceil(2).max(2);
    (0..count)
        .map(|i| {
            let step = i % (2 * half - 2).max(1);
            let at = if step < half {
                step
            } else {
                2 * half - 2 - step
            };
            let value = from + (to - from) * at as f64 / (half - 1) as f64;
            (value * 100.0).round() / 100.0
        })
        .collect()
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// A run of timings as the report records it.
fn summary(ms: &[f64]) -> Value {
    let Some(distribution) = luxforge_testbase::Distribution::of(ms.iter().copied()) else {
        return json!({"count": 0});
    };
    json!({
        "count": distribution.count,
        "p50_ms": distribution.p50,
        "p95_ms": distribution.p95,
        "mean_ms": ms.iter().sum::<f64>() / ms.len() as f64,
        "max_ms": distribution.max,
        "ms": ms,
    })
}

/// The draft's GPU preview at `view`, the catalog owner's plan of the tick with `value` set.
fn tick_plan(
    opened: &Opened,
    draft: &Value,
    field: &str,
    value: f64,
    view: (ProxyBounds, Option<Region>),
) -> Result<GpuPreview, String> {
    call(
        &opened.owner,
        opened.client,
        "draft.set",
        json!({"draft_id": draft, "fields": {field: value}}),
    )?;
    let draft_id = serde_json::from_value(draft.clone()).map_err(|error| error.to_string())?;
    let (bounds, region) = view;
    let mut request = PreviewRequest::new(opened.client, opened.asset.clone())
        .draft(draft_id)
        .gpu_fit(bounds);
    if let Some(rect) = region {
        request = request.gpu_region(rect, 1.0);
    }
    let job = super::tasks::ready_preview_job(&opened.owner, request)?;
    job.gpu
        .map(|preview| *preview)
        .ok_or_else(|| "the job planned no GPU preview".to_owned())
}

/// A draft of `stack`'s action opened on `opened`, through its mask when it names one.
fn begin(opened: &Opened, stack: &Stack, recipe: &luxforge_core::Recipe) -> Result<Value, String> {
    let mut params = json!({"asset_id": opened.asset.as_str(), "action": stack.action});
    if let Some(name) = stack.mask {
        let mask = recipe
            .masks
            .iter()
            .find(|mask| mask.name == name)
            .ok_or_else(|| format!("no mask named {name}"))?;
        params["mask"] = json!(mask.id);
    }
    let begun = call(&opened.owner, opened.client, "draft.begin", params)?;
    Ok(begun["draft_id"].clone())
}

fn cancel(opened: &Opened, draft: &Value) {
    let _ = call(
        &opened.owner,
        opened.client,
        "draft.cancel",
        json!({"draft_id": draft}),
    );
}

/// The drag's ticks at `view`: `count + 1` plans, the first drawn untimed.
#[allow(clippy::too_many_arguments)]
fn ticks(
    qualifier: &luxforge_gpu::qualification::Qualifier,
    surface: &mut luxforge_gpu::headless::HeadlessSurface,
    opened: &Opened,
    stack: &Stack,
    recipe: &luxforge_core::Recipe,
    gpu: &luxforge_gpu::GpuSource,
    stage: (u32, u32),
    view: View,
    count: usize,
) -> Result<Value, String> {
    let region = match view {
        View::Fit => None,
        View::Full => Some(full_view(stage).ok_or("no visible region at 100%")?),
    };
    let draft = begin(opened, stack, recipe)?;
    let planned = (|| {
        let mut plans = Vec::with_capacity(count + 1);
        let mut plan_ms = Vec::with_capacity(count + 1);
        for value in sweep(stack.from, stack.to, count + 1) {
            let started = Instant::now();
            let preview = tick_plan(opened, &draft, stack.field, value, (fit_bounds(), region))?;
            plan_ms.push(ms(started.elapsed()));
            match preview {
                GpuPreview {
                    answer: GpuAnswer::Plan(plan),
                    boundary: Some(request),
                    ..
                } => plans.push((*plan, request)),
                GpuPreview {
                    answer: GpuAnswer::Fallback(reason),
                    ..
                } => {
                    return Err(format!(
                        "{}: the GPU does not draw this drag",
                        reason.code()
                    ));
                }
                GpuPreview { .. } => return Err("the drag's plan has no boundary".to_owned()),
            }
        }
        Ok((plans, plan_ms))
    })();
    cancel(opened, &draft);
    let (plans, plan_ms) = planned?;
    let (first, request) = &plans[0];
    let (boundary, origin, grid) = derived_now(gpu, first, request, 1)?;
    let region = request.key.region();
    let mut converted: Vec<(GpuPlan, Option<GpuChange>)> = Vec::with_capacity(plans.len());
    for (serial, (plan, _)) in plans.iter().enumerate() {
        let drawn = surface_plan_over(plan, boundary.clone(), origin, grid.as_ref(), region)
            .map_err(|unrunnable| format!("{}: not runnable", unrunnable.code()))?;
        // The change since the tick before, as the desktop measures it ([`super::gpu_preview`]).
        let change = (serial > 0).then(|| {
            let since = match plan.changes_since(&plans[serial - 1].0) {
                luxforge_core::GpuChange::Nothing => Some((serial as u64, [0; 4])),
                luxforge_core::GpuChange::Inside(rect) => Some((
                    serial as u64,
                    [
                        rect.x0,
                        rect.y0,
                        rect.x0 + rect.width,
                        rect.y0 + rect.height,
                    ],
                )),
                luxforge_core::GpuChange::Anywhere => None,
            };
            GpuChange {
                serial: serial as u64 + 1,
                since,
            }
        });
        converted.push((drawn, change));
    }
    let charged = qualifier
        .charged_bytes(&converted[0].0)
        .map_err(|reason| format!("{reason:?}"))?;
    let budget = luxforge_gpu::GPU_PREVIEW_BUDGET;
    let size = converted[0]
        .0
        .region
        .map_or(converted[0].0.boundary.size(), |region| region.size());
    let ticked = surface
        .ticks(gpu, &converted)
        .map_err(|fallback| format!("a tick fell back: {fallback:?}"))?;
    // The last tick, drawn through the slot that held every tick before it, against the same plan
    // drawn fresh: the ticks drew what they were handed, not a frame a change left stale.
    let (last, _) = converted.last().expect("a tick");
    let fresh = surface
        .draw(gpu, last)
        .map_err(|fallback| format!("the fresh draw fell back: {fallback:?}"))?;
    let tick_ms: Vec<f64> = ticked.times.into_iter().map(ms).collect();
    Ok(json!({
        "status": "measured",
        "size": [size.0, size.1],
        "region": region.map(|rect| [rect.x0, rect.y0, rect.width, rect.height]),
        "charged_bytes": charged,
        "within_budget": charged <= budget,
        "ticks": summary(&tick_ms),
        "last_tick_is_a_fresh_draw": ticked.last == fresh.codes,
        "planning": summary(&plan_ms[1..]),
    }))
}

/// The picture at rest at Fit: an untimed draw, then `runs` timed.
fn rest(
    surface: &mut luxforge_gpu::headless::HeadlessSurface,
    evaluation: &luxforge_core::Evaluation,
    gpu: &luxforge_gpu::GpuSource,
    runs: usize,
) -> Result<Value, String> {
    let rest = luxforge_core::qualification::rest_plan(evaluation, GpuView::Fit(fit_bounds()))
        .map_err(|error| error.to_string())?;
    let tiles = match rest.tiles {
        Some(Ok(tiles)) if tiles.reduction.is_some() => tiles,
        Some(Err(reason)) => return Err(format!("the tiles are not drawn: {}", reason.code())),
        _ => return Err("no reduced tiles at Fit".to_owned()),
    };
    let handed = rest_now(gpu, &tiles, 1)?;
    let mut draw = || {
        let started = Instant::now();
        surface
            .rest(gpu, &handed)
            .map(|_| ms(started.elapsed()))
            .map_err(|fallback| format!("the picture at rest: {fallback:?}"))
    };
    // A first draw that compiles; one past [`LONG`] is the figure itself, its compile a small
    // share of it, rather than drawn again.
    let first = draw()?;
    let drawn_ms = if first > ms(LONG) {
        vec![first]
    } else {
        (0..runs).map(|_| draw()).collect::<Result<_, _>>()?
    };
    Ok(json!({
        "status": "measured",
        "tiles": handed.tiles.len(),
        "first_includes_compile": first > ms(LONG),
        "drawn": summary(&drawn_ms),
    }))
}

/// `evaluation`'s output stage streamed through `worker`, the time it took and the bands taken.
fn export(worker: &GpuTiles, evaluation: &luxforge_core::Evaluation) -> Result<(f64, u64), String> {
    let started = Instant::now();
    let mut stream = worker
        .stream(evaluation, &Cancel::new())
        .map_err(|fallback| format!("the reference renders it: {fallback:?}"))?;
    let mut bands = 0;
    while let Some(band) = stream.next() {
        match band {
            Ok(_) => bands += 1,
            Err(error) => {
                return Err(match stream.fallback() {
                    Some(fallback) => format!("the reference renders it: {fallback:?}"),
                    None => format!("the stream failed: {error}"),
                });
            }
        }
    }
    Ok((started.elapsed().as_secs_f64(), bands))
}

/// The reference renderer's whole frame per tick: the drafted stack's exact frame, `count` ticks.
fn reference_ticks(
    opened: &Opened,
    stack: &Stack,
    recipe: &luxforge_core::Recipe,
    count: usize,
) -> Result<Value, String> {
    let draft = begin(opened, stack, recipe)?;
    let measured = (|| {
        let mut frame_ms = Vec::with_capacity(count);
        for value in sweep(stack.from, stack.to, count) {
            call(
                &opened.owner,
                opened.client,
                "draft.set",
                json!({"draft_id": draft, "fields": {stack.field: value}}),
            )?;
            let draft_id =
                serde_json::from_value(draft.clone()).map_err(|error| error.to_string())?;
            let job = super::tasks::ready_preview_job(
                &opened.owner,
                PreviewRequest::new(opened.client, opened.asset.clone()).draft(draft_id),
            )?;
            let evaluation = job.evaluation;
            let started = Instant::now();
            let frame = luxforge_core::render(
                evaluation.registry(),
                evaluation.source(),
                evaluation.recipe(),
                RenderOptions::exact(&Cancel::never()),
                evaluation.context(),
            )
            .and_then(|rendered| rendered.frame(evaluation.entry().snapshot.id.clone()))
            .map_err(|error| error.to_string())?;
            frame_ms.push(ms(started.elapsed()));
            drop(frame);
        }
        Ok::<_, String>(frame_ms)
    })();
    cancel(opened, &draft);
    Ok(json!({"status": "measured", "frames": summary(&measured?)}))
}

/// What one stack measured, every part a gap naming why where it could not be.
#[allow(clippy::too_many_arguments)]
fn measure(
    qualifier: &luxforge_gpu::qualification::Qualifier,
    exporter: Option<&GpuTiles>,
    stack: &Stack,
    source: &CorpusSource,
    output: &Path,
    ticks_per_view: usize,
    reference_per_stack: usize,
) -> Result<Value, String> {
    let opened = Opened::new(
        source,
        stack.steps.as_array().expect("steps"),
        &output.join("catalogs").join(format!("{}.sqlite", stack.id)),
    )?;
    let job = super::tasks::ready_preview_job(
        &opened.owner,
        PreviewRequest::new(opened.client, opened.asset.clone()),
    )?;
    let evaluation = job.evaluation;
    let recipe = evaluation.recipe().clone();
    // The committed stack's exact frame first, which stores its global estimates in the context
    // its export and its picture at rest read, as a settled frame stores them.
    let started = Instant::now();
    let exact = luxforge_core::render(
        evaluation.registry(),
        evaluation.source(),
        evaluation.recipe(),
        RenderOptions::exact(&Cancel::never()),
        evaluation.context(),
    )
    .and_then(|rendered| rendered.frame(evaluation.entry().snapshot.id.clone()))
    .map_err(|error| error.to_string())?;
    let first_reference_ms = ms(started.elapsed());
    let stage = (exact.width, exact.height);
    drop(exact);
    let gpu =
        gpu_source_of(1, evaluation.source()).ok_or("the source cannot be held on the GPU")?;
    let gap = |error: String| json!({"status": "gap", "reason": error});
    let mut surface = qualifier.surface();
    let mut views = serde_json::Map::new();
    for view in [View::Fit, View::Full] {
        let measured = ticks(
            qualifier,
            &mut surface,
            &opened,
            stack,
            &recipe,
            &gpu,
            stage,
            view,
            ticks_per_view,
        )
        .unwrap_or_else(gap);
        eprintln!(
            "{} at {}: {}",
            stack.id,
            view.id(),
            brief(&measured["ticks"])
        );
        views.insert(view.id().to_owned(), measured);
    }
    let at_rest = rest(&mut surface, &evaluation, &gpu, 3).unwrap_or_else(gap);
    eprintln!("{} at rest: {}", stack.id, brief(&at_rest["drawn"]));
    drop(surface);
    let exported = match exporter {
        None => gap("no adapter for the GPU tile worker".to_owned()),
        // Cold, then warm unless the cold one took past [`LONG`], whose compile is a small share.
        Some(worker) => match export(worker, &evaluation) {
            Ok((cold, bands)) if cold > LONG.as_secs_f64() => {
                json!({"status": "measured", "cold_s": cold, "warm_s": null, "bands": bands})
            }
            Ok((cold, bands)) => match export(worker, &evaluation) {
                Ok((warm, _)) => {
                    json!({"status": "measured", "cold_s": cold, "warm_s": warm, "bands": bands})
                }
                Err(error) => gap(error),
            },
            Err(error) => gap(error),
        },
    };
    eprintln!("{} export: {exported}", stack.id);
    let reference =
        reference_ticks(&opened, stack, &recipe, reference_per_stack).unwrap_or_else(gap);
    eprintln!("{} reference: {}", stack.id, brief(&reference["frames"]));
    Ok(json!({
        "id": stack.id,
        "title": stack.title,
        "steps": stack.steps,
        "dragged": {"action": stack.action, "mask": stack.mask, "field": stack.field,
                    "from": stack.from, "to": stack.to},
        "stage": [stage.0, stage.1],
        "views": views,
        "at_rest_fit": at_rest,
        "export": exported,
        "reference": reference,
        "first_reference_ms": first_reference_ms,
    }))
}

fn brief(summary: &Value) -> String {
    match summary["p50_ms"].as_f64() {
        Some(p50) => format!(
            "p50 {p50:.1} ms p95 {:.1} ms over {}",
            summary["p95_ms"].as_f64().unwrap_or(f64::NAN),
            summary["count"]
        ),
        None => "no figures".to_owned(),
    }
}

/// A figure for the table: milliseconds, or the gap's reason in brief.
fn cell(value: &Value, key: &str) -> String {
    match value["status"].as_str() {
        Some("measured") => match value.pointer(key).and_then(Value::as_f64) {
            Some(figure) if figure >= 1000.0 => format!("{:.2} s", figure / 1000.0),
            Some(figure) => format!("{figure:.1} ms"),
            None => "—".to_owned(),
        },
        Some(_) => format!(
            "gap: {}",
            value["reason"]
                .as_str()
                .unwrap_or("")
                .split(':')
                .next()
                .unwrap_or("")
        ),
        None => "—".to_owned(),
    }
}

/// The report's table, one row a stack.
fn table(document: &Value) -> String {
    let mut out = format!(
        "# Drag ticks on {}\n\nHost: {} logical CPUs, {} {}; build release; {} timed GPU ticks a \
         view, {} reference frames a stack; 24 MP generated JPEG ({}).\n\n\
         | Stack | Fit tick p50 | Fit tick p95 | 100% tick p50 | 100% tick p95 | At rest, Fit | \
         Export | Reference frame p50 | Reference frame p95 |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
        document["adapter"]["summary"].as_str().unwrap_or("?"),
        document["host"]["logical_cpus"],
        document["host"]["os"].as_str().unwrap_or("?"),
        document["host"]["arch"].as_str().unwrap_or("?"),
        document["ticks_per_view"],
        document["reference_frames_per_stack"],
        document["source"].as_str().unwrap_or("?"),
    );
    for stack in document["stacks"].as_array().into_iter().flatten() {
        let fit = &stack["views"]["fit"];
        let full = &stack["views"]["100%"];
        let export = match stack["export"]["status"].as_str() {
            Some("measured") => match stack["export"]["warm_s"].as_f64() {
                Some(warm) => format!("{warm:.2} s"),
                None => format!(
                    "{:.2} s (cold)",
                    stack["export"]["cold_s"].as_f64().unwrap_or(f64::NAN)
                ),
            },
            _ => format!("gap: {}", stack["export"]["reason"].as_str().unwrap_or("")),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            stack["title"].as_str().unwrap_or("?"),
            cell(fit, "/ticks/p50_ms"),
            cell(fit, "/ticks/p95_ms"),
            cell(full, "/ticks/p50_ms"),
            cell(full, "/ticks/p95_ms"),
            cell(&stack["at_rest_fit"], "/drawn/p50_ms"),
            export,
            cell(&stack["reference"], "/frames/p50_ms"),
            cell(&stack["reference"], "/frames/p95_ms"),
        ));
    }
    out
}

fn write(output: &Path, document: &Value) {
    std::fs::write(
        output.join("drag-ticks.json"),
        serde_json::to_string_pretty(document).unwrap(),
    )
    .unwrap();
    std::fs::write(output.join("drag-ticks.md"), table(document)).unwrap();
}

#[test]
#[ignore = "the drag-tick benchmark, release only: `LUXFORGE_DRAG_BENCH_OUTPUT=NEW_DIR cargo test \
            --release --locked -p luxforge-app --bin luxforge \
            app::gpu_drag_bench::drag_tick_benchmark -- --ignored --exact --nocapture`"]
fn drag_tick_benchmark() {
    const TEST: &str = "drag_tick_benchmark";
    if cfg!(debug_assertions) {
        panic!("{TEST} measures release builds only: pass --release");
    }
    let output = PathBuf::from(
        std::env::var("LUXFORGE_DRAG_BENCH_OUTPUT").expect("LUXFORGE_DRAG_BENCH_OUTPUT"),
    );
    assert!(
        output.is_absolute(),
        "LUXFORGE_DRAG_BENCH_OUTPUT must be absolute: cargo runs the test in the crate's directory"
    );
    assert!(
        !output.exists(),
        "{} exists: use a new directory",
        output.display()
    );
    std::fs::create_dir_all(output.join("catalogs")).unwrap();
    let number = |name: &str, default: usize| {
        std::env::var(name)
            .ok()
            .map(|value| value.parse::<usize>().expect("a count"))
            .unwrap_or(default)
            .max(1)
    };
    let ticks_per_view = number("LUXFORGE_DRAG_BENCH_TICKS", 40);
    let reference_per_stack = number("LUXFORGE_DRAG_BENCH_REFERENCE_TICKS", 8);
    let wanted: Option<Vec<String>> = std::env::var("LUXFORGE_DRAG_BENCH_STACKS")
        .ok()
        .map(|list| list.split(',').map(|id| id.trim().to_owned()).collect());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let generated = std::env::var("LUXFORGE_GENERATED_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("fixtures/generated"));
    let source = CorpusSource {
        id: "jpeg-24mp".to_owned(),
        path: generated.join("24mp.jpg"),
        raw: false,
    };
    assert!(
        source.path.is_file(),
        "{} is missing: run `cargo xtask generate-fixtures`",
        source.path.display()
    );
    let environment: serde_json::Map<String, Value> = [
        "LUXFORGE_GPU_ADAPTER",
        "WGPU_BACKEND",
        "VK_ICD_FILENAMES",
        "LP_NUM_THREADS",
    ]
    .iter()
    .map(|name| ((*name).to_owned(), json!(std::env::var(name).ok())))
    .collect();
    let mut document = json!({
        "format": 1,
        "test": TEST,
        "source": source.path,
        "ticks_per_view": ticks_per_view,
        "reference_frames_per_stack": reference_per_stack,
        "host": {
            "logical_cpus": std::thread::available_parallelism().map_or(0, usize::from),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "fit_bounds": {"width": fit_bounds().width, "height": fit_bounds().height},
        "environment": environment,
        "adapter": null,
        "stacks": [],
        "complete": false,
    });
    let Some(qualifier) = headless(TEST) else {
        document["skipped"] = json!("no adapter: the benchmark drew nothing");
        write(&output, &document);
        return;
    };
    document["adapter"] = json!({"summary": qualifier.adapter()});
    eprintln!("{TEST}: adapter {}", qualifier.adapter());
    // The GPU tile worker on the host's adapter, which the export lane streams through.
    let exporter = super::gpu_tiles_tests::host_adapter(TEST).map(|adapter| {
        document["export_adapter"] = json!({"backend": adapter.0, "adapter": adapter.1});
        GpuTiles::new(Some(adapter), false)
    });
    for stack in stacks() {
        if wanted
            .as_ref()
            .is_some_and(|wanted| !wanted.iter().any(|id| id == stack.id))
        {
            continue;
        }
        let measured = measure(
            &qualifier,
            exporter.as_ref(),
            &stack,
            &source,
            &output,
            ticks_per_view,
            reference_per_stack,
        )
        .unwrap_or_else(|error| json!({"id": stack.id, "title": stack.title, "status": "error", "error": error}));
        document["stacks"].as_array_mut().unwrap().push(measured);
        write(&output, &document);
    }
    document["complete"] = json!(true);
    write(&output, &document);
    eprintln!("{}", table(&document));
}

#[test]
fn a_sweep_goes_to_and_fro_between_its_ends() {
    assert_eq!(sweep(0.0, 1.0, 5), vec![0.0, 0.5, 1.0, 0.5, 0.0]);
    assert_eq!(sweep(10.0, 90.0, 3), vec![10.0, 90.0, 10.0]);
    let summarised = summary(&[5.0, 1.0, 3.0, 2.0, 4.0]);
    assert_eq!(summarised["p50_ms"], json!(3.0));
    assert_eq!(summarised["p95_ms"], json!(5.0));
    assert_eq!(summary(&[]), json!({"count": 0}));
}
