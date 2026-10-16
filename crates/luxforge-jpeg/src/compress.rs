//! The encode's libjpeg compress session over `mozjpeg-sys` directly, for what `mozjpeg`'s safe
//! API does not reach because it needs the session's `cinfo`: libjpeg's progress monitor, which
//! reports an optimised encode's passes, and the mozjpeg parameters that choose the standard
//! (Annex K) quantisation tables under its trellis quantisation. Every encode, baseline or
//! optimised, runs through this one session; the baseline one makes the same calls in the same
//! order as `mozjpeg`'s `set_fastest_defaults` encode, so it writes the same bytes.
//!
//! The session keeps the crate's contract as the region decode's session does:
//! - Every call into libjpeg runs under [`guarded`]. libjpeg's fatal errors unwind out of it
//!   (the bindings are `C-unwind`, libjpeg is built with `-fexceptions`) carrying libjpeg's message,
//!   as `mozjpeg`'s own error manager's do, and the caller turns them into a `JpegError`.
//! - A session a failure interrupted is never called again; the caller drops it, and dropping only
//!   destroys it (`jpeg_destroy_compress`, which cannot fail) and frees what libjpeg pointed at.
//! - libjpeg writes through the session's own destination manager into a fixed buffer, which is
//!   written to the caller's writer whenever it fills and at the end. A writer failure raises
//!   libjpeg's own write error, so it unwinds as any other.
//! - During [`Session::finish`] the progress monitor calls the caller's hook between libjpeg's
//!   passes and within them. A hook that answers `false` stops the encode the only way libjpeg can
//!   be left part way through a call: through its error path, unwinding with [`Stopped`], as every
//!   libjpeg error in this crate does.
//!
//! Each function that makes an `unsafe` call allows `unsafe_code` by name, beside a `SAFETY:`
//! comment for each block.

use crate::{Settings, decode::guarded};
use mozjpeg_sys::{
    J_BOOLEAN_PARAM, J_COLOR_SPACE, J_INT_PARAM, JCP_FASTEST, JERR_FILE_WRITE, JPEG_LIB_VERSION,
    JSAMPROW, boolean, c_int, c_uint, jpeg_c_set_bool_param, jpeg_c_set_int_param,
    jpeg_common_struct, jpeg_compress_struct, jpeg_destination_mgr, jpeg_destroy_compress,
    jpeg_error_mgr, jpeg_finish_compress, jpeg_progress_mgr, jpeg_set_defaults, jpeg_set_quality,
    jpeg_simple_progression, jpeg_start_compress, jpeg_std_error, jpeg_write_marker,
    jpeg_write_scanlines,
};
use std::{any::Any, io, io::Write, mem, panic, ptr::NonNull};

/// What a stopped call unwinds with: libjpeg's error, or a hook's stop.
pub(crate) type Payload = Box<dyn Any + Send>;

/// The payload a hook's stop unwinds with.
pub(crate) struct Stopped;

/// Where libjpeg's passes are, as its progress monitor reports them: `completed` of `total` passes
/// done, and `counter` of `limit` units of the pass under way.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Passes {
    pub(crate) completed: u32,
    pub(crate) total: u32,
    pub(crate) counter: u64,
    pub(crate) limit: u64,
}

/// The hook [`Session::finish`] installs, its lifetime erased: `false` stops the encode.
type Hook = dyn FnMut(Passes) -> bool + 'static;

/// libjpeg's progress manager and the hook the monitor calls, with the manager first so that
/// `cinfo.progress` points at the whole.
#[repr(C)]
struct Monitor {
    iface: jpeg_progress_mgr,
    /// The hook, set only for the length of one [`Session::finish`] call. Its lifetime is erased:
    /// it is cleared before that call returns, however it returns.
    hook: Option<NonNull<Hook>>,
}

/// libjpeg's destination manager writing into `buffer`, emptied into `writer`, with the manager
/// first so that `cinfo.dest` points at the whole.
#[repr(C)]
struct Destination<W> {
    iface: jpeg_destination_mgr,
    buffer: Box<[u8]>,
    writer: W,
}

/// One libjpeg compress object writing into `W`.
pub(crate) struct Session<W> {
    /// Boxed so its address stays fixed for libjpeg across moves of the session.
    cinfo: Box<jpeg_compress_struct>,
    /// What `cinfo` points at, leaked from their `Box`es: raw pointers rather than `Box`es, so
    /// moving the session never asserts unique access to memory libjpeg reaches through `cinfo`.
    /// Freed in `drop`, after libjpeg is destroyed.
    err: NonNull<jpeg_error_mgr>,
    monitor: NonNull<Monitor>,
    dest: NonNull<Destination<W>>,
}

