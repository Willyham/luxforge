//! A paged derived source's cells (`docs/design/panorama-merge.md`, "Rendering, samples and
//! export"; `docs/design/gpu-first.md`, "Tolerance by output kind"): a source past the camera
//! limits has no whole frame, so its reference is the reference renderer's banded mode over level 0
//! ([`luxforge_core::qualification::paged_reference`]), never held whole.
//!
//! - **The picture at rest below 100%** is the overview, the stack drawn over the resident level,
//!   its tiles reduced to the view as the editor draws them; against the banded reference reduced
//!   to the same view by the reference's own linear-light reduction, recorded with `"overview":
//!   true`, which the gate holds to the class's overview limits ([`Class::overview_limits`]).
//! - **At 100%** the view plan over a window of level 0 around the visible region, as the photo
//!   surface holds it beside the level, against the banded reference's region, by the class's own
//!   limits. The picture in motion is the view plan in both, against the picture at rest and the
//!   reference, as for every source.
//! - **Samples** are the first GPU tile worker's reads of level 0 at the 5 × 5 grid, against the
//!   banded reference's bytes there; the byte on screen is not compared, its view needing a window
//!   of level 0 the harness does not hold for each point.
//! - **Export** is the GPU tile worker's stream of level 0, compared band by band with the banded
//!   reference's bands as they come, the figures taken over every pixel but the worst block, which
//!   is the worst within a band; the second worker's stream is held to the first by a digest.
//! - **The histogram** counts the overview and **the minimap's overview** is not drawn for a paged
//!   source here: each is a gap naming why, never a pass.
use super::*;
use std::{
    hash::{DefaultHasher, Hasher},
    sync::Arc,
};

/// `measure`'s cells for a paged source, recorded in `record`.
#[allow(clippy::too_many_arguments)]
pub(super) fn measure(
    (qualifier, exporters): (&Qualifier, Option<&Exporters>),
    surface: &mut HeadlessSurface,
    settings: &Settings,
    class: Class,
    opened: &Opened,
    evaluation: &Evaluation,
    cell: &str,
    record: &mut Value,
) -> Result<(), String> {
    luxforge_core::qualification::paged_reference(evaluation, &Cancel::never(), |reference| {
        let stage = reference.stage();
        record["reference"] = json!({
            "stage": [stage.0, stage.1],
            "renderer": "the reference's banded mode over level 0",
        });
        let level = gpu_source_of(1, evaluation.source())
            .ok_or("the paged source's resident level is not held")?;
        if settings.kinds.iter().any(|kind| kind.picture()) {
            for (id, view) in &settings.views {
                let measured = picture(
                    qualifier, surface, settings, evaluation, &level, reference, *view, class, cell,
                )
                .unwrap_or_else(|error| json!({"status": "error", "reason": error}));
                eprintln!("{cell} at {id}: {}", summary(&measured));
                record["views"][id] = measured;
            }
        }
        for kind in &settings.kinds {
            let measured = match kind {
                Kind::PictureAtRest | Kind::PictureInMotion => continue,
                Kind::Histogram => gap(
                    "a paged source's histogram counts its overview, which no limit judges against \
                     level 0's counts yet",
                ),
                Kind::Overview => gap("the minimap's overview of a paged source is not qualified"),
                Kind::Sample => match exporters {
                    Some(exporters) => samples(
                        &exporters.workers[0],
                        opened.client,
                        evaluation,
                        reference,
                        class,
                    )?,
                    None => gap("no adapter for the GPU tile workers"),
                },
                Kind::Export => match exporters {
                    Some(exporters) => export(exporters, evaluation, reference, class)?,
                    None => gap("no adapter for the GPU tile workers"),
                },
                Kind::ExportResized | Kind::Export16 | Kind::ExportSharpened => gap(
                    "a paged source's resized, 16-bit and sharpened exports are not qualified \
                     against the banded reference yet",
                ),
            };
            eprintln!("{cell}: {}: {measured}", kind.name());
            record["kinds"][kind.name()] = measured;
        }
        Ok(())
    })
    .map_err(|error| error.to_string())?
}

