//! The reference comparison harness, which the release gate runs (`docs/design/gpu-first.md`,
//! "Tolerance by output kind"; `cargo xtask gpu-qualification`). For every stack of the
//! qualification corpus on every source this host has, it renders the reference renderer's exact
//! whole frame — the frame export renders — and, at each view the corpus lists, the GPU frame of
//! the stack's plan from its first pixel layer (the frame a drag draws) over the boundary the
//! desktop holds at that view:
//!
//! - **Fit**, at the bounds of an evidence run's window ([`fit_bounds`]), and **33%** and **50%**,
//!   whose proxy is planned at the displayed size as Fit's is, with other bounds
//!   ([`percent_bounds`]): the GPU frame against the reference frame reduced to its size by an
//!   independent area-weighted box average of its linear light ([`tolerance::reduce_srgb8`]).
//! - **100%**, the visible region of the largest window the owner's display holds
//!   ([`super::largest_view`]): the GPU frame against that region of the reference frame,
//!   unreduced.
//!
//! Beside each, as information, the CPU frame of the same view against the reference, which is what
//! the CPU path shows there, and the GPU frame against that CPU frame, the drag-time comparison the
//! program qualification makes, with the jump a Detail stack's settlement makes at Fit.
//!
//! The frame a drag draws at Fit, 33% and 50% processes a source reduced to the view's size; the
//! reference processes the whole frame and reduces it. So at those views the harness also measures
//! a second candidate for the picture at rest, **process-first** ([`process_first`]): the whole
//! output stage drawn on the GPU at full resolution as 100% region plans over a grid of tiles, each
//! over its own boundary and reading the exact stage's estimates, its codes stitched into one frame
//! and reduced to the view's size by the same reduction as the reference. It is judged by the same
//! limits and reported beside the first, with the motion frame's jump to it, for the owner's choice
//! of the picture at rest; the gate judges the frame the GPU draws on this branch. The other
//! output kinds — the histogram and clipping counts, samples and export — are computed on the
//! reference and recorded as not rendered by the GPU: their comparisons ([`histogram_kind`],
//! [`sample_kind`], [`export_kind`]) take the GPU's output once the stage that renders it supplies
//! it.
//!
//! It writes `cells.json` to `LUXFORGE_GPU_CORPUS_OUTPUT`, rewritten after every stack, and judges
//! nothing: `cargo xtask gpu-qualification` holds every cell to its limit and writes the report. A
//! cell's frames are written as PNGs under `frames/` only when its GPU frame passes a limit, or for
//! every cell with `LUXFORGE_GPU_QUALIFICATION_FRAMES=all`.
use super::{
    Cell, CellOptions, CorpusSource, Opened, RegionDraw, corpus_sources, draw_region, fit_bounds,
    headless, proxy_cell, region_cell_in, write_png,
};
use luxforge_core::{
    Cancel, Evaluation, PreviewRequest, ProxyBounds, Raster, Render, RenderOptions,
    analysis::Report,
};
use luxforge_reference::{
    preview_error::{self, Class, Rgb8, Statistics},
    tolerance::{self, Kind},
};
use luxforge_ui::photo_surface::gpu_preview::qualification::Qualifier;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Why the histogram and clipping counts are not compared on this branch.
const HISTOGRAM_NOT_RENDERED: &str = "the GPU does not render the histogram and clipping counts \
    yet: a GPU reduction over the full stage is stage 2's, and until it lands the counts come from \
    the reference renderer";
/// Why samples are not compared on this branch.
const SAMPLE_NOT_RENDERED: &str = "the GPU does not answer samples yet: a sample from a GPU tile \
    render is stage 4's, and until it lands samples come from the reference renderer";
/// Why export is not compared on this branch.
const EXPORT_NOT_RENDERED: &str = "the GPU does not export yet: export through GPU tiles is stage \
    4's, and until it lands export is the reference renderer's";

/// One view of the corpus: Fit, or a percentage of the output stage.
#[derive(Clone, Copy, Debug, PartialEq)]
enum View {
    Fit,
    Percent(f32),
}

impl View {
    /// The view a corpus view's step names: `{"view": {"zoom": "fit"}}` or a percentage.
    fn of(step: &Value) -> Option<Self> {
        match &step["view"]["zoom"] {
            Value::String(fit) if fit == "fit" => Some(Self::Fit),
            Value::Number(percent) => percent
                .as_f64()
                .map(|percent| Self::Percent(percent as f32)),
            _ => None,
        }
    }

    /// The name a frame of this view is written under: `fit`, or the percentage.
    fn token(self) -> String {
        match self {
            Self::Fit => "fit".to_owned(),
            Self::Percent(percent) => format!("{percent}"),
        }
    }
}

