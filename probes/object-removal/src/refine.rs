//! From a chosen candidate to the removal region at full resolution: guided upsampling, the
//! painted stroke, an optional contact band for the object's shadow, and dilation.
//!
//! Every operation works over a mask's window, never the whole frame unless the region spans it.

use crate::image::Rgb8;
use crate::mask::{Logits, Mask, Point, RectPx};
use crate::stroke::{self, Stroke};

#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// Guided-filter radius in source pixels; `None` scales with the logits' cell size.
    pub guided_radius: Option<u32>,
    /// Guided-filter regularization, in squared [0, 1] luminance.
    pub guided_eps: f32,
    /// Dilation of the final region in source pixels; `None` uses `object_dilation` for an object
    /// and none for a painted footprint.
    pub dilation: Option<u32>,
    /// Width in source pixels of the region's soft outer edge; `None` uses `default_feather` for an
    /// object and 15 % of the brush radius for a painted footprint, at least 2 px either way.
    pub feather: Option<u32>,
    /// Add the painted stroke's own footprint to the region (so painting over a shadow includes it).
    pub include_stroke: bool,
    /// Add the heuristic contact band below the object.
    pub contact_band: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            guided_radius: None,
            guided_eps: 1e-3,
            dilation: None,
            feather: None,
            include_stroke: true,
            contact_band: false,
        }
    }
}

/// The soft prior is the hard sigmoid `clamp(0.5 + PRIOR_STEEPNESS · logit, 0, 1)`, which has the
/// logistic sigmoid's slope at 0: decoders train their logits as log-odds, so near the edge it is
/// the decoder's own probability. Unlike the logistic curve it saturates, at logits beyond ±2, so a
/// confident cell is exactly 0 or 255 rather than 1 or 254 and nothing faint leaks outside.
const PRIOR_STEEPNESS: f32 = 0.25;
/// The logit at and below which the prior is 0.
const PRIOR_FLOOR: f32 = -0.5 / PRIOR_STEEPNESS;
/// The upsampling window is padded by at least this many cells.
const WINDOW_PAD_CELLS: f64 = 3.0;
/// The guided filter runs on a grid subsampled so that its radius is about this many samples
/// (He and Sun's fast guided filter); radii below twice this run at full resolution.
const GUIDED_SUBSAMPLED_RADIUS: u32 = 4;
/// The dilation precedent: Inpaint Anything's 15 px kernel on 1–2 MP images, taken at 1.5 MP.
const REFERENCE_DILATION: f64 = 15.0;
const REFERENCE_PIXELS: f64 = 1.5e6;
/// Contact band: the object's lowest share of rows marks the columns that touch the ground.
const BAND_CONTACT_ROWS: f64 = 0.1;
/// Contact band reach as shares of the object's height (up, down) and width (sideways), each with
/// a floor in source pixels.
const BAND_UP: (f64, f64) = (0.2, 4.0);
const BAND_DOWN: (f64, f64) = (0.1, 4.0);
const BAND_SIDE: (f64, f64) = (0.35, 8.0);

/// The most an object's region grows past its core, dilation and feather together: the 15 px
/// precedent for 1–2 MP images, scaled with the image's linear size, `round(15 · √(pixels /
/// 1.5 MP))`, at least 1. About 60 px at 24 MP and 95 px at 60 MP, where one logit cell spans about
/// 37 px.
pub fn default_dilation(image: (u32, u32)) -> u32 {
    let pixels = image.0 as f64 * image.1 as f64;
    (REFERENCE_DILATION * (pixels / REFERENCE_PIXELS).sqrt())
        .round()
        .max(1.0) as u32
}

/// The candidate's coverage at source resolution over its padded bounds, with edges refined by a
/// guided filter against the image's luminance.
///
/// An empty mask when no logit is above 0. Otherwise the window is the bounds of the cells whose
/// prior is non-zero (logit above −2; for a decisive decoder, the logit > 0 bounds grown by under a
/// cell), padded by one cell plus the guided filter's reach and by at least three cells, and
/// clamped to the image, so no coverage is cut at its edge.
///
/// The prior is a hard sigmoid of the bilinearly sampled logits, linear from logit −2 to +2. It is
/// refined by He et al.'s guided filter against Rec. 709 luma of the sRGB-encoded image, with box
/// means from running sums. The radius defaults to one cell in source pixels (at least 2). From a
/// radius of 8, the filter's coefficients are computed on a grid subsampled to keep the radius at
/// about 4 samples and are bilinearly upsampled before `q = a · I + b` is taken at full resolution
/// (He and Sun's fast guided filter), so the working buffers stay small at 60 MP.
pub fn upsample(logits: &Logits, image: &Rgb8, params: &Params) -> Mask {
    let size = (image.width, image.height);
    assert_eq!(logits.source, size, "logits describe a different image");
    let radius = params
        .guided_radius
        .unwrap_or_else(|| default_guided_radius(logits))
        .max(1);
    let Some(window) = logit_window(logits, size, radius) else {
        return Mask::empty(size);
    };
    let data = guided(logits, image, window, radius, params.guided_eps);
    Mask::new(size, window, data)
}

/// The stroke's footprint as a hard-edged mask: every pixel whose centre lies within the brush
/// radius of the polyline is 255.
pub fn stroke_mask(stroke: &Stroke, image: (u32, u32)) -> Mask {
    let radius = stroke.radius.max(0.0) as f64;
    let limit = (image.0 as usize, image.1 as usize);
    let Some(window) = stroke::sweep_window(&stroke.points, radius, (1.0, 1.0), limit) else {
        return Mask::empty(image);
    };
    let bits = stroke::sweep(&stroke.points, radius, (1.0, 1.0), window);
    if !bits.contains(&true) {
        return Mask::empty(image);
    }
    let (x, y, width, height) = window;
    Mask::new(
        image,
        RectPx {
            x: x as u32,
            y: y as u32,
            width: width as u32,
            height: height as u32,
        },
        bits.into_iter().map(|b| if b { 255 } else { 0 }).collect(),
    )
}

