//! The `raw-panel` smoke scenario: the RAW section as the tools panel draws it for a real RAW
//! source, with Basic collapsed so the RAW section sits under the histogram; a Custom temperature
//! drag left open, whose drafted frame approximates the white balance on the developed planes and
//! is labelled so, then released, which redevelops the mosaic and lands the exact frame, at Fit
//! and again at 100%, each keeping the tint in force (the first, from As shot, the camera's as-shot
//! tint); and a double-click reset on each of the three sliders after the committed
//! drag the first press makes: exposure back to 0 EV, and the custom temperature and tint back to
//! As shot, whose fields then show the camera's as-shot equivalent. The RAW band carries no edited
//! dot on the untouched photograph and again once the resets leave As shot at 0 EV; and a crop drafted on
//! the RAW's whole input stage, straightened by 7°, applied at Fit, read at 100% and replaced through
//! the API's `crop-fit`, each commit checked to be the picture on screen.
//!
//! No RAW photograph is checked in (see `fixtures/README.md`), so this scenario is not in
//! [`crate::smoke::SCENARIOS`] and takes its source from `--source`: an owner or raw.pixls.us file
//! the editor supports. It proves what the panel shows, what its double-click does and that a RAW
//! crop is drawn and shown, not RAW decoding, which `raw-editor` covers.
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

/// One double-click the script makes: the field, where its first press lands, the action its
/// reset runs, and what the field shows once that has run.
struct DoubleClick {
    action: &'static str,
    parameter: &'static str,
    value: f64,
    /// The action the reset sends: the field's own, with its declared default, or the reset its
    /// control declares.
    reset: &'static str,
    /// The text the field shows after its reset: its declared default, or `None` for As shot,
    /// whose temperature and tint are the camera's as-shot equivalent, computed from the frame's
    /// own RAW layer by the check.
    shows: Option<&'static str>,
}

const AS_SHOT: &str = "use-as-shot-wb";

const DOUBLE_CLICKS: [DoubleClick; 4] = [
    DoubleClick {
        action: "set-raw-exposure",
        parameter: "ev",
        value: 0.35,
        reset: "set-raw-exposure",
        shows: Some("0.00"),
    },
    // Custom temperature and tint reset to the camera's own white balance, as Lightroom's Temp and
    // Tint do.
    DoubleClick {
        action: "set-raw-temperature",
        parameter: "kelvin",
        value: 5000.0,
        reset: AS_SHOT,
        shows: None,
    },
    DoubleClick {
        action: "set-raw-tint",
        parameter: "tint",
        value: 12.0,
        reset: AS_SHOT,
        shows: None,
    },
    // Basic's own Exposure on the same photograph, for comparison: its commit does not wait for a
    // redevelopment.
    DoubleClick {
        action: "set-basic",
        parameter: "exposure",
        value: 0.4,
        reset: "set-basic",
        shows: Some("0.00"),
    },
];

/// The crop steps follow the double-clicks: a draft opened on the RAW's whole input stage, given
/// a 16:9 ratio and straightened, applied at Fit, inspected at 100% through two pointer readouts,
/// replaced by a `crop-fit` through the API at 100% and read again, then Fit.
const FIRST_CROP_STEP: usize = FIRST_CLICK_STEP + DOUBLE_CLICKS.len();
/// The draft's straightening angle, and the angle the API's `crop-fit` then commits.
const CROP_ANGLE: f64 = 7.0;
const FIT_ANGLE: f64 = -12.0;
/// Where the pointer readouts sample the committed crops at 100%: stage pixels inside the corner of
/// the crop the canvas shows at a zero pan, clear of the scroll bars and the mode strip, for any
/// supplied RAW (the smallest crop, the Z6's, is over 2000 px each way).
const READOUTS: [(u32, u32); 2] = [(300, 200), (1100, 700)];

fn crop_steps() -> Vec<Value> {
    vec![
        json!({"draft":{"start":true}}),
        json!({"draft":{"preset":"16:9"}}),
        json!({"draft":{"angle":CROP_ANGLE}}),
        json!({"draft":{"apply":true}}),
        json!({"view":{"zoom":100.0}}),
        json!({"hover":{"x":READOUTS[0].0,"y":READOUTS[0].1}}),
        json!({"hover":{"x":READOUTS[1].0,"y":READOUTS[1].1}}),
        json!({"api":{"method":"edit.crop-fit","params":{"aspect":"3:2","angle":FIT_ANGLE}}}),
        json!({"hover":{"x":READOUTS[1].0,"y":READOUTS[1].1}}),
        json!({"view":{"zoom":"fit"}}),
    ]
}

