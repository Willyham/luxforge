//! The `raw-panel` smoke scenario: the RAW section as the tools panel draws it for a real RAW
//! source, with Basic collapsed so the RAW section sits under the histogram; a Custom temperature
//! drag left open, whose drafted frame approximates the white balance on the developed planes and
//! is labelled so, then released, which redevelops the mosaic and lands the exact frame, at Fit
//! and again at 100%; and a double-click reset on each of the three sliders after the committed
//! drag the first press makes.
//!
//! No RAW photograph is checked in (see `fixtures/README.md`), so this scenario is not in
//! [`crate::smoke::SCENARIOS`] and takes its source from `--source`: an owner or raw.pixls.us file
//! the editor supports. It proves what the panel shows and what its double-click does, not RAW
//! decoding, which `raw-editor` covers.
use crate::{
    smoke::{columns, frame_identity},
    *,
};

pub const SCENARIO: &str = "raw-panel";
const RAW_MODULE: &str = "lightwell.raw";
const RAW_EFFECT: &str = "lightwell.raw";
const BASIC_MODULE: &str = "lightwell.basic";
const SET_TEMPERATURE: &str = "set-raw-temperature";

/// One scripted temperature drag: the step that leaves it open, the value it stops on, and whether
/// the view is at Fit, where the drafted frame is the display proxy, or at 100%, where it is the
/// full-size frame with no proxy phase. Its release is the step after it, at the same value.
struct Drag {
    step: usize,
    kelvin: f64,
    fit: bool,
}

/// At Fit, far enough from any camera's as-shot white balance to change the picture plainly; then
/// at 100%, far from that committed 3500 K. Both inside 2000..12000 K on the 10 K step.
const DRAGS: [Drag; 2] = [
    Drag {
        step: 2,
        kelvin: 3500.0,
        fit: true,
    },
    Drag {
        step: 5,
        kelvin: 2500.0,
        fit: false,
    },
];
/// Steps 2–3 drag and release at Fit, 4 zooms to 100%, 5–6 drag and release there and 7 returns
/// to Fit; the double-clicks follow from step 8.
const FIRST_CLICK_STEP: usize = 8;
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
    (scenario == SCENARIO).then_some(FIRST_CLICK_STEP + DOUBLE_CLICKS.len())
}

