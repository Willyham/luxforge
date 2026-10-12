//! The bounded analysis proxy panorama registration reads: one developed frame reduced by an
//! integer box factor to at most [`MAX_PROXY_SIDE`] pixels a side, holding per proxy pixel the
//! `log2` of the box's mean Rec. 709 luminance and whether the box is usable (every sample finite
//! and unclipped, and the mean above [`LUMA_FLOOR`]).
//!
//! A merge develops one frame at a time and streams its rows, in bands of any height, through a
//! [`ProxyBuilder`]; it keeps the proxy (5 bytes a proxy pixel, under 12 MiB at the largest) or
//! only the features detected on it, and drops the frame.
//!
//! Proxy pixel `(i, j)` averages the full-resolution box `[i·f, (i + 1)·f) × [j·f, (j + 1)·f)`;
//! only whole boxes are kept, so the last `W mod f` columns and `H mod f` rows are not read. Its
//! centre is the full-resolution point `(f·i + (f − 1)/2, f·j + (f − 1)/2)` ([`PanoramaProxy::to_frame`]).

use crate::{Cancel, Error};

/// The longest proxy side. The box factor is the smallest integer that brings the frame's longer
/// side to at most this: a 6048 × 4024 frame reduces by 4 to 1512 × 1006, a 9504 × 6336 frame by 7
/// to 1357 × 905.
pub const MAX_PROXY_SIDE: u32 = 1536;
/// The shortest frame side registration reads.
pub const MIN_FRAME_SIDE: u32 = 64;
/// A box whose mean luminance is at most this (linear, white 1) is unusable: its `log2` holds the
/// floor's, and no feature is described across it.
pub const LUMA_FLOOR: f32 = 1.0 / 65536.0;
/// The Rec. 709 luminance weights of linear sRGB.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// One frame's analysis proxy. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct PanoramaProxy {
    pub(crate) frame_width: u32,
    pub(crate) frame_height: u32,
    pub(crate) factor: u32,
    pub(crate) width: usize,
    pub(crate) height: usize,
    /// `log2` of the box's mean luminance, at least `log2(LUMA_FLOOR)`.
    pub(crate) log_luma: Vec<f32>,
    pub(crate) usable: Vec<bool>,
}

impl PanoramaProxy {
    /// The box factor of a `width × height` frame for a proxy of at most `max_side` a side.
    pub fn factor_for(width: u32, height: u32, max_side: u32) -> u32 {
        width.max(height).div_ceil(max_side.max(1)).max(1)
    }

    /// The bytes a proxy of a `width × height` frame holds, at most `max_side` a side.
    pub fn bytes_for(width: u32, height: u32, max_side: u32) -> usize {
        let factor = Self::factor_for(width, height, max_side);
        (width / factor) as usize
            * (height / factor) as usize
            * (size_of::<f32>() + size_of::<bool>())
    }

    /// The full-resolution frame this reduces.
    pub fn frame_dimensions(&self) -> (u32, u32) {
        (self.frame_width, self.frame_height)
    }

    pub fn factor(&self) -> u32 {
        self.factor
    }

    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The full-resolution point a proxy point shows.
    pub fn to_frame(&self, x: f64, y: f64) -> (f64, f64) {
        let f = f64::from(self.factor);
        (f * x + (f - 1.0) / 2.0, f * y + (f - 1.0) / 2.0)
    }

    /// The proxy's `log2` luminance, row-major.
    pub fn log_luma(&self) -> &[f32] {
        &self.log_luma
    }

    /// Whether each proxy pixel is usable, row-major.
    pub fn usable(&self) -> &[bool] {
        &self.usable
    }

    /// The bytes this proxy holds.
    pub fn bytes(&self) -> usize {
        self.log_luma.len() * size_of::<f32>() + self.usable.len()
    }
}

/// A band of developed rows: planar linear sRGB with an optional per-sample clipping flag (a RAW
/// sample at its sensor's clipping level, a JPEG sample with an encoded channel at 255).
#[derive(Clone, Copy, Debug)]
pub struct ProxyRows<'a> {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub rgb: [&'a [f32]; 3],
    pub clipped: Option<&'a [bool]>,
}

