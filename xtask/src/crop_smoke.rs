//! The crop smoke scenarios: each one's plan, the steps the editor runs with what each commits, and
//! the checks its captured frames must satisfy beyond the plan.
//!
//! The steps drive the editor's own crop paths — the module's `crop` and `crop-fit` actions through
//! the owner, and the draft through the messages the panel and the canvas publish — and every frame
//! is checked against the crop state recorded with it, so a capture proves the rectangle, angle and
//! output it claims.
use crate::{
    fixtures::COLORS,
    scenario::{Checked, Fixture, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_core::{BoxRect, CROP_EFFECT, CropPayload, CropStage};

/// The fixture both scenarios open, and therefore the crop layer's input stage.
const STAGE: (u32, u32) = (480, 320);
/// Expanded by its own default: `crop-draft` collapses it so the crop section is on screen.
const BASIC_MODULE: &str = "luxforge.basic";
const CROP_MODULE: &str = "luxforge.crop";
/// The straightening angle the scripted `edit.crop` commits.
const ANGLE: f64 = 7.0;
/// How much one scripted nudge adds to it before Apply.
const NUDGE: f64 = 0.5;

fn stage(angle: f64) -> CropStage {
    CropStage {
        width: STAGE.0,
        height: STAGE.1,
        angle,
    }
}

/// The off-centre rectangle the scripted `edit.crop` commits at [`ANGLE`]. It comes from the core's
/// own fitting, so it is covered by the rotated source and lands on whole box pixels; the runner and
/// the editor therefore agree on it without the runner reimplementing the geometry.
fn off_centre() -> Result<CropPayload> {
    let stage = stage(ANGLE);
    let (box_width, box_height) = stage.bounding_box();
    let wanted = BoxRect::from_center(
        (box_width * 0.42, box_height * 0.56),
        box_width * 0.5,
        box_height * 0.45,
    );
    let payload = stage.fit_about_center(wanted).normalized(&stage);
    // Prove the payload is valid before the editor ever sees it: a rejected request would be
    // evidence of the harness, not of the editor.
    stage.covers(&stage.fit_about_center(wanted));
    payload.output_rect(&stage)?;
    Ok(payload)
}

/// Where `crop-draft` releases its drag on the angle's rail: 2.4° on the −45..+45° rail, a hair
/// past it so the rail's 0.05° step, not float arithmetic, lands the angle.
const RAIL_ANGLE_FRACTION: f64 = 0.5 + RAIL_ANGLE / 90.0 + 1e-4;
/// The angle `crop-draft` drags the rail to, as crop-and-straighten.png draws it.
const RAIL_ANGLE: f64 = 2.4;

/// The ratio chips a committed crop reads as, in the order the crop module declares them, each with
/// its width over height (`None` for Original, which is the input stage's own).
const READ_RATIOS: [(&str, Option<f64>); 5] = [
    ("Original", None),
    ("1:1", Some(1.0)),
    ("3:2", Some(1.5)),
    ("4:3", Some(4.0 / 3.0)),
    ("16:9", Some(16.0 / 9.0)),
];

/// The chip the idle section should show chosen for a committed whole-pixel output on `stage`,
/// computed here independently of the editor: the first declared ratio, in either orientation,
/// whose long side is within `2r` pixels of the short side times `r`, since fitting a ratio snaps
/// each extent inward by less than two pixels; otherwise Free.
fn reads_as(stage: (u32, u32), output: [u32; 2]) -> &'static str {
    let (width, height) = (f64::from(output[0]), f64::from(output[1]));
    let (long, short) = (width.max(height), width.min(height));
    READ_RATIOS
        .iter()
        .find(|(_, ratio)| {
            let ratio = ratio.unwrap_or(f64::from(stage.0) / f64::from(stage.1));
            let wide = ratio.max(1.0 / ratio);
            (long - wide * short).abs() <= 2.0 * wide
        })
        .map_or("Free", |(label, _)| label)
}

/// The idle crop section a frame recorded: not drafting, reading `chosen` at `angle`, its lock
/// closed exactly when a ratio is chosen, and its controls acting.
fn idle_section(frame: &Value, chosen: &str, angle: &str) -> Result<Value> {
    let section = &frame["state"]["crop"]["section"];
    let locked = chosen != "Free";
    ensure(
        frame["state"]["crop"]["drafting"] == json!(false)
            && section["drafting"] == json!(false)
            && section["enabled"] == json!(true)
            && section["chosen"] == json!(chosen)
            && section["locked"] == json!(locked)
            && section["can_swap"] == json!(locked)
            && section["angle"] == json!(angle),
        format!("The idle crop section is {section}, expected {chosen} at {angle}°"),
    )?;
    Ok(section.clone())
}

/// A step on the crop draft or the view: it commits nothing.
fn uncommitted(name: &str, script: Value) -> Step {
    Step::new(name, script).commits(0)
}

/// Every `crop` frame, in order: the open, then one per step. Module actions first, then a draft on
/// the crop layer they committed. The expectations here are what each step commits and records;
/// `verify` checks the crop state and what the photograph shows.
pub fn plan(_: &[PathBuf]) -> Plan {
    // The rectangle comes from the core's own fitting of constants, so it is the same on every
    // call, and a unit test proves it is covered before any editor sees it.
    let payload = off_centre().expect("the scripted off-centre rectangle is covered");
    Plan::new(vec![
        Step::opened("opened"),
        // A 16:9 fit at angle zero appends the one crop layer.
        Step::new(
            "fit",
            json!({"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":0.0}}}),
        )
        .commits(1)
        .label("Crop 16:9"),
        // An off-centre straightened rectangle updates that same layer in place.
        Step::new(
            "straightened",
            json!({"api":{"method":"edit.crop","params":{"angle":ANGLE,"x":payload.x,"y":payload.y,"width":payload.width,"height":payload.height}}}),
        )
        .commits(1)
        .label("Crop 7\u{b0}")
        .same_layer(CROP_EFFECT, "fit"),
        // A draft on that layer, an angle change and a discard: nothing commits.
        uncommitted("started", json!({"draft":{"start":true}})),
        uncommitted("angled", json!({"draft":{"angle":12.0}})),
        uncommitted("cancelled", json!({"draft":{"cancel":true}})),
        // A second draft, one nudge and Apply, which commits once to the same layer.
        uncommitted("restarted", json!({"draft":{"start":true}})),
        uncommitted("nudged", json!({"draft":{"nudge":NUDGE}})),
        Step::new("applied", json!({"draft":{"apply":true}}))
            .commits(1)
            .label("Crop 7.5\u{b0}")
            .same_layer(CROP_EFFECT, "fit"),
    ])
}

