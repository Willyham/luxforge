//! Rendered evidence for the opt-in control vocabulary and its identity photo layer.
//!
//! It enables the developer proof, scrolls its generated panel and drives slider, picker and curve
//! drafts, cancellation, point add and remove, discrete controls, group disclosure (on Basic's
//! Colour group, since the proof's controls are its module's only group and draw no header) and the
//! module reset. Its checks correlate history revisions and values with captures and verify that
//! the identity proof preserves the displayed photograph.
use crate::{
    scenario::{
        Checked, Checks, Frame, Plan, Run, Step, asked_no_frame, drawn_wheel, pixels, plan::only,
        selected_view,
    },
    *,
};
use luxforge_evidence::{
    self as script, ControlsStep, CurveStep, CurveStepEvent, GroupStep, PickerStep, PreviewStep,
    SliderEnd, TabStep,
};

const MODULE: &str = "luxforge.controls";
const ACTION: &str = "set-controls";
const EFFECT: &str = "luxforge.controls.identity";

/// The developer pixel proof, whose section the script shows last: X and Y as px fields.
const PIXEL_MODULE: &str = "luxforge.pixel";

/// The label the module reset earns. A commit of the proof's own patch earns its field's label,
/// as every field-patch module's does: the field's name and its value.
const RESET: &str = "Reset Controls";

/// The proof's wheel: its hue and saturation fields, the label a patch of both earns, and the
/// nested group whose tab row shows it compact and large.
const WHEEL_HUE: &str = "wheel-hue";
const WHEEL_SATURATION: &str = "wheel-saturation";
const WHEEL_TINT: &str = "Wheel tint";
const WHEEL_GROUP: [&str; 2] = ["Control vocabulary", "Colour wheel"];
/// Where the tools panel shows the wheel, between the proof's own fields and its curve.
const WHEEL_SCROLL: f64 = 0.45;
/// The entry the history step previews: the Original, which shows the wheel at its defaults.
const WHEEL_HISTORY: u64 = 0;

/// One wheel gesture through `positions`, Shift held when `shift`.
fn wheel(positions: Vec<[f32; 2]>, shift: bool, finish: SliderEnd) -> script::Step {
    script::Step::Controls(ControlsStep::Wheel {
        action: ACTION.into(),
        hue: WHEEL_HUE.into(),
        positions,
        shift,
        command: false,
        option: false,
        finish,
    })
}

/// The wheel group's `index`th view, selected as its tab row does.
fn tab(index: usize) -> script::Step {
    script::Step::Tab(TabStep {
        module: MODULE.into(),
        group: WHEEL_GROUP.map(String::from).into(),
        index,
    })
}