/// A suggested band where the object meets the ground, for its contact shadow.
///
/// A crude heuristic after PredErase, with no light direction. The object is its coverage at or
/// above 128 (any coverage when none reaches it). Its contact columns are those with object pixels
/// in its lowest tenth of rows. The band is centred on the object's bottom edge: full over the
/// contact columns at that edge, it reaches up `max(4, 0.2 h)` and down `max(4, 0.1 h)`, and
/// `max(8, 0.35 w)` sideways beyond the contact columns, falling to 0 with a smoothstep in each
/// direction, where `w` and `h` are the object's width and height.
pub fn contact_band(object: &Mask) -> Mask {
    let image = object.image;
    let solid = if object.data.iter().any(|&v| v >= 128) {
        128
    } else {
        1
    };
    let ww = object.window.width as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (y, row) in object.data.chunks_exact(ww.max(1)).enumerate() {
        for (x, &v) in row.iter().enumerate() {
            if v >= solid {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 == usize::MAX {
        return Mask::empty(image);
    }
    let (w, h) = ((x1 - x0) as f64, (y1 - y0) as f64);
    let lowest = ((h * BAND_CONTACT_ROWS).ceil() as usize).max(1);
    let (mut c0, mut c1) = (usize::MAX, 0);
    for row in object
        .data
        .chunks_exact(ww)
        .take(y1)
        .skip(y1 - lowest.min(y1 - y0))
    {
        for (x, &v) in row.iter().enumerate() {
            if v >= solid {
                c0 = c0.min(x);
                c1 = c1.max(x + 1);
            }
        }
    }
    let base = (object.window.y as usize + y1) as f64;
    let (cx0, cx1) = (
        (object.window.x as usize + c0) as f64,
        (object.window.x as usize + c1) as f64,
    );
    let up = (h * BAND_UP.0).max(BAND_UP.1);
    let down = (h * BAND_DOWN.0).max(BAND_DOWN.1);
    let side = (w * BAND_SIDE.0).max(BAND_SIDE.1);
    let wx0 = (cx0 - side).floor().max(0.0) as u32;
    let wx1 = ((cx1 + side).ceil() as u32).min(image.0);
    let wy0 = (base - up).floor().max(0.0) as u32;
    let wy1 = ((base + down).ceil() as u32).min(image.1);
    if wx0 >= wx1 || wy0 >= wy1 {
        return Mask::empty(image);
    }
    let window = RectPx {
        x: wx0,
        y: wy0,
        width: wx1 - wx0,
        height: wy1 - wy0,
    };
    let across: Vec<f64> = (wx0..wx1)
        .map(|x| {
            let px = x as f64 + 0.5;
            let outside = (cx0 - px).max(px - cx1).max(0.0);
            1.0 - smoothstep(outside / side)
        })
        .collect();
    let mut data = Vec::with_capacity(window.width as usize * window.height as usize);
    for y in wy0..wy1 {
        let py = y as f64 + 0.5;
        let reach = if py < base {
            (base - py) / up
        } else {
            (py - base) / down
        };
        let along = 1.0 - smoothstep(reach);
        data.extend(across.iter().map(|a| (255.0 * a * along).round() as u8));
    }
    Mask::new(image, window, data)
}

/// Pointwise maximum.
pub fn union(a: &Mask, b: &Mask) -> Mask {
    assert_eq!(a.image, b.image, "masks of different images");
    if b.window.is_empty() {
        return a.clone();
    }
    if a.window.is_empty() {
        return b.clone();
    }
    let window = a.window.union(&b.window);
    let mut data = vec![0u8; window.width as usize * window.height as usize];
    for m in [a, b] {
        let (dx, dy) = (
            (m.window.x - window.x) as usize,
            (m.window.y - window.y) as usize,
        );
        let mw = m.window.width as usize;
        for (row, src) in m.data.chunks_exact(mw).enumerate() {
            let start = (dy + row) * window.width as usize + dx;
            for (d, &s) in data[start..start + mw].iter_mut().zip(src) {
                *d = (*d).max(s);
            }
        }
    }
    Mask::new(a.image, window, data)
}

/// `a` with `b` removed: `min(a, 255 − b)`.
pub fn subtract(a: &Mask, b: &Mask) -> Mask {
    assert_eq!(a.image, b.image, "masks of different images");
    let mut out = a.clone();
    if a.window.is_empty() || b.window.is_empty() {
        return out;
    }
    let x0 = a.window.x.max(b.window.x);
    let y0 = a.window.y.max(b.window.y);
    let x1 = (a.window.x + a.window.width).min(b.window.x + b.window.width);
    let y1 = (a.window.y + a.window.height).min(b.window.y + b.window.height);
    let aw = a.window.width as usize;
    for y in y0..y1 {
        let row = (y - a.window.y) as usize * aw;
        for x in x0..x1 {
            let i = row + (x - a.window.x) as usize;
            out.data[i] = out.data[i].min(255 - b.get(x, y));
        }
    }
    out
}

/// Grows coverage by a disc of `radius` pixels, keeping soft edges soft.
///
/// Grayscale dilation by a flat disc: each pixel takes the maximum within `radius` (pixel offsets
/// with `dx² + dy² ≤ r²`). The disc is decomposed into its `2r + 1` horizontal chords, and each row
/// keeps a table of maxima over power-of-two runs, so a chord's maximum is two lookups (Urbach and
/// Wilkinson's chord method). That is at most O(N · r) over the window, with early exits at full
/// coverage and on rows that cannot raise the maximum. The window grows by `radius`, clamped to
/// the image.
pub fn dilate(mask: &Mask, radius: u32) -> Mask {
    if radius == 0 || mask.window.is_empty() {
        return mask.clone();
    }
    let src = mask.window;
    let out = src.grown(radius, mask.image);
    let (sw, sh) = (src.width as usize, src.height as usize);
    let (ow, oh) = (out.width as usize, out.height as usize);
    let r = radius as usize;
    let half: Vec<usize> = (0..=r).map(|d| isqrt(r * r - d * d)).collect();
    // Levels 0..levels hold maxima over runs of 1, 2, 4, … up to the longest chord.
    let levels = (usize::BITS - (2 * r + 1).min(sw).leading_zeros()) as usize;
    let slots = (2 * r + 1).min(sh);
    let mut tables = vec![0u8; slots * levels * sw];
    // Per slot: first and last non-zero column, and the row's maximum.
    let mut rows: Vec<(usize, usize, u8)> = vec![(0, 0, 0); slots];
    let mut built = 0usize;
    let mut data = vec![0u8; ow * oh];
    let off_x = (src.x - out.x) as i64;
    let mut reach: Vec<(usize, i64, i64, i64, u8)> = Vec::with_capacity(2 * r + 1);

    for oy in 0..oh {
        // The source row of this output row, and the source rows within reach.
        let y = oy as i64 + out.y as i64 - src.y as i64;
        let lo = (y - r as i64).max(0);
        let hi = (y + r as i64).min(sh as i64 - 1);
        if lo > hi {
            continue;
        }
        while built as i64 <= hi {
            let slot = built % slots;
            let row = &mask.data[built * sw..(built + 1) * sw];
            rows[slot] = build_row(
                &mut tables[slot * levels * sw..(slot + 1) * levels * sw],
                row,
            );
            built += 1;
        }
        // Nearest rows first: their chords are the widest, so full coverage is found soonest.
        reach.clear();
        for d in 0..=r as i64 {
            let pair = [y - d, y + d];
            for &j in &pair[..if d == 0 { 1 } else { 2 }] {
                if j < lo || j > hi {
                    continue;
                }
                let slot = j as usize % slots;
                let (first, last, max) = rows[slot];
                if max > 0 {
                    reach.push((
                        slot,
                        half[d as usize] as i64,
                        first as i64,
                        last as i64,
                        max,
                    ));
                }
            }
        }
        if reach.is_empty() {
            continue;
        }
        let from = reach.iter().map(|e| e.2 - e.1).min().unwrap_or(0);
        let to = reach.iter().map(|e| e.3 + e.1).max().unwrap_or(-1);
        let line = &mut data[oy * ow..(oy + 1) * ow];
        for x in from.max(-off_x)..=to.min(ow as i64 - 1 - off_x) {
            let mut m = 0u8;
            for &(slot, w, first, last, max) in &reach {
                if max <= m {
                    continue;
                }
                let (a, b) = ((x - w).max(first), (x + w).min(last));
                if a > b {
                    continue;
                }
                let t = &tables[slot * levels * sw..(slot + 1) * levels * sw];
                m = m.max(run_max(t, sw, a as usize, b as usize));
                if m == 255 {
                    break;
                }
            }
            line[(x + off_x) as usize] = m;
        }
    }
    Mask::new(mask.image, out, data)
}

/// The removal region for a chosen candidate: the object's core, joined with the stroke's
/// footprint and the contact band as `params` asks, dilated by `object_dilation` and feathered.
/// A candidate with no core leaves the stroke's `footprint_region`, or nothing.
///
/// The band is computed from the upsampled object alone. The union is trimmed to its non-zero
/// bounds before dilation, so the result's window is those bounds grown by the dilation.
pub fn removal_region(
    candidate: &Logits,
    stroke: Option<&Stroke>,
    image: &Rgb8,
    params: &Params,
) -> Mask {
    let size = (image.width, image.height);
    let core = object_core(candidate, image, params);
    if core.window.is_empty() {
        return match stroke {
            Some(stroke) if params.include_stroke => footprint_region(stroke, size, params),
            _ => Mask::empty(size),
        };
    }
    let mut region = core.clone();
    if params.include_stroke
        && let Some(stroke) = stroke
    {
        region = union(&region, &stroke_mask(stroke, size));
    }
    if params.contact_band {
        region = union(&region, &contact_band(&core));
    }
    let width = params
        .feather
        .unwrap_or_else(|| default_feather(candidate).min(default_dilation(size)));
    let radius = params
        .dilation
        .unwrap_or_else(|| object_dilation(&core, candidate, width));
    feather(&dilate(&trim(&region), radius), width)
}

/// The object at full coverage wherever the decoder places it inside (its bilinearly sampled logit
/// is above 0) or the guided filter gives it at least half coverage, and 0 elsewhere.
///
/// A removal region needs the object's extent, not its probability. Faint guided coverage far from
/// the object widened the fill's crop several times over, and a small object whose guided peak
/// stayed under half (a teal kayak on teal water reached 78) survived its own removal.
pub fn object_core(logits: &Logits, image: &Rgb8, params: &Params) -> Mask {
    let soft = upsample(logits, image, params);
    let w = soft.window;
    if w.is_empty() {
        return soft;
    }
    let data = soft
        .data
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let (x, y) = (w.x + i as u32 % w.width, w.y + i as u32 / w.width);
            let inside =
                v >= 128 || logits.sample(Point::new(x as f32 + 0.5, y as f32 + 0.5)) > 0.0;
            if inside { 255 } else { 0 }
        })
        .collect();
    trim(&Mask::new(soft.image, w, data))
}

/// How far an object's region reaches past its core: half a logit cell, the decoder mask's
/// positional uncertainty, plus 4 % of the core's shorter side for anti-aliasing, blur and the
/// object's colour bleeding into its surroundings; at least 2 px, and never more than
/// `default_dilation` less the feather. A kayak 40 px wide at 20 MP grows by about 12 px, a person
/// 700 px wide at 24 MP by about 40 px.
pub fn object_dilation(core: &Mask, logits: &Logits, feather: u32) -> u32 {
    let Some(b) = core.bounds() else {
        return 0;
    };
    let reach = 0.5 * cell_px(logits) + 0.04 * b.width.min(b.height) as f64;
    let cap = default_dilation(core.image).saturating_sub(feather).max(2);
    (reach.round() as u32).clamp(2, cap)
}

/// One logit cell, at least 2 px: the soft outer edge of an object's region. A quarter of a cell
/// left a visible seam where LaMa's enlarged fill met the deck on the bridge; a cell hides most of it.
pub fn default_feather(logits: &Logits) -> u32 {
    ((1.0 * cell_px(logits)).round() as u32).max(2)
}

/// The region for a stroke that found no object: its footprint, dilated only when
/// `params.dilation` asks (the person painted exactly what they want gone), and feathered.
pub fn footprint_region(stroke: &Stroke, size: (u32, u32), params: &Params) -> Mask {
    let footprint = stroke_mask(stroke, size);
    let grown = match params.dilation {
        Some(r) => dilate(&footprint, r),
        None => footprint,
    };
    let width = params
        .feather
        .unwrap_or_else(|| ((0.15 * stroke.radius).round() as u32).max(2));
    feather(&trim(&grown), width)
}

/// A soft outer edge: the mask keeps its coverage, and coverage falls to 0 over about `width`
/// pixels outside it (the maximum of the mask and two box blurs of radius `width / 2`).
pub fn feather(mask: &Mask, width: u32) -> Mask {
    if width == 0 || mask.window.is_empty() {
        return mask.clone();
    }
    let window = mask.window.grown(width, mask.image);
    let (w, h) = (window.width as usize, window.height as usize);
    let mut src = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            src[y * w + x] = mask.get(window.x + x as u32, window.y + y as u32) as f32;
        }
    }
    let r = (width as usize / 2).max(1);
    let blurred = box_mean(&box_mean(&src, w, h, r), w, h, r);
    let data = src
        .iter()
        .zip(&blurred)
        .map(|(&m, &b)| m.max(b).round().clamp(0.0, 255.0) as u8)
        .collect();
    trim(&Mask::new(mask.image, window, data))
}

