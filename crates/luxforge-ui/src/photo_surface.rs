//! The photograph's own surface: a shader primitive that owns its textures and writes the frames it
//! draws into them during the same frame that draws them.
//!
//! Everything the canvas shows of the picture is drawn here, in one primitive: the displayed frame —
//! the photograph, or the crop layer's input stage while a crop draft shows it — and the bounded
//! overlays laid over the photograph, the clipping overlay and a mask's coverage. Each is a
//! [`Frame`], and each has its own texture in the one pipeline. The interaction canvases (the crop
//! frame and its handles, a mask's handles and strokes) stay canvases stacked above.
//!
//! The toolkit's image widget reaches the GPU through an allocation round trip: the desktop hands
//! the runtime a handle, the runtime answers with an allocation on a later turn of the event loop,
//! and only then can the view draw it. That answer costs one runtime hop — about one display frame
//! on the owner's Mac — on every frame and every overlay, which is exactly the hop the
//! instant-preview design removes
//! ([docs/design/instant-preview.md](../../../../docs/design/instant-preview.md), "One frame per
//! hop"). Here a frame is plain data the view borrows: `prepare` writes it into the pipeline's own
//! texture just before `draw` samples that texture, so a frame reaches the screen in the redraw that
//! follows the update that handed it over and nothing waits on the runtime.
//!
//! Bytes are reproduced exactly as the toolkit's image widget reproduces them, because every choice
//! that touches a byte is the toolkit's own:
//!
//! - **Format.** The toolkit stores image pixels in an `Rgba8UnormSrgb` atlas when it gamma
//!   corrects, and it gamma corrects exactly when it picked an sRGB surface format. So every
//!   texture here is `Rgba8UnormSrgb` when the target format is sRGB and `Rgba8Unorm` when it is
//!   not: the hardware decodes a texel to linear on the sample and encodes it back on the write,
//!   which is the identity on an opaque texel drawn one-to-one.
//! - **Filtering.** The photograph and the crop stage are sampled with a linear sampler clamped to
//!   the edge, the image widget's `FilterMethod::Linear` default. The overlays are cell grids and
//!   are sampled with the nearest texel, which is how they were drawn as images.
//! - **Blending.** The image pipeline's own blend state, so an overlay's translucent cells and the
//!   crop stage's dimmed part composite exactly as the image widget composited them; a photograph's
//!   raster is opaque, where that blend is the identity. The image shader applies an opacity by
//!   scaling the sampled alpha, and so does this one.
//! - **Geometry.** The photograph's rectangle is computed with [`iced::ContentFit`] and centred
//!   exactly as `iced_widget::image::drawing_bounds` computes it, then snapped to the physical
//!   pixel grid in the vertex shader with WGSL's own `round`, which is what the image shader does
//!   with `snap: true`. The overlays are drawn into that same snapped rectangle, so they can never
//!   drift from the picture they describe. The crop stage is placed where the crop canvas puts it,
//!   unsnapped as the canvas drew it, and turned about its own centre by the draft angle.
//!
//! Two GPU limits bound what one primitive may do, and both are the device's limits rather than the
//! GPU's: Iced asks wgpu for its default limits, so on the owner's Mac a texture and a render pass
//! viewport may be at most 8192 px a side, and a viewport may start no further than twice that
//! from the frame's origin.
//!
//! - **The viewport.** The renderer sets the render pass's viewport to the rectangle a primitive is
//!   drawn with. At a percentage zoom the widget is the photograph's whole displayed box inside a
//!   scrollable, which for a 24 MP photograph at 800% is 48000 physical pixels wide, so the widget
//!   draws its primitive with the intersection of its bounds and the viewport it is given — never
//!   larger than the window — and the primitive carries the picture's rectangle relative to that
//!   intersection's origin. The scrollable translates the intersection when it draws, the relative
//!   rectangle moves with it, and the rasterizer clips whatever of the picture lies outside it. At
//!   Fit the whole widget is visible and the intersection is its bounds.
//! - **The texture.** A frame wider or taller than the limit — a 60 MP photograph or crop stage is
//!   9504 or 10000 pixels wide — is held in a grid of tiles, each its own texture within the limit
//!   and drawn as its own quad. Every tile's quad is cut from the same destination, and a turned
//!   stage turns every corner about that destination's centre, so the tiles meet without a gap at
//!   any angle; each texture carries one extra pixel on every side that has a neighbour, so the
//!   linear filter reads across a seam exactly as it reads inside one texture. A frame within the
//!   limit — every display proxy, every overlay, and every exact render up to 8192 pixels a side,
//!   about 45 MP at 3:2 — is one tile, which is the single texture it would otherwise be.
//!
//! Memory: one full photograph slot and two region sets per pipeline. They reserve bounded size
//! buckets so a refit, half-to-exact refinement or one-pixel pan change can reuse their textures;
//! only the active rectangle is written and sampled. A full replacement may overlap one retiring
//! full allocation, each capped at 512 MiB; each region set is capped at 32 MiB. The crop stage
//! and overlays have separate textures. The crop stage's is reserved at exactly its frame's size:
//! the display-size proxy a draft shows at Fit, and a full-size exact stage only at a percentage
//! zoom that needs one, never kept for the proxy after it. The pixels are borrowed from desktop
//! frames, and uploads read their rows directly from those buffers.

use iced::{
    ContentFit, Element, Length, Point, Rectangle, Size, Vector,
    advanced::{Layout, Widget, layout, mouse, renderer, widget::Tree},
    widget::shader::{self, Viewport},
};
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock,
    atomic::{AtomicU64, Ordering},
};

/// What one [`PhotoPipeline`] counts of its own texture work, shared with its retirement worker.
#[derive(Default)]
struct SurfaceFigures {
    /// How many photograph frames the pipeline has written into its texture. Diagnostics only: an
    /// evidence run records it beside each captured frame, which is how a run proves that
    /// redrawing an unchanged photograph — at any zoom, however often the view is rebuilt — writes
    /// nothing. The crop stage and the overlays have textures of their own and are not counted.
    writes: AtomicU64,
    upload_bytes: AtomicU64,
    retirement_pending: AtomicU64,
    diagnostics: Mutex<SurfaceDiagnostics>,
}

impl SurfaceFigures {
    fn diagnostics(&self) -> MutexGuard<'_, SurfaceDiagnostics> {
        self.diagnostics.lock().expect("surface diagnostics lock")
    }
}

/// The figures of the pipeline Iced creates for the application, which the free functions below
/// read. They are process-wide because the desktop has no handle on that pipeline: Iced keeps one
/// per renderer in its own storage, shared by every photo surface on screen. A pipeline a test
/// builds counts into figures of its own ([`PhotoPipeline::with_figures`]).
static PROCESS_FIGURES: OnceLock<Arc<SurfaceFigures>> = OnceLock::new();

fn process_figures() -> &'static Arc<SurfaceFigures> {
    PROCESS_FIGURES.get_or_init(Arc::default)
}

type SurfaceWaker = Arc<dyn Fn() + Send + Sync>;

static RETIREMENT_WAKER: OnceLock<Mutex<Option<SurfaceWaker>>> = OnceLock::new();

const FULL_BUDGET: u64 = 512 * 1024 * 1024;
const REGION_SET_BUDGET: u64 = 32 * 1024 * 1024;

/// Register the desktop's existing buffered wake channel for deferred GPU texture admission.
/// The callback may run on any thread; it should only enqueue a wake, never touch UI state.
pub fn set_surface_waker(waker: Arc<dyn Fn() + Send + Sync>) {
    *RETIREMENT_WAKER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("surface waker lock") = Some(waker);
}

/// Whether a redraw must remain subscribed to the surface's GPU retirement wake.
pub fn surface_retirement_pending() -> bool {
    process_figures().retirement_pending.load(Ordering::Acquire) != 0
}

/// Actual RGBA bytes handed to `wgpu::Queue::write_texture` by photograph uploads.
pub fn texture_upload_bytes() -> u64 {
    process_figures().upload_bytes.load(Ordering::Relaxed)
}

/// A snapshot of actual texture work and draw encoding, distinct from desktop frame adoption.
/// Residency includes textures whose GPU submission has not yet retired. Overlay textures and
/// backend-owned upload staging are outside these photograph-slot byte counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawnRegion {
    pub version: u64,
    pub content_id: u64,
    pub generation: u64,
    pub quality: RegionQuality,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceDiagnostics {
    pub photo_writes: u64,
    pub upload_bytes: u64,
    pub full_resident_bytes: u64,
    pub region_resident_bytes: u64,
    /// The crop draft's input-stage texture: a display-size proxy's at Fit, the exact stage's only
    /// at a percentage zoom that needs it, and nothing once the draft ends.
    pub stage_resident_bytes: u64,
    pub retiring_bytes: u64,
    pub deferred_uploads: u64,
    pub rejected_full_uploads: u64,
    pub rejected_region_uploads: u64,
    pub gpu_retirement_failures: u64,
    /// A photo primitive was encoded with no drawable photograph.
    pub blank_photo_draws: u64,
    /// A previous coherent photograph was encoded while the requested one awaits admission.
    pub stale_photo_draws: u64,
    pub drawn_frames: u64,
    pub drawn_photo_blank: bool,
    pub drawn_stale_photo: bool,
    /// Actual previous content when `drawn_stale_photo` is true. `drawn_content` then stays None,
    /// so a consumer cannot mistake a fallback picture for the requested version.
    pub drawn_fallback_content: Option<u64>,
    pub drawn_content: Option<u64>,
    pub drawn_full_version: Option<u64>,
    pub drawn_region_version: Option<u64>,
    pub drawn_region_generation: Option<u64>,
    pub drawn_region_quality: Option<RegionQuality>,
    /// Each region slot actually encoded in the last photo draw, including a region partly
    /// covered by a higher-priority one. The single fields above describe the topmost region.
    pub drawn_regions: [Option<DrawnRegion>; 2],
    /// Clipping frame whose draw call was encoded with the photograph, if any.
    pub drawn_clipping_version: Option<u64>,
}

pub fn surface_diagnostics() -> SurfaceDiagnostics {
    *process_figures().diagnostics()
}

/// Check a region before requesting it from the renderer. This accounts for each tile's linear
/// filtering apron at the device texture limit. An inadmissible region needs the explicit full
/// frame fallback; it cannot ever become ready through a retirement wake.
pub fn region_texture_admissible(size: (u32, u32), texture_limit: u32) -> bool {
    size.0 > 0
        && size.1 > 0
        && texture_limit > 2
        && u64::from(size.0) * u64::from(size.1) * 4 <= REGION_SET_BUDGET
        && allocated_bytes(&tile_layout(size, texture_limit)) <= REGION_SET_BUDGET
}

/// The number of photograph texture writes so far; see [`SurfaceFigures::writes`].
pub fn texture_writes() -> u64 {
    process_figures().writes.load(Ordering::Relaxed)
}

/// How the photograph is placed inside the widget's bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Scaled to fit inside the bounds, keeping its aspect ratio, and centred: the Fit view.
    Contain,
    /// Stretched to the whole bounds: a percentage zoom, where the widget is already sized to the
    /// exact box the picture belongs in and the raster may be a smaller display proxy.
    Fill,
}

impl Placement {
    fn content_fit(self) -> ContentFit {
        match self {
            Placement::Contain => ContentFit::Contain,
            Placement::Fill => ContentFit::Fill,
        }
    }
}

/// Where a crop draft's input stage is drawn: the unrotated stage at `rect`, turned by `angle`
/// about that rectangle's centre, at full opacity inside `bright` and at `dim` elsewhere. Both
/// rectangles are in the widget's own logical coordinates, `bright` is axis-aligned on screen, and
/// the rotation is the crop contract's matrix: in y-down pixels a positive angle turns clockwise,
/// `(x, y) ↦ (x cos θ − y sin θ, x sin θ + y cos θ)` about the centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Turn {
    pub rect: Rectangle,
    /// Radians.
    pub angle: f32,
    pub bright: Rectangle,
    pub dim: f32,
}

/// One frame as the surface draws it: an RGBA8 buffer, its size, and a version.
///
/// The photograph, the crop stage and both overlays are all frames. The version is the only thing
/// that decides whether a frame is written to its texture, so it must change whenever the pixels do
/// and never otherwise. The desktop gives each layer a counter it increments each time it hands a
/// frame over; any monotone key with that property works.
///
/// The buffer is whatever already holds the pixels, shared as it is — a render's own
/// `Arc<Vec<u8>>`, or the `Vec` an overlay was painted into — so making a frame copies nothing.
#[derive(Clone)]
pub struct Frame {
    pixels: Arc<dyn AsRef<[u8]> + Send + Sync>,
    width: u32,
    height: u32,
    version: u64,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("version", &self.version)
            .finish()
    }
}