impl<W: Write> Session<W> {
    /// Create the compress object, set it up from `settings` for a `width` × `height` RGBA8 image
    /// and start it: libjpeg writes SOI and the JFIF APP0 into `writer`. Answers the session and
    /// libjpeg's pass count, 1 for a baseline encode. A failure answers libjpeg's unwind, after
    /// which the session has been dropped.
    #[allow(unsafe_code)]
    pub(crate) fn start(
        writer: W,
        width: u32,
        height: u32,
        settings: &Settings<'_>,
    ) -> Result<(Self, u32), Payload> {
        // `mozjpeg`'s buffer: the size of a 1-bit-a-pixel file, rounded to 4 KiB, from 4 to 64 KiB.
        let expected = (width as usize * height as usize / 8 + 4095) & !4095;
        let buffer = vec![0; expected.clamp(1 << 12, 1 << 16)].into_boxed_slice();
        let dest = NonNull::from(Box::leak(Box::new(Destination {
            iface: jpeg_destination_mgr {
                next_output_byte: std::ptr::null_mut(),
                free_in_buffer: 0,
                init_destination: Some(init_destination::<W>),
                empty_output_buffer: Some(empty_output_buffer::<W>),
                term_destination: Some(term_destination::<W>),
            },
            buffer,
            writer,
        })));
        let monitor = NonNull::from(Box::leak(Box::new(Monitor {
            iface: jpeg_progress_mgr {
                progress_monitor: Some(monitor),
                pass_counter: 0,
                pass_limit: 0,
                completed_passes: 0,
                total_passes: 0,
            },
            hook: None,
        })));
        // SAFETY: `jpeg_std_error` only fills in the manager it is given, on the heap here.
        let err = NonNull::from(Box::leak(Box::new(unsafe {
            let mut err: jpeg_error_mgr = mem::zeroed();
            jpeg_std_error(&mut err);
            err
        })));
        // SAFETY: as for the region decode's `jpeg_decompress_struct`, every field of
        // `jpeg_compress_struct` is an integer, a float, a raw pointer, an `Option` of a function
        // pointer or a `repr(C)` enum whose zero is a declared variant, so all zeros is a valid
        // value, as `mozjpeg` itself relies on; `jpeg_CreateCompress` zeroes it again before use.
        let mut cinfo: Box<jpeg_compress_struct> = unsafe { Box::new_zeroed().assume_init() };
        cinfo.common.err = err.as_ptr();
        // SAFETY: `err` is the live manager just built; only the session reads it.
        unsafe {
            let err = &mut *err.as_ptr();
            err.error_exit = Some(error_exit);
            err.emit_message = Some(silent_message);
        }
        // From here the session owns all four: its drop destroys whatever libjpeg created
        // (nothing, if creation failed before the memory manager existed) and frees the rest.
        let mut session = Self {
            cinfo,
            err,
            monitor,
            dest,
        };
        let (luma, chroma) = sampling(settings.chroma);
        let passes = session
            .call(|cinfo| {
                // SAFETY: `cinfo` is the session's own zeroed object whose `err` points at a live error
                // manager, as `jpeg_CreateCompress` requires, with the struct size of these bindings.
                // The setters follow `mozjpeg`'s: `jpeg_set_defaults` before the parameters it resets,
                // and the components' sampling written through `comp_info`, which `jpeg_set_defaults`
                // allocated for the three components of `jpeg_default_colorspace`'s YCbCr. `dest` and
                // `progress` point at the session's own managers, which outlive `cinfo`. Errors unwind
                // through these calls and are caught by `call`.
                unsafe {
                    mozjpeg_sys::jpeg_CreateCompress(
                        cinfo,
                        JPEG_LIB_VERSION,
                        mem::size_of::<jpeg_compress_struct>(),
                    );
                    cinfo.in_color_space = J_COLOR_SPACE::JCS_EXT_RGBA;
                    cinfo.input_components = 4;
                    // The default profile, mozjpeg's most compressing: progressive scans, optimised
                    // Huffman tables and trellis quantisation.
                    jpeg_set_defaults(cinfo);
                    if settings.optimise {
                        // Its standard quantisation tables rather than its own, so that a quality
                        // means the same tables as the baseline's.
                        jpeg_c_set_int_param(cinfo, J_INT_PARAM::JINT_BASE_QUANT_TBL_IDX, 0);
                        // Its fixed progressive script rather than its scan search, which would keep
                        // every candidate scan in memory outside libjpeg's pools and skip passes.
                        jpeg_c_set_bool_param(cinfo, J_BOOLEAN_PARAM::JBOOLEAN_OPTIMIZE_SCANS, 0);
                        jpeg_simple_progression(cinfo);
                    } else {
                        // libjpeg-turbo's own profile: baseline, one interleaved scan, the standard
                        // Huffman tables and no trellis quantisation.
                        jpeg_c_set_int_param(
                            cinfo,
                            J_INT_PARAM::JINT_COMPRESS_PROFILE,
                            JCP_FASTEST as c_int,
                        );
                        jpeg_set_defaults(cinfo);
                    }
                    cinfo.image_width = width;
                    cinfo.image_height = height;
                    jpeg_set_quality(cinfo, c_int::from(settings.quality), 0);
                    let components = std::slice::from_raw_parts_mut(
                        cinfo.comp_info,
                        cinfo.num_components as usize,
                    );
                    for (component, (h, v)) in components.iter_mut().zip([luma, chroma, chroma]) {
                        component.h_samp_factor = h;
                        component.v_samp_factor = v;
                    }
                    if let Some(density) = settings.pixels_per_inch {
                        cinfo.density_unit = 1;
                        cinfo.X_density = density;
                        cinfo.Y_density = density;
                    }
                    cinfo.dest = dest.as_ptr().cast();
                    cinfo.common.progress = monitor.as_ptr().cast();
                    jpeg_start_compress(cinfo, 1);
                }
            })
            .map(|()| {
                // SAFETY: libjpeg wrote the pass count into the session's own monitor during
                // `jpeg_start_compress`, which has returned.
                unsafe { (*monitor.as_ptr()).iface.total_passes }
            })?;
        Ok((session, u32::try_from(passes).unwrap_or(1).max(1)))
    }

