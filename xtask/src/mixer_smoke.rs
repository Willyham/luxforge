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
    scenario::{Frame, pixels},
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
            {"tab":{"module":MIXER_MODULE,"index":1}},
            // 10: the Luminance tab, the last of the three.
            {"tab":{"module":MIXER_MODULE,"index":2}}
        ])
    })
}

/// Every section this scenario toggles, for the correlation every recorded
/// frame carries alongside its revision, entry and draft.
const SECTIONS: [&str; 2] = [BASIC_MODULE, MIXER_MODULE];

fn mixer_payload(frame: &Frame) -> Option<&Value> {
    frame.payload(lightwell_core::MIXER_EFFECT)
}

fn mixer_layer_id(frame: &Frame) -> Option<&str> {
    frame.layer_id(lightwell_core::MIXER_EFFECT)
}

/// What one generated mixer field showed when the frame was captured.
fn mixer_field<'a>(frame: &'a Frame, name: &str) -> Result<&'a str> {
    frame.field(SET_MIXER, name)
}

/// Where the wheel is drawn, measured across it rather than down it. The mode strip floats over the
/// bottom of the canvas, so the wheel's own vertical extent is not on screen to be scanned for: at
/// Fit a square fixture fills the canvas and its lowest rows are behind the strip, and at 100% the
/// strip stands clear of a wheel smaller than the canvas but carries a saturated pill of its own
/// that a scan down the frame can join to the wheel. Scanning across it instead needs neither, since
/// the fixture is a disc centred in a square: each row's span of saturated pixels — first to last,
/// not the longest unbroken run, because the wheel's own centre is desaturated and splits every row
/// that crosses it — is widest exactly across the wheel's diameter, the rows that attain that width
/// straddle its centre row, and the diameter is its extent in both axes. This reads the same wheel
/// whatever the chrome around it does, and whether the zoom centres the photograph (Fit) or anchors
/// it to the photo surface's own top-left corner, which 100% does for a wheel smaller than the
/// canvas.
fn wheel_bounds(frame: &Frame) -> Result<[u32; 4]> {
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let [surface_left, surface_right] = frame.columns()?.unwrap_or([0, width]);
    ensure(
        surface_left < surface_right && surface_right <= width,
        "Invalid surface columns",
    )?;
    ensure(
        surface_right - surface_left > 20,
        "Photo surface too narrow to inset from its own edge dividers",
    )?;
    let saturated = |p: [u8; 3]| p.iter().max().unwrap() - p.iter().min().unwrap() >= 40;
    // Clear of the title bar above and the status line below, and of the divider that marks each
    // edge of the photo surface itself: the divider is a thin, full-height line exactly at the
    // surface's own boundary, and a photograph at 100% is drawn flush against it, so the inset
    // clears the divider without eating into the wheel it is measuring.
    let top_margin = (height / 20).max(20);
    let bottom_margin = height - top_margin;
    let side_inset = 2;
    let (inset_left, inset_right) = (surface_left + side_inset, surface_right - side_inset);
    let span = |y: u32| -> Option<(u32, u32)> {
        let mut first = None;
        let mut last = None;
        for x in inset_left..inset_right {
            if saturated(image.get_pixel(x, y).0) {
                first.get_or_insert(x);
                last = Some(x);
            }
        }
        Some((first?, last?))
    };
    let mut diameter = None;
    let (mut left, mut right, mut first_row, mut last_row) = (0, 0, 0, 0);
    for y in top_margin..bottom_margin {
        let Some((row_left, row_right)) = span(y) else {
            continue;
        };
        let across = row_right - row_left;
        if diameter.is_none_or(|widest| across > widest) {
            diameter = Some(across);
            (left, right, first_row, last_row) = (row_left, row_right, y, y);
        } else if diameter == Some(across) {
            (left, right, last_row) = (left.min(row_left), right.max(row_right), y);
        }
    }
    let diameter = diameter.ok_or("No saturated wheel content in any row")?;
    // The rows that span the whole diameter straddle the centre row, so their own midpoint is it.
    let top = (first_row + last_row)
        .checked_sub(diameter)
        .ok_or("The wheel runs off the top of the frame")?
        / 2;
    Ok([left, top, right, top + diameter])
}

/// The mean RGB of a small patch at `angle_deg` around the wheel's own centre, `SAMPLE_RADIUS_FRACTION`
/// of its radius out — the same fractional geometry [`crate::fixtures::hue_wheel`] (private to that
/// module, reproduced here as plain trigonometry) draws the wheel with, so the angle a mixer range is
/// centred on and the angle sampled here agree regardless of Fit/100% scale or the photo surface's
/// own offset.
fn patch_mean(frame: &Frame, bounds: [u32; 4], angle_deg: f64) -> Result<[f64; 3]> {
    let [left, top, right, bottom] = bounds;
    let (cx, cy) = (f64::from(left + right) / 2.0, f64::from(top + bottom) / 2.0);
    let radius = f64::from((right - left).min(bottom - top)) / 2.0;
    let angle = angle_deg.to_radians();
    let (px, py) = (
        cx + SAMPLE_RADIUS_FRACTION * radius * angle.cos(),
        cy + SAMPLE_RADIUS_FRACTION * radius * angle.sin(),
    );
    pixels::mean_rgb(frame.image()?, (px, py), PATCH_HALF)
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(a, b)| (a - b).abs()).sum::<f64>()
}

