//! The `mixer` smoke scenario: the Colour mixer section's own expand, drag, commit, reset and hue
//! rotation, on the real editor.
//!
//! The fixture is a small, deterministically generated hue wheel (`fixtures/generated/hue-wheel.jpg`,
//! built by `cargo xtask generate-fixtures`): the golden orientation fixtures hold only four flat
//! quadrant colours, with no continuous hue range to inspect a mixer hue rotation's continuity
//! across, so this scenario needs its own. Angle 0 on the wheel (its own east point) is pure sRGB
//! red, dead centre of the mixer's own red range; the diametrically opposite point sits in a
//! different colour family entirely, which is what [`patch_mean`] samples at
//! [`OPPOSITE_ANGLE_DEG`] as an unaffected control for a red-hue edit.
use crate::{
    smoke::{columns, frame_identity, longest_run},
    *,
};

const MIXER_MODULE: &str = "lightwell.mixer";
/// The one section the registry lists above the Colour mixer's own that is both a real toggleable
/// section (Pixel declares no expandable section of its own in the tools panel) and expanded by
/// its own descriptor's default: collapsed first, so the module's own sliders and rails are on
/// screen without scrolling.
const BASIC_MODULE: &str = "lightwell.basic";
const SET_MIXER: &str = "set-mixer";
const RED_HUE: &str = "red-hue";
const AQUA_SATURATION: &str = "aqua-saturation";
const SATURATION_GROUP: &str = "Saturation";
pub const FIXTURE: &str = "fixtures/generated/hue-wheel.jpg";

/// The wheel's own east point (angle 0), where the mixer's red range is centred.
const RED_ANGLE_DEG: f64 = 0.0;
/// Diametrically opposite red: a different colour family the red-hue slider should barely touch.
const OPPOSITE_ANGLE_DEG: f64 = 180.0;
/// How far into the wheel's own radius a sample patch sits, safely clear of the centre and the
/// anti-aliased rim.
const SAMPLE_RADIUS_FRACTION: f64 = 0.6;
/// Half the side length, in pixels, of a sampled patch.
const PATCH_HALF: i64 = 4;

/// How far a patch's mean channel values must move before this scenario calls it changed. The
/// measured moves are tens of codes wide, so this is a wide margin, not a threshold tuning could
/// slip past.
const CHANGED: f64 = 20.0;
/// How close two mean channel readings must stay before this scenario calls a patch unaffected.
const UNCHANGED: f64 = 10.0;

/// One open frame plus one per script step.
pub fn frames(scenario: &str) -> Option<usize> {
    (scenario == "mixer").then_some(10)
}

pub fn source(scenario: &str) -> Option<&'static str> {
    (scenario == "mixer").then_some(FIXTURE)
}