// Helpers.

fn smoothstep(e: f64) -> f64 {
    let e = e.clamp(0.0, 1.0);
    e * e * (3.0 - 2.0 * e)
}

fn isqrt(n: usize) -> usize {
    let mut s = (n as f64).sqrt() as usize;
    while s * s > n {
        s -= 1;
    }
    while (s + 1) * (s + 1) <= n {
        s += 1;
    }
    s
}

/// The mask cropped to its non-zero bounds.
fn trim(m: &Mask) -> Mask {
    let Some(b) = m.bounds() else {
        return Mask::empty(m.image);
    };
    if b == m.window {
        return m.clone();
    }
    let mw = m.window.width as usize;
    let (dx, dy) = ((b.x - m.window.x) as usize, (b.y - m.window.y) as usize);
    let mut data = Vec::with_capacity(b.width as usize * b.height as usize);
    for row in m.data.chunks_exact(mw).skip(dy).take(b.height as usize) {
        data.extend_from_slice(&row[dx..dx + b.width as usize]);
    }
    Mask::new(m.image, b, data)
}

/// Fills `tables` (levels × width) for one row: level `k` holds the maximum over `[i, i + 2^k)`,
/// truncated at the row's end. Returns the row's first and last non-zero column and its maximum.
fn build_row(tables: &mut [u8], row: &[u8]) -> (usize, usize, u8) {
    let w = row.len();
    let levels = tables.len() / w;
    tables[..w].copy_from_slice(row);
    for k in 1..levels {
        let step = 1 << (k - 1);
        let (done, rest) = tables.split_at_mut(k * w);
        let prev = &done[(k - 1) * w..];
        for (i, t) in rest[..w].iter_mut().enumerate() {
            *t = if i + step < w {
                prev[i].max(prev[i + step])
            } else {
                prev[i]
            };
        }
    }
    let first = row.iter().position(|&v| v > 0);
    let last = row.iter().rposition(|&v| v > 0);
    match (first, last) {
        (Some(f), Some(l)) => (f, l, row[f..=l].iter().copied().max().unwrap_or(0)),
        _ => (0, 0, 0),
    }
}

