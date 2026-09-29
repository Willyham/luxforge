//! The development of a retained mosaic: the Rust normalization, the one native demosaic call and
//! the output scale, through [`develop_with`]. Its items are crate-private.

use crate::{
    CancelCallback, MAX_GAIN, MAX_RGB_BYTES, NativeMetadata, PARALLEL_PIXELS, PlanarRgb, RawError,
    RawSource, cancelled, native_result, native_tiles, normalize,
};
use std::{
    ffi::{c_char, c_int, c_void},
    sync::atomic::{AtomicBool, Ordering},
};

/// How [`develop_with`] runs a development. The default is production's: every pass on the
/// development executor at the shared pool's width, without diagnostics.
pub(crate) struct DevelopOptions<'a> {
    /// Zero chooses the shared pool's width; nonzero is an exactness-test override, no wider than
    /// the pool. The native tile jobs also keep their eight-lane cap
    /// ([`native_tiles::ExecutorContext`]); the Rust passes do not ([`development_lanes`]).
    pub worker_limit: usize,
    /// Whether the native tile jobs and the Rust passes run on the development executor; `false`
    /// runs every pass in order on the caller.
    pub executor: bool,
    /// Where to record the normalization's and the demosaic's wall time, for the ignored release
    /// profiles.
    pub diagnostics: Option<&'a mut DevelopDiagnostics>,
}

impl Default for DevelopOptions<'_> {
    fn default() -> Self {
        Self {
            worker_limit: 0,
            executor: true,
            diagnostics: None,
        }
    }
}

/// The wall time of a development's normalization and of its native demosaic call, for the
/// ignored release profiles.
#[derive(Debug, Default)]
pub(crate) struct DevelopDiagnostics {
    pub normalization_ns: u64,
    pub demosaic_ns: u64,
}

/// What the native demosaic reads of a frame: its dimensions, its CFA and, for X-Trans, the
/// camera-to-RGB matrix Markesteijn's homogeneity test uses. The decode's `NativeMetadata` stays
/// with the decode. The C++ adapter declares the same layout as `LfDemosaicShape`.
#[repr(C)]
#[derive(Debug, Clone)]
pub(crate) struct DemosaicShape {
    pub width: u32,
    pub height: u32,
    pub cfa_width: u32,
    pub cfa_height: u32,
    /// Row-major CFA channels, green 1 at both Bayer green sites; `cfa_width * cfa_height` used.
    pub cfa: [u8; 36],
    /// LibRaw's three rows of four camera-to-sRGB coefficients.
    pub rgb_cam: [f32; 12],
}

// The adapter asserts the same size, so a field changed on one side fails the build.
const _: () = assert!(std::mem::size_of::<DemosaicShape>() == 100);

impl DemosaicShape {
    pub(crate) fn of(native: &NativeMetadata) -> Self {
        Self {
            width: native.width,
            height: native.height,
            cfa_width: native.cfa_width,
            cfa_height: native.cfa_height,
            cfa: native.cfa,
            rgb_cam: native.rgb_cam,
        }
    }
}

unsafe extern "C" {
    fn lf_raw_develop(
        mosaic: *const f32,
        count: usize,
        shape: *const DemosaicShape,
        red: *mut f32,
        green: *mut f32,
        blue: *mut f32,
        executor: Option<native_tiles::TileExecutor>,
        executor_context: *mut c_void,
        cancel: CancelCallback,
        cancel_context: *mut c_void,
        err: *mut c_char,
        err_len: usize,
    ) -> c_int;
}

