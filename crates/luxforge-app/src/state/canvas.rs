//! The canvas model: the photograph, the mode strip, the draft bar and the notices over it.
use crate::{
    mask_draft::{MaskDraft, MaskDraftOp},
    state::{
        Inputs,
        number::number_text,
        tools::{applies, canvas_pick, pick_reachable},
    },
};
use luxforge_core::{
    Availability, CanvasInteraction, ComponentMode, ErrorKind, MASK_MODE, ModuleDescriptor,
    POINTER_MODE, Zoom, mask::commands::MaskListing,
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
    /// The icon's name in the shared icon vocabulary: the host's own for the pointer and Mask, the
    /// module's declared one for a module's mode. An entry without one shows its label.
    pub(crate) icon: Option<String>,
    pub(crate) shortcut: Option<String>,
    pub(crate) selected: bool,
    pub(crate) enabled: bool,
}

/// The bar over the top of the canvas while a mode has a draft open. Its Apply and Cancel are the
/// one draft lifecycle's, whichever gesture is open, since a client holds only one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DraftBar {
    /// The lead, in the accent: the crop mode's title, or the name of the mask a gesture edits.
    pub(crate) title: String,
    /// What a mask gesture edits, after the lead: its component and that component's mode,
    /// `Radial 1 · Add`. `None` for the crop, whose title already says what it edits.
    pub(crate) subject: Option<String>,
    /// The component kind the subject names.
    pub(crate) kind: Option<&'static str>,
    /// The name of that kind's glyph, from the host's kind table, which the view draws beside the
    /// subject.
    pub(crate) icon: Option<&'static str>,
    /// One line of the draft's own numbers, e.g. `300 × 200 px · 0°`.
    pub(crate) readout: String,
    pub(crate) can_apply: bool,
    pub(crate) conflicted: bool,
    /// Why Apply is refused, when it is; the bar shows it in place of nothing.
    pub(crate) apply_reason: Option<String>,
    /// The gesture commits each stroke on release, so the bar ends it with Done — put the brush
    /// down — rather than with an Apply that would have nothing left to commit.
    pub(crate) done: bool,
}

/// What an open mask gesture is about, in the words the draft bar and the status line both use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GestureNames {
    /// The mask's name, or `New mask` while the gesture will create one: the host names a mask
    /// when it is committed, not before.
    pub(crate) mask: String,
    /// The component's name, or its kind's title while the gesture will add one, for the same
    /// reason: a component's ordinal is spent by the commit that makes it.
    pub(crate) component: String,
    /// The component's mode: the one the gesture will add it with, or the stored one it has.
    pub(crate) mode: &'static str,
}

/// Name what the open gesture edits from the masks the listing holds. A mask or component the
/// listing does not hold yet (a create in flight, a listing a step behind) is named by what the
/// gesture will make rather than left blank.
pub(crate) fn gesture_names(draft: &MaskDraft, listing: Option<&MaskListing>) -> GestureNames {
    let report = draft.mask.as_ref().and_then(|id| {
        listing.and_then(|listing| listing.masks.iter().find(|report| &report.id == id))
    });
    let component = draft.component.as_ref().and_then(|id| {
        report.and_then(|report| {
            report
                .components
                .iter()
                .find(|component| &component.id == id)
        })
    });
    let mode = match (draft.op, component) {
        (MaskDraftOp::Add(mode), _) => mode,
        (_, Some(component)) => component.mode,
        // A mask's first component is always an add, which is what a create makes.
        (MaskDraftOp::Create | MaskDraftOp::Set, None) => ComponentMode::Add,
    };
    GestureNames {
        mask: match (&draft.mask, report) {
            (_, Some(report)) => report.name.clone(),
            (None, None) => MaskDraftOp::Create.label().to_owned(),
            (Some(_), None) => "Mask".to_owned(),
        },
        component: component
            .map(|component| component.name.clone())
            .unwrap_or_else(|| luxforge_core::mask::kind_title(draft.kind())),
        mode: MaskDraftOp::Add(mode).label(),
    }
}

/// A notice's tone: whether it only says what happened, needs a decision, or reports a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeTone {
    Neutral,
    Warning,
    Error,
}

/// A notice's leading icon: the spark for a change another client made, the triangle for the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeIcon {
    Spark,
    Triangle,
}

