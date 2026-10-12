//! A bounded, exposure-invariant feature detector and binary descriptor on a frame's
//! [`PanoramaProxy`]: oriented multi-scale corners in the manner of Brown, Szeliski and Winder's
//! MOPS, described by intensity comparisons in the manner of BRIEF.
//!
//! On the proxy's `log2` luminance, where a frame's exposure and gain are a constant offset:
//!
//! 1. **Pyramid.** Up to [`MAX_LEVELS`] levels, each the one before smoothed by the binomial
//!    `[1 4 6 4 1]/16` and decimated by 2 (level `ℓ` pixel `x` is proxy point `2^ℓ·x`), while the
//!    shorter side stays at least [`MIN_LEVEL_SIDE`]. A level pixel is usable when every proxy
//!    pixel its smoothing reads is.
//! 2. **Corners.** On each level, the Harris structure tensor of the level smoothed by
//!    [`DERIVATIVE_SIGMA`] (central differences), integrated over [`INTEGRATION_SIGMA`]; a corner's
//!    response is the tensor's harmonic mean `det/trace`, at least [`MIN_RESPONSE`] (EV² per level
//!    pixel²), and a strict 3 × 3 maximum (ties go to the earlier pixel in raster order), at least
//!    [`BORDER`] level pixels from the edge, with every pixel its descriptor reads usable. Its
//!    position is refined by one Newton step on the response's 3 × 3 quadratic (its central
//!    gradient and Hessian, cross term included) when that is a maximum within
//!    [`MAX_REFINEMENT`] pixels, else by the vertex of a parabola through it in each direction
//!    (within half a pixel).
//! 3. **Selection.** Each [`CELL`]-pixel cell of a level keeps its [`PER_CELL`] strongest corners,
//!    so features spread over the frame. Each level then takes its share of [`MAX_FEATURES`] in
//!    proportion to its area, strongest first, and any slots a level leaves fill from the other
//!    levels' remaining corners, strongest first. Ties order by level, row and column, so the
//!    selection is deterministic.
//! 4. **Orientation.** The direction from the corner to the centroid of the level smoothed by
//!    [`DESCRIPTOR_SIGMA`], minus its mean, over a disc of radius [`ORIENTATION_RADIUS`].
//! 5. **Descriptor.** An 8 × 8 grid [`DESCRIPTOR_SPACING`] level pixels apart, turned to the
//!    orientation and sampled bilinearly from the same smoothed level, and 256 fixed comparisons
//!    between its samples ([`PAIRS`]): bit `b` is set when sample `PAIRS[b].0` is darker than
//!    sample `PAIRS[b].1`. Comparisons ignore any monotone change of brightness, so exposure,
//!    gain and the response curve do not change a descriptor.
//!
//! Bounds: at most [`MAX_FEATURES`] features a frame, 56 bytes each; the scratch of one detection
//! is [`FrameFeatures::scratch_bytes_for`] (40 bytes a proxy pixel: 60 MiB for a 1536 × 1024
//! proxy, 90 MiB for the largest square one), released before it returns. `cancel` is checked per level and every 64 rows of
//! each pass.

use super::proxy::PanoramaProxy;
use crate::{Cancel, Error};

