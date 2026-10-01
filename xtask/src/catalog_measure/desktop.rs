//! The desktop's frame-time probes: **lane B's module**, which `catalog-measure` calls once with
//! the data it prepared. The probes themselves are `xtask/src/catalog_probes/`: each launches the
//! editor in the background through the scenario library over a directory under
//! `context.scratch`, takes its figures from the editor's own evidence, and answers them under the
//! metric names below, with extra figures beside them (the same timings in ms, a held arrow, a
//! second focus region, the RAW trip's loupe stepping). `context.samples` is the run's `--samples`;
//! a figure that is one observation is a one-sample row.
use super::report::Row;
use crate::catalog_probes::{self, Figure, Outcome, ProbeContext};
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
    let figures = catalog_probes::desktop_probes(&ProbeContext {
        binary: context.binary.clone(),
        scratch: context.scratch.clone(),
        samples: context.samples,
        folder_10k: context.folder_10k.clone(),
        raw_trip: context.raw_trip.clone(),
    })?;
    Ok(figures.into_iter().map(row).collect())
}

/// One probe's figure as a row of the report.
fn row(figure: Figure) -> Row {
    let row = match figure.outcome {
        Outcome::Measured(samples) => Row::measured(&figure.metric, figure.unit, samples),
        Outcome::NotMeasured(reason) => Row::not_measured(&figure.metric, figure.unit, &reason),
        Outcome::Skipped(reason) => Row::skipped(&figure.metric, figure.unit, &reason),
        Outcome::Failed(reason) => Row::failed(&figure.metric, &reason),
    };
    let row = row
        .scope(figure.scope)
        .cache(&figure.cache)
        .detail(figure.detail);
    match figure.target {
        Some(target) => row.target(target),
        None => row,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_measure::report;

    fn figure(outcome: Outcome) -> Figure {
        Figure {
            metric: PROBES[1].0.into(),
            unit: "frames",
            outcome,
            scope: "what was timed".into(),
            cache: "what was warm".into(),
            target: Some(PROBES[1].2),
            detail: json!({"samples": 3}),
        }
    }

    /// A figure keeps its metric, unit, samples, scope, cache, target and detail as a row, and a
    /// figure without samples says why with the status the report gives it.
    #[test]
    fn a_probes_figure_becomes_a_row_of_the_report() {
        let measured = row(figure(Outcome::Measured(vec![1.0, 2.0, 1.0]))).value();
        assert_eq!(measured["metric"], PROBES[1].0);
        assert_eq!(measured["unit"], "frames");
        assert_eq!(measured["status"], report::MEASURED);
        assert_eq!(measured["distribution"]["count"], 3);
        assert_eq!(measured["scope"], "what was timed");
        assert_eq!(measured["cache"], "what was warm");
        assert_eq!(measured["target"], PROBES[1].2);
        assert_eq!(measured["detail"]["samples"], 3);
        let skipped = row(figure(Outcome::Skipped("no corpus".into()))).value();
        assert_eq!(
            (skipped["status"].as_str(), skipped["reason"].as_str()),
            (Some(report::SKIPPED), Some("no corpus"))
        );
        let unmeasured = row(figure(Outcome::NotMeasured("no frame".into()))).value();
        assert_eq!(unmeasured["status"], report::NOT_MEASURED);
        let failed = row(figure(Outcome::Failed("launch 26 timed out".into()))).value();
        assert_eq!(
            (
                failed["metric"].as_str(),
                failed["status"].as_str(),
                failed["reason"].as_str()
            ),
            (
                Some(PROBES[1].0),
                Some(report::FAILED),
                Some("launch 26 timed out")
            )
        );
        assert_eq!(failed["detail"]["samples"], 3, "its detail kept");
        assert_eq!(
            row(figure(Outcome::Measured(Vec::new()))).value()["status"],
            report::NOT_MEASURED,
            "a figure that reached no sample"
        );
    }
}
