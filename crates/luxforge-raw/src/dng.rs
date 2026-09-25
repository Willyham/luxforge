//! Profile-selected DNG stage-3 corrections. Coordinates here are local to ActiveArea;
//! the public source still exposes full-sensor planes and absolute sensor crop.
//!
//! The numeric layout and mapping follow Adobe DNG 1.4 opcodes (DNG 1.7.1
//! specification, pp. 105-116) and the Adobe SDK's dng_gain_map.cpp and
//! dng_lens_correction.cpp. The SDK clips after both opcodes; Luxforge keeps
//! signed/highlight camera values until its terminal display conversion.

use crate::{PlanarRgb, RawError, RawRect, format::DngOpcode};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

// Use the process's shared pool only for photo-sized active areas. A row owns
// its output; stages and channels still join in order and reuse one warp plane.
const PARALLEL_CORRECTION_PIXELS: u64 = 1_000_000;

fn correction_rows(
    pixels: &mut [f32],
    width: usize,
    parallel: bool,
    cancel: &AtomicBool,
    apply: impl Fn(usize, &mut [f32]) -> Result<(), RawError> + Sync + Send,
) -> Result<(), RawError> {
    let row = |(y, pixels): (usize, &mut [f32])| {
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        apply(y, pixels)
    };
    if parallel {
        pixels
            .par_chunks_exact_mut(width)
            .enumerate()
            .try_for_each(row)?;
    } else {
        pixels
            .chunks_exact_mut(width)
            .enumerate()
            .try_for_each(row)?;
    }
    if cancel.load(Ordering::Relaxed) {
        Err(RawError::Cancelled)
    } else {
        Ok(())
    }
}

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
    stages: Vec<Stage3>,
    sensor_repair: Option<SensorRepair>,
}

#[derive(Debug, Clone)]
enum Stage3 {
    Gain(GainMap),
    Warp(Warp),
    Vignette(super::dng_ops::VignetteRadial),
}

#[derive(Debug, Clone)]
enum SensorRepair {
    Constant(u32, u32),
    Listed(super::dng_ops::BadPixels),
}

