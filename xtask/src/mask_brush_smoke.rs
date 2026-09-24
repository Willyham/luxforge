//! The `mask-brush` smoke scenario: phase C's claim, which is that several brushes in one mask are
//! an ordinary list and that a brush can take a region out of a gradient.
//!
//! Three launches over one catalog. The first paints two add strokes on one component, reads the
//! coverage off the overlay, paints a third with the feather at the other end of its range and
//! measures the difference in the pixels, erases across the second stroke, and deletes one stroke on
//! its own as the forward edit it is. The second reopens that catalog, reads the same nine points
//! back, sweeps a radial gradient into the same mask, paints a **subtract** brush inside it, drags a
//! Presence adjustment through the result and undoes it. The third reopens it again and carries on
//! painting: over the picture's own edge, at 100%, and under a rotated crop, where the stroke is
//! read back through the affine the gesture itself maps pointer positions with.
//!
//! **Why the overlay is what is measured.** `mask-on-black` paints the composed coverage as an
//! opaque greyscale at the display's own resolution, so a point at coverage 1 reads white, one at 0
//! reads black, and a point halfway up a feather band reads halfway between them. That is what makes
//! two feather settings legible in the capture itself rather than only in the recorded payload: the
//! same offset from the centre line of a hard stroke and of a fully feathered one is full coverage in
//! one frame and partial coverage in the other.
//!
//! The geometry is stated in **mask-space units**, where one unit is the stage's height on both axes
//! (`docs/design/mask-study.md`), because that is the space a brush's size and a radial's radii live
//! in. A position is a normalized content coordinate, which is what a script paints in and what the
//! stored stroke holds.
use crate::{
    scenario::{Bright, Frame, Launch, Run, Scan, pixels, preamble},
    *,
};

pub const SCENARIO: &str = "mask-brush";
/// The Presence fixture: its bottom-right quadrant is a flat mid-grey, which is where the two
/// patches that measure the masked Presence adjustment sit. `cargo xtask generate-fixtures`.
const FIXTURE: &str = "fixtures/generated/presence.jpg";
const WINDOW: [&str; 2] = workspace_smoke::WINDOW;
/// The fixture's aspect, `W/H`. 1440 × 960.
const ASPECT: f64 = 1.5;

const RADIAL: &str = "radial";
const PRESENCE: &str = "set-presence";
const DEHAZE: &str = "dehaze";
/// What the masked Presence gesture commits. Dehaze is the one Presence unit that moves a flat field
/// at all, which is why `mask-combine` measures the same thing with it, and it is deliberately short
/// of `+100`: a reading at the end of the range would be produced by a broken render as readily as
/// by a correct one.
const DEHAZED: f64 = 30.0;

/// The brush's radius for every stroke here, in mask-space units, and the two feather settings the
/// scenario compares. `0` is the study's explicit hard-edge case and `100` is a band exactly one
/// radius wide, so at half a radius off the centre line the first is exactly 1 and the second is
/// `smooth(0.5)`, which is exactly one half.
const SIZE: f64 = 0.09;
const HARD: f64 = 0.0;
const SOFT: f64 = 100.0;
/// Half a radius, as a normalized `y` offset: one mask-space unit is the stage's height, so a
/// mask-space distance and a normalized `y` are the same number.
const HALF_RADIUS: f64 = SIZE / 2.0;

/// The two add strokes of the first component, the feathered third, and the erase stroke that
/// crosses the second. Each is a horizontal segment except the erase, which is vertical, so the
/// nearest segment to every probe below is a line and not an end cap.
const A: [[f64; 2]; 2] = [[0.10, 0.18], [0.30, 0.18]];
const B: [[f64; 2]; 2] = [[0.10, 0.42], [0.30, 0.42]];
const C: [[f64; 2]; 2] = [[0.60, 0.18], [0.80, 0.18]];
const ERASE: [[f64; 2]; 2] = [[0.25, 0.36], [0.25, 0.48]];
/// The subtract brush's stroke, inside the radial.
const SUBTRACT: [[f64; 2]; 2] = [[0.64, 0.80], [0.80, 0.80]];
/// A stroke that starts outside the picture and paints in over its left edge. Stored positions run
/// from -1 to 2 by design, so this is an ordinary stroke and not an edge case to be clamped.
const EDGE: [[f64; 2]; 2] = [[-0.03, 0.70], [0.06, 0.70]];
/// The stroke painted at 100%, and the one painted under the rotated crop. Both are placed where
/// nothing else has painted, so what they cover is theirs alone.
const ZOOMED: [[f64; 2]; 2] = [[0.36, 0.30], [0.46, 0.30]];
const ROTATED: [[f64; 2]; 2] = [[0.42, 0.55], [0.55, 0.55]];

/// The radial gradient: its centre and the radius the sweep draws it out to, in mask-space units. A
/// sweep from a centre to a point puts `radius_x = |Δx| · W/H` and `radius_y = |Δy|` on the draft, so
/// sweeping to `(x + r · H/W, y + r)` makes a circle.
///
/// It is drawn large deliberately. A swept radial takes the panel's default feather, which puts its
/// full coverage inside half its radius and a ramp over the rest, and both readings below have to
/// sit in that core while staying more than one brush radius apart — which a small circle cannot
/// offer. Its centre is in the fixture's flat grey quadrant, where the masked Presence drag is
/// measured.
const RADIAL_AT: [f64; 2] = [0.72, 0.70];
const RADIAL_R: f64 = 0.30;

const fn radial_to() -> [f64; 2] {
    [RADIAL_AT[0] + RADIAL_R / ASPECT, RADIAL_AT[1] + RADIAL_R]
}

