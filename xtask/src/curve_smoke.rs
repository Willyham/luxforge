//! The `curve` smoke scenario: the Tone curve section on the real editor, over its own generated
//! fixture, `fixtures/generated/tone-ramp.jpg` (`cargo xtask generate-fixtures`): an encoded grey
//! ramp above four flat patches of the orientation fixture's quadrant colours
//! ([`crate::fixtures::TONE_RAMP_FIXTURE`]).
//!
//! The steps are the design's rendered acceptance, in its order: the section listed and collapsed
//! with no layer; Basic collapsed and the Tone curve expanded; three on-diagonal points through the
//! API, a layer that compiles to nothing; a point drag left open, then released as one entry; a point
//! added and one removed, each one entry; the Points list opened, which sends nothing; an S-curve
//! typed into that list one coordinate per Enter; the module reset from the band, which keeps the
//! layer; and a radial mask drawn and the curve dragged through it, with the scope chip on the Tone
//! curve band. A PCHIP curve through three points whose ends are fixed bends only one way, so no
//! typed coordinate of the three points the removal leaves makes an S: the scenario adds the fourth
//! point the S needs before it types.
//!
//! What the plan cannot say is checked against the neutral frame, the expanded section before any
//! layer exists, whose ramp and patches are the fixture as the editor decodes and draws it: along
//! every sampled ramp row no channel decreases where the neutral row does not; no pixel's channel
//! spread grows past the neutral one's by more than one code; each patch's mean Oklab hue angle is
//! unchanged wherever no channel reads as clipped; the S-curve lowers the ramp's lower half and
//! raises its upper half; and a curve that is the exact identity map shows the neutral frame's
//! pixels exactly. Rows and patch windows are sampled at least 16 fixture pixels from every band
//! and patch edge, clear of the JPEG's chroma subsampling and ringing.
use crate::{
    fixtures::{
        TONE_RAMP_BAND, TONE_RAMP_FIXTURE, TONE_RAMP_PATCH, TONE_RAMP_PATCHES, tone_ramp_code,
    },
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, plan::only},
    *,
};
use luxforge_core::CURVE_EFFECT;
use luxforge_evidence::{self as script, CurveStep, CurveStepEvent, MaskStep, SliderEnd};
use luxforge_reference as reference;

pub const FIXTURE: &str = "fixtures/generated/tone-ramp.jpg";
const CURVE_MODULE: &str = "luxforge.curve";
/// Expanded by its own descriptor's default and listed above the Tone curve, so it is collapsed
/// first to bring the curve editor on screen.
const BASIC_MODULE: &str = "luxforge.basic";
const SET_CURVE: &str = "set-curve";
const LUMINANCE: &str = "luminance";
const MASK_MODE: &str = "mask";
const RADIAL: &str = "radial";
/// What the host names the first mask.
const MASK: &str = "Mask 1";

/// The drag's outputs for point 1, in the order the pointer passes them, and where it is released.
const DRAG: [f32; 3] = [0.60, 0.65, 0.70];
/// The point the design adds, on the identity diagonal.
const ADDED: [f32; 2] = [0.25, 0.25];
/// The fourth point the S-curve needs, added on the diagonal before typing.
const S_POINT: [f32; 2] = [0.75, 0.75];
/// The S-curve's typed outputs for points 1 and 2, each one coordinate and one Enter, lower point
/// first so the curve stays monotone at every step. Gentle enough that no patch's channel reaches
/// the run's clamp (the yellow patch's red channel is the closest, about 249).
const S_LOW: &str = "0.2";
const S_HIGH: &str = "0.8";
/// Where the radial is pressed and where the sweep ends, as fractions of the photograph: on the
/// ramp's upper tones, clear of the patches.
const RADIAL_FROM: [f64; 2] = [0.70, 0.25];
const RADIAL_TO: [f64; 2] = [0.85, 0.40];
/// The masked drag moves the white point down through these outputs.
const MASKED: [f32; 2] = [0.90, 0.80];

