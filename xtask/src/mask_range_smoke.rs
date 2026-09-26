//! The `mask-range` smoke scenario: phase D's claim, which is that a deterministic per-pixel
//! selection is worth having **and** that what it cannot do is worth saying.
//!
//! Two launches over one catalog. The first types a luminance band, intersects a gradient with it,
//! applies an adjustment through the result and shows the band taking a grey card along with the sky
//! it was drawn for; then intersects a colour range, picks the sky off the photograph and shows the
//! grey card come back; then puts a `+0.75 EV` layer **ahead** of the masked one and shows the band
//! stop selecting the sky altogether; then asks the composed mask and each component for its overlay,
//! where all four grids are drawn and the two range components' own grids show the failure and its
//! remedy as pictures. The second reopens
//! that catalog and takes the colour range's own limits one at a time — a sampled grey selecting every
//! neutral, a second swatch adding a colour, a third changing nothing, and one person's skin selecting
//! another's — and finishes with the colour-constrained brush: one stroke across two surfaces, and a
//! colour-held erase that takes one of them back out and leaves the other alone — read from the
//! picture and then from that mask's own overlay, which a colour-held stroke used to have none of.
//!
//! **Why the readings are taken from the photograph as well as from the overlay.** Every claim this
//! scenario makes about what a selection selects is read from the rendered photograph with an
//! adjustment applied through the mask, because that is the picture a person is actually editing. The
//! overlay is now read too, and read **against** those same frames: a mask that reads pixels is
//! answered on the input of its first bound layer ([proposal P16](../../docs/design/range-study.md#proposals),
//! decided by the owner on 2026-09-23), so the composed mask's grid and the frame the masked Exposure
//! produced are two views of one selection and are checked patch by patch against each other. The
//! overlay at Fit is still read on a downscaled pixel and the 100% view is still the truth; what the
//! grid buys is that a selection can be *seen* rather than only inferred.
//!
//! **Why the comparisons are against a control inside the same frame.** A value-based selection moves
//! when a layer ahead of it changes the operation's input, and proving that by predicting an output
//! code would be proving the arithmetic twice. The fixture instead holds the **same** sky colour in two
//! places and the mask's gradient reaches one of them, so the claim is read as an equality between two
//! patches of one frame: while the band selects the sky, the two differ by the masked adjustment, and
//! once a layer ahead of it has moved the sky off the band they are equal again.
//!
//! Positions are normalized content coordinates — `x` a fraction of the content stage's width, `y` of
//! its height — which is what a script paints and sweeps in. A pick is in **output-stage pixels**,
//! which is what `render.locate` takes and what the canvas publishes.
//!
//! Each launch is a [`Plan`]: every frame it captures, named by the step that produces it, with what
//! that step commits and records. [`verify`] reads the frames by those names for everything a plan
//! cannot say — above all what the photograph and its overlay show.
use crate::{
    fixtures::{RANGE_COLUMNS, RANGE_PATCHES, RANGE_ROWS},
    scenario::{
        Bright, Checked, Frame, Plan, Run, Scan, Step, Tolerance,
        pixels::{self, compare},
    },
    *,
};
use lightwell_core::BASIC_EFFECT;
use lightwell_evidence::{
    self as script, BrushStep, MaskStep, PaintStep, Reference, SliderStep, WorkspaceStep,
};

pub const SCENARIO: &str = "mask-range";
/// The range fixture: twelve flat patches of the 24-patch chart's own sRGB renderings, laid out so
/// each of the study's measured failures is legible in one frame. `cargo xtask generate-fixtures`.
pub const FIXTURE: &str = "fixtures/generated/range.jpg";

/// The paragraph `reproduce.md` gives this scenario.
pub const NOTE: &str = "Generate the fixture first with `cargo xtask generate-fixtures --output fixtures/generated`.\n\nTwo launches over one catalog: the first types a luminance band, intersects a gradient and a picked colour range with it, shows a grey card taken along with the sky and then given back, shows a layer ahead of the mask stop the selection altogether, and asks the composed mask and each component for its coverage overlay, checking the composition's grid patch by patch against the frame the masked adjustment produced; the second takes the colour range's own limits one at a time and finishes with a colour-held erase across two surfaces, read from the picture and then from that painted mask's own overlay.\n\nThe patches are the 24-patch reflective colour chart's own sRGB renderings, which is what `docs/design/range-study.md` measured over.";

const BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";
const LINEAR: &str = "linear";
const LUMINANCE_RANGE: &str = "luminance-range";
const COLOUR_RANGE: &str = "colour-range";
const SET_LUMINANCE: &str = "mask.set-luminance-range";
/// The gesture a linear component is drawn with, which holds the swept geometry until it is applied.
const ADD_LINEAR: &str = "mask.add-linear";

/// What every masked gesture in this scenario commits, in EV, and the intermediate value the drag
/// passes through. A **darkening** rather than a lift, because the fixture holds a near-white patch
/// and a lift would clip it: a reading against the end of the range is produced by a broken render as
/// readily as by a correct one. One stop moves every patch here by at least 15 output codes.
const MASKED_EV: f64 = -1.0;
const PASSING_EV: f64 = -0.5;

/// The exposure of the **global** layer the scenario puts ahead of the masked one, in EV.
///
/// It is the study's own figure. A `+0.75 EV` lift takes the fixture's sky from `47.29` on the
/// luminance axis to `60.06`, which is past the band's own shoulder at `55`, so the band that selected
/// it fully selects it not at all — measured by `the_selection_follows_the_operations_input_not_the_
/// finished_frame` and reproduced here in the pixels of a render.
const GLOBAL_EV: f64 = 0.75;

/// The band this scenario types, on the histogram's own `0..100` axis.
///
/// It is placed on the fixture's sky, at `47.29`, with shoulders wide enough to be clean on noise
/// (the study measures five units as the narrowest shoulder that does not speckle; four is narrower
/// and is chosen here because the fixture is flat and the band has to exclude `foliage` at `39.81`
/// and `orange` at `57.98` to leave a legible frame). Every other patch of the fixture reads exactly
/// `0.0` under it and the sky and the grey card read exactly `1.0`.
const BAND_LOW: f64 = 44.0;
const BAND_HIGH: f64 = 51.0;
const BAND_FEATHER: f64 = 4.0;

/// The gradient that gives every reading a control: coverage `0` at `GRADIENT_FROM`, `1` at
/// `GRADIENT_TO`, so the fixture's top row is fully inside it and its bottom row fully outside. The
/// two sky patches are therefore the same colour with and without the mask, in the same frame.
const GRADIENT_FROM: [f64; 2] = [0.5, 0.75];
const GRADIENT_TO: [f64; 2] = [0.5, 0.25];

/// The brush the constrained-brush frames are drawn with, in mask-space units and `0..100`. A hard
/// edge, so a patch centre is either fully covered or not at all and the colour limit is the only
/// thing that can remove coverage from one.
const BRUSH_SIZE: f64 = 0.06;
const BRUSH_HARD: f64 = 0.0;

/// Half the side of a measured patch, in capture pixels. Every patch of the fixture is 360 × 320
/// source pixels and is read at its own centre, so this is far inside the nearest boundary at Fit.
const PATCH_HALF: i64 = 5;

/// How far a patch's mean luminance must move before this scenario calls it selected, and how close
/// two readings must stay before it calls a patch untouched. The smallest move any assertion below
/// relies on is the fixture's black patch under one stop, which is 15.8 codes, so `8.0` is a margin
/// and not a tuned threshold; `1.0` is renderer readback of a flat patch with no JPEG between, so the
/// untouched bound is tight on purpose.
const MOVED: f64 = 8.0;
const UNTOUCHED: f64 = 1.0;

/// The measured stroke's pacing and its path.
///
/// `24 ms` is a little over the delivered masked-drag median of `16.8` to `17.9 ms`, so each position
/// has a round trip of its own to complete and the frame it produced can reach the screen before the
/// next position is sent. A faster interval measures the driver's coalescing again, which is the thing
/// this measurement exists to avoid; a slower one measures nothing more.
///
/// The path is twelve positions down the middle of one flat patch, where the pixels are uniform, so
/// the figure is about the gesture and not about what the stroke happened to cross.
const STROKE_INTERVAL_MS: u64 = 24;
const STROKE_POSITIONS: usize = 12;