/// The points every coverage frame is read at, as fractions of the photograph's own drawn rectangle,
/// named for what the mask does to them.
const P_A: [f64; 2] = [0.20, A[0][1]];
const P_A_BAND: [f64; 2] = [0.20, A[0][1] + HALF_RADIUS];
const P_B: [f64; 2] = [0.13, B[0][1]];
const P_C: [f64; 2] = [0.70, C[0][1]];
const P_C_BAND: [f64; 2] = [0.70, C[0][1] + HALF_RADIUS];
const P_ERASED: [f64; 2] = [ERASE[0][0], B[0][1]];
const P_RADIAL: [f64; 2] = [0.72, 0.58];
const P_SUBTRACTED: [f64; 2] = [0.72, SUBTRACT[0][1]];
const P_OUT: [f64; 2] = [0.93, 0.93];
const P_EDGE: [f64; 2] = [0.012, EDGE[0][1]];
const P_ZOOMED: [f64; 2] = [0.41, ZOOMED[0][1]];
const P_ROTATED: [f64; 2] = [0.48, ROTATED[0][1]];

const PROBES: [[f64; 2]; 9] = [
    P_A,
    P_A_BAND,
    P_B,
    P_C,
    P_C_BAND,
    P_ERASED,
    P_RADIAL,
    P_SUBTRACTED,
    P_OUT,
];
const PROBE_NAMES: [&str; 9] = [
    "a",
    "a-band",
    "b",
    "c",
    "c-band",
    "erased",
    "radial",
    "subtracted",
    "out",
];

/// Half the side of a measured patch, in capture pixels. Every probe above is at least two patches
/// clear of the nearest coverage boundary, except the two band readings, which are the measurement.
const PATCH_HALF: i64 = 5;
/// How bright a `mask-on-black` patch must read before this scenario calls it covered, and how dark
/// before it calls it uncovered. The overlay paints coverage straight into all three channels, so
/// full coverage is 255 and none is 0; these are margins, not tuned thresholds.
const COVERED: f64 = 200.0;
const UNCOVERED: f64 = 40.0;
/// What a reading inside a feather band must stay between to count as partial: clear of both
/// endpoints, by the same margins. A hard stroke read at the same offset is `COVERED`, which is the
/// comparison the two settings are proved by.
const PARTIAL_LOW: f64 = 60.0;
const PARTIAL_HIGH: f64 = 195.0;
/// How far the flat quadrant's mean must move before this scenario calls the masked Presence
/// adjustment visible, and how close it must stay before it calls a patch untouched. Both are read
/// from renderer readback with no JPEG between, so the untouched bound is tight on purpose.
const PRESENCE_MOVED: f64 = 4.0;
const PRESENCE_UNTOUCHED: f64 = 1.0;

/// The open frame plus one per script step. Three launches rather than one because every masked
/// frame on this fixture is a render of its own and a launch is bounded by the smoke command's own
/// deadline — and a scenario that takes three processes to stay inside it proves the catalog twice
/// over rather than once.
const LAUNCH1_FRAMES: usize = 14;
const LAUNCH2_FRAMES: usize = 12;
const LAUNCH3_FRAMES: usize = 16;

/// Launch 1: one brush component, painted, feathered at both ends of its range, erased across and
/// one stroke deleted from the middle of it.
fn launch1_script() -> Value {
    json!([
        // 1: Mask mode, through the same `workspace.set` the mode strip sends.
        {"workspace":{"mode":"mask"}},
        // 2: the brush the first strokes are drawn with: one size, and the hard edge.
        {"mask":{"brush":{"size":SIZE,"feather":HARD,"flow":100.0}}},
        // 3: a new mask whose first component is an add brush. A brush is reached from its own
        // section and not from the Add row, because it declares no geometry to type.
        {"mask":{"paint":"new-mask"}},
        // 4: the first stroke: one mask, one component and one stroke, in one history entry.
        {"mask":{"stroke":{"points":A,"release":true}}},
        // 5: the second, on the same component, with no second gesture: the brush re-arms itself.
        {"mask":{"stroke":{"points":B,"release":true}}},
        // 6: the coverage itself on screen, which is what every reading below is taken from.
        {"workspace":{"mask_overlay":"mask-on-black"}},
        // 7-8: the same brush at the other end of its feather range, and a third stroke with it.
        {"mask":{"brush":{"feather":SOFT}}},
        {"mask":{"stroke":{"points":C,"release":true}}},
        // 9-10: an erase stroke across the second one. The erase flag is the brush's, held for the
        // stroke's whole life.
        {"mask":{"brush":{"feather":HARD,"erase":true}}},
        {"mask":{"stroke":{"points":ERASE,"release":true}}},
        // 11: back to adding, so nothing later inherits the erase.
        {"mask":{"brush":{"erase":false}}},
        // 12: the component selected, which is what lists its strokes on the row.
        {"mask":{"select_component":0}},
        // 13: one stroke deleted on its own — the feathered one — as a forward edit: one entry
        // appended, every other stroke exactly where it was.
        {"mask":{"row":{"component":0,"delete_stroke":2}}}
    ])
}

