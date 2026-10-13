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
use super::report::{self, CALIBRATED, LIGHTROOM, LUXFORGE, Mark, Series, number};
use super::{FORMAT, NEUTRAL, readback, targets};
use crate::*;
use luxforge_core::{
    BASIC_EFFECT, BoxRect, CURVE_EFFECT, Component, ComponentMode, CropPayload, CropStage,
    DETAIL_EFFECT, Layer, MIXER_EFFECT, Mask, ModuleRegistry, PERSPECTIVE_EFFECT, PRESENCE_EFFECT,
    RECIPE_FORMAT, Recipe, RenderContext, RenderOptions, SourceImage, VIGNETTE_EFFECT, open_source,
    render,
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
    spatial: Option<Spatial>,
    /// `L*` at a quarter of the frame's size on each axis, for positional measures.
    plane: Option<(usize, Vec<f64>)>,
    dots: Vec<[f64; 2]>,
    thumb: Option<image::RgbImage>,
}

/// One target the round renders from: its layout, Luxforge's decode of its JPEG and its pristine
/// pixels as the generator computed them.
struct Subject {
    id: String,
    width: u32,
    height: u32,
    layout: Value,
    source: SourceImage,
    pristine: Vec<u8>,
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
    };
    let same = (frame.width, frame.height) == (subject.width, subject.height);
    Reduced {
        width: frame.width,
        height: frame.height,
        samples,
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
fn parallel<T: Sync, R: Send>(
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

/// One Luxforge render to make: its subject, recipe, sampling and sheet view.
struct Job {
    subject: String,
    recipe: Recipe,
    sampling: Sampling,
    view: Option<View>,
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
            let study = render(
                &self.registry,
                &subject.source,
                &job.recipe,
                RenderOptions::default(),
                &self.render,
            )
            .and_then(|r| r.study_frame16())
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
            let tiff = crate::tiff::read(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
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

fn open_subjects(manifest: &Value, folder: &Path) -> Result<BTreeMap<String, Subject>> {
    let mut subjects = BTreeMap::new();
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
                source,
                pristine: ours.rgb,
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
}

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
        if spec.response == Response::Colour {
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
        });
        for u in &values {
            jobs.push(Job {
                subject: s.clone(),
                recipe: (spec.recipe)(*u),
                sampling: sampling(s),
                view: (*s == first && identities.contains(&key(*u))).then_some(view),
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
                id: format!("{s}__{}__{label}", spec.id),
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
                missing.push(format!("{s}__{}__{label}", spec.id));
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
    let fit = match spec.response {
        Response::Colour => {
            let dense_labs: Vec<Vec<Lab>> =
                spec.dense.iter().map(|u| concat(&lux[&key(*u)])).collect();
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
    finish(ctx, spec, &fit, rows, lux_curve, sheet, missing)
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
    // Locality: a pointwise Luxforge control against a local Lightroom one.
    if let Some(si) = subject_index("surround")
        && spec.area == "basic"
    {
        let pairs: Vec<(f64, f64)> = rows
            .iter()
            .filter(|r| r.far)
            .filter_map(|r| Some((r.lr[si].locality?, r.cal[si].locality?)))
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
    Unmeasured(String),
}

fn plan<'f>(ctx: &'f Context, family: &Value) -> Result<Method<'f>> {
    let id = family["id"].as_str().unwrap_or_default().to_owned();
    let area = family["area"].as_str().unwrap_or_default().to_owned();
    let subjects: Vec<String> = family["subjects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect();
    if subjects.iter().all(|s| s == "photographs") {
        return Ok(Method::Unmeasured(
            "the round holds no RAW photographs (generated without --corpus or --manifest), so this family has no variant".into(),
        ));
    }
    match id.as_str() {
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
    let dense = match (effect, field.as_str()) {
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
                sheet.push((
                    label.clone(),
                    vec![lv.thumb.clone(), None, rv.thumb.clone()],
                ));
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
        &["Luxforge", "(no calibration)", "Lightroom"],
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
        sheet.push((
            label.clone(),
            vec![lx.thumb.clone(), None, lr.thumb.clone()],
        ));
    }
    report::contact_sheet(
        &ctx.out.join("sheets/crop.png"),
        &["Luxforge", "(no calibration)", "Lightroom"],
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
    // the strength.
    let matched_ev: Vec<f64> = lr_measures
        .iter()
        .zip(&settings)
        .map(|(m, setting)| {
            m.as_ref()
                .and_then(|m| m.ev)
                .unwrap_or_else(|| setting["local_exposure"].as_f64().unwrap_or(-1.0))
                .clamp(-5.0, 5.0)
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
            "lightroom_stops": l.ev, "luxforge_stops": x.ev, "matched_stops": matched_ev[i],
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
        }])?;
        let pristine_frame = Frame {
            width: subject.width,
            height: subject.height,
            rgb: subject
                .pristine
                .iter()
                .map(|c| u16::from(*c) * 257)
                .collect(),
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
    let ctx = Context {
        manifest: &manifest,
        exports: exports.to_owned(),
        out: out.to_owned(),
        registry: ModuleRegistry::developer(),
        render: RenderContext::new(),
        subjects: open_subjects(&manifest, &folder)?,
        variants,
        ledger: Mutex::new(Ledger::default()),
        maps: Mutex::new(HashMap::new()),
    };
    fs::create_dir(out)?;
    for dir in ["pages", "svg", "sheets"] {
        fs::create_dir(out.join(dir))?;
    }
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
        let outcome = match plan(&ctx, family)? {
            Method::Numeric(spec) => Some(numeric(&ctx, &spec)?),
            Method::Curve => Some(curve(&ctx, family)?),
            Method::Crop => Some(crop(&ctx)?),
            Method::Mask(radial) => Some(mask(&ctx, &id, radial)?),
            Method::Unmeasured(why) => {
                entries.push(
                    json!({"family": id, "area": family["area"], "kind": "unmeasured", "why": why}),
                );
                None
            }
        };
        if let Some(outcome) = outcome {
            fs::write(out.join("pages").join(format!("{id}.md")), &outcome.page)?;
            entries.push(outcome.entry);
        }
        println!("{id}: {:.1} s", started.elapsed().as_secs_f64());
    }
    let base = base_rendering(&ctx)?;
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
        let keep = ["-50", "+25", "+100"];
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
            subjects: open_subjects(&manifest, &round).unwrap(),
            variants: HashMap::new(),
            ledger: Mutex::new(Ledger::default()),
            maps: Mutex::new(HashMap::new()),
        };
        let write = |subject: &str, id: &str, recipe: &Recipe| {
            let study = render(
                &ctx.registry,
                &ctx.subject(subject).source,
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
                crate::tiff::write(study.width, study.height, &study.rgb, Some(xmp)),
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
}
