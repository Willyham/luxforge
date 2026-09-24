//! The smoke scenarios' one library. [`launch`] is the envelope every runner shares: the output
//! directory, the hashes, each editor launch, the source check and the result and reproduce files,
//! and the replay that reruns a recorded run's checks without launching. [`plan`] is what one launch
//! captures: every frame in order with the step that produces it and what it must show, from which
//! the script and the frame count are derived and against which every frame is checked. [`frame`]
//! is a captured frame: its provenance, its state accessors and its capture, decoded once.
//! [`pixels`] is what a check measures a capture with: the fixture check, where the photograph is
//! drawn and what its patches read.
pub mod frame;
pub mod launch;
pub mod pixels;
pub mod plan;

pub use frame::{Frame, columns, events, identity, preamble};
pub use launch::{Launch, Run};
pub use pixels::{Bright, Fixture, Scan, Tolerance};
pub use plan::{Checked, Plan, Step};
