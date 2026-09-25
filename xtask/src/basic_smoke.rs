//! The `basic` smoke scenario: the generated Exposure slider's whole gesture, on the real editor.
//!
//! It drives the same messages a pointer drag, a typed value, a reset and the Changed elsewhere
//! notice produce, and checks each captured frame against the draft, the revision, the history
//! label and the photograph's own brightness. Nothing here names a pixel the desktop chose: the
//! brightness check reads a centred window of the canvas, which is inside the fitted photograph at
//! every zoom this scenario uses and clear of the notices above it and the mode strip below it.
use crate::{
    scenario::{
        Checked, Fixture, Frame, Plan, Run, Step,
        pixels::{self, Tolerance, compare},
        plan::only,
    },
    *,
};
use luxforge_core::BASIC_EFFECT;

const BASIC_MODULE: &str = "luxforge.basic";
const SET_BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";
/// The group whose reset the scenario runs, as the Basic descriptor labels it.
const TONE_GROUP: &str = "Tone";

/// How far apart two means must be before this scenario calls one brighter than the other. The
/// measured steps are tens of codes wide, so this is a wide margin, not a threshold that tuning
/// could slip past.
const BRIGHTER: f64 = 10.0;
/// How close two means must be before this scenario calls them the same picture. Both frames are
/// the same render of the same stack, so this is JPEG-free readback noise only.
const SAME: f64 = 2.0;

/// Every frame, in order: the open, then one per step. Each step is one gesture, one request or
/// one decision; the expectations here are what it commits and records, and `verify` below checks
/// what the photograph shows.
pub fn plan(_: &[PathBuf]) -> Plan {
    let slider = |name: &str, values: Value, release: bool| {
        let mut slider = json!({"action":SET_BASIC,"parameter":EXPOSURE,"values":values});
        if release {
            slider["release"] = json!(true);
        }
        Step::new(name, json!({ "slider": slider }))
    };
    Plan::new(vec![
        // The fixture opens with the Basic section listed, its slider at the declared default and
        // no draft anywhere.
        Step::opened("opened")
            .field(SET_BASIC, EXPOSURE, "0.00")
            .no_draft()
            .no_layer(BASIC_EFFECT),
        // A drag to +1.00 EV, left open: the frame shows the drafted preview, nothing committed.
        slider("drag", json!([0.25, 0.5, 1.0]), false)
            .commits(0)
            .draft(SET_BASIC, json!({ EXPOSURE: 1.0 }))
            .no_layer(BASIC_EFFECT)
            .field(SET_BASIC, EXPOSURE, "1.00"),
        // The same gesture released: one entry, labelled by the module, one revision.
        slider("release", json!([1.0]), true)
            .no_draft()
            .commits(1)
            .label("Exposure +1.00 EV")
            .payload(BASIC_EFFECT, json!({ EXPOSURE: 1.0 })),
        // A second drag that returns to where it started: no entry at all.
        slider("return", json!([0.5, 1.0]), true)
            .no_draft()
            .commits(0),
        // The value field and Enter, which commits one field without a draft.
        Step::new(
            "typed",
            json!({"field":{"action":SET_BASIC,"parameter":EXPOSURE,"text":"-0.5","submit":true}}),
        )
        .no_draft()
        .commits(1)
        .label("Exposure -0.50 EV")
        .payload(BASIC_EFFECT, json!({ EXPOSURE: -0.5 })),
        // Undo: the slider re-seeds from the entry that is current again.
        Step::new("undo", json!({"api":{"method":"history.undo"}}))
            .commits(1)
            .field(SET_BASIC, EXPOSURE, "1.00"),
        // The Tone group's own reset button: the slider at 0, the layer kept with its neutral
        // payload.
        Step::new(
            "reset",
            json!({"reset":{"module":BASIC_MODULE,"group":TONE_GROUP}}),
        )
        .commits(1)
        .label("Reset Tone")
        .field(SET_BASIC, EXPOSURE, "0.00")
        .payload(BASIC_EFFECT, json!({})),
        // A drag left open, then somebody else commits under it: the draft is kept, marked
        // conflicted.
        Step::new(
            "drag-open",
            json!({"slider":{"action":SET_BASIC,"parameter":EXPOSURE,"values":[2.0]}}),
        )
        .commits(0)
        .draft(SET_BASIC, json!({ EXPOSURE: 2.0 })),
        Step::new(
            "conflict",
            json!({"api":{"method":"edit.transform","params":{"transform":"rotate-right"}}}),
        )
        .commits(1)
        .conflicted(SET_BASIC, json!({ EXPOSURE: 2.0 }))
        .field(SET_BASIC, EXPOSURE, "2.00"),
        // The notice's two decisions, in turn: Reapply rebases the draft and re-sends its value;
        // Discard ends the gesture with nothing committed and the authoritative value back.
        Step::new("reapply", json!({"slider_draft":"reapply"}))
            .commits(0)
            .draft(SET_BASIC, json!({ EXPOSURE: 2.0 })),
        Step::new("discard", json!({"slider_draft":"discard"}))
            .no_draft()
            .commits(0)
            .field(SET_BASIC, EXPOSURE, "0.00"),
    ])
}

