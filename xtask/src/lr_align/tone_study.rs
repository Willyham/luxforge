//! `cargo xtask lr-align tone-study`: the study behind `docs/design/basic-local-tone.md`.
//!
//! `tone-study --charts ROUND1 --photographs ROUND1B --output NEW [--reduce N] [--candidates ID,...]
//! [--cache DIR]` compares candidate Highlights, Shadows, Whites and Blacks operators with Lightroom
//! Classic's exports of round 1's charts (the wedge, the patch grid and the surround target) and of
//! the corpus photographs, and asks of each candidate whether one response (slider value to
//! strength) fits both within A8 (median CIEDE2000 at most 2 at Lightroom values of ±25 and beyond).
//!
//! Every candidate is a pure `f64` function from `luxforge_reference::local_tone`, evaluated here
//! over the subject's own pixels, never the editor's renderer: a chart is its target in linear
//! light; a photograph is its development's scene-linear planes, rendered once through the
//! reference renderer at −3 EV (a plain gain on RAW) so the 16-bit study output keeps three stops
//! of headroom, then multiplied back, reduced by `--reduce` (4 by default) per axis, with the Look
//! its import gave it read from its payload and applied by `luxforge_reference::look`. A candidate
//! runs before the Look (on scene values, where Basic's tone unit runs today) or after it (on
//! rendered values). Responses are each editor's change from its own neutral, Lightroom's carried
//! onto Luxforge's neutral in Oklab, as `lr-align measure` compares them.
//!
//! `--pairs ROUND2` also renders round 2's photograph pairs (Contrast with Whites and with Blacks,
//! Exposure with Highlights and with Whites) through each candidate at the values the sliders give
//! today (their `response.json` strengths; Exposure a plain gain on the scene planes) and reports
//! each pair's residual against Lightroom's: whether moving the tone after the Look, or making it
//! local, brings the combinations closer. Dehaze with Contrast is not rendered: the study has no
//! Dehaze.
//!
//! The rounds' folders are only read. `--cache` keeps the loaded subjects and Lightroom's reduced
//! exports between runs; a cache is the study's own and holds no original.
use super::NEUTRAL;
use super::photographs::{Development, Photo};
use super::report::{self, number};
use crate::*;
use luxforge_core::{
    BASIC_EFFECT, LOOK_EFFECT, Layer, LinearSettings, ModuleRegistry, RECIPE_FORMAT, Recipe,
    RenderContext, RenderOptions, RenderSource, basic_response, open_source, render,
};
use luxforge_reference::local_tone::{
    self as lt, Frame, Levels, LocalConfig, Reconstruction, Smoother,
};
use luxforge_reference::look::{Look, LookTone, look_pixel};
use luxforge_reference::lr_align::{self as la, Lab, Target};
use luxforge_reference::preview_error::ciede2000;
use luxforge_reference::srgb;
use luxforge_reference::tone::{ToneParams, luminance};
use std::collections::BTreeMap;

const CHART_SUBJECTS: [&str; 3] = ["wedge", "patches", "surround"];
const CHART_LABELS: [&str; 10] = [
    "-100", "-75", "-50", "-25", "-10", "+10", "+25", "+50", "+75", "+100",
];
const PHOTO_LABELS: [&str; 4] = ["-100", "-50", "+50", "+100"];
/// Columns of a photograph's normalized cells, as `lr-align measure` lays them out.
const PHOTO_COLUMNS: usize = 240;
/// Band-energy scales, in samples of the study's reduced frame (a quarter of full size at the
/// default reduction, the scales `lr-align measure` reads).
const BANDS: [usize; 3] = [1, 4, 16];
/// The exposure the scene planes are rendered at, so three stops above white survive the 16-bit
/// study output.
const SCENE_EV: f64 = -3.0;
/// The A8 threshold, and the share of the range from the default at which it applies.
const A8: f64 = 2.0;
const A8_FROM: f64 = 25.0;
/// How finely the fit refines between grid strengths around its best.
const REFINE: usize = 8;
/// Thumbnails' long side.
const THUMB: usize = 300;

pub fn run(root: &Path, mut a: Args) -> Result {
    let charts = absolute(root, &a.path("--charts")?);
    let photographs = absolute(root, &a.path("--photographs")?);
    let out = absolute(root, &a.path("--output")?);
    let reduction: usize = match a.value("--reduce")? {
        Some(v) => v
            .to_string_lossy()
            .parse()
            .map_err(|_| "--reduce takes a factor")?,
        None => 4,
    };
    let only: Option<Vec<String>> = a
        .value("--candidates")?
        .map(|v| v.to_string_lossy().split(',').map(str::to_owned).collect());
    let fields: Option<Vec<String>> = a
        .value("--fields")?
        .map(|v| v.to_string_lossy().split(',').map(str::to_owned).collect());
    let cache = a.value("--cache")?.map(|p| absolute(root, Path::new(&p)));
    let pairs_folder = a.value("--pairs")?.map(|p| absolute(root, Path::new(&p)));
    a.done()?;
    ensure(!out.exists(), format!("{} exists", out.display()))?;
    ensure(reduction >= 1, "--reduce takes a factor of at least 1")?;
    for folder in [Some(&charts), Some(&photographs), pairs_folder.as_ref()]
        .into_iter()
        .flatten()
    {
        ensure(
            !out.starts_with(folder),
            "the output may not lie inside a round folder",
        )?;
    }
    fs::create_dir_all(out.join("sheets"))?;
    fs::create_dir_all(out.join("svg"))?;
    let study = match &cache {
        Some(dir) if dir.join("study.json").exists() => Study::load(dir)?,
        _ => {
            let study = Study::read(&charts, &photographs, &out, reduction)?;
            if let Some(dir) = &cache {
                study.save(dir)?;
            }
            study
        }
    };
    let mut candidates = candidates();
    if let Some(only) = &only {
        candidates.retain(|c| only.contains(&c.id));
        ensure(!candidates.is_empty(), "no candidate of that name")?;
    }
    // `--fields none` runs only the pairs.
    let mut chosen = Field::ALL.to_vec();
    if let Some(fields) = &fields {
        chosen.retain(|f| fields.iter().any(|n| n == f.id()));
    }
    let mut report = if chosen.is_empty() {
        json!({"tool": "cargo xtask lr-align tone-study", "reduction": study.reduction})
    } else {
        study.run(&candidates, &chosen, &out)?
    };
    if let Some(folder) = &pairs_folder {
        let pairs = Pairs::open(&study, folder, cache.as_deref())?;
        report["pairs"] = pairs.run(&study, &candidates, &out)?;
    }
    write_json(&out.join("tone-study.json"), &report)?;
    fs::write(out.join("study.md"), page(&study, &report))?;
    println!("wrote {}", out.join("study.md").display());
    Ok(())
}

// -------------------------------------------------------------------------------------------
// Fields and candidates
// -------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Field {
    Highlights,
    Shadows,
    Whites,
    Blacks,
}

impl Field {
    const ALL: [Field; 4] = [Self::Highlights, Self::Shadows, Self::Whites, Self::Blacks];

