//! High precision scene-linear rendering for prepared RAW sources.
//!
//! This module is deliberately independent of a RAW decoder. A decoder or source-preparation
//! worker supplies immutable planar RGB values in unbounded linear sRGB/D65. The recipe is then
//! evaluated in f64 and converted to the existing byte [`Raster`] only at the terminal boundary.
//! [`Linear`] is this path's [`PixelDomain`]: the one pipeline in [`super::pipeline`] evaluates it
//! exactly as it evaluates a JPEG's bytes, and only what a linear pixel is lives here.

use super::{
    Cancel, ColorRun, Compiled, Entry, Evaluation, PixelDomain, Raster, RenderContext, RowScratch,
    Segment, SegmentRows, SpatialMode, Taps, apply_units, segment_pass, spatial,
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    Error, SnapshotId,
    colour::{mat3, srgb},
    modules::{Parallelism, Region, Resample, Stage},
};
use std::{borrow::Cow, sync::Arc};

const MAX_PIXELS: u64 = lightwell_raw::MAX_PIXELS as u64;
const MAX_SIDE: u32 = 16_384;
const MAX_SOURCE_BYTES: u64 = lightwell_raw::MAX_RGB_BYTES as u64;
const MAX_RGBA_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RESAMPLES: usize = 1;

fn layout(width: u32, height: u32) -> Result<(usize, usize), Error> {
    if width == 0 || height == 0 {
        return Err(Error::validation(
            "linear source dimensions must be nonzero",
        ));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::resource_limit(
            "linear source side exceeds 16384 pixels",
        ));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| Error::resource_limit("linear source dimensions overflow"))?;
    if pixels > MAX_PIXELS {
        return Err(Error::resource_limit(
            "linear source exceeds 128 megapixels",
        ));
    }
    let values = pixels
        .checked_mul(3)
        .ok_or_else(|| Error::resource_limit("linear source plane length overflow"))?;
    let bytes = values
        .checked_mul(u64::from(std::mem::size_of::<f32>() as u32))
        .ok_or_else(|| Error::resource_limit("linear source byte length overflow"))?;
    if bytes > MAX_SOURCE_BYTES {
        return Err(Error::resource_limit("linear RGB source exceeds 1.5 GiB"));
    }
    let values = usize::try_from(values)
        .map_err(|_| Error::resource_limit("linear source is not addressable"))?;
    let plane_len = usize::try_from(pixels)
        .map_err(|_| Error::resource_limit("linear source plane is not addressable"))?;
    Ok((values, plane_len))
}

pub(super) fn output_len(width: u32, height: u32) -> Result<usize, Error> {
    if width == 0 || height == 0 {
        return Err(Error::validation(
            "linear output dimensions must be nonzero",
        ));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::resource_limit(
            "linear output side exceeds 16384 pixels",
        ));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| Error::resource_limit("linear output dimensions overflow"))?;
    if pixels > lightwell_raw::MAX_PIXELS as u64 {
        return Err(Error::resource_limit(
            "linear output exceeds 128 megapixels",
        ));
    }
    let bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| Error::resource_limit("linear output byte length overflow"))?;
    if bytes > MAX_RGBA_BYTES {
        return Err(Error::resource_limit("linear output exceeds 512 MiB"));
    }
    usize::try_from(bytes).map_err(|_| Error::resource_limit("linear output is not addressable"))
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

    #[inline]
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
    /// The source worker's hold on these planes ([`crate::source::PlaneGate`]), shared by every
    /// view and clone. Declared after `planes`, so the planes are freed before it is released.
    held: crate::source::PlanesHeld,
}

/// The source of every [`LinearImage::development`] number. It is a development's identity, not
/// render state: process-unique, so a proxy cache keyed by it can never mistake one development's
/// planes for another's, whichever render context evaluates them.
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
        Self::construct(width, height, planes.into(), fingerprint.into(), false)
    }

    /// Adopt planes whose producer has already checked every output value after its final math.
    /// The RAW camera conversion is the only caller. This remains private to the core: public
    /// constructors must scan untrusted input and reject non-finite values.
    pub(crate) fn from_validated_planes(
        width: u32,
        height: u32,
        planes: Vec<f32>,
        fingerprint: String,
    ) -> Result<Self, Error> {
        Self::construct(width, height, Arc::new(planes), fingerprint, true)
    }

    fn construct(
        width: u32,
        height: u32,
        planes: Arc<Vec<f32>>,
        fingerprint: String,
        already_finite: bool,
    ) -> Result<Self, Error> {
        let (expected, _) = layout(width, height)?;
        if planes.len() != expected {
            return Err(Error::validation(format!(
                "linear source needs {expected} planar values"
            )));
        }
        let capacity_bytes = u64::try_from(planes.capacity())
            .ok()
            .and_then(|capacity| capacity.checked_mul(u64::from(std::mem::size_of::<f32>() as u32)))
            .ok_or_else(|| Error::resource_limit("linear source capacity overflows"))?;
        if capacity_bytes > MAX_SOURCE_BYTES {
            return Err(Error::resource_limit(
                "linear RGB source capacity exceeds 1.5 GiB",
            ));
        }
        if !already_finite && planes.iter().any(|value| !value.is_finite()) {
            return Err(Error::validation(
                "linear source contains a non-finite value",
            ));
        }
        Ok(Self {
            base_width: width,
            base_height: height,
            planes,
            fingerprint,
            view: View {
                x: 0,
                y: 0,
                width,
                height,
                orientation: 1,
            },
            development: NEXT_DEVELOPMENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            held: crate::source::PlanesHeld::default(),
        })
    }

    /// Return a cropped/oriented view without copying the source planes.
    ///
    /// `crop` is `[x, y, width, height]` in the base source-plane coordinates. EXIF orientation
    /// values 1 through 8 use the standard mappings and are applied exactly once to that crop.
    pub fn with_view(&self, crop: [u32; 4], orientation: u8) -> Result<Self, Error> {
        let [x, y, width, height] = crop;
        if !(1..=8).contains(&orientation) {
            return Err(Error::validation(
                "linear source orientation must be EXIF 1 through 8",
            ));
        }
        let right = x
            .checked_add(width)
            .ok_or_else(|| Error::resource_limit("linear crop exceeds dimensions"))?;
        let bottom = y
            .checked_add(height)
            .ok_or_else(|| Error::resource_limit("linear crop exceeds dimensions"))?;
        if width == 0 || height == 0 || right > self.base_width || bottom > self.base_height {
            return Err(Error::validation("linear crop lies outside source planes"));
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
            held: self.held.clone(),
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

    /// Make the source worker's memory gate wait for these planes: every view and clone taken
    /// from now on shares the hold, and the gate opens when the last of them drops. Called once,
    /// before the development is shared.
    pub(crate) fn hold(&mut self, lease: crate::source::PlaneLease) {
        debug_assert_eq!(Arc::strong_count(&self.planes), 1, "hold before sharing");
        self.held = crate::source::PlanesHeld::new(lease);
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
    #[inline]
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

    #[inline(always)]
    fn pixel_f64(&self, x: u32, y: u32) -> Result<[f64; 3], Error> {
        let pixel = self.pixel(x, y).ok_or_else(|| {
            Error::validation(format!(
                "linear source coordinate ({x}, {y}) is outside the view"
            ))
        })?;
        let pixel = pixel.map(f64::from);
        if pixel.iter().all(|value| value.is_finite()) {
            Ok(pixel)
        } else {
            Err(Error::render("linear source produced a non-finite pixel"))
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

    /// A viewed row with its mapping resolved once. Construction validated the crop, orientation
    /// and plane lengths, so its pixels advance by one column or one base-plane row, in either
    /// direction. No pixel data is copied or allocated to make this iterator.
    fn row(&self, y: u32) -> Option<impl ExactSizeIterator<Item = [f32; 3]> + '_> {
        if y >= self.height {
            return None;
        }
        let (base_x, base_y) = self.view.map(0, y)?;
        let first = base_y as usize * self.base_width as usize + base_x as usize;
        let stride = match self.view.orientation {
            1 | 4 => 1,
            2 | 3 => -1,
            5 | 8 => self.base_width as isize,
            6 | 7 => -(self.base_width as isize),
            _ => return None,
        };
        Some((0..self.width).map(move |x| {
            // Every addressed index is inside the validated crop. The signed offset represents
            // reversed rows as well; it never wraps the resulting address outside the plane.
            let index = first.wrapping_add_signed(x as isize * stride);
            [
                self.planes[index],
                self.planes[self.plane_len + index],
                self.planes[2 * self.plane_len + index],
            ]
        }))
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
                return Err(Error::validation(
                    "white-balance gains must be finite and positive",
                ));
            }
            *value = to / from;
        }
        let inverse = mat3::hadamard_checked_inverse(camera_to_srgb)?;
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
            Err(Error::validation(
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
        mat3::matvec_f64(&self.matrix, pixel)
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

impl LinearSettings {
    pub(super) fn multiplier(self) -> Result<f64, Error> {
        if !self.exposure_ev.is_finite() || !(-5.0..=5.0).contains(&self.exposure_ev) {
            return Err(Error::validation(
                "linear exposure must be finite and between -5 and +5 EV",
            ));
        }
        let multiplier = self.exposure_ev.exp2();
        if multiplier.is_finite() {
            Ok(multiplier)
        } else {
            Err(Error::render("linear exposure multiplier overflow"))
        }
    }
}

#[inline]
fn decode_rgb(value: [u8; 3]) -> [f64; 3] {
    value.map(srgb::decode_u8)
}

fn terminal_srgb(linear: f64) -> Result<u8, Error> {
    if !linear.is_finite() {
        return Err(Error::render(
            "linear evaluation produced a non-finite value",
        ));
    }
    let linear = linear.clamp(0.0, 1.0);
    let code = srgb::quantize_channel(linear);
    // Inverting the half-code thresholds avoids a power function for ordinary values, but
    // f64 encode/decode are not exact inverses. Keep the canonical forward evaluation close to
    // either neighbouring threshold. This conservative guard is covered by native boundary
    // tests; powf has no cross-platform ULP bound, so those tests remain part of platform
    // qualification. The JPEG quantizer keeps its own contract.
    const ROUNDING_GUARD: f64 = 1e-12;
    let thresholds = &*srgb::CODE_THRESHOLDS;
    let lower = thresholds[usize::from(code.saturating_sub(1))];
    let upper = thresholds[usize::from(code.min(254))];
    if (linear - lower).abs() > ROUNDING_GUARD && (linear - upper).abs() > ROUNDING_GUARD {
        return Ok(code);
    }
    let encoded = srgb::encode(linear);
    let rounded = (encoded * 255.0).round();
    if !rounded.is_finite() || !(0.0..=255.0).contains(&rounded) {
        return Err(Error::render("terminal sRGB conversion overflow"));
    }
    Ok(rounded as u8)
}

/// Refuse a stack the linear path cannot evaluate: more than one resample stage.
pub(super) fn check_resamples(compiled: &Compiled) -> Result<(), Error> {
    let resamples = compiled
        .segments
        .iter()
        .filter(|segment| matches!(segment.entry.as_ref().map(Entry::resample), Some(Some(_))))
        .count();
    if resamples > MAX_RESAMPLES {
        return Err(Error::validation(
            "linear evaluation supports at most one resample stage",
        ));
    }
    Ok(())
}

/// The linear domain: a developed RAW's planes in signed unbounded linear sRGB, with the exposure
/// and any approximate white balance applied to each source pixel in `f64`. A segment with colour
/// is `f32` from its entry to its end and one without stays `f64`; nothing is quantized before the
/// terminal boundary, so a replacement is decoded rather than quantized at, a resample blends in
/// `f64`, and a spatial operation's `f32` output is read back exactly. There is no alpha.
#[derive(Clone, Copy)]
pub(crate) struct Linear<'a> {
    source: &'a LinearImage,
    exposure_multiplier: f64,
    /// Applied to each source pixel before the exposure multiply, when the settings carry one.
    white_balance: Option<WhiteBalanceApproximation>,
}

impl<'a> Linear<'a> {
    /// `source` under `settings`, refused when the exposure is out of range.
    pub(crate) fn new(source: &'a LinearImage, settings: LinearSettings) -> Result<Self, Error> {
        Ok(Self {
            source,
            exposure_multiplier: settings.multiplier()?,
            white_balance: settings.white_balance,
        })
    }

    /// The one point where the settings touch a source pixel: `exposure · p`, or
    /// `exposure · (W · p)` under an approximate white balance. Shared by the point evaluation and
    /// the rendered rows, so both keep the exact f64 WB-then-exposure order and the same
    /// finite-result failure.
    #[inline(always)]
    fn adjust_source_pixel(&self, pixel: [f64; 3]) -> Result<[f64; 3], Error> {
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
            Err(Error::render("linear exposure produced a non-finite value"))
        }
    }
}

impl PixelDomain for Linear<'_> {
    type Pixel = [f64; 3];
    /// Three `f32` planes inside the RAW planar limit.
    type SpatialFrame = Vec<f32>;
    type TileOutput = (Region, Vec<f32>);

    const GRID: SpatialMode = SpatialMode::Frames;

    fn fingerprint(&self) -> &str {
        self.source.fingerprint()
    }

    /// Fingerprint alone does not identify developed pixels: public callers may omit it, two
    /// developments of a file differ, and crop/orientation views share their source's identity.
    /// Direct settings can also change exposure without changing the recipe prefix. And the
    /// estimate store is keyed by the recipe prefix, which an approximate white balance does not
    /// change: the drafted recipe names the target gains whichever planes it is evaluated over. So
    /// an approximate evaluation keys its estimates apart, and a committed render of the same
    /// recipe never takes one estimated from approximate pixels.
    fn estimate_prefix<'p>(&self, prefix_hash: &'p str) -> Cow<'p, str> {
        let input_prefix = format!(
            "{prefix_hash}+linear:{}:{:?}:{:016x}",
            self.source.development(),
            self.source.view(),
            self.exposure_multiplier.to_bits()
        );
        Cow::Owned(match self.white_balance {
            Some(balance) => format!(
                "{input_prefix}+white-balance-approximation:{}",
                balance.key()
            ),
            None => input_prefix,
        })
    }

    fn check(&self, compiled: &Compiled) -> Result<(), Error> {
        check_resamples(compiled)
    }

    fn check_output(&self, width: u32, height: u32) -> Result<(), Error> {
        output_len(width, height).map(drop)
    }

    #[inline(always)]
    fn source_pixel(&self, x: u32, y: u32) -> Result<[f64; 3], Error> {
        self.adjust_source_pixel(self.source.pixel_f64(x, y)?)
    }

    fn source_alpha(&self, _: u32, _: u32) -> u8 {
        255
    }

    #[inline]
    fn replace(_: [f64; 3], rgb: [u8; 3]) -> [f64; 3] {
        decode_rgb(rgb)
    }

    /// The colour phases are the byte path's, applied to one linear pixel: every run processes the
    /// value in place and nothing is quantized between runs, which is the whole point of the linear
    /// path: a scene value above 1 or below 0 survives to the next unit and only the terminal
    /// boundary encodes it.
    #[inline(always)]
    fn colour<'r>(
        pixel: [f64; 3],
        runs: impl Iterator<Item = ColorRun<'r>>,
        x: u32,
        y: u32,
    ) -> Result<[f64; 3], Error> {
        let mut linear = [pixel.map(|value| value as f32)];
        // One pixel of snapshot scratch on the stack: a masked operation blends against its own
        // input, and a point pulls single pixels, so nothing is allocated per pixel.
        let mut scratch = [[0.0f32; 3]; 1];
        for run in runs {
            apply_units(&run, y, x, &mut linear, &mut scratch)?;
        }
        Ok(linear[0].map(f64::from))
    }

    /// The row form of [`Self::colour`]: one `f32` row through every run, which is what the rows
    /// of a rendered segment do too.
    fn colour_row<'r>(
        pixels: &mut [[f64; 3]],
        runs: impl Iterator<Item = ColorRun<'r>> + Clone,
        y: u32,
        x0: u32,
        scratch: &mut RowScratch,
    ) -> Result<(), Error> {
        let RowScratch { linear, snapshot } = scratch;
        linear.clear();
        linear.extend(pixels.iter().map(|pixel| pixel.map(|value| value as f32)));
        snapshot.resize(pixels.len().max(1), [0.0; 3]);
        for run in runs {
            apply_units(&run, y, x0, linear, snapshot)?;
        }
        for (pixel, value) in pixels.iter_mut().zip(linear.iter()) {
            *pixel = Self::finish(value.map(f64::from))?;
        }
        Ok(())
    }

    #[inline(always)]
    fn finish(pixel: [f64; 3]) -> Result<[f64; 3], Error> {
        if pixel.iter().all(|value| value.is_finite()) {
            Ok(pixel)
        } else {
            Err(Error::render(
                "linear evaluation produced a non-finite pixel",
            ))
        }
    }

    #[inline(always)]
    fn blend(
        u: f64,
        v: f64,
        width: u32,
        height: u32,
        mut fetch: impl FnMut(u32, u32) -> Result<[f64; 3], Error>,
    ) -> Result<[f64; 3], Error> {
        if !u.is_finite() || !v.is_finite() || width == 0 || height == 0 {
            return Err(Error::render(
                "linear resample has invalid coordinates or dimensions",
            ));
        }
        let taps = Taps::new(u, v, width, height);
        let [top_left, top_right, bottom_left, bottom_right] = taps.corners;
        let [w0, w1, w2, w3] = taps.weights;
        let corners = [
            (fetch(top_left.0, top_left.1)?, w0),
            (fetch(top_right.0, top_right.1)?, w1),
            (fetch(bottom_left.0, bottom_left.1)?, w2),
            (fetch(bottom_right.0, bottom_right.1)?, w3),
        ];
        let output = std::array::from_fn(|channel| {
            corners
                .iter()
                .map(|(pixel, weight)| pixel[channel] * weight)
                .sum::<f64>()
        });
        if output.iter().all(|value: &f64| value.is_finite()) {
            Ok(output)
        } else {
            Err(Error::render("linear resample produced a non-finite value"))
        }
    }

    #[inline]
    fn spatial_input(pixel: [f64; 3]) -> [f32; 3] {
        pixel.map(|value| value as f32)
    }

    #[inline]
    fn spatial_output(rgb: [f32; 3], _: impl FnOnce() -> u8) -> Result<[f64; 3], Error> {
        Ok(rgb.map(f64::from))
    }

    #[inline]
    fn terminal(pixel: [f64; 3]) -> Result<[u8; 4], Error> {
        terminal_pixel(pixel)
    }

    #[inline]
    fn mask_input(pixel: [f64; 3]) -> [f64; 3] {
        pixel
    }

    /// The RAW planar limit applies to this float frame exactly as it does to the source's.
    fn spatial_frame(stage: Stage) -> Result<Vec<f32>, Error> {
        let (values, _) = layout(stage.width, stage.height)?;
        Ok(vec![0.0_f32; values])
    }

    fn tile_output(
        region: Region,
        values: Vec<f32>,
        _: Region,
        _: Parallelism,
    ) -> (Region, Vec<f32>) {
        (region, values)
    }

    fn write_tile(
        frame: &mut Vec<f32>,
        stage: Stage,
        tile: Region,
        (region, values): (Region, Vec<f32>),
        _: &impl Fn(u32, u32) -> u8,
    ) {
        let plane = (u64::from(stage.width) * u64::from(stage.height)) as usize;
        for y in tile.y0..tile.y1() {
            for x in tile.x0..tile.x1() {
                let pixel = spatial::plane_pixel(region, &values, x, y);
                let offset = (u64::from(y) * u64::from(stage.width) + u64::from(x)) as usize;
                frame[offset] = pixel[0];
                frame[plane + offset] = pixel[1];
                frame[2 * plane + offset] = pixel[2];
            }
        }
    }

    #[inline]
    fn frame_pixel(frame: &Vec<f32>, stage: Stage, x: u32, y: u32) -> [f64; 3] {
        let plane = (u64::from(stage.width) * u64::from(stage.height)) as usize;
        let offset = (u64::from(y) * u64::from(stage.width) + u64::from(x)) as usize;
        [
            f64::from(frame[offset]),
            f64::from(frame[plane + offset]),
            f64::from(frame[2 * plane + offset]),
        ]
    }
}

