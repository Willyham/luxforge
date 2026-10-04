//! Whether the GPU stage can draw at all on its pipeline's device, as its capability check and its
//! device answer, and the launch's refusal of it.
//!
//! The pipeline Iced creates for the photo surface checks, once, whether its device can run the
//! stage ([`super::GpuStage::new`]); a stage that cannot draws every frame from the CPU frame it is
//! given and names `no-adapter` for any plan handed to it. A launch may refuse the stage before any
//! pipeline exists ([`refuse_gpu_stage`], the desktop's `--no-gpu-render`): the capability check
//! then answers unavailable whatever the device could do, nothing of the stage is created, and the
//! editor behaves exactly as on a machine whose adapter cannot run it. A device lost later makes
//! the stage unavailable for good, as its frames' `device-lost` says.
//!
//! The answer is published in the pipeline's figures and read live ([`GpuStageState`]), so the
//! desktop hands an unavailable stage no plan at all and reports which renderer draws its picture.
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Whether the photo surface's GPU stage can draw on this pipeline's device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpuStageState {
    /// No pipeline exists yet, so no device has been checked: no photograph has been drawn.
    #[default]
    Unchecked,
    /// The device can run the stage and has not been lost.
    Available,
    /// The capability check answered unavailable: the device lacks read-only storage buffers in the
    /// fragment stage or draws to a target that is not sRGB, or the launch refused the stage
    /// (`refused`), which the check answers the same way. Every plan falls back with `no-adapter`.
    NoAdapter { refused: bool },
    /// The device was lost after the stage was available. Nothing waits for a recovery.
    DeviceLost,
}

impl GpuStageState {
    /// Why every frame draws the CPU frame, when the stage cannot draw at all: the fallback its
    /// frames name.
    pub fn unavailable(self) -> Option<super::GpuFallback> {
        match self {
            Self::NoAdapter { .. } => Some(super::GpuFallback::NoAdapter),
            Self::DeviceLost => Some(super::GpuFallback::DeviceLost),
            Self::Unchecked | Self::Available => None,
        }
    }

    /// How evidence names it: `unchecked`, `available`, or the fallback its frames name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unchecked => "unchecked",
            Self::Available => "available",
            Self::NoAdapter { .. } | Self::DeviceLost => self
                .unavailable()
                .map_or("unavailable", super::GpuFallback::as_str),
        }
    }

    /// Whether the launch refused the stage.
    pub fn refused(self) -> bool {
        matches!(self, Self::NoAdapter { refused: true })
    }
}

/// The launch refused the GPU stage: set once, before the window and so before any pipeline, and
/// read by the capability check of the pipeline Iced creates. Never cleared.
static REFUSED: AtomicBool = AtomicBool::new(false);

/// Refuse the photo surface's GPU stage for the life of the process, as `--no-gpu-render` does:
/// the capability check of every pipeline created afterwards answers unavailable, so the editor
/// draws every frame from its CPU frames and names `no-adapter`, as on a machine whose adapter
/// cannot run the stage. Call it once at launch, before the window opens.
pub fn refuse_gpu_stage() {
    REFUSED.store(true, Ordering::Release);
}

/// Whether the launch refused the GPU stage ([`refuse_gpu_stage`]).
pub(in crate::photo_surface) fn gpu_stage_refused() -> bool {
    REFUSED.load(Ordering::Acquire)
}

const UNCHECKED: u8 = 0;
const AVAILABLE: u8 = 1;
const NO_ADAPTER: u8 = 2;
const REFUSED_STAGE: u8 = 3;

/// The capability check's answer as a pipeline's figures hold it, beside its lost flag.
#[derive(Default)]
pub(super) struct StageFigure(AtomicU8);

impl StageFigure {
    /// The capability check answered: the stage can run (`available`), or not, `refused` when the
    /// launch refused it.
    pub(super) fn checked(&self, available: bool, refused: bool) {
        let state = match (available, refused) {
            (true, _) => AVAILABLE,
            (false, true) => REFUSED_STAGE,
            (false, false) => NO_ADAPTER,
        };
        self.0.store(state, Ordering::Release);
    }

    /// The stage's state, with `lost` the device's lost flag.
    pub(super) fn state(&self, lost: &AtomicBool) -> GpuStageState {
        match self.0.load(Ordering::Acquire) {
            UNCHECKED => GpuStageState::Unchecked,
            AVAILABLE if lost.load(Ordering::Acquire) => GpuStageState::DeviceLost,
            AVAILABLE => GpuStageState::Available,
            REFUSED_STAGE => GpuStageState::NoAdapter { refused: true },
            _ => GpuStageState::NoAdapter { refused: false },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state reads what the check answered, a lost device only once the stage was available,
    /// and names each as its frames' fallback does.
    #[test]
    fn the_stage_state_reads_the_check_and_the_lost_device() {
        let figure = StageFigure::default();
        let lost = AtomicBool::new(false);
        assert_eq!(figure.state(&lost), GpuStageState::Unchecked);
        figure.checked(true, false);
        assert_eq!(figure.state(&lost), GpuStageState::Available);
        lost.store(true, Ordering::Release);
        assert_eq!(figure.state(&lost), GpuStageState::DeviceLost);
        assert_eq!(GpuStageState::DeviceLost.as_str(), "device-lost");
        figure.checked(false, true);
        assert_eq!(
            figure.state(&lost),
            GpuStageState::NoAdapter { refused: true },
            "a stage that never ran stays unavailable for the reason it never ran"
        );
        figure.checked(false, false);
        assert_eq!(
            figure.state(&lost),
            GpuStageState::NoAdapter { refused: false }
        );
        assert_eq!(
            GpuStageState::NoAdapter { refused: true }.as_str(),
            "no-adapter"
        );
        assert!(GpuStageState::NoAdapter { refused: true }.refused());
        assert_eq!(GpuStageState::Unchecked.unavailable(), None);
        assert_eq!(GpuStageState::Available.as_str(), "available");
    }
}
