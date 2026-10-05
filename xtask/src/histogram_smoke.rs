//! The `histogram` smoke scenario: the inspector, the clipping overlays and a `render.sample`.
//!
//! The fixture is the ordinary `orientation-1` pattern, whose clipped pixels are known from the
//! generator rather than guessed: four quadrant colours with no channel at an endpoint, a white
//! centre line and arrow (every channel at 255), and a band of black dashes across the middle
//! (every channel at 0). Nothing in it has one channel at 0 and another at 255, so the scenario
//! commits one `edit.set-pixel` of `(0, 128, 255)` to make exactly one both-endpoint pixel — which
//! is also the isolated-clipped-pixel case the contract asks the Fit overlay to survive. The pixel
//! proof is a test module, so the scenario's one launch is a developer launch.
//!
//! Every count a frame reports is checked against `analysis::reduce` of an **independent**
//! core render of the same fixture through the same recipe, so the plot is verified against the
//! reducer rather than against itself — and so are the words the triangles' tooltips state them in.
//! Counts the reference renderer reduced must equal it exactly; counts the GPU took over the
//! stack's tiles (`state.histogram.source` `gpu`, `docs/design/gpu-first.md`, stage 2) are held to
//! the recorded tolerance, each counter within 0.1% of the output pixel count, and the report the
//! owner's store holds for them, read back through `analysis.request`, is held to it bin by bin.
//! The pixel the scenario sets is a pixel-stage layer the GPU does not draw, so that stack is the
//! reference's; the Original's and a later exposure-only stack are the GPU's.
//!
//! The inspector is the plot with the triangles in its bottom corners and nothing else, with no
//! caption. Nothing is read under the pointer; the pixel the scenario sets is read back through the
//! public `render.sample`, whose answer the step records.
use crate::{
    scenario::{Checked, Checks, Fixture, Frame, Plan, Run, Step, Tolerance, pixels, plan::only},
    *,
};
use luxforge_core::{
    BASIC_EFFECT, CROP_EFFECT, Cancel, EFFECT_FORMAT, Layer, LayerId, ModuleRegistry, PIXEL_EFFECT,
    RECIPE_FORMAT, Recipe, SnapshotId, analysis, render as core_render,
};
use luxforge_evidence::{self as script, PreviewStep, SliderStep, ViewStep, WorkspaceStep};

/// The fixture: 480x320, orientation 1, the quadrant pattern with the white centre line and the
/// black dash band.
const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
const SOURCE: (u32, u32) = (480, 320);

/// The content-stage pixel the scenario sets to a both-endpoint colour, in the middle of the gold
/// quadrant and far from every other clipped pixel in the fixture, so its overlay cell is isolated.
const BOTH_PIXEL: (u32, u32) = (360, 240);
/// Its colour: one channel at 0 and one at 255, which is the both-endpoint class exactly.
const BOTH_RGB: [u8; 3] = [0, 128, 255];

/// The rule both triangles state on hover, as the contract words it; the counts follow it.
const RULE: &str =
    "Any channel at 0 \u{b7} blue; any at 255 \u{b7} red; both endpoints \u{b7} magenta";

/// Source points the checks sample, each named by what the fixture puts there.
/// A black dash: every channel 0, so shadow and never highlight.
const DASH: (u32, u32) = (60, 160);
/// The white centre line: every channel 255, so highlight and never shadow.
const LINE: (u32, u32) = (240, 200);
/// Quadrant interiors with no channel at either endpoint, so no overlay may appear on them.
const CLEAN: [(u32, u32); 4] = [(120, 60), (400, 60), (120, 270), (420, 285)];

