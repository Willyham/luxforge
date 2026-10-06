//! The view: one file per region, each a pure function from a model to an `Element`. Nothing here
//! reads authoritative state, validates a parameter or calls the owner; the models carry everything
//! the screen shows, so a rendering change cannot change what the editor does.
//!
//! The five-region shell from the [Develop workspace design](../../../../docs/design/develop-workspace.md#layout):
//! a title bar, a middle row of the state panel, the canvas and the tools panel, and a status bar.
//! The two side panels collapse independently through the session's own workspace state; the canvas
//! takes whatever remains. Every region is styled from [`luxforge_ui::theme`], never an ad hoc
//! colour, and the window carries no outer padding: the canvas pads its photo itself.
pub(crate) mod canvas;
pub(crate) mod canvas_view;
mod capabilities;
mod compare_canvas;
pub(crate) mod crop_canvas;
pub(crate) mod cursor_probe;
mod gallery;
pub(crate) mod mask_canvas;
pub(crate) mod masks_panel;
pub(crate) mod palette;
pub(crate) mod query_choice;
pub(crate) mod settings;
pub(crate) mod state_panel;
pub(crate) mod status_bar;
pub(crate) mod title_bar;
pub(crate) mod tools_panel;
mod warped_path;

pub(crate) use gallery::{gallery, page_info as gallery_page_info};

use crate::{
    app::message::Message,
    crop_draft::CropDraft,
    layout::{
        DIVIDER_WIDTH, STATE_PANEL_WIDTH, STATUS_BAR_HEIGHT, TITLE_BAR_HEIGHT, TOOLS_PANEL_WIDTH,
    },
    state::Workspace,
};
use iced::{
    Length,
    widget::{Space, column, container, row, stack},
};
use luxforge_ui::theme;
use luxforge_ui::{Element, Theme};

/// The pixels and the transient draft the canvas borrows for one frame. They are not view-model
/// data: the model says what to draw, these are what it is drawn from.
#[derive(Clone, Copy)]
pub(crate) struct Surfaces<'a> {
    /// The frames below are plain data rather than allocations: the photo surface owns their
    /// textures and writes each into its own while it draws, so no round trip stands between a
    /// frame and the screen.
    ///
    /// The photograph.
    pub(crate) photo: Option<&'a luxforge_ui::Frame>,
    pub(crate) comparison: Option<(&'a luxforge_ui::Frame, f32)>,
    pub(crate) photo_content: Option<u64>,
    pub(crate) current_content: u64,
    /// A mask's coverage of the region the GPU draws at 100% and above, laid over that region.
    pub(crate) region_coverage: Option<&'a luxforge_ui::RegionOverlay>,
    /// The crop layer's input stage, drawn in place of the photograph while its draft is open.
    pub(crate) stage: Option<&'a luxforge_ui::Frame>,
    /// The crop stage's layer prefix drawn by the GPU, in tiles reduced to the stage's display
    /// bounds, which the stage's surface draws in place of the stand-in frame it holds.
    pub(crate) stage_rest: Option<&'a luxforge_ui::photo_surface::GpuRest>,
    /// The clipping overlay's bounded cell grid, present only when it belongs to the photograph on
    /// screen. The surface lays it over the photograph, never changing the photograph itself.
    pub(crate) clipping: Option<&'a luxforge_ui::Frame>,
    /// The mask coverage's bounded cell grid, present only when it belongs to the photograph on
    /// screen, laid over the clipping overlay.
    pub(crate) coverage: Option<&'a luxforge_ui::Frame>,
    /// The open mask shape gesture and the affine its handles are drawn through.
    pub(crate) mask_draft: Option<&'a crate::mask_draft::MaskDraft>,
    pub(crate) mask_map: Option<&'a crate::mask_draft::ContentMap>,
    pub(crate) draft: Option<&'a CropDraft>,
    /// A GPU plan the photograph is drawn from in place of its frame, which stays the surface's
    /// fallback: a whole frame's at Fit and below 100%, a region's at 100% or more. An open
    /// gesture's ([`crate::app::gpu_preview`]), or an evidence run's GPU identity hook's. None is
    /// given while the GPU stage is refused (`Editor::gpu_plan`).
    pub(crate) gpu: Option<&'a luxforge_ui::photo_surface::GpuPlan>,
    /// Keep the plan's slot but draw the frame: the CPU frame of the plan's revision is presented.
    pub(crate) gpu_hold: bool,
    /// The draft revision the plan's output is reported under.
    pub(crate) gpu_tag: Option<u64>,
    /// The plan's serial and where it changes since the plan the surface holds.
    pub(crate) gpu_change: Option<luxforge_ui::photo_surface::GpuChange>,
    /// The program sequences the committed stack's gestures are likely to need, compiled ahead.
    pub(crate) gpu_warm: Option<&'a luxforge_ui::photo_surface::GpuWarm>,
    /// The prepared source every GPU boundary is derived from, which the pipeline holds on the GPU
    /// for every surface that hands it ([`crate::app::gpu_preview`]). None is given while the GPU
    /// stage is refused.
    pub(crate) gpu_source: Option<&'a luxforge_ui::photo_surface::GpuSource>,
    /// The displayed stack's picture at rest the GPU draws in tiles, which a whole-frame
    /// photograph draws in place of its frame once the last tile is in.
    pub(crate) gpu_rest: Option<&'a luxforge_ui::photo_surface::GpuRest>,
    /// The same tiles drawn for their histogram and clipping counts alone, which put nothing on
    /// screen: handed to whichever surface draws the photograph, a percentage view's included,
    /// while the counts of the content the GPU presents are to come and the picture is not handed.
    pub(crate) gpu_counts: Option<&'a luxforge_ui::photo_surface::GpuRest>,
    /// Compare's After side on the GPU: the GPU picture of the stack on screen when Compare
    /// began, retained while it is shown — its view plan, with its serial, and its picture at rest
    /// in tiles — drawn in place of the retained After frame, which stays the surface's fallback.
    pub(crate) compare_gpu: Option<&'a luxforge_ui::photo_surface::GpuPlan>,
    pub(crate) compare_change: Option<luxforge_ui::photo_surface::GpuChange>,
    pub(crate) compare_rest: Option<&'a luxforge_ui::photo_surface::GpuRest>,
    /// A settle's dissolve from the GPU frame on screen to the CPU frame that replaces it
    /// ([`crate::app::gpu_settle`]).
    pub(crate) dissolve: Option<luxforge_ui::photo_surface::Dissolve>,
}

