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
//!
//! Each launch is a [`Plan`]: every frame it captures, named, with what its step commits, the label
//! of the entry it leaves, the layers in the stack, the mask's components, the canvas mode, the
//! overlay and the zoom. [`verify`] reads the frames by those names and checks what a plan cannot
//! say: the strokes, the brush the panel holds and, above all, the coverage the overlay shows.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance},
    *,
};
use luxforge_core::PRESENCE_EFFECT;
use luxforge_evidence::{
    self as script, BrushStep, MaskRow, MaskStep, PaintStep, Reference, RowStep, SliderStep,
    ViewStep, WorkspaceStep,
};

pub const SCENARIO: &str = "mask-brush";
/// The Presence fixture: its bottom-right quadrant is a flat mid-grey, which is where the two
/// patches that measure the masked Presence adjustment sit. `cargo xtask generate-fixtures`.
pub const FIXTURE: &str = "fixtures/generated/presence.jpg";
/// The paragraph `reproduce.md` gives the scenario.
pub const NOTE: &str = "Generate the fixture first with `cargo xtask generate-fixtures --output fixtures/generated`.\n\nThree launches over one catalog: the first paints several strokes on one brush, feathers one at the other end of the range, erases across another and deletes one on its own; the second reopens it, subtracts a second brush from a radial gradient and drags Presence through the result; the third reopens it again and paints over the picture's edge, at 100% and under a rotated crop.";
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
/// How close to full coverage's 255 a `mask-on-black` patch must read before this scenario calls it
/// covered (200 or brighter), and how close to no coverage's 0 before it calls it uncovered (40 or
/// darker). The overlay paints coverage straight into all three channels; these are margins, not
/// tuned thresholds.
const COVERED_WITHIN: f64 = 55.0;
const UNCOVERED: f64 = 40.0;
/// How close to 127.5 a reading inside a feather band must stay to count as partial: 60 to 195,
/// clear of both endpoints by the same margins. A hard stroke read at the same offset is covered,
/// which is the comparison the two settings are proved by.
const PARTIAL_WITHIN: f64 = 67.5;
/// How far the hard strokes' band must read above the feathered stroke's, at the same offset from
/// the centre line, before this scenario calls the two feather settings different.
const BANDS_APART: f64 = 50.0;
/// How far the flat quadrant's mean must move before this scenario calls the masked Presence
/// adjustment visible, and how close it must stay before it calls a patch untouched. Both are read
/// from renderer readback with no JPEG between, so the untouched bound is tight on purpose.
const PRESENCE_MOVED: f64 = 4.0;
const PRESENCE_UNTOUCHED: f64 = 1.0;

/// A script step that commits nothing: the same revision and the same current entry as the frame
/// before.
fn uncommitted(name: &str, script: script::Step) -> Step {
    Step::new(name, script).commits(0)
}

/// The composed mask launch 2 leaves: the brush launch 1 painted, the radial beside it and the
/// subtracting brush.
const COMPOSED: [&str; 3] = ["add brush", "add radial", "subtract brush"];

/// The coverage overlay on, which is what every coverage reading is taken from, or off.
fn overlay(name: &str, setting: &str) -> Step {
    uncommitted(
        name,
        script::Step::Workspace(WorkspaceStep::default().mask_overlay(setting)),
    )
    .workspace("mask_overlay", json!(setting))
}

/// Mask mode, through the same `workspace.set` the mode strip sends.
fn mask_mode() -> Step {
    uncommitted(
        "mask-mode",
        script::Step::Workspace(WorkspaceStep::default().mode("mask")),
    )
    .mode("mask")
}

/// A stroke painted and released with the brush the panel holds: one entry, labelled `label`.
fn stroke(name: &str, points: [[f64; 2]; 2], label: &str) -> Step {
    Step::new(
        name,
        MaskStep::Stroke {
            points: Vec::from(points),
            release: true,
            interval_ms: None,
            settle_between: false,
        },
    )
    .commits(1)
    .label(label)
}

