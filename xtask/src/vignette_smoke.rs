//! The `vignette` smoke scenario: the section's own expand, drag, commit, roundness and feather
//! extremes, a post-crop recentre and the module reset, on the real editor.
//!
//! The fixture is `fixtures/s0/orientation-1.jpg`, the ordinary quadrant pattern with a white
//! centre cross and a black dash band, so every sample point below is chosen to avoid both: a
//! corner point stays inside its own quadrant's flat colour, and the edge-midpoint points this file
//! samples for the roundness and feather checks sit near the very top and bottom of the frame,
//! outside the cross's own row range and clear of the dash band around the centre.
//!
//! The differential checks below (roundness -100 against +100, feather 0 against 100) are derived
//! from the frozen mask geometry in `crates/luxforge-core/src/modules/vignette/unit.rs`: at the
//! default midpoint and feather, an ellipse's edge-midpoint radius is well inside the falloff's
//! inner bound while a rounded rectangle's is beyond its outer one, so roundness -100 darkens an
//! edge midpoint fully while roundness +100 only partly does; a feather of 0 collapses the falloff
//! to a hard step at the midpoint radius while a feather of 100 spreads it from the centre to the
//! corner, so a point partway out is always darkened more at feather 0 than at feather 100.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, plan::only},
    *,
};
use luxforge_core::{CROP_EFFECT, VIGNETTE_EFFECT};
use luxforge_evidence::{self as script, SliderStep, ViewStep};

const VIGNETTE_MODULE: &str = "luxforge.vignette";
/// The sections the registry lists above Vignette that declare a real toggleable section (Pixel
/// does not) and are expanded by their own descriptor's default (Mixer is already collapsed by
/// default, so it needs no explicit step): collapsed first, so the module's own sliders are on
/// screen without scrolling.
const BASIC_MODULE: &str = "luxforge.basic";
const TRANSFORM_MODULE: &str = "luxforge.transform";
const CROP_MODULE: &str = "luxforge.crop";
const SET_VIGNETTE: &str = "set-vignette";
const AMOUNT: &str = "amount";
const ROUNDNESS: &str = "roundness";
const FEATHER: &str = "feather";
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";

/// A patch fractionally inset from each true corner: deep enough into the quadrant colour to clear
/// the seam at the frame's own centre lines, shallow enough to stay in the mask's fully-darkened
/// zone at every parameter combination this scenario commits.
const CORNER_INSET: f64 = 0.06;
/// A near-centre patch, offset from the exact centre so it clears both the white cross (which
/// spans rows 30..(height - 30) of the 320-tall source, at the very centre column) and the dash
/// band (the middle 40 rows), while staying inside the mask's unaffected zone at the default
/// midpoint and feather.
const CENTRE_OFFSET: f64 = 0.1;
/// The top/bottom edge-midpoint patches the roundness and feather checks read, nudged off the
/// centre column so they read one quadrant's own colour rather than the seam between two.
const EDGE_X: f64 = 0.52;
const EDGE_Y_INSET: f64 = 0.04;
/// Half the side length, in pixels, of a sampled patch: wide enough that even a sample point close
/// to one of the fixture's own quadrant-label strokes still covers plenty of the plain background
/// around it, which [`patch`]'s darkest-pixel reading then picks out.
const PATCH_HALF: i64 = 8;

/// How far a patch's darkest-pixel luminance must move before this scenario calls it darkened. The
/// frozen mask makes a fully darkened corner about 60% of its own brightness at amount -60, so this
/// is a wide margin, not a threshold tuning could slip past.
const DARKER: f64 = 15.0;
/// How close two darkest-pixel luminance readings must stay before this scenario calls a patch
/// unaffected.
const SAME: f64 = 10.0;

/// A committed Vignette slider release.
fn release(name: &str, parameter: &str, value: f64) -> Step {
    Step::new(
        name,
        SliderStep::new(SET_VIGNETTE, parameter, [value]).release(),
    )
    .no_draft()
    .commits(1)
}

