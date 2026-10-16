//! `cargo xtask lr-align measure`: the rig's measurement and fitting
//! (`docs/design/lightroom-alignment.md`, "The rig", "Measuring a response, not a look" and
//! "Fitting the translation").
//!
//! `measure --round N (--folder ROUND | --exports DIR) --output NEW [--families ID,...]` reads a
//! round folder `lr-align generate` wrote and the owner's Lightroom exports of it (the round's
//! `exports/` by default, the round the exports folder's parent when only it is named), renders the same variants headlessly through the whole-frame CPU reference renderer
//! with its study-only 16-bit sRGB terminal conversion (`Render::study_frame16`), and writes under
//! the new evidence directory:
//!
//! - `pages/<family>.md` per setting: its response curve, its monotone map as 21 knots, its
//!   residual and its contact sheet (Luxforge at the identity conversion, Luxforge through the map,
//!   Lightroom), with the kind-specific measures in tables;
//! - `svg/` and `sheets/`, the figures the pages show;
//! - `summary.md`, the settings ranked by residual with the level C candidates, the base rendering
//!   and the exports' own metadata; and `lr-align.json`, every figure.
//!
//! Every response is measured against the same editor's neutral variant of the same subject, and a
//! Lightroom response is carried onto Luxforge's neutral rendering before it is compared
//! ([`la::carried`]), so each editor's base rendering cancels to first order. The bare-target
//! exports, which carry no XMP, are Lightroom's controls: they show its base rendering of an
//! untouched JPEG and are compared with its neutral variants and the pristine targets.
//!
//! The round folder is only read: the evidence directory must not exist, its parent must, and it
//! may not lie inside the round folder. Pure measures and fits are `luxforge_reference::lr_align`;
//! this module renders, decodes, reduces and writes.
use super::photographs::{Development, Photo};
use super::report::{self, CALIBRATED, LIGHTROOM, LUXFORGE, Mark, Series, number};
use super::{FORMAT, NEUTRAL, readback, targets};
use crate::*;
use luxforge_core::{
    BASIC_EFFECT, BoxRect, CURVE_EFFECT, Component, ComponentMode, CropPayload, CropStage,
    DETAIL_EFFECT, Layer, LinearSettings, MIXER_EFFECT, Mask, ModuleRegistry, PERSPECTIVE_EFFECT,
    PRESENCE_EFFECT, RECIPE_FORMAT, Recipe, RenderContext, RenderOptions, RenderSource,
    SourceImage, VIGNETTE_EFFECT, open_source, render,
};
use luxforge_reference::lr_align::{self as la, Lab, Target};
use luxforge_reference::srgb;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{LazyLock, Mutex};

/// The median CIEDE2000 above which a fitted point is a level C candidate (A8).
const BEHAVIOUR_RESIDUAL: f64 = 2.0;
/// The share of a Lightroom range from the default at and beyond which a point counts towards the
/// A8 threshold: ±25 of a −100..100 slider, and the same share of any other range.
const THRESHOLD_SHARE: f64 = 0.125;
/// The memory one run's concurrent frames may hold, which bounds its workers.
const FRAME_BUDGET: f64 = 8e9;
/// The thumbnails' largest side.
const THUMB: u32 = 240;
/// The side of the 1:1 crops a spatial setting's sheet shows.
const CROP: u32 = 160;

pub fn run(root: &Path, mut a: Args) -> Result {
    let round: u32 = a
        .value("--round")?
        .ok_or("Required: --round")?
        .to_string_lossy()
        .parse()
        .map_err(|_| "--round takes a round number")?;
    let folder = a.value("--folder")?.map(|p| absolute(root, Path::new(&p)));
    let exports = a.value("--exports")?.map(|p| absolute(root, Path::new(&p)));
    let out = absolute(root, &a.path("--output")?);
    // Either names the other: the exports default to the round's `exports/`, the round to the
    // exports' parent.
    let (folder, exports) = match (folder, exports) {
        (Some(folder), exports) => {
            let exports = exports.unwrap_or_else(|| folder.join(super::EXPORTS));
            (folder, exports)
        }
        (None, Some(exports)) => (
            exports
                .parent()
                .ok_or("the exports folder has no parent")?
                .to_path_buf(),
            exports,
        ),
        (None, None) => return Err(super::USAGE.into()),
    };
    let only: Option<Vec<String>> = a
        .value("--families")?
        .map(|v| v.to_string_lossy().split(',').map(str::to_owned).collect());
    a.done()?;
    measure(round, &folder, &exports, &out, only.as_deref())
}

// -------------------------------------------------------------------------------------------
// Frames and their reduction
// -------------------------------------------------------------------------------------------

/// Linear light of every 16-bit sRGB code.
static LINEAR: LazyLock<Vec<f64>> = LazyLock::new(|| {
    (0..=65535u32)
        .map(|code| srgb::decode_encoded(f64::from(code) / 65535.0))
        .collect()
});

/// One frame at 16 bits per channel: Luxforge's study output or a decoded export.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u16>,
}

impl Frame {
    fn linear(&self, x: u32, y: u32) -> [f64; 3] {
        let at = ((y as usize) * self.width as usize + x as usize) * 3;
        [0, 1, 2].map(|c| LINEAR[usize::from(self.rgb[at + c])])
    }

    fn mean(&self, x0: u32, y0: u32, width: u32, height: u32) -> [f64; 3] {
        let mut sum = [0.0; 3];
        let (x1, y1) = ((x0 + width).min(self.width), (y0 + height).min(self.height));
        if x0 >= x1 || y0 >= y1 {
            return [f64::NAN; 3];
        }
        for y in y0..y1 {
            for x in x0..x1 {
                let p = self.linear(x, y);
                for c in 0..3 {
                    sum[c] += p[c];
                }
            }
        }
        let n = f64::from((x1 - x0) * (y1 - y0));
        sum.map(|v| v / n)
    }
}

/// CIELAB `L*` of a linear colour.
fn lstar(rgb: [f64; 3]) -> f64 {
    let y = la::luminance(rgb);
    let f = if y > 216.0 / 24389.0 {
        y.cbrt()
    } else {
        (24389.0 / 27.0 * y + 16.0) / 116.0
    };
    116.0 * f - 16.0
}

/// How a subject's frame is reduced to the samples a fit compares.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Sampling {
    /// The layout's patch interiors.
    Patches,
    /// Square cells of this side over the whole frame.
    Cells(u32),
    /// None: a geometric subject is compared by its dots.
    Dots,
    /// Cells laid out in normalized coordinates, this many across: a photograph, whose frames
    /// from the two editors may differ in size by their own default crops.
    Normalized(u32),
}

/// What a contact sheet shows of a frame.
#[derive(Clone, Copy, Debug)]
enum View {
    Whole,
    /// A 1:1 crop at the centre of a spatial chart's named panel.
    Panel(&'static str),
}

/// A spatial chart's measures of one frame.
#[derive(Clone, Debug)]
struct Spatial {
    /// `L*` across the hard and the soft vertical edges, each the mean of the panel's central rows.
    hard: Vec<f64>,
    soft: Vec<f64>,
    /// The zone plate's `L*` RMS per band.
    bands: Vec<f64>,
    /// The deviation of `L*` over the luminance-noise panel and of `(a*, b*)` over the colour one.
    noise: f64,
    chroma_noise: f64,
}

/// One frame reduced to what the measures read.
struct Reduced {
    width: u32,
    height: u32,
    samples: Vec<[f64; 3]>,
    patches: Vec<[f64; 3]>,
    /// A photograph's `L*` band energy at [`PHOTO_BANDS`].
    energy: Vec<f64>,
    spatial: Option<Spatial>,
    /// `L*` at a quarter of the frame's size on each axis, for positional measures.
    plane: Option<(usize, Vec<f64>)>,
    dots: Vec<[f64; 2]>,
    thumb: Option<image::RgbImage>,
}

/// One subject the round renders from: a target, with its layout, Luxforge's decode of its JPEG
/// and its pristine pixels as the generator computed them; or a RAW photograph, developed by the
/// editor service.
struct Subject {
    id: String,
    width: u32,
    height: u32,
    layout: Value,
    kind: Kind,
}

enum Kind {
    Target {
        source: SourceImage,
        pristine: Vec<u8>,
    },
    Photograph(Box<Photo>),
}

impl Subject {
    fn photo(&self) -> Option<&Photo> {
        match &self.kind {
            Kind::Photograph(photo) => Some(photo),
            Kind::Target { .. } => None,
        }
    }
}

fn rect_of(value: &Value) -> Option<(u32, u32, u32, u32)> {
    let r = value.as_array()?;
    let at = |i: usize| r.get(i)?.as_f64().map(|v| v as u32);
    Some((at(0)?, at(1)?, at(2)?, at(3)?))
}

fn panel(layout: &Value, name: &str) -> Option<(u32, u32, u32, u32)> {
    layout["panels"]
        .as_array()?
        .iter()
        .find(|p| p["name"] == name)
        .and_then(|p| rect_of(&p["rect"]))
}

fn patch_means(frame: &Frame, layout: &Value) -> Vec<[f64; 3]> {
    let size = layout["patch"].as_u64().unwrap_or(0) as u32;
    let inset = layout["inset"].as_u64().unwrap_or(0) as u32;
    layout["patches"]
        .as_array()
        .map(|patches| {
            patches
                .iter()
                .map(|p| {
                    let (x, y) = (
                        p["x"].as_u64().unwrap_or(0) as u32,
                        p["y"].as_u64().unwrap_or(0) as u32,
                    );
                    frame.mean(x + inset, y + inset, size - 2 * inset, size - 2 * inset)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn cells(frame: &Frame, side: u32) -> Vec<[f64; 3]> {
    let mut out = Vec::new();
    for row in 0..frame.height / side {
        for column in 0..frame.width / side {
            out.push(frame.mean(column * side, row * side, side, side));
        }
    }
    out
}

fn spatial(frame: &Frame, layout: &Value) -> Option<Spatial> {
    let profile = |name: &str| -> Option<Vec<f64>> {
        let (x0, y0, side, _) = panel(layout, name)?;
        let (from, to) = (y0 + side / 5, y0 + 4 * side / 5);
        Some(
            (x0..x0 + side)
                .map(|x| {
                    (from..to).map(|y| lstar(frame.linear(x, y))).sum::<f64>()
                        / f64::from(to - from)
                })
                .collect(),
        )
    };
    let plate = &layout["zone_plate"];
    let centre = plate["centre"][0].as_f64()?;
    let radius = plate["radius"].as_f64()?;
    let (x0, x1) = (
        (centre - radius).floor().max(0.0) as u32,
        ((centre + radius).ceil() as u32).min(frame.width),
    );
    let (y0, y1) = (x0, ((centre + radius).ceil() as u32).min(frame.height));
    let width = (x1 - x0) as usize;
    let mut plane = Vec::with_capacity(width * (y1 - y0) as usize);
    for y in y0..y1 {
        for x in x0..x1 {
            plane.push(lstar(frame.linear(x, y)));
        }
    }
    let local = centre - f64::from(x0);
    let bands = la::band_rms(&plane, width, (local, centre - f64::from(y0)), radius, 8);
    let inner = |name: &str| -> Option<(u32, u32, u32, u32)> {
        let (x, y, w, h) = panel(layout, name)?;
        Some((x + w / 10, y + h / 10, w * 8 / 10, h * 8 / 10))
    };
    let (nx, ny, nw, nh) = inner("luminance-noise")?;
    let mut values = Vec::new();
    for y in ny..ny + nh {
        for x in nx..nx + nw {
            values.push(lstar(frame.linear(x, y)));
        }
    }
    let noise = la::deviation(&values);
    let (cx, cy, cw, ch) = inner("colour-noise")?;
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for y in cy..cy + ch {
        for x in cx..cx + cw {
            let lab = la::lab(frame.linear(x, y));
            a.push(lab[1]);
            b.push(lab[2]);
        }
    }
    let chroma_noise = la::deviation(&a).hypot(la::deviation(&b));
    Some(Spatial {
        hard: profile("vertical-edge")?,
        soft: profile("vertical-edge-soft")?,
        bands,
        noise,
        chroma_noise,
    })
}

/// Square-ish cells over the frame in normalized coordinates, `columns` across and as many rows as
/// keep them square at the frame's aspect, each read in the frame's own pixels.
fn normalized_cells(frame: &Frame, columns: u32) -> Vec<[f64; 3]> {
    let rows = ((f64::from(columns) * f64::from(frame.height) / f64::from(frame.width)).round()
        as u32)
        .max(1);
    let mut out = Vec::with_capacity((columns * rows) as usize);
    for row in 0..rows {
        let (y0, y1) = (row * frame.height / rows, (row + 1) * frame.height / rows);
        for column in 0..columns {
            let (x0, x1) = (
                column * frame.width / columns,
                (column + 1) * frame.width / columns,
            );
            out.push(frame.mean(x0, y0, (x1 - x0).max(1), (y1 - y0).max(1)));
        }
    }
    out
}

/// The scales a photograph's band energy is read at, in quarter-size pixels.
const PHOTO_BANDS: [usize; 3] = [1, 4, 16];

fn quarter_plane(frame: &Frame) -> (usize, Vec<f64>) {
    let (w, h) = (frame.width / 4, frame.height / 4);
    let mut plane = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            plane.push(lstar(frame.mean(x * 4, y * 4, 4, 4)));
        }
    }
    (w as usize, plane)
}

fn frame_dots(frame: &Frame) -> Vec<[f64; 2]> {
    let mut plane = Vec::with_capacity((frame.width * frame.height) as usize);
    for y in 0..frame.height {
        for x in 0..frame.width {
            plane.push(lstar(frame.linear(x, y)));
        }
    }
    // Midway between the dot's and the paper's lightness.
    let threshold = (lstar([srgb::decode(30); 3]) + lstar([srgb::decode(200); 3])) / 2.0;
    la::dots(&plane, frame.width as usize, threshold, 4)
}

fn thumbnail(frame: &Frame, layout: &Value, view: View) -> image::RgbImage {
    let (x0, y0, w, h) = match view {
        View::Whole => (0, 0, frame.width, frame.height),
        View::Panel(name) => match panel(layout, name) {
            Some((x, y, pw, ph)) => {
                let side = CROP.min(pw).min(ph);
                (x + pw / 2 - side / 2, y + ph / 2 - side / 2, side, side)
            }
            None => (0, 0, frame.width, frame.height),
        },
    };
    let (w, h) = (
        w.min(frame.width - x0.min(frame.width)),
        h.min(frame.height - y0.min(frame.height)),
    );
    let factor = w.max(h).div_ceil(THUMB).max(1);
    let (tw, th) = ((w / factor).max(1), (h / factor).max(1));
    image::RgbImage::from_fn(tw, th, |x, y| {
        let mean = frame.mean(x0 + x * factor, y0 + y * factor, factor, factor);
        image::Rgb(mean.map(srgb::code))
    })
}

fn reduce(frame: &Frame, subject: &Subject, sampling: Sampling, view: Option<View>) -> Reduced {
    let is_patches = subject.layout["kind"] == "patches";
    let patches = if is_patches {
        patch_means(frame, &subject.layout)
    } else {
        Vec::new()
    };
    let samples = match sampling {
        Sampling::Patches => patches.clone(),
        Sampling::Cells(side) => cells(frame, side),
        Sampling::Dots => Vec::new(),
        Sampling::Normalized(columns) => normalized_cells(frame, columns),
    };
    let same = (frame.width, frame.height) == (subject.width, subject.height);
    let energy = if subject.photo().is_some() {
        let (width, plane) = quarter_plane(frame);
        la::band_energy(&plane, width, &PHOTO_BANDS)
    } else {
        Vec::new()
    };
    Reduced {
        width: frame.width,
        height: frame.height,
        samples,
        energy,
        patches,
        spatial: (same && subject.layout["kind"] == "spatial")
            .then(|| spatial(frame, &subject.layout))
            .flatten(),
        plane: (same && subject.id == "flat").then(|| quarter_plane(frame)),
        dots: if subject.id == "grid" {
            frame_dots(frame)
        } else {
            Vec::new()
        },
        thumb: view.map(|v| thumbnail(frame, &subject.layout, v)),
    }
}

// -------------------------------------------------------------------------------------------
// The round, its exports and Luxforge's renders
// -------------------------------------------------------------------------------------------

/// What one export's embedded XMP says Lightroom applied: its `crs:` attributes, the first of
/// each name, which is the document's own before any mask's.
type Xmp = HashMap<String, String>;

fn parse_xmp(packet: &str) -> Xmp {
    static ATTRIBUTE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(crs|xmp):(\w+)="([^"]*)""#).expect("a pattern"));
    let mut out = Xmp::new();
    for capture in ATTRIBUTE.captures_iter(packet) {
        let name = if &capture[1] == "xmp" {
            format!("xmp:{}", &capture[2])
        } else {
            capture[2].to_owned()
        };
        out.entry(name).or_insert_with(|| capture[3].to_owned());
    }
    out
}

/// The evidence of the exports themselves, gathered as they are read.
#[derive(Default)]
struct Ledger {
    /// Per export id: its `ProcessVersion` and creator.
    process: BTreeMap<String, (String, String)>,
    /// A variant setting Lightroom recorded with a value other than the one written.
    rewritten: Vec<Value>,
    /// Size mismatches and unreadable exports.
    problems: Vec<String>,
    /// The intermediate 8-bit boundaries Luxforge's renders kept.
    quantization: BTreeSet<String>,
}

struct Context<'a> {
    manifest: &'a Value,
    exports: PathBuf,
    out: PathBuf,
    registry: ModuleRegistry,
    render: RenderContext,
    subjects: BTreeMap<String, Subject>,
    variants: HashMap<String, &'a Value>,
    ledger: Mutex<Ledger>,
    /// Each fitted setting's map as `(Lightroom, Luxforge)` points with its anchor, for the
    /// families measured after it that hold that setting away from neutral.
    maps: Mutex<HashMap<String, Vec<(f64, f64)>>>,
}

fn number_of(text: &str) -> Option<f64> {
    text.trim_start_matches('+').parse().ok()
}

fn workers(pixels: u64) -> usize {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    ((FRAME_BUDGET / (pixels.max(1) as f64 * 48.0)) as usize).clamp(1, cores)
}

/// `work` over `items` on `workers` scoped threads, results in item order, or the first failure.
pub(super) fn parallel<T: Sync, R: Send>(
    items: &[T],
    workers: usize,
    work: impl Fn(&T) -> std::result::Result<R, String> + Sync,
) -> Result<Vec<R>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<Option<std::result::Result<R, String>>> =
        std::iter::repeat_with(|| None).take(items.len()).collect();
    std::thread::scope(|scope| -> Result {
        let handles: Vec<_> = (0..workers.min(items.len()).max(1))
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            return done;
                        };
                        done.push((index, work(item)));
                    }
                })
            })
            .collect();
        for handle in handles {
            for (index, result) in handle.join().map_err(|_| "a measure worker panicked")? {
                results[index] = Some(result);
            }
        }
        Ok(())
    })?;
    results
        .into_iter()
        .map(|result| Ok(result.ok_or("an item no worker took")??))
        .collect()
}

/// One Luxforge render to make: its subject, recipe, sampling and sheet view, and for a photograph
/// the development it renders over.
struct Job {
    subject: String,
    recipe: Recipe,
    sampling: Sampling,
    view: Option<View>,
    development: Development,
}

/// One export to read.
struct Read {
    subject: String,
    id: String,
    sampling: Sampling,
    view: Option<View>,
}

