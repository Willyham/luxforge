//! The `mask-range` smoke scenario: phase D's claim, which is that a deterministic per-pixel
//! selection is worth having **and** that what it cannot do is worth saying.
//!
//! Two launches over one catalog. The first creates a luminance range, which selects its row, arms
//! the eyedropper and shows Tint with no adjustment bound to the mask, and clicks the sky to set a
//! band around its tone; types the band's edges; intersects a gradient with it, which puts the
//! eyedropper down; applies an adjustment through the result and shows the band taking a grey card
//! along with the sky it was drawn for; then intersects a colour range, clicks the sky, ends
//! picking with Return and shows the grey card come back; then puts a `+0.75 EV` layer **ahead** of
//! the masked one and shows the band stop selecting the sky altogether; then asks the composed mask
//! and each component for its overlay, where all four grids are drawn and the two range components'
//! own grids show the failure and its remedy as pictures. The second reopens that catalog and takes
//! the colour range's own limits one at a time, each picked **before** any adjustment is bound and
//! read after one is dragged with the eyedropper still armed — a click on a grey selecting every
//! neutral, a Shift-click adding a colour, a plain click replacing the swatches, and a drag over
//! one person's skin selecting another's — then Escape, Pick and Return on the eyedropper, and
//! finishes with the colour-constrained brush: one stroke across two surfaces, and a colour-held
//! erase that takes one of them back out and leaves the other alone — read from the picture and
//! then from that mask's own overlay, which a colour-held stroke has too.
//!
//! **Why the readings are taken from the photograph as well as from the overlay.** Every claim this
//! scenario makes about what a selection selects is read from the rendered photograph with an
//! adjustment applied through the mask, because that is the picture a person is actually editing. The
//! overlay is read too, and read **against** those same frames: a mask that reads pixels is
//! answered on the input of its first bound layer ([proposal
//! P16](../../docs/design/range-study.md#proposals)), so the composed mask's grid and the frame the
//! masked Exposure produced are two views of one selection and are checked patch by patch against
//! each other. The overlay at Fit is still read on a downscaled pixel and the 100% view is still
//! the truth; what the grid buys is that a selection can be *seen* rather than only inferred.
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
//! which is what `render.locate` takes and what the canvas publishes, and is a press and a release of
//! the armed eyedropper: a click, a click with Shift, or a drag to a second point. Arming the
//! eyedropper shows Tint over a stored Off unless Off was chosen, so every frame that reads the
//! photograph follows an explicit Off.
//!
//! Each launch is a [`Plan`]: every frame it captures, named by the step that produces it, with what
//! that step commits and records. [`verify`] reads the frames by those names for everything a plan
//! cannot say — above all what the photograph and its overlay show.
use crate::{
    fixtures::{RANGE_COLUMNS, RANGE_PATCHES, RANGE_ROWS},
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance},
    *,
};
use luxforge_core::BASIC_EFFECT;
use luxforge_evidence::{
    self as script, BrushStep, MaskStep, PaintStep, Reference, SliderStep, WorkspaceStep,
};

pub const SCENARIO: &str = "mask-range";
/// The range fixture: twelve flat patches of the 24-patch chart's own sRGB renderings, laid out so
/// each of the study's measured failures is legible in one frame. `cargo xtask generate-fixtures`.
pub const FIXTURE: &str = "fixtures/generated/range.jpg";

/// The paragraph `reproduce.md` gives this scenario.
pub const NOTE: &str = "Generate the fixture first with `cargo xtask generate-fixtures --output fixtures/generated`.\n\nTwo launches over one catalog: the first creates a luminance range (selected, the eyedropper armed, Tint, no adjustment bound), clicks the sky to set a band, types the band, intersects a gradient and a clicked colour range with it, shows a grey card taken along with the sky and then given back, shows a layer ahead of the mask stop the selection altogether, and asks the composed mask and each component for its coverage overlay, checking the composition's grid patch by patch against the frame the masked adjustment produced; the second picks colour ranges with a click, a Shift-click, a replacing click and a drag before and between adjustments, ends picking with Escape and Return, and finishes with a colour-held erase across two surfaces, read from the picture and then from that painted mask's own overlay.\n\nThe patches are the 24-patch reflective colour chart's own sRGB renderings, which is what `docs/design/range-study.md` measured over.";

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
/// It is the study's own figure. A pure `+0.75 EV` lift takes the fixture's sky from `47.29` on the
/// luminance axis to `60.06`, which is past the band's own shoulder at `55`, so the band that selected
/// it fully selects it not at all — measured by `the_selection_follows_the_operations_input_not_the_
/// finished_frame` and reproduced here in the pixels of a render. Basic's exposure follows
/// Lightroom's response rather than a pure lift, and takes the sky further past the shoulder, which
/// the band's own input read (`global-area`) shows.
const GLOBAL_EV: f64 = 0.75;

/// The fixture's sky on the band's own `0..100` axis, which a click on it sets the band around, and
/// how far from the click's ten-unit span either edge may read: the sky is flat, so this is the
/// sampled tone's own error and not a tuned margin.
const SKY_TONE: f64 = 47.29;
const TONE_TOLERANCE: f64 = 2.0;

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

/// Mask mode, through the same `workspace.set` the mode strip sends. A mode commits nothing.
fn mask_mode(name: &str) -> Step {
    Step::new(name, WorkspaceStep::default().mode("mask"))
        .commits(0)
        .mode("mask")
}

/// The coverage overlay set to `setting`, which commits nothing.
fn overlay(name: &str, setting: &str) -> Step {
    Step::new(name, WorkspaceStep::default().mask_overlay(setting))
        .commits(0)
        .workspace("mask_overlay", json!(setting))
}

/// Launch 1's mask as it grows: the band, the gradient intersected with it, then the colour range.
const BANDED: [&str; 3] = [
    "add luminance-range",
    "intersect linear",
    "intersect colour-range",
];

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

/// The luminance and colour range kinds are an experiment that is off by default, so another
/// client of the session turns it on, as a script's `flags.set` does, before a range is offered.
/// The desktop reads the flag from the event that follows and offers both kinds from then on.
pub fn range_experiment(name: &str) -> Step {
    Step::new(
        name,
        script::Step::agent(
            "flags.set",
            json!({"flag": luxforge_core::flags::RANGE_MASKS, "value": true}),
        ),
    )
    .commits(0)
}

/// One click on the named patch, in output-stage pixels, with the eyedropper armed. What it commits
/// is the caller's: a luminance click or a colour click that changes the component is one entry.
/// A colour click **replaces** the swatches the component holds.
fn pick(name: &str, patch: &str) -> Step {
    let [x, y] = pick_at(patch);
    Step::new(name, script::Step::pick(x, y))
}

/// A click with Shift held on the named patch, which adds a colour to the swatches held.
fn pick_added(name: &str, patch: &str) -> Step {
    let [x, y] = pick_at(patch);
    Step::new(name, script::Step::pick_shift(x, y))
}

/// A drag over a square of `PICK_AREA` pixels each way around the named patch's centre, which picks
/// the area. Well inside the patch, so the area holds one colour and one tone.
fn pick_area(name: &str, patch: &str) -> Step {
    let [x, y] = pick_at(patch);
    Step::new(
        name,
        script::Step::pick_drag(
            [x - PICK_AREA, y - PICK_AREA],
            [x + PICK_AREA, y + PICK_AREA],
            false,
        ),
    )
}