impl Frame {
    /// A frame of `width` × `height` RGBA8 pixels, or `None` when the buffer does not hold exactly
    /// that many bytes. A surface never draws a buffer it cannot account for. The frame holds the
    /// caller's own `Arc`, so a buffer already shared is not wrapped again.
    pub fn new<P: AsRef<[u8]> + Send + Sync + 'static>(
        pixels: Arc<P>,
        width: u32,
        height: u32,
        version: u64,
    ) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))?;
        (width > 0 && height > 0 && (*pixels).as_ref().len() == expected).then(|| Self {
            pixels,
            width,
            height,
            version,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

/// The detail of a region published to the surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionQuality {
    Interactive,
    Exact,
}

/// A viewport raster or overlay in its rendered stage's coordinates. `rect` is end-exclusive;
/// `full_stage` is the exact output size used to place the whole photograph. The stage ratio, not
/// a nominal half-scale factor, maps odd-sized stages without right or bottom edge drift.
#[derive(Clone, Debug)]
pub struct RegionFrame {
    pub frame: Frame,
    pub rect: [u32; 4],
    pub stage: (u32, u32),
    pub full_stage: (u32, u32),
    pub scale: f32,
    pub quality: RegionQuality,
    pub content_id: u64,
    pub generation: u64,
}

/// A viewport overlay grid painted over `rect` in `stage` coordinates. Its frame may be a much
/// smaller OR grid, so its raster dimensions deliberately differ from the rectangle's size.
#[derive(Clone, Debug)]
pub struct RegionOverlay {
    pub frame: Frame,
    /// End-exclusive rectangle in `stage` coordinates.
    pub rect: [u32; 4],
    pub stage: (u32, u32),
    pub full_stage: (u32, u32),
    pub quality: RegionQuality,
    pub content_id: u64,
    pub generation: u64,
}

impl RegionOverlay {
    pub fn new(
        frame: Frame,
        rect: [u32; 4],
        stage: (u32, u32),
        full_stage: (u32, u32),
        quality: RegionQuality,
        content_id: u64,
        generation: u64,
    ) -> Option<Self> {
        RegionKey::valid(rect, stage, full_stage).then_some(Self {
            frame,
            rect,
            stage,
            full_stage,
            quality,
            content_id,
            generation,
        })
    }

    fn key(&self) -> RegionKey {
        RegionKey {
            rect: self.rect,
            stage: self.stage,
            full_stage: self.full_stage,
            quality: self.quality,
            content_id: self.content_id,
            generation: self.generation,
        }
    }

    fn matches_region(&self, key: RegionKey) -> bool {
        self.content_id == key.content_id
            && self.generation == key.generation
            && self.quality == key.quality
            && self.full_stage == key.full_stage
    }
}

impl RegionFrame {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        frame: Frame,
        rect: [u32; 4],
        stage: (u32, u32),
        full_stage: (u32, u32),
        scale: f32,
        quality: RegionQuality,
        content_id: u64,
        generation: u64,
    ) -> Option<Self> {
        (RegionKey::valid(rect, stage, full_stage)
            && frame.size() == (rect[2] - rect[0], rect[3] - rect[1])
            && scale.is_finite()
            && scale > 0.0)
            .then_some(Self {
                frame,
                rect,
                stage,
                full_stage,
                scale,
                quality,
                content_id,
                generation,
            })
    }

    fn key(&self) -> RegionKey {
        RegionKey {
            rect: self.rect,
            stage: self.stage,
            full_stage: self.full_stage,
            quality: self.quality,
            content_id: self.content_id,
            generation: self.generation,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RegionKey {
    rect: [u32; 4],
    stage: (u32, u32),
    full_stage: (u32, u32),
    quality: RegionQuality,
    content_id: u64,
    generation: u64,
}

impl RegionKey {
    fn valid(rect: [u32; 4], stage: (u32, u32), full_stage: (u32, u32)) -> bool {
        rect[0] < rect[2]
            && rect[1] < rect[3]
            && rect[2] <= stage.0
            && rect[3] <= stage.1
            && full_stage.0 > 0
            && full_stage.1 > 0
    }

    fn placement(self, tile: TileLayout, raster: (u32, u32)) -> [f32; 4] {
        let [x0, y0, x1, y1] = tile.content;
        let [rx, ry, right, bottom] = self.rect;
        let width = (right - rx) as f32;
        let height = (bottom - ry) as f32;
        [
            (rx as f32 + x0 as f32 * width / raster.0 as f32) / self.stage.0 as f32,
            (ry as f32 + y0 as f32 * height / raster.1 as f32) / self.stage.1 as f32,
            (rx as f32 + x1 as f32 * width / raster.0 as f32) / self.stage.0 as f32,
            (ry as f32 + y1 as f32 * height / raster.1 as f32) / self.stage.1 as f32,
        ]
    }
}

/// The textures one pipeline holds, one set per layer, in the order a primitive draws them: the
/// displayed frame (the photograph or the crop stage), then the clipping overlay, then the mask
/// coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layer {
    Photo,
    Stage,
    Clipping,
    Coverage,
}

impl Layer {
    fn index(self) -> usize {
        self as usize
    }

    /// The overlays are cell grids, drawn a cell to a block of screen pixels.
    fn nearest(self) -> bool {
        matches!(self, Layer::Clipping | Layer::Coverage)
    }
}

/// How the displayed frame is placed.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Base {
    Photo(Placement),
    Stage(Turn),
}

/// The photograph — or the crop stage — with the overlays over it, drawn by a primitive that owns
/// its textures.
///
/// `width`/`height` are the widget's own sizing rule: `Fill`/`Fill` at Fit, where the photograph is
/// placed `Contain` and centred inside whatever the widget is given, and the exact displayed box at
/// a percentage, where it is placed `Fill` and the widget may be far larger than the window.
pub struct PhotoSurface {
    base: Base,
    /// The displayed frame first, then each overlay present, in draw order.
    layers: Vec<(Layer, Frame)>,
    viewport: Option<ViewportFrames>,
    region_overlays: [Option<RegionOverlay>; 2],
    width: Length,
    height: Length,
}

#[derive(Clone, Debug)]
struct ViewportFrames {
    full: Option<(Frame, u64)>,
    region: Option<RegionFrame>,
    current_content: u64,
    full_stage: (u32, u32),
}

/// The photograph, placed by `placement`.
pub fn photo_surface(
    frame: &Frame,
    placement: Placement,
    width: Length,
    height: Length,
) -> PhotoSurface {
    PhotoSurface {
        base: Base::Photo(placement),
        layers: vec![(Layer::Photo, frame.clone())],
        viewport: None,
        region_overlays: [None, None],
        width,
        height,
    }
}

/// A percentage-zoom photograph with independently retained full and viewport rasters. Matching
/// content is preferred; a coherent previous picture stays visible while admission is deferred.
pub fn viewport_surface(
    full: Option<(&Frame, u64)>,
    region: Option<&RegionFrame>,
    current_content: u64,
    full_stage: (u32, u32),
    placement: Placement,
    width: Length,
    height: Length,
) -> PhotoSurface {
    let region = region
        .filter(|region| region.full_stage == full_stage)
        .cloned();
    PhotoSurface {
        base: Base::Photo(placement),
        layers: Vec::new(),
        viewport: Some(ViewportFrames {
            full: full.map(|(frame, content)| (frame.clone(), content)),
            region,
            current_content,
            full_stage,
        }),
        region_overlays: [None, None],
        width,
        height,
    }
}

/// A crop draft's input stage, turned and dimmed as `turn` says. It has a texture of its own, so the
/// photograph's stays written while the draft is open.
pub fn stage_surface(frame: &Frame, turn: Turn, width: Length, height: Length) -> PhotoSurface {
    PhotoSurface {
        base: Base::Stage(turn),
        layers: vec![(Layer::Stage, frame.clone())],
        viewport: None,
        region_overlays: [None, None],
        width,
        height,
    }
}

impl PhotoSurface {
    /// Lay the clipping overlay and then a mask's coverage over the picture, each stretched over
    /// exactly the rectangle the picture is drawn into. Either may be absent.
    pub fn overlays(mut self, clipping: Option<&Frame>, coverage: Option<&Frame>) -> Self {
        for (layer, frame) in [(Layer::Clipping, clipping), (Layer::Coverage, coverage)] {
            if let Some(frame) = frame {
                self.layers.push((layer, frame.clone()));
            }
        }
        self
    }

    /// A viewport-bounded clipping grid and mask coverage, each tied to the exact region pixels
    /// it describes. A stale identity, quality or generation cannot cover a newer region.
    pub fn region_overlays(
        mut self,
        clipping: Option<&RegionOverlay>,
        coverage: Option<&RegionOverlay>,
    ) -> Self {
        self.region_overlays = [clipping.cloned(), coverage.cloned()];
        self
    }
}

impl<'a, Message: 'a> From<PhotoSurface> for Element<'a, Message> {
    fn from(surface: PhotoSurface) -> Self {
        Element::new(surface)
    }
}

/// Where the toolkit draws a fitted raster inside `bounds`, in the bounds' own coordinate frame:
/// [`ContentFit`] sized and centred, which is `iced_widget::image::drawing_bounds` for an image
/// with no crop, no rotation and unit scale.
fn placement_rect(
    placement: Placement,
    (width, height): (u32, u32),
    bounds: Rectangle,
) -> Option<Rectangle> {
    let content = Size::new(width as f32, height as f32);
    if !(content.width > 0.0
        && content.height > 0.0
        && bounds.width > 0.0
        && bounds.height > 0.0
        && bounds.x.is_finite()
        && bounds.y.is_finite())
    {
        return None;
    }
    let size = placement.content_fit().fit(content, bounds.size());
    (size.width > 0.0 && size.height > 0.0).then(|| {
        Rectangle::new(
            Point::new(
                bounds.center_x() - size.width / 2.0,
                bounds.center_y() - size.height / 2.0,
            ),
            size,
        )
    })
}

/// Where the displayed frame goes inside `bounds`, in the layout's coordinates.
fn destination(base: Base, raster: (u32, u32), bounds: Rectangle) -> Option<Rectangle> {
    match base {
        Base::Photo(placement) => placement_rect(placement, raster, bounds),
        Base::Stage(turn) => {
            let rect = turn.rect;
            (raster.0 > 0
                && raster.1 > 0
                && rect.width > 0.0
                && rect.height > 0.0
                && rect.x.is_finite()
                && rect.y.is_finite()
                && rect.width.is_finite()
                && rect.height.is_finite())
            .then(|| Rectangle::new(bounds.position() + Vector::new(rect.x, rect.y), rect.size()))
        }
    }
}

/// What one frame of the picture needs from the layout: the visible part of the widget, which the
/// primitive is drawn with, and where the picture — and its bright part, for a turned stage — goes
/// relative to that part's origin.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Visible {
    /// The widget's bounds clipped to the viewport it was drawn in, in the layout's coordinates.
    clip: Rectangle,
    /// The picture's top-left corner minus `clip`'s, in logical pixels. It is a difference of two
    /// points in the same frame, so any translation applied to `clip` later leaves it valid.
    offset: Vector,
    /// The picture's size in logical pixels.
    size: Size,
    /// The part drawn at full opacity, relative to `clip`'s origin like `offset`, and the opacity
    /// everywhere else. `None` draws everything at full opacity.
    bright: Option<(Rectangle, f32)>,
}

/// The visible part of a surface laid out at `bounds` and drawn in `viewport`, both in the layout's
/// own coordinates, with the picture placed by `base` inside the whole of `bounds`. `None` when
/// nothing of the widget is visible or there is no picture to place.
fn visible_placement(
    base: Base,
    raster: (u32, u32),
    bounds: Rectangle,
    viewport: Rectangle,
) -> Option<Visible> {
    let destination = destination(base, raster, bounds)?;
    let clip = bounds.intersection(&viewport)?;
    let bright = match base {
        Base::Photo(_) => None,
        Base::Stage(turn) => Some((
            Rectangle::new(
                Point::ORIGIN
                    + (bounds.position() - clip.position())
                    + Vector::new(turn.bright.x, turn.bright.y),
                turn.bright.size(),
            ),
            turn.dim,
        )),
    };
    Some(Visible {
        clip,
        offset: destination.position() - clip.position(),
        size: destination.size(),
        bright,
    })
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for PhotoSurface
where
    Renderer: iced_wgpu::primitive::Renderer,
{
    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, self.width, self.height)
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let raster = match &self.viewport {
            Some(viewport) => viewport.full_stage,
            None => match self.layers.first() {
                Some((_, frame)) => frame.size(),
                None => return,
            },
        };
        // Inside a scrollable, `viewport` is already in the content's own coordinates — the
        // scrollable shifts it by the scroll offset before handing it down — so it and the layout
        // bounds are in one frame and their intersection is the part on screen.
        let Some(visible) = visible_placement(self.base, raster, layout.bounds(), *viewport) else {
            return;
        };
        let (angle, snap) = match self.base {
            // The photograph is snapped as the image widget snaps it.
            Base::Photo(_) => (0.0, true),
            // The stage is placed where the crop canvas drew it, unsnapped, as the canvas's images
            // were drawn.
            Base::Stage(turn) => (turn.angle, false),
        };
        renderer.draw_primitive(
            visible.clip,
            PhotoPrimitive {
                layers: self.layers.clone(),
                viewport: self.viewport.clone(),
                region_overlays: self.region_overlays.clone(),
                offset: visible.offset,
                size: visible.size,
                clip_size: visible.clip.size(),
                bright: visible.bright,
                angle,
                snap,
            },
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        // The photograph is not a control: every pointer event belongs to the mouse area or the
        // canvas around it, which is what the image widget reports too.
        mouse::Interaction::None
    }
}

/// The frames and where they go, as the renderer receives them for one frame. The placement is
/// relative to the rectangle the primitive is drawn with, which the renderer hands to `prepare`
/// after applying whatever translation a scrollable put it under.
#[derive(Debug)]
pub struct PhotoPrimitive {
    layers: Vec<(Layer, Frame)>,
    viewport: Option<ViewportFrames>,
    region_overlays: [Option<RegionOverlay>; 2],
    offset: Vector,
    size: Size,
    clip_size: Size,
    bright: Option<(Rectangle, f32)>,
    angle: f32,
    snap: bool,
}

/// The uniform block's first two rectangles, in physical pixels of the whole frame: the render
/// pass's viewport, which the renderer sets to `bounds` — the visible part of the widget, where it
/// now is on screen — scaled exactly as it scales them, and the picture at its offset from that
/// part's origin. The picture may extend far outside the viewport; the rasterizer clips it.
fn physical_rects(
    bounds: Rectangle,
    offset: Vector,
    size: Size,
    scale: f32,
) -> ([f32; 4], [f32; 4]) {
    (
        [
            bounds.x * scale,
            bounds.y * scale,
            bounds.width * scale,
            bounds.height * scale,
        ],
        [
            (bounds.x + offset.x) * scale,
            (bounds.y + offset.y) * scale,
            size.width * scale,
            size.height * scale,
        ],
    )
}

/// The bright rectangle as `[x0, y0, x1, y1]` physical pixels of the whole frame, each corner
/// rounded as the toolkit rounds a clip rectangle to its scissor (`Rectangle::snap`), and the
/// opacity outside it. With no bright part it covers every pixel, at full opacity.
fn physical_bright(bounds: Rectangle, bright: Option<(Rectangle, f32)>, scale: f32) -> [f32; 5] {
    match bright {
        Some((rect, dim)) => [
            ((bounds.x + rect.x) * scale).round(),
            ((bounds.y + rect.y) * scale).round(),
            ((bounds.x + rect.x + rect.width) * scale).round(),
            ((bounds.y + rect.y + rect.height) * scale).round(),
            dim,
        ],
        None => [f32::MIN, f32::MIN, f32::MAX, f32::MAX, 1.0],
    }
}

/// The physical bounds of a region after the same endpoint snap the vertex shader applies to
/// the whole photo. Used as an alpha clip for a full-stage overlay whose rounded rectangle can
/// extend fractionally beyond its matching half-stage pixels.
fn region_physical_rect(destination: [f32; 4], key: RegionKey) -> [f32; 4] {
    let left = destination[0].round_ties_even();
    let top = destination[1].round_ties_even();
    let right = (destination[0] + destination[2]).round_ties_even();
    let bottom = (destination[1] + destination[3]).round_ties_even();
    [
        left + (right - left) * key.rect[0] as f32 / key.stage.0 as f32,
        top + (bottom - top) * key.rect[1] as f32 / key.stage.1 as f32,
        left + (right - left) * key.rect[2] as f32 / key.stage.0 as f32,
        top + (bottom - top) * key.rect[3] as f32 / key.stage.1 as f32,
    ]
}

/// One texture of a frame and the part of the frame it draws, in frame pixels.
///
/// A frame wider or taller than the device's largest texture is held in several textures, each
/// within that limit. `content` is the part a tile draws, and the tiles' contents partition the
/// frame. `texels` is the part its texture holds: `content` plus one pixel on each side that has
/// a neighbouring tile, so the linear filter at a seam reads the same neighbour a single texture
/// would and the picture is sampled as if it were one texture. A frame within the limit is one
/// tile whose texture holds all of it, which is the single texture it always was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TileLayout {
    /// `[x0, y0, x1, y1]`: the pixels this tile draws, end exclusive.
    content: [u32; 4],
    /// `[x0, y0, x1, y1]`: the pixels its texture holds, end exclusive.
    texels: [u32; 4],
}

impl TileLayout {
    fn texture_size(&self) -> (u32, u32) {
        (
            self.texels[2] - self.texels[0],
            self.texels[3] - self.texels[1],
        )
    }

    /// Where the tile's pixels start in the frame's bytes, and the frame's row length: the tile is
    /// written straight out of the shared buffer, with no copy of its own.
    fn copy_layout(&self, raster_width: u32) -> wgpu::TexelCopyBufferLayout {
        let (_, rows) = self.texture_size();
        wgpu::TexelCopyBufferLayout {
            offset: (u64::from(self.texels[1]) * u64::from(raster_width)
                + u64::from(self.texels[0]))
                * 4,
            bytes_per_row: Some(raster_width * 4),
            rows_per_image: Some(rows),
        }
    }

    /// The uniform's third and fourth rectangles: the part of the picture this tile draws as
    /// fractions of the whole, and the same part in the tile texture's own coordinates.
    fn placement(
        &self,
        (width, height): (u32, u32),
        (texture_width, texture_height): (u32, u32),
    ) -> ([f32; 4], [f32; 4]) {
        let [x0, y0, x1, y1] = self.content;
        let [tx, ty, ..] = self.texels;
        (
            [
                x0 as f32 / width as f32,
                y0 as f32 / height as f32,
                x1 as f32 / width as f32,
                y1 as f32 / height as f32,
            ],
            [
                (x0 - tx) as f32 / texture_width as f32,
                (y0 - ty) as f32 / texture_height as f32,
                (x1 - tx) as f32 / texture_width as f32,
                (y1 - ty) as f32 / texture_height as f32,
            ],
        )
    }
}

/// Split a frame into tiles whose textures are at most `limit` pixels a side, as evenly as the
/// count allows. Each axis is split on its own, so the tiles form a grid, listed row by row.
fn tile_layout_for_grid(
    (width, height): (u32, u32),
    (columns, rows): (u32, u32),
) -> Vec<TileLayout> {
    // `[content start, content end, texels start, texels end]` along one axis.
    let spans = |extent: u32, count: u32| -> Vec<[u32; 4]> {
        let edge = |index: u32| (u64::from(extent) * u64::from(index) / u64::from(count)) as u32;
        (0..count)
            .map(|index| {
                let (start, end) = (edge(index), edge(index + 1));
                [start, end, start.saturating_sub(1), (end + 1).min(extent)]
            })
            .collect()
    };
    let columns = spans(width, columns);
    spans(height, rows)
        .iter()
        .flat_map(|row| {
            columns.iter().map(move |column| TileLayout {
                content: [column[0], row[0], column[1], row[1]],
                texels: [column[2], row[2], column[3], row[3]],
            })
        })
        .collect()
}

fn tile_grid((width, height): (u32, u32), limit: u32) -> (u32, u32) {
    // Two pixels of each interior tile may be filtering apron.
    let usable = limit.saturating_sub(2).max(1);
    let count = |extent: u32| {
        if extent <= limit {
            1
        } else {
            extent.div_ceil(usable)
        }
    };
    (count(width), count(height))
}

fn tile_layout(size: (u32, u32), limit: u32) -> Vec<TileLayout> {
    tile_layout_for_grid(size, tile_grid(size, limit))
}

/// Six `vec4<f32>`.
const UNIFORM_SIZE: usize = 96;