    fn id(self) -> &'static str {
        match self {
            Self::Highlights => "highlights",
            Self::Shadows => "shadows",
            Self::Whites => "whites",
            Self::Blacks => "blacks",
        }
    }

    fn params(self, strength: f64) -> ToneParams {
        let mut p = ToneParams::NEUTRAL;
        match self {
            Self::Highlights => p.highlights = strength,
            Self::Shadows => p.shadows = strength,
            Self::Whites => p.whites = strength,
            Self::Blacks => p.blacks = strength,
        }
        p
    }

    /// The strengths a candidate is rendered at: Highlights and Shadows to ±150, where their
    /// stage stays monotone (A10), Whites and Blacks to ±200.
    fn grid(self) -> Vec<f64> {
        let end = match self {
            Self::Highlights | Self::Shadows => 150,
            Self::Whites | Self::Blacks => 200,
        };
        (-end / 10..=end / 10).map(|k| f64::from(k * 10)).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    /// On scene values, before a photograph's Look: where Basic's tone unit runs today.
    BeforeLook,
    /// On scene values lowered by this many stops and raised back after: the curve anchored
    /// lower on the scene scale, as if a photograph's mid-grey sat lower in the editor's working
    /// space than Luxforge's scene scale puts it. A chart (no Look) is unaffected.
    BeforeLookShifted(f64),
    /// On scene values divided by the photograph's own white, `factor` times the `quantile` of
    /// its scene luminance, and multiplied back after: the curve spans the scene's range, which
    /// on RAW runs past display white. A chart (no Look) is unaffected.
    BeforeLookWhite { quantile: f64, factor: f64 },
    /// On rendered values, after the Look.
    AfterLook,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Operator {
    Global(Reconstruction),
    Local(LocalConfig),
}

#[derive(Clone, Debug)]
struct Candidate {
    id: String,
    describes: String,
    placement: Placement,
    operator: Operator,
    /// The quantiles of the acted-on frame's encoded luminance the curve's black and white are
    /// read at (`None` keeps 0 or 1): a curve adapted to the picture's own range.
    adapt: (Option<f64>, Option<f64>),
    /// Whether `adapt` applies to a photograph acted on before its Look as well as to rendered
    /// input; when false a scene-referred photograph keeps the unit levels.
    adapt_scene: bool,
    /// The fields acting on scene values by the placement and operator above; the rest act after
    /// the Look through the global curve. Rendered only on round 2's pairs: on one field at a
    /// time a split candidate is the placement its field falls on.
    split: Option<Split>,
}

/// Which of Contrast, Highlights, Shadows, Whites and Blacks act on scene values.
#[derive(Clone, Copy, Debug)]
struct Split {
    contrast: bool,
    highlights: bool,
    shadows: bool,
    whites: bool,
    blacks: bool,
}

impl Split {
    /// `params` divided into the scene part and the rendered part.
    fn divide(self, params: ToneParams) -> (ToneParams, ToneParams) {
        let mut scene = ToneParams::NEUTRAL;
        let mut rendered = ToneParams::NEUTRAL;
        let mut put = |on_scene: bool, value: f64, field: fn(&mut ToneParams) -> &mut f64| {
            *field(if on_scene { &mut scene } else { &mut rendered }) = value;
        };
        put(self.contrast, params.contrast, |p| &mut p.contrast);
        put(self.highlights, params.highlights, |p| &mut p.highlights);
        put(self.shadows, params.shadows, |p| &mut p.shadows);
        put(self.whites, params.whites, |p| &mut p.whites);
        put(self.blacks, params.blacks, |p| &mut p.blacks);
        (scene, rendered)
    }
}

fn candidates() -> Vec<Candidate> {
    use Placement::*;
    use Reconstruction::*;
    let local = |reduction, radius, eps, detail| {
        Operator::Local(LocalConfig {
            smoother: Smoother::Guided {
                reduction,
                radius,
                eps,
            },
            detail,
            reconstruction: LuminanceRatio,
        })
    };
    let mut out = vec![
        Candidate {
            id: "global-pre".into(),
            describes: "today's global curve, before the Look".into(),
            placement: BeforeLook,
            operator: Operator::Global(LuminanceRatio),
            adapt: (None, None),
            split: None,
            adapt_scene: true,
        },
        Candidate {
            id: "global-pre-shift1".into(),
            describes: "today's global curve before the Look, anchored a stop lower".into(),
            placement: BeforeLookShifted(1.0),
            operator: Operator::Global(LuminanceRatio),
            adapt: (None, None),
            split: None,
            adapt_scene: true,
        },
        Candidate {
            id: "global-post".into(),
            describes: "the global curve after the Look".into(),
            placement: AfterLook,
            operator: Operator::Global(LuminanceRatio),
            adapt: (None, None),
            split: None,
            adapt_scene: true,
        },
        Candidate {
            id: "global-post-oklab".into(),
            describes: "the global curve after the Look, Oklab chroma kept".into(),
            placement: AfterLook,
            operator: Operator::Global(OklabChroma),
            adapt: (None, None),
            split: None,
            adapt_scene: true,
        },
        Candidate {
            id: "box-post".into(),
            describes: "control: a box base of 5% of the long side, after the Look".into(),
            placement: AfterLook,
            operator: Operator::Local(LocalConfig {
                smoother: Smoother::Box {
                    reduction: 4,
                    radius: 0.05,
                },
                detail: 1.0,
                reconstruction: LuminanceRatio,
            }),
            adapt: (None, None),
            split: None,
            adapt_scene: true,
        },
    ];
    for (radius, eps) in [
        (0.016, 0.01),
        (0.05, 0.01),
        (0.05, 0.04),
        (0.1, 0.04),
        (0.1, 0.16),
        (0.2, 0.16),
    ] {
        out.push(Candidate {
            id: format!("guided-post-r{radius}-e{eps}"),
            describes: format!(
                "guided base at {}% of the long side, eps {eps}, after the Look",
                radius * 100.0
            ),
            placement: AfterLook,
            operator: local(4, radius, eps, 1.0),
            adapt: (None, None),
            split: None,
            adapt_scene: true,
        });
        out.push(Candidate {
            id: format!("guided-pre-r{radius}-e{eps}"),
            describes: format!(
                "guided base at {}% of the long side, eps {eps}, before the Look",
                radius * 100.0
            ),
            placement: BeforeLook,
            operator: local(4, radius, eps, 1.0),
            adapt: (None, None),
            split: None,
            adapt_scene: true,
        });
    }
    // The curve over the scene's own white on RAW.
    for (quantile, factor) in [
        (0.999, 1.0),
        (0.999, 2.0),
        (0.999, 3.0),
        (0.999, 4.0),
        (0.99, 3.0),
    ] {
        for (adapt, name) in [((None, None), ""), ((None, Some(0.999)), "-w99.9")] {
            out.push(Candidate {
                id: format!("white-pre-q{}-c{factor}{name}", quantile * 100.0),
                describes: format!(
                    "the global curve before the Look over {factor} times the {} quantile of the \
                     photograph's scene luminance{}",
                    quantile,
                    if name.is_empty() {
                        ""
                    } else {
                        ", within its 99.9% white"
                    }
                ),
                placement: BeforeLookWhite { quantile, factor },
                operator: Operator::Global(LuminanceRatio),
                adapt,
                split: None,
                adapt_scene: true,
            });
        }
    }
    // The scene-white curve with an edge-aware base: the fast guided filter's coefficients on a
    // grid of at most 256 samples (a whole-frame estimate, no halo), or the reduced guided base.
    for (factor, smoother, name) in [
        (
            3.0,
            Smoother::FastGuided {
                long: 256,
                radius: 0.05,
                eps: 0.04,
            },
            "fg256-r0.05-e0.04",
        ),
        (
            3.0,
            Smoother::FastGuided {
                long: 256,
                radius: 0.1,
                eps: 0.04,
            },
            "fg256-r0.1-e0.04",
        ),
        (
            3.0,
            Smoother::FastGuided {
                long: 256,
                radius: 0.1,
                eps: 0.01,
            },
            "fg256-r0.1-e0.01",
        ),
        (
            3.0,
            Smoother::FastGuided {
                long: 128,
                radius: 0.1,
                eps: 0.04,
            },
            "fg128-r0.1-e0.04",
        ),
        (
            3.0,
            Smoother::FastGuided {
                long: 256,
                radius: 0.2,
                eps: 0.04,
            },
            "fg256-r0.2-e0.04",
        ),
        (
            3.0,
            Smoother::Guided {
                reduction: 4,
                radius: 0.016,
                eps: 0.01,
            },
            "guided-r0.016-e0.01",
        ),
        (
            4.0,
            Smoother::FastGuided {
                long: 256,
                radius: 0.1,
                eps: 0.04,
            },
            "fg256-r0.1-e0.04",
        ),
    ] {
        out.push(Candidate {
            id: format!("white-pre-q99.9-c{factor}-{name}"),
            describes: format!(
                "the curve before the Look over {factor} times the 0.999 quantile of the \
                 photograph's scene luminance, on an edge-aware base ({name})"
            ),
            placement: BeforeLookWhite {
                quantile: 0.999,
                factor,
            },
            operator: Operator::Local(LocalConfig {
                smoother,
                detail: 1.0,
                reconstruction: LuminanceRatio,
            }),
            adapt: (None, None),
            split: None,
            adapt_scene: true,
        });
    }
    // One rule for both kinds of input: on scene-referred input the curve spans `factor` times
    // the scene's 0.999 luminance quantile, on rendered input the picture's own 0.999 encoded
    // white.
    for factor in [2.0, 3.0, 4.0] {
        out.push(Candidate {
            id: format!("unified-c{factor}-w99.9"),
            describes: format!(
                "the curve over {factor} times the photograph's 0.999 scene-luminance quantile \
                 before the Look; a chart's curve within its own 0.999 encoded white"
            ),
            placement: BeforeLookWhite {
                quantile: 0.999,
                factor,
            },
            operator: Operator::Global(LuminanceRatio),
            adapt: (None, Some(0.999)),
            split: None,
            adapt_scene: false,
        });
    }
    // The same rule with a fixed anchor instead of the scene's own white: the curve's white
    // `stops` above scene 1.0 on every photograph (1.5 stops is about where the Look renders
    // white, `x_max`).
    for stops in [1.5, 2.0] {
        out.push(Candidate {
            id: format!("global-pre-shift{stops}"),
            describes: format!(
                "today's global curve before the Look, anchored {stops} stops lower"
            ),
            placement: BeforeLookShifted(stops),
            operator: Operator::Global(LuminanceRatio),
            adapt: (None, None),
            split: None,
            adapt_scene: true,
        });
        out.push(Candidate {
            id: format!("unified-shift{stops}-w99.9"),
            describes: format!(
                "the curve anchored {stops} stops lower before the Look; a chart's curve within \
                 its own 0.999 encoded white"
            ),
            placement: BeforeLookShifted(stops),
            operator: Operator::Global(LuminanceRatio),
            adapt: (None, Some(0.999)),
            split: None,
            adapt_scene: false,
        });
    }
    // Split: some fields on the scene-white curve before the Look, the rest after it.
    let fg = Smoother::FastGuided {
        long: 256,
        radius: 0.1,
        eps: 0.04,
    };
    let white = |factor| BeforeLookWhite {
        quantile: 0.999,
        factor,
    };
    for (name, split, smoother, placement, anchor) in [
        (
            "hs",
            (false, true, true, false, false),
            None,
            white(3.0),
            "c3",
        ),
        (
            "hsw",
            (false, true, true, true, false),
            None,
            white(3.0),
            "c3",
        ),
        (
            "hs",
            (false, true, true, false, false),
            Some(fg),
            white(3.0),
            "c3",
        ),
        (
            "hs",
            (false, true, true, false, false),
            None,
            white(4.0),
            "c4",
        ),
        (
            "hs",
            (false, true, true, false, false),
            None,
            BeforeLookShifted(1.5),
            "shift1.5",
        ),
        (
            "hsw",
            (false, true, true, true, false),
            None,
            BeforeLookShifted(1.5),
            "shift1.5",
        ),
    ] {
        let (contrast, highlights, shadows, whites, blacks) = split;
        out.push(Candidate {
            id: format!(
                "split-{name}-{anchor}{}",
                if smoother.is_some() {
                    "-fg256-r0.1-e0.04"
                } else {
                    ""
                }
            ),
            describes: format!(
                "{name} on the curve before the Look ({anchor}){}, the rest after the Look",
                if smoother.is_some() {
                    " on a fast guided base"
                } else {
                    ""
                }
            ),
            placement,
            operator: match smoother {
                Some(smoother) => Operator::Local(LocalConfig {
                    smoother,
                    detail: 1.0,
                    reconstruction: LuminanceRatio,
                }),
                None => Operator::Global(LuminanceRatio),
            },
            adapt: (None, None),
            split: Some(Split {
                contrast,
                highlights,
                shadows,
                whites,
                blacks,
            }),
            adapt_scene: true,
        });
    }
    // The curve within the picture's own black and white points.
    for (placement, at) in [(BeforeLook, "pre"), (AfterLook, "post")] {
        for (black, white) in [
            (Some(0.001), Some(0.999)),
            (Some(0.01), Some(0.99)),
            (None, Some(0.999)),
            (Some(0.001), None),
        ] {
            let name = |q: Option<f64>, end: &str| {
                q.map(|q| format!("-{end}{}", q * 100.0))
                    .unwrap_or_default()
            };
            out.push(Candidate {
                id: format!("adapt-{at}{}{}", name(black, "b"), name(white, "w")),
                describes: format!(
                    "the global curve within the picture's black at {:?} and white at {:?} \
                     (quantiles of encoded luminance), {}",
                    black,
                    white,
                    if placement == BeforeLook {
                        "before the Look"
                    } else {
                        "after the Look"
                    }
                ),
                placement,
                operator: Operator::Global(LuminanceRatio),
                adapt: (black, white),
                split: None,
                adapt_scene: true,
            });
        }
    }
    out
}

// -------------------------------------------------------------------------------------------
// Subjects and Lightroom's exports
// -------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Charts,
    Photos,
}