/// The most features one frame keeps.
pub const MAX_FEATURES: usize = 4000;
/// The most pyramid levels, the proxy included.
pub const MAX_LEVELS: usize = 3;
/// A level is not made below this many pixels on its shorter side.
pub const MIN_LEVEL_SIDE: usize = 64;
/// The derivative scale of the corner response, in level pixels.
pub const DERIVATIVE_SIGMA: f32 = 1.0;
/// The integration scale of the structure tensor, in level pixels.
pub const INTEGRATION_SIGMA: f32 = 1.5;
/// The weakest corner response kept: the structure tensor's harmonic mean in EV² per level pixel².
/// Sensor noise of a few percent in the shadows makes about `3·10⁻⁵`; a corner between surfaces a
/// stop apart makes `10⁻²` or more.
pub const MIN_RESPONSE: f32 = 1e-3;
/// The furthest a corner's Newton refinement may move it on either axis, in level pixels.
pub const MAX_REFINEMENT: f32 = 0.75;
/// The side of a selection cell, in level pixels.
pub const CELL: usize = 16;
/// The most corners one cell keeps.
pub const PER_CELL: usize = 3;
/// The smoothing the orientation and descriptor read, in level pixels.
pub const DESCRIPTOR_SIGMA: f32 = 2.0;
/// The descriptor grid's spacing, in level pixels.
pub const DESCRIPTOR_SPACING: f32 = 4.0;
/// The descriptor grid's side.
const GRID: usize = 8;
/// The orientation disc's radius, in level pixels.
pub const ORIENTATION_RADIUS: i32 = 9;
/// Corners keep this far from a level's edge, in level pixels, so the turned descriptor grid
/// (reaching `3.5·√2·DESCRIPTOR_SPACING`, under 20 pixels) and the orientation disc stay inside it.
pub const BORDER: usize = 21;
/// Rows between cancellation checks in each pass.
const CHECK_ROWS: usize = 64;

/// The descriptor's 256 comparisons between grid samples, numbered row-major: fixed pseudo-random
/// pairs of distinct samples, drawn once from a xorshift sequence and never changed (a changed
/// table changes every descriptor).
pub const PAIRS: [(u8, u8); 256] = pairs();

const fn pairs() -> [(u8, u8); 256] {
    let mut out = [(0u8, 0u8); 256];
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut n = 0;
    while n < 256 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let a = (state % 64) as u8;
        let b = ((state >> 32) % 64) as u8;
        if a != b {
            let mut repeated = false;
            let mut k = 0;
            while k < n {
                if (out[k].0 == a && out[k].1 == b) || (out[k].0 == b && out[k].1 == a) {
                    repeated = true;
                }
                k += 1;
            }
            if !repeated {
                out[n] = (a, b);
                n += 1;
            }
        }
    }
    out
}

/// One feature: its full-resolution position, the pyramid level it was found on, its orientation
/// in radians (in the frame's pixel axes) and its corner response.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Feature {
    pub x: f64,
    pub y: f64,
    pub level: u8,
    pub angle: f32,
    pub response: f32,
}

/// A 256-bit descriptor.
pub type Descriptor = [u64; 4];

/// The features of one frame, with the frame's full-resolution size.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameFeatures {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) factor: u32,
    pub(crate) features: Vec<Feature>,
    pub(crate) descriptors: Vec<Descriptor>,
}

/// One pyramid level: its `log2` luminance and usability.
struct Level {
    width: usize,
    height: usize,
    luma: Vec<f32>,
    usable: Vec<bool>,
}

/// A corner found on a level, before selection.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    level: usize,
    x: usize,
    y: usize,
    response: f32,
    /// The sub-pixel offset of the refined position from the pixel.
    offset: (f32, f32),
}

impl FrameFeatures {
    /// Detect at most [`MAX_FEATURES`] features on `proxy`. See the module documentation.
    pub fn detect(proxy: &PanoramaProxy, cancel: &Cancel) -> Result<Self, Error> {
        Self::detect_at_most(proxy, MAX_FEATURES, cancel)
    }

