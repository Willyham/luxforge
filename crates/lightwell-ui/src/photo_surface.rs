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
//! Memory: one set of tiles per [`Layer`] per pipeline — that is, for the whole application, because
//! one photograph is on screen at a time — recreated only when a frame's dimensions change, plus a
//! 96-byte uniform buffer per tile. The photograph's tiles are kept while a crop draft shows its
//! stage, so returning to the photograph writes nothing; the stage's and the overlays' tiles are
//! released by the first draw that does not show them. The pixels themselves are borrowed from the
//! frames the desktop already holds; this crate copies none of them, and the one copy per changed
//! frame is the write into the textures, which reads each tile straight out of the shared buffer.

use iced::{
    ContentFit, Element, Length, Point, Rectangle, Size, Vector,
    advanced::{Layout, Widget, layout, mouse, renderer, widget::Tree},
    widget::shader::{self, Viewport},
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// How many photograph frames every photo surface in the process has written into its texture.
/// Diagnostics only: an evidence run records it beside each captured frame, which is how a run
/// proves that redrawing an unchanged photograph — at any zoom, however often the view is rebuilt —
/// writes nothing. The crop stage and the overlays have textures of their own and are not counted.
static TEXTURE_WRITES: AtomicU64 = AtomicU64::new(0);

/// The number of photograph texture writes so far; see [`TEXTURE_WRITES`].
pub fn texture_writes() -> u64 {
    TEXTURE_WRITES.load(Ordering::Relaxed)
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
/// The buffer is whatever already holds the pixels — a render's shared `Arc<[u8]>`, or the `Vec` an
/// overlay was painted into — taken as it is, so making a frame copies nothing.
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
    /// that many bytes. A surface never draws a buffer it cannot account for.
    pub fn new(
        pixels: impl AsRef<[u8]> + Send + Sync + 'static,
        width: u32,
        height: u32,
        version: u64,
    ) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))?;
        (width > 0 && height > 0 && pixels.as_ref().len() == expected).then(|| Self {
            pixels: Arc::new(pixels),
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
    width: Length,
    height: Length,
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
        let Some((_, frame)) = self.layers.first() else {
            return;
        };
        // Inside a scrollable, `viewport` is already in the content's own coordinates — the
        // scrollable shifts it by the scroll offset before handing it down — so it and the layout
        // bounds are in one frame and their intersection is the part on screen.
        let Some(visible) = visible_placement(self.base, frame.size(), layout.bounds(), *viewport)
        else {
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
                offset: visible.offset,
                size: visible.size,
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
    offset: Vector,
    size: Size,
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
    fn placement(&self, (width, height): (u32, u32)) -> ([f32; 4], [f32; 4]) {
        let (texture_width, texture_height) = self.texture_size();
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
fn tile_layout((width, height): (u32, u32), limit: u32) -> Vec<TileLayout> {
    // `[content start, content end, texels start, texels end]` along one axis.
    let spans = |extent: u32| -> Vec<[u32; 4]> {
        if extent <= limit {
            return vec![[0, extent, 0, extent]];
        }
        // Two pixels of every texture may be apron, so each span's content is at most this.
        let usable = limit.saturating_sub(2).max(1);
        let count = extent.div_ceil(usable);
        let edge = |index: u32| (u64::from(extent) * u64::from(index) / u64::from(count)) as u32;
        (0..count)
            .map(|index| {
                let (start, end) = (edge(index), edge(index + 1));
                [start, end, start.saturating_sub(1), (end + 1).min(extent)]
            })
            .collect()
    };
    let columns = spans(width);
    spans(height)
        .iter()
        .flat_map(|row| {
            columns.iter().map(move |column| TileLayout {
                content: [column[0], row[0], column[1], row[1]],
                texels: [column[2], row[2], column[3], row[3]],
            })
        })
        .collect()
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
        // Every layer but the photograph is released as soon as a draw does not show it: the
        // stage when its draft ends, an overlay when it is turned off. The photograph is kept
        // while the stage stands in for it, so going back to it writes nothing.
        for layer in [Layer::Stage, Layer::Clipping, Layer::Coverage] {
            if !self.layers.iter().any(|(drawn, _)| *drawn == layer) {
                pipeline.slots[layer.index()] = None;
            }
        }
        // One write per changed frame, never per redraw: the textures are recreated only when a
        // frame's dimensions change, and written only when its version does.
        for (layer, frame) in &self.layers {
            pipeline.write(device, queue, *layer, frame);
        }
        // The uniforms are refreshed every prepare instead, because the bounds and the viewport
        // can change with no new frame at all — a window resize, a pan, a panel opening. `bounds`
        // is the visible part of the widget, translated to where it is drawn.
        let scale = viewport.scale_factor();
        let (viewport, destination) = physical_rects(*bounds, self.offset, self.size, scale);
        let [x0, y0, x1, y1, dim] = physical_bright(*bounds, self.bright, scale);
        let turn = turn_uniform(self.angle, dim, self.snap);
        for (layer, _) in &self.layers {
            let Some(picture) = &pipeline.slots[layer.index()] else {
                continue;
            };
            for tile in &picture.tiles {
                let (region, texels) = tile.layout.placement((picture.width, picture.height));
                queue.write_buffer(
                    &tile.uniform,
                    0,
                    &uniform_bytes([
                        viewport,
                        destination,
                        region,
                        texels,
                        [x0, y0, x1, y1],
                        turn,
                    ]),
                );
            }
        }
    }

    fn draw(&self, pipeline: &PhotoPipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        // The render pass's viewport is already the visible part of this widget and its scissor
        // that part clipped to the layer, so each tile's quad is positioned inside that frame by
        // its uniform alone and whatever of it falls outside is clipped. The layers are drawn in
        // order, each blended over the one before.
        render_pass.set_pipeline(&pipeline.pipeline);
        for (layer, _) in &self.layers {
            let Some(picture) = &pipeline.slots[layer.index()] else {
                continue;
            };
            for tile in &picture.tiles {
                render_pass.set_bind_group(0, &tile.bindings, &[]);
                render_pass.draw(0..6, 0..1);
            }
        }
        // Drawn either way: with no texture there is nothing to show, and the encoder fallback
        // would only begin a render pass to draw the same nothing.
        true
    }
}

/// One tile on the GPU: its texture, its own uniform and the bindings that join them.
struct Tile {
    layout: TileLayout,
    texture: wgpu::Texture,
    uniform: wgpu::Buffer,
    bindings: wgpu::BindGroup,
}

/// One layer's frame on the GPU, with what it holds.
struct Picture {
    tiles: Vec<Tile>,
    width: u32,
    height: u32,
    /// The texture limit the tiles were cut for.
    limit: u32,
    /// The version of the frame last written into it.
    version: u64,
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
}

impl PhotoPipeline {
    /// Make `layer`'s textures hold `frame`, creating them when the dimensions changed and writing
    /// the pixels when the version did.
    fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, layer: Layer, frame: &Frame) {
        let (width, height) = frame.size();
        // The device's limit, not the GPU's: Iced asks wgpu for its default limits, so this is
        // 8192 on the owner's Mac although the GPU could hold larger textures.
        let limit = device.limits().max_texture_dimension_2d;
        let fresh = !self.slots[layer.index()].as_ref().is_some_and(|picture| {
            picture.width == width && picture.height == height && picture.limit == limit
        });
        if fresh {
            let tiles = tile_layout((width, height), limit)
                .into_iter()
                .map(|tile| self.tile(device, layer, tile))
                .collect();
            self.slots[layer.index()] = Some(Picture {
                tiles,
                width,
                height,
                limit,
                // No version can match until the pixels are written below.
                version: frame.version.wrapping_sub(1),
            });
        }
        let Some(picture) = &mut self.slots[layer.index()] else {
            return;
        };
        if picture.version == frame.version {
            return;
        }
        for tile in &picture.tiles {
            let (tile_width, tile_height) = tile.layout.texture_size();
            queue.write_texture(
                tile.texture.as_image_copy(),
                (*frame.pixels).as_ref(),
                tile.layout.copy_layout(width),
                wgpu::Extent3d {
                    width: tile_width,
                    height: tile_height,
                    depth_or_array_layers: 1,
                },
            );
        }
        picture.version = frame.version;
        if layer == Layer::Photo {
            TEXTURE_WRITES.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// One tile's texture, uniform and bindings, sampled as its layer is.
    fn tile(&self, device: &wgpu::Device, layer: Layer, layout: TileLayout) -> Tile {
        let (width, height) = layout.texture_size();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lightwell.photo_surface.texture"),
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
            label: Some("lightwell.photo_surface.uniform"),
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
            label: Some("lightwell.photo_surface.bindings"),
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
            texture,
            uniform,
            bindings,
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
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
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
            "lightwell.photo_surface.sampler",
            wgpu::FilterMode::Linear,
        );
        let nearest = sampler(
            device,
            "lightwell.photo_surface.overlay_sampler",
            wgpu::FilterMode::Nearest,
        );
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lightwell.photo_surface.layout"),
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
            label: Some("lightwell.photo_surface.shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "photo_surface.wgsl"
            ))),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lightwell.photo_surface.pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lightwell.photo_surface.pipeline"),
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
        Self {
            pipeline,
            layout,
            linear,
            nearest,
            texture_format,
            slots: [None, None, None, None],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(width: u32, height: u32, version: u64) -> Frame {
        let pixels: Arc<[u8]> = vec![0u8; (width * height * 4) as usize].into();
        Frame::new(pixels, width, height, version).expect("a whole raster")
    }

    /// A frame is exactly its declared size, or it is not a frame at all.
    #[test]
    fn a_raster_is_refused_unless_the_buffer_matches_its_dimensions() {
        let pixels: Arc<[u8]> = vec![0u8; 16].into();
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
        assert!(Frame::new(vec![0u8; 16], 2, 2, 1).is_some());
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
            let (region, texels) = tiles[0].placement(size);
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
        let (left_region, left_texels) = left.placement(size);
        let (right_region, right_texels) = right.placement(size);
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
        let (left, _) = tiles[0].placement(size);
        let (right, _) = tiles[1].placement(size);
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
