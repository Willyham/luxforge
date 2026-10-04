//! Dehaze: the atmospheric model `I = t*J + (1 - t)*A`, inverted for a positive amount and run
//! forward for a negative one, on linear-light RGB because a veil is chromatic.
//!
//! ```text
//! d(p)      = min over channels of clamp(I_red_c(p) / A_c, 0, 1)
//! dark(p)   = min over the (2*r_dark+1)^2 window of d
//! t_raw     = 1 - omega * dark                       omega = OMEGA_MAX * |amount| / 100
//! G         = encode(luminance(I_red))
//! t_reduced = guided_filter(G, t_raw, r_guide, EPS_DEHAZE)
//! t         = clamp(upsample(t_reduced, 4), T_FLOOR, 1)
//!
//! amount > 0 :  out_c = (I_c - A_c) / t + A_c
//! amount < 0 :  t_veil = t * (1 - VEIL_MAX * |amount| / 100)
//!               out_c  = t_veil * I_c + (1 - t_veil) * A_c
//! ```
//!
//! Frozen in `docs/design/presence-study.md`. The three clamps are part of the estimator, not a
//! gamut clamp on pixel values: `d` in `[0, 1]` keeps a pixel brighter than `A` from reading as
//! opaque haze, `T_FLOOR` bounds the recovery gain at 10, and the ceiling of 1 exists because the
//! guided refinement can overshoot slightly past 1 beside a transition. The inverse branch itself
//! is not clamped, so a recovered value may leave `[0, 1]` and is preserved.

use super::filters::{
    self, GUIDED_PLANES, Geometry, Plane, PlaneMut, Rect, Scratch, box_min, guided_filter,
    reduced_frame, reduced_rect, upsample,
};
use crate::{
    Cancel, Error,
    modules::{
        Global, Parallelism, Planes, PlanesMut, Reduced, ReducedGrid, Reduction, Region,
        SpatialUnit, Stage,
    },
    render::gpu::GpuSpatialUnit,
};
use std::borrow::Cow;

/// The integer reduction factor per axis the transmission map is computed on.
pub(super) const REDUCTION: i64 = 4;
/// The dark-channel min-filter radius in full-resolution pixels at the reference long side (0.2%).
const R_DARK_6000: f64 = 12.0;
/// The guided-filter refinement radius in full-resolution pixels at the reference long side (0.4%).
const R_GUIDE_6000: f64 = 24.0;
/// The transmission guided filter's regularization, in squared encoded units.
pub(super) const EPS_DEHAZE: f32 = 1.0e-4;
/// The veil fraction the transmission estimate removes at `|amount| = 100`. `1.0`, not the
/// literature's `0.95`: at `+100` this unit is the exact inverse of the forward model wherever the
/// dark-channel estimate of `t` is exact.
const OMEGA_MAX: f64 = 1.0;
/// The transmission floor, bounding the recovery gain at `1 / T_FLOOR = 10`.
pub(super) const T_FLOOR: f32 = 0.1;
/// The extra uniform veil a negative amount adds on top of deepening the estimated one. Without it
/// a negative amount would be an exact no-op on a haze-free photograph, whose dark-channel estimate
/// is `t = 1` everywhere.
const VEIL_MAX: f64 = 0.5;
/// The floor on each channel of the atmospheric light, so `I / A` is always finite.
pub(super) const A_FLOOR: f64 = 1.0e-3;
/// The fraction of the host reduction's brightest dark-channel pixels averaged for `A`.
const ATMOSPHERE_FRACTION: f64 = 0.001;
/// The minimum number of reduction pixels averaged for `A`.
pub(super) const ATMOSPHERE_MIN_COUNT: usize = 16;
/// [`ATMOSPHERE_FRACTION`] as the divisor it is: the CPU's `ceil(fraction * n)` is `ceil(n /
/// divisor)` exactly for every reduction the host builds, which the GPU computes in integers.
pub(super) const ATMOSPHERE_DIVISOR: u32 = 1000;

