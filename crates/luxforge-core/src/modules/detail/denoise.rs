use super::filters::{self, Geometry, Kernel};
use crate::{
    Cancel, Error,
    modules::{Global, Parallelism, Planes, PlanesMut, Region, SamplingScale, SpatialUnit, Stage},
    render::gpu::GpuSpatialUnit,
};

const BAND_NOISE: [f64; 4] = [0.8914, 0.1992, 0.0860, 0.0417];
const LUMINANCE_THRESHOLD: f64 = 0.10;
const CHROMA_THRESHOLD: f64 = 0.10;

#[derive(Debug)]
pub(super) struct Denoise {
    scale: SamplingScale,
    luminance: f64,
    colour: f64,
    luminance_detail: f32,
    colour_detail: f32,
    thresholds: Vec<[f32; 2]>,
    kernels: Vec<[Kernel; 2]>,
    halo: u32,
}
impl Denoise {
    pub fn new(
        luminance: f64,
        luminance_detail: f64,
        colour: f64,
        colour_detail: f64,
        scale: SamplingScale,
    ) -> Self {
        let levels = if colour == 0.0 { 3 } else { 4 };
        Self::with_levels(
            luminance,
            luminance_detail,
            colour,
            colour_detail,
            scale,
            levels,
        )
    }

    /// The unit with every level it can hold, a level whose thresholds are zero changing nothing:
    /// its GPU shape (`CompileStage::gpu_shape`), whose passes do not change as Colour leaves or
    /// returns to zero. No CPU frame runs it; the CPU's own shape is [`Self::new`]'s.
    pub fn every_level(
        luminance: f64,
        luminance_detail: f64,
        colour: f64,
        colour_detail: f64,
        scale: SamplingScale,
    ) -> Self {
        Self::with_levels(
            luminance,
            luminance_detail,
            colour,
            colour_detail,
            scale,
            BAND_NOISE.len(),
        )
    }

    fn with_levels(
        luminance: f64,
        luminance_detail: f64,
        colour: f64,
        colour_detail: f64,
        scale: SamplingScale,
        levels: usize,
    ) -> Self {
        let mut kernels = filters::denoise_kernels(scale);
        kernels.truncate(levels);
        let halo = (0..2)
            .map(|axis| kernels.iter().map(|k| k[axis].radius).sum::<u32>() + 1)
            .max()
            .unwrap();
        debug_assert!(
            halo <= if scale.x == 1.0 && scale.y == 1.0 {
                filters::DENOISE_EXACT_HALO_MAX
            } else {
                filters::DENOISE_SAMPLED_HALO_MAX
            }
        );
        let thresholds = BAND_NOISE
            .iter()
            .enumerate()
            .take(levels)
            .map(|(j, k)| {
                [
                    if j < 3 {
                        (LUMINANCE_THRESHOLD * (luminance / 100.0).powf(1.5) * k) as f32
                    } else {
                        0.0
                    },
                    (CHROMA_THRESHOLD * (colour / 100.0).powf(1.5) * k) as f32,
                ]
            })
            .collect();
        Self {
            scale,
            luminance,
            colour,
            luminance_detail: (luminance_detail / 100.0) as f32,
            colour_detail: (colour_detail / 100.0) as f32,
            thresholds,
            kernels,
            halo,
        }
    }

    /// The planes level `level` smooths: lightness (0) while this or a later level has a lightness
    /// threshold, and a and b (1 and 2) while one has a chroma threshold. A kind whose thresholds
    /// from here on are all zero changes nothing more: lightness at the chroma-only fourth level,
    /// and a kind whose strength is zero or so small that its thresholds round to zero.
    fn channels(&self, level: usize) -> std::ops::Range<usize> {
        let [luminance, chroma] =
            [0, 1].map(|kind| self.thresholds[level..].iter().any(|t| t[kind] != 0.0));
        let start = if luminance { 0 } else { 1 };
        let end = if chroma { 3 } else { 1 };
        start..end
    }
}
#[cfg(feature = "qualification")]
impl Denoise {
    /// The per-level thresholds `[luminance, chroma]` the unit holds.
    pub(super) fn thresholds(&self) -> &[[f32; 2]] {
        &self.thresholds
    }
}
#[cfg(test)]
impl Denoise {
    /// The kernels and thresholds of each level the unit holds: with [`Self::details`], what the
    /// frozen reference in `exactness.rs` runs.
    pub(super) fn levels(&self) -> (&[[Kernel; 2]], &[[f32; 2]]) {
        (&self.kernels, &self.thresholds)
    }

