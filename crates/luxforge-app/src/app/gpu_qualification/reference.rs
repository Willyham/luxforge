//! The reference comparison harness, which the release gate runs (`docs/design/gpu-first.md`,
//! "Tolerance by output kind"; `cargo xtask gpu-qualification`). For every stack of the
//! qualification corpus on every source this host has, it renders the reference renderer's exact
//! whole frame — the frame export renders — and, at each view the corpus lists, the editor's GPU
//! frames of the stack, drawn by the photo surface's own drawing on a headless device
//! ([`HeadlessSurface`]) from the photograph's source held on the GPU as the editor holds it:
//! **Fit**, at the bounds of an evidence run's window ([`fit_bounds`]); **33%** and **50%**, whose
//! view is planned at the displayed size as Fit's is, with other bounds ([`percent_bounds`]); and
//! **100%**, the visible region of the largest window the owner's display holds
//! ([`super::largest_view`]). The reference at a view is the reference frame reduced to the view's
//! size by an independent area-weighted average of its linear light ([`tolerance::reduce_srgb8`]),
//! or at 100% its visible region, unreduced.
//!
//! It records the picture as the design's two kinds, by its recorded default of 2026-10-05:
//!
//! - **The picture at rest**, against the reference: the stack's picture at rest as the editor
//!   plans and draws it — at Fit, 33% and 50% its tiles at full resolution, each over its window
//!   cut from the source, reduced to the view's size as they are drawn; where the view draws the
//!   stage at its own size, at 100%, or where the tiles cannot be drawn, its view plan over the
//!   window the view reads.
//! - **The picture in motion**, the frame a drag draws — the view plan over the boundary the
//!   surface derives from the source, the source reduced to the view's proxy at Fit and below 100%
//!   and a window of it cut at full scale at 100% — against the picture at rest it settles to. Its
//!   distance from the reference is recorded beside it, which the gate judges instead on request.
//!
//! **Export** is the GPU's ([`export_kind`]): each stack's output stage streamed in tiles by the
//! desktop's GPU tile worker on this host's adapter, as the export lane streams an export, after
//! the reference frame's render has stored the stack's global estimates, as a settled frame
//! stores them; its codes, which the encoder reads, against the reference frame's, which the
//! reference export encodes, by the display limit of the stack's class over every pixel; and the
//! same stream drawn again by a second worker on a device of its own, which must be the same bytes.
//! A stream the GPU refuses or stops drawing is a gap naming why, and a stack whose export the
//! export lane would hand the reference before any render had stored its Dehaze light says so.
//!
//! The histogram and clipping counts and samples are computed on the reference and recorded as not
//! rendered by the GPU: their comparisons ([`histogram_kind`], [`sample_kind`]) take the GPU's
//! output once the stage that renders it supplies it.
//!
//! It writes `cells.json` to `LUXFORGE_GPU_CORPUS_OUTPUT`, rewritten after every stack, and judges
//! nothing: `cargo xtask gpu-qualification` holds every cell to its limit and writes the report. A
//! cell's frames are written as PNGs under `frames/` only when a frame the gate judges passes a
//! limit — the picture at rest, and the motion frame where the owner gates it
//! (`LUXFORGE_GPU_QUALIFICATION_MOTION=rest` or `reference`) — or for every cell with
//! `LUXFORGE_GPU_QUALIFICATION_FRAMES=all`.
use super::{CorpusSource, EMPTY_MASK, Opened, corpus_sources, fit_bounds, headless, write_png};
use crate::app::gpu_tiles::GpuTiles;
use crate::app::{
    gpu_plan::surface_plan_over,
    gpu_preview::{derived_now, gpu_source_of, rest_now},
};
use luxforge_core::{
    Cancel, Evaluation, GpuAnswer, GpuFallback, GpuPreview, GpuView, PreviewRequest, ProxyBounds,
    Raster, RenderOptions, RendererReason, STREAM_TILE_SIDES,
    analysis::Report,
    tiles::{TileFallback, TileService},
};
use luxforge_reference::{
    preview_error::{self, Class, Rgb8, Statistics},
    tolerance::{self, Kind},
};
use luxforge_ui::photo_surface::{
    GpuPlan, GpuProgram, GpuSource, GpuStep, MaskedColour, TexelMap,
    gpu_preview::{headless::HeadlessSurface, qualification::Qualifier},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
/// Why the histogram and clipping counts are not compared on this branch.
const HISTOGRAM_NOT_RENDERED: &str = "the GPU does not render the histogram and clipping counts \
    yet: a GPU reduction over the full stage is stage 2's, and until it lands the counts come from \
    the reference renderer";
/// Why samples are not compared on this branch.
const SAMPLE_NOT_RENDERED: &str = "the GPU does not answer samples yet: a sample from a GPU tile \
    render is stage 4's, and until it lands samples come from the reference renderer";

/// The two GPU tile workers the export kind streams each stack's output stage through, as the
/// export lane streams an export, each on a device of its own on this host's adapter: the second
/// draws every stream again, which must be the same bytes.
struct Exporters {
    /// The adapter's backend and name.
    adapter: (String, String),
    workers: [GpuTiles; 2],
}

impl Exporters {
    /// Two workers on this host's adapter, or none without one.
    fn new(test: &str) -> Option<Self> {
        let adapter = crate::app::gpu_tiles_tests::host_adapter(test)?;
        let worker = || GpuTiles::new(Some(adapter.clone()), false);
        Some(Self {
            workers: [worker(), worker()],
            adapter,
        })
    }
}

/// The GPU's export of a stack: the codes of its output stage, three bytes a pixel, as the first
/// worker's stream handed them to the encoder, whether the second's were the same bytes, and the
/// bands and tiles the first drew it in; or why the GPU did not export it.
enum Exported {
    Drawn {
        codes: Vec<u8>,
        repeatable: bool,
        bands: u64,
        tiles: u64,
    },
    Gap(String),
}

/// `evaluation`'s output stage streamed through each of `exporters`' workers in turn, the second
/// compared band by band with the first.
fn exported(exporters: &Exporters, evaluation: &Evaluation) -> Exported {
    let [one, two] = &exporters.workers;
    let before = one.figures();
    let mut codes = Vec::new();
    if let Err(reason) = streamed(one, evaluation, |rgba| codes.extend(rgb_of(rgba))) {
        return Exported::Gap(reason);
    }
    let after = one.figures();
    let (mut at, mut repeatable) = (0, true);
    let again = streamed(two, evaluation, |rgba| {
        let band: Vec<u8> = rgb_of(rgba).collect();
        repeatable &= codes.get(at..at + band.len()) == Some(band.as_slice());
        at += band.len();
    });
    if let Err(reason) = again {
        return Exported::Gap(format!("the second worker: {reason}"));
    }
    Exported::Drawn {
        repeatable: repeatable && at == codes.len(),
        codes,
        bands: after.bands - before.bands,
        tiles: after.tiles - before.tiles,
    }
}

/// The three codes of each pixel of `rgba`.
fn rgb_of(rgba: &[u8]) -> impl Iterator<Item = u8> + '_ {
    rgba.chunks_exact(4)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
}