fn opcode_error(error: super::dng_ops::OpcodeError) -> RawError {
    use super::dng_ops::OpcodeError;
    match error {
        OpcodeError::Truncated => RawError::InvalidInput("truncated DNG correction"),
        OpcodeError::Invalid(message) => RawError::InvalidInput(message),
        OpcodeError::Unsupported(message) => RawError::UnsupportedMode(message.into()),
        OpcodeError::Resource(message) => RawError::ResourceLimit(message),
    }
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
            return Err(RawError::UnsupportedMode("DNG GainMap layout".into()));
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
                "DNG WarpRectilinear layout".into(),
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
        settings: &super::profiles::Dng,
    ) -> Result<Self, RawError> {
        if opcodes.iter().any(|op| op.ifd != raw_ifd) {
            return Err(RawError::InvalidInput("DNG opcode list outside raw IFD"));
        }
        let required: Vec<_> = opcodes.iter().filter(|op| op.flags & 1 == 0).collect();
        if required.len() != settings.required_opcodes.len()
            || required
                .iter()
                .zip(&settings.required_opcodes)
                .any(|(op, expected)| {
                    op.id != expected.id
                        || op.list != expected.list
                        || op.ifd != raw_ifd
                        || op.version != expected.version
                        || op.flags != expected.flags
                })
        {
            return Err(RawError::UnsupportedMode(
                "DNG opcode stage/order/version".into(),
            ));
        }
        let skipped_optional: Vec<_> = opcodes
            .iter()
            .filter(|op| op.flags & 1 != 0)
            .map(provenance)
            .collect();
        let mut stages = Vec::new();
        let mut sensor_repair = None;
        for op in &required {
            match (op.list, op.id) {
                (51022, 9) => stages.push(Stage3::Gain(GainMap::parse(&op.data, active)?)),
                (51022, 1) => stages.push(Stage3::Warp(Warp::parse(&op.data, active)?)),
                (51022, 3) => stages.push(Stage3::Vignette(
                    super::dng_ops::parse_vignette_radial(&op.data).map_err(opcode_error)?,
                )),
                (51008, 4 | 5) if sensor_repair.is_none() => {
                    sensor_repair = Some(if op.id == 4 {
                        let (constant, phase) = super::dng_ops::parse_bad_pixels_constant(&op.data)
                            .map_err(opcode_error)?;
                        SensorRepair::Constant(constant, phase)
                    } else {
                        SensorRepair::Listed(
                            super::dng_ops::parse_bad_pixels_list(&op.data)
                                .map_err(opcode_error)?,
                        )
                    });
                }
                _ => return Err(RawError::UnsupportedRequiredOpcodes(vec![op.id])),
            }
        }
        Ok(Self {
            active,
            stages,
            sensor_repair,
            metadata: DngCorrectionMetadata {
                interpretation: settings.interpretation.clone(),
                applied: required.into_iter().map(provenance).collect(),
                skipped_optional,
                calibration,
            },
        })
    }

    pub(crate) fn mosaic_corrections(
        &self,
        source: &[u16],
        width: usize,
        height: usize,
        cfa: &[u8],
    ) -> Result<(Vec<crate::MosaicCorrection>, usize), RawError> {
        let phase = match &self.sensor_repair {
            None => return Ok((Vec::new(), 0)),
            Some(SensorRepair::Constant(_, phase)) => *phase,
            Some(SensorRepair::Listed(list)) => list.bayer_phase,
        };
        let expected = match phase {
            0 => [0, 1, 1, 2],
            1 => [1, 0, 2, 1],
            2 => [1, 2, 0, 1],
            3 => [2, 1, 1, 0],
            _ => return Err(RawError::InvalidInput("DNG bad-pixel Bayer phase")),
        };
        if cfa != expected {
            return Err(RawError::InvalidInput(
                "DNG bad-pixel phase differs from sensor CFA",
            ));
        }
        let (patches, unresolved) = match &self.sensor_repair {
            None => return Ok((Vec::new(), 0)),
            Some(SensorRepair::Constant(value, phase)) => {
                super::dng_ops::bad_pixel_constant_replacements_with_unresolved(
                    source, width, height, *value, *phase,
                )
            }
            Some(SensorRepair::Listed(list)) => {
                list.replacements_with_unresolved(source, width, height)
            }
        }
        .map_err(opcode_error)?;
        Ok((
            patches
                .into_iter()
                .map(|(index, value)| crate::MosaicCorrection {
                    index: index as u32,
                    value,
                })
                .collect(),
            unresolved,
        ))
    }

    pub(crate) fn source_location(
        &self,
        x: u32,
        y: u32,
        channel: usize,
    ) -> Result<(f64, f64), RawError> {
        let mut point = (x as f64, y as f64);
        for stage in self.stages.iter().rev() {
            if let Stage3::Warp(warp) = stage {
                point = warp.source(point.0, point.1, channel, self.active)?;
            }
        }
        Ok(point)
    }
    /// Combined multiplier evaluated backwards from a corrected output point.
    /// This preserves whether each gain occurs before or after a spatial warp.
    pub(crate) fn gain_at(&self, x: f64, y: f64, channel: usize) -> Result<f64, RawError> {
        let mut point = (x, y);
        let mut gain = 1.0;
        for stage in self.stages.iter().rev() {
            match stage {
                Stage3::Gain(map) => gain *= map.gain(point.0, point.1, channel, self.active)?,
                Stage3::Warp(warp) => {
                    point = warp.source(point.0, point.1, channel, self.active)?
                }
                Stage3::Vignette(radial) => {
                    gain *= radial
                        .gain(
                            point.0 - self.active.x as f64,
                            point.1 - self.active.y as f64,
                            self.active.width as usize,
                            self.active.height as usize,
                        )
                        .map_err(opcode_error)?
                }
            }
        }
        Ok(gain)
    }

    pub(crate) fn apply(&self, rgb: &mut PlanarRgb, cancel: &AtomicBool) -> Result<(), RawError> {
        let parallel = u64::from(self.active.width) * u64::from(self.active.height)
            >= PARALLEL_CORRECTION_PIXELS;
        self.apply_rows(rgb, cancel, parallel)
    }

    fn apply_rows(
        &self,
        rgb: &mut PlanarRgb,
        cancel: &AtomicBool,
        parallel: bool,
    ) -> Result<(), RawError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        let width = rgb.width as usize;
        let n = rgb.plane_len();
        let area_w = self.active.width as usize;
        let area_h = self.active.height as usize;
        // One active-area plane is allocated lazily for the first nonidentity
        // warp, then reused across all channels/stages. Gain-only/no-op recipes
        // allocate no frame scratch. The parent frame's pixel limit bounds it.
        let mut scratch = Vec::new();
        for stage in &self.stages {
            for channel in 0..3 {
                let plane = &mut rgb.data[channel * n..(channel + 1) * n];
                match stage {
                    Stage3::Gain(_) | Stage3::Vignette(_) => {
                        let first = self.active.y as usize * width;
                        let rows = &mut plane[first..first + area_h * width];
                        correction_rows(rows, width, parallel, cancel, |yy, row| {
                            let y = self.active.y as usize + yy;
                            let left = self.active.x as usize;
                            for (xx, pixel) in row[left..left + area_w].iter_mut().enumerate() {
                                let x = self.active.x as usize + xx;
                                let gain = match stage {
                                    Stage3::Gain(map) => {
                                        map.gain(x as f64, y as f64, channel, self.active)?
                                    }
                                    Stage3::Vignette(radial) => radial
                                        .gain(xx as f64, yy as f64, area_w, area_h)
                                        .map_err(opcode_error)?,
                                    Stage3::Warp(_) => unreachable!(),
                                };
                                let value = *pixel as f64 * gain;
                                if !value.is_finite() || value.abs() > f32::MAX as f64 {
                                    return Err(RawError::InvalidInput("DNG gain output overflow"));
                                }
                                *pixel = value as f32;
                            }
                            Ok(())
                        })?;
                    }
                    Stage3::Warp(warp) => {
                        if warp.is_identity(channel) {
                            continue;
                        }
                        if scratch.is_empty() {
                            let len = area_w
                                .checked_mul(area_h)
                                .ok_or(RawError::ResourceLimit("DNG warp plane overflow"))?;
                            scratch.try_reserve_exact(len).map_err(|_| {
                                RawError::ResourceLimit("DNG warp scratch allocation")
                            })?;
                            scratch.resize(len, 0.0_f32);
                        }
                        correction_rows(&mut scratch, area_w, parallel, cancel, |yy, row| {
                            for (xx, pixel) in row.iter_mut().enumerate() {
                                let x = self.active.x + xx as u32;
                                let y = self.active.y + yy as u32;
                                let (sx, sy) =
                                    warp.source(x as f64, y as f64, channel, self.active)?;
                                let value = bicubic(plane, width, self.active, sx, sy);
                                if !value.is_finite() || value.abs() > f32::MAX as f64 {
                                    return Err(RawError::InvalidInput("DNG warp output overflow"));
                                }
                                *pixel = value as f32;
                            }
                            Ok(())
                        })?;
                        for yy in 0..area_h {
                            if cancel.load(Ordering::Relaxed) {
                                return Err(RawError::Cancelled);
                            }
                            let dst =
                                (self.active.y as usize + yy) * width + self.active.x as usize;
                            plane[dst..dst + area_w]
                                .copy_from_slice(&scratch[yy * area_w..(yy + 1) * area_w]);
                        }
                    }
                }
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

    fn row_fixture(width: u32, height: u32) -> (DngCorrection, PlanarRgb) {
        let active = RawRect {
            x: 3,
            y: 5,
            width,
            height,
        };
        let gain = GainMap {
            area: RawRect {
                x: 1,
                y: 2,
                width: width - 2,
                height: height - 4,
            },
            plane: 0,
            planes: 3,
            row_pitch: 2,
            col_pitch: 3,
            rows: 2,
            cols: 2,
            spacing: [0.5, 0.5],
            origin: [0.0, 0.0],
            map_planes: 3,
            values: vec![1.0, 1.2, 0.9, 2.0, 1.5, 1.0, 1.25, 0.5, 1.5, 3.0, 2.0, 0.75],
        };
        let warp = Warp {
            radial: [
                [0.91, 0.05, 0.0, 0.0],
                [1.0, 0.0, 0.0, 0.0],
                [1.03, 0.02, 0.01, 0.0],
            ],
            tangential: [[0.002, -0.001], [0.0, 0.0], [-0.003, 0.001]],
            center_pixels: [width as f64 * 0.45, height as f64 * 0.53],
            norm_radius: (width as f64).hypot(height as f64) / 2.0,
        };
        let correction = DngCorrection {
            active,
            metadata: DngCorrectionMetadata {
                interpretation: "row-test".into(),
                applied: vec![],
                skipped_optional: vec![],
                calibration: DngCalibrationMetadata {
                    illuminants: [17, 21],
                    color_matrix1_sha256: String::new(),
                    color_matrix2_sha256: String::new(),
                    selected: "test".into(),
                },
            },
            sensor_repair: None,
            stages: vec![
                Stage3::Gain(gain),
                Stage3::Warp(warp),
                Stage3::Vignette(super::super::dng_ops::VignetteRadial {
                    coefficients: [0.1, -0.02, 0.03, 0.0, 0.01],
                    center: [0.45, 0.53],
                }),
            ],
        };
        let width = width + 9;
        let height = height + 11;
        let data = (0..width as usize * height as usize * 3)
            .map(|i| ((i * 7919 % 65521) as f32 - 8192.0) / 16384.0)
            .collect();
        (
            correction,
            PlanarRgb {
                width,
                height,
                data,
            },
        )
    }

    #[test]
    fn correction_rows_match_serial_bits_and_preserve_sensor_borders() {
        let cancel = AtomicBool::new(false);
        for (width, height) in [(17, 13), (513, 257), (1001, 1003)] {
            let (mut correction, original) = row_fixture(width, height);
            // Exercise both sides of the stage-order boundary; each warp must
            // read a completed channel, including its untouched sensor border.
            for reverse in [false, true] {
                if reverse {
                    correction.stages.reverse();
                }
                let mut serial = PlanarRgb {
                    width: original.width,
                    height: original.height,
                    data: original.data.clone(),
                };
                let mut parallel = PlanarRgb {
                    width: original.width,
                    height: original.height,
                    data: original.data.clone(),
                };
                correction.apply_rows(&mut serial, &cancel, false).unwrap();
                correction.apply_rows(&mut parallel, &cancel, true).unwrap();
                for (i, (&a, &b)) in serial.data.iter().zip(&parallel.data).enumerate() {
                    assert_eq!(
                        a.to_bits(),
                        b.to_bits(),
                        "{width}x{height}, reverse={reverse}, value {i}"
                    );
                    let pixel = i % original.plane_len();
                    let x = pixel % original.width as usize;
                    let y = pixel / original.width as usize;
                    let active = correction.active;
                    if x < active.x as usize
                        || x >= (active.x + active.width) as usize
                        || y < active.y as usize
                        || y >= (active.y + active.height) as usize
                    {
                        assert_eq!(a.to_bits(), original.data[i].to_bits());
                    }
                }
                assert!(parallel.data.iter().any(|v| *v < 0.0));
                assert!(parallel.data.iter().any(|v| *v > 1.0));
            }
        }
    }

    #[test]
    fn correction_rows_report_cancellation_and_overflow() {
        for parallel in [false, true] {
            let cancel = AtomicBool::new(true);
            let (correction, mut pixels) = row_fixture(33, 19);
            let before = pixels.data.clone();
            assert_eq!(
                correction.apply_rows(&mut pixels, &cancel, parallel),
                Err(RawError::Cancelled)
            );
            assert_eq!(pixels.data, before);

            // A row that raises cancellation prevents later scheduled rows
            // from entering their pixel loop. Partially produced data is never
            // returned as a development.
            cancel.store(false, Ordering::Relaxed);
            let mut rows = vec![0.0; 1024];
            let result = correction_rows(&mut rows, 16, parallel, &cancel, |_, _| {
                cancel.store(true, Ordering::Relaxed);
                Ok(())
            });
            assert_eq!(result, Err(RawError::Cancelled));

            cancel.store(false, Ordering::Relaxed);
            pixels.data.fill(f32::MAX);
            assert_eq!(
                correction.apply_rows(&mut pixels, &cancel, parallel),
                Err(RawError::InvalidInput("DNG gain output overflow"))
            );
        }
    }

    #[test]
    #[ignore = "photo-sized serial/pool timing; run alone in release"]
    fn correction_row_parallel_threshold_timing() {
        use std::{hint::black_box, time::Instant};
        for (width, height) in [(128, 128), (512, 512), (1000, 1000), (2000, 1500)] {
            let (correction, mut pixels) = row_fixture(width, height);
            let original = pixels.data.clone();
            let cancel = AtomicBool::new(false);
            for parallel in [false, true, true, false] {
                let mut timings = Vec::new();
                for _ in 0..5 {
                    pixels.data.copy_from_slice(&original);
                    let start = Instant::now();
                    correction
                        .apply_rows(&mut pixels, &cancel, parallel)
                        .unwrap();
                    timings.push(start.elapsed().as_secs_f64() * 1000.0);
                    black_box(&pixels);
                }
                eprintln!(
                    "{}",
                    serde_json::json!({"width":width,"height":height,"parallel":parallel,"ms":timings})
                );
            }
        }
    }

    #[test]
    fn ordered_gain_and_warp_match_separate_stages_and_point_queries() {
        let active = RawRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        };
        let gain = GainMap {
            area: active,
            plane: 0,
            planes: 3,
            row_pitch: 1,
            col_pitch: 1,
            rows: 2,
            cols: 2,
            spacing: [1.0, 1.0],
            origin: [0.0, 0.0],
            map_planes: 3,
            values: vec![1.0, 1.0, 1.0, 3.0, 3.0, 3.0, 2.0, 2.0, 2.0, 4.0, 4.0, 4.0],
        };
        let warp = Warp {
            radial: [[0.8, 0.0, 0.0, 0.0]; 3],
            tangential: [[0.0; 2]; 3],
            center_pixels: [4.0, 4.0],
            norm_radius: 32.0_f64.sqrt(),
        };
        let metadata = DngCorrectionMetadata {
            interpretation: "synthetic".into(),
            applied: vec![],
            skipped_optional: vec![],
            calibration: DngCalibrationMetadata {
                illuminants: [17, 21],
                color_matrix1_sha256: String::new(),
                color_matrix2_sha256: String::new(),
                selected: "test".into(),
            },
        };
        let original: Vec<f32> = (0..64).map(|i| 1.0 + i as f32 / 64.0).collect();
        let cancel = AtomicBool::new(false);
        for gains_first in [true, false] {
            let correction = DngCorrection {
                active,
                metadata: metadata.clone(),
                sensor_repair: None,
                stages: if gains_first {
                    vec![Stage3::Gain(gain.clone()), Stage3::Warp(warp.clone())]
                } else {
                    vec![Stage3::Warp(warp.clone()), Stage3::Gain(gain.clone())]
                },
            };
            let mut image = PlanarRgb {
                width: 8,
                height: 8,
                data: original.repeat(3),
            };
            correction.apply(&mut image, &cancel).unwrap();
            let gained: Vec<f32> = original
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    (*v as f64
                        * gain
                            .gain((i % 8) as f64, (i / 8) as f64, 0, active)
                            .unwrap()) as f32
                })
                .collect();
            for y in 0..8 {
                for x in 0..8 {
                    let (sx, sy) = warp.source(x as f64, y as f64, 0, active).unwrap();
                    let expected = if gains_first {
                        bicubic(&gained, 8, active, sx, sy) as f32
                    } else {
                        (bicubic(&original, 8, active, sx, sy) as f32 as f64
                            * gain.gain(x as f64, y as f64, 0, active).unwrap())
                            as f32
                    };
                    assert_eq!(image.data[y * 8 + x], expected);
                    assert_eq!(
                        correction.source_location(x as u32, y as u32, 0).unwrap(),
                        (sx, sy)
                    );
                    let expected_gain = if gains_first {
                        gain.gain(sx, sy, 0, active)
                    } else {
                        gain.gain(x as f64, y as f64, 0, active)
                    }
                    .unwrap();
                    assert_eq!(
                        correction.gain_at(x as f64, y as f64, 0).unwrap(),
                        expected_gain
                    );
                }
            }
        }
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
