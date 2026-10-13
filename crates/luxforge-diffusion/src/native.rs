//! The binding over the shim's C ABI (`native/shim.cpp`): stable-diffusion.cpp and ggml, built by
//! `native/build.rs` from the pinned sources in `vendor/` with the `cc` crate. Nothing here knows
//! the core; the object removal probe compiles this same file into its own crate.
//!
//! One [`Context`] holds one loaded model (a diffusion model, its VAE and optionally a text
//! encoder) and generates one image at a time. A text conditioning the encoder computes can be
//! saved to a file ([`save_condition`]) and supplied to a context later
//! ([`Context::supply_condition`]), so a context with a fixed prompt needs no text encoder.
#![allow(unsafe_code)]

use std::{
    ffi::{CStr, CString, c_char, c_void},
    path::Path,
    ptr::NonNull,
    sync::Mutex,
};

#[repr(C)]
struct OpenOptionsC {
    diffusion_model: *const c_char,
    vae: *const c_char,
    llm: *const c_char,
    tokenizer: *const c_char,
    n_threads: i32,
    flash_attn: i32,
    params_backend: *const c_char,
}

#[repr(C)]
struct RequestC {
    init_rgb: *const u8,
    mask: *const u8,
    width: u32,
    height: u32,
    ref_rgb: *const u8,
    ref_width: u32,
    ref_height: u32,
    prompt: *const c_char,
    lora_path: *const c_char,
    lora_multiplier: f32,
    steps: i32,
    cfg_scale: f32,
    strength: f32,
    seed: i64,
    sample_method: *const c_char,
}

type ProgressC = extern "C" fn(step: i32, steps: i32, seconds: f32, user: *mut c_void);

unsafe extern "C" {
    fn lf_sd_set_log_level(level: i32);
    fn lf_sd_set_progress(cb: Option<ProgressC>, user: *mut c_void);
    fn lf_sd_system_info() -> *const c_char;
    fn lf_sd_warm(error: *mut c_char, error_len: usize) -> i32;
    fn lf_sd_open(
        options: *const OpenOptionsC,
        error: *mut c_char,
        error_len: usize,
    ) -> *mut c_void;
    fn lf_sd_close(ctx: *mut c_void);
    fn lf_sd_cancel(ctx: *mut c_void);
    fn lf_sd_model_version(ctx: *const c_void) -> *const c_char;
    fn lf_sd_default_sampler(ctx: *const c_void) -> *const c_char;
    fn lf_sd_default_scheduler(ctx: *const c_void) -> *const c_char;
    fn lf_sd_generate(
        ctx: *mut c_void,
        request: *const RequestC,
        out_rgb: *mut u8,
        error: *mut c_char,
        error_len: usize,
    ) -> i32;
    fn lf_sd_condition_save(
        text: *const c_char,
        path: *const c_char,
        error: *mut c_char,
        error_len: usize,
    ) -> i32;
    fn lf_sd_condition_validate(path: *const c_char, error: *mut c_char, error_len: usize) -> i32;
    fn lf_sd_condition_supply(
        ctx: *mut c_void,
        text: *const c_char,
        path: *const c_char,
        error: *mut c_char,
        error_len: usize,
    ) -> i32;
}

const ERROR_LEN: usize = 2048;

fn c_string(text: &str) -> Result<CString, String> {
    CString::new(text).map_err(|_| format!("{text:?} holds a NUL byte"))
}

fn c_path(path: &Path) -> Result<CString, String> {
    c_string(
        path.to_str()
            .ok_or_else(|| format!("{} is not UTF-8", path.display()))?,
    )
}