/// `mask.sample-input-area` of Mask 1 over the middle half of the named patch, each way: the input
/// its masked operation receives there, which names the renderer that read it. A query, so nothing
/// is committed.
fn area_read(name: &str, patch: &str) -> Step {
    let [x, y] = at(patch);
    let (half_x, half_y) = (
        0.25 / f64::from(RANGE_COLUMNS),
        0.25 / f64::from(RANGE_ROWS),
    );
    Step::new(
        name,
        script::Step::call(
            "mask.sample-input-area",
            json!({"mask": {"name": "Mask 1"}, "x": x - half_x, "y": y - half_y,
                   "width": 2.0 * half_x, "height": 2.0 * half_y}),
        ),
    )
    .commits(0)
}

/// Half the side of a dragged pick, in output-stage pixels: a patch is 360 by 320 of them.
const PICK_AREA: u32 = 60;
/// A patch's shorter side, in output pixels: the dragged square stays within its middle half.
const _: () = assert!(PICK_AREA * 2 <= 320 / 2);

/// The key that ends picking as the pick bar's Done does.
const KEY_DONE: &str = "Return";

/// The eyedropper put down by Escape, which commits nothing and keeps Mask mode.
fn escape(name: &str) -> Step {
    Step::new(name, script::Step::key(script::KEY_ESCAPE))
        .commits(0)
        .mode("mask")
        .picker(None)
}

/// Launch 1: the band, the gradient, the failure, the remedy, the input dependence and the overlays.
pub fn launch1_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        // The fixture as launched, with nothing in the recipe: every reading is against this frame.
        Step::opened("opened").no_layer(BASIC_EFFECT).masks(0),
        range_experiment("range-experiment"),
        mask_mode("mask-mode"),
        // A new mask whose first component is a luminance range. A **typed** kind: every field of
        // its geometry carries a default, so the button creates it in one history entry rather than
        // opening a gesture with no shape to drag. It starts as the whole tonal range with soft
        // shoulders. The new row is selected and the eyedropper armed, with no adjustment bound to
        // the mask: its selection is read at the stage a first Basic adjustment will receive.
        Step::new("band-created", MaskStep::New(LUMINANCE_RANGE.into()))
            .commits(1)
            .label("Add luminance range")
            .no_draft()
            .masks(1)
            .components(&BANDED[..1])
            .selected_component(Some("Luminance range 1"))
            .picker(Some(LUMINANCE_RANGE)),
        // The pointer on the new row asks for its own coverage grid, which is drawn as the picker's
        // Tint over a stored overlay of Off: a grid read for a mask no layer is bound to.
        Step::new("band-tint", MaskStep::Hover(Some(Reference::Index(0))))
            .commits(0)
            .picker(Some(LUMINANCE_RANGE)),
        Step::new("band-tint-off-list", MaskStep::Hover(None))
            .commits(0)
            .picker(Some(LUMINANCE_RANGE)),
        // Arming shows Tint unless Off was chosen, and the frames below measure the photograph, so
        // Off is chosen once now. The eyedropper stays armed.
        overlay("band-photograph", "off").picker(Some(LUMINANCE_RANGE)),
        // One click on the sky: the band is set around its tone, one entry, and the photograph is
        // exactly where it was because no layer is bound to the mask.
        pick("band-click", "sky-top")
            .commits(1)
            .label("Pick Luminance range 1")
            .picker(Some(LUMINANCE_RANGE)),
        // The area a drag over the sky would read, asked through the API: the input at the unbound
        // mask's site, answered by the renderer of record, which its answer names.
        area_read("band-area", "sky-top").picker(Some(LUMINANCE_RANGE)),
        // The band typed, one declared field at a time, each its own entry, so the rest of the
        // scenario reads a band of known edges. `low` before `high` because the payload refuses a
        // crossed band rather than rendering an empty selection. The fields stay editable while the
        // eyedropper is armed.
        typed("band-low", "low", BAND_LOW),
        typed("band-high", "high", BAND_HIGH),
        typed("band-low-feather", "low_feather", BAND_FEATHER),
        typed("band-typed", "high_feather", BAND_FEATHER),
        // A linear gradient intersected with the band, swept from the side it leaves alone towards
        // the side it selects, and committed. It is what gives the two sky patches their
        // difference: the top row is inside it, the bottom row outside. The sweep is a drafted
        // gesture holding the swept geometry, and the apply commits it as one entry.
        Step::new("gradient-mode", MaskStep::Mode("intersect".into())).commits(0),
        // A shape gesture owns the next press, so starting one puts the eyedropper down.
        Step::new("gradient-added", MaskStep::Add(LINEAR.into()))
            .commits(0)
            .picker(None),
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
        Step::new("gradient-applied", MaskStep::Release)
            .commits(1)
            .label("Add intersect linear")
            .no_draft()
            .components(&BANDED[..2]),
        // Applying selects the gradient, whose handles then rest over the patches the probes read,
        // so its row is put down before the frames that are read.
        Step::new("gradient-deselected", MaskStep::SelectComponent(None)).commits(0),
        // One stop down through the mask, as the panel's own drag. **This is the failure frame**:
        // the band was drawn for the sky and takes the grey card beside it, because the two are 1.4
        // output codes apart on the axis the band measures.
        masked_exposure("failure", "Mask 1 · Exposure -1.00 EV")
            .payload(BASIC_EFFECT, json!({ EXPOSURE: MASKED_EV })),
        // A colour range intersected with both. Created with no swatches, which selects nothing:
        // the picture goes back to the one the mask never touched, and that is what an unsampled
        // colour range means rather than a component that does nothing. Created, it is selected and
        // the eyedropper is armed on it again, and the overlay is Tint again over an Off the
        // person chose before, so Off is chosen once more for the frames that read the photograph.
        Step::new("colour-mode", MaskStep::Mode("intersect".into())).commits(0),
        Step::new("colour-added", MaskStep::Add(COLOUR_RANGE.into()))
            .commits(1)
            .label("Add intersect colour range")
            .components(&BANDED)
            .selected_component(Some("Colour range 1"))
            .picker(Some(COLOUR_RANGE)),
        overlay("colour-photograph", "off").picker(Some(COLOUR_RANGE)),
        // One click on the sky, which replaces the swatches (it held none). **This is the remedy
        // frame**: the sky stays selected and the grey card comes back, which is the component
        // list doing the work the band cannot.
        pick("remedy", "sky-top")
            .commits(1)
            .label("Pick Colour range 1")
            .picker(Some(COLOUR_RANGE)),
        // Return is the pick bar's Done: the eyedropper is put down, Mask mode and the selection
        // stay, and nothing is committed.
        Step::new("pick-done", script::Step::key(KEY_DONE))
            .commits(0)
            .mode("mask")
            .selected_component(Some("Colour range 1"))
            .picker(None),
        // A **global** exposure layer, from JSON with no mask in the request, which the host places
        // ahead of the masked one: the stack's first Basic layer is this one. **This is the
        // input-dependence frame**: the band reads the pixel its own operation receives, that pixel
        // is now well past the band's upper shoulder, and the sky is outside the band — so the masked
        // layer stops applying to it entirely.
        Step::new(
            "global",
            script::Step::call("edit.set-basic", json!({"exposure":GLOBAL_EV})),
        )
        .commits(1)
        .label("Exposure +0.75 EV")
        .payload(BASIC_EFFECT, json!({ EXPOSURE: GLOBAL_EV })),
        // The same area read again: the bound mask's input is now behind the global layer.
        area_read("global-area", "sky-top"),
        // Undone, and the selection comes back with the input it was drawn against. An undo moves
        // the revision on like any commit, to the entry the pick made, and the masked layer is the
        // stack's first Basic layer again.
        Step::new("global-undone", script::Step::api("history.undo"))
            .commits(1)
            .label("Pick Colour range 1")
            .payload(BASIC_EFFECT, json!({ EXPOSURE: MASKED_EV }))
            .same_layer(BASIC_EFFECT, "failure"),
        // The overlay on with the pointer off the list, which asks for the **composed** mask's
        // grid. This mask reads pixels, and a layer is bound to it — the masked Exposure of
        // `failure` — so the grid is read on that layer's own input and drawn. It is checked against
        // `global-undone`: the patches it calls selected are the patches that frame moved.
        overlay("overlay-composed", "mask-on-black"),
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
        overlay("overlay-off", "off"),
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
        Step::opened("reopened").label("Pick Colour range 1"),
        range_experiment("range-experiment"),
        mask_mode("mask-mode"),
        // A second mask, one colour range. Created, it is selected and the eyedropper is armed on
        // it with nothing bound to the mask and nothing sampled, which selects nothing; and the
        // overlay is Tint over the stored Off, so Off is chosen for the frames that read the
        // photograph. The eyedropper stays armed through everything below until it is put down.
        Step::new("grey-mask", MaskStep::New(COLOUR_RANGE.into()))
            .commits(1)
            .label("Mask 2 · Add colour range")
            .selected_component(Some("Colour range 1"))
            .picker(Some(COLOUR_RANGE)),
        overlay("grey-photograph", "off").picker(Some(COLOUR_RANGE)),
        // One click on the **grey card**, before any adjustment is bound: the pick reads the stage
        // a first Basic adjustment will receive. Every neutral is one colour to a metric with no
        // lightness term — white, the two greys above it and black are mutually within `0.0015`, a
        // third of the tightest radius — so one sampled grey selects the whole tonal range. No
        // layer is bound, so the picture does not change yet.
        pick("neutral-picked", "grey-card")
            .commits(1)
            .label("Mask 2 · Pick Colour range 1")
            .picker(Some(COLOUR_RANGE)),
        // An adjustment through the mask, dragged while the eyedropper is still armed, which lands
        // on the open mask's own layer and **leaves the selection where the pick put it**: the
        // frame shows five patches move at once.
        masked_exposure("neutral", "Mask 2 · Exposure -1.00 EV").picker(Some(COLOUR_RANGE)),
        // A second swatch, on the orange, with Shift held. A colour range folds its samples by
        // nearest, which is the same union the component list composes by, so the orange joins the
        // selection and the neutrals stay in it.
        pick_added("orange", "orange")
            .commits(1)
            .label("Mask 2 · Pick Colour range 1")
            .picker(Some(COLOUR_RANGE)),
        // A plain click **replaces** the swatches held: the grey card alone is sampled again, the
        // orange leaves the selection and nothing else moves. One entry, because the swatches
        // changed.
        pick("replaced", "grey-card")
            .commits(1)
            .label("Mask 2 · Pick Colour range 1")
            .picker(Some(COLOUR_RANGE)),
        // A third mask, one colour range, and a **drag** over the interior of the **light skin**
        // patch with no adjustment bound: the area's representative colour is the sample. Dark skin
        // is `0.0108` away in the frozen metric, a third of what one face's own shading spans, so
        // any setting that holds a lit face takes both — and the frame after the adjustment shows
        // both move.
        Step::new("skin-mask", MaskStep::New(COLOUR_RANGE.into()))
            .commits(1)
            .label("Mask 3 · Add colour range")
            .selected_component(Some("Colour range 1"))
            .picker(Some(COLOUR_RANGE)),
        overlay("skin-photograph", "off").picker(Some(COLOUR_RANGE)),
        pick_area("skin-picked", "light-skin")
            .commits(1)
            .label("Mask 3 · Pick Colour range 1")
            .picker(Some(COLOUR_RANGE)),
        masked_exposure("skin", "Mask 3 · Exposure -1.00 EV").picker(Some(COLOUR_RANGE)),
        // Escape puts the eyedropper down and keeps Mask mode and the selection; the row's Pick
        // arms it again on the selected component; Return, the pick bar's Done, puts it down.
        escape("picker-escaped").selected_component(Some("Colour range 1")),
        Step::new("picker-rearmed", MaskStep::Pick)
            .commits(0)
            .mode("mask")
            .picker(Some(COLOUR_RANGE)),
        Step::new("picker-done", script::Step::key(KEY_DONE))
            .commits(0)
            .mode("mask")
            .picker(None),
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
        // Arming a brush shows its live coverage by default. These frames measure the
        // photograph, so make the manual Off choice after arming and retain it through rearming.
        overlay("brush-photograph", "off"),
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
        overlay("held-erase-photograph", "off"),
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
        // The overlay on and off over that mask. **This is the frame P16 gives a mask a person
        // painted**: a colour-held stroke makes a brush component read pixels, and P16 draws its
        // grid, so what the erase has taken shows on the overlay as well as in a picture adjusted
        // through the mask. The grid is checked against that picture — `constrained`'s own
        // readings — and then the overlay is switched off and the photograph is exactly where it was.
        overlay("held-overlay", "mask-on-black"),
        overlay("held-overlay-off", "off"),
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
        // `settle_between` exists because, on a heavily loaded host, `STROKE_INTERVAL_MS`
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
        Step::new("pointer-mode", WorkspaceStep::default().mode("pointer"))
            .commits(0)
            .mode("pointer"),
    ])
}

