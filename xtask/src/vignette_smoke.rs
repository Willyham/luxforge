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
//! from the frozen mask geometry in `crates/lightwell-core/src/modules/vignette/unit.rs`: at the
//! default midpoint and feather, an ellipse's edge-midpoint radius is well inside the falloff's
//! inner bound while a rounded rectangle's is beyond its outer one, so roundness -100 darkens an
//! edge midpoint fully while roundness +100 only partly does; a feather of 0 collapses the falloff
//! to a hard step at the midpoint radius while a feather of 100 spreads it from the centre to the
//! corner, so a point partway out is always darkened more at feather 0 than at feather 100.
use crate::{
    scenario::{Bright, Frame, Scan, pixels},
    *,
};

const VIGNETTE_MODULE: &str = "lightwell.vignette";
/// The sections the registry lists above Vignette that declare a real toggleable section (Pixel
/// does not) and are expanded by their own descriptor's default (Mixer is already collapsed by
/// default, so it needs no explicit step): collapsed first, so the module's own sliders are on
/// screen without scrolling.
const BASIC_MODULE: &str = "lightwell.basic";
const TRANSFORM_MODULE: &str = "lightwell.transform";
const CROP_MODULE: &str = "lightwell.crop";
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
/// around it, which [`patch_luminance`]'s darkest-pixel reading then picks out.
const PATCH_HALF: i64 = 8;

/// How far a patch's darkest-pixel luminance must move before this scenario calls it darkened. The
/// frozen mask makes a fully darkened corner about 60% of its own brightness at amount -60, so this
/// is a wide margin, not a threshold tuning could slip past.
const DARKER: f64 = 15.0;
/// How close two darkest-pixel luminance readings must stay before this scenario calls a patch
/// unaffected.
const SAME: f64 = 10.0;

/// One open frame plus one per script step.
pub fn frames(scenario: &str) -> Option<usize> {
    (scenario == "vignette").then_some(15)
}

pub fn source(scenario: &str) -> Option<&'static str> {
    (scenario == "vignette").then_some(FIXTURE)
}

/// The evidence script. Each step is one gesture, one request or one decision; `verify` below
/// checks exactly what each one is supposed to prove.
pub fn script(scenario: &str) -> Option<Value> {
    (scenario == "vignette").then(|| {
        json!([
            // 1-3: collapse the sections the registry lists above Vignette that declare a real
            // toggleable section, each expanded by its own default, so the module's own four
            // sliders land on screen without scrolling once it expands. Pixel declares no
            // expandable section, and Mixer is already collapsed by default, so neither needs a
            // step.
            {"section":{"module":BASIC_MODULE,"expanded":false}},
            {"section":{"module":TRANSFORM_MODULE,"expanded":false}},
            {"section":{"module":CROP_MODULE,"expanded":false}},
            // 4: expand the section. Its one group starts expanded, so this alone exposes it.
            {"section":{"module":VIGNETTE_MODULE,"expanded":true}},
            // 5: a drag on Amount, left open: the frame shows the drafted preview.
            {"slider":{"action":SET_VIGNETTE,"parameter":AMOUNT,"values":[-20.0,-40.0,-60.0]}},
            // 6: the same gesture released: one entry, committed at Fit.
            {"slider":{"action":SET_VIGNETTE,"parameter":AMOUNT,"values":[-60.0],"release":true}},
            // 7: the same committed state at 100%.
            {"view":{"zoom":"100"}},
            // 8: back to Fit for the roundness and feather commits below.
            {"view":{"zoom":"fit"}},
            // 9: Roundness -100, a rounded rectangle.
            {"slider":{"action":SET_VIGNETTE,"parameter":ROUNDNESS,"values":[-100.0],"release":true}},
            // 10: Roundness +100, a circle.
            {"slider":{"action":SET_VIGNETTE,"parameter":ROUNDNESS,"values":[100.0],"release":true}},
            // 11: Feather 0, a hard step.
            {"slider":{"action":SET_VIGNETTE,"parameter":FEATHER,"values":[0.0],"release":true}},
            // 12: Feather 100, the widest falloff.
            {"slider":{"action":SET_VIGNETTE,"parameter":FEATHER,"values":[100.0],"release":true}},
            // 13: a crop applied after the vignette already exists; the host places the crop layer
            // before it regardless, so the mask recentres on the cropped output stage.
            {"api":{"method":"edit.crop-fit","params":{"aspect":"1:1","angle":0}}},
            // 14: the module's own header reset.
            {"reset":{"module":VIGNETTE_MODULE}}
        ])
    })
}