/// Every `histogram` frame, in order: the open, then one per step. The expectations here are what
/// each step commits and records; `verify` checks the counts, the sample and the overlays.
pub fn plan(_: &[PathBuf]) -> Plan {
    // A step that changes what is shown and commits nothing.
    let view = |name: &str, script: script::Step| Step::new(name, script).commits(0);
    Plan::new(vec![
        // The default screen: the fixture's own counts, both overlays off.
        Step::opened("opened")
            .workspace("clip_shadows", json!(false))
            .workspace("clip_highlights", json!(false)),
        // One both-endpoint pixel, committed through the ordinary edit path.
        Step::new(
            "pixel",
            script::Step::call(
                "edit.set-pixel",
                json!({"x":BOTH_PIXEL.0,"y":BOTH_PIXEL.1,"rgb":BOTH_RGB}),
            ),
        )
        .commits(1)
        .label("Pixel 360, 240")
        .payload(
            PIXEL_EFFECT,
            json!({"x":BOTH_PIXEL.0,"y":BOTH_PIXEL.1,"rgb":BOTH_RGB}),
        ),
        // The public point query over exactly that pixel; its answer is recorded with the step.
        view(
            "sample",
            script::Step::call("render.sample", json!({"x":BOTH_PIXEL.0,"y":BOTH_PIXEL.1})),
        ),
        // The shadow overlay alone.
        view(
            "shadows",
            script::Step::Workspace(WorkspaceStep::default().clip_shadows(true)),
        )
        .workspace("clip_shadows", json!(true))
        .workspace("clip_highlights", json!(false)),
        // Both overlays, which is where magenta appears.
        view(
            "both",
            script::Step::Workspace(WorkspaceStep::default().clip_highlights(true)),
        )
        .workspace("clip_shadows", json!(true))
        .workspace("clip_highlights", json!(true)),
        // 100%, one overlay cell per source pixel.
        view("percent", script::Step::View(ViewStep::Percent(100.0))).percent(100.0),
        // Back to Fit.
        view("fit", script::Step::View(ViewStep::Fit)).fit(),
        // Both overlays off again; the photograph is untouched underneath.
        view(
            "overlays-off",
            script::Step::Workspace(
                WorkspaceStep::default()
                    .clip_shadows(false)
                    .clip_highlights(false),
            ),
        )
        .workspace("clip_shadows", json!(false))
        .workspace("clip_highlights", json!(false)),
        // The Original entry, whose counts are the fixture's own again.
        view("original", script::Step::Preview(PreviewStep::Sequence(0))),
        // Back to current, because a gesture is refused while a historical entry is shown.
        view("current", script::Step::Preview(PreviewStep::Current)),
        // The drafted render's counts are the reference's: with the GPU preview off a drag is
        // drawn by the reference renderer, whose frames of this small fixture are its exact
        // frames, each reduced into a report. With it on, the plot shows the counts of the frame
        // in motion, marked updating, which the GPU steps below hold.
        Step::new(
            "gpu-preview-off",
            luxforge_evidence::PaletteStep::Run("gpu preview".into()),
        )
        .commits(0)
        .workspace("gpu_preview", json!(false)),
        // An Exposure drag left open, so the photograph on screen is the drafted render.
        Step::new("drag", SliderStep::new("set-basic", "exposure", [0.5, 1.0]))
            .commits(0)
            .draft("set-basic", json!({"exposure": 1.0}))
            .no_layer(BASIC_EFFECT),
        // The same gesture released, which commits once; the plot follows the new stack.
        Step::new(
            "release",
            SliderStep::new("set-basic", "exposure", [1.0]).release(),
        )
        .commits(1)
        .no_draft()
        .label("Exposure +1.00 EV")
        .payload(BASIC_EFFECT, json!({"exposure": 1.0})),
        // The GPU preview back on, and the history undone to the Original, whose stack the GPU
        // draws: its counts are the GPU's.
        Step::new(
            "gpu-preview-on",
            luxforge_evidence::PaletteStep::Run("gpu preview".into()),
        )
        .commits(0)
        .workspace("gpu_preview", json!(true)),
        Step::new("undo-exposure", script::Step::api("history.undo")).commits(1),
        Step::new("undo-pixel", script::Step::api("history.undo"))
            .commits(1)
            .no_layer(BASIC_EFFECT),
        // An Exposure drag left open on the GPU: the plot shows the counts of the frame in motion,
        // marked updating.
        Step::new(
            "gpu-drag",
            SliderStep::new("set-basic", "exposure", [0.5, 1.0]),
        )
        .commits(0)
        .draft("set-basic", json!({"exposure": 1.0}))
        .no_layer(BASIC_EFFECT),
        // Released: the GPU presents the committed stack with no CPU render, and its tiles'
        // counts are its report.
        Step::new(
            "gpu-release",
            SliderStep::new("set-basic", "exposure", [1.0]).release(),
        )
        .commits(1)
        .no_draft()
        .label("Exposure +1.00 EV")
        .payload(BASIC_EFFECT, json!({"exposure": 1.0})),
        // The owner's store holds that report: an agent's request for the current stack is
        // answered at once.
        view(
            "analysis",
            script::Step::call("analysis.request", json!({"target": {"kind": "current"}})),
        ),
    ])
}