/// The ramp rows sampled, in fixture pixels: at least 16 from either edge of the ramp band.
const RAMP_ROWS: [u32; 5] = [16, 40, 64, 88, 111];
/// How far inside a band or patch edge a sample stays, in fixture pixels.
const INSET: u32 = 16;
/// Capture pixels skipped at the photograph's left and right edges, where the drawn raster meets
/// the canvas.
const EDGE: u32 = 2;
/// How far a curve's channel spread may exceed the neutral one's at a pixel, in 8-bit codes.
const SPREAD: f64 = 1.0;
/// How far a patch's mean Oklab hue angle may move, in degrees: the 8-bit rounding of a scaled
/// patch moves it by about 0.1°.
const HUE: f64 = 1.0;
/// A patch channel whose mean reads at or above this may have met the run's output clamp, which
/// the reconstruction's hue claim does not cover.
const CLIPPED: f64 = 254.0;
/// How far a mean must move, in 8-bit codes, before the scenario calls it changed.
const CHANGED: f64 = 2.0;

fn points_payload(points: &[[f64; 2]]) -> Value {
    json!({ LUMINANCE: points })
}

/// A coordinate as the curve editor publishes it, in single precision, read back as the desktop
/// sends it.
fn wide(value: f32) -> f64 {
    f64::from(value)
}

fn curve(event: CurveStepEvent, finish: SliderEnd) -> script::Step {
    script::Step::Curve(CurveStep {
        action: SET_CURVE.into(),
        parameter: LUMINANCE.into(),
        event,
        finish,
    })
}

fn drag(index: usize, points: Vec<[f32; 2]>, finish: SliderEnd) -> script::Step {
    curve(CurveStepEvent::Move { index, points }, finish)
}

fn typed(index: usize, axis: usize, text: &str) -> script::Step {
    curve(
        CurveStepEvent::Type {
            index,
            axis,
            text: text.into(),
        },
        SliderEnd::Open,
    )
}

/// The committed curve at each step, as the recipe stores it.
struct Curves {
    identity: Vec<[f64; 2]>,
    released: Vec<[f64; 2]>,
    added: Vec<[f64; 2]>,
    removed: Vec<[f64; 2]>,
    four: Vec<[f64; 2]>,
    low: Vec<[f64; 2]>,
    s_curve: Vec<[f64; 2]>,
}

fn curves() -> Curves {
    let lifted = [0.5, wide(DRAG[2])];
    let added = [wide(ADDED[0]), wide(ADDED[1])];
    let s_point = [wide(S_POINT[0]), wide(S_POINT[1])];
    let low: f64 = S_LOW.parse().expect("a number");
    let high: f64 = S_HIGH.parse().expect("a number");
    Curves {
        identity: vec![[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]],
        released: vec![[0.0, 0.0], lifted, [1.0, 1.0]],
        added: vec![[0.0, 0.0], added, lifted, [1.0, 1.0]],
        removed: vec![[0.0, 0.0], added, [1.0, 1.0]],
        four: vec![[0.0, 0.0], added, s_point, [1.0, 1.0]],
        low: vec![[0.0, 0.0], [added[0], low], s_point, [1.0, 1.0]],
        s_curve: vec![[0.0, 0.0], [added[0], low], [s_point[0], high], [1.0, 1.0]],
    }
}

/// One step in Mask mode that commits nothing.
fn quiet(name: &str, step: impl Into<script::Step>) -> Step {
    Step::new(name, step).commits(0).mode(MASK_MODE)
}

