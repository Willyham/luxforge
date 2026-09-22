//! The view: one file per region, each a pure function from a model to an `Element`. Nothing here
//! reads authoritative state, validates a parameter or calls the owner; the models carry everything
//! the screen shows, so a rendering change cannot change what the editor does.
//!
//! The five-region shell from the [Develop workspace design](../../../../docs/design/develop-workspace.md#layout):
//! a title bar, a middle row of the state panel, the canvas and the tools panel, and a status bar.
//! The two side panels collapse independently through the session's own workspace state; the canvas
//! takes whatever remains. Every region is styled from [`lightwell_ui::theme`], never an ad hoc
//! colour, and the window carries no outer padding: the canvas pads its photo itself.
pub(crate) mod canvas;
mod gallery;
pub(crate) mod palette;
pub(crate) mod state_panel;
pub(crate) mod status_bar;
pub(crate) mod title_bar;
pub(crate) mod tools_panel;

pub(crate) use gallery::{gallery, page_info as gallery_page_info};

use crate::{app::message::Message, crop_draft::CropDraft, state::Workspace};
use iced::{
    Element, Length, Theme,
    widget::{Space, column, container, row, stack},
};
use iced_runtime::image as image_memory;
use lightwell_ui::theme;

/// The title bar's fixed height.
pub(crate) const TITLE_BAR_HEIGHT: f32 = 44.0;
/// The state panel's fixed width, at the layout's left edge.
pub(crate) const STATE_PANEL_WIDTH: f32 = 240.0;
/// The tools panel's fixed width, at the layout's right edge.
pub(crate) const TOOLS_PANEL_WIDTH: f32 = 300.0;
/// The status bar's fixed height.
pub(crate) const STATUS_BAR_HEIGHT: f32 = 26.0;

/// The GPU-resident pixels and the transient draft the canvas borrows for one frame. They are not
/// view-model data: the model says what to draw, these are what it is drawn from.
pub(crate) struct Surfaces<'a> {
    pub(crate) photo: Option<&'a image_memory::Allocation>,
    pub(crate) draft_photo: Option<&'a image_memory::Allocation>,
    /// The clipping overlay's own bounded texture, present only when it belongs to the photograph
    /// on screen. It is a second image laid over the first, never a change to the first.
    pub(crate) overlay: Option<&'a image_memory::Allocation>,
    pub(crate) draft: Option<&'a CropDraft>,
}

pub(crate) fn workspace<'a>(model: &'a Workspace, surfaces: Surfaces<'a>) -> Element<'a, Message> {
    let title = container(
        row![
            title_bar::identity(&model.title),
            Space::new().width(Length::Fill),
            title_bar::view_controls(model),
            Space::new().width(Length::Fill),
            title_bar::actions(&model.title),
        ]
        .spacing(theme::SPACING)
        .align_y(iced::Alignment::Center)
        .width(Length::Fill),
    )
    .height(Length::Fixed(TITLE_BAR_HEIGHT))
    .padding([0.0, theme::SPACING])
    .align_y(iced::alignment::Vertical::Center)
    .style(theme::bar_surface);

    let canvas_area = container(canvas::surface(&model.canvas, surfaces))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::canvas_surface);

    let mut middle = row![].height(Length::Fill);
    if model.title.state_panel_open {
        middle = middle.push(
            container(state_panel::state_panel(&model.panel))
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
            container(tools_panel::tools_panel(&model.tools, &model.histogram))
                .width(Length::Fixed(TOOLS_PANEL_WIDTH))
                .height(Length::Fill)
                .style(theme::panel_surface),
        );
    }

    let status = container(status_bar::status_bar(&model.status))
        .height(Length::Fixed(STATUS_BAR_HEIGHT))
        .padding([0.0, theme::SPACING])
        .align_y(iced::alignment::Vertical::Center)
        .style(theme::panel_surface);

    let screen = column![
        title,
        horizontal_divider(),
        middle,
        horizontal_divider(),
        status
    ];
    match palette::palette(&model.palette) {
        Some(overlay) => stack![screen, overlay].into(),
        None => screen.into(),
    }
}

/// A 1 px vertical rule between the middle row's regions.
fn vertical_divider<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fixed(theme::BORDER_WIDTH))
        .height(Length::Fill)
        .style(divider_style)
        .into()
}

/// A 1 px horizontal rule between the title bar, the middle row and the status bar.
fn horizontal_divider<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(divider_style)
        .into()
}

fn divider_style(_theme: &Theme) -> container::Style {
    container::Style::default().background(theme::BORDER)
}

/// The physical x range of the canvas region inside a captured frame, so evidence can prove the
/// image was drawn where the layout puts it: the state panel's width from the left edge when it is
/// open, and the tools panel's width taken off the right edge when it is open.
pub(crate) fn surface_columns(logical_width: f32, scale: f32, model: &Workspace) -> [u32; 2] {
    let left = if model.title.state_panel_open {
        STATE_PANEL_WIDTH
    } else {
        0.0
    };
    let right_edge = if model.title.tools_panel_open {
        logical_width - TOOLS_PANEL_WIDTH
    } else {
        logical_width
    };
    [
        (left * scale).round() as u32,
        (right_edge * scale).round() as u32,
    ]
}
