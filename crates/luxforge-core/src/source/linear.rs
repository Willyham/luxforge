//! The RAW source's own data type: immutable planar float RGB in linear sRGB/D65, and the views
//! that crop and orient it without copying its planes.

use super::{PlaneLease, PlanesHeld, Retention};
use crate::Error;
use std::sync::Arc;

const MAX_PIXELS: u64 = luxforge_raw::MAX_PIXELS as u64;
const MAX_SOURCE_BYTES: u64 = luxforge_raw::MAX_RGB_BYTES as u64;

/// The value count of three planes of a `width` × `height` linear source and the length of one
/// plane, refused past the RAW planar limit: the one bound a source and a spatial frame share.
pub(crate) fn layout(width: u32, height: u32) -> Result<(usize, usize), Error> {
    if width == 0 || height == 0 {
        return Err(Error::validation(
            "linear source dimensions must be nonzero",
        ));
    }
    if width > luxforge_raw::MAX_SIDE || height > luxforge_raw::MAX_SIDE {
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
    held: PlanesHeld,
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
            held: PlanesHeld::default(),
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

    /// A rectangle in this image's *oriented output* coordinates, composed into its existing
    /// base-plane view. The new image shares the same planar allocation and development identity.
    pub(crate) fn window(&self, region: crate::Region) -> Result<Self, Error> {
        if region.is_empty() || region.x1() > self.width() || region.y1() > self.height() {
            return Err(Error::validation(
                "linear source window lies outside the image",
            ));
        }
        let corners = [
            (region.x0, region.y0),
            (region.x1() - 1, region.y0),
            (region.x0, region.y1() - 1),
            (region.x1() - 1, region.y1() - 1),
        ];
        let mut x0 = u32::MAX;
        let mut y0 = u32::MAX;
        let mut x1 = 0;
        let mut y1 = 0;
        for (x, y) in corners {
            let (px, py) = self.view.map(x, y).ok_or_else(|| {
                Error::internal("a validated linear view could not map its window")
            })?;
            x0 = x0.min(px);
            y0 = y0.min(py);
            x1 = x1.max(px);
            y1 = y1.max(py);
        }
        self.with_view([x0, y0, x1 - x0 + 1, y1 - y0 + 1], self.view.orientation)
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

    /// The bytes of the planar allocation every view of this development shares.
    pub(crate) fn plane_bytes(&self) -> u64 {
        (self.planes.capacity() * std::mem::size_of::<f32>()) as u64
    }

    /// The memory gate's exemption for these planes while the returned hold lives, or `None` when
    /// no gate leased them ([`crate::source::SecondDevelopment`]).
    pub(crate) fn retention(&self) -> Option<Retention> {
        self.held.retention()
    }

    /// Make the source worker's memory gate wait for these planes: every view and clone taken
    /// from now on shares the hold, and the gate opens when the last of them drops. Called once,
    /// before the development is shared.
    pub(crate) fn hold(&mut self, lease: PlaneLease) {
        debug_assert_eq!(Arc::strong_count(&self.planes), 1, "hold before sharing");
        self.held = PlanesHeld::new(lease);
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

    /// Read one view pixel without allocating. This is also useful to a source-stage picker. A
    /// caller that reads many pixels takes [`Self::reader`] once instead.
    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[f32; 3]> {
        self.reader().pixel(x, y)
    }
}

/// Reads viewed pixels of a [`LinearImage`] without recomputing its layout per access. It borrows
/// the one immutable plane allocation and copies nothing.
#[derive(Clone, Copy)]
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

    /// One viewed pixel, or `None` outside the view. [`LinearImage::pixel`] and the linear
    /// domain's point path read through this, and [`Self::row`] walks the same mapping, which is
    /// what makes a bulk read agree with a point read.
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
    pub(crate) fn row(&self, y: u32) -> Option<impl ExactSizeIterator<Item = [f32; 3]> + '_> {
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
