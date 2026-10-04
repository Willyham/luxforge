//! Exact geometry and the resample: composing and mapping exact integer steps, mapping a resample's
//! output back into its input frame, the one rectangle a resample reads, and the byte domain's
//! bilinear pass.

use super::{Raster, Taps, frame_mut, parallel, zeroed_frame};
use crate::{
    Cancel, Error,
    colour::srgb::{Quantizer, decode_table, quantizer},
    modules::{ExactGeometry, Region, Resample, Stage},
};
use rayon::prelude::*;
use std::sync::Arc;

/// Composition and mapping of exact geometry belong to the host; modules only declare one step.
impl ExactGeometry {
    pub(crate) fn identity(width: u32, height: u32) -> Self {
        Self {
            a: 1,
            b: 0,
            c: 0,
            d: 1,
            tx: 0,
            ty: 0,
            output_width: width,
            output_height: height,
        }
    }

    /// Compose `self` followed by `next`.
    pub(crate) fn then(self, next: Self) -> Self {
        Self {
            a: next.a * self.a + next.b * self.c,
            b: next.a * self.b + next.b * self.d,
            c: next.c * self.a + next.d * self.c,
            d: next.c * self.b + next.d * self.d,
            tx: next.a * self.tx + next.b * self.ty + next.tx,
            ty: next.c * self.tx + next.d * self.ty + next.ty,
            output_width: next.output_width,
            output_height: next.output_height,
        }
    }

    /// A pure translation that copies one rectangle of its input stage: what a crop with no
    /// straightening declares. Exact, and composable with neighbouring transforms into one pass.
    pub(crate) fn crop(x: i64, y: i64, width: u32, height: u32) -> Self {
        Self {
            a: 1,
            b: 0,
            c: 0,
            d: 1,
            tx: -x,
            ty: -y,
            output_width: width,
            output_height: height,
        }
    }

    /// Where an input-stage point lands, or `None` when it falls outside the output stage: a point
    /// replacement cropped away later is simply not visible.
    pub(super) fn map(self, x: u32, y: u32) -> Option<(u32, u32)> {
        let out_x = self.a * i64::from(x) + self.b * i64::from(y) + self.tx;
        let out_y = self.c * i64::from(x) + self.d * i64::from(y) + self.ty;
        let inside = out_x >= 0
            && out_x < i64::from(self.output_width)
            && out_y >= 0
            && out_y < i64::from(self.output_height);
        inside.then_some((out_x as u32, out_y as u32))
    }

    /// Whether every output pixel of this mapping reads a pixel that exists in the given input
    /// stage. A module declares its own exact step, so the host checks it before any pass reads a
    /// frame through it: the image of the output rectangle is a rectangle, so its corners decide.
    pub(crate) fn reads_inside(self, input_width: u32, input_height: u32) -> bool {
        if self.output_width == 0 || self.output_height == 0 {
            return false;
        }
        let far_x = i64::from(self.output_width) - 1;
        let far_y = i64::from(self.output_height) - 1;
        [(0, 0), (far_x, 0), (0, far_y), (far_x, far_y)]
            .into_iter()
            .all(|(x, y)| {
                let translated_x = x - self.tx;
                let translated_y = y - self.ty;
                let input_x = self.a * translated_x + self.c * translated_y;
                let input_y = self.b * translated_x + self.d * translated_y;
                (0..i64::from(input_width)).contains(&input_x)
                    && (0..i64::from(input_height)).contains(&input_y)
            })
    }

