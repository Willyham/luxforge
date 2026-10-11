//! The bounded analysis proxy registration and deghosting read: one input's upright frame reduced
//! by an integer box factor to at most [`MAX_PROXY_SIDE`] pixels a side, holding per proxy pixel
//! the `log2` of the box's mean Rec. 709 luminance and the box's largest and mean
//! [`SensorLevel`]s.
//!
//! The merge develops one input at a time: it streams each developed frame's rows through a
//! [`ProxyBuilder`] (or [`AnalysisProxy::from_linear`]), drops the frame, and keeps only the
//! proxies, 8 bytes a proxy pixel (8 MiB at the largest), so registration never needs two full
//! frames at once.
//!
//! Proxy pixel `(i, j)` averages the full-resolution box `[i·f, (i + 1)·f) × [j·f, (j + 1)·f)`;
//! only whole boxes are kept, so the last `W mod f` columns and `H mod f` rows are not read. Its
//! centre is the full-resolution point `(f·i + (f − 1)/2, f·j + (f − 1)/2)`.

use super::clipping::SensorClipModel;
use super::exposure::luminance;
use super::frame::{FrameRect, SensorLevel};
use crate::{Cancel, Error, LinearImage};

/// The longest proxy side. The box factor is the smallest integer that brings the frame's longer
/// side to at most this: a 6048 × 4024 frame reduces by 6 to 1008 × 670.
pub const MAX_PROXY_SIDE: u32 = 1024;
/// The shortest frame side alignment reads: a proxy must hold a coarse pyramid level of at least
/// 16 pixels a side.
pub const MIN_FRAME_SIDE: u32 = 64;
/// A proxy pixel is usable for comparison when its box's mean level is at least this (above the
/// noise floor)...
pub const PROXY_FLOOR: f64 = 1.0 / 256.0;
/// ...and its largest level at most this: the fusion's highlight roll-off start, so no sample a
/// clipped or nearly clipped site touched is compared.
pub const PROXY_CEILING: f64 = 0.8;

/// One input's analysis proxy. See the module documentation.
#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisProxy {
    pub(crate) frame_width: u32,
    pub(crate) frame_height: u32,
    pub(crate) factor: u32,
    pub(crate) width: usize,
    pub(crate) height: usize,
    /// `log2` of the box's mean luminance, `-∞` where it is not positive.
    pub(crate) log_luma: Vec<f32>,
    pub(crate) peak: Vec<SensorLevel>,
    pub(crate) mean: Vec<SensorLevel>,
}

impl AnalysisProxy {
    /// The box factor of a `width × height` frame: the smallest integer bringing its longer side to
    /// at most [`MAX_PROXY_SIDE`].
    pub fn factor_for(width: u32, height: u32) -> u32 {
        width.max(height).div_ceil(MAX_PROXY_SIDE).max(1)
    }

    /// The proxy dimensions of a `width × height` frame.
    pub fn dimensions_for(width: u32, height: u32) -> (usize, usize) {
        let factor = Self::factor_for(width, height);
        ((width / factor) as usize, (height / factor) as usize)
    }

    /// The bytes one proxy of a `width × height` frame holds.
    pub fn bytes_for(width: u32, height: u32) -> usize {
        let (w, h) = Self::dimensions_for(width, height);
        w * h * (size_of::<f32>() + 2 * size_of::<SensorLevel>())
    }

