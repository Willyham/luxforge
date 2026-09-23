//! Rendered evidence for the opt-in control vocabulary and its identity photo layer.
use crate::{
    smoke::{columns, frame_identity, script_request_matches},
    *,
};

const MODULE: &str = "lightwell.controls";
const ACTION: &str = "set-controls";
const EFFECT: &str = "lightwell.controls.identity";
const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
pub const WINDOW: [&str; 2] = ["1440", "900"];

/// One import frame and one frame for each of the 25 real editor interactions.
pub fn frames(scenario: &str) -> Option<usize> {
    (scenario == "controls").then_some(26)
}

/// The developer pixel proof, whose section the script shows last: X and Y as px fields.
const PIXEL_MODULE: &str = "lightwell.pixel";

pub fn source(scenario: &str) -> Option<&'static str> {
    (scenario == "controls").then_some(FIXTURE)
}

pub fn script(scenario: &str) -> Option<Value> {
    (scenario == "controls").then(|| json!([
        // Expose the proof section, then capture both its beginning and end in the tools panel.
        {"section":{"module":"lightwell.basic","expanded":false}},
        {"section":{"module":"lightwell.crop","expanded":false}},
        {"section":{"module":MODULE,"expanded":true}},
        {"tools_scroll":0.5},
        {"picker":{"action":ACTION,"parameter":"rgb","open":true,"finish":"open"}},
        {"tools_scroll":1.0},
        // Continuous values are drafts until the release. The proof module is pixel identity.
        {"controls":{"action":ACTION,"parameter":"amount","gesture":"slider","fractions":[0.25,0.75],"finish":"open"}},
        {"controls":{"action":ACTION,"parameter":"amount","gesture":"slider","fractions":[0.75],"finish":"release"}},
        // A cancelled picker leaves both history and the photograph unchanged; the next commits.
        {"picker":{"action":ACTION,"parameter":"rgb","hue":0.125,"plane":[0.75,0.625],"finish":"open"}},
        {"picker":{"action":ACTION,"parameter":"rgb","hue":0.125,"plane":[0.75,0.625],"finish":"cancel"}},
        {"picker":{"action":ACTION,"parameter":"rgb","hue":0.875,"plane":[0.75,0.875],"finish":"release"}},
        // Master point add is discrete; moving the point drafts and commits once.
        {"curve":{"action":ACTION,"parameter":"master","event":"add","point":[0.25,0.25]}},
        {"curve":{"action":ACTION,"parameter":"master","event":"move","index":1,"points":[[0.375,0.375]],"finish":"open"}},
        {"curve":{"action":ACTION,"parameter":"master","event":"move","index":1,"points":[[0.375,0.375]],"finish":"release"}},
        {"curve":{"action":ACTION,"parameter":"master","event":"channel","index":1}},
        {"curve":{"action":ACTION,"parameter":"red","event":"move","index":1,"points":[[0.5,0.75]],"finish":"release"}},
        {"curve":{"action":ACTION,"parameter":"red","event":"remove","index":1}},
        // Discrete controls commit exactly once each.
        {"controls":{"action":ACTION,"parameter":"enabled","gesture":"discrete","value":true}},
        {"controls":{"action":ACTION,"parameter":"mode","gesture":"discrete","value":"two"}},
        // The proof's controls are its module's only group, which the panel draws without a
        // header and cannot collapse; group disclosure is a group of a module with several.
        {"group":{"module":"lightwell.basic","path":[2],"expanded":false}},
        {"group":{"module":"lightwell.basic","path":[2],"expanded":true}},
        {"reset":{"module":MODULE}},
        // The pixel proof's section on its own: X and Y as labelled px fields, RGB, the picker
        // and Apply pixel.
        {"section":{"module":MODULE,"expanded":false}},
        {"section":{"module":PIXEL_MODULE,"expanded":true}},
        {"tools_scroll":1.0}
    ]))
}

fn payload(frame: &Value) -> Option<&Value> {
    frame["state"]["stack"]["layers"]
        .as_array()?
        .iter()
        .find(|layer| layer["effect"] == EFFECT)
        .map(|layer| &layer["payload"])
}

fn revision(frame: &Value) -> Result<u64> {
    frame["state"]["stack"]["revision"]
        .as_u64()
        .ok_or_else(|| "Missing stack revision".into())
}

fn control_model<'a>(frame: &'a Value, kind: &str, parameter: &str) -> Option<&'a Value> {
    frame["state"]["control_ui"][kind]
        .as_array()?
        .iter()
        .find(|model| model["action"] == ACTION && model["parameter"] == parameter)
}

