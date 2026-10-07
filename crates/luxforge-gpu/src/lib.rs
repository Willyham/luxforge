//! Shared wgpu execution on a caller-owned device: compilation, bounded allocation, source
//! uploads, chains, staged/light sweeps, counts and retirement. Hosts own recipe lowering,
//! source/worker policy and presentation. Qualification helpers are test-only features.
use std::sync::{Arc, Mutex, MutexGuard, atomic::AtomicU64, mpsc::Sender};

pub mod adapters;
pub mod execution;
pub mod retirement;
pub use execution::*;

pub(crate) fn no_wake() {}
pub const UNIFORM_SIZE: usize = luxforge_gpu_types::layout::PLACEMENT_BYTES;

/// Device output with a placement buffer; only the presentation adapter binds/writes that buffer.
pub struct OutputTile {
    pub texture: wgpu::Texture,
    pub uniform: wgpu::Buffer,
}
pub struct Output {
    pub tiles: Vec<OutputTile>,
    pub width: u32,
    pub height: u32,
    pub capacity: (u32, u32),
    pub version: u64,
    pub region_key: Option<GpuRegion>,
}

#[derive(Default)]
pub struct Diagnostics {
    pub gpu_source: Option<SourceFigures>,
    pub gpu_source_refused: Option<(u64, GpuFallback)>,
    pub gpu_retirement_failures: u64,
}

/// Shared device facts and counters. Photo admission remains with the presentation caller.
pub struct Signals {
    pub preview: Arc<Figures>,
    pub retirement_pending: AtomicU64,
    diagnostics: Mutex<Diagnostics>,
    pub wake_callback: fn(),
}
impl Default for Signals {
    fn default() -> Self {
        Self::new(no_wake)
    }
}
impl Signals {
    pub fn new(wake_callback: fn()) -> Self {
        let preview = Arc::new(Figures::default());
        preview.set_wake(wake_callback);
        Self {
            preview,
            retirement_pending: AtomicU64::new(0),
            diagnostics: Mutex::default(),
            wake_callback,
        }
    }
    pub fn diagnostics(&self) -> MutexGuard<'_, Diagnostics> {
        self.diagnostics
            .lock()
            .expect("GPU device diagnostics lock")
    }
    pub fn wake(&self) {
        (self.wake_callback)();
    }
}

/// The execution resources of one surface, independent of IDs, placement, dissolve and redraw.
#[derive(Default)]
pub struct State {
    pub gpu: Option<GpuSlot>,
    pub gpu_lights: light::Lights,
    pub rest: Option<Box<RestSlot>>,
    pub rest_refused: Option<(u64, GpuFallback)>,
    pub histogram: Option<histogram::HistogramReduction>,
    pub tick_histogram: Option<histogram::HistogramReduction>,
    pub tick_counted: Option<(u64, u64)>,
    pub tick_counts: Option<TickCounted>,
    pub evaluation: EvaluationFigures,
}
impl State {
    pub fn rest_figures(&self) -> Option<RestFigures> {
        match (&self.rest, self.rest_refused) {
            (Some(rest), _) => Some(rest.figures()),
            (None, Some((version, fallback))) => Some(RestFigures {
                version,
                fallback: Some(fallback),
                ..Default::default()
            }),
            (None, None) => None,
        }
    }
    pub fn rest_output(&self) -> Option<&Output> {
        self.rest.as_ref()?.output()
    }
}

/// A neutral retained-resource completion supplied by a caller with an independent charge.
pub enum Retired {
    Preview(RetiredPreview),
    External(Box<dyn FnOnce(bool) + Send>),
}
impl Retired {
    pub fn complete_external(self, failed: bool) {
        if let Self::External(finish) = self {
            finish(failed);
        }
    }
}

/// The existing renderer on the caller's device and queue. No device is opened here.
pub struct Executor {
    pub figures: Arc<Signals>,
    pub retirement_sender: Sender<Retired>,
    pub gpu: GpuStage,
    pub kept_lights: light::LightCache,
}
impl Executor {
    pub fn with_figures(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        figures: Arc<Signals>,
    ) -> Self {
        Self::with_stage(device, queue, format, figures, false)
    }
    pub fn with_stage(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        figures: Arc<Signals>,
        refused: bool,
    ) -> Self {
        let finish_figures = Arc::clone(&figures);
        let retirement_sender = retirement::start(
            device,
            queue,
            Arc::new(move |retired, failed| match retired {
                Retired::Preview(retired) => finish_retirement(&finish_figures, retired, failed),
                Retired::External(finish) => finish(failed),
            }),
        );
        let gpu = GpuStage::new(device, format, &figures.preview, refused);
        Self {
            figures,
            retirement_sender,
            gpu,
            kept_lights: light::LightCache::default(),
        }
    }
    pub fn new_surface(&self) -> State {
        State::default()
    }
}

/// The physical output reservation buckets, shared with photo presentation; admission/charges
/// for photographs stay in the UI. These preserve the existing 512 MiB / 32 MiB limits.
pub const FULL_OUTPUT_BUCKET_BUDGET: u64 = 512 * 1024 * 1024;
pub const REGION_OUTPUT_BUCKET_BUDGET: u64 = 32 * 1024 * 1024;
fn full_capacity(size: (u32, u32), limit: u32) -> (u32, u32) {
    luxforge_gpu_types::layout::full_capacity(size, limit, FULL_OUTPUT_BUCKET_BUDGET)
}
fn exact_region_capacity(size: (u32, u32), limit: u32) -> (u32, u32) {
    luxforge_gpu_types::layout::region_capacity(size, limit, REGION_OUTPUT_BUCKET_BUDGET)
}
/// The evaluated frame whose histogram may be counted; presentation supplies its identity.
pub struct CountedFrame<'a> {
    pub plan: &'a GpuPlan,
    pub boundary: u64,
    pub tag: u64,
}
