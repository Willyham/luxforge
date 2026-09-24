//! The `mask-combine` smoke scenario: phase B's claim, which is that combining mask kinds and
//! modes in one mask is ordinary.
//!
//! One mask is built up on the canvas out of four radial components — an `add`, a `subtract`, an
//! `intersect` and a second `add` — through the Add row's mode control and the gesture that
//! follows it. After each commit the coverage overlay is read at four fixed points of the
//! photograph, so what the composition algebra says is checked against the pixels the editor drew
//! rather than against the state it recorded. Each component's own contribution is then read the
//! same way, by putting the pointer on its row. A reorder moves the second `add` above the
//! subtraction, which is the one reorder this list admits that changes the picture, and the
//! coverage follows. Then the overlay goes off, a Presence adjustment is dragged through the mask,
//! and one undo takes it back.
//!
//! **Why the overlay is what is measured.** `mask-on-black` paints the composed coverage as an
//! opaque greyscale at the display's own resolution, so a point at coverage 1 reads white and one
//! at coverage 0 reads black. That makes the algebra legible in the capture itself: the four
//! points below are chosen so that each commit moves exactly one of them, and every one of them
//! sits clear of every component's feather band, so each reading is an endpoint and not a ramp.
//!
//! The geometry is stated in **mask-space units**, where one unit is the stage's height on both
//! axes (`docs/design/mask-study.md`), because that is the space a radial's radii live in. A
//! sweep from a centre to a point puts `radius_x = |Δx| · W/H` and `radius_y = |Δy|` on the draft,
//! so each component below is swept to `(x + r · H/W, y + r)` and is a circle.
use crate::{
    scenario::{Bright, Frame, Launch, Run, Scan, pixels, preamble},
    *,
};

pub const SCENARIO: &str = "mask-combine";
/// The Presence fixture: its bottom-right quadrant is a flat mid-grey, which is where the two
/// patches that measure the masked Presence adjustment sit. `cargo xtask generate-fixtures`.
const FIXTURE: &str = "fixtures/generated/presence.jpg";
const WINDOW: [&str; 2] = workspace_smoke::WINDOW;

const RADIAL: &str = "radial";
const PRESENCE: &str = "set-presence";
const DEHAZE: &str = "dehaze";
/// What the masked Presence gesture commits. Dehaze is the one Presence unit that moves a flat
/// field at all — Texture and Clarity are neighbourhood operations and leave one exactly as it was,
/// which `presence` asserts — so it is the one this fixture's flat quadrant can measure. It is
/// deliberately short of `+100`, which drives this fixture's flat grey to black: a reading at the
/// end of the range would be produced by a broken render as readily as by a correct one.
const DEHAZED: f64 = 30.0;

/// One component: where its gesture presses, and the radius that press is swept out to, in
/// mask-space units. The sweep's own end point is derived, so the radius is stated once.
struct Shape {
    x: f64,
    y: f64,
    radius: f64,
}

/// The fixture's aspect, `W/H`. 1440 × 960.
const ASPECT: f64 = 1.5;

/// The four components, in the order they are drawn and composed: an `add`, a `subtract` taken out
/// of it, an `intersect` that keeps only part of what is left, and a second `add` that puts the
/// subtracted region back.
const A: Shape = Shape {
    x: 0.470,
    y: 0.490,
    radius: 0.675,
};
const S: Shape = Shape {
    x: 0.575,
    y: 0.250,
    radius: 0.315,
};
const I: Shape = Shape {
    x: 0.600,
    y: 0.490,
    radius: 0.450,
};
const D: Shape = Shape {
    x: 0.570,
    y: 0.250,
    radius: 0.240,
};

impl Shape {
    /// Where the sweep ends: a press at the centre dragged out to one radius on both axes.
    const fn to(&self) -> [f64; 2] {
        [self.x + self.radius / ASPECT, self.y + self.radius]
    }

    const fn from(&self) -> [f64; 2] {
        [self.x, self.y]
    }
}

