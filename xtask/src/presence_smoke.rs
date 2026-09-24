//! The `presence` smoke scenario: the section's own expand, a Clarity drag and cancel, Texture,
//! Clarity and Dehaze each committed, all three at once through the raw API, and the module reset,
//! on the real editor.
//!
//! The fixture is `fixtures/generated/presence.jpg` (`cargo xtask generate-fixtures`): the golden
//! `orientation-1` fixture has only hard flat-colour edges and flat fields, with no smooth gradient
//! and nothing at Texture's own fine medium-frequency band, so none of the three units has anything
//! to visibly act on there. The generated fixture holds, in one quadrant each: a smooth gradient, a
//! hard step edge (for Clarity's local-contrast gain and, at 100%, its halo), a low-amplitude
//! checker at Texture's own frozen fine/coarse radii for this fixture's long side (a *high*-contrast
//! checker reads as an edge to the frozen guided-filter pair and is left alone, per
//! `docs/design/presence-study.md`; a period-2 checker is not a usable probe either, so the fixture
//! uses an 8-pixel period, the widest-gain period this study's units produce at this fixture's long
//! side), and a flat mid-grey deep enough in from every edge to clear Clarity's own reduced-grid base
//! radius, which the "flat field stays flat" and "grey stays grey" checks below read.
use crate::{
    scenario::{Bright, Frame, Scan, pixels},
    *,
};

const PRESENCE_MODULE: &str = "lightwell.presence";
/// The one section the registry lists above Presence that is both a real toggleable section
/// (Pixel declares none) and expanded by its own descriptor's default: collapsed first, so the
/// module's own three sliders land on screen without scrolling.
const BASIC_MODULE: &str = "lightwell.basic";
const SET_PRESENCE: &str = "set-presence";
const TEXTURE: &str = "texture";
const CLARITY: &str = "clarity";
const DEHAZE: &str = "dehaze";
pub const FIXTURE: &str = "fixtures/generated/presence.jpg";

/// Fractional source positions, in the same `[0, 1]` space the generated fixture is drawn in, of
/// the sample points below. `crate::fixtures` keeps the fixture private to that module; these
/// mirror its own quadrant layout as plain fractions.
/// The step edge's low (pre-edge) and high (post-edge) sample points: 20 px either side of the edge
/// at x = 0.75, close enough to sit inside Clarity's own local-contrast overshoot (measured against
/// the real render: the gap barely moved at the 36 px offset a first attempt at this fixture used),
/// clear of the sampled patch's own half-width so it never crosses the edge itself.
const EDGE_LOW_X: f64 = 0.75 - 20.0 / 1440.0;
const EDGE_HIGH_X: f64 = 0.75 + 20.0 / 1440.0;
const EDGE_Y: f64 = 0.25;
/// A patch inside the fine checker, away from its own quadrant seams.
const TEXTURE_X: f64 = 0.25;
const TEXTURE_Y: f64 = 0.75;
/// Deep in the flat quadrant's own interior, clear of every edge by more than Clarity's own base
/// radius at this fixture's long side.
const FLAT_X: f64 = 0.75;
const FLAT_Y: f64 = 0.75;
/// Half the side length, in pixels, of a sampled patch.
const PATCH_HALF: i64 = 10;

/// How far the edge contrast (the gap between the low and high edge samples) must widen before
/// this scenario calls Clarity's local-contrast gain visible. Measured against the real render at
/// this fixture's own geometry, Clarity +100 widens it by about 7.2; the margin here sits below
/// that and comfortably above `UNCHANGED`'s own noise floor.
const CONTRAST_WIDER: f64 = 5.0;
/// How far the texture range (the gap between the checker's own light and dark cells in a small
/// patch) must widen before this scenario calls Texture's band gain visible.
const RANGE_WIDER: f64 = 4.0;
/// How close a reading must stay to its own baseline before this scenario calls it unaffected: the
/// flat quadrant's own mean under Texture or Clarity, and every sample's own channel spread (a
/// grey point staying grey, whatever its brightness does under Dehaze).
const UNCHANGED: f64 = 3.0;

