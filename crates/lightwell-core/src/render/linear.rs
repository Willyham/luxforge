//! High precision scene-linear rendering for prepared RAW sources.
//!
//! This module is deliberately independent of a RAW decoder. A decoder or source-preparation
//! worker supplies immutable planar RGB values in unbounded linear sRGB/D65. The recipe is then
//! evaluated in f64 and converted to the existing byte [`Raster`] only at the terminal boundary.
//! The byte JPEG evaluator in [`super::render`] remains unchanged.

use super::{
    Cancel, Compiled, Entry, Raster,
    spatial::{
        self, PRODUCTION_TILE, SpatialPlan, build_reduction, fill_planes, resolve_globals,
        run_batches, run_tile,
    },
};
use crate::{
    Error, ErrorKind, Recipe, SnapshotId,
    modules::{Global, ModuleRegistry, SpatialOperation, Stage},
};
use rayon::prelude::*;
use std::sync::{Arc, Weak};

const MAX_PIXELS: u64 = lightwell_raw::MAX_PIXELS as u64;
const MAX_SIDE: u32 = 16_384;
const MAX_SOURCE_BYTES: u64 = lightwell_raw::MAX_RGB_BYTES as u64;
const MAX_RGBA_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RESAMPLES: usize = 1;
const PARALLEL_RENDER_PIXELS: u64 = 1_000_000;

fn layout(width: u32, height: u32) -> Result<(usize, usize), Error> {
    if width == 0 || height == 0 {
        return Err(Error::new(
            ErrorKind::Validation,
            "linear source dimensions must be nonzero",
        ));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            "linear source side exceeds 16384 pixels",
        ));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::ResourceLimit,
                "linear source dimensions overflow",
            )
        })?;
    if pixels > MAX_PIXELS {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            "linear source exceeds 128 megapixels",
        ));
    }
    let values = pixels.checked_mul(3).ok_or_else(|| {
        Error::new(
            ErrorKind::ResourceLimit,
            "linear source plane length overflow",
        )
    })?;
    let bytes = values
        .checked_mul(u64::from(std::mem::size_of::<f32>() as u32))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::ResourceLimit,
                "linear source byte length overflow",
            )
        })?;
    if bytes > MAX_SOURCE_BYTES {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            "linear RGB source exceeds 512 MiB",
        ));
    }
    let values = usize::try_from(values)
        .map_err(|_| Error::new(ErrorKind::ResourceLimit, "linear source is not addressable"))?;
    let plane_len = usize::try_from(pixels).map_err(|_| {
        Error::new(
            ErrorKind::ResourceLimit,
            "linear source plane is not addressable",
        )
    })?;
    Ok((values, plane_len))
}

fn output_len(width: u32, height: u32) -> Result<usize, Error> {
    if width == 0 || height == 0 {
        return Err(Error::new(
            ErrorKind::Validation,
            "linear output dimensions must be nonzero",
        ));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            "linear output side exceeds 16384 pixels",
        ));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| {
            Error::new(
                ErrorKind::ResourceLimit,
                "linear output dimensions overflow",
            )
        })?;
    if pixels > lightwell_raw::MAX_PIXELS as u64 {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            "linear output exceeds 128 megapixels",
        ));
    }
    let bytes = pixels.checked_mul(4).ok_or_else(|| {
        Error::new(
            ErrorKind::ResourceLimit,
            "linear output byte length overflow",
        )
    })?;
    if bytes > MAX_RGBA_BYTES {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            "linear output exceeds 512 MiB",
        ));
    }
    usize::try_from(bytes)
        .map_err(|_| Error::new(ErrorKind::ResourceLimit, "linear output is not addressable"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct View {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    orientation: u8,
}

impl View {
    fn output_dimensions(self) -> (u32, u32) {
        if (5..=8).contains(&self.orientation) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    fn map(self, x: u32, y: u32) -> Option<(u32, u32)> {
        let (source_x, source_y) = match self.orientation {
            1 => (x, y),
            2 => (self.width.checked_sub(1)?.checked_sub(x)?, y),
            3 => (
                self.width.checked_sub(1)?.checked_sub(x)?,
                self.height.checked_sub(1)?.checked_sub(y)?,
            ),
            4 => (x, self.height.checked_sub(1)?.checked_sub(y)?),
            5 => (y, x),
            6 => (y, self.height.checked_sub(1)?.checked_sub(x)?),
            7 => (
                self.width.checked_sub(1)?.checked_sub(y)?,
                self.height.checked_sub(1)?.checked_sub(x)?,
            ),
            8 => (self.width.checked_sub(1)?.checked_sub(y)?, x),
            _ => return None,
        };
        Some((self.x + source_x, self.y + source_y))
    }
}

/// Immutable planar f32 RGB prepared in linear sRGB/D65.
///
/// `planes` is laid out as one complete R plane, followed by G and B. Values may be negative or
/// above one; only non-finite values are rejected. A view can crop and orient these planes without
/// copying them, which lets a source adapter expose its active/upright content rectangle cheaply.
/// The `Arc<Vec<f32>>` stores the caller's moved `Vec` without copying its pixel buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearImage {
    base_width: u32,
    base_height: u32,
    planes: Arc<Vec<f32>>,
    fingerprint: String,
    view: View,
    /// Which development these planes are: a process-unique number taken when the planes were
    /// adopted, shared by every view over them and by nothing else. A redevelopment of the same
    /// source is a new number even when the allocator hands its planes the address the old ones
    /// had, which is why a cache keys on this and never on an address.
    development: u64,
}

/// The source of every [`LinearImage::development`] number.
static NEXT_DEVELOPMENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl LinearImage {
    /// Construct an identity-view image from contiguous planar R, G and B values.
    pub fn new(width: u32, height: u32, planes: impl Into<Arc<Vec<f32>>>) -> Result<Self, Error> {
        Self::with_fingerprint(width, height, planes, String::new())
    }

    /// Construct an identity-view image with the source identity copied to output rasters.
    pub fn with_fingerprint(
        width: u32,
        height: u32,
        planes: impl Into<Arc<Vec<f32>>>,
        fingerprint: impl Into<String>,
    ) -> Result<Self, Error> {
        let planes = planes.into();
        let (expected, _) = layout(width, height)?;
        if planes.len() != expected {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("linear source needs {expected} planar values"),
            ));
        }
        let capacity_bytes = u64::try_from(planes.capacity())
            .ok()
            .and_then(|capacity| capacity.checked_mul(u64::from(std::mem::size_of::<f32>() as u32)))
            .ok_or_else(|| {
                Error::new(ErrorKind::ResourceLimit, "linear source capacity overflows")
            })?;
        if capacity_bytes > MAX_SOURCE_BYTES {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                "linear RGB source capacity exceeds 512 MiB",
            ));
        }
        if planes.iter().any(|value| !value.is_finite()) {
            return Err(Error::new(
                ErrorKind::Validation,
                "linear source contains a non-finite value",
            ));
        }
        Ok(Self {
            base_width: width,
            base_height: height,
            planes,
            fingerprint: fingerprint.into(),
            view: View {
                x: 0,
                y: 0,
                width,
                height,
                orientation: 1,
            },
            development: NEXT_DEVELOPMENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        })
    }

    /// Return a cropped/oriented view without copying the source planes.
    ///
    /// `crop` is `[x, y, width, height]` in the base source-plane coordinates. EXIF orientation
    /// values 1 through 8 use the standard mappings and are applied exactly once to that crop.
    pub fn with_view(&self, crop: [u32; 4], orientation: u8) -> Result<Self, Error> {
        let [x, y, width, height] = crop;
        if !(1..=8).contains(&orientation) {
            return Err(Error::new(
                ErrorKind::Validation,
                "linear source orientation must be EXIF 1 through 8",
            ));
        }
        let right = x.checked_add(width).ok_or_else(|| {
            Error::new(ErrorKind::ResourceLimit, "linear crop exceeds dimensions")
        })?;
        let bottom = y.checked_add(height).ok_or_else(|| {
            Error::new(ErrorKind::ResourceLimit, "linear crop exceeds dimensions")
        })?;
        if width == 0 || height == 0 || right > self.base_width || bottom > self.base_height {
            return Err(Error::new(
                ErrorKind::Validation,
                "linear crop lies outside source planes",
            ));
        }
        let view = View {
            x,
            y,
            width,
            height,
            orientation,
        };
        let (output_width, output_height) = view.output_dimensions();
        let _ = layout(output_width, output_height)?;
        Ok(Self {
            base_width: self.base_width,
            base_height: self.base_height,
            planes: Arc::clone(&self.planes),
            fingerprint: self.fingerprint.clone(),
            view,
            development: self.development,
        })
    }

    pub fn width(&self) -> u32 {
        self.view.output_dimensions().0
    }

    pub fn height(&self) -> u32 {
        self.view.output_dimensions().1
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn view(&self) -> ([u32; 4], u8) {
        (
            [self.view.x, self.view.y, self.view.width, self.view.height],
            self.view.orientation,
        )
    }

    /// The development these planes belong to: equal for every view over the same adopted planes,
    /// different for every redevelopment, and never reused within the process.
    pub fn development(&self) -> u64 {
        self.development
    }

    pub(crate) fn storage_weak(&self) -> Weak<Vec<f32>> {
        Arc::downgrade(&self.planes)
    }

    /// A bulk reader over this image's viewed pixels. The plane length and the view are resolved
    /// once here instead of per access, which is what the proxy downscale needs: it reads every
    /// viewed pixel at most twice per axis and allocates nothing of its own to do it.
    pub(crate) fn reader(&self) -> ViewReader<'_> {
        let (width, height) = self.view.output_dimensions();
        ViewReader {
            planes: self.planes.as_slice(),
            base_width: self.base_width,
            // `layout` accepted these dimensions when the image was built, so the product is
            // addressable and this cannot overflow `usize`.
            plane_len: self.base_width as usize * self.base_height as usize,
            view: self.view,
            width,
            height,
        }
    }

    pub fn planes(&self) -> &[f32] {
        self.planes.as_slice()
    }

    /// Read one view pixel without allocating. This is also useful to a source-stage picker.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        let (width, height) = self.view.output_dimensions();
        if x >= width || y >= height {
            return None;
        }
        let (base_x, base_y) = self.view.map(x, y)?;
        let index =
            usize::try_from(u64::from(base_y) * u64::from(self.base_width) + u64::from(base_x))
                .ok()?;
        let plane_len =
            usize::try_from(u64::from(self.base_width) * u64::from(self.base_height)).ok()?;
        Some([
            self.planes[index],
            self.planes[plane_len + index],
            self.planes[2 * plane_len + index],
        ])
    }

    fn pixel_f64(&self, x: u32, y: u32) -> Result<[f64; 3], Error> {
        let pixel = self.pixel(x, y).ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                format!("linear source coordinate ({x}, {y}) is outside the view"),
            )
        })?;
        let pixel = pixel.map(f64::from);
        if pixel.iter().all(|value| value.is_finite()) {
            Ok(pixel)
        } else {
            Err(Error::new(
                ErrorKind::Render,
                "linear source produced a non-finite pixel",
            ))
        }
    }
}