/// The uniform block the shader reads for one tile, as little-endian floats: the render pass's
/// viewport and the whole picture's destination, both in physical pixels of the whole frame; the
/// part of the picture the tile draws as fractions of the whole, and that part in the tile
/// texture's coordinates; the bright rectangle as `[x0, y0, x1, y1]` physical pixels; and the turn
/// as `[sin, cos, dim, snap]`.
fn uniform_bytes(rectangles: [[f32; 4]; 6]) -> [u8; UNIFORM_SIZE] {
    let mut bytes = [0u8; UNIFORM_SIZE];
    for (index, value) in rectangles.iter().flatten().enumerate() {
        // GPU buffers are little-endian on every platform wgpu targets.
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// The turn as the shader reads it. An angle of exactly zero has a sine of exactly zero, which the
/// shader takes as "not turned", so the photograph's corners are never moved by a rotation's
/// rounding.
fn turn_uniform(angle: f32, dim: f32, snap: bool) -> [f32; 4] {
    let (sin, cos) = if angle == 0.0 {
        (0.0, 1.0)
    } else {
        angle.sin_cos()
    };
    [sin, cos, dim, if snap { 1.0 } else { 0.0 }]
}

impl shader::Primitive for PhotoPrimitive {
    type Pipeline = PhotoPipeline;

    fn prepare(
        &self,
        pipeline: &mut PhotoPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        for layer in [Layer::Stage, Layer::Clipping, Layer::Coverage] {
            let region_overlay = match layer {
                Layer::Clipping => self.region_overlays[0].is_some(),
                Layer::Coverage => self.region_overlays[1].is_some(),
                _ => false,
            };
            if !region_overlay && !self.layers.iter().any(|(drawn, _)| *drawn == layer) {
                pipeline.slots[layer.index()] = None;
            }
        }
        for (layer, frame) in &self.layers {
            let content_id = self.viewport.as_ref().and_then(|viewport| {
                (viewport.current_content
                    == viewport
                        .full
                        .as_ref()
                        .map_or(u64::MAX, |(_, content)| *content))
                .then_some(viewport.current_content)
            });
            if self.viewport.is_none() || *layer != Layer::Photo {
                pipeline.write(device, queue, *layer, frame, content_id, None);
            }
        }
        if let Some(view) = &self.viewport {
            if let Some((frame, content)) = &view.full
                && *content == view.current_content
            {
                pipeline.write(device, queue, Layer::Photo, frame, Some(*content), None);
            }
            let full_ready = pipeline.slots[Layer::Photo.index()]
                .as_ref()
                .is_some_and(|picture| picture.content_id == Some(view.current_content));
            if !full_ready
                && let Some(region) = &view.region
                && region.content_id == view.current_content
            {
                pipeline.write_region(device, queue, region);
            }
            for (index, layer) in [Layer::Clipping, Layer::Coverage].into_iter().enumerate() {
                if let Some(overlay) = &self.region_overlays[index]
                    && let key = overlay.key()
                    && key.content_id == view.current_content
                    && key.full_stage == view.full_stage
                {
                    pipeline.write(
                        device,
                        queue,
                        layer,
                        &overlay.frame,
                        Some(key.content_id),
                        Some(key),
                    );
                }
            }
        }
        // The uniforms are refreshed every prepare instead, because the bounds and the viewport
        // can change with no new frame at all — a window resize, a pan, a panel opening. `bounds`
        // is the visible part of the widget, translated to where it is drawn.
        let scale = viewport.scale_factor();
        let (viewport, destination) = physical_rects(*bounds, self.offset, self.size, scale);
        let [x0, y0, x1, y1, dim] = physical_bright(*bounds, self.bright, scale);
        let turn = turn_uniform(self.angle, dim, self.snap);
        for (layer, _) in &self.layers {
            if let Some(picture) = &pipeline.slots[layer.index()] {
                write_uniforms(
                    queue,
                    picture,
                    viewport,
                    destination,
                    [x0, y0, x1, y1],
                    turn,
                );
            }
        }
        if self.viewport.is_some() {
            if let Some(picture) = &pipeline.slots[Layer::Photo.index()] {
                write_uniforms(
                    queue,
                    picture,
                    viewport,
                    destination,
                    [x0, y0, x1, y1],
                    turn,
                );
            }
            for picture in pipeline.regions.iter().flatten() {
                write_uniforms(
                    queue,
                    picture,
                    viewport,
                    destination,
                    [x0, y0, x1, y1],
                    turn,
                );
            }
            for (index, layer) in [Layer::Clipping, Layer::Coverage].into_iter().enumerate() {
                if let Some(overlay) = &self.region_overlays[index]
                    && let Some(picture) = &pipeline.slots[layer.index()]
                {
                    let matched =
                        if pipeline.slots[Layer::Photo.index()]
                            .as_ref()
                            .is_some_and(|full| {
                                full.content_id == Some(overlay.content_id)
                                    && full.region_key.is_none()
                            })
                            && overlay.quality == RegionQuality::Exact
                        {
                            None
                        } else {
                            pipeline
                                .regions
                                .iter()
                                .flatten()
                                .filter_map(|picture| picture.region_key)
                                .find(|key| overlay.matches_region(*key))
                        };
                    let mut overlay_turn = turn;
                    let overlay_bright = matched.map_or([x0, y0, x1, y1], |key| {
                        overlay_turn[2] = 0.0;
                        region_physical_rect(destination, key)
                    });
                    write_uniforms(
                        queue,
                        picture,
                        viewport,
                        destination,
                        overlay_bright,
                        overlay_turn,
                    );
                }
            }
        }
        pipeline.publish_diagnostics();
    }

    fn draw(&self, pipeline: &PhotoPipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        // The render pass's viewport is already the visible part of this widget and its scissor
        // that part clipped to the layer, so each tile's quad is positioned inside that frame by
        // its uniform alone and whatever of it falls outside is clipped. The layers are drawn in
        // order, each blended over the one before.
        render_pass.set_pipeline(&pipeline.pipeline);
        let mut drawn_clipping_version = None;
        let mut drawn_full_version = None;
        let mut drawn_region_version = None;
        let mut drawn_region_generation = None;
        let mut drawn_region_quality = None;
        let mut drawn_regions = [None; 2];
        let mut drawn_content = None;
        let mut drawn_fallback_content = None;
        let mut drew_photo = false;
        let mut stale_photo = false;
        if let Some(view) = &self.viewport {
            let matching_full = pipeline.slots[Layer::Photo.index()]
                .as_ref()
                .filter(|picture| {
                    picture.content_id == Some(view.current_content)
                        && picture.region_key.is_none()
                        && (picture.width, picture.height) == view.full_stage
                });
            let matching_regions = if matching_full.is_some() {
                Vec::new()
            } else {
                region_draw_order(&pipeline.regions, view.current_content, view.full_stage)
                    .into_iter()
                    .filter(|&index| {
                        pipeline.regions[index]
                            .as_ref()
                            .and_then(|picture| picture.region_key)
                            .is_some_and(|key| {
                                region_intersects_clip(key, self.offset, self.size, self.clip_size)
                            })
                    })
                    .collect()
            };
            // A deferred upload must leave a coherent previous photograph on screen. Prefer a
            // whole picture, then one previous region content; never composite different contents.
            let full = matching_full.or_else(|| {
                matching_regions.is_empty().then(|| {
                    pipeline.slots[Layer::Photo.index()]
                        .as_ref()
                        .filter(|picture| picture.region_key.is_none())
                })?
            });
            let region_order = if full.is_some() {
                Vec::new()
            } else if !matching_regions.is_empty() {
                matching_regions
            } else {
                pipeline
                    .regions
                    .iter()
                    .flatten()
                    .filter_map(|picture| picture.region_key)
                    .filter(|key| {
                        key.full_stage == view.full_stage
                            && region_intersects_clip(*key, self.offset, self.size, self.clip_size)
                    })
                    .max_by_key(|key| key.generation)
                    .map_or_else(Vec::new, |key| {
                        region_draw_order(&pipeline.regions, key.content_id, view.full_stage)
                            .into_iter()
                            .filter(|&index| {
                                pipeline.regions[index]
                                    .as_ref()
                                    .and_then(|picture| picture.region_key)
                                    .is_some_and(|key| {
                                        region_intersects_clip(
                                            key,
                                            self.offset,
                                            self.size,
                                            self.clip_size,
                                        )
                                    })
                            })
                            .collect()
                    })
            };
            let region_keys: [Option<RegionKey>; 2] = std::array::from_fn(|index| {
                pipeline.regions[index]
                    .as_ref()
                    .and_then(|picture| picture.region_key)
            });
            if let Some(picture) = full {
                draw_picture(render_pass, picture);
                drew_photo = true;
                drawn_full_version = Some(picture.version);
                stale_photo = matching_full.is_none();
                if stale_photo {
                    drawn_fallback_content = picture.content_id;
                } else {
                    drawn_content = Some(view.current_content);
                }
            } else {
                for &index in &region_order {
                    let picture = pipeline.regions[index].as_ref().expect("selected region");
                    draw_picture(render_pass, picture);
                    drew_photo = true;
                    if let Some(key) = picture.region_key {
                        drawn_region_version = Some(picture.version);
                        drawn_region_generation = Some(key.generation);
                        drawn_region_quality = Some(key.quality);
                        stale_photo = key.content_id != view.current_content;
                        if stale_photo {
                            drawn_fallback_content = Some(key.content_id);
                        } else {
                            drawn_content = Some(view.current_content);
                        }
                        drawn_regions[index] = Some(DrawnRegion {
                            version: picture.version,
                            content_id: key.content_id,
                            generation: key.generation,
                            quality: key.quality,
                        });
                    }
                }
            }
            for (layer, _) in &self.layers {
                if *layer != Layer::Photo
                    && let Some(picture) = &pipeline.slots[layer.index()]
                    && matching_full.is_some()
                    && picture.content_id == Some(view.current_content)
                {
                    draw_picture(render_pass, picture);
                    if *layer == Layer::Clipping {
                        drawn_clipping_version = Some(picture.version);
                    }
                }
            }
            for (index, layer) in [Layer::Clipping, Layer::Coverage].into_iter().enumerate() {
                let Some(overlay) = &self.region_overlays[index] else {
                    continue;
                };
                let key = overlay.key();
                let matching_region = !stale_photo
                    && full.is_none()
                    && overlay_matches_draw_order(overlay, region_keys, &region_order);
                let matching_full = !stale_photo
                    && matching_full.is_some()
                    && key.content_id == view.current_content
                    && key.quality == RegionQuality::Exact;
                if (matching_region || matching_full)
                    && let Some(picture) = &pipeline.slots[layer.index()]
                    && picture.region_key == Some(key)
                {
                    draw_picture(render_pass, picture);
                    if layer == Layer::Clipping {
                        drawn_clipping_version = Some(picture.version);
                    }
                }
            }
        } else {
            let photo_ready = self
                .layers
                .iter()
                .find(|(layer, _)| *layer == Layer::Photo)
                .is_none_or(|(_, frame)| {
                    pipeline.slots[Layer::Photo.index()]
                        .as_ref()
                        .is_some_and(|picture| {
                            picture.matching_frame(frame, None, None)
                                && picture.width == frame.width
                                && picture.height == frame.height
                        })
                });
            for (layer, frame) in &self.layers {
                if matches!(*layer, Layer::Clipping | Layer::Coverage) && !photo_ready {
                    continue;
                }
                if let Some(picture) = &pipeline.slots[layer.index()] {
                    draw_picture(render_pass, picture);
                    if *layer == Layer::Photo {
                        drew_photo = true;
                        drawn_full_version = Some(picture.version);
                        stale_photo = !picture.matching_frame(frame, None, None)
                            || picture.width != frame.width
                            || picture.height != frame.height;
                        if stale_photo {
                            drawn_fallback_content = picture.content_id;
                        }
                    }
                    if *layer == Layer::Clipping {
                        drawn_clipping_version = Some(picture.version);
                    }
                }
            }
        }
        // Every attempted photo draw writes a complete identity snapshot, including blanks. A
        // stale previous photo reports its actual content separately from the requested content.
        let expects_photo =
            self.viewport.is_some() || self.layers.iter().any(|(layer, _)| *layer == Layer::Photo);
        let mut diagnostic = pipeline.figures.diagnostics();
        let blank_photo = expects_photo && !drew_photo;
        let status_changed = diagnostic.drawn_photo_blank != blank_photo
            || diagnostic.drawn_stale_photo != stale_photo;
        diagnostic.drawn_content = drawn_content;
        diagnostic.drawn_full_version = drawn_full_version;
        diagnostic.drawn_region_version = drawn_region_version;
        diagnostic.drawn_region_generation = drawn_region_generation;
        diagnostic.drawn_region_quality = drawn_region_quality;
        diagnostic.drawn_regions = drawn_regions;
        diagnostic.drawn_clipping_version = drawn_clipping_version;
        diagnostic.drawn_photo_blank = blank_photo;
        diagnostic.drawn_stale_photo = stale_photo;
        diagnostic.drawn_fallback_content = drawn_fallback_content;
        if drew_photo {
            diagnostic.drawn_frames += 1;
            if stale_photo {
                diagnostic.stale_photo_draws += 1;
            }
        } else if expects_photo {
            diagnostic.blank_photo_draws += 1;
        }
        drop(diagnostic);
        if status_changed {
            // A draw can discover staleness after the app has built its status bar. One buffered
            // wake refreshes that label on the next update, and another clears it after recovery.
            wake_surface();
        }
        // Drawn either way: with no texture there is nothing to show, and the encoder fallback
        // would only begin a render pass to draw the same nothing.
        true
    }
}

fn write_uniforms(
    queue: &wgpu::Queue,
    picture: &Picture,
    viewport: [f32; 4],
    destination: [f32; 4],
    bright: [f32; 4],
    turn: [f32; 4],
) {
    for tile in &picture.tiles {
        let (whole_region, texels) = tile
            .layout
            .placement((picture.width, picture.height), tile.capacity);
        let region = picture.region_key.map_or(whole_region, |key| {
            key.placement(tile.layout, (picture.width, picture.height))
        });
        queue.write_buffer(
            &tile.uniform,
            0,
            &uniform_bytes([viewport, destination, region, texels, bright, turn]),
        );
    }
}

fn draw_picture(render_pass: &mut wgpu::RenderPass<'_>, picture: &Picture) {
    for tile in &picture.tiles {
        render_pass.set_bind_group(0, &tile.bindings, &[]);
        render_pass.draw(0..6, 0..1);
    }
}

fn region_key_order(
    keys: [Option<RegionKey>; 2],
    current_content: u64,
    full_stage: (u32, u32),
) -> Vec<usize> {
    let mut ordered: Vec<_> = keys
        .into_iter()
        .enumerate()
        .filter_map(|(index, key)| {
            let key = key?;
            (key.content_id == current_content && key.full_stage == full_stage)
                .then_some((index, key))
        })
        .collect();
    ordered.sort_by_key(|(index, key)| {
        (
            matches!(key.quality, RegionQuality::Exact),
            key.generation,
            *index,
        )
    });
    ordered.into_iter().map(|(index, _)| index).collect()
}

/// Test overlap in the common full-stage coordinate frame without rounding half-stage edges.
/// Both keys already name the same full stage when they are selected for one draw.
fn regions_overlap(a: RegionKey, b: RegionKey) -> bool {
    let axis = |a0: u32, a1: u32, a_stage: u32, b0: u32, b1: u32, b_stage: u32| {
        u64::from(a0) * u64::from(b_stage) < u64::from(b1) * u64::from(a_stage)
            && u64::from(b0) * u64::from(a_stage) < u64::from(a1) * u64::from(b_stage)
    };
    axis(
        a.rect[0], a.rect[2], a.stage.0, b.rect[0], b.rect[2], b.stage.0,
    ) && axis(
        a.rect[1], a.rect[3], a.stage.1, b.rect[1], b.rect[3], b.stage.1,
    )
}

/// A region's grid describes only its own pixels. When a later, higher-priority region with a
/// different identity covers even part of that grid, omit the grid until a matching one arrives;
/// drawing it over the newer pixels could falsely mark or miss clipping or mask coverage.
fn overlay_matches_draw_order(
    overlay: &RegionOverlay,
    keys: [Option<RegionKey>; 2],
    order: &[usize],
) -> bool {
    let Some(position) = order
        .iter()
        .rposition(|&index| keys[index].is_some_and(|key| overlay.matches_region(key)))
    else {
        return false;
    };
    let overlay_key = overlay.key();
    !order[position + 1..].iter().any(|&index| {
        keys[index]
            .is_some_and(|key| !overlay.matches_region(key) && regions_overlap(key, overlay_key))
    })
}

fn region_draw_order(
    regions: &[Option<Picture>; 2],
    current_content: u64,
    full_stage: (u32, u32),
) -> Vec<usize> {
    region_key_order(
        std::array::from_fn(|index| {
            regions[index]
                .as_ref()
                .and_then(|picture| picture.region_key)
        }),
        current_content,
        full_stage,
    )
}

/// Whether a region's actual stage rectangle can put any photograph pixels in this primitive's
/// visible clip. A pan can leave a retained region wholly off screen; encoding that quad does not
/// count as drawing a photograph.
fn region_intersects_clip(key: RegionKey, offset: Vector, size: Size, clip: Size) -> bool {
    let left = offset.x + size.width * key.rect[0] as f32 / key.stage.0 as f32;
    let top = offset.y + size.height * key.rect[1] as f32 / key.stage.1 as f32;
    let right = offset.x + size.width * key.rect[2] as f32 / key.stage.0 as f32;
    let bottom = offset.y + size.height * key.rect[3] as f32 / key.stage.1 as f32;
    left < clip.width && right > 0.0 && top < clip.height && bottom > 0.0
}

/// One tile on the GPU: its texture, its own uniform and the bindings that join them.
struct Tile {
    layout: TileLayout,
    capacity: (u32, u32),
    texture: wgpu::Texture,
    uniform: wgpu::Buffer,
    bindings: wgpu::BindGroup,
}

/// One layer's frame on the GPU, with what it holds.
struct Picture {
    tiles: Vec<Tile>,
    width: u32,
    height: u32,
    capacity: (u32, u32),
    grid: (u32, u32),
    /// The texture limit the tiles were cut for.
    limit: u32,
    /// The version of the frame last written into it.
    version: u64,
    content_id: Option<u64>,
    region_key: Option<RegionKey>,
    allocated_bytes: u64,
}

struct RetiredPicture {
    picture: Picture,
    full: bool,
}

impl Picture {
    fn matching_frame(
        &self,
        frame: &Frame,
        content_id: Option<u64>,
        key: Option<RegionKey>,
    ) -> bool {
        self.version == frame.version && self.content_id == content_id && self.region_key == key
    }

    fn layouts_for(&self, size: (u32, u32), limit: u32) -> Option<Vec<TileLayout>> {
        if limit != self.limit
            || size.0 > self.capacity.0
            || size.1 > self.capacity.1
            || size.0 < self.grid.0
            || size.1 < self.grid.1
        {
            return None;
        }
        let layouts = tile_layout_for_grid(size, self.grid);
        layouts
            .iter()
            .zip(&self.tiles)
            .all(|(layout, tile)| {
                let (width, height) = layout.texture_size();
                width <= tile.capacity.0 && height <= tile.capacity.1
            })
            .then_some(layouts)
    }

    fn set_layouts(&mut self, size: (u32, u32), layouts: Vec<TileLayout>) {
        self.width = size.0;
        self.height = size.1;
        for (tile, layout) in self.tiles.iter_mut().zip(layouts) {
            tile.layout = layout;
        }
    }
}

fn allocated_bytes(tiles: &[TileLayout]) -> u64 {
    tiles
        .iter()
        .map(|tile| {
            let (width, height) = tile.texture_size();
            u64::from(width) * u64::from(height) * 4
        })
        .sum()
}

/// A full-slot proxy commonly changes orientation or grows a little on a refit. Reserve a square
/// bucket while it fits the slot's own byte cap, so those changes only rewrite used texels.
fn full_capacity((width, height): (u32, u32), limit: u32) -> (u32, u32) {
    let longer = width.max(height);
    let edge = if longer <= 2048 {
        longer.next_power_of_two()
    } else {
        longer.next_multiple_of(512)
    };
    let square = (edge, edge);
    if edge <= limit && allocated_bytes(&tile_layout(square, limit)) <= FULL_BUDGET {
        square
    } else {
        (width, height)
    }
}

/// A half-detail region reserves its exact-stage footprint, plus a small bucket for the one-pixel
/// floor/ceil changes of moving pans. The reservation itself must fit one 32 MiB region set.
fn region_capacity(region: &RegionFrame, limit: u32) -> (u32, u32) {
    let (width, height) = region.frame.size();
    let projected = |used: u32, stage: u32, full: u32| {
        let exact = u64::from(used)
            .saturating_mul(u64::from(full))
            .div_ceil(u64::from(stage))
            .saturating_add(2)
            .min(u64::from(u32::MAX)) as u32;
        exact.max(used).next_multiple_of(64)
    };
    let reserved = (
        projected(width, region.stage.0, region.full_stage.0),
        projected(height, region.stage.1, region.full_stage.1),
    );
    if allocated_bytes(&tile_layout(reserved, limit)) <= REGION_SET_BUDGET {
        reserved
    } else {
        (width, height)
    }
}

/// The render pipeline, the samplers and every layer's textures, shared by every instance of
/// [`PhotoPrimitive`]. One photograph is on screen at a time, so this is one set of textures per
/// layer for the application: one texture each unless a frame is larger than the device allows.
pub struct PhotoPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    linear: wgpu::Sampler,
    nearest: wgpu::Sampler,
    texture_format: wgpu::TextureFormat,
    slots: [Option<Picture>; 4],
    regions: [Option<Picture>; 2],
    region_front: usize,
    retiring_bytes: Arc<AtomicU64>,
    retiring_full: Arc<AtomicU64>,
    retiring_regions: Arc<AtomicU64>,
    retiring_region_bytes: Arc<AtomicU64>,
    retirement_sender: std::sync::mpsc::Sender<RetiredPicture>,
    deferred_photo: bool,
    deferred_region: bool,
    /// What this pipeline counts of its own texture work.
    figures: Arc<SurfaceFigures>,
}

impl PhotoPipeline {
    fn retire(&self, picture: Picture, full: bool) {
        let bytes = picture.allocated_bytes;
        self.retiring_bytes.fetch_add(bytes, Ordering::AcqRel);
        if full {
            self.retiring_full.fetch_add(1, Ordering::AcqRel);
        } else {
            self.retiring_regions.fetch_add(1, Ordering::AcqRel);
            self.retiring_region_bytes
                .fetch_add(bytes, Ordering::AcqRel);
        }
        self.figures
            .retirement_pending
            .fetch_add(1, Ordering::AcqRel);
        {
            let mut diagnostic = self.figures.diagnostics();
            diagnostic.retiring_bytes += bytes;
        }
        // The worker receives each retirement once. At most the one full slot and two region
        // sets can be charged at once, so this queue is bounded by admission rather than a timer.
        if let Err(error) = self
            .retirement_sender
            .send(RetiredPicture { picture, full })
        {
            // Device loss or pipeline teardown can end the worker. Its GPU allocations are then
            // invalid; release their charge and wake the desktop instead of waiting forever.
            finish_retirement(
                &self.figures,
                error.0,
                &self.retiring_bytes,
                &self.retiring_full,
                &self.retiring_regions,
                &self.retiring_region_bytes,
                true,
            );
        }
    }

    fn resident_region_bytes(&self) -> u64 {
        self.regions
            .iter()
            .flatten()
            .map(|picture| picture.allocated_bytes)
            .sum()
    }

    fn publish_diagnostics(&self) {
        let mut diagnostic = self.figures.diagnostics();
        diagnostic.full_resident_bytes = self.slots[Layer::Photo.index()]
            .as_ref()
            .map_or(0, |picture| picture.allocated_bytes);
        diagnostic.region_resident_bytes = self.resident_region_bytes();
        diagnostic.stage_resident_bytes = self.slots[Layer::Stage.index()]
            .as_ref()
            .map_or(0, |picture| picture.allocated_bytes);
        diagnostic.photo_writes = self.figures.writes.load(Ordering::Relaxed);
        diagnostic.upload_bytes = self.figures.upload_bytes.load(Ordering::Relaxed);
    }

    fn defer(&mut self, region: bool) {
        self.figures.diagnostics().deferred_uploads += 1;
        if region {
            self.deferred_region = true;
        } else {
            self.deferred_photo = true;
        }
    }

    /// Make `layer`'s textures hold `frame`, creating them when the dimensions changed and writing
    /// the pixels when the version did.
    fn write(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layer: Layer,
        frame: &Frame,
        content_id: Option<u64>,
        region_key: Option<RegionKey>,
    ) -> bool {
        let (width, height) = frame.size();
        // The device's limit, not the GPU's: Iced asks wgpu for its default limits, so this is
        // 8192 on the owner's Mac although the GPU could hold larger textures.
        let limit = device.limits().max_texture_dimension_2d;
        let reusable = self.slots[layer.index()]
            .as_ref()
            // A crop stage is reserved at exactly its frame's size, so the display-size proxy a
            // draft shows at Fit never keeps the full-size texture an exact stage at a percentage
            // zoom allocated.
            .filter(|picture| layer != Layer::Stage || picture.capacity == (width, height))
            .and_then(|picture| picture.layouts_for((width, height), limit));
        let fresh = reusable.is_none();
        if fresh {
            let capacity = if layer == Layer::Photo {
                full_capacity((width, height), limit)
            } else {
                (width, height)
            };
            let layouts = tile_layout(capacity, limit);
            let bytes = allocated_bytes(&layouts);
            if layer == Layer::Photo {
                if bytes > FULL_BUDGET {
                    self.figures.diagnostics().rejected_full_uploads += 1;
                    return false;
                }
                // At most the current and one retiring full allocation may exist. If an older
                // retirement is still in flight, keep the current texture drawable until it ends.
                if self.retiring_full.load(Ordering::Acquire) > 0 {
                    self.defer(false);
                    self.publish_diagnostics();
                    return false;
                }
                if let Some(old) = self.slots[layer.index()].take() {
                    self.retire(old, true);
                }
            }
            let grid = tile_grid(capacity, limit);
            let tiles = layouts
                .into_iter()
                .map(|tile| self.tile(device, layer, tile))
                .collect();
            self.slots[layer.index()] = Some(Picture {
                tiles,
                width,
                height,
                capacity,
                grid,
                limit,
                // No version can match until the pixels are written below.
                version: frame.version.wrapping_sub(1),
                content_id: None,
                region_key: None,
                allocated_bytes: bytes,
            });
        }
        let Some(picture) = &mut self.slots[layer.index()] else {
            return false;
        };
        if let Some(layouts) = reusable {
            picture.set_layouts((width, height), layouts);
        } else if let Some(layouts) = picture.layouts_for((width, height), limit) {
            picture.set_layouts((width, height), layouts);
        }
        if picture.matching_frame(frame, content_id, region_key) {
            if layer == Layer::Photo && std::mem::take(&mut self.deferred_photo) {
                wake_surface();
            }
            return true;
        }
        upload_picture(
            queue,
            picture,
            frame,
            (layer == Layer::Photo).then_some(&self.figures.upload_bytes),
        );
        picture.version = frame.version;
        picture.content_id = content_id;
        picture.region_key = region_key;
        if layer == Layer::Photo {
            self.figures.writes.fetch_add(1, Ordering::Relaxed);
        }
        self.publish_diagnostics();
        if layer == Layer::Photo && std::mem::take(&mut self.deferred_photo) {
            // The prior retirement wake scheduled this prepare; one more app update observes the
            // newly drawn identity and removes the temporary updating label.
            wake_surface();
        }
        true
    }

    fn write_region(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, region: &RegionFrame) {
        let key = region.key();
        if self
            .regions
            .iter()
            .flatten()
            .any(|picture| picture.matching_frame(&region.frame, Some(key.content_id), Some(key)))
        {
            if std::mem::take(&mut self.deferred_region) {
                wake_surface();
            }
            return;
        }
        let index = 1 - self.region_front;
        let (width, height) = region.frame.size();
        let limit = device.limits().max_texture_dimension_2d;
        let reusable = self.regions[index]
            .as_ref()
            .and_then(|picture| picture.layouts_for((width, height), limit));
        let fresh = reusable.is_none();
        if fresh {
            let capacity = region_capacity(region, limit);
            let layouts = tile_layout(capacity, limit);
            let bytes = allocated_bytes(&layouts);
            if bytes > REGION_SET_BUDGET {
                self.figures.diagnostics().rejected_region_uploads += 1;
                return;
            }
            if let Some(old) = self.regions[index].take() {
                self.retire(old, false);
            }
            // A retired region remains charged. The two sets together, including aprons, stay
            // within 64 MiB and there are never more than two live or retiring allocations.
            let region_count = self.regions.iter().flatten().count() as u64
                + self.retiring_regions.load(Ordering::Acquire);
            let total = self.resident_region_bytes()
                + self.retiring_region_bytes.load(Ordering::Acquire)
                + bytes;
            if region_count >= 2 || total > 2 * REGION_SET_BUDGET {
                self.defer(true);
                self.publish_diagnostics();
                return;
            }
            let grid = tile_grid(capacity, limit);
            let tiles = layouts
                .into_iter()
                .map(|tile| self.tile(device, Layer::Photo, tile))
                .collect();
            self.regions[index] = Some(Picture {
                tiles,
                width,
                height,
                capacity,
                grid,
                limit,
                version: region.frame.version.wrapping_sub(1),
                content_id: None,
                region_key: None,
                allocated_bytes: bytes,
            });
        }
        let picture = self.regions[index].as_mut().expect("admitted region");
        if let Some(layouts) = reusable {
            picture.set_layouts((width, height), layouts);
        } else if let Some(layouts) = picture.layouts_for((width, height), limit) {
            picture.set_layouts((width, height), layouts);
        }
        upload_picture(
            queue,
            picture,
            &region.frame,
            Some(&self.figures.upload_bytes),
        );
        picture.version = region.frame.version;
        picture.content_id = Some(key.content_id);
        picture.region_key = Some(key);
        self.region_front = index;
        self.figures.writes.fetch_add(1, Ordering::Relaxed);
        self.publish_diagnostics();
        if std::mem::take(&mut self.deferred_region) {
            wake_surface();
        }
    }

    /// One tile's texture, uniform and bindings, sampled as its layer is.
    fn tile(&self, device: &wgpu::Device, layer: Layer, layout: TileLayout) -> Tile {
        let (width, height) = layout.texture_size();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("luxforge.photo_surface.texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.texture_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("luxforge.photo_surface.uniform"),
            size: UNIFORM_SIZE as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = if layer.nearest() {
            &self.nearest
        } else {
            &self.linear
        };
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("luxforge.photo_surface.bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        Tile {
            layout,
            capacity: (width, height),
            texture,
            uniform,
            bindings,
        }
    }
}

impl Drop for PhotoPipeline {
    fn drop(&mut self) {
        if let Some(picture) = self.slots[Layer::Photo.index()].take() {
            self.retire(picture, true);
        }
        for index in 0..self.regions.len() {
            if let Some(picture) = self.regions[index].take() {
                self.retire(picture, false);
            }
        }
        self.publish_diagnostics();
    }
}

/// Queue directly from the borrowed frame, in bounded chunks. The surface owns no pixel staging;
/// wgpu's backend staging is separate and measured in native GPU evidence. A tile's apron bytes
/// are included in the write count and allocated-byte admission.
fn upload_picture(
    queue: &wgpu::Queue,
    picture: &Picture,
    frame: &Frame,
    counted: Option<&AtomicU64>,
) {
    const UPLOAD_CHUNK: u64 = 8 * 1024 * 1024;
    for tile in &picture.tiles {
        let (tile_width, tile_height) = tile.layout.texture_size();
        let rows_per_chunk = (UPLOAD_CHUNK / (u64::from(tile_width) * 4)).max(1) as u32;
        let mut row = 0;
        while row < tile_height {
            let rows = rows_per_chunk.min(tile_height - row);
            let mut destination = tile.texture.as_image_copy();
            destination.origin.y = row;
            let mut source = tile.layout.copy_layout(frame.width);
            source.offset += u64::from(row) * u64::from(frame.width) * 4;
            source.rows_per_image = Some(rows);
            queue.write_texture(
                destination,
                (*frame.pixels).as_ref(),
                source,
                wgpu::Extent3d {
                    width: tile_width,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
            if let Some(counted) = counted {
                counted.fetch_add(
                    u64::from(tile_width) * u64::from(rows) * 4,
                    Ordering::Relaxed,
                );
            }
            row += rows;
        }
        // The active raster may use only the upper-left part of a reusable bucket. The linear
        // sampler must see the active edge texel on both sides of that edge, just as it would in
        // an exactly sized texture. Copy one borrowed edge column/row into the unused border.
        let (capacity_width, capacity_height) = tile.capacity;
        if tile_width < capacity_width {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tile.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: tile_width,
                        y: 0,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                (*frame.pixels).as_ref(),
                wgpu::TexelCopyBufferLayout {
                    offset: (u64::from(tile.layout.texels[1]) * u64::from(frame.width)
                        + u64::from(tile.layout.texels[2] - 1))
                        * 4,
                    bytes_per_row: Some(frame.width * 4),
                    rows_per_image: Some(tile_height),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: tile_height,
                    depth_or_array_layers: 1,
                },
            );
            if let Some(counted) = counted {
                counted.fetch_add(u64::from(tile_height) * 4, Ordering::Relaxed);
            }
        }
        if tile_height < capacity_height {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tile.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: tile_height,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                (*frame.pixels).as_ref(),
                wgpu::TexelCopyBufferLayout {
                    offset: (u64::from(tile.layout.texels[3] - 1) * u64::from(frame.width)
                        + u64::from(tile.layout.texels[0]))
                        * 4,
                    bytes_per_row: Some(frame.width * 4),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: tile_width,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            if let Some(counted) = counted {
                counted.fetch_add(u64::from(tile_width) * 4, Ordering::Relaxed);
            }
        }
        if tile_width < capacity_width && tile_height < capacity_height {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tile.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: tile_width,
                        y: tile_height,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                (*frame.pixels).as_ref(),
                wgpu::TexelCopyBufferLayout {
                    offset: (u64::from(tile.layout.texels[3] - 1) * u64::from(frame.width)
                        + u64::from(tile.layout.texels[2] - 1))
                        * 4,
                    bytes_per_row: Some(4),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            if let Some(counted) = counted {
                counted.fetch_add(4, Ordering::Relaxed);
            }
        }
    }
}

fn finish_retirement(
    figures: &SurfaceFigures,
    retired: RetiredPicture,
    retiring_bytes: &AtomicU64,
    retiring_full: &AtomicU64,
    retiring_regions: &AtomicU64,
    retiring_region_bytes: &AtomicU64,
    failed: bool,
) {
    let bytes = retired.picture.allocated_bytes;
    drop(retired.picture);
    retiring_bytes.fetch_sub(bytes, Ordering::AcqRel);
    if retired.full {
        retiring_full.fetch_sub(1, Ordering::AcqRel);
    } else {
        retiring_regions.fetch_sub(1, Ordering::AcqRel);
        retiring_region_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }
    figures.retirement_pending.fetch_sub(1, Ordering::AcqRel);
    {
        let mut diagnostic = figures.diagnostics();
        diagnostic.retiring_bytes = diagnostic.retiring_bytes.saturating_sub(bytes);
        if failed {
            diagnostic.gpu_retirement_failures += 1;
        }
    }
    wake_surface();
}

fn wake_surface() {
    if let Some(waker) = RETIREMENT_WAKER
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|guard| guard.clone()))
    {
        waker();
    }
}

/// Ends one retirement, whether it `failed`: [`finish_retirement`] over the charges and figures of
/// the pipeline that retired it.
type FinishRetirement = Arc<dyn Fn(RetiredPicture, bool) + Send + Sync>;

fn retirement_worker(
    device: wgpu::Device,
    queue: wgpu::Queue,
    receiver: std::sync::mpsc::Receiver<RetiredPicture>,
    finish: FinishRetirement,
) {
    let mut pending: Vec<Arc<Mutex<Option<RetiredPicture>>>> = Vec::new();
    loop {
        // Sleep indefinitely when idle. While one or two resources retire, poll maintenance at
        // bounded intervals; wgpu invokes completion callbacks only during submit or poll.
        let incoming = if pending.is_empty() {
            match receiver.recv() {
                Ok(retired) => Some(retired),
                Err(_) => break,
            }
        } else {
            match receiver.recv_timeout(std::time::Duration::from_millis(2)) {
                Ok(retired) => Some(retired),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    None
                }
            }
        };
        if let Some(retired) = incoming {
            // `prepare` removed this picture before the current draw is encoded. A previous draw
            // was already submitted; this empty submit also flushes any pending write_texture
            // staging before the completion callback's fence is registered.
            queue.submit(None);
            let slot = Arc::new(Mutex::new(Some(retired)));
            let callback_slot = Arc::clone(&slot);
            let callback_finish = Arc::clone(&finish);
            queue.on_submitted_work_done(move || {
                if let Some(retired) = callback_slot.lock().expect("retirement slot lock").take() {
                    callback_finish(retired, false);
                }
            });
            pending.push(slot);
        }
        if !pending.is_empty() {
            if device.poll(wgpu::PollType::Poll).is_err() {
                for slot in &pending {
                    if let Some(retired) = slot.lock().expect("retirement slot lock").take() {
                        finish(retired, true);
                    }
                }
            }
            pending.retain(|slot| slot.lock().expect("retirement slot lock").is_some());
        }
    }
}

/// A sampler clamped to the edge, filtering with `filter` both ways, as the image widget's
/// `FilterMethod` of the same name does.
fn sampler(device: &wgpu::Device, label: &str, filter: wgpu::FilterMode) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(label),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        min_filter: filter,
        mag_filter: filter,
        mipmap_filter: filter,
        ..wgpu::SamplerDescriptor::default()
    })
}