/// Every `crop-draft` frame: the draft's own gestures and controls, at Fit and at 100%. Basic is
/// collapsed and the section expanded first, so the idle section is on screen, and the panel is
/// scrolled to its end once the draft is open, so the drafting section is too. The angle is then
/// dragged on its rail to 2.4°, so the section shows a straightened draft. After Apply the idle
/// section reads the committed square, and pressing 16:9 there opens a draft on it with that
/// ratio; Cancel returns to the idle section with nothing committed.
pub fn draft_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        uncommitted(
            "basic-collapsed",
            json!({"section":{"module":BASIC_MODULE,"expanded":false}}),
        )
        .collapsed(BASIC_MODULE),
        // The idle section, expanded under a collapsed Basic.
        uncommitted(
            "crop-expanded",
            json!({"section":{"module":CROP_MODULE,"expanded":true}}),
        )
        .expanded(CROP_MODULE)
        .collapsed(BASIC_MODULE),
        // A neutral draft on a stack without a crop layer.
        uncommitted("started", json!({"draft":{"start":true}})).no_layer(CROP_EFFECT),
        uncommitted("rect", json!({"draft":{"rect":[40.0,24.0,300.0,200.0]}})),
        uncommitted("square", json!({"draft":{"preset":"1:1"}})),
        uncommitted("scrolled", json!({"tools_scroll":1.0})),
        uncommitted(
            "rail",
            json!({"draft":{"angle_rail":[0.6,RAIL_ANGLE_FRACTION]}}),
        ),
        uncommitted("percent", json!({"view":{"zoom":"100"}})),
        uncommitted("fit", json!({"view":{"zoom":"fit"}})),
        // Apply commits the straightened square once.
        Step::new("applied", json!({"draft":{"apply":true}}))
            .commits(1)
            .label("Crop 2.4\u{b0}"),
        // 16:9 pressed in the idle section opens a draft on the committed layer.
        uncommitted("idle-preset", json!({"draft":{"preset":"16:9"}})),
        // Cancel ends it with the committed layer untouched.
        uncommitted("cancelled", json!({"draft":{"cancel":true}}))
            .same_layer(CROP_EFFECT, "applied"),
    ])
}

/// The one crop layer of a frame's committed stack: its identity, its payload and the whole-pixel
/// output that payload declares.
fn committed(frame: &Value) -> Result<(String, CropPayload, [u32; 2])> {
    let layers = frame["state"]["stack"]["layers"]
        .as_array()
        .ok_or("The frame records no committed stack")?;
    let crops: Vec<&Value> = layers
        .iter()
        .filter(|layer| layer["effect"] == CROP_EFFECT)
        .collect();
    ensure(
        crops.len() == 1,
        format!("Expected exactly one crop layer, found {}", crops.len()),
    )?;
    let layer = crops[0];
    let id = layer["id"]
        .as_str()
        .ok_or("The crop layer has no identity")?
        .to_owned();
    let payload: CropPayload = serde_json::from_value(layer["payload"].clone())?;
    let rect = payload.output_rect(&stage(payload.angle))?;
    Ok((id, payload, [rect.width, rect.height]))
}

/// A committed crop frame: the displayed image has the ratio the committed payload declares.
fn shows_committed(frame: &Frame, quadrants: bool) -> Result<Value> {
    let (_, _, output) = committed(frame)?;
    let aspect = f64::from(output[0]) / f64::from(output[1]);
    let measured = frame.fixture(Fixture {
        aspect: Some(aspect),
        // One displayed pixel of slack on the shorter axis, plus the detector's own four-pixel
        // step.
        tolerance: 0.02,
        min_height: 0.25,
        quadrants,
        ..Fixture::fit(1)
    })?;
    Ok(json!({"output_dimensions":output,"displayed":measured}))
}

