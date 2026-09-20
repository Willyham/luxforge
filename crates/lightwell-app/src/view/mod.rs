//! The view: one file per region, each a pure function from a model to an `Element`. Nothing here
//! reads authoritative state, validates a parameter or calls the owner; the models carry everything
//! the screen shows, so a rendering change cannot change what the editor does.
//!
//! The arrangement is still the plain one: a header, the photo beside one sidebar, a status row.
//! The five-region layout from the Develop workspace design lands with the panels themselves.
pub(crate) mod canvas;
pub(crate) mod palette;
pub(crate) mod state_panel;
pub(crate) mod status_bar;
pub(crate) mod title_bar;
pub(crate) mod tools_panel;

use crate::{app::message::Message, crop_draft::CropDraft, state::Workspace};
use iced::{
    Element, Length,
    widget::{Column, column, container, row, scrollable, stack},
};
use iced_runtime::image as image_memory;

/// Layout constants shared by the view and the evidence frame description.
pub(crate) const PADDING: f32 = 16.0;
pub(crate) const SPACING: f32 = 12.0;
pub(crate) const SIDEBAR_WIDTH: f32 = 340.0;
/// The gap between the blocks inside the sidebar.
const BLOCK_SPACING: f32 = 18.0;

/// The GPU-resident pixels and the transient draft the canvas borrows for one frame. They are not
/// view-model data: the model says what to draw, these are what it is drawn from.
pub(crate) struct Surfaces<'a> {
    pub(crate) photo: Option<&'a image_memory::Allocation>,
    pub(crate) draft_photo: Option<&'a image_memory::Allocation>,
    pub(crate) draft: Option<&'a CropDraft>,
}

pub(crate) fn workspace<'a>(model: &'a Workspace, surfaces: Surfaces<'a>) -> Element<'a, Message> {
    let surface = container(canvas::surface(&model.canvas, surfaces))
        .width(Length::Fill)
        .height(Length::Fill);
    let middle: Element<'_, Message> = match sidebar(model) {
        Some(sidebar) => row![surface, sidebar]
            .spacing(SPACING)
            .height(Length::Fill)
            .into(),
        None => surface.into(),
    };
    let screen = column![
        title_bar::title_bar(&model.title),
        middle,
        status_bar::status_bar(&model.status),
    ]
    .spacing(SPACING)
    .padding(PADDING);
    match palette::palette(&model.palette) {
        Some(overlay) => stack![screen, overlay].into(),
        None => screen.into(),
    }
}

/// The two panels share one sidebar until they collapse independently.
fn sidebar<'a>(model: &'a Workspace) -> Option<Element<'a, Message>> {
    let mut blocks: Vec<Element<'a, Message>> = Vec::new();
    if model.title.tools_panel_open {
        blocks.push(tools_panel::tools_panel(&model.tools));
        blocks.push(title_bar::view_controls(model));
    }
    if model.title.state_panel_open {
        blocks.push(state_panel::state_panel(&model.panel));
    }
    if blocks.is_empty() {
        return None;
    }
    Some(
        scrollable(
            Column::with_children(blocks)
                .spacing(BLOCK_SPACING)
                .padding(12),
        )
        .width(SIDEBAR_WIDTH)
        .into(),
    )
}

/// The physical x range of the photo surface inside a captured frame, so evidence can prove the
/// image was drawn where the layout puts it.
pub(crate) fn surface_columns(logical_width: f32, scale: f32, model: &Workspace) -> [u32; 2] {
    let sidebar = if model.title.state_panel_open || model.title.tools_panel_open {
        SIDEBAR_WIDTH + SPACING
    } else {
        0.0
    };
    [
        (PADDING * scale).round() as u32,
        ((logical_width - PADDING - sidebar) * scale).round() as u32,
    ]
}