/// The range eyedropper as the frame's panel reported it: armed, with the hint of the kind it is
/// armed on and, for a colour range, how many swatches are held of the most it holds.
fn armed(frame: &Frame, hint: &str, count: Option<&str>) -> Result {
    let picker = &frame.state()["masks"]["picker"];
    ensure(
        picker["armed"] == json!(true)
            && picker["hint"]
                .as_str()
                .is_some_and(|said| said.contains(hint))
            && picker["count"] == json!(count),
        format!("The eyedropper reads {picker}, expected a hint with {hint:?} and count {count:?}"),
    )
}

/// The overlay as Tint over a stored setting of Off, which is what an armed eyedropper shows when
/// the person has not chosen otherwise.
fn tinting(frame: &Frame) -> Result {
    let overlay = &frame.state()["mask_overlay"];
    ensure(
        overlay["setting"] == json!("off") && overlay["effective"] == json!("tint"),
        format!("The overlay reads {overlay}, expected Tint over a stored Off"),
    )
}

/// The overlay chosen Off, which is what keeps the photograph clean while the eyedropper is armed.
fn photograph(frame: &Frame) -> Result {
    let overlay = &frame.state()["mask_overlay"];
    ensure(
        overlay["setting"] == json!("off") && overlay["effective"] == json!("off"),
        format!("The overlay reads {overlay}, expected Off"),
    )
}

/// No adjustment is bound to any mask: the stack holds no Basic layer with a mask.
fn unbound(frame: &Frame) -> Result {
    let bound: Vec<_> = basic_layers(frame)
        .into_iter()
        .filter(|(_, mask)| *mask != Value::Null)
        .collect();
    ensure(
        bound.is_empty(),
        format!("The frame binds adjustments to masks: {bound:?}"),
    )
}