fn fixture_colour(pixel: [u8; 3]) -> bool {
    COLORS
        .iter()
        .any(|colour| pixel.iter().zip(colour).all(|(a, b)| a.abs_diff(*b) <= 8))
}

/// The overlay's frame and handles are white at 0.9 alpha, so every channel stays high whatever they
/// are drawn over.
fn whitish(pixel: [u8; 3]) -> bool {
    pixel.iter().all(|channel| *channel >= 190)
}

/// How far a pixel is from the window background, as the largest per-channel difference. Dimming is
/// an alpha blend toward that background, so this falls where the stage is dimmed and stays high
/// where it is drawn at full opacity, whichever channels the colour happens to use. Luminance does
/// not work here: the fixture's red quadrant keeps most of its luminance when its red channel halves.
fn from_background(pixel: [u8; 3], background: [u8; 3]) -> f64 {
    pixel
        .iter()
        .zip(background)
        .map(|(a, b)| f64::from(a.abs_diff(b)))
        .fold(0.0, f64::max)
}

/// Is there a solid whitish block within `search` pixels of this point? A handle is a filled square,
/// so a block rather than a single pixel distinguishes it from bright image detail.
fn handle_at(image: &image::RgbImage, centre: (i64, i64), search: i64, half: i64) -> bool {
    let (width, height) = image.dimensions();
    let solid = |x: i64, y: i64| {
        (-half..=half).all(|dy| {
            (-half..=half).all(|dx| {
                let (px, py) = (x + dx, y + dy);
                px >= 0
                    && py >= 0
                    && px < i64::from(width)
                    && py < i64::from(height)
                    && whitish(image.get_pixel(px as u32, py as u32).0)
            })
        })
    };
    (-search..=search).any(|dy| (-search..=search).any(|dx| solid(centre.0 + dx, centre.1 + dy)))
}