/// Dehaze's dark-channel min-filter radius on its reduced grid.
pub(super) fn dark_radius(long_side: u32) -> i64 {
    filters::reduced_radius(filters::scaled_radius(R_DARK_6000, long_side), REDUCTION)
}

/// Dehaze's transmission guided-filter radius on its reduced grid.
pub(super) fn guide_radius(long_side: u32) -> i64 {
    filters::reduced_radius(filters::scaled_radius(R_GUIDE_6000, long_side), REDUCTION)
}

/// Dehaze's halo in full-resolution pixels: the min filter and the guided filter are sequential on
/// the reduced grid, so their reduced reaches add.
pub(super) fn halo(long_side: u32) -> i64 {
    filters::reduced_halo(
        dark_radius(long_side) + 2 * guide_radius(long_side),
        REDUCTION,
    )
}

/// The Dehaze unit of one compiled Presence operation. `long_side` is the compiled stage's, as in
/// [`super::texture::Texture`].
#[derive(Debug)]
pub(super) struct Dehaze {
    amount: f64,
    long_side: u32,
    omega: f32,
    veil: f32,
    r_dark: i64,
    r_guide: i64,
}

impl Dehaze {
    pub(super) fn new(amount: f64, long_side: u32) -> Self {
        Self {
            amount,
            long_side,
            omega: (OMEGA_MAX * amount.abs() / 100.0) as f32,
            veil: (1.0 - VEIL_MAX * amount.abs() / 100.0) as f32,
            r_dark: dark_radius(long_side),
            r_guide: guide_radius(long_side),
        }
    }

    /// The coefficients the GPU description writes as its words.
    pub(super) fn dark_radius(&self) -> i64 {
        self.r_dark
    }

    pub(super) fn guide_radius(&self) -> i64 {
        self.r_guide
    }

    pub(super) fn omega(&self) -> f32 {
        self.omega
    }

    pub(super) fn veil(&self) -> f32 {
        self.veil
    }

    /// Whether the unit removes the veil, which is the inverse branch, or deepens it.
    pub(super) fn positive(&self) -> bool {
        self.amount > 0.0
    }

    /// Whether the amount is 0: a unit only the GPU shape holds, which changes nothing.
    pub(super) fn neutral(&self) -> bool {
        self.amount == 0.0
    }

    /// Every rectangle filling `output` of `stage` reads, as [`SpatialUnit::apply`] has always
    /// computed them.
    fn rects(&self, stage: Stage, output: Region) -> Rects {
        let frame = Rect::frame(i64::from(stage.width), i64::from(stage.height));
        let out = Rect::of(output);
        let (reduced_width, reduced_height) = reduced_frame(frame.x1, frame.y1, REDUCTION);
        let reduced_frame_rect = Rect::frame(reduced_width, reduced_height);
        let refined = reduced_rect(out, REDUCTION)
            .expand(1)
            .clip(reduced_frame_rect);
        let raw = refined.expand(2 * self.r_guide).clip(reduced_frame_rect);
        let dark_source = raw.expand(self.r_dark).clip(reduced_frame_rect);
        Rects {
            frame,
            out,
            reduced_frame: reduced_frame_rect,
            refined,
            raw,
            dark_source,
        }
    }