/// Every frame, in order: the open, then one per step, each with what it commits and shows.
pub fn plan(_: &[PathBuf]) -> Plan {
    let c = curves();
    let drafted = |points: &[[f64; 2]]| json!({ LUMINANCE: points });
    let committed = |step: Step, label: &str, points: &[[f64; 2]]| {
        step.commits(1)
            .no_draft()
            .label(label)
            .payload(CURVE_EFFECT, points_payload(points))
            .expanded(CURVE_MODULE)
            .collapsed(BASIC_MODULE)
    };
    Plan::new(vec![
        // The fixture opens with Basic expanded, the Tone curve listed and collapsed, no draft and
        // no curve layer.
        Step::opened("opened")
            .collapsed(CURVE_MODULE)
            .expanded(BASIC_MODULE)
            .no_draft()
            .no_layer(CURVE_EFFECT),
        // 1: Basic collapsed, so the curve editor lands on screen once the section expands.
        Step::new(
            "basic-collapsed",
            script::Step::section(BASIC_MODULE, false),
        )
        .commits(0)
        .collapsed(BASIC_MODULE),
        // 2: the Tone curve expanded: the neutral frame every pixel check is read against.
        Step::new("expanded", script::Step::section(CURVE_MODULE, true))
            .commits(0)
            .no_layer(CURVE_EFFECT)
            .expanded(CURVE_MODULE)
            .collapsed(BASIC_MODULE),
        // 3: three on-diagonal points through the API: a stored, labelled layer that compiles to
        // nothing, so the frame shows the neutral bytes.
        committed(
            Step::new(
                "identity",
                script::Step::call("edit.set-curve", json!({ LUMINANCE: c.identity })),
            ),
            "Tone curve 3 points",
            &c.identity,
        ),
        // 4: point 1 dragged up through three outputs and held: drafted frames, nothing committed.
        Step::new(
            "drag",
            drag(1, DRAG.iter().map(|y| [0.5, *y]).collect(), SliderEnd::Open),
        )
        .commits(0)
        .draft(SET_CURVE, drafted(&c.released))
        .payload(CURVE_EFFECT, points_payload(&c.identity)),
        // 5: the same gesture released: one entry.
        committed(
            Step::new("release", drag(1, vec![[0.5, DRAG[2]]], SliderEnd::Release)),
            "Tone curve 3 points",
            &c.released,
        )
        .same_layer(CURVE_EFFECT, "identity"),
        // 6: a point added on the diagonal below the lifted one.
        committed(
            Step::new("add", curve(CurveStepEvent::Add(ADDED), SliderEnd::Open)),
            "Tone curve 4 points",
            &c.added,
        )
        .same_layer(CURVE_EFFECT, "identity"),
        // 7: point 2, the lifted one, removed: the on-diagonal three that remain are the identity
        // map again.
        committed(
            Step::new("remove", curve(CurveStepEvent::Remove(2), SliderEnd::Open)),
            "Tone curve 3 points",
            &c.removed,
        )
        .same_layer(CURVE_EFFECT, "identity"),
        // 8: the Points list opened: view state, no request.
        Step::new(
            "points-open",
            curve(CurveStepEvent::Points(true), SliderEnd::Open),
        )
        .commits(0)
        .no_draft()
        .payload(CURVE_EFFECT, points_payload(&c.removed)),
        // 9: the S-curve's fourth point, added on the diagonal.
        committed(
            Step::new(
                "s-point",
                curve(CurveStepEvent::Add(S_POINT), SliderEnd::Open),
            ),
            "Tone curve 4 points",
            &c.four,
        ),
        // 10-11: the S typed one coordinate per Enter, the lower point's output first.
        committed(
            Step::new("typed-low", typed(1, 1, S_LOW)),
            "Tone curve 4 points",
            &c.low,
        ),
        committed(
            Step::new("s-curve", typed(2, 1, S_HIGH)),
            "Tone curve 4 points",
            &c.s_curve,
        )
        .same_layer(CURVE_EFFECT, "identity"),
        // 12: the module reset from the band: the layer is kept, neutral.
        Step::new("reset", script::Step::reset(CURVE_MODULE, None))
            .commits(1)
            .no_draft()
            .label("Reset Tone curve")
            .payload(CURVE_EFFECT, json!({}))
            .same_layer(CURVE_EFFECT, "identity"),
        // 13-16: Mask mode, and a radial drawn and committed by its release as the first mask.
        quiet(
            "mask-mode",
            luxforge_evidence::WorkspaceStep::default().mode(MASK_MODE),
        ),
        quiet("radial-new", MaskStep::New(RADIAL.into())),
        quiet(
            "radial-swept",
            MaskStep::Sweep {
                from: RADIAL_FROM,
                to: RADIAL_TO,
            },
        ),
        // The pointer lifted: the release commits the radial it placed.
        Step::new("radial-applied", MaskStep::Release)
            .commits(1)
            .label(format!("Add {RADIAL}"))
            .no_draft()
            .mode(MASK_MODE)
            .mask_names(&[MASK])
            .open_mask(Some(MASK)),
        // 17: the white point dragged down through the mask and held: the draft is the mask's.
        Step::new(
            "masked-drag",
            drag(
                1,
                MASKED.iter().map(|y| [1.0, *y]).collect(),
                SliderEnd::Open,
            ),
        )
        .commits(0)
        .mode(MASK_MODE)
        .draft(SET_CURVE, drafted(&[[0.0, 0.0], [1.0, wide(MASKED[1])]]))
        .payload(CURVE_EFFECT, json!({})),
        // 18: released: one entry, labelled with the mask it went through.
        Step::new(
            "masked-release",
            drag(1, vec![[1.0, MASKED[1]]], SliderEnd::Release),
        )
        .commits(1)
        .no_draft()
        .mode(MASK_MODE)
        .label(format!("{MASK} · Tone curve 2 points"))
        .payload(CURVE_EFFECT, json!({}))
        .same_layer(CURVE_EFFECT, "identity"),
    ])
}

