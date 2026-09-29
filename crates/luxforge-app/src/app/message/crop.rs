//! The crop draft.
use crate::{app::crop::StagePlan, crop_draft::Handle};
use luxforge_core::PreviewJob;

/// One pointer step of a crop gesture, already mapped to box pixels by the canvas.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CropPointer {
    Begin { handle: Handle, x: f64, y: f64 },
    Drag { x: f64, y: f64, option: bool },
    End,
}

/// Every crop draft change is one message, so a script can drive the whole editor through the
/// update function without simulating a pointer. The angle is not among them: it is the generic
/// stepper of the frame's declared `angle` field, whose
/// [`ControlMessage`](super::control::ControlMessage)s the crop driver takes.
#[derive(Clone, Debug)]
pub(crate) enum CropMessage {
    /// Open a draft on the current stack. Apply, Cancel and Reapply are the one draft lifecycle's
    /// [`DraftMessage`](super::draft::DraftMessage)s, as they are for every other gesture.
    Start,
    /// The truncated preview job for the input stage of a start or a reapply, or for a zoom that
    /// needs a phase of the stage on screen no held frame serves: which one the plan says.
    PreviewReady(StagePlan, Result<Box<PreviewJob>, String>),
    Pointer(CropPointer),
    /// The index of one generated ratio preset.
    Preset(usize),
    CustomWidth(String),
    CustomHeight(String),
    Swap,
    Lock,
    /// The Straighten guide toggle: a drag on the image draws a levelling line instead.
    Guide(bool),
    /// Option (Alt) is held, so a handle scales uniformly about the centre.
    Option(bool),
    /// Space is held, so a drag pans instead of touching the draft.
    Space(bool),
    /// A Space drag asked for this many logical pixels of scroll.
    Pan {
        dx: f32,
        dy: f32,
    },
}