pub fn script(scenario: &str) -> Option<Value> {
    (scenario == SCENARIO).then(|| {
        let mut steps = vec![
            // 1: collapse Basic, expanded by its own default, so the RAW section above it and the
            // collapsed bands under it are on screen together.
            json!({"section":{"module":BASIC_MODULE,"expanded":false}}),
        ];
        for drag in &DRAGS {
            if !drag.fit {
                // 4: 100%, where a frame shows a stage pixel per display pixel and has no proxy.
                steps.push(json!({"view":{"zoom":100.0}}));
            }
            // 2 and 5: a Custom temperature drag left open. Its frame is the drafted value
            // approximated on the planes developed at the committed white balance.
            steps.push(json!({"slider":{"action":SET_TEMPERATURE,"parameter":"kelvin","values":[drag.kelvin]}}));
            // 3 and 6: its release at the same value, which commits it and redevelops the mosaic.
            steps.push(json!({"slider":{"action":SET_TEMPERATURE,"parameter":"kelvin","values":[drag.kelvin],"release":true}}));
            if !drag.fit {
                // 7: back to Fit for the double-clicks.
                steps.push(json!({"view":{"zoom":"fit"}}));
            }
        }
        // 8–11: a double-click on each RAW slider's rail, then on Basic's Exposure. The first press
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

/// How far the photo surface of one capture is from another's: the mean absolute channel
/// difference in codes over the surface columns the frame records, between its top and bottom
/// tenths (the title and status bars stay outside), and the share of those pixels differing by more
/// than two codes in any channel. The surface draws the photograph and nothing else here: no
/// overlay is on and no draft bar is shown for a slider gesture.
fn surface_difference(first: &Path, second: &Path, frame: &Value) -> Result<(f64, f64)> {
    let first = image::open(first)?.to_rgb8();
    let second = image::open(second)?.to_rgb8();
    ensure(
        first.dimensions() == second.dimensions(),
        "The two captures are different sizes",
    )?;
    let (width, height) = first.dimensions();
    let [left, right] = columns(frame)?.unwrap_or([0, width]);
    let (mut total, mut over, mut count) = (0_u64, 0_u64, 0_u64);
    for y in height / 10..height - height / 10 {
        for x in left..right.min(width) {
            let (a, b) = (first.get_pixel(x, y).0, second.get_pixel(x, y).0);
            let differences = [0, 1, 2].map(|channel| a[channel].abs_diff(b[channel]));
            total += differences
                .iter()
                .map(|value| u64::from(*value))
                .sum::<u64>();
            over += u64::from(differences.iter().any(|value| *value > 2));
            count += 1;
        }
    }
    ensure(count > 0, "The frame records no photo surface")?;
    Ok((
        total as f64 / (3 * count) as f64,
        over as f64 / count as f64,
    ))
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
    for drag in &DRAGS {
        checks.push(white_balance_drag(evidence, app, events, frames, drag)?);
    }

    // Each double-click is two history entries: the first press's committed jump, then the
    // reset, sent against the revision that commit produced and never refused as stale.
    for (offset, click) in DOUBLE_CLICKS.iter().enumerate() {
        let step = offset + FIRST_CLICK_STEP;
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
    let raw = raw_payload(&frames[FIRST_CLICK_STEP + 2])?;
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

/// One temperature drag and its release.
///
/// Left open, the drafted frame is on screen and labelled approximate — in the state, the status
/// bar and every `preview_displayed` of its job — and it differs plainly from the frame before the
/// drag. At Fit it is the display proxy; at 100% it is the full-size frame, with no proxy phase.
/// The histogram is not adopted from it: the plot keeps the last exact report, marked updating,
/// and no report is adopted for the drafted generation.
///
/// Released, the commit redevelops the mosaic and lands the exact frame: unlabelled, its report
/// adopted, and the first frame handed to the surface after the commit — so the approximate frame
/// stayed on screen until it was replaced, with nothing drawn in between. On average it is within
/// a code of the approximate one.
fn white_balance_drag(
    evidence: &Path,
    app: &Value,
    events: &[Value],
    frames: &[Value],
    drag: &Drag,
) -> Result<Value> {
    let (drag_step, release_step, kelvin) = (drag.step, drag.step + 1, drag.kelvin);
    let (before, drafted, released) = (
        &frames[drag_step - 1],
        &frames[drag_step],
        &frames[release_step],
    );
    let state = &drafted["state"];
    let generation = state["surface"]["generation"].clone();
    ensure(
        state["proxy"]["presented"] == json!(drag.fit),
        format!(
            "The drafted frame at {} is {}the proxy",
            if drag.fit { "Fit" } else { "100%" },
            if drag.fit { "not " } else { "" }
        ),
    )?;
    ensure(
        state["approximate_white_balance"] == json!(true),
        format!(
            "The open temperature drag's frame is not labelled approximate: {}",
            state["approximate_white_balance"]
        ),
    )?;
    ensure(
        state["draft"]["action"] == SET_TEMPERATURE
            && state["draft"]["fields"]["kelvin"] == json!(kelvin)
            && !state["displayed_draft_revision"].is_null(),
        format!(
            "The open drag's frame is not its drafted value: {}",
            state["draft"]
        ),
    )?;
    let render = state["status_bar"]["render"].as_str().unwrap_or_default();
    let label = if drag.fit {
        " ms (proxy, approximate)"
    } else {
        " ms (approximate)"
    };
    ensure(
        render.ends_with(label) && state["status_bar"]["render_approximate"] == true,
        format!("The status bar does not say the drafted frame is approximate: {render:?}"),
    )?;
    let histogram = &state["histogram"];
    ensure(
        histogram["status"] == "updating"
            && histogram["identity"]["generation"] != generation
            && histogram["identity"]["draft_revision"].is_null(),
        format!("The histogram was adopted from the approximate frame: {histogram}"),
    )?;
    let drag_events = step_events(events, drag_step);
    let displayed: Vec<&&Value> = drag_events
        .iter()
        .filter(|event| {
            event["event"] == "preview_displayed" && event["detail"]["generation"] == generation
        })
        .collect();
    ensure(
        !displayed.is_empty()
            && displayed
                .iter()
                .all(|event| event["detail"]["approximate_white_balance"] == true),
        format!("The drafted generation's frames are not all labelled approximate: {displayed:?}"),
    )?;
    ensure(
        !drag_events.iter().any(|event| {
            event["event"] == "analysis_adopted" && event["detail"]["generation"] == generation
        }),
        "A report was adopted for the approximate generation",
    )?;
    let unpreviewed = drag_events
        .iter()
        .filter(|event| event["event"] == "slider_draft_unpreviewed")
        .count();
    ensure(
        unpreviewed == 0,
        format!("{unpreviewed} drafted values had no preview"),
    )?;
    let (drag_mean, drag_over) = surface_difference(
        &frame_identity(evidence, app, before)?,
        &frame_identity(evidence, app, drafted)?,
        drafted,
    )?;
    ensure(
        drag_mean > 1.0 && drag_over > 0.1,
        format!(
            "The drafted frame barely differs from the one before the drag: {drag_mean:.3} codes on average, {:.1}% of pixels over 2",
            drag_over * 100.0
        ),
    )?;

    let state = &released["state"];
    let generation = state["surface"]["generation"].clone();
    ensure(
        state["approximate_white_balance"] == json!(false)
            && state["draft"].is_null()
            && raw_payload(released)?["temperature_kelvin"] == json!(kelvin)
            && raw_payload(released)?["wb_mode"] == "custom",
        format!(
            "The release did not land the exact committed frame: approximate {}, draft {}, RAW {}",
            state["approximate_white_balance"],
            state["draft"],
            raw_payload(released)?
        ),
    )?;
    let histogram = &state["histogram"];
    ensure(
        histogram["status"] == "ready"
            && histogram["identity"]["generation"] == generation
            && histogram["identity"]["draft_revision"].is_null(),
        format!("The committed frame's own report is not plotted: {histogram}"),
    )?;
    ensure(
        revision(released)? == revision(drafted)? + 1,
        "The release did not commit exactly one entry",
    )?;
    let release_events = step_events(events, release_step);
    let commit = release_events
        .iter()
        .position(|event| event["event"] == "slider_draft_commit")
        .ok_or("The release did not commit")?;
    let after: Vec<&&Value> = release_events[commit..]
        .iter()
        .filter(|event| event["event"] == "preview_displayed")
        .collect();
    let first = after
        .first()
        .ok_or("Nothing was presented after the commit")?;
    ensure(
        first["detail"]["generation"] == generation
            && first["detail"]["draft_revision"].is_null()
            && first["detail"]["approximate_white_balance"] == false,
        format!(
            "The first frame after the commit is not the committed exact frame: {}",
            first["detail"]
        ),
    )?;
    ensure(
        !release_events
            .iter()
            .any(|event| event["event"] == "render_failed"),
        "A render failed between the release and the committed frame",
    )?;
    // Exactly one raster reached the surface between the drafted frame and the committed one:
    // the committed frame's own — its proxy at Fit, whose exact phase is adopted without being
    // drawn, or its one full-size frame at 100%.
    let versions = (
        drafted["state"]["surface"]["version"].as_u64(),
        state["surface"]["version"].as_u64(),
    );
    ensure(
        matches!(versions, (Some(drafted), Some(released)) if released == drafted + 1),
        format!(
            "The surface was handed more than the committed frame after the drag: {versions:?}"
        ),
    )?;
    ensure(
        release_events.iter().any(|event| {
            event["event"] == "analysis_adopted" && event["detail"]["generation"] == generation
        }),
        "The committed frame's report was not adopted",
    )?;
    let (settled_mean, settled_over) = surface_difference(
        &frame_identity(evidence, app, drafted)?,
        &frame_identity(evidence, app, released)?,
        released,
    )?;
    ensure(
        settled_mean < 1.0,
        format!("The exact frame is {settled_mean:.3} codes from the approximate one on average"),
    )?;
    Ok(json!({
        "step": drag_step,
        "kelvin": kelvin,
        "view": if drag.fit { "fit" } else { "100%" },
        "drafted_frame": drafted["file"],
        "drafted_render": drafted["state"]["status_bar"]["render"],
        "drafted_histogram": drafted["state"]["histogram"]["status"],
        "drafted_against_before": {"mean_codes": drag_mean, "share_over_2": drag_over},
        "released_frame": released["file"],
        "released_render": state["status_bar"]["render"],
        "released_histogram": histogram["status"],
        "released_against_drafted": {"mean_codes": settled_mean, "share_over_2": settled_over},
        "surface_versions": [versions.0, versions.1],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each drag stops on a value the temperature control declares, on its step, and the script
    /// holds each drag, its release and the zoom around the 100% one where the checks look.
    #[test]
    fn each_drag_is_a_declared_temperature_on_its_step_where_the_checks_look() {
        let registry = lightwell_core::ModuleRegistry::builtin();
        let (_, action) = registry.action(SET_TEMPERATURE).expect("a declared action");
        let parameter = action.parameter("kelvin").expect("a declared field");
        let lightwell_core::ParameterKind::Number { min, max } = parameter.kind else {
            panic!("kelvin is a number");
        };
        let step = parameter.step.unwrap_or(1.0);
        let steps = script(SCENARIO).unwrap();
        for drag in &DRAGS {
            assert!((min..=max).contains(&drag.kelvin));
            assert_eq!((drag.kelvin / step).round() * step, drag.kelvin);
            let open = &steps[drag.step - 1]["slider"];
            let release = &steps[drag.step]["slider"];
            assert_eq!(open["values"], json!([drag.kelvin]));
            assert_eq!(open["release"], Value::Null);
            assert_eq!(release["values"], json!([drag.kelvin]));
            assert_eq!(release["release"], json!(true));
            if !drag.fit {
                assert_eq!(steps[drag.step - 2], json!({"view":{"zoom":100.0}}));
                assert_eq!(steps[drag.step + 1], json!({"view":{"zoom":"fit"}}));
            }
        }
        assert!(steps[FIRST_CLICK_STEP - 1]["double_click"].is_object());
    }

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
