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
    scenario::{Checked, Frame, Plan, Run, Step, pixels, plan::only},
    *,
};
use luxforge_core::MIXER_EFFECT;
use luxforge_evidence::{self as script, SliderStep, TabStep, ViewStep};

const MIXER_MODULE: &str = "luxforge.mixer";
/// The one section the registry lists above the Colour mixer's own that is both a real toggleable
/// section (Pixel declares no expandable section of its own in the tools panel) and expanded by
/// its own descriptor's default: collapsed first, so the module's own sliders and rails are on
/// screen without scrolling.
const BASIC_MODULE: &str = "luxforge.basic";
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

/// Every frame, in order: the open, then one per step. Each step is one gesture, one request or one
/// decision; the plan says what it commits and records, and `verify` below checks what the wheel
/// shows.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The fixture opens with Basic expanded above it (its own descriptor default), the mixer
        // section listed and collapsed, every field at its default, no draft and no mixer layer.
        Step::opened("opened")
            .collapsed(MIXER_MODULE)
            .field(SET_MIXER, RED_HUE, "0")
            .no_draft()
            .no_layer(MIXER_EFFECT),
        // 1: collapse Basic, expanded by its own default and the one other section the
        // registry lists above Colour mixer, so the module's own sliders and rails land on
        // screen without scrolling once it expands.
        Step::new(
            "basic-collapsed",
            script::Step::section(BASIC_MODULE, false),
        )
        .commits(0)
        .collapsed(BASIC_MODULE),
        // 2: expand the section. Hue starts expanded by the module's own descriptor, so this
        // alone exposes its eight rails.
        Step::new("expanded", script::Step::section(MIXER_MODULE, true))
            .commits(0)
            .expanded(MIXER_MODULE)
            .collapsed(BASIC_MODULE),
        // 3: a drag on Red hue, left open: the frame shows the drafted preview.
        Step::new(
            "drag",
            SliderStep::new(SET_MIXER, RED_HUE, [30.0, 60.0, 90.0]),
        )
        .commits(0)
        .draft(SET_MIXER, json!({ RED_HUE: 90.0 }))
        .no_layer(MIXER_EFFECT)
        .field(SET_MIXER, RED_HUE, "90"),
        // 4: the same gesture released: one entry, one revision, committed at Fit.
        Step::new(
            "release",
            SliderStep::new(SET_MIXER, RED_HUE, [90.0]).release(),
        )
        .no_draft()
        .commits(1)
        .label("Red hue +90")
        .payload(MIXER_EFFECT, json!({ RED_HUE: 90.0 })),
        // 5: the same committed state at 100%.
        Step::new("percent", ViewStep::Percent(100.0))
            .no_draft()
            .commits(0),
        // 6: a Saturation field, so its group's reset below has something to undo. The same layer
        // merges the second field.
        Step::new(
            "saturation",
            script::Step::field(SET_MIXER, AQUA_SATURATION, "-40", true),
        )
        .commits(1)
        .label("Aqua saturation -40")
        .payload(
            MIXER_EFFECT,
            json!({ RED_HUE: 90.0, AQUA_SATURATION: -40.0 }),
        )
        .same_layer(MIXER_EFFECT, "release"),
        // 7: the Saturation group's own reset, leaving the Hue field alone and the layer kept.
        // Saturation starts collapsed, so its own group is off screen; the group reset button
        // lives on the group header and runs the same way whether or not that group is expanded.
        Step::new(
            "saturation-reset",
            script::Step::reset(MIXER_MODULE, Some(SATURATION_GROUP)),
        )
        .commits(1)
        .label(format!("Reset {SATURATION_GROUP}"))
        .payload(MIXER_EFFECT, json!({ RED_HUE: 90.0 }))
        .same_layer(MIXER_EFFECT, "release")
        .field(SET_MIXER, AQUA_SATURATION, "0")
        .field(SET_MIXER, RED_HUE, "90"),
        // 8: a stronger hue shift, still at 100%, where continuity across the wheel shows, with
        // the Colour mixer still the only expanded section above it.
        Step::new(
            "stronger",
            SliderStep::new(SET_MIXER, RED_HUE, [100.0]).release(),
        )
        .no_draft()
        .commits(1)
        .label("Red hue +100")
        .payload(MIXER_EFFECT, json!({ RED_HUE: 100.0 }))
        .expanded(MIXER_MODULE)
        .collapsed(BASIC_MODULE),
        // 9: the Saturation tab: the mixer's groups are tabs, and choosing one is view state.
        Step::new(
            "saturation-tab",
            TabStep {
                module: MIXER_MODULE.into(),
                index: 1,
            },
        )
        .commits(0),
        // 10: the Luminance tab, the last of the three.
        Step::new(
            "luminance-tab",
            TabStep {
                module: MIXER_MODULE.into(),
                index: 2,
            },
        )
        .commits(0),
    ])
}

/// Every section this scenario toggles, for the correlation every recorded
/// frame carries alongside its revision, entry and draft.
const SECTIONS: [&str; 2] = [BASIC_MODULE, MIXER_MODULE];