/// A `mask.sample-input-area` step's answer over a flat patch: read by the GPU, the renderer of
/// record, and every point's tone on the band's own axis within [`TONE_TOLERANCE`] of their mean,
/// which is the answer. The patch is flat, so the tolerance is the sampled tone's own error.
fn area_tone(frame: &Frame, what: &str) -> Result<f64> {
    let answer = &frame["step"]["result"];
    ensure(
        answer["renderer"] == json!({"record": "gpu", "reason": null}),
        format!(
            "{what}: mask.sample-input-area was not the GPU's: {}",
            answer["renderer"]
        ),
    )?;
    let points: Vec<[f64; 3]> = serde_json::from_value(answer["points"].clone())
        .map_err(|error| format!("{what}: unreadable points: {error}"))?;
    ensure(!points.is_empty(), format!("{what}: no points"))?;
    let tones: Vec<f64> = points
        .iter()
        .map(|[r, g, b]| {
            100.0 * luxforge_reference::srgb::encode_extended(0.2126 * r + 0.7152 * g + 0.0722 * b)
        })
        .collect();
    let mean = tones.iter().sum::<f64>() / tones.len() as f64;
    let worst = tones
        .iter()
        .map(|value| (value - mean).abs())
        .fold(0.0, f64::max);
    ensure(
        worst <= TONE_TOLERANCE,
        format!("{what}: a point's tone is {worst} from the area's mean {mean}"),
    )?;
    Ok(mean)
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

/// Every one of the fixture's twelve patches in one capture, by name, in the photograph's recorded
/// rectangle.
fn read(frame: &Frame) -> Result<Vec<(String, f64)>> {
    let mut out = Vec::new();
    for ((name, _), at) in RANGE_PATCHES.iter().zip(probes()) {
        out.push(((*name).to_owned(), frame.luminance_at(at, PATCH_HALF)?));
    }
    Ok(out)
}

fn reading(read: &[(String, f64)], name: &str) -> Result<f64> {
    read.iter()
        .find(|(held, _)| held == name)
        .map(|(_, value)| *value)
        .ok_or_else(|| format!("No patch called {name} was read").into())
}

/// Exactly the patches named moved between `before` and `frame`, and every other one is where it
/// was: a named patch by at least [`MOVED`], every other one by at most [`UNTOUCHED`], which is
/// also what makes the set that moved exactly the one named. This is what makes "a band takes a
/// grey card as well as a sky" a claim about the whole photograph and not about the two patches this
/// scenario happened to look at. Returns the frame's readings.
fn only(
    checks: &mut Checks,
    frame: &Frame,
    what: &str,
    before: &[(String, f64)],
    selected: &[&str],
) -> Result<Vec<(String, f64)>> {
    let after = read(frame)?;
    for (name, value) in &after {
        let tolerance = if selected.contains(&name.as_str()) {
            Tolerance::Apart(MOVED)
        } else {
            Tolerance::Within(UNTOUCHED)
        };
        checks.compare(
            frame,
            &format!("{what}: {name}"),
            *value,
            reading(before, name)?,
            tolerance,
        )?;
    }
    Ok(after)
}

/// Every patch of `frame` within [`UNTOUCHED`] of the same patch in `reference`. Returns the
/// frame's readings.
fn same(
    checks: &mut Checks,
    frame: &Frame,
    what: &str,
    reference: &[(String, f64)],
) -> Result<Vec<(String, f64)>> {
    only(checks, frame, what, reference, &[])
}

/// A `mask-on-black` reading of one patch: full coverage is 200 or brighter, within 55 of the
/// overlay's 255; none is 40 or darker, within 40 of its 0; a ramp between them is 100 to 155,
/// within 27.5 of 127.5.
fn covered(
    checks: &mut Checks,
    frame: &Frame,
    what: &str,
    value: f64,
    (centre, within): (f64, f64),
) -> Result {
    checks.compare(frame, what, value, centre, Tolerance::Within(within))
}

const FULL: (f64, f64) = (255.0, 55.0);
const NONE: (f64, f64) = (0.0, 40.0);
const RAMP: (f64, f64) = (127.5, 27.5);

/// A coverage grid against the photograph's own measured change under the same mask: every patch
/// the grid calls selected (128 or brighter) is a patch `moved` moved by at least [`MOVED`] from
/// `before`, and every patch it calls unselected is one it did not.
fn agrees(
    checks: &mut Checks,
    frame: &Frame,
    what: &str,
    moved: &[(String, f64)],
    before: &[(String, f64)],
) -> Result {
    let grid = read(frame)?;
    for (name, value) in &grid {
        let lifted = (reading(moved, name)? - reading(before, name)?).abs();
        ensure(
            (lifted >= MOVED) == (*value >= 128.0),
            format!(
                "{what}: {name} read {value:.1} of coverage where the photograph moved by \
                 {lifted:.2} under the same mask"
            ),
        )?;
    }
    checks.note(
        frame,
        &format!("{what}: every patch the grid calls selected is one the masked adjustment moved"),
        json!({"patches": grid, "moved_against": moved}),
    );
    Ok(())
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

/// Every answer the overlay got while the run was going, in order: a coverage grid that was painted
/// and uploaded, or one the host refused with its reason.
///
/// It is read from the events rather than from the frames because the grid is not part of the state
/// summary — what a frame carries is the photograph with the overlay drawn over it, which is what the
/// pixel readings below check. These are the host's own record of what each overlay request was
/// answered with, and for which component. Nothing is refused: the overlay of a mask that reads
/// pixels, and of each of its range components, reads the input of the mask's first bound layer,
/// which launch 1 binds when it drags the masked Exposure.
fn overlay_answers(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        // A tint a shape gesture showed of its own accord, over a setting of `off`, is not an
        // answer to the overlay the script asked for: the gesture forces it while a handle is
        // dragged and drops it after, so it is left out of the count either way it went.
        .filter(|event| {
            let detail = &event["detail"];
            detail["forced"] != json!(true)
                && (detail["setting"].is_null() || detail["setting"] == detail["mode"])
        })
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
    let mut checks = Checks::new();
    let remedy = verify_launch1(launch1, &mut checks)?;
    verify_launch2(launch2, &remedy, &mut checks)?;
    // The stroke's own end-to-end latency, from the events of the launch that painted one, with the
    // recipe it was painted on and the load the host was under. It is a smoke-run figure over one
    // fixture and is labelled as such where it is recorded; the interval is the same one
    // `editor-latency` measures for a slider.
    let recipe = masked_recipe(launch2)?;
    let latency = stroke_latency(run.root(), &launch2.events, recipe)?;
    run.record("stroke_latency", latency.clone());
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "overlay_answers": overlay_answers(&launch1.events),
            "stroke_latency": latency,
            "moved_threshold": MOVED,
            "untouched_threshold": UNTOUCHED,
            "scope": "Mean Rec. 709 luminance of the twelve fixture patches in the photograph the editor records drawing, read back from the renderer with no JPEG between; every claim about what a selection stopped selecting is an equality against a control patch of the same colour in the same frame, or against the frame before it in the same launch, and none is a colorimetric claim",
        }),
    )
}
/// A painted stroke's own control-to-frame latency, paired from the run's events.
///
/// The pairing is exact rather than by order: every `mask_draft_set` is answered by one
/// `mask_draft_preview` carrying the preview generation it queued, and `preview_displayed` repeats
/// that generation. A gesture holds one round trip at a time, so a set with no answer between it and
/// the next one is a fault and not a measurement.
///
/// The stroke this pairs is painted with `settle_between: true` (see `verify`'s
/// `paced-stroke` step), so the desktop itself holds every position after the first until the one
/// before it has reached the screen: a heavily loaded host stretches the stroke's real time rather
/// than superseding every drafted frame before it can be paired. That leaves the emptiness check
/// below to catch a genuine fault — a stroke that never puts a single frame on screen at all — rather
/// than load-only flakiness, so it stays an `ensure` and not a provisional
/// reading: an empty pairing is a broken stroke, not a busy host.
///
/// **What this figure is not.** It is one stroke on one 1.4 MP fixture during a smoke run, not an
/// `editor-latency` baseline: that harness drives field-patch sliders over the 24 MP and 60 MP
/// fixtures and a stroke is a different gesture; its own `--mode paint` paced stroke does not set
/// `settle_between`, because it is a controlled, dedicated run rather than a smoke scenario sharing
/// the host with whatever else is running, so its measurement definition is left as `STROKE_INTERVAL_MS`
/// alone. This figure is also taken on the **heaviest** recipe this scenario builds — four masked
/// colour layers, three of them holding a component that reads pixels and therefore bounds the whole
/// stage — which is recorded beside it, because that is most of what the number is. The figure is
/// written as one row of the one shape every timing tool writes, and the one-minute load average
/// through the one load record (`launch::load`), since a figure taken above the threshold is
/// `unreliable` by the repository's own rule.
fn stroke_latency(root: &Path, events: &[Value], recipe: Value) -> Result<Value> {
    // The pairing itself lives in `editor_latency`, beside the `--mode paint` run that takes the same
    // measurement on a bare recipe at 24 and 60 MP, so the two figures are one definition and not
    // two implementations that could drift apart. The paced stroke does not race the render
    // pipeline, so there is always at least one pair to find here unless the stroke is genuinely
    // broken.
    let feedback = editor_latency::paced_stroke_latencies(events)?;
    ensure(
        !feedback.photograph_ms.is_empty() || !feedback.coverage_ms.is_empty(),
        "The run painted no stroke with photograph or authoritative coverage feedback",
    )?;
    let displayed = feedback.photograph_ms.len();
    Ok(json!({
        "inputs": feedback.inputs,
        "displayed": displayed,
        "coverage_feedback": feedback.coverage_ms.len(),
        "rows": [stats::row("input_to_presented_frame", "ms", feedback.photograph_ms),
            stats::row("input_to_authoritative_mask_coverage", "ms", feedback.coverage_ms)],
        "interval_ms": STROKE_INTERVAL_MS,
        "positions": STROKE_POSITIONS,
        "recipe": recipe,
        "load": launch::load(launch::load_average(root)),
        "scope": "Photograph timing pairs mask_draft_set with preview_displayed by generation/revision. An unbound change that reuses photograph pixels pairs accepted draft ID/revision with mask_coverage_ready instead, in its own authoritative coverage row; it is not a photograph render or display scanout. This smoke workload uses a 1440x960 fixture and records its masked layers beside the timings; it is not a 24/60 MP baseline.",
    }))
}

