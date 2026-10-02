//! Profile-selected DNG stage-3 corrections. Coordinates here are local to ActiveArea;
//! the public source still exposes full-sensor planes and absolute sensor crop.
//!
//! The numeric layout and mapping follow Adobe DNG 1.4 opcodes (DNG 1.7.1
//! specification, pp. 105-116) and the Adobe SDK's dng_gain_map.cpp,
//! dng_lens_correction.cpp and dng_bad_pixels.cpp. The SDK clips after both
//! opcodes; Luxforge keeps signed/highlight camera values until its terminal
//! display conversion. Unknown or unsupported layouts fail.

use crate::{
    PlanarRgb, RawError, RawRect,
    format::{DngOpcode, Endian, f64_at, u32_at},
    native_tiles,
    opcodes::Opcode,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

/// Rows in one correction job: about 88 thousand pixels on the Air 2S's active area, a few
/// hundred microseconds of warp for one channel, so a pool thread that steals one returns to its
/// own work within about the time of a native tile job.
const CORRECTION_JOB_ROWS: usize = 16;

/// Apply `apply(y, row)` to every `width`-pixel row of `pixels`, in jobs of
/// [`CORRECTION_JOB_ROWS`] rows on the development executor with `lanes` at once. Cancellation is
/// checked before every row; the first error or cancellation stops the queue and is returned
/// once every started job has joined.
fn correction_rows(
    pixels: &mut [f32],
    width: usize,
    lanes: usize,
    cancel: &AtomicBool,
    apply: impl Fn(usize, &mut [f32]) -> Result<(), RawError> + Sync,
) -> Result<(), RawError> {
    let jobs = pixels.chunks_mut(width * CORRECTION_JOB_ROWS).enumerate();
    native_tiles::refill_each(lanes, jobs, |(job, rows)| {
        for (index, row) in rows.chunks_exact_mut(width).enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(RawError::Cancelled);
            }
            apply(job * CORRECTION_JOB_ROWS + index, row)?;
        }
        Ok(())
    })?;
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
    repair_active_only: bool,
    sensor_gains: Vec<GainMap>,
    sensor_vignette: Option<VignetteRadial>,
}

#[derive(Debug, Clone)]
enum Stage3 {
    Gain(GainMap),
    Warp(Warp),
    Vignette(VignetteRadial),
}

#[derive(Debug, Clone)]
enum SensorRepair {
    Constant(u32, u32),
    Listed(BadPixels),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WarpRole {
    Distortion,
    LateralCa,
    Identity,
}

/// A big-endian opcode field, through the crate's one reader set.
fn be_u32(bytes: &[u8], p: usize) -> Result<u32, RawError> {
    u32_at(bytes, p, Endian::Big).ok_or(RawError::InvalidInput("truncated DNG opcode"))
}
fn be_i32(bytes: &[u8], p: usize) -> Result<i32, RawError> {
    Ok(be_u32(bytes, p)? as i32)
}
fn be_f64(bytes: &[u8], p: usize) -> Result<f64, RawError> {
    f64_at(bytes, p, Endian::Big).ok_or(RawError::InvalidInput("truncated DNG opcode"))
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
        Self::parse_for(data, active, false)
    }

    fn parse_for(data: &[u8], active: RawRect, sensor: bool) -> Result<Self, RawError> {
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
            || if sensor {
                planes != 1
                    || map_planes != 1
                    || !(1..=2).contains(&row_pitch)
                    || !(1..=2).contains(&col_pitch)
            } else {
                planes != 3 || map_planes != 3 || row_pitch != 1 || col_pitch != 1
            }
            || rows == 0
            || cols == 0
            || rows > 256
            || cols > 256
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
        if !self.covers(channel) {
            return Ok(1.0);
        }
        let (Some(row), Some(column)) = (self.row_taps(ly, active), self.column_taps(lx, active))
        else {
            return Ok(1.0);
        };
        Ok(self.interpolate(row, column, channel))
    }

    /// Whether the map corrects `channel`; every other channel keeps a gain of 1.0.
    fn covers(&self, channel: usize) -> bool {
        channel >= self.plane as usize && channel < (self.plane + self.planes) as usize
    }

    /// The map rows that weigh active-local pixel row `ly`, or `None` where the map leaves that
    /// row at a gain of 1.0. It depends on the row alone.
    fn row_taps(&self, ly: f64, active: RawRect) -> Option<Taps> {
        if ly < self.area.y as f64
            || ly >= (self.area.y + self.area.height) as f64
            || !(ly as u32 - self.area.y).is_multiple_of(self.row_pitch)
        {
            return None;
        }
        // DNG map coordinates are normalized over the entire stage image,
        // including the 0.5 pixel-center offset. Clamp at map borders.
        let fy = (((ly + 0.5) / active.height as f64) - self.origin[0]) / self.spacing[0];
        Some(Taps::clamped(fy, self.rows))
    }

    /// The map columns that weigh active-local pixel column `lx`, or `None` where the map leaves
    /// that column at a gain of 1.0. It depends on the column alone.
    fn column_taps(&self, lx: f64, active: RawRect) -> Option<Taps> {
        if lx < self.area.x as f64
            || lx >= (self.area.x + self.area.width) as f64
            || !(lx as u32 - self.area.x).is_multiple_of(self.col_pitch)
        {
            return None;
        }
        let fx = (((lx + 0.5) / active.width as f64) - self.origin[1]) / self.spacing[1];
        Some(Taps::clamped(fx, self.cols))
    }

    /// Every active row's and every active column's taps, in active-local order: what
    /// [`Self::gain`] derives per pixel, derived once per stage. Each vector is bounded by one
    /// side of the active area, at most `MAX_SIDE` entries.
    fn active_taps(&self, active: RawRect) -> (Vec<Option<Taps>>, Vec<Option<Taps>>) {
        let rows = (0..active.height)
            .map(|ly| self.row_taps(ly as f64, active))
            .collect();
        let columns = (0..active.width)
            .map(|lx| self.column_taps(lx as f64, active))
            .collect();
        (rows, columns)
    }