    /// [`Self::detect`] keeping at most `limit` features, `limit` itself at most [`MAX_FEATURES`].
    pub fn detect_at_most(
        proxy: &PanoramaProxy,
        limit: usize,
        cancel: &Cancel,
    ) -> Result<Self, Error> {
        let limit = limit.min(MAX_FEATURES);
        let mut levels = vec![Level {
            width: proxy.width,
            height: proxy.height,
            luma: proxy.log_luma.clone(),
            usable: proxy.usable.clone(),
        }];
        while levels.len() < MAX_LEVELS {
            cancel.check()?;
            let last = levels.last().expect("the proxy level");
            if last.width / 2 < MIN_LEVEL_SIDE || last.height / 2 < MIN_LEVEL_SIDE {
                break;
            }
            let next = decimate(last, cancel)?;
            levels.push(next);
        }
        let mut candidates = Vec::new();
        let mut smoothed = Vec::with_capacity(levels.len());
        for (index, level) in levels.iter().enumerate() {
            cancel.check()?;
            let response = corner_response(level, cancel)?;
            let found = corners(index, level, &response, cancel)?;
            drop(response);
            let described = gaussian(
                &level.luma,
                level.width,
                level.height,
                DESCRIPTOR_SIGMA,
                cancel,
            )?;
            candidates.push(found);
            smoothed.push(described);
        }
        let chosen = select(&levels, candidates, limit);
        let mut features = Vec::with_capacity(chosen.len());
        let mut descriptors = Vec::with_capacity(chosen.len());
        for (n, candidate) in chosen.into_iter().enumerate() {
            if n % 256 == 0 {
                cancel.check()?;
            }
            let level = &levels[candidate.level];
            let image = &smoothed[candidate.level];
            let (x, y) = (
                candidate.x as f32 + candidate.offset.0,
                candidate.y as f32 + candidate.offset.1,
            );
            let angle = orientation(image, level.width, candidate.x, candidate.y);
            let descriptor = describe(image, level.width, level.height, x, y, angle);
            let s = f64::from(1u32 << candidate.level);
            let (fx, fy) = proxy.to_frame(f64::from(x) * s, f64::from(y) * s);
            features.push(Feature {
                x: fx,
                y: fy,
                level: candidate.level as u8,
                angle,
                response: candidate.response,
            });
            descriptors.push(descriptor);
        }
        Ok(Self {
            width: proxy.frame_width,
            height: proxy.frame_height,
            factor: proxy.factor,
            features,
            descriptors,
        })
    }

    pub fn len(&self) -> usize {
        self.features.len()
    }

    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    pub fn features(&self) -> &[Feature] {
        &self.features
    }

    /// The full-resolution frame the features were found on.
    pub fn frame_dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The proxy's box factor: one proxy pixel is this many frame pixels.
    pub fn factor(&self) -> u32 {
        self.factor
    }

    /// The bytes the features hold.
    pub fn bytes(&self) -> usize {
        self.features.len() * (size_of::<Feature>() + size_of::<Descriptor>())
    }

    /// The most bytes one detection on a `width × height` proxy holds at once besides the proxy
    /// and its result: the pyramid (a copy of the proxy and two smaller levels, 5 bytes a pixel)
    /// and, on the level being searched, its smoothed copy, gradients, tensor products, a pass
    /// buffer and its response (7 `f32` planes), plus every level's descriptor smoothing.
    pub fn scratch_bytes_for(width: usize, height: usize) -> usize {
        let area = width * height;
        let pyramid = area + area / 4 + area / 16;
        pyramid * (size_of::<f32>() + size_of::<bool>())
            + 7 * area * size_of::<f32>()
            + pyramid * size_of::<f32>()
    }
}

