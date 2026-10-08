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
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, pixels, plan::only},
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
/// The module's HSL tab, whose own tab row shows Hue, Saturation and Luminance.
const HSL_GROUP: &str = "HSL";
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
            .commits(0)
            .percent(100.0),
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
        .collapsed(BASIC_MODULE)
        .percent(100.0),
        // 9: the HSL tab's Saturation view: HSL's three properties are a nested tab row, and
        // choosing one is view state.
        Step::new(
            "saturation-tab",
            TabStep {
                module: MIXER_MODULE.into(),
                group: vec![HSL_GROUP.into()],
                index: 1,
            },
        )
        .commits(0),
        // 10: the Luminance tab, the last of the three.
        Step::new(
            "luminance-tab",
            TabStep {
                module: MIXER_MODULE.into(),
                group: vec![HSL_GROUP.into()],
                index: 2,
            },
        )
        .commits(0),
    ])
}

/// The mean RGB of a small patch at `angle_deg` around the wheel's own centre, `SAMPLE_RADIUS_FRACTION`
/// of its radius out — the same fractional geometry [`crate::fixtures::hue_wheel`] (private to that
/// module, reproduced here as plain trigonometry) draws the wheel with, so the angle a mixer range is
/// centred on and the angle sampled here agree regardless of Fit/100% scale or the photo surface's
/// own offset. The wheel is a disc as wide as its square fixture, so it spans the photograph's
/// recorded rectangle.
fn patch_mean(frame: &Frame, angle_deg: f64) -> Result<[f64; 3]> {
    let [left, top, right, bottom] = frame.photo()?;
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
    let mut checks = Checks::new();
    // The red patch and the opposite, control patch of a frame.
    let patches = |frame: &Frame| -> Result<([f64; 3], [f64; 3])> {
        Ok((
            patch_mean(frame, RED_ANGLE_DEG)?,
            patch_mean(frame, OPPOSITE_ANGLE_DEG)?,
        ))
    };

    // The fixture as launched, the module listed and available, the wheel at its opened colours.
    let opened = launch.at("opened")?;
    opened.module_available(MIXER_MODULE)?;
    let (opened_red, opened_opposite) = patches(opened)?;

    // A red-hue edit moves the red patch and leaves the opposite one alone: mid-gesture at +90,
    // with the drafted frame on screen, and committed at +100 at 100%, where hue continuity across
    // the wheel can be inspected. The bounded rotation angle need not still be moving noticeably
    // between +90 and +100 this close to its own bound, so the +100 shift is checked against the
    // opened baseline again, not against the +90 frame.
    launch.at("drag")?.displays_draft()?;
    for (step, what) in [
        ("drag", "+90 red hue drafted"),
        ("stronger", "+100 red hue"),
    ] {
        let frame = launch.at(step)?;
        let (red, opposite) = patches(frame)?;
        checks.compare(
            frame,
            &format!("{what} moves the red patch (distance from the opened one)"),
            distance(red, opened_red),
            0.0,
            Tolerance::Above(CHANGED),
        )?;
        checks.compare(
            frame,
            &format!("{what} leaves the opposite patch alone (distance from the opened one)"),
            distance(opposite, opened_opposite),
            0.0,
            Tolerance::Under(UNCHANGED),
        )?;
    }

    // The release, displayed at Fit, and the same committed state at 100%: nothing changed but the
    // zoom.
    let percent = launch.at("percent")?;
    checks.compare(
        percent,
        "the same committed edit at Fit and at 100% (distance between the red patches)",
        distance(patches(percent)?.0, patches(launch.at("release")?)?.0),
        0.0,
        Tolerance::Under(UNCHANGED),
    )?;

    // The Saturation and Luminance tabs of the HSL tab's own row, each session view state alone.
    for (step, view) in [
        ("saturation-tab", "Saturation"),
        ("luminance-tab", "Luminance"),
    ] {
        let views = &launch.at(step)?.state()["workspace"]["views"];
        ensure(
            views.as_array().is_some_and(|rows| {
                rows.contains(&json!({"module": MIXER_MODULE, "group": [HSL_GROUP], "view": view}))
            }),
            format!("The {step} step selected {views}, not {view}"),
        )?;
    }

    // The HSL tab's nested row shows one property at a time: the Saturation and Luminance views
    // draw different rails. A row drawn as stacked groups would leave the panel unchanged.
    ensure(
        crate::controls_smoke::sidebar_difference(
            launch.at("saturation-tab")?,
            launch.at("luminance-tab")?,
        )? >= 100,
        "Selecting the Luminance view did not change the tools panel",
    )?;

    checks.write(
        &launch.evidence,
        "mixer",
        json!({
            "changed_margin": CHANGED,
            "unchanged_tolerance": UNCHANGED,
            "sample_geometry": {"red_angle_deg": RED_ANGLE_DEG, "opposite_angle_deg": OPPOSITE_ANGLE_DEG, "radius_fraction": SAMPLE_RADIUS_FRACTION},
            "scope": "Mean RGB of small patches on the hue wheel the editor records drawing, read back from the renderer; a hue-continuity and range-locality demonstration, not a colorimetric claim",
        }),
    )
}