    /// The luminance and colour details the unit holds.
    pub(super) fn details(&self) -> [f32; 2] {
        [self.luminance_detail, self.colour_detail]
    }
}
impl SpatialUnit for Denoise {
    fn halo(&self, _: Stage) -> u32 {
        self.halo
    }
    fn scratch_bytes(&self, region: Stage) -> u64 {
        filters::scratch_bytes(region, 13)
    }
    fn apply(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        _: Option<&Global>,
        scratch: &mut [f32],
        parallelism: Parallelism,
    ) -> Result<(), Error> {
        self.apply_cancellable(input, output, None, scratch, parallelism, &Cancel::never())
    }
    fn apply_cancellable(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        _: Option<&Global>,
        scratch: &mut [f32],
        parallelism: Parallelism,
        cancel: &Cancel,
    ) -> Result<(), Error> {
        cancel.check()?;
        let stage = input.stage();
        let out = output.region();
        if out.is_empty() {
            return Ok(());
        }
        let held = out.grown(self.halo, stage);
        let geometry = Geometry { stage, held };
        let len = held.pixels() as usize;
        let mut scratch = scratch;
        let lab = filters::take(&mut scratch, 3 * len)?;
        let mut coarse = filters::take(&mut scratch, 3 * len)?;
        let mut next = filters::take(&mut scratch, 3 * len)?;
        let temporary = filters::take(&mut scratch, len)?;
        let delta = filters::take(&mut scratch, 3 * len)?;
        filters::lab(input, lab, geometry, parallelism, cancel)?;
        let first = self.channels(0);
        coarse[first.start * len..first.end * len]
            .copy_from_slice(&lab[first.start * len..first.end * len]);
        delta.fill(0.0);
        let mut valid = held;
        for (level, (kernels, thresholds)) in self.kernels.iter().zip(&self.thresholds).enumerate()
        {
            cancel.check()?;
            let next_valid = valid.shrunk(kernels[0].radius.max(kernels[1].radius), stage);
            for c in self.channels(level) {
                filters::smooth_band(
                    &mut coarse[c * len..(c + 1) * len],
                    valid,
                    &mut next[c * len..(c + 1) * len],
                    temporary,
                    geometry,
                    next_valid,
                    kernels,
                    parallelism,
                    cancel,
                )?;
            }
            // The shrinkage reads the band only where this level wrote it.
            filters::guard(next_valid, out.grown(1, stage));
            shrink(
                coarse,
                delta,
                geometry,
                out,
                *thresholds,
                [self.luminance_detail, self.colour_detail],
                parallelism,
                cancel,
            )?;
            // The next level smooths this level's smoothing, which `next` holds over `next_valid`:
            // the planes trade places rather than copy, because every later read of the level's
            // input lies inside `next_valid` (`smooth_band`'s guard), the band is not read again
            // and a plane this level did not smooth is not smoothed again.
            std::mem::swap(&mut coarse, &mut next);
            valid = next_valid;
            #[cfg(test)]
            filters::finished_level(cancel);
        }
        finish(input, output, lab, delta, geometry, parallelism, cancel)
    }
    fn gpu(&self) -> Option<GpuSpatialUnit> {
        super::gpu::denoise(
            &self.kernels,
            &self.thresholds,
            [self.luminance_detail, self.colour_detail],
        )
    }
    fn is_finite(&self) -> bool {
        self.thresholds.iter().flatten().all(|v| v.is_finite())
            && self.luminance_detail.is_finite()
            && self.colour_detail.is_finite()
    }
    fn describe(&self) -> String {
        format!(
            "detail denoise(luminance={}, detail={}, colour={}, detail={}, scale={:?}, halo={})",
            self.luminance,
            self.luminance_detail,
            self.colour,
            self.colour_detail,
            self.scale,
            self.halo
        )
    }
}