    /// Where an input-stage pixel rectangle lands in the output stage, clipped to it.
    ///
    /// Closed form and exact: `a`, `b`, `c`, `d` are a signed permutation, so the image of a
    /// rectangle is a rectangle and its two opposite corners decide it. A rectangle that maps
    /// entirely outside the output stage comes back empty, which is what lets a masked run skip a
    /// whole frame whose mask was cropped away.
    pub(super) fn map_region(self, region: Region) -> Region {
        if region.is_empty() || self.output_width == 0 || self.output_height == 0 {
            return Region::EMPTY;
        }
        let corner = |x: u32, y: u32| -> (i64, i64) {
            (
                self.a * i64::from(x) + self.b * i64::from(y) + self.tx,
                self.c * i64::from(x) + self.d * i64::from(y) + self.ty,
            )
        };
        let (first_x, first_y) = corner(region.x0, region.y0);
        let (last_x, last_y) = corner(region.x1() - 1, region.y1() - 1);
        let x0 = first_x.min(last_x).clamp(0, i64::from(self.output_width)) as u32;
        let x1 = (first_x.max(last_x) + 1).clamp(0, i64::from(self.output_width)) as u32;
        let y0 = first_y.min(last_y).clamp(0, i64::from(self.output_height)) as u32;
        let y1 = (first_y.max(last_y) + 1).clamp(0, i64::from(self.output_height)) as u32;
        if x1 <= x0 || y1 <= y0 {
            return Region::EMPTY;
        }
        Region {
            x0,
            y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }

    #[inline]
    pub(super) fn unmap(self, x: u32, y: u32) -> (u32, u32) {
        let translated_x = i64::from(x) - self.tx;
        let translated_y = i64::from(y) - self.ty;
        let input_x = self.a * translated_x + self.c * translated_y;
        let input_y = self.b * translated_x + self.d * translated_y;
        debug_assert!(input_x >= 0 && input_y >= 0);
        (input_x as u32, input_y as u32)
    }

    /// The input-stage steps [`Self::unmap`] takes for one more output column and one more output
    /// row: `(a, b)` and `(c, d)`, each a unit step along one axis. With the unmap of a rectangle's
    /// first pixel they are every pixel it reads, which is how the linear rows walk a source view
    /// or a spatial frame.
    #[inline]
    pub(super) fn unmap_steps(self) -> ((i64, i64), (i64, i64)) {
        ((self.a, self.b), (self.c, self.d))
    }

    /// The input-stage rectangle an output-stage rectangle reads: the exact inverse of
    /// [`Self::map_region`], from the two opposite corners, since the mapping is a signed
    /// permutation with an integer translation. `region` must lie inside the output stage.
    pub(crate) fn unmap_region(self, region: Region) -> Region {
        if region.is_empty() {
            return region;
        }
        let (first_x, first_y) = self.unmap(region.x0, region.y0);
        let (last_x, last_y) = self.unmap(region.x1() - 1, region.y1() - 1);
        Region {
            x0: first_x.min(last_x),
            y0: first_y.min(last_y),
            width: first_x.abs_diff(last_x) + 1,
            height: first_y.abs_diff(last_y) + 1,
        }
    }

    /// Whether every output row is one run of an input row: the linear part is the identity, so
    /// the geometry at most translates.
    pub(super) fn keeps_rows(self) -> bool {
        (self.a, self.b, self.c, self.d) == (1, 0, 0, 1)
    }

    pub(crate) fn is_identity(self, input_width: u32, input_height: u32) -> bool {
        self.output_width == input_width
            && self.output_height == input_height
            && (self.a, self.b, self.c, self.d, self.tx, self.ty) == (1, 0, 0, 1, 0, 0)
    }
}

/// Pixels [`Resample::reads`] keeps beyond the taps a resample's output reads, on every side: a
/// bilinear tap reads the pixel at `floor(u - ½)` and the one after it, and the corners of an affine
/// image bound every interior coordinate only up to rounding, which this covers with room to spare.
pub(crate) const TAP_MARGIN: u32 = 2;

/// Mapping the output of one resample back into its input frame is the host's too.
impl Resample {
    /// The continuous input coordinate one output pixel center samples.
    #[inline]
    fn input_at(&self, x: u32, y: u32) -> (f64, f64) {
        self.map.input_at(f64::from(x) + 0.5, f64::from(y) + 0.5)
    }

