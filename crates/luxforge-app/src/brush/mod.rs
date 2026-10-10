//! The one brush system on the desktop: the capture, bindings, gestures and mapping every painted
//! path shares, whichever consumer it is painted for (`docs/design/masking.md`, "One brush
//! system").
//!
//! The mask brush paints through the host's `mask.add-stroke`; a module paints through the
//! `brush-mask` canvas it declares ([`luxforge_core::CanvasInteraction::BrushMask`]). Both capture a
//! path with [`StrokeCapture`] on the host's own grid, both place a pointer through the core's
//! geometry map ([`ContentMap`]), and both clamp and send their settings by what their
//! [`BrushBinding`] names, so a desktop stroke and an agent's semantic request arrive at the same
//! stored path. The gesture rules a declared brush's workspace needs — click or drag, `⇧`, `⌘` and
//! `⌥`, the hit order of pins and handles, a persistent set source and handle drags — are
//! [`gesture`]'s. Nothing here holds a framework type or calls the host.
//!
//! The declared brush's mode, band and canvas are the Corrections workspace's
//! ([`crate::app::corrections`]).
mod binding;
mod capture;
mod gesture;
mod map;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;

pub(crate) use binding::{BrushBinding, BrushSettings, clamp, step};
#[cfg(test)]
pub(crate) use binding::EditRequest;
pub(crate) use capture::StrokeCapture;
#[cfg(test)]
pub(crate) use capture::counts;
#[cfg(test)]
pub(crate) use gesture::{CLICK_SLOP, HIT_RADIUS};
pub(crate) use gesture::{
    BrushGesture, Handle, HandleChange, HandleEdit, HeldOperation, Hit,
    Modifiers, Moved, Released, SourceChoice, ViewPoint, hit,
};
pub(crate) use map::{ContentMap, Displayed, MapRefusal};

/// The brush cursor's two rings for a radius (units of the content height) and a feather (percent
/// of the radius over which coverage falls to zero): the solid outer ring where coverage ends and
/// the dashed inner ring where the ramp starts, each with its alpha. A feather of 100 has no inner
/// ring. Every consumer's cursor is these rings, drawn through the same map as its path.
pub(crate) fn cursor_rings(radius: f64, feather: f64) -> impl Iterator<Item = (f64, f32, bool)> {
    [(1.0, 0.9, false), (1.0 - feather / 100.0, 0.5, true)]
        .into_iter()
        .filter(|(scale, _, _)| *scale > 0.0)
        .map(move |(scale, alpha, dashed)| (radius * scale, alpha, dashed))
}