impl shader::Pipeline for PhotoPipeline {
    /// The application's pipeline, which counts into the process-wide figures the desktop reads.
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        Self::with_figures(device, queue, format, Arc::clone(process_figures()))
    }
}

impl PhotoPipeline {
    /// A pipeline that counts its texture work into `figures`.
    fn with_figures(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        figures: Arc<SurfaceFigures>,
    ) -> Self {
        // The toolkit gamma corrects exactly when it chose an sRGB target, and stores image pixels
        // in an sRGB-typed texture when it does. Matching that is what makes a frame's byte land
        // on the surface as the image widget lands it.
        let texture_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let linear = sampler(
            device,
            "luxforge.photo_surface.sampler",
            wgpu::FilterMode::Linear,
        );
        let nearest = sampler(
            device,
            "luxforge.photo_surface.overlay_sampler",
            wgpu::FilterMode::Nearest,
        );
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("luxforge.photo_surface.layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // The fragment stage reads the bright rectangle and the dim opacity.
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("luxforge.photo_surface.shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "photo_surface.wgsl"
            ))),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("luxforge.photo_surface.pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("luxforge.photo_surface.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // The image pipeline's own blend state, so an overlay's translucent cells and a
                    // dimmed stage composite the same way. A photograph's raster is opaque, where
                    // this is a replacement.
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            // The pass a custom primitive draws into is the frame itself, which is not
            // multisampled; the toolkit resolves its own antialiased meshes separately.
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        let retiring_bytes = Arc::new(AtomicU64::new(0));
        let retiring_full = Arc::new(AtomicU64::new(0));
        let retiring_regions = Arc::new(AtomicU64::new(0));
        let retiring_region_bytes = Arc::new(AtomicU64::new(0));
        let (retirement_sender, retirement_receiver) = std::sync::mpsc::channel();
        let waiter_device = device.clone();
        let waiter_queue = queue.clone();
        let finish: FinishRetirement = {
            let figures = Arc::clone(&figures);
            let bytes = retiring_bytes.clone();
            let full = retiring_full.clone();
            let regions = retiring_regions.clone();
            let region_bytes = retiring_region_bytes.clone();
            Arc::new(move |retired, failed| {
                finish_retirement(
                    &figures,
                    retired,
                    &bytes,
                    &full,
                    &regions,
                    &region_bytes,
                    failed,
                );
            })
        };
        std::thread::spawn(move || {
            retirement_worker(waiter_device, waiter_queue, retirement_receiver, finish);
        });
        Self {
            pipeline,
            layout,
            linear,
            nearest,
            texture_format,
            slots: [None, None, None, None],
            regions: [None, None],
            region_front: 0,
            retiring_bytes,
            retiring_full,
            retiring_regions,
            retiring_region_bytes,
            retirement_sender,
            deferred_photo: false,
            deferred_region: false,
            figures,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(width: u32, height: u32, version: u64) -> Frame {
        let pixels = Arc::new(vec![0u8; (width * height * 4) as usize]);
        Frame::new(pixels, width, height, version).expect("a whole raster")
    }

    #[test]
    fn region_frames_validate_bounds_and_overlay_grids_can_be_smaller() {
        let region = RegionFrame::new(
            raster(20, 10, 7),
            [10, 5, 30, 15],
            (50, 30),
            (100, 60),
            0.5,
            RegionQuality::Interactive,
            4,
            9,
        )
        .expect("bounded region");
        assert_eq!(region.key().rect, [10, 5, 30, 15]);
        assert!(
            RegionFrame::new(
                raster(20, 10, 7),
                [10, 5, 31, 15],
                (50, 30),
                (100, 60),
                0.5,
                RegionQuality::Interactive,
                4,
                9,
            )
            .is_none()
        );
        assert!(
            RegionOverlay::new(
                raster(4, 2, 8),
                [20, 10, 60, 30],
                (100, 60),
                (100, 60),
                RegionQuality::Interactive,
                4,
                9,
            )
            .is_some()
        );
    }

    #[test]
    fn odd_sized_half_stage_places_regions_by_stage_ratio() {
        let key = RegionKey {
            rect: [250, 100, 501, 333],
            stage: (501, 333),
            full_stage: (1001, 665),
            quality: RegionQuality::Interactive,
            content_id: 3,
            generation: 8,
        };
        let tile = tile_layout((251, 233), 8192)[0];
        let placement = key.placement(tile, (251, 233));
        assert_eq!(placement[0], 250.0 / 501.0);
        assert_eq!(placement[1], 100.0 / 333.0);
        assert_eq!(placement[2..], [1.0, 1.0]);
        assert_ne!(placement[0], 500.0 / 1001.0);
        let overlay = RegionOverlay::new(
            raster(10, 10, 11),
            [500, 200, 1001, 665],
            (1001, 665),
            (1001, 665),
            RegionQuality::Interactive,
            3,
            8,
        )
        .expect("full-stage overlay grid");
        assert!(overlay.matches_region(key));
        let grid_tile = tile_layout((10, 10), 8192)[0];
        assert_eq!(
            overlay.key().placement(grid_tile, (10, 10)),
            [500.0 / 1001.0, 200.0 / 665.0, 1.0, 1.0],
        );
        let clip = region_physical_rect([0.0, 0.0, 1001.0, 665.0], key);
        assert_eq!(clip[0], 1001.0 * 250.0 / 501.0);
        assert_ne!(clip[0], 500.0);
    }

    #[test]
    fn padded_region_tiles_count_toward_admission() {
        assert!(region_texture_admissible((8198, 1023), 8192));
        assert!(!region_texture_admissible((8199, 1023), 8192));
        assert!(!region_texture_admissible((8192, 1025), 8192));
    }

    #[test]
    fn stale_content_is_hidden_and_exact_regions_cover_interactive_ones() {
        let key = |content_id, generation, quality| RegionKey {
            rect: [0, 0, 100, 80],
            stage: (100, 80),
            full_stage: (100, 80),
            quality,
            content_id,
            generation,
        };
        assert_eq!(
            region_key_order(
                [
                    Some(key(7, 10, RegionQuality::Interactive)),
                    Some(key(7, 9, RegionQuality::Exact)),
                ],
                7,
                (100, 80),
            ),
            vec![0, 1],
            "older exact pixels should override same-recipe coarse pixels"
        );
        assert_eq!(
            region_key_order(
                [
                    Some(key(6, 10, RegionQuality::Exact)),
                    Some(key(7, 9, RegionQuality::Interactive)),
                ],
                7,
                (100, 80),
            ),
            vec![1],
            "an earlier recipe cannot leak into uncovered pixels"
        );
        assert!(
            region_key_order([Some(key(6, 10, RegionQuality::Exact)), None], 7, (100, 80),)
                .is_empty()
        );
    }

    #[test]
    fn mixed_quality_overlap_suppresses_only_the_covered_region_overlay() {
        let full_stage = (1000, 800);
        let interactive = RegionKey {
            rect: [200, 50, 400, 300],
            stage: (500, 400),
            full_stage,
            quality: RegionQuality::Interactive,
            content_id: 7,
            generation: 11,
        };
        let exact = RegionKey {
            rect: [700, 100, 950, 600],
            stage: full_stage,
            full_stage,
            quality: RegionQuality::Exact,
            content_id: 7,
            generation: 10,
        };
        let overlay = |rect, quality, generation| {
            RegionOverlay::new(
                raster(10, 10, generation),
                rect,
                full_stage,
                full_stage,
                quality,
                7,
                generation,
            )
            .expect("region overlay")
        };
        let interactive_overlay = overlay([400, 100, 800, 600], RegionQuality::Interactive, 11);
        let exact_overlay = overlay(exact.rect, RegionQuality::Exact, 10);
        let visible = |overlay: &RegionOverlay, keys: [Option<RegionKey>; 2]| {
            let order = region_key_order(keys, 7, full_stage);
            overlay_matches_draw_order(overlay, keys, &order)
        };
        assert!(!visible(
            &interactive_overlay,
            [Some(interactive), Some(exact)]
        ));
        assert!(visible(&exact_overlay, [Some(interactive), Some(exact)]));

        let disjoint = RegionKey {
            rect: [850, 100, 950, 600],
            ..exact
        };
        assert!(visible(
            &interactive_overlay,
            [Some(interactive), Some(disjoint)]
        ));
        let same_quality = RegionKey {
            quality: RegionQuality::Interactive,
            ..exact
        };
        assert!(visible(
            &interactive_overlay,
            [Some(interactive), Some(same_quality)]
        ));
        assert!(!visible(&interactive_overlay, [Some(exact), None]));
    }

    #[test]
    fn later_same_quality_region_suppresses_an_older_overlapping_overlay() {
        let full_stage = (1000, 800);
        let older = RegionKey {
            rect: [100, 100, 400, 300],
            stage: (500, 400),
            full_stage,
            quality: RegionQuality::Interactive,
            content_id: 7,
            generation: 10,
        };
        let newer = RegionKey {
            rect: [350, 100, 650, 300],
            generation: 11,
            ..older
        };
        let older_overlay = RegionOverlay::new(
            raster(10, 10, 1),
            older.rect,
            older.stage,
            full_stage,
            older.quality,
            older.content_id,
            older.generation,
        )
        .expect("older clipping overlay");
        let keys = [Some(older), Some(newer)];
        let order = region_key_order(keys, 7, full_stage);
        assert_eq!(order, vec![0, 1]);
        assert!(regions_overlap(older_overlay.key(), newer));
        assert!(
            !overlay_matches_draw_order(&older_overlay, keys, &order),
            "the old grid must not cover newer pixels of the same quality"
        );

        let disjoint = RegionKey {
            rect: [700, 100, 950, 300],
            ..newer
        };
        let keys = [Some(older), Some(disjoint)];
        let order = region_key_order(keys, 7, full_stage);
        assert!(overlay_matches_draw_order(&older_overlay, keys, &order));
    }

    /// A frame is exactly its declared size, or it is not a frame at all.
    #[test]
    fn a_raster_is_refused_unless_the_buffer_matches_its_dimensions() {
        let pixels = Arc::new(vec![0u8; 16]);
        assert_eq!(
            raster(2, 2, 7).size(),
            (2, 2),
            "four RGBA pixels are a 2x2 raster"
        );
        assert_eq!(raster(2, 2, 7).version(), 7);
        for (width, height) in [(2, 3), (3, 2), (0, 2), (2, 0)] {
            assert!(
                Frame::new(pixels.clone(), width, height, 1).is_none(),
                "{width}x{height}"
            );
        }
        // Any buffer that holds the bytes will do, taken as it is: a painted overlay's own `Vec`.
        assert!(Frame::new(Arc::new(vec![0u8; 16]), 2, 2, 1).is_some());
    }

    /// Contain is the toolkit's own rule: the largest rectangle of the raster's ratio that fits,
    /// centred in the bounds. This is the Fit view, and it is where the thirds overlay and the
    /// pointer mapping in `view::canvas` expect the photograph to be.
    #[test]
    fn contain_centres_the_largest_fitting_rectangle() {
        let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(400.0, 400.0));
        let rect = placement_rect(Placement::Contain, (200, 100), bounds).expect("a rectangle");
        assert_eq!((rect.width, rect.height), (400.0, 200.0));
        assert_eq!((rect.x, rect.y), (10.0, 120.0));
        // A taller-than-wide raster is bounded by the width instead, and still centred.
        let rect = placement_rect(Placement::Contain, (100, 400), bounds).expect("a rectangle");
        assert_eq!((rect.width, rect.height), (100.0, 400.0));
        assert_eq!((rect.x, rect.y), (160.0, 20.0));
        // The raster's own ratio decides, never the widget's: a proxy of the same picture lands in
        // the same rectangle as the exact render, to within its own rounding.
        let exact = placement_rect(Placement::Contain, (6000, 4000), bounds).expect("a rectangle");
        let proxy = placement_rect(Placement::Contain, (1200, 800), bounds).expect("a rectangle");
        assert_eq!((exact.x, exact.y), (proxy.x, proxy.y));
        assert_eq!((exact.width, exact.height), (proxy.width, proxy.height));
    }

    /// Fill takes the whole widget, whatever the raster's size: at a percentage the widget is
    /// already the exact stage's displayed box, and the texture in it may be a smaller proxy.
    #[test]
    fn fill_takes_the_whole_widget_whatever_the_raster_measures() {
        let bounds = Rectangle::new(Point::new(5.0, 7.0), Size::new(300.0, 200.0));
        for size in [(6000, 4000), (1200, 800), (10, 10000)] {
            let rect = placement_rect(Placement::Fill, size, bounds).expect("a rectangle");
            assert_eq!((rect.x, rect.y), (bounds.x, bounds.y), "{size:?}");
            assert_eq!(
                (rect.width, rect.height),
                (bounds.width, bounds.height),
                "{size:?}"
            );
        }
    }

    /// Nothing is placed where there is no room, and nothing is placed for a raster with no pixels.
    #[test]
    fn a_surface_with_no_room_places_nothing() {
        let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(400.0, 400.0));
        for empty in [
            Rectangle::new(Point::new(0.0, 0.0), Size::new(0.0, 400.0)),
            Rectangle::new(Point::new(0.0, 0.0), Size::new(400.0, 0.0)),
            Rectangle::new(Point::new(f32::NAN, 0.0), Size::new(400.0, 400.0)),
        ] {
            assert!(placement_rect(Placement::Contain, (200, 100), empty).is_none());
        }
        assert!(placement_rect(Placement::Contain, (0, 100), bounds).is_none());
    }

