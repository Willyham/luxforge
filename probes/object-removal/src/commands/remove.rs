//! `removal-probe remove`: the whole chain from a point or a stroke to the filled image.
//!
//! A point is decoded alone and picked as hover picks (`stroke::pick_hover`); its region has no
//! stroke. A stroke is turned into prompts, decoded in one batch and decided (`stroke::decide`):
//! **one** object is removed with the stroke's footprint (unless `--no-stroke-region`); **several**
//! are drawn with numbered outlines and nothing is removed unless `--pick` chooses among them, in
//! which case the chosen objects' regions are joined without the footprint (the stroke also crosses
//! the objects left out); **nothing** removes the stroke's footprint, feathered.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::args::Args;
use crate::commands::segment::area_px;
use crate::image::Rgb8;
use crate::lab::log::{ms, round3};
use crate::lab::pipeline::{self, Filler, Target};
use crate::lab::raster::{self, Sampling};
use crate::mask::{Logits, Mask, Point, RectPx};
use crate::models;
use crate::refine;
use crate::runtime::Backend;
use crate::segment::{self, Candidate, Family, Prompt};
use crate::stroke::{self, Decision, Stroke};

pub const USAGE: &str = "  remove --image IMG [--family evit-l0|sam21-small] [--backend B] [--fill-backend B] [--fill-model lama|lama-fp32]
          (--point x,y | --stroke \"x,y x,y ...\" --radius R) [--pick N[,M...]] [--no-stroke-region]
          [--contact-band] [--param NAME=VALUE]... [--models DIR] [--out DIR]
                                           segment, decide, build the region and fill; write decision.json,
                                           overlay.png (several: numbered outlines), region.png, result.png and
                                           compare.png. Several removes nothing unless --pick chooses by number.
                                           --param sets a stroke::Params field (e.g. min_predicted_iou=0.6).
                                           DIR defaults to a new directory under the system temp dir";

/// Overlays and comparisons are at most this long on their long side.
const VIEW_LONG: u32 = 2048;
/// Overlays show at least this many source pixels across, and are enlarged to at least this size.
const VIEW_MIN: u32 = 768;

enum Input {
    Point(Point),
    Stroke(Stroke),
}

pub fn run(mut a: Args) -> Result<(), String> {
    let image_path = PathBuf::from(a.required("--image")?);
    let family: Family = a.parsed("--family")?.unwrap_or(Family::EfficientVitL0);
    let backend: Backend = a.parsed("--backend")?.unwrap_or(Backend::OrtCpu);
    let fill_backend: Backend = a.parsed("--fill-backend")?.unwrap_or(Backend::OrtCpu);
    let fill_model: crate::models::FillModel = a.parsed("--fill-model")?.unwrap_or_default();
    let point = a.opt("--point")?.map(|s| parse_point(&s)).transpose()?;
    let stroke_points = a.opt("--stroke")?;
    let radius: Option<f32> = a.parsed("--radius")?;
    let picks = a.opt("--pick")?.map(|s| parse_picks(&s)).transpose()?;
    let mut stroke_params = stroke::Params::for_family(family);
    for setting in a.all("--param")? {
        crate::lab::stroke_param(&mut stroke_params, &setting)?;
    }
    let no_stroke_region = a.flag("--no-stroke-region");
    let contact_band = a.flag("--contact-band");
    let models = models::directory(a.path("--models")?);
    let out = a.path("--out")?;
    a.done()?;

    let input = match (point, stroke_points, radius) {
        (Some(p), None, None) => Input::Point(p),
        (None, Some(s), Some(r)) if r > 0.0 && r.is_finite() => Input::Stroke(Stroke {
            points: parse_points(&s)?,
            radius: r,
        }),
        _ => {
            return Err(
                "give either --point x,y, or --stroke \"x,y x,y ...\" with a positive --radius R"
                    .into(),
            );
        }
    };
    if picks.is_some() && matches!(input, Input::Point(_)) {
        return Err("--pick chooses among the objects a stroke found; a point finds one".into());
    }
    let out = out.unwrap_or_else(|| default_out(&image_path));
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    eprintln!("remove: writing to {}", out.display());
    let paths = Outputs::new(&out);
    paths.check_not(&image_path)?;

    let refine_params = refine::Params {
        include_stroke: !no_stroke_region,
        contact_band,
        ..refine::Params::default()
    };
    let mut timings = Timings::default();

    let t = Instant::now();
    let image = Rgb8::load(&image_path)?;
    timings.push("image_load", t);
    let size = (image.width, image.height);
    check_inside(&input, size)?;

    let t = Instant::now();
    let (mut segmenter, load) = segment::open_timed(family, backend, backend, &models)?;
    timings.push("segmenter_open", t);
    timings.add("encoder_load", ms(load.encoder));
    timings.add("decoder_load", ms(load.decoder));
    let t = Instant::now();
    let embedding = segmenter.encode(&image)?;
    timings.push("encode", t);

    let mut report = json!({
        "image": image_path.display().to_string(),
        "source": [image.width, image.height],
        "family": family.name(),
        "backend": backend.name(),
        "fill_backend": fill_backend.name(),
        "fill_model": fill_model.name(),
        "stroke_params": {
            "sample_points": stroke_params.sample_points,
            "min_predicted_iou": stroke_params.min_predicted_iou,
            "min_whole_iou": stroke_params.min_whole_iou,
            "min_stability": stroke_params.min_stability,
            "nms_iou": stroke_params.nms_iou,
            "one_object_stroke_coverage": stroke_params.one_object_stroke_coverage,
            "min_object_coverage": stroke_params.min_object_coverage,
        },
    });

    // Segment and decide: what to remove, and what the overlay shows.
    let mut shown: Vec<Shown> = Vec::new();
    let mut prompts_drawn: Vec<Prompt> = Vec::new();
    let targets: Option<Vec<Target>>;
    let decision_kind: &str;
    let mut why_not: Option<String> = None;
    match &input {
        Input::Point(p) => {
            report["input"] = json!({"point": [p.x, p.y]});
            let prompt = Prompt {
                positive: vec![*p],
                ..Prompt::default()
            };
            let t = Instant::now();
            let candidates = segmenter.decode(&embedding, &prompt)?;
            timings.push("decode", t);
            let t = Instant::now();
            let pick = stroke::pick_hover(&candidates, &stroke_params);
            timings.push("pick", t);
            report["candidates"] = Value::Array(
                candidates
                    .iter()
                    .enumerate()
                    .map(|(i, c)| candidate_json(i, c, pick == Some(i)))
                    .collect(),
            );
            for (i, c) in candidates.iter().enumerate() {
                shown.push(Shown {
                    logits: c.logits.clone(),
                    look: if pick == Some(i) {
                        Look::Chosen
                    } else {
                        Look::Other
                    },
                });
            }
            prompts_drawn.push(prompt);
            match pick {
                Some(i) => {
                    decision_kind = "one";
                    report["chosen"] = json!([i]);
                    targets = Some(vec![Target::Object {
                        logits: candidates[i].logits.clone(),
                        stroke: None,
                    }]);
                }
                None => {
                    decision_kind = "nothing";
                    why_not = Some("no candidate under the point".into());
                    targets = None;
                }
            }
        }
        Input::Stroke(s) => {
            report["input"] = json!({
                "stroke": s.points.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(),
                "radius": s.radius,
            });
            let t = Instant::now();
            let prompts = stroke::prompts(s, size, &stroke_params);
            timings.push("prompts", t);
            let t = Instant::now();
            let results = segmenter.decode_many(&embedding, &prompts)?;
            timings.push("decode", t);
            let t = Instant::now();
            let resolution = stroke::decide(s, &results, &stroke_params);
            timings.push("decide", t);
            report["prompts"] = Value::Array(prompts.iter().map(prompt_json).collect());
            // Every decoded candidate, before `decide` filters them, for tuning its floors.
            report["decoded"] = Value::Array(
                results
                    .iter()
                    .enumerate()
                    .flat_map(|(p, candidates)| {
                        candidates.iter().enumerate().map(move |(i, c)| {
                            json!({
                                "prompt": p, "candidate": i,
                                "predicted_iou": round3(c.predicted_iou as f64),
                                "stability": round3(stroke::stability(&c.logits, 1.0) as f64),
                                "area_px": area_px(&c.logits),
                            })
                        })
                    })
                    .collect(),
            );
            let logits_of = |i: usize| {
                let sc = &resolution.scored[i];
                results[sc.prompt][sc.candidate].logits.clone()
            };
            report["candidates"] = Value::Array(
                resolution
                    .scored
                    .iter()
                    .enumerate()
                    .map(|(i, sc)| {
                        let l = logits_of(i);
                        json!({
                            "index": i, "prompt": sc.prompt, "candidate": sc.candidate,
                            "predicted_iou": round3(sc.predicted_iou as f64),
                            "stability": round3(sc.stability as f64),
                            "stroke_coverage": round3(sc.stroke_coverage as f64),
                            "object_coverage": round3(sc.object_coverage as f64),
                            "area_px": sc.area.round(),
                            "bounds": raster::positive_bounds(&l).map(|b| b.map(|v| v.round())),
                        })
                    })
                    .collect(),
            );
            let chosen: Vec<usize> = match &resolution.decision {
                Decision::One(i) => vec![*i],
                Decision::Several(v) => v.clone(),
                Decision::Nothing => Vec::new(),
            };
            report["chosen"] = json!(chosen);
            for i in 0..resolution.scored.len() {
                if !chosen.contains(&i) {
                    shown.push(Shown {
                        logits: logits_of(i),
                        look: Look::Other,
                    });
                }
            }
            prompts_drawn = prompts;
            match &resolution.decision {
                Decision::One(i) => {
                    decision_kind = "one";
                    shown.push(Shown {
                        logits: logits_of(*i),
                        look: Look::Chosen,
                    });
                    targets = Some(vec![Target::Object {
                        logits: logits_of(*i),
                        stroke: Some(s.clone()),
                    }]);
                }
                Decision::Several(list) => {
                    decision_kind = "several";
                    let mut numbers = Vec::new();
                    for (k, &i) in list.iter().enumerate() {
                        let l = logits_of(i);
                        let pin = raster::pin(&l);
                        numbers.push(json!({"number": k + 1, "index": i, "pin": pin.map(|p| [p.x.round(), p.y.round()])}));
                        let picked = picks.as_ref().is_some_and(|p| p.contains(&(k + 1)));
                        shown.push(Shown {
                            logits: l,
                            look: Look::Numbered {
                                number: k + 1,
                                pin,
                                picked,
                            },
                        });
                    }
                    report["numbers"] = Value::Array(numbers);
                    match &picks {
                        None => {
                            why_not = Some(format!(
                                "{} objects under the stroke; choose with --pick N[,M...]",
                                list.len()
                            ));
                            targets = None;
                        }
                        Some(p) => {
                            if let Some(bad) = p.iter().find(|&&n| n == 0 || n > list.len()) {
                                write_overlay(&paths, &image, &shown, &prompts_drawn, Some(s))?;
                                return Err(format!(
                                    "--pick {bad}: the stroke found objects 1 to {}",
                                    list.len()
                                ));
                            }
                            targets = Some(
                                p.iter()
                                    .map(|&n| Target::Object {
                                        logits: logits_of(list[n - 1]),
                                        stroke: None,
                                    })
                                    .collect(),
                            );
                        }
                    }
                }
                Decision::Nothing => {
                    decision_kind = "nothing";
                    targets = Some(vec![Target::Footprint(s.clone())]);
                }
            }
            if picks.is_some() && decision_kind != "several" {
                write_overlay(&paths, &image, &shown, &prompts_drawn, Some(s))?;
                return Err(format!(
                    "--pick chooses among several objects, but the decision is {decision_kind}"
                ));
            }
        }
    }
    report["decision"] = json!(decision_kind);
    report["picks"] = json!(picks);

    let t = Instant::now();
    let stroke = match &input {
        Input::Stroke(s) => Some(s),
        Input::Point(_) => None,
    };
    write_overlay(&paths, &image, &shown, &prompts_drawn, stroke)?;
    timings.push("write_overlay", t);

    let Some(targets) = targets else {
        report["removed"] = json!(false);
        report["why_not"] = json!(why_not);
        report["timings_ms"] = timings.json();
        report["outputs"] = json!({"overlay": "overlay.png"});
        write_json(&paths.decision, &report)?;
        println!(
            "decision {decision_kind}: nothing removed ({}); see {}",
            why_not.unwrap_or_default(),
            paths.decision.display()
        );
        return Ok(());
    };

    let t = Instant::now();
    let region = pipeline::region(&targets, &image, &refine_params);
    timings.push("region", t);
    let bounds = region.bounds().ok_or("the removal region is empty")?;
    report["region"] = json!({
        "bounds": rect_json(bounds),
        "area_px": region.area(),
        "include_stroke": refine_params.include_stroke && decision_kind == "one" && stroke.is_some(),
        "contact_band": refine_params.contact_band,
        "dilation_px": refine_params.dilation,
        "feather_px": refine_params.feather,
        "targets": targets.len(),
    });

    let t = Instant::now();
    let (mut filler, fill_load) = Filler::open(
        fill_model,
        fill_backend,
        &models,
        crate::fill::Params::default(),
    )?;
    timings.push("fill_open", t);
    timings.add("fill_load", ms(fill_load));
    let t = Instant::now();
    let filled = filler.run(&image, &region);
    timings.push("fill", t);
    let (result, fill_report) = match filled {
        Ok(r) => r,
        Err(e) => {
            region.save_png(&paths.region)?;
            report["removed"] = json!(false);
            report["why_not"] = json!(format!("the fill failed: {e}"));
            report["timings_ms"] = timings.json();
            report["outputs"] = json!({"overlay": "overlay.png", "region": "region.png"});
            write_json(&paths.decision, &report)?;
            return Err(format!(
                "the fill failed: {e} (decision and region written to {})",
                out.display()
            ));
        }
    };
    let outside = outside_changes(&image, &result, &region);
    report["fill"] = serde_json::to_value(&fill_report).map_err(|e| e.to_string())?;
    report["outside_region_changed_px"] = json!(outside);

    let t = Instant::now();
    region.save_png(&paths.region)?;
    result.save_png(&paths.result)?;
    write_compare(&paths.compare, &image, &result, &region, bounds)?;
    timings.push("write_images", t);
    report["removed"] = json!(true);
    report["timings_ms"] = timings.json();
    report["outputs"] = json!({"overlay": "overlay.png", "region": "region.png", "result": "result.png", "compare": "compare.png"});
    write_json(&paths.decision, &report)?;
    println!(
        "decision {decision_kind}: removed {} px in {:?} with {}; outside the region {} px changed; see {}",
        region.area(),
        rect_json(bounds),
        fill_report.fill,
        outside,
        paths.decision.display()
    );
    if outside != 0 {
        return Err(format!(
            "{outside} pixels outside the region changed; the fill must leave them byte-identical"
        ));
    }
    Ok(())
}

struct Outputs {
    decision: PathBuf,
    overlay: PathBuf,
    region: PathBuf,
    result: PathBuf,
    compare: PathBuf,
}

impl Outputs {
    fn new(dir: &Path) -> Outputs {
        Outputs {
            decision: dir.join("decision.json"),
            overlay: dir.join("overlay.png"),
            region: dir.join("region.png"),
            result: dir.join("result.png"),
            compare: dir.join("compare.png"),
        }
    }

    /// Refuses an output directory where an output would be the input.
    fn check_not(&self, input: &Path) -> Result<(), String> {
        let input =
            std::fs::canonicalize(input).map_err(|e| format!("{}: {e}", input.display()))?;
        for p in [
            &self.decision,
            &self.overlay,
            &self.region,
            &self.result,
            &self.compare,
        ] {
            if std::fs::canonicalize(p).is_ok_and(|p| p == input) {
                return Err(format!(
                    "{} would overwrite the input; choose another --out",
                    p.display()
                ));
            }
        }
        Ok(())
    }
}

fn default_out(image: &Path) -> PathBuf {
    let stem = image
        .file_stem()
        .map_or("image".into(), |s| s.to_string_lossy().into_owned());
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    std::env::temp_dir().join(format!("removal-probe-remove-{stem}-{stamp}"))
}

#[derive(Default)]
struct Timings(Vec<(&'static str, f64)>);

impl Timings {
    fn push(&mut self, stage: &'static str, since: Instant) {
        self.0.push((stage, ms(since.elapsed())));
    }

    fn add(&mut self, stage: &'static str, ms: f64) {
        self.0.push((stage, ms));
    }

    fn json(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect(),
        )
    }
}

fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn rect_json(r: RectPx) -> [u32; 4] {
    [r.x, r.y, r.width, r.height]
}

fn candidate_json(i: usize, c: &Candidate, picked: bool) -> Value {
    json!({
        "index": i,
        "predicted_iou": round3(c.predicted_iou as f64),
        "stability": round3(stroke::stability(&c.logits, 1.0) as f64),
        "area_px": area_px(&c.logits),
        "bounds": raster::positive_bounds(&c.logits).map(|b| b.map(|v| v.round())),
        "picked": picked,
    })
}

fn prompt_json(p: &Prompt) -> Value {
    json!({
        "positive": p.positive.iter().map(|q| [q.x, q.y]).collect::<Vec<_>>(),
        "box": p.bbox.map(|(a, b)| [a.x, a.y, b.x, b.y]),
    })
}

/// Pixels whose coverage is 0 but whose bytes differ: must be 0 for every fill.
fn outside_changes(before: &Rgb8, after: &Rgb8, region: &Mask) -> u64 {
    let w = before.width as usize;
    let mut n = 0;
    for (i, (a, b)) in before
        .data
        .chunks_exact(3)
        .zip(after.data.chunks_exact(3))
        .enumerate()
    {
        if a != b && region.get((i % w) as u32, (i / w) as u32) == 0 {
            n += 1;
        }
    }
    n
}

fn parse_numbers(s: &str, what: &str) -> Result<Vec<f32>, String> {
    s.split(',')
        .map(|t| t.trim().parse::<f32>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("{what} {s:?}: expected x,y"))
        .and_then(|v| {
            if v.len() == 2 && v.iter().all(|x| x.is_finite()) {
                Ok(v)
            } else {
                Err(format!("{what} {s:?}: expected x,y"))
            }
        })
}

fn parse_point(s: &str) -> Result<Point, String> {
    let v = parse_numbers(s, "point")?;
    Ok(Point::new(v[0], v[1]))
}

fn parse_points(s: &str) -> Result<Vec<Point>, String> {
    let points: Vec<Point> = s
        .split_whitespace()
        .map(parse_point)
        .collect::<Result<_, _>>()?;
    if points.is_empty() {
        return Err("--stroke needs at least one x,y".into());
    }
    Ok(points)
}

fn parse_picks(s: &str) -> Result<Vec<usize>, String> {
    let mut v: Vec<usize> = s
        .split(',')
        .map(|t| t.trim().parse::<usize>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("--pick {s:?}: expected numbers like 1,3"))?;
    v.sort_unstable();
    v.dedup();
    Ok(v)
}

fn check_inside(input: &Input, (w, h): (u32, u32)) -> Result<(), String> {
    let points: &[Point] = match input {
        Input::Point(p) => std::slice::from_ref(p),
        Input::Stroke(s) => &s.points,
    };
    for q in points {
        if !(0.0..=w as f32).contains(&q.x) || !(0.0..=h as f32).contains(&q.y) {
            return Err(format!(
                "({}, {}) is outside the {w}×{h} image (after its EXIF orientation)",
                q.x, q.y
            ));
        }
    }
    Ok(())
}

// ---- Drawing ----

enum Look {
    /// A candidate that is neither chosen nor offered: a thin white outline.
    Other,
    /// The object to remove: tinted with a bright edge.
    Chosen,
    /// One of several offered, with its number.
    Numbered {
        number: usize,
        pin: Option<Point>,
        picked: bool,
    },
}

struct Shown {
    logits: Logits,
    look: Look,
}

/// A window of the source drawn at `scale` source pixels per view pixel.
struct View {
    img: Rgb8,
    origin: Point,
    scale: f32,
}

impl View {
    /// `window` of the image, reduced to at most `VIEW_LONG` on its long side and enlarged to at
    /// least `VIEW_MIN` so small objects stay legible.
    fn new(image: &Rgb8, window: RectPx) -> View {
        let long = window.width.max(window.height).max(1);
        let target = long.clamp(VIEW_MIN, VIEW_LONG);
        let crop = image.crop(window);
        let img = if target == long {
            crop
        } else {
            let (w, h) = crop.long_side_size(target);
            if target > long {
                crop.resized(w, h)
            } else {
                segment::resample_rgb8(&crop, w, h)
            }
        };
        View {
            scale: window.width as f32 / img.width as f32,
            origin: Point::new(window.x as f32, window.y as f32),
            img,
        }
    }

    fn at(&self, p: Point) -> (f32, f32) {
        (
            (p.x - self.origin.x) / self.scale,
            (p.y - self.origin.y) / self.scale,
        )
    }
}

/// The overlay's window: the stroke's box or the point, and the objects chosen or offered, padded
/// by half their size and at least `VIEW_MIN` source pixels across, within the image.
fn overlay_window(
    image: (u32, u32),
    shown: &[Shown],
    prompts: &[Prompt],
    stroke: Option<&Stroke>,
) -> RectPx {
    let mut lo = Point::new(f32::MAX, f32::MAX);
    let mut hi = Point::new(f32::MIN, f32::MIN);
    let mut grow = |a: Point, b: Point| {
        lo = Point::new(lo.x.min(a.x), lo.y.min(a.y));
        hi = Point::new(hi.x.max(b.x), hi.y.max(b.y));
    };
    if let Some(s) = stroke {
        for p in &s.points {
            grow(
                Point::new(p.x - s.radius, p.y - s.radius),
                Point::new(p.x + s.radius, p.y + s.radius),
            );
        }
    }
    for p in prompts.iter().flat_map(|p| &p.positive) {
        grow(*p, *p);
    }
    for s in shown.iter().filter(|s| !matches!(s.look, Look::Other)) {
        if let Some([x, y, w, h]) = raster::positive_bounds(&s.logits) {
            grow(Point::new(x, y), Point::new(x + w, y + h));
        }
    }
    let (iw, ih) = (image.0 as f32, image.1 as f32);
    if lo.x > hi.x {
        return RectPx {
            x: 0,
            y: 0,
            width: image.0,
            height: image.1,
        };
    }
    let pad = |a: f32, b: f32, size: f32| {
        let reach = ((b - a) * 1.5).max(VIEW_MIN as f32).min(size);
        let centre = (a + b) / 2.0;
        let start = (centre - reach / 2.0).clamp(0.0, size - reach);
        (start.floor() as u32, reach.ceil() as u32)
    };
    let (x, width) = pad(lo.x, hi.x, iw);
    let (y, height) = pad(lo.y, hi.y, ih);
    RectPx {
        x,
        y,
        width: width.min(image.0 - x),
        height: height.min(image.1 - y),
    }
}

fn blend(img: &mut Rgb8, x: i64, y: i64, c: [u8; 3], alpha: f32) {
    if x < 0 || y < 0 || x >= img.width as i64 || y >= img.height as i64 {
        return;
    }
    let i = (y as usize * img.width as usize + x as usize) * 3;
    for (o, c) in img.data[i..i + 3].iter_mut().zip(c) {
        *o = (*o as f32 * (1.0 - alpha) + c as f32 * alpha).round() as u8;
    }
}

/// Paints a candidate over the view: `tint` inside at `alpha`, `edge` on its boundary.
fn paint(v: &mut View, l: &Logits, tint: Option<([u8; 3], f32)>, edge: [u8; 3]) {
    let Some([x, y, w, h]) = raster::positive_bounds(l) else {
        return;
    };
    let (ax, ay) = v.at(Point::new(x, y));
    let (bx, by) = v.at(Point::new(x + w, y + h));
    let x0 = ax.floor().max(0.0) as usize;
    let y0 = ay.floor().max(0.0) as usize;
    let x1 = (bx.ceil().max(0.0) as usize).min(v.img.width as usize);
    let y1 = (by.ceil().max(0.0) as usize).min(v.img.height as usize);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let s = Sampling {
        origin: Point::new(
            v.origin.x + x0 as f32 * v.scale,
            v.origin.y + y0 as f32 * v.scale,
        ),
        step: (v.scale, v.scale),
        size: (x1 - x0, y1 - y0),
    };
    let bits = raster::inside(l, &s);
    let (sw, sh) = s.size;
    let at = |i: isize, j: isize| {
        i >= 0
            && j >= 0
            && (i as usize) < sw
            && (j as usize) < sh
            && bits[j as usize * sw + i as usize]
    };
    for j in 0..sh as isize {
        for i in 0..sw as isize {
            if !at(i, j) {
                continue;
            }
            let (px, py) = (x0 as i64 + i as i64, y0 as i64 + j as i64);
            let boundary = !at(i - 1, j) || !at(i + 1, j) || !at(i, j - 1) || !at(i, j + 1);
            if boundary {
                for (dx, dy) in [(0, 0), (1, 0), (0, 1)] {
                    blend(&mut v.img, px + dx, py + dy, edge, 1.0);
                }
            } else if let Some((c, a)) = tint {
                blend(&mut v.img, px, py, c, a);
            }
        }
    }
}

fn ring(img: &mut Rgb8, (cx, cy): (f32, f32), r: f32, c: [u8; 3]) {
    let reach = (r + 3.0).ceil() as i64;
    for y in (cy as i64 - reach)..=(cy as i64 + reach) {
        for x in (cx as i64 - reach)..=(cx as i64 + reach) {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            if (r - 1.5..=r).contains(&d) {
                blend(img, x, y, c, 1.0);
            } else if (r..=r + 1.5).contains(&d) || (r - 3.0..r - 1.5).contains(&d) {
                blend(img, x, y, [0, 0, 0], 1.0);
            }
        }
    }
}

/// 5 × 7 digits, one row per byte, the leftmost column in bit 4.
const DIGITS: [[u8; 7]; 10] = [
    [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
    [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
    [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
    [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
    [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
    [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
    [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
    [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
    [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
    [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
];

/// Glyph pixels per font dot.
const PIN_DOT: i64 = 2;

/// A numbered pin: a disc in `c` (white when picked) with a black ring and the number in black.
fn pin(img: &mut Rgb8, (cx, cy): (f32, f32), number: usize, c: [u8; 3], picked: bool) {
    let text = number.to_string();
    let k = PIN_DOT;
    let width = text.len() as i64 * 6 * k - k;
    let r = (width.max(7 * k) as f32) * 0.5 + 2.0 * k as f32;
    let reach = (r + 2.0).ceil() as i64;
    for y in (cy as i64 - reach)..=(cy as i64 + reach) {
        for x in (cx as i64 - reach)..=(cx as i64 + reach) {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            if d <= r {
                blend(img, x, y, if picked { [255, 255, 255] } else { c }, 1.0);
            } else if d <= r + 2.0 {
                blend(img, x, y, [0, 0, 0], 1.0);
            }
        }
    }
    let (ox, oy) = (cx as i64 - width / 2, cy as i64 - 7 * k / 2);
    for (n, ch) in text.bytes().enumerate() {
        let glyph = DIGITS[(ch - b'0') as usize];
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                if bits & (0x10 >> col) == 0 {
                    continue;
                }
                for dy in 0..k {
                    for dx in 0..k {
                        blend(
                            img,
                            ox + n as i64 * 6 * k + col * k + dx,
                            oy + row as i64 * k + dy,
                            [0, 0, 0],
                            1.0,
                        );
                    }
                }
            }
        }
    }
}

/// The candidates around the stroke or point: the stroke translucent yellow, other survivors
/// outlined white, the chosen object tinted blue, several objects each outlined in its colour with
/// a numbered pin (white when picked); prompt points as green rings and the stroke's box in
/// magenta. The view is the window `overlay_window` gives.
fn write_overlay(
    paths: &Outputs,
    image: &Rgb8,
    shown: &[Shown],
    prompts: &[Prompt],
    stroke: Option<&Stroke>,
) -> Result<(), String> {
    let window = overlay_window((image.width, image.height), shown, prompts, stroke);
    let mut v = View::new(image, window);
    if let Some(s) = stroke {
        let scaled = Stroke {
            points: s
                .points
                .iter()
                .map(|&p| {
                    let (x, y) = v.at(p);
                    Point::new(x, y)
                })
                .collect(),
            radius: s.radius / v.scale,
        };
        let m = refine::stroke_mask(&scaled, (v.img.width, v.img.height));
        let w = m.window;
        for y in w.y..w.y + w.height {
            for x in w.x..w.x + w.width {
                if m.get(x, y) > 0 {
                    blend(&mut v.img, x as i64, y as i64, [255, 230, 0], 0.3);
                }
            }
        }
    }
    for s in shown.iter().filter(|s| matches!(s.look, Look::Other)) {
        paint(&mut v, &s.logits, None, [235, 235, 235]);
    }
    for s in shown {
        match s.look {
            Look::Other => {}
            Look::Chosen => paint(
                &mut v,
                &s.logits,
                Some(([30, 144, 255], 0.45)),
                [0, 255, 255],
            ),
            Look::Numbered { number, picked, .. } => {
                let c = raster::palette(number - 1);
                paint(
                    &mut v,
                    &s.logits,
                    Some((c, if picked { 0.5 } else { 0.2 })),
                    c,
                );
            }
        }
    }
    let r = (v.img.width.max(v.img.height) as f32 / 300.0).max(4.0);
    for p in prompts {
        for q in &p.positive {
            let at = v.at(*q);
            ring(&mut v.img, at, r, [0, 230, 0]);
        }
        if let Some((a, b)) = p.bbox {
            let ((ax, ay), (bx, by)) = (v.at(a), v.at(b));
            for x in ax as i64..=bx as i64 {
                for t in -1..=1 {
                    blend(&mut v.img, x, ay as i64 + t, [255, 0, 255], 1.0);
                    blend(&mut v.img, x, by as i64 + t, [255, 0, 255], 1.0);
                }
            }
            for y in ay as i64..=by as i64 {
                for t in -1..=1 {
                    blend(&mut v.img, ax as i64 + t, y, [255, 0, 255], 1.0);
                    blend(&mut v.img, bx as i64 + t, y, [255, 0, 255], 1.0);
                }
            }
        }
    }
    for s in shown {
        if let Look::Numbered {
            number,
            pin: Some(p),
            picked,
        } = s.look
        {
            let at = v.at(p);
            pin(&mut v.img, at, number, raster::palette(number - 1), picked);
        }
    }
    v.img.save_png(&paths.overlay)
}

/// Before (with the region tinted red) and after, side by side, cropped around the region.
fn write_compare(
    path: &Path,
    before: &Rgb8,
    after: &Rgb8,
    region: &Mask,
    bounds: RectPx,
) -> Result<(), String> {
    let pad = (bounds.width.max(bounds.height) / 4).max(32);
    let crop = bounds.grown(pad, (before.width, before.height));
    let (mut a, b) = (before.crop(crop), after.crop(crop));
    for y in 0..crop.height {
        for x in 0..crop.width {
            let c = region.get(crop.x + x, crop.y + y);
            if c > 0 {
                blend(
                    &mut a,
                    x as i64,
                    y as i64,
                    [255, 0, 0],
                    0.4 * c as f32 / 255.0,
                );
            }
        }
    }
    let gap = 8;
    let (w, h) = (crop.width * 2 + gap, crop.height);
    let mut both = Rgb8::new(w, h, vec![255; w as usize * h as usize * 3]);
    for (img, x0) in [(&a, 0), (&b, crop.width + gap)] {
        for y in 0..h {
            let src = &img.data[(y * crop.width) as usize * 3..((y + 1) * crop.width) as usize * 3];
            let start = ((y * w + x0) * 3) as usize;
            both.data[start..start + src.len()].copy_from_slice(src);
        }
    }
    let long = w.max(h);
    let shown = if long > VIEW_LONG {
        let (sw, sh) = both.long_side_size(VIEW_LONG);
        segment::resample_rgb8(&both, sw, sh)
    } else {
        both
    };
    shown.save_png(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_parse() {
        assert_eq!(parse_point("1.5, 2").unwrap(), Point::new(1.5, 2.0));
        assert!(parse_point("1,2,3").is_err());
        assert_eq!(
            parse_points("1,2 3,4").unwrap(),
            vec![Point::new(1.0, 2.0), Point::new(3.0, 4.0)]
        );
        assert!(parse_points("  ").is_err());
        assert_eq!(parse_picks("3,1,3").unwrap(), vec![1, 3]);
        assert!(parse_picks("1,a").is_err());
    }

    #[test]
    fn points_outside_the_image_are_refused() {
        let s = Input::Stroke(Stroke {
            points: vec![Point::new(5.0, 5.0), Point::new(5.0, 50.0)],
            radius: 2.0,
        });
        assert!(check_inside(&s, (100, 40)).is_err());
        assert!(check_inside(&s, (40, 100)).is_ok());
    }

    #[test]
    fn outside_changes_count_only_uncovered_pixels() {
        let before = Rgb8::new(4, 1, vec![0; 12]);
        let mut after = before.clone();
        after.data[0] = 9; // pixel 0, covered
        after.data[9] = 9; // pixel 3, not covered
        let region = Mask::new(
            (4, 1),
            RectPx {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            vec![255],
        );
        assert_eq!(outside_changes(&before, &after, &region), 1);
    }

    #[test]
    fn an_output_that_is_the_input_is_refused() {
        let dir = std::env::temp_dir().join(format!("remove-out-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("result.png");
        std::fs::write(&input, b"x").unwrap();
        assert!(Outputs::new(&dir).check_not(&input).is_err());
        let elsewhere = dir.join("sub");
        assert!(Outputs::new(&elsewhere).check_not(&input).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn digits_draw_inside_their_pin() {
        let mut img = Rgb8::new(80, 80, vec![128; 80 * 80 * 3]);
        pin(&mut img, (40.0, 40.0), 12, [255, 0, 0], false);
        assert_eq!(img.pixel(40, 29), [255, 0, 0]);
        assert_eq!(img.pixel(2, 2), [128, 128, 128]);
        // Some black glyph pixels near the centre.
        let black = (32..48)
            .flat_map(|y| (28..52).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixel(x, y) == [0, 0, 0])
            .count();
        assert!(black > 40, "{black}");
    }
}