/// Launch 1: one brush component, painted, feathered at both ends of its range, erased across and
/// one stroke deleted from the middle of it.
///
/// Three launches rather than one because every masked frame on this fixture is a render of its own
/// and a launch is bounded by the smoke command's own deadline — and a scenario that takes three
/// processes to stay inside it proves the catalog twice over rather than once.
pub fn plan1(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The fixture as launched, with no mask in the recipe.
        Step::opened("opened").masks(0),
        // 1: Mask mode.
        mask_mode(),
        // 2: the brush the first strokes are drawn with: one size, and the hard edge.
        uncommitted(
            "hard-brush",
            script::Step::Mask(MaskStep::Brush(BrushStep {
                size: Some(SIZE),
                feather: Some(HARD),
                flow: Some(100.0),
                ..BrushStep::default()
            })),
        ),
        // 3: a new mask whose first component is an add brush. A brush is reached from its own
        // section and not from the Add row, because it declares no geometry to type.
        uncommitted(
            "new-mask",
            script::Step::Mask(MaskStep::Paint(PaintStep::NewMask)),
        ),
        // 4: the first stroke: one mask, one component and one stroke, in one history entry.
        stroke("stroke-a", A, "Add brush")
            .masks(1)
            .components(&COMPOSED[..1]),
        // 5: the second, on the same component, with no second gesture: the brush re-arms itself.
        // Several strokes in one mask are an ordinary list: still one row.
        stroke("stroke-b", B, "Update Brush 1").components(&COMPOSED[..1]),
        // 6: the coverage itself on screen, which is what every reading below is taken from.
        overlay("overlay", "mask-on-black"),
        // 7-8: the same brush at the other end of its feather range, and a third stroke with it.
        uncommitted(
            "soft-brush",
            script::Step::Mask(MaskStep::Brush(BrushStep {
                feather: Some(SOFT),
                ..BrushStep::default()
            })),
        ),
        stroke("stroke-c", C, "Update Brush 1"),
        // 9-10: an erase stroke across the second one. The erase flag is the brush's, held for the
        // stroke's whole life.
        uncommitted(
            "erase-brush",
            script::Step::Mask(MaskStep::Brush(BrushStep {
                feather: Some(HARD),
                erase: Some(true),
                ..BrushStep::default()
            })),
        ),
        stroke("erase", ERASE, "Update Brush 1"),
        // 11: back to adding, so nothing later inherits the erase.
        uncommitted(
            "add-brush",
            script::Step::Mask(MaskStep::Brush(BrushStep {
                erase: Some(false),
                ..BrushStep::default()
            })),
        ),
        // 12: the component selected, which is what lists its strokes on the row.
        uncommitted(
            "select",
            script::Step::Mask(MaskStep::SelectComponent(Some(Reference::Index(0)))),
        ),
        // 13: one stroke deleted on its own — the feathered one — as a forward edit: one entry
        // appended, every other stroke exactly where it was.
        Step::new(
            "delete-stroke",
            MaskStep::Row(MaskRow {
                component: Reference::Index(0),
                edit: RowStep::DeleteStroke(Reference::Index(2)),
            }),
        )
        .commits(1)
        .label("Delete a stroke from Brush 1"),
    ])
}

/// Launch 2, over the same catalog: a radial gradient beside the painted brush, a second brush
/// subtracting from it, an adjustment through the result, and an undo.
pub fn plan2(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The catalog reopened in a new process: its one mask, no gesture open and no Presence
        // layer.
        Step::opened("reopened")
            .no_draft()
            .no_layer(PRESENCE_EFFECT)
            .masks(1),
        // 1-2: Mask mode and the coverage on screen, in a new process: what the first launch
        // painted is read back from the pixels before anything is added to it.
        mask_mode(),
        overlay("overlay", "mask-on-black"),
        // 3-5: a radial gradient in the same mask, swept from its centre out to one radius. The Add
        // row is at Add, which is where a reopened panel starts.
        uncommitted(
            "add-radial",
            script::Step::Mask(MaskStep::Add(RADIAL.into())),
        ),
        uncommitted(
            "sweep",
            script::Step::Mask(MaskStep::Sweep {
                from: RADIAL_AT,
                to: radial_to(),
            }),
        ),
        Step::new("apply-radial", MaskStep::Release)
            .commits(1)
            .label("Add radial")
            .components(&COMPOSED[..2]),
        // 6-8: a second brush component, in subtract mode, painted inside that gradient. This is
        // the owner's own requirement, and it is one more row in the same list.
        uncommitted(
            "subtract-mode",
            script::Step::Mask(MaskStep::Mode("subtract".into())),
        ),
        uncommitted(
            "new-brush",
            script::Step::Mask(MaskStep::Paint(PaintStep::NewBrush)),
        ),
        stroke("subtract-stroke", SUBTRACT, "Add subtract brush").components(&COMPOSED),
        // 9: the overlay off, leaving the photograph.
        overlay("overlay-off", "off"),
        // 10: Presence through the finished mask, as the panel's own drag. The brush is still armed
        // from the stroke above and gives its draft up to this gesture, exactly as it gives it up to
        // every other one.
        Step::new(
            "dehaze",
            SliderStep::new(PRESENCE, DEHAZE, [12.0, DEHAZED]).release(),
        )
        .commits(1)
        .label("Mask 1 · Dehaze +30")
        .no_draft()
        .payload(PRESENCE_EFFECT, json!({ DEHAZE: DEHAZED })),
        // 11: and undone. An undo moves the revision on by one, to the entry before, which is the
        // subtract stroke's, with every component where it was.
        Step::new("undo", script::Step::api("history.undo"))
            .commits(1)
            .label("Add subtract brush")
            .no_layer(PRESENCE_EFFECT)
            .components(&COMPOSED),
    ])
}

