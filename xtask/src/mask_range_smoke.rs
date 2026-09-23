//! The `mask-range` smoke scenario: phase D's claim, which is that a deterministic per-pixel
//! selection is worth having **and** that what it cannot do is worth saying.
//!
//! Two launches over one catalog. The first types a luminance band, intersects a gradient with it,
//! applies an adjustment through the result and shows the band taking a grey card along with the sky
//! it was drawn for; then intersects a colour range, picks the sky off the photograph and shows the
//! grey card come back; then puts a `+0.75 EV` layer **ahead** of the masked one and shows the band
//! stop selecting the sky altogether; then asks each component for its overlay, where a gradient's
//! grid is drawn and both range components' are refused in the host's own words. The second reopens
//! that catalog and takes the colour range's own limits one at a time — a sampled grey selecting every
//! neutral, a second swatch adding a colour, a third changing nothing, and one person's skin selecting
//! another's — and finishes with the colour-constrained brush: one stroke across two surfaces, and a
//! colour-held erase that takes one of them back out and leaves the other alone.
//!
//! **Why the readings are taken from the photograph and not from the overlay.** The three delivered
//! scenarios before this one read coverage off the `mask-on-black` overlay. A mask holding a range
//! component has **no** overlay: the grid is a function of position over the finished frame, whose
//! pixels are the masked operation's *output*, and painting it would draw a selection the render never
//! made ([proposal P16](../../docs/design/range-study.md#proposals), open and the owner's). So every
//! coverage reading here is taken from the rendered photograph with an adjustment applied through the
//! mask, which is what the design says the 100% view is for, and the refusals are captured as frames
//! of their own rather than worked around.
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
use crate::{
    fixtures::{RANGE_COLUMNS, RANGE_PATCHES, RANGE_ROWS},
    smoke::{self, columns, frame_identity},
    *,
};
use std::time::Duration;

pub const SCENARIO: &str = "mask-range";
/// The range fixture: twelve flat patches of the 24-patch chart's own sRGB renderings, laid out so
/// each of the study's measured failures is legible in one frame. `cargo xtask generate-fixtures`.
const FIXTURE: &str = "fixtures/generated/range.jpg";
const WINDOW: [&str; 2] = workspace_smoke::WINDOW;

const BASIC: &str = "set-basic";
const EXPOSURE: &str = "exposure";
const LINEAR: &str = "linear";
const LUMINANCE_RANGE: &str = "luminance-range";
const COLOUR_RANGE: &str = "colour-range";
const SET_LUMINANCE: &str = "mask.set-luminance-range";

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

/// The open frame plus one per script step.
const LAUNCH1_FRAMES: usize = 28;
const LAUNCH2_FRAMES: usize = 32;

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

/// Launch 1: the band, the gradient, the failure, the remedy, the input dependence and the overlays.
fn launch1_script() -> Value {
    let sky = pick_at("sky-top");
    json!([
        // 1: Mask mode, through the same `workspace.set` the mode strip sends.
        {"workspace":{"mode":"mask"}},
        // 2: a new mask whose first component is a luminance range. A **typed** kind: every field of
        // its geometry carries a default, so the button creates it in one history entry rather than
        // opening a gesture with no shape to drag. It starts as the whole tonal range with soft
        // shoulders, which is the picture, and is narrowed from there.
        {"mask":{"new":LUMINANCE_RANGE}},
        // 3: the row open, which is what shows its number fields and, above them, the host's own
        // statement of what a band cannot separate.
        {"mask":{"select_component":0}},
        // 4-7: the band typed, one declared field at a time, each its own entry. `low` before `high`
        // because the payload refuses a crossed band rather than rendering an empty selection.
        {"field":{"action":SET_LUMINANCE,"parameter":"low","text":BAND_LOW.to_string(),"submit":true}},
        {"field":{"action":SET_LUMINANCE,"parameter":"high","text":BAND_HIGH.to_string(),"submit":true}},
        {"field":{"action":SET_LUMINANCE,"parameter":"low_feather","text":BAND_FEATHER.to_string(),"submit":true}},
        {"field":{"action":SET_LUMINANCE,"parameter":"high_feather","text":BAND_FEATHER.to_string(),"submit":true}},
        // 8-11: a linear gradient intersected with the band, swept from the side it leaves alone
        // towards the side it selects, and committed. It is what gives the two sky patches their
        // difference: the top row is inside it, the bottom row outside.
        {"mask":{"mode":"intersect"}},
        {"mask":{"add":LINEAR}},
        {"mask":{"sweep":{"from":GRADIENT_FROM,"to":GRADIENT_TO}}},
        {"mask":{"apply":true}},
        // 12: one stop down through the mask, as the panel's own drag. **This is the failure frame**:
        // the band was drawn for the sky and takes the grey card beside it, because the two are 1.4
        // output codes apart on the axis the band measures.
        {"slider":{"action":BASIC,"parameter":EXPOSURE,"values":[PASSING_EV,MASKED_EV],"release":true}},
        // 13-14: a colour range intersected with both. Created with no swatches, which selects
        // nothing: the picture goes back to the one the mask never touched, and that is what an
        // unsampled colour range means rather than a component that does nothing.
        {"mask":{"mode":"intersect"}},
        {"mask":{"add":COLOUR_RANGE}},
        // 15-17: its row open, the host's own pick entered, and one click on the sky. **This is the
        // remedy frame**: the sky stays selected and the grey card comes back, which is the component
        // list doing the work the band cannot.
        {"mask":{"select_component":2}},
        {"mask":{"pick":true}},
        {"pick":{"x":sky[0],"y":sky[1]}},
        // 18: back to Mask mode. A pick is its own canvas mode and it **latches**, exactly as the
        // delivered neutral picker's does, so leaving it is a decision and not a side effect of having
        // clicked once. The strip is where a person leaves it and `workspace.set` is what the strip
        // sends.
        {"workspace":{"mode":"mask"}},
        // 19: a **global** exposure layer, from JSON with no mask in the request, which the host
        // places ahead of the masked one. **This is the input-dependence frame**: the band reads the
        // pixel its own operation receives, that pixel is now 12.8 units further up the axis, and the
        // sky is outside the band — so the masked layer stops applying to it entirely.
        {"api":{"method":"edit.set-basic","params":{"exposure":GLOBAL_EV}}},
        // 20: undone, and the selection comes back with the input it was drawn against.
        {"api":{"method":"history.undo","params":{}}},
        // 21: the overlay on with the pointer off the list, which asks for the **composed** mask's
        // grid. This mask reads pixels, so there is no grid: the step is captured with the host's own
        // reason on it rather than waiting out the run for a texture nothing will fill.
        {"workspace":{"mask_overlay":"mask-on-black"}},
        // 22: the pointer on the gradient's row, which asks for that one component's grid. A gradient
        // is a function of position, so it has one, and this frame is the only coverage a person can
        // see of this mask.
        {"mask":{"hover":1}},
        // 23-24: the pointer on each range component's row in turn. Each is refused, by name, for the
        // same reason the composition was.
        {"mask":{"hover":0}},
        {"mask":{"hover":2}},
        // 25: the overlay off, leaving the photograph.
        {"workspace":{"mask_overlay":"off"}},
        // 26-27: the band's own row open and the tools panel scrolled to it, so the frame carries
        // **the product's own statement of what a band cannot separate** where a person reads it:
        // above the four numbers it applies to, on the row they belong to. The statement is checked
        // in the state on every frame that has the row open; this is the one that shows it.
        {"mask":{"select_component":0}},
        {"tools_scroll":STATEMENT_SCROLL}
    ])
}