struct Subject {
    id: String,
    class: Class,
    layout: Value,
    /// A chart's target in linear light; a photograph's scene-linear planes at the study's scale.
    input: Frame,
    look: Option<Look>,
    /// The Look layer's payload, which the cache keeps.
    look_payload: Value,
    /// The picture at neutral before the output clamp: a chart's input, a photograph's Look over
    /// its input.
    rendered: Frame,
}

/// One export reduced to what the study reads.
#[derive(Clone, Default)]
struct Export {
    samples: Vec<[f64; 3]>,
    /// A photograph's `L*` band energy at [`BANDS`].
    energy: Vec<f64>,
    /// A photograph's cells' `L*`, for its tone locality.
    lightness: Vec<f64>,
    thumb: Option<image::RgbImage>,
}

struct Study {
    reduction: usize,
    subjects: Vec<Subject>,
    /// Lightroom's exports by `<subject>__<family>__<label>`, and `<subject>__neutral`.
    lr: BTreeMap<String, Export>,
    /// The editor's own neutral render of each photograph against the study's, median and 95th
    /// percentile CIEDE2000 over its cells: what reducing before the Look costs.
    sanity: BTreeMap<String, (f64, f64)>,
}

fn lab_l(rgb: [f64; 3]) -> f64 {
    la::lab(rgb)[0]
}

fn clamped(frame: &Frame) -> Frame {
    Frame {
        width: frame.width,
        height: frame.height,
        rgb: frame
            .rgb
            .iter()
            .map(|p| p.map(|c| c.clamp(0.0, 1.0)))
            .collect(),
    }
}

/// A frame of 16-bit sRGB codes in linear light, reduced by `factor` per axis (whole blocks).
fn from_codes(width: usize, height: usize, rgb: &[u16], factor: usize, gain: f64) -> Frame {
    let (w, h) = (width / factor, height / factor);
    let mut out = vec![[0.0; 3]; w * h];
    let table: Vec<f64> = (0..=65535u32)
        .map(|c| srgb::decode_encoded(f64::from(c) / 65535.0) * gain)
        .collect();
    let n = (factor * factor) as f64;
    for j in 0..h {
        for i in 0..w {
            let mut sum = [0.0; 3];
            for y in j * factor..(j + 1) * factor {
                for x in i * factor..(i + 1) * factor {
                    let at = (y * width + x) * 3;
                    for c in 0..3 {
                        sum[c] += table[usize::from(rgb[at + c])];
                    }
                }
            }
            out[j * w + i] = sum.map(|s| s / n);
        }
    }
    Frame {
        width: w,
        height: h,
        rgb: out,
    }
}

fn patch_means(frame: &Frame, layout: &Value) -> Vec<[f64; 3]> {
    let size = layout["patch"].as_u64().unwrap_or(0) as usize;
    let inset = layout["inset"].as_u64().unwrap_or(0) as usize;
    layout["patches"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            let (x0, y0) = (
                p["x"].as_u64().unwrap_or(0) as usize + inset,
                p["y"].as_u64().unwrap_or(0) as usize + inset,
            );
            let side = size - 2 * inset;
            let mut sum = [0.0; 3];
            for y in y0..y0 + side {
                for x in x0..x0 + side {
                    let v = frame.rgb[y * frame.width + x];
                    for c in 0..3 {
                        sum[c] += v[c];
                    }
                }
            }
            sum.map(|s| s / (side * side) as f64)
        })
        .collect()
}

fn normalized_cells(frame: &Frame) -> Vec<[f64; 3]> {
    let columns = PHOTO_COLUMNS;
    let rows =
        ((columns as f64 * frame.height as f64 / frame.width as f64).round() as usize).max(1);
    let mut out = Vec::with_capacity(columns * rows);
    for row in 0..rows {
        let (y0, y1) = (
            row * frame.height / rows,
            ((row + 1) * frame.height / rows).max(row * frame.height / rows + 1),
        );
        for column in 0..columns {
            let (x0, x1) = (
                column * frame.width / columns,
                ((column + 1) * frame.width / columns).max(column * frame.width / columns + 1),
            );
            let mut sum = [0.0; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    let v = frame.rgb[y * frame.width + x];
                    for c in 0..3 {
                        sum[c] += v[c];
                    }
                }
            }
            out.push(sum.map(|s| s / ((y1 - y0) * (x1 - x0)) as f64));
        }
    }
    out
}

fn thumbnail(frame: &Frame) -> image::RgbImage {
    let factor = frame.width.max(frame.height).div_ceil(THUMB).max(1);
    let (w, h) = (frame.width / factor, frame.height / factor);
    image::RgbImage::from_fn(w as u32, h as u32, |x, y| {
        let mut sum = [0.0; 3];
        for yy in 0..factor {
            for xx in 0..factor {
                let v =
                    frame.rgb[(y as usize * factor + yy) * frame.width + x as usize * factor + xx];
                for c in 0..3 {
                    sum[c] += v[c];
                }
            }
        }
        let n = (factor * factor) as f64;
        image::Rgb(sum.map(|s| srgb::code(s / n)))
    })
}

impl Subject {
    /// What the study reads of a frame of this subject at the output (clamped).
    fn reduce(&self, frame: &Frame, thumb: bool) -> Export {
        let frame = clamped(frame);
        match self.class {
            Class::Charts => Export {
                samples: patch_means(&frame, &self.layout),
                thumb: thumb.then(|| thumbnail(&frame)),
                ..Export::default()
            },
            Class::Photos => {
                let samples = normalized_cells(&frame);
                let plane: Vec<f64> = frame.rgb.iter().map(|p| lab_l(*p)).collect();
                Export {
                    lightness: samples.iter().map(|s| lab_l(*s)).collect(),
                    samples,
                    energy: la::band_energy(&plane, frame.width, &BANDS),
                    thumb: thumb.then(|| thumbnail(&frame)),
                }
            }
        }
    }
}

fn look_payload(base: &Recipe) -> Value {
    base.layers
        .iter()
        .find(|l| l.effect_id == LOOK_EFFECT)
        .map(|l| l.payload.clone())
        .unwrap_or(Value::Null)
}

fn look_of(p: &Value) -> Result<Option<Look>> {
    if p.is_null() || p["look"] == "neutral" {
        return Ok(None);
    }
    let knots: Vec<[f64; 2]> = p["tone"]
        .as_array()
        .ok_or("a Look without tone knots")?
        .iter()
        .map(|k| {
            [
                k[0].as_f64().unwrap_or(f64::NAN),
                k[1].as_f64().unwrap_or(f64::NAN),
            ]
        })
        .collect();
    Ok(Some(Look {
        tone: LookTone::new(&knots),
        chroma: p["chroma"].as_f64().ok_or("a Look without chroma")?,
        knee: p["knee"].as_f64().ok_or("a Look without a knee")?,
        amount: p["amount"].as_f64().unwrap_or(100.0),
    }))
}

fn apply_look(look: Option<&Look>, frame: &Frame) -> Frame {
    match look {
        None => frame.clone(),
        Some(look) => Frame {
            width: frame.width,
            height: frame.height,
            rgb: frame.rgb.iter().map(|p| look_pixel(look, *p)).collect(),
        },
    }
}

/// `work` over `items` on `measure`'s scoped pool, one worker per core, results in item order.
fn parallel<T: Sync, R: Send>(
    items: &[T],
    work: impl Fn(&T) -> Result<R> + Sync,
) -> Result<Vec<R>> {
    let workers = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    super::measure::parallel(items, workers, |item| work(item).map_err(|e| e.to_string()))
}