/// The evidence script. Each step is one gesture, one request or one decision; `verify` below
/// checks exactly what each one is supposed to prove.
pub fn script(scenario: &str) -> Option<Value> {
    (scenario == "mixer").then(|| {
        json!([
            // 1: collapse Basic, expanded by its own default and the one other section the
            // registry lists above Colour mixer, so the module's own sliders and rails land on
            // screen without scrolling once it expands.
            {"section":{"module":BASIC_MODULE,"expanded":false}},
            // 2: expand the section. Hue starts expanded by the module's own descriptor, so this
            // alone exposes its eight rails.
            {"section":{"module":MIXER_MODULE,"expanded":true}},
            // 3: a drag on Red hue, left open: the frame shows the drafted preview.
            {"slider":{"action":SET_MIXER,"parameter":RED_HUE,"values":[30.0,60.0,90.0]}},
            // 4: the same gesture released: one entry, one revision, committed at Fit.
            {"slider":{"action":SET_MIXER,"parameter":RED_HUE,"values":[90.0],"release":true}},
            // 5: the same committed state at 100%.
            {"view":{"zoom":"100"}},
            // 6: a Saturation field, so its group's reset below has something to undo.
            {"field":{"action":SET_MIXER,"parameter":AQUA_SATURATION,"text":"-40","submit":true}},
            // 7: the Saturation group's own reset, leaving the Hue field alone.
            {"reset":{"module":MIXER_MODULE,"group":SATURATION_GROUP}},
            // 8: a stronger hue shift, still at 100%, where continuity across the wheel shows.
            {"slider":{"action":SET_MIXER,"parameter":RED_HUE,"values":[100.0],"release":true}},
            // 9: the Saturation tab: the mixer's groups are tabs, and choosing one is view state.
            {"tab":{"module":MIXER_MODULE,"index":1}}
        ])
    })
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

fn draft(frame: &Value) -> &Value {
    &frame["state"]["draft"]
}

fn expect_no_draft(frame: &Value, what: &str) -> Result {
    ensure(
        draft(frame) == &Value::Null,
        format!("{what}: a draft is still open: {}", draft(frame)),
    )
}

fn section_expanded(frame: &Value, module: &str) -> bool {
    frame["state"]["expanded"][module] == json!(true)
}

/// The expanded state of every section this scenario toggles, for the correlation every recorded
/// frame carries alongside its revision, entry and draft.
fn expanded_sections(frame: &Value) -> Value {
    json!({
        BASIC_MODULE: section_expanded(frame, BASIC_MODULE),
        MIXER_MODULE: section_expanded(frame, MIXER_MODULE),
    })
}

/// The stack's one Colour mixer layer's payload, or `None` when the stack holds no mixer layer.
fn mixer_payload(frame: &Value) -> Option<&Value> {
    frame["state"]["stack"]["layers"]
        .as_array()?
        .iter()
        .find(|layer| layer["effect"] == json!(lightwell_core::MIXER_EFFECT))
        .map(|layer| &layer["payload"])
}

fn mixer_layer_id(frame: &Value) -> Option<&str> {
    frame["state"]["stack"]["layers"]
        .as_array()?
        .iter()
        .find(|layer| layer["effect"] == json!(lightwell_core::MIXER_EFFECT))
        .and_then(|layer| layer["id"].as_str())
}

/// What one generated mixer field showed when the frame was captured.
fn mixer_field<'a>(frame: &'a Value, name: &str) -> Result<&'a str> {
    frame["state"]["controls"][format!("{SET_MIXER}.{name}")]
        .as_str()
        .ok_or_else(|| format!("Frame records no {name} field").into())
}

/// Where the wheel is drawn: found by the row with the widest run of saturated pixels (an
/// accurate horizontal extent no title-bar icon or coloured control rail can win, since none of
/// them spans as wide as the wheel itself) and the column with the tallest run (the same, for the
/// vertical extent). This works whether the zoom centres the photograph (Fit) or anchors it to the
/// photo surface's own top-left corner, which 100% does for a wheel smaller than the canvas.
fn wheel_bounds(path: &Path, frame: &Value) -> Result<[u32; 4]> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [surface_left, surface_right] = columns(frame)?.unwrap_or([0, width]);
    ensure(
        surface_left < surface_right && surface_right <= width,
        "Invalid surface columns",
    )?;
    ensure(
        surface_right - surface_left > 20,
        "Photo surface too narrow to inset from its own edge dividers",
    )?;
    let saturated = |p: [u8; 3]| p.iter().max().unwrap() - p.iter().min().unwrap() >= 40;
    // Clear of the title bar above and the mode strip / status line below, and of the divider that
    // marks each edge of the photo surface itself, so none of those is ever scanned as a row or a
    // column: the divider is a thin, full-height line exactly at the surface's own boundary.
    let top_margin = (height / 20).max(20);
    let bottom_margin = height - top_margin;
    let side_inset = 10;
    let (inset_left, inset_right) = (surface_left + side_inset, surface_right - side_inset);
    let mut widest: Option<(u32, u32, u32)> = None;
    for y in top_margin..bottom_margin {
        let run =
            longest_run((inset_left..inset_right).map(|x| (x, saturated(image.get_pixel(x, y).0))));
        if let Some((left, right)) = run
            && widest.is_none_or(|(w, ..)| right - left > w)
        {
            widest = Some((right - left, left, right));
        }
    }
    let (_, left, right) = widest.ok_or("No saturated wheel content in any row")?;
    let mut tallest: Option<(u32, u32, u32)> = None;
    for x in inset_left..inset_right {
        let run = longest_run(
            (top_margin..bottom_margin).map(|y| (y, saturated(image.get_pixel(x, y).0))),
        );
        if let Some((top, bottom)) = run
            && tallest.is_none_or(|(h, ..)| bottom - top > h)
        {
            tallest = Some((bottom - top, top, bottom));
        }
    }
    let (_, top, bottom) = tallest.ok_or("No saturated wheel content in any column")?;
    Ok([left, top, right, bottom])
}