/// Launch 2, over the same catalog: a radial gradient beside the painted brush, a second brush
/// subtracting from it, an adjustment through the result, and an undo.
fn launch2_script() -> Value {
    json!([
        // 1-2: Mask mode and the coverage on screen, in a new process: what the first launch
        // painted is read back from the pixels before anything is added to it.
        {"workspace":{"mode":"mask"}},
        {"workspace":{"mask_overlay":"mask-on-black"}},
        // 3-5: a radial gradient in the same mask, swept from its centre out to one radius. The Add
        // row is at Add, which is where a reopened panel starts.
        {"mask":{"add":RADIAL}},
        {"mask":{"sweep":{"from":RADIAL_AT,"to":radial_to()}}},
        {"mask":{"apply":true}},
        // 6-8: a second brush component, in subtract mode, painted inside that gradient. This is
        // the owner's own requirement, and it is one more row in the same list.
        {"mask":{"mode":"subtract"}},
        {"mask":{"paint":"new-brush"}},
        {"mask":{"stroke":{"points":SUBTRACT,"release":true}}},
        // 9: the overlay off, leaving the photograph.
        {"workspace":{"mask_overlay":"off"}},
        // 10: Presence through the finished mask, as the panel's own drag. The brush is still armed
        // from the stroke above and gives its draft up to this gesture, exactly as it gives it up to
        // every other one.
        {"slider":{"action":PRESENCE,"parameter":DEHAZE,"values":[12.0,DEHAZED],"release":true}},
        // 11: and undone.
        {"api":{"method":"history.undo","params":{}}}
    ])
}

/// Launch 3, over the same catalog again: painting carried on over the picture's own edge, at 100%,
/// and under a rotated crop.
fn launch3_script() -> Value {
    json!([
        // 1-2: Mask mode and the coverage, in a third process.
        {"workspace":{"mode":"mask"}},
        {"workspace":{"mask_overlay":"mask-on-black"}},
        // 3-4: the brush in hand again — a reopened editor holds no gesture — armed on the
        // component the first launch painted, by the name the panel gives it.
        {"mask":{"brush":{"size":SIZE,"feather":HARD,"flow":100.0}}},
        {"mask":{"paint":{"component":{"name":"Brush 1"}}}},
        // 5: a stroke that starts off the picture and paints in over its left edge.
        {"mask":{"stroke":{"points":EDGE,"release":true}}},
        // 6-8: painting at 100%, where the exact frame is what is on screen, and back to Fit, where
        // that stroke is where the content coordinates it was painted in put it.
        {"view":{"zoom":"100"}},
        {"mask":{"stroke":{"points":ZOOMED,"release":true}}},
        {"view":{"zoom":"fit"}},
        // 9: the overlay off, so the cropped photograph below can be found in the capture at all.
        {"workspace":{"mask_overlay":"off"}},
        // 10: the geometry tail as one affine, before the crop: the map a gesture places a stroke
        // with, read through the same method the canvas reads it through.
        {"api":{"method":"render.transform","params":{}}},
        // 11: a straightened, fitted crop: the picture is now rotated under the mask.
        {"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":8.0}}},
        // 12: the affine again, which is what the probe below is placed by.
        {"api":{"method":"render.transform","params":{}}},
        // 13-15: the coverage back on, the brush re-armed on that component after the crop, and one
        // more stroke painted in content coordinates under the rotated picture.
        {"workspace":{"mask_overlay":"mask-on-black"}},
        {"mask":{"paint":{"component":{"name":"Brush 1"}}}},
        {"mask":{"stroke":{"points":ROTATED,"release":true}}}
    ])
}

/// The content addresses of one component's strokes, in the order they compose. The panel lists them
/// on the selected row, so a frame that records them is a frame whose row was open.
fn strokes(frame: &Frame, index: usize) -> Result<Vec<String>> {
    Ok(frame.component(index)?["strokes"]
        .as_array()
        .ok_or("The component records no stroke list")?
        .iter()
        .map(|stroke| stroke["stroke"].as_str().unwrap_or_default().to_owned())
        .collect())
}

/// The brush the panel is holding, as the frame recorded it: the settings the next stroke is drawn
/// with, read from the fields `mask.add-stroke` itself declares.
fn brush(frame: &Frame, field: &str) -> Result<f64> {
    let held = &frame["state"]["masks"]["brush"]["fields"][field];
    held.as_f64()
        .or_else(|| held.as_str().and_then(|text| text.parse::<f64>().ok()))
        .ok_or_else(|| {
            format!(
                "Frame records no brush {field}: {}",
                frame["state"]["masks"]["brush"]
            )
            .into()
        })
}

/// The stack's one Presence layer, or `None` when the stack holds none.
fn presence_layer(frame: &Frame) -> Option<&Value> {
    frame.layer(lightwell_core::PRESENCE_EFFECT)
}

/// Where the photograph is drawn inside the capture.
///
/// It is found on a frame the overlay has not painted — the opened fixture, or the frame after a
/// crop — and reused for the frames beside it: the zoom and the panels do not move between them, and
/// it cannot be found again from a frame painted mostly black. The vertical extent is taken first,
/// for the reason [`Scan::Tallest`] records.
const BOUNDS: Bright = Bright {
    threshold: 32,
    scan: Scan::Tallest { last_row: false },
    least: Some((200, 100)),
};

/// Mean Rec. 709 luminance of one small patch of the displayed photograph.
fn patch(frame: &Frame, bounds: [u32; 4], at: [f64; 2]) -> Result<f64> {
    pixels::luminance_at(frame, bounds, at, PATCH_HALF)
}

/// All nine probes of one capture.
fn probes(frame: &Frame, bounds: [u32; 4]) -> Result<[f64; 9]> {
    let mut out = [0.0; 9];
    for (slot, at) in out.iter_mut().zip(PROBES) {
        *slot = patch(frame, bounds, at)?;
    }
    Ok(out)
}

/// What one probe must read: full coverage, none, or somewhere strictly between the two.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reads {
    Full,
    None,
    Partial,
}