/// Launch 3, over the same catalog again — the one launch 1 wrote, which launch 2 opened by path and
/// added to: painting carried on over the picture's own edge, at 100%, and under a rotated crop.
pub fn plan3(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The catalog reopened in a third process, holding no gesture.
        Step::opened("reopened").no_draft(),
        // 1-2: Mask mode, where the panel lists the composed mask launch 2 left, and the coverage,
        // in a third process.
        mask_mode().components(&COMPOSED),
        overlay("overlay", "mask-on-black"),
        // 3-4: the brush in hand again — a reopened editor holds no gesture — armed on the
        // component the first launch painted, by the name the panel gives it.
        uncommitted(
            "brush",
            script::Step::Mask(MaskStep::Brush(BrushStep {
                size: Some(SIZE),
                feather: Some(HARD),
                flow: Some(100.0),
                ..BrushStep::default()
            })),
        ),
        uncommitted(
            "arm",
            script::Step::Mask(MaskStep::Paint(PaintStep::Component(Reference::name(
                "Brush 1",
            )))),
        ),
        // 5: a stroke that starts off the picture and paints in over its left edge.
        stroke("edge-stroke", EDGE, "Update Brush 1"),
        // 6-8: painting at 100%, where the exact frame is what is on screen, and back to Fit, where
        // that stroke is where the content coordinates it was painted in put it.
        uncommitted("zoom-100", script::Step::View(ViewStep::Percent(100.0))).percent(100.0),
        stroke("zoomed-stroke", ZOOMED, "Update Brush 1").percent(100.0),
        uncommitted("fit", script::Step::View(ViewStep::Fit)).fit(),
        // 9: the overlay off.
        overlay("overlay-off", "off"),
        // 10: the geometry tail as one affine, before the crop: the map a gesture places a stroke
        // with, read through the same method the canvas reads it through.
        uncommitted("transform-before", script::Step::api("render.transform")),
        // 11: a straightened, fitted crop: the picture is now rotated under the mask.
        Step::new(
            "crop",
            script::Step::call("edit.crop-fit", json!({"aspect":"16:9","angle":8.0})),
        )
        .commits(1)
        .label("Crop 16:9"),
        // 12: the affine again, which is what the probe below is placed by.
        uncommitted("transform-after", script::Step::api("render.transform")),
        // 13-15: the coverage back on, the brush re-armed on that component after the crop, and one
        // more stroke painted in content coordinates under the rotated picture.
        overlay("overlay-again", "mask-on-black"),
        uncommitted(
            "rearm",
            script::Step::Mask(MaskStep::Paint(PaintStep::Component(Reference::name(
                "Brush 1",
            )))),
        ),
        stroke("rotated-stroke", ROTATED, "Update Brush 1").components(&COMPOSED),
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
    let held = &frame.state()["masks"]["brush"]["fields"][field];
    held.as_f64()
        .or_else(|| held.as_str().and_then(|text| text.parse::<f64>().ok()))
        .ok_or_else(|| {
            format!(
                "Frame records no brush {field}: {}",
                frame.state()["masks"]["brush"]
            )
            .into()
        })
}

/// All nine probes of one capture: the mean Rec. 709 luminance of a small patch at each, in the
/// photograph's recorded rectangle.
fn probes(frame: &Frame) -> Result<[f64; 9]> {
    let mut out = [0.0; 9];
    for (slot, at) in out.iter_mut().zip(PROBES) {
        *slot = frame.luminance_at(at, PATCH_HALF)?;
    }
    Ok(out)
}