/// Every section this scenario toggles, for the correlation every recorded
/// frame carries alongside its revision, entry and draft.
const SECTIONS: [&str; 4] = [BASIC_MODULE, TRANSFORM_MODULE, CROP_MODULE, VIGNETTE_MODULE];

fn vignette_payload(frame: &Frame) -> Option<&Value> {
    frame.payload(lightwell_core::VIGNETTE_EFFECT)
}

fn vignette_layer_id(frame: &Frame) -> Option<&str> {
    frame.layer_id(lightwell_core::VIGNETTE_EFFECT)
}

/// The stack's layer identities in stored order, so the crop-recentre frame can prove the crop
/// layer the host inserted sits before the vignette layer it recentres.
fn layer_effects(frame: &Frame) -> Vec<String> {
    frame["state"]["stack"]["layers"]
        .as_array()
        .map(|layers| {
            layers
                .iter()
                .filter_map(|layer| layer["effect"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn vignette_field<'a>(frame: &'a Frame, name: &str) -> Result<&'a str> {
    frame.field(SET_VIGNETTE, name)
}

/// Where the photograph is drawn: found by the column with the tallest run of bright pixels, and
/// then by the widest run of bright pixels among that vertical extent's own rows, its last row
/// included (a rectangular fixture's least-vignetted row — the one through its own vertical centre —
/// is always at least as wide as any other). See [`Scan::Tallest`] for why the vertical extent comes
/// first. `presets` measures its photograph the same way.
pub const BOUNDS: Bright = Bright {
    threshold: 60,
    scan: Scan::Tallest { last_row: true },
    least: None,
};

/// The darkest pixel in a small patch at fraction `(fx, fy)` of `bounds`. The fixture draws its
/// quadrant labels in white, so a mean over the patch can read bright when a sample point happens
/// to sit close to a label stroke; the darkest pixel instead reads the plain background colour
/// underneath, which is what every check here actually means by "this point of the mask".
fn patch_luminance(frame: &Frame, bounds: [u32; 4], fx: f64, fy: f64) -> Result<f64> {
    pixels::darkest_luminance(frame.image()?, pixels::at(bounds, [fx, fy]), PATCH_HALF)
}

/// The four corners, inset by [`CORNER_INSET`], each safely in the mask's fully darkened zone at
/// every parameter combination this scenario commits.
fn corner_luminances(frame: &Frame, bounds: [u32; 4]) -> Result<[f64; 4]> {
    let mut values = [0.0; 4];
    for (index, (fx, fy)) in [
        (CORNER_INSET, CORNER_INSET),
        (1.0 - CORNER_INSET, CORNER_INSET),
        (CORNER_INSET, 1.0 - CORNER_INSET),
        (1.0 - CORNER_INSET, 1.0 - CORNER_INSET),
    ]
    .into_iter()
    .enumerate()
    {
        values[index] = patch_luminance(frame, bounds, fx, fy)?;
    }
    Ok(values)
}

fn centre_luminance(frame: &Frame, bounds: [u32; 4]) -> Result<f64> {
    patch_luminance(frame, bounds, 0.5 - CENTRE_OFFSET, 0.5 - CENTRE_OFFSET)
}

/// The mean of the top and bottom edge-midpoint patches: see the module doc for why roundness and
/// feather each move this reading in a known direction.
fn edge_luminance(frame: &Frame, bounds: [u32; 4]) -> Result<f64> {
    let top = patch_luminance(frame, bounds, EDGE_X, EDGE_Y_INSET)?;
    let bottom = patch_luminance(frame, bounds, EDGE_X, 1.0 - EDGE_Y_INSET)?;
    Ok((top + bottom) / 2.0)
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

    // Frame 0: the fixture opens with the Vignette section listed and collapsed, every field at
    // its default, no draft and no vignette layer yet.
    let vignette = frames[0]["state"]["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|module| module["id"] == json!(VIGNETTE_MODULE))
        .ok_or("The Vignette module is not listed at all")?;
    ensure(
        vignette["available"] == json!(true),
        "The Vignette module is not available",
    )?;
    ensure(
        !frames[0].section_expanded(VIGNETTE_MODULE),
        "The Vignette section is not collapsed as launched",
    )?;
    ensure(
        vignette_field(&frames[0], AMOUNT)? == "0",
        format!(
            "Amount does not start neutral: {}",
            vignette_field(&frames[0], AMOUNT)?
        ),
    )?;
    frames[0].expect_no_draft("Frame 0")?;
    ensure(
        vignette_payload(&frames[0]).is_none(),
        "The opened stack already holds a Vignette layer",
    )?;
    let opened_bounds = pixels::bright_bounds(&frames[0], BOUNDS)?;
    let opened_corners = corner_luminances(&frames[0], opened_bounds)?;
    let opened_centre = centre_luminance(&frames[0], opened_bounds)?;
    record(
        &frames[0],
        "the collapsed Vignette section as launched",
        json!({"corner_luminance": opened_corners, "centre_luminance": opened_centre, "expanded": frames[0].expanded_sections(&SECTIONS)}),
    );

    // Frames 1-3: Basic, Transform and Crop collapsed in turn, each above Vignette in the
    // registry order and declaring a real toggleable section (Pixel does not), each expanded by
    // its own default, so none of them is still expanded once Vignette itself opens.
    for (index, module) in [(1, BASIC_MODULE), (2, TRANSFORM_MODULE), (3, CROP_MODULE)] {
        ensure(
            !frames[index].section_expanded(module),
            format!("The section step did not collapse {module}"),
        )?;
        ensure(
            frames[index].revision()? == frames[0].revision()?,
            format!("Collapsing {module} committed something"),
        )?;
        record(
            &frames[index],
            "a section above Vignette collapsed, out of the way of its own sliders",
            json!({"collapsed": module, "expanded": frames[index].expanded_sections(&SECTIONS)}),
        );
    }

    // Frame 4: the section expanded, with nothing above it still expanded. Its one group starts
    // expanded, so its four sliders show without any further group step or scrolling.
    ensure(
        frames[4].section_expanded(VIGNETTE_MODULE)
            && !frames[4].section_expanded(BASIC_MODULE)
            && !frames[4].section_expanded(TRANSFORM_MODULE)
            && !frames[4].section_expanded(CROP_MODULE),
        format!(
            "The section step did not expand Vignette alone: {}",
            frames[4].expanded_sections(&SECTIONS)
        ),
    )?;
    ensure(
        frames[4].revision()? == frames[3].revision()?,
        "Expanding the section committed something",
    )?;
    record(
        &frames[4],
        "the Vignette section expanded: its four sliders on screen with nothing above it expanded",
        json!({"expanded": frames[4].expanded_sections(&SECTIONS)}),
    );

    // Frame 5: mid-gesture at Amount -60. The draft is open, nothing is committed.
    let drafted = frames[5].draft();
    ensure(
        drafted["action"] == json!(SET_VIGNETTE)
            && drafted["fields"] == json!({ AMOUNT: -60.0 })
            && drafted["conflicted"] == json!(false),
        format!("Frame 5's draft is not the open Amount gesture: {drafted}"),
    )?;
    ensure(
        frames[5]["state"]["displayed_draft_revision"] == drafted["draft_revision"],
        format!(
            "Frame 5 displays draft revision {} while the draft is at {}",
            frames[5]["state"]["displayed_draft_revision"], drafted["draft_revision"]
        ),
    )?;
    ensure(
        frames[5].revision()? == frames[4].revision()? && vignette_payload(&frames[5]).is_none(),
        "A drag committed something",
    )?;
    ensure(
        vignette_field(&frames[5], AMOUNT)? == "-60",
        format!(
            "The slider does not show the drafted value: {}",
            vignette_field(&frames[5], AMOUNT)?
        ),
    )?;
    record(
        &frames[5],
        "a drag to Amount -60, mid-gesture: the drafted preview",
        json!({"draft": drafted, "expanded": frames[5].expanded_sections(&SECTIONS)}),
    );

    // Frame 6: the release. One entry, labelled by the module, at Fit; every corner is darker than
    // it was at the opened baseline, and the near-centre patch is unaffected.
    frames[6].expect_no_draft("Frame 6")?;
    ensure(
        frames[6].revision()? == frames[5].revision()? + 1,
        "The release did not advance the revision by one",
    )?;
    ensure(
        frames[6].entry()? != frames[4].entry()?,
        "The release created no new history entry",
    )?;
    ensure(
        frames[6].label()? == "Vignette amount -60",
        format!("The committed entry is labelled {:?}", frames[6].label()?),
    )?;
    ensure(
        vignette_payload(&frames[6]) == Some(&json!({ AMOUNT: -60.0 })),
        format!(
            "The committed Vignette layer holds {:?}",
            vignette_payload(&frames[6])
        ),
    )?;
    let layer = vignette_layer_id(&frames[6])
        .ok_or("The committed stack holds no Vignette layer")?
        .to_owned();
    let fit_bounds = pixels::bright_bounds(&frames[6], BOUNDS)?;
    let fit_corners = corner_luminances(&frames[6], fit_bounds)?;
    let fit_centre = centre_luminance(&frames[6], fit_bounds)?;
    for (index, (opened, darkened)) in opened_corners.iter().zip(fit_corners).enumerate() {
        ensure(
            *opened - darkened > DARKER,
            format!("Corner {index} did not darken: {opened} against {darkened}"),
        )?;
    }
    ensure(
        (opened_centre - fit_centre).abs() < SAME,
        format!("Amount -60 moved the unaffected centre: {opened_centre} against {fit_centre}"),
    )?;
    record(
        &frames[6],
        "released: one entry \"Vignette amount -60\" at Fit, every corner darker, the centre unaffected",
        json!({"revision": frames[6].revision()?, "label": frames[6].label()?, "corner_luminance": fit_corners, "centre_luminance": fit_centre, "layer": layer, "expanded": frames[6].expanded_sections(&SECTIONS)}),
    );

    // Frame 7: the same committed state at 100%. Nothing changed but the zoom.
    frames[7].expect_no_draft("Frame 7")?;
    ensure(
        frames[7].revision()? == frames[6].revision()?
            && frames[7].entry()? == frames[6].entry()?,
        "Changing zoom committed something",
    )?;
    ensure(
        frames[7]["step"]["request"] == json!({"view":{"zoom":100.0}}),
        format!(
            "Frame 7 did not request 100%: {}",
            frames[7]["step"]["request"]
        ),
    )?;
    let percent_bounds = pixels::bright_bounds(&frames[7], BOUNDS)?;
    let percent_corners = corner_luminances(&frames[7], percent_bounds)?;
    let percent_centre = centre_luminance(&frames[7], percent_bounds)?;
    // The near-centre patch is clear of the fixture's own quadrant labels at both zooms, so it
    // reads the same regardless of scale; a corner patch can sit close enough to a label at one
    // scale and not the other that resampling shifts its own reading, which is a rendering detail
    // of this label-bearing fixture, not a claim about the committed edit these frames share.
    ensure(
        (fit_centre - percent_centre).abs() < SAME,
        format!(
            "The unaffected centre reads differently at Fit and at 100%: {fit_centre} against {percent_centre}"
        ),
    )?;
    record(
        &frames[7],
        "the same committed Amount -60, at 100%",
        json!({"corner_luminance": percent_corners, "centre_luminance": percent_centre, "zoom_request": frames[7]["step"]["request"], "expanded": frames[7].expanded_sections(&SECTIONS)}),
    );

    // Frame 8: back to Fit, unchanged, ready for the roundness and feather commits.
    frames[8].expect_no_draft("Frame 8")?;
    ensure(
        frames[8].revision()? == frames[7].revision()?,
        "Returning to Fit committed something",
    )?;
    ensure(
        frames[8]["step"]["request"] == json!({"view":{"zoom":"fit"}}),
        format!(
            "Frame 8 did not request Fit: {}",
            frames[8]["step"]["request"]
        ),
    )?;
    record(
        &frames[8],
        "back to Fit",
        json!({"zoom_request": frames[8]["step"]["request"]}),
    );

    // Frame 9: Roundness -100, a rounded rectangle: at the default midpoint and feather, an edge
    // midpoint is beyond the falloff's outer bound and reads as darkened as a corner.
    ensure(
        frames[9].revision()? == frames[8].revision()? + 1,
        "The Roundness -100 commit did not advance the revision by one",
    )?;
    ensure(
        frames[9].label()? == "Vignette roundness -100",
        format!("Frame 9 is labelled {:?}", frames[9].label()?),
    )?;
    ensure(
        vignette_payload(&frames[9]) == Some(&json!({ AMOUNT: -60.0, ROUNDNESS: -100.0 })),
        format!(
            "The Roundness -100 layer holds {:?}",
            vignette_payload(&frames[9])
        ),
    )?;
    ensure(
        vignette_layer_id(&frames[9]) == Some(layer.as_str()),
        "Roundness replaced the Vignette layer instead of updating it",
    )?;
    let rect_bounds = pixels::bright_bounds(&frames[9], BOUNDS)?;
    let rect_edge = edge_luminance(&frames[9], rect_bounds)?;
    record(
        &frames[9],
        "Roundness -100 (a rounded rectangle) at Fit",
        json!({"label": frames[9].label()?, "edge_luminance": rect_edge, "expanded": frames[9].expanded_sections(&SECTIONS)}),
    );

    // Frame 10: Roundness +100, a circle: the same edge midpoint is well inside the falloff's inner
    // bound and reads brighter than it did as a rounded rectangle.
    ensure(
        frames[10].revision()? == frames[9].revision()? + 1,
        "The Roundness +100 commit did not advance the revision by one",
    )?;
    ensure(
        frames[10].label()? == "Vignette roundness +100",
        format!("Frame 10 is labelled {:?}", frames[10].label()?),
    )?;
    ensure(
        vignette_payload(&frames[10]) == Some(&json!({ AMOUNT: -60.0, ROUNDNESS: 100.0 })),
        format!(
            "The Roundness +100 layer holds {:?}",
            vignette_payload(&frames[10])
        ),
    )?;
    let circle_bounds = pixels::bright_bounds(&frames[10], BOUNDS)?;
    let circle_edge = edge_luminance(&frames[10], circle_bounds)?;
    ensure(
        circle_edge - rect_edge > DARKER,
        format!(
            "Roundness +100's edge midpoint is not brighter than -100's: {circle_edge} against {rect_edge}"
        ),
    )?;
    record(
        &frames[10],
        "Roundness +100 (a circle) at Fit: its edge midpoint reads brighter than the rectangle's did",
        json!({"label": frames[10].label()?, "edge_luminance": circle_edge}),
    );

    // Frame 11: Feather 0, a hard step at the midpoint radius: the same edge midpoint, beyond that
    // radius, is fully darkened.
    ensure(
        frames[11].revision()? == frames[10].revision()? + 1,
        "The Feather 0 commit did not advance the revision by one",
    )?;
    ensure(
        frames[11].label()? == "Vignette feather 0",
        format!("Frame 11 is labelled {:?}", frames[11].label()?),
    )?;
    ensure(
        vignette_payload(&frames[11])
            == Some(&json!({ AMOUNT: -60.0, ROUNDNESS: 100.0, FEATHER: 0.0 })),
        format!(
            "The Feather 0 layer holds {:?}",
            vignette_payload(&frames[11])
        ),
    )?;
    let hard_bounds = pixels::bright_bounds(&frames[11], BOUNDS)?;
    let hard_edge = edge_luminance(&frames[11], hard_bounds)?;
    record(
        &frames[11],
        "Feather 0 (a hard step) at Fit",
        json!({"label": frames[11].label()?, "edge_luminance": hard_edge}),
    );

    // Frame 12: Feather 100, the falloff spread from the centre to the corner: the same edge
    // midpoint is only partway through it and reads brighter than the hard step did.
    ensure(
        frames[12].revision()? == frames[11].revision()? + 1,
        "The Feather 100 commit did not advance the revision by one",
    )?;
    ensure(
        frames[12].label()? == "Vignette feather 100",
        format!("Frame 12 is labelled {:?}", frames[12].label()?),
    )?;
    ensure(
        vignette_payload(&frames[12])
            == Some(&json!({ AMOUNT: -60.0, ROUNDNESS: 100.0, FEATHER: 100.0 })),
        format!(
            "The Feather 100 layer holds {:?}",
            vignette_payload(&frames[12])
        ),
    )?;
    let soft_bounds = pixels::bright_bounds(&frames[12], BOUNDS)?;
    let soft_edge = edge_luminance(&frames[12], soft_bounds)?;
    ensure(
        soft_edge - hard_edge > DARKER,
        format!(
            "Feather 100's edge midpoint is not brighter than Feather 0's: {soft_edge} against {hard_edge}"
        ),
    )?;
    record(
        &frames[12],
        "Feather 100 (the widest falloff) at Fit: its edge midpoint reads brighter than the hard step did",
        json!({"label": frames[12].label()?, "edge_luminance": soft_edge}),
    );

    // Frame 13: a crop applied after the vignette already existed. The host still places the crop
    // layer before it, so the mask recentres on the cropped output stage: the new frame's own
    // corners read darker than its own near-centre patch, exactly as frame 6 did on the whole
    // photograph.
    ensure(
        frames[13].revision()? == frames[12].revision()? + 1,
        "The crop did not commit exactly one revision",
    )?;
    let effects = layer_effects(&frames[13]);
    let crop_position = effects
        .iter()
        .position(|effect| effect == lightwell_core::CROP_EFFECT)
        .ok_or("The crop commit added no crop layer")?;
    let vignette_position = effects
        .iter()
        .position(|effect| effect == lightwell_core::VIGNETTE_EFFECT)
        .ok_or("The crop commit lost the Vignette layer")?;
    ensure(
        crop_position < vignette_position,
        format!("The crop layer does not precede the Vignette layer: {effects:?}"),
    )?;
    ensure(
        vignette_layer_id(&frames[13]) == Some(layer.as_str()),
        "The crop replaced the Vignette layer instead of leaving it in place",
    )?;
    let cropped_bounds = pixels::bright_bounds(&frames[13], BOUNDS)?;
    let cropped_corners = corner_luminances(&frames[13], cropped_bounds)?;
    let cropped_centre = centre_luminance(&frames[13], cropped_bounds)?;
    // Each corner against its own un-vignetted baseline from frame 0 (same hue, same corner index),
    // not against a single shared centre reading: the fixture's four quadrant colours have very
    // different Rec. 709 luminance to begin with, so the same relative darkening moves each of them
    // by a different absolute amount, and a fixed threshold shared across hues is not the claim
    // this check makes. A 1:1 crop keeps the full height and trims width symmetrically, so each
    // corner of the crop is still deep in its own quadrant's flat colour.
    for (index, (opened, cropped)) in opened_corners.iter().zip(cropped_corners).enumerate() {
        ensure(
            *opened - cropped > DARKER,
            format!(
                "Corner {index} of the cropped frame is not darker than its own un-vignetted baseline, so the mask did not recentre: {opened} against {cropped}"
            ),
        )?;
    }
    record(
        &frames[13],
        "a 1:1 crop applied after the vignette: the mask recentres on the cropped output stage",
        json!({"corner_luminance": cropped_corners, "centre_luminance": cropped_centre, "layers": effects}),
    );

    // Frame 14: the module's own header reset. One entry labelled "Reset Vignette"; the layer is
    // kept at its all-default payload, and the crop from the previous step is untouched.
    ensure(
        frames[14].revision()? == frames[13].revision()? + 1,
        "The module reset did not commit exactly one revision",
    )?;
    ensure(
        frames[14].label()? == "Reset Vignette",
        format!("The module reset is labelled {:?}", frames[14].label()?),
    )?;
    ensure(
        vignette_payload(&frames[14]) == Some(&json!({})),
        format!(
            "The reset layer holds {:?}, expected the all-default payload",
            vignette_payload(&frames[14])
        ),
    )?;
    ensure(
        vignette_layer_id(&frames[14]) == Some(layer.as_str()),
        "The module reset replaced the Vignette layer instead of keeping it",
    )?;
    let reset_effects = layer_effects(&frames[14]);
    ensure(
        reset_effects.contains(&lightwell_core::CROP_EFFECT.to_owned()),
        "The module reset lost the crop from the previous step",
    )?;
    record(
        &frames[14],
        "the module's own header reset: entry \"Reset Vignette\", the layer kept and neutral",
        json!({"label": frames[14].label()?, "payload": vignette_payload(&frames[14]), "layer": layer, "expanded": frames[14].expanded_sections(&SECTIONS)}),
    );

    write_json(
        &evidence.join("vignette-checks.json"),
        &json!({
            "checks": checks,
            "darker_margin": DARKER,
            "same_tolerance": SAME,
            "sample_geometry": {"corner_inset": CORNER_INSET, "centre_offset": CENTRE_OFFSET, "edge_x": EDGE_X, "edge_y_inset": EDGE_Y_INSET},
            "scope": "Mean Rec. 709 luminance of small patches, read back from the renderer; a mask-shape and recentring demonstration, not a colorimetric claim",
        }),
    )?;
    Ok(())
}
