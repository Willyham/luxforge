//! Rendered evidence for the opt-in control vocabulary and its identity photo layer.
use crate::{
    scenario::{Checked, Frame, Plan, Run, Step, pixels, plan::only},
    *,
};

const MODULE: &str = "luxforge.controls";
const ACTION: &str = "set-controls";
const EFFECT: &str = "luxforge.controls.identity";

/// The developer pixel proof, whose section the script shows last: X and Y as px fields.
const PIXEL_MODULE: &str = "luxforge.pixel";

/// The label a commit of the proof's own action earns, and the one its module reset earns.
const SET: &str = "Set controls";
const RESET: &str = "Reset controls";

/// Every frame, in order: the open, then one per interaction. Opening, scrolling and drafting
/// create no history; one release or one discrete event makes exactly one entry.
pub fn plan(_: &[PathBuf]) -> Plan {
    let step = |name: &str, script: Value| Step::new(name, script);
    let set = |name: &str, script: Value| step(name, script).commits(1).label(SET);
    let view = |name: &str, script: Value| step(name, script).commits(0);
    Plan::new(vec![
        Step::opened("opened").no_layer(EFFECT),
        // Expose the proof section, then capture both its beginning and end in the tools panel.
        view(
            "basic-collapsed",
            json!({"section":{"module":"luxforge.basic","expanded":false}}),
        )
        .collapsed("luxforge.basic"),
        view(
            "crop-collapsed",
            json!({"section":{"module":"luxforge.crop","expanded":false}}),
        )
        .collapsed("luxforge.crop"),
        view(
            "controls-expanded",
            json!({"section":{"module":MODULE,"expanded":true}}),
        )
        .expanded(MODULE),
        view("scroll-half", json!({"tools_scroll":0.5})),
        view(
            "picker-open",
            json!({"picker":{"action":ACTION,"parameter":"rgb","open":true,"finish":"open"}}),
        ),
        view("scroll-end", json!({"tools_scroll":1.0})),
        // Continuous values are drafts until the release. The proof module is pixel identity.
        view(
            "slider-drag",
            json!({"controls":{"action":ACTION,"parameter":"amount","gesture":"slider","fractions":[0.25,0.75],"finish":"open"}}),
        ),
        set(
            "slider-release",
            json!({"controls":{"action":ACTION,"parameter":"amount","gesture":"slider","fractions":[0.75],"finish":"release"}}),
        ),
        // A cancelled picker leaves both history and the photograph unchanged; the next commits.
        view(
            "picker-drag",
            json!({"picker":{"action":ACTION,"parameter":"rgb","hue":0.125,"plane":[0.75,0.625],"finish":"open"}}),
        ),
        view(
            "picker-cancel",
            json!({"picker":{"action":ACTION,"parameter":"rgb","hue":0.125,"plane":[0.75,0.625],"finish":"cancel"}}),
        )
        .no_draft(),
        set(
            "picker-release",
            json!({"picker":{"action":ACTION,"parameter":"rgb","hue":0.875,"plane":[0.75,0.875],"finish":"release"}}),
        ),
        // Master point add is discrete; moving the point drafts and commits once.
        set(
            "curve-add",
            json!({"curve":{"action":ACTION,"parameter":"master","event":"add","point":[0.25,0.25]}}),
        ),
        view(
            "curve-drag",
            json!({"curve":{"action":ACTION,"parameter":"master","event":"move","index":1,"points":[[0.375,0.375]],"finish":"open"}}),
        ),
        set(
            "curve-release",
            json!({"curve":{"action":ACTION,"parameter":"master","event":"move","index":1,"points":[[0.375,0.375]],"finish":"release"}}),
        ),
        view(
            "red-channel",
            json!({"curve":{"action":ACTION,"parameter":"master","event":"channel","index":1}}),
        ),
        set(
            "red-move",
            json!({"curve":{"action":ACTION,"parameter":"red","event":"move","index":1,"points":[[0.5,0.75]],"finish":"release"}}),
        ),
        set(
            "red-remove",
            json!({"curve":{"action":ACTION,"parameter":"red","event":"remove","index":1}}),
        ),
        // Discrete controls commit exactly once each.
        set(
            "toggle",
            json!({"controls":{"action":ACTION,"parameter":"enabled","gesture":"discrete","value":true}}),
        ),
        set(
            "choice",
            json!({"controls":{"action":ACTION,"parameter":"mode","gesture":"discrete","value":"two"}}),
        ),
        // The proof's controls are its module's only group, which the panel draws without a
        // header and cannot collapse; group disclosure is a group of a module with several.
        view(
            "group-collapsed",
            json!({"group":{"module":"luxforge.basic","path":[2],"expanded":false}}),
        ),
        view(
            "group-expanded",
            json!({"group":{"module":"luxforge.basic","path":[2],"expanded":true}}),
        ),
        step("reset", json!({"reset":{"module":MODULE}}))
            .commits(1)
            .label(RESET)
            .payload(EFFECT, json!({})),
        // The pixel proof's section on its own: X and Y as labelled px fields, RGB, the picker
        // and Apply pixel.
        view(
            "controls-collapsed",
            json!({"section":{"module":MODULE,"expanded":false}}),
        )
        .collapsed(MODULE),
        view(
            "pixel-expanded",
            json!({"section":{"module":PIXEL_MODULE,"expanded":true}}),
        )
        .expanded(PIXEL_MODULE),
        view("pixel-scrolled", json!({"tools_scroll":1.0}))
            .expanded(PIXEL_MODULE)
            .collapsed(MODULE),
    ])
}

fn payload(frame: &Frame) -> Option<&Value> {
    frame.payload(EFFECT)
}