impl Context<'_> {
    fn subject(&self, id: &str) -> &Subject {
        &self.subjects[id]
    }

    fn renders(&self, jobs: &[Job]) -> Result<Vec<Reduced>> {
        let pixels = jobs
            .iter()
            .map(|j| {
                u64::from(self.subject(&j.subject).width)
                    * u64::from(self.subject(&j.subject).height)
            })
            .max()
            .unwrap_or(1);
        parallel(jobs, workers(pixels), |job| {
            let subject = self.subject(&job.subject);
            let study = match &subject.kind {
                Kind::Target { source, .. } => render(
                    &self.registry,
                    source,
                    &job.recipe,
                    RenderOptions::default(),
                    &self.render,
                )
                .and_then(|r| r.study_frame16()),
                Kind::Photograph(photo) => {
                    let developed = photo.develop(&job.development)?;
                    let recipe = over(&self.registry, &developed.base, &job.recipe);
                    render(
                        &self.registry,
                        RenderSource::Linear {
                            image: &developed.image,
                            settings: LinearSettings::default(),
                        },
                        &recipe,
                        RenderOptions::default(),
                        &self.render,
                    )
                    .and_then(|r| r.study_frame16())
                }
            }
            .map_err(|e| format!("{}: {e}", job.subject))?;
            if !study.narrow_boundaries.is_empty() {
                let mut ledger = self.ledger.lock().map_err(|_| "a poisoned ledger")?;
                for (segment, resample) in &study.narrow_boundaries {
                    ledger.quantization.insert(format!(
                        "segment {segment}'s output, read by {}, is quantized to 8 bits",
                        if *resample {
                            "a resample (geometry)"
                        } else {
                            "a spatial or repair stage"
                        }
                    ));
                }
            }
            let frame = Frame {
                width: study.width,
                height: study.height,
                rgb: study.rgb,
            };
            Ok(reduce(&frame, subject, job.sampling, job.view))
        })
    }

    /// Each export reduced with what its XMP records, `None` when it is missing.
    fn exports(&self, reads: &[Read]) -> Result<Vec<Option<(Reduced, Xmp)>>> {
        let pixels = reads
            .iter()
            .map(|r| {
                u64::from(self.subject(&r.subject).width)
                    * u64::from(self.subject(&r.subject).height)
            })
            .max()
            .unwrap_or(1);
        parallel(reads, workers(pixels), |read| {
            let path = self.exports.join(format!("{}.tif", read.id));
            if !path.exists() {
                return Ok(None);
            }
            let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let tiff =
                crate::tiff_reader::read(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            drop(bytes);
            let xmp = tiff.xmp.as_deref().map(parse_xmp).unwrap_or_default();
            let subject = self.subject(&read.subject);
            {
                let mut ledger = self.ledger.lock().map_err(|_| "a poisoned ledger")?;
                ledger.process.insert(
                    read.id.clone(),
                    (
                        xmp.get("ProcessVersion")
                            .cloned()
                            .unwrap_or_else(|| "none".into()),
                        xmp.get("xmp:CreatorTool")
                            .cloned()
                            .unwrap_or_else(|| "none".into()),
                    ),
                );
                if tiff.bits != 16 {
                    ledger
                        .problems
                        .push(format!("{}: {}-bit, not 16-bit", read.id, tiff.bits));
                }
                if let Some(variant) = self.variants.get(&read.id) {
                    for (name, written) in variant["settings"].as_object().into_iter().flatten() {
                        let Some(written) = written.as_str() else {
                            continue;
                        };
                        let recorded = xmp.get(name);
                        let same = recorded.is_some_and(|r| {
                            r == written
                                || matches!((number_of(r), number_of(written)), (Some(a), Some(b)) if (a - b).abs() < 1e-6)
                        });
                        if !same {
                            ledger.rewritten.push(json!({
                                "variant": read.id, "setting": name, "written": written,
                                "recorded": recorded,
                            }));
                        }
                    }
                }
            }
            let frame = Frame {
                width: tiff.width,
                height: tiff.height,
                rgb: tiff.rgb,
            };
            Ok(Some((
                reduce(&frame, subject, read.sampling, read.view),
                xmp,
            )))
        })
    }
}

/// Refuse a round this command cannot pair with its own targets.
fn check_round(manifest: &Value, round: u32, folder: &Path) -> Result {
    ensure(
        manifest["format"] == json!(FORMAT) && manifest["tool"] == "cargo xtask lr-align generate",
        "the round manifest is not lr-align generate's format; generate a new round",
    )?;
    ensure(
        manifest["round"] == json!(round),
        format!(
            "the round folder holds round {}, not {round}",
            manifest["round"]
        ),
    )?;
    for target in manifest["targets"].as_array().into_iter().flatten() {
        let id = target["id"].as_str().unwrap_or_default();
        let ours = targets::target(id).ok_or(format!(
            "the round names a target {id} this build does not draw"
        ))?;
        ensure(
            target["pixels_sha256"] == json!(format!("{:x}", Sha256::digest(&ours.rgb))),
            format!("target {id}'s pixels are not this build's; generate a new round"),
        )?;
        let file = folder.join(target["file"].as_str().unwrap_or_default());
        ensure(
            target["sha256"] == json!(format!("{:x}", Sha256::digest(fs::read(&file)?))),
            format!("{} is not the file the manifest recorded", file.display()),
        )?;
    }
    Ok(())
}

/// The round's targets, and with `catalogs` its photographs, each imported into a scratch catalog
/// of its own there from the round's copy beside its neutral variant.
fn open_subjects(
    manifest: &Value,
    folder: &Path,
    catalogs: Option<&Path>,
) -> Result<BTreeMap<String, Subject>> {
    let mut subjects = BTreeMap::new();
    for photograph in manifest["photographs"].as_array().into_iter().flatten() {
        let Some(catalogs) = catalogs else { break };
        let id = photograph["id"].as_str().unwrap_or_default().to_owned();
        let extension = photograph["extension"].as_str().unwrap_or_default();
        let copy = folder
            .join(super::LIGHTROOM)
            .join(&id)
            .join(format!("{id}__{NEUTRAL}.{extension}"));
        let photo = Photo::open(
            &copy,
            photograph["sha256"].as_str().unwrap_or_default(),
            &catalogs.join(&id),
        )?;
        println!(
            "photograph {id}: developed {} x {}, lens profile {}",
            photo.width,
            photo.height,
            photo.lens.as_deref().unwrap_or("none eligible")
        );
        subjects.insert(
            id.clone(),
            Subject {
                id,
                width: photo.width,
                height: photo.height,
                layout: Value::Null,
                kind: Kind::Photograph(Box::new(photo)),
            },
        );
    }
    for target in manifest["targets"].as_array().into_iter().flatten() {
        let id = target["id"].as_str().unwrap_or_default().to_owned();
        let source = open_source(&folder.join(target["file"].as_str().unwrap_or_default()))?;
        let ours = targets::target(&id).ok_or("an unknown target")?;
        subjects.insert(
            id.clone(),
            Subject {
                id,
                width: source.width,
                height: source.height,
                layout: target["layout"].clone(),
                kind: Kind::Target {
                    source,
                    pristine: ours.rgb,
                },
            },
        );
    }
    Ok(subjects)
}

// -------------------------------------------------------------------------------------------
// Settings and recipes
// -------------------------------------------------------------------------------------------

fn effect_of(action: &str) -> Option<&'static str> {
    Some(match action {
        "set-basic" => BASIC_EFFECT,
        "set-presence" => PRESENCE_EFFECT,
        "set-mixer" => MIXER_EFFECT,
        "set-vignette" => VIGNETTE_EFFECT,
        "set-detail" => DETAIL_EFFECT,
        "set-curve" => CURVE_EFFECT,
        "set-perspective" => PERSPECTIVE_EFFECT,
        _ => return None,
    })
}

/// A Luxforge field: its effect, payload key and value.
type Field = (&'static str, String, Value);

/// `fields` grouped into one layer per effect, plus `extra` layers, each inserted where the
/// registry places its effect, over `masks`.
fn recipe(
    registry: &ModuleRegistry,
    fields: &[Field],
    extra: Vec<Layer>,
    masks: Vec<Mask>,
) -> Recipe {
    let mut grouped: Vec<(&'static str, serde_json::Map<String, Value>)> = Vec::new();
    for (effect, key, value) in fields {
        match grouped.iter_mut().find(|(e, _)| e == effect) {
            Some((_, payload)) => {
                payload.insert(key.clone(), value.clone());
            }
            None => {
                let mut payload = serde_json::Map::new();
                payload.insert(key.clone(), value.clone());
                grouped.push((effect, payload));
            }
        }
    }
    let mut layers: Vec<Layer> = Vec::new();
    let all = grouped
        .into_iter()
        .map(|(effect, payload)| Layer::new(effect, Value::Object(payload)))
        .chain(extra);
    for layer in all {
        let at = registry.insertion_index_for(&layers, &layer.effect_id);
        layers.insert(at, layer);
    }
    Recipe {
        format: RECIPE_FORMAT,
        layers,
        masks,
        ..Recipe::default()
    }
}

/// `extra`'s layers and masks over `base`'s, each layer inserted where the registry places its
/// effect: a photograph's RAW layer (and lens layer) under a family's settings.
fn over(registry: &ModuleRegistry, base: &Recipe, extra: &Recipe) -> Recipe {
    let mut layers = base.layers.clone();
    for layer in &extra.layers {
        let at = registry.insertion_index_for(&layers, &layer.effect_id);
        layers.insert(at, layer.clone());
    }
    let mut masks = base.masks.clone();
    masks.extend(extra.masks.iter().cloned());
    Recipe {
        format: RECIPE_FORMAT,
        layers,
        masks,
        ..Recipe::default()
    }
}

fn empty() -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        ..Recipe::default()
    }
}

/// Luxforge's default, range and whether the value is an integer, per payload key.
fn luxforge_scale(effect: &str, key: &str) -> (f64, (f64, f64), bool) {
    match (effect, key) {
        (_, "exposure") => (0.0, (-5.0, 5.0), false),
        (e, "sharpening") if e == DETAIL_EFFECT => (0.0, (0.0, 150.0), false),
        (e, "radius") if e == DETAIL_EFFECT => (1.0, (0.5, 3.0), false),
        (e, "sharpen-detail") if e == DETAIL_EFFECT => (25.0, (0.0, 100.0), false),
        (e, "luminance-detail" | "colour-detail") if e == DETAIL_EFFECT => {
            (50.0, (0.0, 100.0), false)
        }
        (e, _) if e == DETAIL_EFFECT => (0.0, (0.0, 100.0), false),
        (e, "midpoint" | "feather") if e == VIGNETTE_EFFECT => (50.0, (0.0, 100.0), false),
        (e, _) if e == PERSPECTIVE_EFFECT => (0.0, (-100.0, 100.0), true),
        _ => (0.0, (-100.0, 100.0), false),
    }
}

/// Lightroom's range for a setting, across which the knots are tabulated.
fn lightroom_range(setting: &str) -> (f64, f64) {
    match setting {
        "Exposure2012" => (-5.0, 5.0),
        "Sharpness" => (0.0, 150.0),
        "SharpenRadius" => (0.5, 3.0),
        "SharpenDetail"
        | "SharpenEdgeMasking"
        | "LuminanceSmoothing"
        | "LuminanceNoiseReductionDetail"
        | "ColorNoiseReduction"
        | "ColorNoiseReductionDetail"
        | "PostCropVignetteMidpoint"
        | "PostCropVignetteFeather" => (0.0, 100.0),
        "CropAngle" => (-45.0, 45.0),
        _ => (-100.0, 100.0),
    }
}

fn grid(low: f64, high: f64, step: f64) -> Vec<f64> {
    let count = ((high - low) / step).round() as usize;
    (0..=count).map(|i| low + i as f64 * step).collect()
}

/// How a family's subject is sampled.
fn sampling_of(area: &str, subject: &str) -> Sampling {
    match subject {
        "wedge" | "patches" => Sampling::Patches,
        "surround" if !matches!(area, "presence" | "detail") => Sampling::Patches,
        "surround" => Sampling::Cells(8),
        "edges" => Sampling::Cells(4),
        "edges-24mp" => Sampling::Cells(10),
        "flat" => Sampling::Cells(10),
        _ => Sampling::Dots,
    }
}

fn view_of(family: &str, subject: &str) -> View {
    match subject {
        "edges" | "edges-24mp" => {
            if family.starts_with("colour-noise") {
                View::Panel("colour-noise")
            } else if family.starts_with("luminance-noise") {
                View::Panel("luminance-noise")
            } else {
                View::Panel("vertical-edge")
            }
        }
        _ => View::Whole,
    }
}

/// The largest centred box of the frame's own aspect inside a straighten of `angle` degrees.
fn straighten(width: u32, height: u32, angle: f64) -> CropPayload {
    let stage = CropStage {
        width,
        height,
        angle,
    };
    let (bw, bh) = stage.bounding_box();
    let centre = (bw / 2.0, bh / 2.0);
    let (w, h) = (f64::from(width), f64::from(height));
    let scale = stage.max_scale_about(centre, w / 2.0, h / 2.0).min(1.0) * 0.999;
    BoxRect::from_center(centre, w * scale, h * scale).normalized(&stage)
}

// -------------------------------------------------------------------------------------------
// Measures of one response
// -------------------------------------------------------------------------------------------

/// The measures one response (neutral → adjusted, both from one editor) gives on each subject.
#[derive(Clone, Debug, Default)]
struct Measures {
    gain: Option<f64>,
    locality: Option<f64>,
    chroma: Option<f64>,
    sectors: Vec<la::Sector>,
    transfer: Vec<[f64; 2]>,
    hard: Option<la::Edge>,
    soft: Option<la::Edge>,
    bands: Vec<la::Band>,
    noise: Option<f64>,
    chroma_noise: Option<f64>,
    profile: Vec<[f64; 2]>,
    rho50: Option<f64>,
    ramp: Option<f64>,
    corner: Option<f64>,
    horizontal50: Option<f64>,
    vertical50: Option<f64>,
    angle: Option<f64>,
    keystone: Option<(f64, f64)>,
    spacing: Option<f64>,
    size: Option<(u32, u32)>,
    /// A photograph's band energy at [`PHOTO_BANDS`] over its editor's own neutral's.
    energy: Vec<f64>,
}

/// A profile normalized by its value at its far end, and its 25, 50 and 75% crossings.
fn crossings(profile: &[[f64; 2]], reference: f64) -> (Vec<[f64; 2]>, [Option<f64>; 3]) {
    if reference.abs() < 1e-9 {
        return (Vec::new(), [None; 3]);
    }
    let normal: Vec<[f64; 2]> = profile.iter().map(|p| [p[0], p[1] / reference]).collect();
    let at = [0.25, 0.5, 0.75].map(|level| la::crossing(&normal, level));
    (normal, at)
}

/// An edge's measures within what its panel can show: a halo reaching the plateaus the edge is
/// measured against is beyond the panel (`NaN`), and the line-spread blur is read only from a
/// profile with neither overshoot nor undershoot, whose spread is a blur's.
fn bounded(mut edge: la::Edge, length: usize) -> la::Edge {
    if edge.halo >= 0.45 * length as f64 {
        edge.halo = f64::NAN;
    }
    if edge.overshoot > 0.005 || edge.undershoot > 0.005 {
        edge.spread = f64::NAN;
    }
    edge
}

fn measures(subject: &Subject, neutral: &Reduced, adjusted: &Reduced) -> Measures {
    let mut m = Measures {
        size: Some((adjusted.width, adjusted.height)),
        ..Measures::default()
    };
    if subject.photo().is_some() {
        // A photograph: its band energy at three scales and how far its tone response depends on
        // more than each cell's own lightness.
        m.energy = neutral
            .energy
            .iter()
            .zip(&adjusted.energy)
            .map(|(n, a)| a / n)
            .collect();
        let lightness =
            |samples: &[[f64; 3]]| samples.iter().map(|s| lstar(*s)).collect::<Vec<_>>();
        let locality = la::tone_locality(
            &lightness(&neutral.samples),
            &lightness(&adjusted.samples),
            1.0,
            20,
        );
        m.locality = locality.is_finite().then_some(locality);
        return m;
    }
    match subject.id.as_str() {
        "wedge" => {
            m.transfer = la::transfer(&neutral.patches, &adjusted.patches);
            m.gain = la::linear_gain(&neutral.patches, &adjusted.patches, 0.002, 0.95);
        }
        "surround" => {
            let mut groups: BTreeMap<u64, Vec<Pair>> = BTreeMap::new();
            for (i, p) in subject.layout["patches"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let (Some(n), Some(a)) = (neutral.patches.get(i), adjusted.patches.get(i)) {
                    groups
                        .entry(p["code"][0].as_u64().unwrap_or(0))
                        .or_default()
                        .push((*n, *a));
                }
            }
            m.locality = Some(la::locality(&groups.into_values().collect::<Vec<_>>()));
        }
        "patches" => {
            m.sectors = la::sectors(&neutral.patches, &adjusted.patches, 8, 0.02);
            m.chroma = la::sectors(&neutral.patches, &adjusted.patches, 1, 0.02)
                .first()
                .map(|s| s.chroma_ratio);
        }
        "edges" | "edges-24mp" => {
            if let (Some(n), Some(a)) = (&neutral.spatial, &adjusted.spatial) {
                m.hard = Some(bounded(
                    la::edge(&a.hard, a.hard.len() as f64 / 2.0),
                    a.hard.len(),
                ));
                m.soft = Some(bounded(
                    la::edge(&a.soft, a.soft.len() as f64 / 2.0),
                    a.soft.len(),
                ));
                m.bands = la::gains_of(&n.bands, &a.bands);
                m.noise = Some(a.noise / n.noise);
                m.chroma_noise = Some(a.chroma_noise / n.chroma_noise);
            }
        }
        "flat" => {
            if let (Some((w, n)), Some((_, a))) = (&neutral.plane, &adjusted.plane) {
                let delta: Vec<f64> = a.iter().zip(n).map(|(a, n)| a - n).collect();
                let h = delta.len() / w;
                let centre = (*w as f64 / 2.0, h as f64 / 2.0);
                let profile = la::radial_profile(&delta, *w, centre, centre, 0.0, 36, 1.45);
                let corner = profile.last().map(|p| p[1]).unwrap_or(0.0);
                let (normal, [r25, r50, r75]) = crossings(&profile, corner);
                m.profile = normal;
                m.corner = Some(corner);
                m.rho50 = r50;
                m.ramp = r25.zip(r75).map(|(a, b)| b - a);
                // Along the middle row and column, outward from the centre.
                let row: Vec<[f64; 2]> = (w / 2..*w)
                    .map(|x| {
                        [
                            ((x as f64 + 0.5) - centre.0) / centre.0,
                            delta[(h / 2) * w + x],
                        ]
                    })
                    .collect();
                let column: Vec<[f64; 2]> = (h / 2..h)
                    .map(|y| {
                        [
                            ((y as f64 + 0.5) - centre.1) / centre.1,
                            delta[y * w + w / 2],
                        ]
                    })
                    .collect();
                let edge = |p: &Vec<[f64; 2]>| p.last().map(|v| v[1]).unwrap_or(0.0);
                m.horizontal50 = crossings(&row, edge(&row)).1[1];
                m.vertical50 = crossings(&column, edge(&column)).1[1];
            }
        }
        "grid" => {
            m.angle = la::grid_angle(&adjusted.dots);
            m.keystone =
                la::keystone(&adjusted.dots, true).zip(la::keystone(&adjusted.dots, false));
            let spacings: Vec<f64> = adjusted
                .dots
                .iter()
                .map(|p| {
                    adjusted
                        .dots
                        .iter()
                        .filter(|q| *q != p)
                        .map(|q| (p[0] - q[0]).hypot(p[1] - q[1]))
                        .fold(f64::INFINITY, f64::min)
                })
                .collect();
            m.spacing = Some(la::median(&spacings));
        }
        _ => {}
    }
    m
}

/// One sample's neutral and adjusted linear colours.
type Pair = ([f64; 3], [f64; 3]);

fn labs(samples: &[[f64; 3]]) -> Vec<Lab> {
    samples.iter().map(|s| la::lab(*s)).collect()
}

/// The median CIEDE2000 from `neutral` of `labs` over `target`'s samples: a response's size on
/// the samples Lightroom's response moves.
fn size_on(target: &Target, labs: &[Lab], neutral: &[Lab]) -> f64 {
    let values: Vec<f64> = target
        .samples
        .iter()
        .map(|&i| luxforge_reference::preview_error::ciede2000(neutral[i], labs[i]))
        .collect();
    la::median(&values)
}

// -------------------------------------------------------------------------------------------
// Families
// -------------------------------------------------------------------------------------------

/// How a numeric family's responses are compared.
#[derive(Clone, Copy, PartialEq)]
enum Response {
    /// Colours over the subjects' samples, by CIEDE2000.
    Colour,
    /// The dot grid's rotation, in degrees.
    Rotation,
    /// The dot grid's keystone along its rows (`true`) or columns, in percent.
    Keystone(bool),
}