/// Reads viewed pixels of a [`LinearImage`] without recomputing its layout per access. It borrows
/// the one immutable plane allocation and copies nothing.
pub(crate) struct ViewReader<'a> {
    planes: &'a [f32],
    base_width: u32,
    plane_len: usize,
    view: View,
    width: u32,
    height: u32,
}

impl ViewReader<'_> {
    pub(crate) fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// One viewed pixel, or `None` outside the view: the same mapping and the same values as
    /// [`LinearImage::pixel`], which is what makes a bulk read agree with a point read.
    #[inline]
    pub(crate) fn pixel(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let (base_x, base_y) = self.view.map(x, y)?;
        let index = base_y as usize * self.base_width as usize + base_x as usize;
        Some([
            self.planes[index],
            self.planes[self.plane_len + index],
            self.planes[2 * self.plane_len + index],
        ])
    }
}

/// Per-evaluation linear settings. Zero EV is the neutral default; the setting is applied to the
/// source before recipe content edits and never to an intermediate display raster.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearSettings {
    pub exposure_ev: f64,
    /// An approximate white-balance change, applied to each source pixel before the exposure
    /// multiply: `exposure · (W · p)`. Only the preview of an open draft carries one, when the
    /// drafted temperature or tint asks for sensor gains the developed planes were not developed
    /// at. Every committed render, export, point sample and analysis is `None`, which is bit for
    /// bit the evaluation without this field. See [`WhiteBalanceApproximation`].
    pub white_balance: Option<WhiteBalanceApproximation>,
}

impl Default for LinearSettings {
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            white_balance: None,
        }
    }
}

/// A RAW white-balance change approximated on planes developed at another white balance.
///
/// The retained planes are `R · D(g)` per pixel, where `D(g)` is the native demosaic of the mosaic
/// after the sensor gains `g` (camera RGB) and `R` is the camera-to-linear-sRGB matrix. The demosaic
/// is nonlinear, which is why a committed white balance redevelops the mosaic, but to first order
/// `D(g') ≈ diag(g'/g) · D(g)`. So planes developed at `g` approximate the planes at `g'` by
///
/// `W = R · diag(g'_c / g_c) · R⁻¹`
///
/// applied to every pixel. The approximation is used only for a drafted preview during a gesture:
/// nothing committed, exported, sampled or analysed is ever evaluated through it, and a frame
/// rendered with it says so. The matrix is private and only the constructors build it, so every
/// instance is finite by construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WhiteBalanceApproximation {
    matrix: [[f64; 3]; 3],
}