/// Stream `evaluation`'s output stage through `worker`, handing each band's codes to `take`; why
/// the reference renders the export instead when the worker refuses the stream or stops drawing
/// it, as the export's result names it, or why the stream failed.
fn streamed(
    worker: &GpuTiles,
    evaluation: &Evaluation,
    mut take: impl FnMut(&[u8]),
) -> Result<(), String> {
    let mut stream = worker
        .stream(evaluation, &Cancel::new())
        .map_err(|fallback| refused(&fallback))?;
    while let Some(band) = stream.next() {
        match band {
            Ok(band) => take(&band.rgba),
            Err(error) => {
                return Err(match stream.fallback() {
                    Some(fallback) => refused(fallback),
                    None => format!("the stream failed: {error}"),
                });
            }
        }
    }
    Ok(())
}

/// Why the reference renders an export, as its result names it, with what the GPU said.
fn refused(fallback: &TileFallback) -> String {
    let reason = RendererReason::from(fallback).as_str();
    match fallback {
        TileFallback::Plan(plan) => format!("the reference renders it: {reason} ({plan})"),
        TileFallback::Budget { requested, budget } => format!(
            "the reference renders it: {reason} ({requested} bytes for one tile, past {budget})"
        ),
        TileFallback::Unavailable(_) | TileFallback::Stage(_) => {
            format!("the reference renders it: {reason}")
        }
    }
}

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
    /// What the gate holds the picture in motion to, where the owner gates it: its misses have
    /// their frames written. `None` while it is reported ungated.
    motion_gated: Option<MotionGate>,
}