/// A crop-draft frame: the rectangle drawn at full opacity, the eight handles on its edges and the
/// dimmed stage around it. Every expectation comes from the crop summary captured with the frame.
fn shows_draft(frame: &Frame) -> Result<Value> {
    let draft = &frame["state"]["crop"];
    ensure(
        draft["drafting"] == json!(true),
        "The frame's state is not a crop draft",
    )?;
    ensure(
        draft["input_stage_loaded"] == json!(true),
        "The draft's input stage never reached the GPU",
    )?;
    ensure(
        draft["paused"] == json!(false) && draft["conflicted"] == json!(false),
        "The draft was paused or conflicted",
    )?;
    let rect: [f64; 4] = serde_json::from_value(draft["rect"].clone())?;
    let input: [u32; 2] = serde_json::from_value(draft["input_stage"].clone())?;
    ensure(
        input == [STAGE.0, STAGE.1],
        format!("The draft's input stage is {input:?}"),
    )?;
    let angle = draft["angle"]
        .as_f64()
        .ok_or("The draft records no angle")?;
    let stage = stage(angle);
    let (box_width, box_height) = stage.bounding_box();

    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let [surface_left, surface_right] = frame.columns()?.unwrap_or([0, width]);
    ensure(
        surface_left < surface_right && surface_right <= width,
        "Invalid surface columns",
    )?;
    // Only the crop rectangle is drawn at full opacity, so the fixture's own colours bound it.
    let mut bounds: Option<[u32; 4]> = None;
    for y in 0..height {
        for x in surface_left..surface_right {
            if fixture_colour(image.get_pixel(x, y).0) {
                bounds = Some(match bounds {
                    None => [x, y, x, y],
                    Some([left, top, right, bottom]) => {
                        [left.min(x), top.min(y), right.max(x), bottom.max(y)]
                    }
                });
            }
        }
    }
    let [left, top, right, bottom] =
        bounds.ok_or("No full-opacity crop pixels: the draft did not render")?;
    // The one-pixel frame covers the rectangle's own edge, so the measured region is inset by a
    // pixel or two on every side; the scale it implies is still exact to well under a percent.
    let (drawn_width, drawn_height) = (f64::from(right - left), f64::from(bottom - top));
    let (scale_x, scale_y) = (drawn_width / rect[2], drawn_height / rect[3]);
    ensure(
        scale_x.is_finite() && scale_x > 0.4 && scale_y > 0.4,
        format!("The drawn crop is {drawn_width}x{drawn_height} for a {rect:?} rectangle"),
    )?;
    ensure(
        (scale_x / scale_y - 1.0).abs() <= 0.04,
        format!(
            "The drawn crop's ratio {:.4} is not the draft's {:.4}",
            drawn_width / drawn_height,
            rect[2] / rect[3]
        ),
    )?;
    let (mid_x, mid_y) = ((left + right) / 2, (top + bottom) / 2);
    let mut found = Vec::new();
    for (name, point) in [
        ("top-left", (left, top)),
        ("top", (mid_x, top)),
        ("top-right", (right, top)),
        ("left", (left, mid_y)),
        ("right", (right, mid_y)),
        ("bottom-left", (left, bottom)),
        ("bottom", (mid_x, bottom)),
        ("bottom-right", (right, bottom)),
    ] {
        ensure(
            handle_at(image, (i64::from(point.0), i64::from(point.1)), 6, 2),
            format!("No overlay handle at the {name} of the crop rectangle"),
        )?;
        found.push(name);
    }

    // Dimming: a band just outside the rectangle against one just inside it, both over real source
    // content. A draft that covers the whole stage has no outside to compare.
    let (gap, band) = (10.0, 12.0);
    // The window's own padding, left of the photo surface, is the background every dim blends into.
    let background = image.get_pixel(1, 1).0;
    let mean = |x0: f64, y0: f64, x1: f64, y1: f64| -> Option<f64> {
        let screen = |bx: f64, by: f64| {
            (
                f64::from(left) + (bx - rect[0]) * scale_x,
                f64::from(top) + (by - rect[1]) * scale_y,
            )
        };
        let (sx0, sy0) = screen(x0, y0);
        let (sx1, sy1) = screen(x1, y1);
        let clamp = |value: f64, limit: u32| value.max(0.0).min(f64::from(limit - 1)) as u32;
        let (x0, y0) = (clamp(sx0, width), clamp(sy0, height));
        let (x1, y1) = (clamp(sx1, width), clamp(sy1, height));
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let mut total = 0.0;
        let mut count = 0u32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                total += from_background(image.get_pixel(x, y).0, background);
                count += 1;
            }
        }
        (count > 0).then(|| total / f64::from(count))
    };
    let (inner_x0, inner_x1) = (rect[0] + rect[2] * 0.3, rect[0] + rect[2] * 0.7);
    let (inner_y0, inner_y1) = (rect[1] + rect[3] * 0.3, rect[1] + rect[3] * 0.7);
    let sides = [
        (
            "above",
            [inner_x0, rect[1] - gap - band, inner_x1, rect[1] - gap],
            [inner_x0, rect[1] + gap, inner_x1, rect[1] + gap + band],
        ),
        (
            "below",
            [
                inner_x0,
                rect[1] + rect[3] + gap,
                inner_x1,
                rect[1] + rect[3] + gap + band,
            ],
            [
                inner_x0,
                rect[1] + rect[3] - gap - band,
                inner_x1,
                rect[1] + rect[3] - gap,
            ],
        ),
        (
            "left of",
            [rect[0] - gap - band, inner_y0, rect[0] - gap, inner_y1],
            [rect[0] + gap, inner_y0, rect[0] + gap + band, inner_y1],
        ),
        (
            "right of",
            [
                rect[0] + rect[2] + gap,
                inner_y0,
                rect[0] + rect[2] + gap + band,
                inner_y1,
            ],
            [
                rect[0] + rect[2] - gap - band,
                inner_y0,
                rect[0] + rect[2] - gap,
                inner_y1,
            ],
        ),
    ];
    // The outside band must lie over the rotated source, or it would measure the window background
    // instead of a dimmed photograph.
    let covered = |[x0, y0, x1, y1]: [f64; 4]| {
        [0.0, 0.5, 1.0].iter().all(|fx| {
            [0.0, 0.5, 1.0]
                .iter()
                .all(|fy| stage.contains(x0 + (x1 - x0) * fx, y0 + (y1 - y0) * fy))
        })
    };
    let dimming = match sides
        .into_iter()
        .filter(|(_, outside, _)| covered(*outside))
        .find_map(|(name, outside, inside)| {
            let outside = mean(outside[0], outside[1], outside[2], outside[3])?;
            let inside = mean(inside[0], inside[1], inside[2], inside[3])?;
            Some((name, outside, inside))
        }) {
        Some((name, outside, inside)) => {
            ensure(
                inside >= 20.0,
                format!(
                    "The band inside the crop is indistinguishable from the window background: {inside:.1}"
                ),
            )?;
            ensure(
                outside < inside * 0.75,
                format!(
                    "The stage {name} the crop is not dimmed: it sits {outside:.1} from the background against {inside:.1} inside"
                ),
            )?;
            json!({"side":name,"background_rgb":background,"outside_distance":outside,"inside_distance":inside,"ratio":outside/inside,"threshold":0.75})
        }
        None => json!({
            "scope":"not applicable: no band outside the rectangle lies over the source",
        }),
    };
    Ok(json!({
        "angle":angle,
        "rect":rect,
        "box":[box_width,box_height],
        "drawn_bounds":[left,top,right,bottom],
        "box_pixels_per_captured_pixel":[scale_x,scale_y],
        "handles":found,
        "dimming":dimming,
        "scope":"Overlay presence, drawn ratio and dimming from the frame's own crop summary; the committed render is verified separately",
    }))
}

/// The event whose detail matches this frame's crop summary, proving the log and the capture describe
/// the same draft.
fn correlated(events: &[Value], name: &str, frame: &Value) -> Result<Value> {
    let draft = &frame["state"]["crop"];
    let found = events
        .iter()
        .filter(|event| event["event"] == name)
        .find(|event| {
            let detail = &event["detail"];
            let detail = if detail.get("draft").is_some() {
                &detail["draft"]
            } else {
                detail
            };
            detail["angle"] == draft["angle"] && detail["rect"] == draft["rect"]
        })
        .ok_or_else(|| {
            format!(
                "No {name} event matches the captured draft (angle {}, rect {})",
                draft["angle"], draft["rect"]
            )
        })?;
    Ok(found.clone())
}

