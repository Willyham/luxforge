//! The `raw-panel` smoke scenario: the RAW section as the tools panel draws it for a real RAW
//! source, with Basic collapsed so the RAW section sits under the histogram, and a double-click
//! reset on each of its three sliders after the committed drag the first press makes.
//!
//! No RAW photograph is checked in (see `fixtures/README.md`), so this scenario is not in
//! [`crate::smoke::SCENARIOS`] and takes its source from `--source`: an owner or raw.pixls.us file
//! the editor supports. It proves what the panel shows and what its double-click does, not RAW
//! decoding, which `raw-editor` covers.
use crate::{smoke::frame_identity, *};

pub const SCENARIO: &str = "raw-panel";
const RAW_MODULE: &str = "lightwell.raw";
const RAW_EFFECT: &str = "lightwell.raw";
const BASIC_MODULE: &str = "lightwell.basic";
/// Between a double-click's release and its second press: a person's ordinary double-click, well
/// inside the 300 ms iced gives the two presses.
const GAP_MS: u64 = 120;

/// One double-click the script makes: the field, where its first press lands, and the text the
/// field shows once the reset has run, which is the parameter's declared default.
struct DoubleClick {
    action: &'static str,
    parameter: &'static str,
    value: f64,
    default: &'static str,
}

const DOUBLE_CLICKS: [DoubleClick; 4] = [
    DoubleClick {
        action: "set-raw-exposure",
        parameter: "ev",
        value: 0.35,
        default: "0.00",
    },
    DoubleClick {
        action: "set-raw-temperature",
        parameter: "kelvin",
        value: 5000.0,
        default: "6504",
    },
    DoubleClick {
        action: "set-raw-tint",
        parameter: "tint",
        value: 12.0,
        default: "0",
    },
    // Basic's own Exposure on the same photograph, for comparison: its commit does not wait for a
    // redevelopment.
    DoubleClick {
        action: "set-basic",
        parameter: "exposure",
        value: 0.4,
        default: "0.00",
    },
];

/// One open frame plus one per script step.
pub fn frames(scenario: &str) -> Option<usize> {
    (scenario == SCENARIO).then_some(2 + DOUBLE_CLICKS.len())
}

pub fn script(scenario: &str) -> Option<Value> {
    (scenario == SCENARIO).then(|| {
        let mut steps = vec![
            // 1: collapse Basic, expanded by its own default, so the RAW section above it and the
            // collapsed bands under it are on screen together.
            json!({"section":{"module":BASIC_MODULE,"expanded":false}}),
        ];
        // 2–5: a double-click on each RAW slider's rail, then on Basic's Exposure. The first press
        // moves the value, which commits on release; the second press resets the field.
        steps.extend(DOUBLE_CLICKS.iter().map(|click| {
            json!({"double_click":{"action":click.action,"parameter":click.parameter,
                "value":click.value,"gap_ms":GAP_MS}})
        }));
        Value::Array(steps)
    })
}

fn revision(frame: &Value) -> Result<u64> {
    frame["state"]["stack"]["revision"]
        .as_u64()
        .ok_or_else(|| "Missing stack revision".into())
}

fn raw_payload(frame: &Value) -> Result<&Value> {
    frame["state"]["stack"]["layers"]
        .as_array()
        .and_then(|layers| layers.iter().find(|layer| layer["effect"] == RAW_EFFECT))
        .map(|layer| &layer["payload"])
        .ok_or_else(|| "The stack has no RAW layer".into())
}

/// The events one script step logged, from its own `script_step` to the next one's.
fn step_events(events: &[Value], step: usize) -> Vec<&Value> {
    let mut current = 0;
    events
        .iter()
        .filter(|event| {
            if event["event"] == "script_step" {
                current = event["detail"]["step"].as_u64().unwrap_or(0) as usize;
            }
            current == step
        })
        .collect()
}

