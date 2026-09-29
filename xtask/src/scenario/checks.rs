//! A scenario's own evidence: every pixel claim it makes and every frame it notes, in the order it
//! made them, written once as `<name>-checks.json` beside the evidence. A claim goes through
//! [`Checks::compare`], which checks it with [`pixels::compare`] and records both readings and the
//! threshold whether it holds or not; what a frame shows beyond its plan goes through
//! [`Checks::note`]. The file is written by [`Checks::write`] with the scenario's constants and
//! scope, so no scenario builds its own record.
use super::{Frame, Tolerance, pixels};
use crate::*;

/// The claims and notes of one scenario, or of one launch of it.
#[derive(Debug, Default)]
pub struct Checks {
    records: Vec<Value>,
}

/// Where a frame's capture sits in the run, as `launch/frame-N.png`, so the frames of different
/// launches never share a name.
fn named(frame: &Frame) -> Value {
    let file = frame["file"].as_str().unwrap_or("no file");
    match frame
        .path()
        .ok()
        .and_then(Path::parent)
        .and_then(Path::file_name)
    {
        Some(launch) => json!(format!("{}/{file}", launch.to_string_lossy())),
        None => json!(file),
    }
}

fn tolerance(tolerance: Tolerance) -> Value {
    match tolerance {
        Tolerance::Within(most) => json!({"within": most}),
        Tolerance::Under(most) => json!({"under": most}),
        Tolerance::Apart(least) => json!({"apart": least}),
        Tolerance::Beyond(least) => json!({"beyond": least}),
        Tolerance::Above(margin) => json!({"above": margin}),
    }
}

impl Checks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reading `a` of `frame` against reading `b` under `tolerance`, recorded and then checked:
    /// the error is [`pixels::compare`]'s, naming `what`, both readings and the threshold.
    pub fn compare(
        &mut self,
        frame: &Frame,
        what: &str,
        a: f64,
        b: f64,
        within: Tolerance,
    ) -> Result {
        let held = pixels::compare(what, a, b, within);
        self.records.push(json!({
            "frame": named(frame),
            "check": what,
            "a": a,
            "b": b,
            "tolerance": tolerance(within),
            "held": held.is_ok(),
        }));
        held
    }

    /// What `frame` shows, in a line, with the readings and state that show it.
    pub fn note(&mut self, frame: &Frame, shows: &str, detail: Value) {
        self.records
            .push(json!({"frame": named(frame), "shows": shows, "detail": detail}));
    }

    /// Everything recorded, in order.
    pub fn records(&self) -> &[Value] {
        &self.records
    }

    /// Write `dir/<name>-checks.json`: every record under `checks`, and each of `extra`'s fields
    /// (the scenario's thresholds, sample geometry and scope) beside it.
    pub fn write(self, dir: &Path, name: &str, extra: Value) -> Result {
        let mut file = serde_json::Map::new();
        file.insert("checks".into(), Value::Array(self.records));
        if let Value::Object(extra) = extra {
            file.extend(extra);
        }
        write_json(
            &dir.join(format!("{name}-checks.json")),
            &Value::Object(file),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claim_is_recorded_whether_it_holds_or_not_and_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let launch = tmp.path().join("launch1");
        fs::create_dir_all(&launch).unwrap();
        let capture = launch.join("frame-2.png");
        image::RgbImage::new(2, 2).save(&capture).unwrap();
        let frame = Frame::unchecked(json!({"file":"frame-2.png"}), &capture);
        let mut checks = Checks::new();
        assert!(
            checks
                .compare(&frame, "lifted", 23.0, 10.0, Tolerance::Above(12.0))
                .is_ok()
        );
        let error = checks
            .compare(&frame, "same", 10.0, 11.5, Tolerance::Within(1.0))
            .unwrap_err()
            .to_string();
        assert_eq!(error, "same: 10.00 and 11.50 differ by more than 1");
        checks.note(&frame, "the frame", json!({"x": 1}));
        checks
            .write(tmp.path(), "scenario", json!({"scope": "a test"}))
            .unwrap();
        let written = read_json(&tmp.path().join("scenario-checks.json")).unwrap();
        assert_eq!(written["scope"], "a test");
        let records = written["checks"].as_array().unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0]["frame"], "launch1/frame-2.png");
        assert_eq!(records[0]["tolerance"], json!({"above": 12.0}));
        assert_eq!(records[1]["held"], false);
        assert_eq!(records[2]["shows"], "the frame");
    }
}
