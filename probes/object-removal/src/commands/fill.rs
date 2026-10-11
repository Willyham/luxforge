//! `removal-probe fill`: remove a masked region with LaMa, or paste a synthetic object with a
//! shadow onto an image, remove it, and score the fill against the background it covered.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};

use crate::args::Args;
use crate::fill::{self, Details, Params, Report};
use crate::image::Rgb8;
use crate::mask::{Mask, RectPx};
use crate::models;
use crate::runtime::{self, Backend};

pub const USAGE: &str = "  fill --image IMG (--mask MASK.png | --paste [--paste-at X,Y] [--paste-size W,H] [--dilate N])
       [--backend B] [--fill-model lama|lama-fp32] [--models DIR] [--seed N] [--margin F] [--no-grain] [--no-pyramid] [--bgr | --rgb] --out DIR
                                           fill a region with LaMa; --paste adds a synthetic object and
                                           shadow, removes it and scores the fill against what it covered";

pub fn run(mut a: Args) -> Result<(), String> {
    let started = Instant::now();
    let image_path = a.path("--image")?.ok_or("--image is required")?;
    let mask_path = a.path("--mask")?;
    let paste = a.flag("--paste");
    let paste_at = a
        .opt("--paste-at")?
        .map(|v| pair(&v, "--paste-at"))
        .transpose()?;
    let paste_size = a
        .opt("--paste-size")?
        .map(|v| pair(&v, "--paste-size"))
        .transpose()?;
    let dilation: u32 = a.parsed("--dilate")?.unwrap_or(6);
    let backend: Backend = a.parsed("--backend")?.unwrap_or(Backend::OrtCpu);
    let models_dir = models::directory(a.path("--models")?);
    let mut params = Params::default();
    if let Some(seed) = a.parsed("--seed")? {
        params.seed = seed;
    }
    if let Some(margin) = a.parsed("--margin")? {
        params.margin = margin;
    }
    params.grain = !a.flag("--no-grain");
    params.pyramid_blend = !a.flag("--no-pyramid");
    match (a.flag("--bgr"), a.flag("--rgb")) {
        (true, true) => return Err("--bgr and --rgb exclude each other".into()),
        (true, false) => params.bgr = true,
        (false, true) => params.bgr = false,
        (false, false) => {}
    }
    let fill_model: models::FillModel = a.parsed("--fill-model")?.unwrap_or_default();
    let out = a.path("--out")?.ok_or("--out is required")?;
    a.done()?;
    if paste == mask_path.is_some() {
        return Err("give exactly one of --mask and --paste".into());
    }

    let image = Rgb8::load(&image_path)?;
    let mask_name = mask_path.as_ref().map(|p| p.display().to_string());
    let (input, region, truth) = if paste {
        let size = paste_size.unwrap_or_else(|| {
            let m = image.width.min(image.height);
            (m * 3 / 20, m * 9 / 100)
        });
        let centre = paste_at.unwrap_or((image.width / 2, image.height / 2));
        let p = paste_object(&image, centre, size, dilation);
        (p.image, p.region, Some(image))
    } else {
        let path = mask_path.expect("checked above");
        let region = Mask::load_png(&path)?;
        if region.image != (image.width, image.height) {
            return Err(format!(
                "{}: the mask is {}×{}, the image {}×{}",
                path.display(),
                region.image.0,
                region.image.1,
                image.width,
                image.height
            ));
        }
        (image, region, None)
    };

    let lama = fill_model.model().verified(&models_dir)?;
    let (mut session, load) = runtime::open(backend, &lama)?;
    let fill_start = Instant::now();
    let (result, report, details) =
        fill::remove_detailed(&input, &region, session.as_mut(), &params)?;
    let total_ms = fill_start.elapsed().as_secs_f64() * 1e3;

    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    result.save_png(&out.join("result.png"))?;
    region.save_png(&out.join("region.png"))?;
    details.model_input.save_png(&out.join("model-in.png"))?;
    details.model_output.save_png(&out.join("model-out.png"))?;
    save_grey(
        &details.model_mask,
        params.model_size,
        &out.join("model-mask.png"),
    )?;
    let mut json = report_json(&report, &details, &params, total_ms);
    json["image"] = json!(image_path.display().to_string());
    json["backend"] = json!(backend.name());
    json["fill_model"] = json!(fill_model.name());
    json["model_load_ms"] = json!(load.as_secs_f64() * 1e3);
    if let Some(truth) = &truth {
        input.save_png(&out.join("pasted.png"))?;
        json["truth"] = truth_json(&result, truth, &region);
    } else {
        json["mask"] = json!(mask_name);
    }
    json["command_ms"] = json!(started.elapsed().as_secs_f64() * 1e3);
    write_json(&out.join("report.json"), &json)?;
    println!("{}", serde_json::to_string_pretty(&json).expect("json"));
    Ok(())
}