/// What one probe must read: full coverage, none, or somewhere strictly between the two. Each is a
/// distance from a centre: full is 200 or brighter, within [`COVERED_WITHIN`] of the overlay's
/// 255; none is 40 or darker, within [`UNCOVERED`] of its 0; partial is 60 to 195, within
/// [`PARTIAL_WITHIN`] of 127.5.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reads {
    Full,
    None,
    Partial,
}

impl Reads {
    fn centre(self) -> (f64, f64, &'static str) {
        match self {
            Self::Full => (255.0, COVERED_WITHIN, "full coverage"),
            Self::None => (0.0, UNCOVERED, "no coverage"),
            Self::Partial => (127.5, PARTIAL_WITHIN, "partial coverage"),
        }
    }
}

/// One reading of `frame` at `at` against what it must read.
fn reads(checks: &mut Checks, frame: &Frame, what: &str, value: f64, wanted: Reads) -> Result {
    let (centre, within, name) = wanted.centre();
    checks.compare(
        frame,
        &format!("{what}: {name}"),
        value,
        centre,
        Tolerance::Within(within),
    )
}

/// One coverage frame against what the composition and the accumulation rules say it must be, at
/// each of the nine probes in turn.
fn coverage(
    checks: &mut Checks,
    frame: &Frame,
    what: &str,
    expected: [Reads; 9],
) -> Result<[f64; 9]> {
    let read = probes(frame)?;
    for ((value, want), name) in read.iter().zip(expected).zip(PROBE_NAMES) {
        reads(checks, frame, &format!("{what}, {name}"), *value, want)?;
    }
    Ok(read)
}

/// One `render.transform` answer, as the step recorded it: the geometry tail as one affine, which is
/// what maps a content position onto the frame a person sees.
struct Tail {
    content: (f64, f64),
    output: (f64, f64),
    mapping: luxforge_core::GeometryMap,
}

impl Tail {
    fn read(step: &Value) -> Result<Self> {
        let answer = &step["result"];
        let number = |value: &Value| -> Result<f64> {
            value
                .as_f64()
                .ok_or_else(|| format!("render.transform answered {answer}").into())
        };
        Ok(Self {
            content: (
                number(&answer["content"]["width"])?,
                number(&answer["content"]["height"])?,
            ),
            output: (
                number(&answer["output"]["width"])?,
                number(&answer["output"]["height"])?,
            ),
            mapping: serde_json::from_value::<luxforge_core::MappingDescriptor>(answer.clone())?
                .geometry,
        })
    }

    /// Where a normalized content position lands in the frame, as a fraction of the drawn
    /// photograph: the same map the canvas puts the brush cursor through.
    fn place(&self, at: [f64; 2]) -> [f64; 2] {
        let (x, y) = (at[0] * self.content.0, at[1] * self.content.1);
        let (ox, oy) = self.mapping.to_output(x, y).expect("captured geometry");
        [ox / self.output.0, oy / self.output.1]
    }
}

/// The whole scenario's own checks, once each launch's plan has held: what the three launches show,
/// checked in order, launch 2 against the mask and the pixels launch 1 left.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [launch1, launch2, launch3] = launches else {
        return Err(format!("Expected three launches, found {}", launches.len()).into());
    };
    let mut checks = Checks::new();
    let (mask, opened) = verify_launch1(launch1, &mut checks)?;
    verify_launch2(launch2, &mask, opened, &mut checks)?;
    verify_launch3(launch3, &mut checks)?;
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "mask": mask,
            "covered_within": COVERED_WITHIN,
            "uncovered_within": UNCOVERED,
            "partial_within": PARTIAL_WITHIN,
            "scope": "Mean Rec. 709 luminance of patches of the photograph the editor records drawing, read back from the renderer; the coverage readings are of the mask-on-black overlay, which paints coverage straight into all three channels, and are not a colorimetric claim",
        }),
    )
}

use Reads::{Full, None as Clear, Partial};

