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
//! No RAW photograph is checked in (see `fixtures/README.md`), so this scenario is outside the
//! rendered tier of [`crate::smoke::SCENARIOS`] and takes its source from `--source`: an owner or raw.pixls.us file
//! the editor supports. It proves what the panel shows, what its double-click does and that a RAW
//! crop is drawn and shown, not RAW decoding, which `raw-editor` covers.
use crate::{
    scenario::{Checked, Frame, Plan, Run, Step, plan::only},
    *,
};
use lightwell_core::CROP_EFFECT;

pub const SCENARIO: &str = "raw-panel";
const RAW_MODULE: &str = "lightwell.raw";
const RAW_EFFECT: &str = "lightwell.raw";
const BASIC_MODULE: &str = "lightwell.basic";
const SET_TEMPERATURE: &str = "set-raw-temperature";

/// The steps the checks read by name, apart from the drags, double-clicks and readouts, whose
/// tables carry their own. The plan and the checks share each name, so a misspelt one does not
/// build: no RAW is checked in, so no recorded run would catch it.
mod names {
    pub const OPENED: &str = "opened";
    pub const BASIC_COLLAPSED: &str = "basic-collapsed";
    pub const CROP_STARTED: &str = "crop-started";
    pub const CROP_STRAIGHTENED: &str = "crop-straightened";
    pub const CROP_APPLIED: &str = "crop-applied";
    pub const CROP_AT_100: &str = "crop-at-100";
    pub const CROP_FITTED: &str = "crop-fitted";
    pub const FITTED_READOUT: &str = "fitted-readout";
    pub const FITTED_AT_FIT: &str = "fitted-at-fit";
}

/// One scripted temperature drag: the step that leaves it open, the step after it that releases it
/// at the same value, the value it stops on, and whether the view is at Fit, where the drafted
/// frame is the display proxy, or at 100%, where it is the full-size frame with no proxy phase.
struct Drag {
    drag: &'static str,
    release: &'static str,
    kelvin: f64,
    fit: bool,
}

/// At Fit, far enough from any camera's as-shot white balance to change the picture plainly; then
/// at 100%, far from that committed 3500 K. Both inside 2000..12000 K on the 10 K step.
const DRAGS: [Drag; 2] = [
    Drag {
        drag: "drag-at-fit",
        release: "release-at-fit",
        kelvin: 3500.0,
        fit: true,
    },
    Drag {
        drag: "drag-at-100",
        release: "release-at-100",
        kelvin: 2500.0,
        fit: false,
    },
];
/// Between a double-click's release and its second press: a person's ordinary double-click, well
/// inside the 300 ms iced gives the two presses.
const GAP_MS: u64 = 120;

/// One double-click the script makes: its step, the field, where its first press lands, the action
/// its reset runs, and what the field shows once that has run.
struct DoubleClick {
    step: &'static str,
    action: &'static str,
    parameter: &'static str,
    value: f64,
    /// The action the reset sends: the field's own, with its declared default, or the reset its
    /// control declares.
    reset: &'static str,
    /// The text the field shows after its reset: its declared default, or `None` for As shot,
    /// whose entry is labelled [`AS_SHOT_LABEL`] and whose temperature and tint are the camera's
    /// as-shot equivalent, computed from the frame's own RAW layer by the check.
    shows: Option<&'static str>,
}

const AS_SHOT: &str = "use-as-shot-wb";
const AS_SHOT_LABEL: &str = "As shot white balance";

const DOUBLE_CLICKS: [DoubleClick; 4] = [
    DoubleClick {
        step: "raw-exposure-reset",
        action: "set-raw-exposure",
        parameter: "ev",
        value: 0.35,
        reset: "set-raw-exposure",
        shows: Some("0.00"),
    },
    // Custom temperature and tint reset to the camera's own white balance, as Lightroom's Temp and
    // Tint do.
    DoubleClick {
        step: "raw-temperature-reset",
        action: "set-raw-temperature",
        parameter: "kelvin",
        value: 5000.0,
        reset: AS_SHOT,
        shows: None,
    },
    DoubleClick {
        step: "raw-tint-reset",
        action: "set-raw-tint",
        parameter: "tint",
        value: 12.0,
        reset: AS_SHOT,
        shows: None,
    },
    // Basic's own Exposure on the same photograph, for comparison: its commit does not wait for a
    // redevelopment.
    DoubleClick {
        step: "basic-exposure-reset",
        action: "set-basic",
        parameter: "exposure",
        value: 0.4,
        reset: "set-basic",
        shows: Some("0.00"),
    },
];

