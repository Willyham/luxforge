//! Grid scroll over 10,000 files. The design's target: "presented frames p95 within 16 ms at
//! 120 Hz".
//!
//! Not measured yet. The Select grid (lane D's `app/select*`, `state/select*`, `view/select*`)
//! records no evidence of the frames it presents while it scrolls, and no evidence step scrolls it:
//! a step that sends the offset the grid's scrollable publishes (`SelectMessage::Scrolled`) once per
//! display frame, recording each frame tick it rides, and an event in the update whose derived grid
//! draws a new offset — naming the offset, the cells drawn and how many drew a decoded preview, a
//! soft stand-in or nothing yet — are what this probe would time.
use super::{ProbeContext, not_measured};
use crate::*;

/// The row this probe would record.
const METRIC: &str = "grid_scroll_presented_interval";

pub(super) fn probe(context: &ProbeContext) -> Vec<Value> {
    // The folder's images, its manifest aside: what the row would have been measured over.
    let images = files(&context.folder_10k).map_or_else(
        |error| format!("a folder that cannot be read: {error}"),
        |files| {
            let images = files
                .iter()
                .filter(|file| file.extension().is_none_or(|extension| extension != "json"))
                .count();
            format!("the generated folder of {images} files")
        },
    );
    vec![not_measured(
        METRIC,
        format!(
            "the Select grid records no presented-frame evidence while it scrolls and no evidence \
             step scrolls it (lane D's app/select*, view/select*), so scrolling {images} cannot be \
             timed"
        ),
    )]
}