fn mixer_payload(frame: &Frame) -> Option<&Value> {
    frame.payload(MIXER_EFFECT)
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

/// What the wheel shows at each step, once the plan has held.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Vec::new();
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };
    // The red patch and the opposite, control patch of a frame.
    let patches = |frame: &Frame| -> Result<([f64; 3], [f64; 3])> {
        let bounds = wheel_bounds(frame)?;
        Ok((
            patch_mean(frame, bounds, RED_ANGLE_DEG)?,
            patch_mean(frame, bounds, OPPOSITE_ANGLE_DEG)?,
        ))
    };

    // The fixture as launched, the module listed and available, the wheel at its opened colours.
    let opened = launch.at("opened")?;
    let mixer = opened["state"]["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|module| module["id"] == json!(MIXER_MODULE))
        .ok_or("The Colour mixer module is not listed at all")?;
    ensure(
        mixer["available"] == json!(true),
        "The Colour mixer module is not available",
    )?;
    let (opened_red, opened_opposite) = patches(opened)?;
    record(
        opened,
        "the collapsed Colour mixer section as launched, the wheel at its opened colours",
        json!({"red_patch": opened_red, "opposite_patch": opened_opposite, "expanded": opened.expanded_sections(&SECTIONS)}),
    );
    let basic = launch.at("basic-collapsed")?;
    record(
        basic,
        "the Basic section collapsed, above Colour mixer in the registry order",
        json!({"expanded": basic.expanded_sections(&SECTIONS)}),
    );
    let expanded = launch.at("expanded")?;
    record(
        expanded,
        "the Colour mixer section expanded: the Hue group and its eight rails, on screen with nothing above it expanded",
        json!({"expanded": expanded.expanded_sections(&SECTIONS)}),
    );

    // Mid-gesture at Red hue +90: the drafted frame is on screen, the red patch has visibly moved
    // and the opposite patch has not.
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
    let (drafted_red, drafted_opposite) = patches(drag)?;
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
        drag,
        "a drag to Red hue +90, mid-gesture: the drafted preview, nothing committed, Colour mixer still the only expanded section",
        json!({"draft": drafted, "red_patch": drafted_red, "opposite_patch": drafted_opposite, "expanded": drag.expanded_sections(&SECTIONS)}),
    );

    // The release, displayed at Fit, and the same committed state at 100%: nothing changed but the
    // zoom.
    let release = launch.at("release")?;
    let (committed_fit_red, _) = patches(release)?;
    let layer = release.layer_id(MIXER_EFFECT);
    record(
        release,
        "released: one entry \"Red hue +90\", displayed at Fit",
        json!({"revision": release.revision()?, "label": release.label()?, "red_patch": committed_fit_red, "layer": layer, "expanded": release.expanded_sections(&SECTIONS)}),
    );
    let percent = launch.at("percent")?;
    let (committed_100_red, _) = patches(percent)?;
    ensure(
        distance(committed_100_red, committed_fit_red) < UNCHANGED,
        "The same committed edit reads differently at Fit and at 100%",
    )?;
    record(
        percent,
        "the same committed Red hue +90, at 100%",
        json!({"red_patch": committed_100_red, "zoom_request": percent["step"]["request"], "expanded": percent.expanded_sections(&SECTIONS)}),
    );

    let saturation = launch.at("saturation")?;
    record(
        saturation,
        "Aqua saturation typed as -40 and committed with Enter: the same layer, both fields",
        json!({"label": saturation.label()?, "payload": mixer_payload(saturation), "expanded": saturation.expanded_sections(&SECTIONS)}),
    );
    let reset = launch.at("saturation-reset")?;
    record(
        reset,
        "the Saturation group reset: one entry, that field neutral, Hue untouched",
        json!({"label": reset.label()?, "payload": mixer_payload(reset), "layer": layer, "expanded": reset.expanded_sections(&SECTIONS)}),
    );

    // A stronger hue shift to +100, at 100%, where hue continuity across the wheel can be
    // inspected: the red patch has moved further still and the opposite patch stays clear. The
    // bounded rotation angle need not still be moving noticeably between +90 and +100 this close
    // to its own bound, so this checks the shift against the true opened baseline again, not
    // against the +90 frame; the wheel itself is where a reader inspects hue continuity.
    let stronger = launch.at("stronger")?;
    let (final_red, final_opposite) = patches(stronger)?;
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
    record(
        stronger,
        "a strong hue shift to Red hue +100 at 100%: hue continuity across the wheel is inspected here, with the Colour mixer sliders and rails still on screen",
        json!({"red_patch": final_red, "opposite_patch": final_opposite, "expanded": stronger.expanded_sections(&SECTIONS)}),
    );

    // The Saturation and Luminance tabs, each per-client view state alone.
    for (step, index, shows) in [
        (
            "saturation-tab",
            1,
            "the Saturation tab selected: its eight rails shown under the tab row, nothing committed",
        ),
        (
            "luminance-tab",
            2,
            "the Luminance tab selected: its eight dark-to-light rails under the tab row, nothing committed",
        ),
    ] {
        let frame = launch.at(step)?;
        let selected = &frame["state"]["control_ui"]["selected_tab"][MIXER_MODULE];
        ensure(
            selected == &json!(index),
            format!("The {step} step selected tab {selected}, not {index}"),
        )?;
        record(frame, shows, json!({"selected_tab": selected}));
    }

    write_json(
        &launch.evidence.join("mixer-checks.json"),
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