    /// [`Self::input_at`] in a frame that holds only a window of the resample's input stage, whose
    /// top-left pixel is `origin` in that stage: the same coordinate, translated after the
    /// resample's own arithmetic. An integer subtracted from a non-negative coordinate below 2^52
    /// is exact in `f64`, so every tap of a windowed frame is the same pixel with the same weight
    /// as in the whole one. `(0, 0)` is every exact render's origin, and changes nothing.
    #[inline]
    pub(super) fn input_from(&self, origin: (u32, u32), x: u32, y: u32) -> (f64, f64) {
        let (u, v) = self.input_at(x, y);
        if origin == (0, 0) {
            (u, v)
        } else {
            (u - f64::from(origin.0), v - f64::from(origin.1))
        }
    }

    /// The rectangle of its input frame, a `input` stage whose top-left pixel is `origin` in the
    /// stage the resample was compiled against, that the resample reads over `window`, a
    /// rectangle of its full output stage: the one read-rectangle rule, for the colour band before
    /// a resample, a windowed proxy's cut and the linear driver's tap blocks alike.
    ///
    /// The mapping supplies conservative continuous bounds, including radial extrema for a warp
    /// chain. A bilinear tap reads the pixel at `floor(u - ½)` and the one after it, clamped to the
    /// stage edge, and [`TAP_MARGIN`] pixels on every side cover rounding with room to spare.
    /// `None` when the window or input is empty or the mapping cannot bound finite coordinates,
    /// which a caller answers by reading everything or reading each tap on its own.
    pub(crate) fn reads(&self, origin: (u32, u32), window: Region, input: Stage) -> Option<Region> {
        if window.is_empty() || input.width == 0 || input.height == 0 {
            return None;
        }
        let (x0, y0, x1, y1) = self.map.bounds((
            f64::from(window.x0) + 0.5,
            f64::from(window.y0) + 0.5,
            f64::from(window.x1()) - 0.5,
            f64::from(window.y1()) - 0.5,
        ))?;
        let tap = |value: f64| (value - 0.5).floor();
        let low = [x0 - f64::from(origin.0), y0 - f64::from(origin.1)].map(tap);
        let high =
            [x1 - f64::from(origin.0), y1 - f64::from(origin.1)].map(|value| tap(value) + 1.0);
        if !low.iter().chain(high.iter()).all(|v| v.is_finite()) {
            return None;
        }
        // Each tap index clamped to the stage as the blend clamps it, then the margin, then the
        // stage again: `first..=last`, never empty.
        let margin = f64::from(TAP_MARGIN);
        let span = |low: f64, high: f64, limit: u32| {
            let last = f64::from(limit - 1);
            let first = (low.clamp(0.0, last) - margin).max(0.0) as u32;
            let last = (high.clamp(0.0, last) + margin).min(last) as u32;
            (first, last - first + 1)
        };
        let (x0, width) = span(low[0], high[0], input.width);
        let (y0, height) = span(low[1], high[1], input.height);
        Some(Region {
            x0,
            y0,
            width,
            height,
        })
    }
}

/// The input pixel one continuous input coordinate falls in, clamped to the frame exactly as the
/// sampler clamps its own indices. A resample maps an output pixel center between input pixels, and
/// this is the corner the bilinear blend weights most, so it is the pixel that output pixel shows.
#[inline]
pub(super) fn nearest_index(value: f64, limit: u32) -> u32 {
    let last = limit.saturating_sub(1);
    let index = value.floor();
    if index <= 0.0 {
        0
    } else if index >= f64::from(last) {
        last
    } else {
        index as u32
    }
}

/// One bilinear sample of a byte frame in linear light, through the taps [`Taps`] clamps to the
/// frame's edge, quantized by forward rounding, `round(255 · encode(v))`, through the guarded
/// threshold search [`Quantizer::rounded`] the RAW terminal shares: the byte domain's resample.
///
/// `fetch` reads one pixel of that frame; the rasterizing path reads a buffer and the point-query
/// path evaluates the previous segment recursively, so both produce identical bytes. The decode
/// table and the quantizer are the caller's, so a pass takes them once rather than per sample.
#[inline]
pub(super) fn bilinear(
    table: &[f32; 256],
    quantizer: &Quantizer,
    u: f64,
    v: f64,
    width: u32,
    height: u32,
    mut fetch: impl FnMut(u32, u32) -> Result<[u8; 4], Error>,
) -> Result<[u8; 4], Error> {
    let taps = Taps::new(u, v, width, height);
    let [top_left, top_right, bottom_left, bottom_right] = taps.corners;
    let [w0, w1, w2, w3] = taps.weights;
    let corners = [
        (fetch(top_left.0, top_left.1)?, w0),
        (fetch(top_right.0, top_right.1)?, w1),
        (fetch(bottom_left.0, bottom_left.1)?, w2),
        (fetch(bottom_right.0, bottom_right.1)?, w3),
    ];
    // Frames are opaque by contract, so a resample writes alpha rather than blending it.
    let mut pixel = [0, 0, 0, 255];
    for (channel, slot) in pixel.iter_mut().enumerate().take(3) {
        let linear: f64 = corners
            .iter()
            .map(|(corner, weight)| weight * f64::from(table[corner[channel] as usize]))
            .sum();
        *slot = quantizer.rounded(linear);
    }
    Ok(pixel)
}

/// One interpolating pass: the resample reads the frame it was given and writes the next one.
pub(super) fn resample_frame(
    input: &[u8],
    input_width: u32,
    input_height: u32,
    resample: &Resample,
    origin: (u32, u32),
    window: Region,
    cancel: &Cancel,
) -> Result<Arc<Vec<u8>>, Error> {
    if window.is_empty()
        || window.x1() > resample.output_width
        || window.y1() > resample.output_height
    {
        return Err(Error::internal(
            "resample window lies outside its output stage",
        ));
    }
    let (width, height) = (window.width, window.height);
    cancel.check()?;
    let mut frame = zeroed_frame(Raster::expected_len(width, height)?);
    let output = frame_mut(&mut frame);
    let row_bytes = usize::try_from(u64::from(width) * 4)
        .map_err(|_| Error::resource_limit("image row is not addressable"))?;
    let fetch = |x: u32, y: u32| -> Result<[u8; 4], Error> {
        let offset = ((u64::from(y) * u64::from(input_width) + u64::from(x)) * 4) as usize;
        let pixel = &input[offset..offset + 4];
        Ok([pixel[0], pixel[1], pixel[2], pixel[3]])
    };
    let (table, quantizer) = (decode_table(), quantizer());
    // One relaxed load per output row, ahead of that row's samples; the point sampler and the
    // bilinear blend are untouched.
    let sample_row = |out_y: usize, row: &mut [u8]| -> Result<(), Error> {
        cancel.check()?;
        for out_x in 0..width {
            let (u, v) = resample.input_from(origin, window.x0 + out_x, window.y0 + out_y as u32);
            let pixel = bilinear(table, quantizer, u, v, input_width, input_height, fetch)?;
            let to = out_x as usize * 4;
            row[to..to + 4].copy_from_slice(&pixel);
        }
        Ok(())
    };
    if parallel::pooled(
        parallel::resample_pass(resample),
        u64::from(width) * u64::from(height),
    ) {
        output
            .par_chunks_exact_mut(row_bytes)
            .enumerate()
            .try_for_each(|(out_y, row)| sample_row(out_y, row))?;
    } else {
        output
            .chunks_exact_mut(row_bytes)
            .enumerate()
            .try_for_each(|(out_y, row)| sample_row(out_y, row))?;
    }
    Ok(frame)
}
