//! The `basic` smoke scenario: the generated Exposure slider's whole gesture, on the real editor.
//!
//! It drives the same messages a pointer drag, a typed value, a reset and the Changed elsewhere
//! notice produce, and checks each captured frame against the draft, the revision, the history
//! label and the photograph's own brightness. Nothing here names a pixel the desktop chose: the
//! brightness check reads a centred window of the canvas, which is inside the fitted photograph at
//! every zoom this scenario uses and clear of the notices above it and the mode strip below it.
use crate::{
    scenario::{Fixture, Frame, Launch, Run, pixels, preamble},
    *,
};

const BASIC_MODULE: &str = "lightwell.basic";
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

/// The evidence script. Each step is one gesture, one request or one decision; `verify` below
/// checks exactly what each one is supposed to prove.
pub fn script(scenario: &str) -> Option<Value> {
    match scenario {
        "basic" => Some(json!([
            // A drag to +1.00 EV, left open: the frame shows the drafted preview.
            {"slider":{"action":SET_BASIC,"parameter":EXPOSURE,"values":[0.25,0.5,1.0]}},
            // The same gesture released: one entry, one revision.
            {"slider":{"action":SET_BASIC,"parameter":EXPOSURE,"values":[1.0],"release":true}},
            // A second drag that returns to where it started: no entry at all.
            {"slider":{"action":SET_BASIC,"parameter":EXPOSURE,"values":[0.5,1.0],"release":true}},
            // The value field and Enter, which commits one field without a draft.
            {"field":{"action":SET_BASIC,"parameter":EXPOSURE,"text":"-0.5","submit":true}},
            // Undo: the slider re-seeds from the entry that is current again.
            {"api":{"method":"history.undo"}},
            // The Tone group's own reset button.
            {"reset":{"module":BASIC_MODULE,"group":TONE_GROUP}},
            // A drag left open, then somebody else commits under it.
            {"slider":{"action":SET_BASIC,"parameter":EXPOSURE,"values":[2.0]}},
            {"api":{"method":"edit.transform","params":{"transform":"rotate-right"}}},
            // The notice's two decisions, in turn.
            {"slider_draft":"reapply"},
            {"slider_draft":"discard"}
        ])),
        _ => None,
    }
}

/// What the Exposure field showed when the frame was captured.
fn exposure_field(frame: &Frame) -> Result<&str> {
    frame.field(SET_BASIC, EXPOSURE)
}

/// The one Basic layer's stored payload, or `None` when the stack holds no Basic layer.
fn basic_payload(frame: &Frame) -> Option<&Value> {
    frame.payload(lightwell_core::BASIC_EFFECT)
}

fn brighter(what: &str, more: f64, less: f64) -> Result {
    ensure(
        more > less + BRIGHTER,
        format!("{what}: mean luminance {more:.1} is not above {less:.1} by {BRIGHTER}"),
    )
}

fn same(what: &str, a: f64, b: f64) -> Result {
    ensure(
        (a - b).abs() <= SAME,
        format!("{what}: mean luminance {a:.1} and {b:.1} differ by more than {SAME}"),
    )
}

