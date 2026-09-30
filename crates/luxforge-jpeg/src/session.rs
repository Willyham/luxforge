//! A libjpeg decompress session over `mozjpeg-sys` directly, for the two calls `mozjpeg`'s safe
//! API does not reach because they need the session's `cinfo`: `jpeg_crop_scanline`, which limits
//! decoding to a window of columns, and `jpeg_skip_scanlines`, which passes over rows without
//! producing them. It is the crate's one module of `unsafe` calls into libjpeg: each function that
//! makes one allows `unsafe_code` by name, beside a `SAFETY:` comment for each block.
//!
//! The session keeps the crate's contract as the `mozjpeg` sessions do:
//! - Every call into libjpeg runs under [`guarded`], with the decoder's own error manager
//!   ([`error_manager`]): an error, or a warning the table in `warnings.rs` refuses, unwinds out
//!   of libjpeg and becomes the same [`JpegError`] a full decode returns ([`decode_failure`]).
//! - The first failed call marks the session failed, and libjpeg is never called on it again;
//!   dropping the session only destroys it (`jpeg_destroy_decompress`, the one call libjpeg allows
//!   after an error, which cannot fail), then frees the error manager.
//! - The data is read in place through libjpeg's memory source (`jpeg_mem_src`, `jdatasrc.c`),
//!   which the session's lifetime ties to the caller's bytes. It fakes an EOI with
//!   `JWRN_JPEG_EOF` where the data ends, as `mozjpeg`'s source manager does, so the same files
//!   refuse; the one difference is a marker segment after the first scan whose length runs past
//!   the data, which libjpeg skips with `JWRN_JPEG_EOF` here ([`JpegError::Corrupt`]) and
//!   `mozjpeg` refuses with its fatal read error ([`JpegError::Undecodable`]). Both refuse.
//! - Output rows are written only into the caller's slice, checked against the output width and
//!   component count libjpeg reports before each call.

use crate::{
    JpegError,
    decode::{decode_failure, error_manager, guarded},
};
use mozjpeg_sys::{
    DCTSIZE, J_COLOR_SPACE, JDIMENSION, jpeg_create_decompress, jpeg_crop_scanline,
    jpeg_decompress_struct, jpeg_destroy_decompress, jpeg_error_mgr, jpeg_mem_src,
    jpeg_read_header, jpeg_read_scanlines, jpeg_save_markers, jpeg_skip_scanlines,
    jpeg_start_decompress,
};
use std::{marker::PhantomData, os::raw::c_ulong, ptr::NonNull};

/// The APP2 marker, whose segments carry the ICC profile.
const APP2: i32 = 0xe2;

/// libjpeg's `JPEG_HEADER_OK`: `jpeg_read_header` found an image, not only tables.
const HEADER_OK: i32 = 1;

/// The bytes each RGBA output pixel takes.
const RGBA: usize = 4;

/// One libjpeg decompress object reading `'a` bytes in place: header read at [`Session::open`],
/// then started, cropped, skipped and read row by row.
pub(crate) struct Session<'a> {
    /// Boxed so its address stays fixed for libjpeg across moves of the session.
    cinfo: Box<jpeg_decompress_struct>,
    /// The error manager `cinfo.common.err` points at, leaked from its `Box`: a raw pointer rather
    /// than a `Box`, so moving the session never asserts unique access to memory libjpeg reaches
    /// through that pointer. Freed in `drop`, after libjpeg is done with it.
    err: NonNull<jpeg_error_mgr>,
    /// Set by the first call that failed: libjpeg is not called again.
    failed: bool,
    /// libjpeg's memory source points into the caller's bytes for the session's whole life.
    bytes: PhantomData<&'a [u8]>,
}

/// What libjpeg's header says of the frame.
pub(crate) struct Frame {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) components: i32,
    pub(crate) colour_space: J_COLOR_SPACE,
}