/// The four points every coverage frame is read at, as fractions of the photograph's own drawn
/// rectangle. They are named for what the composition does to them.
///
/// `KEEP` and `OUT` are both inside the fixture's flat grey quadrant, so the same two patches also
/// measure the masked Presence adjustment: one inside the finished mask and one outside it.
const KEEP: [f64; 2] = [0.635, 0.660];
const CUT: [f64; 2] = [0.600, 0.310];
const INTERSECTED: [f64; 2] = [0.270, 0.475];
const OUT: [f64; 2] = [0.885, 0.860];
const PROBES: [[f64; 2]; 4] = [KEEP, CUT, INTERSECTED, OUT];
const PROBE_NAMES: [&str; 4] = ["keep", "cut", "intersected", "out"];

/// Half the side of a measured patch, in capture pixels. The nearest boundary to any probe is
/// about 29 capture pixels away, so a patch this size is well clear of every one of them.
const PATCH_HALF: i64 = 6;
/// How bright a `mask-on-black` patch must read before this scenario calls it covered, and how
/// dark before it calls it uncovered. The overlay paints coverage straight into all three
/// channels, so full coverage is 255 and none is 0; these are margins, not tuned thresholds.
const COVERED: f64 = 200.0;
const UNCOVERED: f64 = 40.0;
/// How far the flat quadrant's mean must move before this scenario calls the masked Presence
/// adjustment visible, and how close it must stay before it calls a patch untouched. Both are
/// read from renderer readback with no JPEG between, so the untouched bound is tight on purpose.
const PRESENCE_MOVED: f64 = 4.0;
const PRESENCE_UNTOUCHED: f64 = 1.0;

/// The open frame plus one per script step.
const FRAMES: usize = 29;
/// The one step this scenario expects the host to refuse. Frame 0 is the open, so a step's own
/// one-based number and its frame's index are the same number.
const REFUSED_FRAME: usize = 24;

fn script() -> Value {
    json!([
        // 1: Mask mode, through the same `workspace.set` the mode strip sends.
        {"workspace":{"mode":"mask"}},
        // 2-5: the first component. A new mask whose first component is a radial, swept from its
        // centre out to one radius, the pointer lifted, then committed.
        {"mask":{"new":RADIAL}},
        {"mask":{"sweep":{"from":A.from(),"to":A.to()}}},
        {"mask":{"release":true}},
        {"mask":{"apply":true}},
        // 6: the coverage itself on screen, which is what every reading below is taken from.
        {"workspace":{"mask_overlay":"mask-on-black"}},
        // 7-10: a subtract, its mode chosen on the Add row before the gesture rather than guessed
        // from a modifier afterwards.
        {"mask":{"mode":"subtract"}},
        {"mask":{"add":RADIAL}},
        {"mask":{"sweep":{"from":S.from(),"to":S.to()}}},
        {"mask":{"apply":true}},
        // 11-14: an intersect.
        {"mask":{"mode":"intersect"}},
        {"mask":{"add":RADIAL}},
        {"mask":{"sweep":{"from":I.from(),"to":I.to()}}},
        {"mask":{"apply":true}},
        // 15-18: a second add, over the region the subtraction took out.
        {"mask":{"mode":"add"}},
        {"mask":{"add":RADIAL}},
        {"mask":{"sweep":{"from":D.from(),"to":D.to()}}},
        {"mask":{"apply":true}},
        // 19-22: each component's own contribution, by putting the pointer on its row. This is
        // what makes a subtraction on top of a gradient legible instead of guesswork.
        {"mask":{"hover":0}},
        {"mask":{"hover":1}},
        {"mask":{"hover":2}},
        {"mask":{"hover":3}},
        // 23: the pointer off the list, so the composition is shown again.
        {"mask":{"hover":null}},
        // 24: the one move this list refuses: a subtract at the front. The panel states that rule
        // on the row rather than offering the move, so only an explicit position reaches the
        // host's own refusal — and the refusal is what ends this step, because a refused command
        // renders nothing for it to settle on.
        {"mask":{"row":{"component":1,"index":0}}},
        // 25: the reorder that does change the picture: the second add above the subtraction, so
        // what it put back is taken out again.
        {"mask":{"row":{"component":3,"index":1}}},
        // 26: the overlay off, leaving the photograph.
        {"workspace":{"mask_overlay":"off"}},
        // 27: Presence through the mask, as the panel's own drag: the sections below the list are
        // bound to the open mask, so this commits a masked spatial layer.
        {"slider":{"action":PRESENCE,"parameter":DEHAZE,"values":[12.0,DEHAZED],"release":true}},
        // 28: and undone.
        {"api":{"method":"history.undo","params":{}}}
    ])
}