fn reading_holds(value: f64, wanted: Reads) -> bool {
    match wanted {
        Reads::Full => value >= COVERED,
        Reads::None => value <= UNCOVERED,
        Reads::Partial => (PARTIAL_LOW..=PARTIAL_HIGH).contains(&value),
    }
}

fn describe(wanted: Reads) -> &'static str {
    match wanted {
        Reads::Full => "full coverage",
        Reads::None => "no coverage",
        Reads::Partial => "partial coverage",
    }
}

/// One coverage frame against what the composition and the accumulation rules say it must be, at
/// each of the nine probes in turn.
fn coverage(frame: &Frame, bounds: [u32; 4], what: &str, expected: [Reads; 9]) -> Result<[f64; 9]> {
    let read = probes(frame, bounds)?;
    for ((value, want), name) in read.iter().zip(expected).zip(PROBE_NAMES) {
        ensure(
            reading_holds(*value, want),
            format!(
                "{what}: {name} read {value:.1}, which is not {} (covered is >= {COVERED}, \
                 uncovered <= {UNCOVERED}, partial {PARTIAL_LOW}..{PARTIAL_HIGH})",
                describe(want)
            ),
        )?;
    }
    Ok(read)
}

fn untouched(what: &str, after: f64, before: f64) -> Result {
    ensure(
        (after - before).abs() <= PRESENCE_UNTOUCHED,
        format!("{what}: {after:.2} moved from {before:.2} by more than {PRESENCE_UNTOUCHED}"),
    )
}

fn moved(what: &str, after: f64, before: f64) -> Result {
    ensure(
        (after - before).abs() >= PRESENCE_MOVED,
        format!("{what}: {after:.2} did not move from {before:.2} by {PRESENCE_MOVED}"),
    )
}

/// One `render.transform` answer, as the step recorded it: the geometry tail as one affine, which is
/// what maps a content position onto the frame a person sees.
struct Tail {
    content: (f64, f64),
    output: (f64, f64),
    forward: [f64; 6],
}

impl Tail {
    fn read(step: &Value) -> Result<Self> {
        let answer = &step["result"];
        let number = |value: &Value| -> Result<f64> {
            value
                .as_f64()
                .ok_or_else(|| format!("render.transform answered {answer}").into())
        };
        let forward = answer["forward"]
            .as_array()
            .ok_or("render.transform answered no forward affine")?;
        ensure(forward.len() == 6, "An affine has six coefficients")?;
        let mut coefficients = [0.0; 6];
        for (slot, value) in coefficients.iter_mut().zip(forward) {
            *slot = number(value)?;
        }
        Ok(Self {
            content: (
                number(&answer["content"]["width"])?,
                number(&answer["content"]["height"])?,
            ),
            output: (
                number(&answer["output"]["width"])?,
                number(&answer["output"]["height"])?,
            ),
            forward: coefficients,
        })
    }

    /// Where a normalized content position lands in the frame, as a fraction of the drawn
    /// photograph: the same map the canvas puts the brush cursor through.
    fn place(&self, at: [f64; 2]) -> [f64; 2] {
        let (x, y) = (at[0] * self.content.0, at[1] * self.content.1);
        let f = self.forward;
        [
            (f[0] * x + f[1] * y + f[2]) / self.output.0,
            (f[3] * x + f[4] * y + f[5]) / self.output.1,
        ]
    }
}

/// The whole scenario: three launches over one catalog, checked together.
pub fn run(mut run: Run) -> Result {
    let fixture = run.root().join(FIXTURE);
    run.note(
        "Generate the fixture first with `cargo xtask generate-fixtures --output fixtures/generated`.\n\nThree launches over one catalog: the first paints several strokes on one brush, feathers one at the other end of the range, erases across another and deletes one on its own; the second reopens it, subtracts a second brush from a radial gradient and drags Presence through the result; the third reopens it again and paints over the picture's edge, at 100% and under a rotated crop.",
    );
    run.check(|run| {
        ensure(
            fixture.is_file(),
            format!("{FIXTURE} is missing; run `cargo xtask generate-fixtures`"),
        )?;
        run.hash(std::slice::from_ref(&fixture))?;
        let launch1 = run.launch(
            Launch::named("launch1")
                .open(&fixture)
                .script("script1.json", launch1_script())
                .window(WINDOW),
        )?;
        let (app1, _) = preamble(&launch1, LAUNCH1_FRAMES)?;
        let checks = verify_launch1(&launch1, &app1)?;
        run.record("launch1", checks.clone());

        // Launch 2: the same catalog and the same file, in a new process.
        let catalog = launch1.join("catalog.sqlite");
        ensure(catalog.is_file(), "Launch 1 wrote no catalog")?;
        let launch2 = run.launch(
            Launch::named("launch2")
                .catalog(&catalog)
                .open(&fixture)
                .script("script2.json", launch2_script())
                .window(WINDOW),
        )?;
        let (app2, _) = preamble(&launch2, LAUNCH2_FRAMES)?;
        let composed = verify_launch2(&launch2, &app2, &checks)?;
        run.record("launch2", composed.clone());

        // Launch 3: the same catalog once more — the one launch 1 wrote, which launch 2 opened by
        // path and added to — and painting carried on in it.
        let catalog = launch1.join("catalog.sqlite");
        ensure(catalog.is_file(), "Launch 1's catalog is gone")?;
        let launch3 = run.launch(
            Launch::named("launch3")
                .catalog(&catalog)
                .open(&fixture)
                .script("script3.json", launch3_script())
                .window(WINDOW),
        )?;
        let (app3, _) = preamble(&launch3, LAUNCH3_FRAMES)?;
        let carried = verify_launch3(&launch3, &app3, &composed)?;
        run.record("launch3", carried.clone());
        run.sources_unchanged()?;
        write_json(
            &run.out().join("mask-brush-checks.json"),
            &json!({"launch1": checks, "launch2": composed, "launch3": carried}),
        )?;
        Ok(())
    })
}