    /// The bilinear gain of `channel` between one row's and one column's taps.
    fn interpolate(&self, row: Taps, column: Taps, channel: usize) -> f64 {
        let (y0, y1, wy) = (row.near, row.far, row.weight);
        let (x0, x1, wx) = (column.near, column.far, column.weight);
        let at = |row: usize, col: usize| -> f64 {
            self.values
                [(row * self.cols + col) * self.map_planes + channel.min(self.map_planes - 1)]
                as f64
        };
        (at(y0, x0) * (1.0 - wx) + at(y0, x1) * wx) * (1.0 - wy)
            + (at(y1, x0) * (1.0 - wx) + at(y1, x1) * wx) * wy
    }
}

/// One map axis's bilinear taps for one pixel row or column: the map index at or before the
/// pixel's map coordinate, the next one (clamped at the map's edge) and the next one's weight.
#[derive(Debug, Clone, Copy)]
struct Taps {
    near: usize,
    far: usize,
    weight: f64,
}

impl Taps {
    /// The taps of map coordinate `f` on an axis of `count` map points, clamped to the map.
    fn clamped(f: f64, count: usize) -> Self {
        let f = f.clamp(0.0, (count - 1) as f64);
        let near = f.floor() as usize;
        Self {
            near,
            far: (near + 1).min(count - 1),
            weight: f - near as f64,
        }
    }
}