impl Surfaces<'_> {
    /// Whether a percentage view of 100% or more draws the photograph as a whole frame, as Fit and
    /// every view below 100% do: no exact frame is held for the view's own surface to draw, so the
    /// photograph's frame alone fills the view's box. A GPU region plan — the GPU's picture of the
    /// view at 100% and above — is drawn through the view's own surface instead.
    pub(crate) fn whole_frame(&self) -> bool {
        self.photo_content.is_none() && self.gpu.is_none_or(|plan| plan.region.is_none())
    }
}

pub(crate) fn workspace<'a>(model: &'a Workspace, surfaces: Surfaces<'a>) -> Element<'a, Message> {
    let title = container(title_bar::title_bar(model))
        .width(Length::Fill)
        .height(Length::Fixed(TITLE_BAR_HEIGHT))
        .style(theme::title_bar_surface);

    let canvas_area = container(canvas::surface(&model.canvas, surfaces))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(canvas::background_surface(model.canvas.background));

    let mut middle = row![].height(Length::Fill);
    if model.title.state_panel_open {
        middle = middle.push(
            container(state_panel::state_panel(&model.panel, &model.performance))
                .width(Length::Fixed(STATE_PANEL_WIDTH))
                .height(Length::Fill)
                .style(theme::panel_surface),
        );
        middle = middle.push(vertical_divider());
    }
    middle = middle.push(canvas_area);
    if model.title.tools_panel_open {
        middle = middle.push(vertical_divider());
        middle = middle.push(
            container(tools_panel::tools_panel(
                &model.tools,
                &model.histogram,
                &model.masks,
                model.canvas.mask_panel,
            ))
            .width(Length::Fixed(TOOLS_PANEL_WIDTH))
            .height(Length::Fill)
            .style(theme::panel_surface),
        );
    }

    let status = container(status_bar::status_bar(&model.status))
        .height(Length::Fixed(STATUS_BAR_HEIGHT))
        .padding([0.0, theme::TITLE_BAR_INSET])
        .align_y(iced::alignment::Vertical::Center)
        .style(theme::panel_surface);

    let screen = column![
        title,
        horizontal_divider(),
        middle,
        horizontal_divider(),
        status
    ];
    let mut layers = stack![screen];
    if let Some(overlay) = palette::palette(&model.palette) {
        layers = layers.push(overlay);
    }
    if let Some(sheet) = settings::settings(&model.settings) {
        layers = layers.push(sheet);
    }
    layers.into()
}

/// A 1 px vertical rule between the middle row's regions.
fn vertical_divider<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fixed(DIVIDER_WIDTH))
        .height(Length::Fill)
        .style(divider_style)
        .into()
}

/// A 1 px horizontal rule between the title bar, the middle row and the status bar.
fn horizontal_divider<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(DIVIDER_WIDTH))
        .style(divider_style)
        .into()
}

/// The rules are the theme's band border, the border's 6% white stored opaque, because Iced blends
/// in linear light and would draw the translucent border far brighter than the board does.
fn divider_style(iced_theme: &Theme) -> container::Style {
    theme::divider_surface(iced_theme)
}