/// Launch 1, frame by frame: the strokes, the feather, the erase, the delete, the gradient the
/// second brush subtracts from, the masked adjustment and the undo.
/// Launch 1, frame by frame: the strokes, the two feather settings, the erase and the delete.
fn verify_launch1(evidence: &Path, app: &Value) -> Result<Value> {
    ensure(
        app["had_input_errors"] == json!(false),
        format!("The run recorded an input error: {}", app["script"]),
    )?;
    let frames = Frame::all(evidence, app)?;
    ensure(
        frames.len() == LAUNCH1_FRAMES,
        format!("Launch 1 wrote {} frames", frames.len()),
    )?;
    let bounds = pixels::bright_bounds(&frames[0], BOUNDS)?;
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // Frame 0: the fixture as launched, with no mask in the recipe.
    ensure(
        frames[0].masks()?.is_empty(),
        "The fixture opened with a mask already in the recipe",
    )?;
    let opened = probes(&frames[0], bounds)?;
    record(
        &frames[0],
        "the fixture as launched, with no mask in the recipe",
        json!({"patches":opened,"bounds":bounds}),
    );

    // Frame 4: the first stroke. One mask, one brush component, one stroke, one entry.
    ensure(
        frames[4].revision()? == frames[0].revision()? + 1,
        format!(
            "The first stroke moved the revision to {}",
            frames[4].revision()?
        ),
    )?;
    ensure(
        frames[4].label()? == "Add brush",
        format!("The first stroke is labelled {:?}", frames[4].label()?),
    )?;
    ensure(
        frames[4].kinds()? == ["add brush"],
        format!("The first stroke made {:?}", frames[4].kinds()?),
    )?;
    let mask = frames[4].only_mask()?["id"]
        .as_str()
        .ok_or("The listed mask has no identity")?
        .to_owned();
    record(
        &frames[4],
        "one painted stroke: a mask, a brush component and the stroke, in one history entry",
        json!({"label":frames[4].label()?,"kinds":frames[4].kinds()?,
               "brush":{"size":brush(&frames[4],"size")?,"feather":brush(&frames[4],"feather")?}}),
    );

    // Frame 5: the second stroke, on the same component and with no second gesture. Several strokes
    // in one mask are an ordinary list: two strokes, one row, two entries.
    ensure(
        frames[5].revision()? == frames[4].revision()? + 1,
        "The second stroke did not commit one entry",
    )?;
    ensure(
        frames[5].label()? == "Update Brush 1",
        format!("The second stroke is labelled {:?}", frames[5].label()?),
    )?;
    ensure(
        frames[5].components()?.len() == 1,
        format!("The second stroke made {:?}", frames[5].kinds()?),
    )?;
    record(
        &frames[5],
        "a second stroke on the same brush: one more entry, still one component",
        json!({"label":frames[5].label()?,"components":frames[5].components()?.len()}),
    );

    // Frame 6: the coverage itself. Both strokes are covered, the band beside the hard one is
    // covered too — a hard edge is coverage 1 right up to the radius — and nothing else is.
    let two_strokes = coverage(
        &frames[6],
        bounds,
        "two hard add strokes",
        [
            Reads::Full,
            Reads::Full,
            Reads::Full,
            Reads::None,
            Reads::None,
            Reads::Full,
            Reads::None,
            Reads::None,
            Reads::None,
        ],
    )?;
    record(
        &frames[6],
        "the coverage of two add strokes on one brush component",
        json!({"patches":two_strokes}),
    );

    // Frame 7: the brush at the other feather, before it has painted anything. The panel's own field
    // says so and nothing is committed.
    ensure(
        frames[7].revision()? == frames[5].revision()?,
        "Changing the brush committed something",
    )?;
    ensure(
        (brush(&frames[7], "feather")? - SOFT).abs() < 0.5,
        format!("The brush holds feather {}", brush(&frames[7], "feather")?),
    )?;
    record(
        &frames[7],
        "the brush set to its widest feather, with nothing painted yet",
        json!({"feather":brush(&frames[7],"feather")?,"size":brush(&frames[7],"size")?}),
    );

    // Frame 8: the feathered stroke. Its centre line is full coverage and its band at half a radius
    // is strictly between the endpoints, where the hard strokes' band at the same offset is full.
    // That is the two feather settings, measured in the photograph rather than read off a payload.
    let feathered = coverage(
        &frames[8],
        bounds,
        "a fully feathered third stroke",
        [
            Reads::Full,
            Reads::Full,
            Reads::Full,
            Reads::Full,
            Reads::Partial,
            Reads::Full,
            Reads::None,
            Reads::None,
            Reads::None,
        ],
    )?;
    ensure(
        feathered[1] - feathered[4] > 50.0,
        format!(
            "The two feather settings read {:.1} and {:.1} at the same offset: not a difference",
            feathered[1], feathered[4]
        ),
    )?;
    record(
        &frames[8],
        "a third stroke at full feather: its band reads partial coverage where the hard strokes' \
         band at the same offset reads full",
        json!({"hard_band":feathered[1],"soft_band":feathered[4],"patches":feathered}),
    );

    // Frame 10: the erase stroke. It takes coverage out of the second stroke where it crosses it and
    // leaves the rest of that stroke exactly as it was.
    ensure(
        frames[10].revision()? == frames[8].revision()? + 1,
        "The erase stroke did not commit one entry",
    )?;
    let erased = coverage(
        &frames[10],
        bounds,
        "an erase stroke across the second add stroke",
        [
            Reads::Full,
            Reads::Full,
            Reads::Full,
            Reads::Full,
            Reads::Partial,
            Reads::None,
            Reads::None,
            Reads::None,
            Reads::None,
        ],
    )?;
    record(
        &frames[10],
        "an erase stroke: coverage removed where it crosses, the rest of that stroke untouched",
        json!({"label":frames[10].label()?,"patches":erased}),
    );

    // Frame 12: the row selected, which is what lists its strokes. Four strokes, in the order they
    // compose, and selecting a row commits nothing.
    ensure(
        frames[12].revision()? == frames[10].revision()?,
        "Selecting a component committed something",
    )?;
    let listed = strokes(&frames[12], 0)?;
    ensure(
        listed.len() == 4,
        format!("The brush row lists {} strokes", listed.len()),
    )?;
    record(
        &frames[12],
        "the brush row with its four strokes listed, each with a delete of its own",
        json!({"strokes":listed}),
    );

    // Frame 13: one stroke deleted on its own. A forward edit: one entry appended, the feathered
    // stroke gone from the picture, every other stroke exactly where it was — including the erase,
    // which is what proves the order survived a removal from the middle of it.
    ensure(
        frames[13].revision()? == frames[12].revision()? + 1,
        "Deleting a stroke did not commit one entry",
    )?;
    ensure(
        frames[13].label()? == "Delete a stroke from Brush 1",
        format!("The delete is labelled {:?}", frames[13].label()?),
    )?;
    let kept = strokes(&frames[13], 0)?;
    ensure(
        kept == [listed[0].clone(), listed[1].clone(), listed[3].clone()],
        format!("The delete left {kept:?} of {listed:?}"),
    )?;
    let deleted = coverage(
        &frames[13],
        bounds,
        "the feathered stroke deleted on its own",
        [
            Reads::Full,
            Reads::Full,
            Reads::Full,
            Reads::None,
            Reads::None,
            Reads::None,
            Reads::None,
            Reads::None,
            Reads::None,
        ],
    )?;
    record(
        &frames[13],
        "one stroke deleted on its own: the others keep their order and their coverage",
        json!({"label":frames[13].label()?,"strokes":kept,"patches":deleted}),
    );

    Ok(json!({
        "mask": mask,
        "kinds": frames[13].kinds()?,
        "strokes": kept,
        "opened": opened,
        "revision": frames[13].revision()?,
        "feather": {"hard_band": feathered[1], "soft_band": feathered[4]},
        "covered_threshold": COVERED,
        "uncovered_threshold": UNCOVERED,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of nine patches of the displayed photograph, read back from the renderer; the coverage readings are of the mask-on-black overlay, which paints coverage straight into all three channels, and are not a colorimetric claim",
    }))
}