impl ProxyRows<'_> {
    fn check(&self) -> Result<(), Error> {
        let needed = if self.height == 0 {
            0
        } else {
            (self.height - 1)
                .checked_mul(self.stride)
                .and_then(|n| n.checked_add(self.width))
                .ok_or_else(|| Error::validation("proxy rows overflow"))?
        };
        if self.stride < self.width
            || self.rgb.iter().any(|plane| plane.len() < needed)
            || self.clipped.is_some_and(|clipped| clipped.len() < needed)
        {
            return Err(Error::validation(
                "proxy rows are shorter than their dimensions",
            ));
        }
        Ok(())
    }
}

/// Builds one [`PanoramaProxy`] from a frame's rows in order. Holds one row of box accumulators.
#[derive(Debug)]
pub struct ProxyBuilder {
    proxy: PanoramaProxy,
    rows: u32,
    sum: Vec<f64>,
    bad: Vec<bool>,
}

impl ProxyBuilder {
    /// A builder for a `width × height` upright frame with a proxy of at most [`MAX_PROXY_SIDE`] a
    /// side.
    pub fn new(width: u32, height: u32) -> Result<Self, Error> {
        Self::with_max_side(width, height, MAX_PROXY_SIDE)
    }

    /// A builder whose proxy is at most `max_side` a side (at least [`MIN_FRAME_SIDE`]); refused
    /// below [`MIN_FRAME_SIDE`] a side or past the linear source limits.
    pub fn with_max_side(width: u32, height: u32, max_side: u32) -> Result<Self, Error> {
        crate::source::layout(width, height)?;
        if width < MIN_FRAME_SIDE || height < MIN_FRAME_SIDE || max_side < MIN_FRAME_SIDE {
            return Err(Error::validation(format!(
                "panorama registration needs frames and proxies of at least {MIN_FRAME_SIDE} pixels a side"
            )));
        }
        let factor = PanoramaProxy::factor_for(width, height, max_side);
        let (w, h) = ((width / factor) as usize, (height / factor) as usize);
        Ok(Self {
            proxy: PanoramaProxy {
                frame_width: width,
                frame_height: height,
                factor,
                width: w,
                height: h,
                log_luma: Vec::with_capacity(w * h),
                usable: Vec::with_capacity(w * h),
            },
            rows: 0,
            sum: vec![0.0; w],
            bad: vec![false; w],
        })
    }

    /// Take the next rows of the frame, checking `cancel` per band: `rows.width` must be the
    /// frame's width. Rows past the last whole box are counted and ignored.
    pub fn push_rows(&mut self, rows: &ProxyRows<'_>, cancel: &Cancel) -> Result<(), Error> {
        cancel.check()?;
        rows.check()?;
        if rows.width != self.proxy.frame_width as usize {
            return Err(Error::validation("proxy rows must span the frame"));
        }
        let factor = self.proxy.factor as usize;
        let kept = self.proxy.width * factor;
        for y in 0..rows.height {
            if self.rows >= self.proxy.frame_height {
                return Err(Error::validation("proxy rows run past the frame"));
            }
            if self.proxy.log_luma.len() < self.proxy.width * self.proxy.height {
                let start = y * rows.stride;
                for x in 0..kept {
                    let i = start + x;
                    let rgb = [rows.rgb[0][i], rows.rgb[1][i], rows.rgb[2][i]];
                    if !rgb.iter().all(|v| v.is_finite()) {
                        return Err(Error::validation("panorama frame sample is not finite"));
                    }
                    let b = x / factor;
                    self.sum[b] +=
                        f64::from(LUMA[0] * rgb[0] + LUMA[1] * rgb[1] + LUMA[2] * rgb[2]);
                    if rows.clipped.is_some_and(|clipped| clipped[i]) {
                        self.bad[b] = true;
                    }
                }
            }
            self.rows += 1;
            if self.rows.is_multiple_of(self.proxy.factor)
                && self.proxy.log_luma.len() < self.proxy.width * self.proxy.height
            {
                let area = (factor * factor) as f64;
                for b in 0..self.proxy.width {
                    let mean = (self.sum[b] / area) as f32;
                    let lit = mean > LUMA_FLOOR;
                    self.proxy.log_luma.push(mean.max(LUMA_FLOOR).log2());
                    self.proxy.usable.push(lit && !self.bad[b]);
                }
                self.sum.fill(0.0);
                self.bad.fill(false);
            }
        }
        Ok(())
    }

    /// The proxy, once every whole box is complete.
    pub fn finish(self) -> Result<PanoramaProxy, Error> {
        if self.proxy.log_luma.len() != self.proxy.width * self.proxy.height {
            return Err(Error::validation("proxy is missing frame rows"));
        }
        Ok(self.proxy)
    }
}