    /// The uniform block is the six rectangles the shader reads, in order, as little-endian floats:
    /// the viewport, the destination, the tile's region, its texels, the bright rectangle and the
    /// turn, twenty-four floats in ninety-six bytes, which is what each tile's buffer is sized for.
    #[test]
    fn the_uniform_block_is_the_viewport_destination_region_texels_bright_then_turn() {
        let rectangles: [[f32; 4]; 6] =
            std::array::from_fn(|row| std::array::from_fn(|column| (row * 4 + column + 1) as f32));
        let bytes = uniform_bytes(rectangles);
        assert_eq!(bytes.len(), UNIFORM_SIZE);
        let read = |index: usize| {
            f32::from_le_bytes(
                bytes[index * 4..index * 4 + 4]
                    .try_into()
                    .expect("four bytes"),
            )
        };
        for index in 0..24 {
            assert_eq!(read(index), index as f32 + 1.0);
        }
    }

    /// The device limit Iced asks wgpu for: its default, whatever the GPU could do.
    const LIMIT: u32 = 8192;

    /// A raster within the limit — every display proxy, and every exact render up to 8192 pixels a
    /// side — is one tile holding all of it, drawing all of it, with the whole texture as its
    /// texels: the uniform is exactly the single texture's, so nothing about the Fit view or a
    /// 24 MP exact render changes.
    #[test]
    fn a_raster_within_the_limit_is_one_texture() {
        for size in [
            (1, 1),
            (1716, 1144),
            (6000, 4000),
            (8192, 5461),
            (4000, 8192),
        ] {
            let tiles = tile_layout(size, LIMIT);
            assert_eq!(
                tiles,
                vec![TileLayout {
                    content: [0, 0, size.0, size.1],
                    texels: [0, 0, size.0, size.1],
                }],
                "{size:?}"
            );
            let (region, texels) = tiles[0].placement(size, tiles[0].texture_size());
            assert_eq!(region, [0.0, 0.0, 1.0, 1.0], "{size:?}");
            assert_eq!(texels, [0.0, 0.0, 1.0, 1.0], "{size:?}");
            let copy = tiles[0].copy_layout(size.0);
            assert_eq!(copy.offset, 0);
            assert_eq!(copy.bytes_per_row, Some(size.0 * 4));
            assert_eq!(copy.rows_per_image, Some(size.1));
        }
    }

