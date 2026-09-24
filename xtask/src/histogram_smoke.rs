//! The `histogram` smoke scenario: the inspector, the clipping overlays and the pointer readout.
//!
//! The fixture is the ordinary `orientation-1` pattern, whose clipped pixels are known from the
//! generator rather than guessed: four quadrant colours with no channel at an endpoint, a white
//! centre line and arrow (every channel at 255), and a band of black dashes across the middle
//! (every channel at 0). Nothing in it has one channel at 0 and another at 255, so the scenario
//! commits one `edit.set-pixel` of `(0, 128, 255)` to make exactly one both-endpoint pixel — which
//! is also the isolated-clipped-pixel case the contract asks the Fit overlay to survive.
//!
//! Every count a frame reports is checked against `analysis::reduce_raster` of an **independent**
//! core render of the same fixture through the same recipe, so the plot is verified against the
//! reducer rather than against itself — and so are the words the triangles' tooltips state them in.
//!
//! The inspector is the plot and the triangle row and nothing else; the pointer readout is in the
//! status bar. The hover frame is compared with the frame before it pixel for pixel: the tools
//! panel is identical, and the status bar changes only inside the readout's own slot.
use crate::{
    smoke::{Expect, columns, frame_identity, pixels},
    *,
};
use lightwell_core::{
    BASIC_EFFECT, EFFECT_FORMAT, Layer, LayerId, ModuleRegistry, RECIPE_FORMAT, Recipe, SnapshotId,
    analysis, render as core_render,
};

/// The fixture: 480x320, orientation 1, the quadrant pattern with the white centre line and the
/// black dash band.
const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
pub const WINDOW: [&str; 2] = ["1440", "900"];
const SOURCE: (u32, u32) = (480, 320);

/// The content-stage pixel the scenario sets to a both-endpoint colour, in the middle of the gold
/// quadrant and far from every other clipped pixel in the fixture, so its overlay cell is isolated.
const BOTH_PIXEL: (u32, u32) = (360, 240);
/// Its colour: one channel at 0 and one at 255, which is the both-endpoint class exactly.
const BOTH_RGB: [u8; 3] = [0, 128, 255];

/// The rule both triangles state on hover, as the contract words it; the counts follow it.
const RULE: &str =
    "Any channel at 0 \u{b7} blue; any at 255 \u{b7} red; both endpoints \u{b7} magenta";

/// What the plot states on hover: the domain the counts describe.
const DOMAIN: &str = "Output \u{b7} sRGB \u{b7} after crop";

/// The editor's layout in points, which a capture's recorded scale turns into physical rows and
/// columns: the title bar and the status bar, each with its 1 pt rule, and the width of the status
/// bar's readout slot.
const TITLE_BAR_PT: f64 = 44.0;
const STATUS_BAR_PT: f64 = 26.0;
const RULE_PT: f64 = 1.0;
const READOUT_SLOT_PT: f64 = 240.0;
/// The trailing facts at the status bar's right edge (the zoom and what it means on this display)
/// fit inside this many points; a hover must change nothing there.
const TRAILING_PT: f64 = 100.0;

/// Source points the checks sample, each named by what the fixture puts there.
/// A black dash: every channel 0, so shadow and never highlight.
const DASH: (u32, u32) = (60, 160);
/// The white centre line: every channel 255, so highlight and never shadow.
const LINE: (u32, u32) = (240, 200);
/// Quadrant interiors with no channel at either endpoint, so no overlay may appear on them.
const CLEAN: [(u32, u32); 4] = [(120, 60), (400, 60), (120, 270), (420, 285)];

/// How many frames each scenario captures: one for the open, then one per script step.
pub fn frames(scenario: &str) -> Option<usize> {
    match scenario {
        "histogram" => Some(12),
        "basic-crop" => Some(4),
        _ => None,
    }
}

/// The scenario's own fixture, so `smoke::run` opens the one the checks below are written against.
pub fn source(scenario: &str) -> Option<&'static str> {
    matches!(scenario, "histogram" | "basic-crop").then_some(FIXTURE)
}

pub fn script(scenario: &str) -> Option<Value> {
    (scenario == "histogram").then(|| {
        json!([
            // 1: one both-endpoint pixel, committed through the ordinary edit path.
            {"api":{"method":"edit.set-pixel","params":{"x":BOTH_PIXEL.0,"y":BOTH_PIXEL.1,"rgb":BOTH_RGB}}},
            // 2: the pointer readout over exactly that pixel.
            {"hover":{"x":BOTH_PIXEL.0,"y":BOTH_PIXEL.1}},
            // 3: the shadow overlay alone.
            {"workspace":{"clip_shadows":true}},
            // 4: both overlays, which is where magenta appears.
            {"workspace":{"clip_highlights":true}},
            // 5: 100%, one overlay cell per source pixel.
            {"view":{"zoom":100}},
            // 6: back to Fit.
            {"view":{"zoom":"fit"}},
            // 7: both overlays off again; the photograph is untouched underneath.
            {"workspace":{"clip_shadows":false,"clip_highlights":false}},
            // 8: the Original entry, whose counts are the fixture's own again.
            {"preview":{"sequence":0}},
            // 9: back to current, because a gesture is refused while a historical entry is shown.
            {"preview":"current"},
            // 10: an Exposure drag left open, so the photograph on screen is the drafted render.
            {"slider":{"action":"set-basic","parameter":"exposure","values":[0.5,1.0]}},
            // 11: the same gesture released, which commits once; the plot follows the new stack.
            {"slider":{"action":"set-basic","parameter":"exposure","values":[1.0],"release":true}}
        ])
    })
}

/// The `basic-crop` scenario: a Basic commit, then a 16:9 fit and a 7 degree straighten over it.
/// Its frames prove the counts describe the composition **after** the crop, and that the
/// photograph is placed at the ratio the committed payload declares.
pub fn crop_script(scenario: &str) -> Option<Value> {
    (scenario == "basic-crop").then(|| {
        json!([
            // 1: one Basic commit, so every later frame composes colour with geometry.
            {"api":{"method":"edit.set-basic","params":{"exposure":1.0}}},
            // 2: a 16:9 fit at angle zero, which is an exact copy of its input stage.
            {"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":0.0}}},
            // 3: the same ratio straightened by 7 degrees, which resamples.
            {"api":{"method":"edit.crop-fit","params":{"aspect":"16:9","angle":7.0}}}
        ])
    })
}