/// Launch 1, step by step: the strokes, the two feather settings, the erase and the delete. Returns
/// the mask's identity and the opened photograph's probes.
fn verify_launch1(launch: &Checked, checks: &mut Checks) -> Result<(String, [f64; 9])> {
    let opened = probes(launch.at("opened")?)?;
    let mask = launch.at("stroke-a")?.only_mask()?["id"]
        .as_str()
        .ok_or("The listed mask has no identity")?
        .to_owned();

    // The coverage itself. Both strokes are covered, the band beside the hard one is covered too —
    // a hard edge is coverage 1 right up to the radius — and nothing else is.
    coverage(
        checks,
        launch.at("overlay")?,
        "two hard add strokes",
        [Full, Full, Full, Clear, Clear, Full, Clear, Clear, Clear],
    )?;

    // The brush at the other feather, before it has painted anything: the brush the panel holds
    // says so, and the plan holds that nothing is committed.
    let soft = launch.at("soft-brush")?;
    ensure(
        (brush(soft, "feather")? - SOFT).abs() < 0.5,
        format!("The brush holds feather {}", brush(soft, "feather")?),
    )?;

    // The feathered stroke. Its centre line is full coverage and its band at half a radius is
    // strictly between the endpoints, where the hard strokes' band at the same offset is full. That
    // is the two feather settings, measured in the photograph rather than read off a payload.
    let third = launch.at("stroke-c")?;
    let feathered = coverage(
        checks,
        third,
        "a fully feathered third stroke",
        [Full, Full, Full, Full, Partial, Full, Clear, Clear, Clear],
    )?;
    checks.compare(
        third,
        "the hard strokes' band against the feathered one's, at the same offset",
        feathered[1],
        feathered[4],
        Tolerance::Above(BANDS_APART),
    )?;

    // The erase stroke. It takes coverage out of the second stroke where it crosses it and leaves
    // the rest of that stroke exactly as it was.
    coverage(
        checks,
        launch.at("erase")?,
        "an erase stroke across the second add stroke",
        [Full, Full, Full, Full, Partial, Clear, Clear, Clear, Clear],
    )?;

    // The row selected, which is what lists its strokes: four, in the order they compose.
    let listed = strokes(launch.at("select")?, 0)?;
    ensure(
        listed.len() == 4,
        format!("The brush row lists {} strokes", listed.len()),
    )?;

    // One stroke deleted on its own. A forward edit: one entry appended, the feathered stroke gone
    // from the picture, every other stroke exactly where it was — including the erase, which is what
    // proves the order survived a removal from the middle of it.
    let delete = launch.at("delete-stroke")?;
    let kept = strokes(delete, 0)?;
    ensure(
        kept == [listed[0].clone(), listed[1].clone(), listed[3].clone()],
        format!("The delete left {kept:?} of {listed:?}"),
    )?;
    coverage(
        checks,
        delete,
        "the feathered stroke deleted on its own",
        [Full, Full, Full, Clear, Clear, Clear, Clear, Clear, Clear],
    )?;
    Ok((mask, opened))
}