impl WhiteBalanceApproximation {
    /// `W = R · diag(target / developed) · R⁻¹` for the camera-to-linear-sRGB matrix `R`, with
    /// `R⁻¹` computed in f64. A non-finite or non-positive gain, a non-finite `R`, an `R` that is
    /// singular (or so close to it that its inverse is meaningless) and a non-finite `W` are each
    /// refused: there is no approximation, never a wrong one.
    pub fn between(
        camera_to_srgb: [[f64; 3]; 3],
        developed: [f32; 3],
        target: [f32; 3],
    ) -> Result<Self, Error> {
        let mut ratio = [0.0; 3];
        for (channel, value) in ratio.iter_mut().enumerate() {
            let (from, to) = (f64::from(developed[channel]), f64::from(target[channel]));
            if !(from.is_finite() && to.is_finite() && from > 0.0 && to > 0.0) {
                return Err(Error::new(
                    ErrorKind::Validation,
                    "white-balance gains must be finite and positive",
                ));
            }
            *value = to / from;
        }
        let inverse = invert(camera_to_srgb)?;
        let matrix = std::array::from_fn(|row| {
            std::array::from_fn(|column| {
                (0..3)
                    .map(|k| camera_to_srgb[row][k] * ratio[k] * inverse[k][column])
                    .sum::<f64>()
            })
        });
        Self::from_matrix(matrix)
    }

    /// An explicit linear-sRGB matrix, refused unless every entry is finite.
    pub fn from_matrix(matrix: [[f64; 3]; 3]) -> Result<Self, Error> {
        if matrix.iter().flatten().all(|value| value.is_finite()) {
            Ok(Self { matrix })
        } else {
            Err(Error::new(
                ErrorKind::Validation,
                "a white-balance approximation must be finite",
            ))
        }
    }

    /// The matrix applied to each linear-sRGB pixel, row by row.
    pub fn matrix(&self) -> [[f64; 3]; 3] {
        self.matrix
    }

    #[inline]
    fn apply(&self, pixel: [f64; 3]) -> [f64; 3] {
        self.matrix
            .map(|row| row[0] * pixel[0] + row[1] * pixel[1] + row[2] * pixel[2])
    }

    /// A key that tells this approximation's evaluation apart from an exact one of the same
    /// recipe, for a cache keyed by recipe: the matrix's own bits.
    fn key(&self) -> String {
        self.matrix
            .iter()
            .flatten()
            .map(|value| format!("{:016x}", value.to_bits()))
            .collect()
    }
}

/// The inverse of a 3×3 matrix in f64, by the adjugate. Refused when the matrix is not finite or
/// its determinant is negligible against the product of its row norms (Hadamard's bound on it),
/// which is where an inverse stops meaning anything.
fn invert(matrix: [[f64; 3]; 3]) -> Result<[[f64; 3]; 3], Error> {
    let singular = || {
        Error::new(
            ErrorKind::UnsupportedColor,
            "the camera matrix is singular, so no white-balance approximation exists",
        )
    };
    if !matrix.iter().flatten().all(|value| value.is_finite()) {
        return Err(singular());
    }
    let [[a, b, c], [d, e, f], [g, h, i]] = matrix;
    let cofactors = [
        [e * i - f * h, c * h - b * i, b * f - c * e],
        [f * g - d * i, a * i - c * g, c * d - a * f],
        [d * h - e * g, b * g - a * h, a * e - b * d],
    ];
    let determinant = a * cofactors[0][0] + b * cofactors[1][0] + c * cofactors[2][0];
    let bound: f64 = matrix
        .iter()
        .map(|row| row.iter().map(|value| value * value).sum::<f64>().sqrt())
        .product();
    if !determinant.is_finite() || bound == 0.0 || determinant.abs() <= 1.0e-12 * bound {
        return Err(singular());
    }
    let inverse = cofactors.map(|row| row.map(|value| value / determinant));
    if inverse.iter().flatten().all(|value| value.is_finite()) {
        Ok(inverse)
    } else {
        Err(singular())
    }
}

impl LinearSettings {
    fn multiplier(self) -> Result<f64, Error> {
        if !self.exposure_ev.is_finite() || !(-5.0..=5.0).contains(&self.exposure_ev) {
            return Err(Error::new(
                ErrorKind::Validation,
                "linear exposure must be finite and between -5 and +5 EV",
            ));
        }
        let multiplier = self.exposure_ev.exp2();
        if multiplier.is_finite() {
            Ok(multiplier)
        } else {
            Err(Error::new(
                ErrorKind::Render,
                "linear exposure multiplier overflow",
            ))
        }
    }
}

fn decode_srgb(value: u8) -> f64 {
    let encoded = f64::from(value) / 255.0;
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

fn decode_rgb(value: [u8; 3]) -> [f64; 3] {
    value.map(decode_srgb)
}

fn terminal_srgb(linear: f64) -> Result<u8, Error> {
    if !linear.is_finite() {
        return Err(Error::new(
            ErrorKind::Render,
            "linear evaluation produced a non-finite value",
        ));
    }
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    let rounded = (encoded * 255.0).round();
    if !rounded.is_finite() || !(0.0..=255.0).contains(&rounded) {
        return Err(Error::new(
            ErrorKind::Render,
            "terminal sRGB conversion overflow",
        ));
    }
    Ok(rounded as u8)
}

fn clamp_index(value: f64, limit: u32) -> u32 {
    let last = limit.saturating_sub(1);
    if value <= 0.0 {
        0
    } else if value >= f64::from(last) {
        last
    } else {
        value as u32
    }
}

fn linear_bilinear(
    u: f64,
    v: f64,
    width: u32,
    height: u32,
    fetch: impl Fn(u32, u32) -> Result<[f64; 3], Error>,
) -> Result<[f64; 3], Error> {
    if !u.is_finite() || !v.is_finite() || width == 0 || height == 0 {
        return Err(Error::new(
            ErrorKind::Render,
            "linear resample has invalid coordinates or dimensions",
        ));
    }
    let x = u - 0.5;
    let y = v - 0.5;
    let left = x.floor();
    let top = y.floor();
    let weight_x = x - left;
    let weight_y = y - top;
    let (left_x, right_x) = (clamp_index(left, width), clamp_index(left + 1.0, width));
    let (top_y, bottom_y) = (clamp_index(top, height), clamp_index(top + 1.0, height));
    let corners = [
        (fetch(left_x, top_y)?, (1.0 - weight_x) * (1.0 - weight_y)),
        (fetch(right_x, top_y)?, weight_x * (1.0 - weight_y)),
        (fetch(left_x, bottom_y)?, (1.0 - weight_x) * weight_y),
        (fetch(right_x, bottom_y)?, weight_x * weight_y),
    ];
    let output = std::array::from_fn(|channel| {
        corners
            .iter()
            .map(|(pixel, weight)| pixel[channel] * weight)
            .sum::<f64>()
    });
    if output.iter().all(|value| value.is_finite()) {
        Ok(output)
    } else {
        Err(Error::new(
            ErrorKind::Render,
            "linear resample produced a non-finite value",
        ))
    }
}

struct LinearEvaluation<'a> {
    source: &'a LinearImage,
    compiled: Compiled,
    exposure_multiplier: f64,
    /// Applied to each source pixel before the exposure multiply, when the settings carry one.
    white_balance: Option<WhiteBalanceApproximation>,
    /// The frame a spatial entry produces, at the index of the segment it enters. The linear path
    /// pulls single pixels through the compiled prefix, and a neighbourhood cannot be pulled one
    /// pixel at a time, so each spatial operation's output is materialized once, in stage order,
    /// as three `f32` planes inside the 512 MiB frame limit. Nothing is quantized here: the values
    /// stay float until the terminal boundary.
    spatial_frames: Vec<Option<Arc<Vec<f32>>>>,
    /// The output tile a spatial entry is evaluated in; [`PRODUCTION_TILE`] outside the tests that
    /// prove the result does not depend on it.
    tile: u32,
}

