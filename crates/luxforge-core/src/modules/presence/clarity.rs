//! Clarity: broad local contrast, the residual of encoded luminance against an edge-preserving base
//! computed on a 4x reduced grid, scaled by the compressive gain and reapplied.
//!
//! ```text
//! E      = encode(luminance(rgb))
//! E_red  = downsample(E, 4)
//! B_red  = guided_self(E_red, r_red, EPS_CLARITY)
//! B      = upsample(B_red, 4)
//! delta  = soft_clip(gain_clarity(amount) * (E - B), E, LIMIT_CLARITY)
//! out    = reconstruct(rgb, L, decode(E + delta))
//! ```
//!
//! Frozen in `docs/design/presence-study.md`. The base is computed on the reduced grid because a
//! full-resolution guided filter at 1.6% of the long side would need 321 x 321 window sums per
//! pixel at 60 MP; the reduction cuts the work by sixteen and not the neighbourhood, so the halo is
//! still roughly twice the full-resolution radius.

use super::filters::{
    self, GUIDED_SELF_PLANES, Geometry, Plane, PlaneMut, Rect, Scratch, downsample, full_rect,
    guided_self, reduced_frame, reduced_rect, upsample,
};
use crate::{
    Cancel, Error,
    colour::{luma, srgb},
    modules::{
        Global, Parallelism, Planes, PlanesMut, Reduced, ReducedGrid, Region, SpatialUnit, Stage,
    },
    render::gpu::GpuSpatialUnit,
};
use std::borrow::Cow;

/// The integer reduction factor per axis the base is computed on.
pub(super) const REDUCTION: i64 = 4;
/// The base radius in full-resolution pixels at the reference long side: 1.6% of the long side, the
/// largest round value whose halo leaves margin inside the host's 512 px bound at 60 MP.
const R_CLARITY_6000: f64 = 96.0;
/// The guided filter's regularization in squared encoded units: `a = 0.5` at a window standard
/// deviation of `0.1` encoded, about 26 of 255 codes.
pub(super) const EPS_CLARITY: f32 = 1.0e-2;
/// The soft-clip limit on the encoded excursion, in encoded units.
pub(super) const LIMIT_CLARITY: f32 = 0.15;
/// `+100` doubles the broad residual; `-100` removes three quarters of it. The negative gain is
/// deliberately not 1: removing the residual entirely replaces the image with its own base, which
/// is a blur rather than the soft rendering a negative Clarity is reached for.
const GAIN_POS: f64 = 1.0;
const GAIN_NEG: f64 = 0.75;

/// Clarity's guided-filter radius on its reduced grid.
pub(super) fn reduced_radius(long_side: u32) -> i64 {
    filters::reduced_radius(filters::scaled_radius(R_CLARITY_6000, long_side), REDUCTION)
}

/// Clarity's halo in full-resolution pixels: one guided filter on the reduced grid, so the reduced
/// reach is twice its reduced radius.
pub(super) fn halo(long_side: u32) -> i64 {
    filters::reduced_halo(2 * reduced_radius(long_side), REDUCTION)
}

fn gain(amount: f64) -> f32 {
    (if amount >= 0.0 {
        amount / 100.0 * GAIN_POS
    } else {
        amount / 100.0 * GAIN_NEG
    }) as f32
}

/// The Clarity unit of one compiled Presence operation. `long_side` is the compiled stage's, as in
/// [`super::texture::Texture`].
#[derive(Debug)]
pub(super) struct Clarity {
    amount: f64,
    long_side: u32,
    gain: f32,
    r_red: i64,
}

impl Clarity {
    pub(super) fn new(amount: f64, long_side: u32) -> Self {
        Self {
            amount,
            long_side,
            gain: gain(amount),
            r_red: reduced_radius(long_side),
        }
    }

    /// The base smoother's radius on the reduced grid and the residual's gain, which the GPU
    /// description writes as its words.
    pub(super) fn reduced_radius(&self) -> i64 {
        self.r_red
    }

    pub(super) fn gain(&self) -> f32 {
        self.gain
    }