    /// [`SpatialUnit::apply`], with the guide and the dark channel's input computed from the
    /// block means and, for [`Reduced::Hand`], the tile's cells of them handed back, or read from
    /// [`Reduced::Held`] planes instead.
    fn run(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        global: Option<&Global>,
        scratch: &mut [f32],
        parallelism: Parallelism,
        reduced: Option<Reduced<'_>>,
    ) -> Result<(), Error> {
        // A missing estimate is never silently treated as neutral: the effect would be omitted from
        // the render without saying so.
        let values = global
            .map(Global::values)
            .filter(|values| values.len() == 3);
        let Some(values) = values else {
            return Err(Error::internal(
                "dehaze was evaluated without its atmospheric light",
            ));
        };
        let atmosphere = [values[0], values[1], values[2]];

        let Rects {
            frame,
            out,
            reduced_frame: reduced_frame_rect,
            refined: refined_rect,
            raw: raw_rect,
            dark_source: dark_source_rect,
        } = self.rects(input.stage(), output.region());
        if out.is_empty() {
            return Ok(());
        }
        let geometry = Geometry::new(frame.x1, frame.y1, out);
        let reduced_geometry = Geometry::new(
            reduced_frame_rect.x1,
            reduced_frame_rect.y1,
            reduced_frame_rect,
        );

        let mut scratch = Scratch::new(scratch);
        if let Some(Reduced::Held(planes)) = reduced {
            // The guide and the dark channel's input are held, so nothing of the input outside
            // the output is read: the block means are never summed.
            let transmission_buffer = scratch.take(out.pixels())?;
            let dark_buffer = scratch.take(raw_rect.pixels())?;
            let raw_buffer = scratch.take(raw_rect.pixels())?;
            let refined_buffer = scratch.take(refined_rect.pixels())?;
            let guide = filters::held_plane(&planes, 0, reduced_geometry, raw_rect)?;
            let normalized = filters::held_plane(&planes, 1, reduced_geometry, dark_source_rect)?;
            let mut dark = PlaneMut::over(dark_buffer, reduced_geometry, raw_rect)?;
            self.dark_channel(
                &normalized,
                &mut dark,
                &mut scratch,
                reduced_frame_rect,
                parallelism,
            )?;
            let dark = dark.as_plane();
            let mut raw = PlaneMut::over(raw_buffer, reduced_geometry, raw_rect)?;
            raw.for_rows(parallelism, |j, row| {
                for i in raw_rect.x0..raw_rect.x1 {
                    row[(i - raw_rect.x0) as usize] = 1.0 - self.omega * dark.get(i, j);
                }
            });
            return self.finish(
                input,
                output,
                atmosphere,
                &guide,
                &raw.as_plane(),
                PlaneMut::over(refined_buffer, reduced_geometry, refined_rect)?,
                PlaneMut::over(transmission_buffer, geometry, out)?,
                &mut scratch,
                parallelism,
            );
        }

        let transmission_buffer = scratch.take(out.pixels())?;
        let red_buffer = scratch.take(dark_source_rect.pixels())?;
        let green_buffer = scratch.take(dark_source_rect.pixels())?;
        let blue_buffer = scratch.take(dark_source_rect.pixels())?;
        let normalized_buffer = scratch.take(dark_source_rect.pixels())?;
        let dark_buffer = scratch.take(raw_rect.pixels())?;
        let raw_buffer = scratch.take(raw_rect.pixels())?;
        let guide_buffer = scratch.take(raw_rect.pixels())?;
        let refined_buffer = scratch.take(refined_rect.pixels())?;

        // The 4x reduction of the operation's own input, block-averaged on the grid anchored at the
        // stage origin exactly as the host's own reduction is.
        let mut reduced_planes = [
            PlaneMut::over(red_buffer, reduced_geometry, dark_source_rect)?,
            PlaneMut::over(green_buffer, reduced_geometry, dark_source_rect)?,
            PlaneMut::over(blue_buffer, reduced_geometry, dark_source_rect)?,
        ];
        let mut normalized = PlaneMut::over(normalized_buffer, reduced_geometry, dark_source_rect)?;
        let [red, green, blue] = &mut reduced_planes;
        // The blocks' columns, all inside the stage (the last block stops at its edge), so the
        // block sums read the input's rows as slices, as do the output's reads below.
        let blocks_x0 = dark_source_rect.x0 * REDUCTION;
        let columns = filters::input_columns(
            input,
            blocks_x0,
            (dark_source_rect.x1 * REDUCTION).min(frame.x1),
        );
        filters::for_rows_of(
            parallelism,
            [red, green, blue, &mut normalized],
            |j, rows| {
                let y0 = j * REDUCTION;
                let y1 = ((j + 1) * REDUCTION).min(frame.y1);
                let lines: [[&[f32]; 3]; REDUCTION as usize] = std::array::from_fn(|line| {
                    let y = (y0 + line as i64).min(y1 - 1);
                    input.row(y).map(|plane| &plane[columns.clone()])
                });
                let lines = &lines[..(y1 - y0) as usize];
                for i in dark_source_rect.x0..dark_source_rect.x1 {
                    let x0 = (i * REDUCTION - blocks_x0) as usize;
                    let x1 = (((i + 1) * REDUCTION).min(frame.x1) - blocks_x0) as usize;
                    let mut sums = [0.0_f64; 3];
                    let mut count = 0.0_f64;
                    for line in lines {
                        let [r, g, b] = line.map(|plane| &plane[x0..x1]);
                        for ((r, g), b) in r.iter().zip(g).zip(b) {
                            sums[0] += f64::from(*r);
                            sums[1] += f64::from(*g);
                            sums[2] += f64::from(*b);
                            count += 1.0;
                        }
                    }
                    let column = (i - dark_source_rect.x0) as usize;
                    let mut smallest = f32::INFINITY;
                    for (channel, sum) in sums.iter().enumerate() {
                        let mean = sum / count;
                        rows[channel][column] = mean as f32;
                        smallest =
                            smallest.min((mean / atmosphere[channel]).clamp(0.0, 1.0) as f32);
                    }
                    rows[3][column] = smallest;
                }
            },
        );

        let mut dark = PlaneMut::over(dark_buffer, reduced_geometry, raw_rect)?;
        self.dark_channel(
            &normalized.as_plane(),
            &mut dark,
            &mut scratch,
            reduced_frame_rect,
            parallelism,
        )?;

        let mut raw = PlaneMut::over(raw_buffer, reduced_geometry, raw_rect)?;
        let mut guide = PlaneMut::over(guide_buffer, reduced_geometry, raw_rect)?;
        let dark = dark.as_plane();
        let means = reduced_planes.each_ref().map(|plane| plane.as_plane());
        filters::for_rows_of(parallelism, [&mut raw, &mut guide], |j, rows| {
            for i in raw_rect.x0..raw_rect.x1 {
                let column = (i - raw_rect.x0) as usize;
                rows[0][column] = 1.0 - self.omega * dark.get(i, j);
                let pixel = [means[0].get(i, j), means[1].get(i, j), means[2].get(i, j)];
                rows[1][column] = filters::encoded_luminance(pixel);
            }
        });
        if let Some(Reduced::Hand(cells)) = reduced {
            filters::hand_back(cells, 0, &guide.as_plane());
            filters::hand_back(cells, 1, &normalized.as_plane());
        }
        self.finish(
            input,
            output,
            atmosphere,
            &guide.as_plane(),
            &raw.as_plane(),
            PlaneMut::over(refined_buffer, reduced_geometry, refined_rect)?,
            PlaneMut::over(transmission_buffer, geometry, out)?,
            &mut scratch,
            parallelism,
        )
    }