impl<'a> Session<'a> {
    /// Create the decompress object on `bytes`, saving the APP2 segments, and read libjpeg's
    /// header. The container has been walked and checked before, so `bytes` hold at least SOI and
    /// EOI.
    #[allow(unsafe_code)]
    pub(crate) fn open(bytes: &'a [u8]) -> Result<Self, JpegError> {
        // libjpeg's memory source takes an `unsigned long`, 32 bits on Windows.
        let size = c_ulong::try_from(bytes.len()).map_err(|_| {
            JpegError::Internal("jpeg decode: data too large for libjpeg's memory source".into())
        })?;
        let err = NonNull::from(Box::leak(Box::new(error_manager())));
        // SAFETY: every field of `jpeg_decompress_struct` is an integer, a float, a raw pointer,
        // an `Option` of a function pointer or a `repr(C)` enum whose zero is a declared variant
        // (`JCS_UNKNOWN`, `JDCT_ISLOW`, `JDITHER_NONE`), so all zeros is a valid value, as
        // `mozjpeg` itself relies on; `jpeg_CreateDecompress` zeroes it again before use.
        let mut cinfo: Box<jpeg_decompress_struct> = unsafe { Box::new_zeroed().assume_init() };
        cinfo.common.err = err.as_ptr();
        // From here the session owns both: its drop destroys whatever libjpeg created (nothing, if
        // creation failed before the memory manager existed) and frees the error manager.
        let mut session = Self {
            cinfo,
            err,
            failed: false,
            bytes: PhantomData,
        };
        let found = session.call(|cinfo| {
            // SAFETY: `cinfo` is the session's own zeroed object whose `err` points at a live
            // error manager, as `jpeg_CreateDecompress` requires, with the struct size of these
            // bindings (`jpeg_create_decompress` passes it). The memory source reads `size` bytes
            // from `bytes`, which outlive the session (`'a`), and only reads them. The saved
            // markers are allocated in the session's own pools. Errors unwind through these calls
            // (the bindings are `C-unwind` and libjpeg is built with `-fexceptions`) and are
            // caught by `call`.
            unsafe {
                jpeg_create_decompress(cinfo);
                jpeg_mem_src(cinfo, bytes.as_ptr(), size);
                jpeg_save_markers(cinfo, APP2, 0xffff);
                // Not requiring an image, as `mozjpeg` does not: a file of tables only is refused
                // below rather than by libjpeg's fatal error.
                jpeg_read_header(cinfo, 0)
            }
        })?;
        if found != HEADER_OK {
            return Err(JpegError::Malformed(
                "JPEG header: no image in the JPEG file".into(),
            ));
        }
        Ok(session)
    }

    /// Run one call into libjpeg under [`guarded`]; a failure marks the session failed, and a
    /// failed session refuses every later call without reaching libjpeg.
    fn call<T>(
        &mut self,
        call: impl FnOnce(&mut jpeg_decompress_struct) -> T,
    ) -> Result<T, JpegError> {
        if self.failed {
            return Err(JpegError::Internal(
                "jpeg decode: the session has ended".into(),
            ));
        }
        let cinfo = &mut *self.cinfo;
        guarded(move || call(cinfo)).map_err(|payload| {
            self.failed = true;
            decode_failure(payload)
        })
    }

    /// The frame libjpeg's header describes.
    pub(crate) fn frame(&self) -> Frame {
        Frame {
            width: self.cinfo.image_width,
            height: self.cinfo.image_height,
            components: self.cinfo.num_components,
            colour_space: self.cinfo.jpeg_color_space,
        }
    }

    /// The payloads of the APP2 segments libjpeg saved from the header, in file order.
    #[allow(unsafe_code)]
    pub(crate) fn app2_segments(&self) -> Vec<&[u8]> {
        let mut segments = Vec::new();
        let mut marker = self.cinfo.marker_list;
        while !marker.is_null() {
            // SAFETY: `marker_list` is libjpeg's list of the segments it saved while reading the
            // header, each node and its `data_length` bytes allocated in the session's image pool.
            // They stay valid until the session is aborted, finished or destroyed, each of which
            // takes the session mutably, so not while `&self` (and the slices borrowed from it)
            // lives. libjpeg saved each payload in full (length limit 0xFFFF).
            let (next, code, data) = unsafe {
                let node = &*marker;
                (
                    node.next,
                    node.marker,
                    std::slice::from_raw_parts(node.data, node.data_length as usize),
                )
            };
            if i32::from(code) == APP2 {
                segments.push(data);
            }
            marker = next;
        }
        segments
    }

    /// Start decompressing to RGBA at full scale, keeping every other setting at the defaults
    /// `jpeg_read_header` chose, as the full decode does (the accurate integer IDCT and fancy
    /// upsampling). Returns the output size.
    #[allow(unsafe_code)]
    pub(crate) fn start_rgba(&mut self) -> Result<(u32, u32), JpegError> {
        self.cinfo.out_color_space = J_COLOR_SPACE::JCS_EXT_RGBA;
        let started = self.call(|cinfo| {
            // SAFETY: the header has been read, so the object is ready to start; a multi-scan
            // file's scans are read into libjpeg's coefficient buffer now, from the memory source
            // `open` set.
            unsafe { jpeg_start_decompress(cinfo) }
        })?;
        if started == 0 || self.cinfo.output_components as usize != RGBA {
            return Err(JpegError::Internal(
                "jpeg decode: libjpeg did not start an RGBA output".into(),
            ));
        }
        Ok((self.cinfo.output_width, self.cinfo.output_height))
    }