impl Study {
    fn read(charts: &Path, photographs: &Path, out: &Path, reduction: usize) -> Result<Self> {
        let registry = ModuleRegistry::builtin();
        let context = RenderContext::default();
        let mut subjects = Vec::new();
        let mut sanity = BTreeMap::new();
        let chart_manifest = read_json(&charts.join("manifest.json"))?;
        for id in CHART_SUBJECTS {
            let target = chart_manifest["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|t| t["id"] == id)
                .ok_or(format!("round 1 has no target {id}"))?;
            let source = open_source(&charts.join(target["file"].as_str().unwrap_or_default()))?;
            let rgb: Vec<[f64; 3]> = source
                .rgba
                .chunks_exact(4)
                .map(|p| [srgb::decode(p[0]), srgb::decode(p[1]), srgb::decode(p[2])])
                .collect();
            let input = Frame {
                width: source.width as usize,
                height: source.height as usize,
                rgb,
            };
            subjects.push(Subject {
                id: id.into(),
                class: Class::Charts,
                layout: target["layout"].clone(),
                rendered: input.clone(),
                input,
                look: None,
                look_payload: Value::Null,
            });
        }
        let photo_manifest = read_json(&photographs.join("manifest.json"))?;
        for photograph in photo_manifest["photographs"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let id = photograph["id"].as_str().unwrap_or_default().to_owned();
            let extension = photograph["extension"].as_str().unwrap_or_default();
            let copy = photographs
                .join(super::LIGHTROOM)
                .join(&id)
                .join(format!("{id}__{NEUTRAL}.{extension}"));
            let photo = Photo::open(
                &copy,
                photograph["sha256"].as_str().unwrap_or_default(),
                &out.join("catalogs").join(&id),
            )?;
            let developed = photo.develop(&Development::AsShot)?;
            let payload = look_payload(&developed.base);
            let look = look_of(&payload)?;
            let mut layers: Vec<Layer> = developed
                .base
                .layers
                .iter()
                .filter(|l| l.effect_id != LOOK_EFFECT)
                .cloned()
                .collect();
            let at = registry.insertion_index_for(&layers, BASIC_EFFECT);
            layers.insert(at, Layer::new(BASIC_EFFECT, json!({"exposure": SCENE_EV})));
            let scene_recipe = Recipe {
                format: RECIPE_FORMAT,
                layers,
                ..Recipe::default()
            };
            let study16 = |recipe: &Recipe| -> Result<luxforge_core::StudyFrame> {
                Ok(render(
                    &registry,
                    RenderSource::Linear {
                        image: &developed.image,
                        settings: LinearSettings::default(),
                    },
                    recipe,
                    RenderOptions::default(),
                    &context,
                )
                .and_then(|r| r.study_frame16())?)
            };
            let scene = study16(&scene_recipe)?;
            let input = from_codes(
                scene.width as usize,
                scene.height as usize,
                &scene.rgb,
                reduction,
                2f64.powf(-SCENE_EV),
            );
            drop(scene);
            let editor = study16(&developed.base)?;
            let editor = from_codes(
                editor.width as usize,
                editor.height as usize,
                &editor.rgb,
                reduction,
                1.0,
            );
            let rendered = apply_look(look.as_ref(), &input);
            let ours = normalized_cells(&clamped(&rendered));
            let theirs = normalized_cells(&editor);
            let differences: Vec<f64> = ours
                .iter()
                .zip(&theirs)
                .map(|(a, b)| ciede2000(la::lab(*a), la::lab(*b)))
                .collect();
            sanity.insert(
                id.clone(),
                (la::median(&differences), la::quantile(&differences, 0.95)),
            );
            println!(
                "photograph {id}: {} x {} at 1/{reduction}, neutral against the editor's {:.3} median",
                input.width,
                input.height,
                la::median(&differences)
            );
            subjects.push(Subject {
                id,
                class: Class::Photos,
                layout: Value::Null,
                input,
                look,
                look_payload: payload,
                rendered,
            });
        }
        // Lightroom's exports: the neutral and every variant of the four fields.
        let mut reads: Vec<(usize, String, PathBuf)> = Vec::new();
        for (si, s) in subjects.iter().enumerate() {
            let (folder, labels, suffix) = match s.class {
                Class::Charts => (charts.join(super::EXPORTS), &CHART_LABELS[..], ""),
                Class::Photos => (
                    photographs.join(super::EXPORTS),
                    &PHOTO_LABELS[..],
                    "-photographs",
                ),
            };
            let neutral = format!("{}__{NEUTRAL}", s.id);
            reads.push((si, neutral.clone(), folder.join(format!("{neutral}.tif"))));
            for field in Field::ALL {
                for label in labels {
                    let id = format!("{}__{}{suffix}__{label}", s.id, field.id());
                    reads.push((si, id.clone(), folder.join(format!("{id}.tif"))));
                }
            }
        }
        let exports = parallel(&reads, |(si, id, path)| {
            let subject = &subjects[*si];
            let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let tiff = crate::tiff::read(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            drop(bytes);
            let factor = match subject.class {
                Class::Charts => 1,
                Class::Photos => reduction,
            };
            let frame = from_codes(
                tiff.width as usize,
                tiff.height as usize,
                &tiff.rgb,
                factor,
                1.0,
            );
            ensure(
                (frame.width, frame.height) == (subject.input.width, subject.input.height),
                format!("{id}: not the subject's size"),
            )?;
            Ok((id.clone(), subject.reduce(&frame, true)))
        })?;
        Ok(Self {
            reduction,
            subjects,
            lr: exports.into_iter().collect(),
            sanity,
        })
    }

    fn save(&self, dir: &Path) -> Result {
        fs::create_dir_all(dir.join("thumbs"))?;
        let mut subjects = Vec::new();
        for s in &self.subjects {
            let mut bytes = Vec::with_capacity(s.input.rgb.len() * 12);
            for p in &s.input.rgb {
                for c in p {
                    bytes.extend_from_slice(&(*c as f32).to_le_bytes());
                }
            }
            fs::write(dir.join(format!("{}.f32", s.id)), bytes)?;
            subjects.push(json!({
                "id": s.id, "photo": s.class == Class::Photos, "layout": s.layout,
                "width": s.input.width, "height": s.input.height, "look": s.look_payload,
            }));
        }
        let mut lr = serde_json::Map::new();
        for (id, e) in &self.lr {
            if let Some(t) = &e.thumb {
                t.save(dir.join("thumbs").join(format!("{id}.png")))?;
            }
            lr.insert(
                id.clone(),
                json!({"samples": e.samples, "energy": e.energy, "lightness": e.lightness}),
            );
        }
        write_json(
            &dir.join("study.json"),
            &json!({
                "reduction": self.reduction, "subjects": subjects, "lr": lr,
                "sanity": self.sanity,
            }),
        )
    }

    fn load(dir: &Path) -> Result<Self> {
        let v = read_json(&dir.join("study.json"))?;
        let mut subjects = Vec::new();
        for s in v["subjects"].as_array().into_iter().flatten() {
            let id = s["id"].as_str().unwrap_or_default().to_owned();
            let bytes = fs::read(dir.join(format!("{id}.f32")))?;
            let values: Vec<f64> = bytes
                .chunks_exact(4)
                .map(|b| f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])))
                .collect();
            let input = Frame {
                width: s["width"].as_u64().unwrap_or(0) as usize,
                height: s["height"].as_u64().unwrap_or(0) as usize,
                rgb: values.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
            };
            let look = look_of(&s["look"])?;
            let rendered = apply_look(look.as_ref(), &input);
            subjects.push(Subject {
                id,
                class: if s["photo"] == true {
                    Class::Photos
                } else {
                    Class::Charts
                },
                layout: s["layout"].clone(),
                input,
                look,
                look_payload: s["look"].clone(),
                rendered,
            });
        }
        let mut lr = BTreeMap::new();
        for (id, e) in v["lr"].as_object().into_iter().flatten() {
            let thumb = image::open(dir.join("thumbs").join(format!("{id}.png")))
                .ok()
                .map(|i| i.to_rgb8());
            lr.insert(
                id.clone(),
                Export {
                    samples: serde_json::from_value(e["samples"].clone())?,
                    energy: serde_json::from_value(e["energy"].clone())?,
                    lightness: serde_json::from_value(e["lightness"].clone())?,
                    thumb,
                },
            );
        }
        Ok(Self {
            reduction: v["reduction"].as_u64().unwrap_or(4) as usize,
            subjects,
            lr,
            sanity: serde_json::from_value(v["sanity"].clone())?,
        })
    }
}

// -------------------------------------------------------------------------------------------
// Evaluation and fit
// -------------------------------------------------------------------------------------------

/// A candidate's prepared input on one subject: the frame it acts on, its encoded luminance and,
/// for a local operator, its base.
struct Prepared {
    frame: Frame,
    encoded: Vec<f64>,
    base: Vec<f64>,
    levels: Levels,
    /// The stops the photograph's scene values were lowered by before the curve.
    shift: f64,
}

/// `frame` scaled by `gain`.
fn scaled(frame: &Frame, gain: f64) -> Frame {
    Frame {
        width: frame.width,
        height: frame.height,
        rgb: frame.rgb.iter().map(|p| p.map(|c| c * gain)).collect(),
    }
}

/// The stops a placement lowers a photograph's scene values by before the curve.
fn shift_of(placement: Placement, scene: &Frame) -> f64 {
    match placement {
        Placement::BeforeLookShifted(stops) => stops,
        Placement::BeforeLookWhite { quantile, factor } => {
            let mut l: Vec<f64> = scene.rgb.iter().map(|p| luminance(*p)).collect();
            l.sort_by(f64::total_cmp);
            let white = l[((l.len() - 1) as f64 * quantile).round() as usize].max(1e-6);
            (factor * white).log2()
        }
        _ => 0.0,
    }
}

fn prepare(subject: &Subject, candidate: &Candidate) -> Prepared {
    let frame = match (subject.class, candidate.placement) {
        (Class::Photos, Placement::BeforeLook) => subject.input.clone(),
        (
            Class::Photos,
            p @ (Placement::BeforeLookShifted(_) | Placement::BeforeLookWhite { .. }),
        ) => scaled(&subject.input, 2f64.powf(-shift_of(p, &subject.input))),
        _ => subject.rendered.clone(),
    };
    let encoded = frame.encoded_luminance();
    let base = match candidate.operator {
        Operator::Local(config) => lt::smooth(&encoded, frame.width, frame.height, config.smoother),
        Operator::Global(_) => Vec::new(),
    };
    let scene = subject.class == Class::Photos && candidate.placement != Placement::AfterLook;
    let levels = if scene && !candidate.adapt_scene {
        Levels::UNIT
    } else {
        Levels::of(&encoded, candidate.adapt.0, candidate.adapt.1)
    };
    let shift = match subject.class {
        Class::Photos => shift_of(candidate.placement, &subject.input),
        Class::Charts => 0.0,
    };
    Prepared {
        frame,
        encoded,
        base,
        levels,
        shift,
    }
}

/// The subject's picture under `candidate` at `params`, before the output clamp.
fn evaluate(
    subject: &Subject,
    candidate: &Candidate,
    prepared: &Prepared,
    params: ToneParams,
) -> Frame {
    let toned = match candidate.operator {
        Operator::Global(how) => lt::global_within(&prepared.frame, params, how, prepared.levels),
        Operator::Local(config) => lt::local_within(
            &prepared.frame,
            &prepared.encoded,
            &prepared.base,
            params,
            config,
            prepared.levels,
        ),
    };
    match (subject.class, candidate.placement) {
        (Class::Photos, Placement::BeforeLook) => apply_look(subject.look.as_ref(), &toned),
        (Class::Photos, Placement::BeforeLookShifted(_) | Placement::BeforeLookWhite { .. }) => {
            apply_look(
                subject.look.as_ref(),
                &scaled(&toned, 2f64.powf(prepared.shift)),
            )
        }
        _ => toned,
    }
}