    /// Larger rasters are cut into a grid whose textures all fit the limit, whose contents
    /// partition the raster exactly, and whose textures overlap each neighbour by the one pixel the
    /// linear filter reads across a seam.
    #[test]
    fn a_large_raster_is_cut_into_tiles_that_fit_and_partition_it() {
        for (size, limit, grid) in [
            ((10000, 6000), LIMIT, (2, 1)),
            ((9504, 6336), LIMIT, (2, 1)),
            ((11648, 8736), LIMIT, (2, 2)),
            ((8193, 8193), LIMIT, (2, 2)),
            ((250, 99), 100, (3, 1)),
            ((7, 7), 3, (7, 7)),
        ] {
            let tiles = tile_layout(size, limit);
            assert_eq!(tiles.len(), grid.0 * grid.1, "{size:?} at {limit}");
            let mut covered = vec![0u8; (size.0 * size.1) as usize];
            for tile in &tiles {
                let (width, height) = tile.texture_size();
                assert!(width <= limit && height <= limit, "{tile:?} at {limit}");
                let [x0, y0, x1, y1] = tile.content;
                let [tx0, ty0, tx1, ty1] = tile.texels;
                // The texels are the content plus one pixel wherever there is a neighbour, and
                // never beyond the raster.
                assert_eq!(tx0, x0.saturating_sub(1), "{tile:?}");
                assert_eq!(ty0, y0.saturating_sub(1), "{tile:?}");
                assert_eq!(tx1, (x1 + 1).min(size.0), "{tile:?}");
                assert_eq!(ty1, (y1 + 1).min(size.1), "{tile:?}");
                for y in y0..y1 {
                    for x in x0..x1 {
                        covered[(y * size.0 + x) as usize] += 1;
                    }
                }
                // The copy reads the tile's own first pixel out of the shared raster.
                let copy = tile.copy_layout(size.0);
                assert_eq!(
                    copy.offset,
                    (u64::from(ty0) * u64::from(size.0) + u64::from(tx0)) * 4
                );
                assert_eq!(copy.bytes_per_row, Some(size.0 * 4));
                assert_eq!(copy.rows_per_image, Some(height));
            }
            assert!(
                covered.iter().all(|count| *count == 1),
                "{size:?} at {limit}: every pixel is drawn by exactly one tile"
            );
        }
    }

    /// Neighbouring tiles cut their quads from the same destination with the same fraction, so
    /// they share their edge exactly; and each maps the screen to the raster exactly as one texture
    /// would, including the apron pixel it holds beyond the edge.
    #[test]
    fn tiles_meet_exactly_and_sample_the_raster_as_one_texture_would() {
        let size = (10000, 6000);
        let tiles = tile_layout(size, LIMIT);
        let (left, right) = (tiles[0], tiles[1]);
        assert_eq!(left.content, [0, 0, 5000, 6000]);
        assert_eq!(right.content, [5000, 0, 10000, 6000]);
        assert_eq!(left.texels, [0, 0, 5001, 6000]);
        assert_eq!(right.texels, [4999, 0, 10000, 6000]);
        let (left_region, left_texels) = left.placement(size, left.texture_size());
        let (right_region, right_texels) = right.placement(size, right.texture_size());
        assert_eq!(left_region[2], right_region[0], "one shared edge");
        assert_eq!(left_region, [0.0, 0.0, 0.5, 1.0]);
        // Texture coordinates of the content, in each tile's own texture.
        assert_eq!(left_texels[2] * 5001.0, 5000.0);
        assert_eq!(right_texels[0] * 5001.0, 1.0);
        // What the vertex shader does, on the CPU: a screen x inside a tile's quad maps to the same
        // raster x through either the tile or the whole picture.
        let destination = [-2400.0f32, 0.0, 160000.0, 96000.0];
        let (top_left, bottom_right) = (
            destination[0].round_ties_even(),
            (destination[0] + destination[2]).round_ties_even(),
        );
        let mix = |a: f32, b: f32, t: f32| a * (1.0 - t) + b * t;
        for (tile, region, texels) in [
            (left, left_region, left_texels),
            (right, right_region, right_texels),
        ] {
            let (start, end) = (
                mix(top_left, bottom_right, region[0]),
                mix(top_left, bottom_right, region[2]),
            );
            for fraction in [0.0f32, 0.25, 0.5, 0.999] {
                let screen = mix(start, end, fraction);
                let u = mix(texels[0], texels[2], fraction);
                let through_tile = tile.texels[0] as f32 + u * tile.texture_size().0 as f32;
                let whole = (screen - top_left) / (bottom_right - top_left) * size.0 as f32;
                assert!(
                    (through_tile - whole).abs() < 1e-2,
                    "{tile:?} at {fraction}: {through_tile} against {whole}"
                );
            }
        }
    }

    /// `rect` moved by `by`, which is what the renderer does to a primitive's rectangle under a
    /// scrollable's translation.
    fn translated(rect: Rectangle, by: Vector) -> Rectangle {
        Rectangle::new(rect.position() + by, rect.size())
    }

    /// The physical corners the vertex shader snaps a destination to: WGSL's `round`, which breaks
    /// ties to even.
    fn snapped(rect: [f32; 4]) -> [f32; 4] {
        [
            rect[0].round_ties_even(),
            rect[1].round_ties_even(),
            (rect[0] + rect[2]).round_ties_even(),
            (rect[1] + rect[3]).round_ties_even(),
        ]
    }

    /// What drawing the whole widget produced: the render pass's viewport at its full bounds and
    /// the picture placed inside them, in physical pixels.
    fn whole_widget(
        placement: Placement,
        raster: (u32, u32),
        bounds: Rectangle,
        scale: f32,
    ) -> ([f32; 4], [f32; 4]) {
        let destination = placement_rect(placement, raster, bounds).expect("a rectangle");
        (
            [
                bounds.x * scale,
                bounds.y * scale,
                bounds.width * scale,
                bounds.height * scale,
            ],
            [
                destination.x * scale,
                destination.y * scale,
                destination.width * scale,
                destination.height * scale,
            ],
        )
    }