    /// The proxy of `image`'s upright view, with each pixel's level from `clip`, read one band of
    /// box rows at a time and checking `cancel` per band. Allocates the proxy and one band's
    /// accumulators, never a frame.
    pub fn from_linear(
        image: &LinearImage,
        clip: &SensorClipModel,
        cancel: &Cancel,
    ) -> Result<Self, Error> {
        let reader = image.reader();
        let (width, height) = reader.dimensions();
        let mut builder = ProxyBuilder::new(width, height)?;
        let factor = builder.proxy.factor;
        let mut band: Vec<([f32; 3], SensorLevel)> =
            vec![([0.0; 3], SensorLevel(0)); width as usize * factor as usize];
        for j in 0..builder.proxy.height {
            cancel.check()?;
            let walk = reader
                .walk((0, j as u32 * factor), (1, 0), (0, 1), width, factor)
                .ok_or_else(|| Error::internal("proxy band lies outside its view"))?;
            reader.visit(walk, |offset, rgb| {
                band[offset] = (rgb, clip.level(rgb));
                Ok::<(), Error>(())
            })?;
            for row in band.chunks_exact(width as usize) {
                builder.push_row(row.iter().copied())?;
            }
        }
        builder.finish()
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

    /// Whether proxy pixel `i` may be compared: its mean level above [`PROXY_FLOOR`], its largest
    /// at most [`PROXY_CEILING`] and its luminance positive.
    #[inline]
    pub(crate) fn usable(&self, i: usize) -> bool {
        usable(self.log_luma[i], self.peak[i], self.mean[i])
    }
}

#[inline]
pub(crate) fn usable(log_luma: f32, peak: SensorLevel, mean: SensorLevel) -> bool {
    log_luma.is_finite()
        && !peak.is_clipped()
        && peak.fraction() <= PROXY_CEILING
        && mean.fraction() >= PROXY_FLOOR
}

/// Builds one [`AnalysisProxy`] from a frame's rows in order, from any planar source: the merge
/// hands it each developed band as it has it. Holds one row of box accumulators.
#[derive(Debug)]
pub struct ProxyBuilder {
    proxy: AnalysisProxy,
    /// Full-resolution rows taken so far.
    rows: u32,
    sum_luma: Vec<f64>,
    sum_level: Vec<f64>,
    peak: Vec<SensorLevel>,
}

impl ProxyBuilder {
    /// A builder for a `width × height` upright frame; refused below [`MIN_FRAME_SIDE`] or past the
    /// linear source limits.
    pub fn new(width: u32, height: u32) -> Result<Self, Error> {
        crate::source::layout(width, height)?;
        if width < MIN_FRAME_SIDE || height < MIN_FRAME_SIDE {
            return Err(Error::validation(format!(
                "HDR alignment needs frames of at least {MIN_FRAME_SIDE} pixels a side"
            )));
        }
        let factor = AnalysisProxy::factor_for(width, height);
        let (w, h) = AnalysisProxy::dimensions_for(width, height);
        Ok(Self {
            proxy: AnalysisProxy {
                frame_width: width,
                frame_height: height,
                factor,
                width: w,
                height: h,
                log_luma: Vec::with_capacity(w * h),
                peak: Vec::with_capacity(w * h),
                mean: Vec::with_capacity(w * h),
            },
            rows: 0,
            sum_luma: vec![0.0; w],
            sum_level: vec![0.0; w],
            peak: vec![SensorLevel(0); w],
        })
    }

    /// Take the next rows of the frame: `rows.width` must be the frame's width. Rows past the last
    /// whole box are counted and ignored.
    pub fn push_rows(&mut self, rows: &FrameRect<'_>) -> Result<(), Error> {
        rows.check()?;
        if rows.width != self.proxy.frame_width as usize {
            return Err(Error::validation("proxy rows must span the frame"));
        }
        for y in 0..rows.height {
            self.push_row((0..rows.width).map(|x| {
                let (rgb, level) = rows.at(x, y);
                (rgb, level)
            }))?;
        }
        Ok(())
    }

    fn push_row(
        &mut self,
        row: impl ExactSizeIterator<Item = ([f32; 3], SensorLevel)>,
    ) -> Result<(), Error> {
        if self.rows >= self.proxy.frame_height {
            return Err(Error::validation("proxy rows run past the frame"));
        }
        let factor = self.proxy.factor as usize;
        let kept = self.proxy.width * factor;
        if self.proxy.log_luma.len() < self.proxy.width * self.proxy.height {
            for (x, (rgb, level)) in row.take(kept).enumerate() {
                if !rgb.iter().all(|value| value.is_finite()) {
                    return Err(Error::validation("HDR input sample is not finite"));
                }
                let i = x / factor;
                self.sum_luma[i] += f64::from(luminance(rgb));
                self.sum_level[i] += level.fraction();
                self.peak[i] = self.peak[i].max(level);
            }
        }
        self.rows += 1;
        if self.rows.is_multiple_of(self.proxy.factor)
            && self.proxy.log_luma.len() < self.proxy.width * self.proxy.height
        {
            let area = (factor * factor) as f64;
            for i in 0..self.proxy.width {
                let luma = self.sum_luma[i] / area;
                self.proxy.log_luma.push(if luma > 0.0 {
                    luma.log2() as f32
                } else {
                    f32::NEG_INFINITY
                });
                self.proxy
                    .mean
                    .push(SensorLevel::from_fraction(self.sum_level[i] / area));
                self.proxy.peak.push(self.peak[i]);
            }
            self.sum_luma.fill(0.0);
            self.sum_level.fill(0.0);
            self.peak.fill(SensorLevel(0));
        }
        Ok(())
    }

    /// The proxy, once every whole box is complete: the rows past the last whole box may be
    /// pushed or left out.
    pub fn finish(self) -> Result<AnalysisProxy, Error> {
        if self.proxy.log_luma.len() != self.proxy.width * self.proxy.height {
            return Err(Error::validation("proxy is missing frame rows"));
        }
        Ok(self.proxy)
    }
}
