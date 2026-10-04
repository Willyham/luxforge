//! The development of a retained mosaic: the per-site normalization, the one native demosaic call
//! and the output scale, through [`develop_with`]. The demosaic reads each site either from the
//! retained u16 mosaic through the per-site tables, with no float mosaic, or, when the sensor
//! stage rewrites the normalized values, from a float mosaic the Rust normalization writes
//! ([`DemosaicInput`]). Its items are crate-private.

use crate::{
    CancelCallback, MAX_GAIN, MAX_RGB_BYTES, NativeMetadata, PARALLEL_PIXELS, PlanarRgb, RawError,
    RawSource, cancelled, native_result, native_tiles, normalize, zeroed::zeroed_vec,
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
    /// Where to record the development's input, its bytes and the normalization's and the
    /// demosaic's wall time, for tests and the ignored release profiles.
    pub diagnostics: Option<&'a mut DevelopDiagnostics>,
    /// Normalize into a float mosaic even when the sensor sites would do: the reference the
    /// sensor-site input's exactness tests compare with.
    #[cfg(test)]
    pub float_mosaic: bool,
}

impl Default for DevelopOptions<'_> {
    fn default() -> Self {
        Self {
            worker_limit: 0,
            executor: true,
            diagnostics: None,
            #[cfg(test)]
            float_mosaic: false,
        }
    }
}

/// What the native demosaic of one development reads, chosen once per development.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DemosaicInput {
    /// The retained u16 mosaic, each site read through its black level, white scale and gain
    /// ([`normalize::Sites`]) in the same `f32` operations the float mosaic is written with. No
    /// float mosaic is allocated.
    SensorSites,
    /// The normalized float mosaic, for a development whose sensor stage rewrites the
    /// normalized values: a DNG sensor-stage correction (a stage-one radial vignette or stage-two
    /// gain maps) or sparse sensor repairs.
    FloatMosaic,
}

impl DemosaicInput {
    /// The input `raw`'s developments take.
    pub(crate) fn of(raw: &RawSource) -> Self {
        let rewrites_sensor = !raw.mosaic_corrections.is_empty()
            || raw
                .dng_correction
                .as_ref()
                .is_some_and(|correction| correction.changes_normalization());
        if rewrites_sensor {
            Self::FloatMosaic
        } else {
            Self::SensorSites
        }
    }
}