/// Launch 2, over the same catalog: the colour range's own limits, and the colour-constrained brush.
fn launch2_script() -> Value {
    let grey = pick_at("grey-card");
    let orange = pick_at("orange");
    let skin = pick_at("light-skin");
    json!([
        // 1: Mask mode, in a new process.
        {"workspace":{"mode":"mask"}},
        // 2-3: a second mask, one colour range, and an adjustment through it before anything is
        // sampled. The order is the host's rule and not a convenience: a pick reads the pixel the
        // operation this mask modulates receives, so a mask no layer is bound to is refused by name.
        // With no swatch the mask selects nothing, so this commits a layer and changes no pixel.
        {"mask":{"new":COLOUR_RANGE}},
        {"slider":{"action":BASIC,"parameter":EXPOSURE,"values":[PASSING_EV,MASKED_EV],"release":true}},
        // 4-6: the row open, the pick entered, and one click on the **grey card**. Every neutral is
        // one colour to a metric with no lightness term — white, the two greys above it and black are
        // mutually within `0.0015`, a third of the tightest radius — so one sampled grey selects the
        // whole tonal range and the frame shows five patches move at once.
        {"mask":{"select_component":0}},
        {"mask":{"pick":true}},
        {"pick":{"x":grey[0],"y":grey[1]}},
        // 7: back to Mask mode, because a pick latches. Every pick below is left the same way.
        {"workspace":{"mode":"mask"}},
        // 8-10: a second swatch, on the orange. A colour range folds its samples by nearest, which is
        // the same union the component list composes by, so the orange joins the selection and the
        // neutrals stay in it.
        {"mask":{"pick":true}},
        {"pick":{"x":orange[0],"y":orange[1]}},
        {"workspace":{"mode":"mask"}},
        // 11-13: a third swatch, on the grey card again. It is the colour the component already holds,
        // so it is exactly a no-op: the fold is by nearest sample and a duplicate changes no pixel's
        // coverage. The frame is the evidence that a picker misfire costs nothing.
        {"mask":{"pick":true}},
        {"pick":{"x":grey[0],"y":grey[1]}},
        {"workspace":{"mode":"mask"}},
        // 14-19: a third mask, its own adjustment, and one click on the **light skin** patch. Dark
        // skin is `0.0108` away in the frozen metric, a third of what one face's own shading spans,
        // so any setting that holds a lit face takes both — and the frame shows both move.
        {"mask":{"new":COLOUR_RANGE}},
        {"slider":{"action":BASIC,"parameter":EXPOSURE,"values":[PASSING_EV,MASKED_EV],"release":true}},
        {"mask":{"select_component":0}},
        {"mask":{"pick":true}},
        {"pick":{"x":skin[0],"y":skin[1]}},
        {"workspace":{"mode":"mask"}},
        // 20-23: the colour-constrained brush, in two halves. First an ordinary stroke across the
        // boundary between the bottom sky patch and the foliage beside it, and an adjustment through
        // it: one stroke, two surfaces, both selected.
        {"mask":{"brush":{"size":BRUSH_SIZE,"feather":BRUSH_HARD,"flow":100.0,"erase":false,"limit_to_colour":false}}},
        {"mask":{"paint":"new-mask"}},
        {"mask":{"stroke":{"points":[at("sky-bottom"),at("foliage")],"release":true}}},
        {"slider":{"action":BASIC,"parameter":EXPOSURE,"values":[PASSING_EV,MASKED_EV],"release":true}},
        // 24-26: then the same path erased back the other way, held to the colour under the brush
        // where the stroke begins — which is the foliage. The script sets a flag and never a colour:
        // the host reads the pixel the masked operation receives at the stroke's own first stored
        // position. **This is the constrained-brush frame**: the foliage comes out of the selection
        // and the sky the stroke also crossed stays in it, because the two are `0.116` apart in a
        // metric whose radius here is `0.035`.
        {"mask":{"brush":{"erase":true,"limit_to_colour":true}}},
        {"mask":{"paint":{"component":0}}},
        {"mask":{"stroke":{"points":[at("foliage"),at("sky-bottom")],"release":true}}},
        // 27: undone. The erase is one entry like any other stroke, so the foliage is selected again.
        {"api":{"method":"history.undo","params":{}}},
        // 28-30: one **paced** stroke, which is the measurement rather than a claim about pixels.
        // Every stroke above sends its whole path in one update, which is what a fast drag does and
        // what a correctness reading wants; this one sends a position every `STROKE_INTERVAL_MS` in
        // real time, so each is its own input with its own round trip and its own drafted frame. That
        // is the only way an end-to-end figure for a paint gesture exists at all: `editor-latency`
        // drives field-patch sliders, and a stroke is a different gesture.
        {"mask":{"brush":{"erase":false,"limit_to_colour":false}}},
        {"mask":{"paint":{"component":0}}},
        {"mask":{"stroke":{"points":paced_path(),"release":true,"interval_ms":STROKE_INTERVAL_MS}}},
        // 31: Mask mode left, which returns the tools panel and leaves every selection where it is.
        {"workspace":{"mode":"pointer"}}
    ])
}