/// Launch 2: the reopened catalog, a radial gradient beside the brush, a second brush subtracting
/// from it, the masked adjustment and the undo.
fn verify_launch2(launch: &Checked, mask: &str, opened: [f64; 9], checks: &mut Checks) -> Result {
    // The reopened catalog. The mask, its component and its strokes are back, by the same
    // identities the first launch committed, with no mask gesture open; the plan holds that no
    // slider gesture is open and no Presence layer is in the stack.
    let reopened = launch.at("reopened")?;
    ensure(
        reopened.only_mask()?["id"] == json!(mask),
        format!("The reopened catalog holds {}", reopened.only_mask()?["id"]),
    )?;
    ensure(
        reopened.state()["mask_draft"] == Value::Null,
        "A reopened editor holds an open mask gesture",
    )?;

    // The coverage after the reopen, read at the same nine points the first launch ended on. The
    // strokes survived as pixels and not only as rows.
    coverage(
        checks,
        launch.at("overlay")?,
        "the reopened mask",
        [Full, Full, Full, Clear, Clear, Clear, Clear, Clear, Clear],
    )?;

    // The radial gradient, committed into the same mask as a second component.
    coverage(
        checks,
        launch.at("apply-radial")?,
        "a radial gradient beside the brush",
        [Full, Full, Full, Clear, Clear, Clear, Full, Full, Clear],
    )?;

    // The subtract brush inside that gradient. This is the requirement in one frame: a brush takes
    // a region out of a gradient, and it is one more row rather than a special gesture.
    coverage(
        checks,
        launch.at("subtract-stroke")?,
        "a brush subtracting from the radial",
        [Full, Full, Full, Clear, Clear, Clear, Full, Clear, Clear],
    )?;

    // The overlay off. No layer is bound to the mask yet, so the photograph is byte-unchanged from
    // the one the first launch opened: a mask on its own is a selection, not an edit.
    let off = launch.at("overlay-off")?;
    ensure(
        off.only_mask()?["layers"] == json!([]),
        "A mask with no adjustment already has a layer bound to it",
    )?;
    let bare = probes(off)?;
    for (index, name) in PROBE_NAMES.iter().enumerate() {
        checks.compare(
            off,
            &format!("{name} with the mask drawn and no layer bound to it"),
            bare[index],
            opened[index],
            Tolerance::Within(PRESENCE_UNTOUCHED),
        )?;
    }

    // Presence through the painted mask. The plan holds the entry and the layer's payload; here the
    // layer names the mask, and the flat quadrant moves inside the mask and is left exactly as it
    // was outside it and where the subtract brush removed the gradient.
    let dehaze = launch.at("dehaze")?;
    let layer = dehaze
        .layer(PRESENCE_EFFECT)
        .ok_or("The masked gesture committed no Presence layer")?;
    ensure(
        layer["mask"] == json!(mask),
        format!("The committed Presence layer names {}", layer["mask"]),
    )?;
    let dehazed = probes(dehaze)?;
    checks.compare(
        dehaze,
        "the covered patch under masked Presence",
        dehazed[6],
        bare[6],
        Tolerance::Apart(PRESENCE_MOVED),
    )?;
    // Strictly inside 1..254, which is strictly within 126.5 of 127.5: a clipped reading is
    // produced by a broken render as readily as by a correct one.
    checks.compare(
        dehaze,
        "the covered patch under masked Presence, clear of the ends of the range",
        dehazed[6],
        127.5,
        Tolerance::Under(126.5),
    )?;
    checks.compare(
        dehaze,
        "the uncovered patch under masked Presence",
        dehazed[8],
        bare[8],
        Tolerance::Within(PRESENCE_UNTOUCHED),
    )?;
    checks.compare(
        dehaze,
        "the patch the subtract brush removed, under masked Presence",
        dehazed[7],
        bare[7],
        Tolerance::Within(PRESENCE_UNTOUCHED),
    )?;

    // Undo. The plan holds that the layer is gone and every component is where it was; the
    // photograph is back to the one the mask alone left.
    let undo = launch.at("undo")?;
    let undone = probes(undo)?;
    for (index, name) in PROBE_NAMES.iter().enumerate() {
        checks.compare(
            undo,
            &format!("{name} after undo"),
            undone[index],
            bare[index],
            Tolerance::Within(PRESENCE_UNTOUCHED),
        )?;
    }
    Ok(())
}

/// Launch 3: painting carried on over the picture's own edge, at 100%, and under a rotated crop.
fn verify_launch3(launch: &Checked, checks: &mut Checks) -> Result {
    // A stroke that begins outside the picture. It is an ordinary stroke — stored positions run
    // from -1 to 2 — and the coverage reaches the picture's own left edge.
    let edge_stroke = launch.at("edge-stroke")?;
    let edge = edge_stroke.luminance_at(P_EDGE, PATCH_HALF)?;
    reads(
        checks,
        edge_stroke,
        "the picture's left edge after a stroke painted in over it",
        edge,
        Full,
    )?;

    // Painting at 100%, where the exact frame is what is on screen and no proxy stands in for it;
    // the plan holds the zoom and the entry. Back at Fit, where the whole picture is on screen again,
    // the stroke painted at 100% is where the content coordinates it was painted in say it is. A
    // zoom is a view and a stroke is an edit; this is what keeps the two apart.
    let fit = launch.at("fit")?;
    let zoomed = fit.luminance_at(P_ZOOMED, PATCH_HALF)?;
    reads(
        checks,
        fit,
        "the stroke painted at 100%, read at Fit",
        zoomed,
        Full,
    )?;

    // The rotated crop, with the overlay off. The mask is in content coordinates, so nothing about
    // it moved; what changed is the affine between those coordinates and the frame.
    let before = Tail::read(&launch.at("transform-before")?["step"])?;
    let after = Tail::read(&launch.at("transform-after")?["step"])?;
    ensure(
        after.output != before.output,
        format!(
            "The crop left the output stage at {:?}, so nothing was straightened",
            after.output
        ),
    )?;

    // One more stroke under the rotated picture. It is painted in content coordinates and read back
    // where the geometry tail's own affine says those coordinates land — which is the map the canvas
    // draws the brush cursor and the handles through.
    let rotated = launch.at("rotated-stroke")?;
    let placed = after.place(P_ROTATED);
    let painted = rotated.luminance_at(placed, PATCH_HALF)?;
    reads(
        checks,
        rotated,
        &format!(
            "the stroke painted under the rotated crop, at {placed:?}, where the geometry tail's \
             affine places the content position {P_ROTATED:?}"
        ),
        painted,
        Full,
    )?;
    Ok(())
}
