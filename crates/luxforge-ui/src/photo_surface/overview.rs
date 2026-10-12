//! The high-zoom minimap's overview on a photo surface (`docs/design/minimap.md`, "Overview
//! rendering and lifetime"): what the photograph's surface keeps of it, and the small widget that
//! draws it.
//!
//! - **Kept by the photograph's surface.** The overview's textures belong to the surface that drew
//!   the picture at rest they were reduced from ([`luxforge_gpu::OverviewStore`]), at most the two
//!   versions the desktop's [`OverviewWant`] names, handed with every frame of that surface
//!   ([`super::PhotoSurface::gpu_overview`]). A frame with no want lets them go, and so does the
//!   surface's release when a frame no longer draws it.
//! - **Drawn by its own widget.** [`overview_image`] draws a version the surface holds, placed
//!   `Contain` in its own bounds, through the photograph's pipeline: it reads the surface's
//!   textures and writes only the overview's placement, never the photograph's slots, plans or
//!   pictures. A version the surface does not hold draws nothing; nothing is uploaded or reduced.
//! - **Reported.** The surface's diagnostics carry what it holds, a refusal, its counters and the
//!   pipeline's overview bytes ([`super::SurfaceDiagnostics::gpu_overview`]); a change of what it
//!   holds wakes the desktop once.
use super::{
    PhotoPipeline, PhotoPrimitive, Picture, Placement, SurfaceId, SurfaceSlots, draw_picture,
    physical_bright, physical_rects, turn_uniform, visible_placement, write_uniforms,
};
use iced::{
    Element, Length, Rectangle, Size,
    advanced::{Layout, Widget, layout, mouse, renderer, widget::Tree},
};
pub use luxforge_gpu::{
    OVERVIEW_GPU_BUDGET, OverviewFigures, OverviewShown, OverviewUpload, OverviewWant,
};

impl PhotoPipeline {
    /// Keep what `want` names of `surface`'s overviews, upload a reference overview it hands, and
    /// bind each finished overview once for drawing ([`luxforge_gpu::Executor::prepare_overview`]).
    pub(super) fn prepare_overview(
        &self,
        surface: &mut SurfaceSlots,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        want: Option<&OverviewWant>,
    ) {
        self.executor
            .prepare_overview(&mut surface.state, device, queue, want);
        self.sync_overviews(surface, device);
    }

    /// Retire every overview `surface` holds.
    pub(super) fn release_overview(&self, surface: &mut SurfaceSlots) {
        self.executor.release_overview(&mut surface.state);
        surface.overview_pictures.clear();
    }

    /// The surface's finished overviews as pictures the photograph's pipeline draws, each bound
    /// once while its texture is held.
    fn sync_overviews(&self, surface: &mut SurfaceSlots, device: &wgpu::Device) {
        let mut held = std::mem::take(&mut surface.overview_pictures);
        for output in surface.state.overview.outputs() {
            let at = held
                .iter()
                .position(|picture| picture.tiles[0].texture == output.tiles[0].texture);
            let mut picture = at.map(|at| held.swap_remove(at));
            self.sync_picture(device, &mut picture, Some(output));
            surface.overview_pictures.extend(picture);
        }
    }
}

impl SurfaceSlots {
    /// The finished overview of `version`, as the photograph's pipeline draws it.
    fn overview_picture(&self, version: u64) -> Option<&Picture> {
        self.overview_pictures
            .iter()
            .find(|picture| picture.version == version)
    }
}

impl PhotoPrimitive {
    /// The primitive that draws surface `surface`'s overview of `version`, placed at `offset` with
    /// `size` from the visible part of its widget.
    pub(super) fn overview(surface: SurfaceId, version: u64, offset: iced::Vector, size: Size) -> Self {
        Self {
            surface,
            layers: Vec::new(),
            viewport: None,
            region_overlays: [None, None],
            gpu: None,
            gpu_options: Default::default(),
            dissolve: None,
            source: None,
            rest: None,
            overview: None,
            overview_draw: Some(version),
            offset,
            size,
            bright: None,
            angle: 0.0,
            snap: true,
        }
    }

    /// An overview primitive's `prepare`: the overview's placement written, nothing else of the
    /// surface touched, and nothing when the surface does not hold it.
    pub(super) fn prepare_overview_draw(
        &self,
        pipeline: &PhotoPipeline,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        scale: f32,
        version: u64,
    ) {
        let Some(picture) = pipeline
            .surfaces
            .get(&self.surface)
            .and_then(|surface| surface.overview_picture(version))
        else {
            return;
        };
        let (viewport, destination) = physical_rects(*bounds, self.offset, self.size, scale);
        let [x0, y0, x1, y1, dim] = physical_bright(*bounds, None, scale);
        let turn = turn_uniform(0.0, dim, true);
        write_uniforms(queue, picture, viewport, destination, [x0, y0, x1, y1], turn);
    }

    /// An overview primitive's `draw`: the overview alone, when the surface holds it.
    pub(super) fn draw_overview(
        &self,
        pipeline: &PhotoPipeline,
        render_pass: &mut wgpu::RenderPass<'_>,
        version: u64,
    ) {
        if let Some(picture) = pipeline
            .surfaces
            .get(&self.surface)
            .and_then(|surface| surface.overview_picture(version))
        {
            render_pass.set_pipeline(&pipeline.pipeline);
            draw_picture(render_pass, picture);
        }
    }
}

/// The overview a minimap draws: the surface that holds it, its version, and its size in pixels,
/// which it is placed `Contain` by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverviewImage {
    pub surface: SurfaceId,
    pub version: u64,
    pub size: (u32, u32),
}

/// A widget drawing `image`, scaled to fit `width` × `height` and centred, through the photograph's
/// pipeline; nothing while its surface does not hold that version.
pub fn overview_image(image: OverviewImage, width: Length, height: Length) -> OverviewSurface {
    OverviewSurface {
        image,
        width,
        height,
    }
}

/// The widget [`overview_image`] makes.
pub struct OverviewSurface {
    image: OverviewImage,
    width: Length,
    height: Length,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for OverviewSurface
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
        let Some(visible) = visible_placement(
            super::Base::Photo(Placement::Contain),
            self.image.size,
            layout.bounds(),
            *viewport,
        ) else {
            return;
        };
        renderer.draw_primitive(
            visible.clip,
            PhotoPrimitive::overview(
                self.image.surface,
                self.image.version,
                visible.offset,
                visible.size,
            ),
        );
    }
}

impl<'a, Message: 'a> From<OverviewSurface> for crate::Element<'a, Message> {
    fn from(surface: OverviewSurface) -> Self {
        Element::new(surface)
    }
}
