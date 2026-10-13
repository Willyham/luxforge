//! The whole-stage overview a picture at rest's tiles are reduced to beside the backdrop, for
//! Develop's minimap at 200% and above (`docs/design/minimap.md`): the same reduction and
//! quantization passes as the picture at rest's own ([`super::rest`]), into an accumulator and an
//! output of the overview's size, within 256 × 256, and then read back once, without a wait, so the
//! desktop holds its pixels keyed by the content they show.
//!
//! - **Independent.** The overview's parts are charged and created after the picture at rest's
//!   own; a budget that holds the picture but not the overview, or an overview past
//!   [`OVERVIEW_GPU_BYTES`], leaves the picture, the backdrop and the counts as they are and answers
//!   why there is no overview ([`OverviewError`]).
//! - **Read back once.** After the last tile its output is copied into a staging buffer, charged
//!   while it lives, and mapped on submit, as the counts are ([`super::histogram`]): the mapping's
//!   callback, on whichever thread polls the device next, copies the rows out of their padding and
//!   wakes the desktop. Nothing waits, and no timer or thread is added.
//! - **Bounded.** An overview's parts and its staging copy take at most [`OVERVIEW_GPU_BYTES`]; one
//!   retiring beside the one being made stays within the design's 4 MiB.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

/// The most an overview may take of the GPU-preview budget, its parts and its staging copy
/// together: 2 MiB, so an overview retiring beside the one being made stays within 4 MiB.
pub const OVERVIEW_GPU_BYTES: u64 = 2 << 20;

/// The largest side an overview has, in pixels.
pub const OVERVIEW_SIDE: u32 = 256;

/// An overview's pixels, read back: opaque RGBA8 codes, `width × 4` bytes a row, top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverviewPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

/// Why a picture at rest has no overview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewError {
    /// Its parts would pass [`OVERVIEW_GPU_BYTES`] or what the GPU-preview budget leaves.
    Budget,
    /// Its reduction does not describe an overview: a size past [`OVERVIEW_SIDE`], or coverage
    /// tables of another size.
    Invalid,
    /// The reduction's passes could not be made.
    Pipeline,
    /// The picture at rest's tiles fell back before the last was drawn.
    Fallback,
    /// The staging copy could not be mapped.
    ReadbackFailed,
    /// The device was lost before the pixels were read.
    DeviceLost,
}

impl OverviewError {
    /// A stable kebab-case name, for diagnostics and evidence.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Budget => "budget-exceeded",
            Self::Invalid => "invalid",
            Self::Pipeline => "pipeline-failed",
            Self::Fallback => "fallback",
            Self::ReadbackFailed => "readback-failed",
            Self::DeviceLost => "device-lost",
        }
    }
}

/// An overview as the desktop reads it, without a wait.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverviewOutcome {
    /// Its tiles are still being drawn, or its pixels are on their way back.
    Pending,
    Ready(OverviewPixels),
    Failed(OverviewError),
}

/// One picture at rest's overview, shared between its slot, the mapping's callback and the
/// desktop's reads: every clone answers the same.
#[derive(Clone, Debug)]
pub struct OverviewReadback {
    outcome: Arc<Mutex<Option<Result<OverviewPixels, OverviewError>>>>,
    /// The device's lost flag, which its lost callback sets.
    lost: Arc<AtomicBool>,
}

impl OverviewReadback {
    pub(super) fn new(lost: Arc<AtomicBool>) -> Self {
        Self {
            outcome: Arc::default(),
            lost,
        }
    }

    /// Settle it, once: a later answer never replaces the first.
    pub(super) fn settle(&self, answer: Result<OverviewPixels, OverviewError>) {
        let mut outcome = self.outcome.lock().expect("overview readback lock");
        if outcome.is_none() {
            *outcome = Some(answer);
        }
    }

    /// What it answers now: its pixels or why there are none once settled; a device lost before
    /// then answers [`OverviewError::DeviceLost`], never earlier pixels.
    pub fn outcome(&self) -> OverviewOutcome {
        let outcome = self.outcome.lock().expect("overview readback lock").clone();
        match outcome {
            Some(Ok(pixels)) => OverviewOutcome::Ready(pixels),
            Some(Err(error)) => OverviewOutcome::Failed(error),
            None if self.lost.load(Ordering::Acquire) => {
                OverviewOutcome::Failed(OverviewError::DeviceLost)
            }
            None => OverviewOutcome::Pending,
        }
    }
}

/// A surface's picture at rest's overview as its slot last reported it: its readback, and whether
/// its tiles had fallen back, which settles nothing on its own.
#[derive(Clone, Debug)]
pub struct RestOverview {
    pub(super) readback: OverviewReadback,
    pub(super) fell_back: bool,
    /// Its size.
    pub size: (u32, u32),
}

impl RestOverview {
    /// As the desktop reads it, without a wait.
    pub fn outcome(&self) -> OverviewOutcome {
        match self.readback.outcome() {
            OverviewOutcome::Pending if self.fell_back => {
                OverviewOutcome::Failed(OverviewError::Fallback)
            }
            outcome => outcome,
        }
    }
}

/// The bytes a row of an overview `width` pixels wide takes in its staging copy: its codes, padded
/// to the copy's row alignment.
pub(super) fn padded_row(width: u32) -> u32 {
    (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

/// Copy `rows` of `width` pixels out of a staging copy whose rows are [`padded_row`] apart.
pub(super) fn unpadded(bytes: &[u8], width: u32, rows: u32) -> Arc<[u8]> {
    let stride = padded_row(width) as usize;
    let row = width as usize * 4;
    let mut rgba = Vec::with_capacity(row * rows as usize);
    for y in 0..rows as usize {
        rgba.extend_from_slice(&bytes[y * stride..y * stride + row]);
    }
    rgba.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_readback_settles_once_and_a_lost_device_answers_no_pixels() {
        let lost = Arc::new(AtomicBool::new(false));
        let readback = OverviewReadback::new(Arc::clone(&lost));
        assert_eq!(readback.outcome(), OverviewOutcome::Pending);
        let rest = RestOverview {
            readback: readback.clone(),
            fell_back: true,
            size: (2, 1),
        };
        assert_eq!(
            rest.outcome(),
            OverviewOutcome::Failed(OverviewError::Fallback)
        );
        lost.store(true, Ordering::Release);
        assert_eq!(
            readback.outcome(),
            OverviewOutcome::Failed(OverviewError::DeviceLost)
        );
        let pixels = OverviewPixels {
            width: 2,
            height: 1,
            rgba: Arc::from(&[1u8, 2, 3, 255, 4, 5, 6, 255][..]),
        };
        readback.settle(Ok(pixels.clone()));
        readback.settle(Err(OverviewError::ReadbackFailed));
        assert_eq!(readback.outcome(), OverviewOutcome::Ready(pixels.clone()));
        assert_eq!(rest.outcome(), OverviewOutcome::Ready(pixels));
    }

    #[test]
    fn a_staging_copys_padding_is_dropped_row_by_row() {
        assert_eq!(padded_row(256), 1024);
        assert_eq!(padded_row(171), 768);
        assert_eq!(padded_row(1), 256);
        let mut bytes = vec![0u8; 256 * 2];
        bytes[..4].copy_from_slice(&[1, 2, 3, 4]);
        bytes[256..260].copy_from_slice(&[5, 6, 7, 8]);
        assert_eq!(&*unpadded(&bytes, 1, 2), &[1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
