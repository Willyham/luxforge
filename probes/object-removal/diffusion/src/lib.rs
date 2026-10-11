//! The harness's generative runtime: stable-diffusion.cpp and ggml, built by `build.rs` from the
//! pinned sources in `vendor/` with the `cc` crate, behind the C ABI in `native/shim.cpp`.
//!
//! One [`Context`] holds one loaded model (a diffusion model, its VAE and optionally a text
//! encoder) and generates one image at a time. A text conditioning the encoder computes can be
//! saved to a file ([`save_condition`]) and supplied later ([`supply_condition`]), so a run with
//! a fixed prompt needs no text encoder.

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
    fn lf_sd_open(options: *const OpenOptionsC, error: *mut c_char, error_len: usize) -> *mut c_void;
    fn lf_sd_close(ctx: *mut c_void);
    fn lf_sd_cancel(ctx: *mut c_void);
    fn lf_sd_model_version(ctx: *const c_void) -> *const c_char;
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
    fn lf_sd_condition_supply(
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

/// Upstream's log levels: 0 debug, 1 verbose, 2 info, 3 warn, 4 error. Messages at or above the
/// level go to standard error.
pub fn set_log_level(level: i32) {
    // SAFETY: stores an integer and installs the shim's own callbacks once.
    unsafe { lf_sd_set_log_level(level) }
}

/// The ggml backends and CPU features compiled in.
pub fn system_info() -> String {
    // SAFETY: upstream returns a pointer to a static, NUL-terminated buffer.
    unsafe { CStr::from_ptr(lf_sd_system_info()) }
        .to_string_lossy()
        .into_owned()
}

type ProgressFn = Box<dyn FnMut(i32, i32, f32) + Send>;
static PROGRESS: Mutex<Option<ProgressFn>> = Mutex::new(None);

extern "C" fn progress_trampoline(step: i32, steps: i32, seconds: f32, _user: *mut c_void) {
    if let Ok(mut guard) = PROGRESS.lock()
        && let Some(callback) = guard.as_mut()
    {
        callback(step, steps, seconds);
    }
}

/// Called after each sampling step with the step, the step count and the step's seconds. The
/// callback is process-wide, as upstream's is.
pub fn set_progress(callback: Option<ProgressFn>) {
    let install = callback.is_some();
    *PROGRESS.lock().unwrap() = callback;
    // SAFETY: the trampoline is a plain function that only reads the static above.
    unsafe {
        lf_sd_set_progress(
            if install {
                Some(progress_trampoline)
            } else {
                None
            },
            std::ptr::null_mut(),
        )
    }
}

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

// SAFETY: upstream's context is used from one thread at a time through `&mut self`, and only
// `cancel` (which upstream documents as callable during a generation) takes `&self`.
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
        unsafe { CStr::from_ptr(lf_sd_model_version(self.0.as_ptr())) }
            .to_string_lossy()
            .into_owned()
    }

    /// Ask a running generation to stop at the next step.
    pub fn cancel(&self) {
        // SAFETY: the context is live; upstream sets an atomic flag.
        unsafe { lf_sd_cancel(self.0.as_ptr()) }
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
            ref_rgb: request.reference.map_or(std::ptr::null(), |(rgb, _, _)| rgb.as_ptr()),
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

/// Write the conditioning the text encoder computed for `text` in this process to `path`.
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
    if status == 0 { Ok(()) } else { Err(message(&error)) }
}

/// Use the conditioning in `path` whenever `text` is the prompt, without the text encoder, or
/// with `None` stop doing so.
pub fn supply_condition(text: &str, path: Option<&Path>) -> Result<(), String> {
    let text = c_string(text)?;
    let path = path.map(c_path).transpose()?;
    let mut error = vec![0u8; ERROR_LEN];
    // SAFETY: both strings outlive the call; the error buffer is ERROR_LEN bytes.
    let status = unsafe {
        lf_sd_condition_supply(
            text.as_ptr(),
            path.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            error.as_mut_ptr().cast(),
            ERROR_LEN,
        )
    };
    if status == 0 { Ok(()) } else { Err(message(&error)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_links_and_reports_its_backends() {
        let info = system_info();
        assert!(!info.is_empty());
    }

    #[test]
    fn a_missing_conditioning_file_is_refused_by_name() {
        let error = supply_condition("x", Some(Path::new("/nonexistent/condition.bin"))).unwrap_err();
        assert!(error.contains("/nonexistent/condition.bin"), "{error}");
    }

    #[test]
    fn a_text_never_encoded_has_no_conditioning_to_save() {
        let dir = std::env::temp_dir().join("luxforge-diffusion-test-save");
        let error = save_condition("never encoded", &dir).unwrap_err();
        assert!(error.contains("no conditioning"), "{error}");
    }
}