/// One open frame plus one per script step.
pub fn frames(scenario: &str) -> Option<usize> {
    (scenario == SCENARIO).then_some(FIRST_CROP_STEP + crop_steps().len())
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
        // 12–21: a straightened crop drafted, applied and inspected; see `crop_steps`.
        steps.extend(crop_steps());
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
                && sent[0]["detail"]["action"] == click.reset
                && sent[0]["detail"]["field"]
                    == json!({"action": click.action, "parameter": click.parameter})
                && sent[0]["detail"]["revision"] == json!(revision(before)? + 1),
            format!(
                "{field}: the reset was not {} sent once, after the jump's commit: {sent:?}",
                click.reset
            ),
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
        let mut shown = json!({"field": after["state"]["controls"][&field]});
        match click.shows {
            Some(default) => ensure(
                after["state"]["controls"][&field] == json!(default),
                format!(
                    "{field} shows {} after its reset, not its default {default}",
                    after["state"]["controls"][&field]
                ),
            )?,
            None => {
                // As shot: the entry is labelled so, the development is the camera's own white
                // balance, and both white-balance fields show its equivalent.
                let label = &after["state"]["stack"]["label"];
                ensure(
                    label == "As shot white balance" && raw_payload(after)?["wb_mode"] == "as-shot",
                    format!(
                        "{field}: the reset left {label} with {}",
                        raw_payload(after)?
                    ),
                )?;
                shown = shows_as_shot_equivalent(after, &field)?;
            }
        }
        checks.push(json!({
            "step": step,
            "field": field,
            "reset": click.reset,
            "label": after["state"]["stack"]["label"],
            "revision_before": revision(before)?,
            "revision_after": revision(after)?,
            "reset_sent_at_revision": sent[0]["detail"]["revision"],
            "queued": !named("field_reset_queued").is_empty(),
            "shown": shown,
        }));
    }
    // The RAW development the resets leave: exposure back at 0 EV and the camera's own white
    // balance, which is the Original's development, so the band carries no dot again.
    let reset = &frames[FIRST_CLICK_STEP + 2];
    let raw = raw_payload(reset)?;
    ensure(
        raw["exposure_ev"] == json!(0.0) && raw["wb_mode"] == "as-shot",
        format!("The RAW layer after the three resets is {raw}"),
    )?;
    // The band's dot: none on the untouched photograph, one once a drag has committed a custom
    // white balance, and none again once the resets leave As shot at 0 EV.
    for (frame, dotted, when) in [
        (&frames[0], false, "untouched"),
        (
            &frames[DRAGS[0].step + 1],
            true,
            "after a committed custom temperature",
        ),
        (reset, false, "back at As shot and 0 EV"),
    ] {
        ensure(
            frame["state"]["active"][RAW_MODULE] == json!(dotted),
            format!(
                "The RAW band's dot is {} {when}, not {dotted}",
                frame["state"]["active"][RAW_MODULE]
            ),
        )?;
        checks.push(
            json!({"frame": frame["file"], "when": when, "raw_dot": dotted,
            "revision": frame["state"]["stack"]["revision"]}),
        );
    }
    checks.push(raw_crop(evidence, app, events, frames)?);
    write_json(&evidence.join("raw-panel-checks.json"), &json!(checks))?;
    Ok(())
}