    /// The width of an iMCU column in output pixels at full scale: libjpeg aligns a crop's left
    /// edge to it.
    pub(crate) fn imcu_width(&self) -> u32 {
        DCTSIZE as u32 * self.cinfo.max_h_samp_factor.max(1) as u32
    }

    /// Decode only the columns from `x` for `width`: libjpeg moves `x` left to its iMCU boundary
    /// and widens `width` by as much. Returns the window libjpeg will decode, `(x, width)`. Before
    /// any row is read or skipped.
    #[allow(unsafe_code)]
    pub(crate) fn crop(&mut self, x: u32, width: u32) -> Result<(u32, u32), JpegError> {
        if width == 0
            || x.checked_add(width)
                .is_none_or(|end| end > self.cinfo.output_width)
        {
            return Err(JpegError::Internal(
                "jpeg decode: crop outside the image".into(),
            ));
        }
        let (mut offset, mut span): (JDIMENSION, JDIMENSION) = (x, width);
        self.call(|cinfo| {
            // SAFETY: decompression has started and no row has been read or skipped; the window
            // lies inside the output width (checked above), and libjpeg writes the aligned window
            // back through the two references, which live across the call.
            unsafe { jpeg_crop_scanline(cinfo, &mut offset, &mut span) }
        })?;
        if offset > x || offset + span != x + width || self.cinfo.output_width != span {
            return Err(JpegError::Internal(
                "jpeg decode: libjpeg cropped another window".into(),
            ));
        }
        Ok((offset, span))
    }

    /// Pass over the next `rows` rows without producing them. A single-scan file's data for them
    /// is still entropy-decoded, so its warnings are judged; a multi-scan file's was read at
    /// start.
    #[allow(unsafe_code)]
    pub(crate) fn skip(&mut self, rows: u32) -> Result<(), JpegError> {
        let remaining = self.cinfo.output_height - self.cinfo.output_scanline;
        if rows >= remaining {
            // libjpeg would end the input pass instead of skipping; the caller never needs that.
            return Err(JpegError::Internal(
                "jpeg decode: rows skipped to or past the end".into(),
            ));
        }
        let skipped = self.call(|cinfo| {
            // SAFETY: decompression has started and the memory source cannot suspend, which
            // `jpeg_skip_scanlines` requires; the rows skipped end before the last row.
            unsafe { jpeg_skip_scanlines(cinfo, rows) }
        })?;
        if skipped != rows {
            return Err(JpegError::Internal(
                "jpeg decode: libjpeg skipped another number of rows".into(),
            ));
        }
        Ok(())
    }

    /// Decode the next row into `row`, which must hold exactly one output row: the output width
    /// (the cropped window's, after [`Self::crop`]) of RGBA pixels.
    #[allow(unsafe_code)]
    pub(crate) fn read_row(&mut self, row: &mut [u8]) -> Result<(), JpegError> {
        let stride = self.cinfo.output_width as usize * self.cinfo.output_components as usize;
        if row.len() != stride || self.cinfo.output_scanline >= self.cinfo.output_height {
            return Err(JpegError::Internal(
                "jpeg decode: a row that is not the next output row".into(),
            ));
        }
        let mut target = row.as_mut_ptr();
        let read = self.call(|cinfo| {
            // SAFETY: libjpeg writes one row of `output_width` pixels of `output_components`
            // bytes each through the one pointer it is given, which points at `row`, exactly that
            // long (checked above), and borrowed mutably across the call.
            unsafe { jpeg_read_scanlines(cinfo, &mut target, 1) }
        })?;
        if read != 1 {
            return Err(JpegError::Malformed(
                "JPEG scan data: no row decoded".into(),
            ));
        }
        Ok(())
    }
}

impl Drop for Session<'_> {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `jpeg_destroy_decompress` releases every pool the session's memory manager
        // holds, or nothing when creation failed before there was one (`mem` null, which
        // `jpeg_CreateDecompress` sets first and the zeroed struct already held); it raises no
        // error, so nothing unwinds out of `drop`, and it is the call libjpeg allows after an error
        // interrupted the object. The error manager was leaked from its `Box` in `open`, is freed
        // nowhere else, and is freed only after libjpeg is done with the object that points at it.
        unsafe {
            jpeg_destroy_decompress(&mut self.cinfo);
            drop(Box::from_raw(self.err.as_ptr()));
        }
    }
}
