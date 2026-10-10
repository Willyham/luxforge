//! Sensor-clipping evidence for a developed RAW frame, from its linear planes and its RAW
//! interpretation alone, with no second read of the mosaic and no change to the shared
//! development.
//!
//! The development normalizes each site to sensor white (1.0) and multiplies it by its channel's
//! white-balance gain `g_c` before the demosaic, and calibrates the demosaiced camera RGB into
//! linear sRGB with `rgb_cam` (`docs/design/initial-raw.md`, pixel and colour contract). So a
//! developed pixel's camera RGB is `rgb_cam⁻¹ · rgb`, and channel `c` reached its sensor ceiling
//! when its camera value reached:
//!
//! - **Bayer:** `min(g_c, 65536/65535)`, because RCD clips each gained site to its input ceiling
//!   ([`BAYER_INPUT_CEILING`]): with `g_c > 1` a site saturates in the demosaic below sensor
//!   white, which is exactly the latitude the development loses.
//! - **X-Trans, linear RGB, monochrome:** `g_c`, sensor white at its gain.
//!
//! A pixel's level is `max_c (camera_c / ceiling_c)` ([`SensorLevel`]).
//!
//! Limitations, all erring towards rejecting a sample rather than trusting a clipped one where it
//! matters:
//!
//! - The demosaic interpolates: a pixel beside a saturated site reads its neighbour's capped
//!   value blended with its own, so its level can sit just below the ceiling. Fusion's highlight
//!   roll-off starts at 0.8, absorbing this.
//! - DNG corrections after the demosaic (a gain map or radial vignette lifting the corners, a
//!   warp resampling) move a pixel's value against the uniform ceiling. A lift raises the level,
//!   which over-reports clipping; a warp blends levels like the demosaic.
//! - Under-black sites are clipped to zero by RCD; such pixels read a low level, where fusion
//!   already prefers the longer exposures.
//! - A sensor that saturates below its declared white level reads below 1 at saturation; the
//!   roll-off covers declared levels within 5% of the true one.

use super::frame::{SensorLayout, SensorLevel, view_len};
use crate::{Error, colour::mat3};

/// The pinned RCD's input ceiling as a fraction of sensor white: it reads each site as
/// `LIM01(value / 65536)` of a normalized value whose white is 65535.
pub const BAYER_INPUT_CEILING: f64 = 65536.0 / 65535.0;

/// How to read a developed frame's sensor levels.
#[derive(Clone, Debug, PartialEq)]
pub struct SensorClipModel {
    camera_from_rgb: [[f64; 3]; 3],
    ceiling: [f64; 3],
}

impl SensorClipModel {
    /// The model of a development at `gains` through `rgb_cam` (LibRaw's camera-to-sRGB rows) for
    /// `layout`. Refused when the matrix is singular or not finite, or the gains are not finite
    /// and positive (monochrome takes only unit gains, as its development does).
    pub fn new(
        rgb_cam: [[f32; 3]; 3],
        gains: [f32; 3],
        layout: SensorLayout,
    ) -> Result<Self, Error> {
        if !gains.iter().all(|gain| gain.is_finite() && *gain > 0.0)
            || (layout.is_monochrome() && gains != [1.0; 3])
        {
            return Err(Error::validation(
                "sensor clipping needs finite, positive development gains",
            ));
        }
        let matrix = rgb_cam.map(|row| row.map(f64::from));
        let (inverse, determinant) = mat3::inverse_and_determinant(&matrix);
        if !determinant.is_finite()
            || determinant.abs() < 1e-8
            || !inverse.iter().flatten().all(|value| value.is_finite())
        {
            return Err(Error::unsupported_color(
                "the camera matrix is singular, so sensor clipping cannot be read",
            ));
        }
        let ceiling = gains.map(|gain| match layout {
            SensorLayout::Bayer => f64::from(gain).min(BAYER_INPUT_CEILING),
            SensorLayout::XTrans | SensorLayout::LinearRgb | SensorLayout::Monochrome => {
                f64::from(gain)
            }
        });
        Ok(Self {
            camera_from_rgb: inverse,
            ceiling,
        })
    }

    /// The model of a decoded interpretation developed at `gains`. Refused for an unrecognized
    /// colour filter array.
    pub fn of_raw(metadata: &luxforge_raw::RawMetadata, gains: [f32; 3]) -> Result<Self, Error> {
        let layout = SensorLayout::of_raw(metadata).ok_or_else(|| {
            Error::unsupported_input("sensor clipping is unknown for this colour filter array")
        })?;
        let rgb_cam = metadata.rgb_cam.map(|row| [row[0], row[1], row[2]]);
        Self::new(rgb_cam, gains, layout)
    }

    /// Each camera channel's ceiling in developed units.
    pub fn ceiling(&self) -> [f64; 3] {
        self.ceiling
    }

    /// The fraction of its ceiling the largest camera channel of `rgb` reached.
    pub fn fraction(&self, rgb: [f32; 3]) -> f64 {
        let camera = mat3::matvec_f64(&self.camera_from_rgb, rgb.map(f64::from));
        (0..3)
            .map(|c| camera[c] / self.ceiling[c])
            .fold(f64::NEG_INFINITY, f64::max)
    }

    pub fn level(&self, rgb: [f32; 3]) -> SensorLevel {
        SensorLevel::from_fraction(self.fraction(rgb))
    }

    /// Write the levels of `width × height` pixels of three planes at row `stride` into `out`,
    /// at the same stride. Pixels between rows are left untouched.
    pub fn levels(
        &self,
        width: usize,
        height: usize,
        stride: usize,
        rgb: [&[f32]; 3],
        out: &mut [SensorLevel],
    ) -> Result<(), Error> {
        let need = view_len(width, height, stride)
            .filter(|_| stride >= width)
            .ok_or_else(|| Error::validation("sensor level stride is narrower than a row"))?;
        if rgb.iter().any(|plane| plane.len() < need) || out.len() < need {
            return Err(Error::validation(
                "sensor level slices are shorter than their dimensions",
            ));
        }
        for y in 0..height {
            for x in 0..width {
                let i = y * stride + x;
                out[i] = self.level([rgb[0][i], rgb[1][i], rgb[2][i]]);
            }
        }
        Ok(())
    }
}
