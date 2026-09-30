//! The one report `catalog-measure` writes: `catalog-measure.json`, and `catalog-measure.md`
//! beside it with the same rows as a table.
//!
//! The report mirrors the shape `measure`'s `measurements.json` uses: a header (the tool, its
//! status, the command, the git commit, the host, the build profiles, the scale and sample counts,
//! the data it ran over) and a `rows` array read by [`stats::rows`]. Every row is one
//! [`stats::row`] — `{"metric","unit","distribution"}` over the one nearest-rank `Distribution` —
//! with what the design asks each figure to carry beside it:
//!
//! - `status`: `measured`, `not_measured` (nothing in this build can take it yet, or the run
//!   reached no sample), `skipped` (its data is absent, such as the RAW corpus) or `failed` (its
//!   step returned an error). A row that is not `measured` names why in `reason`; nothing is left
//!   out silently.
//! - `step`, and `load`: the one-minute load average at the start and the end of the step that took
//!   it, each in the one `{"load_average_1m","load_threshold","reliability"}` shape.
//! - `scope` (what exactly was timed, over what), `cache` (what was warm), `target` (the design's
//!   provisional target, when it has one) and `detail` (counts the figure depends on).
//!
//! The report is rewritten after every step, so a run that stops part way still holds what it
//! measured.
use crate::{launch, stats, *};

/// A row's status.
pub const MEASURED: &str = "measured";
pub const NOT_MEASURED: &str = "not_measured";
pub const SKIPPED: &str = "skipped";
pub const FAILED: &str = "failed";

/// The report's file names inside the output directory.
pub const JSON: &str = "catalog-measure.json";
pub const MARKDOWN: &str = "catalog-measure.md";

/// One row of the report, built from a [`stats::row`].
#[derive(Debug)]
pub struct Row(Value);

impl Row {
    /// A figure over `samples`: `measured`, or `not_measured` when there are none.
    pub fn measured(metric: &str, unit: &str, samples: impl IntoIterator<Item = f64>) -> Self {
        let mut row = stats::row(metric, unit, samples);
        if row["distribution"].is_object() {
            row["status"] = json!(MEASURED);
        } else {
            row["status"] = json!(NOT_MEASURED);
            row["reason"] = json!("the run reached no sample");
        }
        Self(row)
    }

    /// A row read back from another tool's report (`editor-latency`'s `latency.json`), renamed.
    pub fn from_report(metric: &str, row: &Value) -> Self {
        let samples: Vec<f64> = row["distribution"]["samples"]
            .as_array()
            .map(|samples| samples.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        Self::measured(metric, row["unit"].as_str().unwrap_or_default(), samples)
    }

    fn unmeasured(metric: &str, unit: &str, status: &str, reason: &str) -> Self {
        let mut row = stats::row(metric, unit, Vec::new());
        row["status"] = json!(status);
        row["reason"] = json!(reason);
        Self(row)
    }

    /// A figure nothing in this build can take yet.
    pub fn not_measured(metric: &str, unit: &str, reason: &str) -> Self {
        Self::unmeasured(metric, unit, NOT_MEASURED, reason)
    }

    /// A figure whose data is absent from this host.
    pub fn skipped(metric: &str, unit: &str, reason: &str) -> Self {
        Self::unmeasured(metric, unit, SKIPPED, reason)
    }

    /// A step that returned an error, in place of the figures it would have taken.
    pub fn failed(step: &str, reason: &str) -> Self {
        Self::unmeasured(step, "", FAILED, reason)
    }

    pub fn scope(mut self, scope: impl Into<String>) -> Self {
        self.0["scope"] = json!(scope.into());
        self
    }

    pub fn target(mut self, target: &str) -> Self {
        self.0["target"] = json!(target);
        self
    }

    pub fn cache(mut self, cache: &str) -> Self {
        self.0["cache"] = json!(cache);
        self
    }

    pub fn detail(mut self, detail: Value) -> Self {
        self.0["detail"] = detail;
        self
    }

    pub fn value(self) -> Value {
        self.0
    }
}

/// The report while the run fills it.
pub struct Report {
    out: PathBuf,
    value: Value,
}

impl Report {
    /// A report into `out` with `header`'s fields and no rows yet.
    pub fn new(out: &Path, header: Value) -> Self {
        let mut value = header;
        value["status"] = json!("in_progress");
        value["rows"] = json!([]);
        Self {
            out: out.into(),
            value,
        }
    }

    pub fn set(&mut self, key: &str, value: Value) {
        self.value[key] = value;
    }

    pub fn get(&self, key: &str) -> &Value {
        &self.value[key]
    }