pub fn verify(evidence: &Path, app: &Value, events: &[Value]) -> Result {
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
        if index > 0 {
            ensure(
                frame["step"]["step"] == json!(index) && frame["step"]["status"] == "sent",
                format!("RAW panel frame {index} is not its scripted step, sent"),
            )?;
        }
        checks.push(json!({
            "frame": frame["file"],
            "expanded": state["expanded"],
            "source_dimensions": state["source_dimensions"],
            "revision": state["stack"]["revision"],
            "controls": state["controls"],
        }));
    }
    ensure(
        frames[1]["state"]["expanded"][BASIC_MODULE] == json!(false)
            && frames[1]["step"]["step"] == json!(1),
        "The second RAW panel frame is not Basic collapsed by step 1",
    )?;

    // Each double-click is two history entries: the first press's committed jump, then the
    // reset, sent against the revision that commit produced and never refused as stale.
    for (offset, click) in DOUBLE_CLICKS.iter().enumerate() {
        let step = offset + 2;
        let (before, after) = (&frames[step - 1], &frames[step]);
        let field = format!("{}.{}", click.action, click.parameter);
        let logged = step_events(events, step);
        let named = |name: &str| -> Vec<&&Value> {
            logged
                .iter()
                .filter(|event| event["event"] == name)
                .collect()
        };
        ensure(
            named("command_failed").is_empty(),
            format!(
                "{field}: a request was refused during the double-click: {:?}",
                named("command_failed")
            ),
        )?;
        let sent = named("field_reset_sent");
        ensure(
            sent.len() == 1
                && sent[0]["detail"]["action"] == click.action
                && sent[0]["detail"]["revision"] == json!(revision(before)? + 1),
            format!("{field}: the reset was not sent once, after the jump's commit: {sent:?}"),
        )?;
        ensure(
            named("slider_draft_commit").len() == 1,
            format!("{field}: the first press did not commit its jump once"),
        )?;
        ensure(
            revision(after)? == revision(before)? + 2,
            format!(
                "{field}: revision {} after {}, not the jump and the reset",
                revision(after)?,
                revision(before)?
            ),
        )?;
        ensure(
            after["state"]["controls"][&field] == json!(click.default),
            format!(
                "{field} shows {} after its reset, not its default {}",
                after["state"]["controls"][&field], click.default
            ),
        )?;
        checks.push(json!({
            "step": step,
            "field": field,
            "revision_before": revision(before)?,
            "revision_after": revision(after)?,
            "reset_sent_at_revision": sent[0]["detail"]["revision"],
            "queued": !named("field_reset_queued").is_empty(),
            "shown": after["state"]["controls"][&field],
        }));
    }
    // The RAW development the resets leave: exposure back at 0 EV, and a custom white balance at
    // the declared 6504 K and 0 tint, which is what those two fields reset to.
    let raw = raw_payload(&frames[4])?;
    ensure(
        raw["exposure_ev"] == json!(0.0)
            && raw["wb_mode"] == "custom"
            && raw["temperature_kelvin"] == json!(6504.0)
            && raw["tint"] == json!(0.0),
        format!("The RAW layer after the three resets is {raw}"),
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

    /// Every double-click names a declared slider field whose one value is a whole request, lands
    /// its first press inside the declared range and off the default, and expects the declared
    /// default back, formatted as the field shows it.
    #[test]
    fn every_double_click_is_a_declared_drafting_field_reset_to_its_default() {
        let registry = lightwell_core::ModuleRegistry::builtin();
        for click in &DOUBLE_CLICKS {
            let (_, action) = registry.action(click.action).expect("a declared action");
            let parameter = action.parameter(click.parameter).expect("a declared field");
            assert!(
                action.patch || action.parameters.len() == 1,
                "{}",
                click.action
            );
            let lightwell_core::ParameterKind::Number { min, max } = parameter.kind else {
                panic!("{} is a number", click.parameter);
            };
            assert!((min..=max).contains(&click.value));
            let default = parameter.default.as_ref().and_then(Value::as_f64).unwrap();
            assert_ne!(default, click.value, "the first press moves the value");
            let decimals = usize::from(parameter.precision.unwrap_or(0));
            assert_eq!(format!("{default:.decimals$}"), click.default);
        }
    }

    // The gap stays inside the window iced gives a double-click's two presses.
    const _: () = assert!(GAP_MS <= 250);
}
