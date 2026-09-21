//! FC3411 DNG stage-3 corrections. Coordinates here are local to ActiveArea;
//! the public source still exposes full-sensor planes and absolute sensor crop.
//!
//! The numeric layout and mapping follow Adobe DNG 1.4 opcodes (DNG 1.7.1
//! specification, pp. 105-116) and the Adobe SDK's dng_gain_map.cpp and
//! dng_lens_correction.cpp. The SDK clips after both opcodes; Lightwell keeps
//! signed/highlight camera values until its terminal display conversion.

use crate::{PlanarRgb, RawError, RawRect, format::DngOpcode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DngOpcodeProvenance {
    pub list: u16,
    pub id: u32,
    pub version: u32,
    pub flags: u32,
    pub payload_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DngCorrectionMetadata {
    /// Changes to math, interpolation, coordinate or clipping policy change
    /// this identity and make an older catalog interpretation explicit.
    pub interpretation: String,
    pub applied: Vec<DngOpcodeProvenance>,
    pub skipped_optional: Vec<DngOpcodeProvenance>,
    pub calibration: DngCalibrationMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DngCalibrationMetadata {
    pub illuminants: [u16; 2],
    pub color_matrix1_sha256: String,
    pub color_matrix2_sha256: String,
    pub selected: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DngCorrection {
    pub metadata: DngCorrectionMetadata,
    active: RawRect,
    gain: GainMap,
    warp: Warp,
}

#[derive(Debug, Clone)]
struct GainMap {
    area: RawRect,
    plane: u32,
    planes: u32,
    row_pitch: u32,
    col_pitch: u32,
    rows: usize,
    cols: usize,
    spacing: [f64; 2], // vertical, horizontal
    origin: [f64; 2],
    map_planes: usize,
    values: Vec<f32>,
}

#[derive(Debug, Clone)]
struct Warp {
    radial: [[f64; 4]; 3],
    tangential: [[f64; 2]; 3],
    center_pixels: [f64; 2],
    norm_radius: f64,
}

fn be_u32(bytes: &[u8], p: usize) -> Result<u32, RawError> {
    let a: [u8; 4] = bytes
        .get(p..p + 4)
        .ok_or(RawError::InvalidInput("truncated DNG opcode"))?
        .try_into()
        .unwrap();
    Ok(u32::from_be_bytes(a))
}
fn be_f64(bytes: &[u8], p: usize) -> Result<f64, RawError> {
    let a: [u8; 8] = bytes
        .get(p..p + 8)
        .ok_or(RawError::InvalidInput("truncated DNG opcode"))?
        .try_into()
        .unwrap();
    Ok(f64::from_bits(u64::from_be_bytes(a)))
}
fn checked_rect(
    top: u32,
    left: u32,
    bottom: u32,
    right: u32,
    width: u32,
    height: u32,
) -> Result<RawRect, RawError> {
    if top >= bottom || left >= right || bottom > height || right > width {
        return Err(RawError::InvalidInput(
            "DNG opcode area outside active image",
        ));
    }
    Ok(RawRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

impl GainMap {
    fn parse(data: &[u8], active: RawRect) -> Result<Self, RawError> {
        // AreaSpec: t,l,b,r,plane,planes,rowPitch,colPitch (8 x uint32).
        // GainMap: pointsV/H, spacingV/H, originV/H, mapPlanes, float entries.
        if data.len() < 76 {
            return Err(RawError::InvalidInput("DNG GainMap size"));
        }
        let area = checked_rect(
            be_u32(data, 0)?,
            be_u32(data, 4)?,
            be_u32(data, 8)?,
            be_u32(data, 12)?,
            active.width,
            active.height,
        )?;
        let plane = be_u32(data, 16)?;
        let planes = be_u32(data, 20)?;
        let row_pitch = be_u32(data, 24)?;
        let col_pitch = be_u32(data, 28)?;
        let rows = be_u32(data, 32)? as usize;
        let cols = be_u32(data, 36)? as usize;
        let spacing = [be_f64(data, 40)?, be_f64(data, 48)?];
        let origin = [be_f64(data, 56)?, be_f64(data, 64)?];
        let map_planes = be_u32(data, 72)? as usize;
        if plane != 0
            || planes != 3
            || row_pitch != 1
            || col_pitch != 1
            || rows == 0
            || cols == 0
            || rows > 256
            || cols > 256
            || map_planes != 3
            || !spacing.iter().all(|v| v.is_finite() && *v > 0.0)
            || !origin
                .iter()
                .all(|v| v.is_finite() && *v >= -1.0 && *v <= 2.0)
        {
            return Err(RawError::UnsupportedMode(
                "FC3411 DNG GainMap layout".into(),
            ));
        }
        let entries = rows
            .checked_mul(cols)
            .and_then(|v| v.checked_mul(map_planes))
            .ok_or(RawError::ResourceLimit("DNG GainMap entries"))?;
        if entries > 256 * 256 * 3 || data.len() != 76 + entries * 4 {
            return Err(RawError::InvalidInput("DNG GainMap payload length"));
        }
        let mut values = Vec::new();
        values
            .try_reserve_exact(entries)
            .map_err(|_| RawError::ResourceLimit("DNG GainMap allocation"))?;
        for chunk in data[76..].chunks_exact(4) {
            let value = f32::from_bits(u32::from_be_bytes(chunk.try_into().unwrap()));
            if !value.is_finite() || value <= 0.0 || value > 64.0 {
                return Err(RawError::InvalidInput(
                    "DNG GainMap nonpositive/nonfinite gain",
                ));
            }
            values.push(value);
        }
        Ok(Self {
            area,
            plane,
            planes,
            row_pitch,
            col_pitch,
            rows,
            cols,
            spacing,
            origin,
            map_planes,
            values,
        })
    }

    fn gain(&self, x: f64, y: f64, channel: usize, active: RawRect) -> Result<f64, RawError> {
        if channel >= 3 || !x.is_finite() || !y.is_finite() {
            return Err(RawError::InvalidInput("DNG GainMap sample"));
        }
        let lx = x - active.x as f64;
        let ly = y - active.y as f64;
        if lx < 0.0 || ly < 0.0 || lx >= active.width as f64 || ly >= active.height as f64 {
            return Err(RawError::InvalidInput(
                "DNG GainMap point outside active image",
            ));
        }
        if lx < self.area.x as f64
            || ly < self.area.y as f64
            || lx >= (self.area.x + self.area.width) as f64
            || ly >= (self.area.y + self.area.height) as f64
            || channel < self.plane as usize
            || channel >= (self.plane + self.planes) as usize
            || !(ly as u32 - self.area.y).is_multiple_of(self.row_pitch)
            || !(lx as u32 - self.area.x).is_multiple_of(self.col_pitch)
        {
            return Ok(1.0);
        }
        // DNG map coordinates are normalized over the entire stage image,
        // including the 0.5 pixel-center offset. Clamp at map borders.
        let fy = (((ly + 0.5) / active.height as f64) - self.origin[0]) / self.spacing[0];
        let fx = (((lx + 0.5) / active.width as f64) - self.origin[1]) / self.spacing[1];
        let fy = fy.clamp(0.0, (self.rows - 1) as f64);
        let fx = fx.clamp(0.0, (self.cols - 1) as f64);
        let y0 = fy.floor() as usize;
        let x0 = fx.floor() as usize;
        let y1 = (y0 + 1).min(self.rows - 1);
        let x1 = (x0 + 1).min(self.cols - 1);
        let wy = fy - y0 as f64;
        let wx = fx - x0 as f64;
        let at = |row: usize, col: usize| -> f64 {
            self.values[(row * self.cols + col) * self.map_planes + channel] as f64
        };
        Ok((at(y0, x0) * (1.0 - wx) + at(y0, x1) * wx) * (1.0 - wy)
            + (at(y1, x0) * (1.0 - wx) + at(y1, x1) * wx) * wy)
    }
}

impl Warp {
    fn is_identity(&self, channel: usize) -> bool {
        self.radial[channel] == [1.0, 0.0, 0.0, 0.0] && self.tangential[channel] == [0.0, 0.0]
    }

    fn parse(data: &[u8], active: RawRect) -> Result<Self, RawError> {
        if data.len() != 164 || be_u32(data, 0)? != 3 {
            return Err(RawError::UnsupportedMode(
                "FC3411 DNG WarpRectilinear layout".into(),
            ));
        }
        let mut radial = [[0.0; 4]; 3];
        let mut tangential = [[0.0; 2]; 3];
        for plane in 0..3 {
            let p = 4 + plane * 48;
            for (i, v) in radial[plane].iter_mut().enumerate() {
                *v = be_f64(data, p + i * 8)?;
            }
            for (i, v) in tangential[plane].iter_mut().enumerate() {
                *v = be_f64(data, p + 32 + i * 8)?;
            }
        }
        let center = [be_f64(data, 148)?, be_f64(data, 156)?];
        if !center
            .iter()
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            || !radial
                .iter()
                .flatten()
                .all(|v| v.is_finite() && v.abs() <= 16.0)
            || !tangential
                .iter()
                .flatten()
                .all(|v| v.is_finite() && v.abs() <= 16.0)
        {
            return Err(RawError::InvalidInput("DNG WarpRectilinear coefficient"));
        }
        let cx = active.width as f64 * center[0];
        let cy = active.height as f64 * center[1];
        let norm_radius = cx
            .hypot(cy)
            .max((active.width as f64 - cx).hypot(cy))
            .max(cx.hypot(active.height as f64 - cy))
            .max((active.width as f64 - cx).hypot(active.height as f64 - cy));
        if !norm_radius.is_finite() || norm_radius <= 0.0 {
            return Err(RawError::InvalidInput("DNG warp radius"));
        }
        let warp = Self {
            radial,
            tangential,
            center_pixels: [cx, cy],
            norm_radius,
        };
        warp.validate(active)?;
        Ok(warp)
    }

    fn validate(&self, active: RawRect) -> Result<(), RawError> {
        // A finite coefficient list can still collapse the entire image or
        // fold geometry. Check radial monotonicity, then a bounded two-axis
        // Jacobian grid including corners for tangential distortions.
        for channel in 0..3 {
            let k = self.radial[channel];
            for i in 0..=256 {
                let r2 = i as f64 / 256.0;
                let derivative = k[0] + r2 * (3.0 * k[1] + r2 * (5.0 * k[2] + r2 * 7.0 * k[3]));
                if !derivative.is_finite() || derivative <= 0.01 {
                    return Err(RawError::InvalidInput("DNG warp nonmonotonic radial map"));
                }
            }
            for gy in 0..=16 {
                for gx in 0..=16 {
                    let x = active.x as f64 + (active.width as f64 - 1.0) * gx as f64 / 16.0;
                    let y = active.y as f64 + (active.height as f64 - 1.0) * gy as f64 / 16.0;
                    let (x0, y0) = self.source(x, y, channel, active)?;
                    let (xx, yx) = self.source(x + 0.01, y, channel, active)?;
                    let (xy, yy) = self.source(x, y + 0.01, channel, active)?;
                    let det = ((xx - x0) * (yy - y0) - (xy - x0) * (yx - y0)) / 0.0001;
                    let dx_dx = (xx - x0) / 0.01;
                    let dy_dy = (yy - y0) / 0.01;
                    if !det.is_finite()
                        || det <= 0.01
                        || !dx_dx.is_finite()
                        || dx_dx <= 0.01
                        || !dy_dy.is_finite()
                        || dy_dy <= 0.01
                    {
                        return Err(RawError::InvalidInput("DNG warp folded geometry"));
                    }
                }
            }
        }
        Ok(())
    }

    fn source(
        &self,
        x: f64,
        y: f64,
        channel: usize,
        active: RawRect,
    ) -> Result<(f64, f64), RawError> {
        if channel >= 3 || !x.is_finite() || !y.is_finite() {
            return Err(RawError::InvalidInput("DNG WarpRectilinear point"));
        }
        let lx = x - active.x as f64;
        let ly = y - active.y as f64;
        if lx < -0.1
            || ly < -0.1
            || lx > active.width as f64 + 0.1
            || ly > active.height as f64 + 0.1
        {
            return Err(RawError::InvalidInput(
                "DNG warp point outside active image",
            ));
        }
        // The exact identity payload must map to exact pixel centers. A
        // subtract/divide/multiply sequence otherwise rounds an integer just
        // below itself and selects the preceding 1/128 bicubic phase.
        if self.is_identity(channel) {
            return Ok((x, y));
        }
        let [cx, cy] = self.center_pixels;
        let norm = self.norm_radius;
        let dx = lx - cx;
        let dy = ly - cy;
        let nx = dx / norm;
        let ny = dy / norm;
        let r2 = (nx * nx + ny * ny).min(1.0);
        let k = self.radial[channel];
        let ratio = k[0] + r2 * (k[1] + r2 * (k[2] + r2 * k[3]));
        let t = self.tangential[channel];
        let tx = t[1] * (r2 + 2.0 * nx * nx) + 2.0 * t[0] * nx * ny;
        let ty = t[0] * (r2 + 2.0 * ny * ny) + 2.0 * t[1] * nx * ny;
        let sx = cx + norm * (nx * ratio + tx);
        let sy = cy + norm * (ny * ratio + ty);
        if !sx.is_finite() || !sy.is_finite() || sx.abs() > 1_000_000.0 || sy.abs() > 1_000_000.0 {
            return Err(RawError::InvalidInput("DNG warp mapped point overflow"));
        }
        Ok((sx + active.x as f64, sy + active.y as f64))
    }
}

fn provenance(opcode: &DngOpcode) -> DngOpcodeProvenance {
    DngOpcodeProvenance {
        list: opcode.list,
        id: opcode.id,
        version: opcode.version,
        flags: opcode.flags,
        payload_sha256: format!("{:x}", Sha256::digest(&opcode.data)),
    }
}

impl DngCorrection {
    pub(crate) fn parse(
        opcodes: &[DngOpcode],
        active: RawRect,
        raw_ifd: u32,
        calibration: DngCalibrationMetadata,
    ) -> Result<Self, RawError> {
        if opcodes.iter().any(|op| op.ifd != raw_ifd) {
            return Err(RawError::InvalidInput("DNG opcode list outside raw IFD"));
        }
        let required: Vec<_> = opcodes.iter().filter(|op| op.flags & 1 == 0).collect();
        if required.len() != 2
            || required[0].id != 9
            || required[1].id != 1
            || required.iter().any(|op| {
                op.list != 51022 || op.ifd != raw_ifd || op.version != 0x0103_0000 || op.flags != 0
            })
        {
            return Err(RawError::UnsupportedMode(
                "FC3411 DNG opcode stage/order/version".into(),
            ));
        }
        let skipped_optional: Vec<_> = opcodes
            .iter()
            .filter(|op| op.flags & 1 != 0)
            .map(provenance)
            .collect();
        let gain = GainMap::parse(&required[0].data, active)?;
        let warp = Warp::parse(&required[1].data, active)?;
        Ok(Self {
            active,
            gain,
            warp,
            metadata: DngCorrectionMetadata {
                interpretation: "fc3411-stage3-active-v1-unclipped-bicubic-a-0.75".into(),
                applied: required.into_iter().map(provenance).collect(),
                skipped_optional,
                calibration,
            },
        })
    }

    pub(crate) fn source_location(
        &self,
        x: u32,
        y: u32,
        channel: usize,
    ) -> Result<(f64, f64), RawError> {
        self.warp.source(x as f64, y as f64, channel, self.active)
    }
    pub(crate) fn gain_at(&self, x: f64, y: f64, channel: usize) -> Result<f64, RawError> {
        self.gain.gain(x, y, channel, self.active)
    }

    pub(crate) fn apply(&self, rgb: &mut PlanarRgb, cancel: &AtomicBool) -> Result<(), RawError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        let width = rgb.width as usize;
        let n = rgb.plane_len();
        let area_w = self.active.width as usize;
        let area_h = self.active.height as usize;
        let scratch_len = area_w
            .checked_mul(area_h)
            .ok_or(RawError::ResourceLimit("DNG warp plane overflow"))?;
        let mut scratch = Vec::new();
        scratch
            .try_reserve_exact(scratch_len)
            .map_err(|_| RawError::ResourceLimit("DNG warp scratch allocation"))?;
        scratch.resize(scratch_len, 0.0_f32);
        for channel in 0..3 {
            let plane = &mut rgb.data[channel * n..(channel + 1) * n];
            // OpcodeList3 order: gain the existing camera plane in place.
            for yy in 0..area_h {
                if yy & 63 == 0 && cancel.load(Ordering::Relaxed) {
                    return Err(RawError::Cancelled);
                }
                let y = self.active.y as usize + yy;
                for xx in 0..area_w {
                    let x = self.active.x as usize + xx;
                    let gain = self.gain_at(x as f64, y as f64, channel)?;
                    let idx = y * width + x;
                    let value = plane[idx] as f64 * gain;
                    if !value.is_finite() || value.abs() > f32::MAX as f64 {
                        return Err(RawError::InvalidInput("DNG gain output overflow"));
                    }
                    plane[idx] = value as f32;
                }
            }
            if self.warp.is_identity(channel) {
                continue;
            }
            // Warp into one active-area plane, then reuse this allocation for
            // the next channel. No second full RGB frame is ever live.
            for yy in 0..area_h {
                if yy & 63 == 0 && cancel.load(Ordering::Relaxed) {
                    return Err(RawError::Cancelled);
                }
                for xx in 0..area_w {
                    let x = self.active.x + xx as u32;
                    let y = self.active.y + yy as u32;
                    let (sx, sy) = self.source_location(x, y, channel)?;
                    let value = bicubic(plane, width, self.active, sx, sy);
                    if !value.is_finite() || value.abs() > f32::MAX as f64 {
                        return Err(RawError::InvalidInput("DNG warp output overflow"));
                    }
                    scratch[yy * area_w + xx] = value as f32;
                }
            }
            for yy in 0..area_h {
                let dst = (self.active.y as usize + yy) * width + self.active.x as usize;
                plane[dst..dst + area_w].copy_from_slice(&scratch[yy * area_w..(yy + 1) * area_w]);
            }
        }
        Ok(())
    }
}

fn cubic(x: f64) -> f64 {
    let x = x.abs();
    let a = -0.75;
    if x >= 2.0 {
        0.0
    } else if x >= 1.0 {
        ((a * x - 5.0 * a) * x + 8.0 * a) * x - 4.0 * a
    } else {
        ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0
    }
}

fn cubic_weights() -> &'static [[f64; 4]; 128] {
    static WEIGHTS: OnceLock<[[f64; 4]; 128]> = OnceLock::new();
    WEIGHTS.get_or_init(|| {
        std::array::from_fn(|phase| {
            let fract = phase as f64 / 128.0;
            let mut w: [f64; 4] = std::array::from_fn(|tap| cubic(tap as f64 - 1.0 - fract));
            let sum: f64 = w.iter().sum();
            for value in &mut w {
                *value /= sum;
            }
            w
        })
    })
}

fn bicubic(plane: &[f32], width: usize, active: RawRect, x: f64, y: f64) -> f64 {
    // Adobe's warp filter quantizes each fractional coordinate to 1/128 and
    // uses a separable Keys cubic, A=-0.75. Replicate edge samples beyond the
    // active image, the same image-edge extension used by its tile filter.
    let lx = x - active.x as f64;
    let ly = y - active.y as f64;
    let ix = lx.floor();
    let iy = ly.floor();
    let fx = ((lx - ix) * 128.0).floor().clamp(0.0, 127.0) as usize;
    let fy = ((ly - iy) * 128.0).floor().clamp(0.0, 127.0) as usize;
    let wx = &cubic_weights()[fx];
    let wy = &cubic_weights()[fy];
    let mut sum = 0.0;
    for (ky, &y_weight) in wy.iter().enumerate() {
        let py = ((iy as i64 + ky as i64 - 1).clamp(0, active.height as i64 - 1) as usize)
            + active.y as usize;
        for (kx, &x_weight) in wx.iter().enumerate() {
            let px = ((ix as i64 + kx as i64 - 1).clamp(0, active.width as i64 - 1) as usize)
                + active.x as usize;
            let weight = y_weight * x_weight;
            sum += weight * plane[py * width + px] as f64;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn reference() -> Value {
        serde_json::from_str(include_str!("../../../probes/raw/dng_reference.json")).unwrap()
    }
    #[test]
    fn identity_warp_and_cubic_sampling() {
        let warp = Warp {
            radial: [[1.0, 0.0, 0.0, 0.0]; 3],
            tangential: [[0.0; 2]; 3],
            center_pixels: [2.5, 2.0],
            norm_radius: 3.2015621187164243,
        };
        let active = RawRect {
            x: 2,
            y: 3,
            width: 5,
            height: 4,
        };
        assert_eq!(warp.source(4.0, 5.0, 0, active).unwrap(), (4.0, 5.0));
        let pixels: Vec<_> = (0..80).map(|v| v as f32).collect();
        assert!((bicubic(&pixels, 10, active, 4.0, 5.0) - 54.0).abs() < 1e-12);
    }

    #[test]
    fn gain_map_interpolates_pixel_centers_and_channels() {
        let active = RawRect {
            x: 10,
            y: 20,
            width: 4,
            height: 4,
        };
        let map = GainMap {
            area: RawRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            },
            plane: 0,
            planes: 3,
            row_pitch: 1,
            col_pitch: 1,
            rows: 2,
            cols: 2,
            spacing: [0.5, 0.5],
            origin: [0.0, 0.0],
            map_planes: 3,
            values: vec![1.0, 2.0, 3.0, 2.0, 3.0, 4.0, 3.0, 4.0, 5.0, 4.0, 5.0, 6.0],
        };
        // Pixel (10,20) is at normalized (.125,.125), one-quarter between
        // first and second map knots in both axes.
        assert!((map.gain(10.0, 20.0, 0, active).unwrap() - 1.75).abs() < 1e-12);
        assert!((map.gain(10.0, 20.0, 2, active).unwrap() - 3.75).abs() < 1e-12);
        assert_eq!(map.gain(13.0, 23.0, 1, active).unwrap(), 5.0);
    }

    #[test]
    fn chromatic_warp_and_folded_coefficients() {
        let active = RawRect {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        };
        let mut payload = vec![0_u8; 164];
        payload[..4].copy_from_slice(&3_u32.to_be_bytes());
        for plane in 0..3 {
            payload[4 + plane * 48..12 + plane * 48]
                .copy_from_slice(&1.0_f64.to_bits().to_be_bytes());
        }
        payload[148..156].copy_from_slice(&0.5_f64.to_bits().to_be_bytes());
        payload[156..164].copy_from_slice(&0.5_f64.to_bits().to_be_bytes());
        let identity = Warp::parse(&payload, active).unwrap();
        assert_eq!(identity.source(3.0, 2.0, 2, active).unwrap(), (3.0, 2.0));
        payload[12..20].copy_from_slice(&0.1_f64.to_bits().to_be_bytes());
        let chromatic = Warp::parse(&payload, active).unwrap();
        assert!((chromatic.source(3.0, 2.0, 0, active).unwrap().0 - 3.0125).abs() < 1e-12);
        assert_eq!(chromatic.source(3.0, 2.0, 1, active).unwrap(), (3.0, 2.0));
        payload[4..12].copy_from_slice(&0.0_f64.to_bits().to_be_bytes());
        assert!(matches!(
            Warp::parse(&payload, active),
            Err(RawError::InvalidInput(_))
        ));
        payload[4..12].copy_from_slice(&1.0_f64.to_bits().to_be_bytes());
        payload[12..20].copy_from_slice(&f64::NAN.to_bits().to_be_bytes());
        assert!(matches!(
            Warp::parse(&payload, active),
            Err(RawError::InvalidInput(_))
        ));
    }

    #[test]
    fn independent_gain_and_bicubic_reference_vectors() {
        let vectors = reference();
        let gain = &vectors["reference_samples"]["synthetic_gain_square_nonzero_origin"];
        let bounds = gain["bounds_tlbr"].as_array().unwrap();
        let active = RawRect {
            x: bounds[1].as_u64().unwrap() as u32,
            y: bounds[0].as_u64().unwrap() as u32,
            width: (bounds[3].as_u64().unwrap() - bounds[1].as_u64().unwrap()) as u32,
            height: (bounds[2].as_u64().unwrap() - bounds[0].as_u64().unwrap()) as u32,
        };
        let map = GainMap {
            area: RawRect {
                x: 0,
                y: 0,
                width: active.width,
                height: active.height,
            },
            plane: 0,
            planes: 3,
            row_pitch: 1,
            col_pitch: 1,
            rows: 2,
            cols: 2,
            spacing: [0.5, 0.5],
            origin: [0.0, 0.0],
            map_planes: 3,
            values: vec![1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 3.0, 3.0, 3.0, 4.0, 4.0, 4.0],
        };
        for sample in gain["samples"].as_array().unwrap() {
            let x = sample["col"].as_f64().unwrap();
            let y = sample["row"].as_f64().unwrap();
            let expected = sample["value"].as_f64().unwrap();
            for channel in 0..3 {
                assert!((map.gain(x, y, channel, active).unwrap() - expected).abs() < 1e-12);
            }
        }

        let cubic = &vectors["reference_samples"]["synthetic_bicubic_headroom"];
        let source = cubic["source_4x4"].as_array().unwrap();
        let pixels = source
            .iter()
            .flat_map(|row| {
                row.as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_f64().unwrap() as f32)
            })
            .collect::<Vec<_>>();
        let square = RawRect {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        };
        let expected = cubic["unclipped_bicubic_value"].as_f64().unwrap();
        assert!((bicubic(&pixels, 4, square, 1.5, 1.5) - expected).abs() < 5e-8);
        assert_eq!(
            pixels[0] as f64,
            cubic["negative_scalar_unclipped"].as_f64().unwrap()
        );
        assert!(expected > 1.0);
    }

    #[test]
    fn independent_chromatic_warp_reference_vectors() {
        let vectors = reference();
        let coeffs = vectors["opcodes"][1]["coefficients"].as_array().unwrap();
        let mut payload = vec![0_u8; 164];
        payload[..4].copy_from_slice(&3_u32.to_be_bytes());
        for (plane, values) in coeffs.iter().enumerate() {
            for (i, value) in values.as_array().unwrap().iter().enumerate() {
                let p = 4 + 48 * plane + 8 * i;
                payload[p..p + 8].copy_from_slice(&value.as_f64().unwrap().to_bits().to_be_bytes());
            }
        }
        for (i, value) in vectors["opcodes"][1]["center"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let p = 148 + i * 8;
            payload[p..p + 8].copy_from_slice(&value.as_f64().unwrap().to_bits().to_be_bytes());
        }
        let active = RawRect {
            x: 96,
            y: 0,
            width: 5472,
            height: 3648,
        };
        let warp = Warp::parse(&payload, active).unwrap();
        assert!(warp.is_identity(1));
        assert_eq!(warp.source(100.0, 4.0, 1, active).unwrap(), (100.0, 4.0));
        assert!(!warp.is_identity(0));
        assert!(!warp.is_identity(2));
        for sample in vectors["reference_samples"]["warp_source"]
            .as_array()
            .unwrap()
        {
            let x = sample["col"].as_f64().unwrap() + active.x as f64;
            let y = sample["row"].as_f64().unwrap();
            let channel = sample["plane"].as_u64().unwrap() as usize;
            let expected_y = sample["value"][0].as_f64().unwrap();
            let expected_x = sample["value"][1].as_f64().unwrap() + active.x as f64;
            let (actual_x, actual_y) = warp.source(x, y, channel, active).unwrap();
            assert!(
                (actual_x - expected_x).abs() < 1e-8,
                "x {actual_x} vs {expected_x}"
            );
            assert!(
                (actual_y - expected_y).abs() < 1e-8,
                "y {actual_y} vs {expected_y}"
            );
        }
        let offcenter = &vectors["reference_samples"]["warp_noncentral_center"];
        let center = offcenter["center_xy"].as_array().unwrap();
        for (i, value) in center.iter().enumerate() {
            let p = 148 + i * 8;
            payload[p..p + 8].copy_from_slice(&value.as_f64().unwrap().to_bits().to_be_bytes());
        }
        let offcenter_warp = Warp::parse(&payload, active).unwrap();
        let x = offcenter["col"].as_f64().unwrap() + active.x as f64;
        let y = offcenter["row"].as_f64().unwrap();
        let (actual_x, actual_y) = offcenter_warp.source(x, y, 0, active).unwrap();
        let expected_x = offcenter["value"][1].as_f64().unwrap() + active.x as f64;
        let expected_y = offcenter["value"][0].as_f64().unwrap();
        assert!((actual_x - expected_x).abs() < 1e-8);
        assert!((actual_y - expected_y).abs() < 1e-8);
    }
}