fn revision(frame: &Value) -> Result<u64> {
    frame["state"]["stack"]["revision"]
        .as_u64()
        .ok_or_else(|| "Frame records no revision".into())
}

fn label(frame: &Value) -> Result<&str> {
    frame["state"]["stack"]["label"]
        .as_str()
        .ok_or_else(|| "Frame records no history label".into())
}

fn masks(frame: &Value) -> Result<&Vec<Value>> {
    frame["state"]["masks"]["masks"]
        .as_array()
        .ok_or_else(|| "Frame records no mask list".into())
}

/// Every component of the open mask, as the panel derived them.
fn components(frame: &Value) -> Result<&Vec<Value>> {
    frame["state"]["masks"]["components"]
        .as_array()
        .ok_or_else(|| "Frame records no component list".into())
}

fn component(frame: &Value, index: usize) -> Result<&Value> {
    components(frame)?
        .get(index)
        .ok_or_else(|| format!("The mask holds no component {index}").into())
}

/// The kinds and modes of the open mask's components, in list order.
fn kinds(frame: &Value) -> Result<Vec<String>> {
    Ok(components(frame)?
        .iter()
        .map(|component| {
            format!(
                "{} {}",
                component["mode"].as_str().unwrap_or_default(),
                component["kind"].as_str().unwrap_or_default()
            )
        })
        .collect())
}

/// One component's own declared geometry, as the **open row's number fields** report it. It is what a
/// person reads rather than the payload behind it, which is why it is empty on a closed row.
fn fields(frame: &Value, index: usize) -> Result<&Value> {
    Ok(&component(frame, index)?["fields"])
}

/// The colours one component has sampled, as the row lists them. A closed row lists none, exactly as
/// the panel draws it.
fn sample_count(frame: &Value, index: usize) -> Result<usize> {
    Ok(component(frame, index)?["samples"]
        .as_array()
        .ok_or("The component records no sample list")?
        .len())
}

/// What the open row says this kind does **not** select. It is the host's own sentence from the kind
/// table, and a frame that records none for a range component is a frame whose product said nothing.
fn limits(frame: &Value, index: usize) -> Result<Vec<String>> {
    Ok(component(frame, index)?["limits"]
        .as_array()
        .ok_or("The component records no limits")?
        .iter()
        .map(|line| line.as_str().unwrap_or_default().to_owned())
        .collect())
}

/// The colours one component has sampled, as the panel lists them on its row.
fn samples(frame: &Value, index: usize) -> Result<Vec<String>> {
    Ok(component(frame, index)?["samples"]
        .as_array()
        .ok_or("The component records no sample list")?
        .iter()
        .map(|sample| sample["text"].as_str().unwrap_or_default().to_owned())
        .collect())
}