/// A capture pixel's position for the centre of fixture pixel `(x, y)`.
fn screen(bounds: [u32; 4], x: f64, y: f64) -> (u32, u32) {
    let [left, top, right, bottom] = bounds;
    let (width, height) = TONE_RAMP_FIXTURE;
    let sx = f64::from(left) + (x + 0.5) / f64::from(width) * f64::from(right - left);
    let sy = f64::from(top) + (y + 0.5) / f64::from(height) * f64::from(bottom - top);
    (sx.floor() as u32, sy.floor() as u32)
}

/// One sampled ramp row as the frame shows it: every capture pixel across the photograph, less
/// [`EDGE`] at each side.
fn ramp_row(frame: &Frame, row: u32) -> Result<Vec<[u8; 3]>> {
    let bounds = frame.photo()?;
    let image = frame.image()?;
    let (_, y) = screen(bounds, 0.0, f64::from(row));
    Ok((bounds[0] + EDGE..bounds[2] - EDGE)
        .map(|x| image.get_pixel(x, y).0)
        .collect())
}

/// The capture rectangle of fixture rectangle `[x0, y0, x1, y1)`, pixel centres inclusive.
fn window(bounds: [u32; 4], [x0, y0, x1, y1]: [u32; 4]) -> [u32; 4] {
    let (left, top) = screen(bounds, f64::from(x0), f64::from(y0));
    let (right, bottom) = screen(bounds, f64::from(x1 - 1), f64::from(y1 - 1));
    [left, top, right + 1, bottom + 1]
}

/// Mean 8-bit RGB over a capture rectangle.
fn mean(frame: &Frame, [left, top, right, bottom]: [u32; 4]) -> Result<[f64; 3]> {
    let image = frame.image()?;
    let mut totals = [0.0; 3];
    let mut count = 0.0;
    for y in top..bottom {
        for x in left..right {
            for (total, channel) in totals.iter_mut().zip(image.get_pixel(x, y).0) {
                *total += f64::from(channel);
            }
            count += 1.0;
        }
    }
    ensure(count > 0.0, "Sampled no pixels")?;
    Ok(totals.map(|total| total / count))
}

/// Patch `index`'s window, [`INSET`] inside its edges and the patch band's.
fn patch_window(index: usize) -> [u32; 4] {
    let left = index as u32 * TONE_RAMP_PATCH;
    let (_, height) = TONE_RAMP_FIXTURE;
    [
        left + INSET,
        TONE_RAMP_BAND + INSET,
        left + TONE_RAMP_PATCH - INSET,
        height - INSET,
    ]
}

/// Mean Rec. 709 luminance, in codes, of the ramp band's lower or upper half, [`INSET`] inside the
/// band's top and bottom.
fn ramp_half(frame: &Frame, upper: bool) -> Result<f64> {
    let (width, _) = TONE_RAMP_FIXTURE;
    let half = width / 2;
    let x0 = if upper { half } else { 0 };
    let bounds = frame.photo()?;
    let [left, top, right, bottom] = window(bounds, [x0, INSET, x0 + half, TONE_RAMP_BAND - INSET]);
    let rgb = mean(
        frame,
        [
            left.max(bounds[0] + EDGE),
            top,
            right.min(bounds[2] - EDGE),
            bottom,
        ],
    )?;
    Ok(0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2])
}

/// The Oklab hue angle of a mean 8-bit sRGB colour, in degrees, through the shared test reference's
/// sRGB decoding and Oklab conversion, independent of the core's.
fn hue(rgb: [f64; 3]) -> f64 {
    let linear = rgb.map(|code| reference::srgb::decode_encoded(code / 255.0));
    reference::colour::hue_degrees(reference::colour::to_oklab(linear))
}

