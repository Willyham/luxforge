//! One input's aligned samples for one rectangle in reference coordinates: the single resample
//! of the merge, in the linear domain, producing the [`FrameRect`] fusion reads.
//!
//! Interpolation is bilinear: output pixel `p` reads the four input pixels around
//! `q = transform(p)` and blends them in `f64`, rounded once to `f32`. Bilinear never overshoots
//! (every value lies between its taps, so signs and headroom stay as recorded and no ringing
//! crosses a highlight) and reads a 2 × 2 footprint, so clipping evidence spreads by at most one
//! pixel. The level is the largest level among the taps the blend weights above zero, never an
//! interpolation of levels. The identity (the reference, and every input with alignment off) is
//! an exact copy.
//!
//! A rectangle must lie inside the bracket's [`super::Coverage`]: every pixel's taps are then
//! recorded samples, and nothing is invented at a border. The worker reads a bounded window of the
//! input for each rectangle: [`source_window`] is the smallest whole-pixel rectangle of input
//! pixels the rectangle's taps lie in (the bounding box of its four mapped corners, widened by one
//! pixel for the right and lower taps and by `1e-6` against rounding, within the frame). For the
//! largest 1024 × 1024 rectangle and a 3° rotation it is at most about 1080 × 1080 pixels.

use super::align::{Affine, PixelRect, RigidTransform};
use super::coverage::inside;
use super::frame::{FrameRect, SensorLevel};
use super::fusion::MAX_FUSION_PIXELS;
use crate::{Cancel, Error};

/// The input pixels `rect`'s bilinear taps read under `transform`, in a `width × height` frame:
/// see the module documentation. `None` when `rect` is empty.
pub fn source_window(
    transform: &RigidTransform,
    rect: PixelRect,
    width: u32,
    height: u32,
) -> Option<PixelRect> {
    if rect.is_empty() {
        return None;
    }
    if transform.is_identity() {
        return Some(rect);
    }
    let affine = transform.affine();
    let (x0, y0) = (f64::from(rect.x), f64::from(rect.y));
    let (x1, y1) = (
        x0 + f64::from(rect.width - 1),
        y0 + f64::from(rect.height - 1),
    );
    let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| affine.map(x, y));
    let (mut lo, mut hi) = (
        (f64::INFINITY, f64::INFINITY),
        (f64::NEG_INFINITY, f64::NEG_INFINITY),
    );
    for (x, y) in corners {
        lo = (lo.0.min(x), lo.1.min(y));
        hi = (hi.0.max(x), hi.1.max(y));
    }
    let clamp = |value: f64, last: u32| value.clamp(0.0, f64::from(last)) as u32;
    let left = clamp((lo.0 - 1e-6).floor(), width - 1);
    let top = clamp((lo.1 - 1e-6).floor(), height - 1);
    let right = clamp((hi.0 + 1e-6).floor() + 1.0, width - 1);
    let bottom = clamp((hi.1 + 1e-6).floor() + 1.0, height - 1);
    Some(PixelRect {
        x: left,
        y: top,
        width: right - left + 1,
        height: bottom - top + 1,
    })
}

/// A window of one input's developed frame: its samples and levels, `frame` holding the window's
/// `width × height` pixels, whose first is input pixel `(x, y)`.
#[derive(Clone, Copy, Debug)]
pub struct SourceWindow<'a> {
    pub x: u32,
    pub y: u32,
    pub frame: FrameRect<'a>,
}

/// One input's aligned samples for one rectangle, reusable across rectangles: resampling into it
/// keeps its allocation when the next rectangle is no larger.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlignedRect {
    pub width: usize,
    pub height: usize,
    pub rgb: [Vec<f32>; 3],
    pub levels: Vec<SensorLevel>,
}