/// The bounds of the proxy a percentage view below 100% plans, as the desktop plans it
/// ([`crate::app::Editor::proxy_bounds_for`]): the displayed size of the whole output stage at
/// `zoom`, through the core's display-bounds limits, when that is smaller than the stage on both
/// axes. `None` when the photograph is not drawn below its size.
fn percent_bounds(stage: (u32, u32), zoom: f32) -> Option<ProxyBounds> {
    let displayed = crate::state::histogram::displayed_size(
        crate::state::canvas::ZoomView::Percent(zoom),
        stage,
        // A percentage's displayed size depends on the stage alone: the surface, the display scale
        // and the Fit inset are Fit's.
        (1.0, 1.0),
        1.0,
        (0.0, 0.0),
    )?;
    (displayed.0 < stage.0 as f32 && displayed.1 < stage.1 as f32)
        .then(|| crate::app::preview::bounds_of(displayed))
        .flatten()
}

/// What a run measures, read from its environment.
struct Settings {
    output: PathBuf,
    /// The corpus's views to measure, each with its id.
    views: Vec<(String, View)>,
    kinds: Vec<Kind>,
    families: Option<Vec<String>>,
    recipes: Option<Vec<String>>,
    sources: Option<Vec<String>>,
    /// Write every measured cell's frames, not only those past a limit.
    all_frames: bool,
}

/// A comma-separated list in the environment variable `name`, when it is set.
fn list(name: &str) -> Option<Vec<String>> {
    std::env::var(name).ok().map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect()
    })
}

/// The corpus at every view it lists, every output kind against the reference, for every stack on
/// every source this host has, written to `cells.json` in `LUXFORGE_GPU_CORPUS_OUTPUT` (a new
/// directory). Reads `LUXFORGE_GENERATED_FIXTURES` (the generated JPEGs, `fixtures/generated` by
/// default), for the RAWs `LUXFORGE_RAW_MANIFEST`, and, to measure less, the comma-separated
/// `LUXFORGE_GPU_QUALIFICATION_VIEWS` (view ids), `LUXFORGE_GPU_QUALIFICATION_KINDS` (output kinds),
/// `LUXFORGE_GPU_CORPUS_FAMILIES`, `LUXFORGE_GPU_CORPUS_RECIPES` and `LUXFORGE_GPU_CORPUS_SOURCES`;
/// with
/// `LUXFORGE_GPU_QUALIFICATION_FRAMES=all` it writes every cell's frames. Without an adapter it
/// records that it was skipped and measures nothing.
pub(crate) fn against_reference(test: &str) {
    let output = PathBuf::from(
        std::env::var("LUXFORGE_GPU_CORPUS_OUTPUT").expect("LUXFORGE_GPU_CORPUS_OUTPUT"),
    );
    assert!(
        !output.exists(),
        "{} exists: use a new directory",
        output.display()
    );
    std::fs::create_dir_all(&output).unwrap();
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
    let wanted = list("LUXFORGE_GPU_QUALIFICATION_VIEWS");
    let views: Vec<(String, View)> = corpus["views"]
        .as_array()
        .expect("views")
        .iter()
        .map(|view| {
            let id = view["id"].as_str().expect("a view's id").to_owned();
            let parsed = View::of(&view["step"]).expect("a view step");
            (id, parsed)
        })
        .filter(|(id, _)| wanted.as_ref().is_none_or(|wanted| wanted.contains(id)))
        .collect();
    if let Some(wanted) = &wanted {
        for id in wanted {
            assert!(
                views.iter().any(|(view, _)| view == id),
                "the corpus lists no view {id}"
            );
        }
    }
    let kinds: Vec<Kind> = list("LUXFORGE_GPU_QUALIFICATION_KINDS").map_or_else(
        || Kind::ALL.to_vec(),
        |names| {
            names
                .iter()
                .map(|name| Kind::parse(name).unwrap_or_else(|| panic!("no output kind {name}")))
                .collect()
        },
    );
    let settings = Settings {
        output: output.clone(),
        views,
        kinds,
        families: list("LUXFORGE_GPU_CORPUS_FAMILIES"),
        recipes: list("LUXFORGE_GPU_CORPUS_RECIPES"),
        sources: list("LUXFORGE_GPU_CORPUS_SOURCES"),
        all_frames: std::env::var("LUXFORGE_GPU_QUALIFICATION_FRAMES").as_deref() == Ok("all"),
    };
    let bounds = fit_bounds();
    let mut document = json!({
        "format": 1,
        "test": test,
        "adapter": null,
        "skipped": null,
        "views": settings.views.iter().map(|(id, view)| match view {
            View::Fit => json!({"id": id, "zoom": "fit", "bounds": [bounds.width, bounds.height]}),
            View::Percent(zoom) if *zoom < 100.0 => json!({"id": id, "zoom": zoom}),
            View::Percent(zoom) => json!({
                "id": id,
                "zoom": zoom,
                "window": [super::LARGEST_WINDOW.0, super::LARGEST_WINDOW.1],
            }),
        }).collect::<Vec<_>>(),
        "kinds": settings.kinds.iter().map(|kind| kind.name()).collect::<Vec<_>>(),
        "families": settings.families,
        "recipes": settings.recipes,
        "selected_sources": settings.sources,
        "sources": [],
        "pairs": [],
        "complete": false,
    });
    let Some(qualifier) = headless(test) else {
        document["skipped"] = json!(
            "no GPU adapter on this host: the harness rendered nothing and is not GPU evidence"
        );
        document["complete"] = json!(true);
        write(&output, &document);
        return;
    };
    document["adapter"] = json!(qualifier.adapter());
    eprintln!(
        "{test}: adapter {}, views {:?}, kinds {:?}",
        qualifier.adapter(),
        settings.views,
        settings.kinds
    );
    let sources = corpus_sources(&corpus, &generated, manifest.as_ref());
    document["sources"] = corpus["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .map(|source| {
            let id = source["id"].as_str().unwrap_or_default();
            match sources.iter().find(|found| found.id == id) {
                Some(found) => json!({"id": id, "status": "found", "path": found.path}),
                None => json!({"id": id, "status": "missing", "detail": "no file on this host"}),
            }
        })
        .collect();
    std::fs::create_dir_all(output.join("catalogs")).unwrap();
    std::fs::create_dir_all(output.join("frames")).unwrap();
    write(&output, &document);
    for recipe in corpus["recipes"].as_array().expect("recipes") {
        let id = recipe["id"].as_str().expect("a recipe's id");
        let family = recipe["family"].as_str().unwrap_or_default();
        if settings
            .families
            .as_ref()
            .is_some_and(|families| !families.iter().any(|wanted| wanted == family))
            || settings
                .recipes
                .as_ref()
                .is_some_and(|recipes| !recipes.iter().any(|wanted| wanted == id))
        {
            continue;
        }
        for source_id in recipe["sources"].as_array().expect("sources") {
            let source_id = source_id.as_str().expect("a source id");
            if settings
                .sources
                .as_ref()
                .is_some_and(|sources| !sources.iter().any(|wanted| wanted == source_id))
            {
                continue;
            }
            let record = match sources.iter().find(|found| found.id == source_id) {
                Some(source) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pair(&qualifier, &settings, recipe, source)
                }))
                .unwrap_or_else(|panic| {
                    let reason = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                        .unwrap_or_else(|| "a panic".to_owned());
                    json!({
                        "recipe": id, "family": family, "class": recipe["class"],
                        "source": source_id, "cell": format!("{id}--{source_id}"),
                        "status": "error", "error": format!("the harness panicked: {reason}"),
                    })
                }),
                None => json!({
                    "recipe": id, "family": family, "class": recipe["class"],
                    "source": source_id, "cell": format!("{id}--{source_id}"),
                    "status": "missing-source", "error": "no file on this host",
                }),
            };
            document["pairs"]
                .as_array_mut()
                .expect("pairs")
                .push(record);
            write(&output, &document);
        }
    }
    document["complete"] = json!(true);
    write(&output, &document);
    eprintln!(
        "{test}: {} stacks in {}",
        document["pairs"].as_array().map_or(0, Vec::len),
        output.display()
    );
}