/// Every `basic-crop` frame: a Basic commit, then a 16:9 fit and a 7 degree straighten over it.
/// Its frames prove the counts describe the composition **after** the crop, and that the
/// photograph is placed at the ratio the committed payload declares.
pub fn crop_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        // One Basic commit, so every later frame composes colour with geometry.
        Step::new(
            "exposure",
            script::Step::call("edit.set-basic", json!({"exposure":1.0})),
        )
        .commits(1)
        .label("Exposure +1.00 EV")
        .payload(BASIC_EFFECT, json!({"exposure": 1.0})),
        // A 16:9 fit at angle zero, which is an exact copy of its input stage.
        Step::new(
            "fit",
            script::Step::call("edit.crop-fit", json!({"aspect":"16:9","angle":0.0})),
        )
        .commits(1)
        .label("Crop 16:9")
        .same_layer(BASIC_EFFECT, "exposure"),
        // The same ratio straightened by 7 degrees, which resamples: the one crop layer updated in
        // place.
        Step::new(
            "straightened",
            script::Step::call("edit.crop-fit", json!({"aspect":"16:9","angle":7.0})),
        )
        .commits(1)
        .label("Crop 16:9")
        .same_layer(CROP_EFFECT, "fit")
        .same_layer(BASIC_EFFECT, "exposure"),
    ])
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
    let source = luxforge_core::open_source(&root.join(FIXTURE))?;
    let registry = ModuleRegistry::developer();
    let context = luxforge_core::RenderContext::new();
    let options = luxforge_core::RenderOptions::default();
    let raster =
        core_render(&registry, &source, recipe, options, &context)?.frame(SnapshotId::new())?;
    Ok(analysis::reduce(
        &raster.rgba,
        raster.width,
        raster.height,
        &Cancel::never(),
    )?)
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
                    id: luxforge_core::LayerId::parse(
                        layer["id"].as_str().ok_or("A displayed layer has no id")?,
                    )?,
                    effect_id: layer["effect"]
                        .as_str()
                        .ok_or("A displayed layer has no effect")?
                        .to_owned(),
                    effect_format: luxforge_core::EFFECT_FORMAT,
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
    let index = ModuleRegistry::developer().insertion_index_for(&recipe.layers, BASIC_EFFECT);
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

/// Whether `frame`'s counts are the GPU's, taken over the stack's tiles, rather than the reference
/// renderer's reduction.
fn gpu_counted(frame: &Value) -> bool {
    frame["state"]["histogram"]["source"] == json!("gpu")
}

/// The most a GPU clipping count may differ from the independent reduction's: 0.1% of the output
/// pixels.
fn tolerated(report: &analysis::Report) -> u64 {
    (luxforge_reference::tolerance::HISTOGRAM_FRACTION * report.pixel_count() as f64).floor() as u64
}