/// The one Basic layer's stored payload, or `None` when the stack holds no Basic layer.
fn basic_payload(frame: &Frame) -> Option<&Value> {
    frame.payload(BASIC_EFFECT)
}

/// Whether the module registry lists `module` and offers it.
fn available(frame: &Frame, module: &str) -> Result {
    let listed = frame["state"]["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|listed| listed["id"] == json!(module))
        .ok_or_else(|| format!("The {module} module is not listed at all"))?;
    ensure(
        listed["available"] == json!(true),
        format!("The {module} module is not available"),
    )
}

/// What the photograph shows at each step, once the plan has held.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let luminance: Vec<f64> = launch
        .frames
        .iter()
        .map(pixels::window_luminance)
        .collect::<Result<Vec<_>>>()?;
    let lum = |step: &str| -> Result<f64> { Ok(luminance[launch.index(step)?]) };
    let mut checks = Vec::new();
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };

    let opened = launch.at("opened")?;
    available(opened, BASIC_MODULE)?;
    record(
        opened,
        "the default panel with the Basic section, Exposure at 0",
        json!({
            "pixels": opened.fixture(Fixture::fit(1))?,
            "mean_luminance": lum("opened")?,
        }),
    );

    // Mid-gesture at +1.00 EV: the photograph on screen is the drafted render, which is brighter.
    let drag = launch.at("drag")?;
    let drafted = drag.draft();
    ensure(
        drafted["draft_revision"]
            .as_u64()
            .is_some_and(|value| value >= 1),
        format!("The drag's draft carries no draft revision: {drafted}"),
    )?;
    ensure(
        drag["state"]["displayed_draft_revision"] == drafted["draft_revision"],
        format!(
            "The drag displays draft revision {} while the draft is at {}",
            drag["state"]["displayed_draft_revision"], drafted["draft_revision"]
        ),
    )?;
    compare(
        "+1.00 EV drafted against the neutral open",
        lum("drag")?,
        lum("opened")?,
        Tolerance::Above(BRIGHTER),
    )?;
    record(
        drag,
        "a drag to +1.00 EV, mid-gesture: the drafted preview, nothing committed",
        json!({"draft": drafted, "mean_luminance": lum("drag")?}),
    );

    let release = launch.at("release")?;
    compare(
        "the committed render against the drafted one",
        lum("release")?,
        lum("drag")?,
        Tolerance::Within(SAME),
    )?;
    record(
        release,
        "released: one entry \"Exposure +1.00 EV\", the revision advanced by one",
        json!({"revision": release.revision()?, "label": release.label()?, "mean_luminance": lum("release")?}),
    );

    let returned = launch.at("return")?;
    compare(
        "the return-to-start render",
        lum("return")?,
        lum("release")?,
        Tolerance::Within(SAME),
    )?;
    record(
        returned,
        "a drag back to +1.00 EV and released: no entry, no revision",
        json!({"revision": returned.revision()?, "entry": returned.entry()?}),
    );

    // -0.50 EV typed: the photograph is darker than it was at neutral.
    let typed = launch.at("typed")?;
    compare(
        "the neutral open against -0.50 EV",
        lum("opened")?,
        lum("typed")?,
        Tolerance::Above(BRIGHTER),
    )?;
    record(
        typed,
        "-0.50 EV typed and submitted with Enter: one entry",
        json!({"label": typed.label()?, "mean_luminance": lum("typed")?}),
    );

    // Undo: the current entry is the +1.00 one again, and the pixels follow.
    let undo = launch.at("undo")?;
    ensure(
        undo.entry()? == release.entry()?,
        "Undo did not return to the +1.00 EV entry",
    )?;
    compare(
        "the undone +1.00 EV against -0.50 EV",
        lum("undo")?,
        lum("typed")?,
        Tolerance::Above(BRIGHTER),
    )?;
    compare(
        "undo against the committed +1.00 EV render",
        lum("undo")?,
        lum("release")?,
        Tolerance::Within(SAME),
    )?;
    record(
        undo,
        "history.undo: the values re-seed to +1.00 and the pixels follow",
        json!({"entry": undo.entry()?, "exposure": undo.field(SET_BASIC, EXPOSURE)?, "mean_luminance": lum("undo")?}),
    );

    // The Tone group's reset: the photograph back to the opened one.
    let reset = launch.at("reset")?;
    compare(
        "the reset render against the opened one",
        lum("reset")?,
        lum("opened")?,
        Tolerance::Within(SAME),
    )?;
    record(
        reset,
        "the Tone group reset: entry \"Reset Tone\", the slider at 0, the layer kept and neutral",
        json!({"label": reset.label()?, "payload": basic_payload(reset), "mean_luminance": lum("reset")?}),
    );

    let drag_open = launch.at("drag-open")?;
    compare(
        "+2.00 EV drafted against neutral",
        lum("drag-open")?,
        lum("reset")?,
        Tolerance::Above(BRIGHTER),
    )?;
    record(
        drag_open,
        "a drag to +2.00 EV, left open",
        json!({"draft": drag_open.draft(), "mean_luminance": lum("drag-open")?}),
    );

    // A commit by another route while that gesture is open: the Changed elsewhere notice offers the
    // two decisions.
    let conflict = launch.at("conflict")?;
    let conflict_notices = conflict.notices();
    ensure(
        conflict_notices
            .iter()
            .any(|notice| notice == "Changed elsewhere"),
        format!("The conflict frame's notices do not include it: {conflict_notices:?}"),
    )?;
    record(
        conflict,
        "a commit during the gesture: Changed elsewhere, the draft kept and conflicted",
        json!({"notices": conflict_notices, "draft": conflict.draft(), "revision": conflict.revision()?}),
    );

    // Reapply: the draft is rebased on the new revision and the drafted preview returns over the
    // committed stack.
    let reapply = launch.at("reapply")?;
    let reapplied = reapply.draft();
    ensure(
        reapplied["base_revision"].as_u64() == Some(reapply.revision()?),
        format!(
            "The reapplied draft is based on {} while the asset is at {}",
            reapplied["base_revision"],
            reapply.revision()?
        ),
    )?;
    ensure(
        reapply.notices().is_empty(),
        format!("Reapply still shows a notice: {:?}", reapply.notices()),
    )?;
    compare(
        "the reapplied +2.00 EV against the committed stack",
        lum("reapply")?,
        lum("conflict")?,
        Tolerance::Above(BRIGHTER),
    )?;
    record(
        reapply,
        "Reapply: the draft rebases, its value is re-sent and the drafted preview returns",
        json!({"draft": reapplied, "mean_luminance": lum("reapply")?}),
    );

    // Discard: the canvas is the committed stack again.
    let discard = launch.at("discard")?;
    compare(
        "the discarded gesture against the committed stack",
        lum("discard")?,
        lum("conflict")?,
        Tolerance::Within(SAME),
    )?;
    record(
        discard,
        "Discard: the gesture ends, nothing is committed and the committed pixels return",
        json!({"revision": discard.revision()?, "mean_luminance": lum("discard")?}),
    );

    // Every drafted and committed frame the gesture presented reports its own render time, and the
    // status bar in each captured frame states one of them, not the time since the open.
    let render_times = crate::smoke::expect_render_times(&launch.events, &launch.frames)?;

    write_json(
        &launch.evidence.join("basic-checks.json"),
        &json!({
            "checks": checks,
            "render_times": render_times,
            "mean_luminance_per_frame": luminance,
            "brighter_margin": BRIGHTER,
            "same_tolerance": SAME,
            "scope": "Mean Rec. 709 luminance of a centred window of the photo surface, read back from the renderer; not a colorimetric claim",
        }),
    )?;
    Ok(())
}

