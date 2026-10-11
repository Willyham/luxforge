//! `removal-probe segment`: encode an image, decode one prompt, and write an overlay per candidate
//! with a JSON summary.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::args::Args;
use crate::image::Rgb8;
use crate::mask::{Logits, Point, RectPx};
use crate::models;
use crate::runtime::Backend;
use crate::segment::{self, Family, Prompt};

pub const USAGE: &str = "  segment --image IMG --family evit-l0|sam21-small [--encoder-backend B] [--decoder-backend B]
          [--point x,y]... [--negative x,y]... [--box x0,y0,x1,y1] [--view x0,y0,x1,y1] [--overlay-size N]
          [--models DIR] --out DIR         decode one prompt; write an overlay PNG per candidate (of the --view
                                           window, at most N px on its long side, 0 = full) and candidates.json";

/// The overlay's default long side: large enough to judge an edge, small enough to open.
const OVERLAY_SIZE: u32 = 2048;

#[derive(Serialize)]
struct Report {
    image: String,
    source: (u32, u32),
    family: &'static str,
    encoder_backend: &'static str,
    decoder_backend: &'static str,
    prompt: PromptJson,
    load_ms: LoadMs,
    image_load_ms: f64,
    encode_ms: f64,
    decode_ms: f64,
    embedding_bytes: usize,
    candidates: Vec<CandidateJson>,
}

#[derive(Serialize)]
struct PromptJson {
    positive: Vec<[f32; 2]>,
    negative: Vec<[f32; 2]>,
    #[serde(rename = "box")]
    bbox: Option<[f32; 4]>,
}

#[derive(Serialize)]
struct LoadMs {
    encoder: f64,
    decoder: f64,
}