/// Launch 2: the reopened catalog, a radial gradient beside the brush, a second brush subtracting
/// from it, the masked adjustment and the undo.
fn verify_launch2(evidence: &Path, app: &Value, launch1: &Value) -> Result<Value> {
    ensure(
        app["had_input_errors"] == json!(false),
        format!("The run recorded an input error: {}", app["script"]),
    )?;
    let frames = Frame::all(evidence, app)?;
    ensure(
        frames.len() == LAUNCH2_FRAMES,
        format!("Launch 2 wrote {} frames", frames.len()),
    )?;
    let bounds = pixels::bright_bounds(&frames[0], BOUNDS)?;
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // Frame 0: the reopened catalog. The mask, its component and its strokes are back, by the same
    // identities the first launch committed, with no gesture open and no Presence layer.
    ensure(
        frames[0].only_mask()?["id"] == launch1["mask"],
        format!(
            "The reopened catalog holds {}",
            frames[0].only_mask()?["id"]
        ),
    )?;
    ensure(
        frames[0]["state"]["mask_draft"] == Value::Null && presence_layer(&frames[0]).is_none(),
        "A reopened editor holds an open gesture or a Presence layer",
    )?;
    record(
        &frames[0],
        "the catalog reopened in a new process, with the painted mask in the recipe",
        json!({"mask":frames[0].only_mask()?["id"]}),
    );

    // Frame 2: the coverage after the reopen, read at the same nine points the first launch ended
    // on. The strokes survived as pixels and not only as rows.
    let reopened = coverage(
        &frames[2],
        bounds,
        "the reopened mask",
        [
            Reads::Full,
            Reads::Full,
            Reads::Full,
            Reads::None,
            Reads::None,
            Reads::None,
            Reads::None,
            Reads::None,
            Reads::None,
        ],
    )?;
    record(
        &frames[2],
        "the reopened mask's own coverage: every stroke back where it was painted",
        json!({"patches":reopened,"kinds":frames[2].kinds()?}),
    );

    // Frame 5: the radial gradient, committed into the same mask as a second component.
    ensure(
        frames[5].revision()? == frames[2].revision()? + 1,
        "The radial did not commit one entry",
    )?;
    ensure(
        frames[5].kinds()? == ["add brush", "add radial"],
        format!("The mask holds {:?}", frames[5].kinds()?),
    )?;
    let with_radial = coverage(
        &frames[5],
        bounds,
        "a radial gradient beside the brush",
        [
            Reads::Full,
            Reads::Full,
            Reads::Full,
            Reads::None,
            Reads::None,
            Reads::None,
            Reads::Full,
            Reads::Full,
            Reads::None,
        ],
    )?;
    record(
        &frames[5],
        "a radial gradient added to the mask the brush drew",
        json!({"kinds":frames[5].kinds()?,"label":frames[5].label()?,"patches":with_radial}),
    );

    // Frame 8: the subtract brush inside that gradient. This is the requirement in one frame: a
    // brush takes a region out of a gradient, and it is one more row rather than a special gesture.
    ensure(
        frames[8].revision()? == frames[5].revision()? + 1,
        "The subtract brush did not commit one entry",
    )?;
    ensure(
        frames[8].kinds()? == ["add brush", "add radial", "subtract brush"],
        format!("The mask holds {:?}", frames[8].kinds()?),
    )?;
    let subtracted = coverage(
        &frames[8],
        bounds,
        "a brush subtracting from the radial",
        [
            Reads::Full,
            Reads::Full,
            Reads::Full,
            Reads::None,
            Reads::None,
            Reads::None,
            Reads::Full,
            Reads::None,
            Reads::None,
        ],
    )?;
    record(
        &frames[8],
        "a second brush, in subtract mode, taking a region out of the gradient",
        json!({"kinds":frames[8].kinds()?,"label":frames[8].label()?,"patches":subtracted}),
    );

    // Frame 9: the overlay off. No layer is bound to the mask yet, so the photograph is
    // byte-unchanged from the one this launch opened: a mask on its own is a selection, not an edit.
    ensure(
        frames[9].only_mask()?["layers"] == json!([]),
        "A mask with no adjustment already has a layer bound to it",
    )?;
    let bare = probes(&frames[9], bounds)?;
    let opened: Vec<f64> = launch1["opened"]
        .as_array()
        .ok_or("Launch 1 recorded no opened patches")?
        .iter()
        .map(|value| value.as_f64().unwrap_or_default())
        .collect();
    for (index, name) in PROBE_NAMES.iter().enumerate() {
        untouched(
            &format!("{name} with the mask drawn and no layer bound to it"),
            bare[index],
            opened[index],
        )?;
    }
    record(
        &frames[9],
        "the overlay off: the painted mask is a selection and nothing else",
        json!({"patches":bare}),
    );

    // Frame 10: Presence through the painted mask. The flat quadrant moves inside the mask and is
    // left exactly as it was outside it and where the subtract brush removed the gradient, and the
    // layer the panel committed names the mask.
    ensure(
        frames[10].revision()? == frames[9].revision()? + 1,
        format!(
            "The masked drag moved the revision to {}",
            frames[10].revision()?
        ),
    )?;
    let layer =
        presence_layer(&frames[10]).ok_or("The masked gesture committed no Presence layer")?;
    ensure(
        layer["mask"] == launch1["mask"],
        format!("The committed Presence layer names {}", layer["mask"]),
    )?;
    ensure(
        layer["payload"][DEHAZE] == json!(DEHAZED),
        format!("The committed layer holds {}", layer["payload"]),
    )?;
    let dehazed = probes(&frames[10], bounds)?;
    moved(
        "the covered patch under masked Presence",
        dehazed[6],
        bare[6],
    )?;
    ensure(
        dehazed[6] > 1.0 && dehazed[6] < 254.0,
        format!(
            "The covered patch read {:.2}, which is against the end of the range: a clipped \
             reading is produced by a broken render as readily as by a correct one",
            dehazed[6]
        ),
    )?;
    untouched(
        "the uncovered patch under masked Presence",
        dehazed[8],
        bare[8],
    )?;
    untouched(
        "the patch the subtract brush removed, under masked Presence",
        dehazed[7],
        bare[7],
    )?;
    record(
        &frames[10],
        "Presence applied through the painted mask: the covered patch moved, the subtracted one and \
         the outside one left alone",
        json!({"layer":layer["id"],DEHAZE:DEHAZED,"label":frames[10].label()?,
               "covered":dehazed[6],"subtracted":dehazed[7],"uncovered":dehazed[8]}),
    );

    // Frame 11: undo. The layer is gone, every component is where it was, and the photograph is back
    // to the one the mask alone left.
    ensure(
        presence_layer(&frames[11]).is_none(),
        "Undo left the masked Presence layer in the stack",
    )?;
    ensure(
        frames[11].kinds()? == frames[8].kinds()?,
        "Undo changed the component list",
    )?;
    let undone = probes(&frames[11], bounds)?;
    for (index, name) in PROBE_NAMES.iter().enumerate() {
        untouched(&format!("{name} after undo"), undone[index], bare[index])?;
    }
    record(
        &frames[11],
        "the undone state: the composed mask, with nothing applied through it",
        json!({"patches":undone,"label":frames[11].label()?}),
    );

    Ok(json!({
        "mask": launch1["mask"],
        "kinds": frames[11].kinds()?,
        "reopened": reopened,
        "presence": {"covered": dehazed[6], "subtracted": dehazed[7], "uncovered": dehazed[8]},
        "revision": frames[11].revision()?,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of nine patches of the displayed photograph, read back from the renderer; the coverage readings are of the mask-on-black overlay and are not a colorimetric claim",
    }))
}