// -------------------------------------------------------------------------------------------
// The `basic-restart` scenario: a Basic edit survives a restart of the real editor.
// -------------------------------------------------------------------------------------------

/// What the first launch commits: two fields in one patch, so the second launch proves both the
/// stored payload and the panel's re-seeding rather than a single slider.
const RESTART_EXPOSURE: f64 = 1.5;
const RESTART_TEMPERATURE: f64 = 25.0;
/// The entry the module labels a patch of those two fields with.
const RESTART_LABEL: &str = "Basic (2 fields)";

pub const RESTART_NOTE: &str = "Two launches, because a restart cannot be simulated inside one process: the first opens the fixture and commits one `edit.set-basic` patch of exposure and temperature; the second reuses that launch's own catalog (`--catalog <dir1>/catalog.sqlite`) and reopens the same file, which the catalog dedupes to the same asset, so the saved Basic layer, its identity, the slider values, the history label and the rendered brightness all come back.";

/// The first launch: the fixture opened, then one Basic patch committed through the ordinary edit
/// path.
pub fn restart_first(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_layer(BASIC_EFFECT),
        Step::new(
            "committed",
            json!({"api":{"method":"edit.set-basic","params":{"exposure":RESTART_EXPOSURE,"temperature":RESTART_TEMPERATURE}}}),
        )
        .commits(1)
        .label(RESTART_LABEL)
        .payload(
            BASIC_EFFECT,
            json!({ EXPOSURE: RESTART_EXPOSURE, TEMPERATURE: RESTART_TEMPERATURE }),
        ),
    ])
}