pub fn verify(evidence: &Path, app: &Value, _events: &[Value]) -> Result {
    ensure(
        app["had_input_errors"] == json!(false),
        "The run recorded an input error",
    )?;
    let frames = Frame::all(evidence, app)?;
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
        !frames[0].section_expanded(MIXER_MODULE),
        "The Colour mixer section is not collapsed as launched",
    )?;
    ensure(
        mixer_field(&frames[0], RED_HUE)? == "0",
        format!(
            "Red hue does not start neutral: {}",
            mixer_field(&frames[0], RED_HUE)?
        ),
    )?;
    frames[0].expect_no_draft("Frame 0")?;
    ensure(
        mixer_payload(&frames[0]).is_none(),
        "The opened stack already holds a Colour mixer layer",
    )?;
    let opened_bounds = wheel_bounds(&frames[0])?;
    let opened_red = patch_mean(&frames[0], opened_bounds, RED_ANGLE_DEG)?;
    let opened_opposite = patch_mean(&frames[0], opened_bounds, OPPOSITE_ANGLE_DEG)?;
    record(
        &frames[0],
        "the collapsed Colour mixer section as launched, the wheel at its opened colours",
        json!({"red_patch": opened_red, "opposite_patch": opened_opposite, "expanded": frames[0].expanded_sections(&SECTIONS)}),
    );

    // Frame 1: Basic collapsed, so nothing above Colour mixer is expanded once it opens.
    ensure(
        !frames[1].section_expanded(BASIC_MODULE),
        "The section step did not collapse Basic",
    )?;
    ensure(
        frames[1].revision()? == frames[0].revision()?,
        "Collapsing Basic committed something",
    )?;
    record(
        &frames[1],
        "the Basic section collapsed, above Colour mixer in the registry order",
        json!({"expanded": frames[1].expanded_sections(&SECTIONS)}),
    );

    // Frame 2: the section expanded, with Basic still collapsed above it. Hue starts expanded by
    // the module's own descriptor, so its eight rails are visible without a further group step or
    // any scrolling.
    ensure(
        frames[2].section_expanded(MIXER_MODULE) && !frames[2].section_expanded(BASIC_MODULE),
        format!(
            "The section step did not expand Colour mixer alone: {}",
            frames[2].expanded_sections(&SECTIONS)
        ),
    )?;
    ensure(
        frames[2].revision()? == frames[1].revision()?,
        "Expanding the section committed something",
    )?;
    record(
        &frames[2],
        "the Colour mixer section expanded: the Hue group and its eight rails, on screen with nothing above it expanded",
        json!({"expanded": frames[2].expanded_sections(&SECTIONS)}),
    );

    // Frame 3: mid-gesture at Red hue +90. The draft is open, nothing is committed, and the red
    // patch has visibly moved while the opposite patch has not.
    let drafted = frames[3].draft();
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
        frames[3].revision()? == frames[2].revision()? && mixer_payload(&frames[3]).is_none(),
        "A drag committed something",
    )?;
    ensure(
        mixer_field(&frames[3], RED_HUE)? == "90",
        format!(
            "The slider does not show the drafted value: {}",
            mixer_field(&frames[3], RED_HUE)?
        ),
    )?;
    let drafted_bounds = wheel_bounds(&frames[3])?;
    let drafted_red = patch_mean(&frames[3], drafted_bounds, RED_ANGLE_DEG)?;
    let drafted_opposite = patch_mean(&frames[3], drafted_bounds, OPPOSITE_ANGLE_DEG)?;
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
        json!({"draft": drafted, "red_patch": drafted_red, "opposite_patch": drafted_opposite, "expanded": frames[3].expanded_sections(&SECTIONS)}),
    );

    // Frame 4: the release. One entry, labelled by the module, the revision advanced by one,
    // displayed at Fit.
    frames[4].expect_no_draft("Frame 4")?;
    ensure(
        frames[4].revision()? == frames[3].revision()? + 1,
        format!(
            "The release advanced the revision from {} to {}, expected one step",
            frames[3].revision()?,
            frames[4].revision()?
        ),
    )?;
    ensure(
        frames[4].entry()? != frames[2].entry()?,
        "The release created no new history entry",
    )?;
    ensure(
        frames[4].label()? == "Red hue +90",
        format!("The committed entry is labelled {:?}", frames[4].label()?),
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
    let committed_fit_bounds = wheel_bounds(&frames[4])?;
    let committed_fit_red = patch_mean(&frames[4], committed_fit_bounds, RED_ANGLE_DEG)?;
    record(
        &frames[4],
        "released: one entry \"Red hue +90\", displayed at Fit",
        json!({"revision": frames[4].revision()?, "label": frames[4].label()?, "red_patch": committed_fit_red, "layer": layer, "expanded": frames[4].expanded_sections(&SECTIONS)}),
    );

    // Frame 5: the same committed state at 100%. Nothing changed but the zoom.
    frames[5].expect_no_draft("Frame 5")?;
    ensure(
        frames[5].revision()? == frames[4].revision()?
            && frames[5].entry()? == frames[4].entry()?,
        "Changing zoom committed something",
    )?;
    ensure(
        frames[5]["step"]["request"] == json!({"view":{"zoom":100.0}}),
        format!(
            "Frame 5 did not request 100%: {}",
            frames[5]["step"]["request"]
        ),
    )?;
    let committed_100_bounds = wheel_bounds(&frames[5])?;
    let committed_100_red = patch_mean(&frames[5], committed_100_bounds, RED_ANGLE_DEG)?;
    ensure(
        distance(committed_100_red, committed_fit_red) < UNCHANGED,
        "The same committed edit reads differently at Fit and at 100%",
    )?;
    record(
        &frames[5],
        "the same committed Red hue +90, at 100%",
        json!({"red_patch": committed_100_red, "zoom_request": frames[5]["step"]["request"], "expanded": frames[5].expanded_sections(&SECTIONS)}),
    );

    // Frame 6: a Saturation field, typed and submitted. The same layer merges the second field.
    ensure(
        frames[6].revision()? == frames[5].revision()? + 1,
        "Enter in the value field did not commit exactly one revision",
    )?;
    ensure(
        frames[6].label()? == "Aqua saturation -40",
        format!("The typed entry is labelled {:?}", frames[6].label()?),
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
        json!({"label": frames[6].label()?, "payload": mixer_payload(&frames[6]), "expanded": frames[6].expanded_sections(&SECTIONS)}),
    );

    // Frame 7: the Saturation group's own reset. One entry labelled by the module, that group's
    // field back to neutral, Red hue untouched, the layer kept.
    ensure(
        frames[7].label()? == format!("Reset {SATURATION_GROUP}"),
        format!("The group reset is labelled {:?}", frames[7].label()?),
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
        json!({"label": frames[7].label()?, "payload": mixer_payload(&frames[7]), "layer": layer, "expanded": frames[7].expanded_sections(&SECTIONS)}),
    );

    // Frame 8: a stronger hue shift to +100, still at 100%, where hue continuity across the wheel
    // can be inspected: the red patch has moved further still and the opposite patch stays clear.
    frames[8].expect_no_draft("Frame 8")?;
    ensure(
        frames[8].revision()? == frames[7].revision()? + 1,
        "The final release did not commit exactly one revision",
    )?;
    ensure(
        frames[8].label()? == "Red hue +100",
        format!("The final entry is labelled {:?}", frames[8].label()?),
    )?;
    ensure(
        mixer_payload(&frames[8]) == Some(&json!({ RED_HUE: 100.0 })),
        format!(
            "The final mixer layer holds {:?}",
            mixer_payload(&frames[8])
        ),
    )?;
    let final_bounds = wheel_bounds(&frames[8])?;
    let final_red = patch_mean(&frames[8], final_bounds, RED_ANGLE_DEG)?;
    let final_opposite = patch_mean(&frames[8], final_bounds, OPPOSITE_ANGLE_DEG)?;
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
        frames[8].section_expanded(MIXER_MODULE) && !frames[8].section_expanded(BASIC_MODULE),
        format!(
            "Colour mixer is not still the only expanded section above it: {}",
            frames[8].expanded_sections(&SECTIONS)
        ),
    )?;
    record(
        &frames[8],
        "a strong hue shift to Red hue +100 at 100%: hue continuity across the wheel is inspected here, with the Colour mixer sliders and rails still on screen",
        json!({"red_patch": final_red, "opposite_patch": final_opposite, "expanded": frames[8].expanded_sections(&SECTIONS)}),
    );

    // Frame 9: the Saturation tab selected. It is per-client view state: no entry, no revision.
    let selected = &frames[9]["state"]["control_ui"]["selected_tab"][MIXER_MODULE];
    ensure(
        selected == &json!(1) && frames[9].revision()? == frames[8].revision()?,
        format!("The Saturation tab was not selected as view state alone: {selected}"),
    )?;
    record(
        &frames[9],
        "the Saturation tab selected: its eight rails shown under the tab row, nothing committed",
        json!({"selected_tab": selected}),
    );

    // Frame 10: the Luminance tab, again view state alone.
    let selected = &frames[10]["state"]["control_ui"]["selected_tab"][MIXER_MODULE];
    ensure(
        selected == &json!(2) && frames[10].revision()? == frames[9].revision()?,
        format!("The Luminance tab was not selected as view state alone: {selected}"),
    )?;
    record(
        &frames[10],
        "the Luminance tab selected: its eight dark-to-light rails under the tab row, nothing committed",
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
