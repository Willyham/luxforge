//! The desktop's frame-time probes: **lane B's module**, which `catalog-measure` calls once with
//! the data it prepared. Until lane B fills it, every probe is a `not_measured` row naming it, so
//! the report lists each desktop target and never leaves one out.
//!
//! To fill a probe: launch the editor in the background through the scenario library
//! (`scenario::Run::tool` over a directory under `context.scratch`, `scenario::Launch`), take the
//! figure from its evidence, and return it as `Row::measured(metric, unit, samples)` (one
//! [`stats::row`](crate::stats::row) over the one `Distribution`) with its `scope`, `cache` and
//! `target`, keeping the metric names below. `context.samples` is the run's `--samples`; a figure
//! that is one observation is a one-sample row.
use super::report::Row;
use crate::*;

/// What the desktop probes run over.
pub struct DesktopContext {
    /// The release editor every launch runs (`--binary`).
    pub binary: PathBuf,
    /// A directory the probes own, which does not exist yet: their catalogs, evidence and logs.
    pub scratch: PathBuf,
    /// Samples a figure (`--samples`).
    pub samples: usize,
    /// A folder of real JPEGs, as many as the scale's 10,000-file folder (500 at `--scale tiny`).
    pub folder_10k: PathBuf,
    /// The RAW trip's folder, absent when the corpus is.
    pub raw_trip: Option<PathBuf>,
}

/// Lane B's reason, until each probe is built.
const PENDING: &str =
    "lane B's desktop frame-time probe (xtask/src/catalog_measure/desktop.rs) is not built yet";

/// Each desktop probe: its metric, unit and the design's provisional target.
pub const PROBES: [(&str, &str, &str); 6] = [
    (
        "desktop.grid_scroll_10k.frame_time",
        "ms",
        "Grid scroll over 10,000 files: presented frames p95 within 16 ms at 120 Hz",
    ),
    (
        "desktop.loupe_step.key_to_presented",
        "frames",
        "Loupe stepping with the look-ahead warm: the next frame presented in the frame after the key; a held arrow presents every frame at the key-repeat rate",
    ),
    (
        "desktop.focus_check.embedded",
        "ms",
        "100% focus check from a full-size embedded preview: within 50 ms of Z",
    ),
    (
        "desktop.focus_check.development",
        "ms",
        "100% focus check from an on-demand development: reported per camera",
    ),
    (
        "desktop.memory.decoded_grid",
        "MiB",
        "Decoded grid previews in the desktop under 192 MiB whatever the view's size",
    ),
    (
        "desktop.memory.decoded_loupe",
        "MiB",
        "Decoded loupe previews in the desktop under 256 MiB whatever the view's size",
    ),
];

impl DesktopContext {
    /// What the probes were handed, for a row's `detail`.
    pub fn record(&self) -> Value {
        json!({
            "binary": self.binary,
            "scratch": self.scratch,
            "samples": self.samples,
            "folder_10k": self.folder_10k,
            "raw_trip": self.raw_trip,
        })
    }
}

/// Every desktop probe's rows.
pub fn desktop_probes(context: &DesktopContext) -> Result<Vec<Row>> {
    Ok(PROBES
        .iter()
        .map(|(metric, unit, target)| {
            Row::not_measured(metric, unit, PENDING)
                .target(target)
                .detail(context.record())
        })
        .collect())
}