fn paced_path() -> Vec<[f64; 2]> {
    let [x, y] = at("grey-65");
    (0..STROKE_POSITIONS)
        .map(|index| {
            let t = index as f64 / (STROKE_POSITIONS - 1) as f64;
            [x, y - 0.08 + t * 0.16]
        })
        .collect()
}

/// How far down the tools panel the frame carrying the product's own statement is scrolled. The
/// Masks panel is longer than the window at this point — a mask, three components, a brush section
/// and the maskable modules' own sections — so the statement that sits above a band's numbers is
/// below the fold until the panel is scrolled to it, exactly as it is for a person.
const STATEMENT_SCROLL: f64 = 0.55;

/// Where each patch of the fixture is read, as a fraction of the photograph's own drawn rectangle:
/// the centre of its cell in the generator's own grid, so the probe and the fixture cannot disagree
/// about which patch is which.
fn probes() -> Vec<[f64; 2]> {
    (0..RANGE_PATCHES.len() as u32)
        .map(|index| {
            let (column, row) = (index % RANGE_COLUMNS, index / RANGE_COLUMNS);
            [
                (f64::from(column) + 0.5) / f64::from(RANGE_COLUMNS),
                (f64::from(row) + 0.5) / f64::from(RANGE_ROWS),
            ]
        })
        .collect()
}

/// One patch's position in the fixture, by the generator's own name. A name the fixture does not hold
/// is a programming error in this file and panics rather than reading the wrong patch.
fn at(name: &str) -> [f64; 2] {
    let index = RANGE_PATCHES
        .iter()
        .position(|(held, _)| *held == name)
        .unwrap_or_else(|| panic!("the range fixture holds no patch called {name}"));
    probes()[index]
}

/// One patch's centre in **output-stage pixels**, which is what a canvas pick takes. The fixture is
/// EXIF orientation 1 and this scenario applies no crop, so the output stage is the source's own size.
fn pick_at(name: &str) -> [u32; 2] {
    let (width, height) = crate::fixtures::RANGE_FIXTURE;
    let [fx, fy] = at(name);
    [
        (fx * f64::from(width)).round() as u32,
        (fy * f64::from(height)).round() as u32,
    ]
}

/// Mask mode, through the same `workspace.set` the mode strip sends. A pick is its own canvas mode
/// and it **latches**, exactly as the delivered neutral picker's does, so leaving it is a decision
/// and not a side effect of having clicked once: every pick is left this way too. A mode commits
/// nothing.
fn mask_mode(name: &str) -> Step {
    Step::new(name, WorkspaceStep::default().mode("mask")).commits(0)
}

/// One row of the open mask selected, which opens it: its number fields, its samples and, above
/// them, what its kind cannot do. A panel state, so nothing is committed.
fn select(name: &str, component: usize) -> Step {
    Step::new(
        name,
        MaskStep::SelectComponent(Some(Reference::Index(component))),
    )
    .commits(0)
}

/// One of the band's declared fields typed and submitted: its own entry.
fn typed(name: &str, parameter: &str, value: f64) -> Step {
    Step::new(
        name,
        script::Step::field(SET_LUMINANCE, parameter, value.to_string(), true),
    )
    .commits(1)
    .label("Update Luminance range 1")
}

/// One stop down through the open mask, as the panel's own drag, released: one entry.
fn masked_exposure(name: &str, label: &str) -> Step {
    Step::new(
        name,
        SliderStep::new(BASIC, EXPOSURE, [PASSING_EV, MASKED_EV]).release(),
    )
    .commits(1)
    .label(label)
}

/// The host's own pick entered on the open row. Nothing is committed until the click.
fn pick_entered(name: &str) -> Step {
    Step::new(name, MaskStep::Pick).commits(0)
}

/// One click on the named patch, in output-stage pixels. What it commits is the caller's: a new
/// colour is one entry and a colour the component already holds is none.
fn pick(name: &str, patch: &str) -> Step {
    let [x, y] = pick_at(patch);
    Step::new(name, script::Step::pick(x, y))
}

/// Launch 1: the band, the gradient, the failure, the remedy, the input dependence and the overlays.
pub fn launch1_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The fixture as launched, with nothing in the recipe: every reading is against this frame.
        Step::opened("opened").no_layer(BASIC_EFFECT),
        mask_mode("mask-mode"),
        // A new mask whose first component is a luminance range. A **typed** kind: every field of
        // its geometry carries a default, so the button creates it in one history entry rather than
        // opening a gesture with no shape to drag. It starts as the whole tonal range with soft
        // shoulders, which is the picture, and is narrowed from there.
        Step::new("band-created", MaskStep::New(LUMINANCE_RANGE.into()))
            .commits(1)
            .label("Add luminance range")
            .no_draft(),
        // The row open, which is what shows its number fields and, above them, the host's own
        // statement of what a band cannot separate.
        select("band-row", 0),
        // The band typed, one declared field at a time, each its own entry. `low` before `high`
        // because the payload refuses a crossed band rather than rendering an empty selection.
        typed("band-low", "low", BAND_LOW),
        typed("band-high", "high", BAND_HIGH),
        typed("band-low-feather", "low_feather", BAND_FEATHER),
        typed("band-typed", "high_feather", BAND_FEATHER),
        // A linear gradient intersected with the band, swept from the side it leaves alone towards
        // the side it selects, and committed. It is what gives the two sky patches their
        // difference: the top row is inside it, the bottom row outside. The sweep is a drafted
        // gesture holding the swept geometry, and the apply commits it as one entry.
        Step::new("gradient-mode", MaskStep::Mode("intersect".into())).commits(0),
        Step::new("gradient-added", MaskStep::Add(LINEAR.into())).commits(0),
        Step::new(
            "gradient-swept",
            MaskStep::Sweep {
                from: GRADIENT_FROM,
                to: GRADIENT_TO,
            },
        )
        .commits(0)
        .draft(
            ADD_LINEAR,
            json!({"mode":"intersect","x0":GRADIENT_FROM[0],"y0":GRADIENT_FROM[1],
                   "x1":GRADIENT_TO[0],"y1":GRADIENT_TO[1]}),
        ),
        Step::new("gradient-applied", MaskStep::Apply)
            .commits(1)
            .label("Add intersect linear")
            .no_draft(),
        // One stop down through the mask, as the panel's own drag. **This is the failure frame**:
        // the band was drawn for the sky and takes the grey card beside it, because the two are 1.4
        // output codes apart on the axis the band measures.
        masked_exposure("failure", "Mask 1 · Exposure -1.00 EV")
            .payload(BASIC_EFFECT, json!({ EXPOSURE: MASKED_EV })),
        // A colour range intersected with both. Created with no swatches, which selects nothing:
        // the picture goes back to the one the mask never touched, and that is what an unsampled
        // colour range means rather than a component that does nothing.
        Step::new("colour-mode", MaskStep::Mode("intersect".into())).commits(0),
        Step::new("colour-added", MaskStep::Add(COLOUR_RANGE.into()))
            .commits(1)
            .label("Add intersect colour range"),
        // Its row open, the host's own pick entered, and one click on the sky. **This is the
        // remedy frame**: the sky stays selected and the grey card comes back, which is the
        // component list doing the work the band cannot.
        select("colour-row", 2),
        pick_entered("sky-pick"),
        pick("remedy", "sky-top")
            .commits(1)
            .label("Sample Colour range 1"),
        mask_mode("sky-picked"),
        // A **global** exposure layer, from JSON with no mask in the request, which the host places
        // ahead of the masked one: the stack's first Basic layer is this one. **This is the
        // input-dependence frame**: the band reads the pixel its own operation receives, that pixel
        // is now 12.8 units further up the axis, and the sky is outside the band — so the masked
        // layer stops applying to it entirely.
        Step::new(
            "global",
            script::Step::call("edit.set-basic", json!({"exposure":GLOBAL_EV})),
        )
        .commits(1)
        .label("Exposure +0.75 EV")
        .payload(BASIC_EFFECT, json!({ EXPOSURE: GLOBAL_EV })),
        // Undone, and the selection comes back with the input it was drawn against. An undo moves
        // the revision on like any commit, to the entry the pick made, and the masked layer is the
        // stack's first Basic layer again.
        Step::new("global-undone", script::Step::api("history.undo"))
            .commits(1)
            .label("Sample Colour range 1")
            .payload(BASIC_EFFECT, json!({ EXPOSURE: MASKED_EV }))
            .same_layer(BASIC_EFFECT, "failure"),
        // The overlay on with the pointer off the list, which asks for the **composed** mask's
        // grid. This mask reads pixels, and a layer is bound to it — the masked Exposure of
        // `failure` — so the grid is read on that layer's own input and drawn. It is checked against
        // `global-undone`: the patches it calls selected are the patches that frame moved.
        Step::new(
            "overlay-composed",
            WorkspaceStep::default().mask_overlay("mask-on-black"),
        )
        .commits(0),
        // The pointer on the gradient's row, which asks for that one component's grid.
        Step::new(
            "overlay-gradient",
            MaskStep::Hover(Some(Reference::Index(1))),
        )
        .commits(0),
        // The pointer on each range component's row in turn. Each has its own grid now, and the pair
        // is the study's failure and its remedy as pictures: the band takes the grey card beside the
        // sky, and the picked colour range does not.
        Step::new("overlay-band", MaskStep::Hover(Some(Reference::Index(0)))).commits(0),
        Step::new("overlay-colour", MaskStep::Hover(Some(Reference::Index(2)))).commits(0),
        // The overlay off, leaving the photograph.
        Step::new("overlay-off", WorkspaceStep::default().mask_overlay("off")).commits(0),
        // The band's own row open and the tools panel scrolled to it, so the frame carries **the
        // product's own statement of what a band cannot separate** where a person reads it: above
        // the four numbers it applies to, on the row they belong to. The statement is checked in the
        // state on every frame that has the row open; this is the one that shows it.
        select("band-row-again", 0),
        Step::new("statement", script::Step::tools_scroll(STATEMENT_SCROLL)).commits(0),
    ])
}