#[inline]
pub(super) fn terminal_pixel(pixel: [f64; 3]) -> Result<[u8; 4], Error> {
    Ok([
        terminal_srgb(pixel[0])?,
        terminal_srgb(pixel[1])?,
        terminal_srgb(pixel[2])?,
        255,
    ])
}

/// The whole frame of a frames-mode evaluation, terminally produced as bytes: the linear path's half
/// of [`super::Render::frame`], for the exact phase and the proxy phase alike.
///
/// This driver materializes only the last segment's output, as terminal bytes, in one
/// [`segment_pass`] whose rows pull their entry through the evaluation: the source's rows directly
/// when the stack is one segment read through the identity, and otherwise each pixel through the
/// geometry from the source, the evaluation's spatial frame or a resample of the segment before.
/// What lies before a resample is pulled, never materialized, because it is `f64`: its taps are
/// read one block of output pixels at a time, through the rectangle of the segment before them
/// that the block reads ([`LinearRows::load_resampled`]).
pub(super) fn rasterize(
    evaluation: &Evaluation<'_, Linear<'_>>,
    snapshot_id: SnapshotId,
    cancel: &Cancel,
    context: &RenderContext,
) -> Result<Raster, Error> {
    let stage = evaluation.stage();
    // Write into the Arc-backed frame that the raster returns, avoiding an output publication copy.
    let mut frame = super::zeroed_frame(output_len(stage.width, stage.height)?);
    let index = evaluation.compiled.segments.len() - 1;
    let segment = &evaluation.compiled.segments[index];
    let source = evaluation.domain.source;
    let reader = (segment.entry.is_none()
        && segment
            .geometry
            .is_identity(source.width(), source.height()))
    .then(|| source.reader());
    segment_pass(
        &LinearRows {
            evaluation,
            index,
            segment,
            reader,
        },
        segment,
        super::frame_mut(&mut frame),
        0..stage.height as usize,
        cancel,
        context.scratch(),
    )?;
    Ok(Raster {
        width: stage.width,
        height: stage.height,
        rgba: frame,
        source_fingerprint: source.fingerprint.clone(),
        snapshot_id,
    })
}

/// How many output columns one block of a resampled segment's rows reads its taps for at once.
const TAP_BLOCK_COLUMNS: u32 = 64;

/// The most pixels of the segment before a resample one block holds. A block of up to 16 rows by
/// 64 columns reads about 1,400 of them at a small angle and about 3,500 at the 45 degree limit;
/// a mapping that would need more than this reads its taps one at a time instead.
const TAP_BLOCK_PIXELS: u64 = 16 * 1024;

/// The last segment's rows on the linear path. A segment with colour holds its rows as `f32`
/// between its entry and the terminal boundary, exactly the value [`Linear::colour`] converts a
/// pixel to; one without colour has nothing to hold, so its entry and replacements go straight to
/// terminal bytes.
struct LinearRows<'e, 'x, 's> {
    evaluation: &'e Evaluation<'x, Linear<'s>>,
    index: usize,
    segment: &'e Segment,
    /// The source's rows, when the segment reads the source through the identity.
    reader: Option<ViewReader<'e>>,
}

/// What one worker reuses for every chunk of linear rows it takes.
#[derive(Default)]
struct LinearScratch {
    /// A colour segment's rows, in `f32`; empty for a segment without colour.
    rows: Vec<[f32; 3]>,
    /// The pixels of the segment before a resample that one block of taps reads.
    block: Vec<[f64; 3]>,
    /// One row of that segment's colour.
    row: RowScratch,
}

