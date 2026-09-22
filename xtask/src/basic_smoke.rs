//! The `basic` smoke scenario: the generated Exposure slider's whole gesture, on the real editor.
//!
//! It drives the same messages a pointer drag, a typed value, a reset and the Changed elsewhere
//! notice produce, and checks each captured frame against the draft, the revision, the history
//! label and the photograph's own brightness. Nothing here names a pixel the desktop chose: the
//! brightness check reads a centred window of the canvas, which is inside the fitted photograph at
//! every zoom this scenario uses and clear of the notices above it and the mode strip below it.
use crate::{
    smoke::{Expect, columns, frame_identity, pixels},
    *,
};

const BASIC_MODULE: &str = "lightwell.basic";
const TRANSFORM_MODULE: &str = "lightwell.transform";
const CROP_MODULE: &str = "lightwell.crop";
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

/// One open frame plus one per script step.
pub fn frames(scenario: &str) -> Option<usize> {
    match scenario {
        "basic" => Some(11),
        "basic-panel" => Some(11),
        _ => None,
    }
}

/// Which of this file's two verifiers a scenario uses.
pub fn verify_scenario(evidence: &Path, scenario: &str, app: &Value, events: &[Value]) -> Result {
    match scenario {
        "basic-panel" => verify_panel(evidence, app, events),
        _ => verify(evidence, app, events),
    }
}

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

/// The mean Rec. 709 luminance of a centred window of the photo surface. The window is 40% of the
/// surface's width and 30% of the capture's height about its centre, which lies inside the fitted
/// photograph at both orientations this scenario shows and touches neither the notice cards at the
/// top of the canvas nor the mode strip at its bottom.
fn photo_luminance(path: &Path, frame: &Value) -> Result<f64> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [left, right] = columns(frame)?.unwrap_or([0, width]);
    ensure(left < right && right <= width, "Invalid surface columns")?;
    let surface = right - left;
    let (cx, cy) = ((left + right) / 2, height / 2);
    let (half_w, half_h) = (surface / 5, height * 3 / 20);
    ensure(
        half_w > 10 && half_h > 10 && cx > half_w && cy > half_h,
        "Photo surface too small to sample",
    )?;
    let mut total = 0.0;
    let mut count = 0u32;
    for y in (cy - half_h)..(cy + half_h) {
        for x in (cx - half_w)..(cx + half_w) {
            let p = image.get_pixel(x, y).0;
            total += 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
            count += 1;
        }
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(total / f64::from(count))
}

fn revision(frame: &Value) -> Result<u64> {
    frame["state"]["stack"]["revision"]
        .as_u64()
        .ok_or_else(|| "Frame records no revision".into())
}

fn entry(frame: &Value) -> Result<&str> {
    frame["state"]["stack"]["entry"]
        .as_str()
        .ok_or_else(|| "Frame records no current entry".into())
}

fn label(frame: &Value) -> Result<&str> {
    frame["state"]["stack"]["label"]
        .as_str()
        .ok_or_else(|| "Frame records no history label".into())
}

/// What the Exposure field showed when the frame was captured.
fn exposure_field(frame: &Value) -> Result<&str> {
    frame["state"]["controls"][format!("{SET_BASIC}.{EXPOSURE}")]
        .as_str()
        .ok_or_else(|| "Frame records no Exposure field".into())
}