/// Launch 2, over the same catalog: the colour range's own limits, and the colour-constrained brush.
pub fn launch2_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The catalog reopened in a new process, on the entry launch 1 ended on.
        Step::opened("reopened").label("Sample Colour range 1"),
        mask_mode("mask-mode"),
        // A second mask, one colour range, and an adjustment through it before anything is
        // sampled. The order is the host's rule and not a convenience: a pick reads the pixel the
        // operation this mask modulates receives, so a mask no layer is bound to is refused by name.
        // With no swatch the mask selects nothing, so this commits a layer and changes no pixel.
        Step::new("grey-mask", MaskStep::New(COLOUR_RANGE.into()))
            .commits(1)
            .label("Mask 2 · Add colour range"),
        masked_exposure("grey-mask-exposure", "Mask 2 · Exposure -1.00 EV"),
        // The row open, the pick entered, and one click on the **grey card**. Every neutral is one
        // colour to a metric with no lightness term — white, the two greys above it and black are
        // mutually within `0.0015`, a third of the tightest radius — so one sampled grey selects the
        // whole tonal range and the frame shows five patches move at once.
        select("grey-row", 0),
        pick_entered("grey-pick"),
        pick("neutral", "grey-card")
            .commits(1)
            .label("Mask 2 · Sample Colour range 1"),
        mask_mode("grey-picked"),
        // A second swatch, on the orange. A colour range folds its samples by nearest, which is the
        // same union the component list composes by, so the orange joins the selection and the
        // neutrals stay in it.
        pick_entered("orange-pick"),
        pick("orange", "orange")
            .commits(1)
            .label("Mask 2 · Sample Colour range 1"),
        mask_mode("orange-picked"),
        // A third swatch, on the grey card again. It is the colour the component already holds, so
        // it is exactly a no-op: the duplicate is dropped before it is stored, so no entry is
        // appended, and the fold is by nearest sample so no pixel's coverage changes. The frame is
        // the evidence that a picker misfire costs nothing.
        pick_entered("duplicate-pick"),
        pick("duplicate", "grey-card").commits(0),
        mask_mode("duplicate-picked"),
        // A third mask, its own adjustment, and one click on the **light skin** patch. Dark skin is
        // `0.0108` away in the frozen metric, a third of what one face's own shading spans, so any
        // setting that holds a lit face takes both — and the frame shows both move.
        Step::new("skin-mask", MaskStep::New(COLOUR_RANGE.into()))
            .commits(1)
            .label("Mask 3 · Add colour range"),
        masked_exposure("skin-mask-exposure", "Mask 3 · Exposure -1.00 EV"),
        select("skin-row", 0),
        pick_entered("skin-pick"),
        pick("skin", "light-skin")
            .commits(1)
            .label("Mask 3 · Sample Colour range 1"),
        mask_mode("skin-picked"),
        // The colour-constrained brush, in two halves. First an ordinary stroke across the boundary
        // between the bottom sky patch and the foliage beside it, and an adjustment through it: one
        // stroke, two surfaces, both selected. The stroke makes the mask and its first stroke in one
        // entry.
        Step::new(
            "brush",
            MaskStep::Brush(BrushStep {
                size: Some(BRUSH_SIZE),
                feather: Some(BRUSH_HARD),
                flow: Some(100.0),
                erase: Some(false),
                limit_to_colour: Some(false),
                ..BrushStep::default()
            }),
        )
        .commits(0),
        Step::new("brush-mask", MaskStep::Paint(PaintStep::NewMask)).commits(0),
        Step::new(
            "stroke",
            MaskStep::Stroke {
                points: vec![at("sky-bottom"), at("foliage")],
                release: true,
                interval_ms: None,
                settle_between: false,
            },
        )
        .commits(1)
        .label("Mask 4 · Add brush"),
        masked_exposure("painted", "Mask 4 · Exposure -1.00 EV"),
        // Then the same path erased back the other way, held to the colour under the brush where
        // the stroke begins — which is the foliage. The script sets a flag and never a colour: the
        // host reads the pixel the masked operation receives at the stroke's own first stored
        // position. **This is the constrained-brush frame**: the foliage comes out of the selection
        // and the sky the stroke also crossed stays in it, because the two are `0.116` apart in a
        // metric whose radius here is `0.035`.
        Step::new(
            "held-erase",
            MaskStep::Brush(BrushStep {
                erase: Some(true),
                limit_to_colour: Some(true),
                ..BrushStep::default()
            }),
        )
        .commits(0),
        Step::new(
            "held-erase-paint",
            MaskStep::Paint(PaintStep::Component(Reference::Index(0))),
        )
        .commits(0),
        Step::new(
            "constrained",
            MaskStep::Stroke {
                points: vec![at("foliage"), at("sky-bottom")],
                release: true,
                interval_ms: None,
                settle_between: false,
            },
        )
        .commits(1)
        .label("Mask 4 · Update Brush 1"),
        // The overlay on and off over that mask. **This is the frame P16 bought for a mask a person
        // painted**: a colour-held stroke makes a brush component read pixels, so before this there
        // was no grid for it at all and the only way to see what the erase had taken was to apply an
        // adjustment and look at the picture. The grid is checked against exactly that —
        // `constrained`'s own readings — and then the overlay is switched off and the photograph is
        // exactly where it was.
        Step::new(
            "held-overlay",
            WorkspaceStep::default().mask_overlay("mask-on-black"),
        )
        .commits(0),
        Step::new(
            "held-overlay-off",
            WorkspaceStep::default().mask_overlay("off"),
        )
        .commits(0),
        // Undone. The erase is one entry like any other stroke, so the foliage is selected again and
        // the current entry is the adjustment's once more.
        Step::new("held-erase-undone", script::Step::api("history.undo"))
            .commits(1)
            .label("Mask 4 · Exposure -1.00 EV"),
        // One **paced** stroke, which is the measurement rather than a claim about pixels. Every
        // stroke above sends its whole path in one update, which is what a fast drag does and what a
        // correctness reading wants; this one sends a position every `STROKE_INTERVAL_MS` in real
        // time, so each is its own input with its own round trip and its own drafted frame. That is
        // the only way an end-to-end figure for a paint gesture exists at all: `editor-latency`
        // drives field-patch sliders, and a stroke is a different gesture.
        //
        // `settle_between` is what TASK-018 added: on a heavily loaded host `STROKE_INTERVAL_MS`
        // alone is not enough — the render can take longer than the interval to reach the screen, so
        // every later position supersedes the frame before it and the stroke has nothing to pair a
        // latency to. Setting it holds each tick after the first until the position before it has
        // actually reached the screen (`Editor::paced_stroke_settled`), so the wall-clock cadence
        // stretches under load instead of the run losing every frame. `editor-latency --mode paint`
        // takes the same measurement without this field: it is a controlled, dedicated run rather
        // than a smoke scenario sharing the host, so `docs/design/masking.md` and
        // `docs/engineering/development.md` record the choice as scoped to this scenario.
        Step::new(
            "plain-brush",
            MaskStep::Brush(BrushStep {
                erase: Some(false),
                limit_to_colour: Some(false),
                ..BrushStep::default()
            }),
        )
        .commits(0),
        Step::new(
            "paced-paint",
            MaskStep::Paint(PaintStep::Component(Reference::Index(0))),
        )
        .commits(0),
        Step::new(
            "paced-stroke",
            MaskStep::Stroke {
                points: paced_path(),
                release: true,
                interval_ms: Some(STROKE_INTERVAL_MS),
                settle_between: true,
            },
        )
        .commits(1)
        .label("Mask 4 · Update Brush 1"),
        // Mask mode left, which returns the tools panel and leaves every selection where it is.
        Step::new("pointer-mode", WorkspaceStep::default().mode("pointer")).commits(0),
    ])
}

