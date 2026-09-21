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
        _ => None,
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
        exposure_field(&frames[0])? == "0",
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
        exposure_field(&frames[1])? == "1",
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
        exposure_field(&frames[5])? == "1",
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
        exposure_field(&frames[6])? == "0",
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
        exposure_field(&frames[8])? == "2",
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
        exposure_field(&frames[10])? == "0",
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