/// Launch 1, step by step: what the photograph, the panel and the overlay show. What each step
/// commits, its label, the Basic layers' payloads, the gestures' drafts, the mask's components and
/// the overlay's setting are the plan's. Returns the remedy frame's readings, which launch 2 reopens.
fn verify_launch1(launch: &Checked, checks: &mut Checks) -> Result<Vec<(String, f64)>> {
    let start = launch.at("opened")?;
    let opened = read(start)?;
    // The two sky patches are the same colour, which is what makes every control comparison below a
    // comparison and not a prediction.
    checks.compare(
        start,
        "the fixture's two sky patches",
        reading(&opened, "sky-top")?,
        reading(&opened, "sky-bottom")?,
        Tolerance::Within(UNTOUCHED),
    )?;

    // The band created by its own button. A typed kind: the plan holds it to one entry, no draft,
    // its one component selected and the eyedropper armed on it; here the mask's own gesture state
    // is empty too, and nothing is bound to the mask.
    let created = launch.at("band-created")?;
    ensure(
        created.state()["mask_draft"] == Value::Null,
        "A typed kind opened a gesture",
    )?;
    unbound(created)?;
    armed(created, "Click a tone", None)?;
    tinting(created)?;

    // The row open as created, which shows what it was created as — the whole tonal range with soft
    // shoulders — and where the product says what a band cannot do, before a person has typed or
    // picked a number into it.
    let fresh = fields(created, 0)?;
    ensure(
        fresh["low"] == json!(0.0) && fresh["high"] == json!(100.0),
        format!("A new band starts at {fresh}"),
    )?;
    let said = limits(created, 0)?;
    ensure(
        said.len() == 2
            && said[0].contains("output codes apart")
            && said[1].contains("the 100% view is the truth"),
        format!("The open row said {said:?}"),
    )?;

    // The tint of a mask **no layer is bound to**: the selection is read at the stage a first Basic
    // adjustment will receive, so the grid for the new row was drawn rather than refused. It was
    // drawn as Tint over a stored Off, which is the eyedropper's, not a setting anyone chose.
    let (mask, band) = (
        created.only_mask()?["id"].clone(),
        created.component(0)?["id"].clone(),
    );
    ensure(
        launch.events.iter().any(|event| {
            let detail = &event["detail"];
            event["event"] == "mask_overlay"
                && detail["mask"] == mask
                && detail["component"] == band
                && detail["mode"] == json!("tint")
                && detail["setting"] == json!("off")
        }),
        "No coverage grid was drawn for the new band while no layer was bound to its mask",
    )?;
    ensure(
        !launch.events.iter().any(|event| {
            event["event"] == "mask_overlay_absent" && event["detail"]["mask"] == mask
        }),
        "The overlay refused the unbound band",
    )?;
    let tint = launch.at("band-tint")?;
    ensure(
        tint.component(0)?["hovered"] == json!(true),
        "The band's row was not hovered for its grid",
    )?;

    // Off chosen, the eyedropper still armed: the photograph is the one the mask never touched.
    let clean = launch.at("band-photograph")?;
    photograph(clean)?;
    armed(clean, "Click a tone", None)?;
    same(checks, clean, "with the overlay chosen Off", &opened)?;

    // One click on the sky. The band is set around its tone — ten units each side, and shoulders of
    // ten — and, with no layer bound, the photograph has not changed.
    let clicked = launch.at("band-click")?;
    let around = fields(clicked, 0)?;
    let tone = SKY_TONE;
    for (field, expected) in [
        ("low", tone - 10.0),
        ("high", tone + 10.0),
        ("low_feather", 10.0),
        ("high_feather", 10.0),
    ] {
        let value = around[field].as_f64().unwrap_or(f64::NAN);
        ensure(
            (value - expected).abs() <= TONE_TOLERANCE,
            format!(
                "A click on the sky set {field} to {value}, expected {expected} within {TONE_TOLERANCE}"
            ),
        )?;
    }
    armed(clicked, "Click a tone", None)?;
    same(
        checks,
        clicked,
        "a band picked with no layer bound",
        &opened,
    )?;

    // The area under the click, asked through the API: the sky's own tone, read by the GPU.
    let area = launch.at("band-area")?;
    let what = "the sky's area at the unbound band's input site";
    checks.compare(
        area,
        what,
        area_tone(area, what)?,
        SKY_TONE,
        Tolerance::Within(TONE_TOLERANCE),
    )?;

    // The band typed, one field per entry — four entries, which the plan counts — and four numbers,
    // nothing dragged.
    let band = fields(launch.at("band-typed")?, 0)?;
    ensure(
        band == &json!({"low":BAND_LOW,"low_feather":BAND_FEATHER,"high":BAND_HIGH,
                        "high_feather":BAND_FEATHER}),
        format!("The typed band reads {band}"),
    )?;

    // Starting the gradient put the eyedropper down, without leaving Mask mode.
    ensure(
        launch.at("gradient-added")?.state()["masks"]["picker"] == Value::Null,
        "A shape gesture started with the eyedropper still armed",
    )?;

    // The gradient committed, intersected with the band, claiming no limit.
    ensure(
        limits(launch.at("gradient-applied")?, 1)?.is_empty(),
        "A gradient claimed a limit a position-based component does not have",
    )?;

    // One stop down through the mask. **The failure.** The band was drawn for the sky and takes the
    // grey card beside it, because the two are 1.4 output codes apart on the axis a band measures;
    // the bottom sky patch, the same colour outside the gradient, does not move at all.
    let failed = launch.at("failure")?;
    let failure = only(
        checks,
        failed,
        "a band drawn for the sky",
        &opened,
        &["sky-top", "grey-card"],
    )?;
    checks.compare(
        failed,
        "the selected sky against its own control outside the gradient",
        reading(&failure, "sky-top")?,
        reading(&failure, "sky-bottom")?,
        Tolerance::Apart(MOVED),
    )?;

    // An unsampled colour range intersected in. It is selected, the eyedropper is armed on it with
    // no swatch held, and the overlay is Tint again; with Off chosen it selects nothing, so the whole
    // picture is back to the one the mask never touched — which is what an empty swatch list means.
    let added = launch.at("colour-added")?;
    ensure(
        sample_count(added, 2)? == 0,
        "A new colour range already holds a swatch",
    )?;
    armed(added, "Shift-click to add", Some("0 of 5"))?;
    tinting(added)?;
    let unsampled = launch.at("colour-photograph")?;
    photograph(unsampled)?;
    same(checks, unsampled, "an unsampled colour range", &opened)?;

    // One click on the sky. **The remedy.** The sky is selected again and the grey card is back
    // where it started, which is the component list doing what the band cannot.
    let remedied = launch.at("remedy")?;
    let picked = samples(remedied, 2)?;
    ensure(
        picked.len() == 1,
        format!("The pick left {picked:?} on the row"),
    )?;
    armed(remedied, "Shift-click to add", Some("1 of 5"))?;
    let remedy = only(
        checks,
        remedied,
        "a colour range picked on the sky",
        &opened,
        &["sky-top"],
    )?;

    // Return, the pick bar's Done, put the eyedropper down and left everything else where it was.
    let done = launch.at("pick-done")?;
    ensure(
        done.state()["masks"]["picker"] == Value::Null,
        "Done left the eyedropper armed",
    )?;
    same(checks, done, "after Done", &remedy)?;

    // A **global** exposure layer, placed ahead of the masked one. The band reads the pixel its own
    // operation receives, that pixel has moved past the band's upper shoulder, and the sky is outside the
    // band: the masked layer stops reaching it. Read as an equality against the control patch of the
    // identical colour, so nothing here is a predicted output code.
    let global = launch.at("global")?;
    let layers = basic_layers(global);
    ensure(
        layers.len() == 2 && layers[0].1 == Value::Null && layers[1].1 != Value::Null,
        format!("The stack holds Basic layers {layers:?}"),
    )?;
    let reordered = read(global)?;
    checks.compare(
        global,
        "the sky under a +0.75 EV layer ahead of the band, against its control",
        reading(&reordered, "sky-top")?,
        reading(&reordered, "sky-bottom")?,
        Tolerance::Within(UNTOUCHED),
    )?;
    checks.compare(
        global,
        "the sky itself under the global layer",
        reading(&reordered, "sky-top")?,
        reading(&remedy, "sky-top")?,
        Tolerance::Apart(MOVED),
    )?;

    // The band's input, read over the sky again: the global layer is ahead of it now, and the sky's
    // tone there is past the band's upper shoulder.
    let lifted = launch.at("global-area")?;
    let what = "the sky's area at the bound band's input behind the global layer";
    let tone = area_tone(lifted, what)?;
    ensure(
        tone > BAND_HIGH + BAND_FEATHER,
        format!("{what}: the sky's tone {tone} is not past the band's upper shoulder"),
    )?;
    checks.compare(
        lifted,
        what,
        tone,
        SKY_TONE,
        Tolerance::Apart(BAND_HIGH + BAND_FEATHER - SKY_TONE),
    )?;

    // Undone. The selection is back with the input it was drawn against, byte for byte the picture
    // the pick produced.
    let undone = same(
        checks,
        launch.at("global-undone")?,
        "after the global layer was undone",
        &remedy,
    )?;

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
            answer["component"] == hovered.component(row)?["id"]
                && hovered.component(row)?["hovered"] == json!(true),
            format!(
                "The grid for step {step:?} names {}, and row {row} is {}",
                answer["component"],
                hovered.component(row)?
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
    agrees(
        checks,
        launch.at("overlay-composed")?,
        "the composed mask's overlay",
        &undone,
        &opened,
    )?;

    // The gradient's own row. A position-based component's grid has not changed: the fixture's top
    // row is inside the gradient and reads white, its bottom row is outside and reads black, and the
    // row between them is on the ramp and reads between the two — the coverage a gradient from 0.75
    // to 0.25 of the height has there.
    let ramp = launch.at("overlay-gradient")?;
    for (index, (name, value)) in read(ramp)?.iter().enumerate() {
        let row = index as u32 / RANGE_COLUMNS;
        let expected = match row {
            0 => FULL,
            1 => RAMP,
            _ => NONE,
        };
        covered(
            checks,
            ramp,
            &format!("the gradient's own overlay: {name} in row {row}"),
            *value,
            expected,
        )?;
    }

    // Each range component's own row, each with a grid of its own now. A component's grid is that
    // component alone, so neither is bounded by the gradient: the band takes both sky patches and the
    // grey card beside them — which is this study's `1.4` output codes, drawn — and the colour range
    // picked on the sky takes the sky and leaves the grey card. That pair is the failure and its
    // remedy, in the overlay, where before they could only be read off the picture.
    for (step, what, grey) in [
        ("overlay-band", "the band's own overlay", FULL),
        (
            "overlay-colour",
            "the picked colour range's own overlay",
            NONE,
        ),
    ] {
        let frame = launch.at(step)?;
        let grid = read(frame)?;
        for (name, expected) in [("sky-top", FULL), ("sky-bottom", FULL), ("grey-card", grey)] {
            covered(
                checks,
                frame,
                &format!("{what}: {name}"),
                reading(&grid, name)?,
                expected,
            )?;
        }
    }

    // The statement itself, on screen. The row is open and the panel is scrolled to it, so what a
    // person reads before typing a number into a band is in a capture and not only in a model.
    let statement = launch.at("statement")?;
    ensure(
        limits(statement, 0)? == said,
        format!("The row on screen says {:?}", limits(statement, 0)?),
    )?;
    Ok(remedy)
}

/// Launch 2, step by step: the colour range's own limits, and the colour-constrained brush.
fn verify_launch2(launch: &Checked, remedy: &[(String, f64)], checks: &mut Checks) -> Result {
    for name in ["stroke", "painted", "constrained", "held-overlay-off"] {
        photograph_only(launch.at(name)?)?;
    }
    // The reopened catalog. Launch 1's mask and its selection are back, in the pixels and not only in
    // the rows.
    let reopened = same(
        checks,
        launch.at("reopened")?,
        "after the catalog was reopened",
        remedy,
    )?;

    // A second mask, created with the eyedropper armed and nothing bound to any mask but launch
    // 1's. With Off chosen the photograph is the reopened one.
    let created = launch.at("grey-mask")?;
    armed(created, "Shift-click to add", Some("0 of 5"))?;
    tinting(created)?;
    let clean = launch.at("grey-photograph")?;
    photograph(clean)?;
    same(
        checks,
        clean,
        "a new colour range with nothing sampled",
        &reopened,
    )?;

    // A sampled grey, picked **before any adjustment is bound to the mask**. The pick reads the
    // stage a first Basic adjustment will receive, so it commits and selects, and the photograph
    // has not changed because nothing is bound.
    let picked = launch.at("neutral-picked")?;
    ensure(
        samples(picked, 0)?.len() == 1,
        format!("The row lists {:?}", samples(picked, 0)?),
    )?;
    armed(picked, "Shift-click to add", Some("1 of 5"))?;
    same(
        checks,
        picked,
        "a grey picked with nothing bound",
        &reopened,
    )?;

    // **Every neutral is one colour** to a metric with no lightness term — within 0.0015 of every
    // other — so one click selects the whole tonal range and five patches move at once when an
    // adjustment is dragged through the mask. The eyedropper is still armed, and the adjustment
    // landed on the open mask's own layer.
    let adjusted = launch.at("neutral")?;
    armed(adjusted, "Shift-click to add", Some("1 of 5"))?;
    let neutral = only(
        checks,
        adjusted,
        "a colour range sampled on a grey card, adjusted",
        &reopened,
        &["grey-card", "white", "grey-65", "black", "grey-8"],
    )?;
    let layers = basic_layers(adjusted);
    ensure(
        layers
            .iter()
            .filter(|(_, mask)| *mask != Value::Null)
            .count()
            == 2,
        format!("The stack holds Basic layers {layers:?}"),
    )?;

    // A second swatch, added with Shift held. A colour range folds its samples by nearest, which is
    // the same union the component list composes by, so the orange joins and the neutrals stay.
    let orange = launch.at("orange")?;
    let two = only(
        checks,
        orange,
        "a second swatch on the orange",
        &neutral,
        &["orange"],
    )?;
    ensure(
        samples(orange, 0)?.len() == 2,
        format!("The row lists {:?}", samples(orange, 0)?),
    )?;
    armed(orange, "Shift-click to add", Some("2 of 5"))?;

    // A plain click **replaces** the swatches held: the grey card alone is sampled again, the
    // orange leaves the selection, and the neutrals stay.
    let replaced = launch.at("replaced")?;
    ensure(
        samples(replaced, 0)?.len() == 1 && samples(replaced, 0)? == samples(picked, 0)?,
        format!(
            "The row lists {:?}, and the grey alone was {:?}",
            samples(replaced, 0)?,
            samples(picked, 0)?
        ),
    )?;
    armed(replaced, "Shift-click to add", Some("1 of 5"))?;
    let three = only(
        checks,
        replaced,
        "a plain click replacing the swatches",
        &two,
        &["orange"],
    )?;
    checks.compare(
        replaced,
        "the orange after the replacing click against its unmasked reading",
        reading(&three, "orange")?,
        reading(&reopened, "orange")?,
        Tolerance::Within(UNTOUCHED),
    )?;

    // A third mask, picked by **dragging over the interior of the light skin** with nothing bound
    // to it: one swatch, the area's representative colour, and the photograph unchanged.
    armed(
        launch.at("skin-mask")?,
        "Shift-click to add",
        Some("0 of 5"),
    )?;
    photograph(launch.at("skin-photograph")?)?;
    let dragged = launch.at("skin-picked")?;
    ensure(
        samples(dragged, 0)?.len() == 1,
        format!("The drag left {:?} on the row", samples(dragged, 0)?),
    )?;
    ensure(
        dragged.state()["masks"]["picker"]["rectangle"] == Value::Null,
        "A released drag left its rectangle on the canvas",
    )?;
    same(checks, dragged, "a drag picked with nothing bound", &three)?;

    // One person's skin. **Two people's skin is one colour**: dark skin is 0.0108 from light skin, a
    // third of what one face's own shading spans, so any setting that holds a lit face takes both.
    let skin = only(
        checks,
        launch.at("skin")?,
        "a colour range picked by a drag on light skin, adjusted",
        &three,
        &["light-skin", "dark-skin"],
    )?;

    // Escape put the eyedropper down and left Mask mode and the selection alone, the row's Pick
    // armed it again on the same component with its swatch held, and Return put it down.
    let escaped = launch.at("picker-escaped")?;
    ensure(
        escaped.state()["masks"]["picker"] == Value::Null
            && escaped.state()["workspace"]["mode"] == json!("mask"),
        "Escape did not leave Mask mode with the eyedropper down",
    )?;
    same(checks, escaped, "after Escape", &skin)?;
    armed(
        launch.at("picker-rearmed")?,
        "Shift-click to add",
        Some("1 of 5"),
    )?;
    ensure(
        launch.at("picker-done")?.state()["masks"]["picker"] == Value::Null,
        "Return left the eyedropper armed",
    )?;

    // One ordinary stroke across two surfaces, with an adjustment through it. Both move, which is
    // what a brush without a colour limit does: it selects where it is drawn.
    let painted = only(
        checks,
        launch.at("painted")?,
        "an unlimited stroke across a sky and a foliage patch",
        &skin,
        &["sky-bottom", "foliage"],
    )?;

    // The same path erased back, held to the colour under the brush where the stroke began. **The
    // constrained brush.** The foliage it was seeded on comes out of the selection; the sky the
    // stroke crossed just as far stays in it, and the foliage is back at its own unmasked reading.
    let erased = launch.at("constrained")?;
    let constrained = only(
        checks,
        erased,
        "a colour-held erase seeded on the foliage",
        &painted,
        &["foliage"],
    )?;
    checks.compare(
        erased,
        "the foliage after the held erase against its own unmasked reading",
        reading(&constrained, "foliage")?,
        reading(&reopened, "foliage")?,
        Tolerance::Within(UNTOUCHED),
    )?;

    // **The overlay of a mask a person painted and held to a colour.** A colour-held stroke makes a
    // brush component read the pixel its operation receives, and P16 draws its grid, so what the
    // erase has taken shows on the overlay as well as in a picture adjusted through the mask. The
    // grid is checked against that picture: every patch the overlay calls
    // selected is a patch `constrained` moved from `skin` — the last frame before this mask existed,
    // so the difference is this mask's own layer and nothing else — and every patch it calls
    // unselected is one that frame left alone; the sky the stroke crossed reads covered and the
    // foliage it was seeded on does not. Then the overlay goes off and the photograph is exactly
    // where it was, which is the view-setting contract.
    let overlaid = launch.at("held-overlay")?;
    agrees(
        checks,
        overlaid,
        "the colour-held brush mask's overlay",
        &constrained,
        &skin,
    )?;
    let held = read(overlaid)?;
    covered(
        checks,
        overlaid,
        "the held erase in the overlay: the sky",
        reading(&held, "sky-bottom")?,
        FULL,
    )?;
    covered(
        checks,
        overlaid,
        "the held erase in the overlay: the foliage",
        reading(&held, "foliage")?,
        NONE,
    )?;
    same(
        checks,
        launch.at("held-overlay-off")?,
        "with the overlay switched off again",
        &constrained,
    )?;

    // Undone. The held erase is one entry like every other stroke.
    only(
        checks,
        launch.at("held-erase-undone")?,
        "the held erase undone",
        &constrained,
        &["foliage"],
    )?;
    Ok(())
}

/// A photograph baseline must have no automatic tint or stale adopted coverage.
fn photograph_only(frame: &Frame) -> Result {
    let overlay = &frame.state()["mask_overlay"];
    ensure(
        overlay["effective"] == json!("off")
            && overlay["forced"] == json!(false)
            && overlay["coverage"]["adopted"] == Value::Null,
        format!("A photograph baseline retained mask coverage: {overlay}"),
    )
}

/// Regression coverage over synthetic events: `stroke_latency` must still pass a
/// stroke every one of whose positions reached the screen, and must still fail one that reached it
/// for none of them, which is the "broken stroke" the acceptance criteria keep as a real failure
/// rather than the load-only flakiness `settle_between` removes.
#[cfg(test)]
mod tests {
    use super::*;

    /// Both plans are well formed, and the picking flow is ordered the way the editor answers it: a
    /// pick lands only while the eyedropper is armed, and Escape or Return only while it is, because
    /// an Escape with the eyedropper down leaves Mask mode, which is not what these steps mean.
    #[test]
    fn the_plans_pick_only_while_armed_and_end_picking_only_while_armed() {
        let key = |value: &Value, name: &str| value == &json!({"key": name});
        let mut picks = 0;
        for plan in [launch1_plan(&[]), launch2_plan(&[])] {
            plan.validate().unwrap();
            let mut armed = false;
            for step in plan.steps() {
                let script = step.script();
                let sends = |name: &str| script.as_ref().is_some_and(|value| key(value, name));
                let pick = script
                    .as_ref()
                    .is_some_and(|value| value.get("pick").is_some());
                if pick {
                    picks += 1;
                }
                if pick || sends(script::KEY_ESCAPE) || sends(KEY_DONE) {
                    assert!(
                        armed,
                        "step {:?} acts on an eyedropper that is down",
                        step.name()
                    );
                }
                // What the step leaves is what it says, and a step that says nothing leaves it.
                if let Some(picker) = &step.expect().masks.picker {
                    armed = picker.is_some();
                }
            }
        }
        // Launch 1 clicks a tone and a colour; launch 2 clicks, Shift-clicks, clicks to replace
        // and drags.
        assert_eq!(picks, 6);
    }

    /// The pick steps the scenario uses are the ones the editor parses: a click, a Shift-click and a
    /// drag over a square well inside its patch.
    #[test]
    fn the_pick_steps_are_a_click_a_shift_click_and_a_drag_inside_the_patch() {
        let [x, y] = pick_at("light-skin");
        assert_eq!(
            pick_area("drag", "light-skin").script().unwrap(),
            json!({"pick":{"x":x - PICK_AREA,"y":y - PICK_AREA,"to":[x + PICK_AREA,y + PICK_AREA]}})
        );
        assert_eq!(
            pick_added("shift", "orange").script().unwrap()["pick"]["shift"],
            json!(true)
        );
        assert_eq!(
            pick("click", "grey-card").script().unwrap(),
            json!({"pick":{"x":pick_at("grey-card")[0],"y":pick_at("grey-card")[1]}})
        );
        for plan in [launch1_plan(&[]), launch2_plan(&[])] {
            plan.validate().unwrap();
        }
    }

    #[test]
    fn photograph_baselines_explicitly_hide_coverage_after_each_brush_arm() {
        let plan = launch2_plan(&[]);
        plan.validate().unwrap();
        for name in ["stroke", "constrained"] {
            let at = plan.index(name).unwrap();
            assert_eq!(
                plan.steps()[at - 1].script().unwrap(),
                json!({"workspace":{"mask_overlay":"off"}})
            );
        }
        let baseline = |effective, forced, adopted| {
            Frame::state_only(&json!({"state":{"mask_overlay":{
                "setting":"off","effective":effective,"forced":forced,
                "coverage":{"adopted":adopted}
            }}}))
        };
        photograph_only(&baseline("off", false, Value::Null)).unwrap();
        assert!(photograph_only(&baseline("tint", true, json!({"mask":"mask-a"}))).is_err());
        assert!(photograph_only(&baseline("off", false, json!({"mask":"mask-a"}))).is_err());
    }

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
    /// host does to a position without `settle_between`.
    fn superseded(set_ms: f64, preview_ms: f64, generation: u64) -> Vec<Value> {
        vec![
            json!({"event":"mask_draft_set","elapsed_ms":set_ms}),
            json!({"event":"mask_draft_preview","elapsed_ms":preview_ms,"detail":{"generation":generation}}),
        ]
    }

    #[test]
    fn a_settled_stroke_pairs_every_position_and_passes() {
        // Three positions, each settled before the next was sent (as `settle_between`
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
        // The one row shape and the one nearest-rank distribution: three 28 ms intervals.
        let stroke = stats::distribution(&latency, "input_to_presented_frame").expect("a row");
        assert_eq!(stroke["count"], 3);
        assert_eq!(stroke["samples"], json!([28.0, 28.0, 28.0]));
        assert_eq!(
            (stroke["p50"].clone(), stroke["p95"].clone()),
            (json!(28.0), json!(28.0))
        );
        assert_eq!(latency["load"]["load_threshold"], launch::LOAD_THRESHOLD);
        assert!(latency["load"]["reliability"].is_string());
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
            "The run painted no stroke with photograph or authoritative coverage feedback"
        );
    }

    #[test]
    fn reused_photo_strokes_pair_coverage_by_draft_identity_and_never_count_a_photo_render() {
        let stamp =
            |id: &str, revision: u64| json!({"draft":{"draft_id":id,"draft_revision":revision}});
        let events = vec![
            json!({"event":"mask_draft_set","elapsed_ms":0.0,"detail":{"draft_id":"draft-a"}}),
            json!({"event":"mask_draft_preview","elapsed_ms":1.0,"detail":{"generation":5,"draft_revision":1}}),
            json!({"event":"preview_pixels_reused","elapsed_ms":2.0,"detail":{"generation":5,"identity":stamp("draft-a",1)}}),
            json!({"event":"mask_coverage_ready","elapsed_ms":3.0,"detail":{"generation":5,"identity":stamp("draft-a",2)}}),
            json!({"event":"mask_coverage_ready","elapsed_ms":4.0,"detail":{"generation":5,"identity":stamp("draft-a",1)}}),
            json!({"event":"mask_draft_set","elapsed_ms":5.0,"detail":{"draft_id":"draft-b"}}),
            json!({"event":"mask_draft_preview","elapsed_ms":6.0,"detail":{"generation":5,"draft_revision":1}}),
            json!({"event":"preview_pixels_reused","elapsed_ms":7.0,"detail":{"generation":5,"identity":stamp("draft-b",1)}}),
            json!({"event":"mask_coverage_ready","elapsed_ms":8.0,"detail":{"generation":5,"identity":stamp("draft-a",1)}}),
            json!({"event":"mask_coverage_ready","elapsed_ms":9.0,"detail":{"generation":5,"identity":stamp("draft-b",1)}}),
            json!({"event":"preview_displayed","elapsed_ms":10.0,"detail":{"generation":5,"draft_revision":1}}),
        ];
        let root = tempfile::tempdir().unwrap();
        let result = stroke_latency(root.path(), &events, json!({})).unwrap();
        assert_eq!(result["inputs"], json!(2));
        assert_eq!(result["displayed"], json!(0));
        assert_eq!(result["coverage_feedback"], json!(2));
        let coverage =
            stats::distribution(&result, "input_to_authoritative_mask_coverage").unwrap();
        assert_eq!(coverage["samples"], json!([4.0, 4.0]));
        assert!(stats::distribution(&result, "input_to_presented_frame").is_none());
    }
}
