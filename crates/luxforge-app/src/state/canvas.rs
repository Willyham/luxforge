//! The canvas model: the photograph, the mode strip, the draft bar and the notices over it.
use crate::state::{Inputs, number::number_text, tools::canvas_pick};
use luxforge_core::{
    Availability, CanvasInteraction, ErrorKind, MASK_MODE, ModuleDescriptor, POINTER_MODE, Zoom,
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
    /// The bar belongs to a mask shape gesture rather than to the crop draft, so Apply and Cancel
    /// reach the gesture that is actually open.
    pub(crate) mask: bool,
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
    /// The same two decisions for the open slider or mask gesture's core draft.
    DiscardGesture,
    ReapplyGesture,
    ReturnCurrent,
    /// Grant exactly the scope the open consent notice names, then retry what was refused.
    AllowConsent,
    /// Record that the person did not allow that scope.
    DenyConsent,
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
    /// A module declares a pick and the current state can be edited. The view's whole share of the
    /// work is turning a reported point into a pixel of the raster on screen; the core maps that
    /// to the content stage.
    pub(crate) picking: bool,
    pub(crate) pointer: Option<(u32, u32)>,
    pub(crate) surface_mode: SurfaceMode,
    /// Option is held, so a handle scales about the centre.
    pub(crate) option: bool,
    /// Mask mode is active, so the tools panel shows the Masks panel and the canvas draws the
    /// selected mask's handles and overlay.
    pub(crate) masking: bool,
    /// The tools panel shows the Masks panel: Mask mode, or one of the host's picks, which is
    /// entered from that panel and must not hide it.
    pub(crate) mask_panel: bool,
}

/// Whether the workspace is on a mask: Mask mode itself, or one of the host's own canvas picks,
/// which fill part of a mask and are entered from the Masks panel.
///
/// The panel a pick was started from has to stay on screen while the pick is taken — a button that
/// hides the list it belongs to is not a control — so the tools panel, the maskable sections and the
/// mask each adjustment is bound to all read this. The canvas's own `masking` flag is *not* this: a
/// pick mode takes a click, while Mask mode drives a gesture, and only one of the two may own the
/// pointer.
pub(crate) fn mask_workspace(mode: &str) -> bool {
    mode == MASK_MODE || luxforge_core::mask::commands::canvas_pick(mode).is_some()
}

pub(crate) fn derive(inputs: &Inputs<'_>) -> CanvasModel {
    let editable = inputs.state.is_some() && inputs.session.preview.can_edit() && !inputs.busy;
    let drafting = inputs.drafting;
    // Pointer first, then one entry per available module that takes the whole canvas over — a
    // declared crop frame — in registry order, and the view overlays after them. A pick mode is
    // not a canvas takeover: it belongs beside the controls its pick fills, so its module declares
    // a picker control in its own panel and the strip does not list it. Developer modules stay out
    // of the strip unless the run asked for them.
    let mut modes = vec![
        ModeEntry {
            id: POINTER_MODE.into(),
            label: "Pointer".into(),
            shortcut: Some("V".into()),
            selected: inputs.session.workspace.mode == POINTER_MODE,
            enabled: true,
        },
        // Mask is a host mode, not a module's: a mask is a host object in the recipe, so no module
        // declares its canvas and the strip offers it whatever is registered.
        ModeEntry {
            id: MASK_MODE.into(),
            label: "Mask".into(),
            shortcut: Some("M".into()),
            selected: inputs.session.workspace.mode == MASK_MODE,
            enabled: editable,
        },
    ];
    modes.extend(
        inputs
            .modules
            .iter()
            .filter(|module| module.is_available())
            .filter(|module| !module.developer || inputs.developer)
            .filter_map(|module| match module.canvas.as_ref() {
                Some(canvas @ CanvasInteraction::CropFrame { .. }) => Some(ModeEntry {
                    id: module.id.clone(),
                    label: canvas.title().to_owned(),
                    shortcut: canvas.shortcut().map(str::to_owned),
                    selected: inputs.session.workspace.mode == module.id,
                    enabled: editable,
                }),
                Some(CanvasInteraction::PointPick { .. })
                | Some(CanvasInteraction::SampleApply { .. })
                | None => None,
            }),
    );
    CanvasModel {
        photo: photo_view(inputs, drafting),
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
        // A click on the photograph belongs to the canvas mode that is on screen, so the surface
        // takes picks only while a mode declaring one is active. A sensor pick also needs a RAW
        // source behind it, which is the one thing the descriptor cannot say.
        picking: editable
            && canvas_pick(inputs.modules, &inputs.session.workspace.mode).is_some()
            && (inputs.session.workspace.mode != "luxforge.raw"
                || inputs.state.is_some_and(|state| {
                    matches!(state.asset.source, luxforge_core::SourceKind::Raw { .. })
                })),
        pointer: inputs.pointer,
        surface_mode: if inputs.crop_space {
            SurfaceMode::Pan
        } else if inputs.crop_guide {
            SurfaceMode::Guide
        } else {
            SurfaceMode::Frame
        },
        option: inputs.crop_option,
        masking: inputs.session.workspace.mode == MASK_MODE,
        mask_panel: mask_workspace(&inputs.session.workspace.mode),
    }
}