/// An independent reduction of the fixture through `recipe`: decode, render and reduce in this
/// process, with no reference to anything the editor reported.
fn reference(root: &Path, recipe: &Recipe) -> Result<analysis::Report> {
    let report = reduction(root, recipe)?;
    ensure(
        (report.width, report.height) == SOURCE,
        format!("Reference render is {}x{}", report.width, report.height),
    )?;
    Ok(report)
}

/// The same reduction without the source-sized expectation, for a composition whose crop changes
/// the output stage.
fn reduction(root: &Path, recipe: &Recipe) -> Result<analysis::Report> {
    let source = lightwell_core::open_source(&root.join(FIXTURE))?;
    let registry = ModuleRegistry::builtin();
    let raster = core_render(&registry, &source, SnapshotId::new(), recipe)?;
    Ok(analysis::reduce_raster(&raster)?)
}

/// The stack a frame says it is displaying, rebuilt as a recipe so this runner renders and reduces
/// it independently instead of trusting the counts beside it.
fn displayed_recipe(frame: &Value) -> Result<Recipe> {
    let layers = frame["state"]["stack"]["displayed"]["layers"]
        .as_array()
        .ok_or("The frame records no displayed stack")?;
    Ok(Recipe {
        format: RECIPE_FORMAT,
        layers: layers
            .iter()
            .map(|layer| -> Result<Layer> {
                Ok(Layer {
                    id: lightwell_core::LayerId::parse(
                        layer["id"].as_str().ok_or("A displayed layer has no id")?,
                    )?,
                    effect_id: layer["effect"]
                        .as_str()
                        .ok_or("A displayed layer has no effect")?
                        .to_owned(),
                    effect_format: lightwell_core::EFFECT_FORMAT,
                    payload: layer["payload"].clone(),
                    mask: None,
                    artifacts: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        masks: Vec::new(),
        ..Recipe::default()
    })
}

/// The stack an open gesture's frame is actually showing: the committed layers it displays, with
/// the drafted Basic payload the frame records placed by the host's own rule for a colour-stage
/// layer. This is what `draft.commit` would persist, rebuilt here so the drafted render is reduced
/// independently rather than trusted.
fn drafted_recipe(frame: &Value) -> Result<Recipe> {
    let mut recipe = displayed_recipe(frame)?;
    let draft = &frame["state"]["draft"];
    ensure(
        draft["action"] == json!("set-basic"),
        format!("The frame's draft is not a Basic gesture: {draft}"),
    )?;
    let fields = draft["fields"].clone();
    ensure(
        fields.as_object().is_some_and(|fields| !fields.is_empty()),
        format!("The frame's draft carries no fields: {draft}"),
    )?;
    // Merging a drafted patch over an existing Basic payload is the module's own arithmetic; this
    // scenario drafts against a stack that holds no Basic layer, so the patch is the whole payload.
    ensure(
        !recipe
            .layers
            .iter()
            .any(|layer| layer.effect_id == BASIC_EFFECT),
        "The displayed stack already holds a Basic layer, so the drafted payload is a merge",
    )?;
    let index = ModuleRegistry::builtin().insertion_index_for(&recipe.layers, BASIC_EFFECT);
    recipe.layers.insert(
        index,
        Layer {
            id: LayerId::new(),
            effect_id: BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: fields,
            mask: None,
            artifacts: Vec::new(),
        },
    );
    Ok(recipe)
}

/// The eleven counters as the frame's correlated state records them.
fn counters(frame: &Value) -> &Value {
    &frame["state"]["histogram"]["counters"]
}

/// Check one frame's histogram against an independent reduction, counter by counter.
fn expect_counts(frame: &Value, report: &analysis::Report, what: &str) -> Result<Value> {
    let state = &frame["state"]["histogram"];
    ensure(
        state["status"] == json!("ready"),
        format!("{what}: histogram status is {}", state["status"]),
    )?;
    ensure(
        state["stale"] == json!(false),
        format!("{what}: the shown counts are stale"),
    )?;
    ensure(
        state["caption"] == json!(DOMAIN) && state["tooltips"]["plot"] == json!(DOMAIN),
        format!(
            "{what}: the plot states the domain as {} (caption {})",
            state["tooltips"]["plot"], state["caption"]
        ),
    )?;
    // A frame with a report draws nothing over the plot: the notice is only for a missing one.
    ensure(
        state["notice"] == Value::Null,
        format!("{what}: the plot carries the notice {}", state["notice"]),
    )?;
    // The triangles' tooltips state the counts in words, and the words are the independent
    // reduction's, not the model's own numbers read back.
    let shadow = format!(
        "{RULE}\n0 \u{b7} R {} G {} B {} \u{b7} any {} \u{b7} all {}",
        report.r0, report.g0, report.b0, report.any_shadow, report.all_shadow
    );
    let highlight = format!(
        "{RULE}\n255 \u{b7} R {} G {} B {} \u{b7} any {} \u{b7} all {}\nboth {}",
        report.r255,
        report.g255,
        report.b255,
        report.any_highlight,
        report.all_highlight,
        report.both
    );
    ensure(
        state["tooltips"]["shadow"] == json!(shadow),
        format!(
            "{what}: the shadow triangle states {}, the reduction says {shadow:?}",
            state["tooltips"]["shadow"]
        ),
    )?;
    ensure(
        state["tooltips"]["highlight"] == json!(highlight),
        format!(
            "{what}: the highlight triangle states {}, the reduction says {highlight:?}",
            state["tooltips"]["highlight"]
        ),
    )?;
    ensure(
        state["identity"]["domain"] == json!("srgb-8bit-output"),
        format!(
            "{what}: the identity domain is {}",
            state["identity"]["domain"]
        ),
    )?;
    ensure(
        state["identity"]["width"] == json!(report.width)
            && state["identity"]["height"] == json!(report.height),
        format!(
            "{what}: the identity names {} for a {}x{} reduction",
            state["identity"], report.width, report.height
        ),
    )?;
    let expected = json!({
        "r0": report.r0, "g0": report.g0, "b0": report.b0,
        "r255": report.r255, "g255": report.g255, "b255": report.b255,
        "any_shadow": report.any_shadow, "any_highlight": report.any_highlight,
        "all_shadow": report.all_shadow, "all_highlight": report.all_highlight,
        "both": report.both,
    });
    ensure(
        counters(frame) == &expected,
        format!(
            "{what}: counters are {}, the independent reduction says {expected}",
            counters(frame)
        ),
    )?;
    // The plot's shared scale is the largest count in any channel, which is checkable too.
    let max = report
        .r
        .iter()
        .chain(report.g.iter())
        .chain(report.b.iter())
        .copied()
        .max()
        .unwrap_or(0);
    ensure(
        state["plotted_max"] == json!(max),
        format!(
            "{what}: one full-height bin stands for {}, the reduction's tallest bin is {max}",
            state["plotted_max"]
        ),
    )?;
    Ok(json!({"counters":expected,"plotted_max":max,"identity":state["identity"]}))
}

/// The photograph's exact rectangle in a capture: every pixel of the photo surface that carries one
/// of the fixture's four quadrant colours. The scan is exhaustive rather than stepped, because the
/// overlay checks below map single source pixels into it and a four-pixel error would miss them.
fn photo_rect(path: &Path, frame: &Value) -> Result<([u32; 4], image::RgbImage)> {
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [left_edge, right_edge] = columns(frame)?.unwrap_or([0, width]);
    let quadrant = |pixel: &[u8; 3]| {
        fixtures::COLORS
            .iter()
            .any(|colour| pixel.iter().zip(colour).all(|(a, b)| a.abs_diff(*b) <= 8))
    };
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0u32, 0u32);
    for y in 0..height {
        for x in left_edge..right_edge.min(width) {
            if quadrant(&image.get_pixel(x, y).0) {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    ensure(
        right > left && bottom > top,
        "No fixture colours in the photo surface",
    )?;
    Ok(([left, top, right, bottom], image))
}

/// Where one source pixel's centre lands in a capture whose photograph occupies `rect`.
fn map(rect: [u32; 4], (x, y): (u32, u32)) -> (u32, u32) {
    let [left, top, right, bottom] = rect;
    let scale_x = f64::from(right - left) / f64::from(SOURCE.0);
    let scale_y = f64::from(bottom - top) / f64::from(SOURCE.1);
    (
        (f64::from(left) + (f64::from(x) + 0.5) * scale_x).round() as u32,
        (f64::from(top) + (f64::from(y) + 0.5) * scale_y).round() as u32,
    )
}

/// How one captured pixel changed between two frames of the same photograph at the same zoom.
///
/// The overlay is a translucent mask, so its composited colour depends on what is underneath, and
/// the fixture's own quadrants are themselves strongly coloured — the red quadrant read on its own
/// is as red as a highlight mask is. Every check below therefore works on the **difference** from a
/// frame of the same stack at the same zoom with the overlays off, which cancels the base out and
/// leaves the mask's own contribution. Each class then has a signature that does not depend on the
/// pixel underneath, because a mask at opacity `a` moves a pixel by `a * (mask - base)` and the
/// differences between channels of that vector are the differences between channels of the mask:
///
/// - shadow (`#4c8be0`): blue rises far more than red;
/// - highlight (`#e5534b`): red rises far more than blue and than green;
/// - both (`#e553e0`, the two tokens combined): against the shadow mask it is the same pixel with
///   red added, which is what the magenta check compares.
#[derive(Clone, Copy, Debug)]
struct Delta {
    r: i32,
    g: i32,
    b: i32,
}

impl Delta {
    fn between(after: [u8; 3], before: [u8; 3]) -> Self {
        Self {
            r: i32::from(after[0]) - i32::from(before[0]),
            g: i32::from(after[1]) - i32::from(before[1]),
            b: i32::from(after[2]) - i32::from(before[2]),
        }
    }

    fn largest(self) -> i32 {
        self.r.abs().max(self.g.abs()).max(self.b.abs())
    }

    fn is_shadow_mask(self) -> bool {
        self.b - self.r >= 40 && self.b - self.g >= 15
    }

    fn is_highlight_mask(self) -> bool {
        self.r - self.b >= 40 && self.r - self.g >= 40
    }

    /// The shadow mask turned into the both mask: only the red channel of the mask changed, so only
    /// the red channel of the composite moves, and it moves up.
    fn is_both_upgrade(self) -> bool {
        self.r >= 60 && self.r - self.g >= 60 && self.r - self.b >= 40
    }
}

/// How far a captured pixel may move between two frames and still count as untouched. Two captures
/// of the same content through the same renderer are identical in practice; this leaves room for a
/// single least-significant bit rather than for a faint mask.
const UNCHANGED: i32 = 8;

/// The deltas of every pixel in a window around one source point, so a check tolerates the mapped
/// centre landing a pixel either side of the cell it describes.
fn window(
    after: &image::RgbImage,
    before: &image::RgbImage,
    rect: [u32; 4],
    point: (u32, u32),
    radius: i64,
) -> Vec<Delta> {
    let (cx, cy) = map(rect, point);
    let (width, height) = after.dimensions();
    let mut deltas = Vec::new();
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let x = i64::from(cx) + dx;
            let y = i64::from(cy) + dy;
            if x < 0 || y < 0 || x >= i64::from(width) || y >= i64::from(height) {
                continue;
            }
            let (x, y) = (x as u32, y as u32);
            deltas.push(Delta::between(
                after.get_pixel(x, y).0,
                before.get_pixel(x, y).0,
            ));
        }
    }
    deltas
}

/// Nothing within the window around each of these source points moved at all between the two
/// frames: an overlay that spread beyond the pixels it describes would show up here.
fn untouched(
    after: &image::RgbImage,
    before: &image::RgbImage,
    rect: [u32; 4],
    points: &[(u32, u32)],
    radius: i64,
    what: &str,
) -> Result {
    for point in points {
        let moved = window(after, before, rect, *point, radius)
            .into_iter()
            .map(Delta::largest)
            .max()
            .unwrap_or(0);
        ensure(
            moved <= UNCHANGED,
            format!("{what}: the pixels around source {point:?} moved by {moved}"),
        )?;
    }
    Ok(())
}

/// Two frames of the same photograph at the same zoom occupy the same rectangle, so their pixels
/// can be compared where they stand.
fn same_rect(a: [u32; 4], b: [u32; 4], what: &str) -> Result {
    ensure(
        a == b,
        format!(
            "{what}: the photograph moved between the frames being compared, {a:?} against {b:?}"
        ),
    )
}

/// The radius one source pixel's window needs, from the measured rectangle: a little over one
/// source pixel's own width on screen, so a single clipped cell is found wherever rounding put it.
fn radius(rect: [u32; 4]) -> i64 {
    let [left, _, right, _] = rect;
    let per_source = f64::from(right - left) / f64::from(SOURCE.0);
    (per_source.ceil() as i64 + 3).max(4)
}

/// What changed between two captures of the same screen, where the inspector's words used to make
/// the tools panel move: every pixel of the tools panel, and the status bar column by column.
///
/// The tools panel is the capture right of the photo surface and its 1 pt divider, between the
/// title bar's rule and the status bar's. The status bar is the bottom 26 pt. Both come from the
/// capture's own recorded scale and surface columns rather than a guess at the layout.
fn chrome_changes(before: &Path, after: &Path, frame: &Value) -> Result<Value> {
    let before = image::open(before)?.to_rgb8();
    let after = image::open(after)?.to_rgb8();
    ensure(
        before.dimensions() == after.dimensions(),
        "The two captures are different sizes",
    )?;
    let (width, height) = after.dimensions();
    let scale = frame["scale"]
        .as_f64()
        .ok_or("The frame records no scale")?;
    let [_, surface_right] = columns(frame)?.ok_or("The frame records no surface columns")?;
    let px = |points: f64| (points * scale).ceil() as u32;
    let panel = [
        surface_right + px(RULE_PT),
        px(TITLE_BAR_PT + RULE_PT),
        width,
        height.saturating_sub(px(STATUS_BAR_PT + RULE_PT)),
    ];
    ensure(
        panel[0] < panel[2] && panel[1] < panel[3],
        "The tools panel is not open in the capture",
    )?;
    let mut panel_changed = 0u64;
    for y in panel[1]..panel[3] {
        for x in panel[0]..panel[2] {
            if before.get_pixel(x, y) != after.get_pixel(x, y) {
                panel_changed += 1;
            }
        }
    }
    let bar_top = height.saturating_sub((STATUS_BAR_PT * scale).floor() as u32);
    let changed_columns: Vec<u32> = (0..width)
        .filter(|&x| (bar_top..height).any(|y| before.get_pixel(x, y) != after.get_pixel(x, y)))
        .collect();
    Ok(json!({
        "tools_panel_rect": panel,
        "tools_panel_pixels_changed": panel_changed,
        "status_bar_rows": [bar_top, height],
        "status_bar_changed_span": match (changed_columns.first(), changed_columns.last()) {
            (Some(first), Some(last)) => json!([first, last + 1]),
            _ => Value::Null,
        },
        "readout_slot_px": READOUT_SLOT_PT * scale,
        "trailing_px": TRAILING_PT * scale,
        "width": width,
    }))
}

pub fn verify(root: &Path, evidence: &Path, app: &Value, events: &[Value]) -> Result {
    let frames = app["frames"].as_array().ok_or("Missing frames")?.clone();
    ensure(
        app["had_input_errors"] == json!(false),
        "The run recorded an input error",
    )?;
    let paths: Vec<PathBuf> = frames
        .iter()
        .map(|frame| frame_identity(evidence, app, frame))
        .collect::<Result<Vec<_>>>()?;
    let mut checks = Vec::new();
    let mut record = |frame: &Value, shows: &str, detail: Value| {
        checks.push(json!({"frame":frame["file"],"shows":shows,"detail":detail}));
    };

    // The two recipes the run displays, rendered and reduced independently in this process.
    let plain = reference(root, &Recipe::default())?;
    let edited = reference(
        root,
        &Recipe {
            format: RECIPE_FORMAT,
            layers: vec![Layer::pixel(BOTH_PIXEL.0, BOTH_PIXEL.1, BOTH_RGB)],
            masks: Vec::new(),
            ..Recipe::default()
        },
    )?;
    ensure(
        plain.both == 0 && plain.any_shadow > 0 && plain.any_highlight > 0,
        format!(
            "The fixture's own clipping is not what this scenario is written against: shadow {}, highlight {}, both {}",
            plain.any_shadow, plain.any_highlight, plain.both
        ),
    )?;
    ensure(
        edited.both == plain.both + 1,
        format!(
            "Setting one both-endpoint pixel did not add exactly one: {} against {}",
            edited.both, plain.both
        ),
    )?;

    // Frame 0: the default screen. The histogram is ready and its counts are the fixture's own.
    let opened = expect_counts(&frames[0], &plain, "frame 0, the opened fixture")?;
    ensure(
        frames[0]["state"]["histogram"]["overlay"] == Value::Null,
        "An overlay was derived before either flag was set",
    )?;
    ensure(
        frames[0]["state"]["workspace"]["clip_shadows"] == json!(false)
            && frames[0]["state"]["workspace"]["clip_highlights"] == json!(false),
        "The clipping flags do not start off",
    )?;
    record(
        &frames[0],
        "the default screen with the histogram ready, counts equal to an independent reduction",
        json!({"histogram":opened,"pixels":pixels(&paths[0], &Expect { columns: columns(&frames[0])?, ..Expect::fit(1) })?}),
    );

    // Frame 1: one both-endpoint pixel committed. The counts follow the new stack exactly.
    ensure(
        frames[1]["state"]["stack"]["revision"] == json!(1),
        "edit.set-pixel did not commit revision 1",
    )?;
    let after = expect_counts(&frames[1], &edited, "frame 1, after edit.set-pixel")?;
    record(
        &frames[1],
        "one both-endpoint pixel committed; the plot follows the new stack",
        after,
    );

    // Frame 2: the pointer readout of that very pixel, in output codes.
    let readout = &frames[2]["state"]["readout"];
    ensure(
        readout["rgba"] == json!([BOTH_RGB[0], BOTH_RGB[1], BOTH_RGB[2], 255]),
        format!(
            "The readout reports {}, expected the pixel just set",
            readout
        ),
    )?;
    ensure(
        readout["x"] == json!(BOTH_PIXEL.0) && readout["y"] == json!(BOTH_PIXEL.1),
        format!("The readout names {}, {}", readout["x"], readout["y"]),
    )?;
    ensure(
        readout["text"] == json!("R 0 \u{b7} G 128 \u{b7} B 255 \u{b7} 360, 240"),
        format!("The readout line is {}", readout["text"]),
    )?;
    // The readout is the status bar's, and it arrived with the hover: the frame before it has none.
    ensure(
        frames[2]["state"]["status_bar"]["readout"] == readout["text"],
        format!(
            "The status bar shows {} while the readout is {}",
            frames[2]["state"]["status_bar"]["readout"], readout["text"]
        ),
    )?;
    ensure(
        frames[1]["state"]["status_bar"]["readout"] == Value::Null,
        "The status bar showed a readout before the pointer reached the photograph",
    )?;
    // Nothing else moved. The tools panel is pixel for pixel the frame before the hover, and the
    // status bar changed only inside one readout-slot-wide span that stops short of the trailing
    // facts: had the readout pushed anything, the zoom at the right edge would have moved too.
    let chrome = chrome_changes(&paths[1], &paths[2], &frames[2])?;
    ensure(
        chrome["tools_panel_pixels_changed"] == json!(0),
        format!(
            "The pointer readout changed {} pixels of the tools panel",
            chrome["tools_panel_pixels_changed"]
        ),
    )?;
    let span = chrome["status_bar_changed_span"]
        .as_array()
        .and_then(|span| Some((span.first()?.as_f64()?, span.get(1)?.as_f64()?)))
        .ok_or("The readout did not change the status bar at all")?;
    let slot = chrome["readout_slot_px"].as_f64().unwrap_or(0.0);
    let bar_width = chrome["width"].as_f64().unwrap_or(0.0);
    let trailing = chrome["trailing_px"].as_f64().unwrap_or(0.0);
    ensure(
        span.1 - span.0 <= slot && span.1 <= bar_width - trailing,
        format!(
            "The status bar changed over columns {span:?}, wider than the {slot} px readout slot or into the trailing {trailing} px"
        ),
    )?;
    // The same pixel through the public method, from this process: the readout uses that path, so
    // this is the same answer read a second way rather than a second implementation of it.
    ensure(
        counters(&frames[2]) == counters(&frames[1]),
        "Hovering changed the histogram",
    )?;
    record(
        &frames[2],
        "the pointer readout over the pixel that was just set, in the status bar; the tools panel is pixel-identical to the frame before and the status bar changed only inside the readout slot",
        json!({"readout": readout, "status_bar": frames[2]["state"]["status_bar"], "chrome": chrome}),
    );

    // The reference the overlay frames are compared against: the same stack, the same zoom, the
    // same panels, both overlays off. Every mask check below is a difference from this, so the
    // fixture's own strongly coloured quadrants cancel out instead of being mistaken for a mask.
    let (bare_rect, bare) = photo_rect(&paths[2], &frames[2])?;
    let bare_radius = radius(bare_rect);

    // Frame 3: the shadow overlay alone. Blue over the black dashes, and nothing red anywhere.
    ensure(
        frames[3]["state"]["workspace"]["clip_shadows"] == json!(true)
            && frames[3]["state"]["workspace"]["clip_highlights"] == json!(false),
        format!("Frame 3's flags are {}", frames[3]["state"]["workspace"]),
    )?;
    let overlay = &frames[3]["state"]["histogram"]["overlay"];
    ensure(
        overlay["cells"] == json!([SOURCE.0, SOURCE.1]),
        format!(
            "The Fit overlay grid is {}, expected one cell per source pixel",
            overlay["cells"]
        ),
    )?;
    ensure(overlay["drawn"] == json!(true), "The overlay was not drawn")?;
    let (rect3, image3) = photo_rect(&paths[3], &frames[3])?;
    same_rect(rect3, bare_rect, "frame 3")?;
    let shadow_on_dash = window(&image3, &bare, rect3, DASH, bare_radius);
    ensure(
        shadow_on_dash.iter().any(|delta| delta.is_shadow_mask()),
        format!(
            "No blue overlay over the black dash band at source {DASH:?}; the largest change there was {}",
            shadow_on_dash
                .iter()
                .map(|d| d.largest())
                .max()
                .unwrap_or(0)
        ),
    )?;
    // The white centre line is at code 255 in every channel, so with only the shadow flag on it
    // must be exactly as it was.
    untouched(
        &image3,
        &bare,
        rect3,
        &[LINE],
        bare_radius,
        "the shadow overlay alone touched the 255 line",
    )?;
    untouched(
        &image3,
        &bare,
        rect3,
        &CLEAN,
        bare_radius,
        "the shadow overlay reached an unclipped quadrant",
    )?;
    // The committed stack is untouched by a view flag: same revision, same layers.
    ensure(
        frames[3]["state"]["stack"] == frames[1]["state"]["stack"],
        "Switching an overlay on changed the committed stack",
    )?;
    record(
        &frames[3],
        "the shadow overlay: blue over the pixels with a channel at 0, nothing over the 255 line",
        json!({"overlay":overlay,"photo_rect":rect3,"window_radius":bare_radius}),
    );

    // Frame 4: both overlays. Blue over the dashes, red over the white line, magenta where both
    // hold at once. The fixture has no pixel at both endpoints of its own, so the magenta comes
    // from the one pixel the scenario set; that is the isolated-pixel case a Fit overlay must keep.
    ensure(
        frames[4]["state"]["workspace"]["clip_shadows"] == json!(true)
            && frames[4]["state"]["workspace"]["clip_highlights"] == json!(true),
        format!("Frame 4's flags are {}", frames[4]["state"]["workspace"]),
    )?;
    let (rect4, image4) = photo_rect(&paths[4], &frames[4])?;
    same_rect(rect4, bare_rect, "frame 4")?;
    ensure(
        window(&image4, &bare, rect4, DASH, bare_radius)
            .iter()
            .any(|delta| delta.is_shadow_mask()),
        format!("No blue overlay over the black dash band at source {DASH:?}"),
    )?;
    ensure(
        window(&image4, &bare, rect4, LINE, bare_radius)
            .iter()
            .any(|delta| delta.is_highlight_mask()),
        format!("No red overlay over the white centre line at source {LINE:?}"),
    )?;
    // Magenta is measured against the shadow-only frame rather than the bare one: turning the
    // highlight flag on changes that one cell from the shadow mask to the both mask, which adds
    // red and nothing else, whatever the photograph underneath is.
    let both_upgrade = window(&image4, &image3, rect4, BOTH_PIXEL, bare_radius);
    ensure(
        both_upgrade.iter().any(|delta| delta.is_both_upgrade()),
        format!(
            "The isolated both-endpoint pixel at source {BOTH_PIXEL:?} did not turn magenta; the largest change from the shadow-only frame was {}",
            both_upgrade.iter().map(|d| d.largest()).max().unwrap_or(0)
        ),
    )?;
    untouched(
        &image4,
        &bare,
        rect4,
        &CLEAN,
        bare_radius,
        "an overlay reached an unclipped quadrant",
    )?;
    record(
        &frames[4],
        "both overlays: blue at code 0, red at code 255, magenta on the one pixel at both",
        json!({
            "overlay": frames[4]["state"]["histogram"]["overlay"],
            "photo_rect": rect4,
            "window_radius": bare_radius,
            "note": "the fixture's own quadrant colours reach neither endpoint, so the only magenta is the isolated pixel this scenario set",
        }),
    );

    // Frame 5: 100%. One overlay cell per source pixel, and the masks still land on their pixels.
    // There is no overlay-off frame at 100% to difference against, so the two checks here are the
    // ones whose base colour is unambiguous on its own: the dash band is black and the centre line
    // is white, so a blue-dominant dash and a red-dominant line can only be the masks.
    ensure(
        frames[5]["state"]["histogram"]["overlay"]["cells"] == json!([SOURCE.0, SOURCE.1]),
        format!(
            "The 100% overlay grid is {}",
            frames[5]["state"]["histogram"]["overlay"]["cells"]
        ),
    )?;
    let (rect5, image5) = photo_rect(&paths[5], &frames[5])?;
    let hundred_width = rect5[2] - rect5[0];
    ensure(
        hundred_width.abs_diff(SOURCE.0) <= 2,
        format!(
            "At 100% the photograph is {hundred_width} physical pixels wide, expected {}",
            SOURCE.0
        ),
    )?;
    let radius5 = radius(rect5);
    let black = image::Rgb([0u8, 0, 0]);
    let white = image::Rgb([255u8, 255, 255]);
    let against = |base: image::Rgb<u8>, point: (u32, u32)| {
        let (cx, cy) = map(rect5, point);
        let mut deltas = Vec::new();
        for dy in -radius5..=radius5 {
            for dx in -radius5..=radius5 {
                let x = i64::from(cx) + dx;
                let y = i64::from(cy) + dy;
                if x < 0
                    || y < 0
                    || x >= i64::from(image5.width())
                    || y >= i64::from(image5.height())
                {
                    continue;
                }
                deltas.push(Delta::between(
                    image5.get_pixel(x as u32, y as u32).0,
                    base.0,
                ));
            }
        }
        deltas
    };
    ensure(
        against(black, DASH)
            .iter()
            .any(|delta| delta.is_shadow_mask()),
        "At 100% the shadow overlay no longer lines up with the black dash band",
    )?;
    ensure(
        against(white, LINE)
            .iter()
            .any(|delta| delta.is_highlight_mask()),
        "At 100% the highlight overlay no longer lines up with the white centre line",
    )?;
    ensure(
        counters(&frames[5]) == counters(&frames[1]),
        "Zooming changed the histogram",
    )?;
    record(
        &frames[5],
        "100% with both overlays on: one cell per physical pixel, still aligned",
        json!({"photo_rect":rect5,"physical_width":hundred_width,"overlay":frames[5]["state"]["histogram"]["overlay"]}),
    );

    // Frame 6: back to Fit, still no re-analysis.
    ensure(
        counters(&frames[6]) == counters(&frames[1]),
        "Returning to Fit changed the histogram",
    )?;
    record(
        &frames[6],
        "back at Fit with both overlays on",
        counters(&frames[6]).clone(),
    );

    // Frame 7: both overlays off. The photograph underneath is byte for byte the frame from before
    // either flag was set, at every point the masks had covered.
    ensure(
        frames[7]["state"]["workspace"]["clip_shadows"] == json!(false)
            && frames[7]["state"]["workspace"]["clip_highlights"] == json!(false),
        "The overlays did not switch off",
    )?;
    ensure(
        frames[7]["state"]["histogram"]["overlay"] == Value::Null,
        "An overlay is still derived with both flags off",
    )?;
    let (rect7, image7) = photo_rect(&paths[7], &frames[7])?;
    same_rect(rect7, bare_rect, "frame 7")?;
    let mut covered = vec![DASH, LINE, BOTH_PIXEL];
    covered.extend(CLEAN);
    untouched(
        &image7,
        &bare,
        rect7,
        &covered,
        bare_radius,
        "an overlay survived both flags being switched off",
    )?;
    ensure(
        counters(&frames[7]) == counters(&frames[1]),
        "Switching the overlays off changed the histogram",
    )?;
    record(
        &frames[7],
        "both overlays off: the photograph is the fixture again and the counts are unchanged",
        json!({"photo_rect":rect7,"points_compared":covered}),
    );

    // Frame 8: the Original entry. The plot follows the displayed generation, not the newest stack.
    let original = expect_counts(&frames[8], &plain, "frame 8, previewing the Original")?;
    ensure(
        counters(&frames[8]) != counters(&frames[1]),
        "The Original's counts are indistinguishable from the edited stack's",
    )?;
    ensure(
        frames[8]["state"]["histogram"]["identity"]["entry"]
            != frames[1]["state"]["histogram"]["identity"]["entry"],
        "The plot still names the entry the edited stack belongs to",
    )?;
    // The readout describes one pixel of one stack, so moving to another entry clears it rather
    // than leaving the edited stack's codes on screen over the Original.
    ensure(
        frames[8]["state"]["readout"] == Value::Null,
        format!(
            "The readout survived the change of displayed entry: {}",
            frames[8]["state"]["readout"]
        ),
    )?;
    record(
        &frames[8],
        "previewing the Original: the plot follows the displayed entry, not the newest one",
        original,
    );

    // Frame 9: back to current. A gesture is refused while a historical entry is shown, so the
    // run returns first; the plot is the edited stack's again.
    ensure(
        counters(&frames[9]) == counters(&frames[1]),
        "Returning to current did not restore the edited stack's counts",
    )?;
    record(
        &frames[9],
        "back to current before the gesture",
        counters(&frames[9]).clone(),
    );

    // Frame 10: an Exposure drag left open. The photograph on screen is the drafted render, and
    // the contract ties the counts to the image presented, drafts included — so the inspector
    // describes that render: its identity carries the draft revision the pixels were planned from
    // and its counters equal an independent render and reduction of the drafted stack, computed
    // here from the layers the frame says it displays and the drafted payload it records.
    let drafted = &frames[10]["state"]["draft"];
    ensure(
        drafted["action"] == json!("set-basic") && drafted["fields"] == json!({"exposure": 1.0}),
        format!("Frame 10's draft is not the open Exposure gesture: {drafted}"),
    )?;
    ensure(
        frames[10]["state"]["displayed_draft_revision"] == drafted["draft_revision"]
            && drafted["draft_revision"].as_u64().is_some_and(|r| r >= 1),
        format!(
            "Frame 10 displays draft revision {} while the draft is at {}",
            frames[10]["state"]["displayed_draft_revision"], drafted["draft_revision"]
        ),
    )?;
    let drafted_report = reduction(root, &drafted_recipe(&frames[10])?)?;
    let drafted_detail = expect_counts(&frames[10], &drafted_report, "frame 10, the open gesture")?;
    ensure(
        frames[10]["state"]["histogram"]["identity"]["draft_revision"] == drafted["draft_revision"],
        format!(
            "The plot names draft revision {} while the frame displays {}",
            frames[10]["state"]["histogram"]["identity"]["draft_revision"],
            drafted["draft_revision"]
        ),
    )?;
    // The drafted exposure is +1 EV, so it clips highlights the committed stack does not: the
    // counts are demonstrably the drafted population rather than the previous frame's relabelled.
    ensure(
        counters(&frames[10]) != counters(&frames[9]),
        "The drafted exposure left the counts identical to the committed frame's",
    )?;
    ensure(
        frames[10]["state"]["stack"]["revision"] == frames[9]["state"]["stack"]["revision"],
        "The open gesture committed something",
    )?;
    record(
        &frames[10],
        "an Exposure drag left open: the photograph is the drafted render and the plot is that render, its identity carrying the draft revision and its counts equal to an independent reduction of the drafted stack",
        json!({"draft": drafted, "histogram": drafted_detail}),
    );

    // Frame 11: the gesture released. One commit, and the plot follows the composed stack: an
    // independent render and reduction of exactly the layers the frame says it displays.
    ensure(
        frames[11]["state"]["draft"] == Value::Null,
        "The released gesture left a draft open",
    )?;
    ensure(
        frames[11]["state"]["stack"]["revision"]
            == json!(
                frames[10]["state"]["stack"]["revision"]
                    .as_u64()
                    .unwrap_or(0)
                    + 1
            ),
        "The release did not advance the revision by exactly one",
    )?;
    let released = reduction(root, &displayed_recipe(&frames[11])?)?;
    let released_detail = expect_counts(&frames[11], &released, "frame 11, the released gesture")?;
    ensure(
        counters(&frames[11]) != counters(&frames[9]),
        "The committed exposure left the counts unchanged",
    )?;
    record(
        &frames[11],
        "the gesture released: one commit, and the counts equal an independent reduction of the composed pixel-and-exposure stack",
        released_detail,
    );

    // Every frame the run presented reports its own render time, and each captured status bar
    // states one of them rather than the time since the open.
    let render_times = crate::smoke::expect_render_times(events, &frames)?;
    checks.push(json!({"shows":"the render time of every presented frame","detail":render_times}));

    write_json(&evidence.join("histogram-checks.json"), &json!(checks))?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The `basic-crop` scenario: colour composed with geometry, on the real editor.
// ---------------------------------------------------------------------------------------------

/// The ratio the two crop frames commit.
const CROP_RATIO: f64 = 16.0 / 9.0;

/// The photograph's bounding box in a capture, found by brightness rather than by the fixture's
/// own quadrant colours: a Basic edit moves those colours, so matching them would be matching the
/// edit rather than the placement. Inside the photo surface, between the notices at the top of the
/// canvas and the floating mode strip at its bottom, nothing is as bright as the photograph.
fn bright_rect(path: &Path, frame: &Value) -> Result<[u32; 4]> {
    /// Well above the canvas surface (`#19191b`) and the bars over it (`#232326`), and well below
    /// every quadrant of this fixture at any exposure this scenario uses.
    const BRIGHT: u32 = 70;
    /// The 1 px dividers at the surface's own edges are as bright as dark content, so the scan
    /// starts inside them.
    const INSET: u32 = 4;
    let image = image::open(path)?.to_rgb8();
    let (width, height) = image.dimensions();
    let [left_edge, right_edge] = columns(frame)?.unwrap_or([0, width]);
    ensure(
        left_edge + INSET < right_edge.saturating_sub(INSET) && right_edge <= width,
        "Invalid surface columns",
    )?;
    let (band_top, band_bottom) = (height * 3 / 20, height * 22 / 25);
    let (mut left, mut top, mut right, mut bottom) = (width, height, 0u32, 0u32);
    for y in band_top..band_bottom {
        for x in (left_edge + INSET)..(right_edge - INSET) {
            let pixel = image.get_pixel(x, y).0;
            let mean = (u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2])) / 3;
            if mean >= BRIGHT {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    ensure(
        right > left && bottom > top,
        "No photograph in the frame: blank or wrong render",
    )?;
    Ok([left, top, right, bottom])
}

/// The displayed ratio and centring of a captured photograph, against the ratio its committed crop
/// payload declares.
fn expect_placement(path: &Path, frame: &Value, ratio: f64, what: &str) -> Result<Value> {
    let [left, top, right, bottom] = bright_rect(path, frame)?;
    let image = image::open(path)?.to_rgb8();
    let [surface_left, surface_right] = columns(frame)?.unwrap_or([0, image.width()]);
    let measured = f64::from(right - left) / f64::from(bottom - top);
    ensure(
        (measured - ratio).abs() < 0.02,
        format!("{what}: the displayed ratio is {measured:.4}, the payload declares {ratio:.4}"),
    )?;
    ensure(
        (f64::from(left + right) / 2.0 - f64::from(surface_left + surface_right) / 2.0).abs()
            <= 6.0,
        format!("{what}: the photograph is not centred in the photo surface"),
    )?;
    ensure(
        f64::from(right - left) > f64::from(surface_right - surface_left) * 0.4,
        format!("{what}: the photograph does not fill the surface at Fit"),
    )?;
    Ok(json!({
        "image_bounds": [left, top, right, bottom],
        "surface_columns": [surface_left, surface_right],
        "measured_ratio": measured,
        "expected_ratio": ratio,
        "ratio_tolerance": 0.02,
        "scope": "Displayed placement read back from the renderer; not monitor calibration",
    }))
}

pub fn verify_crop(root: &Path, evidence: &Path, app: &Value, _events: &[Value]) -> Result {
    let frames = app["frames"].as_array().ok_or("Missing frames")?.clone();
    ensure(
        app["had_input_errors"] == json!(false),
        "The run recorded an input error",
    )?;
    let paths: Vec<PathBuf> = frames
        .iter()
        .map(|frame| frame_identity(evidence, app, frame))
        .collect::<Result<Vec<_>>>()?;

    // Every frame's counts are an independent render and reduction of exactly the layers that
    // frame says it displays, so the plot is proved against the composition rather than itself.
    let expected: Vec<&str> = vec![
        "the opened fixture",
        "one Basic commit",
        "a 16:9 fit at angle zero over the Basic layer",
        "the same ratio straightened by 7 degrees",
    ];
    let mut details = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let recipe = displayed_recipe(frame)?;
        let report = reduction(root, &recipe)?;
        let counts = expect_counts(
            frame,
            &report,
            &format!("frame {index}, {}", expected[index]),
        )?;
        details.push(json!({
            "frame": frame["file"],
            "shows": expected[index],
            "stack": recipe.layers.iter().map(|layer| layer.effect_id.clone()).collect::<Vec<_>>(),
            "output": [report.width, report.height],
            "counts": counts,
        }));
    }

    // The Basic commit is one entry and one revision, and it changes the population.
    ensure(
        frames[1]["state"]["stack"]["revision"] == json!(1),
        "edit.set-basic did not commit revision 1",
    )?;
    ensure(
        counters(&frames[1]) != counters(&frames[0]),
        "The exposure left the population unchanged",
    )?;
    // Each crop commits once and keeps the one crop layer with its identity.
    let crop_layer = |frame: &Value| -> Result<String> {
        frame["state"]["stack"]["layers"]
            .as_array()
            .ok_or("No layers")?
            .iter()
            .find(|layer| layer["effect"] == json!(lightwell_core::CROP_EFFECT))
            .and_then(|layer| layer["id"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| "The frame holds no crop layer".into())
    };
    ensure(
        crop_layer(&frames[2])? == crop_layer(&frames[3])?,
        "The straighten replaced the crop layer instead of updating it",
    )?;
    ensure(
        frames[3]["state"]["stack"]["revision"] == json!(3),
        format!(
            "The straighten left the revision at {}",
            frames[3]["state"]["stack"]["revision"]
        ),
    )?;
    // The counts follow the crop: a 16:9 rectangle of this stage holds fewer pixels than the
    // whole one, and the identity says so.
    let pixels_of = |frame: &Value| -> u64 {
        frame["state"]["histogram"]["identity"]["width"]
            .as_u64()
            .unwrap_or(0)
            * frame["state"]["histogram"]["identity"]["height"]
                .as_u64()
                .unwrap_or(0)
    };
    ensure(
        pixels_of(&frames[2]) < pixels_of(&frames[1])
            && pixels_of(&frames[3]) < pixels_of(&frames[1]),
        "A crop did not reduce the analysed output stage",
    )?;

    // Placement: each crop frame shows a 16:9 photograph, centred in the photo surface.
    let mut placements = Vec::new();
    for (index, ratio) in [
        (0usize, 3.0 / 2.0),
        (1, 3.0 / 2.0),
        (2, CROP_RATIO),
        (3, CROP_RATIO),
    ] {
        placements.push(expect_placement(
            &paths[index],
            &frames[index],
            ratio,
            &format!("frame {index}"),
        )?);
    }

    write_json(
        &evidence.join("basic-crop-checks.json"),
        &json!({
            "frames": details,
            "placement": placements,
            "crop_layer": crop_layer(&frames[3])?,
            "scope": "Counts against an independent core render and reduction of the displayed stack; placement read back from the renderer",
        }),
    )?;
    Ok(())
}