/// The unit's last pass: each pixel of `output` reconstructed from its input, its Oklab `lab` and
/// the levels' change `delta`, both planar L, a and b over `geometry.held`. A pixel's
/// reconstruction reads its own place alone, which lies inside the stage, so it reads row slices
/// of the planes and the input with no clamp, the columns checked once.
pub(super) fn finish(
    input: &Planes<'_>,
    output: &mut PlanesMut<'_>,
    lab: &[f32],
    delta: &[f32],
    geometry: Geometry,
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    let out = output.region();
    if out.is_empty() {
        return cancel.check();
    }
    filters::guard(geometry.held, out);
    let first = (out.x0 - geometry.held.x0) as usize;
    let columns = filters::input_columns(input, out.x0, out.x1());
    let len = geometry.held.pixels() as usize;
    let lab: [&[f32]; 3] = std::array::from_fn(|c| &lab[c * len..(c + 1) * len]);
    let delta: [&[f32]; 3] = std::array::from_fn(|c| &delta[c * len..(c + 1) * len]);
    output.for_rows(parallelism, |y, red, green, blue| {
        if cancel.is_cancelled() {
            return;
        }
        let (n, y) = (red.len(), i64::from(y));
        let l = geometry.span(lab[0], y, first, n);
        let a = geometry.span(lab[1], y, first, n);
        let b = geometry.span(lab[2], y, first, n);
        let dl = geometry.span(delta[0], y, first, n);
        let da = geometry.span(delta[1], y, first, n);
        let db = geometry.span(delta[2], y, first, n);
        let rgb = filters::input_row(input, y, &columns, n);
        let (green, blue) = (&mut green[..n], &mut blue[..n]);
        for column in 0..n {
            let value = filters::reconstruct(
                [rgb[0][column], rgb[1][column], rgb[2][column]],
                [l[column], a[column], b[column]],
                [dl[column], da[column], db[column]],
            );
            [red[column], green[column], blue[column]] = value;
        }
    });
    cancel.check()
}

/// One level's soft shrinkage over `out`, added to `delta`: for each channel kind with a threshold,
/// the non-negative garrote of the level's detail `band`, its threshold lowered where the band's
/// mean energy over the 3 x 3 neighbourhood is high. `band` and `delta` are planar L, a and b over
/// `geometry.held`. One pass over the rows computes each kind's energy and factor once per pixel,
/// and chroma's factor scales a and b alike.
#[allow(clippy::too_many_arguments)]
pub(super) fn shrink(
    band: &[f32],
    delta: &mut [f32],
    geometry: Geometry,
    out: Region,
    thresholds: [f32; 2],
    details: [f32; 2],
    parallelism: Parallelism,
    cancel: &Cancel,
) -> Result<(), Error> {
    let [luminance, chroma] = thresholds;
    if luminance == 0.0 && chroma == 0.0 {
        return Ok(());
    }
    if !out.is_empty() {
        filters::guard(geometry.held, out.grown(1, geometry.stage));
    }
    let len = geometry.held.pixels() as usize;
    let (l, ab) = band.split_at(len);
    let (a, b) = ab.split_at(len);
    let (delta_l, delta_ab) = delta.split_at_mut(len);
    let (delta_a, delta_b) = delta_ab.split_at_mut(len);
    // Only the first and last stage columns clamp a neighbour. Each is a run of one over its three
    // taps gathered; the columns between are one run over the held rows themselves.
    let width = i64::from(geometry.stage.width);
    let held = i64::from(geometry.held.x0);
    let (x0, x1) = (i64::from(out.x0), i64::from(out.x1()));
    let start = 1.clamp(x0, x1);
    let end = (width - 1).clamp(start, x1);
    let clamped = |x: i64| (x.clamp(0, width - 1) - held) as usize;
    let edges: Vec<(usize, [usize; 3])> = (x0..start)
        .chain(end..x1)
        .map(|x| ((x - x0) as usize, [x - 1, x, x + 1].map(clamped)))
        .collect();
    let interior = (start < end).then(|| {
        let taps = (start - 1 - held) as usize..(end + 1 - held) as usize;
        (taps, (start - x0) as usize..(end - x0) as usize)
    });
    let gather = |rows: [&[f32]; 3], taps: [usize; 3]| rows.map(|row| taps.map(|i| row[i]));
    filters::rows_of(
        [delta_l, delta_a, delta_b],
        geometry,
        out,
        parallelism,
        cancel,
        |y, [delta_l, delta_a, delta_b]| {
            if luminance != 0.0 {
                let l = around(l, geometry, y);
                for &(i, taps) in &edges {
                    let l = gather(l, taps);
                    let l = l.each_ref().map(|taps| &taps[..]);
                    lightness(l, &mut delta_l[i..=i], luminance, details[0]);
                }
                if let Some((taps, run)) = interior.clone() {
                    let l = l.map(|row| &row[taps.clone()]);
                    lightness(l, &mut delta_l[run], luminance, details[0]);
                }
            }
            if chroma != 0.0 {
                let (a, b) = (around(a, geometry, y), around(b, geometry, y));
                for &(i, taps) in &edges {
                    let (a, b) = (gather(a, taps), gather(b, taps));
                    let (a, b) = (a.each_ref().map(|t| &t[..]), b.each_ref().map(|t| &t[..]));
                    let (delta_a, delta_b) = (&mut delta_a[i..=i], &mut delta_b[i..=i]);
                    colour(a, b, delta_a, delta_b, chroma, details[1]);
                }
                if let Some((taps, run)) = interior.clone() {
                    let (a, b) = (
                        a.map(|row| &row[taps.clone()]),
                        b.map(|row| &row[taps.clone()]),
                    );
                    let (delta_a, delta_b) = (&mut delta_a[run.clone()], &mut delta_b[run]);
                    colour(a, b, delta_a, delta_b, chroma, details[1]);
                }
            }
        },
    )
}