/// The evidence script. Each step is one gesture, one request or one decision; `verify` below
/// checks exactly what each one is supposed to prove.
pub fn script(scenario: &str) -> Option<Value> {
    (scenario == "presence").then(|| {
        json!([
            // 1: collapse Basic, expanded by its own default and the one other section the
            // registry lists above Presence, so the module's own sliders land on screen without
            // scrolling once it expands.
            {"section":{"module":BASIC_MODULE,"expanded":false}},
            // 2: expand the section: its one group and three sliders.
            {"section":{"module":PRESENCE_MODULE,"expanded":true}},
            // 3: a drag on Clarity, left open: the frame shows the drafted preview.
            {"slider":{"action":SET_PRESENCE,"parameter":CLARITY,"values":[30.0,60.0,90.0]}},
            // 4: Escape cancels the gesture; nothing commits from it.
            {"slider":{"action":SET_PRESENCE,"parameter":CLARITY,"values":[90.0],"cancel":true}},
            // 5: Texture +100, committed at Fit.
            {"slider":{"action":SET_PRESENCE,"parameter":TEXTURE,"values":[100.0],"release":true}},
            // 6: the same committed state at 100%, where the checker's own fine detail shows.
            {"view":{"zoom":"100"}},
            // 7: back to Fit for the Clarity commit below.
            {"view":{"zoom":"fit"}},
            // 8: Clarity +100, committed at Fit, merged with Texture.
            {"slider":{"action":SET_PRESENCE,"parameter":CLARITY,"values":[100.0],"release":true}},
            // 9: the same committed state at 100%, where the step edge's own halo shows.
            {"view":{"zoom":"100"}},
            // 10: back to Fit for the Dehaze commits below.
            {"view":{"zoom":"fit"}},
            // 11: Dehaze +100, committed at Fit, merged with Texture and Clarity.
            {"slider":{"action":SET_PRESENCE,"parameter":DEHAZE,"values":[100.0],"release":true}},
            // 12: Dehaze -100, committed at Fit: the same one field flips sign.
            {"slider":{"action":SET_PRESENCE,"parameter":DEHAZE,"values":[-100.0],"release":true}},
            // 13: all three at once, through the raw API a generated slider cannot reach (each
            // submits its own one field only): the same "everything is programmable" parity every
            // other scenario's own API step proves.
            {"api":{"method":"edit.set-presence","params":{"texture":100.0,"clarity":100.0,"dehaze":100.0}}},
            // 14: the module's own header reset.
            {"reset":{"module":PRESENCE_MODULE}}
        ])
    })
}

/// Every section this scenario toggles, for the correlation every recorded frame carries alongside
/// its revision, entry and draft.
const SECTIONS: [&str; 2] = [BASIC_MODULE, PRESENCE_MODULE];

fn presence_payload(frame: &Frame) -> Option<&Value> {
    frame.payload(lightwell_core::PRESENCE_EFFECT)
}

fn presence_layer_id(frame: &Frame) -> Option<&str> {
    frame.layer_id(lightwell_core::PRESENCE_EFFECT)
}

fn presence_field<'a>(frame: &'a Frame, name: &str) -> Result<&'a str> {
    frame.field(SET_PRESENCE, name)
}

/// Where the photograph is drawn: found by the row with the widest run of bright pixels and the
/// column with the tallest, both inset from the surface's own edge dividers and clear of the title
/// bar and mode strip / status line. Every level this fixture draws is kept at 40 or above, so a low
/// threshold finds the whole rectangle without mistaking the dark canvas background around it for
/// content. The row is measured first, a scan [`Scan::Widest`] records the mode strip can fool.
const BOUNDS: Bright = Bright {
    threshold: 32,
    scan: Scan::Widest,
    least: None,
};

/// The mean RGB of a small patch at fraction `(fx, fy)` of `bounds`.
fn patch_mean(frame: &Frame, bounds: [u32; 4], fx: f64, fy: f64) -> Result<[f64; 3]> {
    pixels::mean_rgb(frame.image()?, pixels::at(bounds, [fx, fy]), PATCH_HALF)
}