/// One component's own declared geometry, as the **open row's number fields** report it. It is what a
/// person reads rather than the payload behind it, which is why it is empty on a closed row.
fn fields(frame: &Frame, index: usize) -> Result<&Value> {
    Ok(&frame.component(index)?["fields"])
}

/// The colours one component has sampled, as the row lists them. A closed row lists none, exactly as
/// the panel draws it.
fn sample_count(frame: &Frame, index: usize) -> Result<usize> {
    Ok(frame.component(index)?["samples"]
        .as_array()
        .ok_or("The component records no sample list")?
        .len())
}

/// What the open row says this kind does **not** select. It is the host's own sentence from the kind
/// table, and a frame that records none for a range component is a frame whose product said nothing.
fn limits(frame: &Frame, index: usize) -> Result<Vec<String>> {
    Ok(frame.component(index)?["limits"]
        .as_array()
        .ok_or("The component records no limits")?
        .iter()
        .map(|line| line.as_str().unwrap_or_default().to_owned())
        .collect())
}

/// The colours one component has sampled, as the panel lists them on its row.
fn samples(frame: &Frame, index: usize) -> Result<Vec<String>> {
    Ok(frame.component(index)?["samples"]
        .as_array()
        .ok_or("The component records no sample list")?
        .iter()
        .map(|sample| sample["text"].as_str().unwrap_or_default().to_owned())
        .collect())
}