pub fn verify(evidence: &Path, app: &Value, events: &[Value]) -> Result {
    ensure(
        app["had_input_errors"] == json!(false),
        "The run recorded an input error",
    )?;
    let frames = Frame::all(evidence, app)?;
    let luminance: Vec<f64> = frames
        .iter()
        .map(pixels::window_luminance)
        .collect::<Result<Vec<_>>>()?;
    let mut checks = Vec::new();
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };

    // Frame 0: the fixture opens with the Basic section listed, its slider at the declared
    // default and no draft anywhere.
    let basic = frames[0]["state"]["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|module| module["id"] == json!(BASIC_MODULE))
        .ok_or("The Basic module is not listed at all")?;
    ensure(
        basic["available"] == json!(true),
        "The Basic module is not available",
    )?;
    ensure(
        exposure_field(&frames[0])? == "0.00",
        format!(
            "Exposure does not start neutral: {}",
            exposure_field(&frames[0])?
        ),
    )?;
    frames[0].expect_no_draft("Frame 0")?;
    ensure(
        basic_payload(&frames[0]).is_none(),
        "The opened stack already holds a Basic layer",
    )?;
    record(
        &frames[0],
        "the default panel with the Basic section, Exposure at 0",
        json!({
            "pixels": frames[0].fixture(Fixture::fit(1))?,
            "mean_luminance": luminance[0],
        }),
    );

    // Frame 1: mid-gesture at +1.00 EV. The draft is open, nothing is committed, and the
    // photograph on screen is the drafted render, which is brighter.
    let drafted = frames[1].draft();
    ensure(
        drafted["action"] == json!(SET_BASIC)
            && drafted["fields"] == json!({ EXPOSURE: 1.0 })
            && drafted["conflicted"] == json!(false),
        format!("Frame 1's draft is not the open Exposure gesture: {drafted}"),
    )?;
    ensure(
        drafted["draft_revision"]
            .as_u64()
            .is_some_and(|value| value >= 1),
        format!("Frame 1's draft carries no draft revision: {drafted}"),
    )?;
    ensure(
        frames[1]["state"]["displayed_draft_revision"] == drafted["draft_revision"],
        format!(
            "Frame 1 displays draft revision {} while the draft is at {}",
            frames[1]["state"]["displayed_draft_revision"], drafted["draft_revision"]
        ),
    )?;
    ensure(
        frames[1].revision()? == frames[0].revision()? && basic_payload(&frames[1]).is_none(),
        "A drag committed something",
    )?;
    ensure(
        exposure_field(&frames[1])? == "1.00",
        format!(
            "The slider does not show the drafted value: {}",
            exposure_field(&frames[1])?
        ),
    )?;
    brighter(
        "+1.00 EV drafted against the neutral open",
        luminance[1],
        luminance[0],
    )?;
    record(
        &frames[1],
        "a drag to +1.00 EV, mid-gesture: the drafted preview, nothing committed",
        json!({"draft": drafted, "mean_luminance": luminance[1]}),
    );

    // Frame 2: the release. One entry, labelled by the module, and the revision advanced by one.
    frames[2].expect_no_draft("Frame 2")?;
    ensure(
        frames[2].revision()? == frames[1].revision()? + 1,
        format!(
            "The release advanced the revision from {} to {}, expected one step",
            frames[1].revision()?,
            frames[2].revision()?
        ),
    )?;
    ensure(
        frames[2].entry()? != frames[1].entry()?,
        "The release created no new history entry",
    )?;
    ensure(
        frames[2].label()? == "Exposure +1.00 EV",
        format!("The committed entry is labelled {:?}", frames[2].label()?),
    )?;
    ensure(
        basic_payload(&frames[2]) == Some(&json!({ EXPOSURE: 1.0 })),
        format!(
            "The committed Basic layer holds {:?}",
            basic_payload(&frames[2])
        ),
    )?;
    same(
        "the committed render against the drafted one",
        luminance[2],
        luminance[1],
    )?;
    record(
        &frames[2],
        "released: one entry \"Exposure +1.00 EV\", the revision advanced by one",
        json!({"revision": frames[2].revision()?, "label": frames[2].label()?, "mean_luminance": luminance[2]}),
    );

    // Frame 3: a second drag that returned to +1.00 and released. Nothing at all was committed.
    frames[3].expect_no_draft("Frame 3")?;
    ensure(
        frames[3].revision()? == frames[2].revision()?
            && frames[3].entry()? == frames[2].entry()?,
        format!(
            "A return-to-start gesture created an entry: revision {} entry {}",
            frames[3].revision()?,
            frames[3].entry()?
        ),
    )?;
    same("the return-to-start render", luminance[3], luminance[2])?;
    record(
        &frames[3],
        "a drag back to +1.00 EV and released: no entry, no revision",
        json!({"revision": frames[3].revision()?, "entry": frames[3].entry()?}),
    );

    // Frame 4: -0.50 EV typed into the value field and submitted with Enter. One entry, and the
    // photograph is darker than it was at neutral.
    frames[4].expect_no_draft("Frame 4")?;
    ensure(
        frames[4].revision()? == frames[3].revision()? + 1,
        "Enter in the value field did not commit exactly one revision",
    )?;
    ensure(
        frames[4].label()? == "Exposure -0.50 EV",
        format!("The typed entry is labelled {:?}", frames[4].label()?),
    )?;
    ensure(
        basic_payload(&frames[4]) == Some(&json!({ EXPOSURE: -0.5 })),
        format!("The typed value stored {:?}", basic_payload(&frames[4])),
    )?;
    brighter(
        "the neutral open against -0.50 EV",
        luminance[0],
        luminance[4],
    )?;
    record(
        &frames[4],
        "-0.50 EV typed and submitted with Enter: one entry",
        json!({"label": frames[4].label()?, "mean_luminance": luminance[4]}),
    );

    // Frame 5: undo. The current entry is the +1.00 one again and the slider re-seeds from it.
    ensure(
        frames[5].entry()? == frames[2].entry()?,
        "Undo did not return to the +1.00 EV entry",
    )?;
    ensure(
        exposure_field(&frames[5])? == "1.00",
        format!(
            "The slider did not re-seed from the entry undo returned to: {}",
            exposure_field(&frames[5])?
        ),
    )?;
    brighter(
        "the undone +1.00 EV against -0.50 EV",
        luminance[5],
        luminance[4],
    )?;
    same(
        "undo against the committed +1.00 EV render",
        luminance[5],
        luminance[2],
    )?;
    record(
        &frames[5],
        "history.undo: the values re-seed to +1.00 and the pixels follow",
        json!({"entry": frames[5].entry()?, "exposure": exposure_field(&frames[5])?, "mean_luminance": luminance[5]}),
    );

    // Frame 6: the Tone group's reset. One entry labelled by the module, the slider at 0, the
    // layer kept with its neutral payload and the photograph back to the opened one.
    ensure(
        frames[6].label()? == "Reset Tone",
        format!("The group reset is labelled {:?}", frames[6].label()?),
    )?;
    ensure(
        exposure_field(&frames[6])? == "0.00",
        format!(
            "The slider did not return to 0: {}",
            exposure_field(&frames[6])?
        ),
    )?;
    ensure(
        basic_payload(&frames[6]) == Some(&json!({})),
        format!(
            "The reset layer holds {:?}, expected the neutral payload",
            basic_payload(&frames[6])
        ),
    )?;
    same(
        "the reset render against the opened one",
        luminance[6],
        luminance[0],
    )?;
    record(
        &frames[6],
        "the Tone group reset: entry \"Reset Tone\", the slider at 0, the layer kept and neutral",
        json!({"label": frames[6].label()?, "payload": basic_payload(&frames[6]), "mean_luminance": luminance[6]}),
    );

    // Frame 7: a drag to +2.00 EV, left open.
    ensure(
        frames[7].draft()["fields"] == json!({ EXPOSURE: 2.0 })
            && frames[7].draft()["conflicted"] == json!(false),
        format!(
            "Frame 7's draft is not the open gesture: {}",
            frames[7].draft()
        ),
    )?;
    brighter(
        "+2.00 EV drafted against neutral",
        luminance[7],
        luminance[6],
    )?;
    record(
        &frames[7],
        "a drag to +2.00 EV, left open",
        json!({"draft": frames[7].draft(), "mean_luminance": luminance[7]}),
    );

    // Frame 8: a commit by another route while that gesture is open. The draft is kept, marked
    // conflicted, and the Changed elsewhere notice offers the two decisions.
    let conflict_notices = frames[8].notices();
    ensure(
        conflict_notices
            .iter()
            .any(|notice| notice == "Changed elsewhere"),
        format!("Frame 8's notices do not include the conflict: {conflict_notices:?}"),
    )?;
    ensure(
        frames[8].draft()["conflicted"] == json!(true),
        format!(
            "The gesture was not marked conflicted: {}",
            frames[8].draft()
        ),
    )?;
    ensure(
        frames[8].draft()["fields"] == json!({ EXPOSURE: 2.0 }),
        "The conflicted draft lost the value the gesture set",
    )?;
    ensure(
        frames[8].revision()? == frames[7].revision()? + 1,
        "The external commit did not advance the revision by one",
    )?;
    ensure(
        exposure_field(&frames[8])? == "2.00",
        format!(
            "The slider lost its drafted value: {}",
            exposure_field(&frames[8])?
        ),
    )?;
    record(
        &frames[8],
        "a commit during the gesture: Changed elsewhere, the draft kept and conflicted",
        json!({"notices": conflict_notices, "draft": frames[8].draft(), "revision": frames[8].revision()?}),
    );

    // Frame 9: Reapply. The draft is rebased on the new revision, its value is re-sent and the
    // drafted preview returns over the committed stack.
    let reapplied = frames[9].draft();
    ensure(
        reapplied["conflicted"] == json!(false),
        format!("Reapply did not clear the conflict: {reapplied}"),
    )?;
    ensure(
        reapplied["base_revision"].as_u64() == Some(frames[9].revision()?),
        format!(
            "The reapplied draft is based on {} while the asset is at {}",
            reapplied["base_revision"],
            frames[9].revision()?
        ),
    )?;
    ensure(
        reapplied["fields"] == json!({ EXPOSURE: 2.0 }),
        "Reapply lost the field this client set",
    )?;
    ensure(
        frames[9].notices().is_empty(),
        format!("Frame 9 still shows a notice: {:?}", frames[9].notices()),
    )?;
    brighter(
        "the reapplied +2.00 EV against the committed stack",
        luminance[9],
        luminance[8],
    )?;
    record(
        &frames[9],
        "Reapply: the draft rebases, its value is re-sent and the drafted preview returns",
        json!({"draft": reapplied, "mean_luminance": luminance[9]}),
    );

    // Frame 10: Discard. The gesture is over, nothing was committed for it, and the canvas is the
    // committed stack again.
    frames[10].expect_no_draft("Frame 10")?;
    ensure(
        frames[10].revision()? == frames[9].revision()?
            && frames[10].entry()? == frames[9].entry()?,
        "Discarding the draft committed something",
    )?;
    ensure(
        exposure_field(&frames[10])? == "0.00",
        format!(
            "The slider did not return to the authoritative value: {}",
            exposure_field(&frames[10])?
        ),
    )?;
    same(
        "the discarded gesture against the committed stack",
        luminance[10],
        luminance[8],
    )?;
    record(
        &frames[10],
        "Discard: the gesture ends, nothing is committed and the committed pixels return",
        json!({"revision": frames[10].revision()?, "mean_luminance": luminance[10]}),
    );

    // Every drafted and committed frame the gesture presented reports its own render time, and the
    // status bar in each captured frame states one of them, not the time since the open.
    let render_times = crate::smoke::expect_render_times(events, &frames)?;

    write_json(
        &evidence.join("basic-checks.json"),
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

/// The fixture both launches open, and the window both use.
const RESTART_FIXTURE: &str = "fixtures/s0/orientation-1.jpg";

/// What the first launch commits: two fields in one patch, so the second launch proves both the
/// stored payload and the panel's re-seeding rather than a single slider.
const RESTART_EXPOSURE: f64 = 1.5;
const RESTART_TEMPERATURE: f64 = 25.0;

/// Two launches, because a restart cannot be simulated inside one process: the first commits a
/// Basic edit into its own evidence catalog, the second reopens that catalog and the same file and
/// must show the same values, the same layer and the same brighter photograph.
pub fn restart(mut run: Run) -> Result {
    let fixture = run.root().join(RESTART_FIXTURE);
    run.note(
        "Two launches, because a restart cannot be simulated inside one process: the first opens the fixture and commits one `edit.set-basic` patch of exposure and temperature; the second reuses that launch's own catalog (`--catalog <dir1>/catalog.sqlite`) and reopens the same file, which the catalog dedupes to the same asset, so the saved Basic layer, its identity, the slider values, the history label and the rendered brightness all come back.",
    );
    run.check(|run| {
        run.hash(std::slice::from_ref(&fixture))?;

        // Launch 1: open the fixture, commit one Basic patch through the ordinary edit path.
        let launch1 = run.launch(
            Launch::named("launch1")
                .open(&fixture)
                .script(
                    "script1.json",
                    json!([{"api":{"method":"edit.set-basic","params":{"exposure":RESTART_EXPOSURE,"temperature":RESTART_TEMPERATURE}}}]),
                )
                .window(workspace_smoke::WINDOW),
        )?;
        let (app1, _) = preamble(&launch1, 2)?;
        let frames1 = app1["frames"]
            .as_array()
            .ok_or("Launch 1 wrote no frames")?;
        let opened = &Frame::identified(&launch1, &app1, &frames1[0])?;
        let committed = &Frame::identified(&launch1, &app1, &frames1[1])?;
        ensure(
            basic_payload(opened).is_none(),
            "Launch 1 opened with a Basic layer already in the stack",
        )?;
        ensure(
            committed.revision()? == 1,
            format!("Launch 1 committed revision {}", committed.revision()?),
        )?;
        let stored = basic_payload(committed)
            .ok_or("Launch 1 committed no Basic layer")?
            .clone();
        ensure(
            stored == json!({ EXPOSURE: RESTART_EXPOSURE, TEMPERATURE: RESTART_TEMPERATURE }),
            format!("Launch 1 stored {stored}"),
        )?;
        let layer = basic_layer_id(committed)
            .ok_or("Launch 1's Basic layer has no identity")?
            .to_owned();
        let neutral_luminance = pixels::window_luminance(opened)?;
        let edited_luminance = pixels::window_luminance(committed)?;
        brighter(
            "launch 1's committed edit against its own neutral open",
            edited_luminance,
            neutral_luminance,
        )?;

        // Launch 2: the same catalog and the same file, in a new process. The catalog dedupes the
        // reopened file to the same asset by file identity, so its saved stack comes back.
        let catalog = launch1.join("catalog.sqlite");
        ensure(catalog.is_file(), "Launch 1 wrote no catalog")?;
        let launch2 = run.launch(
            Launch::named("launch2")
                .catalog(&catalog)
                .open(&fixture)
                .window(workspace_smoke::WINDOW),
        )?;
        let (app2, _) = preamble(&launch2, 1)?;
        let reopened = &Frame::identified(
            &launch2,
            &app2,
            &app2["frames"]
                .as_array()
                .ok_or("Launch 2 wrote no frames")?[0],
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
            basic_payload(reopened) == Some(&stored),
            format!(
                "Launch 2 reads the Basic layer as {:?}",
                basic_payload(reopened)
            ),
        )?;
        ensure(
            basic_layer_id(reopened) == Some(layer.as_str()),
            "The Basic layer's identity did not survive the restart",
        )?;
        // The sliders re-seed from the stored layer, which is what a person sees on reopening.
        ensure(
            basic_field(reopened, EXPOSURE)? == "1.50"
                && basic_field(reopened, TEMPERATURE)? == "25",
            format!(
                "Launch 2's fields read exposure {:?}, temperature {:?}",
                basic_field(reopened, EXPOSURE)?,
                basic_field(reopened, TEMPERATURE)?
            ),
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
        same(
            "the reopened render against the render launch 1 committed",
            reopened_luminance,
            edited_luminance,
        )?;
        brighter(
            "the reopened render against a neutral open",
            reopened_luminance,
            neutral_luminance,
        )?;
        run.sources_unchanged()?;
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
    })
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
    let registry = lightwell_core::ModuleRegistry::builtin();
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
    frame.layer_id(lightwell_core::BASIC_EFFECT)
}

/// The evidence script. Each step is one gesture, one request or one decision; `verify_panel`
/// checks exactly what each one proves.
pub fn panel_script(scenario: &str) -> Option<Value> {
    match scenario {
        "basic-panel" => Some(json!([
            // The White balance group: a drag to +40 temperature, released.
            {"slider":{"action":SET_BASIC,"parameter":TEMPERATURE,"values":[10.0,25.0,40.0],"release":true}},
            // The Colour group: a typed value committed with Enter.
            {"field":{"action":SET_BASIC,"parameter":VIBRANCE,"text":"25","submit":true}},
            // The Temperature entry, previewed: its own saved values fill the disabled sliders.
            {"preview":{"sequence":1}},
            {"preview":"current"},
            // The Colour group's own reset button.
            {"reset":{"module":BASIC_MODULE,"group":COLOUR_GROUP}},
            // The neutral picker's canvas mode, which `W` also selects.
            {"workspace":{"mode":BASIC_MODULE}},
            // A pick on a neutral grey patch: the picker answers 0 and 0 and commits that.
            {"pick":{"x":NEUTRAL_PICK[0],"y":NEUTRAL_PICK[1]}},
            // A pick on a clipped patch: refused with its reason, nothing committed.
            {"pick":{"x":CLIPPED_PICK[0],"y":CLIPPED_PICK[1]}}
        ])),
        _ => None,
    }
}

pub fn verify_panel(evidence: &Path, app: &Value, _events: &[Value]) -> Result {
    ensure(
        app["had_input_errors"] == json!(false),
        "The run recorded an input error",
    )?;
    let frames = Frame::all(evidence, app)?;
    let channels: Vec<[f64; 3]> = frames
        .iter()
        .map(pixels::window_rgb)
        .collect::<Result<Vec<_>>>()?;
    let luminance: Vec<f64> = frames
        .iter()
        .map(pixels::window_luminance)
        .collect::<Result<Vec<_>>>()?;
    let mut checks = Vec::new();
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };

    // Frame 0: the whole Basic section. Every implemented field is on screen with a value, the
    // section is expanded, and nothing is committed yet.
    let mut listed = Vec::new();
    for name in BASIC_FIELDS {
        listed.push(json!({ name: basic_field(&frames[0], name)? }));
        ensure(
            basic_field(&frames[0], name)? == neutral_text(name)?,
            format!(
                "{name} does not start at its declared default: {:?}",
                basic_field(&frames[0], name)?
            ),
        )?;
    }
    ensure(
        frames[0]["state"]["expanded"][BASIC_MODULE] == json!(true),
        "The Basic section is not expanded on the opened screen",
    )?;
    ensure(
        basic_payload(&frames[0]).is_none(),
        "The opened stack already holds a Basic layer",
    )?;
    ensure(
        balance(channels[0]).abs() <= NEUTRAL,
        format!(
            "The greyscale fixture does not read neutral: {:.1}",
            balance(channels[0])
        ),
    )?;
    // No frame in this scenario draws a notice, which is what lets `placement` read the band
    // between the notices and the mode strip as photograph and canvas surface only.
    for (index, frame) in frames.iter().enumerate() {
        ensure(
            frame["state"]["notices"] == json!([]),
            format!(
                "Frame {index} shows a notice: {}",
                frame["state"]["notices"]
            ),
        )?;
    }
    record(
        &frames[0],
        "the Basic section with White balance, Tone and Colour, every field at its default",
        json!({
            "placement": placement(&frames[0])?,
            "fields": listed,
            "red_minus_blue": balance(channels[0]),
        }),
    );

    // Frame 1: a Temperature drag to +40, released. One entry, one revision, and the neutral
    // fixture is visibly warmer.
    frames[1].expect_no_draft("Frame 1")?;
    ensure(
        frames[1].revision()? == frames[0].revision()? + 1,
        "The released drag did not advance the revision by one",
    )?;
    ensure(
        frames[1].label()? == "Temperature +40",
        format!("The committed entry is labelled {:?}", frames[1].label()?),
    )?;
    ensure(
        basic_payload(&frames[1]) == Some(&json!({ TEMPERATURE: 40.0 })),
        format!("The Basic layer holds {:?}", basic_payload(&frames[1])),
    )?;
    ensure(
        basic_field(&frames[1], TEMPERATURE)? == "40",
        format!(
            "The Temperature slider shows {:?}",
            basic_field(&frames[1], TEMPERATURE)?
        ),
    )?;
    ensure(
        balance(channels[1]) > balance(channels[0]) + WARMER,
        format!(
            "+40 temperature did not warm the photograph: red minus blue {:.1} against {:.1}",
            balance(channels[1]),
            balance(channels[0])
        ),
    )?;
    let identity = basic_layer_id(&frames[1])
        .ok_or("The committed stack holds no Basic layer")?
        .to_owned();
    record(
        &frames[1],
        "Temperature dragged to +40 and released: one entry, the photograph warmer",
        json!({"label": frames[1].label()?, "red_minus_blue": balance(channels[1]), "layer": identity}),
    );

    // Frame 2: Vibrance typed and committed with Enter. The patch merges into the same layer.
    ensure(
        frames[2].revision()? == frames[1].revision()? + 1,
        "Enter in the value field did not commit exactly one revision",
    )?;
    ensure(
        frames[2].label()? == "Vibrance +25",
        format!("The typed entry is labelled {:?}", frames[2].label()?),
    )?;
    ensure(
        basic_payload(&frames[2]) == Some(&json!({ TEMPERATURE: 40.0, VIBRANCE: 25.0 })),
        format!(
            "The merged Basic layer holds {:?}",
            basic_payload(&frames[2])
        ),
    )?;
    ensure(
        basic_layer_id(&frames[2]) == Some(identity.as_str()),
        "The typed value replaced the Basic layer instead of updating it",
    )?;
    record(
        &frames[2],
        "Vibrance typed as 25 and committed with Enter: the same layer, both fields",
        json!({"label": frames[2].label()?, "payload": basic_payload(&frames[2])}),
    );

    // Frame 3: the Temperature entry previewed. The sliders show that entry's own saved values,
    // which are not the current ones, and nothing is committed.
    ensure(
        frames[3].status()?.starts_with("Previewing entry 1"),
        format!(
            "Frame 3 is not previewing entry 1: {:?}",
            frames[3].status()?
        ),
    )?;
    ensure(
        basic_field(&frames[3], TEMPERATURE)? == "40" && basic_field(&frames[3], VIBRANCE)? == "0",
        format!(
            "The previewed entry's values are not shown: temperature {:?}, vibrance {:?}",
            basic_field(&frames[3], TEMPERATURE)?,
            basic_field(&frames[3], VIBRANCE)?
        ),
    )?;
    ensure(
        frames[3].revision()? == frames[2].revision()?,
        "Selecting a history entry committed something",
    )?;
    record(
        &frames[3],
        "the Temperature entry previewed: its own saved values in the disabled sliders",
        json!({"status": frames[3].status()?, "temperature": basic_field(&frames[3], TEMPERATURE)?, "vibrance": basic_field(&frames[3], VIBRANCE)?}),
    );

    // Frame 4: Return to current. The current entry's values come back.
    ensure(
        basic_field(&frames[4], VIBRANCE)? == "25",
        format!(
            "Return to current did not restore the current values: vibrance {:?}",
            basic_field(&frames[4], VIBRANCE)?
        ),
    )?;
    ensure(
        frames[4].revision()? == frames[2].revision()?,
        "Return to current changed the revision",
    )?;
    record(
        &frames[4],
        "Return to current: the current entry's values are shown again",
        json!({"vibrance": basic_field(&frames[4], VIBRANCE)?, "status": frames[4].status()?}),
    );

    // Frame 5: the Colour group's reset. One entry labelled by the module, the group's own fields
    // neutral, every other field untouched and the layer kept.
    ensure(
        frames[5].label()? == format!("Reset {COLOUR_GROUP}"),
        format!("The group reset is labelled {:?}", frames[5].label()?),
    )?;
    ensure(
        basic_payload(&frames[5]) == Some(&json!({ TEMPERATURE: 40.0 })),
        format!(
            "The Colour reset changed more than its group: {:?}",
            basic_payload(&frames[5])
        ),
    )?;
    ensure(
        basic_layer_id(&frames[5]) == Some(identity.as_str()),
        "The group reset replaced the Basic layer instead of updating it",
    )?;
    ensure(
        basic_field(&frames[5], VIBRANCE)? == "0" && basic_field(&frames[5], TEMPERATURE)? == "40",
        "The group reset did not leave the other group alone",
    )?;
    record(
        &frames[5],
        "the Colour group reset: one entry, that group neutral, White balance untouched",
        json!({"label": frames[5].label()?, "payload": basic_payload(&frames[5]), "layer": identity}),
    );

    // Frame 6: the neutral picker's canvas mode.
    ensure(
        frames[6]["state"]["workspace"]["mode"] == json!(BASIC_MODULE),
        format!(
            "The canvas mode is {}",
            frames[6]["state"]["workspace"]["mode"]
        ),
    )?;
    ensure(
        frames[6].status()?.starts_with("Neutral picker"),
        format!("The picker mode says {:?}", frames[6].status()?),
    )?;
    ensure(
        frames[6].revision()? == frames[5].revision()?,
        "Entering the picker mode committed something",
    )?;
    // The picker lives in the White balance group, beside the two fields a pick sets, and reads
    // selected exactly while its mode is active. The mode strip holds no entry for it at all.
    let picker = |frame: &Value| frame["state"]["pickers"][BASIC_MODULE].clone();
    ensure(
        picker(&frames[0])["label"] == json!("Neutral picker")
            && picker(&frames[0])["shortcut"] == json!("W"),
        format!("The Basic panel declares no picker: {}", picker(&frames[0])),
    )?;
    ensure(
        picker(&frames[0])["selected"] == json!(false),
        "The picker reads selected before its mode was entered",
    )?;
    ensure(
        picker(&frames[6])["selected"] == json!(true),
        format!(
            "The picker does not read selected in its own mode: {}",
            picker(&frames[6])
        ),
    )?;
    record(
        &frames[6],
        "the neutral picker mode, entered through workspace.set as W and the panel's own picker \
         button do; that button reads selected inside the White balance group",
        json!({
            "mode": frames[6]["state"]["workspace"]["mode"],
            "status": frames[6].status()?,
            "picker": picker(&frames[6]),
        }),
    );

    // Frame 7: a pick on a neutral grey patch. The picker answers the exact identity, 0 and 0,
    // and that patch is committed once through the ordinary action path, so the warm cast the
    // drag left is gone and the photograph is the opened one again.
    ensure(
        frames[7].revision()? == frames[6].revision()? + 1,
        "The pick did not commit exactly one revision",
    )?;
    ensure(
        basic_field(&frames[7], TEMPERATURE)? == "0" && basic_field(&frames[7], TINT)? == "0",
        format!(
            "The picked settings are not neutral: temperature {:?}, tint {:?}",
            basic_field(&frames[7], TEMPERATURE)?,
            basic_field(&frames[7], TINT)?
        ),
    )?;
    ensure(
        basic_payload(&frames[7]) == Some(&json!({})),
        format!(
            "The picked layer holds {:?}, expected the neutral payload",
            basic_payload(&frames[7])
        ),
    )?;
    ensure(
        basic_layer_id(&frames[7]) == Some(identity.as_str()),
        "The pick replaced the Basic layer instead of updating it",
    )?;
    // The module labels a patch that returns exactly one group to neutral by that group's name,
    // whatever route sent it, and a pick of a neutral patch is exactly such a patch.
    ensure(
        frames[7].label()? == format!("Reset {WHITE_BALANCE_GROUP}"),
        format!("The pick's entry is labelled {:?}", frames[7].label()?),
    )?;
    ensure(
        (balance(channels[7]) - balance(channels[0])).abs() <= NEUTRAL,
        format!(
            "The picked correction did not neutralise the photograph: red minus blue {:.1} against the opened {:.1}",
            balance(channels[7]),
            balance(channels[0])
        ),
    )?;
    ensure(
        frames[7]["state"]["workspace"]["mode"] == json!(BASIC_MODULE),
        "The pick left the picker mode",
    )?;
    record(
        &frames[7],
        "a pick on a neutral grey patch: temperature and tint 0, committed once, the mode kept",
        json!({"label": frames[7].label()?, "payload": basic_payload(&frames[7]), "red_minus_blue": balance(channels[7]), "mean_luminance": luminance[7]}),
    );

    // Frame 8: a pick on a clipped patch. The core's own reason leads the status bar and nothing
    // at all is committed.
    ensure(
        frames[8].status()?.starts_with("clipped:"),
        format!(
            "The refused pick does not lead with its reason: {:?}",
            frames[8].status()?
        ),
    )?;
    ensure(
        frames[8].revision()? == frames[7].revision()?
            && frames[8].entry()? == frames[7].entry()?,
        "A refused pick committed something",
    )?;
    same(
        "the refused pick's render against the picked one",
        luminance[8],
        luminance[7],
    )?;
    record(
        &frames[8],
        "a pick on a clipped patch: refused with its reason, nothing committed",
        json!({"status": frames[8].status()?, "revision": frames[8].revision()?}),
    );

    // Frame 0 is also the default screen the Module panels density is accepted on: Basic expanded
    // and every other section collapsed to its band by its own descriptor, so the histogram, Basic
    // and every other section's band are on screen at once.
    let expanded = &frames[0]["state"]["expanded"];
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
        &frames[0],
        "Basic expanded and every other section collapsed to its band by default",
        expanded.clone(),
    );

    write_json(
        &evidence.join("basic-panel-checks.json"),
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