impl LinearRows<'_, '_, '_> {
    /// One viewed source row, which the reader resolves once instead of per pixel.
    fn source_row<'r>(
        reader: &'r ViewReader<'_>,
        y: u32,
    ) -> Result<impl ExactSizeIterator<Item = [f32; 3]> + 'r, Error> {
        reader
            .row(y)
            .ok_or_else(|| Error::render("linear output coordinate was outside stage"))
    }

    /// The segment's entry value under output pixel `(x, y)`, through its exact geometry.
    #[inline]
    fn entry(&self, x: u32, y: u32) -> Result<[f64; 3], Error> {
        let (input_x, input_y) = self.segment.geometry.unmap(x, y);
        self.evaluation.entry_pixel(self.index, input_x, input_y)
    }

    /// Hand one entry value to the chunk: as `f32` to a colour segment's rows, or as terminal
    /// bytes when the segment has no colour.
    #[inline]
    fn put(
        &self,
        rows: &mut [[f32; 3]],
        chunk: &mut [u8],
        offset: usize,
        pixel: [f64; 3],
    ) -> Result<(), Error> {
        if self.segment.has_color {
            rows[offset] = pixel.map(|value| value as f32);
        } else {
            chunk[offset * 4..offset * 4 + 4].copy_from_slice(&terminal_pixel(pixel)?);
        }
        Ok(())
    }

    /// The rectangle of the segment before `resample` whose pixels the taps of output columns
    /// `x0..x0 + columns` of rows `y0..y0 + rows` read, with a pixel to spare on each side, or
    /// `None` when it is not finite or holds more than [`TAP_BLOCK_PIXELS`]. The segment's exact
    /// geometry maps the block onto a rectangle and the resample is affine, so its corners bound
    /// every tap.
    fn tap_region(
        &self,
        resample: Resample,
        previous: Stage,
        (x0, y0): (u32, u32),
        (columns, rows): (u32, u32),
    ) -> Option<Region> {
        let mut low = [f64::INFINITY; 2];
        let mut high = [f64::NEG_INFINITY; 2];
        for (x, y) in [
            (x0, y0),
            (x0 + columns - 1, y0),
            (x0, y0 + rows - 1),
            (x0 + columns - 1, y0 + rows - 1),
        ] {
            let (input_x, input_y) = self.segment.geometry.unmap(x, y);
            let (u, v) = resample.input_at(input_x, input_y);
            for (axis, value) in [u, v].into_iter().enumerate() {
                let index = (value - 0.5).floor();
                if !index.is_finite() {
                    return None;
                }
                low[axis] = low[axis].min(index - 1.0);
                high[axis] = high[axis].max(index + 2.0);
            }
        }
        let clamp = |value: f64, limit: u32| value.max(0.0).min(f64::from(limit - 1)) as u32;
        let (left, right) = (
            clamp(low[0], previous.width),
            clamp(high[0], previous.width),
        );
        let (top, bottom) = (
            clamp(low[1], previous.height),
            clamp(high[1], previous.height),
        );
        let region = Region {
            x0: left,
            y0: top,
            width: right - left + 1,
            height: bottom - top + 1,
        };
        (region.pixels() <= TAP_BLOCK_PIXELS).then_some(region)
    }

    /// A resampled segment's rows, in blocks of [`TAP_BLOCK_COLUMNS`] columns: each block reads
    /// the rectangle of the segment before the resample its taps need once, through
    /// [`Evaluation::region_in`], and blends every output pixel from it with the resample's own
    /// [`Linear::blend`]. Every tap is the value [`Evaluation::pixel_in`] answers there, so the
    /// result is [`Evaluation::entry_pixel`]'s, while each pixel before the resample is evaluated
    /// about once per block instead of once per tap and its colour runs over rows.
    fn load_resampled(
        &self,
        resample: Resample,
        scratch: &mut LinearScratch,
        y0: u32,
        chunk: &mut [u8],
    ) -> Result<(), Error> {
        let width = self.segment.width;
        let rows = (chunk.len() / (width as usize * 4)) as u32;
        let previous = &self.evaluation.compiled.segments[self.index - 1];
        let stage = Stage {
            width: previous.width,
            height: previous.height,
        };
        let outside = || Error::render("a resample tap was outside its stage");
        let LinearScratch {
            rows: values,
            block,
            row,
        } = scratch;
        for x0 in (0..width).step_by(TAP_BLOCK_COLUMNS as usize) {
            let columns = (width - x0).min(TAP_BLOCK_COLUMNS);
            let held = self.tap_region(resample, stage, (x0, y0), (columns, rows));
            if let Some(region) = held {
                self.evaluation
                    .region_in(self.index - 1, region, block, row)?;
            }
            for y in y0..y0 + rows {
                for x in x0..x0 + columns {
                    let (input_x, input_y) = self.segment.geometry.unmap(x, y);
                    let (u, v) = resample.input_at(input_x, input_y);
                    let pixel =
                        Linear::blend(u, v, stage.width, stage.height, |x, y| match held {
                            Some(region) if region.contains(x, y) => {
                                Ok(block
                                    [((y - region.y0) * region.width + (x - region.x0)) as usize])
                            }
                            _ => self
                                .evaluation
                                .pixel_in(self.index - 1, x, y)?
                                .ok_or_else(outside),
                        })?;
                    let offset = ((y - y0) * width + x) as usize;
                    self.put(values, chunk, offset, pixel)?;
                }
            }
        }
        Ok(())
    }
}

