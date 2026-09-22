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
    smoke::{columns, frame_identity, longest_run},
    *,
};

const VIGNETTE_MODULE: &str = "lightwell.vignette";
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
    (scenario == "vignette").then_some(12)
}

pub fn source(scenario: &str) -> Option<&'static str> {
    (scenario == "vignette").then_some(FIXTURE)
}

/// The evidence script. Each step is one gesture, one request or one decision; `verify` below
/// checks exactly what each one is supposed to prove.
pub fn script(scenario: &str) -> Option<Value> {
    (scenario == "vignette").then(|| {
        json!([
            // 1: expand the section. Its one group starts expanded, so this alone exposes it.
            {"section":{"module":VIGNETTE_MODULE,"expanded":true}},
            // 2: a drag on Amount, left open: the frame shows the drafted preview.
            {"slider":{"action":SET_VIGNETTE,"parameter":AMOUNT,"values":[-20.0,-40.0,-60.0]}},
            // 3: the same gesture released: one entry, committed at Fit.
            {"slider":{"action":SET_VIGNETTE,"parameter":AMOUNT,"values":[-60.0],"release":true}},
            // 4: the same committed state at 100%.
            {"view":{"zoom":"100"}},
            // 5: back to Fit for the roundness and feather commits below.
            {"view":{"zoom":"fit"}},
            // 6: Roundness -100, a rounded rectangle.
            {"slider":{"action":SET_VIGNETTE,"parameter":ROUNDNESS,"values":[-100.0],"release":true}},
            // 7: Roundness +100, a circle.
            {"slider":{"action":SET_VIGNETTE,"parameter":ROUNDNESS,"values":[100.0],"release":true}},
            // 8: Feather 0, a hard step.
            {"slider":{"action":SET_VIGNETTE,"parameter":FEATHER,"values":[0.0],"release":true}},
            // 9: Feather 100, the widest falloff.
            {"slider":{"action":SET_VIGNETTE,"parameter":FEATHER,"values":[100.0],"release":true}},
            // 10: a crop applied after the vignette already exists; the host places the crop layer
            // before it regardless, so the mask recentres on the cropped output stage.
            {"api":{"method":"edit.crop-fit","params":{"aspect":"1:1","angle":0}}},
            // 11: the module's own header reset.
            {"reset":{"module":VIGNETTE_MODULE}}
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

fn expanded(frame: &Value) -> bool {
    frame["state"]["expanded"][VIGNETTE_MODULE] == json!(true)
}

fn vignette_payload(frame: &Value) -> Option<&Value> {
    frame["state"]["stack"]["layers"]
        .as_array()?
        .iter()
        .find(|layer| layer["effect"] == json!(lightwell_core::VIGNETTE_EFFECT))
        .map(|layer| &layer["payload"])
}

fn vignette_layer_id(frame: &Value) -> Option<&str> {
    frame["state"]["stack"]["layers"]
        .as_array()?
        .iter()
        .find(|layer| layer["effect"] == json!(lightwell_core::VIGNETTE_EFFECT))
        .and_then(|layer| layer["id"].as_str())
}

/// The stack's layer identities in stored order, so the crop-recentre frame can prove the crop
/// layer the host inserted sits before the vignette layer it recentres.
fn layer_effects(frame: &Value) -> Vec<String> {
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

fn vignette_field<'a>(frame: &'a Value, name: &str) -> Result<&'a str> {
    frame["state"]["controls"][format!("{SET_VIGNETTE}.{name}")]
        .as_str()
        .ok_or_else(|| format!("Frame records no {name} field").into())
}

/// Where the photograph is drawn: found by the row with the widest run of bright pixels (an
/// accurate horizontal extent no title-bar or mode-strip chrome can win, since none of it spans as
/// wide as the photograph itself, and a rectangular fixture's own least-vignetted row — the one
/// through its own vertical centre — is always at least as wide as any other) and the column with
/// the tallest run, the same way for the vertical extent. This works whether the zoom centres the
/// photograph (Fit) or anchors it to the photo surface's own top-left corner, which 100% does for
/// a photograph smaller than the canvas.
fn bright_bounds(path: &Path, frame: &Value) -> Result<[u32; 4]> {
    const BRIGHT: u32 = 60;
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
    let bright = |p: [u8; 3]| (u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) / 3 >= BRIGHT;
    // Clear of the title bar above and the mode strip / status line below, and of the divider that
    // marks each edge of the photo surface itself, so none of those is ever scanned as a row or a
    // column: the status line's own background is a flat mid-grey bright enough to win a whole row,
    // and the divider is a thin, full-height line exactly at the surface's own boundary.
    let top_margin = (height / 20).max(20);
    let bottom_margin = height - top_margin;
    let side_inset = 10;
    let (inset_left, inset_right) = (surface_left + side_inset, surface_right - side_inset);
    let mut widest: Option<(u32, u32, u32)> = None;
    for y in top_margin..bottom_margin {
        let run =
            longest_run((inset_left..inset_right).map(|x| (x, bright(image.get_pixel(x, y).0))));
        if let Some((left, right)) = run
            && widest.is_none_or(|(w, ..)| right - left > w)
        {
            widest = Some((right - left, left, right));
        }
    }
    let (_, left, right) = widest.ok_or("No photograph in the frame: blank or wrong render")?;
    let mut tallest: Option<(u32, u32, u32)> = None;
    for x in inset_left..inset_right {
        let run =
            longest_run((top_margin..bottom_margin).map(|y| (y, bright(image.get_pixel(x, y).0))));
        if let Some((top, bottom)) = run
            && tallest.is_none_or(|(h, ..)| bottom - top > h)
        {
            tallest = Some((bottom - top, top, bottom));
        }
    }
    let (_, top, bottom) = tallest.ok_or("No photograph in the frame: blank or wrong render")?;
    Ok([left, top, right, bottom])
}

fn luminance(pixel: [u8; 3]) -> f64 {
    0.2126 * f64::from(pixel[0]) + 0.7152 * f64::from(pixel[1]) + 0.0722 * f64::from(pixel[2])
}

/// The darkest pixel in a small patch at fraction `(fx, fy)` of `bounds`. The fixture draws its
/// quadrant labels in white, so a mean over the patch can read bright when a sample point happens
/// to sit close to a label stroke; the darkest pixel instead reads the plain background colour
/// underneath, which is what every check here actually means by "this point of the mask".
fn patch_luminance(path: &Path, bounds: [u32; 4], fx: f64, fy: f64) -> Result<f64> {
    let image = image::open(path)?.to_rgb8();
    let [left, top, right, bottom] = bounds;
    let px = f64::from(left) + fx * f64::from(right - left);
    let py = f64::from(top) + fy * f64::from(bottom - top);
    let (width, height) = image.dimensions();
    let mut darkest = f64::INFINITY;
    let mut count = 0u32;
    for dy in -PATCH_HALF..=PATCH_HALF {
        for dx in -PATCH_HALF..=PATCH_HALF {
            let x = (px as i64 + dx).clamp(0, i64::from(width) - 1) as u32;
            let y = (py as i64 + dy).clamp(0, i64::from(height) - 1) as u32;
            darkest = darkest.min(luminance(image.get_pixel(x, y).0));
            count += 1;
        }
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(darkest)
}

/// The four corners, inset by [`CORNER_INSET`], each safely in the mask's fully darkened zone at
/// every parameter combination this scenario commits.
fn corner_luminances(path: &Path, bounds: [u32; 4]) -> Result<[f64; 4]> {
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
        values[index] = patch_luminance(path, bounds, fx, fy)?;
    }
    Ok(values)
}

fn centre_luminance(path: &Path, bounds: [u32; 4]) -> Result<f64> {
    patch_luminance(path, bounds, 0.5 - CENTRE_OFFSET, 0.5 - CENTRE_OFFSET)
}

/// The mean of the top and bottom edge-midpoint patches: see the module doc for why roundness and
/// feather each move this reading in a known direction.
fn edge_luminance(path: &Path, bounds: [u32; 4]) -> Result<f64> {
    let top = patch_luminance(path, bounds, EDGE_X, EDGE_Y_INSET)?;
    let bottom = patch_luminance(path, bounds, EDGE_X, 1.0 - EDGE_Y_INSET)?;
    Ok((top + bottom) / 2.0)
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
        !expanded(&frames[0]),
        "The Vignette section is not collapsed as launched",
    )?;
    ensure(
        vignette_field(&frames[0], AMOUNT)? == "0",
        format!(
            "Amount does not start neutral: {}",
            vignette_field(&frames[0], AMOUNT)?
        ),
    )?;
    expect_no_draft(&frames[0], "Frame 0")?;
    ensure(
        vignette_payload(&frames[0]).is_none(),
        "The opened stack already holds a Vignette layer",
    )?;
    let opened_bounds = bright_bounds(&paths[0], &frames[0])?;
    let opened_corners = corner_luminances(&paths[0], opened_bounds)?;
    let opened_centre = centre_luminance(&paths[0], opened_bounds)?;
    record(
        &frames[0],
        "the collapsed Vignette section as launched",
        json!({"corner_luminance": opened_corners, "centre_luminance": opened_centre}),
    );

    // Frame 1: the section expanded. Its one group starts expanded, so its four sliders show.
    ensure(
        expanded(&frames[1]),
        "The section step did not expand the Vignette section",
    )?;
    ensure(
        revision(&frames[1])? == revision(&frames[0])?,
        "Expanding the section committed something",
    )?;
    record(
        &frames[1],
        "the Vignette section expanded",
        json!({"expanded": expanded(&frames[1])}),
    );

    // Frame 2: mid-gesture at Amount -60. The draft is open, nothing is committed.
    let drafted = draft(&frames[2]);
    ensure(
        drafted["action"] == json!(SET_VIGNETTE)
            && drafted["fields"] == json!({ AMOUNT: -60.0 })
            && drafted["conflicted"] == json!(false),
        format!("Frame 2's draft is not the open Amount gesture: {drafted}"),
    )?;
    ensure(
        frames[2]["state"]["displayed_draft_revision"] == drafted["draft_revision"],
        format!(
            "Frame 2 displays draft revision {} while the draft is at {}",
            frames[2]["state"]["displayed_draft_revision"], drafted["draft_revision"]
        ),
    )?;
    ensure(
        revision(&frames[2])? == revision(&frames[1])? && vignette_payload(&frames[2]).is_none(),
        "A drag committed something",
    )?;
    ensure(
        vignette_field(&frames[2], AMOUNT)? == "-60",
        format!(
            "The slider does not show the drafted value: {}",
            vignette_field(&frames[2], AMOUNT)?
        ),
    )?;
    record(
        &frames[2],
        "a drag to Amount -60, mid-gesture: the drafted preview",
        json!({"draft": drafted}),
    );

    // Frame 3: the release. One entry, labelled by the module, at Fit; every corner is darker than
    // it was at the opened baseline, and the near-centre patch is unaffected.
    expect_no_draft(&frames[3], "Frame 3")?;
    ensure(
        revision(&frames[3])? == revision(&frames[2])? + 1,
        "The release did not advance the revision by one",
    )?;
    ensure(
        entry(&frames[3])? != entry(&frames[1])?,
        "The release created no new history entry",
    )?;
    ensure(
        label(&frames[3])? == "Amount -60",
        format!("The committed entry is labelled {:?}", label(&frames[3])?),
    )?;
    ensure(
        vignette_payload(&frames[3]) == Some(&json!({ AMOUNT: -60.0 })),
        format!(
            "The committed Vignette layer holds {:?}",
            vignette_payload(&frames[3])
        ),
    )?;
    let layer = vignette_layer_id(&frames[3])
        .ok_or("The committed stack holds no Vignette layer")?
        .to_owned();
    let fit_bounds = bright_bounds(&paths[3], &frames[3])?;
    let fit_corners = corner_luminances(&paths[3], fit_bounds)?;
    let fit_centre = centre_luminance(&paths[3], fit_bounds)?;
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
        &frames[3],
        "released: one entry \"Amount -60\" at Fit, every corner darker, the centre unaffected",
        json!({"revision": revision(&frames[3])?, "label": label(&frames[3])?, "corner_luminance": fit_corners, "centre_luminance": fit_centre, "layer": layer}),
    );

    // Frame 4: the same committed state at 100%. Nothing changed but the zoom.
    expect_no_draft(&frames[4], "Frame 4")?;
    ensure(
        revision(&frames[4])? == revision(&frames[3])? && entry(&frames[4])? == entry(&frames[3])?,
        "Changing zoom committed something",
    )?;
    ensure(
        frames[4]["step"]["request"] == json!({"view":{"zoom":100.0}}),
        format!(
            "Frame 4 did not request 100%: {}",
            frames[4]["step"]["request"]
        ),
    )?;
    let percent_bounds = bright_bounds(&paths[4], &frames[4])?;
    let percent_corners = corner_luminances(&paths[4], percent_bounds)?;
    let percent_centre = centre_luminance(&paths[4], percent_bounds)?;
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
        &frames[4],
        "the same committed Amount -60, at 100%",
        json!({"corner_luminance": percent_corners, "centre_luminance": percent_centre, "zoom_request": frames[4]["step"]["request"]}),
    );

    // Frame 5: back to Fit, unchanged, ready for the roundness and feather commits.
    expect_no_draft(&frames[5], "Frame 5")?;
    ensure(
        revision(&frames[5])? == revision(&frames[4])?,
        "Returning to Fit committed something",
    )?;
    ensure(
        frames[5]["step"]["request"] == json!({"view":{"zoom":"fit"}}),
        format!(
            "Frame 5 did not request Fit: {}",
            frames[5]["step"]["request"]
        ),
    )?;
    record(
        &frames[5],
        "back to Fit",
        json!({"zoom_request": frames[5]["step"]["request"]}),
    );

    // Frame 6: Roundness -100, a rounded rectangle: at the default midpoint and feather, an edge
    // midpoint is beyond the falloff's outer bound and reads as darkened as a corner.
    ensure(
        revision(&frames[6])? == revision(&frames[5])? + 1,
        "The Roundness -100 commit did not advance the revision by one",
    )?;
    ensure(
        label(&frames[6])? == "Roundness -100",
        format!("Frame 6 is labelled {:?}", label(&frames[6])?),
    )?;
    ensure(
        vignette_payload(&frames[6]) == Some(&json!({ AMOUNT: -60.0, ROUNDNESS: -100.0 })),
        format!(
            "The Roundness -100 layer holds {:?}",
            vignette_payload(&frames[6])
        ),
    )?;
    ensure(
        vignette_layer_id(&frames[6]) == Some(layer.as_str()),
        "Roundness replaced the Vignette layer instead of updating it",
    )?;
    let rect_bounds = bright_bounds(&paths[6], &frames[6])?;
    let rect_edge = edge_luminance(&paths[6], rect_bounds)?;
    record(
        &frames[6],
        "Roundness -100 (a rounded rectangle) at Fit",
        json!({"label": label(&frames[6])?, "edge_luminance": rect_edge}),
    );

    // Frame 7: Roundness +100, a circle: the same edge midpoint is well inside the falloff's inner
    // bound and reads brighter than it did as a rounded rectangle.
    ensure(
        revision(&frames[7])? == revision(&frames[6])? + 1,
        "The Roundness +100 commit did not advance the revision by one",
    )?;
    ensure(
        label(&frames[7])? == "Roundness +100",
        format!("Frame 7 is labelled {:?}", label(&frames[7])?),
    )?;
    ensure(
        vignette_payload(&frames[7]) == Some(&json!({ AMOUNT: -60.0, ROUNDNESS: 100.0 })),
        format!(
            "The Roundness +100 layer holds {:?}",
            vignette_payload(&frames[7])
        ),
    )?;
    let circle_bounds = bright_bounds(&paths[7], &frames[7])?;
    let circle_edge = edge_luminance(&paths[7], circle_bounds)?;
    ensure(
        circle_edge - rect_edge > DARKER,
        format!(
            "Roundness +100's edge midpoint is not brighter than -100's: {circle_edge} against {rect_edge}"
        ),
    )?;
    record(
        &frames[7],
        "Roundness +100 (a circle) at Fit: its edge midpoint reads brighter than the rectangle's did",
        json!({"label": label(&frames[7])?, "edge_luminance": circle_edge}),
    );

    // Frame 8: Feather 0, a hard step at the midpoint radius: the same edge midpoint, beyond that
    // radius, is fully darkened.
    ensure(
        revision(&frames[8])? == revision(&frames[7])? + 1,
        "The Feather 0 commit did not advance the revision by one",
    )?;
    ensure(
        label(&frames[8])? == "Feather 0",
        format!("Frame 8 is labelled {:?}", label(&frames[8])?),
    )?;
    ensure(
        vignette_payload(&frames[8])
            == Some(&json!({ AMOUNT: -60.0, ROUNDNESS: 100.0, FEATHER: 0.0 })),
        format!(
            "The Feather 0 layer holds {:?}",
            vignette_payload(&frames[8])
        ),
    )?;
    let hard_bounds = bright_bounds(&paths[8], &frames[8])?;
    let hard_edge = edge_luminance(&paths[8], hard_bounds)?;
    record(
        &frames[8],
        "Feather 0 (a hard step) at Fit",
        json!({"label": label(&frames[8])?, "edge_luminance": hard_edge}),
    );

    // Frame 9: Feather 100, the falloff spread from the centre to the corner: the same edge
    // midpoint is only partway through it and reads brighter than the hard step did.
    ensure(
        revision(&frames[9])? == revision(&frames[8])? + 1,
        "The Feather 100 commit did not advance the revision by one",
    )?;
    ensure(
        label(&frames[9])? == "Feather 100",
        format!("Frame 9 is labelled {:?}", label(&frames[9])?),
    )?;
    ensure(
        vignette_payload(&frames[9])
            == Some(&json!({ AMOUNT: -60.0, ROUNDNESS: 100.0, FEATHER: 100.0 })),
        format!(
            "The Feather 100 layer holds {:?}",
            vignette_payload(&frames[9])
        ),
    )?;
    let soft_bounds = bright_bounds(&paths[9], &frames[9])?;
    let soft_edge = edge_luminance(&paths[9], soft_bounds)?;
    ensure(
        soft_edge - hard_edge > DARKER,
        format!(
            "Feather 100's edge midpoint is not brighter than Feather 0's: {soft_edge} against {hard_edge}"
        ),
    )?;
    record(
        &frames[9],
        "Feather 100 (the widest falloff) at Fit: its edge midpoint reads brighter than the hard step did",
        json!({"label": label(&frames[9])?, "edge_luminance": soft_edge}),
    );

    // Frame 10: a crop applied after the vignette already existed. The host still places the crop
    // layer before it, so the mask recentres on the cropped output stage: the new frame's own
    // corners read darker than its own near-centre patch, exactly as frame 3 did on the whole
    // photograph.
    ensure(
        revision(&frames[10])? == revision(&frames[9])? + 1,
        "The crop did not commit exactly one revision",
    )?;
    let effects = layer_effects(&frames[10]);
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
        vignette_layer_id(&frames[10]) == Some(layer.as_str()),
        "The crop replaced the Vignette layer instead of leaving it in place",
    )?;
    let cropped_bounds = bright_bounds(&paths[10], &frames[10])?;
    let cropped_corners = corner_luminances(&paths[10], cropped_bounds)?;
    let cropped_centre = centre_luminance(&paths[10], cropped_bounds)?;
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
        &frames[10],
        "a 1:1 crop applied after the vignette: the mask recentres on the cropped output stage",
        json!({"corner_luminance": cropped_corners, "centre_luminance": cropped_centre, "layers": effects}),
    );

    // Frame 11: the module's own header reset. One entry labelled "Reset Vignette"; the layer is
    // kept at its all-default payload, and the crop from the previous step is untouched.
    ensure(
        revision(&frames[11])? == revision(&frames[10])? + 1,
        "The module reset did not commit exactly one revision",
    )?;
    ensure(
        label(&frames[11])? == "Reset Vignette",
        format!("The module reset is labelled {:?}", label(&frames[11])?),
    )?;
    ensure(
        vignette_payload(&frames[11]) == Some(&json!({})),
        format!(
            "The reset layer holds {:?}, expected the all-default payload",
            vignette_payload(&frames[11])
        ),
    )?;
    ensure(
        vignette_layer_id(&frames[11]) == Some(layer.as_str()),
        "The module reset replaced the Vignette layer instead of keeping it",
    )?;
    let reset_effects = layer_effects(&frames[11]);
    ensure(
        reset_effects.contains(&lightwell_core::CROP_EFFECT.to_owned()),
        "The module reset lost the crop from the previous step",
    )?;
    record(
        &frames[11],
        "the module's own header reset: entry \"Reset Vignette\", the layer kept and neutral",
        json!({"label": label(&frames[11])?, "payload": vignette_payload(&frames[11]), "layer": layer}),
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