/// Every Basic layer of the stack, with the mask each is bound to, in the durable processing order.
fn basic_layers(frame: &Frame) -> Vec<(String, Value)> {
    frame["state"]["stack"]["layers"]
        .as_array()
        .map(|layers| {
            layers
                .iter()
                .filter(|layer| layer["effect"] == json!(BASIC_EFFECT))
                .map(|layer| {
                    (
                        layer["id"].as_str().unwrap_or_default().to_owned(),
                        layer["mask"].clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Where the photograph is drawn inside the capture.
///
/// Found on the frame the launch opened with and reused for every frame beside it, exactly as
/// `mask-brush` does: the zoom and the panels do not move between them, and a later frame has patches
/// this scenario has deliberately darkened below the threshold a bounds scan uses. The vertical
/// extent is taken first, for the reason [`Scan::Tallest`] records. The threshold is above the
/// brightest chrome on the canvas — the floating bars read 36 — and below the fixture's darkest
/// patch, which is the chart's black at 52.
const BOUNDS: Bright = Bright {
    threshold: 45,
    scan: Scan::Tallest { last_row: false },
    least: Some((200, 100)),
};

/// Every one of the fixture's twelve patches in one capture, by name.
fn read(frame: &Frame, bounds: [u32; 4]) -> Result<Vec<(String, f64)>> {
    let mut out = Vec::new();
    for ((name, _), at) in RANGE_PATCHES.iter().zip(probes()) {
        out.push((
            (*name).to_owned(),
            pixels::luminance_at(frame, bounds, at, PATCH_HALF)?,
        ));
    }
    Ok(out)
}

fn reading(read: &[(String, f64)], name: &str) -> Result<f64> {
    read.iter()
        .find(|(held, _)| held == name)
        .map(|(_, value)| *value)
        .ok_or_else(|| format!("No patch called {name} was read").into())
}

/// Exactly the patches named moved between two captures, and every other one is where it was. This is
/// what makes "a band takes a grey card as well as a sky" a claim about the whole photograph and not
/// about the two patches this scenario happened to look at.
fn only(
    what: &str,
    after: &[(String, f64)],
    before: &[(String, f64)],
    selected: &[&str],
) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for (name, value) in after {
        let was = reading(before, name)?;
        if (value - was).abs() >= MOVED {
            found.push(name.clone());
        }
        let tolerance = if selected.contains(&name.as_str()) {
            Tolerance::Apart(MOVED)
        } else {
            Tolerance::Within(UNTOUCHED)
        };
        compare(&format!("{what}: {name}"), *value, was, tolerance)?;
    }
    let mut expected: Vec<String> = selected.iter().map(|name| (*name).to_owned()).collect();
    expected.sort();
    let mut sorted = found.clone();
    sorted.sort();
    ensure(
        sorted == expected,
        format!("{what}: {sorted:?} moved, expected exactly {expected:?}"),
    )?;
    Ok(found)
}

/// The recipe the last frame of a launch was rendered from, as far as a timing figure needs it: how
/// many masks it holds, how many of them a layer is bound to, and how many of those hold a component
/// that reads pixels and therefore bounds the whole stage.
///
/// It is the shape of the work, not the shape of the panel: a masked layer whose mask reads pixels
/// costs an evaluation at every pixel of the frame rather than inside a rectangle, which is most of
/// what a latency figure taken here is about.
fn masked_recipe(launch: &Checked) -> Result<Value> {
    let last = launch.frames.last().ok_or("The launch wrote no frames")?;
    let masks = last.masks()?;
    let layers = last.state()["stack"]["layers"]
        .as_array()
        .ok_or("The last frame records no layer list")?;
    let masked: Vec<&Value> = layers
        .iter()
        .filter(|layer| layer["mask"] != Value::Null)
        .collect();
    Ok(json!({
        "masks": masks.len(),
        "masked_layers": masked.len(),
        "value_based_kinds": [LUMINANCE_RANGE, COLOUR_RANGE],
        "note": "a value-based component answers the whole stage for its conservative rectangle, so a masked layer holding one is evaluated at every pixel of the frame",
    }))
}

/// The steps the editor refused, as the run's own script records them. Neither plan expects a
/// refusal, so [`Plan::check`] has already held every step `sent` with no input error, and this is the
/// record of that rather than a check of it. Nothing is refused because the overlay of a mask that
/// reads pixels, and of each of its range components, reads the input of the mask's first bound
/// layer, which launch 1 binds when it drags the masked Exposure.
fn refused(launch: &Checked) -> Vec<Value> {
    launch.app["script"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, step)| step["status"] != json!("sent"))
        .map(|(index, step)| json!({"step":index,"reason":step["reason"]}))
        .collect()
}

/// Every answer the overlay got while the run was going, in order: a coverage grid that was painted
/// and uploaded, or one the host refused with its reason.
///
/// It is read from the events rather than from the frames because the grid is not part of the state
/// summary — what a frame carries is the photograph with the overlay drawn over it, which is what the
/// pixel readings below check. These are the host's own record of what each overlay request was
/// answered with, and for which component.
fn overlay_answers(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| match event["event"].as_str() {
            Some("mask_overlay") => Some(json!({"drawn":true,
                "component":event["detail"]["component"].clone(),
                "cells":event["detail"]["cells"].clone()})),
            Some("mask_overlay_absent") => Some(json!({"drawn":false,
                "reason":event["detail"]["detail"].clone()})),
            _ => None,
        })
        .collect()
}

/// Both launches, checked together once each has held to its plan: what the photograph, the panel
/// and the overlay show at each named step, and the paced stroke's own latency.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [launch1, launch2] = launches else {
        return Err(format!("Expected two launches, found {}", launches.len()).into());
    };
    let checks = verify_launch1(launch1)?;
    run.record("launch1", checks.clone());
    let limits = verify_launch2(launch2, &checks)?;
    run.record("launch2", limits.clone());
    // The stroke's own end-to-end latency, from the events of the launch that painted one, with the
    // recipe it was painted on and the load the host was under. It is a smoke-run figure over one
    // fixture and is labelled as such where it is recorded; the interval is the same one
    // `editor-latency` measures for a slider.
    let recipe = masked_recipe(launch2)?;
    let latency = stroke_latency(run.root(), &launch2.events, recipe)?;
    run.record("stroke_latency", latency.clone());
    write_json(
        &run.out().join("mask-range-checks.json"),
        &json!({"launch1": checks, "launch2": limits, "stroke_latency": latency}),
    )?;
    Ok(())
}

/// A painted stroke's own control-to-frame latency, paired from the run's events.
///
/// The pairing is exact rather than by order: every `mask_draft_set` is answered by one
/// `mask_draft_preview` carrying the preview generation it queued, and `preview_displayed` repeats
/// that generation. A gesture holds one round trip at a time, so a set with no answer between it and
/// the next one is a fault and not a measurement.
///
/// **TASK-018.** The stroke this pairs is painted with `settle_between: true` (see `verify`'s
/// `paced-stroke` step), so the desktop itself holds every position after the first until the one
/// before it has reached the screen: a heavily loaded host stretches the stroke's real time rather
/// than superseding every drafted frame before it can be paired. That leaves the emptiness check
/// below to catch a genuine fault — a stroke that never puts a single frame on screen at all — rather
/// than the load-only flakiness it used to catch, so it stays an `ensure` and not a provisional
/// reading: an empty pairing is a broken stroke, not a busy host.
///
/// **What this figure is not.** It is one stroke on one 1.4 MP fixture during a smoke run, not an
/// `editor-latency` baseline: that harness drives field-patch sliders over the 24 MP and 60 MP
/// fixtures and a stroke is a different gesture; its own `--mode paint` paced stroke does not set
/// `settle_between`, because it is a controlled, dedicated run rather than a smoke scenario sharing
/// the host with whatever else is running, so its measurement definition is left as `STROKE_INTERVAL_MS`
/// alone. This figure is also taken on the **heaviest** recipe this scenario builds — four masked
/// colour layers, three of them holding a component that reads pixels and therefore bounds the whole
/// stage — which is recorded beside it, because that is most of what the number is. The one-minute
/// load average is recorded too, since a figure taken above `8.0` is provisional by the repository's
/// own rule.
fn stroke_latency(root: &Path, events: &[Value], recipe: Value) -> Result<Value> {
    // The pairing itself lives in `editor_latency`, beside the `--mode paint` run that takes the same
    // measurement on a bare recipe at 24 and 60 MP, so the two figures are one definition and not
    // two implementations that could drift apart. Neither this call nor that function changed for
    // TASK-018: what changed is that the paced stroke itself no longer races the render pipeline, so
    // there is always at least one pair to find here unless the stroke is genuinely broken.
    let (queued, mut latencies) = editor_latency::paced_stroke_latencies(events)?;
    ensure(
        !latencies.is_empty(),
        "The run painted no stroke whose drafted frame reached the screen",
    )?;
    latencies.sort_by(f64::total_cmp);
    let percentile = |percent: usize| -> f64 {
        let rank = (latencies.len() * percent).div_ceil(100).max(1) - 1;
        latencies[rank.min(latencies.len() - 1)]
    };
    let load = crate::verify::load_average(root);
    Ok(json!({
        "inputs": queued,
        "displayed": latencies.len(),
        "p50_ms": percentile(50),
        "p95_ms": percentile(95),
        "max_ms": latencies.last().copied(),
        "samples_ms": latencies,
        "interval_ms": STROKE_INTERVAL_MS,
        "positions": STROKE_POSITIONS,
        "recipe": recipe,
        "load_average_1m": load,
        "load_threshold": launch::LOAD_THRESHOLD,
        "provisional": load.is_none_or(|load| load > launch::LOAD_THRESHOLD),
        "scope": "mask_draft_set to the preview_displayed of the generation it queued, on this scenario's 1440x960 fixture during a smoke run: the same interval editor-latency measures for a slider, over a different gesture. It is not a 24/60 MP baseline, and it is taken on the heaviest recipe this scenario builds, whose masked layers are recorded beside it — a value-based component bounds the whole stage, so those layers are evaluated over every pixel",
    }))
}

/// Launch 1, step by step: what the photograph, the panel and the overlay show. What each step
/// commits, its label, the Basic layers' payloads and the gestures' drafts are the plan's.
fn verify_launch1(launch: &Checked) -> Result<Value> {
    let start = launch.at("opened")?;
    let bounds = pixels::bright_bounds(start, BOUNDS)?;
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // The fixture as launched, with no mask in the recipe. Every reading below is against this one.
    ensure(
        start.masks()?.is_empty(),
        "The fixture opened with a mask already in the recipe",
    )?;
    let opened = read(start, bounds)?;
    // The two sky patches are the same colour, which is what makes every control comparison below a
    // comparison and not a prediction.
    compare(
        "the fixture's two sky patches",
        reading(&opened, "sky-top")?,
        reading(&opened, "sky-bottom")?,
        Tolerance::Within(UNTOUCHED),
    )?;
    record(
        start,
        "the range fixture as launched: twelve flat chart patches, no mask in the recipe",
        json!({"patches":opened.clone(),"bounds":bounds}),
    );

    // The band created by its own button. A typed kind: the plan holds it to one entry and no
    // draft, and here the mask's own gesture state is empty too; the whole tonal range with soft
    // shoulders, which selects the picture.
    let created = launch.at("band-created")?;
    ensure(
        created["state"]["mask_draft"] == Value::Null,
        "A typed kind opened a gesture",
    )?;
    ensure(
        created.kinds()? == ["add luminance-range"],
        format!("The button made {:?}", created.kinds()?),
    )?;
    // What it was created *as* is read from `band-row`, where the row is open: a closed row shows no
    // numbers, which is the panel's own behaviour and not an omission here.
    record(
        created,
        "a luminance range created by its button in one entry: the whole tonal range with soft \
         shoulders, which is the picture",
        json!({"label":created.label()?,"kinds":created.kinds()?}),
    );

    // The row open. This is where the product says what a band cannot do, before a person has typed
    // a number into it.
    let row = launch.at("band-row")?;
    let fresh = fields(row, 0)?.clone();
    ensure(
        fresh["low"] == json!(0.0) && fresh["high"] == json!(100.0),
        format!("A new band starts at {fresh}"),
    )?;
    let said = limits(row, 0)?;
    ensure(
        said.len() == 2
            && said[0].contains("output codes apart")
            && said[1].contains("the 100% view is the truth"),
        format!("The open row said {said:?}"),
    )?;
    record(
        row,
        "the band's own row, with the host's statement of what brightness alone cannot separate \
         above the numbers it applies to",
        json!({"limits":said.clone(),"fields":fresh}),
    );

    // The band typed, one field per entry — four entries, which the plan counts — and four numbers,
    // nothing dragged.
    let narrowed = launch.at("band-typed")?;
    let band = fields(narrowed, 0)?.clone();
    ensure(
        band == json!({"low":BAND_LOW,"low_feather":BAND_FEATHER,"high":BAND_HIGH,
                       "high_feather":BAND_FEATHER}),
        format!("The typed band reads {band}"),
    )?;
    record(
        narrowed,
        "the band narrowed onto the fixture's sky by its own declared fields, one entry each",
        json!({"fields":band.clone(),"label":narrowed.label()?}),
    );

    // The gradient committed, intersected with the band.
    let gradient = launch.at("gradient-applied")?;
    ensure(
        gradient.kinds()? == ["add luminance-range", "intersect linear"],
        format!("The mask holds {:?}", gradient.kinds()?),
    )?;
    ensure(
        limits(gradient, 1)?.is_empty(),
        "A gradient claimed a limit a position-based component does not have",
    )?;
    record(
        gradient,
        "a linear gradient intersected with the band, so the fixture's top row is inside the mask \
         and its bottom row outside it",
        json!({"kinds":gradient.kinds()?,"label":gradient.label()?,
               "limits":limits(gradient,1)?}),
    );

    // One stop down through the mask. **The failure.** The band was drawn for the sky and takes the
    // grey card beside it; the bottom sky patch, the same colour outside the gradient, does not move
    // at all.
    let failed = launch.at("failure")?;
    let failure = read(failed, bounds)?;
    let took = only(
        "a band drawn for the sky",
        &failure,
        &opened,
        &["sky-top", "grey-card"],
    )?;
    compare(
        "the selected sky against its own control outside the gradient",
        reading(&failure, "sky-top")?,
        reading(&failure, "sky-bottom")?,
        Tolerance::Apart(MOVED),
    )?;
    record(
        failed,
        "one stop down through the band: it takes the grey card as well as the sky it was drawn for, \
         because the two are 1.4 output codes apart on the axis a band measures",
        json!({"moved":took,"sky_top":reading(&failure,"sky-top")?,
               "grey_card":reading(&failure,"grey-card")?,
               "sky_bottom_control":reading(&failure,"sky-bottom")?,"patches":failure.clone()}),
    );

    // An unsampled colour range intersected in. It selects nothing, so the whole picture is back to
    // the one the mask never touched — which is what an empty swatch list means.
    let added = launch.at("colour-added")?;
    ensure(
        added.kinds()?
            == [
                "add luminance-range",
                "intersect linear",
                "intersect colour-range",
            ],
        format!("The mask holds {:?}", added.kinds()?),
    )?;
    ensure(
        sample_count(added, 2)? == 0,
        "A new colour range already holds a swatch",
    )?;
    let unsampled = read(added, bounds)?;
    only("an unsampled colour range", &unsampled, &opened, &[])?;
    record(
        added,
        "an unsampled colour range intersected in: it selects nothing, so the masked layer reaches \
         no pixel at all",
        json!({"kinds":added.kinds()?,"patches":unsampled.clone()}),
    );

    // One click on the sky. **The remedy.** The sky is selected again and the grey card is not,
    // which is the component list doing what the band cannot.
    let remedied = launch.at("remedy")?;
    let picked = samples(remedied, 2)?;
    ensure(
        picked.len() == 1,
        format!("The pick left {picked:?} on the row"),
    )?;
    let remedy = read(remedied, bounds)?;
    let held = only(
        "a colour range picked on the sky",
        &remedy,
        &opened,
        &["sky-top"],
    )?;
    compare(
        "the grey card once the colour range is intersected in",
        reading(&remedy, "grey-card")?,
        reading(&opened, "grey-card")?,
        Tolerance::Within(UNTOUCHED),
    )?;
    record(
        remedied,
        "the sky sampled off the photograph: the selection is the sky alone and the grey card is back \
         where it started",
        json!({"moved":held,"samples":picked.clone(),
               "sky_top":reading(&remedy,"sky-top")?,
               "grey_card":reading(&remedy,"grey-card")?,"patches":remedy.clone()}),
    );

    // A **global** exposure layer, placed ahead of the masked one. The band reads the pixel its own
    // operation receives, that pixel has moved 12.8 units up the axis, and the sky is outside the
    // band: the masked layer stops reaching it. Read as an equality against the control patch of the
    // identical colour, so nothing here is a predicted output code.
    let global = launch.at("global")?;
    let layers = basic_layers(global);
    ensure(
        layers.len() == 2 && layers[0].1 == Value::Null && layers[1].1 != Value::Null,
        format!("The stack holds Basic layers {layers:?}"),
    )?;
    let reordered = read(global, bounds)?;
    compare(
        "the sky under a +0.75 EV layer ahead of the band",
        reading(&reordered, "sky-top")?,
        reading(&reordered, "sky-bottom")?,
        Tolerance::Within(UNTOUCHED),
    )?;
    compare(
        "the sky itself under the global layer",
        reading(&reordered, "sky-top")?,
        reading(&remedy, "sky-top")?,
        Tolerance::Apart(MOVED),
    )?;
    record(
        global,
        "a +0.75 EV layer ahead of the masked one: the band no longer selects the sky at all, so the \
         selected patch and its unselected control of the same colour read the same",
        json!({"layers":layers,"sky_top":reading(&reordered,"sky-top")?,
               "sky_bottom_control":reading(&reordered,"sky-bottom")?,
               "patches":reordered.clone()}),
    );

    // Undone. The selection is back with the input it was drawn against, byte for byte the picture
    // the pick produced.
    let restored = launch.at("global-undone")?;
    let undone = read(restored, bounds)?;
    for (name, value) in &undone {
        compare(
            &format!("{name} after the global layer was undone"),
            *value,
            reading(&remedy, name)?,
            Tolerance::Within(UNTOUCHED),
        )?;
    }
    record(
        restored,
        "the global layer undone: the selection reads what it read before, because its input does",
        json!({"patches":undone.clone(),"label":restored.label()?}),
    );

    // The four answers the overlay got, in order: the composed mask and then each of the three
    // components in turn, every one of them a grid painted and uploaded. Two of those three read the
    // pixel their operation receives, and they are answered because a layer is bound to this mask.
    let answers = overlay_answers(&launch.events);
    ensure(
        answers.len() == 4 && answers.iter().all(|answer| answer["drawn"] == json!(true)),
        format!("The overlay answered {}", json!(answers)),
    )?;
    for (answer, (step, row)) in answers[1..].iter().zip([
        ("overlay-gradient", 1),
        ("overlay-band", 0),
        ("overlay-colour", 2),
    ]) {
        let hovered = launch.at(step)?;
        ensure(
            answer["component"] == hovered.component(row)?["id"],
            format!(
                "The grid for step {step:?} names {}, and row {row} is {}",
                answer["component"],
                hovered.component(row)?["id"]
            ),
        )?;
    }

    // The **composed** mask's own grid, drawn on black, read in the pixels. This is the frame P16
    // exists for, and what it is checked against is the photograph's own measured change: the
    // composition is a band intersected with a gradient intersected with a picked colour range, so a
    // patch it covers is exactly a patch the masked Exposure moved in `global-undone`, and a patch it
    // does not cover is exactly a patch that frame left alone. The overlay and the render are
    // therefore compared against each other in a person's own two frames, not against a number this
    // scenario predicted.
    let overlaid = launch.at("overlay-composed")?;
    let composed = read(overlaid, bounds)?;
    for (name, value) in &composed {
        let lifted = (reading(&undone, name)? - reading(&opened, name)?).abs();
        let selected = lifted >= MOVED;
        ensure(
            selected == (*value >= 128.0),
            format!(
                "the composed mask's overlay: {name} read {value:.1} of coverage where the \
                 photograph moved by {lifted:.2} under the same mask"
            ),
        )?;
    }
    record(
        overlaid,
        "the composed mask's own coverage, drawn on black: every patch the overlay calls selected is \
         a patch the masked Exposure moved in the frame before it, and every patch it calls \
         unselected is one that frame left alone — the overlay is the selection the render makes",
        json!({"patches":composed.clone(),"moved_against":undone.clone()}),
    );

    // The gradient's own row. A position-based component's grid has not changed: the fixture's top
    // row is inside the gradient and reads white, its bottom row is outside and reads black, and the
    // row between them is on the ramp and reads between the two.
    let ramp = launch.at("overlay-gradient")?;
    let drawn = read(ramp, bounds)?;
    ensure(
        ramp.component(1)?["hovered"] == json!(true),
        "The pointer was not on the gradient's row",
    )?;
    for (index, (name, value)) in drawn.iter().enumerate() {
        let row = index as u32 / RANGE_COLUMNS;
        let holds = match row {
            0 => *value >= 200.0,
            1 => (100.0..=155.0).contains(value),
            _ => *value <= 40.0,
        };
        ensure(
            holds,
            format!(
                "the gradient's own overlay: {name} in row {row} read {value:.1}, which is not the \
                 coverage a gradient from 0.75 to 0.25 of the height has there"
            ),
        )?;
    }
    record(
        ramp,
        "the gradient's own contribution, drawn on black: full over the top row, on its ramp in the \
         middle and nothing over the bottom row",
        json!({"patches":drawn.clone(),
               "hovered":ramp.component(1)?["hovered"].clone()}),
    );

    // Each range component's own row, each with a grid of its own now. A component's grid is that
    // component alone, so neither is bounded by the gradient: the band takes both sky patches and the
    // grey card beside them — which is this study's `1.4` output codes, drawn — and the colour range
    // picked on the sky takes the sky and leaves the grey card. That pair is the failure and its
    // remedy, in the overlay, where before they could only be read off the picture.
    let band_row = launch.at("overlay-band")?;
    let colour_row = launch.at("overlay-colour")?;
    let band = read(band_row, bounds)?;
    let picked = read(colour_row, bounds)?;
    for (what, grid) in [("the band's", &band), ("the colour range's", &picked)] {
        ensure(
            reading(grid, "sky-top")? >= 200.0 && reading(grid, "sky-bottom")? >= 200.0,
            format!(
                "{what} own overlay left a sky patch out: {:.1} and {:.1}",
                reading(grid, "sky-top")?,
                reading(grid, "sky-bottom")?
            ),
        )?;
    }
    ensure(
        reading(&band, "grey-card")? >= 200.0,
        format!(
            "the band's own overlay does not show it taking the grey card: {:.1}",
            reading(&band, "grey-card")?
        ),
    )?;
    ensure(
        reading(&picked, "grey-card")? <= 40.0,
        format!(
            "the picked colour range's own overlay still takes the grey card: {:.1}",
            reading(&picked, "grey-card")?
        ),
    )?;
    record(
        band_row,
        "the band's own contribution, drawn on black: brightness alone takes the grey card along \
         with the sky, which is this study's 1.4 output codes shown rather than described",
        json!({"patches":band.clone()}),
    );
    record(
        colour_row,
        "the picked colour range's own contribution, drawn on black: the sky stays and the grey card \
         is gone, which is the remedy the component list is for",
        json!({"patches":picked.clone()}),
    );

    // The statement itself, on screen. The row is open and the panel is scrolled to it, so what a
    // person reads before typing a number into a band is in a capture and not only in a model.
    let statement = launch.at("statement")?;
    ensure(
        limits(statement, 0)? == said,
        format!("The row on screen says {:?}", limits(statement, 0)?),
    )?;
    record(
        statement,
        "the product's own statement of what brightness alone cannot separate, on the band's row and \
         above the four numbers it applies to",
        json!({"limits":limits(statement,0)?,"fields":fields(statement,0)?.clone(),
               "scroll":statement["state"]["tools_scroll"].clone()}),
    );

    let off = launch.at("overlay-off")?;
    Ok(json!({
        "kinds": off.kinds()?,
        "opened": opened,
        "failure": failure,
        "remedy": remedy,
        "revision": off.revision()?,
        "refusals": refused(launch),
        "overlay_answers": answers,
        "limits": said,
        "moved_threshold": MOVED,
        "untouched_threshold": UNTOUCHED,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of the twelve fixture patches in the displayed photograph, read back from the renderer with no JPEG between; every claim about what a selection stopped selecting is an equality against a control patch of the same colour in the same frame, and none is a colorimetric claim",
    }))
}

/// Launch 2, step by step: the colour range's own limits, and the colour-constrained brush.
fn verify_launch2(launch: &Checked, launch1: &Value) -> Result<Value> {
    let start = launch.at("reopened")?;
    let bounds = pixels::bright_bounds(start, BOUNDS)?;
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // The reopened catalog. Launch 1's mask and its selection are back, in the pixels and not only in
    // the rows.
    let reopened = read(start, bounds)?;
    let remedy: Vec<(String, f64)> = serde_json::from_value(launch1["remedy"].clone())?;
    for (name, value) in &reopened {
        compare(
            &format!("{name} after the catalog was reopened"),
            *value,
            reading(&remedy, name)?,
            Tolerance::Within(UNTOUCHED),
        )?;
    }
    record(
        start,
        "the catalog reopened in a new process: the band, the gradient and the picked colour range \
         produce the same photograph they produced before it closed",
        json!({"patches":reopened.clone(),"masks":start.masks()?.len()}),
    );

    // A sampled grey. **Every neutral is one colour** to a metric with no lightness term, so one
    // click selects the whole tonal range and five patches move at once.
    let grey = launch.at("neutral")?;
    let neutral = read(grey, bounds)?;
    let neutrals = only(
        "a colour range sampled on a grey card",
        &neutral,
        &reopened,
        &["grey-card", "white", "grey-65", "black", "grey-8"],
    )?;
    record(
        grey,
        "one click on the grey card: white, both other greys and black move with it, because every \
         neutral is within 0.0015 of every other in a metric with no lightness term",
        json!({"moved":neutrals,"samples":samples(grey,0)?,"patches":neutral.clone()}),
    );

    // A second swatch. The fold is by nearest sample, which is the same union the component list
    // composes by, so the orange joins and the neutrals stay.
    let orange = launch.at("orange")?;
    let two = read(orange, bounds)?;
    only("a second swatch on the orange", &two, &neutral, &["orange"])?;
    ensure(
        samples(orange, 0)?.len() == 2,
        format!("The row lists {:?}", samples(orange, 0)?),
    )?;
    record(
        orange,
        "a second swatch: the orange joins the selection and every neutral stays in it",
        json!({"samples":samples(orange,0)?,"patches":two.clone()}),
    );

    // The same patch sampled again. The duplicate is dropped **before it is stored**, so the row keeps
    // its two swatches, no entry is appended — the plan holds the pick to no commit — and no pixel
    // moves. That is stronger than storing a second copy that happened to change nothing: one of the
    // component's five swatches is not spent on a colour it already holds.
    let duplicate = launch.at("duplicate")?;
    let three = read(duplicate, bounds)?;
    only("a duplicate swatch", &three, &two, &[])?;
    ensure(
        samples(duplicate, 0)? == samples(orange, 0)?,
        format!(
            "The row lists {:?}, and it listed {:?}",
            samples(duplicate, 0)?,
            samples(orange, 0)?
        ),
    )?;
    record(
        duplicate,
        "a third click on a colour the component already holds: the same two swatches, no history \
         entry and not one pixel different, so a misfired picker costs nothing and spends no swatch",
        json!({"samples":samples(duplicate,0)?,"revision":duplicate.revision()?,
               "patches":three.clone()}),
    );

    // One person's skin. **Two people's skin is one colour**: dark skin is 0.0108 from light skin, a
    // third of what one face's own shading spans, so any setting that holds a lit face takes both.
    let lit = launch.at("skin")?;
    let skin = read(lit, bounds)?;
    let both = only(
        "a colour range sampled on light skin",
        &skin,
        &three,
        &["light-skin", "dark-skin"],
    )?;
    record(
        lit,
        "one click on the light skin patch: the dark skin patch moves with it, because the two are \
         0.0108 apart in a metric whose radius here is 0.035",
        json!({"moved":both,"samples":samples(lit,0)?,"patches":skin.clone()}),
    );

    // One ordinary stroke across two surfaces, with an adjustment through it. Both move, which is
    // what a brush without a colour limit does.
    let stroked = launch.at("painted")?;
    let painted = read(stroked, bounds)?;
    only(
        "an unlimited stroke across a sky and a foliage patch",
        &painted,
        &skin,
        &["sky-bottom", "foliage"],
    )?;
    record(
        stroked,
        "one stroke across the boundary between two surfaces: a brush selects where it is drawn, so \
         both of them",
        json!({"patches":painted.clone(),"kinds":stroked.kinds()?}),
    );

    // The same path erased back, held to the colour under the brush where the stroke began. **The
    // constrained brush.** The foliage it was seeded on comes out of the selection; the sky the
    // stroke crossed just as far stays in it.
    let erased = launch.at("constrained")?;
    let constrained = read(erased, bounds)?;
    only(
        "a colour-held erase seeded on the foliage",
        &constrained,
        &painted,
        &["foliage"],
    )?;
    compare(
        "the sky the held erase also crossed",
        reading(&constrained, "sky-bottom")?,
        reading(&painted, "sky-bottom")?,
        Tolerance::Within(UNTOUCHED),
    )?;
    compare(
        "the foliage after the held erase against its own unmasked reading",
        reading(&constrained, "foliage")?,
        reading(&reopened, "foliage")?,
        Tolerance::Within(UNTOUCHED),
    )?;
    record(
        erased,
        "a colour-held erase along the same path: the foliage it was seeded on leaves the selection \
         and the sky the stroke crossed just as far stays in it",
        json!({"patches":constrained.clone(),"strokes":erased.component(0)?["strokes"].clone(),
               "foliage":reading(&constrained,"foliage")?,
               "sky_bottom":reading(&constrained,"sky-bottom")?}),
    );

    // **The overlay of a mask a person painted and held to a colour.** A colour-held stroke makes a
    // brush component read the pixel its operation receives, so before P16 was built this mask had
    // no grid at all and the only way to see what the erase had taken was to apply an adjustment and
    // look at the picture. The grid is checked against exactly that: every patch the overlay calls
    // selected is a patch `constrained` moved, and every patch it calls unselected is one that frame
    // left where the unmasked picture had it. Then the overlay goes off and the photograph is exactly
    // where it was, which is the view-setting contract.
    let overlaid = launch.at("held-overlay")?;
    let held = read(overlaid, bounds)?;
    for (name, value) in &held {
        // `skin` is the last frame before this mask existed, so the difference between it and
        // `constrained` is this mask's own layer and nothing else — which is exactly what its grid
        // describes.
        let lifted = (reading(&constrained, name)? - reading(&skin, name)?).abs();
        ensure(
            (lifted >= MOVED) == (*value >= 128.0),
            format!(
                "the colour-held brush mask's overlay: {name} read {value:.1} of coverage where the \
                 photograph moved by {lifted:.2} under the same mask"
            ),
        )?;
    }
    ensure(
        reading(&held, "sky-bottom")? >= 200.0 && reading(&held, "foliage")? <= 40.0,
        format!(
            "the held erase in the overlay: the sky read {:.1} and the foliage {:.1}",
            reading(&held, "sky-bottom")?,
            reading(&held, "foliage")?
        ),
    )?;
    record(
        overlaid,
        "what a colour-held erase took, drawn rather than inferred: the sky the stroke crossed is \
         still selected and the foliage it was seeded on is gone, and every patch of the grid agrees \
         with what the masked adjustment did to that patch in the frame before it",
        json!({"patches":held.clone(),"moved_against":constrained.clone()}),
    );
    let cleared = launch.at("held-overlay-off")?;
    let off = read(cleared, bounds)?;
    for (name, value) in &off {
        compare(
            &format!("{name} with the overlay switched off again"),
            *value,
            reading(&constrained, name)?,
            Tolerance::Within(UNTOUCHED),
        )?;
    }
    record(
        cleared,
        "the overlay off again: a view setting, so the photograph is exactly where it was",
        json!({"patches":off.clone()}),
    );

    // Undone. The held erase is one entry like every other stroke.
    let restored = launch.at("held-erase-undone")?;
    let undone = read(restored, bounds)?;
    only("the held erase undone", &undone, &constrained, &["foliage"])?;
    record(
        restored,
        "the held erase undone: one stroke is one entry whether or not it was held to a colour",
        json!({"patches":undone.clone(),"label":restored.label()?}),
    );

    Ok(json!({
        "reopened": reopened,
        "neutral": neutral,
        "skin": skin,
        "constrained": constrained,
        "revision": launch.at("plain-brush")?.revision()?,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of the twelve fixture patches in the displayed photograph, read back from the renderer; every comparison is against the frame before it in the same launch, and none is a colorimetric claim",
    }))
}

/// TASK-018's own regression coverage over synthetic events: `stroke_latency` must still pass a
/// stroke every one of whose positions reached the screen, and must still fail one that reached it
/// for none of them, which is the "broken stroke" the acceptance criteria keep as a real failure
/// rather than the load-only flakiness the `settle_between` fix removed.
#[cfg(test)]
mod tests {
    use super::*;

    /// One `mask_draft_set`/`mask_draft_preview`/`preview_displayed` triple per `(set_ms,
    /// preview_ms, displayed_ms, generation)`, the same shape `paced_stroke_latencies` reads out of
    /// a run's own `events.jsonl`.
    fn paired(set_ms: f64, preview_ms: f64, displayed_ms: f64, generation: u64) -> Vec<Value> {
        vec![
            json!({"event":"mask_draft_set","elapsed_ms":set_ms}),
            json!({"event":"mask_draft_preview","elapsed_ms":preview_ms,"detail":{"generation":generation}}),
            json!({"event":"preview_displayed","elapsed_ms":displayed_ms,"detail":{"generation":generation}}),
        ]
    }

    /// A `mask_draft_set`/`mask_draft_preview` pair with no `preview_displayed` at all: the
    /// generation queued a preview job that never reached the screen, exactly what a heavily loaded
    /// host used to do to every position before TASK-018.
    fn superseded(set_ms: f64, preview_ms: f64, generation: u64) -> Vec<Value> {
        vec![
            json!({"event":"mask_draft_set","elapsed_ms":set_ms}),
            json!({"event":"mask_draft_preview","elapsed_ms":preview_ms,"detail":{"generation":generation}}),
        ]
    }

    #[test]
    fn a_settled_stroke_pairs_every_position_and_passes() {
        // Three positions, each settled before the next was sent (as `settle_between` now
        // guarantees), so every one of them has its own displayed frame to pair a latency to.
        let mut events = Vec::new();
        events.extend(paired(0.0, 4.0, 28.0, 1));
        events.extend(paired(28.0, 32.0, 56.0, 2));
        events.extend(paired(56.0, 60.0, 84.0, 3));
        let root = tempfile::tempdir().expect("a temp scenario root");

        let latency =
            stroke_latency(root.path(), &events, json!({})).expect("a settled stroke pairs");

        assert_eq!(latency["inputs"], json!(3));
        assert_eq!(latency["displayed"], json!(3));
        assert_eq!(
            latency["samples_ms"].as_array().expect("sample list").len(),
            3
        );
    }

    #[test]
    fn a_stroke_that_never_reaches_the_screen_still_fails() {
        // Every position queues a preview job, exactly as a settled one does, but none of their
        // generations is ever displayed: a genuinely broken stroke, not a busy host, and the
        // scenario's own guard must still refuse to call it a measurement.
        let mut events = Vec::new();
        events.extend(superseded(0.0, 4.0, 1));
        events.extend(superseded(24.0, 28.0, 2));
        events.extend(superseded(48.0, 52.0, 3));
        let root = tempfile::tempdir().expect("a temp scenario root");

        let error = stroke_latency(root.path(), &events, json!({}))
            .expect_err("a stroke with no displayed frame is not a measurement");

        assert_eq!(
            error.to_string(),
            "The run painted no stroke whose drafted frame reached the screen"
        );
    }
}