/// Every Basic layer of the stack, with the mask each is bound to, in the durable processing order.
fn basic_layers(frame: &Value) -> Vec<(String, Value)> {
    frame["state"]["stack"]["layers"]
        .as_array()
        .map(|layers| {
            layers
                .iter()
                .filter(|layer| layer["effect"] == json!(lightwell_core::BASIC_EFFECT))
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

/// The photograph's own drawn rectangle inside the capture.
///
/// Found on the frame the launch opened with and reused for every frame beside it, exactly as
/// `mask-brush` does: the zoom and the panels do not move between them, and a later frame has patches
/// this scenario has deliberately darkened below the threshold a bounds scan uses.
///
/// The vertical extent is taken first and the horizontal one only among the photograph's own rows,
/// for the reason `vignette` records: the mode strip is a bright floating bar over the same surface
/// and a scan of every row measures whichever of the two happens to be wider.
fn photo_bounds(path: &Path, frame: &Value) -> Result<[u32; 4]> {
    /// Above the brightest chrome on the canvas — the floating bars read 36 — and below the fixture's
    /// darkest patch, which is the chart's black at 52.
    const BRIGHT: u32 = 45;
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [surface_left, surface_right] = columns(frame)?.unwrap_or([0, width]);
    ensure(
        surface_left < surface_right && surface_right <= width,
        "Invalid surface columns",
    )?;
    let bright = |p: [u8; 3]| (u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) / 3 >= BRIGHT;
    let top_margin = (height / 20).max(20);
    let bottom_margin = height - top_margin;
    let (inset_left, inset_right) = (surface_left + 10, surface_right - 10);
    let mut tallest: Option<(u32, u32, u32)> = None;
    for x in inset_left..inset_right {
        if let Some((top, bottom)) = smoke::longest_run(
            (top_margin..bottom_margin).map(|y| (y, bright(image.get_pixel(x, y).0))),
        ) && tallest.is_none_or(|(h, ..)| bottom - top > h)
        {
            tallest = Some((bottom - top, top, bottom));
        }
    }
    let (_, top, bottom) = tallest.ok_or("No photograph in the frame: blank or wrong render")?;
    let mut widest: Option<(u32, u32, u32)> = None;
    for y in top..bottom {
        if let Some((left, right)) = smoke::longest_run(
            (inset_left..inset_right).map(|x| (x, bright(image.get_pixel(x, y).0))),
        ) && widest.is_none_or(|(w, ..)| right - left > w)
        {
            widest = Some((right - left, left, right));
        }
    }
    let (_, left, right) = widest.ok_or("No photograph in the frame: blank or wrong render")?;
    ensure(
        right - left > 200 && bottom - top > 100,
        format!("Photograph too small to measure: {left}..{right}, {top}..{bottom}"),
    )?;
    Ok([left, top, right, bottom])
}

/// Mean Rec. 709 luminance of one small patch of the displayed photograph.
fn patch(path: &Path, bounds: [u32; 4], at: [f64; 2]) -> Result<f64> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [left, top, right, bottom] = bounds;
    let px = f64::from(left) + at[0] * f64::from(right - left);
    let py = f64::from(top) + at[1] * f64::from(bottom - top);
    let mut total = 0.0;
    let mut count = 0u32;
    for dy in -PATCH_HALF..=PATCH_HALF {
        for dx in -PATCH_HALF..=PATCH_HALF {
            let x = (px as i64 + dx).clamp(0, i64::from(width) - 1) as u32;
            let y = (py as i64 + dy).clamp(0, i64::from(height) - 1) as u32;
            let p = image.get_pixel(x, y).0;
            total += 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
            count += 1;
        }
    }
    ensure(count > 0, "Sampled no pixels")?;
    Ok(total / f64::from(count))
}

/// Every one of the fixture's twelve patches in one capture, by name.
fn read(path: &Path, bounds: [u32; 4]) -> Result<Vec<(String, f64)>> {
    let mut out = Vec::new();
    for ((name, _), at) in RANGE_PATCHES.iter().zip(probes()) {
        out.push(((*name).to_owned(), patch(path, bounds, at)?));
    }
    Ok(out)
}

fn reading(read: &[(String, f64)], name: &str) -> Result<f64> {
    read.iter()
        .find(|(held, _)| held == name)
        .map(|(_, value)| *value)
        .ok_or_else(|| format!("No patch called {name} was read").into())
}

/// One patch moved between two captures by at least the margin a selected patch must move by.
fn moved(what: &str, after: f64, before: f64) -> Result {
    ensure(
        (after - before).abs() >= MOVED,
        format!("{what}: {after:.2} did not move from {before:.2} by {MOVED}"),
    )
}

/// One patch is where it was, within renderer-readback noise.
fn untouched(what: &str, after: f64, before: f64) -> Result {
    ensure(
        (after - before).abs() <= UNTOUCHED,
        format!("{what}: {after:.2} moved from {before:.2} by more than {UNTOUCHED}"),
    )
}

/// Two patches of one capture read the same, within the same noise. This is how every claim about
/// what a selection *stopped* selecting is made: against a control of the identical colour in the
/// same frame, rather than against a predicted output code.
fn same(what: &str, left: f64, right: f64) -> Result {
    ensure(
        (left - right).abs() <= UNTOUCHED,
        format!("{what}: {left:.2} and {right:.2} differ by more than {UNTOUCHED}"),
    )
}

fn differ(what: &str, left: f64, right: f64) -> Result {
    ensure(
        (left - right).abs() >= MOVED,
        format!("{what}: {left:.2} and {right:.2} differ by less than {MOVED}"),
    )
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
        let wanted = selected.contains(&name.as_str());
        if (value - was).abs() >= MOVED {
            found.push(name.clone());
        }
        if wanted {
            moved(&format!("{what}: {name}"), *value, was)?;
        } else {
            untouched(&format!("{what}: {name}"), *value, was)?;
        }
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
fn masked_recipe(app: &Value) -> Result<Value> {
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    let last = frames.last().ok_or("The launch wrote no frames")?;
    let masks = last["state"]["masks"]["masks"]
        .as_array()
        .ok_or("The last frame records no mask list")?;
    let layers = last["state"]["stack"]["layers"]
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

/// One script step's own record: whether the editor refused it, and in whose words.
fn step_refusal(app: &Value, index: usize) -> Result<Option<String>> {
    let step = app["script"]
        .as_array()
        .ok_or("The run recorded no script")?
        .get(index)
        .ok_or_else(|| format!("The run recorded no step {index}"))?;
    if step["status"] != json!("failed") {
        return Ok(None);
    }
    Ok(Some(
        step["reason"]
            .as_str()
            .ok_or("A failed step recorded no reason")?
            .to_owned(),
    ))
}

/// Every step the editor refused, by its position in the script, so a scenario that deliberately
/// captures a refusal still fails on one it did not mean to capture.
fn refusals(app: &Value) -> Result<Vec<(usize, String)>> {
    let steps = app["script"].as_array().ok_or("No script")?;
    let mut out = Vec::new();
    for index in 0..steps.len() {
        if let Some(reason) = step_refusal(app, index)? {
            out.push((index, reason));
        }
    }
    Ok(out)
}

/// The refused steps are exactly the ones named, and each says what it was expected to say.
fn only_refusals(app: &Value, expected: &[(usize, &str)]) -> Result<Vec<Value>> {
    let found = refusals(app)?;
    ensure(
        found.len() == expected.len()
            && found
                .iter()
                .zip(expected)
                .all(|((index, _), (wanted, _))| index == wanted),
        format!(
            "The run refused steps {:?}, expected exactly {:?}",
            found.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            expected.iter().map(|(index, _)| *index).collect::<Vec<_>>()
        ),
    )?;
    for ((index, reason), (_, wanted)) in found.iter().zip(expected) {
        ensure(
            reason.contains(wanted),
            format!("Step {index} was refused with {reason:?}, which does not say {wanted:?}"),
        )?;
    }
    Ok(found
        .into_iter()
        .map(|(index, reason)| json!({"step":index,"reason":reason}))
        .collect())
}

/// Every answer the overlay got while the run was going, in order: a coverage grid that was painted
/// and uploaded, or one the host refused with its reason.
///
/// It is read from the events rather than from the frames because the grid is not part of the state
/// summary — what a frame carries is the photograph with the overlay drawn over it, which is what the
/// pixel readings below check. These are the host's own record of *why* one frame has an overlay and
/// three do not.
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

/// The whole scenario: two launches over one catalog, checked together.
pub fn run(root: &Path, out: &Path, bin: &Path, timeout: Duration) -> Result {
    ensure(!out.exists(), "Smoke output must be new")?;
    fs::create_dir_all(out)?;
    let fixture = root.join(FIXTURE);
    ensure(
        fixture.is_file(),
        format!("{FIXTURE} is missing; run `cargo xtask generate-fixtures`"),
    )?;
    let launch1 = out.join("launch1");
    let launch2 = out.join("launch2");
    let mut result = json!({"scenario":SCENARIO,"status":"failed","launch_mode":launch::MODE,"platform":format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH)});
    let check = (|| -> Result {
        result["fixture_hash"] = json!(hash(&fixture)?);
        result["binary_sha256"] = json!(hash(bin)?);
        result["lockfile_sha256"] = json!(hash(&root.join("Cargo.lock"))?);

        let script1 = out.join("script1.json");
        write_json(&script1, &launch1_script())?;
        let args1: Vec<OsString> = vec![
            "--evidence-dir".into(),
            launch1.clone().into_os_string(),
            "--open".into(),
            fixture.clone().into_os_string(),
            "--evidence-script".into(),
            script1.into_os_string(),
            "--window-size".into(),
            WINDOW[0].into(),
            WINDOW[1].into(),
        ];
        let mut child1 = smoke::spawn_editor(root, bin, &args1, &out.join("launch1.log"))?;
        let status1 = smoke::wait(&mut child1, timeout)?;
        result["launch1_exit_code"] = json!(status1.code());
        ensure(status1.success(), format!("Launch 1 exit {status1}"))?;
        let (app1, events1) = smoke::preamble(&launch1, LAUNCH1_FRAMES)?;
        let checks = verify_launch1(&launch1, &app1, &events1)?;
        result["launch1"] = checks.clone();

        let catalog = launch1.join("catalog.sqlite");
        ensure(catalog.is_file(), "Launch 1 wrote no catalog")?;
        let script2 = out.join("script2.json");
        write_json(&script2, &launch2_script())?;
        let args2: Vec<OsString> = vec![
            "--catalog".into(),
            catalog.into_os_string(),
            "--evidence-dir".into(),
            launch2.clone().into_os_string(),
            "--open".into(),
            fixture.clone().into_os_string(),
            "--evidence-script".into(),
            script2.into_os_string(),
            "--window-size".into(),
            WINDOW[0].into(),
            WINDOW[1].into(),
        ];
        let mut child2 = smoke::spawn_editor(root, bin, &args2, &out.join("launch2.log"))?;
        let status2 = smoke::wait(&mut child2, timeout)?;
        result["launch2_exit_code"] = json!(status2.code());
        ensure(status2.success(), format!("Launch 2 exit {status2}"))?;
        let (app2, events2) = smoke::preamble(&launch2, LAUNCH2_FRAMES)?;
        result["launch2"] = verify_launch2(&launch2, &app2, &checks)?;
        // The stroke's own end-to-end latency, from the events of the launch that painted one, with
        // the recipe it was painted on and the load the host was under. It is a smoke-run figure over
        // one fixture and is labelled as such where it is recorded; the interval is the same one
        // `editor-latency` measures for a slider.
        let recipe = masked_recipe(&app2)?;
        result["stroke_latency"] = stroke_latency(root, &events2, recipe)?;
        ensure(
            json!(hash(&fixture)?) == result["fixture_hash"],
            "Source changed",
        )?;
        ensure(!events1.is_empty() && !events2.is_empty(), "No event log")?;
        write_json(&out.join("mask-range-checks.json"), &result)?;
        Ok(())
    })();
    match &check {
        Ok(()) => result["status"] = json!("passed"),
        Err(error) => result["error"] = json!(error.to_string()),
    };
    write_json(&out.join("result.json"), &result)?;
    fs::write(
        out.join("reproduce.md"),
        format!(
            "# Smoke run\n\nScenario: {SCENARIO}. Status: {}.\n\nLaunch mode: {}. Reproduce with `cargo xtask generate-fixtures --output fixtures/generated` then `cargo xtask smoke --scenario {SCENARIO} --output NEW_DIR --binary PATH`; on macOS each launch runs hidden in a background-only bundle, so no window is ever placed on the desktop.\n\nTwo launches over one catalog: the first types a luminance band, intersects a gradient and a picked colour range with it, shows a grey card taken along with the sky and then given back, shows a layer ahead of the mask stop the selection altogether, and asks each component for the overlay a range component does not have; the second takes the colour range's own limits one at a time and finishes with a colour-held erase across two surfaces.\n\nActual renderer readback. Synthetic fixtures only: the patches are the 24-patch reflective colour chart's own sRGB renderings, which is what `docs/design/range-study.md` measured over.\n",
            result["status"],
            launch::MODE
        ),
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    check
}

/// A painted stroke's own control-to-frame latency, paired from the run's events.
///
/// The pairing is exact rather than by order: every `mask_draft_set` is answered by one
/// `mask_draft_preview` carrying the preview generation it queued, and `preview_displayed` repeats
/// that generation. A gesture holds one round trip at a time, so a set with no answer between it and
/// the next one is a fault and not a measurement.
///
/// **What this figure is not.** It is one stroke on one 1.4 MP fixture during a smoke run, not an
/// `editor-latency` baseline: that harness drives field-patch sliders over the 24 MP and 60 MP
/// fixtures and a stroke is a different gesture. It is also taken on the **heaviest** recipe this
/// scenario builds — four masked colour layers, three of them holding a component that reads pixels
/// and therefore bounds the whole stage — which is recorded beside it, because that is most of what
/// the number is. The one-minute load average is recorded too, since a figure taken above `8.0` is
/// provisional by the repository's own rule.
fn stroke_latency(root: &Path, events: &[Value], recipe: Value) -> Result<Value> {
    // The pairing itself lives in `editor_latency`, beside the `--mode paint` run that takes the same
    // measurement on a bare recipe at 24 and 60 MP, so the two figures are one definition and not
    // two implementations that could drift apart.
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

/// Launch 1, frame by frame.
fn verify_launch1(evidence: &Path, app: &Value, events: &[Value]) -> Result<Value> {
    let frames = app["frames"].as_array().ok_or("Missing frames")?.clone();
    ensure(
        frames.len() == LAUNCH1_FRAMES,
        format!("Launch 1 wrote {} frames", frames.len()),
    )?;
    // Three steps are refused on purpose, and nothing else is: the overlay of a mask that reads
    // pixels, and each of its two range components' own. The set is pinned by position and by the
    // host's own words, so a refusal this scenario did not mean to capture still fails the run.
    let refused = only_refusals(
        app,
        &[
            (20, "depends on the pixel it reads"),
            (22, "depends on the pixel it reads"),
            (23, "depends on the pixel it reads"),
        ],
    )?;
    let paths: Vec<PathBuf> = frames
        .iter()
        .map(|frame| frame_identity(evidence, app, frame))
        .collect::<Result<Vec<_>>>()?;
    let bounds = photo_bounds(&paths[0], &frames[0])?;
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // Frame 0: the fixture as launched, with no mask in the recipe. Every reading below is against
    // this one.
    ensure(
        masks(&frames[0])?.is_empty(),
        "The fixture opened with a mask already in the recipe",
    )?;
    let opened = read(&paths[0], bounds)?;
    // The two sky patches are the same colour, which is what makes every control comparison below a
    // comparison and not a prediction.
    same(
        "the fixture's two sky patches",
        reading(&opened, "sky-top")?,
        reading(&opened, "sky-bottom")?,
    )?;
    record(
        &frames[0],
        "the range fixture as launched: twelve flat chart patches, no mask in the recipe",
        json!({"patches":opened.clone(),"bounds":bounds}),
    );

    // Frame 2: the band created by its own button. A typed kind: one history entry, no gesture, and
    // the whole tonal range with soft shoulders, which selects the picture.
    ensure(
        revision(&frames[2])? == revision(&frames[0])? + 1,
        "Creating a luminance range did not commit one entry",
    )?;
    ensure(
        frames[2]["state"]["mask_draft"] == Value::Null,
        "A typed kind opened a gesture",
    )?;
    ensure(
        kinds(&frames[2])? == ["add luminance-range"],
        format!("The button made {:?}", kinds(&frames[2])?),
    )?;
    // What it was created *as* is read from frame 3, where the row is open: a closed row shows no
    // numbers, which is the panel's own behaviour and not an omission here.
    record(
        &frames[2],
        "a luminance range created by its button in one entry: the whole tonal range with soft \
         shoulders, which is the picture",
        json!({"label":label(&frames[2])?,"kinds":kinds(&frames[2])?}),
    );

    // Frame 3: the row open. This is where the product says what a band cannot do, before a person
    // has typed a number into it.
    let fresh = fields(&frames[3], 0)?.clone();
    ensure(
        fresh["low"] == json!(0.0) && fresh["high"] == json!(100.0),
        format!("A new band starts at {fresh}"),
    )?;
    let said = limits(&frames[3], 0)?;
    ensure(
        said.len() == 2
            && said[0].contains("output codes apart")
            && said[1].contains("the 100% view is the truth"),
        format!("The open row said {said:?}"),
    )?;
    record(
        &frames[3],
        "the band's own row, with the host's statement of what brightness alone cannot separate \
         above the numbers it applies to",
        json!({"limits":said.clone(),"fields":fresh}),
    );

    // Frame 7: the band typed, one field per entry. Four entries, four numbers, nothing dragged.
    ensure(
        revision(&frames[7])? == revision(&frames[2])? + 4,
        format!(
            "Typing four fields moved the revision to {}",
            revision(&frames[7])?
        ),
    )?;
    let band = fields(&frames[7], 0)?.clone();
    ensure(
        band == json!({"low":BAND_LOW,"low_feather":BAND_FEATHER,"high":BAND_HIGH,
                       "high_feather":BAND_FEATHER}),
        format!("The typed band reads {band}"),
    )?;
    record(
        &frames[7],
        "the band narrowed onto the fixture's sky by its own declared fields, one entry each",
        json!({"fields":band.clone(),"label":label(&frames[7])?}),
    );

    // Frame 11: the gradient committed, intersected with the band.
    ensure(
        kinds(&frames[11])? == ["add luminance-range", "intersect linear"],
        format!("The mask holds {:?}", kinds(&frames[11])?),
    )?;
    ensure(
        limits(&frames[11], 1)?.is_empty(),
        "A gradient claimed a limit a position-based component does not have",
    )?;
    record(
        &frames[11],
        "a linear gradient intersected with the band, so the fixture's top row is inside the mask \
         and its bottom row outside it",
        json!({"kinds":kinds(&frames[11])?,"label":label(&frames[11])?,
               "limits":limits(&frames[11],1)?}),
    );

    // Frame 12: one stop down through the mask. **The failure.** The band was drawn for the sky and
    // takes the grey card beside it; the bottom sky patch, the same colour outside the gradient, does
    // not move at all.
    let failure = read(&paths[12], bounds)?;
    let took = only(
        "a band drawn for the sky",
        &failure,
        &opened,
        &["sky-top", "grey-card"],
    )?;
    differ(
        "the selected sky against its own control outside the gradient",
        reading(&failure, "sky-top")?,
        reading(&failure, "sky-bottom")?,
    )?;
    record(
        &frames[12],
        "one stop down through the band: it takes the grey card as well as the sky it was drawn for, \
         because the two are 1.4 output codes apart on the axis a band measures",
        json!({"moved":took,"sky_top":reading(&failure,"sky-top")?,
               "grey_card":reading(&failure,"grey-card")?,
               "sky_bottom_control":reading(&failure,"sky-bottom")?,"patches":failure.clone()}),
    );

    // Frame 14: an unsampled colour range intersected in. It selects nothing, so the whole picture is
    // back to the one the mask never touched — which is what an empty swatch list means.
    ensure(
        kinds(&frames[14])?
            == [
                "add luminance-range",
                "intersect linear",
                "intersect colour-range",
            ],
        format!("The mask holds {:?}", kinds(&frames[14])?),
    )?;
    ensure(
        sample_count(&frames[14], 2)? == 0,
        "A new colour range already holds a swatch",
    )?;
    let unsampled = read(&paths[14], bounds)?;
    only("an unsampled colour range", &unsampled, &opened, &[])?;
    record(
        &frames[14],
        "an unsampled colour range intersected in: it selects nothing, so the masked layer reaches \
         no pixel at all",
        json!({"kinds":kinds(&frames[14])?,"patches":unsampled.clone()}),
    );

    // Frame 17: one click on the sky. **The remedy.** The sky is selected again and the grey card is
    // not, which is the component list doing what the band cannot.
    let picked = samples(&frames[17], 2)?;
    ensure(
        picked.len() == 1,
        format!("The pick left {picked:?} on the row"),
    )?;
    let remedy = read(&paths[17], bounds)?;
    let held = only(
        "a colour range picked on the sky",
        &remedy,
        &opened,
        &["sky-top"],
    )?;
    untouched(
        "the grey card once the colour range is intersected in",
        reading(&remedy, "grey-card")?,
        reading(&opened, "grey-card")?,
    )?;
    record(
        &frames[17],
        "the sky sampled off the photograph: the selection is the sky alone and the grey card is back \
         where it started",
        json!({"moved":held,"samples":picked.clone(),
               "sky_top":reading(&remedy,"sky-top")?,
               "grey_card":reading(&remedy,"grey-card")?,"patches":remedy.clone()}),
    );

    // Frame 19: a **global** exposure layer, placed ahead of the masked one. The band reads the pixel
    // its own operation receives, that pixel has moved 12.8 units up the axis, and the sky is outside
    // the band: the masked layer stops reaching it. Read as an equality against the control patch of
    // the identical colour, so nothing here is a predicted output code.
    let layers = basic_layers(&frames[19]);
    ensure(
        layers.len() == 2 && layers[0].1 == Value::Null && layers[1].1 != Value::Null,
        format!("The stack holds Basic layers {layers:?}"),
    )?;
    let reordered = read(&paths[19], bounds)?;
    same(
        "the sky under a +0.75 EV layer ahead of the band",
        reading(&reordered, "sky-top")?,
        reading(&reordered, "sky-bottom")?,
    )?;
    moved(
        "the sky itself under the global layer",
        reading(&reordered, "sky-top")?,
        reading(&remedy, "sky-top")?,
    )?;
    record(
        &frames[19],
        "a +0.75 EV layer ahead of the masked one: the band no longer selects the sky at all, so the \
         selected patch and its unselected control of the same colour read the same",
        json!({"layers":layers,"sky_top":reading(&reordered,"sky-top")?,
               "sky_bottom_control":reading(&reordered,"sky-bottom")?,
               "patches":reordered.clone()}),
    );

    // Frame 20: undone. The selection is back with the input it was drawn against, byte for byte the
    // picture the pick produced.
    let undone = read(&paths[20], bounds)?;
    for (name, value) in &undone {
        untouched(
            &format!("{name} after the global layer was undone"),
            *value,
            reading(&remedy, name)?,
        )?;
    }
    record(
        &frames[20],
        "the global layer undone: the selection reads what it read before, because its input does",
        json!({"patches":undone.clone(),"label":label(&frames[20])?}),
    );

    // The four answers the overlay got, in order: the composed mask refused, the gradient's own grid
    // painted and uploaded, and each range component's refused. One drawn of four asked for is the
    // whole of what a mask holding a range component can show, and it is the gap P16 names.
    let answers = overlay_answers(events);
    ensure(
        answers.len() == 4
            && answers[0]["drawn"] == json!(false)
            && answers[1]["drawn"] == json!(true)
            && answers[2]["drawn"] == json!(false)
            && answers[3]["drawn"] == json!(false),
        format!("The overlay answered {}", json!(answers)),
    )?;
    ensure(
        answers[1]["component"] == component(&frames[22], 1)?["id"],
        format!(
            "The one grid painted names {}, and the gradient's row is {}",
            answers[1]["component"],
            component(&frames[22], 1)?["id"]
        ),
    )?;

    // Frame 21: the composed mask has no overlay. The photograph is still the photograph — a refused
    // grid leaves the picture alone rather than drawing a black frame or an empty texture — and the
    // reason travels with the step.
    let refused_frame = read(&paths[21], bounds)?;
    for (name, value) in &refused_frame {
        untouched(
            &format!("{name} with the overlay asked for and refused"),
            *value,
            reading(&undone, name)?,
        )?;
    }
    record(
        &frames[21],
        "the overlay asked for on the composed mask and refused: there is no grid for a selection \
         evaluated on the operation's input, so the photograph is left exactly as it was and the \
         reason travels with the frame",
        json!({"reason":refused[0]["reason"].clone(),"patches":refused_frame.clone()}),
    );

    // Frame 22: the gradient's own row. A position-based component has a grid, and `mask-on-black`
    // paints it as an opaque greyscale, so this is read in the pixels: the fixture's top row is inside
    // the gradient and reads white, its bottom row is outside and reads black, and the row between
    // them is on the ramp and reads between the two. It is the only coverage of this mask a person
    // can see.
    let drawn = read(&paths[22], bounds)?;
    ensure(
        component(&frames[22], 1)?["hovered"] == json!(true),
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
        &frames[22],
        "the gradient's own contribution, drawn on black: the one component of this mask whose \
         coverage is a function of position, full over the top row, on its ramp in the middle and \
         nothing over the bottom row",
        json!({"patches":drawn.clone(),
               "hovered":component(&frames[22],1)?["hovered"].clone()}),
    );

    // Frames 23-24: each range component's own row. Each is refused for the same reason the
    // composition was, and each leaves the photograph alone.
    for frame in [23usize, 24] {
        let left = read(&paths[frame], bounds)?;
        for (name, value) in &left {
            untouched(
                &format!("{name} with a range component's own overlay refused"),
                *value,
                reading(&undone, name)?,
            )?;
        }
    }
    record(
        &frames[24],
        "each range component's own overlay refused in turn, by name and for the same reason: a \
         value-based selection is not a function of position over the finished frame",
        json!({"reasons":[refused[1]["reason"].clone(),refused[2]["reason"].clone()]}),
    );

    // Frame 27: the statement itself, on screen. The row is open and the panel is scrolled to it, so
    // what a person reads before typing a number into a band is in a capture and not only in a model.
    ensure(
        limits(&frames[27], 0)? == said,
        format!("The row on screen says {:?}", limits(&frames[27], 0)?),
    )?;
    record(
        &frames[27],
        "the product's own statement of what brightness alone cannot separate, on the band's row and \
         above the four numbers it applies to",
        json!({"limits":limits(&frames[27],0)?,"fields":fields(&frames[27],0)?.clone(),
               "scroll":frames[27]["state"]["tools_scroll"].clone()}),
    );

    Ok(json!({
        "kinds": kinds(&frames[25])?,
        "opened": opened,
        "failure": failure,
        "remedy": remedy,
        "revision": revision(&frames[25])?,
        "refusals": refused,
        "overlay_answers": answers,
        "limits": said,
        "moved_threshold": MOVED,
        "untouched_threshold": UNTOUCHED,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of the twelve fixture patches in the displayed photograph, read back from the renderer with no JPEG between; every claim about what a selection stopped selecting is an equality against a control patch of the same colour in the same frame, and none is a colorimetric claim",
    }))
}

/// Launch 2: the colour range's own limits, and the colour-constrained brush.
fn verify_launch2(evidence: &Path, app: &Value, launch1: &Value) -> Result<Value> {
    let frames = app["frames"].as_array().ok_or("Missing frames")?.clone();
    ensure(
        frames.len() == LAUNCH2_FRAMES,
        format!("Launch 2 wrote {} frames", frames.len()),
    )?;
    only_refusals(app, &[])?;
    let paths: Vec<PathBuf> = frames
        .iter()
        .map(|frame| frame_identity(evidence, app, frame))
        .collect::<Result<Vec<_>>>()?;
    let bounds = photo_bounds(&paths[0], &frames[0])?;
    let mut shows = Vec::new();
    let mut record = |frame: &Value, what: &str, detail: Value| {
        shows.push(json!({"frame":frame["file"],"shows":what,"detail":detail}));
    };

    // Frame 0: the reopened catalog. Launch 1's mask and its selection are back, in the pixels and
    // not only in the rows.
    let reopened = read(&paths[0], bounds)?;
    let remedy: Vec<(String, f64)> = serde_json::from_value(launch1["remedy"].clone())?;
    for (name, value) in &reopened {
        untouched(
            &format!("{name} after the catalog was reopened"),
            *value,
            reading(&remedy, name)?,
        )?;
    }
    record(
        &frames[0],
        "the catalog reopened in a new process: the band, the gradient and the picked colour range \
         produce the same photograph they produced before it closed",
        json!({"patches":reopened.clone(),"masks":masks(&frames[0])?.len()}),
    );

    // Frame 6: a sampled grey. **Every neutral is one colour** to a metric with no lightness term, so
    // one click selects the whole tonal range and five patches move at once.
    let neutral = read(&paths[6], bounds)?;
    let neutrals = only(
        "a colour range sampled on a grey card",
        &neutral,
        &reopened,
        &["grey-card", "white", "grey-65", "black", "grey-8"],
    )?;
    record(
        &frames[6],
        "one click on the grey card: white, both other greys and black move with it, because every \
         neutral is within 0.0015 of every other in a metric with no lightness term",
        json!({"moved":neutrals,"samples":samples(&frames[6],0)?,"patches":neutral.clone()}),
    );

    // Frame 9: a second swatch. The fold is by nearest sample, which is the same union the component
    // list composes by, so the orange joins and the neutrals stay.
    let two = read(&paths[9], bounds)?;
    only("a second swatch on the orange", &two, &neutral, &["orange"])?;
    ensure(
        samples(&frames[9], 0)?.len() == 2,
        format!("The row lists {:?}", samples(&frames[9], 0)?),
    )?;
    record(
        &frames[9],
        "a second swatch: the orange joins the selection and every neutral stays in it",
        json!({"samples":samples(&frames[9],0)?,"patches":two.clone()}),
    );

    // Frame 12: the same patch sampled again. The duplicate is dropped **before it is stored**, so
    // the row keeps its two swatches, no entry is appended and no pixel moves. That is stronger than
    // storing a second copy that happened to change nothing: one of the component's five swatches is
    // not spent on a colour it already holds.
    let three = read(&paths[12], bounds)?;
    only("a duplicate swatch", &three, &two, &[])?;
    ensure(
        samples(&frames[12], 0)? == samples(&frames[9], 0)?,
        format!(
            "The row lists {:?}, and it listed {:?}",
            samples(&frames[12], 0)?,
            samples(&frames[9], 0)?
        ),
    )?;
    ensure(
        revision(&frames[12])? == revision(&frames[9])?,
        format!(
            "The duplicate moved the revision to {}",
            revision(&frames[12])?
        ),
    )?;
    record(
        &frames[12],
        "a third click on a colour the component already holds: the same two swatches, no history \
         entry and not one pixel different, so a misfired picker costs nothing and spends no swatch",
        json!({"samples":samples(&frames[12],0)?,"revision":revision(&frames[12])?,
               "patches":three.clone()}),
    );

    // Frame 18: one person's skin. **Two people's skin is one colour**: dark skin is 0.0108 from
    // light skin, a third of what one face's own shading spans, so any setting that holds a lit face
    // takes both.
    let skin = read(&paths[18], bounds)?;
    let both = only(
        "a colour range sampled on light skin",
        &skin,
        &three,
        &["light-skin", "dark-skin"],
    )?;
    record(
        &frames[18],
        "one click on the light skin patch: the dark skin patch moves with it, because the two are \
         0.0108 apart in a metric whose radius here is 0.035",
        json!({"moved":both,"samples":samples(&frames[18],0)?,"patches":skin.clone()}),
    );

    // Frame 23: one ordinary stroke across two surfaces, with an adjustment through it. Both move,
    // which is what a brush without a colour limit does.
    let painted = read(&paths[23], bounds)?;
    only(
        "an unlimited stroke across a sky and a foliage patch",
        &painted,
        &skin,
        &["sky-bottom", "foliage"],
    )?;
    record(
        &frames[23],
        "one stroke across the boundary between two surfaces: a brush selects where it is drawn, so \
         both of them",
        json!({"patches":painted.clone(),"kinds":kinds(&frames[23])?}),
    );

    // Frame 26: the same path erased back, held to the colour under the brush where the stroke began.
    // **The constrained brush.** The foliage it was seeded on comes out of the selection; the sky the
    // stroke crossed just as far stays in it.
    let constrained = read(&paths[26], bounds)?;
    only(
        "a colour-held erase seeded on the foliage",
        &constrained,
        &painted,
        &["foliage"],
    )?;
    untouched(
        "the sky the held erase also crossed",
        reading(&constrained, "sky-bottom")?,
        reading(&painted, "sky-bottom")?,
    )?;
    same(
        "the foliage after the held erase against its own unmasked reading",
        reading(&constrained, "foliage")?,
        reading(&reopened, "foliage")?,
    )?;
    record(
        &frames[26],
        "a colour-held erase along the same path: the foliage it was seeded on leaves the selection \
         and the sky the stroke crossed just as far stays in it",
        json!({"patches":constrained.clone(),"strokes":component(&frames[26],0)?["strokes"].clone(),
               "foliage":reading(&constrained,"foliage")?,
               "sky_bottom":reading(&constrained,"sky-bottom")?}),
    );

    // Frame 27: undone. The held erase is one entry like every other stroke.
    let undone = read(&paths[27], bounds)?;
    only("the held erase undone", &undone, &constrained, &["foliage"])?;
    record(
        &frames[27],
        "the held erase undone: one stroke is one entry whether or not it was held to a colour",
        json!({"patches":undone.clone(),"label":label(&frames[27])?}),
    );

    Ok(json!({
        "reopened": reopened,
        "neutral": neutral,
        "skin": skin,
        "constrained": constrained,
        "revision": revision(&frames[28])?,
        "frames": shows,
        "scope": "Mean Rec. 709 luminance of the twelve fixture patches in the displayed photograph, read back from the renderer; every comparison is against the frame before it in the same launch, and none is a colorimetric claim",
    }))
}