/// The next pyramid level: `[1 4 6 4 1]/16` smoothing, then every other pixel; usable where the
/// whole 5 × 5 footprint is.
fn decimate(level: &Level, cancel: &Cancel) -> Result<Level, Error> {
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let (w, h) = (level.width, level.height);
    let (nw, nh) = (w / 2, h / 2);
    // Horizontal pass at the kept columns only.
    let mut rows = vec![0.0f32; nw * h];
    let mut rows_ok = vec![false; nw * h];
    for y in 0..h {
        if y % CHECK_ROWS == 0 {
            cancel.check()?;
        }
        for i in 0..nw {
            let cx = 2 * i;
            let (mut sum, mut ok) = (0.0, true);
            for (t, k) in K.iter().enumerate() {
                let x = (cx + t).saturating_sub(2).min(w - 1);
                sum += k * level.luma[y * w + x];
                ok &= level.usable[y * w + x];
            }
            rows[y * nw + i] = sum;
            rows_ok[y * nw + i] = ok;
        }
    }
    let mut luma = vec![0.0f32; nw * nh];
    let mut usable = vec![false; nw * nh];
    for j in 0..nh {
        if j % CHECK_ROWS == 0 {
            cancel.check()?;
        }
        let cy = 2 * j;
        for i in 0..nw {
            let (mut sum, mut ok) = (0.0, true);
            for (t, k) in K.iter().enumerate() {
                let y = (cy + t).saturating_sub(2).min(h - 1);
                sum += k * rows[y * nw + i];
                ok &= rows_ok[y * nw + i];
            }
            luma[j * nw + i] = sum;
            usable[j * nw + i] = ok;
        }
    }
    Ok(Level {
        width: nw,
        height: nh,
        luma,
        usable,
    })
}

