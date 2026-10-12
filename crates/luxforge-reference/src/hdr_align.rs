//! Independent f64 references for HDR bracket alignment (`docs/design/hdr-merge.md`, "Alignment,
//! coverage and motion"): known rigid camera motions between bracket frames, textured and moving
//! scenes to record through [`crate::hdr::expose`], the bilinear resample of a frame into the
//! reference's coordinates, and the largest common rectangle every aligned frame covers.
//!
//! Coordinates are pixel indices: pixel `(x, y)`'s centre is at `(x, y)`. A [`Rigid`] maps a
//! reference-coordinate point `p` to the point `q` of the input frame that recorded it,
//! `q = c + R(θ)(p − c) + t`, with `c` the frame centre `((W − 1)/2, (H − 1)/2)`. The scene is
//! defined in reference coordinates everywhere on the plane, so a frame moved off the reference
//! records scene beyond the reference's edge, which an aligned output must never show.
//!
//! Written from the design, not from the production code; no production code depends on this
//! file and it imports nothing from the core.

use crate::SplitMix64;
use crate::hdr::{Camera, Exposure, Noise, expose};

/// A rigid motion about the frame centre, from reference coordinates to an input's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rigid {
    pub center: (f64, f64),
    /// Radians, counter-clockwise in a y-down image (positive turns +x towards +y).
    pub rotation: f64,
    pub shift: (f64, f64),
}

impl Rigid {
    /// The motion of a `width × height` frame about its centre.
    pub fn new(width: usize, height: usize, rotation_deg: f64, shift: (f64, f64)) -> Self {
        Self {
            center: ((width as f64 - 1.0) / 2.0, (height as f64 - 1.0) / 2.0),
            rotation: rotation_deg.to_radians(),
            shift,
        }
    }

    pub fn identity(width: usize, height: usize) -> Self {
        Self::new(width, height, 0.0, (0.0, 0.0))
    }

    /// Where the input recorded reference point `p`.
    pub fn map(&self, p: (f64, f64)) -> (f64, f64) {
        let (s, c) = self.rotation.sin_cos();
        let (dx, dy) = (p.0 - self.center.0, p.1 - self.center.1);
        (
            self.center.0 + c * dx - s * dy + self.shift.0,
            self.center.1 + s * dx + c * dy + self.shift.1,
        )
    }

    /// The reference point an input pixel `q` recorded.
    pub fn unmap(&self, q: (f64, f64)) -> (f64, f64) {
        let (s, c) = self.rotation.sin_cos();
        let (dx, dy) = (
            q.0 - self.shift.0 - self.center.0,
            q.1 - self.shift.1 - self.center.1,
        );
        (
            self.center.0 + c * dx + s * dy,
            self.center.1 - s * dx + c * dy,
        )
    }
}

/// A smooth, textured scene of linear radiance at the reference's exposure: a log-luminance
/// pattern of Gaussian blobs on a jittered 60-pixel grid (one per cell, σ 5 to 25 pixels,
/// ±2 stops) and two gentle oblique gratings (periods 40 to 90 pixels), between `2^−6` and
/// `2^0.5` of white, tinted slightly warm. No feature is narrower than about five pixels, so a
/// point-sampled frame does not alias. Defined on the whole plane, deterministic in `seed`.
pub fn textured_scene(seed: u64) -> impl Fn(f64, f64) -> [f64; 3] {
    const CELL: f64 = 60.0;
    let blob = move |i: i64, j: i64| {
        let mut rng = SplitMix64(
            seed ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (j as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F),
        );
        (
            CELL * (i as f64 + rng.next_range(0.0, 1.0)),
            CELL * (j as f64 + rng.next_range(0.0, 1.0)),
            rng.next_range(5.0, 25.0),
            rng.next_range(-2.0, 2.0),
        )
    };
    let mut rng = SplitMix64(seed);
    let gratings: Vec<(f64, f64, f64, f64)> = (0..2)
        .map(|_| {
            let angle = rng.next_range(0.0, std::f64::consts::PI);
            let period = rng.next_range(40.0, 90.0);
            (
                angle.cos() / period,
                angle.sin() / period,
                rng.next_range(0.0, 1.0),
                rng.next_range(0.2, 0.4),
            )
        })
        .collect();
    move |x, y| {
        let mut stops = -2.5;
        let (ci, cj) = ((x / CELL).floor() as i64, (y / CELL).floor() as i64);
        // σ is at most 25: blobs two cells away contribute under e^−11.
        for j in cj - 2..=cj + 2 {
            for i in ci - 2..=ci + 2 {
                let (bx, by, sigma, amplitude) = blob(i, j);
                let d2 = (x - bx).powi(2) + (y - by).powi(2);
                stops += amplitude * (-d2 / (2.0 * sigma * sigma)).exp();
            }
        }
        for &(fx, fy, phase, amplitude) in &gratings {
            stops += amplitude * (std::f64::consts::TAU * (fx * x + fy * y + phase)).sin();
        }
        let l = stops.clamp(-6.0, 0.5).exp2();
        [l, 0.85 * l, 0.7 * l]
    }
}

/// `scene` with a uniform horizontal ramp only: no texture to align.
pub fn flat_scene(level: f64) -> impl Fn(f64, f64) -> [f64; 3] {
    move |x, _| {
        let l = level * (1.0 + x * 1e-4);
        [l, l, l]
    }
}

/// `scene` with a disc of `radius` and radiance `colour` centred at `centre`: a subject that the
/// background does not explain.
pub fn with_disc(
    scene: impl Fn(f64, f64) -> [f64; 3],
    centre: (f64, f64),
    radius: f64,
    colour: [f64; 3],
) -> impl Fn(f64, f64) -> [f64; 3] {
    move |x, y| {
        if (x - centre.0).powi(2) + (y - centre.1).powi(2) <= radius * radius {
            colour
        } else {
            scene(x, y)
        }
    }
}