/// What the surface shows when there is no photograph to draw: a photo (this frame or the last
/// one still on the GPU), the crop draft's own frame, or, failing both, a placeholder line. A
/// photograph that is open but whose last render failed says so and names the short reason,
/// rather than asking to open one that already is.
fn photo_view(inputs: &Inputs<'_>, drafting: bool) -> PhotoView {
    if drafting {
        return PhotoView::Draft;
    }
    if inputs.photo && inputs.dimensions.is_some() {
        return PhotoView::Plain;
    }
    if inputs.state.is_some()
        && let Some((kind, detail)) = inputs.render_error
    {
        let reason = render_notice(inputs.modules, *kind, detail)
            .map(|notice| notice.body)
            .unwrap_or_else(|| detail.clone());
        return PhotoView::Empty(format!("Preview unavailable: {reason}"));
    }
    PhotoView::default()
}

fn draft_bar(inputs: &Inputs<'_>) -> Option<DraftBar> {
    // A mask shape gesture takes the bar over while it is open: only one draft exists per client, so
    // the two can never both be there.
    if let Some(draft) = inputs.mask_draft {
        let readout = draft
            .values()
            .into_iter()
            .map(|(name, value)| format!("{name} {value:.3}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let apply_reason = if inputs.gesture_conflicted {
            Some("Changed elsewhere: discard the gesture or reapply it".to_owned())
        } else if !inputs.session.preview.can_edit() {
            Some("Return to the current state to apply".to_owned())
        } else if inputs.busy {
            Some("Waiting for the last request".to_owned())
        } else {
            None
        };
        return Some(DraftBar {
            mask: true,
            title: format!("{} · {}", draft.op.label(), draft.kind()),
            readout,
            can_apply: apply_reason.is_none(),
            conflicted: inputs.gesture_conflicted,
            apply_reason,
        });
    }
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
    let apply_reason = if inputs.gesture_conflicted {
        Some("Changed elsewhere: discard the draft or reapply it".into())
    } else if !inputs.session.preview.can_edit() {
        Some("Return to the current state to apply".into())
    } else if inputs.busy {
        Some("Waiting for the last request".into())
    } else {
        None
    };
    Some(DraftBar {
        mask: false,
        title,
        readout,
        can_apply: apply_reason.is_none(),
        conflicted: inputs.gesture_conflicted,
        apply_reason,
    })
}

/// The cards over the top of the canvas. Each one names its cause and carries only the actions the
/// core allows; a render failure is turned into a notice here rather than in the view, because the
/// mapping from an error kind to a cause is editing knowledge.
fn notices(inputs: &Inputs<'_>) -> Vec<Notice> {
    let mut notices = Vec::new();
    // A capability operation the desktop started was refused for want of consent: the person
    // decides here, and nothing else on screen waits for the answer.
    notices.extend(crate::state::capabilities::consent_notice(inputs));
    // A slider gesture's draft conflicts exactly as the crop draft does, and offers the same two
    // decisions. Only one draft exists at a time, so only one of these two ever appears.
    if inputs.slider_draft.is_some_and(|draft| draft.conflicted) {
        let revision = inputs.state.map(|state| state.revision);
        notices.push(Notice {
            tone: NoticeTone::Warning,
            title: "Changed elsewhere".into(),
            body: match revision {
                Some(revision) => format!(
                    "Another client committed revision {revision} while your slider draft was open. Your draft is kept."
                ),
                None => "Another client committed while your slider draft was open. Your draft is kept.".into(),
            },
            actions: vec![
                ("Discard".into(), NoticeAction::DiscardGesture),
                ("Reapply".into(), NoticeAction::ReapplyGesture),
            ],
        });
    }
    if inputs.mask_draft.is_some() && inputs.gesture_conflicted {
        let revision = inputs.state.map(|state| state.revision);
        notices.push(Notice {
            tone: NoticeTone::Warning,
            title: "Changed elsewhere".into(),
            body: match revision {
                Some(revision) => format!(
                    "Another client committed revision {revision} while your mask gesture was open. Your gesture is kept."
                ),
                None => "Another client committed while your mask gesture was open. Your gesture is kept.".into(),
            },
            actions: vec![
                ("Discard".into(), NoticeAction::DiscardGesture),
                ("Reapply".into(), NoticeAction::ReapplyGesture),
            ],
        });
    }
    if inputs.draft.is_some() && inputs.gesture_conflicted {
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