/// `document` written to `output/cells.json` through a temporary file, so a reader never sees half
/// of it.
fn write(output: &Path, document: &Value) {
    let staged = output.join("cells.json.partial");
    std::fs::write(&staged, serde_json::to_string_pretty(document).unwrap()).unwrap();
    std::fs::rename(&staged, output.join("cells.json")).unwrap();
}

/// One stack on one source: its reference frame, every view's picture and every other output kind,
/// as one record of `cells.json`.
fn pair(
    qualifier: &Qualifier,
    settings: &Settings,
    recipe: &Value,
    source: &CorpusSource,
) -> Value {
    let id = recipe["id"].as_str().expect("a recipe's id");
    let cell = format!("{id}--{}", source.id);
    let class = Class::parse(recipe["class"].as_str().unwrap_or_default()).expect("a class");
    let mut record = json!({
        "recipe": id,
        "family": recipe["family"],
        "class": class.name(),
        "source": source.id,
        "cell": cell,
        "views": {},
        "kinds": {},
    });
    let steps = recipe["steps"].as_array().expect("steps");
    match measure(
        qualifier,
        settings,
        class,
        steps,
        source,
        &cell,
        &mut record,
    ) {
        Ok(()) => record["status"] = json!("measured"),
        Err(error) => {
            eprintln!("{cell}: error: {error}");
            record["status"] = json!("error");
            record["error"] = json!(error);
        }
    }
    record
}