    fn close(actual: [f32; 4], expected: [f32; 4], tolerance: f32) -> bool {
        actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| (actual - expected).abs() <= tolerance)
    }

    /// At Fit, and at any zoom whose box fits the window, the whole widget is visible: it is drawn
    /// with its own bounds as before, and the picture lands on exactly the physical pixels it
    /// landed on when the whole widget was handed over. The only difference allowed is float
    /// rounding in the last bit, which never moves a snapped corner.
    #[test]
    fn a_fully_visible_widget_draws_where_the_whole_widget_did() {
        let window = Rectangle::new(Point::ORIGIN, Size::new(1440.0, 900.0));
        for (placement, bounds) in [
            (
                Placement::Contain,
                Rectangle::new(Point::new(264.0, 64.0), Size::new(892.5, 771.25)),
            ),
            (
                Placement::Contain,
                Rectangle::new(Point::new(20.0, 20.0), Size::new(1400.0, 860.0)),
            ),
            (
                Placement::Fill,
                Rectangle::new(Point::new(333.5, 71.0), Size::new(240.0, 160.0)),
            ),
        ] {
            for raster in [(6000, 4000), (1813, 1209), (480, 320), (4000, 6000)] {
                for scale in [1.0, 1.5, 2.0] {
                    let visible = visible_placement(Base::Photo(placement), raster, bounds, window)
                        .expect("a visible widget");
                    assert_eq!(visible.clip, bounds, "the whole widget is on screen");
                    let (viewport, destination) =
                        physical_rects(visible.clip, visible.offset, visible.size, scale);
                    let (whole_viewport, whole_destination) =
                        whole_widget(placement, raster, bounds, scale);
                    let case = format!("{placement:?} {raster:?} at {scale}x in {bounds:?}");
                    assert_eq!(viewport, whole_viewport, "{case}");
                    assert!(
                        close(destination, whole_destination, 1e-3),
                        "{case}: {destination:?} against {whole_destination:?}"
                    );
                    assert_eq!(snapped(destination), snapped(whole_destination), "{case}");
                }
            }
        }
    }

    /// A widget only partly on screen — the viewport it is drawn in covers some of it — is drawn
    /// with that part alone, and the picture keeps the place the whole widget gives it.
    #[test]
    fn a_partly_covered_widget_places_the_picture_as_the_whole_widget_does() {
        let bounds = Rectangle::new(Point::new(20.0, 20.0), Size::new(1400.0, 860.0));
        let viewport = Rectangle::new(Point::ORIGIN, Size::new(700.0, 450.0));
        let visible = visible_placement(
            Base::Photo(Placement::Contain),
            (6000, 4000),
            bounds,
            viewport,
        )
        .expect("visible");
        assert_eq!(
            visible.clip,
            Rectangle::new(Point::new(20.0, 20.0), Size::new(680.0, 430.0))
        );
        let (viewport, destination) =
            physical_rects(visible.clip, visible.offset, visible.size, 2.0);
        assert_eq!(viewport, [40.0, 40.0, 1360.0, 860.0]);
        let (_, whole) = whole_widget(Placement::Contain, (6000, 4000), bounds, 2.0);
        assert!(close(destination, whole, 1e-3), "{destination:?}");
        assert_eq!(snapped(destination), snapped(whole));
    }

    /// Inside the percent-zoom scrollable the widget is the whole displayed box, and its origin is
    /// far above and left of the screen once scrolled. The scrollable hands its content the visible
    /// region shifted by the scroll offset and draws it translated back, so the primitive is drawn
    /// with the region itself, and the picture lands exactly where the whole translated box would
    /// have put it: the scroll offset, in physical pixels, at the region's corner.
    #[test]
    fn a_scrolled_widget_is_drawn_through_its_visible_part_at_the_same_place() {
        // A 6000 x 4000 exact render at 100% on a 2x display: a 3000 x 2000 logical box inside a
        // scrollable showing 960 x 820 at (300, 40).
        let scale = 2.0;
        let raster = (6000, 4000);
        let region = Rectangle::new(Point::new(300.0, 40.0), Size::new(960.0, 820.0));
        let bounds = Rectangle::new(region.position(), Size::new(3000.0, 2000.0));
        for scroll in [
            Vector::new(0.0, 0.0),
            Vector::new(1500.0, 900.0),
            Vector::new(2040.0, 1180.0),
            Vector::new(733.25, 17.5),
        ] {
            let visible = visible_placement(
                Base::Photo(Placement::Fill),
                raster,
                bounds,
                region + scroll,
            )
            .expect("a visible widget");
            assert_eq!(visible.clip, region + scroll, "{scroll:?}: all photograph");
            assert_eq!(visible.offset, -scroll, "{scroll:?}");
            let drawn = translated(visible.clip, -scroll);
            assert_eq!(drawn, region, "{scroll:?}");
            let (viewport, destination) =
                physical_rects(drawn, visible.offset, visible.size, scale);
            assert_eq!(viewport, [600.0, 80.0, 1920.0, 1640.0], "{scroll:?}");
            let (_, whole) =
                whole_widget(Placement::Fill, raster, translated(bounds, -scroll), scale);
            assert_eq!(destination, whole, "{scroll:?}");
            // The region's first physical pixel shows the texel the scroll offset names: one
            // texel per physical pixel at 100%.
            let texel = (
                (viewport[0] - destination[0]) / destination[2] * raster.0 as f32,
                (viewport[1] - destination[1]) / destination[3] * raster.1 as f32,
            );
            assert_eq!(texel, (scroll.x * scale, scroll.y * scale), "{scroll:?}");
        }
    }

    /// The case that motivated drawing the visible part alone: a 9504 x 6336 photograph at 1600%
    /// on a 2x display is a box of 152064 x 101376 physical pixels, far beyond the 8192 a side the
    /// device allows a viewport and beyond the twice-that range its position may take. Wherever it
    /// is scrolled, the viewport handed to wgpu is the photo region of the window, and the picture
    /// still maps the scrolled texel to the region's corner.
    #[test]
    fn a_60_mp_photograph_at_1600_percent_keeps_the_gpu_viewport_inside_the_window() {
        let limit = LIMIT as f32;
        let scale = 2.0;
        let raster = (9504, 6336);
        let zoom = 16.0;
        let window = Size::new(1440.0 * scale, 900.0 * scale);
        let region = Rectangle::new(Point::new(252.0, 44.0), Size::new(936.0, 812.0));
        let size = Size::new(
            raster.0 as f32 * zoom / scale,
            raster.1 as f32 * zoom / scale,
        );
        let bounds = Rectangle::new(region.position(), size);
        // What the toolkit's shader widget would have handed over: every one of these fails
        // wgpu's viewport validation.
        assert!(bounds.width * scale > limit && bounds.height * scale > limit);
        let far = Vector::new(size.width - region.width, size.height - region.height);
        assert!((region.x - far.x) * scale < -2.0 * limit);
        for scroll in [Vector::new(0.0, 0.0), far * 0.5, far] {
            let visible = visible_placement(
                Base::Photo(Placement::Fill),
                raster,
                bounds,
                region + scroll,
            )
            .expect("a visible widget");
            let drawn = translated(visible.clip, -scroll);
            let (viewport, destination) =
                physical_rects(drawn, visible.offset, visible.size, scale);
            let case = format!("scrolled {scroll:?}: viewport {viewport:?}");
            // Within the window, so within every limit wgpu checks: its size, and its position
            // within twice the texture limit of the origin.
            assert!(
                viewport[0] >= 0.0
                    && viewport[1] >= 0.0
                    && viewport[0] + viewport[2] <= window.width
                    && viewport[1] + viewport[3] <= window.height,
                "{case}"
            );
            assert!(viewport[2] <= limit && viewport[3] <= limit, "{case}");
            assert_eq!(drawn, region, "{case}: the region is all photograph");
            // The picture is the whole zoomed extent, placed by the scroll offset.
            assert_eq!(
                [destination[2], destination[3]],
                [raster.0 as f32 * zoom, raster.1 as f32 * zoom]
            );
            let expected = [(region.x - scroll.x) * scale, (region.y - scroll.y) * scale];
            // Float rounding at these magnitudes is a few hundredths of a physical pixel.
            assert!(
                (destination[0] - expected[0]).abs() < 0.05
                    && (destination[1] - expected[1]).abs() < 0.05,
                "{case}: {destination:?} against {expected:?}"
            );
            // The texel at the region's centre is the one the scroll puts there, to within a
            // hundredth of a texel.
            let centre = (
                viewport[0] + viewport[2] / 2.0,
                viewport[1] + viewport[3] / 2.0,
            );
            let texel = (
                (centre.0 - destination[0]) / destination[2] * raster.0 as f32,
                (centre.1 - destination[1]) / destination[3] * raster.1 as f32,
            );
            let wanted = (
                (scroll.x + region.width / 2.0) * scale / zoom,
                (scroll.y + region.height / 2.0) * scale / zoom,
            );
            assert!(
                (texel.0 - wanted.0).abs() < 0.01 && (texel.1 - wanted.1).abs() < 0.01,
                "{case}: texel {texel:?}, expected {wanted:?}"
            );
        }
    }

    /// Nothing is drawn for a widget scrolled wholly out of view, or for a raster with no pixels.
    #[test]
    fn a_widget_out_of_view_draws_nothing() {
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(3000.0, 2000.0));
        for viewport in [
            Rectangle::new(Point::new(3000.0, 0.0), Size::new(900.0, 800.0)),
            Rectangle::new(Point::new(-900.0, 0.0), Size::new(900.0, 800.0)),
            Rectangle::new(Point::new(0.0, 2000.0), Size::new(900.0, 800.0)),
        ] {
            assert!(
                visible_placement(Base::Photo(Placement::Fill), (6000, 4000), bounds, viewport)
                    .is_none(),
                "{viewport:?}"
            );
        }
        assert!(
            visible_placement(Base::Photo(Placement::Fill), (0, 4000), bounds, bounds).is_none()
        );
    }

    /// What the vertex shader does to one corner of one tile, on the CPU: the destination's corners
    /// snapped when asked, the tile's part of it, then, for a turned stage, the corner turned about
    /// the destination's centre.
    fn vertex(
        destination: [f32; 4],
        region: [f32; 4],
        turn: [f32; 4],
        corner: (f32, f32),
    ) -> (f32, f32) {
        let snap = |value: f32| {
            if turn[3] > 0.5 {
                value.round_ties_even()
            } else {
                value
            }
        };
        let (left, top) = (snap(destination[0]), snap(destination[1]));
        let (right, bottom) = (
            snap(destination[0] + destination[2]),
            snap(destination[1] + destination[3]),
        );
        let mix = |a: f32, b: f32, t: f32| a * (1.0 - t) + b * t;
        let start = (mix(left, right, region[0]), mix(top, bottom, region[1]));
        let end = (mix(left, right, region[2]), mix(top, bottom, region[3]));
        let position = (mix(start.0, end.0, corner.0), mix(start.1, end.1, corner.1));
        if turn[0] == 0.0 {
            return position;
        }
        let centre = ((left + right) * 0.5, (top + bottom) * 0.5);
        let (dx, dy) = (position.0 - centre.0, position.1 - centre.1);
        (
            centre.0 + dx * turn[1] - dy * turn[0],
            centre.1 + dx * turn[0] + dy * turn[1],
        )
    }

    /// The photograph is never turned: a zero angle is a zero sine, which the shader takes as no
    /// rotation at all, so its snapped corners are exactly the ones it had before the stage shared
    /// its shader.
    #[test]
    fn a_zero_angle_leaves_the_photograph_where_it_was_snapped() {
        assert_eq!(turn_uniform(0.0, 1.0, true), [0.0, 1.0, 1.0, 1.0]);
        assert_eq!(turn_uniform(0.0, 0.35, false), [0.0, 1.0, 0.35, 0.0]);
        let destination = [527.5, 128.25, 1784.5, 1189.75];
        let turn = turn_uniform(0.0, 1.0, true);
        let whole = [0.0, 0.0, 1.0, 1.0];
        assert_eq!(vertex(destination, whole, turn, (0.0, 0.0)), (528.0, 128.0));
        assert_eq!(
            vertex(destination, whole, turn, (1.0, 1.0)),
            (2312.0, 1318.0)
        );
    }

    /// A turned stage is one rigid rotation, whatever tiles hold it: every tile's corner is the
    /// whole picture's rotation of where it lay, so neighbouring tiles still share their edge at
    /// every angle and the picture's own corners land where the crop contract's matrix puts them.
    #[test]
    fn a_turned_stage_turns_every_tile_as_one_picture() {
        let size = (10000, 6000);
        let tiles = tile_layout(size, LIMIT);
        assert_eq!(tiles.len(), 2);
        let destination = [140.0, 90.5, 1250.0, 750.0];
        let centre = (
            destination[0] + destination[2] / 2.0,
            destination[1] + destination[3] / 2.0,
        );
        let (left, _) = tiles[0].placement(size, tiles[0].texture_size());
        let (right, _) = tiles[1].placement(size, tiles[1].texture_size());
        for degrees in [7.0_f32, -12.5, 44.0] {
            let angle = degrees.to_radians();
            let turn = turn_uniform(angle, 0.35, false);
            let (sin, cos) = angle.sin_cos();
            let rotate = |x: f32, y: f32| {
                let (dx, dy) = (x - centre.0, y - centre.1);
                (
                    centre.0 + dx * cos - dy * sin,
                    centre.1 + dx * sin + dy * cos,
                )
            };
            let near =
                |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
            for v in [0.0, 1.0] {
                let (a, b) = (
                    vertex(destination, left, turn, (1.0, v)),
                    vertex(destination, right, turn, (0.0, v)),
                );
                assert!(
                    near(a, b),
                    "{degrees}°: the seam split into {a:?} and {b:?}"
                );
            }
            for (region, corner, expected) in [
                (left, (0.0, 0.0), rotate(destination[0], destination[1])),
                (
                    right,
                    (1.0, 1.0),
                    rotate(
                        destination[0] + destination[2],
                        destination[1] + destination[3],
                    ),
                ),
            ] {
                let actual = vertex(destination, region, turn, corner);
                assert!(
                    near(actual, expected),
                    "{degrees}°: {actual:?} against {expected:?}"
                );
            }
        }
    }

    /// The stage is placed at its own rectangle inside the widget, and its bright part keeps its
    /// place relative to the visible part the primitive is drawn with, wherever that part begins.
    #[test]
    fn a_turned_stage_is_placed_at_its_rectangle_with_its_bright_part() {
        let turn = Turn {
            rect: Rectangle::new(Point::new(10.0, 20.0), Size::new(300.0, 200.0)),
            angle: 0.1,
            bright: Rectangle::new(Point::new(50.0, 60.0), Size::new(100.25, 80.0)),
            dim: 0.35,
        };
        let bounds = Rectangle::new(Point::new(40.0, 30.0), Size::new(800.0, 600.0));
        for viewport in [
            Rectangle::new(Point::ORIGIN, Size::new(1440.0, 900.0)),
            Rectangle::new(Point::new(100.0, 90.0), Size::new(300.0, 300.0)),
        ] {
            let visible = visible_placement(Base::Stage(turn), (4800, 3200), bounds, viewport)
                .expect("visible");
            let origin = visible.clip.position();
            assert_eq!(visible.offset, Point::new(50.0, 50.0) - origin);
            assert_eq!(visible.size, Size::new(300.0, 200.0));
            let (bright, dim) = visible.bright.expect("a bright part");
            assert_eq!(dim, 0.35);
            assert_eq!(
                (origin.x + bright.x, origin.y + bright.y),
                (90.0, 90.0),
                "{viewport:?}"
            );
            assert_eq!(bright.size(), turn.bright.size());
            // In physical pixels its corners are rounded as the toolkit rounds a scissor.
            assert_eq!(
                physical_bright(visible.clip, visible.bright, 2.0),
                [180.0, 180.0, 381.0, 340.0, 0.35]
            );
        }
        // The photograph has no bright part: everything is drawn at full opacity.
        let [.., x1, y1, dim] = physical_bright(bounds, None, 2.0);
        assert_eq!((x1, y1, dim), (f32::MAX, f32::MAX, 1.0));
        // A stage with no area is not drawn.
        let empty = Turn {
            rect: Rectangle::new(Point::ORIGIN, Size::new(0.0, 200.0)),
            ..turn
        };
        assert!(visible_placement(Base::Stage(empty), (4800, 3200), bounds, bounds).is_none());
    }
}

/// Headless GPU regression coverage for viewport photo residency and retirement.
/// A missing adapter is printed as a skip and must not be reported as native GPU evidence.
///
/// Each test builds its own pipeline with figures of its own ([`own_pipeline`]) and reads only those,
/// so the tests run in parallel and none waits for another's retirements.
#[cfg(test)]
mod gpu_surface_tests {
    use super::*;
    use iced::widget::shader::Primitive as _;
    use luxforge_testbase::{wait_for, wait_until};
    use std::time::{Duration, Instant};

    /// A pipeline counting into figures of its own, never the process-wide ones.
    fn own_pipeline(device: &wgpu::Device, queue: &wgpu::Queue) -> PhotoPipeline {
        PhotoPipeline::with_figures(
            device,
            queue,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            Arc::default(),
        )
    }