fn sidebar_difference(first: &Path, second: &Path, frame: &Value) -> Result<u32> {
    let first = image::open(first)?.to_rgb8();
    let second = image::open(second)?.to_rgb8();
    ensure(
        first.dimensions() == second.dimensions(),
        "Tools captures have different sizes",
    )?;
    let (width, height) = first.dimensions();
    let [_, surface_right] = columns(frame)?.unwrap_or([0, width]);
    ensure(
        surface_right + 40 < width,
        "Controls capture has no tools sidebar",
    )?;
    let mut changed = 0;
    for y in (80..height.saturating_sub(60)).step_by(4) {
        for x in (surface_right + 10..width - 10).step_by(4) {
            let a = first.get_pixel(x, y).0;
            let b = second.get_pixel(x, y).0;
            if a.iter().zip(b).any(|(a, b)| a.abs_diff(b) > 12) {
                changed += 1;
            }
        }
    }
    Ok(changed)
}

/// The control rail and picker can contain the fixture's quadrant colours in the sidebar. Scan
/// only the photo surface, then compare four interior points to the unedited orientation-1 image.
pub fn identity_photo(path: &Path, frame: &Value) -> Result<Value> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [surface_left, surface_right] = columns(frame)?.unwrap_or([0, width]);
    ensure(
        surface_left < surface_right && surface_right <= width,
        "Invalid photo surface",
    )?;
    let colours = fixtures::COLORS;
    let matches =
        |pixel: [u8; 3], colour: [u8; 3]| pixel.iter().zip(colour).all(|(a, b)| a.abs_diff(b) <= 8);
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    for y in (0..height).step_by(4) {
        for x in (surface_left..surface_right).step_by(4) {
            if colours
                .iter()
                .any(|colour| matches(image.get_pixel(x, y).0, *colour))
            {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 4);
                bottom = bottom.max(y + 4);
            }
        }
    }
    ensure(
        right > left + 120 && bottom > top + 80,
        "Identity photo is absent or too small",
    )?;
    let aspect = f64::from(right - left) / f64::from(bottom - top);
    ensure(
        (aspect - 1.5).abs() < 0.02,
        format!("Identity photo aspect is {aspect}"),
    )?;
    let mut samples = Vec::new();
    for ((fx, fy), colour) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)]
        .into_iter()
        .zip(colours)
    {
        let x = (f64::from(left) + fx * f64::from(right - left)).round() as u32;
        let y = (f64::from(top) + fy * f64::from(bottom - top)).round() as u32;
        ensure(
            x < width && y < height,
            "Identity photo sample outside capture",
        )?;
        let pixel = image.get_pixel(x, y).0;
        ensure(
            matches(pixel, colour),
            format!("Identity photo colour changed at {x},{y}"),
        )?;
        samples.push(pixel);
    }
    Ok(
        json!({"bounds":[left,top,right,bottom],"aspect":aspect,"corner_rgb":samples,
        "tolerance_per_channel":8,"scope":"Displayed photo surface; control sidebar excluded"}),
    )
}