/// The modes of the open mask's components, in list order: the composition, in one line.
fn modes(frame: &Frame) -> Result<Vec<String>> {
    Ok(frame
        .components()?
        .iter()
        .map(|component| component["mode"].as_str().unwrap_or_default().to_owned())
        .collect())
}

/// The identities of the open mask's components, in list order.
fn component_ids(frame: &Frame) -> Result<Vec<String>> {
    Ok(frame
        .components()?
        .iter()
        .map(|component| component["id"].as_str().unwrap_or_default().to_owned())
        .collect())
}

/// The stack's one Presence layer, or `None` when the stack holds none.
fn presence_layer(frame: &Frame) -> Option<&Value> {
    frame.layer(lightwell_core::PRESENCE_EFFECT)
}

/// Where the photograph is drawn inside the capture.
///
/// It is found once, on the opened fixture, and reused: the zoom is Fit for the whole run and the
/// panels never move, so the rectangle is the same in every frame — and it cannot be found again
/// from a frame the coverage overlay has painted black, which is most of them. The vertical extent
/// is taken first, for the reason [`Scan::Tallest`] records.
const BOUNDS: Bright = Bright {
    threshold: 32,
    scan: Scan::Tallest { last_row: false },
    least: Some((200, 100)),
};

/// All four probes of one capture: the mean Rec. 709 luminance of a small patch at each.
fn probes(frame: &Frame, bounds: [u32; 4]) -> Result<[f64; 4]> {
    let image = frame.image()?;
    let mut out = [0.0; 4];
    for (slot, at) in out.iter_mut().zip(PROBES) {
        *slot = pixels::mean_luminance(image, pixels::at(bounds, at), PATCH_HALF)?;
    }
    Ok(out)
}