/// One Lightroom value's fit for a candidate: the best strength on each class alone and the
/// joint one, with each class's residual there.
#[derive(Clone, Debug)]
struct Point {
    label: String,
    value: f64,
    charts: Option<(f64, f64)>,
    photos: Option<(f64, f64)>,
    /// The joint strength and the charts' and photographs' residuals there.
    joint: (f64, Option<f64>, Option<f64>),
}

fn labs(samples: &[[f64; 3]]) -> Vec<Lab> {
    samples.iter().map(|s| la::lab(*s)).collect()
}

/// One photograph's own fit at one value: its best strength and residual there, and its target
/// and subject index for its residual at another strength.
struct PhotoFit {
    photo: String,
    best: f64,
    residual: f64,
    target: Target,
    subject: usize,
}

/// A sample's colour at neutral and adjusted.
type NeutralAndAdjusted = ([f64; 3], [f64; 3]);

/// The distance curve over the grid and its refinement around the best, as `(strength, distance)`.
fn best_of(grid: &[f64], distance: impl Fn(usize, Option<(usize, f64)>) -> f64) -> (f64, f64) {
    let coarse: Vec<f64> = (0..grid.len()).map(|i| distance(i, None)).collect();
    let (mut bi, mut bd) = (0, f64::INFINITY);
    for (i, d) in coarse.iter().enumerate() {
        if *d < bd {
            (bi, bd) = (i, *d);
        }
    }
    let mut best = (grid[bi], bd);
    for i in [bi.saturating_sub(1), bi] {
        if i + 1 >= grid.len() {
            continue;
        }
        for k in 1..REFINE {
            let t = k as f64 / REFINE as f64;
            let d = distance(i, Some((i + 1, t)));
            if d < best.1 {
                best = (grid[i] + t * (grid[i + 1] - grid[i]), d);
            }
        }
    }
    best
}

impl Study {
    fn class_subjects(&self, class: Class) -> Vec<usize> {
        (0..self.subjects.len())
            .filter(|i| self.subjects[*i].class == class)
            .collect()
    }

    fn family(&self, field: Field, class: Class) -> String {
        match class {
            Class::Charts => field.id().into(),
            Class::Photos => format!("{}-photographs", field.id()),
        }
    }

    /// Lightroom's targets for `members` of a class: per label, its value and the colours it
    /// carries onto Luxforge's neutral.
    fn targets(
        &self,
        field: Field,
        class: Class,
        members: &[usize],
        neutral: &[Lab],
    ) -> Vec<(String, f64, Target)> {
        let labels: &[&str] = match class {
            Class::Charts => &CHART_LABELS,
            Class::Photos => &PHOTO_LABELS,
        };
        let family = self.family(field, class);
        labels
            .iter()
            .map(|label| {
                let mut carried = Vec::new();
                for &si in members {
                    let s = &self.subjects[si];
                    let ours = s.reduce(&s.rendered, false);
                    let theirs = &self.lr[&format!("{}__{NEUTRAL}", s.id)];
                    let adjusted = &self.lr[&format!("{}__{family}__{label}", s.id)];
                    for ((n, r), a) in ours
                        .samples
                        .iter()
                        .zip(&theirs.samples)
                        .zip(&adjusted.samples)
                    {
                        carried.push(la::lab(la::carried(*n, *r, *a)));
                    }
                }
                let value: f64 = label.trim_start_matches('+').parse().unwrap_or(0.0);
                (label.to_string(), value, Target::new(neutral, carried))
            })
            .collect()
    }

    fn run(&self, candidates: &[Candidate], fields: &[Field], out: &Path) -> Result<Value> {
        let mut results = Vec::new();
        for candidate in candidates.iter().filter(|c| c.split.is_none()) {
            let prepared: Vec<Prepared> =
                parallel(&self.subjects.iter().collect::<Vec<_>>(), |s| {
                    Ok(prepare(s, candidate))
                })?;
            for &field in fields {
                let started = std::time::Instant::now();
                let grid = field.grid();
                let jobs: Vec<(usize, usize)> = (0..self.subjects.len())
                    .flat_map(|si| (0..grid.len()).map(move |gi| (si, gi)))
                    .collect();
                let rendered = parallel(&jobs, |(si, gi)| {
                    let s = &self.subjects[*si];
                    let frame = evaluate(s, candidate, &prepared[*si], field.params(grid[*gi]));
                    Ok(labs(&s.reduce(&frame, false).samples))
                })?;
                // Per class: the neutral, each grid strength's colours and Lightroom's targets.
                let mut per_class = BTreeMap::new();
                for class in [Class::Charts, Class::Photos] {
                    let members = self.class_subjects(class);
                    let neutral: Vec<Lab> = members
                        .iter()
                        .flat_map(|si| {
                            let s = &self.subjects[*si];
                            labs(&s.reduce(&s.rendered, false).samples)
                        })
                        .collect();
                    let strengths: Vec<Vec<Lab>> = (0..grid.len())
                        .map(|gi| {
                            members
                                .iter()
                                .flat_map(|si| rendered[si * grid.len() + gi].iter().copied())
                                .collect()
                        })
                        .collect();
                    let targets = self.targets(field, class, &members, &neutral);
                    per_class.insert(class == Class::Photos, (strengths, targets));
                }
                // Each photograph's own best strength per value: whether one response per value
                // could fit every photograph, or the photographs ask for different strengths.
                // Per photograph and value: its own best strength and residual, and its target
                // and strengths for its residual at the joint strength below.
                let mut per_photo: BTreeMap<String, Vec<PhotoFit>> = BTreeMap::new();
                for si in self.class_subjects(Class::Photos) {
                    let s = &self.subjects[si];
                    let neutral = labs(&s.reduce(&s.rendered, false).samples);
                    let strengths: Vec<&Vec<Lab>> = (0..grid.len())
                        .map(|gi| &rendered[si * grid.len() + gi])
                        .collect();
                    for (label, _, target) in self.targets(field, Class::Photos, &[si], &neutral) {
                        let (best, residual) = best_of(&grid, |i, next| match next {
                            None => target.distance(strengths[i]),
                            Some((j, t)) => {
                                target.distance(&la::lerp_labs(strengths[i], strengths[j], t))
                            }
                        });
                        per_photo.entry(label).or_default().push(PhotoFit {
                            photo: s.id.clone(),
                            best,
                            residual,
                            target,
                            subject: si,
                        });
                    }
                }
                let at_strength = |si: usize, target: &Target, s: f64| {
                    let i = grid
                        .iter()
                        .rposition(|g| *g <= s)
                        .unwrap_or(0)
                        .min(grid.len() - 2);
                    let t = (s - grid[i]) / (grid[i + 1] - grid[i]);
                    target.distance(&la::lerp_labs(
                        &rendered[si * grid.len() + i],
                        &rendered[si * grid.len() + i + 1],
                        t,
                    ))
                };
                let distance =
                    |class: bool, target: &Target, i: usize, next: Option<(usize, f64)>| {
                        let strengths = &per_class[&class].0;
                        match next {
                            None => target.distance(&strengths[i]),
                            Some((j, t)) => {
                                target.distance(&la::lerp_labs(&strengths[i], &strengths[j], t))
                            }
                        }
                    };
                let mut points = Vec::new();
                for (label, value, chart_target) in &per_class[&false].1 {
                    let charts = best_of(&grid, |i, n| distance(false, chart_target, i, n));
                    let photo_target = per_class[&true]
                        .1
                        .iter()
                        .find(|(l, _, _)| l == label)
                        .map(|(_, _, t)| t);
                    let (photos, joint) = match photo_target {
                        Some(pt) => {
                            let photos = best_of(&grid, |i, n| distance(true, pt, i, n));
                            let (s, _) = best_of(&grid, |i, n| {
                                0.5 * (distance(false, chart_target, i, n)
                                    + distance(true, pt, i, n))
                            });
                            let at = |class: bool, target: &Target| {
                                // The strength's place on the grid, read through interpolation.
                                let i = grid
                                    .iter()
                                    .rposition(|g| *g <= s)
                                    .unwrap_or(0)
                                    .min(grid.len() - 2);
                                let t = (s - grid[i]) / (grid[i + 1] - grid[i]);
                                distance(class, target, i, Some((i + 1, t)))
                            };
                            (
                                Some(photos),
                                (s, Some(at(false, chart_target)), Some(at(true, pt))),
                            )
                        }
                        None => (None, (charts.0, Some(charts.1), None)),
                    };
                    points.push(Point {
                        label: label.clone(),
                        value: *value,
                        charts: Some(charts),
                        photos,
                        joint,
                    });
                }
                let measures = self.measures(candidate, &prepared, field, &points, out)?;
                let worst = points
                    .iter()
                    .filter(|p| p.value.abs() >= A8_FROM)
                    .flat_map(|p| [p.joint.1, p.joint.2])
                    .flatten()
                    .fold(0.0, f64::max);
                println!(
                    "{} {}: worst joint residual {worst:.2} ({:.1} s)",
                    candidate.id,
                    field.id(),
                    started.elapsed().as_secs_f64()
                );
                results.push(json!({
                    "levels": self.subjects.iter().zip(&prepared).map(|(s, p)| json!({
                        "subject": s.id, "black": p.levels.black, "white": p.levels.white,
                        "shift": p.shift,
                    })).collect::<Vec<_>>(),
                    "candidate": candidate.id, "describes": candidate.describes,
                    "field": field.id(), "worst": worst, "passes": worst <= A8,
                    "points": points.iter().map(|p| json!({
                        "label": p.label, "value": p.value,
                        "charts": p.charts.map(|(s, d)| json!({"strength": s, "residual": d})),
                        "photos": p.photos.map(|(s, d)| json!({"strength": s, "residual": d})),
                        "joint": {"strength": p.joint.0, "charts": p.joint.1, "photos": p.joint.2},
                        "per_photo": per_photo.get(&p.label).map(|v| v.iter().map(|f| json!({
                            "photo": f.photo, "strength": f.best, "residual": f.residual,
                            "at_joint": at_strength(f.subject, &f.target, p.joint.0),
                        })).collect::<Vec<_>>()),
                    })).collect::<Vec<_>>(),
                    "measures": measures,
                }));
            }
        }
        Ok(json!({
            "tool": "cargo xtask lr-align tone-study",
            "reduction": self.reduction,
            "sanity": self.sanity,
            "observations": self.observations(),
            "results": results,
        }))
    }

