//! The settle dissolve: from the GPU stage's frame to the CPU frame that replaces it.
//!
//! When a gesture settles and the CPU's frame for the content the GPU frame showed arrives, the
//! caller hands the surface that frame with a [`Dissolve`] and no plan. The dissolve names both
//! frames — the GPU frame it starts from, as the caller identifies it (a draft revision or a
//! boundary), and the version of the CPU frame it ends on — and when it began. For
//! [`DISSOLVE_DURATION`] the surface keeps its GPU-preview slot and draws the slot's output, then
//! the CPU frame over it with the dissolve's share as its opacity, through the photograph's own
//! pipeline, placement and filter. The stage runs only on an sRGB-typed target, where the hardware
//! blends in linear light, so the picture is `(1 − t)·gpu + t·cpu` of the linear values, encoded
//! once. Once the share reaches one the frame is the CPU's alone, and the slot is released unless
//! a plan is still held behind the CPU frame for the open draft's next tick.
//!
//! - A whole-frame photograph dissolves from a whole frame's GPU output, and a percentage view from
//!   its region's, each only into the frame its dissolve names — the photograph's frame, or the
//!   view's whole frame or region of its current content — once that frame is in its texture. It runs with no plan, or behind a plan held behind the CPU frame
//!   (`PhotoSurface::gpu_hold`), whose unchanged words leave the slot's output as the GPU frame
//!   last shown. A plan drawn beside it cancels it: an input during a dissolve draws the next GPU
//!   frame.
//! - With no GPU frame to dissolve from — the last frame drawn was the CPU's, or the stage fell
//!   back — the CPU frame is drawn alone.
//! - The widget asks for the next frame only while a dissolve runs, from each redraw's own time, so
//!   the redraw that finds it ended asks for nothing and an idle editor stays asleep.
//! - Each draw records the dissolve it drew ([`DrawnDissolve`]): both identities, the boundary of
//!   the GPU output under it and the CPU frame's share.
use super::super::turn_uniform;
use std::time::{Duration, Instant};

/// How long a settle's dissolve takes: the recorded default.
pub const DISSOLVE_DURATION: Duration = Duration::from_millis(150);

/// A settle's dissolve, from the GPU frame on screen to the CPU frame that replaces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dissolve {
    /// The GPU frame dissolved from, as the caller identifies it: the draft revision or boundary
    /// whose GPU frame was last shown. Evidence only; the surface dissolves from whatever its slot
    /// last drew.
    pub from: u64,
    /// The version of the CPU frame dissolved to: the frame the surface is built with.
    pub to: u64,
    /// When it began. The share is the time since, over [`DISSOLVE_DURATION`].
    pub started: Instant,
}

impl Dissolve {
    /// A dissolve beginning now.
    pub fn start(from: u64, to: u64) -> Self {
        Self {
            from,
            to,
            started: Instant::now(),
        }
    }

    /// The CPU frame's share of the picture at `now`: zero when it begins, rising linearly with
    /// time to one once [`DISSOLVE_DURATION`] has passed.
    pub fn share(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started);
        (elapsed.as_secs_f32() / DISSOLVE_DURATION.as_secs_f32()).min(1.0)
    }
}

/// The dissolve one draw drew, as diagnostics record it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrawnDissolve {
    pub from: u64,
    pub to: u64,
    /// The boundary version of the GPU-stage output under the CPU frame.
    pub gpu_boundary: u64,
    /// The CPU frame's share, in ten-thousandths: [`DrawnDissolve::WHOLE`] would be the CPU frame
    /// alone, which is drawn as no dissolve at all.
    pub share: u16,
}

impl DrawnDissolve {
    /// The share of the CPU frame alone.
    pub const WHOLE: u16 = 10_000;

    /// The share as a fraction.
    pub fn progress(&self) -> f64 {
        f64::from(self.share) / f64::from(Self::WHOLE)
    }
}

/// A dissolve as one frame draws it: the dissolve and its share at that frame, short of one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DissolveFrame {
    pub(crate) dissolve: Dissolve,
    pub(crate) share: f32,
}

impl DissolveFrame {
    /// What a draw records of it, over the GPU output of `gpu_boundary`.
    pub(crate) fn drawn(&self, gpu_boundary: u64) -> DrawnDissolve {
        DrawnDissolve {
            from: self.dissolve.from,
            to: self.dissolve.to,
            gpu_boundary,
            share: (self.share * f32::from(DrawnDissolve::WHOLE)).round() as u16,
        }
    }
}

/// The dissolve a surface draws at `now`: one was handed to a `photo` — a photograph or a
/// percentage view handed no plan, or a plan held behind its frame — whose frame of `version` is
/// the one it names, and its share is still short of one. `None` asks for no further redraw.
pub(crate) fn dissolving(
    dissolve: Option<Dissolve>,
    photo: bool,
    version: Option<u64>,
    now: Instant,
) -> Option<DissolveFrame> {
    let dissolve = dissolve.filter(|_| photo)?;
    version.filter(|version| *version == dissolve.to)?;
    let share = dissolve.share(now);
    (share < 1.0).then_some(DissolveFrame { dissolve, share })
}

/// The photograph's bright rectangle and turn while it is drawn over a dissolving GPU frame: no
/// part bright, so every part is drawn at the share's opacity, unturned and snapped as the
/// photograph always is.
pub(crate) fn photo_uniform(share: f32) -> ([f32; 4], [f32; 4]) {
    (
        [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
        turn_uniform(0.0, share, true),
    )
}

#[cfg(test)]
mod tests;