/// [`pair`]'s measurements, recorded in `record`.
fn measure(
    qualifier: &Qualifier,
    settings: &Settings,
    class: Class,
    steps: &[Value],
    source: &CorpusSource,
    cell: &str,
    record: &mut Value,
) -> Result<(), String> {
    let catalog = settings
        .output
        .join("catalogs")
        .join(format!("{cell}.sqlite"));
    let opened = Opened::new(source, steps, &catalog)?;
    // The reference renderer's frame of the stack: its exact whole frame, as export renders it,
    // rendered through the evaluation's own context so that its store holds the whole stage's
    // global estimates, which the process-first tiles read as a settled view's plans do.
    let job = crate::app::tasks::ready_preview_job(
        &opened.owner,
        PreviewRequest::new(opened.client, opened.asset.clone()),
    )?;
    let evaluation = job.evaluation;
    let exact = luxforge_core::render(
        evaluation.registry(),
        evaluation.source(),
        evaluation.recipe(),
        RenderOptions::exact(&Cancel::never()),
        evaluation.context(),
    )
    .map_err(|error| error.to_string())?;
    let reference = exact
        .frame(evaluation.entry().snapshot.id.clone())
        .map_err(|error| error.to_string())?;
    let stage = (reference.width, reference.height);
    record["reference"] = json!({"stage": [stage.0, stage.1]});
    if settings.kinds.contains(&Kind::Picture) {
        let reduced = settings
            .views
            .iter()
            .any(|(_, view)| !matches!(view, View::Percent(zoom) if *zoom >= 100.0));
        let second = if reduced {
            Some(process_first(
                qualifier,
                &evaluation,
                &exact,
                opened.raw,
                stage,
            )?)
        } else {
            None
        };
        record["process_first"] = match &second {
            Some(Ok(frame)) => json!({
                "status": "drawn",
                "tile": TILE,
                "tiles": frame.tiles,
                "charged_bytes_max": frame.charged,
                "clock": {
                    "note": CLOCK,
                    "total_s": frame.total.as_secs_f64(),
                    "boundaries_s": frame.boundaries.as_secs_f64(),
                    "draws_s": frame.draws.as_secs_f64(),
                },
            }),
            Some(Err(reason)) => json!({"status": "gap", "reason": reason}),
            None => Value::Null,
        };
        eprintln!("{cell}: process-first frame: {}", record["process_first"]);
        for (id, view) in &settings.views {
            let measured = picture(
                qualifier,
                settings,
                &opened,
                &reference,
                second.as_ref(),
                *view,
                class,
                cell,
            )
            .unwrap_or_else(|error| json!({"status": "error", "reason": error}));
            eprintln!("{cell} at {id}: {}", summary(&measured));
            record["views"][id] = measured;
        }
    }
    for kind in &settings.kinds {
        let measured = match kind {
            Kind::Picture => continue,
            Kind::Histogram => histogram_kind(&reference, None)?,
            Kind::Sample => sample_kind(&reference, None, class)?,
            Kind::Export => export_kind(&reference, None, class)?,
        };
        record["kinds"][kind.name()] = measured;
    }
    Ok(())
}

/// One line of a view's figures, for the console.
fn summary(measured: &Value) -> String {
    match measured["status"].as_str() {
        Some("measured") => {
            let figures = |s: &Value| {
                format!(
                    "mean {:.4} block {:.4} p99 {:.4} dL* {:+.4}",
                    s["mean"].as_f64().unwrap_or(f64::NAN),
                    s["worst_block"].as_f64().unwrap_or(f64::NAN),
                    s["p99"].as_f64().unwrap_or(f64::NAN),
                    s["mean_delta_l"].as_f64().unwrap_or(f64::NAN)
                )
            };
            let second = match measured["process_first"]["status"].as_str() {
                Some("measured") => format!(
                    " | process-first against the reference {}",
                    figures(&measured["process_first"]["statistics"])
                ),
                Some(_) => " | process-first: a gap".to_owned(),
                None => String::new(),
            };
            format!(
                "GPU against the reference {} | CPU against it {} | GPU against the CPU {}{second}",
                figures(&measured["statistics"]),
                figures(&measured["cpu_against_reference"]),
                figures(&measured["drag"]["statistics"])
            )
        }
        Some(status) => format!("{status}: {}", measured["reason"].as_str().unwrap_or("")),
        None => "unrecorded".to_owned(),
    }
}

/// The side of the tiles a process-first frame is drawn in: within the 8192 px texture limit, and
/// below the 100% corpus's largest region (3024 × 1964 in the owner's largest window), which every
/// stack of the corpus draws within the 256 MiB bound on a boundary and the 2 GiB GPU-preview
/// budget.
const TILE: u32 = 2048;

/// What a process-first frame's clock is, and is not.
const CLOCK: &str = "the harness's own clock on a loaded host, each tile's boundary rendered on \
    the CPU and its codes read back to the CPU, programs compiled on first use: an order of \
    magnitude, not a timing measurement";

/// Candidate 2 for the picture at rest: the whole output stage drawn on the GPU at full resolution.
struct ProcessFirst {
    /// The whole output frame's codes, three bytes a pixel, row by row.
    codes: Vec<u8>,
    tiles: usize,
    /// The largest charge of any tile's slot against the GPU-preview budget.
    charged: u64,
    /// The harness's own clock ([`CLOCK`]): the whole frame, its tiles' boundaries rendered on the
    /// CPU, and their draws and readbacks on the device.
    total: Duration,
    boundaries: Duration,
    draws: Duration,
}