#[derive(Serialize)]
struct CandidateJson {
    rank: usize,
    predicted_iou: f32,
    /// Source pixels whose centre samples a logit above 0.
    area_px: u64,
    area_share: f64,
    logits: (usize, usize),
    cells_per_px: (f32, f32),
    overlay: String,
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

pub fn run(mut a: Args) -> Result<(), String> {
    let image_path = PathBuf::from(a.required("--image")?);
    let family: Family = a.required("--family")?.parse()?;
    let encoder: Backend = a.parsed("--encoder-backend")?.unwrap_or(Backend::OrtCpu);
    let decoder: Backend = a.parsed("--decoder-backend")?.unwrap_or(Backend::OrtCpu);
    let positive = a
        .all("--point")?
        .iter()
        .map(|s| point(s))
        .collect::<Result<Vec<_>, _>>()?;
    let negative = a
        .all("--negative")?
        .iter()
        .map(|s| point(s))
        .collect::<Result<Vec<_>, _>>()?;
    let bbox = a.opt("--box")?.map(|s| parse_box(&s)).transpose()?;
    let overlay_size: u32 = a.parsed("--overlay-size")?.unwrap_or(OVERLAY_SIZE);
    let view = a.opt("--view")?.map(|s| parse_box(&s)).transpose()?;
    let models = models::directory(a.path("--models")?);
    let out = PathBuf::from(a.required("--out")?);
    a.done()?;

    let started = Instant::now();
    let image = Rgb8::load(&image_path)?;
    let image_load = started.elapsed();
    let prompt = Prompt {
        positive,
        negative,
        bbox,
        previous: None,
    };
    check_inside(&prompt, (image.width, image.height))?;
    let view = view
        .map(|v| window(v, (image.width, image.height)))
        .transpose()?;

    let (mut segmenter, load) = segment::open_timed(family, encoder, decoder, &models)?;
    let started = Instant::now();
    let embedding = segmenter.encode(&image)?;
    let encode = started.elapsed();
    let started = Instant::now();
    let candidates = segmenter.decode(&embedding, &prompt)?;
    let decode = started.elapsed();

    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let view = View::new(&image, view, overlay_size);
    let total = image.width as f64 * image.height as f64;
    let mut listed = Vec::new();
    for (rank, c) in candidates.iter().enumerate() {
        let name = format!("candidate-{rank}.png");
        overlay(&view, &c.logits, &prompt).save_png(&out.join(&name))?;
        let area = area_px(&c.logits);
        listed.push(CandidateJson {
            rank,
            predicted_iou: c.predicted_iou,
            area_px: area,
            area_share: area as f64 / total,
            logits: (c.logits.width, c.logits.height),
            cells_per_px: c.logits.cells_per_px,
            overlay: name,
        });
    }
    let report = Report {
        image: image_path.display().to_string(),
        source: (image.width, image.height),
        family: family.name(),
        encoder_backend: encoder.name(),
        decoder_backend: decoder.name(),
        prompt: PromptJson {
            positive: prompt.positive.iter().map(|p| [p.x, p.y]).collect(),
            negative: prompt.negative.iter().map(|p| [p.x, p.y]).collect(),
            bbox: prompt.bbox.map(|(a, b)| [a.x, a.y, b.x, b.y]),
        },
        load_ms: LoadMs {
            encoder: ms(load.encoder),
            decoder: ms(load.decoder),
        },
        image_load_ms: ms(image_load),
        encode_ms: ms(encode),
        decode_ms: ms(decode),
        embedding_bytes: embedding.bytes(),
        candidates: listed,
    };
    let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    write(&out.join("candidates.json"), &json)?;
    println!("{json}");
    Ok(())
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn numbers(s: &str, n: usize, what: &str) -> Result<Vec<f32>, String> {
    let v: Vec<f32> = s
        .split(',')
        .map(|t| t.trim().parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|_| format!("{what} {s:?}: expected {n} comma-separated numbers"))?;
    if v.len() != n {
        return Err(format!(
            "{what} {s:?}: expected {n} comma-separated numbers"
        ));
    }
    Ok(v)
}

fn point(s: &str) -> Result<Point, String> {
    let v = numbers(s, 2, "point")?;
    Ok(Point::new(v[0], v[1]))
}

fn parse_box(s: &str) -> Result<(Point, Point), String> {
    let v = numbers(s, 4, "box")?;
    Ok((Point::new(v[0], v[1]), Point::new(v[2], v[3])))
}

/// A `--view` rectangle as whole source pixels, which must overlap the image.
fn window((a, b): (Point, Point), (w, h): (u32, u32)) -> Result<RectPx, String> {
    let x0 = a.x.min(b.x).floor().clamp(0.0, w as f32) as u32;
    let y0 = a.y.min(b.y).floor().clamp(0.0, h as f32) as u32;
    let x1 = a.x.max(b.x).ceil().clamp(0.0, w as f32) as u32;
    let y1 = a.y.max(b.y).ceil().clamp(0.0, h as f32) as u32;
    if x1 <= x0 || y1 <= y0 {
        return Err("--view does not overlap the image".into());
    }
    Ok(RectPx {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// Catches coordinates typed for another orientation or image.
fn check_inside(p: &Prompt, (w, h): (u32, u32)) -> Result<(), String> {
    let corners = p.bbox.iter().flat_map(|(a, b)| [*a, *b]);
    for q in p.positive.iter().chain(&p.negative).copied().chain(corners) {
        if !(0.0..=w as f32).contains(&q.x) || !(0.0..=h as f32).contains(&q.y) {
            return Err(format!(
                "({}, {}) is outside the {w}×{h} image (after its EXIF orientation)",
                q.x, q.y
            ));
        }
    }
    Ok(())
}

/// Source pixels whose centre samples a logit above 0, scanning only near positive cells.
pub fn area_px(l: &Logits) -> u64 {
    let (mut u0, mut v0, mut u1, mut v1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, &v) in l.data.iter().enumerate() {
        if v > 0.0 {
            let (u, w) = (i % l.width, i / l.width);
            u0 = u0.min(u);
            v0 = v0.min(w);
            u1 = u1.max(u);
            v1 = v1.max(w);
        }
    }
    if u0 == usize::MAX {
        return 0;
    }
    // A positive sample lies within one cell of a positive cell.
    let range = |lo: usize, hi: usize, cells: f32, size: u32| {
        let a = ((lo as f32 - 1.0 + 0.5) / cells - 0.5).floor().max(0.0) as u32;
        let b = (((hi as f32 + 1.0 + 0.5) / cells - 0.5).ceil() as u32 + 1).min(size);
        (a.min(size), b)
    };
    let (x0, x1) = range(u0, u1, l.cells_per_px.0, l.source.0);
    let (y0, y1) = range(v0, v1, l.cells_per_px.1, l.source.1);
    let mut n = 0;
    for y in y0..y1 {
        for x in x0..x1 {
            if l.sample(Point::new(x as f32 + 0.5, y as f32 + 0.5)) > 0.0 {
                n += 1;
            }
        }
    }
    n
}

/// The part of the source an overlay shows, and at what size.
struct View {
    window: RectPx,
    base: Rgb8,
}

impl View {
    /// `window` of the image (the whole image when `None`), reduced to at most `size` on its
    /// long side (`0` keeps full size).
    fn new(image: &Rgb8, window: Option<RectPx>, size: u32) -> View {
        let window = window.unwrap_or(RectPx {
            x: 0,
            y: 0,
            width: image.width,
            height: image.height,
        });
        let crop = if window.width == image.width && window.height == image.height {
            image.clone()
        } else {
            image.crop(window)
        };
        let long = crop.width.max(crop.height);
        let base = if size == 0 || long <= size {
            crop
        } else {
            let (w, h) = crop.long_side_size(size);
            segment::resample_rgb8(&crop, w, h)
        };
        View { window, base }
    }

    /// Source pixels per overlay pixel along x and y.
    fn scale(&self) -> (f32, f32) {
        (
            self.window.width as f32 / self.base.width as f32,
            self.window.height as f32 / self.base.height as f32,
        )
    }

    fn to_source(&self, x: f32, y: f32) -> Point {
        let (sx, sy) = self.scale();
        Point::new(self.window.x as f32 + x * sx, self.window.y as f32 + y * sy)
    }

    fn to_overlay(&self, p: Point) -> (f32, f32) {
        let (sx, sy) = self.scale();
        (
            (p.x - self.window.x as f32) / sx,
            (p.y - self.window.y as f32) / sy,
        )
    }
}

/// The candidate tinted blue with a cyan edge; positive points as green rings, negative as red
/// rings, the box in magenta. Each overlay pixel samples the logits at its centre in source
/// coordinates.
fn overlay(view: &View, logits: &Logits, prompt: &Prompt) -> Rgb8 {
    let (w, h) = (view.base.width as usize, view.base.height as usize);
    let inside: Vec<bool> = (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
            logits.sample(view.to_source(x, y)) > 0.0
        })
        .collect();
    let mut out = view.base.clone();
    let mut put = |x: usize, y: usize, c: [u8; 3], alpha: f32| {
        let i = (y * w + x) * 3;
        for (o, c) in out.data[i..i + 3].iter_mut().zip(c) {
            *o = (*o as f32 * (1.0 - alpha) + c as f32 * alpha).round() as u8;
        }
    };
    for y in 0..h {
        for x in 0..w {
            if !inside[y * w + x] {
                continue;
            }
            let edge = x == 0
                || y == 0
                || x + 1 == w
                || y + 1 == h
                || !inside[y * w + x - 1]
                || !inside[y * w + x + 1]
                || !inside[(y - 1) * w + x]
                || !inside[(y + 1) * w + x];
            if edge {
                put(x, y, [0, 255, 255], 1.0);
            } else {
                put(x, y, [30, 144, 255], 0.5);
            }
        }
    }
    // Rings leave the object under a point visible.
    let r = (w.max(h) as f32 / 300.0).max(4.0);
    let mut ring = |p: Point, c: [u8; 3]| {
        let (cx, cy) = view.to_overlay(p);
        let reach = r + 3.0;
        let x0 = (cx - reach).clamp(0.0, w as f32) as usize;
        let x1 = (cx + reach).clamp(0.0, w as f32) as usize;
        let y0 = (cy - reach).clamp(0.0, h as f32) as usize;
        let y1 = (cy + reach).clamp(0.0, h as f32) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                if (r - 1.5..=r).contains(&d) {
                    put(x, y, c, 1.0);
                } else if (r..=r + 1.5).contains(&d) || (r - 3.0..r - 1.5).contains(&d) {
                    put(x, y, [0, 0, 0], 1.0);
                }
            }
        }
    };
    for &p in &prompt.positive {
        ring(p, [0, 230, 0]);
    }
    for &p in &prompt.negative {
        ring(p, [240, 0, 0]);
    }
    if let Some((a, b)) = prompt.bbox {
        let (ax, ay) = view.to_overlay(a);
        let (bx, by) = view.to_overlay(b);
        let t = (r / 2.0).max(2.0);
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let within = |lo: f32, hi: f32, v: f32| v >= lo.min(hi) - t && v <= lo.max(hi) + t;
                let near = |edge: f32, v: f32| (v - edge).abs() <= t / 2.0;
                if within(ax, bx, px)
                    && within(ay, by, py)
                    && (near(ax, px) || near(bx, px) || near(ay, py) || near(by, py))
                {
                    put(x, y, [255, 0, 255], 1.0);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_parse() {
        assert_eq!(point("12.5, 7").unwrap(), Point::new(12.5, 7.0));
        assert!(point("1,2,3").is_err());
        assert!(point("a,b").is_err());
        let (a, b) = parse_box("1,2,30,40").unwrap();
        assert_eq!((a, b), (Point::new(1.0, 2.0), Point::new(30.0, 40.0)));
        assert!(parse_box("1,2,3").is_err());
    }

    #[test]
    fn prompts_outside_the_image_are_refused() {
        let p = Prompt {
            positive: vec![Point::new(5.0, 50.0)],
            ..Prompt::default()
        };
        assert!(check_inside(&p, (100, 40)).is_err());
        assert!(check_inside(&p, (40, 100)).is_ok());
    }

    /// Every source pixel's centre sampled, for comparison.
    fn brute_force(l: &Logits) -> u64 {
        let mut n = 0;
        for y in 0..l.source.1 {
            for x in 0..l.source.0 {
                n += (l.sample(Point::new(x as f32 + 0.5, y as f32 + 0.5)) > 0.0) as u64;
            }
        }
        n
    }

    #[test]
    fn area_counts_source_pixels_above_zero() {
        // A 4×4 grid over a 40×40 image (10 px per cell): the zero crossing lies halfway between
        // cell centres, and bilinear interpolation rounds a positive block's corners.
        let grid = |cells: &[(usize, usize)]| {
            let mut data = vec![-1.0f32; 16];
            for &(u, v) in cells {
                data[v * 4 + u] = 1.0;
            }
            Logits {
                width: 4,
                height: 4,
                data,
                source: (40, 40),
                cells_per_px: (0.1, 0.1),
            }
        };
        let centre = grid(&[(1, 1), (2, 1), (1, 2), (2, 2)]);
        let a = area_px(&centre);
        assert_eq!(a, brute_force(&centre));
        assert!((300..400).contains(&a), "{a}");
        for cells in [&[(0, 0)][..], &[(3, 3)], &[(3, 0), (0, 3)]] {
            let l = grid(cells);
            assert_eq!(area_px(&l), brute_force(&l), "{cells:?}");
        }
        assert_eq!(area_px(&grid(&[])), 0);
    }
}