    /// The candidate's measures at each value's joint strength beside Lightroom's: the surround's
    /// locality, the photographs' tone locality and band energy; and a contact sheet.
    fn measures(
        &self,
        candidate: &Candidate,
        prepared: &[Prepared],
        field: Field,
        points: &[Point],
        out: &Path,
    ) -> Result<Value> {
        let jobs: Vec<(usize, usize)> = (0..points.len())
            .flat_map(|pi| (0..self.subjects.len()).map(move |si| (pi, si)))
            .collect();
        let reduced = parallel(&jobs, |(pi, si)| {
            let s = &self.subjects[*si];
            let point = &points[*pi];
            let photo = s.class == Class::Photos;
            if photo && point.photos.is_none() {
                return Ok(None);
            }
            let frame = evaluate(s, candidate, &prepared[*si], field.params(point.joint.0));
            Ok(Some(s.reduce(&frame, photo || s.id == "surround")))
        })?;
        let neutral: Vec<Export> = self
            .subjects
            .iter()
            .map(|s| s.reduce(&s.rendered, false))
            .collect();
        let mut rows = Vec::new();
        let mut sheet = Vec::new();
        for (pi, point) in points.iter().enumerate() {
            let at = |si: usize| reduced[pi * self.subjects.len() + si].as_ref();
            let surround = self.subjects.iter().position(|s| s.id == "surround");
            let locality = |ours: bool| -> Option<f64> {
                let si = surround?;
                let s = &self.subjects[si];
                let (n, a) = if ours {
                    (&neutral[si].samples, &at(si)?.samples)
                } else {
                    (
                        &self.lr[&format!("surround__{NEUTRAL}")].samples,
                        &self.lr[&format!("surround__{}__{}", field.id(), point.label)].samples,
                    )
                };
                let mut groups: BTreeMap<u64, Vec<NeutralAndAdjusted>> = BTreeMap::new();
                for (i, p) in s.layout["patches"].as_array()?.iter().enumerate() {
                    groups
                        .entry(p["code"][0].as_u64().unwrap_or(0))
                        .or_default()
                        .push((n[i], a[i]));
                }
                Some(la::locality(&groups.into_values().collect::<Vec<_>>()))
            };
            let mut photo_rows = Vec::new();
            for (si, s) in self.subjects.iter().enumerate() {
                if s.class != Class::Photos || point.photos.is_none() {
                    continue;
                }
                let Some(ours) = at(si) else { continue };
                let lr_n = &self.lr[&format!("{}__{NEUTRAL}", s.id)];
                let lr_a =
                    &self.lr[&format!("{}__{}-photographs__{}", s.id, field.id(), point.label)];
                let energy = |n: &Export, a: &Export| -> Vec<f64> {
                    n.energy.iter().zip(&a.energy).map(|(n, a)| a / n).collect()
                };
                photo_rows.push(json!({
                    "photo": s.id,
                    "tone_locality": [
                        la::tone_locality(&lr_n.lightness, &lr_a.lightness, 1.0, 20),
                        la::tone_locality(&neutral[si].lightness, &ours.lightness, 1.0, 20),
                    ],
                    "energy": [energy(lr_n, lr_a), energy(&neutral[si], ours)],
                }));
                if point.value.abs() >= 100.0 {
                    sheet.push((
                        format!("{} {} {}", s.id, field.id(), point.label),
                        vec![
                            lr_n.thumb.clone(),
                            lr_a.thumb.clone(),
                            neutral_thumb(s),
                            ours.thumb.clone(),
                        ],
                    ));
                }
            }
            if let Some(si) = surround
                && point.value.abs() >= 100.0
            {
                sheet.push((
                    format!("surround {} {}", field.id(), point.label),
                    vec![
                        self.lr[&format!("surround__{NEUTRAL}")].thumb.clone(),
                        self.lr[&format!("surround__{}__{}", field.id(), point.label)]
                            .thumb
                            .clone(),
                        neutral_thumb(&self.subjects[si]),
                        at(si).and_then(|e| e.thumb.clone()),
                    ],
                ));
            }
            rows.push(json!({
                "label": point.label,
                "surround_locality": [locality(false), locality(true)],
                "photographs": photo_rows,
            }));
        }
        report::contact_sheet(
            &out.join("sheets")
                .join(format!("{}-{}.png", field.id(), candidate.id)),
            &[
                "Lightroom neutral",
                "Lightroom",
                "Luxforge neutral",
                "candidate",
            ],
            &sheet,
        )?;
        Ok(Value::Array(rows))
    }