fn control_model<'a>(frame: &'a Value, kind: &str, parameter: &str) -> Option<&'a Value> {
    frame["state"]["control_ui"][kind]
        .as_array()?
        .iter()
        .find(|model| model["action"] == ACTION && model["parameter"] == parameter)
}

fn sidebar_difference(first: &Frame, second: &Frame) -> Result<u32> {
    let frame = second;
    let first = first.image()?;
    let second = second.image()?;
    ensure(
        first.dimensions() == second.dimensions(),
        "Tools captures have different sizes",
    )?;
    let (width, height) = first.dimensions();
    let [_, surface_right] = frame.columns()?.unwrap_or([0, width]);
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

/// Every step is backed by a renderer readback, state file and matching script event, which the
/// plan checks. The photo checker proves that all proof edits kept the original's exact displayed
/// fixture colours.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let at = |step: &str| launch.at(step);
    let opened = at("opened")?;
    let proof = opened["state"]["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|module| module["id"] == MODULE)
        .ok_or("Proof module not discovered")?;
    ensure(
        proof["available"] == true && opened["state"]["developer"] == true,
        "Controls module not available in developer mode",
    )?;
    ensure(
        opened.revision()? == 0,
        "Controls import did not start with an empty recipe",
    )?;

    let mut checks = Vec::new();
    for (name, frame) in launch.names().iter().zip(&launch.frames) {
        let photo = pixels::identity_photo(frame)?;
        let image = frame.image()?;
        ensure(
            image.width() >= 1440 && image.height() >= 900,
            format!("Controls capture {name:?} is too small to inspect"),
        )?;
        checks.push(
            json!({"frame":frame["file"],"step":name,"revision":frame.revision()?,
            "payload":payload(frame),"photo":photo}),
        );
    }

    ensure(
        at("scroll-half")?["state"]["tools_scroll"] == json!(0.5)
            && at("scroll-end")?["state"]["tools_scroll"] == json!(1.0),
        "The tools panel did not retain its requested scroll fractions",
    )?;
    ensure(
        sidebar_difference(at("scroll-half")?, at("scroll-end")?)? >= 100,
        "The tools panel screenshots did not change when scrolled",
    )?;
    ensure(
        control_model(at("picker-open")?, "pickers", "rgb")
            .is_some_and(|picker| picker["open"] == true),
        "The picker popover was not open in its capture",
    )?;
    ensure(
        at("slider-drag")?["state"]["draft"].is_object()
            && at("picker-drag")?["state"]["draft"].is_object()
            && at("curve-drag")?["state"]["draft"].is_object(),
        "The open slider, picker and curve captures lack draft state",
    )?;
    ensure(
        payload(at("slider-release")?).is_some_and(|p| p["amount"] == json!(2.5)),
        "Amount slider did not persist the soft-range value",
    )?;
    ensure(
        payload(at("picker-cancel")?).is_some_and(|p| p.get("rgb").is_none()),
        "Cancelled picker changed committed RGB",
    )?;
    ensure(
        payload(at("picker-release")?).is_some_and(|p| p["rgb"].as_array().is_some()),
        "Released picker did not commit RGB",
    )?;
    ensure(
        payload(at("curve-add")?)
            .is_some_and(|p| p["master"].as_array().is_some_and(|v| v.len() == 4)),
        "Curve add did not persist four master points",
    )?;
    ensure(
        payload(at("curve-release")?).is_some_and(|p| p["master"][1][0] == json!(0.375)),
        "Curve point move did not commit",
    )?;
    ensure(
        payload(at("red-remove")?)
            .is_some_and(|p| p["red"].as_array().is_some_and(|v| v.len() == 2)),
        "Red curve point removal did not persist",
    )?;
    ensure(
        control_model(at("red-channel")?, "curves", "red")
            .is_some_and(|curve| curve["channel"] == 1 && curve["sample_count"] == 257),
        "The selected red channel lacks its declared query samples",
    )?;
    let red_move = at("red-move")?;
    ensure(
        control_model(red_move, "curves", "red").is_some_and(|curve| {
            payload(red_move).is_some_and(|payload| curve["sample_source"] == payload["red"])
                && curve["sample_source_entry"] == red_move["state"]["stack"]["entry"]
                && curve["sample_asset"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty())
        }),
        "The sampled red curve is not correlated to its points, asset and committed entry",
    )?;
    ensure(
        payload(at("toggle")?).is_some_and(|p| p["enabled"] == true),
        "Toggle did not commit",
    )?;
    ensure(
        payload(at("choice")?).is_some_and(|p| p["mode"] == "two"),
        "Choice did not commit",
    )?;
    let groups = |step: &str| -> Result<Value> {
        Ok(at(step)?["state"]["control_ui"]["group_expanded"].clone())
    };
    ensure(
        groups("group-collapsed")?["luxforge.basic/2"] == false
            && groups("group-expanded")?["luxforge.basic/2"] == true
            && groups("group-expanded")?
                .get("luxforge.controls/0")
                .is_none(),
        "A group of a multi-group module did not collapse and expand",
    )?;
    let scrolled = at("pixel-scrolled")?;
    ensure(
        scrolled["state"]["tools_scroll"] == json!(1.0),
        "The pixel proof's section was not shown on its own at the panel's end",
    )?;
    ensure(
        scrolled["state"]["controls"]["set-pixel.x"].is_string()
            && scrolled["state"]["controls"]["set-pixel.y"].is_string(),
        "The pixel proof's X and Y fields are not in the captured state",
    )?;
    let events = &launch.events;
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
    write_json(
        &launch.evidence.join("controls-checks.json"),
        &json!(checks),
    )?;
    Ok(())
}