/// The difference between two angles in degrees, in `[0, 180]`.
fn angle_between(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// The largest absolute channel difference between two frames over the photograph, which must be
/// drawn in the same rectangle in both.
fn photo_difference(a: &Frame, b: &Frame) -> Result<u8> {
    let bounds = a.photo()?;
    ensure(
        bounds == b.photo()?,
        format!(
            "The photograph is drawn at {bounds:?} in one frame and {:?} in the other",
            b.photo()?
        ),
    )?;
    let (first, second) = (a.image()?, b.image()?);
    let mut most = 0u8;
    for y in bounds[1]..bounds[3] {
        for x in bounds[0]..bounds[2] {
            for (p, q) in first.get_pixel(x, y).0.iter().zip(second.get_pixel(x, y).0) {
                most = most.max(p.abs_diff(q));
            }
        }
    }
    Ok(most)
}

/// The ramp and patch checks every curve frame is held to against the neutral frame: no new
/// decrease along a ramp row, no channel spread grown by more than [`SPREAD`], and each unclipped
/// patch's hue kept within [`HUE`].
fn relative(checks: &mut Checks, name: &str, frame: &Frame, neutral: &Frame) -> Result {
    ensure(
        frame.photo()? == neutral.photo()?,
        format!("{name}: the photograph is not drawn where the neutral frame draws it"),
    )?;
    let mut decreases = 0usize;
    let mut first_decrease = Value::Null;
    let mut spread_excess = f64::NEG_INFINITY;
    for row in RAMP_ROWS {
        let (shown, reference) = (ramp_row(frame, row)?, ramp_row(neutral, row)?);
        for (index, (pixel, base)) in shown.iter().zip(&reference).enumerate() {
            let spread = |p: &[u8; 3]| {
                f64::from(*p.iter().max().unwrap_or(&0)) - f64::from(*p.iter().min().unwrap_or(&0))
            };
            spread_excess = spread_excess.max(spread(pixel) - spread(base));
            if index == 0 {
                continue;
            }
            for channel in 0..3 {
                if base[channel] >= reference[index - 1][channel]
                    && pixel[channel] < shown[index - 1][channel]
                {
                    decreases += 1;
                    if first_decrease.is_null() {
                        first_decrease = json!({"row": row, "column": index, "channel": channel,
                            "was": shown[index - 1][channel], "is": pixel[channel]});
                    }
                }
            }
        }
    }
    checks.note(
        frame,
        "the first new decrease along a sampled ramp row, if any",
        json!({"step": name, "decreases": decreases, "first": first_decrease}),
    );
    checks.compare(
        frame,
        &format!(
            "{name}: channel steps that decrease along the ramp rows where the neutral rows do not"
        ),
        decreases as f64,
        0.0,
        Tolerance::Within(0.0),
    )?;
    checks.compare(
        frame,
        &format!(
            "{name}: the largest growth of a ramp pixel's channel spread over the neutral one's"
        ),
        spread_excess,
        SPREAD,
        Tolerance::AtMost(0.0),
    )?;
    let bounds = frame.photo()?;
    for (index, (patch, _)) in TONE_RAMP_PATCHES.iter().enumerate() {
        let area = window(bounds, patch_window(index));
        let (shown, base) = (mean(frame, area)?, mean(neutral, area)?);
        if shown.iter().any(|channel| *channel >= CLIPPED) {
            checks.note(
                frame,
                "a patch channel at the run's clamp, where the reconstruction's hue claim does not apply",
                json!({"step": name, "patch": patch, "rgb": shown, "neutral": base,
                    "hue_moved_deg": angle_between(hue(shown), hue(base))}),
            );
            continue;
        }
        checks.compare(
            frame,
            &format!("{name}: the {patch} patch's mean Oklab hue angle against the neutral one's, degrees apart"),
            angle_between(hue(shown), hue(base)),
            0.0,
            Tolerance::Within(HUE),
        )?;
    }
    Ok(())
}

/// The curve the frame's editor draws for the global or masked target it is bound to.
fn drawn_curve(frame: &Frame) -> Result<&Value> {
    frame.state()["control_ui"]["curves"]
        .as_array()
        .and_then(|curves| {
            curves
                .iter()
                .find(|curve| curve["action"] == SET_CURVE && curve["parameter"] == LUMINANCE)
        })
        .ok_or_else(|| format!("{} draws no Tone curve", frame["file"]).into())
}

/// Points as the editor draws them, in single precision, from a stored or drafted point list.
fn single(points: &Value) -> Value {
    json!(
        points
            .as_array()
            .map(|points| points
                .iter()
                .map(|point| [
                    point[0].as_f64().unwrap_or(f64::NAN) as f32,
                    point[1].as_f64().unwrap_or(f64::NAN) as f32
                ])
                .collect::<Vec<_>>())
            .unwrap_or_default()
    )
}

/// Every frame from the expanded section on, correlated with what it records: the editor drawn
/// with the histogram behind it, its points the committed or drafted ones, its sampled curve the
/// committed points' at the displayed entry, and the displayed entry the committed one.
fn correlate(checks: &mut Checks, launch: &Checked) -> Result {
    let names = launch.names().to_vec();
    let expanded = launch.index("expanded")?;
    for name in &names[expanded..] {
        let frame = launch.at(name)?;
        let state = frame.state();
        let stack = &state["stack"];
        ensure(
            stack["displayed"]["entry"] == stack["entry"],
            format!(
                "{name}: the frame shows entry {}, not the current {}",
                stack["displayed"]["entry"], stack["entry"]
            ),
        )?;
        let drawn = drawn_curve(frame)?;
        ensure(
            drawn["background"] == json!(true) && drawn["label_shown"] == json!(false),
            format!("{name}: the Tone curve is not drawn headerless over the histogram: {drawn}"),
        )?;
        ensure(
            state["histogram"]["identity"]["entry"] == stack["entry"],
            format!(
                "{name}: the histogram behind the curve is of {}, not the current entry {}",
                state["histogram"]["identity"]["entry"], stack["entry"]
            ),
        )?;
        let mask = state["masks"]["selected"].as_str();
        let target = state["stack"]["layers"]
            .as_array()
            .and_then(|layers| {
                layers
                    .iter()
                    .find(|layer| layer["effect"] == CURVE_EFFECT && layer["mask"].as_str() == mask)
            })
            .map(|layer| &layer["payload"][LUMINANCE]);
        // A mask gesture's draft is the mask's, not the curve's.
        let draft = Some(frame.draft()).filter(|draft| draft["action"] == SET_CURVE);
        let expected = if let Some(draft) = draft {
            single(&draft["fields"][LUMINANCE])
        } else {
            match target {
                Some(points) if points.is_array() => single(points),
                _ => json!([[0.0, 0.0], [1.0, 1.0]]),
            }
        };
        ensure(
            drawn["points"] == expected,
            format!(
                "{name}: the editor draws {}, not {expected}",
                drawn["points"]
            ),
        )?;
        ensure(
            drawn["dragging"] == json!(draft.is_some()),
            format!("{name}: the editor's drag state is {}", drawn["dragging"]),
        )?;
        if draft.is_none() {
            ensure(
                drawn["sample_count"] == json!(257)
                    && single(&drawn["sample_source"]) == expected
                    && drawn["sample_source_entry"] == stack["entry"],
                format!(
                    "{name}: the sampled curve is not the drawn points' at the current entry: {drawn}"
                ),
            )?;
        }
        checks.note(
            frame,
            "the Tone curve editor as drawn",
            json!({"step": name, "points": drawn["points"], "points_open": drawn["points_open"],
                "identity": drawn["identity"], "hint": drawn["hint"], "sample_version": drawn["sample_version"]}),
        );
    }
    Ok(())
}

/// Each scripted step's frame against what the editor logged for that step, from its
/// `script_step` record to the next: a commit puts its own entry on screen, a held drag sends one
/// `draft.set` per pointer move and previews the drafted points without committing, a release
/// commits once, and a view step asks for no render at all.
fn correlate_log(checks: &mut Checks, launch: &Checked) -> Result {
    let mut groups: Vec<Vec<&Value>> = Vec::new();
    for event in &launch.events {
        if event["event"] == "script_step" {
            groups.push(Vec::new());
        }
        if let Some(group) = groups.last_mut() {
            group.push(event);
        }
    }
    let names = launch.names();
    ensure(
        groups.len() + 1 == names.len(),
        format!(
            "The log holds {} script steps for {} frames after the open",
            groups.len(),
            names.len() - 1
        ),
    )?;
    let count = |group: &[&Value], name: &str| group.iter().filter(|e| e["event"] == name).count();
    for (index, group) in groups.iter().enumerate() {
        let name = &names[index + 1];
        let frame = launch.at(name)?;
        let entry = &frame.state()["stack"]["entry"];
        let committed = frame.revision()? > launch.at(&names[index])?.revision()?;
        let drafts = count(group, "slider_draft_set");
        let commits = count(group, "slider_draft_commit");
        let draft = frame.draft();
        if draft["action"] == SET_CURVE {
            let sent: Vec<&Value> = group
                .iter()
                .filter(|e| e["event"] == "slider_draft_set")
                .map(|e| &e["detail"]["fields"])
                .collect();
            let previewed = group
                .iter()
                .rev()
                .find(|e| e["event"] == "slider_draft_preview")
                .map(|e| &e["detail"]["value"]);
            ensure(
                commits == 0
                    && sent.last() == Some(&&draft["fields"])
                    && previewed == Some(&draft["fields"][LUMINANCE]),
                format!(
                    "{name}: the log sent {sent:?} and previewed {previewed:?} for the draft {}, with {commits} commit(s)",
                    draft["fields"]
                ),
            )?;
        } else if committed {
            let shown = group.iter().rev().find_map(|e| match e["event"].as_str() {
                Some("preview_displayed") => Some(&e["detail"]["entry_id"]),
                Some("preview_pixels_reused") => Some(&e["detail"]["identity"]["entry_id"]),
                _ => None,
            });
            ensure(
                shown == Some(entry) && drafts == 0,
                format!(
                    "{name}: the log's last frame for this commit is of {shown:?}, not {entry}, with {drafts} draft set(s)"
                ),
            )?;
        } else {
            ensure(
                count(group, "preview_job_requested") == 0 && drafts == 0 && commits == 0,
                format!("{name}: a step that commits nothing asked for a render or a draft"),
            )?;
        }
        checks.note(
            frame,
            "what the log records for the step",
            json!({"step": name, "draft_sets": drafts, "draft_commits": commits,
                "preview_jobs": count(group, "preview_job_requested"),
                "previews_displayed": count(group, "preview_displayed")}),
        );
    }
    Ok(())
}

/// What the ramp and patches show at each step, once the plan has held.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    let opened = launch.at("opened")?;
    opened.module_available(CURVE_MODULE)?;
    let neutral = launch.at("expanded")?;

    // The fixture as decoded: a ramp from black to white and the four patches.
    let (width, _) = TONE_RAMP_FIXTURE;
    let middle = ramp_row(neutral, RAMP_ROWS[2])?;
    let (first, last) = (middle[0], middle[middle.len() - 1]);
    checks.compare(
        neutral,
        "the ramp's darkest drawn code against its first column's",
        f64::from(first[1]),
        f64::from(tone_ramp_code(0, width)),
        Tolerance::Within(3.0),
    )?;
    checks.compare(
        neutral,
        "the ramp's brightest drawn code against its last column's",
        f64::from(last[1]),
        f64::from(tone_ramp_code(width - 1, width)),
        Tolerance::Within(3.0),
    )?;

    // Curves that are the exact identity map draw the neutral frame's pixels exactly.
    for step in ["identity", "remove", "points-open", "reset"] {
        let frame = launch.at(step)?;
        checks.compare(
            frame,
            &format!(
                "{step}: the largest channel difference from the neutral frame over the photograph"
            ),
            f64::from(photo_difference(frame, neutral)?),
            0.0,
            Tolerance::Within(0.0),
        )?;
    }

    // Every frame with a curve, drafted or committed, against the neutral one.
    for step in [
        "identity",
        "drag",
        "release",
        "add",
        "remove",
        "s-point",
        "typed-low",
        "s-curve",
        "reset",
    ] {
        relative(&mut checks, step, launch.at(step)?, neutral)?;
    }

    // The drag lifts the whole ramp, drafted and released alike, and the release commits what the
    // draft showed.
    launch.at("drag")?.displays_draft()?;
    for step in ["drag", "release"] {
        let frame = launch.at(step)?;
        for upper in [false, true] {
            checks.compare(
                frame,
                &format!(
                    "{step}: the ramp's {} half against the neutral one's (mean luminance, codes)",
                    if upper { "upper" } else { "lower" }
                ),
                ramp_half(frame, upper)?,
                ramp_half(neutral, upper)?,
                Tolerance::Above(CHANGED),
            )?;
        }
    }

    // The S-curve lowers the lower half and raises the upper half.
    let s_curve = launch.at("s-curve")?;
    checks.compare(
        s_curve,
        "s-curve: the neutral ramp's lower half against the S-curve's (mean luminance, codes)",
        ramp_half(neutral, false)?,
        ramp_half(s_curve, false)?,
        Tolerance::Above(CHANGED),
    )?;
    checks.compare(
        s_curve,
        "s-curve: the S-curve's ramp upper half against the neutral one's (mean luminance, codes)",
        ramp_half(s_curve, true)?,
        ramp_half(neutral, true)?,
        Tolerance::Above(CHANGED),
    )?;

    // The Points list: closed until the step opens it, open from then on, and a hint under the
    // plot throughout.
    let points_open =
        |step: &str| -> Result<Value> { Ok(drawn_curve(launch.at(step)?)?["points_open"].clone()) };
    ensure(
        points_open("remove")? == json!(false) && points_open("points-open")? == json!(true),
        "The Points list did not start closed and open on its step",
    )?;
    for step in ["s-point", "typed-low", "s-curve", "reset", "masked-release"] {
        ensure(
            points_open(step)? == json!(true),
            format!("The Points list closed by {step}"),
        )?;
    }

    // The mask: its chip on the Tone curve band while the curve is dragged through it, the drag
    // bound to it and darkening the ramp inside it only.
    for step in ["masked-drag", "masked-release"] {
        let scopes = &launch.at(step)?.state()["scopes"];
        ensure(
            scopes[CURVE_MODULE] == json!(MASK),
            format!("{step}: the Tone curve band carries no {MASK} chip: {scopes}"),
        )?;
    }
    let released = launch.at("masked-release")?;
    let mask_id = released.only_mask()?["id"].clone();
    let masked = released.state()["stack"]["layers"]
        .as_array()
        .and_then(|layers| {
            layers
                .iter()
                .find(|layer| layer["effect"] == CURVE_EFFECT && layer["mask"] == mask_id)
        })
        .ok_or("The masked drag committed no Tone curve layer bound to the mask")?;
    let points = [[0.0, 0.0], [1.0, wide(MASKED[1])]];
    ensure(
        masked["payload"] == points_payload(&points),
        format!("The masked Tone curve layer holds {}", masked["payload"]),
    )?;
    let applied = launch.at("radial-applied")?;
    let inside = [(RADIAL_FROM[0] + RADIAL_TO[0]) / 2.0, RADIAL_FROM[1] + 0.05];
    let outside = [0.2, 0.25];
    for step in ["masked-drag", "masked-release"] {
        let frame = launch.at(step)?;
        checks.compare(
            frame,
            &format!(
                "{step}: inside the radial against the applied radial's frame (luminance, codes)"
            ),
            applied.luminance_at(inside, 3)?,
            frame.luminance_at(inside, 3)?,
            Tolerance::Above(CHANGED),
        )?;
        checks.compare(
            frame,
            &format!(
                "{step}: outside the radial against the applied radial's frame (luminance, codes)"
            ),
            frame.luminance_at(outside, 3)?,
            applied.luminance_at(outside, 3)?,
            Tolerance::Within(1.0),
        )?;
    }

    correlate(&mut checks, launch)?;
    correlate_log(&mut checks, launch)?;

    checks.write(
        &launch.evidence,
        "curve",
        json!({
            "neutral_step": "expanded",
            "ramp_rows": RAMP_ROWS,
            "inset_px": INSET,
            "edge_px": EDGE,
            "spread_codes": SPREAD,
            "hue_degrees": HUE,
            "clipped_code": CLIPPED,
            "changed_codes": CHANGED,
            "scope": "Capture pixels of the photograph the editor records drawing, read back from the renderer and compared with the neutral frame of the same launch at the same capture pixels; hue is the Oklab angle of a patch's mean sRGB, not a colorimetric claim",
        }),
    )
}