    /// The dark channel over `dark`'s rectangle: the minimum of the normalized means over the
    /// dark-channel window.
    fn dark_channel(
        &self,
        normalized: &Plane<'_>,
        dark: &mut PlaneMut<'_>,
        scratch: &mut Scratch<'_>,
        reduced_frame: Rect,
        parallelism: Parallelism,
    ) -> Result<(), Error> {
        let mut temp = scratch.branch();
        let temp_buffer = temp.take(
            dark.rect()
                .expand_y(self.r_dark)
                .clip(reduced_frame)
                .pixels(),
        )?;
        box_min(normalized, self.r_dark, dark, temp_buffer, parallelism)
    }

    /// The transmission refined under the guide and upsampled, then the model inverted or run
    /// forward over the output.
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        atmosphere: [f64; 3],
        guide: &Plane<'_>,
        raw: &Plane<'_>,
        mut refined: PlaneMut<'_>,
        mut transmission: PlaneMut<'_>,
        scratch: &mut Scratch<'_>,
        parallelism: Parallelism,
    ) -> Result<(), Error> {
        let out = Rect::of(output.region());
        guided_filter(
            guide,
            raw,
            self.r_guide,
            EPS_DEHAZE,
            &mut refined,
            scratch,
            parallelism,
        )?;
        upsample(
            &refined.as_plane(),
            REDUCTION,
            &mut transmission,
            parallelism,
        );
        let atmosphere: [f32; 3] = std::array::from_fn(|channel| atmosphere[channel] as f32);
        let positive = self.amount > 0.0;
        let transmission = transmission.as_plane();
        let columns = filters::input_columns(input, out.x0, out.x1);
        output.for_rows(parallelism, |y, red, green, blue| {
            let y = i64::from(y);
            // Every row cut to the output's width, so the loop indexes them with no bounds check.
            let width = red.len();
            let (green, blue) = (&mut green[..width], &mut blue[..width]);
            let [r, g, b] = input.row(y).map(|plane| &plane[columns.clone()][..width]);
            let transmission = &transmission.span(y, out.x0, out.x1)[..width];
            for column in 0..width {
                let pixel = [r[column], g[column], b[column]];
                let t = transmission[column].clamp(T_FLOOR, 1.0);
                let value: [f32; 3] = std::array::from_fn(|channel| {
                    if positive {
                        (pixel[channel] - atmosphere[channel]) / t + atmosphere[channel]
                    } else {
                        let veil = t * self.veil;
                        veil * pixel[channel] + (1.0 - veil) * atmosphere[channel]
                    }
                });
                [red[column], green[column], blue[column]] = value;
            }
        });
        Ok(())
    }
}