/// The mean RGB of a small patch at `angle_deg` around the wheel's own centre, `SAMPLE_RADIUS_FRACTION`
/// of its radius out — the same fractional geometry [`crate::fixtures::hue_wheel`] (private to that
/// module, reproduced here as plain trigonometry) draws the wheel with, so the angle a mixer range is
/// centred on and the angle sampled here agree regardless of Fit/100% scale or the photo surface's
/// own offset.
fn patch_mean(path: &Path, bounds: [u32; 4], angle_deg: f64) -> Result<[f64; 3]> {
    let image = image::open(path)?.to_rgb8();
    let [left, top, right, bottom] = bounds;
    let (cx, cy) = (f64::from(left + right) / 2.0, f64::from(top + bottom) / 2.0);
    let radius = f64::from((right - left).min(bottom - top)) / 2.0;
    let angle = angle_deg.to_radians();
    let (px, py) = (
        cx + SAMPLE_RADIUS_FRACTION * radius * angle.cos(),
        cy + SAMPLE_RADIUS_FRACTION * radius * angle.sin(),
    );
    let (width, height) = image.dimensions();
    let mut totals = [0.0; 3];
    let mut count = 0u32;
    for dy in -PATCH_HALF..=PATCH_HALF {
        for dx in -PATCH_HALF..=PATCH_HALF {
            let x = (px as i64 + dx).clamp(0, i64::from(width) - 1) as u32;
            let y = (py as i64 + dy).clamp(0, i64::from(height) - 1) as u32;
            let p = image.get_pixel(x, y).0;
            for (total, channel) in totals.iter_mut().zip(p) {
                *total += f64::from(channel);
            }
            count += 1;
        }
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(totals.map(|total| total / f64::from(count)))
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(a, b)| (a - b).abs()).sum::<f64>()
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
    let mut checks = Vec::new();
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };

    // Frame 0: the fixture opens with Basic expanded above it (its own descriptor default), the
    // mixer section listed and collapsed, every field at its default, no draft and no mixer layer
    // yet.
    let mixer = frames[0]["state"]["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|module| module["id"] == json!(MIXER_MODULE))
        .ok_or("The Colour mixer module is not listed at all")?;
    ensure(
        mixer["available"] == json!(true),
        "The Colour mixer module is not available",
    )?;
    ensure(
        !section_expanded(&frames[0], MIXER_MODULE),
        "The Colour mixer section is not collapsed as launched",
    )?;
    ensure(
        mixer_field(&frames[0], RED_HUE)? == "0",
        format!(
            "Red hue does not start neutral: {}",
            mixer_field(&frames[0], RED_HUE)?
        ),
    )?;
    expect_no_draft(&frames[0], "Frame 0")?;
    ensure(
        mixer_payload(&frames[0]).is_none(),
        "The opened stack already holds a Colour mixer layer",
    )?;
    let opened_bounds = wheel_bounds(&paths[0], &frames[0])?;
    let opened_red = patch_mean(&paths[0], opened_bounds, RED_ANGLE_DEG)?;
    let opened_opposite = patch_mean(&paths[0], opened_bounds, OPPOSITE_ANGLE_DEG)?;
    record(
        &frames[0],
        "the collapsed Colour mixer section as launched, the wheel at its opened colours",
        json!({"red_patch": opened_red, "opposite_patch": opened_opposite, "expanded": expanded_sections(&frames[0])}),
    );

    // Frame 1: Basic collapsed, so nothing above Colour mixer is expanded once it opens.
    ensure(
        !section_expanded(&frames[1], BASIC_MODULE),
        "The section step did not collapse Basic",
    )?;
    ensure(
        revision(&frames[1])? == revision(&frames[0])?,
        "Collapsing Basic committed something",
    )?;
    record(
        &frames[1],
        "the Basic section collapsed, above Colour mixer in the registry order",
        json!({"expanded": expanded_sections(&frames[1])}),
    );

    // Frame 2: the section expanded, with Basic still collapsed above it. Hue starts expanded by
    // the module's own descriptor, so its eight rails are visible without a further group step or
    // any scrolling.
    ensure(
        section_expanded(&frames[2], MIXER_MODULE) && !section_expanded(&frames[2], BASIC_MODULE),
        format!(
            "The section step did not expand Colour mixer alone: {}",
            expanded_sections(&frames[2])
        ),
    )?;
    ensure(
        revision(&frames[2])? == revision(&frames[1])?,
        "Expanding the section committed something",
    )?;
    record(
        &frames[2],
        "the Colour mixer section expanded: the Hue group and its eight rails, on screen with nothing above it expanded",
        json!({"expanded": expanded_sections(&frames[2])}),
    );

    // Frame 3: mid-gesture at Red hue +90. The draft is open, nothing is committed, and the red
    // patch has visibly moved while the opposite patch has not.
    let drafted = draft(&frames[3]);
    ensure(
        drafted["action"] == json!(SET_MIXER)
            && drafted["fields"] == json!({ RED_HUE: 90.0 })
            && drafted["conflicted"] == json!(false),
        format!("Frame 3's draft is not the open Red hue gesture: {drafted}"),
    )?;
    ensure(
        drafted["draft_revision"]
            .as_u64()
            .is_some_and(|value| value >= 1),
        format!("Frame 3's draft carries no draft revision: {drafted}"),
    )?;
    ensure(
        frames[3]["state"]["displayed_draft_revision"] == drafted["draft_revision"],
        format!(
            "Frame 3 displays draft revision {} while the draft is at {}",
            frames[3]["state"]["displayed_draft_revision"], drafted["draft_revision"]
        ),
    )?;
    ensure(
        revision(&frames[3])? == revision(&frames[2])? && mixer_payload(&frames[3]).is_none(),
        "A drag committed something",
    )?;
    ensure(
        mixer_field(&frames[3], RED_HUE)? == "90",
        format!(
            "The slider does not show the drafted value: {}",
            mixer_field(&frames[3], RED_HUE)?
        ),
    )?;
    let drafted_bounds = wheel_bounds(&paths[3], &frames[3])?;
    let drafted_red = patch_mean(&paths[3], drafted_bounds, RED_ANGLE_DEG)?;
    let drafted_opposite = patch_mean(&paths[3], drafted_bounds, OPPOSITE_ANGLE_DEG)?;
    ensure(
        distance(drafted_red, opened_red) > CHANGED,
        format!(
            "+90 red hue drafted did not move the red patch: {drafted_red:?} against {opened_red:?}"
        ),
    )?;
    ensure(
        distance(drafted_opposite, opened_opposite) < UNCHANGED,
        format!(
            "+90 red hue drafted moved the opposite patch it should leave alone: {drafted_opposite:?} against {opened_opposite:?}"
        ),
    )?;
    record(
        &frames[3],
        "a drag to Red hue +90, mid-gesture: the drafted preview, nothing committed, Colour mixer still the only expanded section",
        json!({"draft": drafted, "red_patch": drafted_red, "opposite_patch": drafted_opposite, "expanded": expanded_sections(&frames[3])}),
    );

    // Frame 4: the release. One entry, labelled by the module, the revision advanced by one,
    // displayed at Fit.
    expect_no_draft(&frames[4], "Frame 4")?;
    ensure(
        revision(&frames[4])? == revision(&frames[3])? + 1,
        format!(
            "The release advanced the revision from {} to {}, expected one step",
            revision(&frames[3])?,
            revision(&frames[4])?
        ),
    )?;
    ensure(
        entry(&frames[4])? != entry(&frames[2])?,
        "The release created no new history entry",
    )?;
    ensure(
        label(&frames[4])? == "Red hue +90",
        format!("The committed entry is labelled {:?}", label(&frames[4])?),
    )?;
    ensure(
        mixer_payload(&frames[4]) == Some(&json!({ RED_HUE: 90.0 })),
        format!(
            "The committed Colour mixer layer holds {:?}",
            mixer_payload(&frames[4])
        ),
    )?;
    let layer = mixer_layer_id(&frames[4])
        .ok_or("The committed stack holds no Colour mixer layer")?
        .to_owned();
    let committed_fit_bounds = wheel_bounds(&paths[4], &frames[4])?;
    let committed_fit_red = patch_mean(&paths[4], committed_fit_bounds, RED_ANGLE_DEG)?;
    record(
        &frames[4],
        "released: one entry \"Red hue +90\", displayed at Fit",
        json!({"revision": revision(&frames[4])?, "label": label(&frames[4])?, "red_patch": committed_fit_red, "layer": layer, "expanded": expanded_sections(&frames[4])}),
    );

    // Frame 5: the same committed state at 100%. Nothing changed but the zoom.
    expect_no_draft(&frames[5], "Frame 5")?;
    ensure(
        revision(&frames[5])? == revision(&frames[4])? && entry(&frames[5])? == entry(&frames[4])?,
        "Changing zoom committed something",
    )?;
    ensure(
        frames[5]["step"]["request"] == json!({"view":{"zoom":100.0}}),
        format!(
            "Frame 5 did not request 100%: {}",
            frames[5]["step"]["request"]
        ),
    )?;
    let committed_100_bounds = wheel_bounds(&paths[5], &frames[5])?;
    let committed_100_red = patch_mean(&paths[5], committed_100_bounds, RED_ANGLE_DEG)?;
    ensure(
        distance(committed_100_red, committed_fit_red) < UNCHANGED,
        "The same committed edit reads differently at Fit and at 100%",
    )?;
    record(
        &frames[5],
        "the same committed Red hue +90, at 100%",
        json!({"red_patch": committed_100_red, "zoom_request": frames[5]["step"]["request"], "expanded": expanded_sections(&frames[5])}),
    );

    // Frame 6: a Saturation field, typed and submitted. The same layer merges the second field.
    ensure(
        revision(&frames[6])? == revision(&frames[5])? + 1,
        "Enter in the value field did not commit exactly one revision",
    )?;
    ensure(
        label(&frames[6])? == "Aqua saturation -40",
        format!("The typed entry is labelled {:?}", label(&frames[6])?),
    )?;
    ensure(
        mixer_payload(&frames[6]) == Some(&json!({ RED_HUE: 90.0, AQUA_SATURATION: -40.0 })),
        format!(
            "The merged mixer layer holds {:?}",
            mixer_payload(&frames[6])
        ),
    )?;
    ensure(
        mixer_layer_id(&frames[6]) == Some(layer.as_str()),
        "The typed value replaced the Colour mixer layer instead of updating it",
    )?;
    record(
        &frames[6],
        "Aqua saturation typed as -40 and committed with Enter: the same layer, both fields",
        json!({"label": label(&frames[6])?, "payload": mixer_payload(&frames[6]), "expanded": expanded_sections(&frames[6])}),
    );

    // Frame 7: the Saturation group's own reset. One entry labelled by the module, that group's
    // field back to neutral, Red hue untouched, the layer kept.
    ensure(
        label(&frames[7])? == format!("Reset {SATURATION_GROUP}"),
        format!("The group reset is labelled {:?}", label(&frames[7])?),
    )?;
    ensure(
        mixer_payload(&frames[7]) == Some(&json!({ RED_HUE: 90.0 })),
        format!(
            "The Saturation reset changed more than its own group: {:?}",
            mixer_payload(&frames[7])
        ),
    )?;
    ensure(
        mixer_layer_id(&frames[7]) == Some(layer.as_str()),
        "The group reset replaced the Colour mixer layer instead of updating it",
    )?;
    ensure(
        mixer_field(&frames[7], AQUA_SATURATION)? == "0"
            && mixer_field(&frames[7], RED_HUE)? == "90",
        "The group reset did not leave Hue alone",
    )?;
    // Saturation starts collapsed, so its own group is off screen; the group reset button lives
    // on the group header and runs the same way whether or not that group is expanded.
    record(
        &frames[7],
        "the Saturation group reset: one entry, that field neutral, Hue untouched",
        json!({"label": label(&frames[7])?, "payload": mixer_payload(&frames[7]), "layer": layer, "expanded": expanded_sections(&frames[7])}),
    );

    // Frame 8: a stronger hue shift to +100, still at 100%, where hue continuity across the wheel
    // can be inspected: the red patch has moved further still and the opposite patch stays clear.
    expect_no_draft(&frames[8], "Frame 8")?;
    ensure(
        revision(&frames[8])? == revision(&frames[7])? + 1,
        "The final release did not commit exactly one revision",
    )?;
    ensure(
        label(&frames[8])? == "Red hue +100",
        format!("The final entry is labelled {:?}", label(&frames[8])?),
    )?;
    ensure(
        mixer_payload(&frames[8]) == Some(&json!({ RED_HUE: 100.0 })),
        format!(
            "The final mixer layer holds {:?}",
            mixer_payload(&frames[8])
        ),
    )?;
    let final_bounds = wheel_bounds(&paths[8], &frames[8])?;
    let final_red = patch_mean(&paths[8], final_bounds, RED_ANGLE_DEG)?;
    let final_opposite = patch_mean(&paths[8], final_bounds, OPPOSITE_ANGLE_DEG)?;
    // The bounded rotation angle need not still be moving noticeably between +90 and +100 this
    // close to its own bound, so this checks the shift against the true opened baseline again,
    // not against the +90 frame; the wheel itself is where a reader inspects hue continuity.
    ensure(
        distance(final_red, opened_red) > CHANGED,
        format!("+100 red hue did not move the red patch: {final_red:?} against {opened_red:?}"),
    )?;
    ensure(
        distance(final_opposite, opened_opposite) < UNCHANGED,
        format!(
            "+100 red hue moved the opposite patch it should leave alone: {final_opposite:?} against {opened_opposite:?}"
        ),
    )?;
    ensure(
        section_expanded(&frames[8], MIXER_MODULE) && !section_expanded(&frames[8], BASIC_MODULE),
        format!(
            "Colour mixer is not still the only expanded section above it: {}",
            expanded_sections(&frames[8])
        ),
    )?;
    record(
        &frames[8],
        "a strong hue shift to Red hue +100 at 100%: hue continuity across the wheel is inspected here, with the Colour mixer sliders and rails still on screen",
        json!({"red_patch": final_red, "opposite_patch": final_opposite, "expanded": expanded_sections(&frames[8])}),
    );

    // Frame 9: the Saturation tab selected. It is per-client view state: no entry, no revision.
    let selected = &frames[9]["state"]["control_ui"]["selected_tab"][MIXER_MODULE];
    ensure(
        selected == &json!(1) && revision(&frames[9])? == revision(&frames[8])?,
        format!("The Saturation tab was not selected as view state alone: {selected}"),
    )?;
    record(
        &frames[9],
        "the Saturation tab selected: its eight rails shown under the tab row, nothing committed",
        json!({"selected_tab": selected}),
    );

    write_json(
        &evidence.join("mixer-checks.json"),
        &json!({
            "checks": checks,
            "changed_margin": CHANGED,
            "unchanged_tolerance": UNCHANGED,
            "sample_geometry": {"red_angle_deg": RED_ANGLE_DEG, "opposite_angle_deg": OPPOSITE_ANGLE_DEG, "radius_fraction": SAMPLE_RADIUS_FRACTION},
            "scope": "Mean RGB of small patches on the hue wheel, read back from the renderer; a hue-continuity and range-locality demonstration, not a colorimetric claim",
        }),
    )?;
    Ok(())
}