/// The second launch: the same catalog and the same file in a new process, which the catalog
/// dedupes to the same asset, so its saved stack comes back and the sliders re-seed from it.
pub fn restart_second(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("reopened")
            .label(RESTART_LABEL)
            .payload(
                BASIC_EFFECT,
                json!({ EXPOSURE: RESTART_EXPOSURE, TEMPERATURE: RESTART_TEMPERATURE }),
            )
            .field(SET_BASIC, EXPOSURE, "1.50")
            .field(SET_BASIC, TEMPERATURE, "25"),
    ])
}

/// The two launches checked together: the reopened asset is the committed one, at the same entry,
/// with the same layer and label, and the same brighter photograph.
pub fn verify_restart(run: &mut Run, launches: &[Checked]) -> Result {
    let [first, second] = launches else {
        return Err(format!("Expected two launches, found {}", launches.len()).into());
    };
    let opened = first.at("opened")?;
    let committed = first.at("committed")?;
    let reopened = second.at("reopened")?;
    ensure(
        committed.revision()? == 1,
        format!("Launch 1 committed revision {}", committed.revision()?),
    )?;
    let stored = basic_payload(committed)
        .ok_or("Launch 1 committed no Basic layer")?
        .clone();
    let layer = basic_layer_id(committed)
        .ok_or("Launch 1's Basic layer has no identity")?
        .to_owned();
    let neutral_luminance = pixels::window_luminance(opened)?;
    let edited_luminance = pixels::window_luminance(committed)?;
    compare(
        "launch 1's committed edit against its own neutral open",
        edited_luminance,
        neutral_luminance,
        Tolerance::Above(BRIGHTER),
    )?;
    ensure(
        reopened.revision()? == committed.revision()? && reopened.entry()? == committed.entry()?,
        format!(
            "Launch 2 reopened at revision {} entry {}",
            reopened.revision()?,
            reopened.entry()?
        ),
    )?;
    ensure(
        basic_layer_id(reopened) == Some(layer.as_str()),
        "The Basic layer's identity did not survive the restart",
    )?;
    ensure(
        reopened.label()? == committed.label()?,
        format!(
            "Launch 2's history label is {:?}, launch 1 committed {:?}",
            reopened.label()?,
            committed.label()?
        ),
    )?;
    // The photograph itself is the edited one again, to the same measured brightness.
    let reopened_luminance = pixels::window_luminance(reopened)?;
    compare(
        "the reopened render against the render launch 1 committed",
        reopened_luminance,
        edited_luminance,
        Tolerance::Within(SAME),
    )?;
    compare(
        "the reopened render against a neutral open",
        reopened_luminance,
        neutral_luminance,
        Tolerance::Above(BRIGHTER),
    )?;
    write_json(
        &run.out().join("basic-restart-checks.json"),
        &json!({
            "stored_payload": stored,
            "basic_layer": layer,
            "label": reopened.label()?,
            "fields_after_restart": {
                EXPOSURE: basic_field(reopened, EXPOSURE)?,
                TEMPERATURE: basic_field(reopened, TEMPERATURE)?,
            },
            "mean_luminance": {
                "launch1_neutral_open": neutral_luminance,
                "launch1_committed": edited_luminance,
                "launch2_reopened": reopened_luminance,
            },
            "brighter_margin": BRIGHTER,
            "same_tolerance": SAME,
            "scope": "Mean Rec. 709 luminance of a centred window of the photo surface, read back from the renderer; not a colorimetric claim",
        }),
    )?;
    Ok(())
}

