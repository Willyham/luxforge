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

    /// The base-plane step that a step of `(dx, dy)` in view coordinates takes: the linear part of
    /// [`Self::map`], a signed permutation for each orientation.
    #[inline]
    fn step(self, (dx, dy): (i64, i64)) -> Option<(i64, i64)> {
        Some(match self.orientation {
            1 => (dx, dy),
            2 => (-dx, dy),
            3 => (-dx, -dy),
            4 => (dx, -dy),
            5 => (dy, dx),
            6 => (dy, -dx),
            7 => (-dy, -dx),
            8 => (-dy, dx),
            _ => return None,
        })
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

#[cfg(test)]
thread_local! {
    /// How many constructions on this thread scanned their planes for a non-finite value: a test's
    /// view of whether a path took the validated constructor.
    static FINITENESS_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `work`'s result and how many [`LinearImage`] constructions on this thread scanned their planes
/// for a non-finite value meanwhile. The scan is the public constructors' guard on untrusted input;
/// a path that already knows its values are finite skips it, which this lets a test see.
#[cfg(test)]
pub(crate) fn finiteness_scans_during<T>(work: impl FnOnce() -> T) -> (T, usize) {
    let before = FINITENESS_SCANS.get();
    let result = work();
    (result, FINITENESS_SCANS.get() - before)
}

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

    /// Adopt planes whose producer guarantees every value is finite, without scanning them again:
    /// the RAW camera conversion, which checks every output value after its final math, and a
    /// proxy's area average, whose weighted mean of finite values is finite. This remains private
    /// to the core: public constructors must scan untrusted input and reject non-finite values.
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
        if !already_finite {
            #[cfg(test)]
            FINITENESS_SCANS.set(FINITENESS_SCANS.get() + 1);
            if planes.iter().any(|value| !value.is_finite()) {
                return Err(Error::validation(
                    "linear source contains a non-finite value",
                ));
            }
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

    /// This image as development `development`: a proxy's, which is derived from the development
    /// and view it was downscaled from and from its plan ([`crate::proxy`]), so two proxies of the
    /// same pixels share it.
    pub(crate) fn with_development(mut self, development: u64) -> Self {
        self.development = development;
        self
    }

    /// Return a cropped/oriented view without copying the source planes.
    ///
    /// `crop` is `[x, y, width, height]` in the base source-plane coordinates. EXIF orientation
    /// values 1 through 8 use the standard mappings and are applied exactly once to that crop.
    pub(crate) fn with_view(&self, crop: [u32; 4], orientation: u8) -> Result<Self, Error> {
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
    #[cfg(any(test, feature = "qualification"))]
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

    /// The viewed width: the content stage a recipe over this development is compiled against.
    pub fn width(&self) -> u32 {
        self.view.output_dimensions().0
    }

    /// The viewed height.
    pub fn height(&self) -> u32 {
        self.view.output_dimensions().1
    }

    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub(crate) fn view(&self) -> ([u32; 4], u8) {
        (
            [self.view.x, self.view.y, self.view.width, self.view.height],
            self.view.orientation,
        )
    }

    /// The development these planes belong to: equal for every view over the same adopted planes,
    /// different for every redevelopment, and never reused within the process.
    pub(crate) fn development(&self) -> u64 {
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

    #[cfg(test)]
    pub(crate) fn planes(&self) -> &[f32] {
        self.planes.as_slice()
    }

    /// What the GPU holds of this development (`docs/design/gpu-preview.md`, "The GPU source"):
    /// the planes every view of it shares, red then green then blue, each `base.0 × base.1` values
    /// row by row, borrowed, never copied; the base size; and this view's crop window
    /// `[x, y, width, height]` of the base planes and its EXIF orientation, which map the content
    /// stage a recipe is compiled against onto them. A caller that keeps the planes past this
    /// borrow keeps a clone of the image, so the source worker's memory gate still counts them.
    pub fn shared_planes(&self) -> (&[f32], (u32, u32), [u32; 4], u8) {
        let (crop, orientation) = self.view();
        (
            self.planes.as_slice(),
            (self.base_width, self.base_height),
            crop,
            orientation,
        )
    }

    /// A new development whose every base pixel is `map` of this one's, under the same view: a
    /// measurement's stand-in for a per-pixel evaluator setting that does not exist.
    #[cfg(test)]
    pub(crate) fn map_pixels(&self, map: impl Fn([f32; 3]) -> [f32; 3]) -> Result<Self, Error> {
        let n = self.base_width as usize * self.base_height as usize;
        let mut planes = vec![0.0; 3 * n];
        for index in 0..n {
            let mapped = map([
                self.planes[index],
                self.planes[n + index],
                self.planes[2 * n + index],
            ]);
            for (channel, value) in mapped.into_iter().enumerate() {
                planes[channel * n + index] = value;
            }
        }
        let mut image = Self::with_fingerprint(
            self.base_width,
            self.base_height,
            planes,
            self.fingerprint.clone(),
        )?;
        image.view = self.view;
        Ok(image)
    }

    /// Read one view pixel, in unbounded linear sRGB, without allocating, or `None` outside the
    /// view. A caller in the core that reads many pixels takes [`Self::reader`] once instead. The
    /// desktop's GPU-preview qualification harness reads a developed proxy through this, off any
    /// owner or UI thread, to hold it as a GPU boundary.
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

/// A rectangle of viewed pixels as the base planes hold it: the base-plane index of its first
/// pixel and the signed index step that one column and one row of it take. A view and the exact
/// geometry that reads through it are each a signed permutation with a translation, and so is
/// their composition, so every pixel of the rectangle lies a fixed step from the one before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Walk {
    first: usize,
    across: isize,
    down: isize,
    columns: usize,
    rows: usize,
}

impl Walk {
    /// How many rows the rectangle has.
    pub(crate) fn rows(self) -> usize {
        self.rows
    }

    /// Row `row` of the rectangle on its own.
    pub(crate) fn row(self, row: usize) -> Self {
        assert!(row < self.rows, "a walk's row lies inside it");
        Self {
            first: self.first.wrapping_add_signed(row as isize * self.down),
            rows: 1,
            ..self
        }
    }
}

impl<'a> ViewReader<'a> {
    /// Three planes laid out as a source's, `width` × `height` each, read through the identity
    /// view: a spatial operation's output frame, which the linear path holds that way.
    pub(crate) fn over(planes: &'a [f32], width: u32, height: u32) -> Self {
        let plane_len = width as usize * height as usize;
        debug_assert_eq!(planes.len(), 3 * plane_len, "three planes of the stage");
        Self {
            planes,
            base_width: width,
            plane_len,
            view: View {
                x: 0,
                y: 0,
                width,
                height,
                orientation: 1,
            },
            width,
            height,
        }
    }
}

impl ViewReader<'_> {
    pub(crate) fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// One viewed pixel, or `None` outside the view. [`LinearImage::pixel`] and the linear
    /// domain's point path read through this, and it is the reference every [`Walk`] is held to:
    /// a bulk read agrees with a point read because each lands where this says.
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

    /// The `columns` × `rows` rectangle of viewed pixels whose first pixel is `at` and whose
    /// columns and rows step `across` and `down` in view coordinates: what an output rectangle
    /// reads through an exact geometry, whose unmap gives its first pixel and its two unit steps.
    /// `None` when it is empty or a corner lies outside the view: the view is a rectangle and the
    /// walk is affine, so a rectangle whose corners are inside lies inside.
    pub(crate) fn walk(
        &self,
        at: (u32, u32),
        across: (i64, i64),
        down: (i64, i64),
        columns: u32,
        rows: u32,
    ) -> Option<Walk> {
        if columns == 0 || rows == 0 {
            return None;
        }
        let (last_column, last_row) = (i64::from(columns - 1), i64::from(rows - 1));
        let corner = |column: i64, row: i64| {
            (
                i64::from(at.0) + column * across.0 + row * down.0,
                i64::from(at.1) + column * across.1 + row * down.1,
            )
        };
        let inside = |(x, y): (i64, i64)| {
            (0..i64::from(self.width)).contains(&x) && (0..i64::from(self.height)).contains(&y)
        };
        let corners = [
            corner(0, 0),
            corner(last_column, 0),
            corner(0, last_row),
            corner(last_column, last_row),
        ];
        if !corners.into_iter().all(inside) {
            return None;
        }
        let (base_x, base_y) = self.view.map(at.0, at.1)?;
        let index_step = |step| {
            let (x, y) = self.view.step(step)?;
            isize::try_from(x + y * i64::from(self.base_width)).ok()
        };
        Some(Walk {
            first: base_y as usize * self.base_width as usize + base_x as usize,
            across: index_step(across)?,
            down: index_step(down)?,
            columns: columns as usize,
            rows: rows as usize,
        })
    }

    /// Every pixel of `walk`, handed to `visit` with its row-major offset in the rectangle and the
    /// values [`Self::pixel`] reads there, in an order that reads each base-plane row it touches
    /// once, as one contiguous span. A rectangle whose rows lie along base rows (an upright or
    /// flipped view) is read row by row. One whose rows step down a base column, as every
    /// portrait view's do, is read as a blocked transpose: each of its columns is one span of a
    /// base row, so the base rows it covers are read in order, and every row of the rectangle
    /// takes one value from each, instead of every pixel of a row touching a new base row of all
    /// three planes. A span one value apart is read as three slices. The first error `visit`
    /// returns stops the walk; the order decides which pixel reports it, never which values land
    /// where.
    pub(crate) fn visit<E>(
        &self,
        walk: Walk,
        mut visit: impl FnMut(usize, [f32; 3]) -> Result<(), E>,
    ) -> Result<(), E> {
        let (red, rest) = self.planes.split_at(self.plane_len);
        let (green, blue) = rest.split_at(self.plane_len);
        if walk.rows > 1 && walk.across.unsigned_abs() != 1 && walk.down.unsigned_abs() == 1 {
            // Columns in the order their base rows lie in the planes.
            for column in 0..walk.columns {
                let column = if walk.across < 0 {
                    walk.columns - 1 - column
                } else {
                    column
                };
                let start = walk
                    .first
                    .wrapping_add_signed(column as isize * walk.across);
                line(
                    [red, green, blue],
                    start,
                    walk.down,
                    walk.rows,
                    |row, rgb| visit(row * walk.columns + column, rgb),
                )?;
            }
            return Ok(());
        }
        for row in 0..walk.rows {
            let start = walk.first.wrapping_add_signed(row as isize * walk.down);
            let offset = row * walk.columns;
            line(
                [red, green, blue],
                start,
                walk.across,
                walk.columns,
                |column, rgb| visit(offset + column, rgb),
            )?;
        }
        Ok(())
    }

    /// A viewed row with its stride taken from the orientation alone: the row walk the linear
    /// rows used before [`Walk`], kept as a second reference beside [`Self::pixel`].
    #[cfg(test)]
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

/// `count` pixels of three planes from base-plane index `start`, `step` apart, handed to `visit`
/// in order with their position along the line. A step of one in either direction reads three
/// slices, so the line costs no index check per pixel; any other step indexes each pixel. Every
/// index lies inside a [`Walk`] its reader checked.
#[inline(always)]
fn line<E>(
    [red, green, blue]: [&[f32]; 3],
    start: usize,
    step: isize,
    count: usize,
    mut visit: impl FnMut(usize, [f32; 3]) -> Result<(), E>,
) -> Result<(), E> {
    match step {
        1 => {
            let span = start..start + count;
            let values = red[span.clone()].iter().zip(&green[span.clone()]);
            for (position, ((r, g), b)) in values.zip(&blue[span]).enumerate() {
                visit(position, [*r, *g, *b])?;
            }
        }
        -1 => {
            let span = start + 1 - count..start + 1;
            let values = red[span.clone()].iter().zip(&green[span.clone()]);
            for (position, ((r, g), b)) in values.zip(&blue[span]).rev().enumerate() {
                visit(position, [*r, *g, *b])?;
            }
        }
        _ => {
            for position in 0..count {
                let index = start.wrapping_add_signed(position as isize * step);
                visit(position, [red[index], green[index], blue[index]])?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `width` × `height` image whose every value is distinct and names its own place: base
    /// index `i` holds `i`, `i + 0.25` and `i + 0.5` in its three planes.
    fn indexed(width: u32, height: u32) -> LinearImage {
        let n = (width * height) as usize;
        let planes: Vec<f32> = (0..3)
            .flat_map(|channel| (0..n).map(move |index| index as f32 + channel as f32 * 0.25))
            .collect();
        LinearImage::new(width, height, planes).unwrap()
    }

    /// The eight unit steps a signed permutation can take one column or one row.
    const STEPS: [((i64, i64), (i64, i64)); 8] = [
        ((1, 0), (0, 1)),
        ((-1, 0), (0, 1)),
        ((1, 0), (0, -1)),
        ((-1, 0), (0, -1)),
        ((0, 1), (1, 0)),
        ((0, -1), (1, 0)),
        ((0, 1), (-1, 0)),
        ((0, -1), (-1, 0)),
    ];

    /// Every walk over every view, under each of the eight orientations and through crops touching
    /// each edge, visits each pixel of its rectangle exactly once, at its row-major offset, with
    /// the values [`ViewReader::pixel`] reads at the viewed pixel the steps reach; a walk with a
    /// corner outside the view is refused. Each value names its base index, so a pixel read one
    /// place over shows.
    #[test]
    fn every_walk_lands_where_pixel_says() {
        let source = indexed(7, 5);
        for orientation in 1..=8 {
            for crop in [
                [0, 0, 7, 5],
                [1, 1, 5, 3],
                [0, 0, 6, 4],
                [1, 1, 6, 4],
                [6, 4, 1, 1],
            ] {
                let view = source.with_view(crop, orientation).unwrap();
                let reader = view.reader();
                let (width, height) = reader.dimensions();
                for (across, down) in STEPS {
                    for y in 0..height {
                        for x in 0..width {
                            for columns in 1..=width.max(height) + 1 {
                                for rows in 1..=width.max(height) + 1 {
                                    let at_pixel = |column: u32, row: u32| {
                                        let x = i64::from(x)
                                            + i64::from(column) * across.0
                                            + i64::from(row) * down.0;
                                        let y = i64::from(y)
                                            + i64::from(column) * across.1
                                            + i64::from(row) * down.1;
                                        let x = u32::try_from(x).ok()?;
                                        let y = u32::try_from(y).ok()?;
                                        reader.pixel(x, y)
                                    };
                                    let inside = (0..rows).all(|row| {
                                        (0..columns).all(|column| at_pixel(column, row).is_some())
                                    });
                                    let what = || {
                                        format!(
                                            "orientation {orientation}, crop {crop:?}, steps \
                                             {across:?} {down:?}, at ({x}, {y}), \
                                             {columns}x{rows}"
                                        )
                                    };
                                    let walk = reader.walk((x, y), across, down, columns, rows);
                                    assert_eq!(walk.is_some(), inside, "{}", what());
                                    let Some(walk) = walk else {
                                        continue;
                                    };
                                    let mut seen = vec![None; (columns * rows) as usize];
                                    reader
                                        .visit(walk, |offset, rgb| {
                                            assert!(seen[offset].is_none(), "{}: {offset}", what());
                                            seen[offset] = Some(rgb.map(f32::to_bits));
                                            Ok::<(), ()>(())
                                        })
                                        .unwrap();
                                    for row in 0..rows {
                                        for column in 0..columns {
                                            let offset = (row * columns + column) as usize;
                                            assert_eq!(
                                                seen[offset],
                                                at_pixel(column, row).map(|p| p.map(f32::to_bits)),
                                                "{}: ({column}, {row})",
                                                what()
                                            );
                                        }
                                    }
                                    for row in 0..rows as usize {
                                        let single = walk.row(row);
                                        assert_eq!(single.rows(), 1);
                                        let mut values = Vec::new();
                                        reader
                                            .visit(single, |_, rgb| {
                                                values.push(rgb.map(f32::to_bits));
                                                Ok::<(), ()>(())
                                            })
                                            .unwrap();
                                        let start = row * columns as usize;
                                        let expected: Vec<_> = seen
                                            [start..start + columns as usize]
                                            .iter()
                                            .map(|value| value.unwrap())
                                            .collect();
                                        assert_eq!(values, expected, "{}: row {row}", what());
                                    }
                                }
                            }
                        }
                    }
                }
                assert!(reader.walk((0, 0), (1, 0), (0, 1), 0, 1).is_none());
                assert!(reader.walk((0, 0), (1, 0), (0, 1), 1, 0).is_none());
            }
        }
    }

    /// Every walk of more than one row and column, over every orientation and crop, reads each
    /// base row it touches once, as one span of adjacent values; a walk whose rows step down a
    /// base column, as a portrait view's chunks do, reads those base rows in the order the planes
    /// hold them. Each value names its base index, so the visits show the order of the reads.
    #[test]
    fn a_walk_reads_each_base_row_once_and_a_portrait_walk_reads_them_in_order() {
        let source = indexed(9, 7);
        let mut transposed = 0;
        for orientation in 1..=8 {
            for crop in [[0, 0, 9, 7], [1, 2, 7, 4], [2, 0, 7, 7]] {
                let view = source.with_view(crop, orientation).unwrap();
                let reader = view.reader();
                let (width, height) = reader.dimensions();
                for (across, down) in STEPS {
                    for (columns, rows) in [(width, height), (height, width), (3, 2), (2, 5)] {
                        for at in [
                            (0, 0),
                            (width - 1, height - 1),
                            (width - 1, 0),
                            (0, height - 1),
                        ] {
                            let Some(walk) = reader.walk(at, across, down, columns, rows) else {
                                continue;
                            };
                            if columns < 2 || rows < 2 {
                                continue;
                            }
                            let mut indices = Vec::new();
                            reader
                                .visit(walk, |_, rgb| {
                                    indices.push(rgb[0] as usize);
                                    Ok::<(), ()>(())
                                })
                                .unwrap();
                            let what = format!(
                                "orientation {orientation}, crop {crop:?}, steps {across:?} \
                                 {down:?}, {columns}x{rows} at {at:?}"
                            );
                            // The base rows, one entry per contiguous span of adjacent values.
                            let mut spans: Vec<usize> = Vec::new();
                            for pair in indices.windows(2) {
                                let adjacent =
                                    pair[0].abs_diff(pair[1]) == 1 && pair[0] / 9 == pair[1] / 9;
                                if !adjacent {
                                    spans.push(pair[0] / 9);
                                }
                            }
                            spans.push(indices[indices.len() - 1] / 9);
                            let mut sorted = spans.clone();
                            sorted.sort_unstable();
                            sorted.dedup();
                            assert_eq!(sorted.len(), spans.len(), "{what}: {indices:?}");
                            if walk.across.unsigned_abs() != 1 {
                                transposed += 1;
                                assert_eq!(sorted, spans, "{what}: {indices:?}");
                                assert_eq!(spans.len(), columns as usize, "{what}");
                            }
                        }
                    }
                }
            }
        }
        assert!(transposed > 0, "some walk steps down a base column");
    }

    /// A visit stops at the first error its visitor returns and hands that error back.
    #[test]
    fn a_walk_stops_at_the_first_error() {
        let source = indexed(5, 3);
        let reader = source.reader();
        let walk = reader.walk((0, 0), (1, 0), (0, 1), 5, 3).unwrap();
        let mut visited = 0;
        let error = reader.visit(walk, |offset, _| {
            visited += 1;
            if offset == 6 { Err(offset) } else { Ok(()) }
        });
        assert_eq!((error, visited), (Err(6), 7));
    }

    /// A frame's planes read through [`ViewReader::over`] are the frame's pixels where they lie.
    #[test]
    fn a_frame_reads_through_the_identity_view() {
        let source = indexed(6, 4);
        let frame = ViewReader::over(source.planes(), 6, 4);
        for y in 0..4 {
            for x in 0..6 {
                assert_eq!(frame.pixel(x, y), source.pixel(x, y));
            }
        }
        assert_eq!(frame.dimensions(), (6, 4));
    }
}