/// Under As shot, a frame's temperature and tint fields show the camera's as-shot equivalent: the
/// core's answer for the frame's own RAW layer, to the precision each field declares, and a
/// temperature and tint whose gains are the as-shot gains. Returns what was compared.
fn shows_as_shot_equivalent(frame: &Value, field: &str) -> Result<Value> {
    let payload: lightwell_core::RawPayload = serde_json::from_value(raw_payload(frame)?.clone())?;
    let [kelvin, tint] =
        lightwell_core::temperature_tint_from_gains(payload.as_shot_gains, payload.cam_xyz)
            .map_err(|error| format!("{field}: the as-shot gains have no equivalent: {error}"))?;
    let back = lightwell_core::gains_from_temperature_tint(kelvin, tint, payload.cam_xyz)?;
    ensure(
        back.iter()
            .zip(payload.as_shot_gains)
            .all(|(gain, shot)| (gain - shot).abs() <= 1.0e-6 * shot),
        format!("{field}: {kelvin} K, {tint} does not reproduce the as-shot gains"),
    )?;
    let controls = &frame["state"]["controls"];
    let registry = lightwell_core::ModuleRegistry::builtin();
    for (action, parameter, expected) in [
        ("set-raw-temperature", "kelvin", kelvin),
        ("set-raw-tint", "tint", tint),
    ] {
        let key = format!("{action}.{parameter}");
        let shown: f64 = controls[&key]
            .as_str()
            .ok_or_else(|| format!("{key} is not shown"))?
            .parse()?;
        // The text is the value rounded to the decimals the parameter declares: half the last one.
        let precision = registry
            .action(action)
            .and_then(|(_, declared)| declared.parameter(parameter))
            .and_then(|declared| declared.precision)
            .ok_or_else(|| format!("{key} declares no precision"))?;
        let tolerance = 0.5 * 10f64.powi(-i32::from(precision)) + 1e-9;
        ensure(
            (shown - expected).abs() <= tolerance,
            format!("{field}: {key} shows {shown}, not the as-shot {expected}"),
        )?;
    }
    Ok(json!({
        "kelvin": controls["set-raw-temperature.kelvin"],
        "tint": controls["set-raw-tint.tint"],
        "as_shot_equivalent": [kelvin, tint],
        "as_shot_gains": payload.as_shot_gains,
    }))
}

/// The step's events that would mean a commit's picture never reached the canvas: a refused
/// request, a failed render, a picture withdrawn for a failure, or a draft that could not open.
fn expect_no_failure(events: &[Value], step: usize, what: &str) -> Result {
    let failures: Vec<&Value> = step_events(events, step)
        .into_iter()
        .filter(|event| {
            [
                "command_failed",
                "render_failed",
                "preview_withdrawn",
                "crop_draft_failed",
            ]
            .contains(&event["event"].as_str().unwrap_or_default())
        })
        .collect();
    ensure(
        failures.is_empty(),
        format!("{what}: a failure was logged: {failures:?}"),
    )
}

/// The one crop layer of a frame's current stack: its identity and payload.
fn crop_layer(frame: &Value) -> Result<(String, lightwell_core::CropPayload)> {
    let layers: Vec<&Value> = frame["state"]["stack"]["layers"]
        .as_array()
        .ok_or("The frame records no stack")?
        .iter()
        .filter(|layer| layer["effect"] == lightwell_core::CROP_EFFECT)
        .collect();
    ensure(
        layers.len() == 1,
        format!("Expected one crop layer, found {}", layers.len()),
    )?;
    Ok((
        layers[0]["id"].as_str().unwrap_or_default().to_owned(),
        serde_json::from_value(layers[0]["payload"].clone())?,
    ))
}

/// A committed crop frame shows that crop: the entry on the surface is the current one, its
/// dimensions are the output the payload declares on the RAW's upright stage, and no failure
/// stands in for it. This is the check a picture left over from before the commit fails.
fn shows_crop(frame: &Value, source: [u32; 2], angle: f64, what: &str) -> Result<[u32; 2]> {
    let state = &frame["state"];
    let (_, payload) = crop_layer(frame)?;
    ensure(
        payload.angle == angle,
        format!("{what}: the crop layer's angle is {}", payload.angle),
    )?;
    let stage = lightwell_core::CropStage {
        width: source[0],
        height: source[1],
        angle,
    };
    let output = payload.output_rect(&stage)?;
    let output = [output.width, output.height];
    let displayed = &state["stack"]["displayed"];
    ensure(
        displayed["entry"] == state["stack"]["entry"],
        format!(
            "{what}: the surface shows entry {} while history's current entry is {}",
            displayed["entry"], state["stack"]["entry"]
        ),
    )?;
    ensure(
        displayed["dimensions"] == json!(output),
        format!(
            "{what}: the surface shows {} for a {output:?} crop",
            displayed["dimensions"]
        ),
    )?;
    ensure(
        state["render_error"].is_null() && state["notices"] == json!([]),
        format!(
            "{what}: {} with notices {}",
            state["render_error"], state["notices"]
        ),
    )?;
    Ok(output)
}