    /// What `pipeline` has counted.
    fn diagnostics(pipeline: &PhotoPipeline) -> SurfaceDiagnostics {
        *pipeline.figures.diagnostics()
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        wait_for("the GPU request", || {
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(value) => Some(value),
                std::task::Poll::Pending => None,
            }
        })
    }

    fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter =
            block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        eprintln!("adapter: {:?}", adapter.get_info());
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    fn headless_with_texture_limit(limit: u32) -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter =
            block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        eprintln!("adapter: {:?}", adapter.get_info());
        let mut descriptor = wgpu::DeviceDescriptor::default();
        descriptor.required_limits.max_texture_dimension_2d = limit;
        block_on(adapter.request_device(&descriptor)).ok()
    }

    fn raster(width: u32, height: u32, version: u64) -> Frame {
        let pixels = Arc::new(vec![0u8; (width * height * 4) as usize]);
        Frame::new(pixels, width, height, version).expect("a whole raster")
    }

    fn solid_raster(width: u32, height: u32, version: u64, rgba: [u8; 4]) -> Frame {
        let pixels = Arc::new(
            vec![rgba; (width * height) as usize]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
        );
        Frame::new(pixels, width, height, version).expect("an opaque raster")
    }

    fn striped_raster(width: u32, height: u32, version: u64, colours: [[u8; 4]; 2]) -> Frame {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&colours[usize::from(x >= width / 2)]);
            }
        }
        Frame::new(Arc::new(pixels), width, height, version).expect("striped raster")
    }

    fn full_primitive(frame: Frame) -> PhotoPrimitive {
        PhotoPrimitive {
            layers: vec![(Layer::Photo, frame)],
            viewport: None,
            region_overlays: [None, None],
            offset: Vector::new(0.0, 0.0),
            size: Size::new(64.0, 64.0),
            clip_size: Size::new(64.0, 64.0),
            bright: None,
            angle: 0.0,
            snap: true,
        }
    }

    fn viewport_primitive(
        full: Option<(Frame, u64)>,
        region: Option<RegionFrame>,
        current_content: u64,
        full_stage: (u32, u32),
    ) -> PhotoPrimitive {
        PhotoPrimitive {
            layers: Vec::new(),
            viewport: Some(ViewportFrames {
                full,
                region,
                current_content,
                full_stage,
            }),
            region_overlays: [None, None],
            offset: Vector::new(0.0, 0.0),
            size: Size::new(64.0, 64.0),
            clip_size: Size::new(64.0, 64.0),
            bright: None,
            angle: 0.0,
            snap: true,
        }
    }

    /// Encode the real prepare and draw calls and read the 64×64 BGRA output. A distinct clear
    /// colour makes a blank or partially uncovered photograph visible to the assertion.
    fn paint(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &mut PhotoPipeline,
        primitive: &PhotoPrimitive,
    ) -> Vec<u8> {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 64.0,
            height: 64.0,
        };
        let viewport = Viewport::with_physical_size(Size::new(64, 64), 1.0);
        primitive.prepare(pipeline, device, queue, &bounds, &viewport);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("photo surface readback target"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("photo surface readback"),
            size: 64 * 64 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("photo surface readback pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 1.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            assert!(primitive.draw(pipeline, &mut pass));
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(64 * 4),
                    rows_per_image: Some(64),
                },
            },
            wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
        );
        let index = queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).expect("readback receiver")
            });
        wait(device, index);
        receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("readback callback")
            .expect("readback mapping");
        let bytes = readback.slice(..).get_mapped_range().to_vec();
        readback.unmap();
        bytes
    }

    fn assert_solid_bgra(bytes: &[u8], expected: [u8; 4]) {
        for (index, pixel) in bytes.chunks_exact(4).enumerate() {
            assert_eq!(pixel, expected, "output pixel {index}");
        }
    }

    /// Wait for `pipeline`'s retirements, which its worker finishes when the GPU is done with them.
    fn settle(pipeline: &PhotoPipeline) {
        wait_until("the pipeline's retirements", || {
            pipeline.retiring_regions.load(Ordering::Acquire) == 0
                && pipeline.retiring_full.load(Ordering::Acquire) == 0
        });
    }

    #[test]
    fn bucketed_full_upload_prepare_and_draw_use_only_current_texels() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline = own_pipeline(&device, &queue);
        let blanks_before = diagnostics(&pipeline).blank_photo_draws;
        let first = full_primitive(solid_raster(20, 12, 1, [0, 255, 0, 255]));
        assert_solid_bgra(
            &paint(&device, &queue, &mut pipeline, &first),
            [0, 255, 0, 255],
        );
        let second = full_primitive(solid_raster(12, 20, 2, [255, 0, 0, 255]));
        assert_solid_bgra(
            &paint(&device, &queue, &mut pipeline, &second),
            [0, 0, 255, 255],
        );
        let third = full_primitive(solid_raster(9, 7, 3, [255, 255, 0, 255]));
        assert_solid_bgra(
            &paint(&device, &queue, &mut pipeline, &third),
            [0, 255, 255, 255],
        );
        assert_eq!(pipeline.retiring_full.load(Ordering::Acquire), 0);
        assert_eq!(diagnostics(&pipeline).blank_photo_draws, blanks_before);
    }

    #[test]
    fn bucketed_tiled_shrink_matches_a_fresh_tiled_draw_at_seams_and_edges() {
        let Some((device, queue)) = headless_with_texture_limit(64) else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut reused = own_pipeline(&device, &queue);
        let old = full_primitive(striped_raster(
            96,
            48,
            1,
            [[255, 0, 0, 255], [0, 255, 0, 255]],
        ));
        paint(&device, &queue, &mut reused, &old);
        let new_frame = striped_raster(80, 40, 2, [[0, 0, 255, 255], [255, 255, 0, 255]]);
        let new = full_primitive(new_frame.clone());
        let actual = paint(&device, &queue, &mut reused, &new);
        assert_eq!(reused.retiring_full.load(Ordering::Acquire), 0);
        let mut fresh = own_pipeline(&device, &queue);
        let reference = paint(&device, &queue, &mut fresh, &full_primitive(new_frame));
        assert_eq!(
            actual, reference,
            "reused tile edge or seam sampled stale texels"
        );
        assert_ne!(actual[0..4], [0, 0, 255, 255], "the old edge was sampled");
    }

    #[test]
    fn bucketed_region_upload_prepare_and_draw_use_only_current_texels() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline = own_pipeline(&device, &queue);
        let blanks_before = diagnostics(&pipeline).blank_photo_draws;
        for (width, height, version, colour, bgra) in [
            (16, 16, 1, [0, 255, 0, 255], [0, 255, 0, 255]),
            (12, 12, 2, [255, 0, 0, 255], [0, 0, 255, 255]),
            (8, 8, 3, [255, 255, 0, 255], [0, 255, 255, 255]),
        ] {
            let region = RegionFrame::new(
                solid_raster(width, height, version, colour),
                [0, 0, width, height],
                (width, height),
                (width, height),
                1.0,
                RegionQuality::Exact,
                version,
                version,
            )
            .expect("whole-region raster");
            let primitive = viewport_primitive(None, Some(region), version, (width, height));
            assert_solid_bgra(&paint(&device, &queue, &mut pipeline, &primitive), bgra);
        }
        assert_eq!(pipeline.retiring_regions.load(Ordering::Acquire), 0);
        assert_eq!(diagnostics(&pipeline).blank_photo_draws, blanks_before);
    }

    /// A crop stage's texture is exactly its frame's size: the exact stage a percentage zoom shows
    /// is not kept to hold the display-size proxy a zoom back to Fit shows, and the resident bytes
    /// say so.
    #[test]
    fn a_crop_stage_texture_is_its_frames_size_and_never_kept_larger() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline = own_pipeline(&device, &queue);
        assert!(pipeline.write(
            &device,
            &queue,
            Layer::Stage,
            &raster(120, 80, 1),
            None,
            None
        ));
        pipeline.publish_diagnostics();
        assert_eq!(diagnostics(&pipeline).stage_resident_bytes, 120 * 80 * 4);
        assert!(pipeline.write(
            &device,
            &queue,
            Layer::Stage,
            &raster(30, 20, 2),
            None,
            None
        ));
        pipeline.publish_diagnostics();
        assert_eq!(diagnostics(&pipeline).stage_resident_bytes, 30 * 20 * 4);
        assert_eq!(
            pipeline.slots[Layer::Stage.index()]
                .as_ref()
                .map(|picture| picture.capacity),
            Some((30, 20))
        );
    }

    #[test]
    fn deferred_full_upload_draws_a_stale_photo_then_progresses_on_retirement() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline = own_pipeline(&device, &queue);
        assert!(pipeline.write(
            &device,
            &queue,
            Layer::Photo,
            &solid_raster(20, 12, 1, [0, 255, 0, 255]),
            Some(1),
            None,
        ));
        let busy = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        assert!(pipeline.write(
            &device,
            &queue,
            Layer::Photo,
            &solid_raster(40, 20, 2, [255, 0, 0, 255]),
            Some(2),
            None,
        ));
        assert_eq!(pipeline.retiring_full.load(Ordering::Acquire), 1);
        let desired = viewport_primitive(
            Some((solid_raster(80, 40, 3, [255, 255, 0, 255]), 3)),
            None,
            3,
            (80, 40),
        );
        let blanks_before = diagnostics(&pipeline).blank_photo_draws;
        assert_solid_bgra(
            &paint(&device, &queue, &mut pipeline, &desired),
            [0, 0, 255, 255],
        );
        let diagnostic = diagnostics(&pipeline);
        assert_eq!(diagnostic.blank_photo_draws, blanks_before);
        assert!(diagnostic.drawn_stale_photo);
        assert_eq!(diagnostic.drawn_content, None);
        assert_eq!(diagnostic.drawn_fallback_content, Some(2));
        assert!(diagnostic.full_resident_bytes + diagnostic.retiring_bytes <= 2 * FULL_BUDGET);
        assert!(pipeline.retiring_full.load(Ordering::Acquire) <= 1);
        wait(&device, busy);
        settle(&pipeline);
        assert_eq!(pipeline.retiring_bytes.load(Ordering::Acquire), 0);
        assert_solid_bgra(
            &paint(&device, &queue, &mut pipeline, &desired),
            [0, 255, 255, 255],
        );
        let diagnostic = diagnostics(&pipeline);
        assert!(!diagnostic.drawn_stale_photo);
        assert_eq!(diagnostic.drawn_content, Some(3));
    }

    #[test]
    fn stale_fit_photo_suppresses_new_overlay_until_replacement_arrives() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline = own_pipeline(&device, &queue);
        assert!(pipeline.write(
            &device,
            &queue,
            Layer::Photo,
            &solid_raster(20, 12, 1, [255, 0, 0, 255]),
            None,
            None,
        ));
        let busy = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        assert!(pipeline.write(
            &device,
            &queue,
            Layer::Photo,
            &solid_raster(40, 20, 2, [255, 0, 0, 255]),
            None,
            None,
        ));
        assert_eq!(pipeline.retiring_full.load(Ordering::Acquire), 1);
        let mut desired = full_primitive(solid_raster(80, 40, 3, [255, 255, 0, 255]));
        desired
            .layers
            .push((Layer::Clipping, solid_raster(80, 40, 1, [0, 255, 0, 255])));
        assert_solid_bgra(
            &paint(&device, &queue, &mut pipeline, &desired),
            [0, 0, 255, 255],
        );
        let diagnostic = diagnostics(&pipeline);
        assert!(diagnostic.drawn_stale_photo);
        assert_eq!(diagnostic.drawn_clipping_version, None);
        wait(&device, busy);
        settle(&pipeline);
        assert_solid_bgra(
            &paint(&device, &queue, &mut pipeline, &desired),
            [0, 255, 0, 255],
        );
        let diagnostic = diagnostics(&pipeline);
        assert!(!diagnostic.drawn_stale_photo);
        assert_eq!(diagnostic.drawn_clipping_version, Some(1));
    }

    #[test]
    fn blank_photo_draw_resets_all_previous_drawn_identities() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline = own_pipeline(&device, &queue);
        let shown = viewport_primitive(
            Some((solid_raster(16, 16, 1, [255, 0, 0, 255]), 1)),
            None,
            1,
            (16, 16),
        );
        assert_solid_bgra(
            &paint(&device, &queue, &mut pipeline, &shown),
            [0, 0, 255, 255],
        );
        let blank = viewport_primitive(None, None, 2, (16, 16));
        // Deliberately remove the previous photo: this test checks the blank diagnostic itself.
        pipeline.slots[Layer::Photo.index()] = None;
        let before = diagnostics(&pipeline).blank_photo_draws;
        assert_solid_bgra(
            &paint(&device, &queue, &mut pipeline, &blank),
            [255, 0, 0, 255],
        );
        let diagnostic = diagnostics(&pipeline);
        assert_eq!(diagnostic.blank_photo_draws, before + 1);
        assert_eq!(diagnostic.drawn_content, None);
        assert_eq!(diagnostic.drawn_full_version, None);
        assert_eq!(diagnostic.drawn_region_version, None);
        assert_eq!(diagnostic.drawn_regions, [None; 2]);
        assert!(!diagnostic.drawn_stale_photo);
        assert_eq!(diagnostic.drawn_fallback_content, None);
    }

    /// Keep the GPU busy: `copies` copies between two `size`-byte buffers in one submission.
    fn busy_gpu(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        size: u64,
        copies: u32,
    ) -> wgpu::SubmissionIndex {
        let usage = wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST;
        let buffer = || {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("review.busy"),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let (a, b) = (buffer(), buffer());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        for index in 0..copies {
            if index % 2 == 0 {
                encoder.copy_buffer_to_buffer(&a, 0, &b, 0, size);
            } else {
                encoder.copy_buffer_to_buffer(&b, 0, &a, 0, size);
            }
        }
        queue.submit([encoder.finish()])
    }

    fn wait(device: &wgpu::Device, index: wgpu::SubmissionIndex) {
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .expect("device wait");
    }

    const BUSY_SIZE: u64 = 64 * 1024 * 1024;
    const BUSY_COPIES: u32 = 96;

    /// A drag at 100%, a pause that refines the region to exact detail, then the drag resumes.
    /// The second resumed revision needs the set that held the exact region, so its upload is
    /// deferred behind retirement; the older revision is hidden; nothing is drawable.
    #[test]
    fn review_resumed_drag_after_exact_refinement_has_a_drawable_region() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline = own_pipeline(&device, &queue);
        let full_stage = (2000, 1200);
        let interactive = |content: u64, generation: u64, version: u64| {
            RegionFrame::new(
                raster(400, 250, version),
                [100, 100, 500, 350],
                (1000, 600),
                full_stage,
                0.5,
                RegionQuality::Interactive,
                content,
                generation,
            )
            .expect("half-detail region")
        };
        let exact = |content: u64, generation: u64, version: u64| {
            RegionFrame::new(
                raster(800, 500, version),
                [200, 200, 1000, 700],
                full_stage,
                full_stage,
                1.0,
                RegionQuality::Exact,
                content,
                generation,
            )
            .expect("exact region")
        };
        pipeline.write_region(&device, &queue, &interactive(1, 1, 1));
        pipeline.write_region(&device, &queue, &interactive(2, 2, 2));
        assert_eq!(region_draw_order(&pipeline.regions, 2, full_stage).len(), 1);
        // Quiet refinement of revision 2 (deferred once, then admitted after retirement).
        pipeline.write_region(&device, &queue, &exact(2, 3, 3));
        settle(&pipeline);
        pipeline.write_region(&device, &queue, &exact(2, 3, 3));
        assert!(pipeline.regions.iter().flatten().any(|picture| {
            picture
                .region_key
                .is_some_and(|key| key.quality == RegionQuality::Exact)
        }));
        // The drag resumes: revision 3 reuses the half-detail set.
        pipeline.write_region(&device, &queue, &interactive(3, 4, 4));
        assert_eq!(
            region_draw_order(&pipeline.regions, 3, full_stage).len(),
            1,
            "revision 3 is drawn"
        );
        // A frame is still executing on the GPU, as during every drag.
        let busy = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        let deferred_before = diagnostics(&pipeline).deferred_uploads;
        pipeline.write_region(&device, &queue, &interactive(4, 5, 5));
        let deferred_after = diagnostics(&pipeline).deferred_uploads;
        let drawable = region_draw_order(&pipeline.regions, 4, full_stage);
        let slots: Vec<_> = pipeline
            .regions
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .map(|picture| (picture.width, picture.height, picture.content_id))
            })
            .collect();
        wait(&device, busy);
        settle(&pipeline);
        assert!(
            !drawable.is_empty(),
            "revision 4 was adopted but the surface can draw nothing for it: the canvas is blank \
             for this frame (older revision hidden, new upload deferred). slots={slots:?} \
             deferred_uploads {deferred_before}->{deferred_after}"
        );
    }

    /// A quick drag at 100% leaves two half-detail sets. Release commits a new recipe identity
    /// whose settle job starts with an exact visible region (full detail, other dimensions).
    #[test]
    fn review_release_at_100_percent_has_a_drawable_region() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline = own_pipeline(&device, &queue);
        let full_stage = (2000, 1200);
        let interactive = |content: u64, generation: u64, version: u64| {
            RegionFrame::new(
                raster(400, 250, version),
                [100, 100, 500, 350],
                (1000, 600),
                full_stage,
                0.5,
                RegionQuality::Interactive,
                content,
                generation,
            )
            .expect("half-detail region")
        };
        pipeline.write_region(&device, &queue, &interactive(1, 1, 1));
        pipeline.write_region(&device, &queue, &interactive(2, 2, 2));
        let busy = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        // The committed recipe (content 3) arrives as its settle job's exact region.
        let committed = RegionFrame::new(
            raster(800, 500, 3),
            [200, 200, 1000, 700],
            full_stage,
            full_stage,
            1.0,
            RegionQuality::Exact,
            3,
            3,
        )
        .expect("exact region");
        pipeline.write_region(&device, &queue, &committed);
        let drawable = region_draw_order(&pipeline.regions, 3, full_stage);
        let slots: Vec<_> = pipeline
            .regions
            .iter()
            .map(|slot| {
                slot.as_ref()
                    .map(|picture| (picture.width, picture.height, picture.content_id))
            })
            .collect();
        wait(&device, busy);
        settle(&pipeline);
        assert!(
            !drawable.is_empty(),
            "the committed exact region was adopted but nothing is drawable for it: slots={slots:?}"
        );
    }

    /// At Fit a proxy of new dimensions (rotation, crop, refit) replaces a small proxy.
    #[test]
    fn review_a_fit_proxy_of_new_dimensions_is_drawn_in_the_frame_it_arrives() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline = own_pipeline(&device, &queue);
        assert!(pipeline.write(
            &device,
            &queue,
            Layer::Photo,
            &raster(1716, 1144, 1),
            None,
            None
        ));
        let busy = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        let wrote = pipeline.write(
            &device,
            &queue,
            Layer::Photo,
            &raster(1144, 1716, 2),
            None,
            None,
        );
        let slot = pipeline.slots[Layer::Photo.index()]
            .as_ref()
            .map(|picture| (picture.width, picture.height, picture.version));
        let retiring = pipeline.retiring_bytes.load(Ordering::Acquire);
        wait(&device, busy);
        settle(&pipeline);
        assert!(
            wrote && slot.is_some(),
            "the new Fit proxy was not written (wrote={wrote}) and the photo slot is {slot:?}: \
             the non-viewport draw has nothing to draw, although only {retiring} bytes were \
             retiring against a 512 MiB budget"
        );
    }

    /// Let `duration` of wall time pass. In the timing probes below it stands for the time a frame
    /// spends encoding before its submit, and gives the retirement worker the moment it would have
    /// in a real frame to take a retirement up: part of the scenario they time, which only they
    /// assert on, outside the default suite.
    fn let_pass(duration: Duration) {
        let from = Instant::now();
        wait_until("the probe's frame time passing", || {
            from.elapsed() >= duration
        });
    }

    /// Characterise the stall with a frame-sized GPU workload (prints only).
    #[test]
    #[ignore = "diagnostic timing probe; run after functional work is complete"]
    fn review_retirement_submit_stall_with_small_gpu_frames() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let pipeline = own_pipeline(&device, &queue);
        let full_stage = (2000, 1200);
        let region = |width: u32, content: u64, version: u64| {
            RegionFrame::new(
                raster(width, 200, version),
                [0, 0, width, 200],
                full_stage,
                full_stage,
                1.0,
                RegionQuality::Exact,
                content,
                version,
            )
            .expect("region")
        };
        drop(pipeline);
        for copies in [2u32, 8, 24] {
            let mut gpus = Vec::new();
            let mut stalls = Vec::new();
            for round in 0..5u64 {
                let mut pipeline = own_pipeline(&device, &queue);
                let base = 10 + round * 10 + u64::from(copies) * 1000;
                pipeline.write_region(&device, &queue, &region(300, base, base));
                pipeline.write_region(&device, &queue, &region(300, base + 1, base + 1));
                settle(&pipeline);
                let index = busy_gpu(&device, &queue, 16 * 1024 * 1024, copies);
                let started = Instant::now();
                wait(&device, index);
                gpus.push(started.elapsed());
                let index = busy_gpu(&device, &queue, 16 * 1024 * 1024, copies);
                pipeline.write_region(&device, &queue, &region(600, base + 2, base + 2));
                let retiring = pipeline.retiring_regions.load(Ordering::Acquire);
                assert!(
                    retiring > 0,
                    "diagnostic requires a real texture retirement"
                );
                // The rest of the frame's prepare and draw encoding before its submit.
                let_pass(Duration::from_micros(300));
                let started = Instant::now();
                queue.submit(None);
                stalls.push((retiring, started.elapsed()));
                wait(&device, index);
                settle(&pipeline);
            }
            eprintln!("{copies} copies: gpu frames {gpus:?}; (retiring, submit stall) {stalls:?}");
        }
    }

    /// Regression for the former retirement worker's `device.poll(Wait)` stall. The render
    /// thread's next `Queue::submit` must not wait for retirement.
    #[test]
    #[ignore = "diagnostic timing probe; run after functional work is complete"]
    fn review_retirement_never_blocks_a_render_thread_submit() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let mut pipeline = own_pipeline(&device, &queue);
        // How long the busy workload takes on this GPU.
        let index = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        let started = Instant::now();
        wait(&device, index);
        let gpu = started.elapsed();
        // Control: an empty submit while the GPU is busy and nothing retires.
        let index = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        let_pass(Duration::from_millis(2));
        let started = Instant::now();
        queue.submit(None);
        let control = started.elapsed();
        wait(&device, index);
        // Two region sets, then a region of new dimensions retires the back set while the GPU
        // is busy — what a frame's `prepare` does before that frame's own submit.
        let full_stage = (2000, 1200);
        let region = |width: u32, content: u64, version: u64| {
            RegionFrame::new(
                raster(width, 200, version),
                [0, 0, width, 200],
                full_stage,
                full_stage,
                1.0,
                RegionQuality::Exact,
                content,
                version,
            )
            .expect("region")
        };
        pipeline.write_region(&device, &queue, &region(300, 1, 1));
        pipeline.write_region(&device, &queue, &region(300, 2, 2));
        settle(&pipeline);
        let index = busy_gpu(&device, &queue, BUSY_SIZE, BUSY_COPIES);
        pipeline.write_region(&device, &queue, &region(600, 3, 3));
        let retiring = pipeline.retiring_regions.load(Ordering::Acquire);
        assert!(
            retiring > 0,
            "diagnostic requires a real texture retirement"
        );
        let_pass(Duration::from_millis(2));
        let started = Instant::now();
        queue.submit(None);
        let blocked = started.elapsed();
        wait(&device, index);
        settle(&pipeline);
        eprintln!(
            "busy workload {gpu:?}; empty submit without retirement {control:?}; \
             with retirement in flight ({retiring} retiring) {blocked:?}"
        );
        assert!(
            blocked < Duration::from_millis(4),
            "the render thread's submit waited {blocked:?} for the retirement worker's GPU wait \
             (busy workload {gpu:?}, control submit {control:?})"
        );
    }
}
