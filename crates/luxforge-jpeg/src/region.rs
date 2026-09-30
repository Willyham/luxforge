//! One rectangle of a frame decoded at full scale without decoding the rest: libjpeg's cropped
//! decode (`jpeg_crop_scanline`) for its columns and skipped rows (`jpeg_skip_scanlines`) above it,
//! through the crate's own libjpeg session ([`Session`]), stopping after its last row.

use crate::{
    JpegError, Limits,
    decode::{check_declared, check_frame, supported_components},
    icc,
    session::Session,
};

/// A rectangle of a frame, in pixels: its top-left corner and its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    /// At least one pixel, and inside a `width` × `height` frame.
    fn inside(self, width: u32, height: u32) -> bool {
        self.width > 0
            && self.height > 0
            && u64::from(self.x) + u64::from(self.width) <= u64::from(width)
            && u64::from(self.y) + u64::from(self.height) <= u64::from(height)
    }
}

/// iMCU columns decoded left of the region. libjpeg takes a crop's left edge for the frame's, so a
/// pixel there comes out as a full decode would not give it: the fancy upsampler reads one chroma
/// sample left of a pixel, and a progressive frame's block smoothing (when its progression leaves
/// coefficients unrefined) estimates a block from the DC values of the two blocks left of it, in
/// every component. A region pixel's left chroma sample lies at most one block left of its own
/// block, so the window must hold that block and the two before it; an iMCU column holds at least
/// one whole block of every component, so three columns do. Two, as tests found, leave a region
/// that starts on an iMCU boundary with its left sample's block smoothed without its far
/// neighbour.
const LEFT_IMCUS: u32 = 3;

/// iMCU columns decoded right of the region: the fancy upsampler reads one chroma sample right of
/// a pixel, and takes the window's last sample for the frame's edge. Block smoothing reads the
/// blocks right of a block from libjpeg's whole-frame coefficients, so needs nothing here.
const RIGHT_IMCUS: u32 = 1;

/// One region decode: the header read and checked, then the region's RGBA rows on request.
///
/// libjpeg decodes a window of whole iMCU columns around the region, from `LEFT_IMCUS` left of
/// it (moved further left to an iMCU boundary) to `RIGHT_IMCUS` right of it, both clamped to the
/// frame, so that every pixel of the region is decoded with the neighbours a full decode gives it
/// and the rows equal the same rectangle cut from a full decode, byte for byte. The rows above the
/// region are skipped: a single-scan frame's data for them is still entropy-decoded, but not
/// transformed or upsampled, and libjpeg keeps the upsampler's context rows across the skip, so
/// no row margin is needed. Rows below the region are never decoded.
///
/// Warnings are judged, as a full decode judges them, for the data the region needs: for a
/// single-scan frame the scan from its start through the iMCU row that holds the region's last
/// row, and the one after it when the upsampler needs the rows below; for a multi-scan frame
/// (progressive, or one scan per component) every scan, which libjpeg reads before the first row.
/// The session ends after the region's last row and is destroyed, not finished to EOI: data after
/// what the region needs is never read, so a warning there (corrupt or missing data below the
/// region, bytes after the last scan) is not seen; the region's pixels do not depend on it.
pub struct RegionDecoder<'a> {
    /// `None` once the region's last row is read, or after a failed call.
    session: Option<Session<'a>>,
    /// The frame's size, as libjpeg's header gives it.
    frame: (u32, u32),
    region: Region,
    components: u8,
    icc: Option<Vec<u8>>,
    /// Set by the first read: the decoded window's row and where the region starts in it.
    window: Option<Window>,
    rows_read: u32,
}

/// One row of the decoded window, and the byte offset of the region's first column in it.
struct Window {
    row: Vec<u8>,
    offset: usize,
}