/// The maximum over `[a, b]` of a row whose tables `build_row` filled.
fn run_max(tables: &[u8], w: usize, a: usize, b: usize) -> u8 {
    let len = b - a + 1;
    let k = (usize::BITS - 1 - len.leading_zeros()) as usize;
    let level = &tables[k * w..(k + 1) * w];
    level[a].max(level[b + 1 - (1 << k)])
}

fn cell_px(l: &Logits) -> f64 {
    (1.0 / l.cells_per_px.0 as f64).max(1.0 / l.cells_per_px.1 as f64)
}

fn default_guided_radius(l: &Logits) -> u32 {
    (cell_px(l).round() as u32).max(2)
}

/// The prior's support in source pixels, padded and clamped to the image; `None` without a
/// positive logit.
fn logit_window(l: &Logits, size: (u32, u32), radius: u32) -> Option<RectPx> {
    stroke::cells_above(l, 0.0)?;
    let (u0, v0, u1, v1) = stroke::cells_above(l, PRIOR_FLOOR)?;
    let (s, r) = subsampling(radius);
    // Coverage can be non-zero one guided reach beyond the prior, which is non-zero within a cell
    // of those cells: two box passes and the bilinear step, plus a block for sampling its centre.
    let reach = ((2 * r + 2) * s) as f64;
    let (cx, cy) = (l.cells_per_px.0 as f64, l.cells_per_px.1 as f64);
    let pad_x = (WINDOW_PAD_CELLS / cx).max(1.0 / cx + reach);
    let pad_y = (WINDOW_PAD_CELLS / cy).max(1.0 / cy + reach);
    let x0 = (u0 as f64 / cx - pad_x).floor().max(0.0) as u32;
    let y0 = (v0 as f64 / cy - pad_y).floor().max(0.0) as u32;
    let x1 = ((u1 + 1) as f64 / cx + pad_x).ceil().min(size.0 as f64) as u32;
    let y1 = ((v1 + 1) as f64 / cy + pad_y).ceil().min(size.1 as f64) as u32;
    (x0 < x1 && y0 < y1).then_some(RectPx {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// The guided filter's subsampling factor and its radius in subsampled samples.
fn subsampling(radius: u32) -> (usize, usize) {
    let s = (radius / GUIDED_SUBSAMPLED_RADIUS).max(1) as usize;
    (s, ((radius as usize + s / 2) / s).max(1))
}

/// Rec. 709 luma of an sRGB-encoded pixel, in [0, 1].
fn luma(image: &Rgb8, x: u32, y: u32) -> f32 {
    let [r, g, b] = image.pixel(x, y);
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

fn prior(logit: f32) -> f32 {
    (0.5 + PRIOR_STEEPNESS * logit).clamp(0.0, 1.0)
}

/// The guided filter over `window`, returning coverage in 0..=255.
fn guided(logits: &Logits, image: &Rgb8, window: RectPx, radius: u32, eps: f32) -> Vec<u8> {
    let (w, h) = (window.width as usize, window.height as usize);
    let (s, r) = subsampling(radius);
    let (lw, lh) = (w.div_ceil(s), h.div_ceil(s));

    // The guide is the block-mean luma; the prior is sampled at each block's centre.
    let mut guide = vec![0f32; lw * lh];
    for y in 0..h {
        let row = &mut guide[(y / s) * lw..(y / s + 1) * lw];
        for x in 0..w {
            row[x / s] += luma(image, window.x + x as u32, window.y + y as u32);
        }
    }
    let mut soft = vec![0f32; lw * lh];
    for j in 0..lh {
        let (y0, y1) = (j * s, ((j + 1) * s).min(h));
        for i in 0..lw {
            let (x0, x1) = (i * s, ((i + 1) * s).min(w));
            guide[j * lw + i] /= ((y1 - y0) * (x1 - x0)) as f32;
            let centre = Point::new(
                window.x as f32 + (x0 + x1) as f32 / 2.0,
                window.y as f32 + (y0 + y1) as f32 / 2.0,
            );
            soft[j * lw + i] = prior(logits.sample(centre));
        }
    }

    let mean_i = box_mean(&guide, lw, lh, r);
    let mean_p = box_mean(&soft, lw, lh, r);
    let ii: Vec<f32> = guide.iter().map(|g| g * g).collect();
    let ip: Vec<f32> = guide.iter().zip(&soft).map(|(g, p)| g * p).collect();
    let corr_ii = box_mean(&ii, lw, lh, r);
    let corr_ip = box_mean(&ip, lw, lh, r);
    let mut a = vec![0f32; lw * lh];
    let mut b = vec![0f32; lw * lh];
    for k in 0..lw * lh {
        let var = (corr_ii[k] - mean_i[k] * mean_i[k]).max(0.0);
        let cov = corr_ip[k] - mean_i[k] * mean_p[k];
        a[k] = cov / (var + eps);
        b[k] = mean_p[k] - a[k] * mean_i[k];
    }
    let mean_a = box_mean(&a, lw, lh, r);
    let mean_b = box_mean(&b, lw, lh, r);

    // q = ā · I + b̄ at full resolution, with ā and b̄ bilinear between block centres.
    let axis = |n: usize, blocks: usize| -> Vec<(usize, usize, f32)> {
        (0..n)
            .map(|i| {
                let u = ((i as f32 + 0.5) / s as f32 - 0.5).clamp(0.0, (blocks - 1) as f32);
                let u0 = u.floor() as usize;
                (u0, (u0 + 1).min(blocks - 1), u - u0 as f32)
            })
            .collect()
    };
    let (xs, ys) = (axis(w, lw), axis(h, lh));
    let mut out = Vec::with_capacity(w * h);
    for (y, &(v0, v1, fv)) in ys.iter().enumerate() {
        for (x, &(u0, u1, fu)) in xs.iter().enumerate() {
            let lerp = |m: &[f32]| {
                let top = m[v0 * lw + u0] * (1.0 - fu) + m[v0 * lw + u1] * fu;
                let bottom = m[v1 * lw + u0] * (1.0 - fu) + m[v1 * lw + u1] * fu;
                top * (1.0 - fv) + bottom * fv
            };
            let i = luma(image, window.x + x as u32, window.y + y as u32);
            let q = lerp(&mean_a) * i + lerp(&mean_b);
            out.push((q.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    out
}

/// The mean over each sample's `(2r + 1)²` box, clipped at the edges: a horizontal pass by prefix
/// sums and a vertical pass by running column sums, both in f64.
fn box_mean(src: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut rows = vec![0f64; w * h];
    let mut prefix = vec![0f64; w + 1];
    for (line, out) in src.chunks_exact(w).zip(rows.chunks_exact_mut(w)) {
        for (x, &v) in line.iter().enumerate() {
            prefix[x + 1] = prefix[x] + v as f64;
        }
        for (x, o) in out.iter_mut().enumerate() {
            let (lo, hi) = (x.saturating_sub(r), (x + r).min(w - 1));
            *o = (prefix[hi + 1] - prefix[lo]) / (hi - lo + 1) as f64;
        }
    }
    let mut column = vec![0f64; w];
    for line in rows.chunks_exact(w).take(r.min(h - 1) + 1) {
        for (c, &v) in column.iter_mut().zip(line) {
            *c += v;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let (lo, hi) = (y.saturating_sub(r), (y + r).min(h - 1));
        let n = (hi - lo + 1) as f64;
        for (o, &c) in out[y * w..(y + 1) * w].iter_mut().zip(&column) {
            *o = (c / n) as f32;
        }
        if y + r + 1 < h {
            for (c, &v) in column
                .iter_mut()
                .zip(&rows[(y + r + 1) * w..(y + r + 2) * w])
            {
                *c += v;
            }
        }
        if y >= r {
            for (c, &v) in column.iter_mut().zip(&rows[(y - r) * w..(y - r + 1) * w]) {
                *c -= v;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> RectPx {
        RectPx {
            x,
            y,
            width,
            height,
        }
    }

    /// Every pixel of the image, for comparisons independent of windows.
    fn pixels(m: &Mask) -> Vec<u8> {
        let (w, h) = m.image;
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| m.get(x, y))
            .collect()
    }

    fn naive_dilate(m: &Mask, radius: u32) -> Mask {
        let out = m.window.grown(radius, m.image);
        let r = radius as i64;
        let mut data = Vec::new();
        for y in out.y..out.y + out.height {
            for x in out.x..out.x + out.width {
                let mut v = 0;
                for dy in -r..=r {
                    for dx in -r..=r {
                        let (sx, sy) = (x as i64 + dx, y as i64 + dy);
                        if dx * dx + dy * dy <= r * r
                            && sx >= 0
                            && sy >= 0
                            && sx < m.image.0 as i64
                            && sy < m.image.1 as i64
                        {
                            v = v.max(m.get(sx as u32, sy as u32));
                        }
                    }
                }
                data.push(v);
            }
        }
        Mask::new(m.image, out, data)
    }

    /// A deterministic soft mask: a few blobs with soft rims plus scattered specks.
    fn blobs(image: (u32, u32), window: RectPx, seed: u64) -> Mask {
        let mut state = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        let (w, h) = (window.width, window.height);
        let centres: Vec<(f32, f32, f32)> = (0..3)
            .map(|_| {
                (
                    (next() % w) as f32,
                    (next() % h) as f32,
                    2.0 + (next() % 6) as f32,
                )
            })
            .collect();
        let mut data = vec![0u8; (w * h) as usize];
        for (i, d) in data.iter_mut().enumerate() {
            let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
            for &(cx, cy, r) in &centres {
                let dist = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
                let v = ((r + 2.0 - dist) / 2.0).clamp(0.0, 1.0);
                *d = (*d).max((v * 255.0).round() as u8);
            }
            if next() % 97 == 0 {
                *d = (*d).max((next() % 256) as u8);
            }
        }
        Mask::new(image, window, data)
    }

    fn flat_image(width: u32, height: u32, value: u8) -> Rgb8 {
        Rgb8::new(
            width,
            height,
            vec![value; width as usize * height as usize * 3],
        )
    }

    /// Logits with one cell per `cell` source pixels on both axes over exactly the image, +6 where
    /// `inside` holds at the cell centre and −6 elsewhere: a blocky decoder mask.
    fn logits_over(size: (u32, u32), cell: f32, inside: impl Fn(f32, f32) -> bool) -> Logits {
        let (w, h) = (
            (size.0 as f32 / cell).ceil() as usize,
            (size.1 as f32 / cell).ceil() as usize,
        );
        let mut data = Vec::with_capacity(w * h);
        for v in 0..h {
            for u in 0..w {
                let (x, y) = ((u as f32 + 0.5) * cell, (v as f32 + 0.5) * cell);
                data.push(if inside(x, y) { 6.0 } else { -6.0 });
            }
        }
        Logits {
            width: w,
            height: h,
            data,
            source: size,
            cells_per_px: (1.0 / cell, 1.0 / cell),
        }
    }

    fn stroke_dist2(x: f64, y: f64, p: Point, q: Point) -> f64 {
        let (px, py, qx, qy) = (p.x as f64, p.y as f64, q.x as f64, q.y as f64);
        let (dx, dy) = (qx - px, qy - py);
        let t = (((x - px) * dx + (y - py) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        (x - px - t * dx).powi(2) + (y - py - t * dy).powi(2)
    }

    #[test]
    fn refine_default_dilation_scales_with_the_image() {
        assert_eq!(default_dilation((1500, 1000)), 15);
        assert_eq!(default_dilation((6000, 4000)), 60);
        assert_eq!(default_dilation((9504, 6336)), 95);
        assert_eq!(default_dilation((10000, 6000)), 95);
        assert_eq!(default_dilation((64, 64)), 1);
        assert_eq!(default_dilation((0, 0)), 1);
        let sizes = [
            (640, 480),
            (1500, 1000),
            (4000, 3000),
            (6048, 4024),
            (9504, 6336),
        ];
        for pair in sizes.windows(2) {
            assert!(default_dilation(pair[0]) < default_dilation(pair[1]));
        }
    }

    #[test]
    fn refine_stroke_mask_is_the_swept_disc() {
        let image = (32, 24);
        let dot = Stroke {
            points: vec![Point::new(10.5, 10.5)],
            radius: 3.0,
        };
        let m = stroke_mask(&dot, image);
        assert_eq!(m.window, rect(7, 7, 7, 7));
        assert_eq!(m.data.iter().filter(|&&v| v == 255).count(), 29);
        assert!(m.data.iter().all(|&v| v == 0 || v == 255));

        // A bent polyline, one through the corner and one off the image, against brute force.
        let strokes = [
            Stroke {
                points: vec![
                    Point::new(3.0, 4.0),
                    Point::new(20.2, 9.7),
                    Point::new(12.0, 20.0),
                ],
                radius: 2.5,
            },
            Stroke {
                points: vec![Point::new(1.0, 1.0), Point::new(-4.0, 30.0)],
                radius: 4.0,
            },
            Stroke {
                points: vec![Point::new(40.0, 5.0), Point::new(50.0, 5.0)],
                radius: 3.0,
            },
        ];
        for s in &strokes {
            let m = stroke_mask(s, image);
            let expected: Vec<u8> = (0..image.1)
                .flat_map(|y| (0..image.0).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                    let near = s
                        .points
                        .windows(2)
                        .any(|w| stroke_dist2(px, py, w[0], w[1]) <= (s.radius as f64).powi(2));
                    if near { 255 } else { 0 }
                })
                .collect();
            assert_eq!(pixels(&m), expected, "{s:?}");
            assert!(m.window.x + m.window.width <= image.0);
            assert!(m.window.y + m.window.height <= image.1);
        }
        assert!(stroke_mask(&strokes[2], image).window.is_empty());
    }

    #[test]
    fn refine_union_and_subtract_combine_windows() {
        let image = (10, 8);
        let a = Mask::new(image, rect(2, 2, 3, 2), vec![10, 20, 30, 40, 50, 60]);
        let b = Mask::new(image, rect(4, 3, 3, 2), vec![250, 5, 5, 5, 200, 5]);
        let u = union(&a, &b);
        assert_eq!(u.window, rect(2, 2, 5, 3));
        #[rustfmt::skip]
        assert_eq!(u.data, vec![
            10, 20, 30, 0, 0,
            40, 50, 250, 5, 5,
            0, 0, 5, 200, 5,
        ]);
        let d = subtract(&a, &b);
        assert_eq!(d.window, a.window);
        assert_eq!(d.data, vec![10, 20, 30, 40, 50, 5]);
        let e = Mask::empty(image);
        assert_eq!(union(&a, &e), a);
        assert_eq!(union(&e, &b), b);
        assert_eq!(subtract(&a, &e), a);
        assert_eq!(subtract(&e, &a), e);
        // Windows at opposite corners join into the whole image.
        let tl = Mask::new(image, rect(0, 0, 1, 1), vec![7]);
        let br = Mask::new(image, rect(9, 7, 1, 1), vec![9]);
        let j = union(&tl, &br);
        assert_eq!(j.window, rect(0, 0, 10, 8));
        assert_eq!((j.get(0, 0), j.get(9, 7), j.get(5, 5)), (7, 9, 0));
    }

    #[test]
    fn refine_dilation_is_a_disc_and_keeps_soft_edges_soft() {
        let image = (41, 41);
        let dot = Mask::new(image, rect(20, 20, 1, 1), vec![255]);
        let d = dilate(&dot, 3);
        assert_eq!(d.window, rect(17, 17, 7, 7));
        assert_eq!(d.data.iter().filter(|&&v| v == 255).count(), 29);
        assert_eq!(d, naive_dilate(&dot, 3));
        assert_eq!(dilate(&dot, 0), dot);

        // A soft ramp keeps its slope and moves out by the radius.
        let ramp: Vec<u8> = (0..5)
            .flat_map(|_| (0..20).map(|x| (12 * x) as u8))
            .collect();
        let soft = Mask::new(image, rect(10, 10, 20, 5), ramp);
        let d = dilate(&soft, 4);
        assert_eq!(d, naive_dilate(&soft, 4));
        for x in 6..26u32 {
            let source = (x + 4).min(29) - 10;
            assert_eq!(d.get(x, 12), (12 * source) as u8, "column {x}");
        }
        let distinct: std::collections::BTreeSet<u8> = d.data.iter().copied().collect();
        assert!(distinct.len() > 15, "soft values survive: {distinct:?}");
    }

    #[test]
    fn refine_dilation_matches_brute_force() {
        let image = (60, 50);
        let windows = [
            rect(10, 12, 30, 20),
            rect(0, 0, 17, 11),
            rect(45, 38, 15, 12),
            rect(0, 20, 60, 5),
            rect(30, 0, 1, 50),
        ];
        for (i, &window) in windows.iter().enumerate() {
            let m = blobs(image, window, i as u64);
            for r in [1, 2, 3, 5, 8, 13, 40] {
                assert_eq!(
                    dilate(&m, r),
                    naive_dilate(&m, r),
                    "window {window:?}, radius {r}"
                );
            }
        }
    }

    #[test]
    fn refine_windows_clamp_at_image_edges() {
        let image = (40, 30);
        let corner = Mask::new(image, rect(0, 0, 3, 3), vec![255; 9]);
        assert_eq!(dilate(&corner, 5).window, rect(0, 0, 8, 8));
        let far = Mask::new(image, rect(37, 28, 3, 2), vec![255; 6]);
        let d = dilate(&far, 5);
        assert_eq!(d.window, rect(32, 23, 8, 7));
        assert_eq!(d, naive_dilate(&far, 5));
        let whole = Mask::new(image, rect(0, 0, 40, 30), vec![0; 1200]);
        assert_eq!(dilate(&whole, 9).window, whole.window);

        // A candidate touching the right and bottom edges keeps its window inside the image.
        let l = logits_over((64, 48), 8.0, |x, y| x > 40.0 && y > 30.0);
        let m = upsample(&l, &flat_image(64, 48, 100), &Params::default());
        // Positive cells from (5, 4), padded by a cell and the filter's 20 px reach.
        assert_eq!(m.window, rect(12, 4, 52, 44));

        // A band under an object on the bottom edge only reaches up.
        let object = Mask::new((40, 30), rect(10, 20, 10, 10), vec![255; 100]);
        let band = contact_band(&object);
        assert_eq!(band.window.y + band.window.height, 30);
        assert!(band.window.y < 30 && band.get(15, 29) > 200);
    }

    #[test]
    fn refine_upsampled_edge_follows_the_luminance_step() {
        // A bright region left of a diagonal, and a decoder mask of the same region on an 8 px
        // grid: the threshold of its bilinear logits is a staircase, the guided edge is not.
        let size = (256u32, 192u32);
        let edge = |y: f32| 70.0 + 0.4 * y;
        let mut data = Vec::new();
        for y in 0..size.1 {
            for x in 0..size.0 {
                let bright = (x as f32 + 0.5) < edge(y as f32 + 0.5);
                data.extend([if bright { 210 } else { 40 }; 3]);
            }
        }
        let image = Rgb8::new(size.0, size.1, data);
        let l = logits_over(size, 8.0, |x, y| x < edge(y));

        // Per row: the first column below half coverage, against the image's own edge.
        let crossing =
            |inside: &dyn Fn(u32) -> bool| (0..size.0).find(|&x| !inside(x)).unwrap_or(size.0);
        let truth = |y: u32| crossing(&|x| (x as f32 + 0.5) < edge(y as f32 + 0.5)) as i64;
        let mut plain_worst = 0i64;
        for y in 8..size.1 - 8 {
            let plain =
                crossing(&|x| l.sample(Point::new(x as f32 + 0.5, y as f32 + 0.5)) > 0.0) as i64;
            plain_worst = plain_worst.max((plain - truth(y)).abs());
        }
        assert!(
            plain_worst >= 3,
            "the blocky threshold is off by {plain_worst} px"
        );

        for radius in [None, Some(6)] {
            let params = Params {
                guided_radius: radius,
                ..Params::default()
            };
            let m = upsample(&l, &image, &params);
            let mut worst = 0i64;
            for y in 8..size.1 - 8 {
                let guided = crossing(&|x| m.get(x, y) >= 128) as i64;
                worst = worst.max((guided - truth(y)).abs());
            }
            assert!(
                worst <= 1,
                "radius {radius:?}: the guided edge is off by {worst} px"
            );
            assert_eq!(m.get(20, 100), 255, "solid inside");
            assert_eq!(m.get(200, 100), 0, "clear outside");
        }
    }

    #[test]
    fn refine_upsample_window_covers_the_prior_and_the_filter_reach() {
        let size = (256u32, 192u32);
        let l = logits_over(size, 8.0, |x, y| {
            (96.0..160.0).contains(&x) && (64.0..128.0).contains(&y)
        });
        let image = flat_image(size.0, size.1, 90);
        // Cells 12..=19 by 8..=15 are positive and every other is below the prior's floor. The
        // default radius of 8 runs at half resolution with 4 samples: a reach of 20 px, plus a cell.
        let m = upsample(&l, &image, &Params::default());
        assert_eq!(m.window, rect(68, 36, 120, 120));
        // A radius of 3 reaches 8 px, so the three-cell floor of 24 px holds.
        let small = Params {
            guided_radius: Some(3),
            ..Params::default()
        };
        assert_eq!(upsample(&l, &image, &small).window, rect(72, 40, 112, 112));
        // Softer logits spread the prior, and the window follows it rather than cutting it.
        let mut soft = l.clone();
        for x in &mut soft.data {
            *x = if *x > 0.0 { 1.0 } else { -1.0 };
        }
        let m = upsample(&soft, &image, &Params::default());
        let edge = |m: &Mask| {
            let (w, h) = (m.window.width as usize, m.window.height as usize);
            (0..w).all(|x| m.data[x] == 0 && m.data[(h - 1) * w + x] == 0)
                && (0..h).all(|y| m.data[y * w] == 0 && m.data[y * w + w - 1] == 0)
        };
        assert_eq!(m.window, rect(0, 0, 256, 192));
        assert!(m.get(128, 96) > 0 && m.get(128, 96) < 255);
        assert!(edge(&upsample(&l, &image, &Params::default())));
        let none = logits_over(size, 8.0, |_, _| false);
        let empty = upsample(&none, &flat_image(size.0, size.1, 0), &Params::default());
        assert!(empty.window.is_empty());
    }

    #[test]
    fn refine_contact_band_sits_under_the_object() {
        let image = (400, 300);
        let object = Mask::new(image, rect(100, 50, 40, 100), vec![255; 4000]);
        let band = contact_band(&object);
        // Reaches up 20, down 10 and 14 sideways beyond the contact columns 100..140.
        assert_eq!(band.window, rect(86, 130, 68, 30));
        assert!(band.get(120, 149) >= 254 && band.get(120, 150) >= 252);
        assert!(band.get(120, 159) <= 5 && band.get(120, 131) <= 5);
        assert!(band.get(86, 149) <= 3 && band.get(153, 149) <= 3);
        for d in 0..14 {
            assert_eq!(
                band.get(99 - d, 145),
                band.get(140 + d, 145),
                "symmetric at {d}"
            );
        }
        // Falls off smoothly and monotonically below the base.
        for y in 150..159 {
            let step = band.get(120, y) as i32 - band.get(120, y + 1) as i32;
            assert!((0..=40).contains(&step), "row {y}: {step}");
        }

        // A T: only the stem touches the ground.
        let mut t = vec![0u8; 60 * 80];
        for (i, v) in t.iter_mut().enumerate() {
            let (x, y) = (i % 60, i / 60);
            if y < 20 || (25..35).contains(&x) {
                *v = 255;
            }
        }
        let band = contact_band(&Mask::new(image, rect(200, 100, 60, 80), t));
        // The stem's columns 225..235 and 21 px sideways (0.35 × 60).
        assert_eq!(band.window.x, 204);
        assert_eq!(band.window.x + band.window.width, 256);
        assert_eq!(contact_band(&Mask::empty(image)), Mask::empty(image));
    }

    #[test]
    fn refine_removal_region_joins_stroke_and_band_then_dilates() {
        let size = (128u32, 96u32);
        let inside = |x: f32, y: f32| (x - 50.0).powi(2) + (y - 40.0).powi(2) < 400.0;
        let mut data = Vec::new();
        for y in 0..size.1 {
            for x in 0..size.0 {
                let v = if inside(x as f32 + 0.5, y as f32 + 0.5) {
                    220
                } else {
                    30
                };
                data.extend([v; 3]);
            }
        }
        let image = Rgb8::new(size.0, size.1, data);
        let l = logits_over(size, 4.0, inside);
        // A stroke across the object and out over its "shadow" to the right.
        let stroke = Stroke {
            points: vec![Point::new(50.0, 40.0), Point::new(100.0, 45.0)],
            radius: 3.0,
        };
        let bare = Params {
            dilation: Some(0),
            feather: Some(0),
            include_stroke: false,
            ..Params::default()
        };
        let object = removal_region(&l, Some(&stroke), &image, &bare);
        assert_eq!(object.get(50, 40), 255);
        assert_eq!(object.get(95, 44), 0, "the shadow is not the object");
        assert_eq!(
            Some(object.window),
            object_core(&l, &image, &bare).bounds(),
            "trimmed to the object"
        );

        let with_stroke = removal_region(
            &l,
            Some(&stroke),
            &image,
            &Params {
                include_stroke: true,
                ..bare.clone()
            },
        );
        assert_eq!(with_stroke.get(95, 44), 255);

        let dilated = removal_region(
            &l,
            Some(&stroke),
            &image,
            &Params {
                include_stroke: true,
                dilation: Some(5),
                ..bare.clone()
            },
        );
        assert_eq!(dilated.window, with_stroke.window.grown(5, size));
        assert_eq!(dilated.get(107, 45), 255, "the stroke's end grows by 5");
        assert_eq!(dilated, dilate(&with_stroke, 5));

        let banded = removal_region(
            &l,
            None,
            &image,
            &Params {
                contact_band: true,
                ..bare.clone()
            },
        );
        // The object's last solid row is 59, so the band is full at row 60 and fades by row 64.
        assert!(object.get(50, 61) < 64 && banded.get(50, 61) > 128);
        assert!(banded.get(50, 62) > object.get(50, 62));

        // An empty candidate leaves only the stroke.
        let none = logits_over(size, 4.0, |_, _| false);
        let r = removal_region(&none, Some(&stroke), &image, &Params::default());
        assert_eq!(r, footprint_region(&stroke, size, &Params::default()));
        assert!(
            removal_region(&none, None, &image, &Params::default())
                .window
                .is_empty()
        );
    }

    #[test]
    fn the_core_holds_a_weak_small_object_whole_and_drops_faint_coverage() {
        let size = (160u32, 120u32);
        // A flat grey image, so the guided filter has no edge to follow and smooths the peak.
        let image = Rgb8::new(
            size.0,
            size.1,
            vec![128; size.0 as usize * size.1 as usize * 3],
        );
        // An 8 px cell grid: one positive cell (a small object) at logit 1, the ring around it at
        // −1.5 (faint prior coverage), everything else at −6.
        let cell = 8.0f32;
        let (gw, gh) = (20usize, 15usize);
        let mut data = vec![-6.0f32; gw * gh];
        for v in 5..10 {
            for u in 8..13 {
                data[v * gw + u] = -1.5;
            }
        }
        data[7 * gw + 10] = 1.0;
        let l = Logits {
            width: gw,
            height: gh,
            data,
            source: size,
            cells_per_px: (1.0 / cell, 1.0 / cell),
        };
        let soft = upsample(&l, &image, &Params::default());
        let centre = (84u32, 60u32);
        assert!(
            soft.get(centre.0, centre.1) < 200,
            "the guided peak is weak"
        );
        assert!(soft.get(70, 50) > 0, "faint coverage reaches the ring");
        let core = object_core(&l, &image, &Params::default());
        assert_eq!(core.get(centre.0, centre.1), 255);
        assert_eq!(core.get(70, 50), 0, "faint coverage is dropped");
        assert!(core.data.iter().all(|&v| v == 0 || v == 255));
    }

    #[test]
    fn object_dilation_scales_with_the_cell_and_the_object() {
        let image = (6048u32, 4024u32);
        let cell = 6048.0 / 256.0;
        let l = Logits {
            width: 256,
            height: 256,
            data: vec![0.0; 256 * 256],
            source: image,
            cells_per_px: (1.0 / cell, 1.0 / cell),
        };
        let core =
            |w: u32, h: u32| Mask::new(image, rect(1000, 1000, w, h), vec![255; (w * h) as usize]);
        // Half a 23.6 px cell plus 4 % of the shorter side.
        assert_eq!(object_dilation(&core(700, 1800), &l, 6), 40);
        assert_eq!(object_dilation(&core(40, 150), &l, 6), 13);
        // Capped by the image's default dilation less the feather.
        assert_eq!(
            object_dilation(&core(3000, 3000), &l, 6),
            default_dilation(image) - 6
        );
        assert_eq!(default_feather(&l), 24);
    }

    #[test]
    fn refine_removal_region_stays_bounded_at_60_mp() {
        // A 9504 × 6336 frame (60.2 MP) with a 1200 × 1800 px ellipse, under EfficientViT-SAM's
        // 256² grid over the padded 9504² frame: one cell is 37.125 px.
        let (w, h) = (9504u32, 6336u32);
        let (cx, cy, rx, ry) = (4000.0f32, 3000.0f32, 600.0f32, 900.0f32);
        let inside = |x: f32, y: f32| ((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2) < 1.0;
        let mut data = vec![0u8; w as usize * h as usize * 3];
        for y in (cy - ry) as u32..(cy + ry) as u32 {
            for x in (cx - rx) as u32..(cx + rx) as u32 {
                if inside(x as f32 + 0.5, y as f32 + 0.5) {
                    let i = (y as usize * w as usize + x as usize) * 3;
                    data[i..i + 3].copy_from_slice(&[200, 190, 180]);
                }
            }
        }
        let image = Rgb8::new(w, h, data);
        let cpp = 256.0 / 9504.0;
        let mut cells = Vec::with_capacity(256 * 256);
        for v in 0..256 {
            for u in 0..256 {
                let (x, y) = ((u as f32 + 0.5) / cpp, (v as f32 + 0.5) / cpp);
                let norm = (((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt();
                let signed = (1.0 - norm) * rx.min(ry);
                cells.push(if y >= h as f32 {
                    -20.0
                } else {
                    (signed / 4.0).clamp(-20.0, 20.0)
                });
            }
        }
        let logits = Logits {
            width: 256,
            height: 256,
            data: cells,
            source: (w, h),
            cells_per_px: (cpp, cpp),
        };
        let stroke = Stroke {
            points: vec![Point::new(3700.0, 2600.0), Point::new(4300.0, 3500.0)],
            radius: 60.0,
        };
        let params = Params {
            contact_band: true,
            ..Params::default()
        };

        let started = std::time::Instant::now();
        let upsampled = upsample(&logits, &image, &params);
        let upsampled_in = started.elapsed();
        let region = removal_region(&logits, Some(&stroke), &image, &params);
        let total = started.elapsed();

        let cell = 9504.0 / 256.0;
        let object = rect(3400, 2100, 1200, 1800);
        let dilation = default_dilation((w, h)) as f64;
        assert_eq!(dilation, 95.0);
        let grown = |r: RectPx, left: f64, top: f64, right: f64, bottom: f64| {
            (
                (r.x as f64 - left).max(0.0),
                (r.y as f64 - top).max(0.0),
                (r.x as f64 + r.width as f64 + right).min(w as f64),
                (r.y as f64 + r.height as f64 + bottom).min(h as f64),
            )
        };
        let within = |r: RectPx, b: (f64, f64, f64, f64)| {
            r.x as f64 >= b.0
                && r.y as f64 >= b.1
                && (r.x + r.width) as f64 <= b.2
                && (r.y + r.height) as f64 <= b.3
        };
        // The upsampled window and its coverage stay within about four cells of the object: a cell
        // of prior beyond it, and the guided filter's reach of 90 px.
        let halo = 5.0 * cell;
        assert!(
            within(upsampled.window, grown(object, halo, halo, halo, halo)),
            "{:?}",
            upsampled.window
        );
        // The region adds the band, 0.35 × 1200 px sideways and 0.1 × 1800 px below, then dilation.
        let side = (0.35 * 1200.0f64).max(halo) + dilation;
        let below = (180.0f64).max(halo) + dilation;
        let bound = grown(object, side, halo + dilation, side, below);
        assert!(
            within(region.window, bound),
            "{:?} within {bound:?}",
            region.window
        );
        assert!(within(object, grown(region.window, 0.0, 0.0, 0.0, 0.0)));
        let frame = w as u64 * h as u64;
        let upsampled_px = upsampled.data.len() as u64;
        let region_px = region.data.len() as u64;
        assert_eq!(
            region_px,
            region.window.width as u64 * region.window.height as u64
        );
        assert!(region_px * 10 < frame, "{region_px} of {frame} pixels");
        assert_eq!(region.get(cx as u32, cy as u32), 255);
        assert_eq!(region.get(3700, 2600), 255);
        assert_eq!(region.get(100, 100), 0);
        eprintln!(
            "60 MP removal region: upsampled {}×{} ({:.1}% of the frame) in {upsampled_in:.2?}; \
             region {}×{} ({:.1}%) in {total:.2?} overall",
            upsampled.window.width,
            upsampled.window.height,
            100.0 * upsampled_px as f64 / frame as f64,
            region.window.width,
            region.window.height,
            100.0 * region_px as f64 / frame as f64,
        );
    }
}