struct Spec<'f> {
    id: String,
    /// The round family whose exports are read, `<subject>__<family>__<label>`: the page's id
    /// unless one family is fitted as several sweeps (RAW white balance's temperature and tint).
    family: String,
    area: String,
    setting: String,
    subjects: Vec<String>,
    values: Vec<(String, f64)>,
    dense: Vec<f64>,
    lr_default: f64,
    lux_default: f64,
    anchor: Option<(f64, f64)>,
    lr_range: (f64, f64),
    lux_range: (f64, f64),
    integer: bool,
    response: Response,
    luxforge: String,
    /// How the settings the family holds away from neutral were converted.
    base: Vec<String>,
    recipe: Box<dyn Fn(f64) -> Recipe + Sync + 'f>,
    /// The development a photograph renders each value over.
    development: Box<dyn Fn(f64) -> Development + Sync + 'f>,
}

/// Columns of a photograph's normalized cells.
const PHOTO_COLUMNS: u32 = 240;

/// The responses of one editor at one value on every subject.
struct Side {
    reduced: Vec<Reduced>,
}

fn key(value: f64) -> String {
    format!("{value:.6}")
}

/// One family's result: its summary entry and its page.
struct Outcome {
    entry: Value,
    page: String,
}

fn numeric(ctx: &Context, spec: &Spec) -> Result<Outcome> {
    let first = spec.subjects[0].clone();
    let sampling = |s: &str| {
        if ctx.subject(s).photo().is_some() {
            Sampling::Normalized(PHOTO_COLUMNS)
        } else if spec.response == Response::Colour {
            sampling_of(&spec.area, s)
        } else {
            Sampling::Dots
        }
    };
    let view = view_of(&spec.id, &first);
    let identity =
        |v: f64| (v >= spec.lux_range.0 - 1e-9 && v <= spec.lux_range.1 + 1e-9).then_some(v);
    let rounded = |u: f64| if spec.integer { u.round() } else { u };
    // Luxforge: the neutral, the dense grid and the identity values, per subject.
    let mut values: Vec<f64> = spec.dense.clone();
    for (_, v) in &spec.values {
        if let Some(u) = identity(*v)
            && !values.iter().any(|d| (d - u).abs() < 1e-9)
        {
            values.push(u);
        }
    }
    let identities: BTreeSet<String> = spec
        .values
        .iter()
        .filter_map(|(_, v)| identity(*v))
        .map(key)
        .collect();
    let mut jobs = Vec::new();
    for s in &spec.subjects {
        jobs.push(Job {
            subject: s.clone(),
            recipe: empty(),
            sampling: sampling(s),
            view: None,
            development: Development::AsShot,
        });
        for u in &values {
            jobs.push(Job {
                subject: s.clone(),
                recipe: (spec.recipe)(*u),
                sampling: sampling(s),
                view: (*s == first && identities.contains(&key(*u))).then_some(view),
                development: (spec.development)(*u),
            });
        }
    }
    let mut rendered = ctx.renders(&jobs)?.into_iter();
    let mut lux_neutral = Vec::new();
    let mut lux: HashMap<String, Side> = HashMap::new();
    for _ in &spec.subjects {
        lux_neutral.push(rendered.next().ok_or("a missing render")?);
        for u in &values {
            lux.entry(key(*u))
                .or_insert_with(|| Side {
                    reduced: Vec::new(),
                })
                .reduced
                .push(rendered.next().ok_or("a missing render")?);
        }
    }
    // Lightroom: the neutral and every variant, per subject.
    let mut reads = Vec::new();
    for s in &spec.subjects {
        reads.push(Read {
            subject: s.clone(),
            id: format!("{s}__{NEUTRAL}"),
            sampling: sampling(s),
            view: None,
        });
        for (label, _) in &spec.values {
            reads.push(Read {
                subject: s.clone(),
                id: format!("{s}__{}__{label}", spec.family),
                sampling: sampling(s),
                view: (*s == first).then_some(view),
            });
        }
    }
    let mut read = ctx.exports(&reads)?.into_iter();
    let mut lr_neutral = Vec::new();
    let mut lr: Vec<Vec<Option<(Reduced, Xmp)>>> = Vec::new();
    for _ in &spec.subjects {
        lr_neutral.push(read.next().flatten().map(|(r, _)| r));
        let mut per = Vec::new();
        for _ in &spec.values {
            per.push(read.next().flatten());
        }
        lr.push(per);
    }
    let mut missing = Vec::new();
    for (si, s) in spec.subjects.iter().enumerate() {
        if lr_neutral[si].is_none() {
            missing.push(format!("{s}__{NEUTRAL}"));
        }
        for (vi, (label, _)) in spec.values.iter().enumerate() {
            if lr[si][vi].is_none() {
                missing.push(format!("{s}__{}__{label}", spec.family));
            }
        }
    }
    ensure(
        lr_neutral.iter().all(Option::is_some),
        format!("{}: a neutral export is missing ({missing:?})", spec.id),
    )?;
    let lr_neutral: Vec<Reduced> = lr_neutral.into_iter().flatten().collect();
    for (si, s) in spec.subjects.iter().enumerate() {
        let subject = ctx.subject(s);
        if subject.photo().is_some() {
            continue;
        }
        ensure(
            (lr_neutral[si].width, lr_neutral[si].height) == (subject.width, subject.height),
            format!(
                "{s}'s neutral export is not the target's size: export at full size, uncropped"
            ),
        )?;
    }
    // The values every subject has an export of.
    let present: Vec<usize> = (0..spec.values.len())
        .filter(|vi| (0..spec.subjects.len()).all(|si| lr[si][*vi].is_some()))
        .collect();
    let neutral_labs: Vec<Lab> = lux_neutral.iter().flat_map(|r| labs(&r.samples)).collect();
    let concat =
        |side: &Side| -> Vec<Lab> { side.reduced.iter().flat_map(|r| labs(&r.samples)).collect() };
    let carried = |vi: usize| -> Vec<Lab> {
        let mut out = Vec::new();
        for si in 0..spec.subjects.len() {
            let adjusted = &lr[si][vi].as_ref().expect("present").0;
            for ((n, r), a) in lux_neutral[si]
                .samples
                .iter()
                .zip(&lr_neutral[si].samples)
                .zip(&adjusted.samples)
            {
                out.push(la::lab(la::carried(*n, *r, *a)));
            }
        }
        out
    };
    let grid_measure = |reduced: &Reduced| -> f64 {
        match spec.response {
            Response::Rotation => la::grid_angle(&reduced.dots).unwrap_or(f64::NAN),
            Response::Keystone(vertical) => {
                la::keystone(&reduced.dots, vertical).unwrap_or(f64::NAN)
            }
            Response::Colour => f64::NAN,
        }
    };
    let scalar_distance = |response: Response| {
        move |target: &f64, candidate: &f64| -> f64 {
            if !target.is_finite() || !candidate.is_finite() {
                return f64::INFINITY;
            }
            match response {
                Response::Keystone(_) => (target.ln() - candidate.ln()).abs() * 100.0,
                _ => (target - candidate).abs(),
            }
        }
    };
    let targets: Vec<(f64, Target)> = present
        .iter()
        .map(|vi| (spec.values[*vi].1, Target::new(&neutral_labs, carried(*vi))))
        .collect();
    // The distance from each Lightroom response at every value of a grid refined between the
    // dense values, for `lr-align fold`'s joint fit.
    let mut distances = Value::Null;
    let fit = match spec.response {
        Response::Colour => {
            let dense_labs: Vec<Vec<Lab>> =
                spec.dense.iter().map(|u| concat(&lux[&key(*u)])).collect();
            distances = distance_table(&spec.dense, &dense_labs, &targets);
            la::fit(
                &spec.dense,
                &dense_labs,
                &targets,
                spec.anchor,
                spec.lr_range,
            )
        }
        response => {
            let responses: Vec<f64> = spec
                .dense
                .iter()
                .map(|u| grid_measure(&lux[&key(*u)].reduced[0]))
                .collect();
            let measured: Vec<(f64, f64)> = present
                .iter()
                .map(|vi| {
                    (
                        spec.values[*vi].1,
                        grid_measure(&lr[0][*vi].as_ref().expect("present").0),
                    )
                })
                .collect();
            la::fit_by(
                &spec.dense,
                &responses,
                &measured,
                scalar_distance(response),
                Some(|a: &f64, b: &f64, t: f64| a + t * (b - a)),
                spec.anchor,
                spec.lr_range,
            )
        }
    };
    if spec.response == Response::Colour && spec.base.is_empty() {
        let mut points: Vec<(f64, f64)> = fit
            .points
            .iter()
            .map(|p| (p.lightroom, p.luxforge))
            .collect();
        points.extend(spec.anchor);
        ctx.maps
            .lock()
            .map_err(|_| "a poisoned map table")?
            .entry(spec.setting.clone())
            .or_insert(points);
    }
    // Luxforge through the map, at each fitted value.
    let calibrated: Vec<f64> = fit.points.iter().map(|p| rounded(p.luxforge)).collect();
    let mut jobs = Vec::new();
    for s in &spec.subjects {
        for u in &calibrated {
            jobs.push(Job {
                subject: s.clone(),
                recipe: (spec.recipe)(*u),
                sampling: sampling(s),
                view: (*s == first).then_some(view),
                development: (spec.development)(*u),
            });
        }
    }
    let mut cal_rendered = ctx.renders(&jobs)?.into_iter();
    let mut cal: Vec<Vec<Reduced>> = Vec::new();
    for _ in &spec.subjects {
        let mut per = Vec::new();
        for _ in &calibrated {
            per.push(cal_rendered.next().ok_or("a missing render")?);
        }
        cal.push(per);
    }
    // Per point: sizes, residuals rendered and each subject's measures for the three responses.
    let far = THRESHOLD_SHARE * (spec.lr_range.1 - spec.lr_range.0);
    let sign_of = |v: f64, default: f64| if v < default { -1.0 } else { 1.0 };
    let mut rows = Vec::new();
    let mut sheet = Vec::new();
    for (pi, point) in fit.points.iter().enumerate() {
        let vi = present[pi];
        let (label, v) = &spec.values[vi];
        let lr_side: Vec<&Reduced> = (0..spec.subjects.len())
            .map(|si| &lr[si][vi].as_ref().expect("present").0)
            .collect();
        let id_side: Option<Vec<&Reduced>> =
            identity(*v).map(|u| lux[&key(u)].reduced.iter().collect());
        let cal_side: Vec<&Reduced> = (0..spec.subjects.len()).map(|si| &cal[si][pi]).collect();
        let per_subject = |side: &[&Reduced], neutral: &[Reduced]| -> Vec<Measures> {
            spec.subjects
                .iter()
                .enumerate()
                .map(|(si, s)| measures(ctx.subject(s), &neutral[si], side[si]))
                .collect()
        };
        let lr_m = per_subject(&lr_side, &lr_neutral);
        let id_m = id_side.as_ref().map(|side| per_subject(side, &lux_neutral));
        let cal_m = per_subject(&cal_side, &lux_neutral);
        let (lr_size, id_size, cal_size, rendered_residual) = if spec.response == Response::Colour {
            let target = &targets[pi].1;
            let cal_labs: Vec<Lab> = cal_side.iter().flat_map(|r| labs(&r.samples)).collect();
            let id_labs: Option<Vec<Lab>> = id_side
                .as_ref()
                .map(|side| side.iter().flat_map(|r| labs(&r.samples)).collect());
            let s = sign_of(*v, spec.lr_default);
            (
                Some(s * target.size(&neutral_labs)),
                id_labs
                    .as_ref()
                    .map(|l| s * size_on(target, l, &neutral_labs)),
                Some(s * size_on(target, &cal_labs, &neutral_labs)),
                Some(target.distance(&cal_labs)),
            )
        } else {
            let lr_g = grid_measure(lr_side[0]);
            let cal_g = grid_measure(cal_side[0]);
            let id_g = id_side.as_ref().map(|s| grid_measure(s[0]));
            let d = scalar_distance(spec.response);
            (Some(lr_g), id_g, Some(cal_g), Some(d(&lr_g, &cal_g)))
        };
        sheet.push((
            format!(
                "Lightroom {label} → Luxforge {} (calibrated {})",
                identity(*v).map_or("–".into(), |u| format!("{u}")),
                number(Some(calibrated[pi]))
            ),
            vec![
                id_side.as_ref().and_then(|s| s[0].thumb.clone()),
                cal_side[0].thumb.clone(),
                lr_side[0].thumb.clone(),
            ],
        ));
        rows.push(Row {
            label: label.clone(),
            value: *v,
            point: *point,
            calibrated: calibrated[pi],
            far: (*v - spec.lr_default).abs() >= far - 1e-9,
            lr_size,
            id_size,
            cal_size,
            rendered_residual,
            lr: lr_m,
            identity: id_m,
            cal: cal_m,
        });
    }
    // Luxforge's own response curve across its dense grid.
    let lux_curve: Vec<[f64; 2]> = spec
        .dense
        .iter()
        .map(|u| {
            let side = &lux[&key(*u)];
            let y = match spec.response {
                Response::Colour => {
                    let l = concat(side);
                    sign_of(*u, spec.lux_default)
                        * Target::new(&neutral_labs, l).size(&neutral_labs)
                }
                _ => grid_measure(&side.reduced[0]),
            };
            [*u, y]
        })
        .collect();
    let mut outcome = finish(ctx, spec, &fit, rows, lux_curve, sheet, missing)?;
    if !distances.is_null() {
        outcome.entry["distances"] = distances;
    }
    Ok(outcome)
}

/// How finely [`distance_table`] refines between dense values.
const DISTANCE_REFINE: usize = 4;