impl Warp {
    /// The reference luminance plane is green (plane 1 of the three supported planes).
    fn plane_role(&self) -> WarpRole {
        if !self.is_identity(1) {
            WarpRole::Distortion
        } else if !self.is_identity(0) || !self.is_identity(2) {
            WarpRole::LateralCa
        } else {
            WarpRole::Identity
        }
    }

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

// FixVignetteRadial (3), FixBadPixelsConstant (4) and FixBadPixelsList (5). The
// caller decides the opcode list and stage; nothing here holds catalog policy.

const MAX_PAYLOAD: usize = 1 << 20;
const MAX_BAD_POINTS: usize = 65_536;

#[derive(Debug, Clone, PartialEq)]
struct VignetteRadial {
    coefficients: [f64; 5],
    /// Normalized horizontal and vertical optical center.
    center: [f64; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BadPixels {
    bayer_phase: u32,
    points: Vec<(i32, i32)>,
    rectangles: Vec<(i32, i32, i32, i32)>,
}

impl VignetteRadial {
    /// Evaluate the DNG gain at an image-local pixel center.  The caller maps
    /// its `RawRect` to `(width, height)` and can apply this lazily per plane.
    fn gain(&self, x: f64, y: f64, width: usize, height: usize) -> Result<f64, RawError> {
        if width == 0 || height == 0 || !x.is_finite() || !y.is_finite() {
            return Err(RawError::InvalidInput("vignette coordinates"));
        }
        // DNG normalizes the optical center and radius against the outer
        // pixel coordinates, so an N-pixel axis spans 0..N-1 (not pixel
        // edges or pixel centers).
        let last_x = (width - 1) as f64;
        let last_y = (height - 1) as f64;
        let cx = self.center[0] * last_x;
        let cy = self.center[1] * last_y;
        let radius = (cx.max(last_x - cx).powi(2) + cy.max(last_y - cy).powi(2)).sqrt();
        if radius == 0.0 || !radius.is_finite() {
            return Err(RawError::InvalidInput("vignette radius"));
        }
        let r2 = (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / radius).powi(2);
        let mut gain = 0.0;
        for coefficient in self.coefficients.iter().rev() {
            gain = r2 * (coefficient + gain);
        }
        let gain = gain + 1.0;
        if gain.is_finite() {
            Ok(gain)
        } else {
            Err(RawError::InvalidInput("vignette gain"))
        }
    }
}

impl BadPixels {
    /// Compute one replacement from an immutable source mosaic.  This is the
    /// integration point for a decoder that must retain the exact original
    /// mosaic: callers can use the value only for the current development
    /// pass and leave source/history bytes untouched.
    #[cfg(test)]
    fn replacement(
        &self,
        source: &[u16],
        width: usize,
        height: usize,
        y: usize,
        x: usize,
    ) -> Result<Option<u16>, RawError> {
        if !self.rectangles.is_empty() {
            return Err(RawError::UnsupportedMode(
                "clustered FixBadPixelsList rectangles".into(),
            ));
        }
        if width == 0
            || height == 0
            || width.checked_mul(height) != Some(source.len())
            || y >= height
            || x >= width
        {
            return Err(RawError::InvalidInput("bad-pixel source coordinates"));
        }
        let listed = self
            .points
            .iter()
            .any(|&(row, col)| row == y as i32 && col == x as i32);
        if !listed {
            return Ok(None);
        }
        let listed_bad = |yy: usize, xx: usize| {
            self.points
                .iter()
                .any(|&(row, col)| row == yy as i32 && col == xx as i32)
        };
        Ok(estimate_same_color(
            source,
            width,
            height,
            y,
            x,
            self.bayer_phase,
            listed_bad,
        ))
    }

    /// Compute all listed replacements from one immutable source pass.  The
    /// result is sorted and duplicate coordinates are emitted once; malformed
    /// coordinates and clustered rectangles fail. The returned unresolved
    /// count follows Adobe's behavior of leaving points with no usable
    /// same-colour neighbour unchanged.
    #[cfg(test)]
    fn replacements(
        &self,
        source: &[u16],
        width: usize,
        height: usize,
    ) -> Result<Vec<(usize, u16)>, RawError> {
        self.replacements_with_unresolved(source, width, height)
            .map(|(patches, _)| patches)
    }

    fn replacements_with_unresolved(
        &self,
        source: &[u16],
        width: usize,
        height: usize,
    ) -> Result<(Vec<(usize, u16)>, usize), RawError> {
        if !self.rectangles.is_empty() {
            return Err(RawError::UnsupportedMode(
                "clustered FixBadPixelsList rectangles".into(),
            ));
        }
        if width == 0 || height == 0 || width.checked_mul(height) != Some(source.len()) {
            return Err(RawError::InvalidInput("bad-pixel source dimensions"));
        }
        let mut points = self.points.clone();
        points.sort_unstable();
        points.dedup();
        if points.len() > MAX_BAD_POINTS {
            return Err(RawError::ResourceLimit("bad pixel replacements"));
        }
        let mut out = Vec::with_capacity(points.len());
        let mut unresolved = 0;
        for &(y, x) in &points {
            if y < 0 || x < 0 || y as usize >= height || x as usize >= width {
                return Err(RawError::InvalidInput("bad pixel outside image"));
            }
            let y = y as usize;
            let x = x as usize;
            // The list can contain 65,536 entries. Repeated linear membership
            // scans would make a valid dense list quadratic to decode.
            let listed_bad =
                |yy: usize, xx: usize| points.binary_search(&(yy as i32, xx as i32)).is_ok();
            if let Some(value) =
                estimate_same_color(source, width, height, y, x, self.bayer_phase, listed_bad)
            {
                out.push((y * width + x, value));
            } else {
                unresolved += 1;
            }
        }
        Ok((out, unresolved))
    }
}

/// Immutable counterpart for opcode 4.  Use this during each develop pass
/// when the retained mosaic must remain byte-for-byte identical.
fn bad_pixel_constant_replacement(
    source: &[u16],
    width: usize,
    height: usize,
    y: usize,
    x: usize,
    constant: u32,
    phase: u32,
) -> Result<Option<u16>, RawError> {
    if constant > u16::MAX as u32
        || width == 0
        || height == 0
        || width.checked_mul(height) != Some(source.len())
        || y >= height
        || x >= width
    {
        return Err(RawError::InvalidInput("bad-pixel source coordinates"));
    }
    let bad = constant as u16;
    if source[y * width + x] != bad {
        return Ok(None);
    }
    Ok(estimate_same_color(
        source,
        width,
        height,
        y,
        x,
        phase,
        |yy, xx| source[yy * width + xx] == bad,
    ))
}

/// Scan once and return sparse replacements, leaving `source` untouched.
#[cfg(test)]
fn bad_pixel_constant_replacements(
    source: &[u16],
    width: usize,
    height: usize,
    constant: u32,
    phase: u32,
) -> Result<Vec<(usize, u16)>, RawError> {
    bad_pixel_constant_replacements_with_unresolved(source, width, height, constant, phase)
        .map(|(patches, _)| patches)
}

#[cfg(test)]
fn bad_pixel_constant_replacements_with_unresolved(
    source: &[u16],
    width: usize,
    height: usize,
    constant: u32,
    phase: u32,
) -> Result<(Vec<(usize, u16)>, usize), RawError> {
    bad_pixel_constant_replacements_in_area(
        source,
        width,
        height,
        constant,
        phase,
        RawRect {
            x: 0,
            y: 0,
            width: width as u32,
            height: height as u32,
        },
    )
}

fn bad_pixel_constant_replacements_in_area(
    source: &[u16],
    width: usize,
    height: usize,
    constant: u32,
    phase: u32,
    active: RawRect,
) -> Result<(Vec<(usize, u16)>, usize), RawError> {
    crate::checked_rect(active, width as u32, height as u32)?;
    if constant > u16::MAX as u32
        || width == 0
        || height == 0
        || width.checked_mul(height) != Some(source.len())
    {
        return Err(RawError::InvalidInput("bad-pixel source dimensions/value"));
    }
    let bad = constant as u16;
    let mut out = Vec::new();
    let mut unresolved = 0;
    for y in active.y as usize..(active.y + active.height) as usize {
        for x in active.x as usize..(active.x + active.width) as usize {
            if source[y * width + x] == bad {
                if let Some(value) =
                    bad_pixel_constant_replacement(source, width, height, y, x, constant, phase)?
                {
                    out.push((y * width + x, value));
                } else {
                    unresolved += 1;
                }
                if out.len() > MAX_BAD_POINTS {
                    return Err(RawError::ResourceLimit("bad pixel replacements"));
                }
            }
        }
    }
    Ok((out, unresolved))
}

fn checked_payload(data: &[u8]) -> Result<&[u8], RawError> {
    if data.len() > MAX_PAYLOAD {
        return Err(RawError::ResourceLimit("opcode payload"));
    }
    Ok(data)
}

/// Parse opcode 3.  Adobe defines five coefficients followed by H/V center.
fn parse_vignette_radial(data: &[u8]) -> Result<VignetteRadial, RawError> {
    let p = checked_payload(data)?;
    if p.len() != 56 {
        return Err(RawError::InvalidInput("FixVignetteRadial parameter length"));
    }
    let finite = |at| {
        let value = be_f64(p, at)?;
        if value.is_finite() {
            Ok(value)
        } else {
            Err(RawError::InvalidInput("non-finite opcode coefficient"))
        }
    };
    let mut coefficients = [0.0; 5];
    for (index, value) in coefficients.iter_mut().enumerate() {
        *value = finite(index * 8)?;
    }
    let center = [finite(40)?, finite(48)?];
    if center.iter().any(|v| !(0.0..=1.0).contains(v)) {
        return Err(RawError::InvalidInput("FixVignetteRadial center"));
    }
    Ok(VignetteRadial {
        coefficients,
        center,
    })
}

/// Parse opcode 4's bare parameter payload: `constant, bayerPhase`.
fn parse_bad_pixels_constant(data: &[u8]) -> Result<(u32, u32), RawError> {
    let p = checked_payload(data)?;
    if p.len() != 8 {
        return Err(RawError::InvalidInput(
            "FixBadPixelsConstant parameter length",
        ));
    }
    let phase = be_u32(p, 4)?;
    if phase > 3 {
        return Err(RawError::InvalidInput("FixBadPixelsConstant Bayer phase"));
    }
    Ok((be_u32(p, 0)?, phase))
}

/// Parse opcode 5's bare parameter payload: `bayerPhase, pointCount,
/// rectangleCount`, followed by image-local coordinates.
fn parse_bad_pixels_list(data: &[u8]) -> Result<BadPixels, RawError> {
    let p = checked_payload(data)?;
    if p.len() < 12 {
        return Err(RawError::InvalidInput("truncated DNG correction"));
    }
    let phase = be_u32(p, 0)?;
    let point_count = be_u32(p, 4)? as usize;
    let rect_count = be_u32(p, 8)? as usize;
    if phase > 3 || point_count > MAX_BAD_POINTS || rect_count > MAX_BAD_POINTS {
        return Err(RawError::ResourceLimit("FixBadPixelsList entries"));
    }
    let expected = 12usize
        .checked_add(
            point_count
                .checked_mul(8)
                .ok_or(RawError::ResourceLimit("bad pixel points"))?,
        )
        .and_then(|v| v.checked_add(rect_count.checked_mul(16)?))
        .ok_or(RawError::ResourceLimit("bad pixel list size"))?;
    if p.len() != expected {
        return Err(RawError::InvalidInput("FixBadPixelsList parameter length"));
    }
    let mut at = 12;
    let mut points = Vec::with_capacity(point_count);
    for _ in 0..point_count {
        points.push((be_i32(p, at)?, be_i32(p, at + 4)?));
        at += 8;
    }
    let mut rectangles = Vec::with_capacity(rect_count);
    for _ in 0..rect_count {
        rectangles.push((
            be_i32(p, at)?,
            be_i32(p, at + 4)?,
            be_i32(p, at + 8)?,
            be_i32(p, at + 12)?,
        ));
        at += 16;
    }
    Ok(BadPixels {
        bayer_phase: phase,
        points,
        rectangles,
    })
}

fn estimate_same_color<F: Fn(usize, usize) -> bool>(
    pixels: &[u16],
    width: usize,
    height: usize,
    y: usize,
    x: usize,
    phase: u32,
    bad: F,
) -> Option<u16> {
    // DNG's phase IDs are 0=top-left red, 1=green on the red row,
    // 2=green on the blue row, 3=top-left blue.  In all four layouts the
    // green samples are the diagonal parity selected by this expression;
    // phase 0/3 therefore use axial neighbours and phase 1/2 diagonals.
    let green = ((y as u32 + x as u32 + phase + (phase >> 1)) & 1) == 1;
    let offsets: &[(isize, isize)] = if green {
        &[(-1, -1), (-1, 1), (1, -1), (1, 1)]
    } else {
        &[(-2, 0), (2, 0), (0, -2), (0, 2)]
    };
    let mut sum = 0u32;
    let mut n = 0u32;
    for &(dy, dx) in offsets {
        let yy = y as isize + dy;
        let xx = x as isize + dx;
        if yy < 0 || xx < 0 || yy >= height as isize || xx >= width as isize {
            continue;
        }
        let yy = yy as usize;
        let xx = xx as usize;
        if !bad(yy, xx) {
            sum += pixels[yy * width + xx] as u32;
            n += 1;
        }
    }
    (n > 0).then(|| ((sum + n / 2) / n) as u16)
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
                .zip(settings.required_opcodes.iter())
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
        let mut sensor_gains = Vec::new();
        let mut sensor_vignette = None;
        for op in &required {
            match Opcode::implemented(op.list, op.id) {
                Some(Opcode::GainMap) => {
                    if op.list == crate::opcodes::OPCODE_LIST2 {
                        sensor_gains.push(GainMap::parse_for(&op.data, active, true)?);
                    } else {
                        stages.push(Stage3::Gain(GainMap::parse(&op.data, active)?));
                    }
                }
                Some(Opcode::WarpRectilinear) => {
                    let warp = Warp::parse(&op.data, active)?;
                    if let Some(role) = settings.optics.and_then(|optics| optics.warp_rectilinear) {
                        let expected = match role {
                            crate::DngOpticalRole::Distortion => WarpRole::Distortion,
                            crate::DngOpticalRole::LateralCa => WarpRole::LateralCa,
                            crate::DngOpticalRole::Shading => {
                                return Err(RawError::UnsupportedMode("DNG optical role".into()));
                            }
                        };
                        if warp.plane_role() != expected {
                            return Err(RawError::UnsupportedMode("DNG optical role".into()));
                        }
                    }
                    stages.push(Stage3::Warp(warp))
                }
                Some(Opcode::FixVignetteRadial) => {
                    let radial = parse_vignette_radial(&op.data)?;
                    if op.list == crate::opcodes::OPCODE_LIST1 {
                        if sensor_vignette.replace(radial).is_some() || sensor_repair.is_some() {
                            return Err(RawError::UnsupportedMode(
                                "DNG repeated/mixed stage-one vignette".into(),
                            ));
                        }
                    } else {
                        stages.push(Stage3::Vignette(radial));
                    }
                }
                Some(repair)
                    if repair.repairs_sensor()
                        && sensor_repair.is_none()
                        && sensor_vignette.is_none() =>
                {
                    sensor_repair = Some(if repair == Opcode::FixBadPixelsConstant {
                        let (constant, phase) = parse_bad_pixels_constant(&op.data)?;
                        SensorRepair::Constant(constant, phase)
                    } else {
                        SensorRepair::Listed(parse_bad_pixels_list(&op.data)?)
                    });
                }
                _ => return Err(RawError::UnsupportedRequiredOpcodes(vec![op.id])),
            }
        }
        Ok(Self {
            active,
            stages,
            sensor_repair,
            repair_active_only: matches!(
                settings.container,
                crate::profiles::DngContainer::IntegerCfaSegments
                    | crate::profiles::DngContainer::IntegerLinearSegments
            ),
            sensor_gains,
            sensor_vignette,
            metadata: DngCorrectionMetadata {
                interpretation: settings.interpretation.to_string(),
                applied: required.into_iter().map(provenance).collect(),
                skipped_optional,
                calibration,
            },
        })
    }

    /// Stage-one radial correction and stage-two CFA maps run before demosaic.
    /// The retained integer data is immutable; only this development's float mosaic changes.
    pub(crate) fn apply_sensor(
        &self,
        pixels: &mut [f32],
        raw: &crate::RawSource,
        gains: [f32; 3],
        cancel: &AtomicBool,
        lanes: usize,
    ) -> Result<(), RawError> {
        if self.sensor_gains.is_empty() && self.sensor_vignette.is_none() {
            return Ok(());
        }
        if raw.metadata.layout != crate::RawLayout::Mosaic {
            return Err(RawError::UnsupportedMode(
                "CFA correction requires a sensor mosaic".into(),
            ));
        }
        let width = raw.metadata.sensor_width as usize;
        let black = crate::normalize::BlackLevels::of(&raw.metadata);
        correction_rows(pixels, width, lanes, cancel, |y, row| {
            for (x, pixel) in row.iter_mut().enumerate() {
                if x < self.active.x as usize
                    || y < self.active.y as usize
                    || x >= (self.active.x + self.active.width) as usize
                    || y >= (self.active.y + self.active.height) as usize
                {
                    continue;
                }
                if let Some(radial) = &self.sensor_vignette {
                    let v = radial.gain(
                        (x - self.active.x as usize) as f64,
                        (y - self.active.y as usize) as f64,
                        self.active.width as usize,
                        self.active.height as usize,
                    )?;
                    let b = black.at(x, y);
                    let channel = raw.metadata.cfa[(y % raw.metadata.cfa_height as usize)
                        * raw.metadata.cfa_width as usize
                        + x % raw.metadata.cfa_width as usize]
                        as usize;
                    *pixel = ((raw.mosaic[y * width + x] as f64 * v - b as f64)
                        * (crate::normalize::SENSOR_SCALE / (raw.metadata.sensor_white - b)) as f64
                        * gains[channel] as f64) as f32;
                }
                for map in &self.sensor_gains {
                    *pixel = (*pixel as f64 * map.gain(x as f64, y as f64, 0, self.active)?) as f32;
                }
            }
            Ok(())
        })
    }

    /// The same pre-WB stage-one and stage-two interpretation at one retained sensor sample.
    /// The picker supplies its nearest same-colour CFA site after the stage-three warp.
    pub(crate) fn sensor_normalized(
        &self,
        x: u32,
        y: u32,
        sample: f64,
        black: f64,
        white: f64,
    ) -> Result<f64, RawError> {
        let radial = self
            .sensor_vignette
            .as_ref()
            .map(|v| {
                v.gain(
                    x as f64 - self.active.x as f64,
                    y as f64 - self.active.y as f64,
                    self.active.width as usize,
                    self.active.height as usize,
                )
            })
            .transpose()?
            .unwrap_or(1.0);
        let mut value = (sample * radial - black) / (white - black);
        for map in &self.sensor_gains {
            value *= map.gain(x as f64, y as f64, 0, self.active)?;
        }
        Ok(value)
    }

    pub(crate) fn changes_normalization(&self) -> bool {
        !self.sensor_gains.is_empty() || self.sensor_vignette.is_some()
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
            Some(SensorRepair::Constant(value, phase)) => bad_pixel_constant_replacements_in_area(
                source,
                width,
                height,
                *value,
                *phase,
                if self.repair_active_only {
                    self.active
                } else {
                    RawRect {
                        x: 0,
                        y: 0,
                        width: width as u32,
                        height: height as u32,
                    }
                },
            ),
            Some(SensorRepair::Listed(list)) => {
                list.replacements_with_unresolved(source, width, height)
            }
        }?;
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
                    gain *= radial.gain(
                        point.0 - self.active.x as f64,
                        point.1 - self.active.y as f64,
                        self.active.width as usize,
                        self.active.height as usize,
                    )?
                }
            }
        }
        Ok(gain)
    }

    /// Apply the stage-3 corrections to developed camera planes in place.
    /// A value that overflows `f32` becomes infinite rather than an error
    /// here: like the development before it, this checks no finiteness, and
    /// the one check is the caller's, where it adopts the converted planes.
    /// Whether any correction runs on the demosaiced planes (a gain map, a vignette or a warp),
    /// so the developed planes no longer share the demosaic's one clip ceiling.
    pub(crate) fn corrects_after_demosaic(&self) -> bool {
        !self.stages.is_empty()
    }

    pub(crate) fn apply(&self, rgb: &mut PlanarRgb, cancel: &AtomicBool) -> Result<(), RawError> {
        // Photo-sized active areas run their row jobs on the development executor at the pool's
        // width; smaller ones run in order on the caller. A row owns its output; stages and
        // channels still join in order and reuse one warp plane.
        let lanes = if u64::from(self.active.width) * u64::from(self.active.height)
            >= crate::limits::PARALLEL_PIXELS
        {
            rayon::current_num_threads()
        } else {
            1
        };
        self.apply_rows(rgb, cancel, lanes)
    }

    fn apply_rows(
        &self,
        rgb: &mut PlanarRgb,
        cancel: &AtomicBool,
        lanes: usize,
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
        let first = self.active.y as usize * width;
        let left = self.active.x as usize;
        for stage in &self.stages {
            // A GainMap's taps depend on the row alone or the column alone: derive them once per
            // stage, then weigh each pixel of each channel by the same expression as `gain`.
            let taps = match stage {
                Stage3::Gain(map) => Some(map.active_taps(self.active)),
                _ => None,
            };
            for channel in 0..3 {
                let plane = &mut rgb.data[channel * n..(channel + 1) * n];
                match stage {
                    Stage3::Gain(map) => {
                        let (row_taps, column_taps) = taps.as_ref().unwrap();
                        let covered = map.covers(channel);
                        let rows = &mut plane[first..first + area_h * width];
                        correction_rows(rows, width, lanes, cancel, |yy, row| {
                            let row_taps = row_taps[yy];
                            for (pixel, column) in
                                row[left..left + area_w].iter_mut().zip(column_taps)
                            {
                                let gain = match (covered, row_taps, *column) {
                                    (true, Some(row), Some(column)) => {
                                        map.interpolate(row, column, channel)
                                    }
                                    _ => 1.0,
                                };
                                *pixel = (*pixel as f64 * gain) as f32;
                            }
                            Ok(())
                        })?;
                    }
                    Stage3::Vignette(radial) => {
                        let rows = &mut plane[first..first + area_h * width];
                        correction_rows(rows, width, lanes, cancel, |yy, row| {
                            for (xx, pixel) in row[left..left + area_w].iter_mut().enumerate() {
                                let gain = radial.gain(xx as f64, yy as f64, area_w, area_h)?;
                                *pixel = (*pixel as f64 * gain) as f32;
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
                        correction_rows(&mut scratch, area_w, lanes, cancel, |yy, row| {
                            for (xx, pixel) in row.iter_mut().enumerate() {
                                let x = self.active.x + xx as u32;
                                let y = self.active.y + yy as u32;
                                let (sx, sy) =
                                    warp.source(x as f64, y as f64, channel, self.active)?;
                                *pixel = bicubic(plane, width, self.active, sx, sy) as f32;
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

    #[test]
    fn sensor_maps_apply_to_their_cfa_parity_before_demosaic_and_share_picker_math() {
        let mut raw = crate::layout_tests::source(crate::RawLayout::Mosaic, vec![576; 16], 4, 4);
        raw.metadata.cfa_width = 2;
        raw.metadata.cfa_height = 2;
        raw.metadata.cfa = vec![0, 1, 1, 2];
        raw.metadata.black_cfa = vec![0, 1, 3, 2];
        let map = GainMap {
            area: RawRect {
                x: 1,
                y: 0,
                width: 3,
                height: 4,
            },
            plane: 0,
            planes: 1,
            row_pitch: 2,
            col_pitch: 2,
            rows: 1,
            cols: 1,
            spacing: [1.0; 2],
            origin: [0.0; 2],
            map_planes: 1,
            values: vec![2.0],
        };
        let correction = DngCorrection {
            active: raw.metadata.active_area,
            stages: vec![],
            sensor_repair: None,
            repair_active_only: true,
            sensor_gains: vec![map],
            sensor_vignette: None,
            metadata: DngCorrectionMetadata {
                interpretation: "test".into(),
                applied: vec![],
                skipped_optional: vec![],
                calibration: DngCalibrationMetadata {
                    illuminants: [17, 21],
                    color_matrix1_sha256: String::new(),
                    color_matrix2_sha256: String::new(),
                    selected: "test".into(),
                },
            },
        };
        let mut normalized = raw
            .normalization([1.0; 3])
            .run(1, &AtomicBool::new(false))
            .unwrap();
        correction
            .apply_sensor(&mut normalized, &raw, [1.0; 3], &AtomicBool::new(false), 1)
            .unwrap();
        for y in 0..4 {
            for x in 0..4 {
                let expected = if y % 2 == 0 && x % 2 == 1 { 1.0 } else { 0.5 };
                assert_eq!(
                    normalized[y * 4 + x],
                    expected * crate::normalize::SENSOR_SCALE
                );
                assert_eq!(
                    correction
                        .sensor_normalized(x as u32, y as u32, 576.0, 128.0, 1024.0)
                        .unwrap(),
                    expected as f64
                );
            }
        }
        assert_eq!(raw.source_samples(), &[576; 16]);
        assert!(matches!(
            correction.apply_sensor(&mut normalized, &raw, [1.0; 3], &AtomicBool::new(true), 1),
            Err(RawError::Cancelled)
        ));
    }

    #[test]
    fn stage_one_vignette_precedes_black_subtraction_at_a_point() {
        let radial = VignetteRadial {
            coefficients: [1.0, 0.0, 0.0, 0.0, 0.0],
            center: [0.5, 0.5],
        };
        // At a corner the radius is one and gain is two. This distinguishes pre-black correction.
        let active = RawRect {
            x: 0,
            y: 0,
            width: 5,
            height: 5,
        };
        let mut correction = DngCorrection {
            active,
            stages: vec![],
            sensor_repair: None,
            repair_active_only: true,
            sensor_gains: vec![],
            sensor_vignette: Some(radial),
            metadata: DngCorrectionMetadata {
                interpretation: "test".into(),
                applied: vec![],
                skipped_optional: vec![],
                calibration: DngCalibrationMetadata {
                    illuminants: [17, 21],
                    color_matrix1_sha256: String::new(),
                    color_matrix2_sha256: String::new(),
                    selected: "test".into(),
                },
            },
        };
        assert_eq!(
            correction
                .sensor_normalized(0, 0, 256.0, 128.0, 1024.0)
                .unwrap(),
            3.0 / 7.0
        );
        assert_eq!(
            correction
                .sensor_normalized(2, 2, 256.0, 128.0, 1024.0)
                .unwrap(),
            1.0 / 7.0
        );
        correction.sensor_vignette = None;
        assert_eq!(
            correction
                .sensor_normalized(0, 0, 256.0, 128.0, 1024.0)
                .unwrap(),
            1.0 / 7.0
        );
    }

    #[test]
    fn constant_bad_pixel_repair_ignores_masked_padding_and_keeps_sparse_bound() {
        let mut pixels = vec![0; 512 * 256];
        for y in 0..256 {
            pixels[y * 512..y * 512 + 8].fill(100);
        }
        pixels[10 * 512 + 4] = 0;
        let (patches, unresolved) = bad_pixel_constant_replacements_in_area(
            &pixels,
            512,
            256,
            0,
            0,
            RawRect {
                x: 0,
                y: 0,
                width: 8,
                height: 256,
            },
        )
        .unwrap();
        assert_eq!(patches, vec![(10 * 512 + 4, 100)]);
        assert_eq!(unresolved, 0);
        assert_eq!(pixels[10 * 512 + 4], 0);
    }

    fn reference() -> Value {
        serde_json::from_str(include_str!("../../../fixtures/raw-dng-reference.json")).unwrap()
    }

    fn fc3411_warp_payload() -> Vec<u8> {
        let vectors = reference();
        let mut payload = vec![0_u8; 164];
        payload[..4].copy_from_slice(&3_u32.to_be_bytes());
        for (plane, values) in vectors["opcodes"][1]["coefficients"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            for (i, value) in values.as_array().unwrap().iter().enumerate() {
                let p = 4 + 48 * plane + 8 * i;
                payload[p..p + 8].copy_from_slice(&value.as_f64().unwrap().to_be_bytes());
            }
        }
        payload[148..156].copy_from_slice(&0.5_f64.to_be_bytes());
        payload[156..164].copy_from_slice(&0.5_f64.to_be_bytes());
        payload
    }

    #[test]
    fn fc3411_warp_green_identity_is_lateral_ca_only() {
        let active = RawRect {
            x: 96,
            y: 0,
            width: 5472,
            height: 3648,
        };
        let warp = Warp::parse(&fc3411_warp_payload(), active).unwrap();
        assert_eq!(warp.plane_role(), WarpRole::LateralCa);
    }

    #[test]
    fn identical_plane_warp_classifies_as_distortion() {
        let active = RawRect {
            x: 0,
            y: 0,
            width: 100,
            height: 80,
        };
        let mut payload = fc3411_warp_payload();
        let reference = payload[4..52].to_vec();
        payload[52..100].copy_from_slice(&reference);
        payload[100..148].copy_from_slice(&reference);
        assert_eq!(
            Warp::parse(&payload, active).unwrap().plane_role(),
            WarpRole::Distortion
        );
        for plane in 0..3 {
            payload[4 + plane * 48..52 + plane * 48].fill(0);
            payload[4 + plane * 48..12 + plane * 48].copy_from_slice(&1.0_f64.to_be_bytes());
        }
        assert_eq!(
            Warp::parse(&payload, active).unwrap().plane_role(),
            WarpRole::Identity
        );
    }

    #[test]
    fn declared_role_mismatch_fails_preparation() {
        let camera = crate::camera_catalog()
            .cameras
            .iter()
            .find(|camera| camera.model == "FC3411")
            .unwrap();
        let mut settings = camera.dng.clone().unwrap();
        settings.required_opcodes = settings
            .required_opcodes
            .iter()
            .filter(|op| op.id == 1)
            .copied()
            .collect::<Vec<_>>()
            .into();
        let active = RawRect {
            x: 0,
            y: 0,
            width: 100,
            height: 80,
        };
        let mut payload = fc3411_warp_payload();
        payload[52..60].copy_from_slice(&1.001_f64.to_be_bytes());
        let opcode = DngOpcode {
            list: crate::opcodes::OPCODE_LIST3,
            id: 1,
            version: crate::opcodes::VERSION,
            flags: 0,
            ifd: 8,
            data: payload,
        };
        let calibration = DngCalibrationMetadata {
            illuminants: [17, 21],
            color_matrix1_sha256: String::new(),
            color_matrix2_sha256: String::new(),
            selected: "ColorMatrix2".into(),
        };
        assert!(
            matches!(DngCorrection::parse(&[opcode], active, 8, calibration, &settings),
            Err(RawError::UnsupportedMode(reason)) if reason == "DNG optical role")
        );
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
            repair_active_only: false,
            sensor_gains: Vec::new(),
            sensor_vignette: None,
            stages: vec![
                Stage3::Gain(gain),
                Stage3::Warp(warp),
                Stage3::Vignette(VignetteRadial {
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
                correction.apply_rows(&mut serial, &cancel, 1).unwrap();
                for lanes in [2, 4, rayon::current_num_threads()] {
                    let mut parallel = PlanarRgb {
                        width: original.width,
                        height: original.height,
                        data: original.data.clone(),
                    };
                    correction
                        .apply_rows(&mut parallel, &cancel, lanes)
                        .unwrap();
                    for (i, (&a, &b)) in serial.data.iter().zip(&parallel.data).enumerate() {
                        assert_eq!(
                            a.to_bits(),
                            b.to_bits(),
                            "{width}x{height}, reverse={reverse}, {lanes} lanes, value {i}"
                        );
                    }
                }
                for (i, &a) in serial.data.iter().enumerate() {
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
                assert!(serial.data.iter().any(|v| *v < 0.0));
                assert!(serial.data.iter().any(|v| *v > 1.0));
            }
        }
    }

    /// Gain, warp and vignette in both stage orders, serial and on the pool,
    /// keep the bits they had before the RAW preparation clean-ups. Captured
    /// on the owner's M4; elsewhere serial and pooled are still compared above.
    #[test]
    fn corrected_planes_keep_their_pinned_bits() {
        use sha2::{Digest, Sha256};
        let cancel = AtomicBool::new(false);
        let mut digests = Vec::new();
        for (width, height) in [(17, 13), (1001, 1003)] {
            let (mut correction, original) = row_fixture(width, height);
            for reverse in [false, true] {
                if reverse {
                    correction.stages.reverse();
                }
                for lanes in [1, rayon::current_num_threads()] {
                    let mut image = PlanarRgb {
                        width: original.width,
                        height: original.height,
                        data: original.data.clone(),
                    };
                    correction.apply_rows(&mut image, &cancel, lanes).unwrap();
                    let mut hash = Sha256::new();
                    for value in &image.data {
                        hash.update(value.to_bits().to_le_bytes());
                    }
                    digests.push(format!("{:x}", hash.finalize()));
                }
            }
        }
        println!("{digests:?}");
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(
            digests,
            [
                "fcd5f7cc99dbb927a41c2bfe0274524bf699becb6645ca32acbd805d0864a7b0",
                "fcd5f7cc99dbb927a41c2bfe0274524bf699becb6645ca32acbd805d0864a7b0",
                "7d0b57f288344e1fa2ff23b2f883125bc687455f72ba4dc274788b51cf395a97",
                "7d0b57f288344e1fa2ff23b2f883125bc687455f72ba4dc274788b51cf395a97",
                "2cfc47d24cafa7b70ac50887d2c16e91bd9567beb1ed01a2b25bab4f635a3620",
                "2cfc47d24cafa7b70ac50887d2c16e91bd9567beb1ed01a2b25bab4f635a3620",
                "aadb5173d355eee114f8b86c8d2446cb4bbfcf6b9bcbabdd4933edfbe99a63c9",
                "aadb5173d355eee114f8b86c8d2446cb4bbfcf6b9bcbabdd4933edfbe99a63c9",
            ]
        );
    }

    #[test]
    fn correction_rows_report_cancellation_and_leave_overflow_to_adoption() {
        for lanes in [1, 4] {
            let cancel = AtomicBool::new(true);
            let (correction, mut pixels) = row_fixture(33, 19);
            let before = pixels.data.clone();
            assert_eq!(
                correction.apply_rows(&mut pixels, &cancel, lanes),
                Err(RawError::Cancelled)
            );
            assert_eq!(pixels.data, before);

            // A row that raises cancellation prevents later scheduled rows
            // from entering their pixel loop. Partially produced data is never
            // returned as a development.
            cancel.store(false, Ordering::Relaxed);
            let mut rows = vec![0.0; 1024];
            let result = correction_rows(&mut rows, 16, lanes, &cancel, |_, _| {
                cancel.store(true, Ordering::Relaxed);
                Ok(())
            });
            assert_eq!(result, Err(RawError::Cancelled));

            // An overflow is left in the planes, where the adopting camera
            // conversion rejects it; nothing here hides it as a finite value.
            cancel.store(false, Ordering::Relaxed);
            pixels.data.fill(f32::MAX);
            correction.apply_rows(&mut pixels, &cancel, lanes).unwrap();
            assert!(pixels.data.iter().any(|value| !value.is_finite()));
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
            let pool = rayon::current_num_threads();
            for lanes in [1, pool, pool, 1] {
                let mut timings = Vec::new();
                for _ in 0..5 {
                    pixels.data.copy_from_slice(&original);
                    let start = Instant::now();
                    correction.apply_rows(&mut pixels, &cancel, lanes).unwrap();
                    timings.push(start.elapsed().as_secs_f64() * 1000.0);
                    black_box(&pixels);
                }
                eprintln!(
                    "{}",
                    serde_json::json!({"width":width,"height":height,"lanes":lanes,"ms":timings})
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
                repair_active_only: false,
                sensor_gains: Vec::new(),
                sensor_vignette: None,
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

    /// The owner's DJI Air 2S DNG answers the neutral picker's bounded point queries through its
    /// corrections: a positive gain and a finite sensor location.
    #[test]
    #[ignore = "requires explicit local authentic DJI DNG"]
    fn owner_dji_dng_answers_corrected_point_queries() {
        let owner = std::env::var("LUXFORGE_RAW_OWNER_DIR").expect("owner fixture directory");
        let bytes = std::fs::read(format!("{owner}/mavic_air_2s.DNG")).expect("read DJI DNG");
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "aab79ce1795a7dd5f1c2e52ec7bd07345cb9bda0262d1d5aa3701db212b09e1d"
        );
        let raw = crate::RawSource::decode(bytes, &AtomicBool::new(false)).expect("decode DJI DNG");
        assert!(raw.gain_at_corrected_sensor(2840.0, 1800.0, 1).unwrap() > 0.0);
        let corrected = raw.corrected_sensor_sample_location(100, 4, 0).unwrap();
        assert!(corrected.0.is_finite() && corrected.1.is_finite());
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

    #[test]
    fn parses_and_applies_radial_gain() {
        let mut p = Vec::new();
        for _ in 0..5 {
            p.extend_from_slice(&0.0f64.to_be_bytes());
        }
        p.extend_from_slice(&0.5f64.to_be_bytes());
        p.extend_from_slice(&0.5f64.to_be_bytes());
        let params = parse_vignette_radial(&p).unwrap();
        assert!((params.gain(0.0, 0.0, 3, 3).unwrap() - 1.0).abs() < 1e-12);
        let mut q = Vec::new();
        for value in [0.5f64, 0.0, 0.0, 0.0, 0.0] {
            q.extend_from_slice(&value.to_be_bytes());
        }
        q.extend_from_slice(&0.5f64.to_be_bytes());
        q.extend_from_slice(&0.5f64.to_be_bytes());
        let curved = parse_vignette_radial(&q).unwrap();
        assert!((curved.gain(0.0, 0.0, 3, 3).unwrap() - 1.5).abs() < 1e-12);
    }

    #[test]
    fn constant_bad_pixel_uses_same_colour_neighbours() {
        let mut px = vec![100u16; 25];
        px[12] = 0;
        let replacements = bad_pixel_constant_replacements(&px, 5, 5, 0, 0).unwrap();
        assert_eq!(replacements, vec![(12, 100)]);
    }

    #[test]
    fn clustered_list_is_explicitly_rejected() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        for n in [1i32, 1, 2, 2] {
            p.extend_from_slice(&n.to_be_bytes());
        }
        let list = parse_bad_pixels_list(&p).unwrap();
        assert!(matches!(
            list.replacement(&[1; 25], 5, 5, 2, 2),
            Err(RawError::UnsupportedMode(_))
        ));
    }

    #[test]
    fn listed_replacements_are_sorted_deduplicated_and_preserve_source() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&3u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        for (y, x) in [(3i32, 3i32), (2, 2), (3, 3)] {
            p.extend_from_slice(&y.to_be_bytes());
            p.extend_from_slice(&x.to_be_bytes());
        }
        let list = parse_bad_pixels_list(&p).unwrap();
        let source = vec![100u16; 49];
        let result = list.replacements(&source, 7, 7).unwrap();
        assert_eq!(result, vec![(16, 100), (24, 100)]);
        assert!(source.iter().all(|v| *v == 100));
    }

    #[test]
    fn listed_border_uses_available_neighbours_and_out_of_bounds_fails() {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        for n in [1i32, 1] {
            p.extend_from_slice(&n.to_be_bytes());
        }
        let list = parse_bad_pixels_list(&p).unwrap();
        assert_eq!(list.replacements(&[1; 25], 5, 5).unwrap(), vec![(6, 1)]);
        let mut q = Vec::new();
        q.extend_from_slice(&0u32.to_be_bytes());
        q.extend_from_slice(&1u32.to_be_bytes());
        q.extend_from_slice(&0u32.to_be_bytes());
        for n in [9i32, 9] {
            q.extend_from_slice(&n.to_be_bytes());
        }
        let out = parse_bad_pixels_list(&q).unwrap();
        assert!(matches!(
            out.replacements(&[1; 25], 5, 5),
            Err(RawError::InvalidInput(_))
        ));
    }

    #[test]
    fn maximum_dense_list_has_no_usable_neighbours() {
        let list = BadPixels {
            bayer_phase: 0,
            points: (0..256)
                .rev()
                .flat_map(|y| (0..256).map(move |x| (y, x)))
                .collect(),
            rectangles: vec![],
        };
        let source = vec![100; MAX_BAD_POINTS];
        let (patches, unresolved) = list
            .replacements_with_unresolved(&source, 256, 256)
            .unwrap();
        assert!(patches.is_empty());
        assert_eq!(unresolved, MAX_BAD_POINTS);
    }

    #[test]
    fn listed_point_with_no_available_same_colour_is_reported() {
        let mut p = Vec::new();
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&5u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        for (y, x) in [(2i32, 2i32), (1, 1), (1, 3), (3, 1), (3, 3)] {
            p.extend_from_slice(&y.to_be_bytes());
            p.extend_from_slice(&x.to_be_bytes());
        }
        let list = parse_bad_pixels_list(&p).unwrap();
        let (patches, unresolved) = list.replacements_with_unresolved(&[1; 25], 5, 5).unwrap();
        assert_eq!(patches.len(), 4);
        assert_eq!(unresolved, 1);
    }

    #[test]
    fn phase_selects_axial_or_diagonal_same_colour_neighbours() {
        let mut source = vec![0u16; 49];
        source[7 + 3] = 10;
        source[5 * 7 + 3] = 20;
        source[3 * 7 + 1] = 30;
        source[3 * 7 + 5] = 40;
        source[2 * 7 + 2] = 1;
        source[2 * 7 + 4] = 2;
        source[4 * 7 + 2] = 3;
        source[4 * 7 + 4] = 4;
        let axial = BadPixels {
            bayer_phase: 0,
            points: vec![(3, 3)],
            rectangles: vec![],
        };
        let diagonal = BadPixels {
            bayer_phase: 1,
            points: vec![(3, 3)],
            rectangles: vec![],
        };
        assert_eq!(axial.replacement(&source, 7, 7, 3, 3).unwrap(), Some(25));
        assert_eq!(diagonal.replacement(&source, 7, 7, 3, 3).unwrap(), Some(3));
    }
}