fn pair(v: &str, name: &str) -> Result<(u32, u32), String> {
    let (a, b) = v
        .split_once(',')
        .ok_or_else(|| format!("{name}: expected A,B, got {v:?}"))?;
    let p = |s: &str| {
        s.trim()
            .parse::<u32>()
            .map_err(|_| format!("{name}: cannot parse {v:?}"))
    };
    Ok((p(a)?, p(b)?))
}

fn save_grey(data: &[u8], n: u32, path: &Path) -> Result<(), String> {
    image::GrayImage::from_raw(n, n, data.to_vec())
        .expect("buffer matches its size")
        .save(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).expect("json");
    std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
}

fn rect_json(r: RectPx) -> Value {
    json!({"x": r.x, "y": r.y, "width": r.width, "height": r.height})
}

/// The report, the decisions behind it and the total milliseconds, as JSON.
pub fn report_json(report: &Report, details: &Details, params: &Params, total_ms: f64) -> Value {
    let timings: serde_json::Map<String, Value> = report
        .timings_ms
        .iter()
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    json!({
        "params": {
            "margin": params.margin,
            "min_margin": params.min_margin,
            "model_size": params.model_size,
            "grain": params.grain,
            "pyramid_blend": params.pyramid_blend,
            "seed": params.seed,
            "bgr": params.bgr,
        },
        "crop": rect_json(report.crop),
        "square": {
            "origin": [details.plan.origin.0, details.plan.origin.1],
            "side": details.plan.side,
            "context": details.plan.context,
            "padding_left_top_right_bottom": details.plan.padding(),
        },
        "region_bounds": rect_json(details.plan.bounds),
        "scale": report.scale,
        "grain_sigma": report.grain_sigma,
        "fill_sigma": details.fill_sigma,
        "grain_added": details.grain_added,
        "grain_correlation": details.grain_correlation,
        "grain_correlation_px": details.grain_correlation_px,
        "ring_width": details.ring_width,
        "model_output_range": [details.output_range.0, details.output_range.1],
        "window": rect_json(details.window),
        "pyramid_levels": details.levels,
        "seam_band_px": details.seam_band,
        "timings_ms": Value::Object(timings),
        "total_ms": total_ms,
    })
}

/// PSNR and texture inside the region against the background the object covered.
pub fn truth_json(result: &Rgb8, truth: &Rgb8, region: &Mask) -> Value {
    json!({
        "psnr_inside_db": psnr_inside(result, truth, region),
        "psnr_edge_band_db": psnr_inside(result, truth, &edge_band(region, 8)),
        "highpass_sigma_truth": fill::highpass_sigma_inside(truth, region),
        "highpass_sigma_result": fill::highpass_sigma_inside(result, region),
        "region_pixels": region.data.iter().filter(|&&v| v > 0).count(),
    })
}

/// A synthetic object pasted onto a background, and the region that removes it.
pub struct Pasted {
    pub image: Rgb8,
    /// The object and its shadow, dilated, at full coverage.
    pub region: Mask,
    /// The object's own bounds, without its shadow.
    pub object: RectPx,
}