/// Each Lightroom response's distance at every value of `dense` refined [`DISTANCE_REFINE`] times
/// through interpolated responses: `{values, rows: [{lightroom, distances}]}`.
fn distance_table(dense: &[f64], labs: &[Vec<Lab>], targets: &[(f64, Target)]) -> Value {
    let mut values = Vec::new();
    let mut responses: Vec<Vec<Lab>> = Vec::new();
    for i in 0..dense.len() {
        if i + 1 == dense.len() {
            values.push(dense[i]);
            responses.push(labs[i].clone());
            break;
        }
        for step in 0..DISTANCE_REFINE {
            let t = step as f64 / DISTANCE_REFINE as f64;
            values.push(dense[i] + t * (dense[i + 1] - dense[i]));
            responses.push(la::lerp_labs(&labs[i], &labs[i + 1], t));
        }
    }
    let rows: Vec<Value> = std::thread::scope(|scope| {
        let handles: Vec<_> = targets
            .iter()
            .map(|(lightroom, target)| {
                let responses = &responses;
                scope.spawn(move || {
                    let distances: Vec<f64> =
                        responses.iter().map(|r| target.distance(r)).collect();
                    json!({"lightroom": lightroom, "distances": distances})
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or(Value::Null))
            .collect()
    });
    json!({"values": values, "rows": rows})
}

/// One fitted point with everything measured at it.
struct Row {
    label: String,
    value: f64,
    point: la::FitPoint,
    calibrated: f64,
    far: bool,
    lr_size: Option<f64>,
    id_size: Option<f64>,
    cal_size: Option<f64>,
    rendered_residual: Option<f64>,
    lr: Vec<Measures>,
    identity: Option<Vec<Measures>>,
    cal: Vec<Measures>,
}

fn residual_unit(response: Response) -> &'static str {
    match response {
        Response::Colour => "median ΔE2000",
        Response::Rotation => "degrees",
        Response::Keystone(_) => "% keystone",
    }
}

fn svg(ctx: &Context, name: &str, content: String) -> Result<String> {
    let file = format!("{name}.svg");
    fs::write(ctx.out.join("svg").join(&file), content)?;
    Ok(format!("../svg/{file}"))
}

/// The page, figures, sheet and summary entry of a numeric family.
fn finish(
    ctx: &Context,
    spec: &Spec,
    fit: &la::Fit,
    rows: Vec<Row>,
    lux_curve: Vec<[f64; 2]>,
    sheet: Vec<(String, Vec<Option<image::RgbImage>>)>,
    missing: Vec<String>,
) -> Result<Outcome> {
    let unit = residual_unit(spec.response);
    let response_label = match spec.response {
        Response::Colour => "signed median ΔE2000 from neutral",
        Response::Rotation => "grid rotation (degrees, clockwise)",
        Response::Keystone(_) => "keystone ratio",
    };
    let response = svg(
        ctx,
        &format!("{}-response", spec.id),
        report::chart(
            &format!("{}: response", spec.id),
            &format!("value ({})", spec.setting),
            response_label,
            &[
                Series::new("Luxforge", LUXFORGE, Mark::Line, lux_curve.clone()),
                Series::new(
                    "Luxforge calibrated",
                    CALIBRATED,
                    Mark::Dashed,
                    rows.iter()
                        .map(|r| [r.value, r.cal_size.unwrap_or(f64::NAN)])
                        .collect(),
                ),
                Series::new(
                    "Lightroom",
                    LIGHTROOM,
                    Mark::Points,
                    rows.iter()
                        .map(|r| [r.value, r.lr_size.unwrap_or(f64::NAN)])
                        .collect(),
                ),
            ],
        ),
    )?;
    let map = svg(
        ctx,
        &format!("{}-map", spec.id),
        report::chart(
            &format!("{}: Lightroom value → Luxforge value", spec.id),
            "Lightroom value",
            &format!("Luxforge {}", spec.luxforge),
            &[
                Series::new(
                    "identity",
                    LUXFORGE,
                    Mark::Line,
                    rows.iter()
                        .filter_map(|r| {
                            (r.value >= spec.lux_range.0 && r.value <= spec.lux_range.1)
                                .then_some([r.value, r.value])
                        })
                        .collect(),
                ),
                Series::new(
                    "fitted map (knots)",
                    CALIBRATED,
                    Mark::Dashed,
                    fit.knots
                        .iter()
                        .filter_map(|k| k.luxforge.map(|u| [k.lightroom, u]))
                        .collect(),
                ),
                Series::new(
                    "best value per sample",
                    LIGHTROOM,
                    Mark::Points,
                    rows.iter().map(|r| [r.value, r.point.luxforge]).collect(),
                ),
            ],
        ),
    )?;
    let residual = svg(
        ctx,
        &format!("{}-residual", spec.id),
        report::chart(
            &format!("{}: residual against Lightroom ({unit})", spec.id),
            "Lightroom value",
            unit,
            &[
                Series::new(
                    "Luxforge (identity)",
                    LUXFORGE,
                    Mark::Line,
                    rows.iter()
                        .map(|r| [r.value, r.point.identity.unwrap_or(f64::NAN)])
                        .collect(),
                ),
                Series::new(
                    "Luxforge calibrated",
                    CALIBRATED,
                    Mark::Dashed,
                    rows.iter().map(|r| [r.value, r.point.residual]).collect(),
                ),
            ],
        ),
    )?;
    let sheet_file = format!("{}.png", spec.id);
    report::contact_sheet(
        &ctx.out.join("sheets").join(&sheet_file),
        &["Luxforge (identity)", "Luxforge calibrated", "Lightroom"],
        &sheet,
    )?;
    // Flags.
    // A geometric response is judged at every point: its threshold distance can lie past the
    // sampled span, and its residual is in its own units.
    let colour = spec.response == Response::Colour;
    let worst = |pick: fn(&Row) -> Option<f64>| -> Option<f64> {
        rows.iter()
            .filter(|r| r.far || !colour)
            .filter_map(pick)
            .filter(|v| v.is_finite())
            .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))))
    };
    let worst_residual = worst(|r| Some(r.point.residual));
    let worst_identity = worst(|r| r.point.identity);
    let mut reasons = Vec::new();
    if spec.response == Response::Colour && worst_residual.is_some_and(|w| w > BEHAVIOUR_RESIDUAL) {
        let at: Vec<String> = rows
            .iter()
            .filter(|r| r.far && r.point.residual > BEHAVIOUR_RESIDUAL)
            .map(|r| r.label.clone())
            .collect();
        reasons.push(format!(
            "median ΔE2000 after the best value above {BEHAVIOUR_RESIDUAL} at {}",
            at.join(", ")
        ));
    }
    let subject_index = |name: &str| spec.subjects.iter().position(|s| s == name);
    // Locality: a pointwise Luxforge control against a local Lightroom one, on the surround
    // target (CIEDE2000 across surrounds) or on the photographs (the spread of ΔL* at one
    // lightness), each judged on its own subjects.
    let local_subjects: Vec<usize> = spec
        .subjects
        .iter()
        .enumerate()
        .filter(|(_, s)| *s == "surround" || ctx.subject(s).photo().is_some())
        .map(|(si, _)| si)
        .collect();
    if !local_subjects.is_empty() && spec.area == "basic" {
        let pairs: Vec<(f64, f64)> = rows
            .iter()
            .filter(|r| r.far)
            .flat_map(|r| {
                local_subjects
                    .iter()
                    .filter_map(|si| Some((r.lr[*si].locality?, r.cal[*si].locality?)))
            })
            .collect();
        let lr_local = la::median(&pairs.iter().map(|p| p.0).collect::<Vec<_>>());
        let lux_local = la::median(&pairs.iter().map(|p| p.1).collect::<Vec<_>>());
        if lr_local - lux_local > 1.0 {
            reasons.push(format!(
                "differs in kind: Lightroom's response depends on the surround (locality {lr_local:.2} ΔE2000, Luxforge calibrated {lux_local:.2})"
            ));
        }
    }
    // Halo radius on the edge chart.
    for name in ["edges", "edges-24mp"] {
        let Some(si) = subject_index(name) else {
            continue;
        };
        let ratios: Vec<f64> = rows
            .iter()
            .filter(|r| r.far)
            .filter_map(|r| {
                let (l, c) = (r.lr[si].hard?.halo, r.cal[si].hard?.halo);
                (l.is_finite() && c.is_finite() && l.max(c) >= 1.5).then(|| l / c.max(0.5))
            })
            .collect();
        let ratio = la::median(&ratios);
        if ratio.is_finite() && !(0.67..=1.5).contains(&ratio) {
            reasons.push(format!(
                "differs in kind: halo of a different radius on {name} (Lightroom's halo {ratio:.2}× Luxforge calibrated's)"
            ));
        }
    }
    // Vibrance's protection of skin hues.
    if spec.id == "vibrance"
        && let Some(si) = subject_index("patches")
        && let Some(row) = rows
            .iter()
            .filter(|r| r.value > 0.0)
            .max_by(|a, b| a.value.total_cmp(&b.value))
    {
        let index = |sectors: &[la::Sector]| -> Option<f64> {
            let skin: Vec<f64> = sectors
                .iter()
                .filter(|s| (30.0..=90.0).contains(&s.hue))
                .map(|s| s.chroma_ratio - 1.0)
                .collect();
            let other: Vec<f64> = sectors
                .iter()
                .filter(|s| !(30.0..=90.0).contains(&s.hue))
                .map(|s| s.chroma_ratio - 1.0)
                .collect();
            let (skin, other) = (la::median(&skin), la::median(&other));
            (other.abs() > 1e-6).then(|| 1.0 - skin / other)
        };
        if let (Some(l), Some(c)) = (index(&row.lr[si].sectors), index(&row.cal[si].sectors))
            && (l - c).abs() > 0.35
        {
            reasons.push(format!(
                "differs in kind: skin-hue protection at {} (Lightroom {l:.2}, Luxforge calibrated {c:.2}; 0 none, 1 full)",
                row.label
            ));
        }
    }
    // A Luxforge control that barely moves these subjects leaves the fit undetermined.
    // A Luxforge control whose values the measurement cannot tell apart at any point, every
    // point's span covering nearly its whole range, leaves the map undetermined.
    let whole =
        spec.dense.last().copied().unwrap_or(0.0) - spec.dense.first().copied().unwrap_or(0.0);
    let reach = lux_curve.iter().map(|p| p[1].abs()).fold(0.0, f64::max);
    let lr_reach = rows
        .iter()
        .filter_map(|r| r.lr_size)
        .map(f64::abs)
        .fold(0.0, f64::max);
    let undetermined = colour
        && !rows.is_empty()
        && rows
            .iter()
            .all(|r| r.point.span[1] - r.point.span[0] >= 0.9 * whole);
    // Lightroom's own response across the sweep: a difference in kind only when it varies.
    let sizes: Vec<f64> = rows
        .iter()
        .filter_map(|r| r.lr_size)
        .map(f64::abs)
        .collect();
    let lr_variation = sizes.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        - sizes.iter().copied().fold(f64::INFINITY, f64::min);
    if undetermined && lr_variation >= 1.0 {
        reasons.push(format!(
            "differs in kind: no value of Luxforge's control can be told from another against Lightroom's response, every point's span covering at least 90% of its range (Luxforge's largest response {reach:.3} ΔE2000; Lightroom's varies by {lr_variation:.2} across the sweep, up to {lr_reach:.2}), so the map is undetermined"
        ));
    }
    // A falloff of another shape on the flat field.
    if let Some(si) = subject_index("flat") {
        let pairs: Vec<(f64, f64)> = rows
            .iter()
            .filter(|r| r.far)
            .filter_map(|r| Some((r.lr[si].rho50?, r.cal[si].rho50?)))
            .collect();
        let gap = la::median(&pairs.iter().map(|(l, c)| l - c).collect::<Vec<_>>());
        if gap.abs() > 0.1 {
            reasons.push(format!(
                "differs in kind: the falloff's half point lies {gap:+.2} in ρ from Luxforge calibrated's (median over the points)"
            ));
        }
    }
    let candidate = !reasons.is_empty();
    // The importers' calibrated row (level A): a determined, increasing map of a setting the
    // importer transfers by value, fitted with nothing else held away from neutral, every knot
    // measured or interpolated; a Lightroom value at or past a shortfall is refused, never clamped.
    let transfers = matches!(
        readback::counterpart(&spec.setting),
        Some((_, _, readback::How::Transfer))
    );
    let calibration = (colour
        && !undetermined
        && fit.direction == la::Direction::Increasing
        && spec.base.is_empty()
        && transfers
        && fit.knots.iter().all(|k| k.luxforge.is_some()))
    .then(|| {
        let round4 = |v: f64| (v * 1e4).round() / 1e4;
        let (action, field, _) = readback::counterpart(&spec.setting).expect("a transfer");
        let below = rows
            .iter()
            .filter(|r| r.point.shortfall && r.value < spec.lr_default)
            .map(|r| r.value)
            .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))));
        let above = rows
            .iter()
            .filter(|r| r.point.shortfall && r.value > spec.lr_default)
            .map(|r| r.value)
            .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.min(b))));
        json!({
            "setting": spec.setting, "action": action, "field": field,
            "knots": fit.knots.iter().map(|k| [round4(k.lightroom), round4(k.luxforge.expect("every knot"))]).collect::<Vec<_>>(),
            "refused_at_or_below": below, "refused_at_or_above": above,
        })
    });
    let shortfalls: Vec<String> = rows
        .iter()
        .filter(|r| r.point.shortfall)
        .map(|r| r.label.clone())
        .collect();
    // The page.
    let mut page = format!(
        "# {}\n\nLightroom `{}` against Luxforge `{}` on {}. Round 1, CPU reference renderer (study 16-bit output) against Lightroom Classic's 16-bit TIFF exports. Responses are each editor's change from its own neutral variant; residuals are {unit}.\n\n",
        spec.id,
        spec.setting,
        spec.luxforge,
        spec.subjects.join(", ")
    );
    page += &format!(
        "- Direction: {}. Anchor: {}.\n- Held: {}.\n- Worst residual{}: calibrated {}, identity {}.\n- Range shortfall: {}.\n- Level C candidate: {}.\n",
        if undetermined {
            "undetermined".to_string()
        } else {
            format!("{:?}", fit.direction)
        },
        spec.anchor.map_or(
            "none (the family holds another setting away from neutral)".into(),
            |(a, b)| format!("Lightroom {a} = Luxforge {b}")
        ),
        if spec.base.is_empty() {
            "nothing".into()
        } else {
            spec.base.join("; ")
        },
        if colour {
            format!(
                " at |value − default| ≥ {:.3}",
                THRESHOLD_SHARE * (spec.lr_range.1 - spec.lr_range.0)
            )
        } else {
            " over every point".into()
        },
        number(worst_residual),
        number(worst_identity),
        if shortfalls.is_empty() {
            "none".into()
        } else {
            shortfalls.join(", ")
        },
        if candidate {
            reasons.join("; ")
        } else {
            "no".into()
        },
    );
    if !missing.is_empty() {
        page += &format!("- Missing exports: {}.\n", missing.join(", "));
    }
    page += &format!("\n![response]({response})\n\n![map]({map})\n\n![residual]({residual})\n\n");
    page += "## Fitted points\n\n| Lightroom | Luxforge best | span | residual | rendered residual | identity residual | shortfall | Lightroom size | Luxforge size | calibrated size |\n|---|---|---|---|---|---|---|---|---|---|\n";
    for r in &rows {
        page += &format!(
            "| {} | {} | {}..{} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.label,
            number(Some(r.point.luxforge)),
            number(Some(r.point.span[0])),
            number(Some(r.point.span[1])),
            number(Some(r.point.residual)),
            number(r.rendered_residual),
            number(r.point.identity),
            if r.point.shortfall { "yes" } else { "no" },
            number(r.lr_size),
            number(r.id_size),
            number(r.cal_size),
        );
    }
    page += &format!(
        "\n## Map: 21 knots over Lightroom {}..{}\n\n| Lightroom | Luxforge | measured |\n|---|---|---|\n",
        spec.lr_range.0, spec.lr_range.1
    );
    for k in &fit.knots {
        page += &format!(
            "| {} | {} | {} |\n",
            number(Some(k.lightroom)),
            number(k.luxforge),
            if k.measured { "yes" } else { "interpolated" }
        );
    }
    page += &measure_tables(ctx, spec, &rows)?;
    page += &format!(
        "\n## Contact sheet\n\nColumns: Luxforge at the identity conversion, Luxforge through the map, Lightroom; one row per Lightroom value, labels in `../sheets/{}.txt`.\n\n![sheet](../sheets/{sheet_file})\n",
        spec.id
    );
    let entry = json!({
        "family": spec.id, "area": spec.area, "kind": "fitted", "setting": spec.setting,
        "luxforge": spec.luxforge, "subjects": spec.subjects, "unit": unit,
        "direction": if undetermined { "undetermined".to_string() } else { format!("{:?}", fit.direction) },
        "held": spec.base,
        "calibration": calibration,
        "worst_residual": worst_residual, "worst_identity_residual": worst_identity,
        "candidate": candidate, "reasons": reasons, "shortfalls": shortfalls,
        "missing_exports": missing,
        "points": rows.iter().map(|r| json!({
            "label": r.label, "lightroom": r.value, "luxforge": r.point.luxforge,
            "rendered": r.calibrated, "residual": r.point.residual,
            "rendered_residual": r.rendered_residual, "identity_residual": r.point.identity,
            "shortfall": r.point.shortfall, "span": r.point.span,
            "lightroom_size": r.lr_size, "luxforge_size": r.id_size, "calibrated_size": r.cal_size,
        })).collect::<Vec<_>>(),
        "knots": fit.knots.iter().map(|k| json!({"lightroom": k.lightroom, "luxforge": k.luxforge, "measured": k.measured})).collect::<Vec<_>>(),
        "page": format!("pages/{}.md", spec.id),
    });
    Ok(Outcome { entry, page })
}