/// What one development read and held, and the wall time of its normalization and its native
/// demosaic call, for tests and the ignored release profiles.
#[derive(Debug, Default)]
pub(crate) struct DevelopDiagnostics {
    /// `None` until a development of a sensor mosaic records its input.
    pub input: Option<DemosaicInput>,
    /// The float mosaic's bytes, zero when none was allocated.
    pub float_mosaic_bytes: usize,
    /// The bytes of the development's own allocations held through its native demosaic: the
    /// float mosaic or the per-site tables, and the RGB planes. The retained u16 mosaic is the
    /// source's, held either way, and is not counted, nor are the native row tables and tile
    /// scratch.
    pub demosaic_bytes: usize,
    /// Building the per-site tables and, for a float mosaic, writing it and any sensor-stage
    /// correction.
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

/// The sensor-site input of one native demosaic: the retained mosaic and the three per-site
/// tables of one period ([`normalize::Sites`]), `period_width * period_height` values each. The
/// C++ adapter declares the same layout as `LfSensorSites`.
#[repr(C)]
struct SensorSites {
    samples: *const u16,
    black: *const f32,
    scale: *const f32,
    gain: *const f32,
    period_width: u32,
    period_height: u32,
}

// The adapter asserts the same size, so a field changed on one side fails the build.
const _: () =
    assert!(std::mem::size_of::<SensorSites>() == 4 * std::mem::size_of::<*const u8>() + 8);

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
    fn lf_raw_develop_sites(
        sites: *const SensorSites,
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

/// A development's prepared demosaic input.
enum Prepared {
    Sites(normalize::Sites),
    Float(Vec<f32>),
}

/// Normalize `raw`'s retained mosaic with `gains`, demosaic it natively and divide the planes by
/// the sensor scale, as `options` say. The demosaic reads the input [`DemosaicInput::of`] chooses
/// for `raw`. The DNG corrections after the demosaic are the caller's.
pub(crate) fn develop_with(
    raw: &RawSource,
    gains: [f32; 3],
    cancel: &AtomicBool,
    options: DevelopOptions<'_>,
) -> Result<PlanarRgb, RawError> {
    #[cfg(test)]
    let force_float_mosaic = options.float_mosaic;
    #[cfg(not(test))]
    let force_float_mosaic = false;
    let DevelopOptions {
        worker_limit,
        executor,
        mut diagnostics,
        ..
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
    if raw.metadata.layout != crate::RawLayout::Mosaic {
        return direct_develop(raw, gains, cancel, worker_limit, executor);
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
    let input = if force_float_mosaic {
        DemosaicInput::FloatMosaic
    } else {
        DemosaicInput::of(raw)
    };
    let clock = diagnostics.is_some().then(std::time::Instant::now);
    let prepared = match input {
        DemosaicInput::SensorSites => {
            Prepared::Sites(raw.normalization(gains).sensor_sites(cancel)?)
        }
        DemosaicInput::FloatMosaic => {
            let mut mosaic = raw.normalization(gains).run(lanes, cancel)?;
            if let Some(correction) = &raw.dng_correction {
                correction.apply_sensor(&mut mosaic, raw, gains, cancel, lanes)?;
            }
            Prepared::Float(mosaic)
        }
    };
    if let (Some(diagnostics), Some(clock)) = (diagnostics.as_deref_mut(), clock) {
        diagnostics.normalization_ns = clock.elapsed().as_nanos() as u64;
    }
    let mut data = zeroed_vec::<f32>(n * 3, "RGB plane allocation")?;
    if let Some(diagnostics) = diagnostics.as_deref_mut() {
        let f32_bytes = std::mem::size_of::<f32>();
        let (float_mosaic_bytes, input_bytes) = match &prepared {
            Prepared::Sites(sites) => (0, sites.bytes()),
            Prepared::Float(mosaic) => {
                let bytes = mosaic.capacity() * f32_bytes;
                (bytes, bytes)
            }
        };
        diagnostics.input = Some(input);
        diagnostics.float_mosaic_bytes = float_mosaic_bytes;
        diagnostics.demosaic_bytes = input_bytes + data.capacity() * f32_bytes;
    }
    let mut executor_context = native_tiles::ExecutorContext {
        cancel,
        worker_limit,
    };
    let executor = executor.then_some((
        native_tiles::execute as native_tiles::TileExecutor,
        (&mut executor_context as *mut native_tiles::ExecutorContext<'_>).cast(),
    ));
    let cancel_context = (cancel as *const AtomicBool).cast_mut().cast();
    let clock = diagnostics.is_some().then(std::time::Instant::now);
    match &prepared {
        Prepared::Sites(sites) => native_demosaic_sites(
            &raw.mosaic,
            sites,
            &raw.shape,
            &mut data,
            executor,
            cancelled,
            cancel_context,
        )?,
        Prepared::Float(mosaic) => native_demosaic(
            mosaic,
            &raw.shape,
            &mut data,
            executor,
            cancelled,
            cancel_context,
        )?,
    }
    if let (Some(diagnostics), Some(clock)) = (diagnostics, clock) {
        diagnostics.demosaic_ns = clock.elapsed().as_nanos() as u64;
    }
    drop(prepared);
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

/// The native demosaic of the retained mosaic `samples` of `shape`, each site read as its
/// normalized value through `sites` (black level, white scale and gain), with no float mosaic.
/// Otherwise as [`native_demosaic`], whose planes it gives for the mosaic
/// [`normalize::Normalization::run`] writes with the same tables. This is the crate's one
/// `lf_raw_develop_sites` call.
pub(crate) fn native_demosaic_sites(
    samples: &[u16],
    sites: &normalize::Sites,
    shape: &DemosaicShape,
    planes: &mut [f32],
    executor: Option<(native_tiles::TileExecutor, *mut c_void)>,
    cancel: CancelCallback,
    cancel_context: *mut c_void,
) -> Result<(), RawError> {
    let n = samples.len();
    if Some(planes.len()) != n.checked_mul(3) {
        return Err(RawError::InvalidInput("RGB planes differ from the mosaic"));
    }
    let period = sites.width.checked_mul(sites.height);
    let (Ok(period_width), Ok(period_height)) =
        (u32::try_from(sites.width), u32::try_from(sites.height))
    else {
        return Err(RawError::InvalidInput(
            "per-site tables differ from their period",
        ));
    };
    if period.is_none_or(|period| {
        period == 0
            || [&sites.black, &sites.scale, &sites.gain]
                .iter()
                .any(|table| table.len() != period)
    }) {
        return Err(RawError::InvalidInput(
            "per-site tables differ from their period",
        ));
    }
    let input = SensorSites {
        samples: samples.as_ptr(),
        black: sites.black.as_ptr(),
        scale: sites.scale.as_ptr(),
        gain: sites.gain.as_ptr(),
        period_width,
        period_height,
    };
    let (red, rest) = planes.split_at_mut(n);
    let (green, blue) = rest.split_at_mut(n);
    let (executor, executor_context) = executor
        .map_or((None, std::ptr::null_mut()), |(run, context)| {
            (Some(run), context)
        });
    let mut error = [0 as c_char; 256];
    // SAFETY: the immutable samples, the three tables of `period_width * period_height` values
    // and the shape, and the three disjoint planes of `n` values each, stay alive for the
    // synchronous call. The executor and cancel contexts are the caller's live state for that
    // call. C++ validates the count and the period against the CFA, reads each table only at
    // `(row % period_height) * period_width + col % period_width`, catches exceptions, joins every
    // tile job before it returns and stores none of these pointers.
    let code = unsafe {
        lf_raw_develop_sites(
            &input,
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

/// Develop integer monochrome or already demosaiced camera RGB without a float mosaic.
fn direct_develop(
    raw: &RawSource,
    gains: [f32; 3],
    cancel: &AtomicBool,
    worker_limit: usize,
    executor: bool,
) -> Result<PlanarRgb, RawError> {
    let m = &raw.metadata;
    if m.layout == crate::RawLayout::Monochrome && gains != [1.0; 3] {
        return Err(RawError::InvalidInput(
            "white balance is unavailable for monochrome originals",
        ));
    }
    let n = RawSource::checked_len(m.sensor_width, m.sensor_height)?;
    let channels = m.layout.channels();
    if raw.mosaic.len() != n * channels || n * 12 > MAX_RGB_BYTES {
        return Err(RawError::ResourceLimit("direct RAW layout or RGB planes"));
    }
    let black = normalize::BlackLevels::of(m);
    let mut data = zeroed_vec::<f32>(n * 3, "RGB plane allocation")?;
    let lanes = development_lanes(n, worker_limit, executor);
    for channel in 0..3 {
        let jobs = data[channel * n..(channel + 1) * n]
            .chunks_mut(m.sensor_width as usize * 16)
            .enumerate();
        native_tiles::refill_each(lanes, jobs, |(job, rows)| {
            for (yy, row) in rows.chunks_exact_mut(m.sensor_width as usize).enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    return Err(RawError::Cancelled);
                }
                let y = job * 16 + yy;
                for (x, value) in row.iter_mut().enumerate() {
                    let c = if channels == 1 { 0 } else { channel };
                    let b = black.at_channel(x, y, c);
                    *value = (raw.mosaic[(y * m.sensor_width as usize + x) * channels + c] as f32
                        - b)
                        / (m.sensor_white - b)
                        * gains[channel];
                }
            }
            Ok(())
        })?;
    }
    Ok(PlanarRgb {
        width: m.sensor_width,
        height: m.sensor_height,
        data,
    })
}