/// Every frame, in order: the open, then one per step. Each step is one gesture, one request or one
/// decision; the expectations here are what it commits and records, and `verify` below checks what
/// the photograph shows.
pub fn plan(_: &[PathBuf]) -> Plan {
    let section = |name: &str, module: &str, expanded: bool| {
        Step::new(name, script::Step::section(module, expanded)).commits(0)
    };
    Plan::new(vec![
        // The fixture opens with the Vignette section listed and collapsed, Amount at its default,
        // no draft and no vignette layer yet.
        Step::opened("opened")
            .collapsed(VIGNETTE_MODULE)
            .field(SET_VIGNETTE, AMOUNT, "0")
            .no_draft()
            .no_layer(VIGNETTE_EFFECT),
        // 1-3: collapse the sections the registry lists above Vignette that declare a real
        // toggleable section, each expanded by its own default, so the module's own four
        // sliders land on screen without scrolling once it expands. Pixel declares no
        // expandable section, and Mixer is already collapsed by default, so neither needs a
        // step.
        section("basic-collapsed", BASIC_MODULE, false).collapsed(BASIC_MODULE),
        section("transform-collapsed", TRANSFORM_MODULE, false).collapsed(TRANSFORM_MODULE),
        section("crop-collapsed", CROP_MODULE, false).collapsed(CROP_MODULE),
        // 4: expand the section, with nothing above it still expanded. Its one group starts
        // expanded, so this alone exposes it.
        section("expanded", VIGNETTE_MODULE, true)
            .expanded(VIGNETTE_MODULE)
            .collapsed(BASIC_MODULE)
            .collapsed(TRANSFORM_MODULE)
            .collapsed(CROP_MODULE),
        // 5: a drag on Amount, left open: the frame shows the drafted preview, nothing committed.
        Step::new(
            "drag",
            SliderStep::new(SET_VIGNETTE, AMOUNT, [-20.0, -40.0, -60.0]),
        )
        .commits(0)
        .draft(SET_VIGNETTE, json!({ AMOUNT: -60.0 }))
        .no_layer(VIGNETTE_EFFECT)
        .field(SET_VIGNETTE, AMOUNT, "-60"),
        // 6: the same gesture released: one entry, committed at Fit.
        release("release", AMOUNT, -60.0)
            .label("Vignette amount -60")
            .payload(VIGNETTE_EFFECT, json!({ AMOUNT: -60.0 })),
        // 7: the same committed state at 100%.
        Step::new("percent", ViewStep::Percent(100.0))
            .no_draft()
            .commits(0)
            .percent(100.0),
        // 8: back to Fit for the roundness and feather commits below.
        Step::new("fit", ViewStep::Fit).no_draft().commits(0).fit(),
        // 9: Roundness -100, a rounded rectangle, updating the same layer.
        release("rectangle", ROUNDNESS, -100.0)
            .label("Vignette roundness -100")
            .payload(VIGNETTE_EFFECT, json!({ AMOUNT: -60.0, ROUNDNESS: -100.0 }))
            .same_layer(VIGNETTE_EFFECT, "release"),
        // 10: Roundness +100, a circle.
        release("circle", ROUNDNESS, 100.0)
            .label("Vignette roundness +100")
            .payload(VIGNETTE_EFFECT, json!({ AMOUNT: -60.0, ROUNDNESS: 100.0 }))
            .same_layer(VIGNETTE_EFFECT, "release"),
        // 11: Feather 0, a hard step.
        release("hard", FEATHER, 0.0)
            .label("Vignette feather 0")
            .payload(
                VIGNETTE_EFFECT,
                json!({ AMOUNT: -60.0, ROUNDNESS: 100.0, FEATHER: 0.0 }),
            )
            .same_layer(VIGNETTE_EFFECT, "release"),
        // 12: Feather 100, the widest falloff.
        release("soft", FEATHER, 100.0)
            .label("Vignette feather 100")
            .payload(
                VIGNETTE_EFFECT,
                json!({ AMOUNT: -60.0, ROUNDNESS: 100.0, FEATHER: 100.0 }),
            )
            .same_layer(VIGNETTE_EFFECT, "release"),
        // 13: a crop applied after the vignette already exists; the host places the crop layer
        // before it regardless, so the mask recentres on the cropped output stage.
        Step::new(
            "cropped",
            script::Step::call("edit.crop-fit", json!({"aspect":"1:1","angle":0})),
        )
        .commits(1)
        .same_layer(VIGNETTE_EFFECT, "release"),
        // 14: the module's own header reset: the layer kept at its all-default payload.
        Step::new("reset", script::Step::reset(VIGNETTE_MODULE, None))
            .commits(1)
            .label("Reset Vignette")
            .payload(VIGNETTE_EFFECT, json!({}))
            .same_layer(VIGNETTE_EFFECT, "release"),
    ])
}