    /// Run one call into libjpeg under [`guarded`].
    fn call<T>(&mut self, call: impl FnOnce(&mut jpeg_compress_struct) -> T) -> Result<T, Payload> {
        let cinfo = &mut *self.cinfo;
        guarded(|| call(cinfo))
    }

    /// Write one marker segment, `marker` and its payload, after the header written so far.
    #[allow(unsafe_code)]
    pub(crate) fn write_marker(&mut self, marker: c_int, payload: &[u8]) -> Result<(), Payload> {
        self.call(|cinfo| {
            // SAFETY: the session is started; libjpeg copies `payload`, at most 65,533 bytes (the
            // caller checked), during the call.
            unsafe {
                jpeg_write_marker(cinfo, marker, payload.as_ptr(), payload.len() as c_uint);
            }
        })
    }

    /// Hand libjpeg `rows`, whole rows of `stride` bytes and at most [`crate::STRIP_ROWS`] of
    /// them, below those already given. libjpeg takes every row it is given unless it suspends,
    /// which this destination never does; a call that takes none is refused as `mozjpeg` refuses
    /// it.
    #[allow(unsafe_code)]
    pub(crate) fn write_scanlines(
        &mut self,
        rows: &[u8],
        stride: usize,
    ) -> Result<io::Result<()>, Payload> {
        let mut pointers = [std::ptr::null(); crate::STRIP_ROWS];
        let mut count = 0;
        for (pointer, row) in pointers.iter_mut().zip(rows.chunks_exact(stride)) {
            *pointer = row.as_ptr();
            count += 1;
        }
        self.call(|cinfo| {
            let mut given: &[JSAMPROW] = &pointers[..count];
            while !given.is_empty() {
                // SAFETY: each pointer is a row of `stride` bytes, the image's width of RGBA8,
                // inside `rows`, which outlives the call; libjpeg only reads them.
                let written =
                    unsafe { jpeg_write_scanlines(cinfo, given.as_ptr(), given.len() as c_uint) }
                        as usize;
                if written == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                given = &given[written.min(given.len())..];
            }
            Ok(())
        })
    }

