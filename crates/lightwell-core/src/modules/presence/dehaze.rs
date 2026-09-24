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
    self, GUIDED_PLANES, Geometry, PlaneMut, Rect, Scratch, box_min, guided_filter, reduced_frame,
    reduced_rect, upsample,
};
use crate::{
    Error, ErrorKind,
    modules::{Global, Parallelism, Planes, PlanesMut, Reduction, SpatialUnit, Stage},
};

/// The integer reduction factor per axis the transmission map is computed on.
pub(super) const REDUCTION: i64 = 4;
/// The dark-channel min-filter radius in full-resolution pixels at the reference long side (0.2%).
const R_DARK_6000: f64 = 12.0;
/// The guided-filter refinement radius in full-resolution pixels at the reference long side (0.4%).
const R_GUIDE_6000: f64 = 24.0;
/// The transmission guided filter's regularization, in squared encoded units.
const EPS_DEHAZE: f32 = 1.0e-4;
/// The veil fraction the transmission estimate removes at `|amount| = 100`. `1.0`, not the
/// literature's `0.95`: at `+100` this unit is the exact inverse of the forward model wherever the
/// dark-channel estimate of `t` is exact.
const OMEGA_MAX: f64 = 1.0;
/// The transmission floor, bounding the recovery gain at `1 / T_FLOOR = 10`.
const T_FLOOR: f32 = 0.1;
/// The extra uniform veil a negative amount adds on top of deepening the estimated one. Without it
/// a negative amount would be an exact no-op on a haze-free photograph, whose dark-channel estimate
/// is `t = 1` everywhere.
const VEIL_MAX: f64 = 0.5;
/// The floor on each channel of the atmospheric light, so `I / A` is always finite.
const A_FLOOR: f64 = 1.0e-3;
/// The fraction of the host reduction's brightest dark-channel pixels averaged for `A`.
const ATMOSPHERE_FRACTION: f64 = 0.001;
/// The minimum number of reduction pixels averaged for `A`.
const ATMOSPHERE_MIN_COUNT: usize = 16;

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
        // A missing estimate is never silently treated as neutral: the effect would be omitted from
        // the render without saying so.
        let values = global
            .map(Global::values)
            .filter(|values| values.len() == 3);
        let Some(values) = values else {
            return Err(Error::new(
                ErrorKind::Internal,
                "dehaze was evaluated without its atmospheric light",
            ));
        };
        let atmosphere = [values[0], values[1], values[2]];

        let stage = input.stage();
        let frame = Rect::frame(i64::from(stage.width), i64::from(stage.height));
        let out = Rect::of(output.region());
        if out.is_empty() {
            return Ok(());
        }
        let geometry = Geometry::new(frame.x1, frame.y1, out);
        let (reduced_width, reduced_height) = reduced_frame(frame.x1, frame.y1, REDUCTION);
        let reduced_geometry = Geometry::new(
            reduced_width,
            reduced_height,
            Rect::frame(reduced_width, reduced_height),
        );
        let reduced_frame_rect = Rect::frame(reduced_width, reduced_height);

        let refined_rect = reduced_rect(out, REDUCTION)
            .expand(1)
            .clip(reduced_frame_rect);
        let raw_rect = refined_rect
            .expand(2 * self.r_guide)
            .clip(reduced_frame_rect);
        let dark_source_rect = raw_rect.expand(self.r_dark).clip(reduced_frame_rect);

        let mut scratch = Scratch::new(scratch);
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
        let mut reduced = [
            PlaneMut::over(red_buffer, reduced_geometry, dark_source_rect)?,
            PlaneMut::over(green_buffer, reduced_geometry, dark_source_rect)?,
            PlaneMut::over(blue_buffer, reduced_geometry, dark_source_rect)?,
        ];
        let mut normalized = PlaneMut::over(normalized_buffer, reduced_geometry, dark_source_rect)?;
        let [red, green, blue] = &mut reduced;
        filters::for_rows_of(
            parallelism,
            [red, green, blue, &mut normalized],
            |j, rows| {
                let y0 = j * REDUCTION;
                let y1 = ((j + 1) * REDUCTION).min(frame.y1);
                for i in dark_source_rect.x0..dark_source_rect.x1 {
                    let x0 = i * REDUCTION;
                    let x1 = ((i + 1) * REDUCTION).min(frame.x1);
                    let mut sums = [0.0_f64; 3];
                    let mut count = 0.0_f64;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let pixel = input.sample(x, y);
                            for (channel, sum) in sums.iter_mut().enumerate() {
                                *sum += f64::from(pixel[channel]);
                            }
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
        {
            let mut temp = scratch.branch();
            let temp_buffer = temp.take(
                raw_rect
                    .expand_y(self.r_dark)
                    .clip(reduced_frame_rect)
                    .pixels(),
            )?;
            box_min(
                &normalized.as_plane(),
                self.r_dark,
                &mut dark,
                temp_buffer,
                parallelism,
            )?;
        }

        let mut raw = PlaneMut::over(raw_buffer, reduced_geometry, raw_rect)?;
        let mut guide = PlaneMut::over(guide_buffer, reduced_geometry, raw_rect)?;
        let dark = dark.as_plane();
        let reduced = reduced.each_ref().map(|plane| plane.as_plane());
        filters::for_rows_of(parallelism, [&mut raw, &mut guide], |j, rows| {
            for i in raw_rect.x0..raw_rect.x1 {
                let column = (i - raw_rect.x0) as usize;
                rows[0][column] = 1.0 - self.omega * dark.get(i, j);
                let pixel = [
                    reduced[0].get(i, j),
                    reduced[1].get(i, j),
                    reduced[2].get(i, j),
                ];
                rows[1][column] = filters::encoded_luminance(pixel);
            }
        });

        let mut refined = PlaneMut::over(refined_buffer, reduced_geometry, refined_rect)?;
        guided_filter(
            &guide.as_plane(),
            &raw.as_plane(),
            self.r_guide,
            EPS_DEHAZE,
            &mut refined,
            &mut scratch,
            parallelism,
        )?;
        let mut transmission = PlaneMut::over(transmission_buffer, geometry, out)?;
        upsample(
            &refined.as_plane(),
            REDUCTION,
            &mut transmission,
            parallelism,
        );

        let atmosphere: [f32; 3] = std::array::from_fn(|channel| atmosphere[channel] as f32);
        let positive = self.amount > 0.0;
        let transmission = transmission.as_plane();
        output.for_rows(parallelism, |y, red, green, blue| {
            let y = i64::from(y);
            for x in out.x0..out.x1 {
                let pixel = input.sample(x, y);
                let t = transmission.get(x, y).clamp(T_FLOOR, 1.0);
                let value: [f32; 3] = std::array::from_fn(|channel| {
                    if positive {
                        (pixel[channel] - atmosphere[channel]) / t + atmosphere[channel]
                    } else {
                        let veil = t * self.veil;
                        veil * pixel[channel] + (1.0 - veil) * atmosphere[channel]
                    }
                });
                let column = (x - out.x0) as usize;
                [red[column], green[column], blue[column]] = value;
            }
        });
        Ok(())
    }

    fn is_finite(&self) -> bool {
        self.amount.is_finite() && self.omega.is_finite() && self.veil.is_finite()
    }

    fn describe(&self) -> String {
        format!(
            "presence dehaze(amount={:+.2}, long side {})",
            self.amount, self.long_side
        )
    }
}