/// The opened frame both scenarios start from: the fixture, ready at Fit. Returns the checks record
/// it starts.
fn opened(launch: &Checked) -> Result<Vec<Value>> {
    let opened = launch.at("opened")?;
    ensure(
        opened["state"]["source_dimensions"] == json!([STAGE.0, STAGE.1])
            && opened["state"]["phase"] == "ready",
        "The fixture did not open",
    )?;
    Ok(vec![
        json!({"frame":opened["file"],"shows":"the fixture at Fit","pixels":opened.fixture(Fixture::fit(1))?}),
    ])
}

/// What the `crop` steps show, once the plan has held: the crop state each frame recorded, the
/// events that correlate with it and the photograph drawn.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let events = &launch.events;
    let mut checks = opened(launch)?;
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };
    let fit = launch.at("fit")?;
    let straightened = launch.at("straightened")?;
    let started = launch.at("started")?;
    let angled = launch.at("angled")?;
    let cancelled = launch.at("cancelled")?;
    let restarted = launch.at("restarted")?;
    let nudged = launch.at("nudged")?;
    let applied = launch.at("applied")?;

    // (a) A 16:9 fit at angle zero appends the one crop layer.
    let (layer, fitted, output) = committed(fit)?;
    ensure(fitted.angle == 0.0, "The fit changed the angle")?;
    let ratio = f64::from(output[0]) / f64::from(output[1]);
    ensure(
        (ratio - 16.0 / 9.0).abs() <= 3.0 / f64::from(output[1]),
        format!("The fitted crop is {output:?}, which is not 16:9 within a pixel"),
    )?;
    record(
        fit,
        "a 16:9 crop-fit at angle 0",
        shows_committed(fit, true)?,
    );
    // The crop section, collapsed here, reads the fitted crop as 16:9 with its lock closed.
    ensure(
        reads_as(STAGE, output) == "16:9",
        format!("The fitted crop {output:?} does not read as 16:9"),
    )?;
    record(
        fit,
        "the crop section reading the fit as 16:9",
        idle_section(fit, "16:9", "0")?,
    );

    // (b) An off-centre straightened rectangle updates that same layer in place, which the plan
    // holds; the payload is the one that was sent.
    let wanted = off_centre()?;
    let (_, committed_payload, straightened_output) = committed(straightened)?;
    let straightened_reads = reads_as(STAGE, straightened_output);
    record(
        straightened,
        "the crop section reading the straightened crop at 7 degrees",
        idle_section(straightened, straightened_reads, "7")?,
    );
    ensure(
        committed_payload.angle == ANGLE
            && [
                (committed_payload.x, wanted.x),
                (committed_payload.y, wanted.y),
                (committed_payload.width, wanted.width),
                (committed_payload.height, wanted.height),
            ]
            .iter()
            .all(|(saved, sent)| (saved - sent).abs() <= f64::EPSILON * 8.0),
        format!("The committed payload is not the one that was sent: {committed_payload:?}"),
    )?;
    record(
        straightened,
        "an off-centre 7 degree crop",
        shows_committed(straightened, false)?,
    );

    // (c) A draft on that layer, an angle change and a discard; the plan holds that none of them
    // commits.
    let draft = &started["state"]["crop"];
    ensure(
        draft["layer"] == json!(layer) && draft["angle"] == json!(ANGLE),
        "The draft did not open on the committed crop layer",
    )?;
    // The draft seeds the ratio the idle section showed, so opening it moves no chip.
    ensure(
        draft["section"]["chosen"] == straightened["state"]["crop"]["section"]["chosen"]
            && draft["section"]["locked"] == straightened["state"]["crop"]["section"]["locked"],
        format!(
            "Opening the draft changed the chosen ratio: {} idle, {} drafting",
            straightened["state"]["crop"]["section"], draft["section"]
        ),
    )?;
    let output = committed_payload.output_rect(&stage(ANGLE))?;
    ensure(
        draft["rect"]
            == json!([
                output.x as f64,
                output.y as f64,
                f64::from(output.width),
                f64::from(output.height)
            ]),
        format!(
            "The draft did not open at the committed rectangle: {}",
            draft["rect"]
        ),
    )?;
    record(
        started,
        "a draft opened on the committed crop",
        json!({"overlay":shows_draft(started)?,"event":correlated(events, "crop_draft_started", started)?["elapsed_ms"]}),
    );
    ensure(
        angled["state"]["crop"]["angle"] == json!(12.0),
        "The scripted angle did not reach the draft",
    )?;
    ensure(
        committed(angled)?.1.angle == ANGLE,
        "A draft change altered the committed stack",
    )?;
    record(
        angled,
        "the same draft straightened to 12 degrees",
        json!({"overlay":shows_draft(angled)?,"event":correlated(events, "crop_draft_changed", angled)?["elapsed_ms"]}),
    );
    ensure(
        cancelled["state"]["crop"]["drafting"] == json!(false)
            && committed(cancelled)?.1.angle == ANGLE,
        "Cancel did not discard the draft or changed the committed crop",
    )?;
    ensure(
        events
            .iter()
            .any(|event| event["event"] == "crop_draft_discarded"),
        "Cancel logged no discard",
    )?;
    record(
        cancelled,
        "the committed crop again after Cancel",
        json!({"pixels":shows_committed(cancelled, false)?,"section":idle_section(cancelled, straightened_reads, "7")?}),
    );

    // (d) A second draft, one nudge and Apply, which the plan holds as one commit to the same
    // layer.
    ensure(
        restarted["state"]["crop"]["angle"] == json!(ANGLE),
        "The second draft did not reopen at the committed angle",
    )?;
    record(
        restarted,
        "a second draft on the same layer",
        shows_draft(restarted)?,
    );
    ensure(
        nudged["state"]["crop"]["angle"] == json!(ANGLE + NUDGE),
        "The nudge did not reach the draft",
    )?;
    record(
        nudged,
        "the draft nudged half a degree",
        shows_draft(nudged)?,
    );
    let (_, applied_payload, nudged_output) = committed(applied)?;
    ensure(
        applied_payload.angle == ANGLE + NUDGE,
        format!("Apply committed angle {}", applied_payload.angle),
    )?;
    ensure(
        applied["state"]["crop"]["drafting"] == json!(false),
        "Apply left the draft open",
    )?;
    let applied_event = correlated(events, "crop_draft_applied", nudged)?;
    ensure(
        applied_event["detail"]["revision"] == json!(applied.revision()?)
            && applied_event["detail"]["entry_id"] == applied["state"]["stack"]["entry"],
        "The applied event does not name the committed entry and revision",
    )?;
    record(
        applied,
        "the applied crop",
        json!({"pixels":shows_committed(applied, false)?,"section":idle_section(applied, reads_as(STAGE, nudged_output), "7.5")?}),
    );
    write_json(&launch.evidence.join("crop-checks.json"), &json!(checks))?;
    Ok(())
}