/// Every step is backed by a renderer readback, state file and matching script event. The photo
/// checker proves that all proof edits kept the original's exact displayed fixture colours.
pub fn verify(evidence: &Path, app: &Value, events: &[Value]) -> Result {
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    let script = script("controls").expect("static script");
    let steps = script.as_array().unwrap();
    ensure(
        frames.len() == steps.len() + 1,
        "Wrong controls capture count",
    )?;
    ensure(
        app["had_input_errors"] == false,
        "Controls script reported an input error",
    )?;
    let proof = frames[0]["state"]["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|module| module["id"] == MODULE)
        .ok_or("Proof module not discovered")?;
    ensure(
        proof["available"] == true && frames[0]["state"]["developer"] == true,
        "Controls module not available in developer mode",
    )?;
    ensure(
        revision(&frames[0])? == 0 && payload(&frames[0]).is_none(),
        "Controls import did not start with an empty recipe",
    )?;

    let logged: Vec<&Value> = events
        .iter()
        .filter(|event| event["event"] == "script_step")
        .collect();
    ensure(
        logged.len() == steps.len(),
        "A controls script event is missing",
    )?;
    let mut checks = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let path = frame_identity(evidence, app, frame)?;
        let photo = identity_photo(&path, frame)?;
        let image = image::open(&path)?;
        ensure(
            image.width() >= 1440 && image.height() >= 900,
            format!("Controls capture {index} is too small to inspect"),
        )?;
        if index > 0 {
            let step = &frame["step"];
            ensure(
                step["step"] == index
                    && step["status"] == "sent"
                    && script_request_matches(&step["request"], &steps[index - 1]),
                format!("Controls frame {index} is not correlated with its scripted interaction"),
            )?;
            ensure(
                logged[index - 1]["detail"]["step"] == index
                    && script_request_matches(
                        &logged[index - 1]["detail"]["request"],
                        &steps[index - 1],
                    ),
                format!("Controls log step {index} disagrees with the capture"),
            )?;
        }
        checks.push(
            json!({"frame":frame["file"],"step":index,"revision":revision(frame)?,
            "payload":payload(frame),"photo":photo}),
        );
    }

    // Opening, scrolling and drafting do not create history. One release or discrete event does.
    let revisions = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 2, 3, 3, 4, 4, 5, 6, 7, 8, 8, 8, 9, 9, 9, 9,
    ];
    for (index, expected) in revisions.into_iter().enumerate() {
        ensure(
            revision(&frames[index])? == expected,
            format!("Controls frame {index} revision differs from one-gesture/one-commit contract"),
        )?;
    }
    ensure(
        frames[1]["state"]["expanded"]["lightwell.basic"] == false
            && frames[2]["state"]["expanded"]["lightwell.crop"] == false
            && frames[3]["state"]["expanded"][MODULE] == true,
        "The proof panel was not exposed by the section steps",
    )?;
    ensure(
        frames[4]["state"]["tools_scroll"] == json!(0.5)
            && frames[6]["state"]["tools_scroll"] == json!(1.0),
        "The tools panel did not retain its requested scroll fractions",
    )?;
    let upper = frame_identity(evidence, app, &frames[4])?;
    let lower = frame_identity(evidence, app, &frames[6])?;
    ensure(
        sidebar_difference(&upper, &lower, &frames[6])? >= 100,
        "The tools panel screenshots did not change when scrolled",
    )?;
    ensure(
        control_model(&frames[5], "pickers", "rgb").is_some_and(|picker| picker["open"] == true),
        "The picker popover was not open in its capture",
    )?;
    ensure(
        frames[7]["state"]["draft"].is_object()
            && frames[9]["state"]["draft"].is_object()
            && frames[13]["state"]["draft"].is_object(),
        "The open slider, picker and curve captures lack draft state",
    )?;
    ensure(
        frames[10]["state"]["draft"].is_null(),
        "Picker cancellation left an open draft",
    )?;
    ensure(
        payload(&frames[8]).is_some_and(|p| p["amount"] == json!(2.5)),
        "Amount slider did not persist the soft-range value",
    )?;
    ensure(
        payload(&frames[10]).is_some_and(|p| p.get("rgb").is_none()),
        "Cancelled picker changed committed RGB",
    )?;
    ensure(
        payload(&frames[11]).is_some_and(|p| p["rgb"].as_array().is_some()),
        "Released picker did not commit RGB",
    )?;
    ensure(
        payload(&frames[12]).is_some_and(|p| p["master"].as_array().is_some_and(|v| v.len() == 4)),
        "Curve add did not persist four master points",
    )?;
    ensure(
        payload(&frames[14]).is_some_and(|p| p["master"][1][0] == json!(0.375)),
        "Curve point move did not commit",
    )?;
    ensure(
        payload(&frames[17]).is_some_and(|p| p["red"].as_array().is_some_and(|v| v.len() == 2)),
        "Red curve point removal did not persist",
    )?;
    ensure(
        control_model(&frames[15], "curves", "red")
            .is_some_and(|curve| curve["channel"] == 1 && curve["sample_count"] == 257),
        "The selected red channel lacks its declared query samples",
    )?;
    ensure(
        control_model(&frames[16], "curves", "red").is_some_and(|curve| {
            curve["sample_source"] == payload(&frames[16]).unwrap()["red"]
                && curve["sample_source_entry"] == frames[16]["state"]["stack"]["entry"]
                && curve["sample_asset"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty())
        }),
        "The sampled red curve is not correlated to its points, asset and committed entry",
    )?;
    ensure(
        payload(&frames[18]).is_some_and(|p| p["enabled"] == true),
        "Toggle did not commit",
    )?;
    ensure(
        payload(&frames[19]).is_some_and(|p| p["mode"] == "two"),
        "Choice did not commit",
    )?;
    ensure(
        frames[20]["state"]["control_ui"]["group_expanded"]["lightwell.basic/2"] == false
            && frames[21]["state"]["control_ui"]["group_expanded"]["lightwell.basic/2"] == true
            && frames[21]["state"]["control_ui"]["group_expanded"]
                .get("lightwell.controls/0")
                .is_none(),
        "A group of a multi-group module did not collapse and expand",
    )?;
    ensure(
        payload(&frames[22]).is_some_and(|p| p.as_object().is_some_and(|o| o.is_empty())),
        "Module reset did not clear all proof values",
    )?;
    ensure(
        frames[23]["state"]["expanded"][MODULE] == false
            && frames[24]["state"]["expanded"][PIXEL_MODULE] == true
            && frames[25]["state"]["expanded"][PIXEL_MODULE] == true
            && frames[25]["state"]["expanded"][MODULE] == false
            && frames[25]["state"]["tools_scroll"] == json!(1.0),
        "The pixel proof's section was not shown on its own at the panel's end",
    )?;
    ensure(
        frames[25]["state"]["controls"]["set-pixel.x"].is_string()
            && frames[25]["state"]["controls"]["set-pixel.y"].is_string(),
        "The pixel proof's X and Y fields are not in the captured state",
    )?;
    ensure(
        events
            .iter()
            .any(|event| event["event"] == "slider_draft_preview")
            && events
                .iter()
                .any(|event| event["event"] == "slider_draft_commit")
            && events
                .iter()
                .any(|event| event["event"] == "preview_displayed"),
        "Control drafts or their displayed frames are not correlated in the log",
    )?;
    write_json(&evidence.join("controls-checks.json"), &json!(checks))?;
    Ok(())
}