/// One view of a paged source: its picture at rest and the frame a drag draws, as [`super::picture`]
/// measures every other source's, against the banded reference at the view.
#[allow(clippy::too_many_arguments)]
fn picture(
    qualifier: &Qualifier,
    surface: &mut HeadlessSurface,
    settings: &Settings,
    evaluation: &Evaluation,
    level: &GpuSource,
    reference: &luxforge_core::qualification::PagedReference<'_>,
    view: View,
    class: Class,
    cell: &str,
) -> Result<Value, String> {
    let stage = reference.stage();
    let (gpu_view, bounds) = match view {
        View::Fit => (GpuView::Fit(fit_bounds()), Some(fit_bounds())),
        View::Percent(zoom) if zoom < 100.0 => match percent_bounds(stage, zoom) {
            Some(bounds) => (GpuView::Fit(bounds), Some(bounds)),
            None => {
                return Ok(gap(format!(
                    "at {zoom}% the photograph is not drawn below its size"
                )));
            }
        },
        View::Percent(zoom) => (
            GpuView::Region {
                rect: super::super::largest_view(stage, zoom).ok_or("no visible region")?,
                magnification: f64::from(zoom) / 100.0,
            },
            None,
        ),
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
    // The source the boundary is derived from: the resident level below 100%, and at 100% a window
    // of level 0 around the region, read through the chunk cache as the photo surface reads it.
    let paged = evaluation.source().paged().ok_or("a paged source")?;
    let window = if *request.key.source() == paged.identity() {
        let (width, height) = paged.dimensions();
        let rect = request.window.unwrap_or(Region {
            x0: 0,
            y0: 0,
            width,
            height,
        });
        let read = paged
            .window(
                [rect.x0, rect.y0, rect.width, rect.height],
                0,
                &Cancel::never(),
            )
            .map_err(|error| format!("the window of level 0: {error}"))?;
        Some(
            GpuSource::paged(2, (width, height))
                .and_then(|level| level.window(read.rect))
                .and_then(|window| window.filled(Arc::clone(&read.planes)))
                .ok_or("the window does not fit level 0")?,
        )
    } else {
        None
    };
    let source = window.as_ref().unwrap_or(level);
    let (boundary, origin, grid) = match derived_now(source, &plan, &request, 1) {
        Ok(derived) => derived,
        Err(reason) => return Ok(gap(reason)),
    };
    let region = request.key.region();
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
    // A light behind a spatial layer is the picture at rest's staged sweeps' own: its tiles, over
    // the level, drawn first.
    let staged = plan.lights.iter().any(luxforge_core::GpuLight::staged);
    let mut drawn_rest = None;
    if staged {
        let Some(Ok(tiles)) = &rest.tiles else {
            return Ok(gap("the picture at rest's tiles are not drawn"));
        };
        let handed = rest_now(level, tiles, 1)?;
        drawn_rest = Some(
            surface
                .rest(level, &handed)
                .map_err(|fallback| format!("the picture at rest in tiles: {fallback:?}"))?,
        );
    }
    let (motion, size) = match surface.draw(source, &converted) {
        Ok(drawn) => (rgb_codes(&drawn.codes), drawn.size),
        Err(fallback) => return Ok(gap(format!("the view plan is not drawn: {fallback:?}"))),
    };
    // The picture at rest: the overview's tiles reduced to the view below 100%, else the view
    // plan.
    let (at_rest, renderer, tiles) = match (&rest.tiles, region) {
        (Some(Ok(tiles)), None) if tiles.reduction.is_some() => {
            let handed = rest_now(level, tiles, 1)?;
            let view = handed.reduction.as_ref().map(|reduction| reduction.view);
            if view != Some(size) {
                return Err(format!(
                    "the tiles are reduced to {view:?}, the view plan draws {size:?}"
                ));
            }
            let drawn = match drawn_rest {
                Some(drawn) => drawn,
                None => surface
                    .rest(level, &handed)
                    .map_err(|fallback| format!("the picture at rest in tiles: {fallback:?}"))?,
            };
            (
                rgb_codes(&drawn.codes),
                "the overview's tiles reduced to the view".to_owned(),
                Some(drawn.figures.tiles as usize),
            )
        }
        _ => (motion.clone(), "the view plan".to_owned(), None),
    };
    // The banded reference at the view: its region at 100%, reduced to the view's bounds below.
    let expected = match (region, bounds) {
        (Some(rect), _) => {
            let rgba = reference
                .rgba([rect.x0, rect.y0, rect.width, rect.height])
                .map_err(|error| format!("the banded reference's region: {error}"))?;
            rgb_of(&rgba).collect::<Vec<u8>>()
        }
        (None, Some(bounds)) => {
            let reduced = reference
                .reduced(bounds)
                .map_err(|error| format!("the banded reference reduced: {error}"))?
                .ok_or("the banded reference fits the view unreduced")?;
            if (reduced.width, reduced.height) != size {
                return Err(format!(
                    "the banded reference is reduced to {}x{}, the GPU draws {size:?}",
                    reduced.width, reduced.height
                ));
            }
            rgb(&reduced)
        }
        (None, None) => return Err("a view with neither a region nor bounds".to_owned()),
    };
    let overview = region.is_none();
    let rest_against = compare(&at_rest, &expected, size)?;
    let motion_against_rest = compare(&motion, &at_rest, size)?;
    let motion_against = compare(&motion, &expected, size)?;
    let limits = if overview {
        class.overview_limits()
    } else {
        class.limits()
    };
    let passed = preview_error::verdict_within(&rest_against, class, limits).passed();
    let mut frames = Vec::new();
    if settings.all_frames || !passed {
        let name = format!("{cell}--{}", view.token());
        for (suffix, bytes) in [
            ("motion", &motion),
            ("rest", &at_rest),
            ("reference", &expected),
        ] {
            let file = format!("{name}-{suffix}");
            write_png(&settings.output.join("frames"), &file, size, bytes)?;
            frames.push(format!("frames/{file}.png"));
        }
    }
    Ok(json!({
        "status": "measured",
        "stage": [size.0, size.1],
        "region": region.map(|rect| [rect.x0, rect.y0, rect.width, rect.height]),
        "overview": overview,
        "boundary": {
            "derived": if window.is_some() { "a window of level 0" } else { "the resident level" },
            "size": [held.0, held.1],
            "origin": [origin.0, origin.1],
        },
        "reference": if overview {
            "the banded reference reduced to the view's size"
        } else {
            "the banded reference's visible region"
        },
        "at_rest": {
            "status": "measured",
            "renderer": renderer,
            "tiles": tiles,
            "against_reference": statistics(&rest_against),
            "limits": if overview { "overview" } else { "class" },
            "passed": passed,
        },
        "in_motion": {
            "settles_to": "the GPU's picture at rest",
            "against_settled": statistics(&motion_against_rest),
            "against_reference": statistics(&motion_against),
        },
        "charged_bytes": charged,
        "softer": null,
        "notes": [],
        "frames": frames,
    }))
}

/// The GPU tile worker's reads of `evaluation`'s output at [`sample_points`], one call and one
/// session, against the banded reference's bytes there by the display limit of `class`.
fn samples(
    worker: &GpuTiles,
    client: ClientId,
    evaluation: &Evaluation,
    reference: &luxforge_core::qualification::PagedReference<'_>,
    class: Class,
) -> Result<Value, String> {
    let points = sample_points(reference.stage());
    let (sender, answers) = std::sync::mpsc::channel();
    let (done, finished) = std::sync::mpsc::sync_channel(1);
    let (held, wanted) = (evaluation.clone(), points.clone());
    worker.submit(TileCall::caller(
        client,
        Cancel::new(),
        move |reads, cancel| {
            let mut session = reads.session(&held, cancel);
            for [x, y] in wanted {
                let rect = Region {
                    x0: x,
                    y0: y,
                    width: 1,
                    height: 1,
                };
                let _ = sender.send(session.read(ReadStage::Output, rect, ReadValues::Codes)?);
            }
            Ok(Value::Null)
        },
        move |result| {
            let _ = done.send(result);
        },
    ));
    finished
        .recv_timeout(luxforge_testbase::HANG)
        .map_err(|_| "the tile worker did not answer the samples".to_owned())?
        .map_err(|error| format!("the samples: {error}"))?;
    let read: Vec<_> = answers.try_iter().collect();
    if read.len() != points.len() {
        return Err(format!(
            "{} answers for {} points",
            read.len(),
            points.len()
        ));
    }
    let mut compared = Vec::with_capacity(points.len());
    for ([x, y], answer) in points.iter().zip(read) {
        if answer.answered != Drawn::gpu() {
            let reason = answer
                .answered
                .reason
                .as_ref()
                .map_or("no reason", TileFallback::code);
            return Ok(gap(format!(
                "{reason}: the reference answered the sample at ({x}, {y})"
            )));
        }
        let code = answer
            .code(*x, *y)
            .ok_or_else(|| format!("no code at ({x}, {y})"))?;
        let expected = reference
            .rgba([*x, *y, 1, 1])
            .map_err(|error| format!("the banded reference at ({x}, {y}): {error}"))?;
        compared.push(tolerance::Sample {
            answered: [code[0], code[1], code[2]],
            on_screen: None,
            reference: [expected[0], expected[1], expected[2]],
        });
    }
    let error = tolerance::samples(&compared)?;
    Ok(json!({
        "status": "measured",
        "points": points,
        "reference": "the banded reference's bytes",
        "samples": error.samples,
        "on_screen": error.on_screen,
        "unequal": error.unequal,
        "mean": error.mean,
        "p99": error.p99,
        "mean_delta_l": error.mean_delta_l,
        "max": error.max,
        "passed": error.passed(class),
        "notes": ["the byte on screen is not compared for a paged source"],
    }))
}

/// The GPU's export of `evaluation`, streamed by the first of `exporters`' workers, against the
/// banded reference's bands as they come, and the second worker's stream held to the first by a
/// digest; or a gap naming why the GPU did not export it.
fn export(
    exporters: &Exporters,
    evaluation: &Evaluation,
    reference: &luxforge_core::qualification::PagedReference<'_>,
    class: Class,
) -> Result<Value, String> {
    let (width, height) = reference.stage();
    let [one, two] = &exporters.workers;
    let output = OutputSpec::full(width, height);
    let mut stream = match one.stream(evaluation, output, &Cancel::new()) {
        Ok(stream) => stream,
        Err(fallback) => return Ok(gap(refused(&fallback))),
    };
    let row_bytes = width as usize * 3;
    let (mut pending, mut digest, mut drawn_bands) = (Vec::new(), DefaultHasher::new(), 0_u64);
    let mut figures = Figures::default();
    let mut rows = 0;
    for band in reference.bands() {
        let (count, rgba) = band.map_err(|error| format!("the banded reference: {error}"))?;
        let expected: Vec<u8> = rgb_of(&rgba).collect();
        while pending.len() < expected.len() {
            match stream.next() {
                Some(Ok(drawn)) => {
                    drawn_bands += 1;
                    let drawn = drawn
                        .rgba8()
                        .map_err(|error| format!("the GPU's stream: {error}"))?;
                    let codes: Vec<u8> = rgb_of(drawn).collect();
                    digest.write(&codes);
                    pending.extend(codes);
                }
                Some(Err(error)) => {
                    return Ok(gap(match stream.fallback() {
                        Some(fallback) => refused(fallback),
                        None => format!("the stream failed: {error}"),
                    }));
                }
                None => {
                    return Err(format!(
                        "the GPU's stream ended at row {} of {height}",
                        rows + pending.len() / row_bytes
                    ));
                }
            }
        }
        let candidate: Vec<u8> = pending.drain(..expected.len()).collect();
        figures.add(&compare(&candidate, &expected, (width, count))?);
        rows += count as usize;
    }
    if !pending.is_empty() || stream.next().is_some() {
        return Err("the GPU's stream holds rows past the reference's".to_owned());
    }
    let first = digest.finish();
    let mut again = DefaultHasher::new();
    if let Err(reason) = streamed(two, evaluation, output, |rows| {
        if let PixelRows::Eight(rgba) = rows {
            again.write(&rgb_of(rgba).collect::<Vec<u8>>());
        }
    }) {
        return Ok(gap(format!("the second worker: {reason}")));
    }
    let error = tolerance::ExportError {
        statistics: figures.statistics(),
        repeatable: again.finish() == first,
    };
    Ok(json!({
        "status": "measured",
        "renderer": "gpu",
        "against": "the banded reference's bands, as the reference export encodes them",
        "stage": [width, height],
        "bands": drawn_bands,
        "statistics": statistics(&error.statistics),
        "worst_block": "the worst within a band",
        "repeatable": error.repeatable,
        "passed": error.passed(class),
    }))
}

/// The export's figures over its bands: the mean and signed mean ΔL\* over every pixel, the worst
/// block within a band, and the p99 and largest difference as the largest of any band's, which is
/// never below the whole export's p99.
#[derive(Default)]
struct Figures {
    pixels: u64,
    mean: f64,
    mean_delta_l: f64,
    worst: Option<Statistics>,
}

impl Figures {
    fn add(&mut self, band: &Statistics) {
        let (before, after) = (self.pixels as f64, (self.pixels + band.pixels) as f64);
        self.mean = (self.mean * before + band.mean * band.pixels as f64) / after;
        self.mean_delta_l =
            (self.mean_delta_l * before + band.mean_delta_l * band.pixels as f64) / after;
        self.pixels += band.pixels;
        let worst = self.worst.get_or_insert(*band);
        if band.worst_block > worst.worst_block {
            worst.worst_block = band.worst_block;
            worst.worst_block_origin = band.worst_block_origin;
        }
        worst.p99 = worst.p99.max(band.p99);
        worst.max = worst.max.max(band.max);
    }

    fn statistics(&self) -> Statistics {
        let worst = self.worst.unwrap_or(Statistics {
            pixels: 0,
            mean: f64::NAN,
            worst_block: f64::NAN,
            worst_block_origin: [0, 0],
            p99: f64::NAN,
            mean_delta_l: f64::NAN,
            max: f64::NAN,
        });
        Statistics {
            pixels: self.pixels,
            mean: self.mean,
            mean_delta_l: self.mean_delta_l,
            ..worst
        }
    }
}

/// A paged source's cell, as the gate runs one on the corpus's `derived-paged` at a moderate size:
/// Fit's overview against the banded reference reduced to the view and 100% over a window of level
/// 0 against its region, each measured; its samples and its export measured by the GPU tile
/// workers, the export repeatable; the histogram and the minimap's overview gaps naming why. The
/// figures are printed, not judged: the corpus run qualifies them. A host with no adapter prints
/// that it was skipped and asserts nothing.
#[test]
#[slow]
fn paged_source_render_qualification_cells_measure_against_the_banded_reference() {
    let test = "paged_source_render_qualification_cells_measure_against_the_banded_reference";
    let Some(qualifier) = headless(test) else {
        return;
    };
    let output = luxforge_testbase::paths::temp_dir("paged-qualification");
    std::fs::create_dir_all(output.join("catalogs")).unwrap();
    std::fs::create_dir_all(output.join("frames")).unwrap();
    let settings = Settings {
        output: output.clone(),
        views: vec![
            ("fit".to_owned(), View::Fit),
            ("100%".to_owned(), View::Percent(100.0)),
        ],
        kinds: Kind::ALL.to_vec(),
        families: None,
        recipes: None,
        sources: None,
        all_frames: false,
        motion_gated: None,
    };
    let source = CorpusSource {
        id: "derived-paged".to_owned(),
        path: PathBuf::new(),
        raw: false,
        derived: Some((16_500, 300)),
    };
    let recipe = json!({
        "id": "detail-moderate",
        "family": "detail",
        "class": "spatial",
        "steps": [{"api": {"method": "edit.set-detail",
                           "params": {"luminance": 40, "colour": 40, "sharpening": 50,
                                      "radius": 1.0}}}],
    });
    let exporters = Exporters::new(test);
    let mut surface = qualifier.surface();
    let record = pair(
        &qualifier,
        &mut surface,
        &settings,
        exporters.as_ref(),
        &recipe,
        &source,
    );
    eprintln!("{test}: {record}");
    assert_eq!(record["status"], "measured", "{record}");
    assert_eq!(record["reference"]["stage"], json!([16_500, 300]));
    let fit = &record["views"]["fit"];
    assert_eq!(fit["status"], "measured", "{fit}");
    assert_eq!(fit["overview"], true);
    assert_eq!(fit["boundary"]["derived"], "the resident level");
    let full = &record["views"]["100%"];
    assert_eq!(full["status"], "measured", "{full}");
    assert_eq!(full["overview"], false);
    assert_eq!(full["boundary"]["derived"], "a window of level 0");
    if exporters.is_some() {
        let export = &record["kinds"]["export"];
        assert_eq!(export["status"], "measured", "{export}");
        assert_eq!(export["repeatable"], true, "{export}");
        assert_eq!(record["kinds"]["sample"]["status"], "measured");
    }
    for kind in ["histogram", "overview"] {
        assert_eq!(record["kinds"][kind]["status"], "gap", "{kind}");
    }
    let _ = std::fs::remove_dir_all(output);
}