/// The canvas background, read inside the canvas's own corner, which no photograph reaches.
fn canvas_background(image: &image::RgbImage, frame: &Value) -> Result<([u8; 3], [u32; 4])> {
    let rect: [u32; 4] = serde_json::from_value(frame["canvas_rect"].clone())
        .map_err(|_| "The frame records no canvas rectangle")?;
    Ok((image.get_pixel(rect[0] + 4, rect[1] + 4).0, rect))
}

/// A straightened draft draws its whole input stage as one rotated picture: sampled on a grid over
/// the stage's interior, mapped through the draft's Fit view and rotation, almost no sample shows
/// the canvas background. Drawn as the toolkit's own fragments of an image wider than one atlas
/// layer, each turned about its own centre, it showed the background through 12% of these samples
/// on the X100VI at 7°, and 66% at 44°.
fn draft_is_whole(path: &Path, frame: &Value, source: [u32; 2]) -> Result<Value> {
    let draft = &frame["state"]["crop"];
    let angle = draft["angle"]
        .as_f64()
        .ok_or("The draft records no angle")?;
    let stage = lightwell_core::CropStage {
        width: source[0],
        height: source[1],
        angle,
    };
    let (box_width, box_height) = stage.bounding_box();
    let image = image::open(path)?.to_rgb8();
    let (background, [left, top, right, bottom]) = canvas_background(&image, frame)?;
    let scale = frame["scale"]
        .as_f64()
        .ok_or("The frame records no scale")?;
    // The Fit view: the canvas less the photo padding, the rotated box centred in it.
    let padding = 20.0 * scale;
    let available = (
        f64::from(right - left) - 2.0 * padding,
        f64::from(bottom - top) - 2.0 * padding,
    );
    let zoom = (available.0 / box_width).min(available.1 / box_height);
    let origin = (
        f64::from(left) + padding + (available.0 - box_width * zoom) / 2.0,
        f64::from(top) + padding + (available.1 - box_height * zoom) / 2.0,
    );
    // The draft bar and the mode strip are drawn over the canvas; rows under them are skipped.
    let (first_row, last_row) = (
        f64::from(top) + 70.0 * scale,
        f64::from(bottom) - 70.0 * scale,
    );
    let (mut sampled, mut background_samples) = (0u32, 0u32);
    for j in 0..60 {
        for i in 0..80 {
            let u = f64::from(source[0]) * (0.03 + 0.94 * (f64::from(i) + 0.5) / 80.0);
            let v = f64::from(source[1]) * (0.03 + 0.94 * (f64::from(j) + 0.5) / 60.0);
            let (x, y) = stage.to_box(u, v);
            let (sx, sy) = (origin.0 + x * zoom, origin.1 + y * zoom);
            if sy < first_row || sy > last_row {
                continue;
            }
            sampled += 1;
            let pixel = image.get_pixel(sx as u32, sy as u32).0;
            if pixel
                .iter()
                .zip(background)
                .all(|(a, b)| a.abs_diff(b) <= 1)
            {
                background_samples += 1;
            }
        }
    }
    let share = f64::from(background_samples) / f64::from(sampled.max(1));
    ensure(
        sampled >= 2000 && share < 0.01,
        format!(
            "The straightened draft shows the canvas through {background_samples} of {sampled} samples of its input stage ({:.1}%): it is not drawn as one picture",
            share * 100.0
        ),
    )?;
    Ok(json!({
        "angle": angle,
        "samples": sampled,
        "background_samples": background_samples,
        "background_rgb": background,
        "threshold_share": 0.01,
    }))
}

