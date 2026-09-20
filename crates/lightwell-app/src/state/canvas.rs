//! The canvas model: the photograph, the mode strip, the draft bar and the notices over it.
use crate::state::{Inputs, tools::point_pick};
use lightwell_core::{CanvasInteraction, POINTER_MODE, Zoom};

/// How the photograph is sized on the surface. The view never reads the session itself.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum ZoomView {
    #[default]
    Fit,
    Percent(f32),
}

/// What the surface shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PhotoView {
    /// Nothing is open: this line is shown instead.
    Empty(String),
    Plain,
    /// The crop layer's input stage under the draft's frame.
    Draft,
}

impl Default for PhotoView {
    fn default() -> Self {
        Self::Empty("Open a photograph".into())
    }
}

/// What a drag on the image does right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SurfaceMode {
    #[default]
    Frame,
    /// Space is held: the drag scrolls the surrounding scrollable.
    Pan,
    /// The Straighten toggle is on: the drag draws a levelling guide.
    Guide,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModeEntry {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) shortcut: Option<String>,
    pub(crate) selected: bool,
    pub(crate) enabled: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftBar {
    pub(crate) title: String,
    pub(crate) readout: String,
    pub(crate) can_apply: bool,
    pub(crate) conflicted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeTone {
    Conflict,
    Unavailable,
}

/// What a notice's button does. Every one is an existing operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoticeAction {
    DiscardDraft,
    ReapplyDraft,
    ReturnCurrent,
}

#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) tone: NoticeTone,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) actions: Vec<(String, NoticeAction)>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CanvasModel {
    pub(crate) photo: PhotoView,
    pub(crate) zoom: ZoomView,
    pub(crate) scale_factor: f32,
    pub(crate) dimensions: Option<(u32, u32)>,
    pub(crate) modes: Vec<ModeEntry>,
    pub(crate) thirds: bool,
    pub(crate) draft_bar: Option<DraftBar>,
    pub(crate) notices: Vec<Notice>,
    /// A module declares a pick and the current state can be edited.
    pub(crate) picking: bool,
    pub(crate) pointer: Option<(u32, u32)>,
    pub(crate) surface_mode: SurfaceMode,
    /// Option is held, so a handle scales about the centre.
    pub(crate) option: bool,
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> CanvasModel {
    let editable = inputs.state.is_some() && inputs.session.preview.can_edit() && !inputs.busy;
    let mut modes = vec![ModeEntry {
        id: POINTER_MODE.into(),
        label: "Pointer".into(),
        shortcut: Some("V".into()),
        selected: inputs.session.workspace.mode == POINTER_MODE,
        enabled: true,
    }];
    modes.extend(
        inputs
            .modules
            .iter()
            .filter(|module| module.is_available())
            .filter_map(|module| {
                module.canvas.as_ref().map(|canvas| ModeEntry {
                    id: module.id.clone(),
                    label: canvas.title().to_owned(),
                    shortcut: canvas.shortcut().map(str::to_owned),
                    selected: inputs.session.workspace.mode == module.id,
                    enabled: editable,
                })
            }),
    );
    CanvasModel {
        photo: if inputs.drafting {
            PhotoView::Draft
        } else if inputs.photo && inputs.dimensions.is_some() {
            PhotoView::Plain
        } else {
            PhotoView::default()
        },
        zoom: match inputs.session.preview.view.zoom {
            Zoom::Fit => ZoomView::Fit,
            Zoom::Percent { value } => ZoomView::Percent(value),
        },
        scale_factor: inputs.scale_factor,
        dimensions: inputs.dimensions,
        modes,
        thirds: inputs.session.workspace.thirds,
        draft_bar: draft_bar(inputs),
        notices: notices(inputs),
        picking: editable && point_pick(inputs.modules).is_some(),
        pointer: inputs.pointer,
        surface_mode: if inputs.crop_space {
            SurfaceMode::Pan
        } else if inputs.crop_guide {
            SurfaceMode::Guide
        } else {
            SurfaceMode::Frame
        },
        option: inputs.crop_option,
    }
}

fn draft_bar(inputs: &Inputs<'_>) -> Option<DraftBar> {
    let draft = inputs.draft?;
    let title = inputs
        .modules
        .iter()
        .find(|module| module.id == inputs.session.workspace.mode)
        .and_then(|module| module.canvas.as_ref())
        .map(CanvasInteraction::title)
        .unwrap_or("Crop")
        .to_owned();
    let readout = match draft.output() {
        Ok(rect) => format!("{} × {} px", rect.width, rect.height),
        Err(error) => error.detail.clone(),
    };
    Some(DraftBar {
        title,
        readout,
        can_apply: !draft.conflicted && inputs.session.preview.can_edit() && !inputs.busy,
        conflicted: draft.conflicted,
    })
}

fn notices(inputs: &Inputs<'_>) -> Vec<Notice> {
    let mut notices = Vec::new();
    if inputs.draft.is_some_and(|draft| draft.conflicted) {
        notices.push(Notice {
            tone: NoticeTone::Conflict,
            title: "Changed elsewhere".into(),
            body: "Another client committed while this draft was open.".into(),
            actions: vec![
                ("Discard draft".into(), NoticeAction::DiscardDraft),
                ("Reapply".into(), NoticeAction::ReapplyDraft),
            ],
        });
    }
    if inputs.draft.is_some() && !inputs.session.preview.can_edit() {
        notices.push(Notice {
            tone: NoticeTone::Unavailable,
            title: "Draft paused".into(),
            body: "A historical state is shown; return to current to keep editing.".into(),
            actions: vec![("Return to current".into(), NoticeAction::ReturnCurrent)],
        });
    }
    for module in inputs.modules {
        if let lightwell_core::Availability::Unavailable { reason } = &module.availability {
            notices.push(Notice {
                tone: NoticeTone::Unavailable,
                title: format!("{} is unavailable", module.title),
                body: reason.clone(),
                actions: Vec::new(),
            });
        }
    }
    notices
}