/// The held rows `y - 1`, `y` and `y + 1` of `plane`, clamped to the stage.
fn around(plane: &[f32], geometry: Geometry, y: u32) -> [&[f32]; 3] {
    [-1, 0, 1].map(|dy| geometry.row(plane, i64::from(y) + dy))
}

/// The shrinkage of lightness over a run of pixels: `rows` holds the band's rows above, at and
/// below the run from the column left of its first pixel, so pixel `j` reads `j..j + 3` of each,
/// and `delta` the run's change. The 3 x 3 energy adds the taps' squares from zero, row by row and
/// left to right.
fn lightness(rows: [&[f32]; 3], delta: &mut [f32], threshold: f32, detail: f32) {
    let n = delta.len();
    let rows = rows.map(|row| &row[..n + 2]);
    let bound = (3.0 * threshold) * (3.0 * threshold);
    for j in 0..n {
        let mut energy = 0.0;
        for row in rows {
            for k in 0..3 {
                energy += row[j + k] * row[j + k];
            }
        }
        let d = rows[1][j + 1];
        let squared = d * d;
        let factor = factor(energy, squared, threshold, detail, bound);
        delta[j] = shrunk(delta[j], d, factor, squared);
    }
}

/// How many pixels [`colour`] takes the factors of at a time, held on the stack.
const COLOUR_RUN: usize = 128;

/// The shrinkage of chroma over a run of pixels, as [`lightness`] lays it out: a tap's energy is
/// its a squared plus its b squared, and one factor scales the pixel's a and b alike. The factors
/// of up to [`COLOUR_RUN`] pixels are taken first and then applied: the compiler vectorizes these
/// two loops and leaves one loop doing both scalar, which took 2.2 to 2.4 times as long on the M4.
fn colour(
    a: [&[f32]; 3],
    b: [&[f32]; 3],
    delta_a: &mut [f32],
    delta_b: &mut [f32],
    threshold: f32,
    detail: f32,
) {
    let n = delta_a.len();
    let bound = (3.0 * threshold) * (3.0 * threshold);
    let (mut factors, mut squares) = ([0.0; COLOUR_RUN], [0.0; COLOUR_RUN]);
    for first in (0..n).step_by(COLOUR_RUN) {
        let m = COLOUR_RUN.min(n - first);
        let [a0, a1, a2] = a.map(|row| &row[first..first + m + 2]);
        let [b0, b1, b2] = b.map(|row| &row[first..first + m + 2]);
        let (factors, squares) = (&mut factors[..m], &mut squares[..m]);
        for j in 0..m {
            let mut energy = 0.0;
            for (a, b) in [(a0, b0), (a1, b1), (a2, b2)] {
                for k in 0..3 {
                    energy += a[j + k] * a[j + k] + b[j + k] * b[j + k];
                }
            }
            let (a, b) = (a1[j + 1], b1[j + 1]);
            squares[j] = a * a + b * b;
            factors[j] = factor(energy, squares[j], threshold, detail, bound);
        }
        let (a, b) = (&a1[1..m + 1], &b1[1..m + 1]);
        let delta_a = &mut delta_a[first..first + m];
        let delta_b = &mut delta_b[first..first + m];
        for j in 0..m {
            delta_a[j] = shrunk(delta_a[j], a[j], factors[j], squares[j]);
            delta_b[j] = shrunk(delta_b[j], b[j], factors[j], squares[j]);
        }
    }
}

/// The garrote's factor at a pixel whose 3 x 3 energy sums to `energy` and whose own is `squared`:
/// the threshold lowered where the mean energy is high against `bound`, three thresholds squared.
#[inline(always)]
fn factor(energy: f32, squared: f32, threshold: f32, detail: f32, bound: f32) -> f32 {
    let energy = energy / 9.0;
    let protection = energy / (energy + bound);
    let effective = threshold * (1.0 - detail * protection);
    (1.0 - effective * effective / squared).max(0.0)
}

/// `change` plus a band value `d` shrunk by `factor`, less `d`, unless the pixel's own energy is
/// zero, which keeps its change.
#[inline(always)]
fn shrunk(change: f32, d: f32, factor: f32, squared: f32) -> f32 {
    if squared != 0.0 {
        change + (d * factor - d)
    } else {
        change
    }
}