// -------------------------------------------------------------------------------------------
// The `basic-panel` scenario: the whole Basic section, historical values and the neutral picker.
// -------------------------------------------------------------------------------------------

/// Every field the Basic descriptor implements, in the order the panel lists them. The scenario
/// checks that each one is on screen with a value, which is what "no control is clipped" means in
/// the recorded state; the frames themselves are inspected for layout.
const BASIC_FIELDS: [&str; 10] = [
    "temperature",
    "tint",
    "exposure",
    "contrast",
    "highlights",
    "shadows",
    "whites",
    "blacks",
    "vibrance",
    "saturation",
];

const TEMPERATURE: &str = "temperature";
const TINT: &str = "tint";
const VIBRANCE: &str = "vibrance";
/// The Colour group, as the Basic descriptor labels it.
const COLOUR_GROUP: &str = "Colour";
/// The White balance group's label, which the module also uses for the history entry a patch
/// returning exactly that group to neutral earns.
const WHITE_BALANCE_GROUP: &str = "White balance";

/// The neutral grey patch the picker samples: `fixtures/s0/greyscale.jpg` is uniform 91/91/91 over
/// the whole 5x5 patch here, so the picker's answer is the exact identity, 0 and 0.
const NEUTRAL_PICK: [u32; 2] = [120, 80];
/// The fixture's white cross, whose 5x5 patch holds code 255: the any-channel clipping rule
/// refuses it, and nothing is committed.
const CLIPPED_PICK: [u32; 2] = [240, 160];

/// How far apart two mean red-minus-blue readings must be before this scenario calls one warmer
/// than the other. A neutral grey fixture reads zero, and +40 temperature moves it tens of codes.
const WARMER: f64 = 8.0;
/// How close two mean red-minus-blue readings must be before this scenario calls them the same
/// balance. Both are renderer readback of a neutral grey, so this is readback noise only.
const NEUTRAL: f64 = 1.5;

/// Where the photograph is drawn in a captured frame, for a fixture with no coloured quadrants to
/// match on: the bright pixels of the band between the notices at the top of the canvas and the
/// floating mode strip at its bottom, which `verify_panel` checks no frame draws a notice into. The
/// threshold is well above the brightest chrome in the band and well below the fixture's darkest
/// grey. The photograph must be the fixture's 3:2, centred, and fill the surface at Fit.
fn placement(frame: &Frame) -> Result<Value> {
    const BRIGHT: u32 = 60;
    let [left, top, right, bottom] = pixels::band_bounds(frame, BRIGHT)?;
    let (width, height) = frame.image()?.dimensions();
    let [surface_left, surface_right] = frame.columns()?.unwrap_or([0, width]);
    let (drawn_width, drawn_height) = (right - left, bottom - top);
    let measured = f64::from(drawn_width) / f64::from(drawn_height);
    let expected = 3.0 / 2.0;
    ensure(
        (measured - expected).abs() < 0.015,
        format!("Displayed aspect ratio {measured:.4}, expected {expected:.4}"),
    )?;
    ensure(
        (f64::from(left + right) / 2.0 - f64::from(surface_left + surface_right) / 2.0).abs()
            <= 5.0,
        "The photograph is not centred in the photo surface",
    )?;
    ensure(
        f64::from(drawn_width) > f64::from(surface_right - surface_left) * 0.5,
        "The photograph does not fill the surface at Fit",
    )?;
    Ok(json!({
        "status": "passed",
        "physical_size": [width, height],
        "surface_columns": [surface_left, surface_right],
        "image_bounds": [left, top, right, bottom],
        "measured_aspect": measured,
        "expected_aspect": expected,
        "bright_threshold": BRIGHT,
        "scope": "Displayed placement of a greyscale fixture, read back from the renderer; not monitor calibration",
    }))
}

