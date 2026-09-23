//! The `raw-panel` smoke scenario: the RAW section as the tools panel draws it for a real RAW
//! source, with Basic collapsed so the RAW section sits under the histogram.
//!
//! No RAW photograph is checked in (see `fixtures/README.md`), so this scenario is not in
//! [`crate::smoke::SCENARIOS`] and takes its source from `--source`: an owner or raw.pixls.us file
//! the editor supports. It proves what the panel shows, not RAW decoding, which `raw-editor` covers.
use crate::{smoke::frame_identity, *};

pub const SCENARIO: &str = "raw-panel";
const RAW_MODULE: &str = "lightwell.raw";
const BASIC_MODULE: &str = "lightwell.basic";

/// One open frame plus one per script step.
pub fn frames(scenario: &str) -> Option<usize> {
    (scenario == SCENARIO).then_some(2)
}

pub fn script(scenario: &str) -> Option<Value> {
    (scenario == SCENARIO).then(|| {
        json!([
            // 1: collapse Basic, expanded by its own default, so the RAW section above it and the
            // collapsed bands under it are on screen together.
            {"section":{"module":BASIC_MODULE,"expanded":false}},
        ])
    })
}

pub fn verify(evidence: &Path, app: &Value) -> Result {
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    let mut checks = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let state = &frame["state"];
        frame_identity(evidence, app, frame)?;
        ensure(
            state["phase"] == "ready",
            format!("RAW panel frame {index} is not ready: {}", state["phase"]),
        )?;
        // The tools panel lists the RAW section only for a RAW source, so its presence in the
        // expanded map is the proof that the source opened as RAW.
        ensure(
            state["expanded"][RAW_MODULE] == json!(true),
            format!("RAW panel frame {index} has no expanded RAW section"),
        )?;
        checks.push(json!({
            "frame": frame["file"],
            "expanded": state["expanded"],
            "source_dimensions": state["source_dimensions"],
        }));
    }
    let last = frames.last().ok_or("Missing frames")?;
    ensure(
        last["state"]["expanded"][BASIC_MODULE] == json!(false) && last["step"]["step"] == json!(1),
        "The last RAW panel frame is not Basic collapsed by step 1",
    )?;
    write_json(&evidence.join("raw-panel-checks.json"), &json!(checks))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scenario_declares_one_frame_per_step_and_one_for_the_open() {
        let steps = script(SCENARIO).expect("a script");
        assert_eq!(
            steps.as_array().expect("an array").len() + 1,
            frames(SCENARIO).expect("a frame count")
        );
        assert!(script("load").is_none() && frames("load").is_none());
        assert!(!crate::smoke::SCENARIOS.contains(&SCENARIO));
    }
}
