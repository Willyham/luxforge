//! The canvas model: the photograph, the mode strip, the draft bar and the notices over it.
use crate::{
    app::fields::number_text,
    state::{Inputs, tools::point_pick},
};
use lightwell_core::{
    Availability, CanvasInteraction, ErrorKind, ModuleDescriptor, POINTER_MODE, Zoom,
};

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

/// One entry of the floating mode strip: the pointer, or a module that declares a canvas
/// interaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModeEntry {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) shortcut: Option<String>,
    pub(crate) selected: bool,
    pub(crate) enabled: bool,
}

/// The bar over the top of the canvas while a mode has a draft open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftBar {
    pub(crate) title: String,
    /// One line of the draft's own numbers, e.g. `300 × 200 px · 0°`.
    pub(crate) readout: String,
    pub(crate) can_apply: bool,
    pub(crate) conflicted: bool,
    /// Why Apply is refused, when it is; the bar shows it in place of nothing.
    pub(crate) apply_reason: Option<String>,
}

/// A notice's tone: whether it needs a decision or only says what happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeTone {
    Neutral,
    Warning,
}

/// What a notice's button does. Every one is an existing operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoticeAction {
    DiscardDraft,
    ReapplyDraft,
    ReturnCurrent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) tone: NoticeTone,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) actions: Vec<(String, NoticeAction)>,
}

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
    let drafting = inputs.drafting;
    // Pointer first, then one entry per available module that declares a canvas interaction, in
    // registry order. Developer modules stay out of the strip unless the run asked for them, so the
    // default workspace shows a photo editor's modes and nothing else.
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
            .filter(|module| !module.developer || inputs.developer)
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
        photo: if drafting {
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
        Ok(rect) => format!(
            "{} × {} px · {}°",
            rect.width,
            rect.height,
            number_text(draft.stage.angle)
        ),
        Err(error) => error.detail.clone(),
    };
    let apply_reason = if draft.conflicted {
        Some("Changed elsewhere: discard the draft or reapply it".into())
    } else if !inputs.session.preview.can_edit() {
        Some("Return to the current state to apply".into())
    } else if inputs.busy {
        Some("Waiting for the last request".into())
    } else {
        None
    };
    Some(DraftBar {
        title,
        readout,
        can_apply: apply_reason.is_none(),
        conflicted: draft.conflicted,
        apply_reason,
    })
}

/// The cards over the top of the canvas. Each one names its cause and carries only the actions the
/// core allows; a render failure is turned into a notice here rather than in the view, because the
/// mapping from an error kind to a cause is editing knowledge.
fn notices(inputs: &Inputs<'_>) -> Vec<Notice> {
    let mut notices = Vec::new();
    if inputs.draft.is_some_and(|draft| draft.conflicted) {
        let revision = inputs.state.map(|state| state.revision);
        notices.push(Notice {
            tone: NoticeTone::Warning,
            title: "Changed elsewhere".into(),
            body: match revision {
                Some(revision) => format!(
                    "Another client committed revision {revision} while your crop draft was open. Your draft is kept."
                ),
                None => "Another client committed while your crop draft was open. Your draft is kept.".into(),
            },
            actions: vec![
                ("Discard".into(), NoticeAction::DiscardDraft),
                ("Reapply".into(), NoticeAction::ReapplyDraft),
            ],
        });
    }
    if inputs.draft.is_some() && !inputs.session.preview.can_edit() {
        notices.push(Notice {
            tone: NoticeTone::Neutral,
            title: "Draft paused".into(),
            body: "A historical state is shown; return to current to keep editing.".into(),
            actions: vec![("Return to current".into(), NoticeAction::ReturnCurrent)],
        });
    }
    if let Some((kind, detail)) = inputs.render_error {
        notices.extend(render_notice(inputs.modules, *kind, detail));
    }
    notices
}

/// The notice one failed preview produces, when its kind is one the workspace explains.
fn render_notice(modules: &[ModuleDescriptor], kind: ErrorKind, detail: &str) -> Option<Notice> {
    match kind {
        // A stack whose provider is missing is reported, never rendered without the effect.
        ErrorKind::Incompatible if detail.starts_with(UNAVAILABLE_EFFECT) => Some(Notice {
            tone: NoticeTone::Neutral,
            title: "Preview is stale".into(),
            body: unavailable_body(modules, detail),
            actions: Vec::new(),
        }),
        // Locate is a later feature, so this notice names the cause and offers nothing.
        ErrorKind::SourceUnavailable | ErrorKind::FileAccess => Some(Notice {
            tone: NoticeTone::Warning,
            title: "Original not found".into(),
            body: detail.to_owned(),
            actions: Vec::new(),
        }),
        ErrorKind::ResourceLimit => Some(Notice {
            tone: NoticeTone::Warning,
            title: "Rendering limit".into(),
            body: detail.to_owned(),
            actions: Vec::new(),
        }),
        _ => None,
    }
}

/// The prefix the registry writes when a stored layer's provider is not registered.
const UNAVAILABLE_EFFECT: &str = "unavailable effect ";

/// The module that provides the effect the failure names, reported by title and reason. The effect
/// identity is the fallback, so a layer whose provider was never registered is still named.
fn unavailable_body(modules: &[ModuleDescriptor], detail: &str) -> String {
    let Some(effect) = detail
        .strip_prefix(UNAVAILABLE_EFFECT)
        .and_then(|rest| rest.split_whitespace().next())
    else {
        return detail.to_owned();
    };
    let module = modules
        .iter()
        .find(|module| module.effects.iter().any(|known| known.id == effect));
    match module {
        Some(module) => match &module.availability {
            Availability::Unavailable { reason } => {
                format!("{} is unavailable: {reason}", module.title)
            }
            Availability::Available => format!("{} did not provide this layer", module.title),
        },
        None => format!("No registered module provides {effect}"),
    }
}