impl<'a> LinearEvaluation<'a> {
    fn new(
        registry: &ModuleRegistry,
        source: &'a LinearImage,
        recipe: &Recipe,
        settings: LinearSettings,
        cancel: &Cancel,
        tile: u32,
    ) -> Result<Self, Error> {
        let exposure_multiplier = settings.multiplier()?;
        let compiled = registry.compile(source.width(), source.height(), recipe)?;
        let resamples = compiled
            .segments
            .iter()
            .filter(|segment| matches!(segment.entry.as_ref().map(Entry::resample), Some(Some(_))))
            .count();
        if resamples > MAX_RESAMPLES {
            return Err(Error::new(
                ErrorKind::Validation,
                "linear evaluation supports at most one resample stage",
            ));
        }
        let spatial_frames = vec![None; compiled.segments.len()];
        let mut evaluation = Self {
            source,
            compiled,
            exposure_multiplier,
            white_balance: settings.white_balance,
            spatial_frames,
            tile,
        };
        // In order, because a later spatial operation pulls its input through the earlier one.
        for index in 0..evaluation.compiled.segments.len() {
            let Some(Entry::Spatial {
                operation,
                prefix_hash,
            }) = &evaluation.compiled.segments[index].entry
            else {
                continue;
            };
            let frame = evaluation.build_spatial_frame(
                index,
                &operation.clone(),
                &prefix_hash.clone(),
                cancel,
            )?;
            evaluation.spatial_frames[index] = Some(Arc::new(frame));
        }
        Ok(evaluation)
    }

    /// Materialize one spatial operation's output over the whole stage, tile by tile, in the same
    /// batches and against the same budget the byte path uses. Each tile's input region is pulled
    /// through `pixel_in` of the previous segment, which already applies the source exposure,
    /// every colour unit and every replacement, so the operation sees exactly the stage the design
    /// says it does without an input frame ever existing.
    fn build_spatial_frame(
        &self,
        index: usize,
        operation: &SpatialOperation,
        prefix_hash: &str,
        cancel: &Cancel,
    ) -> Result<Vec<f32>, Error> {
        let previous = &self.compiled.segments[index - 1];
        let stage = Stage {
            width: previous.width,
            height: previous.height,
        };
        // The frame limit applies to this float frame exactly as it does to a byte frame.
        let (values, _) = layout(stage.width, stage.height)?;
        let read = |x: u32, y: u32| -> Result<[f32; 3], Error> {
            let pixel = self.pixel_in(index - 1, x, y)?.ok_or_else(|| {
                Error::new(
                    ErrorKind::Render,
                    "a spatial read was outside its input stage",
                )
            })?;
            Ok(pixel.map(|value| value as f32))
        };
        let plan = SpatialPlan::new(operation, stage, self.tile)?;
        // The estimate store is keyed by the recipe prefix, which an approximate white balance
        // does not change: the drafted recipe names the target gains whichever planes it is
        // evaluated over. So an approximate evaluation keys its estimates apart, and a committed
        // render of the same recipe never takes one estimated from approximate pixels.
        let approximate_prefix = self.white_balance.map(|balance| {
            format!(
                "{prefix_hash}+white-balance-approximation:{}",
                balance.key()
            )
        });
        let globals: Vec<Option<Global>> = resolve_globals(
            operation,
            stage,
            self.source.fingerprint(),
            approximate_prefix.as_deref().unwrap_or(prefix_hash),
            || build_reduction(stage, read),
        )?;
        let mut frame = vec![0.0_f32; values];
        let plane = (u64::from(stage.width) * u64::from(stage.height)) as usize;
        run_batches(
            &plan,
            cancel,
            |tile| {
                run_tile(&plan, operation, &globals, tile, |region, planes| {
                    fill_planes(region, planes, read)
                })
            },
            |tile, (region, tile_values)| -> Result<(), Error> {
                for y in tile.y0..tile.y1() {
                    for x in tile.x0..tile.x1() {
                        let pixel = spatial::plane_pixel(region, &tile_values, x, y);
                        let offset =
                            (u64::from(y) * u64::from(stage.width) + u64::from(x)) as usize;
                        frame[offset] = pixel[0];
                        frame[plane + offset] = pixel[1];
                        frame[2 * plane + offset] = pixel[2];
                    }
                }
                Ok(())
            },
        )?;
        Ok(frame)
    }

    fn stage(&self) -> (u32, u32) {
        let segment = self
            .compiled
            .segments
            .last()
            .expect("compiled recipe has a segment");
        (segment.width, segment.height)
    }

    /// The one point where the settings touch a source pixel: `exposure · p`, or
    /// `exposure · (W · p)` under an approximate white balance.
    fn source_pixel(&self, x: u32, y: u32) -> Result<[f64; 3], Error> {
        let pixel = self.source.pixel_f64(x, y)?;
        let output = match &self.white_balance {
            // The arithmetic an exact evaluation has always done, untouched.
            None => pixel.map(|value| value * self.exposure_multiplier),
            Some(balance) => balance
                .apply(pixel)
                .map(|value| value * self.exposure_multiplier),
        };
        if output.iter().all(|value| value.is_finite()) {
            Ok(output)
        } else {
            Err(Error::new(
                ErrorKind::Render,
                "linear exposure produced a non-finite value",
            ))
        }
    }

    fn pixel(&self, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        self.pixel_in(self.compiled.segments.len() - 1, x, y)
    }

    fn pixel_in(&self, index: usize, x: u32, y: u32) -> Result<Option<[f64; 3]>, Error> {
        let segment = &self.compiled.segments[index];
        let Some(resolved) = segment.resolve(x, y) else {
            return Ok(None);
        };
        let mut pixel = match &segment.entry {
            None => self.source_pixel(resolved.input_x, resolved.input_y)?,
            Some(Entry::Spatial { .. }) => {
                let previous = &self.compiled.segments[index - 1];
                let frame = self.spatial_frames[index]
                    .as_ref()
                    .expect("a spatial segment's frame is built before any pixel is pulled");
                let plane = (u64::from(previous.width) * u64::from(previous.height)) as usize;
                let offset = (u64::from(resolved.input_y) * u64::from(previous.width)
                    + u64::from(resolved.input_x)) as usize;
                [
                    f64::from(frame[offset]),
                    f64::from(frame[plane + offset]),
                    f64::from(frame[2 * plane + offset]),
                ]
            }
            Some(Entry::Resample(resample)) => {
                let resample = *resample;
                let previous = &self.compiled.segments[index - 1];
                let (u, v) = resample.input_at(resolved.input_x, resolved.input_y);
                linear_bilinear(
                    u,
                    v,
                    previous.width,
                    previous.height,
                    |sample_x, sample_y| {
                        self.pixel_in(index - 1, sample_x, sample_y)
                            .and_then(|pixel| {
                                pixel.ok_or_else(|| {
                                    Error::new(
                                        ErrorKind::Render,
                                        "linear recursive sample was outside stage",
                                    )
                                })
                            })
                    },
                )?
            }
        };
        if let Some((_, rgb)) = resolved.replacement {
            pixel = decode_rgb(rgb);
        }
        // The colour phases are the 8-bit path's, applied to this one linear pixel: the replacement
        // that wins here ends the runs before it, and every run after it processes the value in
        // place. Nothing is quantized between runs, which is the whole point of the linear path: a
        // scene value above 1 or below 0 survives to the next unit and only the terminal boundary
        // encodes it.
        if segment.has_color {
            let after = resolved.replacement.map_or(0, |(index, _)| index + 1);
            let mut linear = [pixel.map(|value| value as f32)];
            // The same coordinates the 8-bit path hands its units, so a position-dependent unit
            // makes `sample_linear` and `render_linear` agree pixel for pixel.
            for run in super::color_runs(&segment.operations).filter(|run| run.start >= after) {
                super::apply_units(&run, y, x, &mut linear)?;
            }
            pixel = linear[0].map(f64::from);
        }
        if pixel.iter().all(|value| value.is_finite()) {
            Ok(Some(pixel))
        } else {
            Err(Error::new(
                ErrorKind::Render,
                "linear evaluation produced a non-finite pixel",
            ))
        }
    }
}