/// The rectangles one output rectangle of Dehaze reads ([`Dehaze::rects`]), on the reduced grid
/// but for the frame and the output.
#[derive(Clone, Copy)]
struct Rects {
    frame: Rect,
    out: Rect,
    reduced_frame: Rect,
    /// The refined transmission: the output's blocks and one more for the upsample.
    refined: Rect,
    /// The raw transmission and the guide the guided filter reads around it.
    raw: Rect,
    /// The block means the dark channel's window reads around the raw transmission: the unit's
    /// reach in the grid.
    dark_source: Rect,
}

impl SpatialUnit for Dehaze {
    fn halo(&self, _: Stage) -> u32 {
        halo(self.long_side) as u32
    }

    /// The full-resolution transmission over the input region, the reduced grid's three colour
    /// planes, its five estimator planes and the guided filter's own.
    fn scratch_bytes(&self, region: Stage) -> u64 {
        let full = filters::region_values(region);
        let reduced = filters::reduced_values(region, REDUCTION as u32)
            .saturating_mul(8 + GUIDED_PLANES as u64);
        filters::scratch_bytes(full.saturating_add(reduced))
    }

    /// The atmospheric light reads the reduction and nothing of this unit, so its identity is the
    /// estimator alone: every Dehaze amount over the same stage prepares from one stored estimate,
    /// and a new amount reduces nothing.
    fn estimate_key(&self) -> Option<Cow<'static, str>> {
        Some(Cow::Borrowed("presence dehaze atmospheric light"))
    }

    /// Detail's filters barely move the block means the light is chosen from, so removing a veil
    /// with the light of Detail's input stays within the spatial limits over the corpus at 100%.
    /// Adding one lays the light itself over the picture, which follows its error: sharpen stress
    /// on the zone plate under −100 moves the picture's lightness by 1.15 (`docs/specs/
    /// performance.md`, "Dehaze behind Detail at 100%"). So an amount below zero holds none.
    fn holds_restored_estimate(&self) -> bool {
        self.amount >= 0.0
    }

    /// The atmospheric light: the pointwise channel minimum of the host's 1/16-per-side reduction,
    /// the brightest [`ATMOSPHERE_FRACTION`] of those pixels (at least [`ATMOSPHERE_MIN_COUNT`]),
    /// their per-channel mean, floored at [`A_FLOOR`]. Three `f64`, 24 bytes.
    ///
    /// The ordering is total — value first, then the smaller row-major index — so the selection is
    /// deterministic for any input, including a constant frame.
    fn prepare(&self, reduction: &Reduction) -> Option<Global> {
        let width = reduction.width();
        let height = reduction.height();
        let pixels = (u64::from(width) * u64::from(height)) as usize;
        if pixels == 0 {
            return None;
        }
        let mut dark: Vec<(f64, u32)> = Vec::with_capacity(pixels);
        for y in 0..height {
            for x in 0..width {
                let pixel = reduction.pixel(x, y)?;
                let value = f64::from(pixel[0])
                    .min(f64::from(pixel[1]))
                    .min(f64::from(pixel[2]));
                dark.push((value, y * width + x));
            }
        }
        let count = ATMOSPHERE_MIN_COUNT
            .max((ATMOSPHERE_FRACTION * pixels as f64).ceil() as usize)
            .clamp(1, pixels);
        // Descending by dark value, ties broken by the smaller index: the same total order the
        // reference sorts by, so the selected set is the same. Selecting rather than sorting is
        // linear and the average below does not depend on the order within the set.
        let order = |left: &(f64, u32), right: &(f64, u32)| {
            right.0.total_cmp(&left.0).then(left.1.cmp(&right.1))
        };
        if count < pixels {
            dark.select_nth_unstable_by(count - 1, order);
        }
        let mut sums = [0.0_f64; 3];
        for (_, index) in dark.iter().take(count) {
            let x = index % width;
            let y = index / width;
            let pixel = reduction.pixel(x, y)?;
            for (channel, sum) in sums.iter_mut().enumerate() {
                *sum += f64::from(pixel[channel]);
            }
        }
        let atmosphere: Vec<f64> = sums
            .iter()
            .map(|sum| (sum / count as f64).max(A_FLOOR))
            .collect();
        Global::new(atmosphere).ok()
    }

    fn apply(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        global: Option<&Global>,
        scratch: &mut [f32],
        parallelism: Parallelism,
    ) -> Result<(), Error> {
        self.run(input, output, global, scratch, parallelism, None)
    }

    /// The two planes the transmission estimate reads its reduced grid through: the guided
    /// filter's guide, the encoded luminance of the `f32` block means, and the dark channel's
    /// input, the smallest over the channels of each `f64` block mean divided by the atmospheric
    /// light, clamped and narrowed. The three means themselves would not do, because the dark
    /// channel's input is computed from the `f64` mean. Both are read before any coefficient; the
    /// second depends on the light, which the host checks beside the key.
    fn reduced_grid(&self) -> Option<ReducedGrid> {
        Some(ReducedGrid {
            key: Cow::Borrowed("presence dehaze guide and dark-channel input 4x block means"),
            factor: REDUCTION as u32,
            planes: 2,
        })
    }

    fn reduced_reach(&self, output: Region, stage: Stage) -> Region {
        let rects = self.rects(stage, output);
        if rects.out.is_empty() {
            return Region::EMPTY;
        }
        filters::grid_region(rects.dark_source)
    }

    fn apply_reduced(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        global: Option<&Global>,
        scratch: &mut [f32],
        parallelism: Parallelism,
        cancel: &Cancel,
        reduced: Reduced<'_>,
    ) -> Result<(), Error> {
        cancel.check()?;
        self.run(input, output, global, scratch, parallelism, Some(reduced))?;
        cancel.check()
    }

    /// The stored atmospheric light when the plan found one for this stage's content, else the
    /// GPU takes it from the stage it holds.
    fn gpu(&self, global: Option<&Global>) -> Option<GpuSpatialUnit> {
        Some(super::gpu::dehaze(self, global))
    }

    fn is_finite(&self) -> bool {
        self.amount.is_finite() && self.omega.is_finite() && self.veil.is_finite()
    }

    fn describe(&self) -> String {
        format!(
            "presence dehaze(amount={:+}, long side {})",
            self.amount, self.long_side
        )
    }
}