/// What a notice's button does. Every one is an existing operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoticeAction {
    /// The two decisions a conflicted draft waits for, whichever gesture holds it.
    DiscardGesture,
    ReapplyGesture,
    ReturnCurrent,
    /// Grant exactly the scope the open consent notice names, then retry what was refused.
    AllowConsent,
    /// Record that the person did not allow that scope.
    DenyConsent,
    /// Locate original…: point the open photograph at a file the person chooses (`source.locate`),
    /// then open it again from that file.
    LocateOriginal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) tone: NoticeTone,
    pub(crate) icon: NoticeIcon,
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
    let editable = inputs.edit_refusal.is_none();
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
            icon: Some("pointer".into()),
            shortcut: Some("V".into()),
            selected: inputs.session.workspace.mode == POINTER_MODE,
            enabled: true,
        },
        // Mask is a host mode, not a module's: a mask is a host object in the recipe, so no module
        // declares its canvas and the strip offers it whatever is registered. Its icon is the
        // host's declaration for the same reason.
        ModeEntry {
            id: MASK_MODE.into(),
            label: "Mask".into(),
            icon: Some("mask".into()),
            shortcut: Some("M".into()),
            selected: inputs.session.workspace.mode == MASK_MODE,
            enabled: editable,
        },
    ];
    modes.extend(
        inputs
            .modules
            .iter()
            .filter(|module| {
                module.is_available() && applies(module, inputs.document.state.as_ref())
            })
            .filter(|module| !module.developer || inputs.developer)
            .filter_map(|module| match module.canvas.as_ref() {
                Some(canvas @ CanvasInteraction::CropFrame { .. }) => Some(ModeEntry {
                    id: module.id.clone(),
                    label: canvas.title().to_owned(),
                    icon: canvas.icon().map(str::to_owned),
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
        scale_factor: inputs.view_state.scale_factor,
        dimensions: inputs.dimensions,
        modes,
        thirds: inputs.session.workspace.thirds,
        draft_bar: draft_bar(inputs),
        notices: notices(inputs),
        // A click on the photograph belongs to the canvas mode that is on screen, so the surface
        // takes picks only while a mode declaring one is active, and only while the module whose
        // mode it is applies to the photo's source kind and a resolved picker names that mode for
        // the photo and target. A host mode belongs to no module and applies to every photo.
        picking: editable
            && canvas_pick(inputs.modules, &inputs.session.workspace.mode).is_some()
            && pick_reachable(
                inputs.modules,
                inputs.document.state.as_ref(),
                inputs.target,
                &inputs.session.workspace.mode,
            ),
        pointer: inputs.hover.pointer,
        surface_mode: if inputs.crop_section.space {
            SurfaceMode::Pan
        } else if inputs.crop_section.guide {
            SurfaceMode::Guide
        } else {
            SurfaceMode::Frame
        },
        option: inputs.crop_section.option,
        masking: inputs.session.workspace.mode == MASK_MODE,
        // A pick taken on a mask keeps its Masks panel on screen, bound to that mask.
        mask_panel: mask_workspace(&inputs.session.workspace.mode) || inputs.target.is_some(),
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
    if inputs.document.state.is_some()
        && let Some(error) = inputs.render_error
    {
        let reason = render_notice(inputs.modules, error, true)
            .map(|notice| notice.body)
            .unwrap_or_else(|| error.detail.clone());
        return PhotoView::Empty(format!("Preview unavailable: {reason}"));
    }
    // A photograph of the development set opening with no preview decoded for it yet.
    if let Some(name) = &inputs.develop.switching {
        return PhotoView::Empty(format!("Opening {name}\u{2026}"));
    }
    PhotoView::default()
}

/// The bar over the open mask shape gesture or crop draft: its title, its own numbers, and whether
/// Apply can run, which is the app's one release refusal and never a rule of the bar's own.
fn draft_bar(inputs: &Inputs<'_>) -> Option<DraftBar> {
    if let Some(draft) = inputs.mask_draft {
        return Some(mask_draft_bar(inputs, draft));
    }
    let (title, readout) = match inputs.draft {
        Some(draft) => (
            inputs
                .modules
                .iter()
                .find(|module| module.id == inputs.session.workspace.mode)
                .and_then(|module| module.canvas.as_ref())
                .map(CanvasInteraction::title)
                .unwrap_or("Crop")
                .to_owned(),
            match draft.output() {
                Ok(rect) => format!(
                    "{} × {} px · {}°",
                    rect.width,
                    rect.height,
                    number_text(draft.stage.angle)
                ),
                Err(error) => error.detail.clone(),
            },
        ),
        None => return None,
    };
    Some(DraftBar {
        title,
        subject: None,
        kind: None,
        icon: None,
        readout,
        can_apply: inputs.apply_refusal.is_none(),
        conflicted: inputs.gesture_conflicted,
        apply_reason: inputs.apply_refusal.clone(),
        done: false,
    })
}

/// The bar over an open mask gesture: the mask in the accent, the component and its mode beside its
/// kind's icon, then the kind's own readout. A painted gesture ends with Done, because each stroke
/// already committed on release; its Apply refusal is not stated, because it offers no Apply.
fn mask_draft_bar(inputs: &Inputs<'_>, draft: &MaskDraft) -> DraftBar {
    let names = gesture_names(draft, inputs.document.masks.as_ref());
    let done = draft.paints();
    DraftBar {
        title: names.mask,
        subject: Some(format!("{} \u{b7} {}", names.component, names.mode)),
        kind: Some(draft.kind()),
        icon: luxforge_core::mask::kind_icon(draft.kind()),
        readout: draft.readout(),
        can_apply: done || inputs.apply_refusal.is_none(),
        conflicted: inputs.gesture_conflicted,
        apply_reason: inputs.apply_refusal.clone().filter(|_| !done),
        done,
    }
}

/// The cards over the top of the canvas. Each one names its cause and carries only the actions the
/// core allows; a render failure is turned into a notice here rather than in the view, because the
/// mapping from an error kind to a cause is editing knowledge.
fn notices(inputs: &Inputs<'_>) -> Vec<Notice> {
    let mut notices = Vec::new();
    // A capability operation the desktop started was refused for want of consent: the person
    // decides here, and nothing else on screen waits for the answer.
    notices.extend(crate::state::capabilities::consent_notice(inputs));
    // Every gesture's draft conflicts the same way and offers the same two decisions, so there is
    // one notice, naming the gesture that is open. Only one draft exists at a time.
    if inputs.gesture_conflicted
        && let Some(gesture) = inputs.gesture
    {
        let revision = inputs.document.state.as_ref().map(|state| state.revision);
        notices.push(Notice {
            tone: NoticeTone::Warning,
            icon: NoticeIcon::Spark,
            title: "Changed elsewhere".into(),
            body: match revision {
                Some(revision) => format!(
                    "Another client committed revision {revision} while your {gesture} was open. Your {gesture} is kept."
                ),
                None => format!(
                    "Another client committed while your {gesture} was open. Your {gesture} is kept."
                ),
            },
            actions: vec![
                ("Discard draft".into(), NoticeAction::DiscardGesture),
                ("Reapply".into(), NoticeAction::ReapplyGesture),
            ],
        });
    }
    if inputs.draft.is_some() && !super::at_current(inputs.document.state.as_ref(), inputs.session)
    {
        notices.push(Notice {
            tone: NoticeTone::Neutral,
            icon: NoticeIcon::Triangle,
            title: "Draft paused".into(),
            body: "A historical state is shown; return to current to keep editing.".into(),
            actions: vec![("Return to current".into(), NoticeAction::ReturnCurrent)],
        });
    }
    if let Some(error) = inputs.render_error {
        notices.extend(render_notice(
            inputs.modules,
            error,
            inputs.document.state.is_some(),
        ));
    }
    notices
}

/// The notice one failed preview produces, when its kind is one the workspace explains. What the
/// failure is comes from its kind and data, never from its message, which is only shown. An
/// original that is not there offers Locate original… while a photograph is `open`.
fn render_notice(
    modules: &[ModuleDescriptor],
    error: &luxforge_core::Error,
    open: bool,
) -> Option<Notice> {
    let detail = &error.detail;
    // A stack whose provider is missing is reported, never rendered without the effect.
    if let Some(effect) = error.unavailable_effect_id() {
        return Some(Notice {
            tone: NoticeTone::Neutral,
            icon: NoticeIcon::Triangle,
            title: "Preview is stale".into(),
            body: unavailable_body(modules, effect),
            actions: Vec::new(),
        });
    }
    match error.kind {
        // The notice names the cause and offers to point the photograph at its original's new
        // place: the same Locate as Missing originals' rows.
        ErrorKind::SourceUnavailable | ErrorKind::FileAccess => Some(Notice {
            tone: NoticeTone::Error,
            icon: NoticeIcon::Triangle,
            title: "Original not found".into(),
            body: detail.to_owned(),
            actions: if open {
                vec![(
                    "Locate original\u{2026}".into(),
                    NoticeAction::LocateOriginal,
                )]
            } else {
                Vec::new()
            },
        }),
        ErrorKind::ResourceLimit => Some(Notice {
            tone: NoticeTone::Error,
            icon: NoticeIcon::Triangle,
            title: "Rendering limit".into(),
            body: detail.to_owned(),
            actions: Vec::new(),
        }),
        _ => None,
    }
}

/// The module that provides the effect the failure names, reported by title and reason. The effect
/// identity is the fallback, so a layer whose provider was never registered is still named.
fn unavailable_body(modules: &[ModuleDescriptor], effect: &str) -> String {
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