fn message(buffer: &[u8]) -> String {
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

/// A static, NUL-terminated string upstream returns, as an owned string.
///
/// # Safety
/// `text` is null or points to a NUL-terminated string that lives for the call.
unsafe fn owned(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: the caller's contract.
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

/// Upstream's log levels: 0 debug, 1 verbose, 2 info, 3 warn, 4 error. Messages at or above the
/// level go to standard error.
pub fn set_log_level(level: i32) {
    // SAFETY: stores an integer and installs the shim's own callbacks once.
    unsafe { lf_sd_set_log_level(level) }
}

/// The ggml backends and CPU features compiled in.
pub fn system_info() -> String {
    // SAFETY: upstream returns a pointer to a static, NUL-terminated buffer.
    unsafe { owned(lf_sd_system_info()) }
}

/// Compile the Metal kernels on this machine by opening and closing the Metal backend, so a
/// context's first generation does not: the first time on a machine takes seconds, and the system
/// caches the result. Does nothing on a CPU build.
pub fn warm() -> Result<(), String> {
    let mut error = vec![0u8; ERROR_LEN];
    // SAFETY: the error buffer is ERROR_LEN bytes.
    let status = unsafe { lf_sd_warm(error.as_mut_ptr().cast(), ERROR_LEN) };
    if status == 0 {
        Ok(())
    } else {
        Err(message(&error))
    }
}

/// A progress callback: the finished step, the step count and the step's seconds.
pub type ProgressFn = Box<dyn FnMut(i32, i32, f32) + Send>;
static PROGRESS: Mutex<Option<ProgressFn>> = Mutex::new(None);

/// The callback of the [`Context::generate_watched`] running on this thread, its lifetime erased
/// for the call's duration only.
type LocalProgress = *mut (dyn FnMut(i32, i32, f32) + 'static);
thread_local! {
    static LOCAL_PROGRESS: std::cell::Cell<Option<LocalProgress>> =
        const { std::cell::Cell::new(None) };
}

extern "C" fn progress_trampoline(step: i32, steps: i32, seconds: f32, _user: *mut c_void) {
    if let Some(local) = LOCAL_PROGRESS.with(std::cell::Cell::get) {
        // SAFETY: set only by `generate_watched` on this thread, to a callback that outlives its
        // call into the runtime, and cleared before that call returns. Upstream calls this from
        // the generating thread, inside that call.
        unsafe { (*local)(step, steps, seconds) };
        return;
    }
    if let Ok(mut guard) = PROGRESS.lock()
        && let Some(callback) = guard.as_mut()
    {
        callback(step, steps, seconds);
    }
}

fn install_trampoline() {
    // SAFETY: the trampoline is a plain function that reads the statics above.
    unsafe { lf_sd_set_progress(Some(progress_trampoline), std::ptr::null_mut()) }
}

/// Called after each sampling step with the step, the step count and the step's seconds, and with
/// step 0 as sampling starts, for every generation that is not [`Context::generate_watched`]. The
/// callback is process-wide, as upstream's is, so a caller that generates on several threads
/// serializes its generations.
pub fn set_progress(callback: Option<ProgressFn>) {
    *PROGRESS.lock().unwrap_or_else(|poison| poison.into_inner()) = callback;
    install_trampoline();
}

/// How often a watched generation looks at its cancellation.
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// What a context loads.
#[derive(Clone, Debug)]
pub struct OpenOptions<'a> {
    pub diffusion_model: &'a Path,
    pub vae: &'a Path,
    /// The text encoder; without it, every prompt needs a supplied conditioning.
    pub llm: Option<&'a Path>,
    /// The text encoder's tokenizer.json.
    pub tokenizer: Option<&'a Path>,
    /// 0 for the physical core count.
    pub threads: i32,
    pub flash_attn: bool,
}

/// One generation.
#[derive(Clone, Debug)]
pub struct Request<'a> {
    /// RGB, `width * height * 3`.
    pub init: &'a [u8],
    /// `width * height`, 255 where the model generates and 0 where the image is kept.
    pub mask: Option<&'a [u8]>,
    pub width: u32,
    pub height: u32,
    /// A reference image (RGB) given to an editing model as context.
    pub reference: Option<(&'a [u8], u32, u32)>,
    pub prompt: &'a str,
    pub lora: Option<(&'a Path, f32)>,
    pub steps: i32,
    pub cfg_scale: f32,
    /// 1.0 regenerates the masked area from noise.
    pub strength: f32,
    pub seed: i64,
    pub sample_method: Option<&'a str>,
}

/// A loaded model.
pub struct Context(NonNull<c_void>);

// SAFETY: upstream's context is used from one thread at a time through `&mut self`; only `cancel`
// (an atomic store upstream documents as callable during a generation) and the name lookups take
// `&self`.
unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl Context {
    pub fn open(options: &OpenOptions) -> Result<Self, String> {
        let diffusion = c_path(options.diffusion_model)?;
        let vae = c_path(options.vae)?;
        let llm = options.llm.map(c_path).transpose()?;
        let tokenizer = options.tokenizer.map(c_path).transpose()?;
        let raw = OpenOptionsC {
            diffusion_model: diffusion.as_ptr(),
            vae: vae.as_ptr(),
            llm: llm.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            tokenizer: tokenizer.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            n_threads: options.threads,
            flash_attn: i32::from(options.flash_attn),
            params_backend: std::ptr::null(),
        };
        let mut error = vec![0u8; ERROR_LEN];
        // SAFETY: every pointer is to a live NUL-terminated string or null, the error buffer is
        // ERROR_LEN bytes, and the shim copies what it keeps.
        let ctx = unsafe { lf_sd_open(&raw, error.as_mut_ptr().cast(), ERROR_LEN) };
        NonNull::new(ctx)
            .map(Context)
            .ok_or_else(|| format!("stable-diffusion.cpp failed to load: {}", message(&error)))
    }

    /// Upstream's name for the model it detected, such as "Flux.2 klein".
    pub fn model_version(&self) -> String {
        // SAFETY: the context is live; upstream returns a static string.
        unsafe { owned(lf_sd_model_version(self.0.as_ptr())) }
    }

    /// The sampler this context denoises with by default, as upstream names it.
    pub fn default_sampler(&self) -> String {
        // SAFETY: the context is live; upstream returns a static string.
        unsafe { owned(lf_sd_default_sampler(self.0.as_ptr())) }
    }

    /// The scheduler this context's default sampler steps through, as upstream names it.
    pub fn default_scheduler(&self) -> String {
        // SAFETY: the context is live; upstream returns a static string.
        unsafe { owned(lf_sd_default_scheduler(self.0.as_ptr())) }
    }

    /// Ask a running generation to stop at its next step or stage. A request made before a
    /// generation starts is cleared by it.
    pub fn cancel(&self) {
        // SAFETY: the context is live; upstream sets an atomic flag.
        unsafe { lf_sd_cancel(self.0.as_ptr()) }
    }

    /// Use the conditioning in `path` whenever `text` is this context's prompt, without its text
    /// encoder, or with `None` stop doing so.
    pub fn supply_condition(&mut self, text: &str, path: Option<&Path>) -> Result<(), String> {
        let text = c_string(text)?;
        let path = path.map(c_path).transpose()?;
        let mut error = vec![0u8; ERROR_LEN];
        // SAFETY: the context is live and not generating (`&mut self`); both strings outlive the
        // call; the error buffer is ERROR_LEN bytes.
        let status = unsafe {
            lf_sd_condition_supply(
                self.0.as_ptr(),
                text.as_ptr(),
                path.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                error.as_mut_ptr().cast(),
                ERROR_LEN,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(message(&error))
        }
    }

    /// [`Self::generate`], calling `progress` on this thread for each of upstream's progress
    /// reports and asking the runtime to stop as soon as `cancelled` answers true: a watcher
    /// thread, alive only for the call, polls it and sets upstream's cancellation flag, which the
    /// runtime checks at every sampling step and between its stages.
    pub fn generate_watched(
        &mut self,
        request: &Request,
        progress: &mut dyn FnMut(i32, i32, f32),
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Vec<u8>, String> {
        install_trampoline();
        // SAFETY: only the lifetime is erased; the pointer is cleared below before `progress`'s
        // borrow ends, and only this thread reads it.
        let local: LocalProgress = unsafe {
            std::mem::transmute::<*mut (dyn FnMut(i32, i32, f32) + '_), LocalProgress>(
                progress as *mut _,
            )
        };
        LOCAL_PROGRESS.with(|cell| cell.set(Some(local)));
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                LOCAL_PROGRESS.with(|cell| cell.set(None));
            }
        }
        let _clear = Clear;
        let raw = self.0.as_ptr() as usize;
        let (done, finished) = std::sync::mpsc::channel::<()>();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                loop {
                    match finished.recv_timeout(CANCEL_POLL) {
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            if cancelled() {
                                // SAFETY: the context outlives the scope; upstream's cancel is an
                                // atomic store, callable while it generates. Repeated, since a
                                // generation clears a request made before it started.
                                unsafe { lf_sd_cancel(raw as *mut c_void) }
                            }
                        }
                        _ => return,
                    }
                }
            });
            let answer = self.generate(request);
            drop(done);
            answer
        })
    }

    /// Generate one RGB image of `width * height`.
    pub fn generate(&mut self, request: &Request) -> Result<Vec<u8>, String> {
        let pixels = request.width as usize * request.height as usize;
        if request.init.len() != pixels * 3 {
            return Err("the init image is not width × height RGB".into());
        }
        if request.mask.is_some_and(|mask| mask.len() != pixels) {
            return Err("the mask is not width × height".into());
        }
        if let Some((rgb, w, h)) = request.reference
            && rgb.len() != w as usize * h as usize * 3
        {
            return Err("the reference is not its width × height RGB".into());
        }
        let prompt = c_string(request.prompt)?;
        let lora = request.lora.map(|(path, _)| c_path(path)).transpose()?;
        let method = request.sample_method.map(c_string).transpose()?;
        let raw = RequestC {
            init_rgb: request.init.as_ptr(),
            mask: request.mask.map_or(std::ptr::null(), <[u8]>::as_ptr),
            width: request.width,
            height: request.height,
            ref_rgb: request
                .reference
                .map_or(std::ptr::null(), |(rgb, _, _)| rgb.as_ptr()),
            ref_width: request.reference.map_or(0, |(_, w, _)| w),
            ref_height: request.reference.map_or(0, |(_, _, h)| h),
            prompt: prompt.as_ptr(),
            lora_path: lora.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            lora_multiplier: request.lora.map_or(0.0, |(_, m)| m),
            steps: request.steps,
            cfg_scale: request.cfg_scale,
            strength: request.strength,
            seed: request.seed,
            sample_method: method.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
        };
        let mut out = vec![0u8; pixels * 3];
        let mut error = vec![0u8; ERROR_LEN];
        // SAFETY: the buffers' sizes were checked above, the output holds width × height × 3
        // bytes, and every string outlives the call.
        let status = unsafe {
            lf_sd_generate(
                self.0.as_ptr(),
                &raw,
                out.as_mut_ptr(),
                error.as_mut_ptr().cast(),
                ERROR_LEN,
            )
        };
        if status == 0 {
            Ok(out)
        } else {
            Err(format!("generation failed: {}", message(&error)))
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: the context is live and dropped once.
        unsafe { lf_sd_close(self.0.as_ptr()) }
    }
}

/// Write the conditioning a text encoder computed for `text` in this process to `path`.
pub fn save_condition(text: &str, path: &Path) -> Result<(), String> {
    let text = c_string(text)?;
    let path = c_path(path)?;
    let mut error = vec![0u8; ERROR_LEN];
    // SAFETY: both strings outlive the call; the error buffer is ERROR_LEN bytes.
    let status = unsafe {
        lf_sd_condition_save(
            text.as_ptr(),
            path.as_ptr(),
            error.as_mut_ptr().cast(),
            ERROR_LEN,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(message(&error))
    }
}

/// Check a conditioning file's shape and length without loading weights or allocating tensor
/// payloads. The same parser supplies the conditioning to a context later.
pub fn validate_condition(path: &Path) -> Result<(), String> {
    let path = c_path(path)?;
    let mut error = vec![0u8; ERROR_LEN];
    // SAFETY: the path is NUL-terminated and the caller-owned error buffer has ERROR_LEN bytes.
    let status =
        unsafe { lf_sd_condition_validate(path.as_ptr(), error.as_mut_ptr().cast(), ERROR_LEN) };
    if status == 0 {
        Ok(())
    } else {
        Err(message(&error))
    }
}