    /// Every rectangle filling `output` of `stage` reads, as [`SpatialUnit::apply`] has always
    /// computed them.
    fn rects(&self, stage: Stage, output: crate::modules::Region) -> Rects {
        let frame = Rect::frame(i64::from(stage.width), i64::from(stage.height));
        let out = Rect::of(output);
        let (reduced_width, reduced_height) = reduced_frame(frame.x1, frame.y1, REDUCTION);
        let reduced_frame_rect = Rect::frame(reduced_width, reduced_height);
        // The bilinear upsample reaches one reduced index beyond the output's own blocks, and the
        // guided filter reaches 2*r_red beyond that. The declared halo is exactly this reach.
        let base_rect = reduced_rect(out, REDUCTION)
            .expand(1)
            .clip(reduced_frame_rect);
        let reduced_source = base_rect.expand(2 * self.r_red).clip(reduced_frame_rect);
        let encoded_rect = full_rect(reduced_source, REDUCTION).clip(frame);
        Rects {
            frame,
            out,
            reduced_frame: reduced_frame_rect,
            base_rect,
            reduced_source,
            encoded_rect,
        }
    }

    /// [`SpatialUnit::apply`], with the reduced source computed and, for [`Reduced::Hand`], the
    /// tile's cells of it handed back, or read from [`Reduced::Held`] planes instead.
    fn run(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        scratch: &mut [f32],
        parallelism: Parallelism,
        reduced: Option<Reduced<'_>>,
    ) -> Result<(), Error> {
        let rects = self.rects(input.stage(), output.region());
        let Rects {
            frame,
            out,
            reduced_frame,
            base_rect,
            reduced_source,
            encoded_rect,
        } = rects;
        if out.is_empty() {
            return Ok(());
        }
        let geometry = Geometry::new(frame.x1, frame.y1, out);
        let reduced_geometry = Geometry::new(reduced_frame.x1, reduced_frame.y1, reduced_frame);

        let mut scratch = Scratch::new(scratch);
        if let Some(Reduced::Held(planes)) = reduced {
            // The reduced source is held, so only the output's own encoded values are computed:
            // the residual reads them at the output's pixels and nowhere else.
            let encoded_buffer = scratch.take(out.pixels())?;
            let base_buffer = scratch.take(out.pixels())?;
            let base_reduced_buffer = scratch.take(base_rect.pixels())?;
            let mut encoded = PlaneMut::over(encoded_buffer, geometry, out)?;
            encode(input, &mut encoded, parallelism);
            let encoded_reduced =
                filters::held_plane(&planes, 0, reduced_geometry, reduced_source)?;
            let mut base_reduced =
                PlaneMut::over(base_reduced_buffer, reduced_geometry, base_rect)?;
            guided_self(
                &encoded_reduced,
                self.r_red,
                EPS_CLARITY,
                &mut base_reduced,
                &mut scratch,
                parallelism,
            )?;
            return self.finish(
                input,
                output,
                &encoded.as_plane(),
                &base_reduced.as_plane(),
                PlaneMut::over(base_buffer, geometry, out)?,
                parallelism,
            );
        }

        let encoded_buffer = scratch.take(encoded_rect.pixels())?;
        let base_buffer = scratch.take(out.pixels())?;
        let reduced_buffer = scratch.take(reduced_source.pixels())?;
        let base_reduced_buffer = scratch.take(base_rect.pixels())?;

        let mut encoded = PlaneMut::over(encoded_buffer, geometry, encoded_rect)?;
        encode(input, &mut encoded, parallelism);
        let encoded: Plane<'_> = encoded.as_plane();

        let mut encoded_reduced = PlaneMut::over(reduced_buffer, reduced_geometry, reduced_source)?;
        downsample(&encoded, REDUCTION, &mut encoded_reduced, parallelism);
        if let Some(Reduced::Hand(cells)) = reduced {
            filters::hand_back(cells, 0, &encoded_reduced.as_plane());
        }
        let mut base_reduced = PlaneMut::over(base_reduced_buffer, reduced_geometry, base_rect)?;
        guided_self(
            &encoded_reduced.as_plane(),
            self.r_red,
            EPS_CLARITY,
            &mut base_reduced,
            &mut scratch,
            parallelism,
        )?;
        self.finish(
            input,
            output,
            &encoded,
            &base_reduced.as_plane(),
            PlaneMut::over(base_buffer, geometry, out)?,
            parallelism,
        )
    }