/// What the picture in motion is gated against, where the owner asks for it to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MotionGate {
    /// The GPU's picture at rest it settles to.
    Rest,
    /// The reference frame at the view's size.
    Reference,
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
/// with `LUXFORGE_GPU_QUALIFICATION_FRAMES=all` it writes every cell's frames, and with
/// `LUXFORGE_GPU_QUALIFICATION_MOTION=rest` or `reference` it writes the frames of a motion frame past
/// a limit against that frame, which the gate then judges. Without an adapter it records that it
/// was skipped and measures nothing.
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
        motion_gated: match std::env::var("LUXFORGE_GPU_QUALIFICATION_MOTION").as_deref() {
            Ok("rest") => Some(MotionGate::Rest),
            Ok("reference") => Some(MotionGate::Reference),
            _ => None,
        },
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
        "motion_gated": match settings.motion_gated {
            None => Value::Null,
            Some(MotionGate::Rest) => json!("rest"),
            Some(MotionGate::Reference) => json!("reference"),
        },
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
    // The export kind's two GPU tile workers, on this host's adapter.
    let exporters = settings
        .kinds
        .contains(&Kind::Export)
        .then(|| Exporters::new(test))
        .flatten();
    if let Some(exporters) = &exporters {
        let (backend, name) = &exporters.adapter;
        document["export_adapter"] = json!({"backend": backend, "adapter": name});
    }
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
                    // The editor's surface on the qualifier's device, a pipeline of its own for
                    // each stack, as the editor's holds one photograph's resources at a time:
                    // nothing one stack holds or charges reaches the next.
                    let mut surface = qualifier.surface();
                    pair(
                        &qualifier,
                        &mut surface,
                        &settings,
                        exporters.as_ref(),
                        recipe,
                        source,
                    )
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
    surface: &mut HeadlessSurface,
    settings: &Settings,
    exporters: Option<&Exporters>,
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
        (qualifier, exporters),
        surface,
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
#[allow(clippy::too_many_arguments)]
fn measure(
    (qualifier, exporters): (&Qualifier, Option<&Exporters>),
    surface: &mut HeadlessSurface,
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
    // global estimates, which the picture at rest's plans read as the editor's do once the stack's
    // exact phase has stored them.
    let job = crate::app::tasks::ready_preview_job(
        &opened.owner,
        PreviewRequest::new(opened.client, opened.asset.clone()),
    )?;
    let evaluation = job.evaluation;
    // Before anything has rendered the stack: whether the export lane would stream it, or hand it
    // the reference for a Dehaze light no render has stored yet, which stage 3's per-frame light
    // replaces. Planning reads no pixel.
    let without_stored_light = settings
        .kinds
        .contains(&Kind::Export)
        .then(|| luxforge_core::plan_stream(&evaluation, STREAM_TILE_SIDES[0]).err())
        .flatten()
        .filter(|fallback| {
            matches!(
                fallback,
                TileFallback::Plan(GpuFallback::RegionEstimate { .. })
            )
        })
        .map(|fallback| RendererReason::from(&fallback).as_str());
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
    if settings.kinds.iter().any(|kind| kind.picture()) {
        // The photograph's source as the editor holds it on the GPU, which every frame of the
        // stack is derived from.
        let gpu = gpu_source_of(1, evaluation.source())
            .ok_or("the photograph's source cannot be held on the GPU")?;
        for (id, view) in &settings.views {
            let measured = picture(
                qualifier,
                surface,
                settings,
                &evaluation,
                &gpu,
                &reference,
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
            Kind::PictureAtRest | Kind::PictureInMotion => continue,
            Kind::Histogram => histogram_kind(&reference, None)?,
            Kind::Sample => sample_kind(&reference, None, class)?,
            Kind::Export => {
                // Streamed now the reference frame's render has stored the stack's estimates, as
                // a settled frame stores them.
                let drawn = match exporters {
                    Some(exporters) => exported(exporters, &evaluation),
                    None => Exported::Gap("no adapter for the GPU tile workers".to_owned()),
                };
                let mut measured = export_kind(&reference, drawn, class)?;
                if let Some(reason) = without_stored_light {
                    measured["without_stored_light"] = json!(reason);
                }
                eprintln!("{cell}: export: {measured}");
                measured
            }
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
            format!(
                "at rest ({}) against the reference {} | motion against the picture at rest {} | \
                 motion against the reference {}",
                measured["at_rest"]["renderer"].as_str().unwrap_or("?"),
                figures(&measured["at_rest"]["against_reference"]),
                figures(&measured["in_motion"]["against_settled"]),
                figures(&measured["in_motion"]["against_reference"]),
            )
        }
        Some(status) => format!("{status}: {}", measured["reason"].as_str().unwrap_or("")),
        None => "unrecorded".to_owned(),
    }
}

/// A view that is not drawn here, and why.
fn gap(reason: impl Into<String>) -> Value {
    json!({"status": "gap", "reason": reason.into()})
}

/// A drawn frame's codes, three bytes a pixel.
fn rgb_codes(codes: &[[u8; 4]]) -> Vec<u8> {
    codes
        .iter()
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

/// Whether the mask of `plan`'s first operation, a masked colour step, selects nothing on its
/// boundary: the plan drawn with that step's units replaced by one that moves every value at
/// least half the range, against its boundary drawn with no step, every code alike. A coverage
/// below one code's share of the range is read as none. `None` when the first operation is not a
/// masked colour step.
fn selects_nothing(
    surface: &mut HeadlessSurface,
    gpu: &GpuSource,
    plan: &GpuPlan,
) -> Result<Option<bool>, String> {
    let Some(GpuStep::Masked(masked)) = plan.steps.first() else {
        return Ok(None);
    };
    let flip = GpuProgram::new(
        "lf_test_flip",
        "fn lf_test_flip(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
         -> vec3<f32> {\n    return select(vec3<f32>(1.0), vec3<f32>(0.0), rgb > \
         vec3<f32>(0.5));\n}\n",
    );
    // Over the boundary alone, with no region or geometry tail: the mask's own field.
    let covered = GpuPlan {
        boundary: plan.boundary.clone(),
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::Masked(MaskedColour {
            units: vec![flip],
            ..masked.clone()
        })],
        region: None,
    };
    let input = GpuPlan {
        steps: Vec::new(),
        ..covered.clone()
    };
    let drawn = |surface: &mut HeadlessSurface, plan: &GpuPlan| {
        surface
            .draw(gpu, plan)
            .map(|drawn| drawn.codes)
            .map_err(|fallback| format!("the mask's coverage: {fallback:?}"))
    };
    let (covered, input) = (drawn(surface, &covered)?, drawn(surface, &input)?);
    Ok(Some(covered == input))
}

/// The picture at `view`, drawn by `surface` from `gpu`, the photograph's source held on the GPU,
/// as the editor draws it: the picture at rest the stack's job plans there — its tiles at full
/// resolution reduced to the view's size at Fit and below 100%, else its view plan over the
/// window the view reads — against the reference at the view's size; and the frame a drag draws,
/// the view plan over the boundary the surface derives from the source, against the picture at
/// rest it settles to and against the reference. A view the editor would not draw on the GPU — a
/// plan the surface cannot run, or a boundary or slot past the budget the editor holds them to —
/// is a gap naming why.
#[allow(clippy::too_many_arguments)]
fn picture(
    qualifier: &Qualifier,
    surface: &mut HeadlessSurface,
    settings: &Settings,
    evaluation: &Evaluation,
    gpu: &GpuSource,
    reference: &Raster,
    view: View,
    class: Class,
    cell: &str,
) -> Result<Value, String> {
    let stage = (reference.width, reference.height);
    let output = &settings.output;
    // The view as the desktop asks the owner to plan the stack's picture at rest for it.
    let gpu_view = match view {
        View::Fit => GpuView::Fit(fit_bounds()),
        View::Percent(zoom) if zoom < 100.0 => match percent_bounds(stage, zoom) {
            Some(bounds) => GpuView::Fit(bounds),
            None => {
                return Ok(gap(format!(
                    "at {zoom}% the photograph is not drawn below its size"
                )));
            }
        },
        View::Percent(zoom) => GpuView::Region {
            rect: super::largest_view(stage, zoom).ok_or("no visible region")?,
            magnification: f64::from(zoom) / 100.0,
        },
    };
    let rest = luxforge_core::qualification::rest_plan(evaluation, gpu_view)
        .map_err(|error| error.to_string())?;
    let (plan, request) = match rest.view {
        GpuPreview {
            answer: GpuAnswer::Plan(plan),
            boundary: Some(request),
            ..
        } => (*plan, request),
        GpuPreview {
            answer: GpuAnswer::Fallback(reason),
            ..
        } => {
            return Ok(gap(format!(
                "{}: the GPU stage does not draw this stack",
                reason.code()
            )));
        }
        GpuPreview { .. } => return Ok(gap("the view plan has no boundary")),
    };
    // The motion frame: the view plan over its boundary derived from the source, as a drag and the
    // stack at rest hold it.
    let (boundary, origin, grid) = match derived_now(gpu, &plan, &request, 1) {
        Ok(derived) => derived,
        Err(reason) => return Ok(gap(reason)),
    };
    let region = request.key.region();
    let derived = match request.key.plan() {
        Some(_) => "reduce",
        None => "cut",
    };
    let held = boundary.size();
    let converted = match surface_plan_over(&plan, boundary, origin, grid.as_ref(), region) {
        Ok(converted) => converted,
        Err(unrunnable) => {
            return Ok(gap(format!(
                "{}: the surface cannot run {unrunnable:?} yet",
                unrunnable.code()
            )));
        }
    };
    let charged = qualifier
        .charged_bytes(&converted)
        .map_err(|reason| format!("{reason:?}"))?;
    let (motion, size) = match surface.draw(gpu, &converted) {
        Ok(drawn) => (rgb_codes(&drawn.codes), drawn.size),
        Err(fallback) => return Ok(gap(format!("the view plan is not drawn: {fallback:?}"))),
    };
    // A mask that selects nothing on this source says nothing about its coverage: noted.
    let mut notes = Vec::new();
    if selects_nothing(surface, gpu, &converted)? == Some(true) {
        notes.push(EMPTY_MASK.to_owned());
    }
    // The picture at rest: its tiles reduced to the view where the job plans them, else the view
    // plan, the motion frame's own.
    let (at_rest, renderer, tiles) = match (rest.tiles, region) {
        (Some(Ok(tiles)), None) => {
            let handed = rest_now(gpu, &tiles, 1)?;
            if handed.view != size {
                return Err(format!(
                    "the tiles are reduced to {:?}, the view plan draws {size:?}",
                    handed.view
                ));
            }
            let drawn = surface
                .rest(gpu, &handed)
                .map_err(|fallback| format!("the picture at rest in tiles: {fallback:?}"))?;
            (
                rgb_codes(&drawn.codes),
                "the picture at rest in tiles".to_owned(),
                Some(handed.tiles.len()),
            )
        }
        (Some(Err(reason)), _) => (
            motion.clone(),
            format!("the view plan, the tiles not drawn: {}", reason.code()),
            None,
        ),
        _ => (motion.clone(), "the view plan".to_owned(), None),
    };
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
    let rest_against = compare(&at_rest, &expected, size)?;
    let motion_against_rest = compare(&motion, &at_rest, size)?;
    let motion_against = compare(&motion, &expected, size)?;
    let at_rest_value = at_rest_kind(&rest_against, renderer, tiles, class);
    // What the gate judges, past a limit: the frames are written. The picture in motion is
    // judged only where the owner gates it.
    let motion_missed = match settings.motion_gated {
        None => false,
        Some(MotionGate::Rest) => !preview_error::verdict(&motion_against_rest, class).passed(),
        Some(MotionGate::Reference) => !preview_error::verdict(&motion_against, class).passed(),
    };
    let (width, height) = size;
    let mut frames = Vec::new();
    if settings.all_frames
        || motion_missed
        || !preview_error::verdict(&rest_against, class).passed()
    {
        let name = format!("{cell}--{}", view.token());
        for (suffix, bytes) in [
            ("motion", &motion),
            ("rest", &at_rest),
            ("reference", &expected),
        ] {
            let file = format!("{name}-{suffix}");
            write_png(&output.join("frames"), &file, (width, height), bytes)?;
            frames.push(format!("frames/{file}.png"));
        }
    }
    Ok(json!({
        "status": "measured",
        "stage": [width, height],
        "region": region.map(|rect| [rect.x0, rect.y0, rect.width, rect.height]),
        "boundary": {"derived": derived, "size": [held.0, held.1], "origin": [origin.0, origin.1]},
        "reference": match region {
            Some(_) => "the reference frame's visible region",
            None if size == stage => "the reference frame",
            None => "the reference frame reduced to the view's size",
        },
        "at_rest": at_rest_value,
        "in_motion": {
            "settles_to": "the GPU's picture at rest",
            "against_settled": statistics(&motion_against_rest),
            "against_reference": statistics(&motion_against),
        },
        "charged_bytes": charged,
        "notes": notes,
        "frames": frames,
    }))
}

/// The picture at rest at one view: the GPU's frame at rest against the reference frame at the
/// view's size, `against`, held to `class`'s limits, with the renderer that drew it and the tiles
/// it was drawn in.
fn at_rest_kind(
    against: &Statistics,
    renderer: String,
    tiles: Option<usize>,
    class: Class,
) -> Value {
    json!({
        "status": "measured",
        "renderer": renderer,
        "tiles": tiles,
        "against_reference": statistics(against),
        "passed": preview_error::verdict(against, class).passed(),
    })
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

/// Export: the GPU's export of the stack, `exported`, against the reference export, by the display
/// limit of `class` over every pixel of the codes each encodes — the GPU's stream's and the
/// reference's exact whole frame, `reference` — and repeatable, its second stream the same bytes;
/// or a gap naming why the GPU did not export it, never a pass.
fn export_kind(reference: &Raster, exported: Exported, class: Class) -> Result<Value, String> {
    let stage = [reference.width, reference.height];
    let (codes, repeatable, bands, tiles) = match exported {
        Exported::Gap(reason) => {
            return Ok(json!({
                "status": "gap",
                "reason": reason,
                "reference": {"stage": stage},
            }));
        }
        Exported::Drawn {
            codes,
            repeatable,
            bands,
            tiles,
        } => (codes, repeatable, bands, tiles),
    };
    let expected = rgb(reference);
    if codes.len() != expected.len() {
        return Err(format!(
            "the GPU's export holds {} codes where the reference's {} × {} stage holds {}",
            codes.len(),
            stage[0],
            stage[1],
            expected.len()
        ));
    }
    let error = tolerance::ExportError {
        statistics: compare(&codes, &expected, (reference.width, reference.height))?,
        repeatable,
    };
    Ok(json!({
        "status": "measured",
        "renderer": "gpu",
        "against": "the reference export's codes: the exact whole frame its encoder reads",
        "stage": stage,
        "bands": bands,
        "tiles": tiles,
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
    let gap = export_kind(
        &reference,
        Exported::Gap("the reference renders it: region-estimate".into()),
        Class::Pointwise,
    )
    .unwrap();
    assert_eq!(gap["status"], "gap", "never a pass");
    assert_eq!(gap["reason"], "the reference renders it: region-estimate");
    assert_eq!(gap["reference"]["stage"], json!([40, 30]));
    let drawn = |codes: Vec<u8>, repeatable| Exported::Drawn {
        codes,
        repeatable,
        bands: 2,
        tiles: 4,
    };
    let judged = export_kind(&reference, drawn(rgb(&reference), true), Class::Pointwise).unwrap();
    assert_eq!(
        (judged["passed"].clone(), judged["repeatable"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(
        (judged["bands"].clone(), judged["tiles"].clone()),
        (json!(2), json!(4))
    );
    // Within the limit but not the same bytes twice: not a pass.
    let judged = export_kind(&reference, drawn(rgb(&reference), false), Class::Pointwise).unwrap();
    assert_eq!(judged["passed"], false);
    let far = rgb(&raster(40, 30, |x, y| [x as u8 * 5, y as u8 * 7, 90]));
    let judged = export_kind(&reference, drawn(far, true), Class::Spatial).unwrap();
    assert_eq!(judged["passed"], false, "{judged}");
    assert!(export_kind(&reference, drawn(vec![0; 9], true), Class::Pointwise).is_err());
}

/// The GPU's export of a stack streamed through two workers on this host's adapter, as the export
/// lane streams it: the second the same bytes as the first, and each band's codes in order.
#[test]
fn an_exported_stack_is_streamed_twice_and_compared_band_by_band() {
    let test = "an_exported_stack_is_streamed_twice_and_compared_band_by_band";
    let Some(exporters) = Exporters::new(test) else {
        return;
    };
    for worker in &exporters.workers {
        worker.draw_streams_at(vec![64]);
    }
    let (_, recipe) = crate::app::gpu_tiles_tests::families()
        .into_iter()
        .find(|(family, _)| *family == "Presence after Detail")
        .expect("the family");
    let source = crate::app::gpu_window_tests::source(luxforge_core::BoundaryFormat::Half);
    let registry = std::sync::Arc::new(luxforge_core::ModuleRegistry::builtin());
    let context = luxforge_core::RenderContext::new();
    let frame = luxforge_core::render(
        &registry,
        &source,
        &recipe,
        RenderOptions::exact(&Cancel::never()),
        &context,
    )
    .unwrap()
    .frame(luxforge_core::SnapshotId::new())
    .unwrap();
    let evaluation = Evaluation::new(
        registry,
        context,
        source,
        crate::app::testing::entry(&luxforge_core::AssetId::new(), 1, None),
        recipe,
        None,
    );
    let Exported::Drawn {
        codes,
        repeatable,
        bands,
        tiles,
    } = exported(&exporters, &evaluation)
    else {
        panic!("{test}: the stream was refused");
    };
    assert!(repeatable, "two devices, the same bytes");
    assert_eq!(codes.len(), rgb(&frame).len());
    assert_eq!(bands, u64::from(frame.height.div_ceil(64)));
    assert!(tiles >= bands);
    let judged = export_kind(
        &frame,
        Exported::Drawn {
            codes,
            repeatable,
            bands,
            tiles,
        },
        Class::Spatial,
    )
    .unwrap();
    eprintln!("{test}: {judged}");
    assert_eq!(judged["passed"], true, "{judged}");
}

#[test]
fn the_at_rest_comparison_judges_the_gpus_frame_against_the_reference() {
    let reference = raster(40, 30, |x, y| [x as u8 * 5, y as u8 * 7, 30]);
    let expected = rgb(&reference);
    let same = compare(&expected, &expected, (40, 30)).unwrap();
    let judged = at_rest_kind(&same, "the view plan".into(), None, Class::Pointwise);
    assert_eq!(
        (judged["status"].clone(), judged["passed"].clone()),
        (json!("measured"), json!(true))
    );
    assert_eq!(judged["renderer"], "the view plan");
    let far = rgb(&raster(40, 30, |x, y| [x as u8 * 5, y as u8 * 7, 90]));
    let apart = compare(&far, &expected, (40, 30)).unwrap();
    let judged = at_rest_kind(
        &apart,
        "the picture at rest in tiles".into(),
        Some(6),
        Class::Spatial,
    );
    assert_eq!(judged["passed"], false, "{judged}");
    assert_eq!(judged["tiles"], 6);
    assert!(compare(&far[3..], &expected, (40, 30)).is_err());
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
