//! The shapes a component kind's declared geometry parameters take.
//!
//! A component kind declares its own [`ParameterDescriptor`]s once, in its own module beside the
//! parser that range-checks the same numbers (`docs/design/masking.md#host-commands`). This file
//! holds the handful of shapes those declarations are built from, so the legal range a parser
//! enforces and the range a generated control offers are one constant read twice and never two that
//! could drift.
//!
//! Every shape is a [`crate::ParameterKind::Number`] with the kind's own bound, plus the display
//! hints a generated number field needs to be usable: a `step` a key press moves by, a `fine_step`
//! for the modifier, the `precision` a field shows, and the `soft_min`/`soft_max` a slider spans
//! when the legal range is wider than the range a person works in. A hint is a hint: the host
//! validates it is finite and positive and stores it, and never rounds a request to it.
use super::{DISTANCE_MAX, DISTANCE_MIN, POSITION_MAX, POSITION_MIN};
use crate::ParameterDescriptor;

/// One normalized stored position: a fraction of the content stage, with one stage extent of
/// overshoot legal on each side, because a gradient dragged from off the canvas and a radial centred
/// outside the frame are ordinary edits. The soft range is the frame itself, so a slider spans what
/// a person works in while a number field still reaches the overshoot.
pub(super) fn position(name: &str, required: bool, notes: &str) -> ParameterDescriptor {
    ParameterDescriptor::number(name, POSITION_MIN, POSITION_MAX)
        .required(required)
        .notes(notes)
        .unit("frame")
        .step(0.01)
        .precision(4)
        .soft_min(0.0)
        .soft_max(1.0)
        .fine_step(0.001)
}

/// One stored distance in mask-space units, where one unit is the content stage's **height** on both
/// axes, so a circle is a circle at any aspect ratio. The floor is what bounds every divisor a
/// falloff takes and the ceiling covers the whole stage from any point; the soft range is the part
/// of it a drawn shape occupies.
pub(super) fn distance(name: &str, required: bool, notes: &str) -> ParameterDescriptor {
    ParameterDescriptor::number(name, DISTANCE_MIN, DISTANCE_MAX)
        .required(required)
        .notes(notes)
        .unit("h")
        .step(0.01)
        .precision(4)
        .soft_min(0.01)
        .soft_max(1.0)
        .fine_step(0.001)
}

/// One stored angle in degrees over a single turn, so two payloads that draw the same shape compare
/// equal and a slider has ends. Zero is upright, which is what a double-click returns it to.
pub(super) fn angle(
    name: &str,
    required: bool,
    min: f64,
    max: f64,
    notes: &str,
) -> ParameterDescriptor {
    ParameterDescriptor::number(name, min, max)
        .required(required)
        .notes(notes)
        .unit("deg")
        .step(1.0)
        .precision(2)
        .fine_step(0.1)
        .zero(0.0)
}

/// One stored percentage, the spelling every 0..100 slider in the editor already takes.
pub(super) fn percentage(
    name: &str,
    required: bool,
    min: f64,
    max: f64,
    zero: f64,
    notes: &str,
) -> ParameterDescriptor {
    ParameterDescriptor::number(name, min, max)
        .required(required)
        .notes(notes)
        .unit("%")
        .step(1.0)
        .precision(1)
        .fine_step(0.1)
        .zero(zero)
}