/// The draft's straightening angle, and the angle the API's `crop-fit` then commits.
const CROP_ANGLE: f64 = 7.0;
const FIT_ANGLE: f64 = -12.0;
/// Where the pointer readouts sample the committed crop at 100%, and the steps that hover there:
/// stage pixels inside the corner of the crop the canvas shows at a zero pan, clear of the scroll
/// bars and the mode strip, for any supplied RAW (the smallest crop, the Z6's, is over 2000 px each
/// way). The API's crop is read again at the second.
const READOUTS: [(&str, (u32, u32)); 2] = [
    ("crop-readout-near", (300, 200)),
    ("crop-readout-far", (1100, 700)),
];

fn hover((x, y): (u32, u32)) -> Value {
    json!({"hover":{"x":x,"y":y}})
}

/// The crop steps follow the double-clicks: a draft opened on the RAW's whole input stage, given
/// a 16:9 ratio and straightened, applied at Fit, inspected at 100% through two pointer readouts,
/// replaced by a `crop-fit` through the API at 100% and read again, then Fit.
fn crop_steps() -> Vec<Step> {
    vec![
        Step::new(names::CROP_STARTED, json!({"draft":{"start":true}})),
        Step::new("crop-ratio", json!({"draft":{"preset":"16:9"}})),
        Step::new(
            names::CROP_STRAIGHTENED,
            json!({"draft":{"angle":CROP_ANGLE}}),
        ),
        // Apply commits one entry.
        Step::new(names::CROP_APPLIED, json!({"draft":{"apply":true}})).commits(1),
        Step::new(names::CROP_AT_100, json!({"view":{"zoom":100.0}})),
        Step::new(READOUTS[0].0, hover(READOUTS[0].1)),
        Step::new(READOUTS[1].0, hover(READOUTS[1].1)),
        // The API's `crop-fit` updates the applied crop's own layer in one entry.
        Step::new(
            names::CROP_FITTED,
            json!({"api":{"method":"edit.crop-fit","params":{"aspect":"3:2","angle":FIT_ANGLE}}}),
        )
        .commits(1)
        .same_layer(CROP_EFFECT, names::CROP_APPLIED),
        Step::new(names::FITTED_READOUT, hover(READOUTS[1].1)),
        Step::new(names::FITTED_AT_FIT, json!({"view":{"zoom":"fit"}})),
    ]
}

