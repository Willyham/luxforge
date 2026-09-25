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
    scenario::{Bright, Checked, Frame, Plan, Run, Scan, Step, pixels, plan::only},
    *,
};
use lightwell_core::PRESENCE_EFFECT;
use lightwell_evidence::{self as script, SliderStep, ViewStep};

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

/// A committed Presence slider release, updating the one layer.
fn release(name: &str, parameter: &str, value: f64) -> Step {
    Step::new(
        name,
        SliderStep::new(SET_PRESENCE, parameter, [value]).release(),
    )
    .commits(1)
}

/// A zoom change: nothing committed and no draft.
fn view(name: &str, zoom: ViewStep) -> Step {
    Step::new(name, zoom).no_draft().commits(0)
}

/// Every frame, in order: the open, then one per step. Each step is one gesture, one request or one
/// decision; the plan says what it commits and records, and `verify` below checks what the
/// photograph shows.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The fixture opens with the Presence section listed and collapsed, every field at its
        // default, no draft and no Presence layer yet.
        Step::opened("opened")
            .collapsed(PRESENCE_MODULE)
            .field(SET_PRESENCE, CLARITY, "0")
            .no_draft()
            .no_layer(PRESENCE_EFFECT),
        // 1: collapse Basic, expanded by its own default and the one other section the
        // registry lists above Presence, so the module's own sliders land on screen without
        // scrolling once it expands.
        Step::new(
            "basic-collapsed",
            script::Step::section(BASIC_MODULE, false),
        )
        .commits(0)
        .collapsed(BASIC_MODULE),
        // 2: expand the section: its one group and three sliders, with Basic still collapsed.
        Step::new("expanded", script::Step::section(PRESENCE_MODULE, true))
            .commits(0)
            .expanded(PRESENCE_MODULE)
            .collapsed(BASIC_MODULE),
        // 3: a drag on Clarity, left open: the frame shows the drafted preview.
        Step::new(
            "drag",
            SliderStep::new(SET_PRESENCE, CLARITY, [30.0, 60.0, 90.0]),
        )
        .commits(0)
        .draft(SET_PRESENCE, json!({ CLARITY: 90.0 }))
        .no_layer(PRESENCE_EFFECT)
        .field(SET_PRESENCE, CLARITY, "90"),
        // 4: Escape cancels the gesture; nothing commits from it, and the field returns to what the
        // stack (still empty) actually holds.
        Step::new(
            "cancel",
            SliderStep::new(SET_PRESENCE, CLARITY, [90.0]).cancel(),
        )
        .no_draft()
        .commits(0)
        .field(SET_PRESENCE, CLARITY, "0"),
        // 5: Texture +100, committed at Fit.
        release("texture", TEXTURE, 100.0)
            .label("Texture +100")
            .payload(PRESENCE_EFFECT, json!({ TEXTURE: 100.0 })),
        // 6: the same committed state at 100%, where the checker's own fine detail shows.
        view("texture-100", ViewStep::Percent(100.0)),
        // 7: back to Fit for the Clarity commit below.
        view("texture-fit", ViewStep::Fit),
        // 8: Clarity +100, committed at Fit, merged with Texture.
        release("clarity", CLARITY, 100.0)
            .label("Clarity +100")
            .payload(PRESENCE_EFFECT, json!({ TEXTURE: 100.0, CLARITY: 100.0 }))
            .same_layer(PRESENCE_EFFECT, "texture"),
        // 9: the same committed state at 100%, where the step edge's own halo shows.
        view("clarity-100", ViewStep::Percent(100.0)),
        // 10: back to Fit for the Dehaze commits below.
        view("clarity-fit", ViewStep::Fit),
        // 11: Dehaze +100, committed at Fit, merged with Texture and Clarity.
        release("dehaze", DEHAZE, 100.0)
            .label("Dehaze +100")
            .payload(
                PRESENCE_EFFECT,
                json!({ TEXTURE: 100.0, CLARITY: 100.0, DEHAZE: 100.0 }),
            )
            .same_layer(PRESENCE_EFFECT, "texture"),
        // 12: Dehaze -100, committed at Fit: the same one field flips sign.
        release("dehaze-negative", DEHAZE, -100.0)
            .label("Dehaze -100")
            .payload(
                PRESENCE_EFFECT,
                json!({ TEXTURE: 100.0, CLARITY: 100.0, DEHAZE: -100.0 }),
            )
            .same_layer(PRESENCE_EFFECT, "texture"),
        // 13: all three at once, through the raw API a generated slider cannot reach (each
        // submits its own one field only): the same "everything is programmable" parity every
        // other scenario's own API step proves. Texture and Clarity are already +100 and Dehaze
        // flips back, so the merged payload is a genuine change, not a no-op.
        Step::new(
            "api",
            script::Step::call(
                "edit.set-presence",
                json!({"texture":100.0,"clarity":100.0,"dehaze":100.0}),
            ),
        )
        .commits(1)
        .label("Presence (3 fields)")
        .payload(
            PRESENCE_EFFECT,
            json!({ TEXTURE: 100.0, CLARITY: 100.0, DEHAZE: 100.0 }),
        )
        .same_layer(PRESENCE_EFFECT, "texture"),
        // 14: the module's own header reset: the layer kept at its neutral payload.
        Step::new("reset", script::Step::reset(PRESENCE_MODULE, None))
            .commits(1)
            .label("Reset Presence")
            .payload(PRESENCE_EFFECT, json!({}))
            .same_layer(PRESENCE_EFFECT, "texture"),
    ])
}

