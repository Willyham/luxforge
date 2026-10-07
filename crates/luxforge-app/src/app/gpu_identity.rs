//! The evidence run's GPU identity hook. Launched with `--evidence-gpu-identity` beside
//! `--evidence-dir`, an evidence run draws its photograph at Fit through the photo surface's GPU
//! stage: the identity program over a boundary held from the very frame on screen. A rendered
//! scenario then checks, in the real editor, that the stage's output is the frame's own pixels and
//! that the state records the drawing path, the fallback and the GPU-preview budget.
//!
//! It is a test hook, not a feature: no control, setting, palette entry or API method reaches it,
//! an ordinary launch refuses the flag, and the desktop never hands the surface a GPU plan
//! otherwise. The plans the GPU stage is for come from the core once it describes its programs.
//!
//! The boundary is the displayed frame decoded to linear light through the core's 8-bit table and
//! held as half floats. It is built once per presented frame, off the UI thread, on the runtime's
//! executor, and until it arrives the surface draws the frame itself, as a gesture's ticks draw the
//! CPU's frame until their boundary is ready.
use super::{Message, message::evidence::EvidenceMessage};
use iced::Task;
use luxforge_gpu::{GpuBoundary, GpuPlan, GpuProgram, GpuStep, TexelMap};
use luxforge_ui::Frame;

/// The identity program: a pointwise colour program that returns its input.
const IDENTITY: &str = "fn evidence_identity(rgb: vec3<f32>, pos: vec2<f32>, words: u32, \
                        block: u32) -> vec3<f32> {\n    return rgb;\n}\n";

/// The hook's state: the plan over the newest boundary built, and the frame being built.
#[derive(Debug, Default)]
pub(crate) struct GpuIdentity {
    plan: Option<GpuPlan>,
    /// The version of the frame whose boundary is being built.
    building: Option<u64>,
}

impl GpuIdentity {
    /// Start building `photo`'s boundary, unless the plan or the build in flight is already for it.
    pub(crate) fn follow(&mut self, photo: Option<&Frame>) -> Task<Message> {
        let Some(photo) = photo else {
            return Task::none();
        };
        let version = photo.version();
        if self.building == Some(version)
            || self
                .plan
                .as_ref()
                .is_some_and(|plan| plan.boundary.version() == version)
        {
            return Task::none();
        }
        self.building = Some(version);
        let frame = photo.clone();
        Task::perform(async move { boundary(&frame) }, |boundary| {
            Message::Evidence(EvidenceMessage::GpuBoundary(boundary))
        })
    }

    /// Hold a built boundary under the identity program.
    pub(crate) fn adopt(&mut self, boundary: Option<GpuBoundary>) {
        let Some(boundary) = boundary else {
            self.building = None;
            return;
        };
        if self.building == Some(boundary.version()) {
            self.building = None;
        }
        self.plan = Some(GpuPlan {
            boundary,
            texels: TexelMap::IDENTITY,
            steps: vec![GpuStep::colour(GpuProgram::new(
                "evidence_identity",
                IDENTITY,
            ))],
            region: None,
            lights: Vec::new(),
        });
    }

    /// The plan for `photo`, when its boundary was held from that very frame.
    pub(crate) fn plan_for(&self, photo: &Frame) -> Option<&GpuPlan> {
        self.plan
            .as_ref()
            .filter(|plan| plan.boundary.version() == photo.version())
    }
}

/// `frame` held as a boundary: each 8-bit code decoded to linear light, alpha opaque.
fn boundary(frame: &Frame) -> Option<GpuBoundary> {
    let table = luxforge_core::colour::srgb::decode_table();
    let (width, height) = frame.size();
    GpuBoundary::from_linear(
        luxforge_gpu::BoundaryFormat::Half,
        width,
        height,
        frame.version(),
        frame.pixels().chunks_exact(4).map(|pixel| {
            [
                table[usize::from(pixel[0])],
                table[usize::from(pixel[1])],
                table[usize::from(pixel[2])],
                1.0,
            ]
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn frame(version: u64) -> Frame {
        Frame::new(
            Arc::new(vec![0, 128, 255, 255, 1, 2, 3, 255]),
            2,
            1,
            version,
        )
        .unwrap()
    }

    /// The identity program is one the surface accepts, and a built boundary is drawn only over
    /// the frame it was held from.
    #[test]
    fn the_hook_holds_one_frame_under_the_identity_program() {
        let mut hook = GpuIdentity::default();
        let shown = frame(3);
        let _ = hook.follow(Some(&shown));
        assert_eq!(hook.building, Some(3));
        // A redraw of the same frame starts nothing more.
        let _ = hook.follow(Some(&shown));
        assert_eq!(hook.building, Some(3));
        hook.adopt(boundary(&shown));
        assert_eq!(hook.building, None);
        let plan = hook
            .plan_for(&shown)
            .expect("a plan for the frame on screen");
        assert_eq!(plan.boundary.size(), (2, 1));
        luxforge_gpu::validate_step(&plan.steps[0]).expect("the identity program");
        assert!(
            hook.plan_for(&frame(4)).is_none(),
            "never over another frame"
        );
    }
}