/// Every frame, in order: the open, then one per interaction. Opening, scrolling and drafting
/// create no history; one release or one discrete event makes exactly one entry.
pub fn plan(_: &[PathBuf]) -> Plan {
    let step = |name: &str, script: script::Step| Step::new(name, script);
    let commit = |name: &str, script: script::Step| step(name, script).commits(1);
    let set = |name: &str, label: &str, script: script::Step| commit(name, script).label(label);
    let view = |name: &str, script: script::Step| step(name, script).commits(0);
    Plan::new(vec![
        Step::opened("opened").no_layer(EFFECT),
        // Expose the proof section, then capture both its beginning and end in the tools panel.
        view(
            "basic-collapsed",
            script::Step::section("luxforge.basic", false),
        )
        .collapsed("luxforge.basic"),
        view(
            "crop-collapsed",
            script::Step::section("luxforge.crop", false),
        )
        .collapsed("luxforge.crop"),
        view("controls-expanded", script::Step::section(MODULE, true)).expanded(MODULE),
        view("scroll-half", script::Step::tools_scroll(0.5)),
        view(
            "picker-open",
            script::Step::Picker(PickerStep {
                action: ACTION.into(),
                parameter: "rgb".into(),
                open: Some(true),
                hue: None,
                plane: None,
                finish: SliderEnd::Open,
            }),
        ),
        view("scroll-end", script::Step::tools_scroll(1.0)),
        // Continuous values are drafts until the release. The proof module is pixel identity.
        view(
            "slider-drag",
            script::Step::Controls(ControlsStep::Slider {
                action: ACTION.into(),
                parameter: "amount".into(),
                fractions: vec![0.25, 0.75],
                finish: SliderEnd::Open,
            }),
        ),
        set(
            "slider-release",
            "Amount +2.50",
            script::Step::Controls(ControlsStep::Slider {
                action: ACTION.into(),
                parameter: "amount".into(),
                fractions: vec![0.75],
                finish: SliderEnd::Release,
            }),
        ),
        // A cancelled picker leaves both history and the photograph unchanged; the next commits.
        view(
            "picker-drag",
            script::Step::Picker(PickerStep {
                action: ACTION.into(),
                parameter: "rgb".into(),
                open: None,
                hue: Some(0.125),
                plane: Some([0.75, 0.625]),
                finish: SliderEnd::Open,
            }),
        ),
        view(
            "picker-cancel",
            script::Step::Picker(PickerStep {
                action: ACTION.into(),
                parameter: "rgb".into(),
                open: None,
                hue: Some(0.125),
                plane: Some([0.75, 0.625]),
                finish: SliderEnd::Cancel,
            }),
        )
        .no_draft(),
        commit(
            "picker-release",
            script::Step::Picker(PickerStep {
                action: ACTION.into(),
                parameter: "rgb".into(),
                open: None,
                hue: Some(0.875),
                plane: Some([0.75, 0.875]),
                finish: SliderEnd::Release,
            }),
        ),
        // Master point add is discrete; moving the point drafts and commits once.
        set(
            "curve-add",
            "Master 4 points",
            script::Step::Curve(CurveStep {
                action: ACTION.into(),
                parameter: "master".into(),
                event: CurveStepEvent::Add([0.25, 0.25]),
                finish: SliderEnd::Open,
            }),
        ),
        view(
            "curve-drag",
            script::Step::Curve(CurveStep {
                action: ACTION.into(),
                parameter: "master".into(),
                event: CurveStepEvent::Move {
                    index: 1,
                    points: vec![[0.375, 0.375]],
                },
                finish: SliderEnd::Open,
            }),
        ),
        set(
            "curve-release",
            "Master 4 points",
            script::Step::Curve(CurveStep {
                action: ACTION.into(),
                parameter: "master".into(),
                event: CurveStepEvent::Move {
                    index: 1,
                    points: vec![[0.375, 0.375]],
                },
                finish: SliderEnd::Release,
            }),
        ),
        view(
            "red-channel",
            script::Step::Curve(CurveStep {
                action: ACTION.into(),
                parameter: "master".into(),
                event: CurveStepEvent::Channel(1),
                finish: SliderEnd::Open,
            }),
        ),
        set(
            "red-move",
            "Red 3 points",
            script::Step::Curve(CurveStep {
                action: ACTION.into(),
                parameter: "red".into(),
                event: CurveStepEvent::Move {
                    index: 1,
                    points: vec![[0.5, 0.75]],
                },
                finish: SliderEnd::Release,
            }),
        ),
        set(
            "red-remove",
            "Red 2 points",
            script::Step::Curve(CurveStep {
                action: ACTION.into(),
                parameter: "red".into(),
                event: CurveStepEvent::Remove(1),
                finish: SliderEnd::Open,
            }),
        ),
        // Discrete controls commit exactly once each.
        set(
            "toggle",
            "Enabled on",
            script::Step::Controls(ControlsStep::Discrete {
                action: ACTION.into(),
                parameter: "enabled".into(),
                value: json!(true),
            }),
        ),
        set(
            "choice",
            "Mode Two",
            script::Step::Controls(ControlsStep::Discrete {
                action: ACTION.into(),
                parameter: "mode".into(),
                value: json!("two"),
            }),
        ),
        // The colour wheel: its nested Colour wheel group shows two tabs over the same three fields.
        // A drag drafts hue and saturation together through one draft and commits one entry.
        view("wheel-scrolled", script::Step::tools_scroll(WHEEL_SCROLL)),
        view(
            "wheel-drag",
            wheel(vec![[0.5, 0.0], [-0.25, -0.433]], false, SliderEnd::Open),
        )
        .draft(ACTION, json!({WHEEL_HUE: 120.0, WHEEL_SATURATION: 50.0})),
        set(
            "wheel-release",
            WHEEL_TINT,
            wheel(vec![[-0.25, -0.433]], false, SliderEnd::Release),
        )
        .field(ACTION, WHEEL_HUE, "120")
        .field(ACTION, WHEEL_SATURATION, "50"),
        // Through the centre: the angle is undefined there, so the hue it had is kept.
        set(
            "wheel-centre",
            WHEEL_TINT,
            wheel(vec![[0.0, 0.0]], false, SliderEnd::Release),
        )
        .field(ACTION, WHEEL_HUE, "120")
        .field(ACTION, WHEEL_SATURATION, "0"),
        // Escape restores the committed values and commits nothing.
        view(
            "wheel-cancel",
            wheel(vec![[0.0, 0.6], [0.0, 0.7]], false, SliderEnd::Cancel),
        )
        .no_draft()
        .field(ACTION, WHEEL_HUE, "120")
        .field(ACTION, WHEEL_SATURATION, "0"),
        // Shift keeps the hue while the saturation follows the pointer to 0.8 of the rim.
        set(
            "wheel-shift",
            WHEEL_TINT,
            wheel(vec![[0.8, 0.0]], true, SliderEnd::Release),
        )
        .field(ACTION, WHEEL_HUE, "120")
        .field(ACTION, WHEEL_SATURATION, "80"),
        // Another client's edit of the same fields moves the wheel.
        commit(
            "wheel-external",
            script::Step::agent(
                format!("edit.{ACTION}"),
                json!({WHEEL_HUE: 240.0, WHEEL_SATURATION: 30.0}),
            ),
        )
        .label(WHEEL_TINT),
        // The Large view: the same fields on a large wheel with their numbers. Selecting a view is
        // session state alone.
        view("view-large", tab(1)),
        // A historical entry shows its own values, disabled.
        view("wheel-history", PreviewStep::Sequence(WHEEL_HISTORY).into()),
        view("wheel-current", PreviewStep::Current.into()),
        view("view-compact", tab(0)),
        // The proof's controls are its module's only group, which the panel draws without a
        // header and cannot collapse; group disclosure is a group of a module with several.
        view(
            "group-collapsed",
            script::Step::Group(GroupStep {
                module: "luxforge.basic".into(),
                path: vec![2],
                expanded: false,
            }),
        ),
        view(
            "group-expanded",
            script::Step::Group(GroupStep {
                module: "luxforge.basic".into(),
                path: vec![2],
                expanded: true,
            }),
        ),
        step("reset", script::Step::reset(MODULE, None))
            .commits(1)
            .label(RESET)
            .payload(EFFECT, json!({})),
        // The pixel proof's section on its own: X and Y as labelled px fields, RGB, the picker
        // and Apply pixel.
        view("controls-collapsed", script::Step::section(MODULE, false)).collapsed(MODULE),
        view("pixel-expanded", script::Step::section(PIXEL_MODULE, true)).expanded(PIXEL_MODULE),
        view("pixel-scrolled", script::Step::tools_scroll(1.0))
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

pub(crate) fn sidebar_difference(first: &Frame, second: &Frame) -> Result<u32> {
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

    let mut checks = Checks::new();
    for (name, frame) in launch.names().iter().zip(&launch.frames) {
        let photo = pixels::identity_photo(frame)?;
        let image = frame.image()?;
        ensure(
            image.width() >= 1440 && image.height() >= 900,
            format!("Controls capture {name:?} is too small to inspect"),
        )?;
        checks.note(
            frame,
            "the proof's edit, and the photograph it leaves exactly as it was",
            json!({"step": name, "payload": payload(frame), "photo": photo}),
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
    let released = at("picker-release")?;
    let rgb = payload(released)
        .and_then(|p| p["rgb"].as_array())
        .ok_or("Released picker did not commit RGB")?;
    let shown: Vec<String> = rgb.iter().map(Value::to_string).collect();
    ensure(
        released.label()? == format!("Colour {}", shown.join(",")),
        format!(
            "The released picker's entry is labelled {:?}, not by its committed colour",
            released.label()?
        ),
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
    // The wheel: one draft of both fields while dragged, its model following the committed and
    // external values, and its views selected as session state with no history or frame.
    let wheel_model = |step: &str| -> Result<Value> {
        drawn_wheel(at(step)?.state(), WHEEL_HUE)
            .cloned()
            .ok_or_else(|| format!("The {step} frame reports no wheel").into())
    };
    ensure(
        wheel_model("wheel-drag")?["dragging"] == true,
        "The dragged wheel is not drawn as dragging",
    )?;
    ensure(
        wheel_model("wheel-centre")?["hue"] == json!(120.0)
            && wheel_model("wheel-centre")?["saturation"] == json!(0.0),
        "A drag through the centre did not keep the wheel's hue",
    )?;
    ensure(
        wheel_model("wheel-external")?["hue"] == json!(240.0)
            && wheel_model("wheel-external")?["saturation"] == json!(30.0),
        "Another client's edit did not move the wheel",
    )?;
    ensure(
        wheel_model("wheel-history")?["hue"] == json!(0.0)
            && wheel_model("wheel-current")?["hue"] == json!(240.0),
        "The previewed Original and the current entry do not show their own wheel values",
    )?;
    ensure(
        selected_view(at("view-large")?.state(), MODULE, &WHEEL_GROUP, "Large")
            && selected_view(at("view-compact")?.state(), MODULE, &WHEEL_GROUP, "Compact"),
        "The session's view selection does not record the Large and Compact views",
    )?;
    let visible = |step: &str| -> Result<Value> {
        Ok(at(step)?["state"]["control_ui"]["tab_rows"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["group"] == json!(WHEEL_GROUP)))
            .map(|row| row["visible"].clone())
            .unwrap_or(Value::Null))
    };
    ensure(
        visible("wheel-scrolled")? == "Compact"
            && visible("view-large")? == "Large"
            && visible("view-compact")? == "Compact",
        "The wheel group's tab row did not show the selected view",
    )?;
    // The two views draw the same fields differently: the large wheel and its numbers replace the
    // compact wheel in the tools panel.
    ensure(
        sidebar_difference(at("wheel-external")?, at("view-large")?)? >= 100,
        "The Large view did not change the tools panel",
    )?;
    ensure(
        sidebar_difference(at("wheel-current")?, at("view-compact")?)? >= 100,
        "The Compact view did not change the tools panel back",
    )?;
    for (step, before) in [
        ("view-large", "wheel-external"),
        ("view-compact", "wheel-current"),
    ] {
        ensure(
            asked_no_frame(at(before)?.state(), at(step)?.state()),
            format!("Selecting the view in {step} asked for a frame"),
        )?;
    }
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
        // A draft's frame is a preview job's, or a tick the GPU draws with none.
        events.iter().any(|event| {
            event["event"] == "slider_draft_preview" || event["event"] == "gpu_preview_tick"
        }) && events
            .iter()
            .any(|event| event["event"] == "slider_draft_commit")
            && events
                .iter()
                .any(|event| event["event"] == "preview_displayed"),
        "Control drafts or their displayed frames are not correlated in the log",
    )?;
    // Each view selection is logged with the identity it was keyed by.
    let selected: Vec<&Value> = events
        .iter()
        .filter(|event| event["event"] == "view_selected")
        .collect();
    ensure(
        selected.len() == 2
            && selected.iter().all(|event| {
                event["detail"]["module"] == MODULE
                    && event["detail"]["group"] == json!(WHEEL_GROUP)
            }),
        format!("Expected two logged view selections of the wheel group, found {selected:?}"),
    )?;
    checks.write(&launch.evidence, "controls", json!({}))
}