/// Mean red minus mean blue: the honest measure of a warm/cool shift on a neutral fixture, where
/// luminance barely moves because the white-balance transform preserves it by construction.
fn balance(channels: [f64; 3]) -> f64 {
    channels[0] - channels[2]
}

/// Zero as one Basic field shows it: the parameter's own declared precision, read from the
/// registry rather than written down here, so a field that changes its precision changes this too.
fn neutral_text(name: &str) -> Result<String> {
    let registry = luxforge_core::ModuleRegistry::builtin();
    let decimals = usize::from(
        registry
            .descriptors()
            .into_iter()
            .find(|module| module.id == BASIC_MODULE)
            .and_then(|module| module.action(SET_BASIC))
            .and_then(|action| action.parameter(name))
            .and_then(|parameter| parameter.precision)
            .ok_or_else(|| format!("{name} declares no display precision"))?,
    );
    Ok(format!("{:.decimals$}", 0.0))
}

/// What one generated Basic field showed when the frame was captured.
fn basic_field<'a>(frame: &'a Frame, name: &str) -> Result<&'a str> {
    frame.field(SET_BASIC, name)
}

/// The one Basic layer's identity, so evidence can prove an edit updated it in place.
fn basic_layer_id(frame: &Frame) -> Option<&str> {
    frame.layer_id(BASIC_EFFECT)
}

/// Every frame of `basic-panel`, in order: the open, then one per step.
pub fn panel_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The whole Basic section, expanded, with nothing committed yet.
        Step::opened("opened")
            .expanded(BASIC_MODULE)
            .no_layer(BASIC_EFFECT),
        // The White balance group: a drag to +40 temperature, released. One entry, one revision.
        Step::new(
            "temperature",
            json!({"slider":{"action":SET_BASIC,"parameter":TEMPERATURE,"values":[10.0,25.0,40.0],"release":true}}),
        )
        .no_draft()
        .commits(1)
        .label("Temperature +40")
        .payload(BASIC_EFFECT, json!({ TEMPERATURE: 40.0 }))
        .field(SET_BASIC, TEMPERATURE, "40"),
        // The Colour group: a typed value committed with Enter, merged into the same layer.
        Step::new(
            "vibrance",
            json!({"field":{"action":SET_BASIC,"parameter":VIBRANCE,"text":"25","submit":true}}),
        )
        .commits(1)
        .label("Vibrance +25")
        .payload(BASIC_EFFECT, json!({ TEMPERATURE: 40.0, VIBRANCE: 25.0 }))
        .same_layer(BASIC_EFFECT, "temperature"),
        // The Temperature entry, previewed: its own saved values fill the disabled sliders, which
        // are not the current ones, and nothing is committed.
        Step::new("preview", json!({"preview":{"sequence":1}}))
            .commits(0)
            .field(SET_BASIC, TEMPERATURE, "40")
            .field(SET_BASIC, VIBRANCE, "0"),
        // Return to current: the current entry's values come back.
        Step::new("current", json!({"preview":"current"}))
            .commits(0)
            .field(SET_BASIC, VIBRANCE, "25"),
        // The Colour group's own reset button: that group neutral, every other field untouched and
        // the layer kept.
        Step::new(
            "colour-reset",
            json!({"reset":{"module":BASIC_MODULE,"group":COLOUR_GROUP}}),
        )
        .commits(1)
        .label(format!("Reset {COLOUR_GROUP}"))
        .payload(BASIC_EFFECT, json!({ TEMPERATURE: 40.0 }))
        .same_layer(BASIC_EFFECT, "temperature")
        .field(SET_BASIC, VIBRANCE, "0")
        .field(SET_BASIC, TEMPERATURE, "40"),
        // The neutral picker's canvas mode, which `W` also selects.
        Step::new("picker-mode", json!({"workspace":{"mode":BASIC_MODULE}})).commits(0),
        // A pick on a neutral grey patch: the picker answers 0 and 0 and commits that, once,
        // through the ordinary action path. The module labels a patch that returns exactly one
        // group to neutral by that group's name, whatever route sent it.
        Step::new(
            "neutral-pick",
            json!({"pick":{"x":NEUTRAL_PICK[0],"y":NEUTRAL_PICK[1]}}),
        )
        .commits(1)
        .field(SET_BASIC, TEMPERATURE, "0")
        .field(SET_BASIC, TINT, "0")
        .payload(BASIC_EFFECT, json!({}))
        .same_layer(BASIC_EFFECT, "temperature")
        .label(format!("Reset {WHITE_BALANCE_GROUP}")),
        // A pick on a clipped patch: refused with its reason in the status bar, nothing committed.
        Step::new(
            "clipped-pick",
            json!({"pick":{"x":CLIPPED_PICK[0],"y":CLIPPED_PICK[1]}}),
        )
        .commits(0),
    ])
}