    /// The base upsampled into `base`, then the residual's compressive gain reapplied over the
    /// output: `encoded` holds at least the output's encoded values, `base_reduced` the base on
    /// the reduced grid around it.
    fn finish(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        encoded: &Plane<'_>,
        base_reduced: &Plane<'_>,
        mut base: PlaneMut<'_>,
        parallelism: Parallelism,
    ) -> Result<(), Error> {
        let out = Rect::of(output.region());
        upsample(base_reduced, REDUCTION, &mut base, parallelism);

        let base = base.as_plane();
        let columns = filters::input_columns(input, out.x0, out.x1);
        output.for_rows(parallelism, |y, red, green, blue| {
            let y = i64::from(y);
            // Every row cut to the output's width, so the loop indexes them with no bounds check.
            let width = red.len();
            let (green, blue) = (&mut green[..width], &mut blue[..width]);
            let [r, g, b] = input.row(y).map(|plane| &plane[columns.clone()][..width]);
            let encoded = &encoded.span(y, out.x0, out.x1)[..width];
            let base = &base.span(y, out.x0, out.x1)[..width];
            for column in 0..width {
                let rgb = [r[column], g[column], b[column]];
                let e = encoded[column];
                let residual = e - base[column];
                let delta = filters::soft_clip(self.gain * residual, e, LIMIT_CLARITY);
                let value = if delta == 0.0 {
                    rgb
                } else {
                    luma::reconstruct(rgb, luma::rec709(rgb), srgb::decode_f32(e + delta))
                };
                [red[column], green[column], blue[column]] = value;
            }
        });
        Ok(())
    }
}

/// The rectangles one output rectangle of Clarity reads ([`Clarity::rects`]).
#[derive(Clone, Copy)]
struct Rects {
    frame: Rect,
    out: Rect,
    reduced_frame: Rect,
    /// The base on the reduced grid: the output's blocks and one more for the upsample.
    base_rect: Rect,
    /// The reduced source the guided filter reads around the base: the unit's reach in the grid.
    reduced_source: Rect,
    /// The full-resolution encoded values the reduced source is the block means of.
    encoded_rect: Rect,
}

/// The encoded luminance of `input` over the rectangle `plane` holds, read as row slices: its
/// columns lie inside the stage, where no read clamps.
fn encode(input: &Planes<'_>, plane: &mut PlaneMut<'_>, parallelism: Parallelism) {
    let rect = plane.rect();
    let columns = filters::input_columns(input, rect.x0, rect.x1);
    plane.for_rows(parallelism, |y, row| {
        let [red, green, blue] = input.row(y).map(|plane| &plane[columns.clone()]);
        for (value, ((r, g), b)) in row.iter_mut().zip(red.iter().zip(green).zip(blue)) {
            *value = filters::encoded_luminance([*r, *g, *b]);
        }
    });
}

impl SpatialUnit for Clarity {
    fn halo(&self, _: Stage) -> u32 {
        halo(self.long_side) as u32
    }

    /// The encoded plane and the upsampled base over the input region, and the reduced grid's own
    /// planes: the reduced source, the base and the guided filter's.
    fn scratch_bytes(&self, region: Stage) -> u64 {
        let full = filters::region_values(region).saturating_mul(2);
        let reduced = filters::reduced_values(region, REDUCTION as u32)
            .saturating_mul(2 + GUIDED_SELF_PLANES as u64);
        filters::scratch_bytes(full.saturating_add(reduced))
    }

    fn apply(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        _: Option<&Global>,
        scratch: &mut [f32],
        parallelism: Parallelism,
    ) -> Result<(), Error> {
        self.run(input, output, scratch, parallelism, None)
    }

    /// The encoded luminance's 4x block means, the reduced source the base is computed from:
    /// anchored at the stage origin and read before any coefficient, so a new amount reads the
    /// same plane.
    fn reduced_grid(&self) -> Option<ReducedGrid> {
        Some(ReducedGrid {
            key: Cow::Borrowed("presence clarity encoded luminance 4x block means"),
            factor: REDUCTION as u32,
            planes: 1,
        })
    }

    fn reduced_reach(&self, output: Region, stage: Stage) -> Region {
        let rects = self.rects(stage, output);
        if rects.out.is_empty() {
            return Region::EMPTY;
        }
        filters::grid_region(rects.reduced_source)
    }

    fn apply_reduced(
        &self,
        input: &Planes<'_>,
        output: &mut PlanesMut<'_>,
        _: Option<&Global>,
        scratch: &mut [f32],
        parallelism: Parallelism,
        cancel: &Cancel,
        reduced: Reduced<'_>,
    ) -> Result<(), Error> {
        cancel.check()?;
        self.run(input, output, scratch, parallelism, Some(reduced))?;
        cancel.check()
    }

    fn gpu(&self, _: Option<&Global>) -> Option<GpuSpatialUnit> {
        Some(super::gpu::clarity(self))
    }

    fn is_finite(&self) -> bool {
        self.amount.is_finite() && self.gain.is_finite()
    }

    fn describe(&self) -> String {
        format!(
            "presence clarity(amount={:+}, long side {})",
            self.amount, self.long_side
        )
    }
}