/// Every section this scenario toggles, for the correlation every recorded frame carries alongside
/// its revision, entry and draft.
const SECTIONS: [&str; 2] = [BASIC_MODULE, PRESENCE_MODULE];

fn presence_payload(frame: &Frame) -> Option<&Value> {
    frame.payload(PRESENCE_EFFECT)
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

/// The step edge's own contrast in a frame: the gap between a point just before it and just after.
fn edge_contrast(frame: &Frame) -> Result<f64> {
    let bounds = pixels::bright_bounds(frame, BOUNDS)?;
    let low = patch_mean(frame, bounds, EDGE_LOW_X, EDGE_Y)?;
    let high = patch_mean(frame, bounds, EDGE_HIGH_X, EDGE_Y)?;
    Ok(mean(high) - mean(low))
}

/// The flat quadrant's mean colour in a frame.
fn flat(frame: &Frame) -> Result<[f64; 3]> {
    patch_mean(frame, pixels::bright_bounds(frame, BOUNDS)?, FLAT_X, FLAT_Y)
}

/// The checker's own light/dark gap in a frame.
fn texture_range(frame: &Frame) -> Result<f64> {
    let (lo, hi) = patch_range(
        frame,
        pixels::bright_bounds(frame, BOUNDS)?,
        TEXTURE_X,
        TEXTURE_Y,
    )?;
    Ok(hi - lo)
}

/// What the photograph shows at each step, once the plan has held.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Vec::new();
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };

    // The fixture as launched, with the Presence module listed and available.
    let opened = launch.at("opened")?;
    let presence = opened["state"]["modules"]
        .as_array()
        .ok_or("Missing modules")?
        .iter()
        .find(|module| module["id"] == json!(PRESENCE_MODULE))
        .ok_or("The Presence module is not listed at all")?;
    ensure(
        presence["available"] == json!(true),
        "The Presence module is not available",
    )?;
    let opened_contrast = edge_contrast(opened)?;
    let opened_texture_range = texture_range(opened)?;
    let opened_flat = flat(opened)?;
    ensure(
        channel_spread(opened_flat) < UNCHANGED,
        format!("The flat quadrant does not open grey: {opened_flat:?}"),
    )?;
    record(
        opened,
        "the collapsed Presence section as launched",
        json!({
            "edge_contrast": opened_contrast,
            "texture_range": opened_texture_range,
            "flat_mean": mean(opened_flat),
            "expanded": opened.expanded_sections(&SECTIONS),
        }),
    );
    let basic = launch.at("basic-collapsed")?;
    record(
        basic,
        "the Basic section collapsed, above Presence in the registry order",
        json!({"expanded": basic.expanded_sections(&SECTIONS)}),
    );
    let expanded = launch.at("expanded")?;
    record(
        expanded,
        "the Presence section expanded: Texture, Clarity and Dehaze, on screen with nothing above it expanded",
        json!({"expanded": expanded.expanded_sections(&SECTIONS)}),
    );
    let drag = launch.at("drag")?;
    record(
        drag,
        "a drag to Clarity +90, mid-gesture: the drafted preview",
        json!({"draft": drag.draft(), "expanded": drag.expanded_sections(&SECTIONS)}),
    );
    let cancel = launch.at("cancel")?;
    record(
        cancel,
        "Escape cancels the Clarity gesture: nothing committed",
        json!({"revision": cancel.revision()?}),
    );

    // Texture +100 at Fit: the flat quadrant unaffected, and still grey.
    let texture = launch.at("texture")?;
    let texture_flat = flat(texture)?;
    ensure(
        (mean(texture_flat) - mean(opened_flat)).abs() < UNCHANGED,
        format!(
            "Texture +100 moved the flat quadrant it should leave alone: {} against {}",
            mean(texture_flat),
            mean(opened_flat)
        ),
    )?;
    ensure(
        channel_spread(texture_flat) < UNCHANGED,
        format!("Texture +100 tinted the flat grey quadrant: {texture_flat:?}"),
    )?;
    let layer = texture.layer_id(PRESENCE_EFFECT);
    record(
        texture,
        "Texture +100 committed at Fit: one entry, the flat quadrant unaffected",
        json!({"label": texture.label()?, "flat_mean": mean(texture_flat), "layer": layer, "expanded": texture.expanded_sections(&SECTIONS)}),
    );

    // The same committed state at 100%, where the checker's own fine detail — the gap between its
    // light and dark cells, widened by Texture's own gain — can be inspected.
    let texture_100 = launch.at("texture-100")?;
    let percent_range = texture_range(texture_100)?;
    ensure(
        percent_range - opened_texture_range > RANGE_WIDER,
        format!(
            "Texture +100 did not widen the checker's own light/dark gap: {percent_range} against {opened_texture_range}"
        ),
    )?;
    record(
        texture_100,
        "the same committed Texture +100, at 100%: fine detail in the checker",
        json!({"texture_range": percent_range, "zoom_request": texture_100["step"]["request"]}),
    );
    let texture_fit = launch.at("texture-fit")?;
    record(
        texture_fit,
        "back to Fit",
        json!({"zoom_request": texture_fit["step"]["request"]}),
    );

    // Clarity +100 at Fit, merged with Texture: the step edge's own contrast widens, and the flat
    // quadrant stays unaffected exactly as it did under Texture.
    let clarity = launch.at("clarity")?;
    let clarity_contrast = edge_contrast(clarity)?;
    ensure(
        clarity_contrast - opened_contrast > CONTRAST_WIDER,
        format!(
            "Clarity +100 did not widen the step edge's own contrast: {clarity_contrast} against {opened_contrast}"
        ),
    )?;
    let clarity_flat = flat(clarity)?;
    ensure(
        (mean(clarity_flat) - mean(opened_flat)).abs() < UNCHANGED,
        format!(
            "Clarity +100 moved the flat quadrant it should leave alone: {} against {}",
            mean(clarity_flat),
            mean(opened_flat)
        ),
    )?;
    ensure(
        channel_spread(clarity_flat) < UNCHANGED,
        format!("Clarity +100 tinted the flat grey quadrant: {clarity_flat:?}"),
    )?;
    record(
        clarity,
        "Clarity +100 committed at Fit, merged with Texture: the step edge's own contrast widened, the flat quadrant unaffected",
        json!({"label": clarity.label()?, "edge_contrast": clarity_contrast, "flat_mean": mean(clarity_flat), "layer": layer, "expanded": clarity.expanded_sections(&SECTIONS)}),
    );

    // The same committed state at 100%, where the halo either side of the step edge that a
    // local-contrast gain leaves can be inspected.
    let clarity_100 = launch.at("clarity-100")?;
    record(
        clarity_100,
        "the same committed Clarity +100, at 100%: the edge and its own halo, for inspection",
        json!({"edge_contrast": edge_contrast(clarity_100)?, "zoom_request": clarity_100["step"]["request"]}),
    );
    let clarity_fit = launch.at("clarity-fit")?;
    record(
        clarity_fit,
        "back to Fit",
        json!({"zoom_request": clarity_fit["step"]["request"]}),
    );

    // Dehaze is not a neighbourhood operation like the other two, so it is not asked to leave the
    // flat quadrant's own brightness alone; on a fixture with no colour anywhere, the atmospheric
    // estimate it solves for cannot be anything but neutral either, so the flat quadrant's own grey
    // is asked to stay grey, whatever its brightness does. The same holds with the sign flipped,
    // adding a veil through the same forward model instead of removing one, and for all three
    // fields sent at once through the raw API.
    for (step, what, shows) in [
        (
            "dehaze",
            "Dehaze +100",
            "Dehaze +100 committed at Fit, merged with Texture and Clarity: the flat quadrant stays grey",
        ),
        (
            "dehaze-negative",
            "Dehaze -100",
            "Dehaze -100 committed at Fit: the flat quadrant still grey",
        ),
        (
            "api",
            "The multi-field commit",
            "all three fields at +100, committed in one request through the raw API",
        ),
    ] {
        let frame = launch.at(step)?;
        let grey = flat(frame)?;
        ensure(
            channel_spread(grey) < UNCHANGED,
            format!("{what} tinted the flat grey quadrant: {grey:?}"),
        )?;
        record(
            frame,
            shows,
            json!({"label": frame.label()?, "flat_mean": mean(grey), "payload": presence_payload(frame), "layer": layer}),
        );
    }

    // The module's own header reset: the fixture reads as it opened.
    let reset = launch.at("reset")?;
    let reset_contrast = edge_contrast(reset)?;
    ensure(
        (reset_contrast - opened_contrast).abs() < CONTRAST_WIDER,
        format!(
            "The reset edge contrast does not read like the opened one: {reset_contrast} against {opened_contrast}"
        ),
    )?;
    record(
        reset,
        "the module's own header reset: entry \"Reset Presence\", the layer kept and neutral, the edge back to its opened contrast",
        json!({"label": reset.label()?, "payload": presence_payload(reset), "layer": layer, "expanded": reset.expanded_sections(&SECTIONS)}),
    );

    write_json(
        &launch.evidence.join("presence-checks.json"),
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