/// `scene` brightened by `stops_per_pixel · (centre − x)` stops: a wide-range scene whose left
/// side only short exposures record and whose right side only long ones do.
pub fn with_ramp(
    scene: impl Fn(f64, f64) -> [f64; 3],
    stops_per_pixel: f64,
    centre: f64,
) -> impl Fn(f64, f64) -> [f64; 3] {
    move |x, y| {
        let gain = (stops_per_pixel * (centre - x)).exp2();
        scene(x, y).map(|v| v * gain)
    }
}

/// Record `scene` (reference coordinates) as an input that moved by `rigid`, at `offset_ev` stops
/// of exposure, through `camera`: input pixel `q` sees the scene at `rigid.unmap(q)`.
pub fn record(
    scene: &dyn Fn(f64, f64) -> [f64; 3],
    width: usize,
    height: usize,
    rigid: &Rigid,
    camera: &Camera,
    offset_ev: f64,
    noise: Option<Noise>,
) -> Exposure {
    let at = |x: usize, y: usize| {
        let p = rigid.unmap((x as f64, y as f64));
        scene(p.0, p.1)
    };
    expose(&at, width, height, camera, offset_ev, noise)
}

/// Whether input `q` lies where a bilinear sample reads only recorded pixels.
fn inside(q: (f64, f64), width: usize, height: usize) -> bool {
    q.0 >= 0.0 && q.1 >= 0.0 && q.0 <= (width - 1) as f64 && q.1 <= (height - 1) as f64
}

/// One frame resampled into reference coordinates over `rect` (`x, y, width, height`): per
/// output pixel the bilinear blend of the four input pixels around `rigid.map(p)` in f64, rounded
/// once to f32, and the largest level code among the input pixels the blend weights above zero.
/// `None` when any output pixel would read outside the frame.
pub fn resample(
    frame: &Exposure,
    rigid: &Rigid,
    rect: (usize, usize, usize, usize),
) -> Option<([Vec<f32>; 3], Vec<u16>)> {
    let (x0, y0, w, h) = rect;
    let mut rgb = [vec![0.0; w * h], vec![0.0; w * h], vec![0.0; w * h]];
    let mut levels = vec![0_u16; w * h];
    for j in 0..h {
        for i in 0..w {
            let q = rigid.map(((x0 + i) as f64, (y0 + j) as f64));
            if !inside(q, frame.width, frame.height) {
                return None;
            }
            let (fx, fy) = (q.0.floor(), q.1.floor());
            let (ax, ay) = (q.0 - fx, q.1 - fy);
            let mut acc = [0.0_f64; 3];
            let mut level = 0_u16;
            for (dx, dy, weight) in [
                (0, 0, (1.0 - ax) * (1.0 - ay)),
                (1, 0, ax * (1.0 - ay)),
                (0, 1, (1.0 - ax) * ay),
                (1, 1, ax * ay),
            ] {
                if weight == 0.0 {
                    continue;
                }
                let index = (fy as usize + dy) * frame.width + fx as usize + dx;
                for (c, value) in acc.iter_mut().enumerate() {
                    *value += weight * f64::from(frame.rgb[c][index]);
                }
                level = level.max(frame.levels[index]);
            }
            let o = j * w + i;
            for c in 0..3 {
                rgb[c][o] = acc[c] as f32;
            }
            levels[o] = level;
        }
    }
    Some((rgb, levels))
}

/// Which reference pixels every motion covers (bilinear reads only recorded input pixels), and the
/// smallest distance of any pixel's mapped point from an input's edge, so a test can see no pixel
/// sits on a rounding tie.
pub fn covered(width: usize, height: usize, rigids: &[Rigid]) -> (Vec<bool>, f64) {
    let mut mask = vec![true; width * height];
    let mut margin = f64::INFINITY;
    for y in 0..height {
        for x in 0..width {
            for rigid in rigids {
                let q = rigid.map((x as f64, y as f64));
                for distance in [
                    q.0,
                    q.1,
                    (width - 1) as f64 - q.0,
                    (height - 1) as f64 - q.1,
                ] {
                    margin = margin.min(distance.abs());
                }
                if !inside(q, width, height) {
                    mask[y * width + x] = false;
                }
            }
        }
    }
    (mask, margin)
}

/// The largest axis-aligned rectangle of `true` pixels, `(x, y, width, height)`, by exhaustive
/// search: every row span `y0..=y1`, the longest run of columns true in all its rows. Ties keep
/// the smallest `y0`, then the smallest `y1`, then the leftmost run.
pub fn largest_rectangle(
    mask: &[bool],
    width: usize,
    height: usize,
) -> Option<(usize, usize, usize, usize)> {
    let mut best: Option<(usize, (usize, usize, usize, usize))> = None;
    for y0 in 0..height {
        let mut all = vec![true; width];
        for y1 in y0..height {
            for x in 0..width {
                all[x] &= mask[y1 * width + x];
            }
            let (mut run, mut start, mut longest, mut longest_start) = (0, 0, 0, 0);
            for (x, &value) in all.iter().enumerate() {
                if value {
                    if run == 0 {
                        start = x;
                    }
                    run += 1;
                    if run > longest {
                        longest = run;
                        longest_start = start;
                    }
                } else {
                    run = 0;
                }
            }
            let area = longest * (y1 - y0 + 1);
            if area > 0 && best.is_none_or(|(best_area, _)| area > best_area) {
                best = Some((area, (longest_start, y0, longest, y1 - y0 + 1)));
            }
        }
    }
    best.map(|(_, rect)| rect)
}