    /// What Lightroom does, read off its exports alone: each surround grey's `ΔL*` on each
    /// surround, the wedge's transfer and the photographs' `ΔL*` by neutral lightness.
    fn observations(&self) -> Value {
        let mut out = serde_json::Map::new();
        for field in Field::ALL {
            let mut entry = serde_json::Map::new();
            if let Some(s) = self.subjects.iter().find(|s| s.id == "surround") {
                let n = &self.lr[&format!("surround__{NEUTRAL}")].samples;
                let mut rows = Vec::new();
                for label in ["-100", "-50", "+50", "+100"] {
                    let a = &self.lr[&format!("surround__{}__{label}", field.id())].samples;
                    let cells: Vec<Value> = s.layout["patches"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .map(|(i, p)| {
                            json!({
                                "code": p["code"][0], "surround": p["surround"],
                                "neutral": lab_l(n[i]), "delta": lab_l(a[i]) - lab_l(n[i]),
                            })
                        })
                        .collect();
                    rows.push(json!({"label": label, "patches": cells}));
                }
                entry.insert("surround".into(), Value::Array(rows));
            }
            if self.subjects.iter().any(|s| s.id == "wedge") {
                let n = &self.lr[&format!("wedge__{NEUTRAL}")].samples;
                let mut rows = Vec::new();
                for label in ["-100", "-50", "+50", "+100"] {
                    let a = &self.lr[&format!("wedge__{}__{label}", field.id())].samples;
                    let points: Vec<[f64; 2]> = (0..n.len())
                        .step_by(8)
                        .map(|i| [lab_l(n[i]), lab_l(a[i])])
                        .collect();
                    rows.push(json!({"label": label, "transfer": points}));
                }
                entry.insert("wedge".into(), Value::Array(rows));
            }
            let mut photos = Vec::new();
            for s in self.subjects.iter().filter(|s| s.class == Class::Photos) {
                let n = &self.lr[&format!("{}__{NEUTRAL}", s.id)].lightness;
                let mut rows = Vec::new();
                for label in PHOTO_LABELS {
                    let a = &self.lr[&format!("{}__{}-photographs__{label}", s.id, field.id())]
                        .lightness;
                    let mut bins = [(0.0, 0usize); 10];
                    for (n, a) in n.iter().zip(a) {
                        let b = ((n / 10.0).floor() as usize).min(9);
                        bins[b].0 += a - n;
                        bins[b].1 += 1;
                    }
                    rows.push(json!({
                        "label": label,
                        "bins": bins.iter().enumerate().map(|(b, (sum, count))| json!({
                            "from": b * 10, "count": count,
                            "delta": if *count > 0 { Some(sum / *count as f64) } else { None },
                        })).collect::<Vec<_>>(),
                    }));
                }
                photos.push(json!({"photo": s.id, "values": rows}));
            }
            entry.insert("photographs".into(), Value::Array(photos));
            out.insert(field.id().into(), Value::Object(entry));
        }
        // Each photograph's encoded luminance at neutral, rendered: the statistics a global
        // adaptation could read.
        let mut statistics = serde_json::Map::new();
        for s in self.subjects.iter().filter(|s| s.class == Class::Photos) {
            let mut e = clamped(&s.rendered).encoded_luminance();
            e.sort_by(f64::total_cmp);
            let q = |p: f64| e[((e.len() - 1) as f64 * p).round() as usize];
            let share = |f: &dyn Fn(f64) -> bool| {
                e.iter().filter(|v| f(**v)).count() as f64 / e.len() as f64
            };
            statistics.insert(
                s.id.clone(),
                json!({
                    "mean": e.iter().sum::<f64>() / e.len() as f64,
                    "p10": q(0.1), "p50": q(0.5), "p90": q(0.9), "p99": q(0.99),
                    "above_0.75": share(&|v| v > 0.75), "below_0.15": share(&|v| v < 0.15),
                }),
            );
        }
        out.insert("statistics".into(), Value::Object(statistics));
        Value::Object(out)
    }
}

fn neutral_thumb(subject: &Subject) -> Option<image::RgbImage> {
    Some(thumbnail(&clamped(&subject.rendered)))
}

// -------------------------------------------------------------------------------------------
// Round 2's pairs
// -------------------------------------------------------------------------------------------

/// Round 2's photograph families the study renders, the fields their labels set in order, and
/// their labels.
const PAIRS: [(&str, [&str; 2], [&str; 4]); 4] = [
    (
        "contrast+whites",
        ["contrast", "whites"],
        ["-100_-100", "-100_+100", "+100_-100", "+100_+100"],
    ),
    (
        "contrast+blacks",
        ["contrast", "blacks"],
        ["-100_-100", "-100_+100", "+100_-100", "+100_+100"],
    ),
    (
        "exposure+highlights",
        ["exposure", "highlights"],
        ["-1_-100", "-1_+100", "+1_-100", "+1_+100"],
    ),
    (
        "exposure+whites",
        ["exposure", "whites"],
        ["-1_-100", "-1_+100", "+1_-100", "+1_+100"],
    ),
];

/// Lightroom's exports of round 2's photograph pairs, by `<photograph>__<family>__<label>` and
/// `<photograph>__neutral`, reduced as the study's subjects are.
struct Pairs {
    lr: BTreeMap<String, Export>,
}

/// The exposure and tone parameters a pair's label sets, through today's responses.
fn pair_settings(fields: [&str; 2], label: &str) -> (f64, ToneParams) {
    let mut exposure = 0.0;
    let mut params = ToneParams::NEUTRAL;
    for (field, value) in fields.iter().zip(label.split('_')) {
        let value: f64 = value.trim_start_matches('+').parse().unwrap_or(0.0);
        let strength = || basic_response(field).map_or(value, |r| r.strength(value));
        match *field {
            "exposure" => exposure = value,
            "contrast" => params.contrast = strength(),
            "highlights" => params.highlights = strength(),
            "shadows" => params.shadows = strength(),
            "whites" => params.whites = strength(),
            "blacks" => params.blacks = strength(),
            _ => {}
        }
    }
    (exposure, params)
}

/// A photograph under `candidate` with `exposure` (a plain gain on its scene planes) and `params`,
/// before the output clamp.
fn evaluate_pair(
    subject: &Subject,
    candidate: &Candidate,
    exposure: f64,
    params: ToneParams,
) -> Frame {
    if let Some(split) = candidate.split {
        let (scene, rendered) = split.divide(params);
        let whole = Candidate {
            split: None,
            adapt_scene: true,
            ..candidate.clone()
        };
        let looked = evaluate_pair(subject, &whole, exposure, scene);
        return lt::global(&looked, rendered, Reconstruction::LuminanceRatio);
    }
    let exposed = scaled(&subject.input, 2f64.powf(exposure));
    let tone = |frame: &Frame| {
        let encoded = frame.encoded_luminance();
        let levels = if candidate.placement != Placement::AfterLook && !candidate.adapt_scene {
            Levels::UNIT
        } else {
            Levels::of(&encoded, candidate.adapt.0, candidate.adapt.1)
        };
        match candidate.operator {
            Operator::Global(how) => lt::global_within(frame, params, how, levels),
            Operator::Local(config) => {
                let base = lt::smooth(&encoded, frame.width, frame.height, config.smoother);
                lt::local_within(frame, &encoded, &base, params, config, levels)
            }
        }
    };
    match candidate.placement {
        Placement::BeforeLook => apply_look(subject.look.as_ref(), &tone(&exposed)),
        p @ (Placement::BeforeLookShifted(_) | Placement::BeforeLookWhite { .. }) => {
            let stops = shift_of(p, &exposed);
            let toned = tone(&scaled(&exposed, 2f64.powf(-stops)));
            apply_look(subject.look.as_ref(), &scaled(&toned, 2f64.powf(stops)))
        }
        Placement::AfterLook => tone(&apply_look(subject.look.as_ref(), &exposed)),
    }
}

impl Pairs {
    fn open(study: &Study, folder: &Path, cache: Option<&Path>) -> Result<Self> {
        let cached = cache.map(|c| c.join("pairs.json"));
        if let Some(path) = &cached
            && path.exists()
        {
            let v = read_json(path)?;
            let mut lr = BTreeMap::new();
            for (id, e) in v.as_object().into_iter().flatten() {
                let thumb = cache
                    .and_then(|c| image::open(c.join("thumbs").join(format!("r2__{id}.png"))).ok())
                    .map(|i| i.to_rgb8());
                lr.insert(
                    id.clone(),
                    Export {
                        samples: serde_json::from_value(e["samples"].clone())?,
                        thumb,
                        ..Export::default()
                    },
                );
            }
            return Ok(Self { lr });
        }
        let exports = folder.join(super::EXPORTS);
        let mut reads: Vec<(usize, String)> = Vec::new();
        for (si, s) in study.subjects.iter().enumerate() {
            if s.class != Class::Photos {
                continue;
            }
            reads.push((si, format!("{}__{NEUTRAL}", s.id)));
            for (family, _, labels) in PAIRS {
                for label in labels {
                    reads.push((si, format!("{}__{family}-photographs__{label}", s.id)));
                }
            }
        }
        let read = parallel(&reads, |(si, id)| {
            let subject = &study.subjects[*si];
            let path = exports.join(format!("{id}.tif"));
            let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let tiff = crate::tiff::read(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            drop(bytes);
            let frame = from_codes(
                tiff.width as usize,
                tiff.height as usize,
                &tiff.rgb,
                study.reduction,
                1.0,
            );
            ensure(
                (frame.width, frame.height) == (subject.input.width, subject.input.height),
                format!("{id}: not the subject's size"),
            )?;
            let frame = clamped(&frame);
            Ok((
                id.clone(),
                Export {
                    samples: normalized_cells(&frame),
                    thumb: Some(thumbnail(&frame)),
                    ..Export::default()
                },
            ))
        })?;
        let lr: BTreeMap<String, Export> = read.into_iter().collect();
        if let (Some(path), Some(dir)) = (&cached, cache) {
            fs::create_dir_all(dir.join("thumbs"))?;
            let mut map = serde_json::Map::new();
            for (id, e) in &lr {
                if let Some(t) = &e.thumb {
                    t.save(dir.join("thumbs").join(format!("r2__{id}.png")))?;
                }
                map.insert(id.clone(), json!({"samples": e.samples}));
            }
            write_json(path, &Value::Object(map))?;
        }
        Ok(Self { lr })
    }

    /// Each candidate's residual on each pair, the photographs' samples pooled as `lr-align
    /// measure` pools them, at today's responses; and a contact sheet per family and candidate.
    fn run(&self, study: &Study, candidates: &[Candidate], out: &Path) -> Result<Value> {
        let photos = study.class_subjects(Class::Photos);
        let neutral: Vec<Vec<Lab>> = photos
            .iter()
            .map(|si| {
                let s = &study.subjects[*si];
                labs(&normalized_cells(&clamped(&s.rendered)))
            })
            .collect();
        let mut rows = Vec::new();
        for candidate in candidates {
            for (family, fields, labels) in PAIRS {
                let started = std::time::Instant::now();
                let jobs: Vec<(usize, &str)> = photos
                    .iter()
                    .enumerate()
                    .flat_map(|(k, _)| labels.iter().map(move |l| (k, *l)))
                    .collect();
                let rendered = parallel(&jobs, |(k, label)| {
                    let s = &study.subjects[photos[*k]];
                    let (exposure, params) = pair_settings(fields, label);
                    let frame = clamped(&evaluate_pair(s, candidate, exposure, params));
                    Ok((labs(&normalized_cells(&frame)), thumbnail(&frame)))
                })?;
                let mut points = Vec::new();
                let mut sheet = Vec::new();
                for label in labels {
                    let (mut ours, mut carried, mut base) = (Vec::new(), Vec::new(), Vec::new());
                    let mut per_photo = Vec::new();
                    for (k, si) in photos.iter().enumerate() {
                        let s = &study.subjects[*si];
                        let theirs = &self.lr[&format!("{}__{NEUTRAL}", s.id)];
                        let adjusted =
                            &self.lr[&format!("{}__{family}-photographs__{label}", s.id)];
                        let ours_neutral = s.reduce(&s.rendered, false);
                        let photo_carried: Vec<Lab> = ours_neutral
                            .samples
                            .iter()
                            .zip(&theirs.samples)
                            .zip(&adjusted.samples)
                            .map(|((n, r), a)| la::lab(la::carried(*n, *r, *a)))
                            .collect();
                        let at = jobs
                            .iter()
                            .position(|(kk, l)| *kk == k && *l == label)
                            .ok_or("a pair no job rendered")?;
                        let (photo_labs, thumb) = &rendered[at];
                        let target = Target::new(&neutral[k], photo_carried.clone());
                        per_photo.push(json!({
                            "photo": s.id,
                            "residual": target.distance(photo_labs),
                            "size": target.size(&neutral[k]),
                        }));
                        ours.extend(photo_labs.iter().copied());
                        carried.extend(photo_carried);
                        base.extend(neutral[k].iter().copied());
                        sheet.push((
                            format!("{} {family} {label}", s.id),
                            vec![
                                theirs.thumb.clone(),
                                adjusted.thumb.clone(),
                                neutral_thumb(s),
                                Some(thumb.clone()),
                            ],
                        ));
                    }
                    let target = Target::new(&base, carried);
                    points.push(json!({
                        "label": label,
                        "residual": target.distance(&ours),
                        "size": target.size(&base),
                        "photographs": per_photo,
                    }));
                }
                report::contact_sheet(
                    &out.join("sheets")
                        .join(format!("pairs-{family}-{}.png", candidate.id)),
                    &[
                        "Lightroom neutral",
                        "Lightroom",
                        "Luxforge neutral",
                        "candidate",
                    ],
                    &sheet,
                )?;
                let worst = points
                    .iter()
                    .filter_map(|p| p["residual"].as_f64())
                    .fold(0.0, f64::max);
                println!(
                    "{} {family}: worst pooled residual {worst:.2} ({:.1} s)",
                    candidate.id,
                    started.elapsed().as_secs_f64()
                );
                rows.push(json!({
                    "candidate": candidate.id, "family": family, "worst": worst, "points": points,
                }));
            }
        }
        Ok(Value::Array(rows))
    }
}

// -------------------------------------------------------------------------------------------
// The page
// -------------------------------------------------------------------------------------------

fn sorted_rows<'a>(rows: &[&'a Value]) -> Vec<&'a Value> {
    let mut sorted = rows.to_vec();
    sorted.sort_by(|a, b| {
        a["worst"]
            .as_f64()
            .unwrap_or(f64::INFINITY)
            .total_cmp(&b["worst"].as_f64().unwrap_or(f64::INFINITY))
    });
    sorted
}