/// A committed crop at Fit: the photograph measured on the canvas is the crop's output fitted into
/// the photo area and centred in it, not the picture from before the commit.
fn fit_placement(path: &Path, frame: &Value, output: [u32; 2]) -> Result<Value> {
    let image = image::open(path)?.to_rgb8();
    let (background, [left, top, right, bottom]) = canvas_background(&image, frame)?;
    let scale = frame["scale"]
        .as_f64()
        .ok_or("The frame records no scale")?;
    let padding = 20.0 * scale;
    let available = (
        f64::from(right - left) - 2.0 * padding,
        f64::from(bottom - top) - 2.0 * padding,
    );
    let fit = (available.0 / f64::from(output[0])).min(available.1 / f64::from(output[1]));
    let expected = (f64::from(output[0]) * fit, f64::from(output[1]) * fit);
    let photo = |x: u32, y: u32| {
        image
            .get_pixel(x, y)
            .0
            .iter()
            .zip(background)
            .any(|(a, b)| a.abs_diff(b) > 1)
    };
    // One row through the middle and one column a quarter of the way in, clear of the mode strip.
    let row = (top + bottom) / 2;
    let column = left + (right - left) / 4;
    let span = |positions: Vec<u32>| {
        positions
            .first()
            .zip(positions.last())
            .map(|(a, b)| (*a, *b))
    };
    let (x0, x1) = span((left..right).filter(|x| photo(*x, row)).collect())
        .ok_or("No photograph on the canvas's middle row")?;
    let (y0, y1) = span((top..bottom).filter(|y| photo(column, *y)).collect())
        .ok_or("No photograph on the canvas's quarter column")?;
    let measured = (f64::from(x1 - x0 + 1), f64::from(y1 - y0 + 1));
    let centre = (
        (f64::from(x0) + f64::from(x1) + 1.0) / 2.0,
        (f64::from(y0) + f64::from(y1) + 1.0) / 2.0,
    );
    let wanted_centre = (f64::from(left + right) / 2.0, f64::from(top + bottom) / 2.0);
    let tolerance = 4.0;
    ensure(
        (measured.0 - expected.0).abs() <= tolerance
            && (measured.1 - expected.1).abs() <= tolerance
            && (centre.0 - wanted_centre.0).abs() <= tolerance
            && (centre.1 - wanted_centre.1).abs() <= tolerance,
        format!(
            "The photograph measures {measured:?} at {centre:?}; the {output:?} crop fits as {expected:?} at {wanted_centre:?}"
        ),
    )?;
    Ok(json!({
        "output": output,
        "measured": [measured.0, measured.1],
        "expected": [expected.0, expected.1],
        "centre": [centre.0, centre.1],
        "tolerance_px": tolerance,
    }))
}

/// A pointer readout over the committed crop at 100%: the codes `render.sample` answered for the
/// stage pixel under the pointer are the codes the canvas shows at that pixel, one stage pixel per
/// physical pixel from the canvas's corner less the pan. The readout is the owner's own point
/// evaluation of the current stack, so this ties the picture on screen to the committed recipe.
fn readout_on_screen(
    path: &Path,
    frame: &Value,
    point: (u32, u32),
    output: [u32; 2],
) -> Result<Value> {
    let state = &frame["state"];
    ensure(
        state["surface"]["raster"] == json!(output) && state["proxy"]["presented"] == json!(false),
        format!(
            "The 100% view is not the exact {output:?} crop: raster {}",
            state["surface"]["raster"]
        ),
    )?;
    let readout = &state["readout"];
    ensure(
        readout["x"] == json!(point.0) && readout["y"] == json!(point.1),
        format!("The readout is {readout}, not at {point:?}"),
    )?;
    let codes: [u8; 4] = serde_json::from_value(readout["rgba"].clone())
        .map_err(|_| format!("The readout carries no codes: {readout}"))?;
    let image = image::open(path)?.to_rgb8();
    let (_, [left, top, _, _]) = canvas_background(&image, frame)?;
    let scale = frame["scale"]
        .as_f64()
        .ok_or("The frame records no scale")?;
    let view = &state["surface"]["view"];
    let pan = (
        view["pan_x"].as_f64().unwrap_or_default() * scale,
        view["pan_y"].as_f64().unwrap_or_default() * scale,
    );
    let screen = (
        (f64::from(left) - pan.0 + f64::from(point.0)).round() as u32,
        (f64::from(top) - pan.1 + f64::from(point.1)).round() as u32,
    );
    let shown = image.get_pixel(screen.0, screen.1).0;
    ensure(
        shown
            .iter()
            .zip(codes)
            .all(|(shown, code)| shown.abs_diff(code) <= 1),
        format!(
            "Stage pixel {point:?} reads {codes:?} but the canvas shows {shown:?} at {screen:?}"
        ),
    )?;
    Ok(json!({
        "point": [point.0, point.1],
        "screen": [screen.0, screen.1],
        "readout": codes,
        "shown": shown,
        "tolerance_codes": 1,
    }))
}