/// Signed distance to a rounded box centred at `(cx, cy)` with half sizes `(hw, hh)`.
fn rounded_box(p: (f32, f32), c: (f32, f32), half: (f32, f32), r: f32) -> f32 {
    let qx = (p.0 - c.0).abs() - half.0 + r;
    let qy = (p.1 - c.1).abs() - half.1 + r;
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

/// Pastes an opaque, anti-aliased object of `size` centred at `centre` (a red body with a dark
/// window and a light stripe, shaded top to bottom) with a soft shadow cast down and to the right,
/// and returns the region covering every changed pixel, dilated by `dilation`.
pub fn paste_object(
    background: &Rgb8,
    centre: (u32, u32),
    size: (u32, u32),
    dilation: u32,
) -> Pasted {
    let (w, h) = (background.width, background.height);
    let (cx, cy) = (centre.0 as f32, centre.1 as f32);
    let (hw, hh) = (size.0 as f32 / 2.0, size.1 as f32 / 2.0);
    let radius = 0.3 * hw.min(hh);
    let shadow_c = (cx + 0.10 * size.0 as f32, cy + 0.20 * size.1 as f32);
    let soft = (0.12 * size.1 as f32).max(2.0);
    let reach = (0.6 * size.0 as f32 + soft + 2.0).max(0.7 * size.1 as f32 + soft + 2.0);
    let x0 = (cx - reach).max(0.0) as u32;
    let y0 = (cy - reach).max(0.0) as u32;
    let x1 = ((cx + reach) as u32 + 1).min(w);
    let y1 = ((cy + reach) as u32 + 1).min(h);
    let mut image = background.clone();
    let mut changed = vec![false; (w * h) as usize];
    for y in y0..y1 {
        for x in x0..x1 {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let d = rounded_box(p, (cx, cy), (hw, hh), radius);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            let ds = rounded_box(p, shadow_c, (hw * 1.02, hh), radius);
            let t = (0.5 - ds / soft).clamp(0.0, 1.0);
            let mut alpha = 0.55 * t * t * (3.0 - 2.0 * t);
            if alpha < 0.02 {
                alpha = 0.0;
            }
            if cov == 0.0 && alpha == 0.0 {
                continue;
            }
            // The object's colour: shaded body, window, stripe.
            let v = ((p.1 - (cy - hh)) / (2.0 * hh)).clamp(0.0, 1.0);
            let shade = 1.15 - 0.35 * v;
            let mut colour = [196.0 * shade, 38.0 * shade, 42.0 * shade];
            let dw = rounded_box(
                p,
                (cx, cy - 0.18 * size.1 as f32),
                (0.64 * hw, 0.32 * hh),
                0.2 * hh,
            );
            let win = (0.5 - dw).clamp(0.0, 1.0);
            let stripe = (1.0 - ((p.1 - (cy + 0.24 * hh)).abs() - 0.04 * hh).clamp(0.0, 1.0))
                * ((hw * 0.9 - (p.0 - cx).abs()).clamp(0.0, 1.0));
            let window_colour = [38.0f32, 48.0, 62.0];
            let stripe_colour = [232.0f32, 230.0, 222.0];
            for ((c, wc), sc) in colour.iter_mut().zip(window_colour).zip(stripe_colour) {
                *c = *c * (1.0 - win) + wc * win;
                *c = *c * (1.0 - stripe) + sc * stripe;
            }
            let i = ((y * w + x) * 3) as usize;
            for (c, &oc) in colour.iter().enumerate() {
                let bg = background.data[i + c] as f32 * (1.0 - alpha);
                image.data[i + c] = (bg * (1.0 - cov) + oc * cov).round().clamp(0.0, 255.0) as u8;
            }
            changed[(y * w + x) as usize] = true;
        }
    }
    let grown = fill::dilate(&changed, w as usize, h as usize, dilation as usize);
    let region = Mask::new(
        (w, h),
        RectPx {
            x: 0,
            y: 0,
            width: w,
            height: h,
        },
        grown.iter().map(|&g| if g { 255 } else { 0 }).collect(),
    );
    let object = RectPx {
        x: (cx - hw).max(0.0) as u32,
        y: (cy - hh).max(0.0) as u32,
        width: size.0,
        height: size.1,
    };
    Pasted {
        image,
        region,
        object,
    }
}

/// The region's pixels within `width` of its outside (a square neighbourhood): where a seam shows.
pub fn edge_band(region: &Mask, width: u32) -> Mask {
    let (w, h) = region.image;
    let outside: Vec<bool> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| region.get(x, y) == 0)
        .collect();
    let near = fill::dilate(&outside, w as usize, h as usize, width as usize);
    let data = outside
        .iter()
        .zip(&near)
        .map(|(&o, &n)| if !o && n { 255 } else { 0 })
        .collect();
    Mask::new(
        (w, h),
        RectPx {
            x: 0,
            y: 0,
            width: w,
            height: h,
        },
        data,
    )
}