impl AlignedRect {
    /// The samples as fusion reads them.
    pub fn view(&self) -> FrameRect<'_> {
        FrameRect {
            width: self.width,
            height: self.height,
            stride: self.width,
            rgb: [&self.rgb[0], &self.rgb[1], &self.rgb[2]],
            levels: &self.levels,
        }
    }

    fn reset(&mut self, width: usize, height: usize) {
        let n = width * height;
        self.width = width;
        self.height = height;
        for plane in &mut self.rgb {
            plane.clear();
            plane.resize(n, 0.0);
        }
        self.levels.clear();
        self.levels.resize(n, SensorLevel(0));
    }
}

/// Resample `rect` (reference coordinates) of the input `source` is a window of, a
/// `frame_width × frame_height` frame, under `transform`, into `out`. Refused when `rect` is
/// larger than a fusion rectangle, or any tap lies outside the frame (the rectangle is not inside
/// the coverage) or outside the window. On an error `out` holds no usable rectangle.
pub fn resample_rect(
    transform: &RigidTransform,
    source: &SourceWindow<'_>,
    frame_width: u32,
    frame_height: u32,
    rect: PixelRect,
    out: &mut AlignedRect,
    cancel: &Cancel,
) -> Result<(), Error> {
    cancel.check()?;
    source.frame.check()?;
    let (width, height) = (rect.width as usize, rect.height as usize);
    if width
        .checked_mul(height)
        .is_none_or(|pixels| pixels > MAX_FUSION_PIXELS)
    {
        return Err(Error::resource_limit(
            "HDR aligned rectangle exceeds 1 Mi pixels",
        ));
    }
    out.reset(width, height);
    let window = PixelRect {
        x: source.x,
        y: source.y,
        width: source.frame.width as u32,
        height: source.frame.height as u32,
    };
    let outside =
        || Error::validation("HDR aligned rectangle reads outside its source window or coverage");
    if transform.is_identity() {
        if !window.contains(&rect)
            || u64::from(rect.x) + u64::from(rect.width) > u64::from(frame_width)
            || u64::from(rect.y) + u64::from(rect.height) > u64::from(frame_height)
        {
            return Err(outside());
        }
        let (dx, dy) = ((rect.x - window.x) as usize, (rect.y - window.y) as usize);
        for y in 0..height {
            let from = (y + dy) * source.frame.stride + dx;
            let to = y * width;
            for c in 0..3 {
                out.rgb[c][to..to + width]
                    .copy_from_slice(&source.frame.rgb[c][from..from + width]);
            }
            out.levels[to..to + width].copy_from_slice(&source.frame.levels[from..from + width]);
        }
        return Ok(());
    }
    let affine: Affine = transform.affine();
    let frame = &source.frame;
    let (wx, wy) = (i64::from(window.x), i64::from(window.y));
    let (ww, wh) = (frame.width as i64, frame.height as i64);
    for y in 0..height {
        for x in 0..width {
            let q = affine.map(f64::from(rect.x) + x as f64, f64::from(rect.y) + y as f64);
            if !inside(q, frame_width, frame_height) {
                return Err(outside());
            }
            let (fx, fy) = (q.0.floor(), q.1.floor());
            let (ax, ay) = (q.0 - fx, q.1 - fy);
            let (tx, ty) = (fx as i64 - wx, fy as i64 - wy);
            let mut acc = [0.0_f64; 3];
            let mut level = SensorLevel(0);
            for (dx, dy, weight) in [
                (0, 0, (1.0 - ax) * (1.0 - ay)),
                (1, 0, ax * (1.0 - ay)),
                (0, 1, (1.0 - ax) * ay),
                (1, 1, ax * ay),
            ] {
                if weight == 0.0 {
                    continue;
                }
                let (sx, sy) = (tx + dx, ty + dy);
                if sx < 0 || sy < 0 || sx >= ww || sy >= wh {
                    return Err(outside());
                }
                let (rgb, tap) = frame.at(sx as usize, sy as usize);
                for c in 0..3 {
                    acc[c] += weight * f64::from(rgb[c]);
                }
                level = level.max(tap);
            }
            let o = y * width + x;
            for (plane, value) in out.rgb.iter_mut().zip(acc) {
                plane[o] = value as f32;
            }
            out.levels[o] = level;
        }
    }
    Ok(())
}