/// A sampled, normalized Gaussian of `sigma` out to `3σ`.
fn kernel(sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as i32;
    let mut k: Vec<f32> = (-radius..=radius)
        .map(|t| (-(t * t) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= sum);
    k
}

/// `image` smoothed by a separable Gaussian, edges clamped.
fn gaussian(
    image: &[f32],
    w: usize,
    h: usize,
    sigma: f32,
    cancel: &Cancel,
) -> Result<Vec<f32>, Error> {
    let k = kernel(sigma);
    let r = (k.len() / 2) as isize;
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        if y % CHECK_ROWS == 0 {
            cancel.check()?;
        }
        let row = &image[y * w..(y + 1) * w];
        for x in 0..w {
            let mut sum = 0.0;
            for (t, kv) in k.iter().enumerate() {
                let sx = (x as isize + t as isize - r).clamp(0, w as isize - 1) as usize;
                sum += kv * row[sx];
            }
            tmp[y * w + x] = sum;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        if y % CHECK_ROWS == 0 {
            cancel.check()?;
        }
        for (t, kv) in k.iter().enumerate() {
            let sy = (y as isize + t as isize - r).clamp(0, h as isize - 1) as usize;
            let (src, dst) = (&tmp[sy * w..(sy + 1) * w], &mut out[y * w..(y + 1) * w]);
            for (d, s) in dst.iter_mut().zip(src) {
                *d += kv * s;
            }
        }
    }
    Ok(out)
}

/// The harmonic-mean corner response of a level.
fn corner_response(level: &Level, cancel: &Cancel) -> Result<Vec<f32>, Error> {
    let (w, h) = (level.width, level.height);
    let smooth = gaussian(&level.luma, w, h, DERIVATIVE_SIGMA, cancel)?;
    let mut xx = vec![0.0f32; w * h];
    let mut yy = vec![0.0f32; w * h];
    let mut xy = vec![0.0f32; w * h];
    for y in 0..h {
        if y % CHECK_ROWS == 0 {
            cancel.check()?;
        }
        let (up, down) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for x in 0..w {
            let (left, right) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let gx = 0.5 * (smooth[y * w + right] - smooth[y * w + left]);
            let gy = 0.5 * (smooth[down * w + x] - smooth[up * w + x]);
            let i = y * w + x;
            xx[i] = gx * gx;
            yy[i] = gy * gy;
            xy[i] = gx * gy;
        }
    }
    drop(smooth);
    let sxx = gaussian(&xx, w, h, INTEGRATION_SIGMA, cancel)?;
    drop(xx);
    let syy = gaussian(&yy, w, h, INTEGRATION_SIGMA, cancel)?;
    drop(yy);
    let sxy = gaussian(&xy, w, h, INTEGRATION_SIGMA, cancel)?;
    drop(xy);
    Ok((0..w * h)
        .map(|i| {
            let det = sxx[i] * syy[i] - sxy[i] * sxy[i];
            let trace = sxx[i] + syy[i];
            if trace > 0.0 {
                (det / trace).max(0.0)
            } else {
                0.0
            }
        })
        .collect())
}

/// The vertex of the parabola through `a`, `m` and `b` at −1, 0 and 1, within half a pixel.
fn vertex(a: f32, b: f32, m: f32) -> f32 {
    let curvature = a - 2.0 * m + b;
    if curvature < 0.0 {
        (0.5 * (a - b) / curvature).clamp(-0.5, 0.5)
    } else {
        0.0
    }
}

/// The sub-pixel offset of a response peak `v` with neighbours `at(dx, dy)`. See the module
/// documentation.
fn refine(at: impl Fn(isize, isize) -> f32, v: f32) -> (f32, f32) {
    let gx = 0.5 * (at(1, 0) - at(-1, 0));
    let gy = 0.5 * (at(0, 1) - at(0, -1));
    let hxx = at(1, 0) - 2.0 * v + at(-1, 0);
    let hyy = at(0, 1) - 2.0 * v + at(0, -1);
    let hxy = 0.25 * (at(1, 1) - at(1, -1) - at(-1, 1) + at(-1, -1));
    let det = hxx * hyy - hxy * hxy;
    if det > 0.0 && hxx < 0.0 {
        let ox = -(hyy * gx - hxy * gy) / det;
        let oy = -(hxx * gy - hxy * gx) / det;
        if ox.abs() <= MAX_REFINEMENT && oy.abs() <= MAX_REFINEMENT {
            return (ox, oy);
        }
    }
    (
        vertex(at(-1, 0), at(1, 0), v),
        vertex(at(0, -1), at(0, 1), v),
    )
}

/// Whether every sample the descriptor grid and orientation disc of a corner at `(x, y)` read is
/// usable, for any orientation: the disc of radius `BORDER` around it.
fn footprint_usable(level: &Level, x: usize, y: usize) -> bool {
    let r = BORDER as isize - 1;
    let r2 = r * r;
    for dy in -r..=r {
        let row = (y as isize + dy) as usize * level.width;
        for dx in -r..=r {
            if dx * dx + dy * dy <= r2 && !level.usable[row + (x as isize + dx) as usize] {
                return false;
            }
        }
    }
    true
}

/// The corners of one level: strict local maxima of the response above [`MIN_RESPONSE`], inside
/// the border, with a usable footprint, each cell keeping its strongest [`PER_CELL`].
fn corners(
    index: usize,
    level: &Level,
    response: &[f32],
    cancel: &Cancel,
) -> Result<Vec<Candidate>, Error> {
    let (w, h) = (level.width, level.height);
    if w <= 2 * BORDER || h <= 2 * BORDER {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for y in BORDER..h - BORDER {
        if y % CHECK_ROWS == 0 {
            cancel.check()?;
        }
        for x in BORDER..w - BORDER {
            let v = response[y * w + x];
            if !(v >= MIN_RESPONSE) {
                continue;
            }
            let mut peak = true;
            'neighbours: for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let n = response[(y as isize + dy) as usize * w + (x as isize + dx) as usize];
                    // Earlier neighbours in raster order win ties.
                    let earlier = dy < 0 || (dy == 0 && dx < 0);
                    if n > v || (earlier && n == v) {
                        peak = false;
                        break 'neighbours;
                    }
                }
            }
            if peak && footprint_usable(level, x, y) {
                let at = |dx: isize, dy: isize| {
                    response[(y as isize + dy) as usize * w + (x as isize + dx) as usize]
                };
                found.push(Candidate {
                    level: index,
                    x,
                    y,
                    response: v,
                    offset: refine(at, v),
                });
            }
        }
    }
    // Strongest first within each cell, then keep each cell's first PER_CELL.
    let cells_x = w.div_ceil(CELL);
    found.sort_by(|a, b| {
        let ca = (a.y / CELL) * cells_x + a.x / CELL;
        let cb = (b.y / CELL) * cells_x + b.x / CELL;
        ca.cmp(&cb)
            .then(b.response.total_cmp(&a.response))
            .then((a.y, a.x).cmp(&(b.y, b.x)))
    });
    let mut kept = Vec::with_capacity(found.len());
    let mut last_cell = usize::MAX;
    let mut in_cell = 0;
    for c in found {
        let cell = (c.y / CELL) * cells_x + c.x / CELL;
        if cell != last_cell {
            last_cell = cell;
            in_cell = 0;
        }
        if in_cell < PER_CELL {
            kept.push(c);
            in_cell += 1;
        }
    }
    Ok(kept)
}

