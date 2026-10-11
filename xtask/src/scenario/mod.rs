//! The smoke scenarios' one library. [`launch`] is the envelope every runner shares: the output
//! directory, the hashes, each editor launch, the source check and the result and reproduce files,
//! and the replay that reruns a recorded run's checks without launching. [`plan`] is what one launch
//! captures: every frame in order with the step that produces it and what it must show, from which
//! the script and the frame count are derived and against which every frame is checked. [`frame`]
//! is a captured frame: its provenance, its state accessors and its capture, decoded once.
//! [`pixels`] is what a check measures a capture with: the fixture check, the one photo locator
//! over the rectangle the editor records per frame and what its patches read. [`checks`] is the
//! one record of a scenario's own pixel claims and notes. [`recipe`] holds the recipes a timing
//! tool and a scenario both commit.
pub mod checks;
pub mod frame;
pub mod launch;
pub mod pixels;
pub mod plan;
pub mod recipe;

pub use checks::Checks;
pub use frame::{Frame, asked_no_frame, columns, drawn_wheel, events, preamble, selected_view};
pub use launch::{Launch, Launched, Run};
pub use pixels::{Fixture, Tolerance};
pub use plan::{Checked, Plan, Step};