/// Candidate 2 for the picture at rest, process-first: `evaluation`'s whole output stage of `stage`
/// drawn on the GPU at full resolution as 100% region plans over a grid of [`TILE`]-sided tiles,
/// each over its own boundary from `exact` and reading the global estimates `evaluation`'s context
/// holds for the whole stage, their codes stitched into one frame. `Err` inside names the first
/// tile the GPU path does not draw, and why: the frame is then a gap.
fn process_first(
    qualifier: &Qualifier,
    evaluation: &Evaluation,
    exact: &Render<'_>,
    linear: bool,
    (width, height): (u32, u32),
) -> Result<Result<ProcessFirst, String>, String> {
    let started = Instant::now();
    let mut frame = ProcessFirst {
        codes: vec![0; width as usize * height as usize * 3],
        tiles: 0,
        charged: 0,
        total: Duration::ZERO,
        boundaries: Duration::ZERO,
        draws: Duration::ZERO,
    };
    let options = CellOptions {
        frames: None,
        settle: false,
        empty_mask: true,
    };
    for y0 in (0..height).step_by(TILE as usize) {
        for x0 in (0..width).step_by(TILE as usize) {
            let rect = luxforge_core::Region {
                x0,
                y0,
                width: TILE.min(width - x0),
                height: TILE.min(height - y0),
            };
            let drawn = match draw_region(
                qualifier, evaluation, exact, linear, rect, 100.0, options, false,
            )? {
                RegionDraw::Drawn(drawn) => drawn,
                RegionDraw::Gap(reason) => {
                    return Ok(Err(format!("the tile at ({x0}, {y0}): {reason}")));
                }
                RegionDraw::NoBoundary(error) => {
                    return Ok(Err(format!(
                        "the tile at ({x0}, {y0}) has no boundary: {}",
                        error.detail
                    )));
                }
            };
            if drawn.approximate {
                return Ok(Err(format!(
                    "region-estimate: the tile at ({x0}, {y0}) takes a global estimate from \
                     itself alone, which the store does not hold"
                )));
            }
            let row = rect.width as usize * 3;
            for (y, codes) in drawn.gpu.chunks_exact(row).enumerate() {
                let start = ((y0 as usize + y) * width as usize + x0 as usize) * 3;
                frame.codes[start..start + row].copy_from_slice(codes);
            }
            frame.tiles += 1;
            frame.charged = frame.charged.max(drawn.charged);
            frame.boundaries += drawn.boundary_time;
            frame.draws += drawn.draw_time;
        }
    }
    frame.total = started.elapsed();
    Ok(Ok(frame))
}

/// The picture at `view`: the GPU frame the desktop's plan draws there against the reference frame
/// at the view's size, with the CPU frame of the same view beside it.
#[allow(clippy::too_many_arguments)]
fn picture(
    qualifier: &Qualifier,
    settings: &Settings,
    opened: &Opened,
    reference: &Raster,
    second: Option<&Result<ProcessFirst, String>>,
    view: View,
    class: Class,
    cell: &str,
) -> Result<Value, String> {
    let stage = (reference.width, reference.height);
    let output = &settings.output;
    let options = CellOptions {
        frames: None,
        settle: view == View::Fit,
        empty_mask: true,
    };
    let drawn = match view {
        View::Fit => proxy_cell(qualifier, opened, fit_bounds(), output, options, class)?,
        View::Percent(zoom) if zoom < 100.0 => match percent_bounds(stage, zoom) {
            Some(bounds) => proxy_cell(qualifier, opened, bounds, output, options, class)?,
            None => {
                return Ok(json!({
                    "status": "gap",
                    "reason": format!("at {zoom}% the photograph is not drawn below its size"),
                }));
            }
        },
        View::Percent(zoom) => region_cell_in(qualifier, opened, zoom, output, options, class)?,
    };
    let (size, proxy, region, drag, program, charged, settled, shape, gpu, cpu, notes) = match drawn
    {
        Cell::Measured {
            stage,
            proxy,
            region,
            statistics,
            program,
            charged,
            settled,
            shape,
            gpu,
            cpu,
            notes,
            ..
        } => (
            stage, proxy, region, statistics, program, charged, settled, shape, gpu, cpu, notes,
        ),
        Cell::Gap(reason) => return Ok(json!({"status": "gap", "reason": reason})),
    };
    let (width, height) = size;
    // The reference at the view: its visible region at 100%, the whole frame reduced elsewhere.
    let expected = match region {
        Some(rect) => tolerance::region_srgb8(
            &reference.rgba,
            4,
            reference.width,
            [rect.x0, rect.y0, rect.width, rect.height],
        )?,
        None => reduce(&reference.rgba, 4, stage, size)?,
    };
    let against = compare(&gpu, &expected, size)?;
    let cpu_against = compare(&cpu, &expected, size)?;
    let mut frames = Vec::new();
    if settings.all_frames || !preview_error::verdict(&against, class).passed() {
        let name = format!("{cell}--{}", view.token());
        for (suffix, bytes) in [("gpu", &gpu), ("reference", &expected), ("cpu", &cpu)] {
            let file = format!("{name}-{suffix}");
            write_png(&output.join("frames"), &file, (width, height), bytes)?;
            frames.push(format!("frames/{file}.png"));
        }
    }
    let mut value = json!({
        "status": "measured",
        "stage": [width, height],
        "proxy": proxy,
        "region": region.map(|rect| [rect.x0, rect.y0, rect.width, rect.height]),
        "reference": match region {
            Some(_) => "the reference frame's visible region",
            None if size == stage => "the reference frame",
            None => "the reference frame reduced to the view's size",
        },
        "statistics": statistics(&against),
        "cpu_against_reference": statistics(&cpu_against),
        "drag": {
            "against": match region {
                Some(_) => "the CPU's exact visible region",
                None if proxy => "the CPU's proxy frame",
                None => "the CPU's exact frame",
            },
            "statistics": statistics(&drag),
            "program": statistics(&program),
        },
        "charged_bytes": charged,
        "shape": shape,
        "notes": notes,
        "frames": frames,
    });
    if let Some(settled) = settled {
        value["settled_from_exact"] = json!({
            "cpu_proxy": statistics(&settled.proxy),
            "gpu": statistics(&settled.gpu),
        });
    }
    // Candidate 2 at a view the reference is reduced for: the process-first frame reduced as the
    // reference is, against it, and the jump the motion frame makes to it.
    if region.is_none()
        && let Some(second) = second
    {
        value["process_first"] = match second {
            Ok(frame) => {
                let reduced = reduce(&frame.codes, 3, stage, size)?;
                let against = compare(&reduced, &expected, size)?;
                let jump = compare(&gpu, &reduced, size)?;
                if settings.all_frames || !preview_error::verdict(&against, class).passed() {
                    let file = format!("{cell}--{}-process-first", view.token());
                    write_png(&output.join("frames"), &file, (width, height), &reduced)?;
                    value["frames"]
                        .as_array_mut()
                        .expect("frames")
                        .push(json!(format!("frames/{file}.png")));
                }
                json!({
                    "status": "measured",
                    "statistics": statistics(&against),
                    "jump_from_motion": statistics(&jump),
                })
            }
            Err(reason) => json!({"status": "gap", "reason": reason}),
        };
    }
    Ok(value)
}