/// The min and max pixel value in the same patch `patch_mean` reads, over all three channels
/// together (every sample here is neutral grey, so the three channels agree): the range a texture
/// or contrast gain widens.
fn patch_range(frame: &Frame, bounds: [u32; 4], fx: f64, fy: f64) -> Result<(f64, f64)> {
    Ok(pixels::grey_range(
        frame.image()?,
        pixels::at(bounds, [fx, fy]),
        PATCH_HALF,
    ))
}

fn channel_spread(rgb: [f64; 3]) -> f64 {
    let max = rgb.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let min = rgb.iter().cloned().fold(f64::INFINITY, f64::min);
    max - min
}

fn mean(rgb: [f64; 3]) -> f64 {
    (rgb[0] + rgb[1] + rgb[2]) / 3.0
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

    // Frame 0: the fixture opens with the Presence section listed and collapsed, every field at
    // its default, no draft and no Presence layer yet.
    let presence = frames[0]["state"]["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|module| module["id"] == json!(PRESENCE_MODULE))
        .ok_or("The Presence module is not listed at all")?;
    ensure(
        presence["available"] == json!(true),
        "The Presence module is not available",
    )?;
    ensure(
        !frames[0].section_expanded(PRESENCE_MODULE),
        "The Presence section is not collapsed as launched",
    )?;
    ensure(
        presence_field(&frames[0], CLARITY)? == "0",
        format!(
            "Clarity does not start neutral: {}",
            presence_field(&frames[0], CLARITY)?
        ),
    )?;
    frames[0].expect_no_draft("Frame 0")?;
    ensure(
        presence_payload(&frames[0]).is_none(),
        "The opened stack already holds a Presence layer",
    )?;
    let opened_bounds = pixels::bright_bounds(&frames[0], BOUNDS)?;
    let opened_edge_low = patch_mean(&frames[0], opened_bounds, EDGE_LOW_X, EDGE_Y)?;
    let opened_edge_high = patch_mean(&frames[0], opened_bounds, EDGE_HIGH_X, EDGE_Y)?;
    let opened_contrast = mean(opened_edge_high) - mean(opened_edge_low);
    let (opened_texture_lo, opened_texture_hi) =
        patch_range(&frames[0], opened_bounds, TEXTURE_X, TEXTURE_Y)?;
    let opened_texture_range = opened_texture_hi - opened_texture_lo;
    let opened_flat = patch_mean(&frames[0], opened_bounds, FLAT_X, FLAT_Y)?;
    ensure(
        channel_spread(opened_flat) < UNCHANGED,
        format!("The flat quadrant does not open grey: {opened_flat:?}"),
    )?;
    record(
        &frames[0],
        "the collapsed Presence section as launched",
        json!({
            "edge_contrast": opened_contrast,
            "texture_range": opened_texture_range,
            "flat_mean": mean(opened_flat),
            "expanded": frames[0].expanded_sections(&SECTIONS),
        }),
    );

    // Frame 1: Basic collapsed, so nothing above Presence is expanded once it opens.
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
        "the Basic section collapsed, above Presence in the registry order",
        json!({"expanded": frames[1].expanded_sections(&SECTIONS)}),
    );

    // Frame 2: the section expanded, with Basic still collapsed above it: Texture, Clarity and
    // Dehaze on screen without scrolling.
    ensure(
        frames[2].section_expanded(PRESENCE_MODULE) && !frames[2].section_expanded(BASIC_MODULE),
        format!(
            "The section step did not expand Presence alone: {}",
            frames[2].expanded_sections(&SECTIONS)
        ),
    )?;
    ensure(
        frames[2].revision()? == frames[1].revision()?,
        "Expanding the section committed something",
    )?;
    record(
        &frames[2],
        "the Presence section expanded: Texture, Clarity and Dehaze, on screen with nothing above it expanded",
        json!({"expanded": frames[2].expanded_sections(&SECTIONS)}),
    );

    // Frame 3: mid-gesture at Clarity +90. The draft is open, nothing is committed.
    let drafted = frames[3].draft();
    ensure(
        drafted["action"] == json!(SET_PRESENCE)
            && drafted["fields"] == json!({ CLARITY: 90.0 })
            && drafted["conflicted"] == json!(false),
        format!("Frame 3's draft is not the open Clarity gesture: {drafted}"),
    )?;
    ensure(
        frames[3].revision()? == frames[2].revision()? && presence_payload(&frames[3]).is_none(),
        "A drag committed something",
    )?;
    ensure(
        presence_field(&frames[3], CLARITY)? == "90",
        format!(
            "The slider does not show the drafted value: {}",
            presence_field(&frames[3], CLARITY)?
        ),
    )?;
    record(
        &frames[3],
        "a drag to Clarity +90, mid-gesture: the drafted preview",
        json!({"draft": drafted, "expanded": frames[3].expanded_sections(&SECTIONS)}),
    );

    // Frame 4: Escape. The gesture ends with nothing committed, and the field returns to what the
    // stack (still empty) actually holds.
    frames[4].expect_no_draft("Frame 4")?;
    ensure(
        frames[4].revision()? == frames[3].revision()?
            && frames[4].entry()? == frames[2].entry()?,
        "Cancelling the gesture committed something",
    )?;
    ensure(
        presence_field(&frames[4], CLARITY)? == "0",
        format!(
            "The slider did not return to the authoritative value: {}",
            presence_field(&frames[4], CLARITY)?
        ),
    )?;
    record(
        &frames[4],
        "Escape cancels the Clarity gesture: nothing committed",
        json!({"revision": frames[4].revision()?}),
    );

    // Frame 5: Texture +100, committed at Fit.
    ensure(
        frames[5].revision()? == frames[4].revision()? + 1,
        "The Texture commit did not advance the revision by one",
    )?;
    ensure(
        frames[5].label()? == "Texture +100",
        format!("Frame 5 is labelled {:?}", frames[5].label()?),
    )?;
    ensure(
        presence_payload(&frames[5]) == Some(&json!({ TEXTURE: 100.0 })),
        format!(
            "The committed Presence layer holds {:?}",
            presence_payload(&frames[5])
        ),
    )?;
    let layer = presence_layer_id(&frames[5])
        .ok_or("The committed stack holds no Presence layer")?
        .to_owned();
    let fit5_bounds = pixels::bright_bounds(&frames[5], BOUNDS)?;
    let fit5_flat = patch_mean(&frames[5], fit5_bounds, FLAT_X, FLAT_Y)?;
    ensure(
        (mean(fit5_flat) - mean(opened_flat)).abs() < UNCHANGED,
        format!(
            "Texture +100 moved the flat quadrant it should leave alone: {} against {}",
            mean(fit5_flat),
            mean(opened_flat)
        ),
    )?;
    ensure(
        channel_spread(fit5_flat) < UNCHANGED,
        format!("Texture +100 tinted the flat grey quadrant: {fit5_flat:?}"),
    )?;
    record(
        &frames[5],
        "Texture +100 committed at Fit: one entry, the flat quadrant unaffected",
        json!({"label": frames[5].label()?, "flat_mean": mean(fit5_flat), "layer": layer, "expanded": frames[5].expanded_sections(&SECTIONS)}),
    );

    // Frame 6: the same committed state at 100%, where the checker's own fine detail — the gap
    // between its light and dark cells, widened by Texture's own gain — can be inspected.
    frames[6].expect_no_draft("Frame 6")?;
    ensure(
        frames[6].revision()? == frames[5].revision()?
            && frames[6].entry()? == frames[5].entry()?,
        "Changing zoom committed something",
    )?;
    ensure(
        frames[6]["step"]["request"] == json!({"view":{"zoom":100.0}}),
        format!(
            "Frame 6 did not request 100%: {}",
            frames[6]["step"]["request"]
        ),
    )?;
    let percent6_bounds = pixels::bright_bounds(&frames[6], BOUNDS)?;
    let (percent6_lo, percent6_hi) =
        patch_range(&frames[6], percent6_bounds, TEXTURE_X, TEXTURE_Y)?;
    let percent6_range = percent6_hi - percent6_lo;
    ensure(
        percent6_range - opened_texture_range > RANGE_WIDER,
        format!(
            "Texture +100 did not widen the checker's own light/dark gap: {percent6_range} against {opened_texture_range}"
        ),
    )?;
    record(
        &frames[6],
        "the same committed Texture +100, at 100%: fine detail in the checker",
        json!({"texture_range": percent6_range, "zoom_request": frames[6]["step"]["request"]}),
    );

    // Frame 7: back to Fit, unchanged, ready for the Clarity commit below.
    frames[7].expect_no_draft("Frame 7")?;
    ensure(
        frames[7].revision()? == frames[6].revision()?,
        "Returning to Fit committed something",
    )?;
    ensure(
        frames[7]["step"]["request"] == json!({"view":{"zoom":"fit"}}),
        format!(
            "Frame 7 did not request Fit: {}",
            frames[7]["step"]["request"]
        ),
    )?;
    record(
        &frames[7],
        "back to Fit",
        json!({"zoom_request": frames[7]["step"]["request"]}),
    );

    // Frame 8: Clarity +100, committed at Fit, merged with Texture: the step edge's own contrast
    // (the gap between a point just before it and just after) widens, and the flat quadrant stays
    // unaffected exactly as it did under Texture.
    ensure(
        frames[8].revision()? == frames[7].revision()? + 1,
        "The Clarity commit did not advance the revision by one",
    )?;
    ensure(
        frames[8].label()? == "Clarity +100",
        format!("Frame 8 is labelled {:?}", frames[8].label()?),
    )?;
    ensure(
        presence_payload(&frames[8]) == Some(&json!({ TEXTURE: 100.0, CLARITY: 100.0 })),
        format!(
            "The merged Presence layer holds {:?}",
            presence_payload(&frames[8])
        ),
    )?;
    ensure(
        presence_layer_id(&frames[8]) == Some(layer.as_str()),
        "Clarity replaced the Presence layer instead of updating it",
    )?;
    let fit8_bounds = pixels::bright_bounds(&frames[8], BOUNDS)?;
    let fit8_edge_low = patch_mean(&frames[8], fit8_bounds, EDGE_LOW_X, EDGE_Y)?;
    let fit8_edge_high = patch_mean(&frames[8], fit8_bounds, EDGE_HIGH_X, EDGE_Y)?;
    let fit8_contrast = mean(fit8_edge_high) - mean(fit8_edge_low);
    ensure(
        fit8_contrast - opened_contrast > CONTRAST_WIDER,
        format!(
            "Clarity +100 did not widen the step edge's own contrast: {fit8_contrast} against {opened_contrast}"
        ),
    )?;
    let fit8_flat = patch_mean(&frames[8], fit8_bounds, FLAT_X, FLAT_Y)?;
    ensure(
        (mean(fit8_flat) - mean(opened_flat)).abs() < UNCHANGED,
        format!(
            "Clarity +100 moved the flat quadrant it should leave alone: {} against {}",
            mean(fit8_flat),
            mean(opened_flat)
        ),
    )?;
    ensure(
        channel_spread(fit8_flat) < UNCHANGED,
        format!("Clarity +100 tinted the flat grey quadrant: {fit8_flat:?}"),
    )?;
    record(
        &frames[8],
        "Clarity +100 committed at Fit, merged with Texture: the step edge's own contrast widened, the flat quadrant unaffected",
        json!({"label": frames[8].label()?, "edge_contrast": fit8_contrast, "flat_mean": mean(fit8_flat), "layer": layer, "expanded": frames[8].expanded_sections(&SECTIONS)}),
    );

    // Frame 9: the same committed state at 100%, where the halo either side of the step edge that
    // a local-contrast gain leaves can be inspected.
    frames[9].expect_no_draft("Frame 9")?;
    ensure(
        frames[9].revision()? == frames[8].revision()?
            && frames[9].entry()? == frames[8].entry()?,
        "Changing zoom committed something",
    )?;
    ensure(
        frames[9]["step"]["request"] == json!({"view":{"zoom":100.0}}),
        format!(
            "Frame 9 did not request 100%: {}",
            frames[9]["step"]["request"]
        ),
    )?;
    let percent9_bounds = pixels::bright_bounds(&frames[9], BOUNDS)?;
    let percent9_edge_low = patch_mean(&frames[9], percent9_bounds, EDGE_LOW_X, EDGE_Y)?;
    let percent9_edge_high = patch_mean(&frames[9], percent9_bounds, EDGE_HIGH_X, EDGE_Y)?;
    let percent9_contrast = mean(percent9_edge_high) - mean(percent9_edge_low);
    record(
        &frames[9],
        "the same committed Clarity +100, at 100%: the edge and its own halo, for inspection",
        json!({"edge_contrast": percent9_contrast, "zoom_request": frames[9]["step"]["request"]}),
    );

    // Frame 10: back to Fit, unchanged, ready for the Dehaze commits below.
    frames[10].expect_no_draft("Frame 10")?;
    ensure(
        frames[10].revision()? == frames[9].revision()?,
        "Returning to Fit committed something",
    )?;
    ensure(
        frames[10]["step"]["request"] == json!({"view":{"zoom":"fit"}}),
        format!(
            "Frame 10 did not request Fit: {}",
            frames[10]["step"]["request"]
        ),
    )?;
    record(
        &frames[10],
        "back to Fit",
        json!({"zoom_request": frames[10]["step"]["request"]}),
    );

    // Frame 11: Dehaze +100, committed at Fit, merged with Texture and Clarity. Dehaze is not a
    // neighbourhood operation like the other two, so it is not asked to leave the flat quadrant's
    // own brightness alone; on a fixture with no colour anywhere, the atmospheric estimate it
    // solves for cannot be anything but neutral either, so the flat quadrant's own grey is asked
    // to stay grey, whatever its brightness does.
    ensure(
        frames[11].revision()? == frames[10].revision()? + 1,
        "The Dehaze +100 commit did not advance the revision by one",
    )?;
    ensure(
        frames[11].label()? == "Dehaze +100",
        format!("Frame 11 is labelled {:?}", frames[11].label()?),
    )?;
    ensure(
        presence_payload(&frames[11])
            == Some(&json!({ TEXTURE: 100.0, CLARITY: 100.0, DEHAZE: 100.0 })),
        format!(
            "The Dehaze +100 layer holds {:?}",
            presence_payload(&frames[11])
        ),
    )?;
    let fit11_bounds = pixels::bright_bounds(&frames[11], BOUNDS)?;
    let fit11_flat = patch_mean(&frames[11], fit11_bounds, FLAT_X, FLAT_Y)?;
    ensure(
        channel_spread(fit11_flat) < UNCHANGED,
        format!("Dehaze +100 tinted the flat grey quadrant: {fit11_flat:?}"),
    )?;
    record(
        &frames[11],
        "Dehaze +100 committed at Fit, merged with Texture and Clarity: the flat quadrant stays grey",
        json!({"label": frames[11].label()?, "flat_mean": mean(fit11_flat), "layer": layer, "expanded": frames[11].expanded_sections(&SECTIONS)}),
    );

    // Frame 12: Dehaze -100, committed at Fit: the same one field flips sign, adding a veil through
    // the same forward model instead of removing one.
    ensure(
        frames[12].revision()? == frames[11].revision()? + 1,
        "The Dehaze -100 commit did not advance the revision by one",
    )?;
    ensure(
        frames[12].label()? == "Dehaze -100",
        format!("Frame 12 is labelled {:?}", frames[12].label()?),
    )?;
    ensure(
        presence_payload(&frames[12])
            == Some(&json!({ TEXTURE: 100.0, CLARITY: 100.0, DEHAZE: -100.0 })),
        format!(
            "The Dehaze -100 layer holds {:?}",
            presence_payload(&frames[12])
        ),
    )?;
    let fit12_bounds = pixels::bright_bounds(&frames[12], BOUNDS)?;
    let fit12_flat = patch_mean(&frames[12], fit12_bounds, FLAT_X, FLAT_Y)?;
    ensure(
        channel_spread(fit12_flat) < UNCHANGED,
        format!("Dehaze -100 tinted the flat grey quadrant: {fit12_flat:?}"),
    )?;
    record(
        &frames[12],
        "Dehaze -100 committed at Fit: the flat quadrant still grey",
        json!({"label": frames[12].label()?, "flat_mean": mean(fit12_flat), "layer": layer}),
    );

    // Frame 13: all three at +100 through the raw API, the request a generated slider cannot send
    // itself (each submits only its own one field): Texture and Clarity are already +100 and
    // Dehaze flips back, so the merged payload is a genuine change, not a no-op.
    ensure(
        frames[13].revision()? == frames[12].revision()? + 1,
        "The multi-field commit did not advance the revision by one",
    )?;
    ensure(
        frames[13].label()? == "Presence (3 fields)",
        format!("Frame 13 is labelled {:?}", frames[13].label()?),
    )?;
    ensure(
        presence_payload(&frames[13])
            == Some(&json!({ TEXTURE: 100.0, CLARITY: 100.0, DEHAZE: 100.0 })),
        format!(
            "The multi-field layer holds {:?}",
            presence_payload(&frames[13])
        ),
    )?;
    ensure(
        presence_layer_id(&frames[13]) == Some(layer.as_str()),
        "The multi-field commit replaced the Presence layer instead of updating it",
    )?;
    let fit13_bounds = pixels::bright_bounds(&frames[13], BOUNDS)?;
    let fit13_flat = patch_mean(&frames[13], fit13_bounds, FLAT_X, FLAT_Y)?;
    ensure(
        channel_spread(fit13_flat) < UNCHANGED,
        format!("The multi-field commit tinted the flat grey quadrant: {fit13_flat:?}"),
    )?;
    record(
        &frames[13],
        "all three fields at +100, committed in one request through the raw API",
        json!({"label": frames[13].label()?, "payload": presence_payload(&frames[13]), "layer": layer}),
    );

    // Frame 14: the module's own header reset. One entry labelled "Reset Presence"; the layer is
    // kept at its neutral payload, and the fixture reads as it opened.
    ensure(
        frames[14].revision()? == frames[13].revision()? + 1,
        "The module reset did not commit exactly one revision",
    )?;
    ensure(
        frames[14].label()? == "Reset Presence",
        format!("The module reset is labelled {:?}", frames[14].label()?),
    )?;
    ensure(
        presence_payload(&frames[14]) == Some(&json!({})),
        format!(
            "The reset layer holds {:?}, expected the neutral payload",
            presence_payload(&frames[14])
        ),
    )?;
    ensure(
        presence_layer_id(&frames[14]) == Some(layer.as_str()),
        "The module reset replaced the Presence layer instead of keeping it",
    )?;
    let reset_bounds = pixels::bright_bounds(&frames[14], BOUNDS)?;
    let reset_edge_low = patch_mean(&frames[14], reset_bounds, EDGE_LOW_X, EDGE_Y)?;
    let reset_edge_high = patch_mean(&frames[14], reset_bounds, EDGE_HIGH_X, EDGE_Y)?;
    let reset_contrast = mean(reset_edge_high) - mean(reset_edge_low);
    ensure(
        (reset_contrast - opened_contrast).abs() < CONTRAST_WIDER,
        format!(
            "The reset edge contrast does not read like the opened one: {reset_contrast} against {opened_contrast}"
        ),
    )?;
    record(
        &frames[14],
        "the module's own header reset: entry \"Reset Presence\", the layer kept and neutral, the edge back to its opened contrast",
        json!({"label": frames[14].label()?, "payload": presence_payload(&frames[14]), "layer": layer, "expanded": frames[14].expanded_sections(&SECTIONS)}),
    );

    write_json(
        &evidence.join("presence-checks.json"),
        &json!({
            "checks": checks,
            "contrast_wider_margin": CONTRAST_WIDER,
            "range_wider_margin": RANGE_WIDER,
            "unchanged_tolerance": UNCHANGED,
            "sample_geometry": {"edge_low_x": EDGE_LOW_X, "edge_high_x": EDGE_HIGH_X, "edge_y": EDGE_Y, "texture_x": TEXTURE_X, "texture_y": TEXTURE_Y, "flat_x": FLAT_X, "flat_y": FLAT_Y},
            "scope": "Mean and min/max Rec. 709-agnostic grey level of small patches, read back from the renderer; a directional demonstration (contrast widens, flat and grey stay flat and grey), not a colorimetric claim",
        }),
    )?;
    Ok(())
}
