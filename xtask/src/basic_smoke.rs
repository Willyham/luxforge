//! The `basic` smoke scenario: the generated Exposure slider's whole gesture, on the real editor.
//!
//! It drives the same messages a pointer drag, a typed value, a reset and the Changed elsewhere
//! notice produce, and checks each captured frame against the draft, the revision, the history
//! label, the notices and the photograph's own brightness, read over the central window of the
//! photograph the editor records drawing.
use crate::{
    scenario::{Checked, Checks, Fixture, Frame, Plan, Run, Step, Tolerance, plan::only},
    *,
};
use luxforge_core::{BASIC_EFFECT, POINTER_MODE};
use luxforge_evidence::{
    self as script, PaletteStep, PreviewStep, SliderDraftStep, SliderStep, WorkspaceStep,
};

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
    let slider = |name: &str, values: &[f64], release: bool| {
        let slider = SliderStep::new(SET_BASIC, EXPOSURE, values);
        Step::new(name, if release { slider.release() } else { slider })
    };
    Plan::new(vec![
        // The fixture opens with the Basic section listed, its slider at the declared default and
        // no draft anywhere.
        Step::opened("opened")
            .field(SET_BASIC, EXPOSURE, "0.00")
            .no_draft()
            .no_layer(BASIC_EFFECT),
        // A drag to +1.00 EV, left open: the frame shows the drafted preview, nothing committed.
        slider("drag", &[0.25, 0.5, 1.0], false)
            .commits(0)
            .draft(SET_BASIC, json!({ EXPOSURE: 1.0 }))
            .no_layer(BASIC_EFFECT)
            .field(SET_BASIC, EXPOSURE, "1.00"),
        // The same gesture released: one entry, labelled by the module, one revision.
        slider("release", &[1.0], true)
            .no_draft()
            .commits(1)
            .label("Exposure +1.00 EV")
            .payload(BASIC_EFFECT, json!({ EXPOSURE: 1.0 })),
        // A second drag that returns to where it started: no entry at all.
        slider("return", &[0.5, 1.0], true).no_draft().commits(0),
        // The value field and Enter, which commits one field without a draft.
        Step::new(
            "typed",
            script::Step::field(SET_BASIC, EXPOSURE, "-0.5", true),
        )
        .no_draft()
        .commits(1)
        .label("Exposure -0.50 EV")
        .payload(BASIC_EFFECT, json!({ EXPOSURE: -0.5 })),
        // Undo: the slider re-seeds from the entry that is current again.
        Step::new("undo", script::Step::api("history.undo"))
            .commits(1)
            .field(SET_BASIC, EXPOSURE, "1.00"),
        // The Tone group's own reset button: the slider at 0, the layer kept with its neutral
        // payload.
        Step::new("reset", script::Step::reset(BASIC_MODULE, Some(TONE_GROUP)))
            .commits(1)
            .label("Reset Tone")
            .field(SET_BASIC, EXPOSURE, "0.00")
            .payload(BASIC_EFFECT, json!({})),
        // A drag left open, then an agent commits under it through a client of its own: the
        // desktop's event sync reads the change back, and the draft is kept, marked conflicted.
        Step::new("drag-open", SliderStep::new(SET_BASIC, EXPOSURE, [2.0]))
            .commits(0)
            .draft(SET_BASIC, json!({ EXPOSURE: 2.0 })),
        Step::new(
            "conflict",
            script::Step::agent("edit.transform", json!({"transform":"rotate-right"})),
        )
        .commits(1)
        .conflicted(SET_BASIC, json!({ EXPOSURE: 2.0 }))
        .field(SET_BASIC, EXPOSURE, "2.00")
        .notice("Changed elsewhere"),
        // The notice's two decisions, in turn: Reapply rebases the draft and re-sends its value;
        // Discard ends the gesture with nothing committed and the authoritative value back.
        Step::new("reapply", SliderDraftStep::Reapply)
            .commits(0)
            .draft(SET_BASIC, json!({ EXPOSURE: 2.0 }))
            .no_notices(),
        Step::new("discard", SliderDraftStep::Discard)
            .no_draft()
            .commits(0)
            .field(SET_BASIC, EXPOSURE, "0.00"),
    ])
}