fn page(study: &Study, report: &Value) -> String {
    let mut md = String::from("# Basic local tone: the study\n\n");
    md += &format!(
        "Candidates against Lightroom Classic's exports of round 1's charts (wedge, patches, \
         surround) and the corpus photographs, through `luxforge_reference::local_tone` on the \
         CPU; photographs at 1/{} per axis. No GPU output. A residual is the median CIEDE2000 \
         between a candidate's response and Lightroom's carried onto Luxforge's neutral, over the \
         samples Lightroom's response moves.\n\n",
        study.reduction
    );
    md += "## The study's neutral against the editor's\n\nMedian and 95th percentile CIEDE2000 \
           over each photograph's cells between the study's reduced Look and the reference \
           renderer's own neutral, reduced: what reducing before the Look costs.\n\n\
           | photograph | median | p95 |\n|---|---|---|\n";
    for (id, (m, p)) in &study.sanity {
        md += &format!("| {id} | {} | {} |\n", number(Some(*m)), number(Some(*p)));
    }
    md += "\n## Fits\n\nPer field and candidate, the worst residual at Lightroom values of ±25 \
           and beyond with one strength per value for both classes (the joint fit); per value, \
           the joint strength and the charts' / photographs' residuals there, and in brackets \
           each class's own best.\n\n";
    let results = report["results"].as_array().cloned().unwrap_or_default();
    for field in Field::ALL {
        let rows: Vec<&Value> = results
            .iter()
            .filter(|r| r["field"] == field.id())
            .collect();
        if rows.is_empty() {
            continue;
        }
        md += &format!("### {}\n\n| candidate | worst | ", field.id());
        let labels: Vec<String> = rows[0]["points"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| p["label"].as_str().unwrap_or("").to_owned())
            .collect();
        md += &labels.join(" | ");
        md += " |\n|---|---|";
        md += &"---|".repeat(labels.len());
        md += "\n";
        for r in sorted_rows(&rows) {
            md += &format!(
                "| {} | {}{} |",
                r["candidate"].as_str().unwrap_or(""),
                number(r["worst"].as_f64()),
                if r["passes"] == true { " ✓" } else { "" }
            );
            for p in r["points"].as_array().into_iter().flatten() {
                let j = &p["joint"];
                let own = |k: &str| {
                    p[k]["strength"]
                        .as_f64()
                        .map(|s| format!("{s:.0}"))
                        .unwrap_or("–".into())
                };
                md += &format!(
                    " {:.0}: {}/{} ({}/{}) |",
                    j["strength"].as_f64().unwrap_or(f64::NAN),
                    number(j["charts"].as_f64()),
                    number(j["photos"].as_f64()),
                    own("charts"),
                    own("photos"),
                );
            }
            md += "\n";
        }
        md += "\n";
        md += "Each photograph's own best strength at −100 / −50 / +50 / +100 (residual there), \
               per candidate:\n\n";
        for r in &sorted_rows(&rows) {
            md += &format!("- {}:", r["candidate"].as_str().unwrap_or(""));
            let points = r["points"].as_array().cloned().unwrap_or_default();
            let mut photos: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for p in &points {
                for e in p["per_photo"].as_array().into_iter().flatten() {
                    photos
                        .entry(e["photo"].as_str().unwrap_or("").into())
                        .or_default()
                        .push(format!(
                            "{:.0} ({})",
                            e["strength"].as_f64().unwrap_or(f64::NAN),
                            number(e["residual"].as_f64())
                        ));
                }
            }
            for (id, v) in photos {
                md += &format!(" {id} {};", v.join(" / "));
            }
            md += "\n";
        }
        md += "\n";
        // Measures of each candidate at ±100.
        md += "Surround locality at ±100 (Lightroom / candidate) and the photographs' median tone \
               locality and mean fine and coarse band energy at ±100 (Lightroom / candidate):\n\n\
               | candidate | surround −100 | surround +100 | tone locality −100 | tone locality +100 | fine −100 | coarse −100 | fine +100 | coarse +100 |\n\
               |---|---|---|---|---|---|---|---|---|\n";
        for r in &rows {
            let m = r["measures"].as_array().cloned().unwrap_or_default();
            let row = |label: &str| m.iter().find(|x| x["label"] == label).cloned();
            let (neg, pos) = (row("-100"), row("+100"));
            let pair = |v: &Option<Value>, f: &dyn Fn(&Value) -> (f64, f64)| -> String {
                v.as_ref()
                    .map(|v| {
                        let (a, b) = f(v);
                        format!("{} / {}", number(Some(a)), number(Some(b)))
                    })
                    .unwrap_or("–".into())
            };
            let surround = |v: &Value| {
                (
                    v["surround_locality"][0].as_f64().unwrap_or(f64::NAN),
                    v["surround_locality"][1].as_f64().unwrap_or(f64::NAN),
                )
            };
            let tone = |v: &Value| {
                let photos = v["photographs"].as_array().cloned().unwrap_or_default();
                let side = |k: usize| {
                    la::median(
                        &photos
                            .iter()
                            .filter_map(|p| p["tone_locality"][k].as_f64())
                            .collect::<Vec<_>>(),
                    )
                };
                (side(0), side(1))
            };
            let band = |b: usize| {
                move |v: &Value| {
                    let photos = v["photographs"].as_array().cloned().unwrap_or_default();
                    let side = |k: usize| {
                        let values: Vec<f64> = photos
                            .iter()
                            .filter_map(|p| p["energy"][k][b].as_f64())
                            .collect();
                        values.iter().sum::<f64>() / values.len().max(1) as f64
                    };
                    (side(0), side(1))
                }
            };
            md += &format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                r["candidate"].as_str().unwrap_or(""),
                pair(&neg, &surround),
                pair(&pos, &surround),
                pair(&neg, &tone),
                pair(&pos, &tone),
                pair(&neg, &band(0)),
                pair(&neg, &band(2)),
                pair(&pos, &band(0)),
                pair(&pos, &band(2)),
            );
        }
        md += "\n";
    }
    if let Some(pairs) = report["pairs"].as_array() {
        md += "## Round 2's photograph pairs\n\nPooled median CIEDE2000 over the five \
               photographs' cells at today's responses (Exposure a plain gain), per candidate; the \
               response's own size in brackets.\n\n";
        for (family, _, labels) in PAIRS {
            let rows: Vec<&Value> = pairs.iter().filter(|r| r["family"] == family).collect();
            if rows.is_empty() {
                continue;
            }
            md += &format!(
                "### {family}\n\n| candidate | worst | {} |\n|---|---|",
                labels.join(" | ")
            );
            md += &"---|".repeat(labels.len());
            md += "\n";
            for r in sorted_rows(&rows) {
                md += &format!(
                    "| {} | {} |",
                    r["candidate"].as_str().unwrap_or(""),
                    number(r["worst"].as_f64())
                );
                for p in r["points"].as_array().into_iter().flatten() {
                    md += &format!(
                        " {} ({}) |",
                        number(p["residual"].as_f64()),
                        number(p["size"].as_f64())
                    );
                }
                md += "\n";
            }
            md += "\n";
        }
    }
    if report["observations"].is_null() {
        return md;
    }
    md += "## Lightroom's responses\n\n### Surround target\n\nEach grey's `ΔL*` on the dark (16), \
           mid (118) and light (240) surround.\n\n";
    let obs = &report["observations"];
    for field in Field::ALL {
        let Some(rows) = obs[field.id()]["surround"].as_array() else {
            continue;
        };
        md += &format!(
            "**{}**\n\n| value | grey | on 16 | on 118 | on 240 |\n|---|---|---|---|---|\n",
            field.id()
        );
        for r in rows {
            let patches = r["patches"].as_array().cloned().unwrap_or_default();
            for code in [40, 80, 128, 176, 224] {
                let on = |surround: u64| {
                    patches
                        .iter()
                        .find(|p| p["code"] == code && p["surround"] == surround)
                        .and_then(|p| p["delta"].as_f64())
                };
                md += &format!(
                    "| {} | {code} | {} | {} | {} |\n",
                    r["label"].as_str().unwrap_or(""),
                    number(on(16)),
                    number(on(118)),
                    number(on(240))
                );
            }
        }
        md += "\n";
    }
    md += "### The photographs' statistics\n\nEncoded luminance of each photograph's neutral \
           render.\n\n| photograph | mean | p10 | p50 | p90 | p99 | above 0.75 | below 0.15 |\n\
           |---|---|---|---|---|---|---|---|\n";
    for (id, v) in obs["statistics"].as_object().into_iter().flatten() {
        let f = |k: &str| number(v[k].as_f64());
        md += &format!(
            "| {id} | {} | {} | {} | {} | {} | {} | {} |\n",
            f("mean"),
            f("p10"),
            f("p50"),
            f("p90"),
            f("p99"),
            f("above_0.75"),
            f("below_0.15")
        );
    }
    md += "\n### Photographs\n\nLightroom's mean `ΔL*` by neutral `L*` decile, over the five \
           photographs' cells pooled.\n\n";
    for field in Field::ALL {
        let photos = obs[field.id()]["photographs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if photos.is_empty() {
            continue;
        }
        md += &format!("**{}**\n\n| value |", field.id());
        for b in 0..10 {
            md += &format!(" {}–{} |", b * 10, b * 10 + 10);
        }
        md += "\n|---|";
        md += &"---|".repeat(10);
        md += "\n";
        for label in PHOTO_LABELS {
            md += &format!("| {label} |");
            for b in 0..10 {
                let (mut sum, mut count) = (0.0, 0u64);
                for p in &photos {
                    for v in p["values"].as_array().into_iter().flatten() {
                        if v["label"] == label {
                            let bin = &v["bins"][b];
                            let c = bin["count"].as_u64().unwrap_or(0);
                            if let Some(d) = bin["delta"].as_f64() {
                                sum += d * c as f64;
                                count += c;
                            }
                        }
                    }
                }
                md += &format!(
                    " {} |",
                    if count > 0 {
                        format!("{:.2}", sum / count as f64)
                    } else {
                        "–".into()
                    }
                );
            }
            md += "\n";
        }
        md += "\n";
    }
    md
}