impl<'a> RegionDecoder<'a> {
    /// Walk the container and check the frame it declares against `limits` and the supported
    /// colour spaces, as [`crate::Decoder::new`] does, and check `region` against it; then read
    /// libjpeg's header, check its reading of the frame and the region the same way, and
    /// reassemble the ICC profile. A region that is empty or not inside the frame is
    /// [`JpegError::Internal`]: a call this crate refuses before libjpeg sees it, as it refuses
    /// rows past the image, since the rectangle is the caller's and not the file's
    /// ([`JpegError::Dimensions`] stays the frame against the limits). Decompression starts with
    /// the first [`Self::read_rows`].
    pub fn new(bytes: &'a [u8], limits: Limits, region: Region) -> Result<Self, JpegError> {
        let outside = || JpegError::Internal("jpeg region decode: region outside the image".into());
        if region.width == 0 || region.height == 0 {
            return Err(outside());
        }
        let declared = check_declared(bytes, limits)?;
        if !region.inside(declared.width, declared.height) {
            return Err(outside());
        }
        let session = Session::open(bytes)?;
        let frame = session.frame();
        check_frame(frame.width, frame.height, limits)?;
        let components = supported_components(frame.components as usize, frame.colour_space)?;
        if !region.inside(frame.width, frame.height) {
            return Err(outside());
        }
        let icc = icc::reassemble(session.app2_segments().into_iter())?;
        Ok(Self {
            session: Some(session),
            frame: (frame.width, frame.height),
            region,
            components,
            icc,
            window: None,
            rows_read: 0,
        })
    }

    /// The region's width.
    pub fn width(&self) -> u32 {
        self.region.width
    }

    /// The region's height.
    pub fn height(&self) -> u32 {
        self.region.height
    }

    /// 1 for greyscale, 3 for colour.
    pub fn components(&self) -> u8 {
        self.components
    }

    /// The reassembled ICC profile, when the file carries one.
    pub fn icc_profile(&self) -> Option<&[u8]> {
        self.icc.as_deref()
    }

    /// Decode the region's next whole rows into `rows`, RGBA8, `width * 4` bytes each. The first
    /// call that asks for rows starts decompression, crops it to the window and skips the rows
    /// above the region; the call that reads the last row ends the session.
    pub fn read_rows(&mut self, rows: &mut [u8]) -> Result<(), JpegError> {
        let stride = self.region.width as usize * 4;
        let count = rows.len() / stride;
        if !rows.len().is_multiple_of(stride)
            || count > (self.region.height - self.rows_read) as usize
        {
            return Err(JpegError::Internal(
                "jpeg region decode: rows requested beyond the region or not whole rows".into(),
            ));
        }
        if count == 0 {
            return Ok(());
        }
        let result = self.read_into(rows, stride);
        match result {
            Ok(()) => {
                self.rows_read += count as u32;
                if self.rows_read == self.region.height {
                    self.session = None;
                }
            }
            Err(_) => self.session = None,
        }
        result
    }

    fn read_into(&mut self, rows: &mut [u8], stride: usize) -> Result<(), JpegError> {
        let Some(session) = self.session.as_mut() else {
            return Err(JpegError::Internal(
                "jpeg region decode: the session has ended".into(),
            ));
        };
        let window = match &mut self.window {
            Some(window) => window,
            None => self.window.insert(start(session, self.frame, self.region)?),
        };
        for row in rows.chunks_exact_mut(stride) {
            session.read_row(&mut window.row)?;
            row.copy_from_slice(&window.row[window.offset..window.offset + stride]);
        }
        Ok(())
    }
}

/// Start `session` at full scale, crop it to the region's window and skip to the region's first
/// row. Returns the window's row buffer, the one allocation this crate makes for the decode beside
/// the caller's rows: the window's width, which is the region's and at most five iMCU columns
/// more (one of them the move to an iMCU boundary), times four bytes.
fn start(
    session: &mut Session<'_>,
    frame: (u32, u32),
    region: Region,
) -> Result<Window, JpegError> {
    if session.start_rgba()? != frame {
        return Err(JpegError::Internal(
            "jpeg region decode: output size differs from the header".into(),
        ));
    }
    let width = frame.0;
    let imcu = session.imcu_width();
    let left = region.x.saturating_sub(LEFT_IMCUS * imcu);
    let right = (region.x + region.width)
        .saturating_add(RIGHT_IMCUS * imcu)
        .min(width);
    let (x, span) = session.crop(left, right - left)?;
    if region.y > 0 {
        session.skip(region.y)?;
    }
    Ok(Window {
        row: vec![0; span as usize * 4],
        offset: (region.x - x) as usize * 4,
    })
}

#[cfg(test)]
mod tests;