    /// Finish the file: libjpeg runs the passes left, if any, and writes the trailer, then the
    /// destination writes what it still buffers and flushes the writer. `hook` is called from the
    /// progress monitor between and within those passes; a `false` from it unwinds with
    /// [`Stopped`]. A baseline encode has no passes left, so the hook is not called.
    #[allow(unsafe_code)]
    pub(crate) fn finish(
        &mut self,
        hook: &mut (dyn FnMut(Passes) -> bool + '_),
    ) -> Result<(), Payload> {
        let monitor = self.monitor;
        let hook: NonNull<dyn FnMut(Passes) -> bool + '_> = NonNull::from(hook);
        // SAFETY: only the lifetime is erased. The hook pointer lives in the session's own monitor
        // until this call returns: it is cleared below on every path, the unwinds `call` catches
        // included, so the monitor never calls it after its borrow ends.
        unsafe {
            (*monitor.as_ptr()).hook = Some(mem::transmute::<
                NonNull<dyn FnMut(Passes) -> bool + '_>,
                NonNull<Hook>,
            >(hook));
        }
        let result = self.call(|cinfo| {
            // SAFETY: the session is started and every row has been given (the caller checked).
            unsafe { jpeg_finish_compress(cinfo) }
        });
        // SAFETY: as above; libjpeg has returned or unwound, so nothing reads the monitor now.
        unsafe { (*monitor.as_ptr()).hook = None };
        result
    }
}

/// libjpeg's private memory manager's count of its pools' bytes, `total_space_allocated` in
/// `jmemmgr.c`'s `my_memory_mgr`: after the public manager, two small-pool and two large-pool list
/// heads and the two virtual-array lists. Pinned to the vendored libjpeg-turbo of `mozjpeg-sys`
/// 2.2.3; test only.
#[cfg(test)]
#[allow(unsafe_code)]
unsafe fn pools(common: &jpeg_common_struct) -> usize {
    let offset = mem::size_of::<mozjpeg_sys::jpeg_memory_mgr>() + 6 * mem::size_of::<*const u8>();
    // SAFETY: `mem` is the session's live memory manager, allocated as a `my_memory_mgr`, whose
    // layout the offset follows.
    unsafe { *common.mem.cast::<u8>().add(offset).cast::<usize>() }
}

#[cfg(test)]
thread_local! {
    /// The most bytes libjpeg's pools held at any progress report on this thread since the last
    /// [`take_peak_pool_bytes`].
    static PEAK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The peak of libjpeg's pool bytes over this thread's encodes since the last call.
#[cfg(test)]
pub(crate) fn take_peak_pool_bytes() -> usize {
    PEAK.with(|peak| peak.replace(0))
}

impl<W> Drop for Session<W> {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `jpeg_destroy_compress` is the one call libjpeg allows on any object, failed or
        // not, and it frees only libjpeg's own pools (it does nothing if creation never got as far
        // as the memory manager). Afterwards nothing points at the managers, which were leaked from
        // their `Box`es in `start` and are freed here once each.
        unsafe {
            jpeg_destroy_compress(&mut self.cinfo);
            drop(Box::from_raw(self.dest.as_ptr()));
            drop(Box::from_raw(self.monitor.as_ptr()));
            drop(Box::from_raw(self.err.as_ptr()));
        }
    }
}

/// The luma and chroma components' sampling factors for chroma samples each covering `(h, v)`
/// pixels, as `mozjpeg`'s `set_chroma_sampling_pixel_sizes` sets them: luma `(h, v)`, chroma
/// `(1, 1)`. A zero is taken as 1.
pub(crate) fn sampling((h, v): (u8, u8)) -> ((c_int, c_int), (c_int, c_int)) {
    ((c_int::from(h.max(1)), c_int::from(v.max(1))), (1, 1))
}

/// libjpeg's fatal error: unwind with its message, as `mozjpeg`'s error manager does.
#[allow(unsafe_code)]
extern "C-unwind" fn error_exit(cinfo: &mut jpeg_common_struct) {
    let mut buffer = [0u8; 80];
    // SAFETY: `err` is the session's manager, filled by `jpeg_std_error`, whose `format_message`
    // writes at most 80 bytes, NUL-terminated, into the buffer. The bindings declare the buffer
    // shared, as `mozjpeg` notes; it is the one place libjpeg writes it.
    let message = unsafe {
        match (*cinfo.err).format_message {
            Some(format) => {
                let format = mem::transmute::<
                    unsafe extern "C-unwind" fn(&mut jpeg_common_struct, &[u8; 80]),
                    unsafe extern "C-unwind" fn(&mut jpeg_common_struct, &mut [u8; 80]),
                >(format);
                format(cinfo, &mut buffer);
                let text = buffer.split(|&byte| byte == 0).next().unwrap_or_default();
                String::from_utf8_lossy(text).into_owned()
            }
            None => format!("code {}", (*cinfo.err).msg_code),
        }
    };
    panic::resume_unwind(Box::new(format!("libjpeg fatal error: {message}")));
}

extern "C-unwind" fn silent_message(_: &mut jpeg_common_struct, _: c_int) {}

/// Raise libjpeg's write error on `cinfo`, which unwinds.
#[allow(unsafe_code)]
fn write_failed(cinfo: &mut jpeg_compress_struct) -> ! {
    // SAFETY: `err` is the session's live manager.
    unsafe { (*cinfo.common.err).msg_code = JERR_FILE_WRITE as c_int };
    error_exit(&mut cinfo.common);
    unreachable!("libjpeg's error exit unwinds")
}

/// The session's destination behind `cinfo.dest`.
///
/// # Safety
/// `cinfo` is a session's object, whose `dest` points at that session's `Destination<W>` (libjpeg
/// calls the destination callbacks only while its own `dest` is set to it).
#[allow(unsafe_code)]
unsafe fn destination<W>(cinfo: &mut jpeg_compress_struct) -> &mut Destination<W> {
    // SAFETY: by the function's contract; libjpeg reaches the destination only through `cinfo`,
    // which the callback holds exclusively, so no other reference to it is live.
    unsafe { &mut *cinfo.dest.cast::<Destination<W>>() }
}

#[allow(unsafe_code)]
extern "C-unwind" fn init_destination<W>(cinfo: &mut jpeg_compress_struct) {
    // SAFETY: libjpeg calls this with the session's own `cinfo`.
    let dest = unsafe { destination::<W>(cinfo) };
    dest.iface.next_output_byte = dest.buffer.as_mut_ptr();
    dest.iface.free_in_buffer = dest.buffer.len();
}

/// libjpeg filled the whole buffer (`jcphuff.c` relies on its being written whole, whatever
/// `free_in_buffer` says).
#[allow(unsafe_code)]
extern "C-unwind" fn empty_output_buffer<W: Write>(cinfo: &mut jpeg_compress_struct) -> boolean {
    // SAFETY: libjpeg calls this with the session's own `cinfo`.
    let dest = unsafe { destination::<W>(cinfo) };
    if dest.writer.write_all(&dest.buffer).is_err() {
        write_failed(cinfo);
    }
    dest.iface.next_output_byte = dest.buffer.as_mut_ptr();
    dest.iface.free_in_buffer = dest.buffer.len();
    1
}

#[allow(unsafe_code)]
extern "C-unwind" fn term_destination<W: Write>(cinfo: &mut jpeg_compress_struct) {
    // SAFETY: libjpeg calls this with the session's own `cinfo`.
    let dest = unsafe { destination::<W>(cinfo) };
    let used = dest.buffer.len().saturating_sub(dest.iface.free_in_buffer);
    if dest.writer.write_all(&dest.buffer[..used]).is_err() || dest.writer.flush().is_err() {
        write_failed(cinfo);
    }
    dest.iface.free_in_buffer = 0;
}

/// libjpeg's progress monitor: forwards the passes to the hook while [`Session::finish`] has one
/// installed, and unwinds with [`Stopped`] when the hook says stop.
#[allow(unsafe_code)]
extern "C-unwind" fn monitor(cinfo: &mut jpeg_common_struct) {
    #[cfg(test)]
    {
        // SAFETY: the session's live memory manager, as `pools` requires.
        let bytes = unsafe { pools(cinfo) };
        PEAK.with(|peak| peak.set(peak.get().max(bytes)));
    }
    // SAFETY: `progress` points at the session's own monitor, set before `jpeg_start_compress`.
    let monitor = unsafe { &mut *cinfo.progress.cast::<Monitor>() };
    let Some(mut hook) = monitor.hook else {
        return;
    };
    let passes = Passes {
        completed: u32::try_from(monitor.iface.completed_passes).unwrap_or(0),
        total: u32::try_from(monitor.iface.total_passes)
            .unwrap_or(1)
            .max(1),
        counter: u64::try_from(monitor.iface.pass_counter).unwrap_or(0),
        limit: u64::try_from(monitor.iface.pass_limit).unwrap_or(0),
    };
    // SAFETY: the hook is set only while `Session::finish` holds its borrow, and libjpeg calls the
    // monitor on that call's thread, inside it.
    if !unsafe { hook.as_mut() }(passes) {
        panic::resume_unwind(Box::new(Stopped));
    }
}