/// Normalize `raw`'s retained mosaic with `gains`, demosaic it natively and divide the planes by
/// the sensor scale, as `options` say. The DNG corrections are the caller's.
pub(crate) fn develop_with(
    raw: &RawSource,
    gains: [f32; 3],
    cancel: &AtomicBool,
    options: DevelopOptions<'_>,
) -> Result<PlanarRgb, RawError> {
    let DevelopOptions {
        worker_limit,
        executor,
        mut diagnostics,
    } = options;
    if cancel.load(Ordering::Relaxed) {
        return Err(RawError::Cancelled);
    }
    if !gains
        .iter()
        .all(|v| v.is_finite() && *v > 0.0 && *v <= MAX_GAIN)
        || (gains[1] - 1.0).abs() > 1e-6
    {
        return Err(RawError::InvalidInput(
            "WB gains must be finite, positive, green-normalized, <=32",
        ));
    }
    let n = raw.mosaic.len();
    let rgb_bytes = n
        .checked_mul(3)
        .and_then(|v| v.checked_mul(std::mem::size_of::<f32>()))
        .ok_or(RawError::ResourceLimit("RGB allocation overflow"))?;
    if rgb_bytes > MAX_RGB_BYTES {
        return Err(RawError::ResourceLimit("RGB planes exceed 1.5 GiB"));
    }
    let lanes = development_lanes(n, worker_limit, executor);
    let clock = diagnostics.is_some().then(std::time::Instant::now);
    let mosaic = raw.normalization(gains).run(lanes, cancel)?;
    if let (Some(diagnostics), Some(clock)) = (diagnostics.as_deref_mut(), clock) {
        diagnostics.normalization_ns = clock.elapsed().as_nanos() as u64;
    }
    let mut data = Vec::new();
    data.try_reserve_exact(n * 3)
        .map_err(|_| RawError::ResourceLimit("RGB plane allocation"))?;
    data.resize(n * 3, 0.0);
    let mut executor_context = native_tiles::ExecutorContext {
        cancel,
        worker_limit,
    };
    let clock = diagnostics.is_some().then(std::time::Instant::now);
    native_demosaic(
        &mosaic,
        &raw.shape,
        &mut data,
        executor.then_some((
            native_tiles::execute as native_tiles::TileExecutor,
            (&mut executor_context as *mut native_tiles::ExecutorContext<'_>).cast(),
        )),
        cancelled,
        (cancel as *const AtomicBool).cast_mut().cast(),
    )?;
    if let (Some(diagnostics), Some(clock)) = (diagnostics, clock) {
        diagnostics.demosaic_ns = clock.elapsed().as_nanos() as u64;
    }
    drop(mosaic);
    normalize::scale_planes(&mut data, raw.metadata.sensor_width as usize, lanes, cancel)?;
    Ok(PlanarRgb {
        width: raw.metadata.sensor_width,
        height: raw.metadata.sensor_height,
        data,
    })
}

/// The native demosaic of a normalized mosaic of `shape`, in which sensor white is 65535, into
/// `planes`, three contiguous planes of the mosaic's length at the same scale. `executor` runs its
/// tile jobs; without one they run in order on the caller. `cancel` is called before the
/// demosaic, before every tile and after it. This is the crate's one `lf_raw_develop` call.
pub(crate) fn native_demosaic(
    mosaic: &[f32],
    shape: &DemosaicShape,
    planes: &mut [f32],
    executor: Option<(native_tiles::TileExecutor, *mut c_void)>,
    cancel: CancelCallback,
    cancel_context: *mut c_void,
) -> Result<(), RawError> {
    let n = mosaic.len();
    if Some(planes.len()) != n.checked_mul(3) {
        return Err(RawError::InvalidInput("RGB planes differ from the mosaic"));
    }
    let (red, rest) = planes.split_at_mut(n);
    let (green, blue) = rest.split_at_mut(n);
    let (executor, executor_context) = executor
        .map_or((None, std::ptr::null_mut()), |(run, context)| {
            (Some(run), context)
        });
    let mut error = [0 as c_char; 256];
    // SAFETY: the immutable mosaic and shape and the three disjoint planes, of `n` values each,
    // stay alive for the synchronous call. The executor and cancel contexts are the caller's live
    // state for that call. C++ validates the count, catches exceptions, joins every tile job before
    // it returns and stores none of these pointers.
    let code = unsafe {
        lf_raw_develop(
            mosaic.as_ptr(),
            n,
            shape,
            red.as_mut_ptr(),
            green.as_mut_ptr(),
            blue.as_mut_ptr(),
            executor,
            executor_context,
            cancel,
            cancel_context,
            error.as_mut_ptr(),
            error.len(),
        )
    };
    native_result(code, &error)
}

/// How many jobs a development's Rust passes run at once: the shared pool's width, or a nonzero
/// exactness-test override no wider than the pool, for a photo-sized frame on the executor; one
/// lane, in order on the caller, otherwise. The native jobs' eight-lane cap bounds their scratch,
/// which these passes do not hold.
pub(crate) fn development_lanes(pixels: usize, worker_limit: usize, use_executor: bool) -> usize {
    if !use_executor || (pixels as u64) < PARALLEL_PIXELS {
        return 1;
    }
    let width = rayon::current_num_threads();
    match worker_limit {
        0 => width,
        limit => limit.min(width),
    }
    .max(1)
}