/// The kind-specific tables of a numeric family's page, and their charts.
fn measure_tables(ctx: &Context, spec: &Spec, rows: &[Row]) -> Result<String> {
    let mut page = String::new();
    let triple = |pick: &dyn Fn(&Measures) -> Option<f64>, r: &Row, si: usize| -> String {
        format!(
            "{} / {} / {}",
            number(pick(&r.lr[si])),
            number(r.identity.as_ref().and_then(|m| pick(&m[si]))),
            number(pick(&r.cal[si]))
        )
    };
    for (si, s) in spec.subjects.iter().enumerate() {
        match s.as_str() {
            "wedge" => {
                page += "\n## Grey wedge\n\nEach cell is Lightroom / Luxforge / Luxforge calibrated. Gain: linear luminance ratio over unclipped patches.\n\n| Lightroom | gain |\n|---|---|\n";
                for r in rows {
                    page += &format!("| {} | {} |\n", r.label, triple(&|m| m.gain, r, si));
                }
                let extremes = [
                    rows.iter().min_by(|a, b| a.value.total_cmp(&b.value)),
                    rows.iter().max_by(|a, b| a.value.total_cmp(&b.value)),
                ];
                for r in extremes.into_iter().flatten() {
                    let chart = report::chart(
                        &format!("{} {}: transfer on the grey wedge", spec.id, r.label),
                        "neutral L*",
                        "adjusted L*",
                        &[
                            Series::new(
                                "Luxforge (identity)",
                                LUXFORGE,
                                Mark::Line,
                                r.identity
                                    .as_ref()
                                    .map(|m| m[si].transfer.clone())
                                    .unwrap_or_default(),
                            ),
                            Series::new(
                                "Luxforge calibrated",
                                CALIBRATED,
                                Mark::Dashed,
                                r.cal[si].transfer.clone(),
                            ),
                            Series::new(
                                "Lightroom",
                                LIGHTROOM,
                                Mark::Points,
                                r.lr[si].transfer.iter().step_by(4).copied().collect(),
                            ),
                        ],
                    );
                    let link = svg(ctx, &format!("{}-transfer-{}", spec.id, r.label), chart)?;
                    page += &format!("\n![transfer {}]({link})\n", r.label);
                }
            }
            "surround" => {
                page += "\n## Surround (locality)\n\nMedian over the five greys of the largest CIEDE2000 between one grey's responses on the dark, mid and light surrounds: 0 for a pointwise change. Lightroom / Luxforge / Luxforge calibrated.\n\n| Lightroom | locality |\n|---|---|\n";
                for r in rows {
                    page += &format!("| {} | {} |\n", r.label, triple(&|m| m.locality, r, si));
                }
            }
            "patches" => {
                page += "\n## Patch grid\n\nOverall Oklab chroma ratio, Lightroom / Luxforge / Luxforge calibrated.\n\n| Lightroom | chroma ratio |\n|---|---|\n";
                for r in rows {
                    page += &format!("| {} | {} |\n", r.label, triple(&|m| m.chroma, r, si));
                }
                for r in [
                    rows.iter().min_by(|a, b| a.value.total_cmp(&b.value)),
                    rows.iter().max_by(|a, b| a.value.total_cmp(&b.value)),
                ]
                .into_iter()
                .flatten()
                {
                    page += &format!(
                        "\nPer Oklab hue sector at {}: chroma ratio, hue shift (°) and ΔL (Oklab), Lightroom against Luxforge calibrated.\n\n| hue | chroma LR | chroma LX cal | hue shift LR | hue shift LX cal | ΔL LR | ΔL LX cal |\n|---|---|---|---|---|---|---|\n",
                        r.label
                    );
                    for (l, c) in r.lr[si].sectors.iter().zip(&r.cal[si].sectors) {
                        page += &format!(
                            "| {} | {} | {} | {} | {} | {} | {} |\n",
                            l.hue,
                            number(Some(l.chroma_ratio)),
                            number(Some(c.chroma_ratio)),
                            number(Some(l.hue_shift)),
                            number(Some(c.hue_shift)),
                            number(Some(l.lightness_shift)),
                            number(Some(c.lightness_shift))
                        );
                    }
                }
            }
            "edges" | "edges-24mp" => {
                page += &format!(
                    "\n## Spatial chart ({s})\n\nHard vertical edge in L*: overshoot and undershoot as shares of the step, halo width (– where it reaches past the panel's plateaus) and line-spread blur σ (only where the edge has no overshoot) in pixels; zone plate gain in bands 2, 4 and 7 of 8 (0.5 is Nyquist); noise deviation relative to the editor's own neutral. Each cell is Lightroom / Luxforge / Luxforge calibrated.\n\n| Lightroom | overshoot | undershoot | halo | blur σ | band 2 | band 4 | band 7 | luminance noise | colour noise |\n|---|---|---|---|---|---|---|---|---|---|\n"
                );
                for r in rows {
                    page += &format!(
                        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                        r.label,
                        triple(&|m| m.hard.map(|e| e.overshoot), r, si),
                        triple(&|m| m.hard.map(|e| e.undershoot), r, si),
                        triple(&|m| m.hard.map(|e| e.halo), r, si),
                        triple(&|m| m.hard.map(|e| e.spread), r, si),
                        triple(&|m| m.bands.get(1).map(|b| b.gain), r, si),
                        triple(&|m| m.bands.get(3).map(|b| b.gain), r, si),
                        triple(&|m| m.bands.get(6).map(|b| b.gain), r, si),
                        triple(&|m| m.noise, r, si),
                        triple(&|m| m.chroma_noise, r, si),
                    );
                }
            }
            "flat" => {
                page += "\n## Flat field\n\nRadial profile of ΔL* from the editor's own neutral, ρ normalized to the frame's half axes (corners at 1.41): the effect at the corner, ρ at half of it, the 25–75% ramp, and the half points along the middle row and column. Lightroom / Luxforge / Luxforge calibrated.\n\n| Lightroom | corner ΔL* | ρ50 | ramp | row 50% | column 50% |\n|---|---|---|---|---|---|\n";
                for r in rows {
                    page += &format!(
                        "| {} | {} | {} | {} | {} | {} |\n",
                        r.label,
                        triple(&|m| m.corner, r, si),
                        triple(&|m| m.rho50, r, si),
                        triple(&|m| m.ramp, r, si),
                        triple(&|m| m.horizontal50, r, si),
                        triple(&|m| m.vertical50, r, si),
                    );
                }
                for r in [
                    rows.iter().min_by(|a, b| a.value.total_cmp(&b.value)),
                    rows.iter().max_by(|a, b| a.value.total_cmp(&b.value)),
                ]
                .into_iter()
                .flatten()
                {
                    let chart = report::chart(
                        &format!(
                            "{} {}: radial profile, share of the corner effect",
                            spec.id, r.label
                        ),
                        "ρ (1 at the frame's half axes)",
                        "share of corner ΔL*",
                        &[
                            Series::new(
                                "Luxforge (identity)",
                                LUXFORGE,
                                Mark::Line,
                                r.identity
                                    .as_ref()
                                    .map(|m| m[si].profile.clone())
                                    .unwrap_or_default(),
                            ),
                            Series::new(
                                "Luxforge calibrated",
                                CALIBRATED,
                                Mark::Dashed,
                                r.cal[si].profile.clone(),
                            ),
                            Series::new(
                                "Lightroom",
                                LIGHTROOM,
                                Mark::Points,
                                r.lr[si].profile.clone(),
                            ),
                        ],
                    );
                    let link = svg(ctx, &format!("{}-profile-{}", spec.id, r.label), chart)?;
                    page += &format!("\n![profile {}]({link})\n", r.label);
                }
            }
            _ if ctx.subject(s).photo().is_some() => {
                page += &format!(
                    "\n## Photograph {s}\n\nTone locality (the spread of ΔL* among cells of one lightness, 0 for a pointwise change) and band energy at three scales (quarter-size radii {PHOTO_BANDS:?}) over the editor's own neutral. Lightroom / Luxforge / Luxforge calibrated.\n\n| Lightroom | locality | fine band | mid band | coarse band |\n|---|---|---|---|---|\n"
                );
                for r in rows {
                    page += &format!(
                        "| {} | {} | {} | {} | {} |\n",
                        r.label,
                        triple(&|m| m.locality, r, si),
                        triple(&|m| m.energy.first().copied(), r, si),
                        triple(&|m| m.energy.get(1).copied(), r, si),
                        triple(&|m| m.energy.get(2).copied(), r, si),
                    );
                }
            }
            "grid" => {
                page += "\n## Dot grid\n\nRotation (degrees, clockwise), keystone along rows and columns, median dot spacing (pixels) and output size. Lightroom / Luxforge / Luxforge calibrated.\n\n| Lightroom | rotation | row keystone | column keystone | spacing | size |\n|---|---|---|---|---|---|\n";
                for r in rows {
                    let size =
                        |m: &Measures| m.size.map_or("–".into(), |(w, h)| format!("{w}×{h}"));
                    page += &format!(
                        "| {} | {} | {} | {} | {} | {} / {} / {} |\n",
                        r.label,
                        triple(&|m| m.angle, r, si),
                        triple(&|m| m.keystone.map(|k| k.0), r, si),
                        triple(&|m| m.keystone.map(|k| k.1), r, si),
                        triple(&|m| m.spacing, r, si),
                        size(&r.lr[si]),
                        r.identity.as_ref().map_or("–".into(), |m| size(&m[si])),
                        size(&r.cal[si]),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(page)
}

/// The numeric spec of a family, or why it is measured another way or not at all.
enum Method<'f> {
    Numeric(Box<Spec<'f>>),
    Curve,
    Crop,
    Mask(bool),
    /// RAW white balance on the photographs: a temperature sweep and a tint sweep.
    WhiteBalance(Vec<String>),
    /// The detected lens profile on the photographs, a conversion at the identity.
    Lens(Vec<String>),
    /// Two settings together (round 2), checked at the identity and through both settings'
    /// calibrated maps.
    Pair(Vec<String>),
    Unmeasured(String),
}

fn plan<'f>(ctx: &'f Context, family: &Value) -> Result<Method<'f>> {
    let id = family["id"].as_str().unwrap_or_default().to_owned();
    let area = family["area"].as_str().unwrap_or_default().to_owned();
    // A family on the photographs is measured on each of the round's photographs.
    let photographs: Vec<String> = ctx
        .subjects
        .values()
        .filter(|subject| subject.photo().is_some())
        .map(|subject| subject.id.clone())
        .collect();
    let subjects: Vec<String> = family["subjects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str())
        .flat_map(|s| {
            if s == "photographs" {
                photographs.clone()
            } else {
                vec![s.to_owned()]
            }
        })
        .collect();
    if subjects.is_empty() {
        return Ok(Method::Unmeasured(
            "the round holds no RAW photographs (generated without --corpus or --manifest), so this family has no variant".into(),
        ));
    }
    let on_photographs = subjects.iter().all(|s| photographs.contains(s));
    if family["luxforge"]
        .as_array()
        .is_some_and(|controls| controls.len() == 2)
        && family["labels"].as_array().is_some_and(|labels| {
            labels
                .iter()
                .all(|l| l.as_str().is_some_and(|l| l.contains('_')))
        })
    {
        return Ok(Method::Pair(subjects));
    }
    match id.as_str() {
        "white-balance" => return Ok(Method::WhiteBalance(subjects)),
        "lens-profile" => return Ok(Method::Lens(subjects)),
        "tone-curve" => return Ok(Method::Curve),
        "crop" => return Ok(Method::Crop),
        "mask-linear" => return Ok(Method::Mask(false)),
        "mask-radial" => return Ok(Method::Mask(true)),
        _ => {}
    }
    let labels: Vec<(String, f64)> = family["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l.as_str())
        .filter_map(|l| Some((l.to_owned(), number_of(l)?)))
        .collect();
    let baseline = &ctx.manifest["baseline"]["jpeg"];
    let lr_default_of = |setting: &str| {
        baseline[setting]
            .as_str()
            .and_then(number_of)
            .unwrap_or(0.0)
    };
    // A held setting goes through its own family's fitted map when one was measured before this
    // family, so the swept setting is judged apart from the held one's mismatch.
    let maps = ctx.maps.lock().map_err(|_| "a poisoned map table")?.clone();
    let mut base_notes = Vec::new();
    let base: Vec<Field> = family["base"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, text)| {
            let (action, field, _) = readback::counterpart(name)?;
            let written = number_of(text.as_str()?)?;
            let converted = maps.get(name.as_str()).and_then(|points| interpolate(points, written));
            let value = match converted {
                Some(u) => {
                    base_notes.push(format!("{name} {written} held at Luxforge {field} {u:.3}, through its own fitted map"));
                    u
                }
                None => {
                    base_notes.push(format!("{name} {written} held at Luxforge {field} {written} (identity; no fitted map)"));
                    written
                }
            };
            Some((effect_of(action)?, field.to_owned(), json!(value)))
        })
        .collect();
    let registry = &ctx.registry;
    if id == "straighten" {
        let subject = ctx.subject("grid");
        let (w, h) = (subject.width, subject.height);
        return Ok(Method::Numeric(Box::new(Spec {
            family: id.clone(),
            id,
            area,
            setting: "CropAngle".into(),
            subjects,
            values: labels,
            dense: grid(-12.0, 12.0, 0.5),
            lr_default: 0.0,
            lux_default: 0.0,
            anchor: Some((0.0, 0.0)),
            lr_range: lightroom_range("CropAngle"),
            lux_range: (-45.0, 45.0),
            integer: false,
            response: Response::Rotation,
            luxforge: "crop angle (largest centred 3:2 box)".into(),
            base: Vec::new(),
            recipe: Box::new(move |u| {
                recipe(
                    registry,
                    &[],
                    vec![Layer::crop(straighten(w, h, u))],
                    Vec::new(),
                )
            }),
            development: Box::new(|_| Development::AsShot),
        })));
    }
    let entry = family["luxforge"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["action"].as_str().and_then(effect_of).is_some())
        .ok_or(format!("{id}: no Luxforge control"))?;
    let setting = entry["lightroom"].as_str().unwrap_or_default().to_owned();
    let effect = entry["action"]
        .as_str()
        .and_then(effect_of)
        .expect("found above");
    let field = entry["field"].as_str().unwrap_or_default().to_owned();
    let (lux_default, lux_range, integer) = luxforge_scale(effect, &field);
    let cells = subjects
        .iter()
        .any(|s| matches!(sampling_of(&area, s), Sampling::Cells(_)));
    let big = subjects.iter().all(|s| s == "edges-24mp");
    // A photograph renders a whole RAW frame per value, so its grids are coarser.
    let dense = match (effect, field.as_str()) {
        (_, "exposure") if on_photographs => grid(-5.0, 5.0, 0.5),
        (e, "sharpening") if e == DETAIL_EFFECT && on_photographs => grid(0.0, 150.0, 25.0),
        _ if on_photographs && lux_range.0 >= 0.0 => grid(lux_range.0, lux_range.1, 20.0),
        _ if on_photographs => grid(lux_range.0, lux_range.1, 25.0),
        (_, "exposure") => grid(-5.0, 5.0, 0.25),
        (e, "radius") if e == DETAIL_EFFECT => grid(0.5, 3.0, 0.1),
        (e, "sharpening") if e == DETAIL_EFFECT => grid(0.0, 150.0, 10.0),
        (e, _) if e == PERSPECTIVE_EFFECT => grid(-100.0, 100.0, 10.0),
        _ if lux_range.0 >= 0.0 => grid(lux_range.0, lux_range.1, if cells { 10.0 } else { 5.0 }),
        _ => grid(
            lux_range.0,
            lux_range.1,
            if big {
                25.0
            } else if cells {
                10.0
            } else {
                5.0
            },
        ),
    };
    let response = match field.as_str() {
        "vertical" if effect == PERSPECTIVE_EFFECT => Response::Keystone(true),
        "horizontal" if effect == PERSPECTIVE_EFFECT => Response::Keystone(false),
        _ => Response::Colour,
    };
    let lr_default = lr_default_of(&setting);
    let luxforge = format!("{} {field}", entry["action"].as_str().unwrap_or_default());
    let fields_base = base.clone();
    let field_key = field.clone();
    Ok(Method::Numeric(Box::new(Spec {
        anchor: base.is_empty().then_some((lr_default, lux_default)),
        family: id.clone(),
        id,
        area,
        lr_range: lightroom_range(&setting),
        setting,
        subjects,
        values: labels,
        dense,
        lr_default,
        lux_default,
        lux_range,
        integer,
        response,
        luxforge,
        base: base_notes,
        recipe: Box::new(move |u| {
            let value = if integer {
                json!(u.round() as i64)
            } else {
                json!(u)
            };
            let mut fields = fields_base.clone();
            fields.push((effect, field_key.clone(), value));
            recipe(registry, &fields, Vec::new(), Vec::new())
        }),
        development: Box::new(|_| Development::AsShot),
    })))
}

/// A map's value at `value` by linear interpolation between its points, `None` outside them.
fn interpolate(points: &[(f64, f64)], value: f64) -> Option<f64> {
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, u)) = sorted.iter().find(|(v, _)| (v - value).abs() < 1e-9) {
        return Some(*u);
    }
    sorted.windows(2).find_map(|pair| {
        let ((v0, u0), (v1, u1)) = (pair[0], pair[1]);
        (value > v0 && value < v1).then(|| u0 + (value - v0) / (v1 - v0) * (u1 - u0))
    })
}

/// A white-balance variant's label, `<Kelvin>K<tint>`, as its Kelvin and tint.
fn white_balance_label(label: &str) -> Option<(f64, f64)> {
    let (kelvin, tint) = label.split_once('K')?;
    Some((kelvin.parse().ok()?, number_of(tint)?))
}

/// RAW white balance on the photographs, fitted as two sweeps on Luxforge's own scale: Kelvin at
/// Lightroom's tint 0, and tint at Lightroom's 5500 K, each holding the other at what the
/// importer converts Lightroom's 5500 K and tint 0 to. Every value is a development of its own.
fn white_balance(ctx: &Context, subjects: Vec<String>) -> Result<Vec<Outcome>> {
    let labels: Vec<(String, (f64, f64))> = variants_on(ctx, "white-balance", &subjects[0])
        .into_iter()
        .filter_map(|(label, _)| white_balance_label(&label).map(|pair| (label, pair)))
        .collect();
    let converted = ctx
        .variants
        .get(&format!("{}__white-balance__5500K0", subjects[0]))
        .map(|variant| &variant["luxforge"]["set-raw"]);
    let held_kelvin = converted
        .and_then(|raw| raw["temperature"].as_f64())
        .unwrap_or(5500.0);
    let held_tint = converted
        .and_then(|raw| raw["tint"].as_f64())
        .unwrap_or(0.0);
    let mut temperatures: Vec<(String, f64)> = labels
        .iter()
        .filter(|(_, (_, tint))| *tint == 0.0)
        .map(|(label, (kelvin, _))| (label.clone(), *kelvin))
        .collect();
    temperatures.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut tints: Vec<(String, f64)> = labels
        .iter()
        .filter(|(_, (kelvin, _))| *kelvin == 5500.0)
        .map(|(label, (_, tint))| (label.clone(), *tint))
        .collect();
    tints.sort_by(|a, b| a.1.total_cmp(&b.1));
    let sweep = |id: &str, setting: &str, values: Vec<(String, f64)>| Spec {
        id: id.to_owned(),
        family: "white-balance".into(),
        area: "white-balance".into(),
        setting: setting.to_owned(),
        subjects: subjects.clone(),
        values,
        dense: if setting == "Temperature" {
            grid(2000.0, 12000.0, 500.0)
        } else {
            grid(-100.0, 100.0, 20.0)
        },
        lr_default: if setting == "Temperature" {
            5500.0
        } else {
            0.0
        },
        lux_default: if setting == "Temperature" {
            held_kelvin
        } else {
            held_tint
        },
        anchor: None,
        lr_range: if setting == "Temperature" {
            (2000.0, 12000.0)
        } else {
            (-150.0, 150.0)
        },
        lux_range: if setting == "Temperature" {
            (2000.0, 12000.0)
        } else {
            (-100.0, 100.0)
        },
        integer: false,
        response: Response::Colour,
        luxforge: if setting == "Temperature" {
            format!("set-raw temperature, tint held at {held_tint:.3}")
        } else {
            format!("set-raw tint, temperature held at {held_kelvin:.1} K")
        },
        base: Vec::new(),
        recipe: Box::new(|_| empty()),
        development: if setting == "Temperature" {
            Box::new(move |u| Development::WhiteBalance {
                temperature: u,
                tint: held_tint,
            })
        } else {
            Box::new(move |u| Development::WhiteBalance {
                temperature: held_kelvin,
                tint: u,
            })
        },
    };
    Ok(vec![
        numeric(
            ctx,
            &sweep("white-balance-temperature", "Temperature", temperatures),
        )?,
        numeric(ctx, &sweep("white-balance-tint", "Tint", tints))?,
    ])
}

/// The lens profile on the photographs: Luxforge's detected profile against Lightroom's profile
/// distortion, a conversion at the identity judged by its residual on each photograph.
fn lens(ctx: &Context, subjects: Vec<String>) -> Result<Outcome> {
    let sampling = Sampling::Normalized(PHOTO_COLUMNS);
    let mut page = String::from(
        "# lens-profile\n\nLightroom's lens profile, distortion only, against Luxforge's detected profile for the same photograph (distortion only), each against its own neutral; no value to fit, so a conversion judged by its residual. CPU reference renderer (study 16-bit output).\n\n| photograph | Luxforge profile | residual (median ΔE2000) |\n|---|---|---|\n",
    );
    let mut points = Vec::new();
    let mut sheet = Vec::new();
    let mut worst: f64 = 0.0;
    for subject in &subjects {
        let photo = ctx.subject(subject).photo().ok_or("a photograph")?;
        let Some(key) = &photo.lens else {
            page += &format!("| {subject} | none eligible | – |\n");
            points.push(json!({"label": subject, "profile": null}));
            continue;
        };
        let job = |development: Development, view: Option<View>| Job {
            subject: subject.clone(),
            recipe: empty(),
            sampling,
            view,
            development,
        };
        let lux = ctx.renders(&[
            job(Development::AsShot, None),
            job(Development::Lens, Some(View::Whole)),
        ])?;
        let read = |id: String, view: Option<View>| Read {
            subject: subject.clone(),
            id,
            sampling,
            view,
        };
        let lr = ctx.exports(&[
            read(format!("{subject}__{NEUTRAL}"), None),
            read(
                format!("{subject}__lens-profile__distortion"),
                Some(View::Whole),
            ),
        ])?;
        let (Some((rn, _)), Some((rv, _))) = (&lr[0], &lr[1]) else {
            page += &format!("| {subject} | {key} | export missing |\n");
            continue;
        };
        let neutral_labs = labs(&lux[0].samples);
        let reached: Vec<Lab> = lux[0]
            .samples
            .iter()
            .zip(&rn.samples)
            .zip(&rv.samples)
            .map(|((n, r), a)| la::lab(la::carried(*n, *r, *a)))
            .collect();
        let residual = Target::new(&neutral_labs, reached).distance(&labs(&lux[1].samples));
        worst = worst.max(residual);
        page += &format!("| {subject} | {key} | {} |\n", number(Some(residual)));
        points.push(json!({"label": subject, "profile": key, "residual": residual}));
        sheet.push((
            subject.clone(),
            vec![lux[1].thumb.clone(), rv.thumb.clone()],
        ));
    }
    report::contact_sheet(
        &ctx.out.join("sheets/lens-profile.png"),
        &["Luxforge", "Lightroom"],
        &sheet,
    )?;
    page += "\n![sheet](../sheets/lens-profile.png)\n";
    let candidate = worst > BEHAVIOUR_RESIDUAL;
    let reasons: Vec<String> = if candidate {
        vec![format!("median ΔE2000 at the identity up to {worst:.2}")]
    } else {
        Vec::new()
    };
    Ok(Outcome {
        entry: json!({"family": "lens-profile", "area": "geometry", "kind": "conversion", "unit": "median ΔE2000", "worst_residual": worst, "candidate": candidate, "reasons": reasons, "points": points, "page": "pages/lens-profile.md"}),
        page,
    })
}

/// One setting of a pair at Lightroom's `value` through its calibrated map: the converted value,
/// or at a range shortfall the end of Luxforge's range it falls short at, flagged; the value
/// itself for a setting with no map.
fn calibrated_or_end(setting: &str, value: f64, range: (f64, f64)) -> (f64, bool) {
    match luxforge_core::lightroom_calibration(setting, value) {
        Some(Ok(converted)) => (converted, false),
        Some(Err(_)) => (if value < 0.0 { range.0 } else { range.1 }, true),
        None => (value, false),
    }
}

/// Two settings together (round 2): each pair rendered at the identity and through both settings'
/// calibrated maps, judged against Lightroom's pair by the same carried CIEDE2000 as a single
/// setting, so a pair whose maps no longer hold together shows a residual its settings alone do
/// not (`docs/design/lightroom-alignment.md`, "Fitting the translation").
fn pair(ctx: &Context, family: &Value, subjects: Vec<String>) -> Result<Outcome> {
    let id = family["id"].as_str().unwrap_or_default().to_owned();
    let area = family["area"].as_str().unwrap_or_default().to_owned();
    let controls: Vec<(String, &'static str, String)> = family["luxforge"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some((
                entry["lightroom"].as_str()?.to_owned(),
                effect_of(entry["action"].as_str()?)?,
                entry["field"].as_str()?.to_owned(),
            ))
        })
        .collect();
    ensure(
        controls.len() == 2,
        format!("{id}: a pair names two Luxforge controls"),
    )?;
    let labels: Vec<(String, [f64; 2])> = family["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|label| {
            let label = label.as_str()?;
            let (a, b) = label.split_once('_')?;
            Some((label.to_owned(), [number_of(a)?, number_of(b)?]))
        })
        .collect();
    let sampling = |s: &str| {
        if ctx.subject(s).photo().is_some() {
            Sampling::Normalized(PHOTO_COLUMNS)
        } else {
            sampling_of(&area, s)
        }
    };
    let first = subjects[0].clone();
    let view = view_of(&id, &first);
    let fields = |values: [f64; 2]| -> Vec<Field> {
        controls
            .iter()
            .zip(values)
            .map(|((_, effect, field), value)| (*effect, field.clone(), json!(value)))
            .collect()
    };
    // Per label: the values each setting renders at through its map, and which fell short.
    let calibrated: Vec<([f64; 2], [bool; 2])> = labels
        .iter()
        .map(|(_, values)| {
            let converted = [0, 1].map(|i| {
                let (_, effect, field) = &controls[i];
                let (_, range, _) = luxforge_scale(effect, field);
                calibrated_or_end(&controls[i].0, values[i], range)
            });
            (
                [converted[0].0, converted[1].0],
                [converted[0].1, converted[1].1],
            )
        })
        .collect();
    let mut jobs = Vec::new();
    let mut reads = Vec::new();
    for s in &subjects {
        let job = |recipe: Recipe, view: Option<View>| Job {
            subject: s.clone(),
            recipe,
            sampling: sampling(s),
            view,
            development: Development::AsShot,
        };
        jobs.push(job(empty(), None));
        reads.push(Read {
            subject: s.clone(),
            id: format!("{s}__{NEUTRAL}"),
            sampling: sampling(s),
            view: None,
        });
        for ((label, values), (converted, _)) in labels.iter().zip(&calibrated) {
            let shown = (*s == first).then_some(view);
            jobs.push(job(
                recipe(&ctx.registry, &fields(*values), Vec::new(), Vec::new()),
                shown,
            ));
            jobs.push(job(
                recipe(&ctx.registry, &fields(*converted), Vec::new(), Vec::new()),
                shown,
            ));
            reads.push(Read {
                subject: s.clone(),
                id: format!("{s}__{id}__{label}"),
                sampling: sampling(s),
                view: shown,
            });
        }
    }
    let lux = ctx.renders(&jobs)?;
    let lr = ctx.exports(&reads)?;
    let per_lux = 1 + 2 * labels.len();
    let per_lr = 1 + labels.len();
    let mut page = format!(
        "# {id}\n\nLightroom `{}` with `{}` on {}: each pair against Luxforge at the identity and through both settings' calibrated maps (round 1's, as preset import converts them; a value past a map's range shortfall renders at the end of Luxforge's range, marked). Residuals are median CIEDE2000 of the carried responses over the samples Lightroom's pair moves. CPU reference renderer (study 16-bit output).\n\n| pair | Luxforge values through the maps | identity residual | calibrated residual |\n|---|---|---|---|\n",
        controls[0].0,
        controls[1].0,
        subjects.join(", ")
    );
    let mut points = Vec::new();
    let mut sheet = Vec::new();
    let mut worst: f64 = 0.0;
    let mut missing = Vec::new();
    for (li, ((label, values), (converted, short))) in labels.iter().zip(&calibrated).enumerate() {
        let mut neutral = Vec::new();
        let mut reached = Vec::new();
        let mut identity = Vec::new();
        let mut through = Vec::new();
        let mut complete = true;
        for (si, s) in subjects.iter().enumerate() {
            let (Some((rn, _)), Some((rv, _))) = (&lr[si * per_lr], &lr[si * per_lr + 1 + li])
            else {
                complete = false;
                missing.push(format!("{s}__{id}__{label}"));
                continue;
            };
            let ln = &lux[si * per_lux];
            neutral.extend(labs(&ln.samples));
            for ((n, r), a) in ln.samples.iter().zip(&rn.samples).zip(&rv.samples) {
                reached.push(la::lab(la::carried(*n, *r, *a)));
            }
            identity.extend(labs(&lux[si * per_lux + 1 + 2 * li].samples));
            through.extend(labs(&lux[si * per_lux + 2 + 2 * li].samples));
            if si == 0 {
                sheet.push((
                    label.clone(),
                    vec![
                        lux[si * per_lux + 1 + 2 * li].thumb.clone(),
                        lux[si * per_lux + 2 + 2 * li].thumb.clone(),
                        rv.thumb.clone(),
                    ],
                ));
            }
        }
        if !complete {
            continue;
        }
        let target = Target::new(&neutral, reached);
        let (at_identity, at_maps) = (target.distance(&identity), target.distance(&through));
        let far = values.iter().zip(&controls).any(|(v, (setting, _, _))| {
            let range = lightroom_range(setting);
            v.abs() >= THRESHOLD_SHARE * (range.1 - range.0)
        });
        if far {
            worst = worst.max(at_maps);
        }
        let marks = |i: usize| if short[i] { " (short)" } else { "" };
        page += &format!(
            "| {label} | {}{}, {}{} | {} | {} |\n",
            number(Some(converted[0])),
            marks(0),
            number(Some(converted[1])),
            marks(1),
            number(Some(at_identity)),
            number(Some(at_maps)),
        );
        points.push(json!({
            "label": label, "lightroom": values, "luxforge": converted, "shortfall": short,
            "identity_residual": at_identity, "residual": at_maps,
        }));
    }
    report::contact_sheet(
        &ctx.out.join("sheets").join(format!("{id}.png")),
        &["Luxforge (identity)", "Luxforge calibrated", "Lightroom"],
        &sheet,
    )?;
    page += &format!(
        "\n## Contact sheet\n\nColumns: Luxforge at the identity, Luxforge through both maps, Lightroom; labels in `../sheets/{id}.txt`.\n\n![sheet](../sheets/{id}.png)\n"
    );
    let candidate = worst > BEHAVIOUR_RESIDUAL;
    let reasons: Vec<String> = if candidate {
        vec![format!(
            "median ΔE2000 through both maps up to {worst:.2} at values a threshold distance or more from the defaults"
        )]
    } else {
        Vec::new()
    };
    Ok(Outcome {
        entry: json!({"family": id, "area": area, "kind": "pair", "unit": "median ΔE2000", "worst_residual": worst, "candidate": candidate, "reasons": reasons, "points": points, "missing_exports": missing, "page": format!("pages/{id}.md")}),
        page,
    })
}