/// The straightened crop drafted, applied and inspected on the RAW itself: the draft draws its
/// whole input stage, Apply commits one entry whose picture is the one on screen at Fit and at
/// 100%, where the pointer readout's codes are the canvas's own, and a `crop-fit` through the API
/// at 100% updates the same layer and is shown the same way.
fn raw_crop(evidence: &Path, app: &Value, events: &[Value], frames: &[Value]) -> Result<Value> {
    let at = |offset: usize| &frames[FIRST_CROP_STEP + offset];
    let source: [u32; 2] = serde_json::from_value(frames[0]["state"]["source_dimensions"].clone())
        .map_err(|_| "The open frame records no source dimensions")?;
    let tiles = source[0].div_ceil(2048) * source[1].div_ceil(2048);

    let opened = at(0);
    expect_no_failure(events, FIRST_CROP_STEP, "The draft's start")?;
    let draft = &opened["state"]["crop"];
    ensure(
        draft["drafting"] == json!(true)
            && draft["input_stage"] == json!(source)
            && draft["input_stage_tiles"] == json!(tiles)
            && tiles > 1,
        format!("The draft did not open on the {source:?} stage in {tiles} tiles: {draft}"),
    )?;
    let straightened = at(2);
    ensure(
        straightened["state"]["crop"]["angle"] == json!(CROP_ANGLE),
        "The draft was not straightened",
    )?;
    let whole = draft_is_whole(
        &frame_identity(evidence, app, straightened)?,
        straightened,
        source,
    )?;

    let applied = at(3);
    expect_no_failure(events, FIRST_CROP_STEP + 3, "Apply")?;
    ensure(
        step_events(events, FIRST_CROP_STEP + 3)
            .iter()
            .filter(|event| event["event"] == "crop_draft_applied")
            .count()
            == 1
            && revision(applied)? == revision(straightened)? + 1
            && applied["state"]["crop"]["drafting"] == json!(false),
        "Apply did not commit exactly one entry and end the draft",
    )?;
    let output = shows_crop(applied, source, CROP_ANGLE, "The applied crop at Fit")?;
    ensure(
        applied["state"]["proxy"]["presented"] == json!(true),
        "The applied crop at Fit is not the display proxy",
    )?;
    let placement = fit_placement(&frame_identity(evidence, app, applied)?, applied, output)?;

    let exact = at(4);
    shows_crop(exact, source, CROP_ANGLE, "The applied crop at 100%")?;
    let mut readouts = Vec::new();
    for (offset, point) in [(5, READOUTS[0]), (6, READOUTS[1])] {
        let frame = at(offset);
        shows_crop(frame, source, CROP_ANGLE, "A readout over the applied crop")?;
        readouts.push(readout_on_screen(
            &frame_identity(evidence, app, frame)?,
            frame,
            point,
            output,
        )?);
    }

    let fitted = at(7);
    expect_no_failure(events, FIRST_CROP_STEP + 7, "The API's crop-fit")?;
    ensure(
        revision(fitted)? == revision(at(6))? + 1
            && crop_layer(fitted)?.0 == crop_layer(applied)?.0,
        "The API's crop-fit did not update the same crop layer in one entry",
    )?;
    let refitted = shows_crop(fitted, source, FIT_ANGLE, "The API's crop at 100%")?;
    ensure(
        fitted["state"]["surface"]["raster"] == json!(refitted),
        format!(
            "The API's crop at 100% is not drawn exactly: raster {}",
            fitted["state"]["surface"]["raster"]
        ),
    )?;
    let frame = at(8);
    shows_crop(frame, source, FIT_ANGLE, "A readout over the API's crop")?;
    readouts.push(readout_on_screen(
        &frame_identity(evidence, app, frame)?,
        frame,
        READOUTS[1],
        refitted,
    )?);
    let back = at(9);
    shows_crop(back, source, FIT_ANGLE, "The API's crop back at Fit")?;
    let back_placement = fit_placement(&frame_identity(evidence, app, back)?, back, refitted)?;
    Ok(json!({
        "crop": {
            "source": source,
            "tiles": tiles,
            "straightened_draft": whole,
            "applied": {"output": output, "fit": placement},
            "readouts": readouts,
            "api_crop_fit": {"angle": FIT_ANGLE, "output": refitted, "fit": back_placement},
        }
    }))
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
    let tint = keeps_the_tint_in_force(before, drafted, released, drag)?;
    Ok(json!({
        "step": drag_step,
        "kelvin": kelvin,
        "tint": tint,
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

/// A temperature drag keeps the tint in force, as Lightroom's Temp does: the committed payload's
/// tint is the core's answer for the development before the drag — for the first drag, which starts
/// from the untouched photograph, the camera's as-shot equivalent — and the Custom tint field reads
/// the same before the drag, while it is open and once it is released.
fn keeps_the_tint_in_force(
    before: &Value,
    drafted: &Value,
    released: &Value,
    drag: &Drag,
) -> Result<Value> {
    let prior: lightwell_core::RawPayload = serde_json::from_value(raw_payload(before)?.clone())?;
    if drag.step == DRAGS[0].step {
        ensure(
            prior.wb_mode == lightwell_core::WhiteBalanceMode::AsShot,
            "The first temperature drag does not start from As shot",
        )?;
    }
    let [_, in_force] = prior.white_balance_controls();
    let committed = raw_payload(released)?["tint"]
        .as_f64()
        .ok_or("The committed RAW layer has no tint")?;
    ensure(
        (committed - in_force).abs() <= 1e-9,
        format!("The temperature drag committed tint {committed}, not the {in_force} in force"),
    )?;
    let field = "set-raw-tint.tint";
    let shown = [before, drafted, released].map(|frame| frame["state"]["controls"][field].clone());
    ensure(
        shown
            .iter()
            .all(|text| *text == shown[0] && text.is_string()),
        format!("The Custom tint field moved during a temperature drag: {shown:?}"),
    )?;
    Ok(json!({
        "from": if prior.wb_mode == lightwell_core::WhiteBalanceMode::AsShot { "as-shot" } else { "custom" },
        "in_force": in_force,
        "committed": committed,
        "field": shown[0],
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

    /// The reset a number control declares for its own field, found the way `module.list` lists it.
    fn declared_reset(
        controls: &[lightwell_core::Control],
        action: &str,
        parameter: &str,
    ) -> Option<Option<lightwell_core::ResetAction>> {
        controls.iter().find_map(|control| match control {
            lightwell_core::Control::Group { controls, .. } => {
                declared_reset(controls, action, parameter)
            }
            lightwell_core::Control::Number {
                action: declared,
                parameter: named,
                reset,
                ..
            } if declared == action && named == parameter => Some(reset.clone()),
            _ => None,
        })
    }

    /// Every double-click names a declared slider field whose one value is a whole request and
    /// lands its first press inside the declared range and off the default. It expects the reset
    /// the control declares — As shot for the RAW temperature and tint — or, for a control that
    /// declares none, its own action and the declared default back, formatted as the field shows
    /// it.
    #[test]
    fn every_double_click_is_a_declared_drafting_field_and_its_declared_reset() {
        let registry = lightwell_core::ModuleRegistry::builtin();
        for click in &DOUBLE_CLICKS {
            let (module, action) = registry.action(click.action).expect("a declared action");
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
            let reset =
                declared_reset(&module.descriptor().controls, click.action, click.parameter)
                    .expect("a number control of the field");
            match reset {
                Some(reset) => {
                    assert_eq!(reset.action, click.reset, "{}", click.action);
                    assert!(reset.preset.is_empty());
                    assert_eq!(click.shows, None);
                }
                None => {
                    assert_eq!(click.reset, click.action);
                    let decimals = usize::from(parameter.precision.unwrap_or(0));
                    assert_eq!(Some(format!("{default:.decimals$}").as_str()), click.shows);
                }
            }
        }
    }

    // The gap stays inside the window iced gives a double-click's two presses.
    const _: () = assert!(GAP_MS <= 250);
}