/// Every frame, in order: the open, then one per step. The expectations here are what each step
/// commits, what its fields and label show and that the RAW section is on screen; `verify` checks
/// the rest.
pub fn plan(_: &[PathBuf]) -> Plan {
    let mut steps = vec![
        Step::opened(names::OPENED),
        // Collapse Basic, expanded by its own default, so the RAW section above it and the
        // collapsed bands under it are on screen together.
        Step::new(
            names::BASIC_COLLAPSED,
            json!({"section":{"module":BASIC_MODULE,"expanded":false}}),
        )
        .collapsed(BASIC_MODULE),
    ];
    for drag in &DRAGS {
        if !drag.fit {
            // 100%, where a frame shows a stage pixel per display pixel and has no proxy.
            steps.push(Step::new("zoom-100", json!({"view":{"zoom":100.0}})));
        }
        // A Custom temperature drag left open. Its frame is the drafted value approximated on the
        // planes developed at the committed white balance.
        steps.push(Step::new(
            drag.drag,
            json!({"slider":{"action":SET_TEMPERATURE,"parameter":"kelvin","values":[drag.kelvin]}}),
        ));
        // Its release at the same value, which commits it and redevelops the mosaic.
        steps.push(
            Step::new(
                drag.release,
                json!({"slider":{"action":SET_TEMPERATURE,"parameter":"kelvin","values":[drag.kelvin],"release":true}}),
            )
            .commits(1)
            .no_draft(),
        );
        if !drag.fit {
            // Back to Fit for the double-clicks.
            steps.push(Step::new("zoom-fit", json!({"view":{"zoom":"fit"}})));
        }
    }
    // A double-click on each RAW slider's rail, then on Basic's Exposure. The first press moves the
    // value, which commits on release; the second press resets the field: two entries.
    steps.extend(DOUBLE_CLICKS.iter().map(|click| {
        let step = Step::new(
            click.step,
            json!({"double_click":{"action":click.action,"parameter":click.parameter,
                "value":click.value,"gap_ms":GAP_MS}}),
        )
        .commits(2);
        match click.shows {
            Some(default) => step.field(click.action, click.parameter, default),
            None => step.label(AS_SHOT_LABEL),
        }
    }));
    // A straightened crop drafted, applied and inspected; see `crop_steps`.
    steps.extend(crop_steps());
    // The tools panel lists the RAW section only for a RAW source, so its section expanded in every
    // frame is the proof that the source opened as RAW.
    Plan::new(
        steps
            .into_iter()
            .map(|step| step.expanded(RAW_MODULE))
            .collect(),
    )
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
fn surface_difference(first: &Frame, second: &Frame) -> Result<(f64, f64)> {
    let frame = second;
    let first = first.image()?;
    let second = second.image()?;
    ensure(
        first.dimensions() == second.dimensions(),
        "The two captures are different sizes",
    )?;
    let (width, height) = first.dimensions();
    let [left, right] = frame.columns()?.unwrap_or([0, width]);
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

/// The events the named step logged. Its number in the script is the one its frame records, which
/// the plan's check has held to the step's own place in the script.
fn step_log<'a>(launch: &'a Checked, step: &str) -> Result<Vec<&'a Value>> {
    let number = launch.at(step)?["step"]["step"]
        .as_u64()
        .ok_or_else(|| format!("Step {step:?} records no script step number"))?;
    Ok(step_events(&launch.events, number as usize))
}

/// The frame captured just before the named step's.
fn frame_before<'a>(launch: &'a Checked, step: &str) -> Result<&'a Frame> {
    launch
        .index(step)?
        .checked_sub(1)
        .map(|before| &launch.frames[before])
        .ok_or_else(|| format!("No frame comes before step {step:?}").into())
}