fn stronger(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.response
        .total_cmp(&a.response)
        .then((a.level, a.y, a.x).cmp(&(b.level, b.y, b.x)))
}

/// The selected corners in a deterministic order: by level, then strongest first.
fn select(levels: &[Level], candidates: Vec<Vec<Candidate>>, limit: usize) -> Vec<Candidate> {
    let areas: Vec<usize> = levels.iter().map(|l| l.width * l.height).collect();
    let total: usize = areas.iter().sum();
    let mut chosen = Vec::new();
    let mut rest = Vec::new();
    for (index, mut list) in candidates.into_iter().enumerate() {
        list.sort_by(stronger);
        let quota = limit * areas[index] / total.max(1);
        let split = quota.min(list.len());
        rest.extend(list.drain(split..));
        chosen.extend(list);
    }
    rest.sort_by(stronger);
    let room = limit.saturating_sub(chosen.len());
    chosen.extend(rest.into_iter().take(room));
    chosen.sort_by(|a, b| a.level.cmp(&b.level).then(stronger(a, b)));
    chosen
}

/// The orientation of a corner: from it to the centroid of the smoothed level minus its mean, over
/// a disc of radius [`ORIENTATION_RADIUS`].
fn orientation(image: &[f32], w: usize, x: usize, y: usize) -> f32 {
    let r = ORIENTATION_RADIUS;
    // Σ d·(v − v̄) = Σ d·v, since Σ d = 0 over the symmetric disc: the mean drops out.
    let mut m = [0.0f64; 2];
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy > r * r {
                continue;
            }
            let v = f64::from(image[(y as i32 + dy) as usize * w + (x as i32 + dx) as usize]);
            m[0] += f64::from(dx) * v;
            m[1] += f64::from(dy) * v;
        }
    }
    if m[0] == 0.0 && m[1] == 0.0 {
        0.0
    } else {
        m[1].atan2(m[0]) as f32
    }
}

/// Bilinear sample of a level, edges clamped.
#[inline]
fn bilinear(image: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let top = image[y0 * w + x0] * (1.0 - fx) + image[y0 * w + x1] * fx;
    let bottom = image[y1 * w + x0] * (1.0 - fx) + image[y1 * w + x1] * fx;
    top * (1.0 - fy) + bottom * fy
}

/// The descriptor of a corner at `(x, y)` with orientation `angle`.
fn describe(image: &[f32], w: usize, h: usize, x: f32, y: f32, angle: f32) -> Descriptor {
    let (s, c) = angle.sin_cos();
    let mut samples = [0.0f32; GRID * GRID];
    for gj in 0..GRID {
        for gi in 0..GRID {
            let u = (gi as f32 - 3.5) * DESCRIPTOR_SPACING;
            let v = (gj as f32 - 3.5) * DESCRIPTOR_SPACING;
            samples[gj * GRID + gi] = bilinear(image, w, h, x + c * u - s * v, y + s * u + c * v);
        }
    }
    let mut bits = [0u64; 4];
    for (b, &(p, q)) in PAIRS.iter().enumerate() {
        if samples[p as usize] < samples[q as usize] {
            bits[b / 64] |= 1 << (b % 64);
        }
    }
    bits
}