/// The stack's layer effects in stored order, so the crop-recentre frame can prove the crop layer
/// the host inserted sits before the vignette layer it recentres.
fn layer_effects(frame: &Frame) -> Vec<String> {
    frame.state()["stack"]["layers"]
        .as_array()
        .map(|layers| {
            layers
                .iter()
                .filter_map(|layer| layer["effect"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The darkest pixel in a small patch at fraction `(fx, fy)` of the photograph. The fixture draws
/// its quadrant labels in white, so a mean over the patch can read bright when a sample point
/// happens to sit close to a label stroke; the darkest pixel instead reads the plain background
/// colour underneath, which is what every check here actually means by "this point of the mask".
fn patch(frame: &Frame, fx: f64, fy: f64) -> Result<f64> {
    frame.darkest_at([fx, fy], PATCH_HALF)
}

/// The four corners, inset by [`CORNER_INSET`], each safely in the mask's fully darkened zone at
/// every parameter combination this scenario commits, then the near-centre patch.
fn readings(frame: &Frame) -> Result<([f64; 4], f64)> {
    let (near, far) = (CORNER_INSET, 1.0 - CORNER_INSET);
    let corners = [
        patch(frame, near, near)?,
        patch(frame, far, near)?,
        patch(frame, near, far)?,
        patch(frame, far, far)?,
    ];
    Ok((
        corners,
        patch(frame, 0.5 - CENTRE_OFFSET, 0.5 - CENTRE_OFFSET)?,
    ))
}

/// The mean of the top and bottom edge-midpoint patches of the named step's frame: see the module
/// doc for why roundness and feather each move this reading in a known direction.
fn edge(launch: &Checked, step: &str) -> Result<f64> {
    let frame = launch.at(step)?;
    Ok((patch(frame, EDGE_X, EDGE_Y_INSET)? + patch(frame, EDGE_X, 1.0 - EDGE_Y_INSET)?) / 2.0)
}

/// What the photograph shows at each step, once the plan has held.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();

    // The fixture as launched: the Vignette module listed and available.
    let opened = launch.at("opened")?;
    let vignette = opened.state()["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|module| module["id"] == json!(VIGNETTE_MODULE))
        .ok_or("The Vignette module is not listed at all")?;
    ensure(
        vignette["available"] == json!(true),
        "The Vignette module is not available",
    )?;
    let (opened_corners, opened_centre) = readings(opened)?;
    checks.note(
        opened,
        "the collapsed Vignette section as launched",
        json!({"corner_luminance": opened_corners, "centre_luminance": opened_centre}),
    );

    // Mid-gesture at Amount -60: the frame on screen is the drafted one.
    let drag = launch.at("drag")?;
    ensure(
        drag.state()["displayed_draft_revision"] == drag.draft()["draft_revision"],
        format!(
            "The drag displays draft revision {} while the draft is at {}",
            drag.state()["displayed_draft_revision"],
            drag.draft()["draft_revision"]
        ),
    )?;

    // The release at Fit: every corner is darker than it was at the opened baseline, and the
    // near-centre patch is unaffected.
    let released = launch.at("release")?;
    let (fit_corners, fit_centre) = readings(released)?;
    for (index, (opened, darkened)) in opened_corners.iter().zip(fit_corners).enumerate() {
        checks.compare(
            released,
            &format!("corner {index} darkened by Amount -60"),
            *opened,
            darkened,
            Tolerance::Above(DARKER),
        )?;
    }
    checks.compare(
        released,
        "the centre unaffected by Amount -60",
        opened_centre,
        fit_centre,
        Tolerance::Under(SAME),
    )?;

    // The same committed state at 100%. The near-centre patch is clear of the fixture's own
    // quadrant labels at both zooms, so it reads the same regardless of scale; a corner patch can
    // sit close enough to a label at one scale and not the other that resampling shifts its own
    // reading, which is a rendering detail of this label-bearing fixture, not a claim about the
    // committed edit these frames share.
    let percent = launch.at("percent")?;
    let (percent_corners, percent_centre) = readings(percent)?;
    checks.compare(
        percent,
        "the unaffected centre at Fit and at 100%",
        fit_centre,
        percent_centre,
        Tolerance::Under(SAME),
    )?;
    checks.note(
        percent,
        "the same committed Amount -60, at 100%",
        json!({"corner_luminance": percent_corners}),
    );

    // Roundness: at the default midpoint and feather, an edge midpoint of a rounded rectangle is
    // beyond the falloff's outer bound and reads as darkened as a corner, while a circle's is well
    // inside its inner bound and reads brighter.
    checks.compare(
        launch.at("circle")?,
        "Roundness +100's edge midpoint brighter than -100's",
        edge(launch, "circle")?,
        edge(launch, "rectangle")?,
        Tolerance::Above(DARKER),
    )?;

    // Feather: a hard step at the midpoint radius fully darkens the same edge midpoint, beyond
    // that radius; the widest falloff leaves it only partway through and reads brighter.
    checks.compare(
        launch.at("soft")?,
        "Feather 100's edge midpoint brighter than Feather 0's",
        edge(launch, "soft")?,
        edge(launch, "hard")?,
        Tolerance::Above(DARKER),
    )?;

    // A crop applied after the vignette already existed. The host still places the crop layer
    // before it, so the mask recentres on the cropped output stage: the new frame's own corners
    // read darker than their own un-vignetted baseline, exactly as the release did on the whole
    // photograph.
    let cropped = launch.at("cropped")?;
    let effects = layer_effects(cropped);
    let position = |effect: &str| effects.iter().position(|e| e == effect);
    ensure(
        position(CROP_EFFECT)
            .is_some_and(|crop| position(VIGNETTE_EFFECT).is_some_and(|vignette| crop < vignette)),
        format!("The crop layer does not precede the Vignette layer: {effects:?}"),
    )?;
    // Each corner against its own un-vignetted baseline from the open (same hue, same corner
    // index), not against a single shared centre reading: the fixture's four quadrant colours have
    // very different Rec. 709 luminance to begin with, so the same relative darkening moves each of
    // them by a different absolute amount, and a fixed threshold shared across hues is not the
    // claim this check makes. A 1:1 crop keeps the full height and trims width symmetrically, so
    // each corner of the crop is still deep in its own quadrant's flat colour.
    let (cropped_corners, _) = readings(cropped)?;
    for (index, (opened, cropped_corner)) in opened_corners.iter().zip(cropped_corners).enumerate()
    {
        checks.compare(
            cropped,
            &format!("corner {index} of the 1:1 crop darker than its un-vignetted baseline"),
            *opened,
            cropped_corner,
            Tolerance::Above(DARKER),
        )?;
    }

    // The module's own header reset leaves the crop from the step before untouched.
    ensure(
        layer_effects(launch.at("reset")?).contains(&CROP_EFFECT.to_owned()),
        "The module reset lost the crop from the previous step",
    )?;

    checks.write(
        &launch.evidence,
        "vignette",
        json!({
            "darker_margin": DARKER,
            "same_tolerance": SAME,
            "sample_geometry": {"corner_inset": CORNER_INSET, "centre_offset": CENTRE_OFFSET, "edge_x": EDGE_X, "edge_y_inset": EDGE_Y_INSET},
            "scope": "Darkest Rec. 709 luminance of small patches of the photograph the editor records drawing, read back from the renderer; a mask-shape and recentring demonstration, not a colorimetric claim",
        }),
    )
}