impl SegmentRows for LinearRows<'_, '_, '_> {
    type Scratch = LinearScratch;

    fn scratch_bytes(&self, width: usize, rows: usize, _: usize) -> usize {
        let colour = if self.segment.has_color {
            rows * width * std::mem::size_of::<[f32; 3]>()
        } else {
            0
        };
        let taps = match self.segment.entry {
            Some(Entry::Resample(_)) => TAP_BLOCK_PIXELS as usize * std::mem::size_of::<[f64; 3]>(),
            _ => 0,
        };
        colour + taps
    }

    fn load(&self, scratch: &mut Self::Scratch, y0: u32, chunk: &mut [u8]) -> Result<(), Error> {
        let width = self.segment.width as usize;
        let domain = &self.evaluation.domain;
        if self.segment.has_color {
            scratch.rows.clear();
            scratch.rows.resize(chunk.len() / 4, [0.0; 3]);
        }
        if let Some(Entry::Resample(resample)) = self.segment.entry {
            return self.load_resampled(resample, scratch, y0, chunk);
        }
        for (row, bytes) in chunk.chunks_exact_mut(width * 4).enumerate() {
            let y = y0 + row as u32;
            let values = &mut scratch.rows;
            // Immutable source planes were checked finite on construction. A source row widens at
            // the same boundary as the point path's source pixel.
            match (&self.reader, self.segment.has_color) {
                (Some(reader), true) => {
                    for (pixel, value) in Self::source_row(reader, y)?
                        .zip(values[row * width..(row + 1) * width].iter_mut())
                    {
                        let pixel = domain.adjust_source_pixel(pixel.map(f64::from))?;
                        *value = pixel.map(|value| value as f32);
                    }
                }
                (Some(reader), false) => {
                    for (pixel, rgba) in Self::source_row(reader, y)?.zip(bytes.chunks_exact_mut(4))
                    {
                        let pixel = domain.adjust_source_pixel(pixel.map(f64::from))?;
                        rgba.copy_from_slice(&terminal_pixel(pixel)?);
                    }
                }
                (None, true) => {
                    for (x, value) in values[row * width..(row + 1) * width]
                        .iter_mut()
                        .enumerate()
                    {
                        *value = self.entry(x as u32, y)?.map(|value| value as f32);
                    }
                }
                (None, false) => {
                    for (x, rgba) in bytes.chunks_exact_mut(4).enumerate() {
                        rgba.copy_from_slice(&terminal_pixel(self.entry(x as u32, y)?)?);
                    }
                }
            }
        }
        Ok(())
    }

    fn replace(
        &self,
        scratch: &mut Self::Scratch,
        chunk: &mut [u8],
        offset: usize,
        rgb: [u8; 3],
    ) -> Result<(), Error> {
        self.put(&mut scratch.rows, chunk, offset, decode_rgb(rgb))
    }

    fn run(
        &self,
        scratch: &mut Self::Scratch,
        _: &mut [u8],
        run: &ColorRun<'_>,
        y0: u32,
        rows: std::ops::Range<usize>,
        snapshot: &mut [[f32; 3]],
    ) -> Result<(), Error> {
        let width = self.segment.width as usize;
        // The same coordinates the byte path hands its units, so a position-dependent unit makes
        // a linear sample and a linear frame agree pixel for pixel.
        for (offset, row) in scratch.rows[rows.start * width..rows.end * width]
            .chunks_mut(width)
            .enumerate()
        {
            apply_units(run, y0 + (rows.start + offset) as u32, 0, row, snapshot)?;
        }
        Ok(())
    }

    fn store(&self, scratch: &mut Self::Scratch, chunk: &mut [u8]) -> Result<(), Error> {
        if self.segment.has_color {
            for (rgba, pixel) in chunk.chunks_exact_mut(4).zip(scratch.rows.iter()) {
                rgba.copy_from_slice(&terminal_pixel(pixel.map(f64::from))?);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{
        spatial::PRODUCTION_TILE,
        testing::{
            linear_evaluation, render_linear, render_linear_cancellable,
            render_linear_proxy_cancellable, sample_linear,
        },
    };
    use crate::{
        Layer, Recipe, SnapshotId,
        modules::{CropPayload, ModuleRegistry},
    };
    use rayon::prelude::*;
    use sha2::{Digest, Sha256};
    use std::time::Instant;

    fn image(width: u32, height: u32, rgb: &[[f32; 3]]) -> LinearImage {
        assert_eq!(rgb.len(), (width * height) as usize);
        let plane_len = (width * height) as usize;
        let mut planes = Vec::with_capacity(plane_len * 3);
        for channel in 0..3 {
            planes.extend(rgb.iter().map(|pixel| pixel[channel]));
        }
        LinearImage::with_fingerprint(width, height, planes, "sha256:linear-test").unwrap()
    }

    /// The point evaluator is the reference for the rendered rows: it resolves the segment, the
    /// replacement that wins and the view for every pixel, applies the colour runs to that pixel
    /// alone, never calls the source row reader, and keeps the production row scheduling
    /// threshold.
    fn generic_linear_reference(
        registry: &ModuleRegistry,
        source: &LinearImage,
        snapshot_id: SnapshotId,
        recipe: &Recipe,
        settings: LinearSettings,
    ) -> Raster {
        let evaluation = linear_evaluation(
            registry,
            source,
            recipe,
            settings,
            &Cancel::never(),
            PRODUCTION_TILE,
            SpatialMode::Frames,
        )
        .unwrap();
        let crate::modules::Stage { width, height } = evaluation.stage();
        let mut frame = super::super::zeroed_frame(output_len(width, height).unwrap());
        let rgba = super::super::frame_mut(&mut frame);
        let row_bytes = width as usize * 4;
        let render_row = |row_index: usize, row: &mut [u8]| -> Result<(), Error> {
            for (x, pixel_bytes) in row.chunks_exact_mut(4).enumerate() {
                let pixel = evaluation
                    .pixel(x as u32, row_index as u32)?
                    .ok_or_else(|| Error::render("reference pixel outside stage"))?;
                pixel_bytes.copy_from_slice(&terminal_pixel(pixel)?);
            }
            Ok(())
        };
        if u64::from(width) * u64::from(height) >= super::super::PARALLEL_RENDER_PIXELS {
            rgba.par_chunks_exact_mut(row_bytes)
                .enumerate()
                .try_for_each(|(row, pixels)| render_row(row, pixels))
                .unwrap();
        } else {
            rgba.chunks_exact_mut(row_bytes)
                .enumerate()
                .try_for_each(|(row, pixels)| render_row(row, pixels))
                .unwrap();
        }
        Raster {
            width,
            height,
            rgba: frame,
            source_fingerprint: source.fingerprint.clone(),
            snapshot_id,
        }
    }

    fn colour_layer(effect_id: &str, payload: serde_json::Value) -> Layer {
        Layer {
            id: crate::LayerId::new(),
            effect_id: effect_id.into(),
            effect_format: crate::EFFECT_FORMAT,
            payload,
            mask: None,
            artifacts: Vec::new(),
        }
    }

    fn colour_recipe(layers: Vec<Layer>) -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        }
    }

    fn percentile(values: &[f64], percentile: f64) -> f64 {
        let mut ordered = values.to_vec();
        ordered.sort_by(f64::total_cmp);
        ordered[((ordered.len() as f64 - 1.0) * percentile).ceil() as usize]
    }

    fn timed_render(
        render: impl FnOnce() -> Raster,
        sampler: &mut lightwell_process::Sampler,
    ) -> (f64, Option<f64>) {
        let cpu_before = sampler.read().cpu_time_ns.ok();
        let start = Instant::now();
        let raster = render();
        std::hint::black_box(&raster);
        drop(raster);
        let wall_ms = start.elapsed().as_secs_f64() * 1e3;
        let cpu_ms = cpu_before
            .zip(sampler.read().cpu_time_ns.ok())
            .map(|(before, after)| after.saturating_sub(before) as f64 / 1e6);
        (wall_ms, cpu_ms)
    }

    #[test]
    fn source_rows_preserve_all_views_and_f64_adjustments() {
        let mut pixels: Vec<_> = (0..99)
            .map(|index| [index as f32 / 59.0 - 0.2, index as f32 / 97.0, 0.37])
            .collect();
        pixels[0] = [-0.0, 0.0, f32::from_bits(1)];
        pixels[1] = [f32::MIN, f32::MAX, -f32::from_bits(1)];
        let source = image(11, 9, &pixels);
        let original: Vec<_> = source
            .planes()
            .iter()
            .map(|value| value.to_bits())
            .collect();
        let registry = ModuleRegistry::builtin();
        let recipe = Recipe::default();
        let balance = WhiteBalanceApproximation::from_matrix([
            [1.3, 0.1, -0.05],
            [0.02, 0.97, 0.01],
            [-0.1, 0.05, 0.62],
        ])
        .unwrap();
        for orientation in 1..=8 {
            for crop in [
                [0, 0, 11, 9],
                [2, 3, 7, 4],
                [10, 8, 1, 1],
                [0, 0, 1, 9],
                [0, 0, 11, 1],
            ] {
                let view = source.with_view(crop, orientation).unwrap();
                assert!(Arc::ptr_eq(&source.planes, &view.planes));
                let reader = view.reader();
                assert!(reader.row(view.height()).is_none());
                for y in 0..view.height() {
                    let row = reader.row(y).unwrap();
                    assert_eq!(row.len(), view.width() as usize);
                    for (x, pixel) in row.enumerate() {
                        assert_eq!(
                            pixel.map(f32::to_bits),
                            view.pixel(x as u32, y).unwrap().map(f32::to_bits),
                            "orientation {orientation}, crop {crop:?}, ({x}, {y})"
                        );
                    }
                }
                for exposure_ev in [-5.0, -0.7, 0.0, 0.7, 5.0] {
                    for white_balance in [None, Some(balance)] {
                        let settings = LinearSettings {
                            exposure_ev,
                            white_balance,
                        };
                        let snapshot = SnapshotId::new();
                        let actual =
                            render_linear(&registry, &view, snapshot.clone(), &recipe, settings)
                                .unwrap();
                        let expected =
                            generic_linear_reference(&registry, &view, snapshot, &recipe, settings);
                        assert_eq!(
                            actual, expected,
                            "orientation {orientation}, crop {crop:?}, settings {settings:?}"
                        );
                    }
                }
            }
        }
        assert_eq!(
            source
                .planes()
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            original
        );
    }

    #[test]
    fn source_rows_preserve_complete_parallel_buffers_for_each_stride() {
        let source = varied(1027, 1025);
        let registry = ModuleRegistry::builtin();
        let recipe = Recipe::default();
        let settings = LinearSettings {
            exposure_ev: 0.37,
            white_balance: None,
        };
        for orientation in [1, 2, 5, 7] {
            let view = source.with_view([1, 1, 1024, 1024], orientation).unwrap();
            let snapshot = SnapshotId::new();
            let actual =
                render_linear(&registry, &view, snapshot.clone(), &recipe, settings).unwrap();
            let expected = generic_linear_reference(&registry, &view, snapshot, &recipe, settings);
            assert_eq!(actual, expected, "orientation {orientation}");
            for (x, y) in [(0, 0), (512, 511), (1023, 1023)] {
                assert_eq!(
                    sample_linear(&registry, &view, &recipe, settings, x, y)
                        .unwrap()
                        .rgba,
                    actual.pixel(x, y)
                );
            }
        }
    }

    #[test]
    fn unmasked_basic_and_mixer_rows_match_the_generic_evaluator() {
        let source = varied(37, 29).with_view([2, 3, 30, 16], 5).unwrap();
        let registry = ModuleRegistry::builtin();
        let basic = colour_layer(
            crate::BASIC_EFFECT,
            serde_json::json!({
                "exposure": 0.5,
                "contrast": 20.0,
                "highlights": -30.0,
                "shadows": 25.0,
                "whites": 40.0,
                "blacks": -10.0,
                "vibrance": 30.0,
                "saturation": 15.0
            }),
        );
        let mixer = colour_layer(
            crate::MIXER_EFFECT,
            serde_json::json!({"red-hue": 20.0, "aqua-saturation": -35.0, "blue-luminance": 15.0}),
        );
        let recipes = [
            colour_recipe(vec![basic.clone()]),
            colour_recipe(vec![mixer.clone()]),
            colour_recipe(vec![basic, mixer]),
        ];
        let settings = LinearSettings {
            exposure_ev: -0.37,
            white_balance: Some(
                WhiteBalanceApproximation::from_matrix([
                    [1.21, -0.11, -0.02],
                    [-0.06, 1.08, -0.02],
                    [0.01, -0.13, 1.12],
                ])
                .unwrap(),
            ),
        };

        for recipe in recipes {
            let snapshot = SnapshotId::new();
            let actual =
                render_linear(&registry, &source, snapshot.clone(), &recipe, settings).unwrap();
            let expected =
                generic_linear_reference(&registry, &source, snapshot, &recipe, settings);
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn parallel_source_colour_rows_match_reference_in_final_partial_chunk() {
        // Just above the production parallel threshold and not divisible by the eight-row chunk.
        let source = varied(1003, 1001);
        let registry = ModuleRegistry::builtin();
        let recipe = colour_recipe(vec![colour_layer(
            crate::BASIC_EFFECT,
            serde_json::json!({
                "exposure": 0.5,
                "contrast": 20.0,
                "highlights": -30.0,
                "shadows": 25.0,
                "whites": 40.0,
                "blacks": -10.0,
                "vibrance": 30.0,
                "saturation": 15.0
            }),
        )]);
        let settings = LinearSettings {
            exposure_ev: 0.37,
            white_balance: None,
        };
        let snapshot = SnapshotId::new();
        let actual =
            render_linear(&registry, &source, snapshot.clone(), &recipe, settings).unwrap();
        let expected = generic_linear_reference(&registry, &source, snapshot, &recipe, settings);
        assert_eq!(actual, expected);
    }

    /// Every shape of stack the rows cover — replacements on either side of a colour run, colour
    /// after a straightened crop's resample, colour after a spatial operation's frame, a masked
    /// spatial operation behind geometry, taps read in blocks through a masked colour segment and
    /// pixel by pixel through one with a replacement — renders the bytes the point evaluator
    /// answers at every pixel, and a point sample the rendered byte, under exact, exposed and
    /// approximately white-balanced settings, on a stage narrower than one tap block and on one
    /// several blocks wide and several row chunks tall.
    #[test]
    fn every_stack_shape_renders_rows_equal_to_the_point_evaluator() {
        let _guard = crate::render::spatial::tests::spatial_guard();
        let sources = [
            varied(41, 29).with_view([1, 2, 38, 26], 6).unwrap(),
            varied(157, 101).with_view([2, 1, 150, 97], 3).unwrap(),
        ];
        let registry = ModuleRegistry::builtin();
        let basic = colour_layer(
            crate::BASIC_EFFECT,
            serde_json::json!({"exposure": 0.4, "contrast": 20.0, "vibrance": 15.0}),
        );
        let vignette = colour_layer(
            crate::VIGNETTE_EFFECT,
            serde_json::json!({"amount": -40.0, "midpoint": 30.0}),
        );
        let presence = colour_layer(
            crate::PRESENCE_EFFECT,
            serde_json::json!({"clarity": 40.0, "texture": 25.0}),
        );
        let crop = Layer::crop(CropPayload {
            angle: 4.0,
            x: 0.15,
            y: 0.1,
            width: 0.7,
            height: 0.75,
        });
        let steep = Layer::crop(CropPayload {
            angle: -30.0,
            x: 0.3,
            y: 0.3,
            width: 0.4,
            height: 0.4,
        });
        let turn = Layer::orientation(crate::Orientation {
            mirror: true,
            turns: 1,
        });
        let mut mask = crate::Mask::new("Mask 1");
        mask.components.push(crate::Component::new(
            "Linear 1",
            crate::ComponentMode::Add,
            "linear",
            serde_json::json!({"x0": 0.2, "y0": 0.1, "x1": 0.8, "y1": 0.9}),
        ));
        let masked = |layer: &Layer| Layer {
            mask: Some(mask.id.clone()),
            ..layer.clone()
        };
        let recipes = [
            colour_recipe(vec![
                Layer::pixel(3, 4, [250, 10, 20]),
                basic.clone(),
                Layer::pixel(5, 6, [1, 200, 30]),
                Layer::pixel(3, 4, [9, 9, 240]),
            ]),
            colour_recipe(vec![Layer::pixel(2, 2, [40, 50, 60]), turn.clone()]),
            colour_recipe(vec![basic.clone(), crop.clone(), vignette.clone()]),
            colour_recipe(vec![
                basic.clone(),
                turn.clone(),
                crop.clone(),
                Layer::pixel(7, 3, [255, 0, 128]),
            ]),
            colour_recipe(vec![presence.clone(), basic.clone(), vignette.clone()]),
            Recipe {
                layers: vec![basic.clone(), masked(&presence), turn.clone(), crop.clone()],
                masks: vec![mask.clone()],
                ..Recipe::default()
            },
            Recipe {
                layers: vec![masked(&basic), steep.clone(), vignette.clone()],
                masks: vec![mask.clone()],
                ..Recipe::default()
            },
            colour_recipe(vec![
                basic.clone(),
                Layer::pixel(9, 8, [200, 100, 50]),
                steep.clone(),
            ]),
        ];
        let balance = WhiteBalanceApproximation::from_matrix([
            [1.21, -0.11, -0.02],
            [-0.06, 1.08, -0.02],
            [0.01, -0.13, 1.12],
        ])
        .unwrap();
        for settings in [
            LinearSettings::default(),
            LinearSettings {
                exposure_ev: 0.7,
                white_balance: None,
            },
            LinearSettings {
                exposure_ev: -0.3,
                white_balance: Some(balance),
            },
        ] {
            for source in &sources {
                for (case, recipe) in recipes.iter().enumerate() {
                    crate::render::testing::clear_estimates();
                    let snapshot = SnapshotId::new();
                    let rendered =
                        render_linear(&registry, source, snapshot.clone(), recipe, settings)
                            .unwrap();
                    assert_eq!(
                        rendered,
                        generic_linear_reference(&registry, source, snapshot, recipe, settings),
                        "case {case}, {settings:?}"
                    );
                    let (width, height) = (rendered.width, rendered.height);
                    for (x, y) in [(0, 0), (width / 2, height / 3), (width - 1, height - 1)] {
                        assert_eq!(
                            sample_linear(&registry, source, recipe, settings, x, y)
                                .unwrap()
                                .rgba,
                            rendered.pixel(x, y),
                            "case {case}, {settings:?} at ({x}, {y})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn masked_and_geometric_colour_rows_match_the_point_evaluator() {
        let source = varied(19, 13);
        let registry = ModuleRegistry::builtin();
        let settings = LinearSettings::default();
        let basic = colour_layer(crate::BASIC_EFFECT, serde_json::json!({"exposure": 0.5}));
        let mut mask = crate::Mask::new("Mask 1");
        mask.components.push(crate::Component::new(
            "Linear 1",
            crate::ComponentMode::Add,
            "linear",
            serde_json::json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}),
        ));
        let masked_recipe = Recipe {
            layers: vec![Layer {
                mask: Some(mask.id.clone()),
                ..basic.clone()
            }],
            masks: vec![mask],
            ..Recipe::default()
        };
        let geometric_recipe = colour_recipe(vec![
            basic,
            Layer::orientation(crate::Orientation {
                mirror: false,
                turns: 1,
            }),
        ]);

        for recipe in [&masked_recipe, &geometric_recipe] {
            let snapshot = SnapshotId::new();
            assert_eq!(
                render_linear(&registry, &source, snapshot.clone(), recipe, settings).unwrap(),
                generic_linear_reference(&registry, &source, snapshot, recipe, settings)
            );
        }
    }

    /// Re-run only on the owner Mac with a private fixture selected through
    /// `LIGHTWELL_RAW_FIXTURE`; it compares the integrated production branch with the original
    /// generic pixel evaluator and reports core-render timings, process CPU and memory snapshots.
    /// Set `LIGHTWELL_RAW_DIAGNOSTIC=contention-production` or `contention-reference` to skip those
    /// timed profiles and isolate the Fit proxy overlap run from load left by full-size rendering.
    #[test]
    #[ignore = "owner-Mac RAW renderer timing diagnostic"]
    fn raw_colour_rows_production_diagnostic() {
        use std::process::Command;

        fn host_load_1m() -> String {
            Command::new("uptime")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|output| {
                    let (_, values) = output
                        .split_once("load averages:")
                        .or_else(|| output.split_once("load average:"))?;
                    values
                        .split_whitespace()
                        .next()
                        .map(|value| value.trim_end_matches(',').to_owned())
                })
                .unwrap_or_else(|| "unavailable".into())
        }

        let fixture = std::path::PathBuf::from(
            std::env::var("LIGHTWELL_RAW_FIXTURE")
                .expect("set LIGHTWELL_RAW_FIXTURE to a qualified private RAW fixture"),
        );
        let bytes = std::fs::read(fixture).expect("read private RAW fixture");
        let prepared = crate::source::RawPrepared::decode(
            bytes,
            "sha256:linear-row-diagnostic".into(),
            None,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("decode private RAW fixture");
        let source = prepared
            .linear
            .clone()
            .expect("fixture retains linear RAW planes");
        let registry = ModuleRegistry::builtin();
        let mut sampler = lightwell_process::Sampler::new();
        let memory_after_decode = sampler.read().memory;
        let recipes = [
            (
                "full-basic",
                colour_recipe(vec![colour_layer(
                    crate::BASIC_EFFECT,
                    serde_json::json!({
                        "exposure": 0.5, "contrast": 25.0, "highlights": -30.0,
                        "shadows": 30.0, "whites": -15.0, "blacks": 15.0,
                        "temperature": 20.0, "tint": -10.0, "vibrance": 30.0,
                        "saturation": 15.0
                    }),
                )]),
            ),
            (
                "mixer",
                colour_recipe(vec![colour_layer(
                    crate::MIXER_EFFECT,
                    serde_json::json!({
                        "red-hue": 30.0, "orange-saturation": 20.0,
                        "blue-luminance": -30.0
                    }),
                )]),
            ),
        ];
        let settings = LinearSettings {
            exposure_ev: 0.7,
            white_balance: None,
        };
        println!(
            "source=retained-raw dimensions={}x{} source_plane_bytes={} output_rgba_bytes={} rayon_threads={} memory_after_decode={:?} loadavg_before={}",
            source.width(),
            source.height(),
            std::mem::size_of_val(source.planes()),
            source.width() as usize * source.height() as usize * 4,
            rayon::current_num_threads(),
            memory_after_decode,
            host_load_1m()
        );

        let diagnostic_mode = std::env::var("LIGHTWELL_RAW_DIAGNOSTIC").unwrap_or_default();
        let contention_only = diagnostic_mode.starts_with("contention");
        for (name, recipe) in recipes {
            if contention_only {
                break;
            }
            let snapshot = SnapshotId::new();
            let reference =
                generic_linear_reference(&registry, &source, snapshot.clone(), &recipe, settings);
            let actual = render_linear(&registry, &source, snapshot.clone(), &recipe, settings)
                .expect("production render");
            let reference_hash = Sha256::digest(&reference.rgba);
            let actual_hash = Sha256::digest(&actual.rgba);
            assert_eq!(
                reference.rgba.as_ref(),
                actual.rgba.as_ref(),
                "complete output bytes: {name}"
            );
            assert_eq!(reference_hash, actual_hash, "complete output hash: {name}");
            assert_eq!(
                (reference.width, reference.height),
                (actual.width, actual.height)
            );
            for (x, y) in [
                (0, 0),
                (reference.width - 1, 0),
                (0, reference.height - 1),
                (reference.width - 1, reference.height - 1),
                (reference.width / 2, reference.height / 2),
            ] {
                assert_eq!(
                    reference.pixel(x, y),
                    actual.pixel(x, y),
                    "{name} at ({x}, {y})"
                );
                assert_eq!(
                    sample_linear(&registry, &source, &recipe, settings, x, y)
                        .unwrap()
                        .rgba,
                    actual.pixel(x, y),
                    "sample {name} at ({x}, {y})"
                );
            }
            drop(reference);
            drop(actual);

            let reference =
                generic_linear_reference(&registry, &source, snapshot.clone(), &recipe, settings);
            drop(reference);
            let memory_after_reference = sampler.read().memory;
            let production = render_linear(&registry, &source, snapshot.clone(), &recipe, settings)
                .expect("production render");
            drop(production);
            let memory_after_production = sampler.read().memory;

            let mut reference_wall = Vec::with_capacity(30);
            let mut production_wall = Vec::with_capacity(30);
            let mut reference_cpu = Vec::with_capacity(30);
            let mut production_cpu = Vec::with_capacity(30);
            for _ in 0..15 {
                let (wall, cpu) = timed_render(
                    || {
                        generic_linear_reference(
                            &registry,
                            &source,
                            snapshot.clone(),
                            &recipe,
                            settings,
                        )
                    },
                    &mut sampler,
                );
                reference_wall.push(wall);
                if let Some(cpu) = cpu {
                    reference_cpu.push(cpu);
                }
                let (wall, cpu) = timed_render(
                    || {
                        render_linear(&registry, &source, snapshot.clone(), &recipe, settings)
                            .expect("production render")
                    },
                    &mut sampler,
                );
                production_wall.push(wall);
                if let Some(cpu) = cpu {
                    production_cpu.push(cpu);
                }
                let (wall, cpu) = timed_render(
                    || {
                        render_linear(&registry, &source, snapshot.clone(), &recipe, settings)
                            .expect("production render")
                    },
                    &mut sampler,
                );
                production_wall.push(wall);
                if let Some(cpu) = cpu {
                    production_cpu.push(cpu);
                }
                let (wall, cpu) = timed_render(
                    || {
                        generic_linear_reference(
                            &registry,
                            &source,
                            snapshot.clone(),
                            &recipe,
                            settings,
                        )
                    },
                    &mut sampler,
                );
                reference_wall.push(wall);
                if let Some(cpu) = cpu {
                    reference_cpu.push(cpu);
                }
            }
            println!(
                "{name}: reference_wall_p50_ms={:.3} reference_wall_p95_ms={:.3} production_wall_p50_ms={:.3} production_wall_p95_ms={:.3} reference_process_cpu_p50_ms={} reference_process_cpu_p95_ms={} production_process_cpu_p50_ms={} production_process_cpu_p95_ms={} row_scratch_bytes_per_folder={} row_scratch_max_at_pool_width={} memory_after_reference={:?} memory_after_production={:?} output_sha256={:x}",
                percentile(&reference_wall, 0.50),
                percentile(&reference_wall, 0.95),
                percentile(&production_wall, 0.50),
                percentile(&production_wall, 0.95),
                if reference_cpu.is_empty() {
                    "unavailable".into()
                } else {
                    format!("{:.3}", percentile(&reference_cpu, 0.50))
                },
                if reference_cpu.is_empty() {
                    "unavailable".into()
                } else {
                    format!("{:.3}", percentile(&reference_cpu, 0.95))
                },
                if production_cpu.is_empty() {
                    "unavailable".into()
                } else {
                    format!("{:.3}", percentile(&production_cpu, 0.50))
                },
                if production_cpu.is_empty() {
                    "unavailable".into()
                } else {
                    format!("{:.3}", percentile(&production_cpu, 0.95))
                },
                source.width() as usize * std::mem::size_of::<[f32; 3]>(),
                source.width() as usize
                    * std::mem::size_of::<[f32; 3]>()
                    * rayon::current_num_threads(),
                memory_after_reference,
                memory_after_production,
                actual_hash
            );
        }

        // A display-bounded nearest-sampled source derived from the retained RAW planes exercises
        // the same pointwise colour work as a Fit proxy without putting decode or proxy creation in
        // the timed region. One exact source render runs sequentially in a background caller while
        // 30 proxy requests use the shared Rayon pool.
        let preview_width = 1920_u32;
        let preview_height = 1280_u32;
        let mut red = Vec::with_capacity((preview_width * preview_height) as usize);
        let mut green = Vec::with_capacity(red.capacity());
        let mut blue = Vec::with_capacity(red.capacity());
        let reader = source.reader();
        for y in 0..preview_height {
            let source_y =
                (u64::from(y) * u64::from(source.height()) / u64::from(preview_height)) as u32;
            let row = reader
                .row(source_y)
                .expect("preview row is inside the retained RAW source")
                .collect::<Vec<_>>();
            for x in 0..preview_width {
                let source_x =
                    (u64::from(x) * u64::from(source.width()) / u64::from(preview_width)) as usize;
                let pixel = row[source_x];
                red.push(pixel[0]);
                green.push(pixel[1]);
                blue.push(pixel[2]);
            }
        }
        let mut preview_planes = red;
        preview_planes.extend(green);
        preview_planes.extend(blue);
        let preview_source = LinearImage::with_fingerprint(
            preview_width,
            preview_height,
            preview_planes,
            "sha256:linear-row-preview-contention",
        )
        .unwrap();
        let preview_recipe = colour_recipe(vec![colour_layer(
            crate::BASIC_EFFECT,
            serde_json::json!({
                "exposure": 0.5, "contrast": 25.0, "highlights": -30.0,
                "shadows": 30.0, "whites": -15.0, "blacks": 15.0,
                "temperature": 20.0, "tint": -10.0, "vibrance": 30.0,
                "saturation": 15.0
            }),
        )]);
        let preview_snapshot = SnapshotId::new();
        let preview_reference = generic_linear_reference(
            &registry,
            &preview_source,
            preview_snapshot.clone(),
            &preview_recipe,
            settings,
        );
        let preview_actual = render_linear_proxy_cancellable(
            &registry,
            &preview_source,
            preview_snapshot.clone(),
            &preview_recipe,
            settings,
            &Cancel::never(),
        )
        .unwrap();
        assert_eq!(preview_reference, preview_actual, "proxy byte identity");
        drop(preview_reference);
        drop(preview_actual);
        let render_preview = || {
            render_linear_proxy_cancellable(
                &registry,
                &preview_source,
                preview_snapshot.clone(),
                &preview_recipe,
                settings,
                &Cancel::never(),
            )
            .expect("Fit proxy render")
        };
        drop(render_preview());

        let mut preview_uncontended_wall = Vec::with_capacity(30);
        let mut preview_uncontended_cpu = Vec::with_capacity(30);
        for _ in 0..30 {
            let (wall, cpu) = timed_render(render_preview, &mut sampler);
            preview_uncontended_wall.push(wall);
            if let Some(cpu) = cpu {
                preview_uncontended_cpu.push(cpu);
            }
        }

        let load_before_contention = host_load_1m();
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_running = running.clone();
        let worker_started = started.clone();
        let worker_barrier = barrier.clone();
        let worker_source = source.clone();
        let worker_recipe = preview_recipe.clone();
        let worker_snapshot = SnapshotId::new();
        let worker_reference = diagnostic_mode == "contention-reference";
        let worker = std::thread::spawn(move || {
            let worker_registry = ModuleRegistry::builtin();
            let mut exact_renders = 0_u32;
            worker_barrier.wait();
            worker_started.store(true, std::sync::atomic::Ordering::Relaxed);
            while worker_running.load(std::sync::atomic::Ordering::Relaxed) {
                let raster = if worker_reference {
                    generic_linear_reference(
                        &worker_registry,
                        &worker_source,
                        worker_snapshot.clone(),
                        &worker_recipe,
                        settings,
                    )
                } else {
                    render_linear(
                        &worker_registry,
                        &worker_source,
                        worker_snapshot.clone(),
                        &worker_recipe,
                        settings,
                    )
                    .expect("background exact RAW render")
                };
                std::hint::black_box(&raster);
                drop(raster);
                exact_renders += 1;
            }
            exact_renders
        });
        let contention_cpu_before = sampler.read().cpu_time_ns.ok();
        let contention_start = Instant::now();
        barrier.wait();
        while !started.load(std::sync::atomic::Ordering::Relaxed) {
            std::hint::spin_loop();
        }
        let mut preview_contended_wall = Vec::with_capacity(30);
        for _ in 0..30 {
            preview_contended_wall.push(timed_render(render_preview, &mut sampler).0);
        }
        running.store(false, std::sync::atomic::Ordering::Relaxed);
        let exact_renders = worker.join().expect("exact render worker");
        let contention_wall_ms = contention_start.elapsed().as_secs_f64() * 1e3;
        let contention_cpu_ms = contention_cpu_before
            .zip(sampler.read().cpu_time_ns.ok())
            .map(|(before, after)| after.saturating_sub(before) as f64 / 1e6);
        let memory_after_contention = sampler.read().memory;
        println!(
            "fit_proxy_contention: variant={} dimensions={}x{} samples=30 uncontended_p50_p95_ms={:.3}/{:.3} uncontended_process_cpu_p50_p95_ms={:.3}/{:.3} contended_p50_p95_ms={:.3}/{:.3} exact_renders_during_proxy_samples={} overlap_wall_ms={:.3} overlap_process_cpu_ms={} memory_after_contention={:?} loadavg_before={}",
            if worker_reference {
                "generic-reference"
            } else {
                "production-rows"
            },
            preview_width,
            preview_height,
            percentile(&preview_uncontended_wall, 0.50),
            percentile(&preview_uncontended_wall, 0.95),
            percentile(&preview_uncontended_cpu, 0.50),
            percentile(&preview_uncontended_cpu, 0.95),
            percentile(&preview_contended_wall, 0.50),
            percentile(&preview_contended_wall, 0.95),
            exact_renders,
            contention_wall_ms,
            contention_cpu_ms.map_or_else(|| "unavailable".into(), |value| format!("{value:.3}")),
            memory_after_contention,
            load_before_contention
        );
        println!("loadavg_after={}", host_load_1m());
    }

    /// Measures a Fit-size RAW proxy while one exact retained-mosaic development repeats on a
    /// background caller. The serial and parallel normalizer arms run in ABBA block order; each
    /// proxy byte buffer is checked against an untimed exact reference after its render timer.
    #[test]
    #[ignore = "owner-Mac RAW normalization contention diagnostic"]
    fn bayer_normalization_fit_proxy_contention_diagnostic() {
        use std::{
            process::Command,
            sync::{
                Barrier,
                atomic::{AtomicBool, Ordering},
            },
        };

        fn host_load_1m() -> String {
            Command::new("uptime")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|output| {
                    let (_, values) = output
                        .split_once("load averages:")
                        .or_else(|| output.split_once("load average:"))?;
                    values
                        .split_whitespace()
                        .next()
                        .map(|value| value.trim_end_matches(',').to_owned())
                })
                .unwrap_or_else(|| "unavailable".into())
        }

        fn render_checked(
            registry: &ModuleRegistry,
            source: &LinearImage,
            snapshot: &SnapshotId,
            recipe: &Recipe,
            settings: LinearSettings,
            oracle: &Raster,
            sampler: &mut lightwell_process::Sampler,
        ) -> (f64, Option<f64>) {
            let cpu_before = sampler.read().cpu_time_ns.ok();
            let start = Instant::now();
            let raster = render_linear_proxy_cancellable(
                registry,
                source,
                snapshot.clone(),
                recipe,
                settings,
                &Cancel::never(),
            )
            .expect("Fit proxy render");
            let wall_ms = start.elapsed().as_secs_f64() * 1e3;
            let cpu_after = sampler.read().cpu_time_ns.ok();
            assert_eq!(raster, *oracle, "complete Fit proxy bytes");
            std::hint::black_box(&raster);
            drop(raster);
            let process_cpu_ms = cpu_before
                .zip(cpu_after)
                .map(|(before, after)| after.saturating_sub(before) as f64 / 1e6);
            (wall_ms, process_cpu_ms)
        }

        struct LegInput<'a> {
            parallel_normalization: bool,
            raw: Arc<lightwell_raw::RawSource>,
            gains: [f32; 3],
            preview: &'a LinearImage,
            preview_snapshot: &'a SnapshotId,
            recipe: &'a Recipe,
            settings: LinearSettings,
            oracle: &'a Raster,
            registry: &'a ModuleRegistry,
        }

        struct LegMeasurement {
            proxy_wall: Vec<f64>,
            proxy_process_cpu: Vec<f64>,
            exact_started_overlap: u32,
            exact_completed_overlap: u32,
            exact_started_drain: u32,
            exact_completed_drain: u32,
            overlap_wall_ms: f64,
            overlap_process_cpu_ms: f64,
            drain_wall_ms: f64,
            drain_process_cpu_ms: f64,
            load_start: String,
            load_end: String,
        }

        fn measure_leg(
            input: LegInput<'_>,
            sampler: &mut lightwell_process::Sampler,
        ) -> LegMeasurement {
            let LegInput {
                parallel_normalization,
                raw,
                gains,
                preview,
                preview_snapshot,
                recipe,
                settings,
                oracle,
                registry,
            } = input;
            let running = Arc::new(AtomicBool::new(true));
            let started = Arc::new(AtomicBool::new(false));
            let exact_started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let exact_completed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let barrier = Arc::new(Barrier::new(2));
            let worker_running = running.clone();
            let worker_started = started.clone();
            let worker_exact_started = exact_started.clone();
            let worker_exact_completed = exact_completed.clone();
            let worker_barrier = barrier.clone();
            let worker_raw = raw.clone();
            let worker = std::thread::spawn(move || {
                let worker_cancel = AtomicBool::new(false);
                worker_barrier.wait();
                worker_started.store(true, Ordering::Release);
                while worker_running.load(Ordering::Acquire) {
                    worker_exact_started.fetch_add(1, Ordering::AcqRel);
                    let developed = worker_raw
                        .develop_for_performance_diagnostic(
                            gains,
                            &worker_cancel,
                            parallel_normalization,
                        )
                        .expect("concurrent exact Bayer development");
                    std::hint::black_box(&developed.data);
                    drop(developed);
                    worker_exact_completed.fetch_add(1, Ordering::AcqRel);
                }
            });

            let load_start = host_load_1m();
            let cpu_before = sampler.read().cpu_time_ns.ok();
            let overlap_start = Instant::now();
            barrier.wait();
            while !started.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            let mut proxy_wall = Vec::with_capacity(15);
            let mut proxy_process_cpu = Vec::with_capacity(15);
            for _ in 0..15 {
                let (wall, cpu) = render_checked(
                    registry,
                    preview,
                    preview_snapshot,
                    recipe,
                    settings,
                    oracle,
                    sampler,
                );
                proxy_wall.push(wall);
                if let Some(cpu) = cpu {
                    proxy_process_cpu.push(cpu);
                }
            }
            // The measured overlap ends with the fifteenth preview. Snapshot counters
            // and process CPU before asking the worker to stop; RCD may finish later.
            let overlap_wall_ms = overlap_start.elapsed().as_secs_f64() * 1e3;
            let overlap_cpu_snapshot = sampler.read().cpu_time_ns.ok();
            let overlap_started = exact_started.load(Ordering::Acquire) as u32;
            let overlap_completed = exact_completed.load(Ordering::Acquire) as u32;
            running.store(false, Ordering::Release);
            let drain_start = Instant::now();
            worker.join().expect("exact RAW worker");
            let drain_wall_ms = drain_start.elapsed().as_secs_f64() * 1e3;
            let after_drain_cpu = sampler.read().cpu_time_ns.ok();
            let drain_process_cpu_ms = overlap_cpu_snapshot
                .zip(after_drain_cpu)
                .map(|(before, after)| after.saturating_sub(before) as f64 / 1e6)
                .unwrap_or(f64::NAN);
            let overlap_process_cpu_ms = cpu_before
                .zip(overlap_cpu_snapshot)
                .map(|(before, after)| after.saturating_sub(before) as f64 / 1e6)
                .unwrap_or(f64::NAN);
            let load_end = host_load_1m();
            let total_started = exact_started.load(Ordering::Acquire) as u32;
            let total_completed = exact_completed.load(Ordering::Acquire) as u32;
            LegMeasurement {
                proxy_wall,
                proxy_process_cpu,
                exact_started_overlap: overlap_started,
                exact_completed_overlap: overlap_completed,
                exact_started_drain: total_started.saturating_sub(overlap_started),
                exact_completed_drain: total_completed.saturating_sub(overlap_completed),
                overlap_wall_ms,
                overlap_process_cpu_ms,
                drain_wall_ms,
                drain_process_cpu_ms,
                load_start,
                load_end,
            }
        }

        let owner = std::env::var("LIGHTWELL_RAW_OWNER_DIR").expect("owner RAW fixture directory");
        let path = std::path::Path::new(&owner).join("nikon_z6.NEF");
        let bytes = std::fs::read(&path).expect("read owner Nikon Z6 source");
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            "e4db4e1f152110da0a3feb77a4b666c9de4e005509c4c443d15a2d8071bd49fb",
            "owner RAW manifest hash"
        );
        let prepared = crate::source::RawPrepared::decode(
            bytes,
            "sha256:bayer-normalization-fit-contention".into(),
            None,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .expect("decode and prepare owner Nikon Z6");
        let raw = prepared.sensor.clone();
        let gains = prepared.gains;
        assert_eq!(
            raw.metadata().mode,
            lightwell_raw::RawMode::NikonZ6Lossless14
        );
        assert_eq!(
            (raw.metadata().sensor_width, raw.metadata().sensor_height),
            (6064, 4040)
        );
        let source = prepared
            .linear
            .clone()
            .expect("retained Nikon linear planes");
        let mut sampler = lightwell_process::Sampler::new();
        let memory_after_decode = sampler.read().memory;

        let preview_width = 1920_u32;
        let preview_height = 1280_u32;
        let mut red = Vec::with_capacity((preview_width * preview_height) as usize);
        let mut green = Vec::with_capacity(red.capacity());
        let mut blue = Vec::with_capacity(red.capacity());
        let reader = source.reader();
        for y in 0..preview_height {
            let source_y =
                (u64::from(y) * u64::from(source.height()) / u64::from(preview_height)) as u32;
            let row = reader
                .row(source_y)
                .expect("Fit proxy row")
                .collect::<Vec<_>>();
            for x in 0..preview_width {
                let source_x =
                    (u64::from(x) * u64::from(source.width()) / u64::from(preview_width)) as usize;
                let pixel = row[source_x];
                red.push(pixel[0]);
                green.push(pixel[1]);
                blue.push(pixel[2]);
            }
        }
        let mut preview_planes = red;
        preview_planes.extend(green);
        preview_planes.extend(blue);
        let preview = LinearImage::with_fingerprint(
            preview_width,
            preview_height,
            preview_planes,
            "sha256:bayer-normalization-fit-contention-proxy",
        )
        .unwrap();
        let registry = ModuleRegistry::builtin();
        let settings = LinearSettings {
            exposure_ev: 0.7,
            white_balance: None,
        };
        let recipe = colour_recipe(vec![colour_layer(
            crate::BASIC_EFFECT,
            serde_json::json!({
                "exposure": 0.5, "contrast": 25.0, "highlights": -30.0,
                "shadows": 30.0, "whites": -15.0, "blacks": 15.0,
                "temperature": 20.0, "tint": -10.0, "vibrance": 30.0,
                "saturation": 15.0
            }),
        )]);
        let snapshot = SnapshotId::new();
        let reference =
            generic_linear_reference(&registry, &preview, snapshot.clone(), &recipe, settings);
        assert_eq!(
            reference,
            render_linear_proxy_cancellable(
                &registry,
                &preview,
                snapshot.clone(),
                &recipe,
                settings,
                &Cancel::never(),
            )
            .unwrap(),
            "Fit proxy equals generic complete-byte reference"
        );
        // Warm native exact and proxy paths in both normalizer modes before sampling.
        for parallel in [false, true] {
            let developed = raw
                .develop_for_performance_diagnostic(
                    gains,
                    &std::sync::atomic::AtomicBool::new(false),
                    parallel,
                )
                .expect("warm exact native development");
            std::hint::black_box(&developed.data);
            drop(developed);
            let _ = render_checked(
                &registry,
                &preview,
                &snapshot,
                &recipe,
                settings,
                &reference,
                &mut sampler,
            );
        }

        let mut serial_wall = Vec::with_capacity(30);
        let mut serial_cpu = Vec::with_capacity(30);
        let mut parallel_wall = Vec::with_capacity(30);
        let mut parallel_cpu = Vec::with_capacity(30);
        let mut exact_started_overlap = [0_u32; 2];
        let mut exact_completed_overlap = [0_u32; 2];
        let mut exact_started_drain = [0_u32; 2];
        let mut exact_completed_drain = [0_u32; 2];
        let mut overlap_wall_ms = [0.0_f64; 2];
        let mut overlap_process_cpu_ms = [0.0_f64; 2];
        let mut drain_wall_ms = [0.0_f64; 2];
        let mut drain_process_cpu_ms = [0.0_f64; 2];
        let mut leg_loads = Vec::new();
        // Each leg contributes 15 preview samples; A-B-B-A therefore gives 30 per arm.
        for parallel in [false, true, true, false] {
            let measurement = measure_leg(
                LegInput {
                    parallel_normalization: parallel,
                    raw: raw.clone(),
                    gains,
                    preview: &preview,
                    preview_snapshot: &snapshot,
                    recipe: &recipe,
                    settings,
                    oracle: &reference,
                    registry: &registry,
                },
                &mut sampler,
            );
            let arm = usize::from(parallel);
            if parallel {
                parallel_wall.extend(measurement.proxy_wall);
                parallel_cpu.extend(measurement.proxy_process_cpu);
            } else {
                serial_wall.extend(measurement.proxy_wall);
                serial_cpu.extend(measurement.proxy_process_cpu);
            }
            exact_started_overlap[arm] += measurement.exact_started_overlap;
            exact_completed_overlap[arm] += measurement.exact_completed_overlap;
            exact_started_drain[arm] += measurement.exact_started_drain;
            exact_completed_drain[arm] += measurement.exact_completed_drain;
            overlap_wall_ms[arm] += measurement.overlap_wall_ms;
            overlap_process_cpu_ms[arm] += measurement.overlap_process_cpu_ms;
            drain_wall_ms[arm] += measurement.drain_wall_ms;
            drain_process_cpu_ms[arm] += measurement.drain_process_cpu_ms;
            leg_loads.push((
                if parallel { "parallel" } else { "serial" },
                measurement.load_start,
                measurement.load_end,
            ));
        }
        let memory = sampler.read().memory;
        println!(
            "bayer_fit_proxy_contention: source=nikon_z6.NEF dimensions=6064x4040 preview={}x{} samples_per_arm=30 order=serial-parallel-parallel-serial rayon_threads={} max_admitted_normalization_callbacks=8 normalizer_extra_scratch_bytes=0 shared_admission_slot_limit=8 preview_serial_wall_p50_p95_ms={:.3}/{:.3} preview_serial_process_cpu_per_preview_window_p50_p95_ms={:.3}/{:.3} preview_parallel_wall_p50_p95_ms={:.3}/{:.3} preview_parallel_process_cpu_per_preview_window_p50_p95_ms={:.3}/{:.3} exact_serial_started_during_overlap={} exact_serial_completed_during_overlap={} exact_serial_started_in_drain={} exact_serial_completed_in_drain={} exact_parallel_started_during_overlap={} exact_parallel_completed_during_overlap={} exact_parallel_started_in_drain={} exact_parallel_completed_in_drain={} serial_overlap_wall_ms={:.1} serial_overlap_process_cpu_ms={:.1} serial_drain_wall_ms={:.1} serial_drain_process_cpu_ms={:.1} parallel_overlap_wall_ms={:.1} parallel_overlap_process_cpu_ms={:.1} parallel_drain_wall_ms={:.1} parallel_drain_process_cpu_ms={:.1} memory_after_prepare={memory_after_decode:?} process_memory_after_overlap={memory:?} process_memory_peak_scope=whole diagnostic process across both arms including retained mosaic, retained linear planes, preview oracle and transient exact RGB output; not per arm; cancellation=measurement does not cancel exact development; per-row normalization checks and joins; Bayer RCD is noncancellable until its native call returns; includes core preview render, excludes app upload and scanout",
            preview_width,
            preview_height,
            rayon::current_num_threads(),
            percentile(&serial_wall, 0.50),
            percentile(&serial_wall, 0.95),
            percentile(&serial_cpu, 0.50),
            percentile(&serial_cpu, 0.95),
            percentile(&parallel_wall, 0.50),
            percentile(&parallel_wall, 0.95),
            percentile(&parallel_cpu, 0.50),
            percentile(&parallel_cpu, 0.95),
            exact_started_overlap[0],
            exact_completed_overlap[0],
            exact_started_drain[0],
            exact_completed_drain[0],
            exact_started_overlap[1],
            exact_completed_overlap[1],
            exact_started_drain[1],
            exact_completed_drain[1],
            overlap_wall_ms[0],
            overlap_process_cpu_ms[0],
            drain_wall_ms[0],
            drain_process_cpu_ms[0],
            overlap_wall_ms[1],
            overlap_process_cpu_ms[1],
            drain_wall_ms[1],
            drain_process_cpu_ms[1],
        );
        for (arm, start, end) in leg_loads {
            println!("contention_leg_load: arm={arm} load_start={start} load_end={end}");
        }
    }

    #[test]
    fn source_rows_keep_validation_fallback_cancellation_and_finite_errors() {
        let source = varied(9, 7);
        let registry = ModuleRegistry::builtin();
        let settings = LinearSettings::default();
        for recipe in [
            Recipe {
                layers: vec![Layer::pixel(1, 1, [30, 60, 90])],
                ..Recipe::default()
            },
            Recipe {
                layers: vec![Layer::orientation(crate::Orientation {
                    mirror: false,
                    turns: 1,
                })],
                ..Recipe::default()
            },
            Recipe {
                layers: vec![Layer::crop(CropPayload {
                    angle: 5.0,
                    x: 0.2,
                    y: 0.2,
                    width: 0.6,
                    height: 0.6,
                })],
                ..Recipe::default()
            },
            cancellation_recipe(),
        ] {
            let snapshot = SnapshotId::new();
            assert_eq!(
                render_linear(&registry, &source, snapshot.clone(), &recipe, settings).unwrap(),
                generic_linear_reference(&registry, &source, snapshot, &recipe, settings)
            );
        }
        let mut unavailable = cancellation_recipe();
        unavailable.layers[0].effect_id = "unavailable.effect".into();
        let expected = registry
            .compile(source.width(), source.height(), &unavailable)
            .err()
            .unwrap();
        let actual = render_linear(
            &registry,
            &source,
            SnapshotId::new(),
            &unavailable,
            settings,
        )
        .unwrap_err();
        assert_eq!(
            (actual.kind, actual.detail),
            (expected.kind, expected.detail)
        );
        for exposure_ev in [f64::NAN, f64::INFINITY, -5.1, 5.1] {
            assert_eq!(
                render_linear(
                    &registry,
                    &source,
                    SnapshotId::new(),
                    &Recipe::default(),
                    LinearSettings {
                        exposure_ev,
                        white_balance: None
                    }
                )
                .unwrap_err()
                .kind,
                ErrorKind::Validation
            );
        }
        let cancel = Cancel::new();
        cancel.cancel();
        assert_eq!(
            render_linear_cancellable(
                &registry,
                &source,
                SnapshotId::new(),
                &Recipe::default(),
                settings,
                &cancel
            )
            .unwrap_err()
            .kind,
            ErrorKind::Cancelled
        );

        // Finite WB coefficients can still overflow while evaluating a pixel. The row path must
        // keep the generic source-adjustment error, not let a later terminal check replace it.
        let overflow = LinearSettings {
            exposure_ev: 5.0,
            white_balance: Some(
                WhiteBalanceApproximation::from_matrix([[f64::MAX; 3]; 3]).unwrap(),
            ),
        };
        let overflowing_source = image(1, 1, &[[1.0; 3]]);
        let generic = linear_evaluation(
            &registry,
            &overflowing_source,
            &Recipe::default(),
            overflow,
            &Cancel::never(),
            PRODUCTION_TILE,
            SpatialMode::Frames,
        )
        .unwrap();
        let expected = generic.pixel(0, 0).unwrap_err();
        let actual = render_linear(
            &registry,
            &overflowing_source,
            SnapshotId::new(),
            &Recipe::default(),
            overflow,
        )
        .unwrap_err();
        assert_eq!(
            (actual.kind, actual.detail),
            (expected.kind, expected.detail)
        );
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
    fn terminal_quantization_preserves_forward_rounding_at_every_f64_boundary() {
        // Independent inverse transfer, including every representable neighbour around each
        // code boundary. Some of these values intentionally disagree with inverse-only lookup.
        for code in 1..=255_u32 {
            let encoded = (f64::from(code) - 0.5) / 255.0;
            let boundary = if encoded <= 0.040_45 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            };
            let bits = boundary.to_bits();
            for bits in bits - 128..=bits + 128 {
                let value = f64::from_bits(bits);
                assert_eq!(
                    terminal_srgb(value).unwrap(),
                    reference_srgb(value),
                    "code {code}, bits {bits:#018x}"
                );
            }
            // Include both sides of the guard as well as values within it. The reference is
            // always the former forward transfer, never the lookup under test.
            for delta in [-2e-12, -1e-12, -5e-13, 5e-13, 1e-12, 2e-12] {
                let value = boundary + delta;
                for value in [value.next_down(), value, value.next_up()] {
                    assert_eq!(terminal_srgb(value).unwrap(), reference_srgb(value));
                }
            }
        }
    }

    #[test]
    fn terminal_quantization_preserves_finite_domain_and_rejects_nonfinite_values() {
        for value in [
            f64::MIN,
            -1.0,
            -f64::MIN_POSITIVE,
            -f64::from_bits(1),
            -0.0,
            0.0,
            f64::from_bits(1),
            f64::MIN_POSITIVE,
            0.003_130_8_f64.next_down(),
            0.003_130_8,
            0.003_130_8_f64.next_up(),
            1.0_f64.next_down(),
            1.0,
            1.0_f64.next_up(),
            32.0,
            f64::MAX,
        ] {
            assert_eq!(terminal_srgb(value).unwrap(), reference_srgb(value));
        }
        for step in 0..=40_000 {
            let value = f64::from(step) / 40_000.0;
            assert_eq!(terminal_srgb(value).unwrap(), reference_srgb(value));
        }
        // Deterministic bit-pattern coverage also visits the very dark/subnormal domain that
        // a uniform sweep misses, plus signed and extended-domain source values.
        let mut bits = 0x1234_5678_9abc_def0_u64;
        for _ in 0..100_000 {
            bits = bits.wrapping_mul(6364136223846793005).wrapping_add(1);
            let value = f64::from_bits(bits);
            if value.is_finite() {
                assert_eq!(terminal_srgb(value).unwrap(), reference_srgb(value));
            }
        }
        for value in [f64::NAN, -f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let error = terminal_srgb(value).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Render);
            assert_eq!(
                error.detail,
                "linear evaluation produced a non-finite value"
            );
        }
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
                mask: None,
                artifacts: Vec::new(),
            }],
            masks: Vec::new(),
            ..Recipe::default()
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
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer::orientation(crate::Orientation {
                mirror: false,
                turns: 2,
            })],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let evaluation = linear_evaluation(
            &ModuleRegistry::builtin(),
            &view,
            &recipe,
            LinearSettings::default(),
            &Cancel::new(),
            PRODUCTION_TILE,
            SpatialMode::Point,
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
    fn validated_plane_adoption_keeps_layout_identity_and_shared_storage() {
        let planes = vec![0.25, 0.5, 0.75, 1.0, -0.5, 2.0];
        let pointer = planes.as_ptr();
        let source =
            LinearImage::from_validated_planes(2, 1, planes, "raw-fingerprint".into()).unwrap();
        let view = source.with_view([0, 0, 2, 1], 2).unwrap();
        assert_eq!(source.planes().as_ptr(), pointer);
        assert_eq!(view.planes().as_ptr(), pointer);
        assert_eq!(source.fingerprint(), "raw-fingerprint");
        assert_eq!(view.development(), source.development());
        assert_eq!(source.pixel(0, 0), Some([0.25, 0.75, -0.5]));
        assert_eq!(view.pixel(0, 0), Some([0.5, 1.0, 2.0]));
        assert_eq!(
            LinearImage::from_validated_planes(2, 2, vec![0.0; 6], String::new())
                .unwrap_err()
                .kind,
            ErrorKind::Validation
        );
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
            Linear::blend(1.0, 1.0, 2, 2, |x, y| Ok(corners[(y * 2 + x) as usize])).unwrap();
        assert_eq!(actual, [1.0, 0.0, 1.0]);

        let precise = [
            [0.125_123_456_789, -0.543_210_987_654, 1.734_567_890_123],
            [0.912_345_678_901, 0.234_567_890_123, -0.876_543_210_987],
            [1.234_567_890_123, -1.345_678_901_234, 0.456_789_012_345],
            [-0.321_098_765_432, 0.678_901_234_567, 1.890_123_456_789],
        ];
        let actual =
            Linear::blend(1.25, 1.25, 2, 2, |x, y| Ok(precise[(y * 2 + x) as usize])).unwrap();
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
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer::pixel(1, 0, [128, 64, 255])],
            masks: Vec::new(),
            ..Recipe::default()
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
                reference_srgb(srgb::decode_u8(128)),
                reference_srgb(srgb::decode_u8(64)),
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
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer::crop(CropPayload {
                angle: 12.0,
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
            })],
            masks: Vec::new(),
            ..Recipe::default()
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
                masks: Vec::new(),
                ..Recipe::default()
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
        assert!(Linear::blend(1.0, 1.0, 2, 2, |_x, _y| Ok([f64::INFINITY; 3])).is_err());
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
                mask: None,
                artifacts: Vec::new(),
            }],
            masks: Vec::new(),
            ..Recipe::default()
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

    /// The terminal bytes are written in the allocation the raster holds and returned with no
    /// copy after the pass, and they are the bytes a point sample reads.
    #[test]
    fn a_linear_render_returns_the_frame_it_wrote() {
        let registry = ModuleRegistry::builtin();
        let source = cancellation_image(40, 30);
        let recipe = cancellation_recipe();
        let settings = LinearSettings::default();
        let (raster, written) = crate::render::frame_writes::record(|| {
            render_linear(&registry, &source, SnapshotId::new(), &recipe, settings).unwrap()
        });
        assert_eq!(written, [raster.rgba.as_ptr() as usize]);
        for (x, y) in [(0, 0), (17, 11), (39, 29)] {
            let sampled = sample_linear(&registry, &source, &recipe, settings, x, y).unwrap();
            assert_eq!(sampled.rgba, raster.pixel(x, y), "({x}, {y})");
        }
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

    fn presence_stack(payload: serde_json::Value) -> Recipe {
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer {
                id: crate::LayerId::new(),
                effect_id: crate::PRESENCE_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload,
                artifacts: Vec::new(),
                mask: None,
            }],
            masks: Vec::new(),
            strokes: Default::default(),
            artifacts: Default::default(),
        }
    }

    #[test]
    fn a_point_evaluation_builds_no_frame_for_its_spatial_segment() {
        let _guard = crate::render::spatial::tests::spatial_guard();
        crate::render::testing::clear_estimates();
        let source = cancellation_image(96, 64);
        let registry = ModuleRegistry::builtin();
        let stack = presence_stack(serde_json::json!({"clarity": 40.0}));
        let evaluate = |mode| {
            linear_evaluation(
                &registry,
                &source,
                &stack,
                LinearSettings::default(),
                &Cancel::new(),
                PRODUCTION_TILE,
                mode,
            )
            .unwrap()
        };
        let frames = evaluate(SpatialMode::Frames);
        assert!(frames.tiles.is_none());
        assert_eq!(
            frames.built.len(),
            1,
            "a render materializes the spatial output"
        );
        assert!(frames.frame.is_some());
        let point = evaluate(SpatialMode::Point);
        assert!(
            point.built.is_empty() && point.frame.is_none(),
            "a point evaluation materializes nothing"
        );
        for (x, y) in [(5, 7), (90, 60), (5, 7)] {
            assert_eq!(point.pixel(x, y).unwrap(), frames.pixel(x, y).unwrap());
        }
        assert_eq!(
            point.tiles.as_ref().unwrap().evaluated().len(),
            1,
            "one tile answers every pixel inside it"
        );
    }

    /// A global Presence layer, a colour layer and three masked Presence layers: four spatial
    /// segments, each a whole float frame on this path.
    fn four_spatial_segments() -> Recipe {
        let mask = |name: &str, x0: f64, x1: f64| {
            let mut mask = crate::Mask::new(name);
            let component = mask.next_component_name("linear");
            mask.components.push(crate::Component::new(
                component,
                crate::ComponentMode::Add,
                "linear",
                serde_json::json!({"x0": x0, "y0": 0.5, "x1": x1, "y1": 0.5}),
            ));
            mask
        };
        let masks = vec![
            mask("Mask 1", 0.2, 0.6),
            mask("Mask 2", 0.9, 0.3),
            mask("Mask 3", -0.2, 0.4),
        ];
        let layer = |effect: &str, payload, mask: Option<&crate::Mask>| Layer {
            id: crate::LayerId::new(),
            effect_id: effect.into(),
            effect_format: crate::EFFECT_FORMAT,
            payload,
            mask: mask.map(|mask| mask.id.clone()),
            artifacts: Vec::new(),
        };
        let mut layers = vec![
            layer(
                crate::PRESENCE_EFFECT,
                serde_json::json!({"clarity": 40.0, "dehaze": 20.0}),
                None,
            ),
            layer(
                crate::BASIC_EFFECT,
                serde_json::json!({"exposure": 0.3}),
                None,
            ),
        ];
        for (mask, payload) in masks.iter().zip([
            serde_json::json!({"texture": 35.0}),
            serde_json::json!({"clarity": -30.0}),
            serde_json::json!({"dehaze": 25.0}),
        ]) {
            layers.push(layer(crate::PRESENCE_EFFECT, payload, Some(mask)));
        }
        Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks,
            ..Recipe::default()
        }
    }

    /// Each spatial frame is built from the one before it and replaces it, so building one holds
    /// two and the evaluation keeps one, the latest; a point evaluation builds none. The counts are
    /// the frames' own reference counts, not bookkeeping: an earlier frame anything still held would
    /// be counted alive.
    #[test]
    fn a_linear_evaluation_keeps_at_most_two_spatial_frames() {
        let _guard = crate::render::spatial::tests::spatial_guard();
        crate::render::testing::clear_estimates();
        let source = cancellation_image(96, 64);
        let registry = ModuleRegistry::builtin();
        let stack = four_spatial_segments();
        let evaluate = |mode| {
            linear_evaluation(
                &registry,
                &source,
                &stack,
                LinearSettings::default(),
                &Cancel::new(),
                PRODUCTION_TILE,
                mode,
            )
            .unwrap()
        };
        let alive = |evaluation: &Evaluation<'_, Linear<'_>>| -> Vec<bool> {
            evaluation
                .built
                .iter()
                .map(|(frame, _)| frame.strong_count() > 0)
                .collect()
        };
        let peaks = |evaluation: &Evaluation<'_, Linear<'_>>| -> Vec<usize> {
            evaluation.built.iter().map(|(_, peak)| *peak).collect()
        };

        let frames = evaluate(SpatialMode::Frames);
        let spatial: Vec<usize> = frames
            .compiled
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| matches!(segment.entry, Some(Entry::Spatial { .. })))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(spatial.len(), 4, "four spatial segments");
        assert_eq!(
            peaks(&frames),
            [1, 2, 2, 2],
            "each frame is finished beside the one it reads and no other"
        );
        assert_eq!(alive(&frames), [false, false, false, true]);
        assert_eq!(
            frames.frame.as_ref().map(|frame| frame.index),
            Some(spatial[3])
        );

        // Point mode materializes no spatial segment: every one is answered from the query's tiles.
        let point = evaluate(SpatialMode::Point);
        assert!(point.built.is_empty() && point.frame.is_none());
        for (x, y) in [(0, 0), (5, 7), (48, 32), (90, 60), (95, 63)] {
            assert_eq!(point.pixel(x, y).unwrap(), frames.pixel(x, y).unwrap());
        }

        let built: Vec<_> = [&frames, &point]
            .iter()
            .flat_map(|evaluation| evaluation.built.iter().map(|(frame, _)| frame.clone()))
            .collect();
        drop((frames, point));
        assert!(
            built.iter().all(|frame| frame.strong_count() == 0),
            "nothing outlives its evaluation"
        );
    }

    /// A sample from one `Compiled` shared by several points equals a sample that compiles for
    /// itself, at every point of a small stack with a colour layer: the split
    /// `HostStage::sample_before`'s RAW path takes to compile a prefix once and reuse it across the
    /// points it samples (TASK-015) reads the same values as compiling fresh for each point.
    #[test]
    fn sample_linear_compiled_from_a_shared_prefix_matches_sample_linear_per_point() {
        let registry = ModuleRegistry::builtin();
        let source = image(
            3,
            3,
            &[
                [0.1, 0.2, 0.3],
                [0.4, 0.5, 0.6],
                [0.7, 0.8, 0.9],
                [0.05, 0.15, 0.25],
                [0.35, 0.45, 0.55],
                [0.65, 0.75, 0.85],
                [0.02, 0.12, 0.22],
                [0.32, 0.42, 0.52],
                [0.62, 0.72, 0.82],
            ],
        );
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![Layer {
                id: crate::LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: crate::EFFECT_FORMAT,
                payload: serde_json::json!({"exposure": 0.4, "contrast": 8.0, "vibrance": -15.0}),
                mask: None,
                artifacts: Vec::new(),
            }],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let settings = LinearSettings {
            exposure_ev: 0.2,
            white_balance: None,
        };
        // Compiled once, as `HostStage::compiled_prefix` compiles a prefix once and clones it for
        // every point sampled from it, instead of every call in this loop compiling its own.
        let compiled = registry
            .compile_layers(
                3,
                3,
                &recipe.layers,
                &recipe.masks,
                &recipe.strokes,
                &recipe.artifacts,
            )
            .unwrap();
        for y in 0..3 {
            for x in 0..3 {
                let expected = sample_linear(&registry, &source, &recipe, settings, x, y).unwrap();
                let actual = crate::render::Render::compiled(
                    crate::RenderSource::Linear {
                        image: &source,
                        settings,
                    },
                    compiled.clone(),
                    crate::RenderOptions::default(),
                    crate::render::testing::context(),
                )
                .unwrap()
                .sample(x, y)
                .unwrap();
                assert_eq!(actual.rgba, expected.rgba, "({x}, {y})");
                assert_eq!(
                    (actual.width, actual.height),
                    (expected.width, expected.height),
                    "({x}, {y})"
                );
            }
        }
    }

    /// On a real RAW file, through Presence: a point sample equals the byte `render_linear` writes
    /// there, at points spread over the stage and the far corner, and the timings of the tile and
    /// of the frame a whole-stage evaluation builds are printed. Run in release with
    /// LIGHTWELL_RAW_FIXTURE pointing to a private qualified NEF, RAF or DNG:
    ///
    /// ```text
    /// LIGHTWELL_RAW_FIXTURE=/path/to/file.NEF cargo test --release -p lightwell-core --lib \
    ///   a_raw_point_sample_through_presence -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_raw_point_sample_through_presence_equals_the_render() {
        use std::time::Instant;
        let path =
            std::path::PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE").expect("fixture path"));
        let bytes = std::fs::read(&path).unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let prepared =
            crate::source::RawPrepared::decode(bytes, "sha256:point-sample".into(), None, &cancel)
                .unwrap();
        let image = prepared.linear.clone().unwrap();
        let registry = ModuleRegistry::builtin();
        let settings = LinearSettings::default();
        let ms = |start: Instant| start.elapsed().as_secs_f64() * 1e3;
        let median = |mut values: Vec<f64>| {
            values.sort_by(f64::total_cmp);
            (values[values.len() / 2], values[values.len() - 1])
        };
        println!("{}: {}x{}", path.display(), image.width(), image.height());
        for payload in [
            serde_json::json!({"clarity": 60.0}),
            serde_json::json!({"clarity": 60.0, "dehaze": 30.0}),
        ] {
            let stack = presence_stack(payload.clone());
            crate::render::testing::clear_estimates();
            let raster =
                render_linear(&registry, &image, SnapshotId::new(), &stack, settings).unwrap();
            let mut state = 0x2545_f491_4f6c_dd1d_u64;
            let mut points: Vec<(u32, u32)> = (0..40)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (
                        (state % u64::from(raster.width)) as u32,
                        ((state >> 32) % u64::from(raster.height)) as u32,
                    )
                })
                .collect();
            points.push((raster.width - 1, raster.height - 1));
            let mut samples = Vec::new();
            for &(x, y) in &points {
                let start = Instant::now();
                let sampled = sample_linear(&registry, &image, &stack, settings, x, y).unwrap();
                samples.push(ms(start));
                assert_eq!(sampled.rgba, raster.pixel(x, y), "{payload} at ({x}, {y})");
            }
            let mut frames = Vec::new();
            for _ in 0..3 {
                let start = Instant::now();
                linear_evaluation(
                    &registry,
                    &image,
                    &stack,
                    settings,
                    &Cancel::new(),
                    PRODUCTION_TILE,
                    SpatialMode::Frames,
                )
                .unwrap();
                frames.push(ms(start));
            }
            let (sample_p50, sample_max) = median(samples);
            let (frame_p50, _) = median(frames);
            println!(
                "{payload}: {} point samples equal the render; sample p50 {sample_p50:.1} ms, \
                 max {sample_max:.1} ms; the spatial frame alone p50 {frame_p50:.0} ms",
                points.len()
            );
        }
    }
}