/// One coverage frame against what the composition algebra says it must be: `1` for covered, `0`
/// for uncovered, at each of the four probes in turn.
fn coverage(frame: &Frame, bounds: [u32; 4], what: &str, expected: [u8; 4]) -> Result<[f64; 4]> {
    let read = probes(frame, bounds)?;
    for ((value, want), name) in read.iter().zip(expected).zip(PROBE_NAMES) {
        let ok = if want == 1 {
            *value >= COVERED
        } else {
            *value <= UNCOVERED
        };
        ensure(
            ok,
            format!(
                "{what}: {name} read {value:.1}, which is not coverage {want} \
                 (covered is >= {COVERED}, uncovered <= {UNCOVERED})"
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

/// The whole scenario: one launch, and every frame checked against the algebra and the pixels.
pub fn run(mut run: Run) -> Result {
    let fixture = run.root().join(FIXTURE);
    run.note(
        "Generate the fixture first with `cargo xtask generate-fixtures --output fixtures/generated`.\n\nOne mask of four radial components in three modes, its coverage read from the `mask-on-black` overlay after every commit, a refused reorder, a reorder that changes the picture, a masked Presence drag and an undo.",
    );
    run.check(|run| {
        ensure(
            fixture.is_file(),
            format!("{FIXTURE} is missing; run `cargo xtask generate-fixtures`"),
        )?;
        run.hash(std::slice::from_ref(&fixture))?;
        let evidence = run.launch(
            Launch::named("launch")
                .open(&fixture)
                .script("script.json", script())
                .window(WINDOW),
        )?;
        let (app, _) = preamble(&evidence, FRAMES)?;
        let checks = verify(&evidence, &app)?;
        run.record("checks", checks.clone());
        run.sources_unchanged()?;
        write_json(
            &run.out().join("mask-combine-checks.json"),
            &json!({"checks": checks}),
        )?;
        Ok(())
    })
}

/// Every frame, in order, against the algebra and against the pixels.
fn verify(evidence: &Path, app: &Value) -> Result<Value> {
    let frames = Frame::all(evidence, app)?;
    ensure(
        frames.len() == FRAMES,
        format!("The run wrote {} frames", frames.len()),
    )?;

    // Exactly one step was refused, and it is the one the script asked to be refused. Unlike every
    // other scenario this one expects `had_input_errors`, because a refusal the host makes is what
    // step 24 is evidence of; what must hold is that it is the *only* one.
    ensure(
        app["had_input_errors"] == json!(true),
        "The run recorded no refusal, but step 24 asks the host for one",
    )?;
    let steps = app["script"].as_array().ok_or("Missing steps")?;
    let failed: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step["status"] == json!("failed"))
        .map(|(index, _)| index + 1)
        .collect();
    ensure(
        failed == vec![REFUSED_FRAME],
        format!("The run refused steps {failed:?}, not only step {REFUSED_FRAME}"),
    )?;

    let bounds = pixels::bright_bounds(&frames[0], BOUNDS)?;
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // Frame 0: the fixture as launched, with no mask in the recipe.
    ensure(
        frames[0].masks()?.is_empty() && presence_layer(&frames[0]).is_none(),
        "The fixture opened with a mask or a Presence layer already in the recipe",
    )?;
    let opened = probes(&frames[0], bounds)?;
    record(
        &frames[0],
        "the fixture as launched, with no mask in the recipe",
        json!({"patches":opened,"bounds":bounds}),
    );

    // Frame 5: the first component committed. One mask, one `add` component, no layer bound to it.
    ensure(
        frames[5].revision()? == frames[0].revision()? + 1,
        format!("Apply moved the revision to {}", frames[5].revision()?),
    )?;
    ensure(
        modes(&frames[5])? == ["add"],
        format!("The first commit holds {:?}", modes(&frames[5])?),
    )?;
    let mask = frames[5].only_mask()?["id"]
        .as_str()
        .ok_or("The listed mask has no identity")?
        .to_owned();

    // Frames 6, 10, 14, 18: the composition after each commit, read off the coverage itself. Each
    // one moves exactly one probe, which is what makes the algebra legible in the captures.
    let stages: [(usize, &str, [u8; 4], &str); 4] = [
        (
            6,
            "the first add alone",
            [1, 1, 1, 0],
            "one radial: everything inside it is selected",
        ),
        (
            10,
            "the add minus the subtract",
            [1, 0, 1, 0],
            "the subtracted radial takes its own region back out",
        ),
        (
            14,
            "intersected",
            [1, 0, 0, 0],
            "the intersect keeps only what both it and what came before hold",
        ),
        (
            18,
            "the second add restored",
            [1, 1, 0, 0],
            "a second add puts back exactly the region the subtraction removed",
        ),
    ];
    let mut readings = Vec::new();
    for (index, what, expected, caption) in stages {
        let read = coverage(&frames[index], bounds, what, expected)?;
        readings.push(
            json!({"frame":frames[index]["file"],"stage":what,"expected":expected,"patches":read}),
        );
        record(
            &frames[index],
            caption,
            json!({"modes":modes(&frames[index])?,"expected":expected,"patches":read}),
        );
    }
    ensure(
        modes(&frames[18])? == ["add", "subtract", "intersect", "add"],
        format!("The finished mask holds {:?}", modes(&frames[18])?),
    )?;
    ensure(
        frames[18].revision()? == frames[0].revision()? + 4,
        "Four components were not four history entries",
    )?;
    let drawn = component_ids(&frames[18])?;

    // Frames 19-22: each component's own contribution, shown by pointing at its row. What the
    // overlay draws is the component's own field, before its mode is applied, so the subtract's row
    // reads 1 where it subtracts. Nothing is committed and nothing is selected by pointing at one.
    //
    // Rows 1 and 3 read the same at these four points, because the subtract and the second add are
    // concentric by construction — that is what lets the second add restore exactly what the
    // subtract removed. What separates them is the composition, which frames 10 and 18 measure, and
    // the radii, which the captures show.
    let alone: [[u8; 4]; 4] = [[1, 1, 1, 0], [0, 1, 0, 0], [1, 1, 0, 0], [0, 1, 0, 0]];
    for (row, expected) in alone.into_iter().enumerate() {
        let index = 19 + row;
        ensure(
            frames[index].revision()? == frames[18].revision()?,
            "Pointing at a component row committed something",
        )?;
        ensure(
            frames[index]
                .components()?
                .iter()
                .position(|component| component["hovered"] == json!(true))
                == Some(row),
            format!(
                "Frame {index} shows {} hovered",
                json!(frames[index].components()?)
            ),
        )?;
        let read = coverage(
            &frames[index],
            bounds,
            &format!("component {row} alone"),
            expected,
        )?;
        record(
            &frames[index],
            "one component's own contribution, shown by pointing at its row",
            json!({"component":drawn[row],"mode":modes(&frames[index])?[row],"expected":expected,"patches":read}),
        );
    }

    // Frame 23: the pointer off the list, and the composition again.
    let composed = coverage(
        &frames[23],
        bounds,
        "the composition with the pointer off the list",
        [1, 1, 0, 0],
    )?;
    record(
        &frames[23],
        "the composed mask again, with the pointer off the list",
        json!({"patches":composed}),
    );

    // Frame 24: the refused reorder. The host's own reason is on the step, the list did not move,
    // and the coverage is exactly what it was — and the run reached this frame at all, which is
    // the point: a refused command renders nothing, so the step is ended by the refusal.
    let refused = &steps[REFUSED_FRAME - 1];
    ensure(
        refused["status"] == json!("failed"),
        format!("Step {REFUSED_FRAME} was not refused: {refused}"),
    )?;
    let reason = refused["reason"]
        .as_str()
        .ok_or("The refused step recorded no reason")?;
    ensure(
        reason.contains("add"),
        format!("The refusal's reason is {reason:?}"),
    )?;
    ensure(
        component_ids(&frames[24])? == drawn && modes(&frames[24])? == modes(&frames[18])?,
        "The refused reorder moved the list",
    )?;
    ensure(
        frames[24].revision()? == frames[18].revision()?,
        "The refused reorder committed something",
    )?;
    let after_refusal = coverage(
        &frames[24],
        bounds,
        "the coverage after a refused reorder",
        [1, 1, 0, 0],
    )?;
    record(
        &frames[24],
        "a reorder the host refused: its reason on the step, the list and the coverage unmoved",
        json!({"reason":reason,"patches":after_refusal,"modes":modes(&frames[24])?}),
    );

    // Frame 25: the reorder that changes the picture. The second add now applies before the
    // subtraction rather than after it, so what it restored is taken out again.
    ensure(
        frames[25].revision()? == frames[24].revision()? + 1,
        "The reorder did not commit one entry",
    )?;
    ensure(
        modes(&frames[25])? == ["add", "add", "subtract", "intersect"],
        format!("The reordered mask holds {:?}", modes(&frames[25])?),
    )?;
    ensure(
        component_ids(&frames[25])?
            == [
                drawn[0].clone(),
                drawn[3].clone(),
                drawn[1].clone(),
                drawn[2].clone(),
            ],
        format!("The reorder produced {:?}", component_ids(&frames[25])?),
    )?;
    let reordered = coverage(
        &frames[25],
        bounds,
        "the coverage after the reorder",
        [1, 0, 0, 0],
    )?;
    record(
        &frames[25],
        "the second add moved above the subtraction, so the region it restored is removed again",
        json!({"modes":modes(&frames[25])?,"label":frames[25].label()?,"patches":reordered}),
    );

    // Frame 26: the overlay off. The mask is a selection and nothing else: no layer is bound to it
    // yet, so the photograph is byte-unchanged from the one that opened.
    ensure(
        frames[26].only_mask()?["layers"] == json!([]),
        "A mask with no adjustment already has a layer bound to it",
    )?;
    let bare = probes(&frames[26], bounds)?;
    for (index, name) in PROBE_NAMES.iter().enumerate() {
        untouched(
            &format!("{name} with the mask drawn and no layer bound to it"),
            bare[index],
            opened[index],
        )?;
    }
    record(
        &frames[26],
        "the overlay off: a mask on its own is a selection, not an edit",
        json!({"patches":bare}),
    );

    // Frame 27: Presence through the mask. The flat quadrant moves inside the mask and is left
    // exactly as it was outside it, and the layer the panel committed names the mask.
    ensure(
        frames[27].revision()? == frames[26].revision()? + 1,
        format!(
            "The masked drag moved the revision to {}",
            frames[27].revision()?
        ),
    )?;
    let layer =
        presence_layer(&frames[27]).ok_or("The masked gesture committed no Presence layer")?;
    ensure(
        layer["mask"] == json!(mask),
        format!("The committed Presence layer names {}", layer["mask"]),
    )?;
    ensure(
        layer["payload"][DEHAZE] == json!(DEHAZED),
        format!("The committed layer holds {}", layer["payload"]),
    )?;
    let dehazed = probes(&frames[27], bounds)?;
    moved(
        "the covered patch under masked Presence",
        dehazed[0],
        bare[0],
    )?;
    ensure(
        dehazed[0] > 1.0 && dehazed[0] < 254.0,
        format!(
            "The covered patch read {:.2}, which is against the end of the range: a clipped \
             reading is produced by a broken render as readily as by a correct one",
            dehazed[0]
        ),
    )?;
    untouched(
        "the uncovered patch under masked Presence",
        dehazed[3],
        bare[3],
    )?;
    record(
        &frames[27],
        "one part of the photograph adjusted through the composed mask and the other left alone",
        json!({"layer":layer["id"],DEHAZE:DEHAZED,"label":frames[27].label()?,
               "covered":dehazed[0],"uncovered":dehazed[3],"before":bare}),
    );

    // Frame 28: undo. The layer is gone, the mask and every component keep their identities in
    // their reordered order, and the photograph is back to the one the mask alone left.
    ensure(
        frames[28].revision()? == frames[27].revision()? + 1,
        "Undo did not advance the revision",
    )?;
    ensure(
        presence_layer(&frames[28]).is_none(),
        "Undo left the masked Presence layer in the stack",
    )?;
    ensure(
        component_ids(&frames[28])? == component_ids(&frames[25])?,
        "Undo changed the component identities or their order",
    )?;
    let undone = probes(&frames[28], bounds)?;
    for (index, name) in PROBE_NAMES.iter().enumerate() {
        untouched(&format!("{name} after undo"), undone[index], bare[index])?;
    }
    record(
        &frames[28],
        "the undone state: the composed mask, with nothing applied through it",
        json!({"patches":undone,"label":frames[28].label()?}),
    );

    Ok(json!({
        "mask": mask,
        "components": drawn,
        "reordered": component_ids(&frames[25])?,
        "modes": modes(&frames[25])?,
        "revision": frames[28].revision()?,
        "refused_step": {"step": REFUSED_FRAME, "reason": reason},
        "coverage": readings,
        "presence": {"covered": dehazed[0], "uncovered": dehazed[3], "before": bare},
        "covered_threshold": COVERED,
        "uncovered_threshold": UNCOVERED,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of four patches of the displayed photograph, read back from the renderer; the coverage readings are of the mask-on-black overlay, which paints coverage straight into all three channels, and are not a colorimetric claim",
    }))
}