fn terminal_pixel(pixel: [f64; 3]) -> Result<[u8; 4], Error> {
    Ok([
        terminal_srgb(pixel[0])?,
        terminal_srgb(pixel[1])?,
        terminal_srgb(pixel[2])?,
        255,
    ])
}

/// Render a prepared linear source through the existing recipe and terminally produce bytes.
pub fn render_linear(
    registry: &ModuleRegistry,
    source: &LinearImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    settings: LinearSettings,
) -> Result<Raster, Error> {
    render_linear_cancellable(
        registry,
        source,
        snapshot_id,
        recipe,
        settings,
        &Cancel::never(),
    )
}

/// [`render_linear`] under a [`Cancel`] token the row pass reads once per row and the spatial
/// operations read once per tile batch. With a token that is never cancelled this is byte for byte
/// [`render_linear`]; it is the same code, and [`render_linear`] is one call to it.
pub fn render_linear_cancellable(
    registry: &ModuleRegistry,
    source: &LinearImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    settings: LinearSettings,
    cancel: &Cancel,
) -> Result<Raster, Error> {
    render_linear_tiled(
        registry,
        source,
        snapshot_id,
        recipe,
        settings,
        cancel,
        PRODUCTION_TILE,
    )
}

/// [`render_linear_cancellable`] with the spatial tile size as a parameter, for the tests that
/// prove a rendered frame does not depend on it.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_linear_tiled(
    registry: &ModuleRegistry,
    source: &LinearImage,
    snapshot_id: SnapshotId,
    recipe: &Recipe,
    settings: LinearSettings,
    cancel: &Cancel,
    tile: u32,
) -> Result<Raster, Error> {
    // A token already cancelled when the call arrives costs no frame at all.
    cancel.check()?;
    let evaluation = LinearEvaluation::new(registry, source, recipe, settings, cancel, tile)?;
    let (width, height) = evaluation.stage();
    let output_len = output_len(width, height)?;
    let row_bytes = usize::try_from(u64::from(width) * 4).map_err(|_| {
        Error::new(
            ErrorKind::ResourceLimit,
            "linear output row is not addressable",
        )
    })?;
    let mut output = vec![0; output_len];
    // One relaxed load per output row, ahead of that row's evaluations; the f64 evaluation and the
    // terminal boundary are untouched.
    let render_row = |row_index: usize, row: &mut [u8]| -> Result<(), Error> {
        cancel.check()?;
        for x in 0..width {
            let pixel = evaluation.pixel(x, row_index as u32)?.ok_or_else(|| {
                Error::new(
                    ErrorKind::Render,
                    "linear output coordinate was outside stage",
                )
            })?;
            let rgba = terminal_pixel(pixel)?;
            let offset = x as usize * 4;
            row[offset..offset + 4].copy_from_slice(&rgba);
        }
        Ok(())
    };
    if u64::from(width) * u64::from(height) >= PARALLEL_RENDER_PIXELS {
        output
            .par_chunks_exact_mut(row_bytes)
            .enumerate()
            .try_for_each(|(row, pixels)| render_row(row, pixels))?;
    } else {
        output
            .chunks_exact_mut(row_bytes)
            .enumerate()
            .try_for_each(|(row, pixels)| render_row(row, pixels))?;
    }
    Ok(Raster {
        width,
        height,
        rgba: output.into(),
        source_fingerprint: source.fingerprint.clone(),
        snapshot_id,
    })
}