/// PSNR over the pixels with non-zero coverage, all channels, in dB.
pub fn psnr_inside(a: &Rgb8, b: &Rgb8, region: &Mask) -> f64 {
    let (mut se, mut n) = (0f64, 0u64);
    for y in 0..a.height {
        for x in 0..a.width {
            if region.get(x, y) == 0 {
                continue;
            }
            let (p, q) = (a.pixel(x, y), b.pixel(x, y));
            for c in 0..3 {
                let d = p[c] as f64 - q[c] as f64;
                se += d * d;
                n += 1;
            }
        }
    }
    if se == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0f64 * 255.0 / (se / n as f64)).log10()
}

/// The evidence directory for this task's model runs.
pub fn evidence_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/evidence/task-003")
        .join(name);
    std::fs::create_dir_all(&dir).expect("evidence directory");
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::tests::{FakeLama, mask_from, noise_image};

    fn corpus(name: &str) -> PathBuf {
        std::env::var_os("LUXFORGE_REMOVAL_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from("/Users/will/projects/lightwell/private/object-removal/corpus")
            })
            .join(name)
    }

    fn backend() -> Backend {
        std::env::var("LUXFORGE_REMOVAL_BACKEND")
            .ok()
            .map(|b| b.parse().expect("backend"))
            .unwrap_or(Backend::OrtCpu)
    }

    fn lama() -> Box<dyn runtime::Session> {
        let path = models::LAMA
            .verified(&models::directory(None))
            .expect("the pinned LaMa model");
        runtime::open(backend(), &path).expect("LaMa loads").0
    }

    #[test]
    fn fill_paste_changes_only_inside_its_region() {
        let bg = noise_image(400, 300, 10.0, 1);
        let p = paste_object(&bg, (200, 150), (120, 70), 4);
        let mut inside = 0;
        for y in 0..300 {
            for x in 0..400 {
                if p.region.get(x, y) == 0 {
                    assert_eq!(bg.pixel(x, y), p.image.pixel(x, y), "({x}, {y})");
                } else {
                    inside += 1;
                }
            }
        }
        assert!(inside > 120 * 70, "{inside}");
        // The object is opaque: its centre is the object's colour, not the background.
        assert!(p.image.pixel(200, 150)[0] > 150);
        assert_eq!(psnr_inside(&bg, &bg, &p.region), f64::INFINITY);
        // The fake model's fill scores something finite.
        let (out, _) = fill::remove(
            &p.image,
            &p.region,
            &mut FakeLama::default(),
            &Params::default(),
        )
        .unwrap();
        assert!(psnr_inside(&out, &bg, &p.region).is_finite());
    }

    /// Mean absolute error per channel inside the region, and over pixels at least `flat` away
    /// from both quadrant boundaries.
    fn quadrant_errors(
        out: &Rgb8,
        truth: &Rgb8,
        region: &Mask,
        split: (u32, u32),
        flat: u32,
    ) -> Value {
        let mut all = [0f64; 3];
        let mut signed = [0f64; 3];
        let mut far = [0f64; 3];
        let (mut n, mut nf) = (0f64, 0f64);
        for y in 0..out.height {
            for x in 0..out.width {
                if region.get(x, y) == 0 {
                    continue;
                }
                let (p, q) = (out.pixel(x, y), truth.pixel(x, y));
                let is_far = x.abs_diff(split.0) >= flat && y.abs_diff(split.1) >= flat;
                for c in 0..3 {
                    let d = p[c] as f64 - q[c] as f64;
                    all[c] += d.abs();
                    signed[c] += d;
                    if is_far {
                        far[c] += d.abs();
                    }
                }
                n += 1.0;
                if is_far {
                    nf += 1.0;
                }
            }
        }
        json!({
            "mean_abs_error": all.map(|v| v / n),
            "mean_abs_error_all_channels": all.iter().sum::<f64>() / (3.0 * n),
            "mean_signed_error": signed.map(|v| v / n),
            "mean_abs_error_away_from_boundaries": far.map(|v| v / nf),
            "mean_abs_error_away_from_boundaries_all_channels": far.iter().sum::<f64>() / (3.0 * nf),
            "pixels": n,
            "pixels_away_from_boundaries": nf,
        })
    }

    /// Margins the model experiments compare: the skeleton's 0.4 (OpenStill's rule) and wider.
    const MARGINS: [f32; 3] = [0.4, 1.0, 1.5];

    fn order_name(bgr: bool) -> &'static str {
        if bgr { "bgr" } else { "rgb" }
    }

    #[test]
    #[ignore = "needs the pinned models and corpus"]
    fn fill_channel_order_on_quadrants() {
        let image = Rgb8::load(&corpus("quadrants-24mp.jpg")).unwrap();
        let (w, h) = (image.width, image.height);
        let split = (w / 2, h / 2);
        let dir = evidence_dir("channel-order");
        let mut lama = lama();
        let mut results = serde_json::Map::new();
        // A square hole and a wide one, both straddling both boundaries, grain and pyramid off so
        // only the model's colours are compared.
        for (name, half) in [("square-800", (400, 400)), ("wide-1600x600", (800, 300))] {
            let hole = RectPx {
                x: split.0 - half.0,
                y: split.1 - half.1,
                width: 2 * half.0,
                height: 2 * half.1,
            };
            let region = mask_from((w, h), |x, y| if hole.contains(x, y) { 255 } else { 0 });
            for margin in MARGINS {
                for bgr in [false, true] {
                    let params = Params {
                        grain: false,
                        pyramid_blend: false,
                        bgr,
                        margin,
                        ..Params::default()
                    };
                    let (out, report, details) =
                        fill::remove_detailed(&image, &region, lama.as_mut(), &params).unwrap();
                    let tag = format!("{name}-margin-{margin}-{}", order_name(bgr));
                    let mut e = quadrant_errors(&out, &image, &region, split, 60);
                    e["model_output_range"] =
                        json!([details.output_range.0, details.output_range.1]);
                    e["scale"] = json!(report.scale);
                    println!(
                        "{tag}: all {:.2}, away from boundaries {:.2}",
                        e["mean_abs_error_all_channels"],
                        e["mean_abs_error_away_from_boundaries_all_channels"]
                    );
                    details
                        .model_output
                        .save_png(&dir.join(format!("{tag}-model-out.png")))
                        .unwrap();
                    results.insert(tag, e);
                }
            }
        }
        // Flat colours carry no natural colour prior, so the same comparison on holes over real
        // content in the natural corpus images, whose original pixels are the truth.
        let mut holes = Vec::new();
        let mut sums = [[0f64; 2]; MARGINS.len()];
        let mut bgr_wins = [0usize; MARGINS.len()];
        for file in [
            "sapa-road.jpg",
            "z6-bridge.jpg",
            "x100vi-sunflowers.jpg",
            "air2s-kayaks.jpg",
        ] {
            let image = Rgb8::load(&corpus(file)).unwrap();
            let (w, h) = (image.width, image.height);
            let stem = file.trim_end_matches(".jpg");
            for (k, side) in [160u32, 240, 320, 400, 480, 560].into_iter().enumerate() {
                let hash = |salt: u64| {
                    let mut z = (k as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)
                        ^ salt
                        ^ (file.len() as u64 * 7919);
                    z = (z ^ (z >> 31)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                    (z ^ (z >> 29)) as f64 / u64::MAX as f64
                };
                let x = (w as f64 * (0.15 + 0.7 * hash(1))) as u32 - side / 2;
                let y = (h as f64 * (0.15 + 0.7 * hash(2))) as u32 - side / 2;
                let hole = RectPx {
                    x,
                    y,
                    width: side,
                    height: side,
                };
                let region =
                    mask_from((w, h), |px, py| if hole.contains(px, py) { 255 } else { 0 });
                let view = hole.grown(side / 2, (w, h));
                let keep = k == 0 || k == 3;
                if keep {
                    image
                        .crop(view)
                        .save_png(&dir.join(format!("natural-{stem}-{side}-truth.png")))
                        .unwrap();
                }
                let mut row = json!({"image": file, "hole": [x, y, side]});
                let mut line = format!("{file} {side}:");
                for (m, margin) in MARGINS.into_iter().enumerate() {
                    let mut psnr = [0f64; 2];
                    for (o, bgr) in [false, true].into_iter().enumerate() {
                        let params = Params {
                            grain: false,
                            pyramid_blend: false,
                            bgr,
                            margin,
                            ..Params::default()
                        };
                        let (out, _) =
                            fill::remove(&image, &region, lama.as_mut(), &params).unwrap();
                        psnr[o] = psnr_inside(&out, &image, &region);
                        sums[m][o] += psnr[o];
                        row[format!("psnr_margin_{margin}_{}", order_name(bgr))] = json!(psnr[o]);
                        if keep {
                            let name = format!(
                                "natural-{stem}-{side}-margin-{margin}-{}.png",
                                order_name(bgr)
                            );
                            out.crop(view).save_png(&dir.join(name)).unwrap();
                        }
                    }
                    bgr_wins[m] += usize::from(psnr[1] > psnr[0]);
                    line += &format!("  margin {margin}: RGB {:.2} BGR {:.2}", psnr[0], psnr[1]);
                }
                println!("{line}");
                holes.push(row);
            }
        }
        let count = holes.len() as f64;
        let mut summary = Vec::new();
        for (m, margin) in MARGINS.into_iter().enumerate() {
            let row = json!({
                "margin": margin,
                "mean_psnr_rgb": sums[m][0] / count,
                "mean_psnr_bgr": sums[m][1] / count,
                "bgr_wins": bgr_wins[m],
                "rgb_wins": holes.len() - bgr_wins[m],
            });
            println!("natural margin {margin}: {row}");
            summary.push(row);
        }
        results.insert(
            "natural".into(),
            json!({"holes": holes, "summary": summary}),
        );
        write_json(&dir.join("channel-order.json"), &Value::Object(results)).unwrap();
    }

    /// The part of `sapa-road.jpg` the synthetic objects are pasted on: 2048² around the road.
    const SAPA_CROP: RectPx = RectPx {
        x: 566,
        y: 1176,
        width: 2048,
        height: 2048,
    };

    #[test]
    #[ignore = "needs the pinned models and corpus"]
    fn fill_synthetic_ground_truth() {
        let background = Rgb8::load(&corpus("sapa-road.jpg"))
            .unwrap()
            .crop(SAPA_CROP);
        let dir = evidence_dir("synthetic-truth");
        background.save_png(&dir.join("background.png")).unwrap();
        let mut lama = lama();
        let mut rows = Vec::new();
        // A car-sized object on the road (the model output enlarged) and a small one (reduced at
        // the narrowest margin), each with its shadow, removed with a 6 px dilation.
        for (name, centre, size) in [
            ("large", (1022, 1030), (560, 300)),
            ("small", (1552, 1052), (150, 84)),
        ] {
            let p = paste_object(&background, centre, size, 6);
            p.image
                .save_png(&dir.join(format!("{name}-pasted.png")))
                .unwrap();
            p.region
                .save_png(&dir.join(format!("{name}-region.png")))
                .unwrap();
            let view = grow(p.object.union(&p.region.bounds().unwrap()), 64, SAPA_CROP);
            background
                .crop(view)
                .save_png(&dir.join(format!("{name}-truth.png")))
                .unwrap();
            p.image
                .crop(view)
                .save_png(&dir.join(format!("{name}-pasted-view.png")))
                .unwrap();
            for margin in MARGINS {
                for bgr in [false, true] {
                    for (grain, pyramid_blend) in
                        [(false, false), (true, false), (false, true), (true, true)]
                    {
                        let params = Params {
                            grain,
                            pyramid_blend,
                            bgr,
                            margin,
                            ..Params::default()
                        };
                        let start = Instant::now();
                        let (out, report, details) =
                            fill::remove_detailed(&p.image, &p.region, lama.as_mut(), &params)
                                .unwrap();
                        let total_ms = start.elapsed().as_secs_f64() * 1e3;
                        let on = |b: bool| if b { "on" } else { "off" };
                        let tag = format!(
                            "{name}-margin-{margin}-{}-grain-{}-pyramid-{}",
                            order_name(bgr),
                            on(grain),
                            on(pyramid_blend)
                        );
                        let mut row = report_json(&report, &details, &params, total_ms);
                        row["truth"] = truth_json(&out, &background, &p.region);
                        row["case"] = json!(tag);
                        println!(
                            "{tag}: PSNR {:.2} dB, high-pass σ truth {} result {}",
                            row["truth"]["psnr_inside_db"],
                            row["truth"]["highpass_sigma_truth"],
                            row["truth"]["highpass_sigma_result"]
                        );
                        if grain == pyramid_blend {
                            out.crop(view)
                                .save_png(&dir.join(format!("{tag}.png")))
                                .unwrap();
                        }
                        if grain && pyramid_blend {
                            let stem = format!("{name}-margin-{margin}-{}", order_name(bgr));
                            details
                                .model_input
                                .save_png(&dir.join(format!("{stem}-model-in.png")))
                                .unwrap();
                            details
                                .model_output
                                .save_png(&dir.join(format!("{stem}-model-out.png")))
                                .unwrap();
                        }
                        rows.push(row);
                    }
                }
            }
        }
        write_json(&dir.join("synthetic-truth.json"), &Value::Array(rows)).unwrap();
    }

    fn grow(r: RectPx, by: u32, within: RectPx) -> RectPx {
        r.grown(by, (within.width, within.height))
    }

    #[test]
    #[ignore = "needs the pinned models and corpus"]
    fn fill_sixty_megapixel_timings() {
        let load = Instant::now();
        let image = Rgb8::load(&corpus("quadrants-60mp.jpg")).unwrap();
        let decode_ms = load.elapsed().as_secs_f64() * 1e3;
        let (w, h) = (image.width, image.height);
        let dir = evidence_dir("sixty-megapixel");
        let mut lama = lama();
        let mut rows =
            vec![json!({"decode_ms": decode_ms, "size": [w, h], "backend": backend().name()})];
        // An object of about 1200 × 900 px and one of about 3000 × 2400, both ellipses over the
        // colour boundaries; each run twice so the second is warm.
        for (name, half) in [
            ("ellipse-1200x900", (600.0f32, 450.0f32)),
            ("ellipse-3000x2400", (1500.0, 1200.0)),
        ] {
            let (cx, cy) = (w as f32 / 2.0 + 150.0, h as f32 / 2.0 - 100.0);
            let region = mask_from((w, h), |x, y| {
                let d = ((x as f32 - cx) / half.0).powi(2) + ((y as f32 - cy) / half.1).powi(2);
                if d <= 1.0 { 255 } else { 0 }
            });
            for run in 0..2 {
                let start = Instant::now();
                let (out, report, details) =
                    fill::remove_detailed(&image, &region, lama.as_mut(), &Params::default())
                        .unwrap();
                let total_ms = start.elapsed().as_secs_f64() * 1e3;
                let mut row = report_json(&report, &details, &Params::default(), total_ms);
                row["case"] = json!(format!("{name}-run-{run}"));
                println!("{name} run {run}: {total_ms:.0} ms {}", row["timings_ms"]);
                if run == 1 {
                    let view = details.plan.inside;
                    let small = out.crop(view);
                    let (sw, sh) = small.long_side_size(1024);
                    small
                        .resized(sw, sh)
                        .save_png(&dir.join(format!("{name}-result.png")))
                        .unwrap();
                    details
                        .model_output
                        .save_png(&dir.join(format!("{name}-model-out.png")))
                        .unwrap();
                }
                rows.push(row);
            }
        }
        write_json(&dir.join("timings.json"), &Value::Array(rows)).unwrap();
    }
}