/// The four statistics and the largest difference, as a cell records them.
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

/// `pixels`, a `from` frame of `stride` bytes a pixel, reduced to `to` by
/// [`tolerance::reduce_srgb8`], its output rows in bands across the host's cores.
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

/// `raster`'s pixels, three bytes each.
fn rgb(raster: &Raster) -> Vec<u8> {
    raster
        .rgba
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// A report's counts, as the independent comparison holds them.
fn counts(report: &Report) -> tolerance::Counts {
    tolerance::Counts {
        bins: [report.r, report.g, report.b],
        clipping: [
            report.r0,
            report.g0,
            report.b0,
            report.r255,
            report.g255,
            report.b255,
            report.any_shadow,
            report.any_highlight,
            report.all_shadow,
            report.all_highlight,
            report.both,
        ],
    }
}

/// The clipping counters by name.
fn clipping(values: &[u64; 11]) -> Value {
    tolerance::CLIPPING
        .iter()
        .zip(values)
        .map(|(name, value)| ((*name).to_owned(), json!(value)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// The histogram and clipping counts: the reference frame's, from the core's own reducer
/// (`luxforge_core::analysis`) over the exact whole frame, and the GPU's `gpu` counts against them
/// when a stage supplies them, by each channel's summed bin difference and each clipping counter
/// within 0.1% of the output pixel count. Without them the kind is not rendered by the GPU, which
/// is neither a pass nor a failure.
fn histogram_kind(reference: &Raster, gpu: Option<&Report>) -> Result<Value, String> {
    let report = luxforge_core::analysis::reduce(
        &reference.rgba,
        reference.width,
        reference.height,
        &Cancel::never(),
    )
    .map_err(|error| error.to_string())?;
    let expected = counts(&report);
    let Some(gpu) = gpu else {
        return Ok(json!({
            "status": "not-rendered",
            "reason": HISTOGRAM_NOT_RENDERED,
            "reference": {"pixels": expected.pixels(), "clipping": clipping(&expected.clipping)},
        }));
    };
    let error = tolerance::histogram(&counts(gpu), &expected)?;
    let (counter, share) = error.worst_clipping();
    Ok(json!({
        "status": "measured",
        "pixels": error.pixels,
        "limit_pixels": error.limit(),
        "bins": error.bins,
        "worst_bins": error.worst_bins(),
        "clipping": clipping(&error.clipping),
        "worst_clipping": {"counter": counter, "share": share},
        "passed": error.passed(),
    }))
}

/// The points a stack is sampled at: a 5 × 5 grid over the output stage, each point at the centre
/// of its cell.
fn sample_points((width, height): (u32, u32)) -> Vec<[u32; 2]> {
    (0..5u32)
        .flat_map(|row| {
            (0..5u32)
                .map(move |column| [(2 * column + 1) * width / 10, (2 * row + 1) * height / 10])
        })
        .collect()
}

/// One sample the GPU answered at a point of [`sample_points`], with the byte on screen there when
/// the view shows it.
#[derive(Clone, Copy, Debug)]
struct Answered {
    rgb: [u8; 3],
    on_screen: Option<[u8; 3]>,
}

/// Samples at [`sample_points`]: the reference frame's bytes there, and the GPU's `gpu` answers
/// against them and against the bytes on screen when a stage supplies them, by the display limit of
/// `class` over the samples. Without them the kind is not rendered by the GPU.
fn sample_kind(
    reference: &Raster,
    gpu: Option<&[Answered]>,
    class: Class,
) -> Result<Value, String> {
    let points = sample_points((reference.width, reference.height));
    let bytes: Vec<[u8; 3]> = points
        .iter()
        .map(|[x, y]| {
            reference
                .pixel(*x, *y)
                .map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .ok_or_else(|| format!("no reference pixel at {x}, {y}"))
        })
        .collect::<Result<_, _>>()?;
    let Some(gpu) = gpu else {
        return Ok(json!({
            "status": "not-rendered",
            "reason": SAMPLE_NOT_RENDERED,
            "points": points,
            "reference": bytes,
        }));
    };
    if gpu.len() != points.len() {
        return Err(format!(
            "{} answers for {} sample points",
            gpu.len(),
            points.len()
        ));
    }
    let samples: Vec<tolerance::Sample> = gpu
        .iter()
        .zip(&bytes)
        .map(|(answered, reference)| tolerance::Sample {
            answered: answered.rgb,
            on_screen: answered.on_screen,
            reference: *reference,
        })
        .collect();
    let error = tolerance::samples(&samples)?;
    Ok(json!({
        "status": "measured",
        "points": points,
        "samples": error.samples,
        "on_screen": error.on_screen,
        "unequal": error.unequal,
        "mean": error.mean,
        "p99": error.p99,
        "mean_delta_l": error.mean_delta_l,
        "max": error.max,
        "passed": error.passed(class),
    }))
}

/// Export: the reference export, the exact whole frame, and the GPU's export and a second export of
/// the same stack, `gpu`, against it and against each other when a stage supplies them, by the
/// display limit of `class` over every pixel. Without them the kind is not rendered by the GPU.
fn export_kind(
    reference: &Raster,
    gpu: Option<(&Raster, &Raster)>,
    class: Class,
) -> Result<Value, String> {
    let Some((export, repeat)) = gpu else {
        return Ok(json!({
            "status": "not-rendered",
            "reason": EXPORT_NOT_RENDERED,
            "reference": {"stage": [reference.width, reference.height]},
        }));
    };
    let (expected, export, repeat) = (rgb(reference), rgb(export), rgb(repeat));
    let (width, height) = (reference.width, reference.height);
    let error = tolerance::export(
        Rgb8::new(width, height, &export)?,
        Rgb8::new(width, height, &repeat)?,
        Rgb8::new(width, height, &expected)?,
    )?;
    Ok(json!({
        "status": "measured",
        "statistics": statistics(&error.statistics),
        "repeatable": error.repeatable,
        "passed": error.passed(class),
    }))
}

/// The GPU qualification against the reference renderer, which `cargo xtask gpu-qualification`
/// runs in release with the environment [`against_reference`] reads.
#[test]
#[ignore = "the release gate's corpus run: `cargo xtask gpu-qualification --output NEW_DIR` sets \
            LUXFORGE_GPU_CORPUS_OUTPUT, LUXFORGE_GENERATED_FIXTURES, LUXFORGE_RAW_MANIFEST and the \
            rest"]
fn gpu_qualification_against_reference() {
    against_reference("gpu_qualification_against_reference");
}

/// A frame of `width × height` whose pixels `pixel` gives, as the reference renderer returns one.
#[cfg(test)]
fn raster(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> Raster {
    let rgba = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let [r, g, b] = pixel(x, y);
            [r, g, b, 255]
        })
        .collect();
    Raster {
        width,
        height,
        rgba: std::sync::Arc::new(rgba),
        source_fingerprint: String::new(),
        snapshot_id: luxforge_core::SnapshotId::new(),
    }
}

#[test]
fn a_percentage_below_100_plans_its_proxy_at_the_displayed_size_within_the_display_bounds() {
    // At 33% of the 24 MP stage the proxy is the displayed size.
    let bounds = percent_bounds((6000, 4000), 33.0).unwrap();
    assert_eq!((bounds.width, bounds.height), (1980, 1320));
    // At 50% of the 60 MP stage the displayed size passes the display-bounds limits, which hold
    // the proxy to 4096 px a side and 8 MP, keeping its aspect.
    let bounds = percent_bounds((10000, 6000), 50.0).unwrap();
    assert!(
        bounds.width <= 4096 && u64::from(bounds.width) * u64::from(bounds.height) <= 8_000_000
    );
    // A view that draws the photograph at its size or larger has no proxy.
    assert!(percent_bounds((6000, 4000), 100.0).is_none());
    assert!(percent_bounds((6000, 4000), 150.0).is_none());
}

#[test]
fn a_corpus_view_step_names_its_view() {
    assert_eq!(View::of(&json!({"view": {"zoom": "fit"}})), Some(View::Fit));
    assert_eq!(
        View::of(&json!({"view": {"zoom": 33}})),
        Some(View::Percent(33.0))
    );
    assert_eq!(View::of(&json!({"wait": {"ms": 5}})), None);
    assert_eq!(View::Percent(50.0).token(), "50");
    assert_eq!(View::Fit.token(), "fit");
}

#[test]
fn the_histogram_comparison_takes_the_reference_frames_counts_and_judges_a_gpus() {
    let reference = raster(100, 50, |x, y| [(x * 2) as u8, (y * 5) as u8, 0]);
    // On this branch the GPU renders no counts: not rendered, neither a pass nor a failure.
    let unrendered = histogram_kind(&reference, None).unwrap();
    assert_eq!(unrendered["status"], "not-rendered");
    assert_eq!(unrendered["reference"]["pixels"], 5000);
    assert_eq!(unrendered["reference"]["clipping"]["b0"], 5000);
    // Counts equal to the reference's pass; counts four pixels off do not, at 0.1% of 5000.
    let same = luxforge_core::analysis::reduce(&reference.rgba, 100, 50, &Cancel::never()).unwrap();
    let judged = histogram_kind(&reference, Some(&same)).unwrap();
    assert_eq!(
        (judged["status"].clone(), judged["passed"].clone()),
        (json!("measured"), json!(true))
    );
    let shifted = raster(100, 50, |x, y| {
        [(x * 2) as u8 + u8::from(x < 2 && y < 2), (y * 5) as u8, 0]
    });
    let moved = luxforge_core::analysis::reduce(&shifted.rgba, 100, 50, &Cancel::never()).unwrap();
    let judged = histogram_kind(&reference, Some(&moved)).unwrap();
    assert_eq!(
        judged["bins"][0], 8,
        "four pixels moved, counted where they left and arrived"
    );
    assert_eq!(judged["passed"], false);
}

#[test]
fn the_sample_comparison_reads_the_reference_at_its_points_and_judges_a_gpus_answers() {
    let reference = raster(60, 40, |x, y| [x as u8 * 3, y as u8 * 4, 90]);
    let unrendered = sample_kind(&reference, None, Class::Pointwise).unwrap();
    assert_eq!(unrendered["status"], "not-rendered");
    assert_eq!(unrendered["points"].as_array().unwrap().len(), 25);
    assert_eq!(unrendered["points"][0], json!([6, 4]));
    assert_eq!(unrendered["reference"][0], json!([18, 16, 90]));
    // Answers equal to the reference and to the screen pass; one that is not the byte on screen
    // fails, however close it is.
    let answers: Vec<Answered> = sample_points((60, 40))
        .iter()
        .map(|[x, y]| {
            let pixel = reference.pixel(*x, *y).unwrap();
            let rgb = [pixel[0], pixel[1], pixel[2]];
            Answered {
                rgb,
                on_screen: Some(rgb),
            }
        })
        .collect();
    let judged = sample_kind(&reference, Some(&answers), Class::Pointwise).unwrap();
    assert_eq!(judged["passed"], true, "{judged}");
    let mut off = answers.clone();
    off[3].on_screen = Some([0, 0, 0]);
    let judged = sample_kind(&reference, Some(&off), Class::Spatial).unwrap();
    assert_eq!(
        (judged["unequal"].clone(), judged["passed"].clone()),
        (json!(1), json!(false))
    );
    assert!(sample_kind(&reference, Some(&answers[1..]), Class::Pointwise).is_err());
}

#[test]
fn the_export_comparison_holds_an_export_to_the_reference_and_to_itself() {
    let reference = raster(40, 30, |x, y| [x as u8 * 5, y as u8 * 7, 30]);
    let unrendered = export_kind(&reference, None, Class::Pointwise).unwrap();
    assert_eq!(unrendered["status"], "not-rendered");
    assert_eq!(unrendered["reference"]["stage"], json!([40, 30]));
    let judged = export_kind(&reference, Some((&reference, &reference)), Class::Pointwise).unwrap();
    assert_eq!(
        (judged["passed"].clone(), judged["repeatable"].clone()),
        (json!(true), json!(true))
    );
    let other = raster(40, 30, |x, y| [x as u8 * 5, y as u8 * 7, 31]);
    let judged = export_kind(&reference, Some((&reference, &other)), Class::Pointwise).unwrap();
    assert_eq!(
        (judged["passed"].clone(), judged["repeatable"].clone()),
        (json!(false), json!(false))
    );
}

#[test]
fn the_reference_at_a_view_is_reduced_or_cut_in_bands_as_one_pass_would_be() {
    let frame = raster(97, 61, |x, y| {
        [(x * 2) as u8, (y * 4) as u8, ((x * y) % 256) as u8]
    });
    for size in [(30, 19), (97, 61), (1, 1)] {
        assert_eq!(
            reduce(&frame.rgba, 4, (97, 61), size).unwrap(),
            tolerance::reduce_srgb8(&frame.rgba, 4, (97, 61), size).unwrap(),
            "{size:?}"
        );
    }
    let (candidate, expected) = (
        tolerance::reduce_srgb8(&frame.rgba, 4, (97, 61), (30, 19)).unwrap(),
        tolerance::reduce_srgb8(
            &raster(97, 61, |x, y| [(x * 2) as u8, (y * 4) as u8, 7]).rgba,
            4,
            (97, 61),
            (30, 19),
        )
        .unwrap(),
    );
    assert_eq!(
        compare(&candidate, &expected, (30, 19)).unwrap(),
        preview_error::compare(
            Rgb8::new(30, 19, &candidate).unwrap(),
            Rgb8::new(30, 19, &expected).unwrap(),
            [0, 0, 30, 19]
        )
        .unwrap()
    );
}