/// Launch 3: painting carried on over the picture's own edge, at 100%, and under a rotated crop.
fn verify_launch3(evidence: &Path, app: &Value, launch2: &Value) -> Result<Value> {
    ensure(
        app["had_input_errors"] == json!(false),
        format!("The run recorded an input error: {}", app["script"]),
    )?;
    let frames = Frame::all(evidence, app)?;
    ensure(
        frames.len() == LAUNCH3_FRAMES,
        format!("Launch 3 wrote {} frames", frames.len()),
    )?;
    let steps = app["script"].as_array().ok_or("Missing steps")?;
    let bounds = pixels::bright_bounds(&frames[0], BOUNDS)?;
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // Frame 1: the same catalog, a third process, the whole composed mask back in the recipe. The
    // component list is the panel's own, so it is read in Mask mode; frame 0 is the photograph as
    // the process opened it, which is also where the drawn rectangle is measured.
    ensure(
        frames[1].kinds()?
            == launch2["kinds"]
                .as_array()
                .ok_or("Launch 2 recorded no kinds")?
                .iter()
                .map(|kind| kind.as_str().unwrap_or_default().to_owned())
                .collect::<Vec<_>>(),
        format!("The reopened mask holds {:?}", frames[1].kinds()?),
    )?;
    record(
        &frames[1],
        "the catalog reopened again, with the composed mask in the recipe",
        json!({"kinds":frames[1].kinds()?}),
    );

    // Frame 5: a stroke that begins outside the picture. It is an ordinary stroke — stored positions
    // run from -1 to 2 — and the coverage reaches the picture's own left edge.
    ensure(
        frames[5].revision()? == frames[2].revision()? + 1,
        "The edge stroke did not commit one entry",
    )?;
    let edge = patch(&frames[5], bounds, P_EDGE)?;
    ensure(
        edge >= COVERED,
        format!("The picture's left edge read {edge:.1} after a stroke painted in over it"),
    )?;
    record(
        &frames[5],
        "a stroke begun outside the picture, painting in over its left edge",
        json!({"label":frames[5].label()?,"edge":edge,"points":EDGE}),
    );

    // Frames 6-7: painting at 100%, where the exact frame is what is on screen and no proxy stands
    // in for it. One entry, and the stroke is stored in the content coordinates it was painted in.
    ensure(
        frames[6]["step"]["request"] == json!({"view":{"zoom":100.0}}),
        format!(
            "Frame 5 did not request 100%: {}",
            frames[6]["step"]["request"]
        ),
    )?;
    ensure(
        frames[7].revision()? == frames[5].revision()? + 1,
        "The stroke painted at 100% did not commit one entry",
    )?;
    let zoomed_strokes = strokes(&frames[7], 0)?;
    record(
        &frames[7],
        "one stroke painted at 100%, on the exact frame rather than a proxy",
        json!({"label":frames[7].label()?,"strokes":zoomed_strokes.len(),
               "zoom":frames[7]["state"]["workspace"]["zoom"]}),
    );

    // Frame 8: back at Fit, where the whole picture is on screen again, the stroke painted at 100%
    // is where the content coordinates it was painted in say it is. A zoom is a view and a stroke is
    // an edit; this is what keeps the two apart.
    let zoomed = patch(&frames[8], bounds, P_ZOOMED)?;
    ensure(
        zoomed >= COVERED,
        format!("The stroke painted at 100% read {zoomed:.1} at Fit"),
    )?;
    record(
        &frames[8],
        "back at Fit: the stroke painted at 100% is where its content coordinates put it",
        json!({"read":zoomed,"at":P_ZOOMED}),
    );

    // Frame 11: the rotated crop, with the overlay off so the cropped photograph can be found in the
    // capture at all. The mask is in content coordinates, so nothing about it moved; what changed is
    // the affine between those coordinates and the frame.
    let before = Tail::read(&steps[9])?;
    let after = Tail::read(&steps[11])?;
    ensure(
        after.output != before.output,
        format!(
            "The crop left the output stage at {:?}, so nothing was straightened",
            after.output
        ),
    )?;
    let cropped_bounds = pixels::bright_bounds(&frames[11], BOUNDS)?;
    record(
        &frames[11],
        "a straightened, fitted crop under the painted mask",
        json!({"before":[before.output.0,before.output.1],
               "after":[after.output.0,after.output.1],"forward":after.forward}),
    );

    // Frame 15: one more stroke under the rotated picture. It is painted in content coordinates and
    // read back where the geometry tail's own affine says those coordinates land — which is the map
    // the canvas draws the brush cursor and the handles through.
    ensure(
        frames[15].revision()? == frames[11].revision()? + 1,
        "The stroke painted under the crop did not commit one entry",
    )?;
    let placed = after.place(P_ROTATED);
    let painted = patch(&frames[15], cropped_bounds, placed)?;
    ensure(
        painted >= COVERED,
        format!(
            "The stroke painted under the rotated crop read {painted:.1} at {placed:?}, where the \
             geometry tail's affine places the content position {P_ROTATED:?}"
        ),
    )?;
    ensure(
        frames[15].kinds()?.len() == frames[1].kinds()?.len(),
        format!("Painting under the crop made {:?}", frames[15].kinds()?),
    )?;
    record(
        &frames[15],
        "a stroke painted under the rotated crop, landing on the content pixels the affine places \
         it on",
        json!({"label":frames[15].label()?,"content":P_ROTATED,"placed":placed,"read":painted}),
    );

    Ok(json!({
        "edge": edge,
        "zoomed": {"at": P_ZOOMED, "read": zoomed, "strokes": zoomed_strokes},
        "rotated": {"content": P_ROTATED, "placed": placed, "read": painted,
                    "output": [after.output.0, after.output.1],
                    "before": [before.output.0, before.output.1]},
        "revision": frames[15].revision()?,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of patches of the displayed photograph, read back from the renderer; the coverage readings are of the mask-on-black overlay and are not a colorimetric claim",
    }))
}