pub fn verify_panel(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let channels: Vec<[f64; 3]> = launch
        .frames
        .iter()
        .map(pixels::window_rgb)
        .collect::<Result<Vec<_>>>()?;
    let luminance: Vec<f64> = launch
        .frames
        .iter()
        .map(pixels::window_luminance)
        .collect::<Result<Vec<_>>>()?;
    let rgb = |step: &str| -> Result<[f64; 3]> { Ok(channels[launch.index(step)?]) };
    let lum = |step: &str| -> Result<f64> { Ok(luminance[launch.index(step)?]) };
    let mut checks = Vec::new();
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };

    // The whole Basic section: every implemented field is on screen at its declared default.
    let opened = launch.at("opened")?;
    let mut listed = Vec::new();
    for name in BASIC_FIELDS {
        listed.push(json!({ name: basic_field(opened, name)? }));
        ensure(
            basic_field(opened, name)? == neutral_text(name)?,
            format!(
                "{name} does not start at its declared default: {:?}",
                basic_field(opened, name)?
            ),
        )?;
    }
    ensure(
        balance(rgb("opened")?).abs() <= NEUTRAL,
        format!(
            "The greyscale fixture does not read neutral: {:.1}",
            balance(rgb("opened")?)
        ),
    )?;
    // No frame in this scenario draws a notice, which is what lets `placement` read the band
    // between the notices and the mode strip as photograph and canvas surface only.
    for (name, frame) in launch.names().iter().zip(&launch.frames) {
        ensure(
            frame["state"]["notices"] == json!([]),
            format!(
                "Step {name:?} shows a notice: {}",
                frame["state"]["notices"]
            ),
        )?;
    }
    record(
        opened,
        "the Basic section with White balance, Tone and Colour, every field at its default",
        json!({
            "placement": placement(opened)?,
            "fields": listed,
            "red_minus_blue": balance(rgb("opened")?),
        }),
    );

    // +40 temperature: the neutral fixture is visibly warmer.
    let temperature = launch.at("temperature")?;
    compare(
        "+40 temperature against the neutral open, red minus blue",
        balance(rgb("temperature")?),
        balance(rgb("opened")?),
        Tolerance::Above(WARMER),
    )?;
    record(
        temperature,
        "Temperature dragged to +40 and released: one entry, the photograph warmer",
        json!({"label": temperature.label()?, "red_minus_blue": balance(rgb("temperature")?), "layer": basic_layer_id(temperature)}),
    );

    let vibrance = launch.at("vibrance")?;
    record(
        vibrance,
        "Vibrance typed as 25 and committed with Enter: the same layer, both fields",
        json!({"label": vibrance.label()?, "payload": basic_payload(vibrance)}),
    );

    let preview = launch.at("preview")?;
    ensure(
        preview.status()?.starts_with("Previewing entry 1"),
        format!(
            "The preview step is not previewing entry 1: {:?}",
            preview.status()?
        ),
    )?;
    record(
        preview,
        "the Temperature entry previewed: its own saved values in the disabled sliders",
        json!({"status": preview.status()?, "temperature": basic_field(preview, TEMPERATURE)?, "vibrance": basic_field(preview, VIBRANCE)?}),
    );

    let current = launch.at("current")?;
    record(
        current,
        "Return to current: the current entry's values are shown again",
        json!({"vibrance": basic_field(current, VIBRANCE)?, "status": current.status()?}),
    );

    let reset = launch.at("colour-reset")?;
    record(
        reset,
        "the Colour group reset: one entry, that group neutral, White balance untouched",
        json!({"label": reset.label()?, "payload": basic_payload(reset), "layer": basic_layer_id(reset)}),
    );

    // The neutral picker's canvas mode. The picker lives in the White balance group, beside the
    // two fields a pick sets, and reads selected exactly while its mode is active. The mode strip
    // holds no entry for it at all.
    let mode = launch.at("picker-mode")?;
    ensure(
        mode["state"]["workspace"]["mode"] == json!(BASIC_MODULE),
        format!("The canvas mode is {}", mode["state"]["workspace"]["mode"]),
    )?;
    ensure(
        mode.status()?.starts_with("Neutral picker"),
        format!("The picker mode says {:?}", mode.status()?),
    )?;
    let picker = |frame: &Value| frame["state"]["pickers"][BASIC_MODULE].clone();
    ensure(
        picker(opened)["label"] == json!("Neutral picker")
            && picker(opened)["shortcut"] == json!("W"),
        format!("The Basic panel declares no picker: {}", picker(opened)),
    )?;
    ensure(
        picker(opened)["selected"] == json!(false),
        "The picker reads selected before its mode was entered",
    )?;
    ensure(
        picker(mode)["selected"] == json!(true),
        format!(
            "The picker does not read selected in its own mode: {}",
            picker(mode)
        ),
    )?;
    record(
        mode,
        "the neutral picker mode, entered through workspace.set as W and the panel's own picker \
         button do; that button reads selected inside the White balance group",
        json!({
            "mode": mode["state"]["workspace"]["mode"],
            "status": mode.status()?,
            "picker": picker(mode),
        }),
    );

    // The pick on a neutral grey patch took the warm cast the drag left away: the photograph is the
    // opened one again, and the mode is kept.
    let pick = launch.at("neutral-pick")?;
    compare(
        "the picked correction against the opened photograph, red minus blue",
        balance(rgb("neutral-pick")?),
        balance(rgb("opened")?),
        Tolerance::Within(NEUTRAL),
    )?;
    ensure(
        pick["state"]["workspace"]["mode"] == json!(BASIC_MODULE),
        "The pick left the picker mode",
    )?;
    record(
        pick,
        "a pick on a neutral grey patch: temperature and tint 0, committed once, the mode kept",
        json!({"label": pick.label()?, "payload": basic_payload(pick), "red_minus_blue": balance(rgb("neutral-pick")?), "mean_luminance": lum("neutral-pick")?}),
    );

    // A pick on a clipped patch: the core's own reason leads the status bar.
    let clipped = launch.at("clipped-pick")?;
    ensure(
        clipped.status()?.starts_with("clipped:"),
        format!(
            "The refused pick does not lead with its reason: {:?}",
            clipped.status()?
        ),
    )?;
    compare(
        "the refused pick's render against the picked one",
        lum("clipped-pick")?,
        lum("neutral-pick")?,
        Tolerance::Within(SAME),
    )?;
    record(
        clipped,
        "a pick on a clipped patch: refused with its reason, nothing committed",
        json!({"status": clipped.status()?, "revision": clipped.revision()?}),
    );

    // The opened frame is also the default screen the Module panels density is accepted on: Basic
    // expanded and every other section collapsed to its band by its own descriptor, so the
    // histogram, Basic and every other section's band are on screen at once.
    let expanded = &opened["state"]["expanded"];
    let others_collapsed = expanded
        .as_object()
        .ok_or("Missing expanded sections")?
        .iter()
        .all(|(module, open)| (module == BASIC_MODULE) == (open == &json!(true)));
    ensure(
        others_collapsed,
        format!("Only Basic should be expanded on the opened screen: {expanded}"),
    )?;
    record(
        opened,
        "Basic expanded and every other section collapsed to its band by default",
        expanded.clone(),
    );

    write_json(
        &launch.evidence.join("basic-panel-checks.json"),
        &json!({
            "checks": checks,
            "red_minus_blue_per_frame": channels.iter().map(|channels| balance(*channels)).collect::<Vec<_>>(),
            "mean_luminance_per_frame": luminance,
            "warmer_margin": WARMER,
            "neutral_tolerance": NEUTRAL,
            "scope": "Mean per-channel readback of a centred window of the photo surface; a warm/cool direction, not a colorimetric claim",
        }),
    )?;
    Ok(())
}