/// Check one frame's histogram against an independent reduction, counter by counter: exactly
/// where the reference renderer reduced it, within the recorded tolerance where the GPU counted it.
fn expect_counts(frame: &Value, report: &analysis::Report, what: &str) -> Result<Value> {
    let state = &frame["state"]["histogram"];
    let gpu = gpu_counted(frame);
    ensure(
        state["status"] == json!("ready"),
        format!("{what}: histogram status is {}", state["status"]),
    )?;
    ensure(
        state["stale"] == json!(false),
        format!("{what}: the shown counts are stale"),
    )?;
    // The plot carries no caption at all, neither under it nor on hover (owner, 2026-09-26).
    ensure(
        state.get("caption").is_none() && state["tooltips"].get("plot").is_none(),
        format!(
            "{what}: the plot states a caption {} (tooltip {})",
            state["caption"], state["tooltips"]["plot"]
        ),
    )?;
    // A frame with a report draws nothing over the plot: the notice is only for a missing one.
    ensure(
        state["notice"] == Value::Null,
        format!("{what}: the plot carries the notice {}", state["notice"]),
    )?;
    // The triangles' tooltips state the counts in words: the independent reduction's where the
    // reference counted, and where the GPU did, the frame's own counts, which are held to the
    // reduction's below.
    let said = |name: &str| {
        if gpu {
            counters(frame)[name].as_u64().unwrap_or(u64::MAX)
        } else {
            match name {
                "r0" => report.r0,
                "g0" => report.g0,
                "b0" => report.b0,
                "r255" => report.r255,
                "g255" => report.g255,
                "b255" => report.b255,
                "any_shadow" => report.any_shadow,
                "any_highlight" => report.any_highlight,
                "all_shadow" => report.all_shadow,
                "all_highlight" => report.all_highlight,
                _ => report.both,
            }
        }
    };
    let shadow = format!(
        "{RULE}\n0 \u{b7} R {} G {} B {} \u{b7} any {} \u{b7} all {}",
        said("r0"),
        said("g0"),
        said("b0"),
        said("any_shadow"),
        said("all_shadow")
    );
    let highlight = format!(
        "{RULE}\n255 \u{b7} R {} G {} B {} \u{b7} any {} \u{b7} all {}\nboth {}",
        said("r255"),
        said("g255"),
        said("b255"),
        said("any_highlight"),
        said("all_highlight"),
        said("both")
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
    let limit = tolerated(report);
    let within = |shown: &Value, expected: &Value| {
        shown
            .as_u64()
            .zip(expected.as_u64())
            .is_some_and(|(shown, expected)| shown.abs_diff(expected) <= limit)
    };
    if gpu {
        let counted = counters(frame);
        let past: Vec<&str> = luxforge_reference::tolerance::CLIPPING
            .iter()
            .copied()
            .filter(|name| !within(&counted[*name], &expected[*name]))
            .collect();
        ensure(
            past.is_empty(),
            format!(
                "{what}: the GPU's counters {counted} pass {limit} pixels from the independent \
                 reduction's {expected} at {past:?}"
            ),
        )?;
    } else {
        ensure(
            counters(frame) == &expected,
            format!(
                "{what}: counters are {}, the independent reduction says {expected}",
                counters(frame)
            ),
        )?;
    }
    // The plot's shared scale is the largest count in any channel, which is checkable too.
    let max = report
        .r
        .iter()
        .chain(report.g.iter())
        .chain(report.b.iter())
        .copied()
        .max()
        .unwrap_or(0);
    // A bin the GPU counts differently moves the tallest: held here to the clipping counts'
    // tolerance, which this fixture's tallest bin meets.
    ensure(
        state["plotted_max"] == json!(max) || (gpu && within(&state["plotted_max"], &json!(max))),
        format!(
            "{what}: one full-height bin stands for {}, the reduction's tallest bin is {max}",
            state["plotted_max"]
        ),
    )?;
    Ok(json!({
        "counters": counters(frame),
        "reduced": expected,
        "plotted_max": state["plotted_max"],
        "identity": state["identity"],
        "source": state["source"],
        "tolerance_pixels": gpu.then_some(limit),
    }))
}

/// The photograph's exact rectangle in a capture: the one the editor records drawing it in, with its
/// edges where the drawn pixels end. The overlay checks below map single source pixels into it, so
/// it must be exact to the pixel.
fn photo_rect(frame: &Frame) -> Result<([u32; 4], &image::RgbImage)> {
    let rect = frame.photo_edges(|pixel| pixel != pixels::CANVAS)?;
    Ok((rect, frame.image()?))
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
/// What the histogram, the sample and the overlays show at each step, once the plan has held.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let root = run.root();
    let opened = launch.at("opened")?;
    let pixel = launch.at("pixel")?;
    let sample = launch.at("sample")?;
    let shadows = launch.at("shadows")?;
    let both = launch.at("both")?;
    let percent = launch.at("percent")?;
    let fit = launch.at("fit")?;
    let off = launch.at("overlays-off")?;
    let original = launch.at("original")?;
    let current = launch.at("current")?;
    let drag = launch.at("drag")?;
    let release = launch.at("release")?;
    let mut checks = Checks::new();

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

    // The default screen. The histogram is ready and its counts are the fixture's own.
    let opened_counts = expect_counts(opened, &plain, "the opened fixture")?;
    ensure(
        opened["state"]["histogram"]["overlay"] == Value::Null,
        "An overlay was derived before either flag was set",
    )?;
    checks.note(
        opened,
        "the default screen with the histogram ready, counts equal to an independent reduction",
        json!({"histogram":opened_counts,"pixels":opened.fixture(Fixture::fit(1))?}),
    );

    // One both-endpoint pixel committed. The counts follow the new stack exactly. The plan holds
    // that the step commits once; this holds which revision that is, a fresh catalog's first.
    ensure(
        pixel["state"]["stack"]["revision"] == json!(1),
        "edit.set-pixel did not commit revision 1",
    )?;
    let after = expect_counts(pixel, &edited, "after edit.set-pixel")?;
    checks.note(
        pixel,
        "one both-endpoint pixel committed; the plot follows the new stack",
        after,
    );

    // The public point query of that very pixel, in output codes, recorded with its step.
    let answer = &sample["step"]["result"];
    ensure(
        answer["rgba"] == json!([BOTH_RGB[0], BOTH_RGB[1], BOTH_RGB[2], 255]),
        format!("render.sample answered {answer}, expected the pixel just set"),
    )?;
    // A point query reads one pixel and changes nothing on screen.
    ensure(
        counters(sample) == counters(pixel),
        "Sampling changed the histogram",
    )?;
    checks.note(
        sample,
        "render.sample of the pixel that was just set answers its three codes; the histogram is unchanged",
        json!({"sample": answer}),
    );

    // The reference the overlay frames are compared against: the same stack, the same zoom, the
    // same panels, both overlays off. Every mask check below is a difference from this, so the
    // fixture's own strongly coloured quadrants cancel out instead of being mistaken for a mask.
    let (bare_rect, bare) = photo_rect(sample)?;
    let bare_radius = radius(bare_rect);

    // The shadow overlay alone, which the plan holds to its flags. Blue over the black dashes,
    // and nothing red anywhere.
    let overlay = &shadows["state"]["histogram"]["overlay"];
    ensure(
        overlay["cells"] == json!([SOURCE.0, SOURCE.1]),
        format!(
            "The Fit overlay grid is {}, expected one cell per source pixel",
            overlay["cells"]
        ),
    )?;
    ensure(overlay["drawn"] == json!(true), "The overlay was not drawn")?;
    let (shadows_rect, shadows_image) = photo_rect(shadows)?;
    same_rect(shadows_rect, bare_rect, "the shadow overlay")?;
    let shadow_on_dash = window(shadows_image, bare, shadows_rect, DASH, bare_radius);
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
        shadows_image,
        bare,
        shadows_rect,
        &[LINE],
        bare_radius,
        "the shadow overlay alone touched the 255 line",
    )?;
    untouched(
        shadows_image,
        bare,
        shadows_rect,
        &CLEAN,
        bare_radius,
        "the shadow overlay reached an unclipped quadrant",
    )?;
    // The committed stack is untouched by a view flag: the plan holds the revision and the entry,
    // and this holds everything else the stack records, its layers included.
    ensure(
        shadows["state"]["stack"] == pixel["state"]["stack"],
        "Switching an overlay on changed the committed stack",
    )?;
    checks.note(
        shadows,
        "the shadow overlay: blue over the pixels with a channel at 0, nothing over the 255 line",
        json!({"overlay":overlay,"photo_rect":shadows_rect,"window_radius":bare_radius}),
    );

    // Both overlays. Blue over the dashes, red over the white line, magenta where both hold at
    // once. The fixture has no pixel at both endpoints of its own, so the magenta comes from the
    // one pixel the scenario set; that is the isolated-pixel case a Fit overlay must keep.
    let (both_rect, both_image) = photo_rect(both)?;
    same_rect(both_rect, bare_rect, "both overlays")?;
    ensure(
        window(both_image, bare, both_rect, DASH, bare_radius)
            .iter()
            .any(|delta| delta.is_shadow_mask()),
        format!("No blue overlay over the black dash band at source {DASH:?}"),
    )?;
    ensure(
        window(both_image, bare, both_rect, LINE, bare_radius)
            .iter()
            .any(|delta| delta.is_highlight_mask()),
        format!("No red overlay over the white centre line at source {LINE:?}"),
    )?;
    // Magenta is measured against the shadow-only frame rather than the bare one: turning the
    // highlight flag on changes that one cell from the shadow mask to the both mask, which adds
    // red and nothing else, whatever the photograph underneath is.
    let both_upgrade = window(
        both_image,
        shadows_image,
        both_rect,
        BOTH_PIXEL,
        bare_radius,
    );
    ensure(
        both_upgrade.iter().any(|delta| delta.is_both_upgrade()),
        format!(
            "The isolated both-endpoint pixel at source {BOTH_PIXEL:?} did not turn magenta; the largest change from the shadow-only frame was {}",
            both_upgrade.iter().map(|d| d.largest()).max().unwrap_or(0)
        ),
    )?;
    untouched(
        both_image,
        bare,
        both_rect,
        &CLEAN,
        bare_radius,
        "an overlay reached an unclipped quadrant",
    )?;
    checks.note(
        both,
        "both overlays: blue at code 0, red at code 255, magenta on the one pixel at both",
        json!({
            "overlay": both["state"]["histogram"]["overlay"],
            "photo_rect": both_rect,
            "window_radius": bare_radius,
            "note": "the fixture's own quadrant colours reach neither endpoint, so the only magenta is the isolated pixel this scenario set",
        }),
    );

    // 100%. One overlay cell per source pixel, and the masks still land on their pixels. There is
    // no overlay-off frame at 100% to difference against, so the two checks here are the ones
    // whose base colour is unambiguous on its own: the dash band is black and the centre line is
    // white, so a blue-dominant dash and a red-dominant line can only be the masks.
    ensure(
        percent["state"]["histogram"]["overlay"]["cells"] == json!([SOURCE.0, SOURCE.1]),
        format!(
            "The 100% overlay grid is {}",
            percent["state"]["histogram"]["overlay"]["cells"]
        ),
    )?;
    let (percent_rect, percent_image) = photo_rect(percent)?;
    let hundred_width = percent_rect[2] - percent_rect[0];
    ensure(
        hundred_width.abs_diff(SOURCE.0) <= 2,
        format!(
            "At 100% the photograph is {hundred_width} physical pixels wide, expected {}",
            SOURCE.0
        ),
    )?;
    let percent_radius = radius(percent_rect);
    let black = image::Rgb([0u8, 0, 0]);
    let white = image::Rgb([255u8, 255, 255]);
    let against = |base: image::Rgb<u8>, point: (u32, u32)| {
        let (cx, cy) = map(percent_rect, point);
        let mut deltas = Vec::new();
        for dy in -percent_radius..=percent_radius {
            for dx in -percent_radius..=percent_radius {
                let x = i64::from(cx) + dx;
                let y = i64::from(cy) + dy;
                if x < 0
                    || y < 0
                    || x >= i64::from(percent_image.width())
                    || y >= i64::from(percent_image.height())
                {
                    continue;
                }
                deltas.push(Delta::between(
                    percent_image.get_pixel(x as u32, y as u32).0,
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
        counters(percent) == counters(pixel),
        "Zooming changed the histogram",
    )?;
    checks.note(
        percent,
        "100% with both overlays on: one cell per physical pixel, still aligned",
        json!({"photo_rect":percent_rect,"physical_width":hundred_width,"overlay":percent["state"]["histogram"]["overlay"]}),
    );

    // Back to Fit, still no re-analysis.
    ensure(
        counters(fit) == counters(pixel),
        "Returning to Fit changed the histogram",
    )?;
    checks.note(
        fit,
        "back at Fit with both overlays on",
        counters(fit).clone(),
    );

    // Both overlays off. The photograph underneath is byte for byte the frame from before either
    // flag was set, at every point the masks had covered.
    ensure(
        off["state"]["histogram"]["overlay"] == Value::Null,
        "An overlay is still derived with both flags off",
    )?;
    let (off_rect, off_image) = photo_rect(off)?;
    same_rect(off_rect, bare_rect, "the overlays switched off")?;
    let mut covered = vec![DASH, LINE, BOTH_PIXEL];
    covered.extend(CLEAN);
    untouched(
        off_image,
        bare,
        off_rect,
        &covered,
        bare_radius,
        "an overlay survived both flags being switched off",
    )?;
    ensure(
        counters(off) == counters(pixel),
        "Switching the overlays off changed the histogram",
    )?;
    checks.note(
        off,
        "both overlays off: the photograph is the fixture again and the counts are unchanged",
        json!({"photo_rect":off_rect,"points_compared":covered}),
    );

    // The Original entry. The plot follows the displayed generation, not the newest stack.
    let original_counts = expect_counts(original, &plain, "previewing the Original")?;
    ensure(
        counters(original) != counters(pixel),
        "The Original's counts are indistinguishable from the edited stack's",
    )?;
    ensure(
        original["state"]["histogram"]["identity"]["entry"]
            != pixel["state"]["histogram"]["identity"]["entry"],
        "The plot still names the entry the edited stack belongs to",
    )?;
    checks.note(
        original,
        "previewing the Original: the plot follows the displayed entry, not the newest one",
        original_counts,
    );

    // Back to current. A gesture is refused while a historical entry is shown, so the run returns
    // first; the plot is the edited stack's again.
    ensure(
        counters(current) == counters(pixel),
        "Returning to current did not restore the edited stack's counts",
    )?;
    checks.note(
        current,
        "back to current before the gesture",
        counters(current).clone(),
    );

    // An Exposure drag left open, which the plan holds as the draft and nothing committed. The
    // photograph on screen is the drafted render, and the contract ties the counts to the image
    // presented, drafts included — so the inspector describes that render: its identity carries
    // the draft revision the pixels were planned from and its counters equal an independent render
    // and reduction of the drafted stack, computed here from the layers the frame says it displays
    // and the drafted payload it records.
    let drafted = drag.draft();
    drag.displays_draft()?;
    let drafted_report = reduction(root, &drafted_recipe(drag)?)?;
    let drafted_detail = expect_counts(drag, &drafted_report, "the open gesture")?;
    ensure(
        drag["state"]["histogram"]["identity"]["draft_revision"] == drafted["draft_revision"],
        format!(
            "The plot names draft revision {} while the frame displays {}",
            drag["state"]["histogram"]["identity"]["draft_revision"], drafted["draft_revision"]
        ),
    )?;
    // The drafted exposure is +1 EV, so it clips highlights the committed stack does not: the
    // counts are demonstrably the drafted population rather than the previous frame's relabelled.
    ensure(
        counters(drag) != counters(current),
        "The drafted exposure left the counts identical to the committed frame's",
    )?;
    checks.note(
        drag,
        "an Exposure drag left open: the photograph is the drafted render and the plot is that render, its identity carrying the draft revision and its counts equal to an independent reduction of the drafted stack",
        json!({"draft": drafted, "histogram": drafted_detail}),
    );

    // The gesture released, which the plan holds as one commit and no draft. The plot follows the
    // composed stack: an independent render and reduction of exactly the layers the frame says it
    // displays.
    let released = reduction(root, &displayed_recipe(release)?)?;
    let released_detail = expect_counts(release, &released, "the released gesture")?;
    ensure(
        counters(release) != counters(current),
        "The committed exposure left the counts unchanged",
    )?;
    checks.note(
        release,
        "the gesture released: one commit, and the counts equal an independent reduction of the composed pixel-and-exposure stack",
        released_detail,
    );

    // The GPU steps. Undone to the Original, whose empty stack the GPU draws at rest: its counts
    // are the GPU's, within the tolerance of the fixture's own.
    let undone = launch.at("undo-pixel")?;
    let undone_counts = expect_counts(undone, &plain, "undone to the Original")?;
    ensure(
        gpu_counted(undone),
        format!(
            "Undone to the Original, the counts are the {}'s, not the GPU's",
            undone["state"]["histogram"]["source"]
        ),
    )?;
    checks.note(
        undone,
        "undone to the Original, which the GPU draws: its tiles' counts are the report, within the tolerance of an independent reduction",
        undone_counts,
    );
    // The drag left open on the GPU: the plot is marked updating; where the GPU's counts of the
    // frame in motion are in, they name the drafted revision they were drawn for.
    let gpu_drag = launch.at("gpu-drag")?;
    let gpu_drafted = gpu_drag.draft();
    let histogram = &gpu_drag["state"]["histogram"];
    ensure(
        histogram["stale"] == json!(true) || histogram["source"] != json!("motion"),
        format!("The counts of the frame in motion are not marked updating: {histogram}"),
    )?;
    if histogram["source"] == json!("motion") {
        ensure(
            histogram["identity"]["draft_revision"].as_u64()
                <= gpu_drafted["draft_revision"].as_u64(),
            format!(
                "The counts in motion name draft revision {} past the frame's {}",
                histogram["identity"]["draft_revision"], gpu_drafted["draft_revision"]
            ),
        )?;
    }
    checks.note(
        gpu_drag,
        "an Exposure drag left open on the GPU: the plot shows the frame in motion's counts, marked updating",
        json!({"draft": gpu_drafted, "histogram": histogram}),
    );
    // Released: the GPU presents the stack, and its tiles' counts are its report.
    let gpu_release = launch.at("gpu-release")?;
    let gpu_released = reduction(root, &displayed_recipe(gpu_release)?)?;
    let gpu_released_detail = expect_counts(gpu_release, &gpu_released, "the release on the GPU")?;
    // A release whose committed stack adds a layer's units compiles its picture at rest first
    // ([GPU-first](docs/design/gpu-first.md), proposals): while it does, the stack the GPU
    // presented is refused, named `compiling`, and the reference counts it.
    let refused_compiling = launch.events.iter().any(|event| {
        event["event"] == "gpu_presented_refused" && event["detail"]["why"] == "compiling"
    });
    ensure(
        gpu_counted(gpu_release) || refused_compiling,
        format!(
            "The release's counts are the {}'s, not the GPU's, and nothing was refused while compiling",
            gpu_release["state"]["histogram"]["source"]
        ),
    )?;
    checks.note(
        gpu_release,
        "the release on the GPU: the stack presented with no CPU render, its tiles' counts within the tolerance of an independent reduction of the composed stack, or, refused while its picture at rest compiled, the reference's exactly",
        json!({"counts": gpu_released_detail, "refused_compiling": refused_compiling}),
    );
    // The owner's store holds that report under the released stack's identity: an agent's
    // request is answered at once, its bins within the recorded tolerance of the independent
    // reduction's, channel by channel: the earth mover's distance within a quarter of a code.
    let asked = launch.at("analysis")?;
    let answer = &asked["step"]["result"];
    ensure(
        answer["status"] == json!("ready"),
        format!(
            "analysis.request answered {}, not a ready hit",
            answer["status"]
        ),
    )?;
    ensure(
        answer["identity"]["entry_id"] == gpu_release["state"]["histogram"]["identity"]["entry"],
        format!(
            "analysis.request answered entry {}, the released frame names {}",
            answer["identity"]["entry_id"], gpu_release["state"]["histogram"]["identity"]["entry"]
        ),
    )?;
    let bins = |name: &str| -> Vec<u64> {
        answer["result"][name]
            .as_array()
            .map(|bins| bins.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default()
    };
    let moved: Vec<f64> = [
        (bins("r"), &gpu_released.r),
        (bins("g"), &gpu_released.g),
        (bins("b"), &gpu_released.b),
    ]
    .iter()
    .map(
        |(answered, reduced)| match <[u64; 256]>::try_from(answered.as_slice()) {
            Ok(answered) => luxforge_reference::tolerance::histogram_emd(&answered, reduced),
            Err(_) => f64::INFINITY,
        },
    )
    .collect();
    let limit = luxforge_reference::tolerance::HISTOGRAM_EMD_CODES;
    ensure(
        moved.iter().all(|codes| *codes <= limit),
        format!(
            "analysis.request's bins are {moved:?} codes from the independent reduction's by the earth mover's distance, past {limit}"
        ),
    )?;
    checks.note(
        asked,
        "analysis.request for the current stack is a ready hit on the GPU's report, each channel within the earth mover's distance tolerance",
        json!({"status": answer["status"], "emd_codes": moved, "tolerance_codes": limit}),
    );

    // Every frame the run presented reports its own render time, and each captured status bar
    // states one of them rather than the time since the open.
    let render_times = crate::smoke::expect_render_times(&launch.events, &launch.frames)?;
    checks.write(
        &launch.evidence,
        "histogram",
        json!({"render_times": render_times}),
    )
}

// ---------------------------------------------------------------------------------------------
// The `basic-crop` scenario: colour composed with geometry, on the real editor.
// ---------------------------------------------------------------------------------------------

/// The ratio the two crop frames commit.
const CROP_RATIO: f64 = 16.0 / 9.0;

/// How bright the photograph's own pixels read at its edges, as a mean of their channels, which
/// ties its recorded rectangle to what was drawn: well above the canvas surface (`#19191b`) and the
/// bars over it (`#232326`), and well below every quadrant of this fixture at any exposure this
/// scenario uses. Brightness rather than the fixture's quadrant colours, because a Basic edit moves
/// those colours.
const BRIGHT: u32 = 70;

/// The displayed ratio and centring of a captured photograph, against the ratio its committed crop
/// payload declares.
fn expect_placement(checks: &mut Checks, frame: &Frame, ratio: f64, what: &str) -> Result {
    let [left, top, right, bottom] = frame
        .photo_edges(|p| (u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])) / 3 >= BRIGHT)?;
    let [surface_left, surface_right] = frame
        .columns()?
        .ok_or("The frame records no photo surface")?;
    checks.compare(
        frame,
        &format!("{what}: the displayed ratio against the payload's"),
        f64::from(right - left) / f64::from(bottom - top),
        ratio,
        Tolerance::Under(0.02),
    )?;
    checks.compare(
        frame,
        &format!("{what}: the photograph's centre against the photo surface's"),
        f64::from(left + right) / 2.0,
        f64::from(surface_left + surface_right) / 2.0,
        Tolerance::Within(6.0),
    )?;
    checks.compare(
        frame,
        &format!("{what}: the photograph's width against 40% of the photo surface's"),
        f64::from(right - left),
        f64::from(surface_right - surface_left) * 0.4,
        Tolerance::Above(0.0),
    )
}

/// What the histogram and the placement show at each `basic-crop` step, once the plan has held.
pub fn verify_crop(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let root = run.root();
    let mut checks = Checks::new();

    // Every frame's counts are an independent render and reduction of exactly the layers that
    // frame says it displays, so the plot is proved against the composition rather than itself.
    for (step, frame) in launch.names().iter().zip(&launch.frames) {
        let recipe = displayed_recipe(frame)?;
        let report = reduction(root, &recipe)?;
        let counts = expect_counts(frame, &report, &format!("step {step:?}"))?;
        checks.note(
            frame,
            "the counts of an independent reduction of the displayed stack",
            json!({
                "stack": recipe.layers.iter().map(|layer| layer.effect_id.clone()).collect::<Vec<_>>(),
                "output": [report.width, report.height],
                "counts": counts,
            }),
        );
    }

    // The Basic commit is one entry, and it changes the population. The plan holds that each step
    // commits once and that the straighten keeps the crop layer; this holds which revision the
    // first commit is, a fresh catalog's first, so with the plan the straighten is revision 3.
    let exposure = launch.at("exposure")?;
    let fit = launch.at("fit")?;
    let straightened = launch.at("straightened")?;
    ensure(
        exposure.revision()? == 1,
        "edit.set-basic did not commit revision 1",
    )?;
    ensure(
        counters(exposure) != counters(launch.at("opened")?),
        "The exposure left the population unchanged",
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
        pixels_of(fit) < pixels_of(exposure) && pixels_of(straightened) < pixels_of(exposure),
        "A crop did not reduce the analysed output stage",
    )?;

    // Placement: each frame shows the photograph at the ratio its stack declares, centred in the
    // photo surface.
    for (step, ratio) in [
        ("opened", 3.0 / 2.0),
        ("exposure", 3.0 / 2.0),
        ("fit", CROP_RATIO),
        ("straightened", CROP_RATIO),
    ] {
        expect_placement(
            &mut checks,
            launch.at(step)?,
            ratio,
            &format!("step {step:?}"),
        )?;
    }

    checks.write(
        &launch.evidence,
        "basic-crop",
        json!({
            "crop_layer": straightened.layer_id(CROP_EFFECT).ok_or("The frame holds no crop layer")?,
            "scope": "Counts against an independent core render and reduction of the displayed stack; placement of the rectangle the editor records drawing, its edges checked against the pixels read back from the renderer",
        }),
    )
}