/// A labelled family's variants as (label, variant entry) on `subject`.
fn variants_on<'v>(ctx: &'v Context, family: &str, subject: &str) -> Vec<(String, &'v Value)> {
    let mut out: Vec<(String, &Value)> = ctx.manifest["variants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["family"] == family && v["subject"] == subject)
        .map(|v| (v["label"].as_str().unwrap_or_default().to_owned(), v))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The Tone curve: each curve rendered through Luxforge's luminance curve at the same points.
fn curve(ctx: &Context, family: &Value) -> Result<Outcome> {
    let subjects = ["wedge", "patches"];
    let mut page = String::from(
        "# tone-curve\n\nLightroom's point curve (`ToneCurvePV2012`, applied per channel) against Luxforge's luminance curve at the same points, on the grey wedge and the patch grid. No value to fit: each curve is a conversion at the identity, judged by its residual. Round 1, CPU reference renderer (study 16-bit output) against Lightroom Classic's 16-bit TIFF exports.\n\n| curve | residual (median ΔE2000) | chroma ratio LR | chroma ratio LX | wedge max |ΔL*| LR | wedge max |ΔL*| LX |\n|---|---|---|---|---|---|\n",
    );
    let mut points = Vec::new();
    let mut sheet = Vec::new();
    let mut worst: f64 = 0.0;
    let labels: Vec<(String, &Value)> =
        variants_on(ctx, family["id"].as_str().unwrap_or_default(), "wedge");
    let mut jobs = Vec::new();
    let mut reads = Vec::new();
    for s in subjects {
        jobs.push(Job {
            subject: s.into(),
            recipe: empty(),
            sampling: Sampling::Patches,
            view: None,
            development: Development::AsShot,
        });
        reads.push(Read {
            subject: s.into(),
            id: format!("{s}__{NEUTRAL}"),
            sampling: Sampling::Patches,
            view: None,
        });
        for (label, variant) in &labels {
            let points = &variant["luxforge"]["set-curve"]["luminance"];
            jobs.push(Job {
                subject: s.into(),
                recipe: recipe(
                    &ctx.registry,
                    &[(CURVE_EFFECT, "luminance".into(), points.clone())],
                    Vec::new(),
                    Vec::new(),
                ),
                sampling: Sampling::Patches,
                view: (s == "patches").then_some(View::Whole),
                development: Development::AsShot,
            });
            reads.push(Read {
                subject: s.into(),
                id: format!("{s}__tone-curve__{label}"),
                sampling: Sampling::Patches,
                view: (s == "patches").then_some(View::Whole),
            });
        }
    }
    let lux = ctx.renders(&jobs)?;
    let lr = ctx.exports(&reads)?;
    let per = labels.len() + 1;
    let mut missing = Vec::new();
    for (li, (label, _)) in labels.iter().enumerate() {
        let mut neutral_labs = Vec::new();
        let mut reached = Vec::new();
        let mut lux_labs = Vec::new();
        let mut row_measures = Vec::new();
        let mut complete = true;
        for (si, s) in subjects.iter().enumerate() {
            let (ln, lv) = (&lux[si * per], &lux[si * per + 1 + li]);
            let (Some((rn, _)), Some((rv, _))) = (&lr[si * per], &lr[si * per + 1 + li]) else {
                complete = false;
                missing.push(format!("{s}__tone-curve__{label}"));
                continue;
            };
            neutral_labs.extend(labs(&ln.samples));
            lux_labs.extend(labs(&lv.samples));
            for ((n, r), a) in ln.samples.iter().zip(&rn.samples).zip(&rv.samples) {
                reached.push(la::lab(la::carried(*n, *r, *a)));
            }
            row_measures.push((
                measures(ctx.subject(s), rn, rv),
                measures(ctx.subject(s), ln, lv),
            ));
            if *s == "patches" {
                sheet.push((label.clone(), vec![lv.thumb.clone(), rv.thumb.clone()]));
            }
        }
        if !complete {
            continue;
        }
        let target = Target::new(&neutral_labs, reached);
        let residual = target.distance(&lux_labs);
        worst = worst.max(residual);
        let max_dl = |t: &[[f64; 2]]| t.iter().map(|p| (p[1] - p[0]).abs()).fold(0.0, f64::max);
        let (wedge_lr, wedge_lx) = (&row_measures[0].0, &row_measures[0].1);
        let (patch_lr, patch_lx) = (&row_measures[1].0, &row_measures[1].1);
        page += &format!(
            "| {label} | {} | {} | {} | {} | {} |\n",
            number(Some(residual)),
            number(patch_lr.chroma),
            number(patch_lx.chroma),
            number(Some(max_dl(&wedge_lr.transfer))),
            number(Some(max_dl(&wedge_lx.transfer)))
        );
        let chart = report::chart(
            &format!("tone-curve {label}: transfer on the grey wedge"),
            "neutral L*",
            "adjusted L*",
            &[
                Series::new("Luxforge", LUXFORGE, Mark::Line, wedge_lx.transfer.clone()),
                Series::new(
                    "Lightroom",
                    LIGHTROOM,
                    Mark::Points,
                    wedge_lr.transfer.iter().step_by(4).copied().collect(),
                ),
            ],
        );
        let link = svg(ctx, &format!("tone-curve-transfer-{label}"), chart)?;
        page += &format!("\n![{label}]({link})\n\n");
        points.push(json!({"label": label, "residual": residual, "chroma_lightroom": patch_lr.chroma, "chroma_luxforge": patch_lx.chroma}));
    }
    report::contact_sheet(
        &ctx.out.join("sheets/tone-curve.png"),
        &["Luxforge", "Lightroom"],
        &sheet,
    )?;
    page += "\n## Contact sheet\n\nColumns: Luxforge, an empty column (a curve has no fitted map), Lightroom; the patch grid; labels in `../sheets/tone-curve.txt`.\n\n![sheet](../sheets/tone-curve.png)\n";
    let candidate = worst > BEHAVIOUR_RESIDUAL;
    let reasons: Vec<String> = if candidate {
        vec![format!(
            "median ΔE2000 at the identity conversion up to {worst:.2}: Lightroom's curve acts per channel, Luxforge's on luminance"
        )]
    } else {
        Vec::new()
    };
    Ok(Outcome {
        entry: json!({"family": "tone-curve", "area": "tone-curve", "kind": "conversion", "unit": "median ΔE2000", "worst_residual": worst, "candidate": candidate, "reasons": reasons, "points": points, "missing_exports": missing, "page": "pages/tone-curve.md"}),
        page,
    })
}

/// The recorded crop box of an export, falling back to the written one.
fn crop_box(xmp: &Xmp, settings: &Value) -> [f64; 4] {
    ["CropLeft", "CropTop", "CropRight", "CropBottom"].map(|name| {
        xmp.get(name)
            .and_then(|v| number_of(v))
            .or_else(|| settings[name].as_str().and_then(number_of))
            .unwrap_or(0.0)
    })
}

/// Plain crops: Luxforge's crop at the box Lightroom recorded, compared by output size and dots.
fn crop(ctx: &Context) -> Result<Outcome> {
    let variants = variants_on(ctx, "crop", "grid");
    let reads: Vec<Read> = variants
        .iter()
        .map(|(label, _)| Read {
            subject: "grid".into(),
            id: format!("grid__crop__{label}"),
            sampling: Sampling::Dots,
            view: Some(View::Whole),
        })
        .collect();
    let lr = ctx.exports(&reads)?;
    let mut jobs = Vec::new();
    for ((_, variant), export) in variants.iter().zip(&lr) {
        let [left, top, right, bottom] = crop_box(
            export.as_ref().map(|e| &e.1).unwrap_or(&Xmp::new()),
            &variant["settings"],
        );
        let payload = CropPayload {
            angle: 0.0,
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        };
        jobs.push(Job {
            subject: "grid".into(),
            recipe: recipe(&ctx.registry, &[], vec![Layer::crop(payload)], Vec::new()),
            sampling: Sampling::Dots,
            view: Some(View::Whole),
            development: Development::AsShot,
        });
    }
    let lux = ctx.renders(&jobs)?;
    let mut page = String::from(
        "# crop\n\nPlain crops on the dot grid: Luxforge's crop at the box Lightroom recorded in each export, against Lightroom's export, by output size and the median distance between matched dot centres. Expected to agree within a pixel: a test of the conversion, not a calibration.\n\n| crop | Lightroom size | Luxforge size | median dot error (px) | unmatched dots | within a pixel |\n|---|---|---|---|---|---|\n",
    );
    let mut points = Vec::new();
    let mut sheet = Vec::new();
    let mut worst: f64 = 0.0;
    for (((label, _), export), lx) in variants.iter().zip(&lr).zip(&lux) {
        let Some((lr, _)) = export else { continue };
        let (error, unmatched) = la::match_points(&lr.dots, &lx.dots, 10.0);
        let within = error <= 1.0
            && (lr.width, lr.height).0.abs_diff(lx.width) <= 1
            && lr.height.abs_diff(lx.height) <= 1;
        worst = worst.max(error);
        page += &format!(
            "| {label} | {}×{} | {}×{} | {} | {unmatched} | {} |\n",
            lr.width,
            lr.height,
            lx.width,
            lx.height,
            number(Some(error)),
            if within { "yes" } else { "no" }
        );
        points.push(json!({"label": label, "lightroom_size": [lr.width, lr.height], "luxforge_size": [lx.width, lx.height], "dot_error": error, "unmatched": unmatched, "within_a_pixel": within}));
        sheet.push((label.clone(), vec![lx.thumb.clone(), lr.thumb.clone()]));
    }
    report::contact_sheet(
        &ctx.out.join("sheets/crop.png"),
        &["Luxforge", "Lightroom"],
        &sheet,
    )?;
    page += "\n![sheet](../sheets/crop.png)\n";
    Ok(Outcome {
        entry: json!({"family": "crop", "area": "geometry", "kind": "conversion", "unit": "pixels (dot error)", "worst_residual": worst, "candidate": false, "reasons": [], "points": points, "page": "pages/crop.md"}),
        page,
    })
}

/// What a mask variant's own response looks like on the flat field.
/// Linear luminance of a CIELAB `L*`.
fn luminance_of(lstar: f64) -> f64 {
    let f = (lstar + 16.0) / 116.0;
    if f > 6.0 / 29.0 {
        f * f * f
    } else {
        (f - 16.0 / 116.0) * 3.0 * (6.0f64 / 29.0).powi(2)
    }
}

#[derive(Clone, Debug, Default)]
struct MaskMeasures {
    /// The exposure, in stops, the effect applies where it is full: the log2 ratio of the
    /// adjusted to the neutral linear luminance over the core of the mask.
    ev: Option<f64>,
    /// The side the effect falls on: "inside"/"outside" the ellipse, or the "zero"/"full" end.
    side: String,
    effect: f64,
    half: Option<f64>,
    ramp: Option<f64>,
    orientation: Option<f64>,
    profile: Vec<[f64; 2]>,
}

fn mask_measures(
    neutral: &Reduced,
    adjusted: &Reduced,
    geometry: &Value,
    radial: bool,
) -> MaskMeasures {
    let (Some((w, n)), Some((_, a))) = (&neutral.plane, &adjusted.plane) else {
        return MaskMeasures::default();
    };
    let delta: Vec<f64> = a.iter().zip(n).map(|(a, n)| a - n).collect();
    let h = delta.len() / w;
    let (fw, fh) = (*w as f64, h as f64);
    let g = |k: &str| geometry[k].as_f64().unwrap_or(0.0);
    let mean_where = |keep: &dyn Fn(f64) -> bool, profile: &[[f64; 2]]| -> f64 {
        let v: Vec<f64> = profile
            .iter()
            .filter(|p| keep(p[0]))
            .map(|p| p[1])
            .collect();
        v.iter().sum::<f64>() / v.len().max(1) as f64
    };
    let peak = delta.iter().map(|v| v.abs()).fold(0.0, f64::max);
    // The stops applied over the pixels the effect reaches fully (within 5% of its peak).
    let (mut ys, mut yn) = (0.0, 0.0);
    for ((d, a), n) in delta.iter().zip(a).zip(n) {
        if peak > 0.0 && d.abs() >= 0.95 * peak {
            ys += luminance_of(*a);
            yn += luminance_of(*n);
        }
    }
    let ev = (yn > 0.0 && ys > 0.0).then(|| (ys / yn).log2());
    let strong: Vec<f64> = delta
        .iter()
        .map(|v| if v.abs() >= 0.5 * peak { *v } else { 0.0 })
        .collect();
    if radial {
        let centre = (
            (g("left") + g("right")) / 2.0 * fw,
            (g("top") + g("bottom")) / 2.0 * fh,
        );
        let axes = (
            (g("right") - g("left")) / 2.0 * fw,
            (g("bottom") - g("top")) / 2.0 * fh,
        );
        let profile = la::radial_profile(&delta, *w, centre, axes, g("angle"), 40, 2.0);
        let inside = mean_where(&|r| r < 0.4, &profile);
        let outside = mean_where(&|r| r > 1.6, &profile);
        let effect = if inside.abs() >= outside.abs() {
            inside
        } else {
            outside
        };
        let (normal, [r25, r50, r75]) = crossings(&profile, effect);
        MaskMeasures {
            ev,
            side: if inside.abs() >= outside.abs() {
                "inside".into()
            } else {
                "outside".into()
            },
            effect,
            half: r50,
            ramp: r25.zip(r75).map(|(a, b)| (b - a).abs()),
            orientation: la::orientation(&strong, *w),
            profile: normal,
        }
    } else {
        let from = (g("zero_x") * fw, g("zero_y") * fh);
        let to = (g("full_x") * fw, g("full_y") * fh);
        let profile = la::axis_profile(&delta, *w, from, to, 60, (-1.0, 2.0));
        let zero = mean_where(&|t| t < -0.5, &profile);
        let full = mean_where(&|t| t > 1.5, &profile);
        let effect = if full.abs() >= zero.abs() { full } else { zero };
        let (normal, [t25, t50, t75]) = crossings(&profile, effect);
        MaskMeasures {
            ev,
            side: if full.abs() >= zero.abs() {
                "full".into()
            } else {
                "zero".into()
            },
            effect,
            half: t50,
            ramp: t25.zip(t75).map(|(a, b)| (b - a).abs()),
            orientation: None,
            profile: normal,
        }
    }
}

/// Linear and radial gradients with local Exposure −1: the mask direction each export shows, and
/// Luxforge's mask of the same geometry against it.
fn mask(ctx: &Context, family: &str, radial: bool) -> Result<Outcome> {
    let variants = variants_on(ctx, family, "flat");
    let subject = ctx.subject("flat");
    let (fw, fh) = (f64::from(subject.width), f64::from(subject.height));
    let settings: Vec<Value> = variants
        .iter()
        .map(|(_, variant)| {
            variant["settings"]
                .as_object()
                .and_then(|s| s.values().find(|v| v.is_object()))
                .cloned()
                .unwrap_or(Value::Null)
        })
        .collect();
    // Luxforge's mask of the written geometry, BASIC exposure `ev` through it.
    let masked = |setting: &Value, ev: f64| -> Recipe {
        let mut m = Mask::new("Mask 1");
        let kind = if radial { "radial" } else { "linear" };
        let name = m.next_component_name(kind);
        let payload = if radial {
            let g = |k: &str| setting[k].as_f64().unwrap_or(0.0);
            json!({
                "x": (g("left") + g("right")) / 2.0, "y": (g("top") + g("bottom")) / 2.0,
                "radius_x": (g("right") - g("left")) / 2.0 * fw / fh, "radius_y": (g("bottom") - g("top")) / 2.0,
                "angle": g("angle"), "feather": g("feather"),
            })
        } else {
            json!({"x0": setting["zero"][0], "y0": setting["zero"][1], "x1": setting["full"][0], "y1": setting["full"][1]})
        };
        m.components
            .push(Component::new(name, ComponentMode::Add, kind, payload));
        let mut layer = Layer::new(BASIC_EFFECT, json!({"exposure": ev}));
        layer.mask = Some(m.id.clone());
        recipe(&ctx.registry, &[], vec![layer], vec![m])
    };
    let geometries: Vec<Value> = settings
        .iter()
        .map(|setting| {
            if radial {
                setting.clone()
            } else {
                json!({"zero_x": setting["zero"][0], "zero_y": setting["zero"][1], "full_x": setting["full"][0], "full_y": setting["full"][1]})
            }
        })
        .collect();
    let flat = |recipe: Recipe, view: Option<View>| Job {
        subject: "flat".into(),
        recipe,
        sampling: Sampling::Cells(10),
        view,
        development: Development::AsShot,
    };
    let mut reads = vec![Read {
        subject: "flat".into(),
        id: format!("flat__{NEUTRAL}"),
        sampling: Sampling::Cells(10),
        view: None,
    }];
    let mut jobs = vec![flat(empty(), None)];
    for ((label, _), setting) in variants.iter().zip(&settings) {
        reads.push(Read {
            subject: "flat".into(),
            id: format!("flat__{family}__{label}"),
            sampling: Sampling::Cells(10),
            view: Some(View::Whole),
        });
        jobs.push(flat(
            masked(setting, setting["local_exposure"].as_f64().unwrap_or(-1.0)),
            Some(View::Whole),
        ));
    }
    let lr = ctx.exports(&reads)?;
    let lux = ctx.renders(&jobs)?;
    let Some((lr_neutral, _)) = &lr[0] else {
        return Err(format!("{family}: the flat field's neutral export is missing").into());
    };
    let lux_neutral = &lux[0];
    let neutral_labs = labs(&lux_neutral.samples);
    let lr_measures: Vec<Option<MaskMeasures>> = (0..variants.len())
        .map(|i| {
            lr[i + 1]
                .as_ref()
                .map(|(lv, _)| mask_measures(lr_neutral, lv, &geometries[i], radial))
        })
        .collect();
    // Luxforge again at the exposure each export shows, so the geometry is compared apart from
    // the strength: the written Exposure scaled by the stops Lightroom's export applies where the
    // effect is full over the stops Luxforge's identity render applies there.
    let matched_ev: Vec<f64> = lr_measures
        .iter()
        .zip(&settings)
        .enumerate()
        .map(|(i, (m, setting))| {
            let written = setting["local_exposure"].as_f64().unwrap_or(-1.0);
            let identity = mask_measures(lux_neutral, &lux[i + 1], &geometries[i], radial).ev;
            match (m.as_ref().and_then(|m| m.ev), identity) {
                (Some(lr), Some(lx)) if lx.abs() > 1e-6 => written * lr / lx,
                _ => written,
            }
        })
        .collect();
    let matched_jobs: Vec<Job> = settings
        .iter()
        .zip(&matched_ev)
        .map(|(s, ev)| flat(masked(s, *ev), Some(View::Whole)))
        .collect();
    let matched = ctx.renders(&matched_jobs)?;
    let mut page = format!(
        "# {family}\n\nA {} gradient with local Exposure −1 on the flat field, written in the gradient-correction form{}. Each export's own response (its change from Lightroom's neutral variant) shows which side of the mask takes the effect and the exposure it applies where it is full. Luxforge's {} mask of the same geometry is rendered with BASIC exposure −1 through it (the identity) and again at the exposure the export shows (matched), so the geometry is compared apart from the strength. Positions are shares of the mask's own geometry: ρ of the ellipse's semi-axes, t from the zero point (0) to the full point (1). Round 1, CPU reference renderer (study 16-bit output).\n\n| variant | Lightroom effect side | Luxforge effect side | stops at full effect LR / LX identity | half point LR / LX matched | 25–75% ramp LR / LX matched | orientation (°) LR / LX matched | residual identity | residual matched (median ΔE2000) | Lightroom recorded |\n|---|---|---|---|---|---|---|---|---|---|\n",
        if radial { "radial" } else { "linear" },
        if radial {
            ", the radial with `Flipped` true for an inside effect"
        } else {
            " from a zero point to a full point"
        },
        if radial {
            "radial (inside selected)"
        } else {
            "linear (0 at x0,y0, full at x1,y1)"
        },
    );
    let mut points = Vec::new();
    let mut sheet = Vec::new();
    let mut worst: f64 = 0.0;
    let mut worst_identity: f64 = 0.0;
    let mut directions = Vec::new();
    let mut charts = String::new();
    for (i, (label, _)) in variants.iter().enumerate() {
        let (Some((lv, xmp)), Some(l)) = (&lr[i + 1], &lr_measures[i]) else {
            continue;
        };
        let (xv, mv) = (&lux[i + 1], &matched[i]);
        let x = mask_measures(lux_neutral, xv, &geometries[i], radial);
        let xm = mask_measures(lux_neutral, mv, &geometries[i], radial);
        let reached: Vec<Lab> = lux_neutral
            .samples
            .iter()
            .zip(&lr_neutral.samples)
            .zip(&lv.samples)
            .map(|((n, r), a)| la::lab(la::carried(*n, *r, *a)))
            .collect();
        let target = Target::new(&neutral_labs, reached);
        let residual = target.distance(&labs(&xv.samples));
        let residual_matched = target.distance(&labs(&mv.samples));
        worst = worst.max(residual_matched);
        worst_identity = worst_identity.max(residual);
        let recorded = if radial {
            format!(
                "Flipped {}, MaskInverted {}, Angle {}, Feather {}, LocalExposure2012 {}",
                xmp.get("Flipped").map_or("–", String::as_str),
                xmp.get("MaskInverted").map_or("–", String::as_str),
                xmp.get("Angle").map_or("–", String::as_str),
                xmp.get("Feather").map_or("–", String::as_str),
                xmp.get("LocalExposure2012").map_or("–", String::as_str)
            )
        } else {
            format!(
                "Zero ({}, {}), Full ({}, {}), LocalExposure2012 {}",
                xmp.get("ZeroX").map_or("–", String::as_str),
                xmp.get("ZeroY").map_or("–", String::as_str),
                xmp.get("FullX").map_or("–", String::as_str),
                xmp.get("FullY").map_or("–", String::as_str),
                xmp.get("LocalExposure2012").map_or("–", String::as_str)
            )
        };
        page += &format!(
            "| {label} | {} | {} | {} / {} | {} / {} | {} / {} | {} / {} | {} | {} | {recorded} |\n",
            l.side,
            x.side,
            number(l.ev),
            number(x.ev),
            number(l.half),
            number(xm.half),
            number(l.ramp),
            number(xm.ramp),
            number(l.orientation),
            number(xm.orientation),
            number(Some(residual)),
            number(Some(residual_matched))
        );
        directions.push(json!({"variant": label, "lightroom": l.side, "luxforge": x.side, "lightroom_stops": l.ev, "recorded": recorded}));
        points.push(json!({
            "label": label, "lightroom_side": l.side, "luxforge_side": x.side,
            "lightroom_stops": l.ev, "luxforge_stops": x.ev, "matched_exposure": matched_ev[i],
            "lightroom_effect": l.effect, "luxforge_effect": x.effect,
            "lightroom_half": l.half, "matched_half": xm.half, "lightroom_ramp": l.ramp, "matched_ramp": xm.ramp,
            "lightroom_orientation": l.orientation, "matched_orientation": xm.orientation,
            "identity_residual": residual, "residual": residual_matched,
        }));
        let chart = report::chart(
            &format!("{family} {label}: share of the effect"),
            if radial {
                "ρ (1 at the ellipse)"
            } else {
                "t (0 at zero point, 1 at full point)"
            },
            "share of the effect",
            &[
                Series::new(
                    "Luxforge (identity)",
                    LUXFORGE,
                    Mark::Line,
                    x.profile.clone(),
                ),
                Series::new(
                    "Luxforge matched",
                    CALIBRATED,
                    Mark::Dashed,
                    xm.profile.clone(),
                ),
                Series::new("Lightroom", LIGHTROOM, Mark::Points, l.profile.clone()),
            ],
        );
        let link = svg(ctx, &format!("{family}-profile-{label}"), chart)?;
        charts += &format!("\n![{label}]({link})\n");
        sheet.push((
            label.clone(),
            vec![xv.thumb.clone(), mv.thumb.clone(), lv.thumb.clone()],
        ));
    }
    page += &charts;
    report::contact_sheet(
        &ctx.out.join("sheets").join(format!("{family}.png")),
        &["Luxforge (identity)", "Luxforge matched", "Lightroom"],
        &sheet,
    )?;
    page += &format!(
        "\n## Contact sheet\n\nColumns: Luxforge at the identity, Luxforge at the matched exposure, Lightroom; labels in `../sheets/{family}.txt`.\n\n![sheet](../sheets/{family}.png)\n"
    );
    let candidate = worst > BEHAVIOUR_RESIDUAL;
    let reasons: Vec<String> = if candidate {
        vec![format!(
            "median ΔE2000 up to {worst:.2} with the exposure matched: the falloff differs"
        )]
    } else {
        Vec::new()
    };
    Ok(Outcome {
        entry: json!({"family": family, "area": "masks", "kind": "conversion", "unit": "median ΔE2000", "worst_residual": worst, "worst_identity_residual": worst_identity, "candidate": candidate, "reasons": reasons, "directions": directions, "points": points, "page": format!("pages/{family}.md")}),
        page,
    })
}

// -------------------------------------------------------------------------------------------
// The run
// -------------------------------------------------------------------------------------------

/// Lightroom's base rendering of each untouched target, from the bare-target exports.
fn base_rendering(ctx: &Context) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for (id, subject) in &ctx.subjects {
        if subject.photo().is_some() {
            // A photograph has no bare control or pristine pixels: its base rendering is
            // Lightroom's neutral (Adobe Color) against Luxforge's (the Standard look).
            let sampling = Sampling::Normalized(PHOTO_COLUMNS);
            let read = ctx.exports(&[Read {
                subject: id.clone(),
                id: format!("{id}__{NEUTRAL}"),
                sampling,
                view: None,
            }])?;
            let lux = ctx.renders(&[Job {
                subject: id.clone(),
                recipe: empty(),
                sampling,
                view: None,
                development: Development::AsShot,
            }])?;
            let spread = read[0].as_ref().map(|(lr, _)| {
                let d: Vec<f64> = lr
                    .samples
                    .iter()
                    .zip(&lux[0].samples)
                    .map(|(x, y)| {
                        luxforge_reference::preview_error::ciede2000(la::lab(*x), la::lab(*y))
                    })
                    .collect();
                let s = la::spread(&d);
                json!({"median": s.median, "p95": s.p95, "max": s.max})
            });
            out.push(json!({
                "target": id, "photograph": true,
                "lightroom_neutral_vs_luxforge_neutral": spread,
            }));
            continue;
        }
        let Kind::Target { pristine, .. } = &subject.kind else {
            continue;
        };
        let sampling = if subject.layout["kind"] == "patches" {
            Sampling::Patches
        } else {
            Sampling::Cells(16)
        };
        let reads = [
            Read {
                subject: id.clone(),
                id: id.clone(),
                sampling,
                view: None,
            },
            Read {
                subject: id.clone(),
                id: format!("{id}__{NEUTRAL}"),
                sampling,
                view: None,
            },
        ];
        let read = ctx.exports(&reads)?;
        let lux = ctx.renders(&[Job {
            subject: id.clone(),
            recipe: empty(),
            sampling,
            view: None,
            development: Development::AsShot,
        }])?;
        let pristine_frame = Frame {
            width: subject.width,
            height: subject.height,
            rgb: pristine.iter().map(|c| u16::from(*c) * 257).collect(),
        };
        let pristine = reduce(&pristine_frame, subject, sampling, None);
        let spread = |a: &Reduced, b: &Reduced| -> Value {
            if a.samples.len() != b.samples.len() {
                return json!("sizes differ");
            }
            let d: Vec<f64> = a
                .samples
                .iter()
                .zip(&b.samples)
                .map(|(x, y)| {
                    luxforge_reference::preview_error::ciede2000(la::lab(*x), la::lab(*y))
                })
                .collect();
            let s = la::spread(&d);
            json!({"median": s.median, "p95": s.p95, "max": s.max})
        };
        let process = |i: usize| {
            read[i].as_ref().map(|(_, x)| {
                x.get("ProcessVersion")
                    .cloned()
                    .unwrap_or_else(|| "none".into())
            })
        };
        out.push(json!({
            "target": id,
            "control_export": read[0].is_some(),
            "control_process_version": process(0),
            "neutral_process_version": process(1),
            "control_vs_neutral_variant": match (&read[0], &read[1]) { (Some(a), Some(b)) => spread(&a.0, &b.0), _ => Value::Null },
            "control_vs_pristine": read[0].as_ref().map(|a| spread(&a.0, &pristine)),
            "luxforge_neutral_vs_pristine": spread(&lux[0], &pristine),
            "lightroom_neutral_vs_luxforge_neutral": read[1].as_ref().map(|b| spread(&b.0, &lux[0])),
        }));
    }
    Ok(out)
}

fn measure(
    round: u32,
    folder: &Path,
    exports: &Path,
    out: &Path,
    only: Option<&[String]>,
) -> Result {
    ensure(!out.exists(), format!("{} already exists", out.display()))?;
    let parent = out.parent().ok_or("the output has no parent")?;
    ensure(
        parent.exists(),
        format!("{} does not exist", parent.display()),
    )?;
    let folder = fs::canonicalize(folder)?;
    ensure(
        !fs::canonicalize(parent)?.starts_with(&folder),
        "the evidence directory may not be inside the round folder, which is read-only input",
    )?;
    ensure(
        exports.is_dir(),
        format!("{} holds no exports", exports.display()),
    )?;
    let manifest = read_json(&folder.join("manifest.json"))?;
    check_round(&manifest, round, &folder)?;
    let variants: HashMap<String, &Value> = manifest["variants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| Some((v["id"].as_str()?.to_owned(), v)))
        .collect();
    // The exports folder's inventory: variants, bare-target controls and anything else.
    let mut listed = BTreeSet::new();
    for entry in fs::read_dir(exports)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(stem) = name.strip_suffix(".tif") {
            listed.insert(stem.to_owned());
        }
    }
    let target_ids: BTreeSet<String> = manifest["targets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["id"].as_str().map(str::to_owned))
        .collect();
    let mut missing: Vec<String> = variants
        .keys()
        .filter(|id| !listed.contains(*id))
        .cloned()
        .collect();
    missing.sort();
    let controls: Vec<&String> = listed
        .iter()
        .filter(|id| target_ids.contains(*id))
        .collect();
    let unknown: Vec<&String> = listed
        .iter()
        .filter(|id| !variants.contains_key(*id) && !target_ids.contains(*id))
        .collect();
    let notes = exports.join("NOTES.txt");
    let notes = notes
        .exists()
        .then(|| fs::read_to_string(&notes))
        .transpose()?;
    fs::create_dir(out)?;
    for dir in ["pages", "svg", "sheets"] {
        fs::create_dir(out.join(dir))?;
    }
    let ctx = Context {
        manifest: &manifest,
        exports: exports.to_owned(),
        out: out.to_owned(),
        registry: ModuleRegistry::developer(),
        render: RenderContext::new(),
        subjects: open_subjects(&manifest, &folder, Some(&out.join("catalogs")))?,
        variants,
        ledger: Mutex::new(Ledger::default()),
        maps: Mutex::new(HashMap::new()),
    };
    let families: Vec<&Value> = manifest["families"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["id"] != NEUTRAL)
        .filter(|f| only.is_none_or(|only| only.iter().any(|o| f["id"] == o.as_str())))
        .collect();
    let mut entries = Vec::new();
    for family in families {
        let id = family["id"].as_str().unwrap_or_default().to_owned();
        let started = std::time::Instant::now();
        let outcomes = match plan(&ctx, family)? {
            Method::Numeric(spec) => vec![numeric(&ctx, &spec)?],
            Method::Curve => vec![curve(&ctx, family)?],
            Method::Crop => vec![crop(&ctx)?],
            Method::Mask(radial) => vec![mask(&ctx, &id, radial)?],
            Method::WhiteBalance(subjects) => white_balance(&ctx, subjects)?,
            Method::Lens(subjects) => vec![lens(&ctx, subjects)?],
            Method::Pair(subjects) => vec![pair(&ctx, family, subjects)?],
            Method::Unmeasured(why) => {
                entries.push(
                    json!({"family": id, "area": family["area"], "kind": "unmeasured", "why": why}),
                );
                Vec::new()
            }
        };
        for outcome in outcomes {
            let page = outcome.entry["family"].as_str().unwrap_or(&id).to_owned();
            fs::write(out.join("pages").join(format!("{page}.md")), &outcome.page)?;
            entries.push(outcome.entry);
        }
        println!("{id}: {:.1} s", started.elapsed().as_secs_f64());
    }
    let base = base_rendering(&ctx)?;
    // The round's photographs, read again: nothing changed them.
    for subject in ctx.subjects.values() {
        if let Some(photo) = subject.photo() {
            ensure(
                photo.intact()?,
                format!("{} changed during the measurement", photo.path.display()),
            )?;
        }
    }
    let ledger = ctx.ledger.into_inner().map_err(|_| "a poisoned ledger")?;
    let mut versions: BTreeMap<String, usize> = BTreeMap::new();
    let mut creators: BTreeMap<String, usize> = BTreeMap::new();
    let mut control_versions: BTreeMap<String, String> = BTreeMap::new();
    for (id, (version, creator)) in &ledger.process {
        if target_ids.contains(id) {
            control_versions.insert(id.clone(), version.clone());
        } else {
            *versions.entry(version.clone()).or_default() += 1;
        }
        *creators.entry(creator.clone()).or_default() += 1;
    }
    let exports_json = json!({
        "folder": exports, "listed": listed.len(), "variants_in_manifest": manifest["variants"].as_array().map_or(0, Vec::len),
        "missing_variants": missing, "bare_target_controls": controls, "unknown": unknown,
        "notes": notes, "variant_process_versions": versions, "control_process_versions": control_versions,
        "creators": creators, "rewritten_settings": ledger.rewritten, "problems": ledger.problems,
    });
    let report_json = json!({
        "tool": "cargo xtask lr-align measure", "round": round, "round_folder": folder,
        "renderer": "Luxforge's whole-frame CPU reference renderer through Render::study_frame16 (16-bit sRGB terminal conversion); no GPU output was measured",
        "intermediate_quantization": {
            "source": "each target is a JPEG decoded to 8-bit sRGB, which both editors read",
            "kept": ledger.quantization,
        },
        "behaviour_threshold": {"residual_de2000": BEHAVIOUR_RESIDUAL, "share_of_range_from_default": THRESHOLD_SHARE},
        "slider_responses": super::fold::build_responses(),
        "exports": exports_json,
        "base_rendering": base,
        "families": entries,
    });
    write_json(&out.join("lr-align.json"), &report_json)?;
    // The calibrated rows, one per setting, the first family measuring it (the 2400 × 1600 chart
    // before its 6000 × 4000 twin): what the presets' frozen calibration table is copied from.
    let mut maps: Vec<Value> = Vec::new();
    for entry in report_json["families"].as_array().into_iter().flatten() {
        let calibration = &entry["calibration"];
        if calibration.is_object() && !maps.iter().any(|m| m["setting"] == calibration["setting"]) {
            let mut map = calibration.clone();
            map["family"] = entry["family"].clone();
            maps.push(map);
        }
    }
    write_json(
        &out.join("maps.json"),
        &json!({
            "format": "luxforge.lightroom-calibration", "version": 1, "round": round,
            "basis": "cargo xtask lr-align measure: Lightroom Classic exports of the round's synthetic targets against Luxforge's CPU reference renderer; each map the knots of the setting's monotone fit",
            "maps": maps,
        }),
    )?;
    fs::write(out.join("summary.md"), summary(&report_json))?;
    println!("wrote {}", out.display());
    Ok(())
}

fn summary(report: &Value) -> String {
    let families = report["families"].as_array().cloned().unwrap_or_default();
    let worst = |f: &Value| f["worst_residual"].as_f64().unwrap_or(f64::NEG_INFINITY);
    let mut colour: Vec<&Value> = families
        .iter()
        .filter(|f| f["unit"] == "median ΔE2000")
        .collect();
    colour.sort_by(|a, b| worst(b).total_cmp(&worst(a)));
    let mut text = String::from(
        "# Lightroom alignment, round 1: measurement\n\nLuxforge renders through the whole-frame CPU reference renderer with the study-only 16-bit sRGB terminal conversion, on a Linux host without Metal: no GPU output was measured. Lightroom's side is the owner's Lightroom Classic exports (16-bit TIFF). Every figure is a response, each editor's change from its own neutral variant, Lightroom's carried onto Luxforge's neutral rendering. A setting is a level C candidate when a point at a Lightroom value at least an eighth of its range from the default (±25 of a −100..100 slider) keeps a median ΔE2000 above 2 after the best Luxforge value, or when a measure finds a difference in kind.\n\n",
    );
    text += "## Ranked by residual (median ΔE2000)\n\nWorst residual at the threshold distance and beyond, after the fitted map (or at the identity for a conversion), with the identity residual for comparison.\n\n| rank | family | kind | residual | identity residual | direction | shortfall | C candidate |\n|---|---|---|---|---|---|---|---|\n";
    for (rank, f) in colour.iter().enumerate() {
        text += &format!(
            "| {} | [{}]({}) | {} | {} | {} | {} | {} | {} |\n",
            rank + 1,
            f["family"].as_str().unwrap_or_default(),
            f["page"].as_str().unwrap_or_default(),
            f["kind"].as_str().unwrap_or_default(),
            number(f["worst_residual"].as_f64()),
            number(f["worst_identity_residual"].as_f64()),
            f["direction"].as_str().unwrap_or("–"),
            f["shortfalls"]
                .as_array()
                .map_or("–".into(), |s| if s.is_empty() {
                    "none".into()
                } else {
                    s.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                }),
            if f["candidate"] == true {
                "**yes**"
            } else {
                "no"
            },
        );
    }
    let geometric: Vec<&Value> = families
        .iter()
        .filter(|f| f["unit"].is_string() && f["unit"] != "median ΔE2000")
        .collect();
    text += "\n## Geometry (own units)\n\n| family | unit | worst residual | identity residual |\n|---|---|---|---|\n";
    for f in geometric {
        text += &format!(
            "| [{}]({}) | {} | {} | {} |\n",
            f["family"].as_str().unwrap_or_default(),
            f["page"].as_str().unwrap_or_default(),
            f["unit"].as_str().unwrap_or_default(),
            number(f["worst_residual"].as_f64()),
            number(f["worst_identity_residual"].as_f64()),
        );
    }
    text += "\n## Level C candidates\n\nReported for the owner's review; nothing here changes a control.\n\n";
    for f in families.iter().filter(|f| f["candidate"] == true) {
        let reasons: Vec<&str> = f["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        text += &format!(
            "- **{}**: {}\n",
            f["family"].as_str().unwrap_or_default(),
            reasons.join("; ")
        );
    }
    text += "\n## Masks: direction from the exports\n\n";
    for f in families.iter().filter(|f| f["area"] == "masks") {
        for d in f["directions"].as_array().into_iter().flatten() {
            text += &format!(
                "- {} {}: Lightroom's effect falls {} (Luxforge {}), {} stops where full; recorded {}\n",
                f["family"].as_str().unwrap_or_default(),
                d["variant"].as_str().unwrap_or_default(),
                d["lightroom"].as_str().unwrap_or_default(),
                d["luxforge"].as_str().unwrap_or_default(),
                number(d["lightroom_stops"].as_f64()),
                d["recorded"].as_str().unwrap_or_default()
            );
        }
    }
    text += "\n## Not measured\n\n";
    for f in families.iter().filter(|f| f["kind"] == "unmeasured") {
        text += &format!(
            "- {}: {}\n",
            f["family"].as_str().unwrap_or_default(),
            f["why"].as_str().unwrap_or_default()
        );
    }
    let e = &report["exports"];
    text += &format!(
        "\n## The exports\n\n- {} TIFFs listed; {} variants in the manifest; missing variants: {}.\n- Bare-target controls (no XMP, not in the manifest): {}.\n- Other files: {}.\n- `NOTES.txt`: {}.\n- Process versions recorded in the variants' XMP: {}; in the controls': {}.\n- Creator recorded: {}.\n- Settings Lightroom recorded with another value than written: {}.\n- Problems: {}.\n",
        e["listed"],
        e["variants_in_manifest"],
        e["missing_variants"],
        e["bare_target_controls"],
        e["unknown"],
        if e["notes"].is_null() {
            "not written (the Lightroom Classic version and the Process Version check were not recorded by the owner)".to_string()
        } else {
            "present".into()
        },
        e["variant_process_versions"],
        e["control_process_versions"],
        e["creators"],
        e["rewritten_settings"],
        e["problems"],
    );
    text += "\n## Base rendering\n\nCIEDE2000 spreads (median, p95, max) over each target's patches or 16-pixel cells.\n\n| target | control vs neutral variant | control vs pristine | Luxforge neutral vs pristine | Lightroom neutral vs Luxforge neutral |\n|---|---|---|---|---|\n";
    let cell = |v: &Value| -> String {
        if v.is_object() {
            format!(
                "{} / {} / {}",
                number(v["median"].as_f64()),
                number(v["p95"].as_f64()),
                number(v["max"].as_f64())
            )
        } else {
            v.as_str().unwrap_or("–").to_owned()
        }
    };
    for b in report["base_rendering"].as_array().into_iter().flatten() {
        text += &format!(
            "| {} | {} | {} | {} | {} |\n",
            b["target"].as_str().unwrap_or_default(),
            cell(&b["control_vs_neutral_variant"]),
            cell(&b["control_vs_pristine"]),
            cell(&b["luxforge_neutral_vs_pristine"]),
            cell(&b["lightroom_neutral_vs_luxforge_neutral"]),
        );
    }
    text += &format!(
        "\n## Intermediate quantization\n\n- {}\n{}",
        report["intermediate_quantization"]["source"]
            .as_str()
            .unwrap_or_default(),
        report["intermediate_quantization"]["kept"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|k| format!("- {k}\n"))
            .collect::<String>()
    );
    text
}

#[cfg(test)]
mod tests {
    use super::super::{Plan, generate, rounds};
    use super::*;
    use luxforge_test_macros::slow;

    /// What Lightroom's XMP attributes read as: the document's own before a mask's, and the
    /// creator under its namespace.
    #[test]
    fn xmp_attributes_read_the_document_before_its_masks() {
        let xmp = parse_xmp(
            r#"<rdf:Description xmp:CreatorTool="Tool 1" crs:Version="18.7" crs:ProcessVersion="11.0"><crs:MaskGroupBasedCorrections crs:Version="2" crs:Flipped="true"/>"#,
        );
        assert_eq!(xmp["Version"], "18.7");
        assert_eq!(xmp["ProcessVersion"], "11.0");
        assert_eq!(xmp["Flipped"], "true");
        assert_eq!(xmp["xmp:CreatorTool"], "Tool 1");
    }

    /// A held setting's map is read by interpolation between its points and never beyond them.
    #[test]
    fn a_map_interpolates_inside_its_points_only() {
        let points = [(0.0, 0.0), (-100.0, -70.0), (-50.0, -40.0)];
        assert_eq!(interpolate(&points, -50.0), Some(-40.0));
        assert_eq!(interpolate(&points, -75.0), Some(-55.0));
        assert_eq!(interpolate(&points, 10.0), None);
    }

    /// A round of Saturation on the patch grid and Vignette Amount on the flat field, generated by
    /// the rig, whose "exports" are Luxforge's own study renders of every variant: the measurement
    /// must fit each as the identity within its resolution, with no residual, shortfall or
    /// candidate. Measuring into the round folder, or as another round, is refused.
    #[test]
    #[slow]
    fn a_round_of_its_own_renders_measures_as_the_identity() {
        let directory = tempfile::tempdir().unwrap();
        let round = directory.path().join("round");
        // Values on the dense grid: between its samples a realigned slider's response bends, so the
        // blend of the two around a value is not its own render.
        let keep = ["-50", "+50", "+100"];
        let families: Vec<rounds::Family> = rounds::round_one()
            .into_iter()
            .filter(|f| matches!(f.id.as_str(), "saturation" | "vignette-amount"))
            .map(|mut f| {
                f.steps.retain(|step| keep.contains(&step.label.as_str()));
                f
            })
            .collect();
        generate(
            &Plan {
                round: 1,
                families,
                photos: Vec::new(),
                excluded: Vec::new(),
            },
            &round,
        )
        .unwrap();
        // Luxforge's study renders of the round, written as its exports.
        let exports = round.join("exports");
        fs::create_dir(&exports).unwrap();
        let manifest = read_json(&round.join("manifest.json")).unwrap();
        let ctx = Context {
            manifest: &manifest,
            exports: exports.clone(),
            out: directory.path().join("unused"),
            registry: ModuleRegistry::developer(),
            render: RenderContext::new(),
            subjects: open_subjects(&manifest, &round, None).unwrap(),
            variants: HashMap::new(),
            ledger: Mutex::new(Ledger::default()),
            maps: Mutex::new(HashMap::new()),
        };
        let write = |subject: &str, id: &str, recipe: &Recipe| {
            let Kind::Target { source, .. } = &ctx.subject(subject).kind else {
                panic!("a target")
            };
            let study = render(
                &ctx.registry,
                source,
                recipe,
                RenderOptions::default(),
                &ctx.render,
            )
            .unwrap()
            .study_frame16()
            .unwrap();
            let xmp = "<x crs:ProcessVersion=\"11.0\"/>";
            fs::write(
                exports.join(format!("{id}.tif")),
                crate::tiff_reader::write(study.width, study.height, &study.rgb, Some(xmp)),
            )
            .unwrap();
        };
        for family in manifest["families"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["id"] != NEUTRAL)
        {
            let Method::Numeric(spec) = plan(&ctx, family).unwrap() else {
                panic!("a numeric family")
            };
            for subject in &spec.subjects {
                write(subject, &format!("{subject}__{NEUTRAL}"), &empty());
                for (label, value) in &spec.values {
                    write(
                        subject,
                        &format!("{subject}__{}__{label}", spec.id),
                        &(spec.recipe)(*value),
                    );
                }
            }
        }
        let out = directory.path().join("measured");
        measure(1, &round, &exports, &out, None).unwrap();
        let report = read_json(&out.join("lr-align.json")).unwrap();
        let families = report["families"].as_array().unwrap();
        assert_eq!(families.len(), 2);
        for family in families {
            assert_eq!(family["candidate"], false, "{family}");
            assert_eq!(family["direction"], "Increasing", "{family}");
            for point in family["points"].as_array().unwrap() {
                let value = point["lightroom"].as_f64().unwrap();
                let span = [0, 1].map(|end| point["span"][end].as_f64().unwrap());
                assert!(
                    span[0] - 1e-9 <= value && value <= span[1] + 1e-9,
                    "{point}"
                );
                assert!(
                    point["residual"].as_f64().unwrap() < la::SHORTFALL_RESIDUAL,
                    "{point}"
                );
                assert_eq!(point["shortfall"], false, "{point}");
            }
            assert!(out.join(family["page"].as_str().unwrap()).exists());
        }
        assert_eq!(report["exports"]["missing_variants"], json!([]));
        assert_eq!(
            report["exports"]["variant_process_versions"],
            json!({"11.0": 8})
        );
        assert!(out.join("summary.md").exists());
        assert!(out.join("sheets/vignette-amount.png").exists());

        let inside = measure(1, &round, &exports, &round.join("measured"), None)
            .unwrap_err()
            .to_string();
        assert!(inside.contains("read-only"), "{inside}");
        let other = measure(2, &round, &exports, &directory.path().join("again"), None)
            .unwrap_err()
            .to_string();
        assert!(other.contains("round 1, not 2"), "{other}");
    }

    /// A round on one synthetic RAW photograph (the test-camera DNG body), its Exposure and
    /// white-balance families, whose "exports" are Luxforge's own renders through the same
    /// photograph path: Exposure fits as the identity, each white-balance sweep fits with no
    /// residual, the lens profile is reported with no eligible profile, the photograph is left as
    /// it was, and the scratch catalog lies in the evidence directory.
    #[test]
    #[slow]
    fn a_photograph_round_of_its_own_renders_measures_without_residual() {
        luxforge_raw::admit_test_cameras();
        let directory = tempfile::tempdir().unwrap();
        let originals = directory.path().join("originals");
        let written = luxforge_testkit::hdr_bracket::write_bracket(
            &originals,
            &luxforge_testkit::hdr_bracket::BracketSpec {
                frames: 2,
                noise: false,
                ..Default::default()
            },
        )
        .unwrap();
        let original = written[0].clone();
        let bytes = fs::read(&original).unwrap();
        let keep = [
            "-1",
            "+1",
            "4000K0",
            "5500K0",
            "6500K0",
            "5500K-25",
            "5500K+25",
            "distortion",
        ];
        let families: Vec<rounds::Family> = rounds::round_one()
            .into_iter()
            .filter(|f| {
                matches!(
                    f.id.as_str(),
                    "exposure-photographs" | "white-balance" | "lens-profile"
                )
            })
            .map(|mut f| {
                f.steps.retain(|step| keep.contains(&step.label.as_str()));
                f
            })
            .collect();
        let photo = super::super::photos::Photo {
            id: "synthetic".into(),
            origin: "test",
            camera: "Luxforge Synthetic Bracket".into(),
            why: "a test".into(),
            path: original.clone(),
            extension: "DNG".into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes: bytes.len() as u64,
            dimensions: [1520, 1008],
        };
        let round = directory.path().join("round");
        generate(
            &Plan {
                round: 1,
                families,
                photos: vec![photo],
                excluded: Vec::new(),
            },
            &round,
        )
        .unwrap();
        let exports = round.join("exports");
        fs::create_dir(&exports).unwrap();
        let manifest = read_json(&round.join("manifest.json")).unwrap();
        let scratch = directory.path().join("scratch");
        let variants: HashMap<String, &Value> = manifest["variants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| (v["id"].as_str().unwrap().to_owned(), v))
            .collect();
        let ctx = Context {
            manifest: &manifest,
            exports: exports.clone(),
            out: scratch.clone(),
            registry: ModuleRegistry::developer(),
            render: RenderContext::new(),
            subjects: open_subjects(&manifest, &round, Some(&scratch)).unwrap(),
            variants,
            ledger: Mutex::new(Ledger::default()),
            maps: Mutex::new(HashMap::new()),
        };
        // Luxforge's own render of a variant, written as its export.
        let write = |id: &str, recipe: Recipe, development: Development| {
            let subject = ctx.subject("synthetic");
            let Kind::Photograph(photo) = &subject.kind else {
                panic!("a photograph")
            };
            let developed = photo.develop(&development).unwrap();
            let recipe = over(&ctx.registry, &developed.base, &recipe);
            let study = render(
                &ctx.registry,
                RenderSource::Linear {
                    image: &developed.image,
                    settings: LinearSettings::default(),
                },
                &recipe,
                RenderOptions::default(),
                &ctx.render,
            )
            .unwrap()
            .study_frame16()
            .unwrap();
            let xmp = "<x crs:ProcessVersion=\"11.0\"/>";
            fs::write(
                exports.join(format!("{id}.tif")),
                crate::tiff_reader::write(study.width, study.height, &study.rgb, Some(xmp)),
            )
            .unwrap();
        };
        write("synthetic__neutral", empty(), Development::AsShot);
        let family = manifest["families"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == "exposure-photographs")
            .unwrap();
        let Method::Numeric(spec) = plan(&ctx, family).unwrap() else {
            panic!("numeric")
        };
        for (label, value) in &spec.values {
            write(
                &format!("synthetic__exposure-photographs__{label}"),
                (spec.recipe)(*value),
                Development::AsShot,
            );
        }
        // White balance as the measurement holds it: each Kelvin at the tint the importer gives
        // Lightroom's 5500 K and tint 0, each tint offset from it at its Kelvin.
        let held = &manifest["variants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == "synthetic__white-balance__5500K0")
            .unwrap()["luxforge"]["set-raw"];
        let (held_kelvin, held_tint) = (
            held["temperature"].as_f64().unwrap_or(5500.0),
            held["tint"].as_f64().unwrap_or(0.0),
        );
        for (label, (kelvin, tint)) in ["4000K0", "5500K0", "6500K0", "5500K-25", "5500K+25"]
            .iter()
            .map(|l| (*l, white_balance_label(l).unwrap()))
        {
            let temperature = if kelvin == 5500.0 {
                held_kelvin
            } else {
                kelvin
            };
            write(
                &format!("synthetic__white-balance__{label}"),
                empty(),
                Development::WhiteBalance {
                    temperature,
                    tint: held_tint + tint,
                },
            );
        }
        drop(spec);
        drop(ctx);
        let out = directory.path().join("measured");
        measure(1, &round, &exports, &out, None).unwrap();
        let report = read_json(&out.join("lr-align.json")).unwrap();
        let family = |id: &str| {
            report["families"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["family"] == id)
                .unwrap()
                .clone()
        };
        // White balance renders a development per value on coarse grids (500 K, 20 tint), so its
        // residual between two of them is the interpolation's, held to 1.
        for (id, limit) in [
            ("exposure-photographs", la::SHORTFALL_RESIDUAL),
            ("white-balance-temperature", 1.0),
            ("white-balance-tint", 1.0),
        ] {
            let family = family(id);
            assert_eq!(family["direction"], "Increasing", "{family}");
            for point in family["points"].as_array().unwrap() {
                assert!(point["residual"].as_f64().unwrap() < limit, "{id}: {point}");
            }
        }
        for point in family("exposure-photographs")["points"].as_array().unwrap() {
            let value = point["lightroom"].as_f64().unwrap();
            let span = [0, 1].map(|end| point["span"][end].as_f64().unwrap());
            assert!(
                span[0] - 1e-9 <= value && value <= span[1] + 1e-9,
                "{point}"
            );
        }
        assert_eq!(family("lens-profile")["points"][0]["profile"], Value::Null);
        assert!(out.join("catalogs/synthetic").is_dir());
        assert_eq!(fs::read(&original).unwrap(), bytes);
    }

    /// A round 2 pair on the patch grid whose "exports" are Luxforge's own renders at the
    /// identity: the pair measures no residual at the identity, and its calibrated renders follow
    /// both settings' frozen maps.
    #[test]
    #[slow]
    fn a_pair_of_its_own_renders_measures_no_residual_at_the_identity() {
        let directory = tempfile::tempdir().unwrap();
        let round = directory.path().join("round");
        let families: Vec<rounds::Family> = rounds::round_two()
            .into_iter()
            .filter(|f| f.id == "vibrance+saturation")
            .map(|mut f| {
                f.steps
                    .retain(|step| matches!(step.label.as_str(), "-50_+50" | "+50_+100"));
                f
            })
            .collect();
        generate(
            &Plan {
                round: 2,
                families,
                photos: Vec::new(),
                excluded: Vec::new(),
            },
            &round,
        )
        .unwrap();
        let exports = round.join("exports");
        fs::create_dir(&exports).unwrap();
        let manifest = read_json(&round.join("manifest.json")).unwrap();
        let ctx = Context {
            manifest: &manifest,
            exports: exports.clone(),
            out: directory.path().join("unused"),
            registry: ModuleRegistry::developer(),
            render: RenderContext::new(),
            subjects: open_subjects(&manifest, &round, None).unwrap(),
            variants: HashMap::new(),
            ledger: Mutex::new(Ledger::default()),
            maps: Mutex::new(HashMap::new()),
        };
        let write = |id: &str, recipe: &Recipe| {
            let Kind::Target { source, .. } = &ctx.subject("patches").kind else {
                panic!("a target")
            };
            let study = render(
                &ctx.registry,
                source,
                recipe,
                RenderOptions::default(),
                &ctx.render,
            )
            .unwrap()
            .study_frame16()
            .unwrap();
            let xmp = "<x crs:ProcessVersion=\"11.0\"/>";
            fs::write(
                exports.join(format!("{id}.tif")),
                crate::tiff_reader::write(study.width, study.height, &study.rgb, Some(xmp)),
            )
            .unwrap();
        };
        write("patches__neutral", &empty());
        for (label, vibrance, saturation) in [("-50_+50", -50.0, 50.0), ("+50_+100", 50.0, 100.0)] {
            let fields = [
                (BASIC_EFFECT, "vibrance".to_owned(), json!(vibrance)),
                (BASIC_EFFECT, "saturation".to_owned(), json!(saturation)),
            ];
            write(
                &format!("patches__vibrance+saturation__{label}"),
                &recipe(&ctx.registry, &fields, Vec::new(), Vec::new()),
            );
        }
        drop(ctx);
        let out = directory.path().join("measured");
        measure(2, &round, &exports, &out, None).unwrap();
        let report = read_json(&out.join("lr-align.json")).unwrap();
        let family = &report["families"][0];
        assert_eq!(family["kind"], "pair", "{family}");
        let points = family["points"].as_array().unwrap();
        assert_eq!(points.len(), 2);
        for point in points {
            assert!(
                point["identity_residual"].as_f64().unwrap() < 1e-3,
                "{point}"
            );
            let [vibrance, saturation] = [0, 1].map(|i| point["lightroom"][i].as_f64().unwrap());
            // Each through its calibration, or as it is where the slider follows Lightroom's.
            let through = |setting: &str, value: f64| {
                luxforge_core::lightroom_calibration(setting, value).map_or(value, |c| c.unwrap())
            };
            let expected = [
                through("Vibrance", vibrance),
                through("Saturation", saturation),
            ];
            assert_eq!(point["luxforge"], json!(expected), "{point}");
        }
    }
}