fn notices(frame: &Value) -> Vec<String> {
    frame["state"]["notices"]
        .as_array()
        .map(|notices| {
            notices
                .iter()
                .filter_map(|notice| notice.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The draft the frame's session reported, or `Null` when the client held none.
fn draft(frame: &Value) -> &Value {
    &frame["state"]["draft"]
}

/// The one Basic layer's stored payload, or `None` when the stack holds no Basic layer.
fn basic_payload(frame: &Value) -> Option<&Value> {
    frame["state"]["stack"]["layers"]
        .as_array()?
        .iter()
        .find(|layer| layer["effect"] == json!(lightwell_core::BASIC_EFFECT))
        .map(|layer| &layer["payload"])
}

fn expect_no_draft(frame: &Value, what: &str) -> Result {
    ensure(
        draft(frame) == &Value::Null,
        format!("{what}: a draft is still open: {}", draft(frame)),
    )
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

pub fn verify(evidence: &Path, app: &Value, _events: &[Value]) -> Result {
    let frames = app["frames"].as_array().ok_or("Missing frames")?.clone();
    ensure(
        app["had_input_errors"] == json!(false),
        "The run recorded an input error",
    )?;
    let paths: Vec<PathBuf> = frames
        .iter()
        .map(|frame| frame_identity(evidence, app, frame))
        .collect::<Result<Vec<_>>>()?;
    let luminance: Vec<f64> = frames
        .iter()
        .zip(&paths)
        .map(|(frame, path)| photo_luminance(path, frame))
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
    expect_no_draft(&frames[0], "Frame 0")?;
    ensure(
        basic_payload(&frames[0]).is_none(),
        "The opened stack already holds a Basic layer",
    )?;
    record(
        &frames[0],
        "the default panel with the Basic section, Exposure at 0",
        json!({
            "pixels": pixels(&paths[0], &Expect { columns: columns(&frames[0])?, ..Expect::fit(1) })?,
            "mean_luminance": luminance[0],
        }),
    );

    // Frame 1: mid-gesture at +1.00 EV. The draft is open, nothing is committed, and the
    // photograph on screen is the drafted render, which is brighter.
    let drafted = draft(&frames[1]);
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
        revision(&frames[1])? == revision(&frames[0])? && basic_payload(&frames[1]).is_none(),
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
    expect_no_draft(&frames[2], "Frame 2")?;
    ensure(
        revision(&frames[2])? == revision(&frames[1])? + 1,
        format!(
            "The release advanced the revision from {} to {}, expected one step",
            revision(&frames[1])?,
            revision(&frames[2])?
        ),
    )?;
    ensure(
        entry(&frames[2])? != entry(&frames[1])?,
        "The release created no new history entry",
    )?;
    ensure(
        label(&frames[2])? == "Exposure +1.00 EV",
        format!("The committed entry is labelled {:?}", label(&frames[2])?),
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
        json!({"revision": revision(&frames[2])?, "label": label(&frames[2])?, "mean_luminance": luminance[2]}),
    );

    // Frame 3: a second drag that returned to +1.00 and released. Nothing at all was committed.
    expect_no_draft(&frames[3], "Frame 3")?;
    ensure(
        revision(&frames[3])? == revision(&frames[2])? && entry(&frames[3])? == entry(&frames[2])?,
        format!(
            "A return-to-start gesture created an entry: revision {} entry {}",
            revision(&frames[3])?,
            entry(&frames[3])?
        ),
    )?;
    same("the return-to-start render", luminance[3], luminance[2])?;
    record(
        &frames[3],
        "a drag back to +1.00 EV and released: no entry, no revision",
        json!({"revision": revision(&frames[3])?, "entry": entry(&frames[3])?}),
    );

    // Frame 4: -0.50 EV typed into the value field and submitted with Enter. One entry, and the
    // photograph is darker than it was at neutral.
    expect_no_draft(&frames[4], "Frame 4")?;
    ensure(
        revision(&frames[4])? == revision(&frames[3])? + 1,
        "Enter in the value field did not commit exactly one revision",
    )?;
    ensure(
        label(&frames[4])? == "Exposure -0.50 EV",
        format!("The typed entry is labelled {:?}", label(&frames[4])?),
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
        json!({"label": label(&frames[4])?, "mean_luminance": luminance[4]}),
    );

    // Frame 5: undo. The current entry is the +1.00 one again and the slider re-seeds from it.
    ensure(
        entry(&frames[5])? == entry(&frames[2])?,
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
        json!({"entry": entry(&frames[5])?, "exposure": exposure_field(&frames[5])?, "mean_luminance": luminance[5]}),
    );

    // Frame 6: the Tone group's reset. One entry labelled by the module, the slider at 0, the
    // layer kept with its neutral payload and the photograph back to the opened one.
    ensure(
        label(&frames[6])? == "Reset Tone",
        format!("The group reset is labelled {:?}", label(&frames[6])?),
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
        json!({"label": label(&frames[6])?, "payload": basic_payload(&frames[6]), "mean_luminance": luminance[6]}),
    );

    // Frame 7: a drag to +2.00 EV, left open.
    ensure(
        draft(&frames[7])["fields"] == json!({ EXPOSURE: 2.0 })
            && draft(&frames[7])["conflicted"] == json!(false),
        format!(
            "Frame 7's draft is not the open gesture: {}",
            draft(&frames[7])
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
        json!({"draft": draft(&frames[7]), "mean_luminance": luminance[7]}),
    );

    // Frame 8: a commit by another route while that gesture is open. The draft is kept, marked
    // conflicted, and the Changed elsewhere notice offers the two decisions.
    let conflict_notices = notices(&frames[8]);
    ensure(
        conflict_notices
            .iter()
            .any(|notice| notice == "Changed elsewhere"),
        format!("Frame 8's notices do not include the conflict: {conflict_notices:?}"),
    )?;
    ensure(
        draft(&frames[8])["conflicted"] == json!(true),
        format!(
            "The gesture was not marked conflicted: {}",
            draft(&frames[8])
        ),
    )?;
    ensure(
        draft(&frames[8])["fields"] == json!({ EXPOSURE: 2.0 }),
        "The conflicted draft lost the value the gesture set",
    )?;
    ensure(
        revision(&frames[8])? == revision(&frames[7])? + 1,
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
        json!({"notices": conflict_notices, "draft": draft(&frames[8]), "revision": revision(&frames[8])?}),
    );

    // Frame 9: Reapply. The draft is rebased on the new revision, its value is re-sent and the
    // drafted preview returns over the committed stack.
    let reapplied = draft(&frames[9]);
    ensure(
        reapplied["conflicted"] == json!(false),
        format!("Reapply did not clear the conflict: {reapplied}"),
    )?;
    ensure(
        reapplied["base_revision"].as_u64() == Some(revision(&frames[9])?),
        format!(
            "The reapplied draft is based on {} while the asset is at {}",
            reapplied["base_revision"],
            revision(&frames[9])?
        ),
    )?;
    ensure(
        reapplied["fields"] == json!({ EXPOSURE: 2.0 }),
        "Reapply lost the field this client set",
    )?;
    ensure(
        notices(&frames[9]).is_empty(),
        format!("Frame 9 still shows a notice: {:?}", notices(&frames[9])),
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
    expect_no_draft(&frames[10], "Frame 10")?;
    ensure(
        revision(&frames[10])? == revision(&frames[9])?
            && entry(&frames[10])? == entry(&frames[9])?,
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
        json!({"revision": revision(&frames[10])?, "mean_luminance": luminance[10]}),
    );

    write_json(
        &evidence.join("basic-checks.json"),
        &json!({
            "checks": checks,
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
pub fn run_restart(root: &Path, out: &Path, bin: &Path, timeout: std::time::Duration) -> Result {
    ensure(!out.exists(), "Smoke output must be new")?;
    fs::create_dir_all(out)?;
    let fixture = root.join(RESTART_FIXTURE);
    let launch1 = out.join("launch1");
    let launch2 = out.join("launch2");
    let mut result = json!({"scenario":"basic-restart","status":"failed","launch_mode":launch::MODE,"platform":format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH)});
    let check = (|| -> Result {
        result["fixture_hash"] = json!(hash(&fixture)?);
        result["binary_sha256"] = json!(hash(bin)?);
        result["lockfile_sha256"] = json!(hash(&root.join("Cargo.lock"))?);

        // Launch 1: open the fixture, commit one Basic patch through the ordinary edit path.
        let script = out.join("script1.json");
        write_json(
            &script,
            &json!([{"api":{"method":"edit.set-basic","params":{"exposure":RESTART_EXPOSURE,"temperature":RESTART_TEMPERATURE}}}]),
        )?;
        let args1: Vec<OsString> = vec![
            "--evidence-dir".into(),
            launch1.clone().into_os_string(),
            "--open".into(),
            fixture.clone().into_os_string(),
            "--evidence-script".into(),
            script.into_os_string(),
            "--window-size".into(),
            workspace_smoke::WINDOW[0].into(),
            workspace_smoke::WINDOW[1].into(),
        ];
        let mut child1 = smoke::spawn_editor(root, bin, &args1, &out.join("launch1.log"))?;
        let status1 = smoke::wait(&mut child1, timeout)?;
        result["launch1_exit_code"] = json!(status1.code());
        ensure(status1.success(), format!("Launch 1 exit {status1}"))?;
        let (app1, _) = smoke::preamble(&launch1, 2)?;
        let frames1 = app1["frames"]
            .as_array()
            .ok_or("Launch 1 wrote no frames")?;
        let opened = &frames1[0];
        let committed = &frames1[1];
        let opened_path = smoke::frame_identity(&launch1, &app1, opened)?;
        let committed_path = smoke::frame_identity(&launch1, &app1, committed)?;
        ensure(
            basic_payload(opened).is_none(),
            "Launch 1 opened with a Basic layer already in the stack",
        )?;
        ensure(
            revision(committed)? == 1,
            format!("Launch 1 committed revision {}", revision(committed)?),
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
        let neutral_luminance = photo_luminance(&opened_path, opened)?;
        let edited_luminance = photo_luminance(&committed_path, committed)?;
        brighter(
            "launch 1's committed edit against its own neutral open",
            edited_luminance,
            neutral_luminance,
        )?;

        // Launch 2: the same catalog and the same file, in a new process. The catalog dedupes the
        // reopened file to the same asset by file identity, so its saved stack comes back.
        let catalog = launch1.join("catalog.sqlite");
        ensure(catalog.is_file(), "Launch 1 wrote no catalog")?;
        let args2: Vec<OsString> = vec![
            "--catalog".into(),
            catalog.into_os_string(),
            "--evidence-dir".into(),
            launch2.clone().into_os_string(),
            "--open".into(),
            fixture.clone().into_os_string(),
            "--window-size".into(),
            workspace_smoke::WINDOW[0].into(),
            workspace_smoke::WINDOW[1].into(),
        ];
        let mut child2 = smoke::spawn_editor(root, bin, &args2, &out.join("launch2.log"))?;
        let status2 = smoke::wait(&mut child2, timeout)?;
        result["launch2_exit_code"] = json!(status2.code());
        ensure(status2.success(), format!("Launch 2 exit {status2}"))?;
        let (app2, _) = smoke::preamble(&launch2, 1)?;
        let reopened = &app2["frames"]
            .as_array()
            .ok_or("Launch 2 wrote no frames")?[0];
        let reopened_path = smoke::frame_identity(&launch2, &app2, reopened)?;
        ensure(
            revision(reopened)? == revision(committed)? && entry(reopened)? == entry(committed)?,
            format!(
                "Launch 2 reopened at revision {} entry {}",
                revision(reopened)?,
                entry(reopened)?
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
            label(reopened)? == label(committed)?,
            format!(
                "Launch 2's history label is {:?}, launch 1 committed {:?}",
                label(reopened)?,
                label(committed)?
            ),
        )?;
        // The photograph itself is the edited one again, to the same measured brightness.
        let reopened_luminance = photo_luminance(&reopened_path, reopened)?;
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
        ensure(
            json!(hash(&fixture)?) == result["fixture_hash"],
            "Source changed",
        )?;
        write_json(
            &out.join("basic-restart-checks.json"),
            &json!({
                "stored_payload": stored,
                "basic_layer": layer,
                "label": label(reopened)?,
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
    })();
    match &check {
        Ok(()) => result["status"] = json!("passed"),
        Err(e) => result["error"] = json!(e.to_string()),
    };
    write_json(&out.join("result.json"), &result)?;
    fs::write(
        out.join("reproduce.md"),
        format!(
            "# Smoke run\n\nScenario: basic-restart. Status: {}.\n\nTwo launches, because a restart cannot be simulated inside one process: the first opens the fixture and commits one `edit.set-basic` patch of exposure and temperature; the second reuses that launch's own catalog (`--catalog <dir1>/catalog.sqlite`) and reopens the same file, which the catalog dedupes to the same asset, so the saved Basic layer, its identity, the slider values, the history label and the rendered brightness all come back.\n\nReproduce with `cargo xtask smoke --scenario basic-restart --output NEW_DIR --binary PATH`.\n",
            result["status"],
        ),
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    check
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
/// match on. Inside the photo surface, between the notices at the top of the canvas and the
/// floating mode strip at its bottom, the only thing brighter than the canvas surface (`#19191b`)
/// and the bars over it (`#232326`) is the photograph itself, so its bounding box is the bright
/// pixels of that band. The scenario draws no notice, which `verify_panel` checks per frame.
fn photo_bounds(path: &Path, frame: &Value) -> Result<Value> {
    /// Well above the brightest chrome in the band and well below the fixture's darkest grey.
    const BRIGHT: u32 = 60;
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [surface_left, surface_right] = columns(frame)?.unwrap_or([0, width]);
    ensure(
        surface_left < surface_right && surface_right <= width,
        "Invalid surface columns",
    )?;
    let (band_top, band_bottom) = (height * 3 / 20, height * 22 / 25);
    // The 1 px dividers at the surface's own edges are 6% white over the panel, which is as bright
    // as this fixture's darkest grey, so the scan starts inside them.
    const INSET: u32 = 4;
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0, 0);
    let mut found = 0u32;
    for y in band_top..band_bottom {
        for x in (surface_left + INSET)..(surface_right - INSET) {
            let pixel = image.get_pixel(x, y).0;
            let mean = (u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2])) / 3;
            if mean >= BRIGHT {
                found += 1;
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    ensure(
        found > 0,
        "No photograph in the frame: blank or wrong render",
    )?;
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

/// The mean per-channel value of the same centred window [`photo_luminance`] reads.
fn photo_channels(path: &Path, frame: &Value) -> Result<[f64; 3]> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [left, right] = columns(frame)?.unwrap_or([0, width]);
    ensure(left < right && right <= width, "Invalid surface columns")?;
    let surface = right - left;
    let (cx, cy) = ((left + right) / 2, height / 2);
    let (half_w, half_h) = (surface / 5, height * 3 / 20);
    ensure(
        half_w > 10 && half_h > 10 && cx > half_w && cy > half_h,
        "Photo surface too small to sample",
    )?;
    let mut totals = [0.0; 3];
    let mut count = 0u32;
    for y in (cy - half_h)..(cy + half_h) {
        for x in (cx - half_w)..(cx + half_w) {
            let p = image.get_pixel(x, y).0;
            for channel in 0..3 {
                totals[channel] += f64::from(p[channel]);
            }
            count += 1;
        }
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(totals.map(|total| total / f64::from(count)))
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
fn basic_field<'a>(frame: &'a Value, name: &str) -> Result<&'a str> {
    frame["state"]["controls"][format!("{SET_BASIC}.{name}")]
        .as_str()
        .ok_or_else(|| format!("Frame records no {name} field").into())
}

/// The one Basic layer's identity, so evidence can prove an edit updated it in place.
fn basic_layer_id(frame: &Value) -> Option<&str> {
    frame["state"]["stack"]["layers"]
        .as_array()?
        .iter()
        .find(|layer| layer["effect"] == json!(lightwell_core::BASIC_EFFECT))
        .and_then(|layer| layer["id"].as_str())
}

fn status(frame: &Value) -> Result<&str> {
    frame["state"]["status"]
        .as_str()
        .ok_or_else(|| "Frame records no status".into())
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
            {"pick":{"x":CLIPPED_PICK[0],"y":CLIPPED_PICK[1]}},
            // The default screen the Module panels density is accepted on: Basic expanded and every
            // other section collapsed to its band (Transforms and Crop open by default today).
            {"section":{"module":TRANSFORM_MODULE,"expanded":false}},
            {"section":{"module":CROP_MODULE,"expanded":false}}
        ])),
        _ => None,
    }
}

pub fn verify_panel(evidence: &Path, app: &Value, _events: &[Value]) -> Result {
    let frames = app["frames"].as_array().ok_or("Missing frames")?.clone();
    ensure(
        app["had_input_errors"] == json!(false),
        "The run recorded an input error",
    )?;
    let paths: Vec<PathBuf> = frames
        .iter()
        .map(|frame| frame_identity(evidence, app, frame))
        .collect::<Result<Vec<_>>>()?;
    let channels: Vec<[f64; 3]> = frames
        .iter()
        .zip(&paths)
        .map(|(frame, path)| photo_channels(path, frame))
        .collect::<Result<Vec<_>>>()?;
    let luminance: Vec<f64> = frames
        .iter()
        .zip(&paths)
        .map(|(frame, path)| photo_luminance(path, frame))
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
    // No frame in this scenario draws a notice, which is what lets `photo_bounds` read the band
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
            "placement": photo_bounds(&paths[0], &frames[0])?,
            "fields": listed,
            "red_minus_blue": balance(channels[0]),
        }),
    );

    // Frame 1: a Temperature drag to +40, released. One entry, one revision, and the neutral
    // fixture is visibly warmer.
    expect_no_draft(&frames[1], "Frame 1")?;
    ensure(
        revision(&frames[1])? == revision(&frames[0])? + 1,
        "The released drag did not advance the revision by one",
    )?;
    ensure(
        label(&frames[1])? == "Temperature +40",
        format!("The committed entry is labelled {:?}", label(&frames[1])?),
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
        json!({"label": label(&frames[1])?, "red_minus_blue": balance(channels[1]), "layer": identity}),
    );

    // Frame 2: Vibrance typed and committed with Enter. The patch merges into the same layer.
    ensure(
        revision(&frames[2])? == revision(&frames[1])? + 1,
        "Enter in the value field did not commit exactly one revision",
    )?;
    ensure(
        label(&frames[2])? == "Vibrance +25",
        format!("The typed entry is labelled {:?}", label(&frames[2])?),
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
        json!({"label": label(&frames[2])?, "payload": basic_payload(&frames[2])}),
    );

    // Frame 3: the Temperature entry previewed. The sliders show that entry's own saved values,
    // which are not the current ones, and nothing is committed.
    ensure(
        status(&frames[3])?.starts_with("Previewing entry 1"),
        format!(
            "Frame 3 is not previewing entry 1: {:?}",
            status(&frames[3])?
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
        revision(&frames[3])? == revision(&frames[2])?,
        "Selecting a history entry committed something",
    )?;
    record(
        &frames[3],
        "the Temperature entry previewed: its own saved values in the disabled sliders",
        json!({"status": status(&frames[3])?, "temperature": basic_field(&frames[3], TEMPERATURE)?, "vibrance": basic_field(&frames[3], VIBRANCE)?}),
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
        revision(&frames[4])? == revision(&frames[2])?,
        "Return to current changed the revision",
    )?;
    record(
        &frames[4],
        "Return to current: the current entry's values are shown again",
        json!({"vibrance": basic_field(&frames[4], VIBRANCE)?, "status": status(&frames[4])?}),
    );

    // Frame 5: the Colour group's reset. One entry labelled by the module, the group's own fields
    // neutral, every other field untouched and the layer kept.
    ensure(
        label(&frames[5])? == format!("Reset {COLOUR_GROUP}"),
        format!("The group reset is labelled {:?}", label(&frames[5])?),
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
        json!({"label": label(&frames[5])?, "payload": basic_payload(&frames[5]), "layer": identity}),
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
        status(&frames[6])?.starts_with("Neutral picker"),
        format!("The picker mode says {:?}", status(&frames[6])?),
    )?;
    ensure(
        revision(&frames[6])? == revision(&frames[5])?,
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
            "status": status(&frames[6])?,
            "picker": picker(&frames[6]),
        }),
    );

    // Frame 7: a pick on a neutral grey patch. The picker answers the exact identity, 0 and 0,
    // and that patch is committed once through the ordinary action path, so the warm cast the
    // drag left is gone and the photograph is the opened one again.
    ensure(
        revision(&frames[7])? == revision(&frames[6])? + 1,
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
        label(&frames[7])? == format!("Reset {WHITE_BALANCE_GROUP}"),
        format!("The pick's entry is labelled {:?}", label(&frames[7])?),
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
        json!({"label": label(&frames[7])?, "payload": basic_payload(&frames[7]), "red_minus_blue": balance(channels[7]), "mean_luminance": luminance[7]}),
    );

    // Frame 8: a pick on a clipped patch. The core's own reason leads the status bar and nothing
    // at all is committed.
    ensure(
        status(&frames[8])?.starts_with("clipped:"),
        format!(
            "The refused pick does not lead with its reason: {:?}",
            status(&frames[8])?
        ),
    )?;
    ensure(
        revision(&frames[8])? == revision(&frames[7])? && entry(&frames[8])? == entry(&frames[7])?,
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
        json!({"status": status(&frames[8])?, "revision": revision(&frames[8])?}),
    );

    // Frame 10: Basic expanded and every other section collapsed, the screen on which the Module
    // panels density puts the histogram, Basic and every other section's band on screen at once.
    let expanded = &frames[10]["state"]["expanded"];
    let others_collapsed = expanded
        .as_object()
        .ok_or("Missing expanded sections")?
        .iter()
        .all(|(module, open)| (module == BASIC_MODULE) == (open == &json!(true)));
    ensure(
        others_collapsed && revision(&frames[10])? == revision(&frames[8])?,
        format!("Only Basic should be expanded, with nothing committed: {expanded}"),
    )?;
    record(
        &frames[10],
        "Basic expanded and every other section collapsed to its band",
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