/// What each frame shows beyond its plan, once the plan has held: every frame ready, the drags'
/// drafted and committed frames, each double-click's events and As shot fields, the RAW band's dot
/// and the crop on screen.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Vec::new();
    for (step, frame) in launch.names().iter().zip(&launch.frames) {
        let state = frame.state();
        ensure(
            state["phase"] == "ready",
            format!("RAW panel step {step:?} is not ready: {}", state["phase"]),
        )?;
        checks.push(json!({
            "step": step,
            "frame": frame["file"],
            "expanded": state["expanded"],
            "source_dimensions": state["source_dimensions"],
            "revision": state["stack"]["revision"],
            "controls": state["controls"],
        }));
    }
    // The plan holds Basic collapsed, which a section the frame does not list at all would pass;
    // that the section is listed, and listed collapsed, is this check's.
    ensure(
        launch.at(names::BASIC_COLLAPSED)?.state()["expanded"][BASIC_MODULE] == json!(false),
        "The Basic section is not listed collapsed once its step has collapsed it",
    )?;
    for drag in &DRAGS {
        checks.push(white_balance_drag(launch, drag)?);
    }

    // Each double-click is two history entries, which the plan counts: the first press's committed
    // jump, then the reset, sent against the revision that commit produced and never refused as
    // stale. The plan also holds the field's default, or As shot's label, once the reset has run.
    for click in &DOUBLE_CLICKS {
        let (before, after) = (frame_before(launch, click.step)?, launch.at(click.step)?);
        let field = format!("{}.{}", click.action, click.parameter);
        let logged = step_log(launch, click.step)?;
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
                && sent[0]["detail"]["revision"] == json!(before.revision()? + 1),
            format!(
                "{field}: the reset was not {} sent once, after the jump's commit: {sent:?}",
                click.reset
            ),
        )?;
        ensure(
            named("slider_draft_commit").len() == 1,
            format!("{field}: the first press did not commit its jump once"),
        )?;
        let mut shown = json!({"field": after["state"]["controls"][&field]});
        if click.shows.is_none() {
            // As shot: the development is the camera's own white balance, and both white-balance
            // fields show its equivalent.
            ensure(
                raw_payload(after)?["wb_mode"] == "as-shot",
                format!("{field}: the reset left {}", raw_payload(after)?),
            )?;
            shown = shows_as_shot_equivalent(after, &field)?;
        }
        checks.push(json!({
            "step": click.step,
            "field": field,
            "reset": click.reset,
            "label": after["state"]["stack"]["label"],
            "revision_before": before.revision()?,
            "revision_after": after.revision()?,
            "reset_sent_at_revision": sent[0]["detail"]["revision"],
            "queued": !named("field_reset_queued").is_empty(),
            "shown": shown,
        }));
    }
    // The RAW development the resets leave, once the last RAW double-click has run: exposure back
    // at 0 EV and the camera's own white balance, which is the Original's development, so the band
    // carries no dot again.
    let last = DOUBLE_CLICKS
        .iter()
        .rfind(|click| click.action.starts_with("set-raw-"))
        .ok_or("No double-click resets a RAW field")?;
    let reset = launch.at(last.step)?;
    let raw = raw_payload(reset)?;
    ensure(
        raw["exposure_ev"] == json!(0.0) && raw["wb_mode"] == "as-shot",
        format!("The RAW layer after the three resets is {raw}"),
    )?;
    // The band's dot: none on the untouched photograph, one once a drag has committed a custom
    // white balance, and none again once the resets leave As shot at 0 EV.
    for (frame, dotted, when) in [
        (launch.at(names::OPENED)?, false, "untouched"),
        (
            launch.at(DRAGS[0].release)?,
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
    checks.push(raw_crop(launch)?);
    write_json(
        &launch.evidence.join("raw-panel-checks.json"),
        &json!(checks),
    )?;
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
fn expect_no_failure(launch: &Checked, step: &str, what: &str) -> Result {
    let failures: Vec<&Value> = step_log(launch, step)?
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

/// The one crop layer of a frame's current stack: its payload. That a later commit updates this
/// same layer is the plan's.
fn crop_layer(frame: &Value) -> Result<lightwell_core::CropPayload> {
    let layers: Vec<&Value> = frame["state"]["stack"]["layers"]
        .as_array()
        .ok_or("The frame records no stack")?
        .iter()
        .filter(|layer| layer["effect"] == CROP_EFFECT)
        .collect();
    ensure(
        layers.len() == 1,
        format!("Expected one crop layer, found {}", layers.len()),
    )?;
    Ok(serde_json::from_value(layers[0]["payload"].clone())?)
}

/// A committed crop frame shows that crop: the entry on the surface is the current one, its
/// dimensions are the output the payload declares on the RAW's upright stage, and no failure
/// stands in for it. This is the check a picture left over from before the commit fails.
fn shows_crop(frame: &Value, source: [u32; 2], angle: f64, what: &str) -> Result<[u32; 2]> {
    let state = &frame["state"];
    let payload = crop_layer(frame)?;
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
fn draft_is_whole(frame: &Frame, source: [u32; 2]) -> Result<Value> {
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
    let image = frame.image()?;
    let (background, [left, top, right, bottom]) = canvas_background(image, frame)?;
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
fn fit_placement(frame: &Frame, output: [u32; 2]) -> Result<Value> {
    let image = frame.image()?;
    let (background, [left, top, right, bottom]) = canvas_background(image, frame)?;
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
fn readout_on_screen(frame: &Frame, point: (u32, u32), output: [u32; 2]) -> Result<Value> {
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
    let image = frame.image()?;
    let (_, [left, top, _, _]) = canvas_background(image, frame)?;
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
/// at 100% updates the same layer and is shown the same way. The entries each commit makes, and
/// that the `crop-fit` keeps the applied crop's layer, are the plan's.
fn raw_crop(launch: &Checked) -> Result<Value> {
    let source: [u32; 2] =
        serde_json::from_value(launch.at(names::OPENED)?["state"]["source_dimensions"].clone())
            .map_err(|_| "The open frame records no source dimensions")?;
    let tiles = source[0].div_ceil(2048) * source[1].div_ceil(2048);

    let started = launch.at(names::CROP_STARTED)?;
    expect_no_failure(launch, names::CROP_STARTED, "The draft's start")?;
    let draft = &started["state"]["crop"];
    ensure(
        draft["drafting"] == json!(true)
            && draft["input_stage"] == json!(source)
            && draft["input_stage_tiles"] == json!(tiles)
            && tiles > 1,
        format!("The draft did not open on the {source:?} stage in {tiles} tiles: {draft}"),
    )?;
    let straightened = launch.at(names::CROP_STRAIGHTENED)?;
    ensure(
        straightened["state"]["crop"]["angle"] == json!(CROP_ANGLE),
        "The draft was not straightened",
    )?;
    let whole = draft_is_whole(straightened, source)?;

    let applied = launch.at(names::CROP_APPLIED)?;
    expect_no_failure(launch, names::CROP_APPLIED, "Apply")?;
    ensure(
        step_log(launch, names::CROP_APPLIED)?
            .iter()
            .filter(|event| event["event"] == "crop_draft_applied")
            .count()
            == 1
            && applied["state"]["crop"]["drafting"] == json!(false),
        "Apply did not apply the draft once and end it",
    )?;
    let output = shows_crop(applied, source, CROP_ANGLE, "The applied crop at Fit")?;
    ensure(
        applied["state"]["proxy"]["presented"] == json!(true),
        "The applied crop at Fit is not the display proxy",
    )?;
    let placement = fit_placement(applied, output)?;

    let exact = launch.at(names::CROP_AT_100)?;
    shows_crop(exact, source, CROP_ANGLE, "The applied crop at 100%")?;
    let mut readouts = Vec::new();
    for (step, point) in READOUTS {
        let frame = launch.at(step)?;
        shows_crop(frame, source, CROP_ANGLE, "A readout over the applied crop")?;
        readouts.push(readout_on_screen(frame, point, output)?);
    }

    let fitted = launch.at(names::CROP_FITTED)?;
    expect_no_failure(launch, names::CROP_FITTED, "The API's crop-fit")?;
    let refitted = shows_crop(fitted, source, FIT_ANGLE, "The API's crop at 100%")?;
    ensure(
        fitted["state"]["surface"]["raster"] == json!(refitted),
        format!(
            "The API's crop at 100% is not drawn exactly: raster {}",
            fitted["state"]["surface"]["raster"]
        ),
    )?;
    let frame = launch.at(names::FITTED_READOUT)?;
    shows_crop(frame, source, FIT_ANGLE, "A readout over the API's crop")?;
    readouts.push(readout_on_screen(frame, READOUTS[1].1, refitted)?);
    let back = launch.at(names::FITTED_AT_FIT)?;
    shows_crop(back, source, FIT_ANGLE, "The API's crop back at Fit")?;
    let back_placement = fit_placement(back, refitted)?;
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
/// a code of the approximate one. That the release closes the draft in one entry is the plan's.
fn white_balance_drag(launch: &Checked, drag: &Drag) -> Result<Value> {
    let kelvin = drag.kelvin;
    let (before, drafted, released) = (
        frame_before(launch, drag.drag)?,
        launch.at(drag.drag)?,
        launch.at(drag.release)?,
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
    let drag_events = step_log(launch, drag.drag)?;
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
    let (drag_mean, drag_over) = surface_difference(before, drafted)?;
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
            && raw_payload(released)?["temperature_kelvin"] == json!(kelvin)
            && raw_payload(released)?["wb_mode"] == "custom",
        format!(
            "The release did not land the exact committed frame: approximate {}, RAW {}",
            state["approximate_white_balance"],
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
    let release_events = step_log(launch, drag.release)?;
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
    let (settled_mean, settled_over) = surface_difference(drafted, released)?;
    ensure(
        settled_mean < 1.0,
        format!("The exact frame is {settled_mean:.3} codes from the approximate one on average"),
    )?;
    let tint = keeps_the_tint_in_force(before, drafted, released, drag)?;
    Ok(json!({
        "step": drag.drag,
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
    if drag.drag == DRAGS[0].drag {
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

    /// What the plan scripts at the named step.
    fn scripted(plan: &Plan, step: &str) -> Value {
        let at = plan
            .index(step)
            .unwrap_or_else(|| panic!("{step:?} is not planned"));
        plan.steps()[at]
            .script()
            .cloned()
            .unwrap_or_else(|| panic!("{step:?} scripts nothing"))
    }

    /// Each drag stops on a value the temperature control declares, on its step, and its release is
    /// the step after it at the same value, with the zoom around the 100% one where the checks look.
    #[test]
    fn each_drag_is_a_declared_temperature_on_its_step_where_the_checks_look() {
        let registry = lightwell_core::ModuleRegistry::builtin();
        let (_, action) = registry.action(SET_TEMPERATURE).expect("a declared action");
        let parameter = action.parameter("kelvin").expect("a declared field");
        let lightwell_core::ParameterKind::Number { min, max } = parameter.kind else {
            panic!("kelvin is a number");
        };
        let step = parameter.step.unwrap_or(1.0);
        let plan = plan(&[]);
        for drag in &DRAGS {
            assert!((min..=max).contains(&drag.kelvin));
            assert_eq!((drag.kelvin / step).round() * step, drag.kelvin);
            let at = plan.index(drag.drag).expect("a planned drag");
            assert_eq!(plan.index(drag.release), Some(at + 1), "{}", drag.release);
            let open = &scripted(&plan, drag.drag)["slider"];
            let release = &scripted(&plan, drag.release)["slider"];
            assert_eq!(open["values"], json!([drag.kelvin]));
            assert_eq!(open["release"], Value::Null);
            assert_eq!(release["values"], json!([drag.kelvin]));
            assert_eq!(release["release"], json!(true));
            if !drag.fit {
                assert_eq!(
                    plan.steps()[at - 1].script(),
                    Some(&json!({"view":{"zoom":100.0}}))
                );
                assert_eq!(
                    plan.steps()[at + 2].script(),
                    Some(&json!({"view":{"zoom":"fit"}}))
                );
            }
        }
    }

    /// One frame for the open and one per script step, each step named for what it scripts and
    /// every frame held to the RAW section expanded; and the table's row runs this plan, outside
    /// `rendered`. No RAW run can be replayed, so this is what ties the names the checks read to
    /// the steps they mean.
    #[test]
    fn the_plan_is_the_open_and_one_frame_per_step_each_named_for_what_it_scripts() {
        let raw = [PathBuf::from("/raw/photo.nef")];
        let plan = plan(&raw);
        assert!(plan.validate().is_ok(), "{:?}", plan.validate());
        let script = plan.script();
        assert_eq!(
            script.as_array().map(|steps| steps.len() + 1),
            Some(plan.len())
        );
        let (open, steps) = plan.steps().split_first().expect("a planned open");
        assert_eq!(open.name(), names::OPENED);
        assert!(open.script().is_none() && steps.iter().all(|step| step.script().is_some()));
        assert!(plan.steps().iter().all(|step| {
            step.expect()
                .expanded
                .contains(&(RAW_MODULE.to_owned(), true))
        }));
        assert_eq!(
            scripted(&plan, names::BASIC_COLLAPSED),
            json!({"section":{"module":BASIC_MODULE,"expanded":false}})
        );
        for click in &DOUBLE_CLICKS {
            assert_eq!(
                scripted(&plan, click.step),
                json!({"double_click":{"action":click.action,"parameter":click.parameter,
                    "value":click.value,"gap_ms":GAP_MS}})
            );
        }
        for (step, request) in [
            (names::CROP_STARTED, json!({"draft":{"start":true}})),
            (
                names::CROP_STRAIGHTENED,
                json!({"draft":{"angle":CROP_ANGLE}}),
            ),
            (names::CROP_APPLIED, json!({"draft":{"apply":true}})),
            (names::CROP_AT_100, json!({"view":{"zoom":100.0}})),
            (READOUTS[0].0, hover(READOUTS[0].1)),
            (READOUTS[1].0, hover(READOUTS[1].1)),
            (
                names::CROP_FITTED,
                json!({"api":{"method":"edit.crop-fit","params":{"aspect":"3:2","angle":FIT_ANGLE}}}),
            ),
            (names::FITTED_READOUT, hover(READOUTS[1].1)),
            (names::FITTED_AT_FIT, json!({"view":{"zoom":"fit"}})),
        ] {
            assert_eq!(scripted(&plan, step), request, "{step}");
        }
        let row = crate::smoke::find(SCENARIO).unwrap();
        assert_eq!((row.launches[0].plan)(&raw).script(), script);
        assert!(!row.rendered());
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