/// What the photograph shows at each step, once the plan has held.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let lum = |step: &str| -> Result<f64> { launch.at(step)?.window_luminance() };
    let mut checks = Checks::new();
    // Each claim: the named frame's reading against another's, under a tolerance.
    let mut claim = |step: &str, what: &str, a: &str, b: &str, within: Tolerance| -> Result {
        checks.compare(launch.at(step)?, what, lum(a)?, lum(b)?, within)
    };
    let (brighter, same) = (Tolerance::Above(BRIGHTER), Tolerance::Within(SAME));

    let opened = launch.at("opened")?;
    opened.module_available(BASIC_MODULE)?;
    opened.fixture(Fixture::fit(1))?;

    // Mid-gesture at +1.00 EV: the photograph on screen is the drafted render, which is brighter;
    // the release commits that render, and a drag back to where it started commits nothing.
    launch.at("drag")?.displays_draft()?;
    claim(
        "drag",
        "+1.00 EV drafted against the neutral open",
        "drag",
        "opened",
        brighter,
    )?;
    claim(
        "release",
        "the committed render against the drafted one",
        "release",
        "drag",
        same,
    )?;
    claim(
        "return",
        "the return-to-start render",
        "return",
        "release",
        same,
    )?;

    // -0.50 EV typed: the photograph is darker than it was at neutral.
    claim(
        "typed",
        "the neutral open against -0.50 EV",
        "opened",
        "typed",
        brighter,
    )?;

    // Undo: the current entry is the +1.00 one again, and the pixels follow.
    ensure(
        launch.at("undo")?.entry()? == launch.at("release")?.entry()?,
        "Undo did not return to the +1.00 EV entry",
    )?;
    claim(
        "undo",
        "the undone +1.00 EV against -0.50 EV",
        "undo",
        "typed",
        brighter,
    )?;
    claim(
        "undo",
        "undo against the committed +1.00 EV render",
        "undo",
        "release",
        same,
    )?;

    // The Tone group's reset: the photograph back to the opened one.
    claim(
        "reset",
        "the reset render against the opened one",
        "reset",
        "opened",
        same,
    )?;
    claim(
        "drag-open",
        "+2.00 EV drafted against neutral",
        "drag-open",
        "reset",
        brighter,
    )?;

    // An agent's commit while that gesture is open raises the Changed elsewhere notice,
    // which the plan holds. Reapply: the draft is rebased on the new revision, the notice gone, and
    // the drafted preview returns over the committed stack; Discard: the canvas is the committed
    // stack again.
    let reapply = launch.at("reapply")?;
    ensure(
        reapply.draft()["base_revision"].as_u64() == Some(reapply.revision()?),
        format!(
            "The reapplied draft is based on {} while the asset is at {}",
            reapply.draft()["base_revision"],
            reapply.revision()?
        ),
    )?;
    claim(
        "reapply",
        "the reapplied +2.00 EV against the committed stack",
        "reapply",
        "conflict",
        brighter,
    )?;
    claim(
        "discard",
        "the discarded gesture against the committed stack",
        "discard",
        "conflict",
        same,
    )?;

    // Every drafted and committed frame the gesture presented reports its own render time, and the
    // status bar in each captured frame states one of them, not the time since the open.
    let render_times = crate::smoke::expect_render_times(&launch.events, &launch.frames)?;

    checks.write(
        &launch.evidence,
        "basic",
        json!({
            "render_times": render_times,
            "brighter_margin": BRIGHTER,
            "same_tolerance": SAME,
            "scope": "Mean Rec. 709 luminance of the central window of the photograph the editor records drawing, read back from the renderer; not a colorimetric claim",
        }),
    )
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
            script::Step::call(
                "edit.set-basic",
                json!({"exposure":RESTART_EXPOSURE,"temperature":RESTART_TEMPERATURE}),
            ),
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
/// with the same layer and label — the plan holds the label, the payload and the fields — and the
/// same brighter photograph.
pub fn verify_restart(run: &mut Run, launches: &[Checked]) -> Result {
    let [first, second] = launches else {
        return Err(format!("Expected two launches, found {}", launches.len()).into());
    };
    let committed = first.at("committed")?;
    let reopened = second.at("reopened")?;
    ensure(
        committed.revision()? == 1,
        format!("Launch 1 committed revision {}", committed.revision()?),
    )?;
    let layer = committed
        .layer_id(BASIC_EFFECT)
        .ok_or("Launch 1's Basic layer has no identity")?;
    ensure(
        reopened.revision()? == committed.revision()? && reopened.entry()? == committed.entry()?,
        format!(
            "Launch 2 reopened at revision {} entry {}",
            reopened.revision()?,
            reopened.entry()?
        ),
    )?;
    ensure(
        reopened.layer_id(BASIC_EFFECT) == Some(layer),
        "The Basic layer's identity did not survive the restart",
    )?;
    // The photograph itself is the edited one again, to the same measured brightness.
    let mut checks = Checks::new();
    let neutral = first.at("opened")?.window_luminance()?;
    let edited = committed.window_luminance()?;
    let again = reopened.window_luminance()?;
    checks.compare(
        committed,
        "launch 1's committed edit against its own neutral open",
        edited,
        neutral,
        Tolerance::Above(BRIGHTER),
    )?;
    checks.compare(
        reopened,
        "the reopened render against the render launch 1 committed",
        again,
        edited,
        Tolerance::Within(SAME),
    )?;
    checks.compare(
        reopened,
        "the reopened render against a neutral open",
        again,
        neutral,
        Tolerance::Above(BRIGHTER),
    )?;
    checks.write(
        run.out(),
        "basic-restart",
        json!({
            "basic_layer": layer,
            "brighter_margin": BRIGHTER,
            "same_tolerance": SAME,
            "scope": "Mean Rec. 709 luminance of the central window of the photograph the editor records drawing, read back from the renderer; not a colorimetric claim",
        }),
    )
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
/// The White balance group's four controls, in order, as the panel draws them on every kind.
const WHITE_BALANCE_CONTROLS: [(&str, &str); 4] = [
    ("number", "Temperature"),
    ("number", "Tint"),
    ("picker", "Neutral picker"),
    ("action", "As shot"),
];
/// The button that returns the White balance group to the file's own rendering on a JPEG.
const AS_SHOT: &str = "As shot";

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

/// How bright the photograph's own pixels read at its edges, as a mean of their channels: well above
/// the canvas surface and well below the greyscale fixture's darkest grey.
const PLACED: u32 = 60;

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

/// Every frame of `basic-panel`, in order: the open, then one per step. No frame draws a notice.
pub fn panel_plan(_: &[PathBuf]) -> Plan {
    let steps = vec![
        // The whole Basic section, expanded, with nothing committed yet.
        Step::opened("opened")
            .expanded(BASIC_MODULE)
            .no_layer(BASIC_EFFECT),
        // The White balance group: a drag to +40 temperature, released. One entry, one revision.
        Step::new(
            "temperature",
            SliderStep::new(SET_BASIC, TEMPERATURE, [10.0, 25.0, 40.0]).release(),
        )
        .no_draft()
        .commits(1)
        .label("Temperature +40")
        .payload(BASIC_EFFECT, json!({ TEMPERATURE: 40.0 }))
        .field(SET_BASIC, TEMPERATURE, "40"),
        // The Colour group: a typed value committed with Enter, merged into the same layer.
        Step::new(
            "vibrance",
            script::Step::field(SET_BASIC, VIBRANCE, "25", true),
        )
        .commits(1)
        .label("Vibrance +25")
        .payload(BASIC_EFFECT, json!({ TEMPERATURE: 40.0, VIBRANCE: 25.0 }))
        .same_layer(BASIC_EFFECT, "temperature"),
        // The Temperature entry, previewed: its own saved values fill the disabled sliders, which
        // are not the current ones, and nothing is committed.
        Step::new("preview", PreviewStep::Sequence(1))
            .commits(0)
            .status_starts("Previewing entry 1")
            .field(SET_BASIC, TEMPERATURE, "40")
            .field(SET_BASIC, VIBRANCE, "0"),
        // Return to current: the current entry's values come back.
        Step::new("current", PreviewStep::Current)
            .commits(0)
            .field(SET_BASIC, VIBRANCE, "25"),
        // The Colour group's own reset button: that group neutral, every other field untouched and
        // the layer kept.
        Step::new(
            "colour-reset",
            script::Step::reset(BASIC_MODULE, Some(COLOUR_GROUP)),
        )
        .commits(1)
        .label(format!("Reset {COLOUR_GROUP}"))
        .payload(BASIC_EFFECT, json!({ TEMPERATURE: 40.0 }))
        .same_layer(BASIC_EFFECT, "temperature")
        .field(SET_BASIC, VIBRANCE, "0")
        .field(SET_BASIC, TEMPERATURE, "40"),
        // The neutral picker's canvas mode, which `W` also selects, saying so in the status bar.
        Step::new("picker-mode", WorkspaceStep::default().mode(BASIC_MODULE))
            .commits(0)
            .mode(BASIC_MODULE)
            .status_starts("Neutral picker"),
        // A pick on a neutral grey patch: the picker answers 0 and 0 and commits that, once,
        // through the ordinary action path, and leaves the mode as Escape does. The module labels
        // a patch that returns exactly one group to neutral by that group's name, whatever route
        // sent it.
        Step::new(
            "neutral-pick",
            script::Step::pick(NEUTRAL_PICK[0], NEUTRAL_PICK[1]),
        )
        .commits(1)
        .field(SET_BASIC, TEMPERATURE, "0")
        .field(SET_BASIC, TINT, "0")
        .payload(BASIC_EFFECT, json!({}))
        .same_layer(BASIC_EFFECT, "temperature")
        .label(format!("Reset {WHITE_BALANCE_GROUP}"))
        .mode(POINTER_MODE),
        // A committed pick leaves the mode, as Escape does, so the second pick enters it again.
        Step::new(
            "picker-mode-again",
            WorkspaceStep::default().mode(BASIC_MODULE),
        )
        .commits(0)
        .mode(BASIC_MODULE),
        // A pick on a clipped patch: refused with its reason leading the status bar, nothing
        // committed.
        Step::new(
            "clipped-pick",
            script::Step::pick(CLIPPED_PICK[0], CLIPPED_PICK[1]),
        )
        .commits(0)
        .status_starts("clipped:"),
        // Warm again, then As shot: on a JPEG the file's own rendering, Temperature and Tint 0,
        // run from the palette entry that sends exactly what its button sends.
        Step::new(
            "warm-again",
            SliderStep::new(SET_BASIC, TEMPERATURE, [20.0, 40.0]).release(),
        )
        .no_draft()
        .commits(1)
        .field(SET_BASIC, TEMPERATURE, "40"),
        Step::new("as-shot", PaletteStep::Run(AS_SHOT.into()))
            .commits(1)
            .label(format!("Reset {WHITE_BALANCE_GROUP}"))
            .payload(BASIC_EFFECT, json!({}))
            .same_layer(BASIC_EFFECT, "temperature")
            .field(SET_BASIC, TEMPERATURE, "0")
            .field(SET_BASIC, TINT, "0"),
    ];
    Plan::new(steps.into_iter().map(Step::no_notices).collect())
}

/// The White balance group's controls as a frame records the Basic section: the controls after
/// the group's own entry, up to the next group.
fn white_balance_controls(frame: &Frame) -> Result<Vec<(String, String)>> {
    let controls = frame["state"]["section_controls"][BASIC_MODULE]
        .as_array()
        .ok_or("The frame records no Basic section controls")?;
    let start = controls
        .iter()
        .position(|control| control["kind"] == "group" && control["label"] == WHITE_BALANCE_GROUP)
        .ok_or("The Basic section draws no White balance group")?;
    Ok(controls[start + 1..]
        .iter()
        .take_while(|control| control["kind"] != "group")
        .map(|control| {
            (
                control["kind"].as_str().unwrap_or_default().to_owned(),
                control["label"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect())
}

pub fn verify_panel(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let balance_of = |step: &str| -> Result<f64> { Ok(balance(launch.at(step)?.window_rgb()?)) };
    let mut checks = Checks::new();

    // The whole Basic section: every implemented field is on screen at its declared default, and
    // the greyscale fixture reads neutral.
    let opened = launch.at("opened")?;
    for name in BASIC_FIELDS {
        ensure(
            basic_field(opened, name)? == neutral_text(name)?,
            format!(
                "{name} does not start at its declared default: {:?}",
                basic_field(opened, name)?
            ),
        )?;
    }
    checks.compare(
        opened,
        "the greyscale fixture's red minus blue",
        balance_of("opened")?,
        0.0,
        Tolerance::Within(NEUTRAL),
    )?;
    // Where the photograph is drawn: the fixture's 3:2, centred in the photo surface and filling it
    // at Fit, with its bright pixels ending at the recorded edges. The threshold is well above the
    // canvas surface and well below the fixture's darkest grey; the plan holds that no frame draws a
    // notice over it.
    let [left, top, right, bottom] = opened
        .photo_edges(|p| (u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) / 3 >= PLACED)?;
    let [surface_left, surface_right] = opened
        .columns()?
        .ok_or("The frame records no photo surface")?;
    let (drawn_width, drawn_height) = (f64::from(right - left), f64::from(bottom - top));
    checks.compare(
        opened,
        "the displayed aspect ratio against the fixture's 3:2",
        drawn_width / drawn_height,
        1.5,
        Tolerance::Under(0.015),
    )?;
    checks.compare(
        opened,
        "the photograph's centre against the photo surface's",
        f64::from(left + right) / 2.0,
        f64::from(surface_left + surface_right) / 2.0,
        Tolerance::Within(5.0),
    )?;
    checks.compare(
        opened,
        "the photograph's width against half the photo surface's",
        drawn_width,
        f64::from(surface_right - surface_left) * 0.5,
        Tolerance::Above(0.0),
    )?;
    // White balance holds its four controls, in order: Temperature, Tint, the Neutral picker and
    // As shot, which on a JPEG sends Basic's own 0 and 0.
    let white_balance = white_balance_controls(opened)?;
    ensure(
        white_balance
            .iter()
            .map(|(kind, label)| (kind.as_str(), label.as_str()))
            .eq(WHITE_BALANCE_CONTROLS),
        format!("The White balance group draws {white_balance:?}"),
    )?;
    let as_shot = opened.state()["section_controls"][BASIC_MODULE]
        .as_array()
        .and_then(|controls| controls.iter().find(|control| control["label"] == AS_SHOT))
        .cloned()
        .unwrap_or_default();
    ensure(
        as_shot["action"] == SET_BASIC,
        format!("As shot on a JPEG is not Basic's own: {as_shot}"),
    )?;
    // The opened frame is also the default screen the Module panels density is accepted on: Basic
    // expanded and every other section collapsed to its band by its own descriptor, so the
    // histogram, Basic and every other section's band are on screen at once.
    let expanded = &opened.state()["expanded"];
    ensure(
        expanded
            .as_object()
            .ok_or("Missing expanded sections")?
            .iter()
            .all(|(module, open)| (module == BASIC_MODULE) == (open == &json!(true))),
        format!("Only Basic should be expanded on the opened screen: {expanded}"),
    )?;

    // +40 temperature, and the same again before As shot: the neutral fixture is visibly warmer.
    for step in ["temperature", "warm-again"] {
        checks.compare(
            launch.at(step)?,
            "+40 temperature against the neutral open, red minus blue",
            balance_of(step)?,
            balance_of("opened")?,
            Tolerance::Above(WARMER),
        )?;
    }

    // The neutral picker's canvas mode, which the plan holds with its status line. The picker
    // lives in the White balance group, beside the two fields a pick sets, and reads selected
    // exactly while its mode is active. The mode strip holds no entry for it at all.
    let picker = |step: &str| -> Result<Value> {
        Ok(launch.at(step)?.state()["pickers"][BASIC_MODULE].clone())
    };
    ensure(
        picker("opened")?["label"] == json!("Neutral picker")
            && picker("opened")?["shortcut"] == json!("W")
            && picker("opened")?["selected"] == json!(false),
        format!(
            "The Basic panel declares no picker, or it reads selected before its mode was entered: {}",
            picker("opened")?
        ),
    )?;
    ensure(
        picker("picker-mode")?["selected"] == json!(true),
        format!(
            "The picker does not read selected in its own mode: {}",
            picker("picker-mode")?
        ),
    )?;

    // The pick on a neutral grey patch took the warm cast the drag left away: the photograph is the
    // opened one again, and the committed pick left the mode, as Escape does. As shot after the
    // second warm drag does the same, in one entry labelled as the group's reset.
    ensure(
        picker("neutral-pick")?["selected"] == json!(false),
        format!(
            "A committed pick left the picker selected: {}",
            picker("neutral-pick")?
        ),
    )?;
    for (step, what) in [
        ("neutral-pick", "the picked correction"),
        ("as-shot", "As shot"),
    ] {
        checks.compare(
            launch.at(step)?,
            &format!("{what} against the opened photograph, red minus blue"),
            balance_of(step)?,
            balance_of("opened")?,
            Tolerance::Within(NEUTRAL),
        )?;
    }

    // A pick on a clipped patch: refused with the core's own reason leading the status bar, which
    // the plan holds, and nothing drawn differently.
    checks.compare(
        launch.at("clipped-pick")?,
        "the refused pick's render against the picked one",
        launch.at("clipped-pick")?.window_luminance()?,
        launch.at("neutral-pick")?.window_luminance()?,
        Tolerance::Within(SAME),
    )?;

    checks.write(
        &launch.evidence,
        "basic-panel",
        json!({
            "warmer_margin": WARMER,
            "neutral_tolerance": NEUTRAL,
            "placed_threshold": PLACED,
            "scope": "Mean per-channel readback of the central window of the photograph the editor records drawing; a warm/cool direction, not a colorimetric claim. Placement is the recorded rectangle, whose edges are checked against where the fixture's pixels end",
        }),
    )
}