    /// Run one step and add its rows, each with the load at the step's start and end; a step that
    /// fails adds one `failed` row naming it and why, and the run goes on to the next step.
    pub fn step(&mut self, root: &Path, name: &str, body: impl FnOnce() -> Result<Vec<Row>>) {
        println!("catalog-measure: {name}");
        let start = launch::load_average(root);
        let rows = body().unwrap_or_else(|error| {
            eprintln!("catalog-measure: {name} failed: {error}");
            vec![Row::failed(name, &error.to_string())]
        });
        let end = launch::load_average(root);
        let load = json!({"start": launch::load(start), "end": launch::load(end)});
        let list = self.value["rows"].as_array_mut().expect("rows is a list");
        for row in rows {
            let mut row = row.value();
            row["step"] = json!(name);
            row["load"] = load.clone();
            list.push(row);
        }
        if let Err(error) = self.write() {
            eprintln!("catalog-measure: the report could not be written: {error}");
        }
    }

    /// Write both files as they stand.
    pub fn write(&self) -> Result {
        write_json(&self.out.join(JSON), &self.value)?;
        fs::write(self.out.join(MARKDOWN), markdown(&self.value))?;
        Ok(())
    }

    /// Settle the report's status from its rows, write it and return it.
    pub fn finish(mut self) -> Result<Value> {
        self.value["status"] = json!(status(stats::rows(&self.value)));
        self.write()?;
        Ok(self.value)
    }
}

/// `failed` when any row failed, `incomplete` when any other row was not measured, `passed` when
/// every row was.
pub fn status(rows: &[Value]) -> &'static str {
    if rows.iter().any(|row| row["status"] == FAILED) {
        "failed"
    } else if rows.iter().all(|row| row["status"] == MEASURED) {
        "passed"
    } else {
        "incomplete"
    }
}

fn figure(distribution: &Value, key: &str) -> String {
    distribution[key]
        .as_f64()
        .map_or_else(|| "–".into(), |value| format!("{value:.3}"))
}

fn load_text(load: &Value) -> String {
    load["load_average_1m"]
        .as_f64()
        .map_or_else(|| "–".into(), |value| format!("{value:.2}"))
}

/// The short summary beside the report: its header, one table row per report row, then why each
/// row was not measured and what each measured row's scope is.
pub fn markdown(report: &Value) -> String {
    let host = &report["host"];
    let text = |value: &Value| value.as_str().unwrap_or("unknown").to_owned();
    let mut out = format!(
        "# Catalog measurements\n\nStatus: {}. Scale: {}, {} samples a figure, {} a journey.\n\n",
        text(&report["status"]),
        text(&report["scale"]),
        report["samples"],
        report["journeys"],
    );
    out.push_str(&format!(
        "Command: `{}`. Commit {}{}.\n\n",
        report["command"]
            .as_array()
            .map(|args| args
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "))
            .unwrap_or_default(),
        text(&report["git"]["sha"]),
        if report["git"]["dirty"] == true {
            " with uncommitted changes"
        } else {
            ""
        },
    ));
    out.push_str(&format!(
        "Host: {} ({}, {} logical CPUs, {} bytes of memory), {} {}. Profiles: harness and bench {}, editor {}. Load threshold {}.\n\n",
        text(&host["model"]),
        text(&host["cpu"]),
        host["logical_cpus"],
        host["memory_bytes"],
        text(&host["os"]),
        text(&host["os_build"]),
        text(&report["profile"]["harness"]),
        text(&report["profile"]["editor"]),
        launch::LOAD_THRESHOLD,
    ));
    out.push_str(
        "| Metric | Status | p50 | p95 | n | Unit | Load start → end | Target |\n| --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    let rows = stats::rows(report);
    for row in rows {
        let distribution = &row["distribution"];
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} | {} → {} | {} |\n",
            text(&row["metric"]),
            text(&row["status"]),
            figure(distribution, "p50"),
            figure(distribution, "p95"),
            distribution["count"].as_u64().unwrap_or(0),
            text(&row["unit"]),
            load_text(&row["load"]["start"]),
            load_text(&row["load"]["end"]),
            row["target"].as_str().unwrap_or(""),
        ));
    }
    let unmeasured: Vec<&Value> = rows
        .iter()
        .filter(|row| row["status"] != MEASURED)
        .collect();
    if !unmeasured.is_empty() {
        out.push_str("\n## Not measured\n\n");
        for row in unmeasured {
            out.push_str(&format!(
                "- `{}` ({}): {}\n",
                text(&row["metric"]),
                text(&row["status"]),
                text(&row["reason"])
            ));
        }
    }
    out.push_str("\n## Scope\n\n");
    for row in rows.iter().filter(|row| row["scope"].is_string()) {
        out.push_str(&format!(
            "- `{}`: {}{}\n",
            text(&row["metric"]),
            text(&row["scope"]),
            row["cache"]
                .as_str()
                .map(|cache| format!(" Cache: {cache}"))
                .unwrap_or_default(),
        ));
    }
    out
}