/// What the `crop-draft` steps show, once the plan has held: the crop state each frame recorded,
/// the events that correlate with it and the overlay drawn.
pub fn verify_draft(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let events = &launch.events;
    let mut checks = opened(launch)?;
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };
    let expanded = launch.at("crop-expanded")?;
    let started = launch.at("started")?;
    let rect_frame = launch.at("rect")?;
    let square_frame = launch.at("square")?;
    let scrolled = launch.at("scrolled")?;
    let rail = launch.at("rail")?;
    let percent = launch.at("percent")?;
    let fit = launch.at("fit")?;
    let applied = launch.at("applied")?;
    let idle_preset = launch.at("idle-preset")?;
    let cancelled = launch.at("cancelled")?;

    // The idle section, expanded under a collapsed Basic as the plan holds, with no draft yet: the
    // Ratio and Angle controls reading an uncropped stack, Free at 0° with the lock open.
    ensure(
        expanded["state"]["crop"]["drafting"] != json!(true),
        "The idle crop section is drafting before the draft starts",
    )?;
    record(
        expanded,
        "the idle section on an uncropped stack: Free at 0 degrees",
        idle_section(expanded, "Free", "0")?,
    );
    // A neutral draft on a stack without a crop layer: the whole stage.
    let draft = &started["state"]["crop"];
    ensure(
        draft["layer"] == Value::Null
            && draft["angle"] == json!(0.0)
            && draft["rect"] == json!([0.0, 0.0, f64::from(STAGE.0), f64::from(STAGE.1)]),
        format!("A neutral draft is not the whole stage: {draft}"),
    )?;
    record(
        started,
        "a neutral draft over the whole stage",
        shows_draft(started)?,
    );
    // Two corner gestures reach the rectangle exactly in Free mode.
    ensure(
        rect_frame["state"]["crop"]["rect"] == json!([40.0, 24.0, 300.0, 200.0]),
        format!(
            "The scripted corner gestures produced {}",
            rect_frame["state"]["crop"]["rect"]
        ),
    )?;
    record(
        rect_frame,
        "an off-centre rectangle from two corner gestures",
        shows_draft(rect_frame)?,
    );
    // The declared 1:1 preset keeps the centre and fits inside that rectangle.
    let square = &square_frame["state"]["crop"];
    ensure(
        square["preset"] == json!("1:1") && square["rect"] == json!([90.0, 24.0, 200.0, 200.0]),
        format!("The 1:1 preset produced {}", square["rect"]),
    )?;
    record(
        square_frame,
        "the declared 1:1 preset, centred on the same rectangle",
        shows_draft(square_frame)?,
    );
    // Scrolling the panel to the drafting section leaves the draft as it was.
    ensure(
        scrolled["state"]["crop"]["rect"] == square["rect"]
            && scrolled["state"]["crop"]["preset"] == square["preset"]
            && scrolled["state"]["tools_scroll"] == json!(1.0),
        "Scrolling the tools panel changed the draft",
    )?;
    record(
        scrolled,
        "the drafting section scrolled into view",
        shows_draft(scrolled)?,
    );
    // A drag on the angle's rail, released: the draft's angle follows the rail on its step, the
    // 1:1 ratio holds, the release is the one logged change, and nothing commits (the plan holds
    // that).
    let straightened = &rail["state"]["crop"];
    let rect = straightened["rect"].as_array();
    ensure(
        straightened["angle"] == json!(RAIL_ANGLE)
            && straightened["preset"] == json!("1:1")
            && rect.is_some_and(|rect| rect.len() == 4 && rect[2] == rect[3]),
        format!("The angle rail produced {straightened}"),
    )?;
    record(
        rail,
        "the angle dragged on its rail to 2.4°, the square refitted, nothing committed",
        json!({"overlay":shows_draft(rail)?,"event":correlated(events, "crop_draft_changed", rail)?["elapsed_ms"]}),
    );
    // The overlay follows the view: 100% draws the box at one input pixel per physical pixel.
    record(percent, "the same draft at 100%", shows_draft(percent)?);
    record(fit, "the same draft back at Fit", shows_draft(fit)?);
    // Apply, which the plan holds as the one commit since the open.
    let (_, applied_payload, output) = committed(applied)?;
    ensure(
        applied_payload.angle == RAIL_ANGLE && output[0] == output[1],
        format!(
            "Apply committed {output:?} at angle {}",
            applied_payload.angle
        ),
    )?;
    ensure(
        applied["state"]["crop"]["drafting"] == json!(false),
        "Apply did not end the draft",
    )?;
    correlated(events, "crop_draft_applied", fit)?;
    // The square crop's own quarter points straddle the fixture's centre line, so this frame
    // proves the four quadrants are present rather than sampling their corners.
    record(
        applied,
        "the applied straightened square crop",
        shows_committed(applied, false)?,
    );
    // The idle section now reads that committed crop: 1:1 chosen with the lock closed, at the
    // rail's 2.4°, the chip computed here from the committed output.
    let (layer, square, output) = committed(applied)?;
    let expected = reads_as(STAGE, output);
    ensure(
        expected == "1:1",
        format!("The committed square {output:?} reads as {expected}"),
    )?;
    record(
        applied,
        "the idle section reading the committed square at 2.4 degrees",
        idle_section(applied, expected, "2.4")?,
    );

    // 16:9 pressed in the idle section opens a draft on the committed layer, seeded as Start seeds
    // it (1:1 at 2.4°, as the started event records), then applies 16:9 to it: the canvas is in
    // crop mode with a 16:9 frame at the same angle, and nothing commits (the plan holds that).
    let draft = &idle_preset["state"]["crop"];
    let rect: [f64; 4] = serde_json::from_value(draft["rect"].clone())?;
    ensure(
        draft["drafting"] == json!(true)
            && draft["layer"] == json!(layer)
            && draft["preset"] == json!("16:9")
            && draft["angle"] == json!(RAIL_ANGLE)
            && (rect[2] - rect[3] * 16.0 / 9.0).abs() <= 2.0 * 16.0 / 9.0
            && draft["section"]["chosen"] == json!("16:9")
            && idle_preset["state"]["workspace"]["mode"] == json!(CROP_MODULE),
        format!("16:9 from the idle section produced {draft}"),
    )?;
    let seeded = events
        .iter()
        .rfind(|event| event["event"] == "crop_draft_started")
        .ok_or("The idle change logged no draft start")?;
    ensure(
        seeded["detail"]["layer"] == json!(layer)
            && seeded["detail"]["preset"] == json!("1:1")
            && seeded["detail"]["angle"] == json!(RAIL_ANGLE),
        format!(
            "The draft the idle change opened was not seeded from the committed crop: {}",
            seeded["detail"]
        ),
    )?;
    record(
        idle_preset,
        "16:9 pressed in the idle section: a draft on the committed square, refitted to 16:9",
        json!({"overlay":shows_draft(idle_preset)?,"seeded":seeded["detail"],"changed":correlated(events, "crop_draft_changed", idle_preset)?["detail"]}),
    );

    // Cancel ends that draft: the plan holds the same layer and nothing committed, and the idle
    // section reads the unchanged committed square again.
    let (_, unchanged, _) = committed(cancelled)?;
    ensure(
        unchanged == square,
        "Cancelling the idle change's draft changed the committed crop",
    )?;
    record(
        cancelled,
        "the idle section again after Cancel, the committed square untouched",
        json!({"section":idle_section(cancelled, expected, "2.4")?,"pixels":shows_committed(cancelled, false)?}),
    );
    write_json(&launch.evidence.join("crop-checks.json"), &json!(checks))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scripted_rectangle_is_covered_and_lands_on_whole_box_pixels() {
        let payload = off_centre().expect("a valid payload");
        let stage = stage(ANGLE);
        let rect = payload.output_rect(&stage).expect("a covered rectangle");
        assert!(rect.width > 100 && rect.height > 100, "{rect:?}");
        // Off-centre in both axes, so the frame cannot be confused with a centred fit.
        let (box_width, box_height) = stage.bounding_box();
        let centre = (
            rect.x as f64 + f64::from(rect.width) / 2.0,
            rect.y as f64 + f64::from(rect.height) / 2.0,
        );
        assert!((centre.0 - box_width / 2.0).abs() > 4.0, "{centre:?}");
        assert!((centre.1 - box_height / 2.0).abs() > 4.0, "{centre:?}");
    }

    /// The runner's own reading of a committed output: Original first, either orientation, and
    /// Free for a rectangle no declared ratio fits.
    #[test]
    fn a_committed_output_reads_as_the_first_ratio_that_fits_it() {
        assert_eq!(reads_as(STAGE, [480, 270]), "16:9");
        assert_eq!(reads_as(STAGE, [270, 480]), "16:9");
        assert_eq!(reads_as(STAGE, [300, 200]), "Original");
        assert_eq!(reads_as((480, 360), [300, 200]), "3:2");
        assert_eq!(reads_as(STAGE, [201, 200]), "1:1");
        assert_eq!(reads_as(STAGE, [258, 169]), "Free");
    }

    #[test]
    fn every_crop_plan_captures_one_frame_for_the_open_and_one_per_step() {
        for (scenario, plan) in [("crop", plan(&[])), ("crop-draft", draft_plan(&[]))] {
            assert!(plan.validate().is_ok(), "{scenario}: {:?}", plan.validate());
            let (open, steps) = plan.steps().split_first().expect("a frame");
            assert!(open.script().is_none(), "{scenario}");
            assert!(
                steps.iter().all(|step| step.script().is_some()),
                "{scenario}"
            );
            assert_eq!(
                plan.script().as_array().expect("an array").len() + 1,
                plan.len(),
                "{scenario}"
            );
        }
    }

    /// A launch of `plan` written the way the editor writes one, every scripted step recorded as
    /// sent except `failed`, and no state beyond what every frame's identity needs.
    fn recorded(dir: &Path, plan: &Plan, failed: Option<&str>) {
        let mut frames = Vec::new();
        let mut script = Vec::new();
        let mut events = vec![json!({"event":"startup","run_id":"r"})];
        let mut number = 0;
        for (index, step) in plan.steps().iter().enumerate() {
            let file = format!("frame-{}.png", index + 1);
            image::RgbImage::new(4, 4).save(dir.join(&file)).unwrap();
            let recorded = match step.script() {
                None => Value::Null,
                Some(request) => {
                    number += 1;
                    let status = if Some(step.name()) == failed {
                        "failed"
                    } else {
                        "sent"
                    };
                    json!({"step":number,"status":status,"request":request})
                }
            };
            if !recorded.is_null() {
                let mut listed = recorded.clone();
                listed["frame"] = json!(file);
                script.push(listed);
                events.push(json!({"event":"script_step","run_id":"r","detail":recorded}));
            }
            let frame = json!({
                "file": file,
                "capture_provenance": "window-renderer-readback",
                "step": recorded,
                "state": {"run_id":"r","backend":{"backend":"metal","adapter":"test"}},
            });
            write_json(&dir.join(format!("state-{}.json", index + 1)), &frame).unwrap();
            frames.push(frame);
        }
        events.push(json!({"event":"shutdown","run_id":"r"}));
        fs::write(
            dir.join("events.jsonl"),
            events
                .iter()
                .map(|event| format!("{event}\n"))
                .collect::<String>(),
        )
        .unwrap();
        write_json(
            &dir.join("result.json"),
            &json!({"status":"captured","run_id":"r","had_input_errors":false,"frames":frames,"script":script}),
        )
        .unwrap();
    }

    #[test]
    fn a_missing_overlay_and_a_stale_step_are_failures() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("flat.png");
        // The stage at full opacity everywhere: the right size, but no frame, handles or dimming.
        image::RgbImage::from_pixel(480, 320, image::Rgb(COLORS[0]))
            .save(&path)
            .unwrap();
        let frame = json!({"state":{"crop":{"drafting":true,"input_stage_loaded":true,"paused":false,"conflicted":false,"angle":0.0,"input_stage":[480,320],"rect":[0.0,0.0,480.0,320.0]},"stack":{"revision":1}},"surface_columns":[0,480]});
        let frame = Frame::unchecked(frame, &path);
        assert!(
            shows_draft(&frame)
                .unwrap_err()
                .to_string()
                .contains("overlay handle"),
            "a frame without an overlay must fail: {:?}",
            shows_draft(&frame)
        );

        // Every step sent: the run gets past every step's record and fails only on the first
        // expectation, which this bare state cannot meet.
        let sent = tempfile::tempdir().unwrap();
        recorded(sent.path(), &plan(&[]), None);
        let error = plan(&[]).check(sent.path()).unwrap_err().to_string();
        assert!(error.starts_with("Step \"fit\" (frame 1"), "{error}");
        assert!(error.contains("no revision"), "{error}");
        // One step the editor did not send fails on that step, before any expectation.
        let stale = tempfile::tempdir().unwrap();
        recorded(stale.path(), &plan(&[]), Some("angled"));
        let error = plan(&[]).check(stale.path()).unwrap_err().to_string();
        assert!(error.starts_with("Step \"angled\" (frame 4"), "{error}");
        assert!(error.contains("was not sent"), "{error}");
    }
}
