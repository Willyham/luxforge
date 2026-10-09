//! The one brush system on the desktop: the capture, bindings, gestures and mapping every painted
//! path shares, whichever consumer it is painted for (`docs/design/masking.md`, "One brush
//! system").
//!
//! The mask brush paints through the host's `mask.add-stroke`; a module paints through the
//! `brush-mask` canvas it declares ([`luxforge_core::CanvasInteraction::BrushMask`]). Both capture a
//! path with [`StrokeCapture`] on the host's own grid, both map a pointer through the core's
//! geometry map ([`ContentMap`]), and both build their request from what their [`BrushBinding`]
//! names, so a desktop stroke and an agent's semantic request arrive at the same stored path.
//! Nothing here holds a framework type or calls the host.
mod capture;

pub(crate) use capture::StrokeCapture;
#[cfg(test)]
pub(crate) use capture::counts;