/// Evaluate one terminal output pixel without allocating a frame.
pub fn sample_linear(
    registry: &ModuleRegistry,
    source: &LinearImage,
    recipe: &Recipe,
    settings: LinearSettings,
    x: u32,
    y: u32,
) -> Result<super::Sample, Error> {
    let evaluation = LinearEvaluation::new(
        registry,
        source,
        recipe,
        settings,
        &Cancel::new(),
        PRODUCTION_TILE,
    )?;
    let (width, height) = evaluation.stage();
    let _ = output_len(width, height)?;
    let rgba = evaluation.pixel(x, y)?.map(terminal_pixel).transpose()?;
    Ok(super::Sample {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Layer, Recipe, SnapshotId,
        modules::{CropPayload, ModuleRegistry},
    };

    fn image(width: u32, height: u32, rgb: &[[f32; 3]]) -> LinearImage {
        assert_eq!(rgb.len(), (width * height) as usize);
        let plane_len = (width * height) as usize;
        let mut planes = Vec::with_capacity(plane_len * 3);
        for channel in 0..3 {
            planes.extend(rgb.iter().map(|pixel| pixel[channel]));
        }
        LinearImage::with_fingerprint(width, height, planes, "sha256:linear-test").unwrap()
    }

    fn reference_srgb(value: f64) -> u8 {
        let value = value.clamp(0.0, 1.0);
        let encoded = if value <= 0.003_130_8 {
            value * 12.92
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        };
        (encoded * 255.0).round() as u8
    }

    #[test]
    fn raw_planar_bound_is_separate_from_terminal_rgba_bound() {
        // This checks admission arithmetic only; LinearImage is not allocated.
        assert!(layout(16_000, 8_000).is_ok());
        assert!(output_len(16_000, 6_250).is_ok());
        assert!(output_len(16_000, 8_000).is_ok());
        assert!(output_len(16_000, 8_001).is_err());
        assert!(layout(16_384, 16_384).is_err());
    }

    /// A colour-stage layer reaches the linear path too: the Basic module's units run on the
    /// scene-linear pixel, without the 8-bit decode and quantize the JPEG path needs, and the only
    /// encoding is the terminal boundary. A RAW stack therefore never silently omits a Basic edit.
    #[test]
    fn a_colour_layer_runs_on_the_linear_pixel_and_encodes_only_at_the_boundary() {
        let source = image(2, 1, &[[0.1, 0.2, 0.3], [0.05, 0.4, 0.6]]);
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer {
                id: crate::LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload: serde_json::json!({"exposure": 1.0}),
            }],
        };
        let raster = render_linear(
            &ModuleRegistry::builtin(),
            &source,
            SnapshotId::new(),
            &recipe,
            LinearSettings::default(),
        )
        .unwrap();
        // +1 EV doubles the linear value; the second pixel's blue clips only at the encoding.
        assert_eq!(
            raster.pixel(0, 0),
            Some([
                reference_srgb(0.2),
                reference_srgb(0.4),
                reference_srgb(0.6),
                255
            ])
        );
        assert_eq!(
            raster.pixel(1, 0),
            Some([
                reference_srgb(0.1),
                reference_srgb(0.8),
                reference_srgb(1.0),
                255
            ])
        );
    }

    #[test]
    fn planar_source_keeps_negative_and_headroom_values_until_terminal_boundary() {
        let source = image(
            2,
            2,
            &[
                [-0.25, 0.18, 1.5],
                [0.5, 0.2, -0.1],
                [1.25, 0.4, 0.75],
                [2.0, -0.5, 0.25],
            ],
        );
        assert_eq!(source.pixel(0, 0), Some([-0.25, 0.18, 1.5]));
        let raster = render_linear(
            &ModuleRegistry::builtin(),
            &source,
            SnapshotId::new(),
            &Recipe::default(),
            LinearSettings::default(),
        )
        .unwrap();
        assert_eq!(
            raster.pixel(0, 0),
            Some([0, reference_srgb(0.18), 255, 255])
        );
        assert_eq!(
            raster.pixel(1, 1),
            Some([255, 0, reference_srgb(0.25), 255])
        );
    }

    #[test]
    fn exposure_is_f64_before_content_and_terminal_clipping() {
        let source = image(1, 1, &[[0.18, -0.1, 0.5]]);
        let raster = render_linear(
            &ModuleRegistry::builtin(),
            &source,
            SnapshotId::new(),
            &Recipe::default(),
            LinearSettings {
                exposure_ev: 1.0,
                white_balance: None,
            },
        )
        .unwrap();
        assert_eq!(
            raster.pixel(0, 0),
            Some([reference_srgb(0.36), 0, reference_srgb(1.0), 255])
        );
        assert!(
            LinearSettings {
                exposure_ev: 5.01,
                white_balance: None,
            }
            .multiplier()
            .is_err()
        );
        assert!(
            LinearSettings {
                exposure_ev: f64::NAN,
                white_balance: None,
            }
            .multiplier()
            .is_err()
        );
    }

    #[test]
    fn exact_recipe_geometry_and_source_view_preserve_working_values() {
        let source = image(
            3,
            2,
            &[
                [0.1, 1.1, -0.1],
                [0.2, 1.2, -0.2],
                [0.3, 1.3, -0.3],
                [0.4, 1.4, -0.4],
                [0.5, 1.5, -0.5],
                [0.6, 1.6, -0.6],
            ],
        );
        let view = source.with_view([0, 0, 3, 2], 6).unwrap();
        assert_eq!(view.width(), 2);
        assert_eq!(view.height(), 3);
        assert_eq!(view.pixel(0, 0), Some([0.4, 1.4, -0.4]));
        assert_eq!(view.pixel(1, 2), Some([0.3, 1.3, -0.3]));
        let recipe = Recipe {
            format: 1,
            layers: vec![Layer::orientation(crate::Orientation {
                mirror: false,
                turns: 2,
            })],
        };
        let evaluation = LinearEvaluation::new(
            &ModuleRegistry::builtin(),
            &view,
            &recipe,
            LinearSettings::default(),
            &Cancel::new(),
            PRODUCTION_TILE,
        )
        .unwrap();
        assert_eq!(
            evaluation.pixel(1, 2).unwrap(),
            Some([f64::from(0.4_f32), f64::from(1.4_f32), f64::from(-0.4_f32),])
        );
    }

    #[test]
    fn all_eight_source_view_orientations_match_independent_literals() {
        let source = image(
            2,
            3,
            &[
                [1.0, 0.0, 0.0],
                [2.0, 0.0, 0.0],
                [3.0, 0.0, 0.0],
                [4.0, 0.0, 0.0],
                [5.0, 0.0, 0.0],
                [6.0, 0.0, 0.0],
            ],
        );
        let expected = [
            (1, 2, 3, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]),
            (2, 2, 3, vec![2.0, 1.0, 4.0, 3.0, 6.0, 5.0]),
            (3, 2, 3, vec![6.0, 5.0, 4.0, 3.0, 2.0, 1.0]),
            (4, 2, 3, vec![5.0, 6.0, 3.0, 4.0, 1.0, 2.0]),
            (5, 3, 2, vec![1.0, 3.0, 5.0, 2.0, 4.0, 6.0]),
            (6, 3, 2, vec![5.0, 3.0, 1.0, 6.0, 4.0, 2.0]),
            (7, 3, 2, vec![6.0, 4.0, 2.0, 5.0, 3.0, 1.0]),
            (8, 3, 2, vec![2.0, 4.0, 6.0, 1.0, 3.0, 5.0]),
        ];
        for (orientation, width, height, expected_red) in expected {
            let view = source.with_view([0, 0, 2, 3], orientation).unwrap();
            let mut actual = Vec::with_capacity((width * height) as usize);
            for y in 0..height {
                for x in 0..width {
                    actual.push(view.pixel(x, y).unwrap()[0]);
                }
            }
            assert_eq!(actual, expected_red, "orientation {orientation}");
        }
    }

    #[test]
    fn source_vec_storage_is_moved_without_pixel_copy_and_views_share_it() {
        let mut planes = Vec::with_capacity(12);
        planes.extend([0.0_f32, 1.0, 2.0, 3.0]);
        planes.extend([4.0, 5.0, 6.0, 7.0]);
        planes.extend([8.0, 9.0, 10.0, 11.0]);
        let pointer = planes.as_ptr();
        let source = LinearImage::new(2, 2, planes).unwrap();
        let view = source.with_view([0, 0, 2, 2], 6).unwrap();
        assert_eq!(source.planes().as_ptr(), pointer);
        assert_eq!(view.planes().as_ptr(), pointer);
        assert_eq!(view.pixel(0, 0), Some([2.0, 6.0, 10.0]));
    }

    #[test]
    fn bilinear_preserves_headroom_and_point_replace_decodes_at_its_stage() {
        let corners = [
            [2.0, -1.0, 0.0],
            [0.0, 1.0, 2.0],
            [2.0, -1.0, 0.0],
            [0.0, 1.0, 2.0],
        ];
        let actual =
            linear_bilinear(1.0, 1.0, 2, 2, |x, y| Ok(corners[(y * 2 + x) as usize])).unwrap();
        assert_eq!(actual, [1.0, 0.0, 1.0]);

        let precise = [
            [0.125_123_456_789, -0.543_210_987_654, 1.734_567_890_123],
            [0.912_345_678_901, 0.234_567_890_123, -0.876_543_210_987],
            [1.234_567_890_123, -1.345_678_901_234, 0.456_789_012_345],
            [-0.321_098_765_432, 0.678_901_234_567, 1.890_123_456_789],
        ];
        let actual =
            linear_bilinear(1.25, 1.25, 2, 2, |x, y| Ok(precise[(y * 2 + x) as usize])).unwrap();
        let expected: [f64; 3] = std::array::from_fn(|channel| {
            precise[0][channel] * 0.0625
                + precise[1][channel] * 0.1875
                + precise[2][channel] * 0.1875
                + precise[3][channel] * 0.5625
        });
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1.0e-15);
        }

        let source = image(2, 2, &[[0.0, 0.0, 0.0]; 4]);
        let recipe = Recipe {
            format: 1,
            layers: vec![Layer::pixel(1, 0, [128, 64, 255])],
        };
        let sample = sample_linear(
            &ModuleRegistry::builtin(),
            &source,
            &recipe,
            LinearSettings::default(),
            1,
            0,
        )
        .unwrap();
        assert_eq!(
            sample.rgba,
            Some([
                reference_srgb(decode_srgb(128)),
                reference_srgb(decode_srgb(64)),
                255,
                255
            ])
        );
    }

    #[test]
    fn sample_matches_full_render_and_crop_has_no_float_intermediate() {
        let source = image(
            4,
            4,
            &(0..16)
                .map(|value| [value as f32 / 8.0, 0.25, -value as f32 / 16.0])
                .collect::<Vec<_>>(),
        );
        let recipe = Recipe {
            format: 1,
            layers: vec![Layer::crop(CropPayload {
                angle: 12.0,
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
            })],
        };
        let registry = ModuleRegistry::builtin();
        let raster = render_linear(
            &registry,
            &source,
            SnapshotId::new(),
            &recipe,
            LinearSettings::default(),
        )
        .unwrap();
        for y in 0..raster.height {
            for x in 0..raster.width {
                assert_eq!(
                    sample_linear(&registry, &source, &recipe, LinearSettings::default(), x, y)
                        .unwrap()
                        .rgba,
                    raster.pixel(x, y)
                );
            }
        }
        assert!(raster.width > 0 && raster.height > 0);
    }

    /// The linear path hands a positional unit the same coordinates the 8-bit path does, so
    /// `sample_linear` equals `render_linear` pixel for pixel through an exact rotation in one
    /// segment and after a crop resample, where the coordinates are the output stage's.
    #[test]
    fn a_positional_colour_unit_agrees_between_linear_render_and_sample() {
        use crate::render::tests::{colour_registry, positional_layer};
        let source = image(
            5,
            4,
            &(0..20)
                .map(|value| [value as f32 / 24.0, 0.25, 0.5 - value as f32 / 40.0])
                .collect::<Vec<_>>(),
        );
        let registry = colour_registry();
        for (case, layers) in [
            ("a positional unit alone", vec![positional_layer()]),
            (
                "after an exact rotation in the same segment",
                vec![
                    Layer::orientation(crate::Orientation {
                        mirror: false,
                        turns: 1,
                    }),
                    positional_layer(),
                ],
            ),
            (
                "after a crop resample, in output coordinates",
                vec![
                    Layer::crop(CropPayload {
                        angle: 0.0,
                        x: 0.2,
                        y: 0.2,
                        width: 0.6,
                        height: 0.6,
                    }),
                    positional_layer(),
                ],
            ),
        ] {
            let recipe = Recipe {
                format: crate::RECIPE_FORMAT,
                layers,
            };
            let raster = render_linear(
                &registry,
                &source,
                SnapshotId::new(),
                &recipe,
                LinearSettings::default(),
            )
            .unwrap();
            assert!(raster.width > 1 && raster.height > 1, "{case}");
            for y in 0..raster.height {
                for x in 0..raster.width {
                    assert_eq!(
                        sample_linear(&registry, &source, &recipe, LinearSettings::default(), x, y)
                            .unwrap()
                            .rgba,
                        raster.pixel(x, y),
                        "{case}: ({x}, {y})"
                    );
                }
            }
            assert_ne!(
                raster.pixel(0, 0),
                raster.pixel(raster.width - 1, raster.height - 1),
                "{case}: the unit varies across the frame"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // The white-balance approximation.
    // -----------------------------------------------------------------------------------------

    /// A varied source with negative and above-one values in every channel.
    fn varied(width: u32, height: u32) -> LinearImage {
        let pixels: Vec<[f32; 3]> = (0..width * height)
            .map(|index| {
                let value = index as f32;
                [
                    (value * 0.037) % 1.3 - 0.1,
                    (value * 0.051) % 1.1,
                    (value * 0.023) % 1.6 - 0.2,
                ]
            })
            .collect();
        image(width, height, &pixels)
    }

    /// A plausible camera-to-sRGB matrix: rows sum to one, strong off-diagonal terms, invertible.
    const CAMERA: [[f64; 3]; 3] = [
        [1.72, -0.61, -0.11],
        [-0.18, 1.49, -0.31],
        [0.04, -0.52, 1.48],
    ];

    fn apply(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
        matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
    }

    /// With no approximation the evaluation is the one every committed render has always done:
    /// each byte is the independent `sRGB(2^EV · p)` of its source pixel. An identity matrix, whose
    /// products are exact, renders the same bytes, so the approximation adds nothing but its matrix.
    #[test]
    fn no_approximation_is_bit_for_bit_the_exposure_evaluation() {
        let registry = ModuleRegistry::builtin();
        let source = varied(9, 7);
        let exposure_ev = 0.7;
        let plain = LinearSettings {
            exposure_ev,
            white_balance: None,
        };
        let raster = render_linear(
            &registry,
            &source,
            SnapshotId::new(),
            &Recipe::default(),
            plain,
        )
        .unwrap();
        let multiplier = exposure_ev.exp2();
        for y in 0..7 {
            for x in 0..9 {
                let pixel = source.pixel(x, y).unwrap().map(f64::from);
                let expected = pixel.map(|value| reference_srgb(value * multiplier));
                assert_eq!(
                    raster.pixel(x, y),
                    Some([expected[0], expected[1], expected[2], 255]),
                    "({x}, {y})"
                );
            }
        }
        let identity = LinearSettings {
            exposure_ev,
            white_balance: Some(
                WhiteBalanceApproximation::from_matrix([
                    [1.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                ])
                .unwrap(),
            ),
        };
        let through_identity = render_linear(
            &registry,
            &source,
            SnapshotId::new(),
            &Recipe::default(),
            identity,
        )
        .unwrap();
        assert_eq!(through_identity.rgba, raster.rgba);
    }

    /// The approximation multiplies each source pixel by `W` before the exposure: the byte is the
    /// independent `sRGB(2^EV · (W · p))`, so a colour layer after it sees the approximated scene
    /// value exactly as it sees an exact one.
    #[test]
    fn the_approximation_applies_its_matrix_before_the_exposure() {
        let matrix = [[1.3, 0.1, -0.05], [0.02, 0.97, 0.01], [-0.1, 0.05, 0.62]];
        let settings = LinearSettings {
            exposure_ev: 1.0,
            white_balance: Some(WhiteBalanceApproximation::from_matrix(matrix).unwrap()),
        };
        let source = varied(6, 5);
        let registry = ModuleRegistry::builtin();
        let raster = render_linear(
            &registry,
            &source,
            SnapshotId::new(),
            &Recipe::default(),
            settings,
        )
        .unwrap();
        for y in 0..5 {
            for x in 0..6 {
                let pixel = source.pixel(x, y).unwrap().map(f64::from);
                let expected = apply(matrix, pixel).map(|value| reference_srgb(2.0 * value));
                assert_eq!(
                    raster.pixel(x, y),
                    Some([expected[0], expected[1], expected[2], 255]),
                    "({x}, {y})"
                );
                // The point sampler takes the same path, so the readout of an approximate stack
                // would agree with its frame — though the host never asks it to.
                assert_eq!(
                    sample_linear(&registry, &source, &Recipe::default(), settings, x, y)
                        .unwrap()
                        .rgba,
                    raster.pixel(x, y)
                );
            }
        }
    }

    /// `W = R · diag(g'/g) · R⁻¹`: a camera-RGB pixel `c` developed at `g` is `R · c`, and the one
    /// developed at `g'` is, to first order, `R · diag(g'/g) · c`, which `W` must reach from the
    /// first. Equal gains are the identity to rounding.
    #[test]
    fn the_matrix_maps_one_development_onto_the_other_in_camera_space() {
        let developed = [2.1_f32, 1.0, 1.45];
        let target = [1.52_f32, 1.0, 2.37];
        let balance = WhiteBalanceApproximation::between(CAMERA, developed, target).unwrap();
        let ratio: [f64; 3] =
            std::array::from_fn(|c| f64::from(target[c]) / f64::from(developed[c]));
        for camera in [
            [0.2, 0.4, 0.1],
            [1.3, 0.05, 0.9],
            [0.0, 0.0, 1.0],
            [-0.02, 0.7, 0.33],
        ] {
            let at_developed = apply(CAMERA, camera);
            let at_target = apply(CAMERA, std::array::from_fn(|c| camera[c] * ratio[c]));
            let approximated = balance.apply(at_developed);
            for channel in 0..3 {
                assert!(
                    (approximated[channel] - at_target[channel]).abs() < 1.0e-12,
                    "{camera:?} channel {channel}: {approximated:?} against {at_target:?}"
                );
            }
        }
        let same = WhiteBalanceApproximation::between(CAMERA, developed, developed).unwrap();
        for (row, values) in same.matrix().iter().enumerate() {
            for (column, value) in values.iter().enumerate() {
                let identity = if row == column { 1.0 } else { 0.0 };
                assert!(
                    (value - identity).abs() < 1.0e-12,
                    "{row},{column}: {value}"
                );
            }
        }
    }

    /// No approximation exists for a singular or non-finite camera matrix, for a gain that is not
    /// finite and positive, or for a non-finite matrix given directly; each is refused rather than
    /// rendering a frame the matrix cannot describe.
    #[test]
    fn a_singular_matrix_or_unusable_gain_has_no_approximation() {
        let gains = [2.0_f32, 1.0, 1.5];
        let rank_two = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.5, -1.0, 0.25]];
        let error = WhiteBalanceApproximation::between(rank_two, gains, [1.0, 1.0, 1.0])
            .expect_err("a rank-two camera matrix has no inverse");
        assert_eq!(error.kind, ErrorKind::UnsupportedColor);
        // Nearly singular: a determinant of 1e-15 against unit rows.
        let nearly = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 1.0e-15]];
        assert!(WhiteBalanceApproximation::between(nearly, gains, [1.0, 1.0, 1.0]).is_err());
        let mut infinite = CAMERA;
        infinite[1][2] = f64::INFINITY;
        assert!(WhiteBalanceApproximation::between(infinite, gains, [1.0, 1.0, 1.0]).is_err());
        for bad in [0.0_f32, -1.0, f32::NAN, f32::INFINITY] {
            assert!(WhiteBalanceApproximation::between(CAMERA, [bad, 1.0, 1.0], gains).is_err());
            assert!(WhiteBalanceApproximation::between(CAMERA, gains, [1.0, 1.0, bad]).is_err());
        }
        let mut matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        matrix[2][0] = f64::NAN;
        assert!(WhiteBalanceApproximation::from_matrix(matrix).is_err());
    }

    #[test]
    fn malformed_sources_views_and_multiple_resamples_fail_closed() {
        assert!(LinearImage::new(2, 2, vec![0.0; 11]).is_err());
        assert!(LinearImage::new(2, 2, vec![f32::NAN; 12]).is_err());
        assert!(LinearImage::new(0, 1, Vec::<f32>::new()).is_err());
        assert!(LinearImage::new(16_385, 1, Vec::<f32>::new()).is_err());
        assert!(output_len(16_000, 8_001).is_err());
        let source = image(2, 2, &[[0.0, 0.0, 0.0]; 4]);
        assert!(source.with_view([1, 1, 2, 2], 1).is_err());
        assert!(source.with_view([u32::MAX, 0, 2, 1], 1).is_err());
        assert!(source.with_view([0, 0, 2, 2], 9).is_err());
        assert!(
            LinearSettings {
                exposure_ev: 5.1,
                white_balance: None,
            }
            .multiplier()
            .is_err()
        );
        assert!(linear_bilinear(1.0, 1.0, 2, 2, |_x, _y| Ok([f64::INFINITY; 3])).is_err());
    }

    /// A synthetic linear source with a varying value in all three channels, filled
    /// programmatically so no file is read.
    fn cancellation_image(width: u32, height: u32) -> LinearImage {
        let pixels = (width * height) as usize;
        let mut planes = Vec::with_capacity(pixels * 3);
        for channel in 0..3 {
            planes.extend((0..pixels).map(|index| ((index * (channel + 1)) % 997) as f32 / 997.0));
        }
        LinearImage::with_fingerprint(width, height, planes, "sha256:linear-cancellation").unwrap()
    }

    fn cancellation_recipe() -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer {
                id: crate::LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload: serde_json::json!({"exposure": 0.5, "contrast": 20.0, "vibrance": 30.0}),
            }],
        }
    }

    #[test]
    fn an_uncancelled_token_renders_the_linear_bytes_the_plain_entry_point_renders() {
        let registry = ModuleRegistry::builtin();
        let source = cancellation_image(160, 120);
        let recipe = cancellation_recipe();
        let snapshot = SnapshotId::new();
        let plain = render_linear(
            &registry,
            &source,
            snapshot.clone(),
            &recipe,
            LinearSettings::default(),
        )
        .unwrap();
        let cancellable = render_linear_cancellable(
            &registry,
            &source,
            snapshot,
            &recipe,
            LinearSettings::default(),
            &Cancel::never(),
        )
        .unwrap();
        assert_eq!(plain, cancellable);
    }

    #[test]
    fn a_pre_cancelled_token_stops_a_linear_render_before_it_allocates_a_frame() {
        let cancel = Cancel::new();
        cancel.cancel();
        let error = render_linear_cancellable(
            &ModuleRegistry::builtin(),
            &cancellation_image(160, 120),
            SnapshotId::new(),
            &cancellation_recipe(),
            LinearSettings::default(),
            &cancel,
        )
        .expect_err("a cancelled token refuses the linear render");
        assert_eq!(error.kind, crate::ErrorKind::Cancelled);
    }
}
