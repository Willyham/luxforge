//! A boundary held over the window its output reads (`docs/design/gpu-preview.md`, "The held input
//! boundary"): the GPU frame over the window is the frame over the whole boundary stage, and at Fit
//! a crop drawn at its exact stage derives that window and is held to the bound and the budget
//! before it is derived.
//!
//! - **The same frame.** For an affine tail (a straightened crop), a projective one (a perspective
//!   warp with the crop) and a lens warp's coordinate grid, on both boundary formats, the surface's
//!   own assembled shader draws the same codes and the same `f32` output over the planner's window
//!   of the boundary stage as over the whole stage, and over that window grown to each edge of the
//!   stage: the tail clamps each tap to the rectangle the CPU's resample reads, then offsets it by
//!   the window's integer origin, so every tap reads the texel it reads in the whole stage. The
//!   planner's windows of crops at the stage's corners reach every edge of it. A spatial step
//!   (Texture and Clarity) over the window, which the planner grows by its filters' margin, draws
//!   the same frame too.
//! - **At Fit.** A tight crop of a photograph that fits the display is drawn at its exact stage:
//!   the tick derives the window its output reads, the boundary held is that window, and a slot
//!   over the budget derives no boundary and names it.
//! - **A chain's charge.** Before its boundary exists, a chained masked plan is held to the slot's
//!   own charge, each link's intermediate and the shared scratch pool counted, over a 100% region
//!   and at Fit's exact stage; the budget reads that pooled figure.
//! - **The paint harness at 100%.** A measurement on the generated 24 MP JPEG: the window of a
//!   chain of masked Presence layers is the region grown by every link's halo, and the desktop's
//!   figure for it is the slot's own.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing.
use super::{
    gpu_plan::surface_plan_at,
    gpu_preview::SurfaceReport,
    gpu_qualification::headless,
    message::{preview::PreviewMessage, sync::SyncMessage},
    testing::{attach_log, events, finish, logged, real_photo, slide},
    *,
};
use luxforge_core::{
    BASIC_EFFECT, BoundaryFormat, Cancel, GpuAnswer, GpuPlanRequest, Layer, LinearImage,
    LinearSettings, ModuleRegistry, PERSPECTIVE_EFFECT, PreviewSource, Recipe, Region,
    RenderContext, RenderOptions, SourceImage, Stage, gpu_plan, render,
};
use luxforge_gpu::{qualification::boundary_as, qualification::held};

pub(super) const WIDTH: u32 = 360;
pub(super) const HEIGHT: u32 = 240;

/// A value with detail at every scale, so a tap that reads another texel, or the same one with
/// another weight, changes the frame.
fn value(x: u32, y: u32, channel: u32) -> f64 {
    let (x, y) = (f64::from(x), f64::from(y));
    let wave = (x * 0.31 + y * 0.17 + f64::from(channel)).sin() * 0.3
        + (x * 0.023 - y * 0.041 * f64::from(channel + 1)).cos() * 0.15;
    (0.45 + wave + ((x * 7.0 + y * 3.0) % 11.0) / 60.0).clamp(0.0, 1.0)
}

/// The photograph on either path: a JPEG's codes, or a RAW's linear planes, some values past
/// white and below black.
pub(super) fn source(format: BoundaryFormat) -> PreviewSource {
    match format {
        BoundaryFormat::Half => {
            let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    for channel in 0..3 {
                        rgba.push((value(x, y, channel) * 255.0).round() as u8);
                    }
                    rgba.push(255);
                }
            }
            PreviewSource::Jpeg(SourceImage {
                width: WIDTH,
                height: HEIGHT,
                rgba: rgba.into(),
                fingerprint: "sha256:gpu-window".into(),
                orientation: 1,
                capture: Default::default(),
            })
        }
        BoundaryFormat::Float => {
            let mut planes = Vec::with_capacity((WIDTH * HEIGHT * 3) as usize);
            for channel in 0..3 {
                for y in 0..HEIGHT {
                    for x in 0..WIDTH {
                        planes.push((value(x, y, channel) * 1.4 - 0.05) as f32);
                    }
                }
            }
            PreviewSource::Raw {
                image: LinearImage::new(WIDTH, HEIGHT, planes).expect("an image"),
                settings: LinearSettings::default(),
            }
        }
    }
}

/// The boundary of a stack whose first layer is its boundary: the source's own values, each a
/// JPEG code's linear value or a RAW's `f32`, row by row.
pub(super) fn whole(source: &PreviewSource) -> Vec<[f32; 3]> {
    match source {
        PreviewSource::Jpeg(image) => {
            let table = luxforge_core::colour::srgb::decode_table();
            image
                .rgba
                .chunks_exact(4)
                .map(|pixel| [0, 1, 2].map(|c| table[usize::from(pixel[c])]))
                .collect()
        }
        PreviewSource::Raw { image, .. } => (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .map(|(x, y)| image.pixel(x, y).expect("a pixel"))
            .collect(),
    }
}

/// `pixels`, a whole stage's, cut to `window`.
pub(super) fn cut(pixels: &[[f32; 3]], window: Region) -> Vec<[f32; 3]> {
    (window.y0..window.y0 + window.height)
        .flat_map(|y| {
            let at = (y * WIDTH + window.x0) as usize;
            pixels[at..at + window.width as usize].iter().copied()
        })
        .collect()
}

/// `window` grown to the stage's edge on `side`: 0 left, 1 top, 2 right, 3 bottom.
fn to_edge(window: Region, side: usize) -> Region {
    let (x1, y1) = (window.x0 + window.width, window.y0 + window.height);
    let [x0, y0, x1, y1] = match side {
        0 => [0, window.y0, x1, y1],
        1 => [window.x0, 0, x1, y1],
        2 => [window.x0, window.y0, WIDTH, y1],
        _ => [window.x0, window.y0, x1, HEIGHT],
    };
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

fn crop(rect: [f64; 4]) -> Layer {
    let stage = luxforge_core::CropStage {
        width: WIDTH,
        height: HEIGHT,
        angle: 7.0,
    };
    let (box_width, box_height) = stage.bounding_box();
    let fitted = stage.fit_about_center(luxforge_core::BoxRect {
        x: rect[0] * box_width,
        y: rect[1] * box_height,
        width: rect[2] * box_width,
        height: rect[3] * box_height,
    });
    Layer::crop(fitted.normalized(&stage))
}

/// The windowed boundary and the whole one draw the same GPU frame, bit for bit: through an
/// affine, a projective and a lens warp's tail, on both formats, over the planner's window of
/// crops in the middle and at each corner of the stage, and over each window grown to every edge.
#[test]
fn gpu_window_a_windowed_boundary_draws_the_whole_boundarys_frame() {
    let Some(qualifier) =
        headless("gpu_window_a_windowed_boundary_draws_the_whole_boundarys_frame")
    else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let basic = Layer::new(
        BASIC_EFFECT,
        serde_json::json!({"exposure": 0.5, "contrast": 25.0, "saturation": 20.0}),
    );
    let perspective = Layer::new(
        PERSPECTIVE_EFFECT,
        serde_json::json!({"horizontal": 25, "vertical": -15}),
    );
    let lens = luxforge_core::qualification::lens_layer(-0.06, (WIDTH, HEIGHT));
    let tails: [(&str, Vec<Layer>); 4] = [
        ("affine", vec![]),
        ("projective", vec![perspective.clone()]),
        ("lens", vec![lens.clone()]),
        ("lens and perspective", vec![lens, perspective]),
    ];
    let crops = [
        ("middle", [0.35, 0.35, 0.3, 0.3]),
        ("wide", [0.0, 0.0, 1.0, 1.0]),
        ("top left", [0.0, 0.0, 0.3, 0.3]),
        ("top right", [0.7, 0.0, 0.3, 0.3]),
        ("bottom left", [0.0, 0.7, 0.3, 0.3]),
        ("bottom right", [0.7, 0.7, 0.3, 0.3]),
    ];
    let mut edges = [false; 4];
    let mut drawn = 0;
    for format in [BoundaryFormat::Half, BoundaryFormat::Float] {
        let source = source(format);
        let pixels = whole(&source);
        for (tail, warps) in &tails {
            for (place, rect) in crops {
                let name = format!("{format:?} {tail}, {place}");
                let mut layers = vec![basic.clone()];
                layers.extend(warps.iter().cloned());
                layers.push(crop(rect));
                let recipe = Recipe {
                    layers,
                    ..Recipe::default()
                };
                let request = GpuPlanRequest::exact(
                    0,
                    Stage {
                        width: WIDTH,
                        height: HEIGHT,
                    },
                )
                .qualifying();
                let request = match format {
                    BoundaryFormat::Float => request.linear(),
                    BoundaryFormat::Half => request,
                };
                let plan = match gpu_plan(&registry, &recipe, request).expect("a stack") {
                    GpuAnswer::Plan(plan) => *plan,
                    GpuAnswer::Fallback(reason) => panic!("{name}: {reason}"),
                };
                let geometry = &plan.geometry;
                match *tail {
                    "affine" => assert!(geometry.affine().is_some(), "{name}"),
                    "projective" => assert!(geometry.projective().is_some(), "{name}"),
                    _ => assert!(geometry.needs_grid(), "{name}"),
                }
                let output = geometry.output();
                let grid = geometry
                    .grid(
                        Region {
                            x0: 0,
                            y0: 0,
                            width: output.width,
                            height: output.height,
                        },
                        1.0,
                    )
                    .expect("a grid")
                    .map(|grid| super::gpu_plan::WarpGrid::new(&grid));
                // The boundary the worker renders for a Fit frame at this exact stage.
                let context = RenderContext::new();
                let exact = render(
                    &registry,
                    &source,
                    &recipe,
                    RenderOptions::exact(&Cancel::never()),
                    &context,
                )
                .expect("the exact render");
                let frame = luxforge_core::qualification::region_boundary(
                    &exact,
                    0,
                    [0, 0, output.width, output.height],
                    format,
                )
                .expect("the windowed boundary");
                let window = Region {
                    x0: frame.origin.0,
                    y0: frame.origin.1,
                    width: frame.width,
                    height: frame.height,
                };
                assert!(
                    window.pixels() < u64::from(WIDTH * HEIGHT) || place == "wide",
                    "{name}: a {window:?} window"
                );
                let reached = [
                    window.x0 == 0,
                    window.y0 == 0,
                    window.x0 + window.width == WIDTH,
                    window.y0 + window.height == HEIGHT,
                ];
                for (edge, reached) in edges.iter_mut().zip(reached) {
                    *edge |= reached && window.pixels() < u64::from(WIDTH * HEIGHT);
                }
                // The worker's texels are the whole stage's there.
                let expected = cut(&pixels, window);
                for (index, texel) in expected.iter().enumerate() {
                    let (x, y) = (index as u32 % window.width, index as u32 / window.width);
                    let stored = match format {
                        BoundaryFormat::Half => texel.map(held),
                        BoundaryFormat::Float => *texel,
                    };
                    assert_eq!(
                        frame.texel(x, y).expect("a texel").map(f32::to_bits),
                        stored.map(f32::to_bits),
                        "{name}: texel ({x}, {y}) of the window"
                    );
                }
                let draw = |window: Region| {
                    let held = boundary_as(
                        format,
                        window.width,
                        window.height,
                        1,
                        &cut(&pixels, window),
                    )
                    .expect("a boundary");
                    let converted =
                        surface_plan_at(&plan, held, (window.x0, window.y0), grid.as_ref())
                            .expect("a runnable plan");
                    let codes = qualifier.evaluate_codes(&converted).expect("the codes");
                    let values: Vec<[u32; 4]> = qualifier
                        .evaluate(&converted)
                        .expect("the values")
                        .iter()
                        .map(|texel| texel.map(f32::to_bits))
                        .collect();
                    (codes, values)
                };
                let reference = draw(Region {
                    x0: 0,
                    y0: 0,
                    width: WIDTH,
                    height: HEIGHT,
                });
                for (what, held) in [
                    ("the planner's window", window),
                    ("grown to the left edge", to_edge(window, 0)),
                    ("grown to the top edge", to_edge(window, 1)),
                    ("grown to the right edge", to_edge(window, 2)),
                    ("grown to the bottom edge", to_edge(window, 3)),
                ] {
                    let (codes, values) = draw(held);
                    assert!(
                        codes == reference.0,
                        "{name}: {what}, {held:?}, draws other codes"
                    );
                    assert!(
                        values == reference.1,
                        "{name}: {what}, {held:?}, draws other values"
                    );
                    drawn += 1;
                }
            }
        }
    }
    assert_eq!(
        edges, [true; 4],
        "the planner's windows reach every edge of the stage"
    );
    eprintln!("gpu_window: {drawn} windowed frames, each the whole boundary's bit for bit");
}

/// A spatial step over the window a crop reads, the planner's margin for its filters included,
/// draws what it draws over the whole boundary stage, but for the rounding of its running sums:
/// Texture and Clarity under a straightened crop, on both formats, within 2 × 10⁻³ in linear light
/// and the pointwise limits.
#[test]
fn gpu_window_a_spatial_step_over_a_window_draws_the_whole_boundarys_frame() {
    let Some(qualifier) =
        headless("gpu_window_a_spatial_step_over_a_window_draws_the_whole_boundarys_frame")
    else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let presence = Layer::new(
        luxforge_core::PRESENCE_EFFECT,
        serde_json::json!({"texture": 40.0, "clarity": 30.0}),
    );
    let mut cut_windows = 0;
    for format in [BoundaryFormat::Half, BoundaryFormat::Float] {
        let source = source(format);
        let pixels = whole(&source);
        for (place, rect) in [
            ("middle", [0.35, 0.35, 0.3, 0.3]),
            ("top left", [0.0, 0.0, 0.3, 0.3]),
            ("bottom right", [0.7, 0.7, 0.3, 0.3]),
        ] {
            let name = format!("{format:?}, {place}");
            let recipe = Recipe {
                layers: vec![presence.clone(), crop(rect)],
                ..Recipe::default()
            };
            let request = GpuPlanRequest::exact(
                0,
                Stage {
                    width: WIDTH,
                    height: HEIGHT,
                },
            )
            .qualifying();
            let request = match format {
                BoundaryFormat::Float => request.linear(),
                BoundaryFormat::Half => request,
            };
            let plan = match gpu_plan(&registry, &recipe, request).expect("a stack") {
                GpuAnswer::Plan(plan) => *plan,
                GpuAnswer::Fallback(reason) => panic!("{name}: {reason}"),
            };
            assert!(!plan.spatial.is_empty(), "{name}: a spatial step");
            let output = plan.geometry.output();
            let context = RenderContext::new();
            let exact = render(
                &registry,
                &source,
                &recipe,
                RenderOptions::exact(&Cancel::never()),
                &context,
            )
            .expect("the exact render");
            let frame = luxforge_core::qualification::region_boundary(
                &exact,
                0,
                [0, 0, output.width, output.height],
                format,
            )
            .expect("the windowed boundary");
            let window = Region {
                x0: frame.origin.0,
                y0: frame.origin.1,
                width: frame.width,
                height: frame.height,
            };
            // The window's origin moves down to the grid of the operation's tiles, so a crop away
            // from the origin of a stage smaller than a tile reads from the origin on.
            cut_windows += usize::from(window.pixels() < u64::from(WIDTH * HEIGHT));
            let draw = |window: Region| {
                let held = boundary_as(
                    format,
                    window.width,
                    window.height,
                    1,
                    &cut(&pixels, window),
                )
                .expect("a boundary");
                let converted = surface_plan_at(&plan, held, (window.x0, window.y0), None)
                    .expect("a runnable plan");
                (
                    qualifier.evaluate_codes(&converted).expect("the codes"),
                    qualifier.evaluate(&converted).expect("the values"),
                )
            };
            let (codes, values) = draw(window);
            let (whole_codes, whole_values) = draw(Region {
                x0: 0,
                y0: 0,
                width: WIDTH,
                height: HEIGHT,
            });
            let largest = values
                .iter()
                .zip(&whole_values)
                .flat_map(|(a, b)| (0..3).map(move |c| (a[c] - b[c]).abs()))
                .fold(0.0_f32, f32::max);
            let codes_differ = codes
                .iter()
                .zip(&whole_codes)
                .filter(|(a, b)| a != b)
                .count();
            // The window holds the operation's halo alone, its origin inside the stage where the
            // CPU's tile grid moved it to the stage's: a box mean's running sums are reseeded every
            // 16 texels from the boundary texture's own edge, so over a window they start at other
            // pixels than over the whole stage and round differently in `f32`, which the guided
            // filters' variances amplify where the picture is flat. With direct sums (a run of one)
            // every case is the whole boundary's bit for bit. Measured: at most 7.2 × 10⁻⁴ in linear
            // light (the `f32` boundary, middle crop), three pixels' codes and a worst block of
            // 0.0025; held to 2 × 10⁻³ and the pointwise limits against the whole boundary's frame.
            let rgb = |codes: &[[u8; 4]]| -> Vec<u8> {
                codes
                    .iter()
                    .flat_map(|code| [code[0], code[1], code[2]])
                    .collect()
            };
            let (drawn, whole) = (rgb(&codes), rgb(&whole_codes));
            let statistics = luxforge_reference::preview_error::compare(
                luxforge_reference::preview_error::Rgb8::new(output.width, output.height, &drawn)
                    .expect("a frame"),
                luxforge_reference::preview_error::Rgb8::new(output.width, output.height, &whole)
                    .expect("a frame"),
                [0, 0, output.width, output.height],
            )
            .expect("the comparison");
            eprintln!(
                "gpu_window spatial {name}: a {}x{} window of {WIDTH}x{HEIGHT}, largest value \
                 difference {largest:e}, {codes_differ} pixels' codes differ, {}",
                window.width,
                window.height,
                super::gpu_qualification::figures(&statistics)
            );
            assert!(largest <= 2e-3, "{name}: {largest}");
            assert!(
                luxforge_reference::preview_error::verdict(
                    &statistics,
                    luxforge_reference::preview_error::Class::Pointwise
                )
                .passed(),
                "{name}: {}",
                super::gpu_qualification::figures(&statistics)
            );
        }
    }
    assert!(cut_windows >= 4, "the windows cut the stage");
}

// ---- At Fit, at the exact stage -----------------------------------------------------------------

fn catalog(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "luxforge-gpu-window-{name}-{}.sqlite",
        std::process::id()
    ))
}

/// Take up the preview worker's results, as the worker's wake does, until `done`.
pub(super) fn deliver_until(
    editor: &mut Editor,
    what: &str,
    mut done: impl FnMut(&Editor) -> bool,
) {
    luxforge_testbase::wait_until(what, || {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        done(editor)
    });
}

/// The fixture photograph with a tight straightened crop committed by another client, as the
/// desktop then shows it: its output fits the display, so Fit draws its exact stage.
fn cropped_photo(catalog: &std::path::Path) -> Editor {
    let (editor, asset, agent) = real_photo(catalog);
    committed(
        editor,
        asset,
        agent,
        "edit.crop",
        serde_json::json!({"angle": 7.0, "x": 0.3, "y": 0.3, "width": 0.35, "height": 0.35}),
    )
}

/// `editor`'s photograph after another client commits `method` with `params`, as the desktop then
/// shows it.
fn committed(
    mut editor: Editor,
    asset: luxforge_core::AssetId,
    agent: luxforge_core::ClientId,
    method: &str,
    params: Value,
) -> Editor {
    answered(&mut editor, &asset, agent, method, params);
    editor
}

/// Another client commits `method` with `params` over `editor`'s photograph, which then shows it as
/// the desktop does: the method's answer.
pub(super) fn answered(
    editor: &mut Editor,
    asset: &luxforge_core::AssetId,
    agent: luxforge_core::ClientId,
    method: &str,
    mut params: Value,
) -> Value {
    let revision = editor.document.state.as_ref().unwrap().revision;
    params["asset_id"] = serde_json::json!(asset);
    params["mutation"] = serde_json::json!({
        "expected_revision": revision, "request_id": format!("window-{revision}"), "actor": "agent"
    });
    let (answer, _) = crate::app::tasks::call(&editor.owner, agent, method, params)
        .expect("the edit is accepted");
    let refreshed = crate::app::tasks::refresh(
        &editor.owner,
        editor.client,
        asset.clone(),
        crate::app::tasks::Scope::Open,
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        refreshed,
    )))));
    answer
}

/// A Fit drag over a tight crop drawn at its exact stage derives the window its output reads,
/// and holds that window, not the whole photograph; the charge it is held to before deriving it is
/// that window's.
#[test]
fn gpu_window_an_exact_fit_crop_holds_the_window_its_output_reads() {
    let catalog = catalog("held");
    let mut editor = cropped_photo(&catalog);
    editor.gpu.surface = Some(SurfaceReport::default());
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, "set-basic", "exposure", 0.2);
    let records = logged(&mut editor, &log);
    let ticks = events(&records, "gpu_preview_tick");
    assert_eq!(ticks[0]["reason"], "surface-pending", "{ticks:?}");
    let (boundary, slot) = editor
        .gpu
        .region_charge()
        .expect("held to the bound and budget");
    assert!(editor.gpu.holds_boundary(), "derived at the tick");
    deliver_until(&mut editor, "the tick's frame", |editor| {
        !editor.presentation.queue.is_busy() && !editor.presentation.queue.ready()
    });
    let (width, height) = editor.presentation.dimensions.expect("the crop's output");
    let summary = editor.gpu.summary();
    let held = &summary["drag"]["boundary"];
    let (held_width, held_height) = (
        held["width"].as_u64().unwrap(),
        held["height"].as_u64().unwrap(),
    );
    // The fixture is 480 × 320; the crop reads a window of it.
    assert!(
        held_width * held_height < 480 * 320 / 2,
        "a {held_width}x{held_height} boundary for a {width}x{height} crop"
    );
    assert!(held_width >= u64::from(width) && held_height >= u64::from(height));
    assert_eq!(held["region"], Value::Null, "at Fit");
    // A JPEG's boundary is eight bytes a texel; the slot adds the tail's intermediate, the 8-bit
    // codes its quantizing pass writes, and the frame: four bytes a texel of the photograph's
    // square bucket, its longer side's next power of two at this size, and its 96-byte uniform.
    assert_eq!(boundary, held_width * held_height * 8);
    let edge = u64::from(width.max(height).next_power_of_two());
    assert_eq!(slot, boundary + boundary / 2 + edge * edge * 4 + 96);
    let _ = editor.update(Message::Draft(
        crate::app::message::draft::DraftMessage::Cancel,
    ));
    finish(editor, catalog);
}

/// At Fit at the exact stage a slot the GPU-preview budget would not hold derives no boundary:
/// every tick takes the CPU path naming the budget, with the bytes and the budget they pass.
#[test]
fn gpu_window_an_exact_fit_slot_over_the_budget_keeps_the_cpu_path() {
    let catalog = catalog("budget");
    let mut editor = cropped_photo(&catalog);
    // A byte beside the source the surface holds, which the budget charges too.
    let source = editor.gpu.source().map_or(0, |source| source.bytes());
    editor.gpu.budget = Some(source + 1);
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, "set-basic", "exposure", 0.2);
    let _ = slide(&mut editor, "set-basic", "exposure", 0.3);
    let records = logged(&mut editor, &log);
    let ticks = events(&records, "gpu_preview_tick");
    assert!(
        ticks
            .iter()
            .all(|tick| tick["path"] == "cpu" && tick["reason"] == "budget-exceeded"),
        "{ticks:?}"
    );
    assert_eq!(editor.gpu.ticks().2, 0, "no boundary is derived");
    let (_, slot) = editor.gpu.region_charge().expect("a charge");
    assert_eq!(
        editor.gpu.summary()["drag"]["over_budget"],
        serde_json::json!({"requested": slot, "budget": 1})
    );
    let _ = editor.update(Message::Draft(
        crate::app::message::draft::DraftMessage::Cancel,
    ));
    finish(editor, catalog);
}

/// On the corpus's Z6 and Air 2S, whose lens profile the crop is fused with, a Fit drag of Basic
/// over a 16:9 crop straightened by 7°, the corpus's, and by 45° derives a windowed proxy's
/// boundary, holds it within the 256 MiB bound, and draws its later ticks on the GPU with no
/// preview job once the surface has evaluated it. The surface is stood in for, as in every desktop
/// test; the corpus draws the same plans on the device.
#[test]
#[ignore = "the corpus RAWs: set LUXFORGE_RAW_MANIFEST to the private RAW manifest"]
fn gpu_window_a_raw_straightened_crop_drag_at_fit_draws_on_the_gpu() {
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(std::env::var("LUXFORGE_RAW_MANIFEST").expect("a manifest"))
            .unwrap(),
    )
    .unwrap();
    for id in ["nikon-z6", "dji-air2s"] {
        let path = manifest["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["id"] == id)
            .and_then(|source| source["path"].as_str())
            .map(std::path::PathBuf::from)
            .expect("the RAW in the manifest");
        for angle in [7.0, 45.0] {
            let catalog = catalog(&format!("raw-{id}-{angle}"));
            let (editor, asset, agent) = crate::app::testing::real_photo_at(&catalog, &path);
            let mut editor = committed(
                editor,
                asset,
                agent,
                "edit.crop-fit",
                serde_json::json!({"aspect": "16:9", "angle": angle}),
            );
            editor.gpu.surface = Some(SurfaceReport::default());
            let _ = slide(&mut editor, "set-basic", "exposure", 0.2);
            deliver_until(&mut editor, "the boundary", |editor| {
                editor.gpu.holds_boundary()
                    || editor.gpu.summary()["drag"]["reason"] == "boundary-failed"
            });
            let summary = editor.gpu.summary();
            let held = &summary["drag"]["boundary"];
            assert!(held.is_object(), "{id} at {angle}°: {summary}");
            let (width, height) = (
                held["width"].as_u64().unwrap(),
                held["height"].as_u64().unwrap(),
            );
            let bytes = width * height * 16;
            assert!(
                bytes <= luxforge_core::BOUNDARY_MAX_BYTES,
                "{id} at {angle}°: {width}x{height}"
            );
            let version = editor.gpu.held_version().unwrap();
            editor.gpu.surface = Some(SurfaceReport {
                ready_boundary: Some(version),
                fallback: None,
                drawn: None,
                evaluated: None,
            });
            let log = attach_log(&mut editor);
            let _ = slide(&mut editor, "set-basic", "exposure", 0.3);
            let _ = slide(&mut editor, "set-basic", "exposure", 0.4);
            let records = logged(&mut editor, &log);
            let ticks = events(&records, "gpu_preview_tick");
            assert!(
                !ticks.is_empty() && ticks.iter().all(|tick| tick["path"] == "gpu"),
                "{id} at {angle}°: {ticks:?}"
            );
            assert_eq!(
                events(&records, "preview_job_requested").len(),
                0,
                "{id} at {angle}°: no preview job a tick"
            );
            eprintln!(
                "{id} at {angle}°: boundary {width}x{height} at {} ({bytes} B), {} GPU ticks",
                held["origin"],
                ticks.len()
            );
            let _ = editor.update(Message::Draft(
                crate::app::message::draft::DraftMessage::Cancel,
            ));
            finish(editor, catalog);
        }
    }
}

// ---- A chain's charge before its boundary exists ------------------------------------------------

/// Commit, as another client, a Basic layer, then three Presence layers of Texture and Clarity, each
/// through a radial mask of its own, then a global Presence layer, over `editor`'s photograph.
fn masked_chain(
    editor: &mut Editor,
    asset: &luxforge_core::AssetId,
    agent: luxforge_core::ClientId,
) {
    let basic = serde_json::json!({"exposure": 0.3});
    answered(editor, asset, agent, "edit.set-basic", basic);
    for x in [0.25, 0.5, 0.75] {
        let radial = serde_json::json!({"x": x, "y": 0.5, "radius_x": 0.15, "radius_y": 0.25,
                                        "angle": 0.0, "feather": 40.0});
        let mask = answered(editor, asset, agent, "mask.create-radial", radial)["mask"].clone();
        let presence = serde_json::json!({"mask": mask, "texture": 40.0, "clarity": 30.0});
        answered(editor, asset, agent, "edit.set-presence", presence);
    }
    let global = serde_json::json!({"texture": 20.0, "clarity": 15.0});
    answered(editor, asset, agent, "edit.set-presence", global);
}

/// Set `editor`'s view to 100% over its photograph once its first frame is in, as the zoom control
/// does.
fn at_100(editor: &mut Editor) {
    deliver_until(editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 100.0 };
}

/// A chained masked plan is held, before its boundary exists, to what the slot drawing it charges.
/// A Basic layer, three masked Presence layers of Texture and Clarity and a global one make a chain
/// of five links: Basic's, then each Presence layer's. Over a 100% region and at Fit's exact stage
/// under a tight crop, the desktop's figure (`region_charge`) is the surface's `slot_charge` for
/// the plan converted over the held boundary (`Qualifier::charged_bytes`) less what only the slot
/// knows: every link's words and blocks buffers, each at its 1 KiB least. Both count the boundary,
/// a tail's intermediate, and the output in its size bucket with its uniform alike
/// (`texture_charge`). The chain's own charge is the same over the steps the desktop converts with
/// no boundary as over those the surface is handed.
#[test]
fn gpu_window_a_chained_masked_plan_is_held_to_the_slots_own_charge() {
    use luxforge_gpu::chain_charge;
    let test = "gpu_window_a_chained_masked_plan_is_held_to_the_slots_own_charge";
    let Some(qualifier) = headless(test) else {
        return;
    };
    for at in ["100%", "Fit"] {
        let catalog = catalog(&format!("chain-{}", at.trim_end_matches('%')));
        let (mut editor, asset, agent) = real_photo(&catalog);
        if at == "Fit" {
            let crop = serde_json::json!({"angle": 7.0, "x": 0.3, "y": 0.3, "width": 0.35,
                                          "height": 0.35});
            answered(&mut editor, &asset, agent, "edit.crop", crop);
        }
        masked_chain(&mut editor, &asset, agent);
        if at == "100%" {
            at_100(&mut editor);
        }
        editor.gpu.surface = Some(SurfaceReport::default());
        let _ = slide(&mut editor, "set-basic", "exposure", 0.4);
        deliver_until(&mut editor, "the boundary", |editor| {
            editor.gpu.holds_boundary()
        });
        // The next tick converts its plan over the held boundary and hands it to the surface.
        let _ = slide(&mut editor, "set-basic", "exposure", 0.5);
        let (_, desktop) = editor
            .gpu
            .region_charge()
            .expect("held to the bound and budget");
        let (core, request) = editor.gpu.planned().expect("the tick's plan");
        assert_eq!(core.spatial.len(), 4, "{at}: an operation a Presence layer");
        assert_eq!(request.key.region().is_some(), at == "100%", "{at}");
        let (plan, _) = editor
            .gpu
            .surface_plan()
            .expect("the plan over the held boundary");
        let size = plan.boundary.size();
        let origin = plan.texels.origin.map(|value| value.max(0.0) as u32);
        let chain = chain_charge(
            &plan.steps,
            size,
            (origin[0], origin[1]),
            plan.boundary.format(),
        );
        assert_eq!(chain.kept.len(), 5, "{at}: five links");
        let window = Region {
            x0: origin[0],
            y0: origin[1],
            width: size.0,
            height: size.1,
        };
        assert_eq!(
            super::gpu_preview::chain_charge(core, window, request.format),
            chain.total(),
            "{at}: the chain's charge over the steps converted with no boundary"
        );
        let slot = qualifier.charged_bytes(plan).expect("a charge");
        let buffers = chain.kept.len() as u64 * 2 * 1024;
        eprintln!(
            "{test}: {at}: a {}x{} boundary at {origin:?}, the desktop {desktop} B, the slot \
             {slot} B, the chain {} B (intermediates {:?}, kept {:?}, pool {} B), buffers \
             {buffers} B",
            size.0,
            size.1,
            chain.total(),
            chain.intermediates,
            chain.kept,
            chain.pool
        );
        assert!(desktop <= slot, "{at}: {desktop} B of {slot} B");
        assert_eq!(desktop + buffers, slot, "{at}");
        let _ = editor.update(Message::Draft(
            crate::app::message::draft::DraftMessage::Cancel,
        ));
        finish(editor, catalog);
    }
}

/// The budget a chained masked plan's boundary is held to before it is derived reads the pooled
/// figure, beside the source the surface holds, which the same budget charges. Over a 100% region,
/// a budget the slot and the source fit to the byte derives the boundary, though the figure the
/// desktop held a plan to before the pool — every plane of every spatial step in a texture of its
/// own, with the passes' parameters and no link's intermediate — passes it, and so does that figure
/// with the intermediates, what each link holding its own scratch charged. A byte less names
/// `budget-exceeded`, with the pooled figure and what the budget leaves the slot in `over_budget`.
#[test]
fn gpu_window_a_chained_masked_plan_derives_its_boundary_when_its_pooled_slot_fits() {
    use luxforge_gpu::chain_charge;
    let catalog = catalog("chain-budget");
    let (mut editor, asset, agent) = real_photo(&catalog);
    masked_chain(&mut editor, &asset, agent);
    at_100(&mut editor);
    // No slot fits a budget of one byte: the first tick names the figure.
    editor.gpu.budget = Some(1);
    let _ = slide(&mut editor, "set-basic", "exposure", 0.4);
    let (_, pooled) = editor.gpu.region_charge().expect("a region plan");
    let (unshared, intermediates) = {
        let (plan, request) = editor.gpu.planned().expect("the tick's plan");
        let window = request.window.expect("the region's window");
        let (origin, size) = ((window.x0, window.y0), (window.width, window.height));
        let steps = super::gpu_plan::plan_steps(plan).expect("convertible steps");
        let format = request.format;
        let chain = chain_charge(&steps, size, origin, format);
        assert_eq!(chain.intermediates.len(), 4, "five links");
        // Each pass's 256-byte parameter slice, as the old figure counted them.
        let parameters: u64 = plan
            .spatial
            .iter()
            .map(|spatial| 256 * spatial.passes.len() as u64)
            .sum();
        let planes: u64 = plan
            .spatial
            .iter()
            .map(|spatial| spatial.plane_bytes(origin, size))
            .sum();
        (
            pooled - chain.total() + planes + parameters,
            chain.intermediates.iter().sum::<u64>(),
        )
    };
    eprintln!(
        "gpu_window budget: pooled {pooled} B, every plane apart {unshared} B, with the \
         intermediates {} B",
        unshared + intermediates
    );
    assert!(pooled < unshared, "{pooled} B, {unshared} B apart");
    // A byte less than the pooled figure beside the source.
    let source = editor.gpu.source().map_or(0, |source| source.bytes());
    editor.gpu.budget = Some(source + pooled - 1);
    let log = attach_log(&mut editor);
    let _ = slide(&mut editor, "set-basic", "exposure", 0.5);
    let records = logged(&mut editor, &log);
    let ticks = events(&records, "gpu_preview_tick");
    assert!(
        !ticks.is_empty()
            && ticks
                .iter()
                .all(|tick| tick["path"] == "cpu" && tick["reason"] == "budget-exceeded"),
        "{ticks:?}"
    );
    assert_eq!(editor.gpu.ticks().2, 0, "no boundary is derived");
    assert_eq!(
        editor.gpu.summary()["drag"]["over_budget"],
        serde_json::json!({"requested": pooled, "budget": pooled - 1})
    );
    // The pooled figure exactly, which the old figure and the slot of unshared links both pass.
    editor.gpu.budget = Some(source + pooled);
    let _ = slide(&mut editor, "set-basic", "exposure", 0.6);
    let summary = editor.gpu.summary();
    assert_eq!(editor.gpu.ticks().2, 1, "the boundary is derived");
    assert_eq!(summary["drag"]["reason"], "surface-pending");
    assert_eq!(summary["drag"]["over_budget"], Value::Null);
    let _ = editor.update(Message::Draft(
        crate::app::message::draft::DraftMessage::Cancel,
    ));
    finish(editor, catalog);
}

/// A photograph of `width` × `height` with detail at every scale, written as a JPEG into a directory
/// of its own: one the view shows a part of at 100%.
fn written(name: &str, width: u32, height: u32) -> std::path::PathBuf {
    let dir = luxforge_testbase::paths::temp_dir(&format!("gpu-window-{name}"));
    let path = dir.join("photograph.jpg");
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([0, 1, 2].map(|channel| (value(x, y, channel) * 255.0).round() as u8))
    });
    let file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    image::codecs::jpeg::JpegEncoder::new_with_quality(file, 95)
        .encode_image(&image)
        .expect("the photograph is written");
    path
}

/// A region drag at 100% whose slot, beside the source, passes the budget is drawn from the draft's
/// plan at the reduced stage of the view's area, the softer frame: the boundary is the source
/// reduced to that stage, the plan's frame its whole output placed over the photograph's full
/// stage, every tick drawn on the GPU while the notice says the frame is softer, and the drag stays
/// at that stage at this zoom; the picture at rest after its release is the region's, its own plan
/// at full scale.
#[test]
fn gpu_window_a_region_past_the_budget_draws_the_reduced_stage_scaled_to_the_view() {
    use super::gpu_preview_tests::surface_ready;
    let catalog = catalog("softer");
    let photograph = written("softer", 3000, 2000);
    let (mut editor, asset, agent) = crate::app::testing::real_photo_at(&catalog, &photograph);
    masked_chain(&mut editor, &asset, agent);
    // Every region plan carries its reduced stage, as one past the core's figure does.
    editor.gpu.reduce_after = Some(0);
    at_100(&mut editor);
    let stage = editor
        .presentation
        .dimensions
        .expect("the photograph's stage");
    let wanted = editor.desired_view_for(stage).expect("a visible region");
    assert!(
        wanted.width < stage.0 && wanted.height < stage.1,
        "the view shows part of the photograph: {wanted:?} of {stage:?}"
    );
    editor.gpu.surface = Some(SurfaceReport::default());
    // Within the budget: the region at full scale.
    let _ = slide(&mut editor, "set-basic", "exposure", 0.4);
    let (_, region) = editor.gpu.region_charge().expect("a region plan");
    assert!(
        editor.gpu.planned().unwrap().1.key.region().is_some(),
        "the region"
    );
    // A byte less than the region's slot beside the source.
    let source = editor.gpu.source().map_or(0, |source| source.bytes());
    editor.gpu.budget = Some(source + region - 1);
    let _ = slide(&mut editor, "set-basic", "exposure", 0.5);
    let summary = editor.gpu.summary();
    assert_eq!(
        summary["drag"]["softer"],
        serde_json::json!(100.0),
        "{summary}"
    );
    let (_, request) = editor.gpu.planned().expect("the tick's plan");
    let reduced = request.key.plan().expect("the reduced stage");
    assert_eq!(request.key.region(), None, "a whole frame");
    assert!(
        reduced.width < stage.0 && reduced.height < stage.1,
        "{reduced:?} of {stage:?}"
    );
    eprintln!(
        "softer: the region's slot {region} B, the reduced stage {}x{} of {stage:?}",
        reduced.width, reduced.height
    );
    // Its boundary is derived; once the surface has evaluated it, the tick draws on the GPU.
    assert!(editor.gpu.holds_boundary(), "the reduced source, derived");
    surface_ready(&mut editor);
    let _ = slide(&mut editor, "set-basic", "exposure", 0.6);
    assert_eq!(editor.gpu.ticks().0, 1, "a tick on the GPU");
    let (plan, _) = editor
        .gpu
        .surface_plan()
        .expect("the plan the surface draws");
    let placed = plan.region.expect("placed as a region");
    assert_eq!(placed.full_stage, stage, "over the photograph's full stage");
    assert_eq!(
        placed.stage,
        (reduced.width, reduced.height),
        "the reduced stage"
    );
    assert_eq!(
        placed.rect,
        [0, 0, reduced.width, reduced.height],
        "all of it"
    );
    assert!(
        editor.gpu_draws_view(wanted),
        "the GPU frame holds the view"
    );
    assert_eq!(
        editor.gpu_plan_fallback().as_deref(),
        Some(super::gpu_preview::SOFTER)
    );
    assert_eq!(
        editor
            .workspace
            .status
            .fallback
            .as_ref()
            .map(|notice| notice.phrase.as_str()),
        Some("Softer while dragging")
    );
    // At this zoom the drag stays at the reduced stage, whatever the budget does.
    editor.gpu.budget = Some(u64::MAX);
    let _ = slide(&mut editor, "set-basic", "exposure", 0.7);
    assert!(editor.gpu.planned().unwrap().1.key.plan().is_some());
    assert_eq!(editor.gpu.ticks().0, 2, "on the GPU");
    let _ = editor.update(Message::Draft(
        crate::app::message::draft::DraftMessage::Cancel,
    ));
    finish(editor, catalog);
}

/// A plan whose steps cannot be converted is charged every plane of its spatial operations in a
/// texture of its own and an intermediate for each (`unconverted_chain_charge`), which is not an
/// upper bound on the charge of the same plan converted (`chain_charge`). Over a 480 × 320 stage of
/// a JPEG, for a Presence layer of Texture and Clarity after a Basic layer, which shares nothing, it
/// is below by the 13 passes' 256-byte parameter slices alone; with three more such layers before
/// the last, each through a mask, it is above by the scratch the pool holds once for all four
/// links, less every pass's slice.
#[test]
fn gpu_window_an_unconverted_plan_is_charged_every_plane_apart() {
    use luxforge_gpu::{ChainCharge, chain_charge};
    let registry = ModuleRegistry::builtin();
    let (width, height) = (480u32, 320u32);
    let window = Region {
        x0: 0,
        y0: 0,
        width,
        height,
    };
    let basic = Layer::new(BASIC_EFFECT, serde_json::json!({"exposure": 0.3}));
    let presence = |mask: Option<&luxforge_core::Mask>| Layer {
        mask: mask.map(|mask| mask.id.clone()),
        ..Layer::new(
            luxforge_core::PRESENCE_EFFECT,
            serde_json::json!({"texture": 40, "clarity": 30}),
        )
    };
    let charges = |recipe: &Recipe| -> (u64, ChainCharge, usize) {
        let request = GpuPlanRequest::exact(0, Stage { width, height });
        let plan = match gpu_plan(&registry, recipe, request).expect("the stack compiles") {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
        let steps = super::gpu_plan::plan_steps(&plan).expect("convertible steps");
        let format = BoundaryFormat::Half;
        let converted = chain_charge(&steps, (width, height), (0, 0), format);
        assert_eq!(
            super::gpu_preview::chain_charge(&plan, window, BoundaryFormat::Half),
            converted.total()
        );
        let passes = plan
            .spatial
            .iter()
            .map(|spatial| spatial.passes.len())
            .sum();
        (
            super::gpu_preview::unconverted_chain_charge(&plan, window, BoundaryFormat::Half),
            converted,
            passes,
        )
    };
    let (alone, converted, passes) = charges(&Recipe {
        layers: vec![basic.clone(), presence(None)],
        ..Recipe::default()
    });
    assert_eq!((converted.intermediates.len(), passes), (1, 13));
    assert_eq!(alone + 13 * 256, converted.total(), "below by the slices");
    let mut chained = Recipe {
        layers: vec![basic],
        ..Recipe::default()
    };
    for x in [0.25, 0.5, 0.75] {
        let mut mask = luxforge_core::Mask::new(format!("Mask {x}"));
        mask.components.push(luxforge_core::Component::new(
            "Radial 1",
            luxforge_core::ComponentMode::Add,
            "radial",
            serde_json::json!({"x": x, "y": 0.5, "radius_x": 0.15, "radius_y": 0.25,
                               "angle": 0.0, "feather": 40.0}),
        ));
        chained.layers.push(presence(Some(&mask)));
        chained.masks.push(mask);
    }
    chained.layers.push(presence(None));
    let (apart, converted, passes) = charges(&chained);
    assert_eq!((converted.intermediates.len(), passes), (4, 4 * 13));
    eprintln!(
        "gpu_window unconverted: one layer {alone} B, four {apart} B against {} B converted, \
         the pool {} B",
        converted.total(),
        converted.pool
    );
    // Every link holds the same scratch, so the pool is one link's, which the figure apart counts
    // for each of the four.
    assert_eq!(
        apart + 4 * 13 * 256,
        converted.total() + 3 * converted.pool,
        "above by the shared scratch, less the slices"
    );
    assert!(apart > converted.total());
}

// ---- The paint harness's masked Presence layers at 100% -----------------------------------------

/// The paint harness's layout (`cargo xtask editor-latency --mode paint --masks N
/// --mask-presence`), committed over `editor`'s photograph by another client: the first mask brushed
/// along the harness's seeding stroke and its measured stroke's path to the end, each further one a
/// radial placed as the harness places it, each mask holding a masked Basic exposure of +0.6 and a
/// masked Presence layer of `presence`'s fields ([`harness_presence`] in the harness). Placement
/// puts every masked Basic layer before every masked Presence layer. Answers the first mask.
fn harness_layout(
    editor: &mut Editor,
    asset: &luxforge_core::AssetId,
    agent: luxforge_core::ClientId,
    masks: usize,
    presence: &Value,
) -> Value {
    let brush = serde_json::json!({"size": 0.06, "feather": 50.0, "flow": 100.0, "erase": false,
                                   "limit_to_colour": false, "colour_refine": 50.0});
    let mut seed = brush.clone();
    seed["points"] = serde_json::json!([[0.2, 0.3], [0.8, 0.3]]);
    let seeded = answered(editor, asset, agent, "mask.add-stroke", seed);
    let first = seeded["mask"].clone();
    // The measured stroke, 120 positions of a sine across the frame, painted to its end.
    let path: Vec<[f64; 2]> = (0..120)
        .map(|index| {
            let t = f64::from(index) / 119.0;
            [
                0.2 + 0.8 * t,
                0.5 + 0.2 * (std::f64::consts::TAU * 2.5 * t).sin(),
            ]
        })
        .collect();
    let mut painted = brush;
    painted["mask"] = first.clone();
    painted["component"] = seeded["component"].clone();
    painted["points"] = serde_json::json!(path);
    answered(editor, asset, agent, "mask.add-stroke", painted);
    let adjust = |editor: &mut Editor, mask: &Value| {
        let basic = serde_json::json!({"mask": mask, "exposure": 0.6});
        answered(editor, asset, agent, "edit.set-basic", basic);
        let mut fields = presence.clone();
        fields["mask"] = mask.clone();
        answered(editor, asset, agent, "edit.set-presence", fields);
    };
    adjust(editor, &first);
    for index in 2..=masks {
        let t = (index - 2) as f64 / (masks.max(3) - 2) as f64;
        let radial = serde_json::json!({"x": 0.15 + 0.7 * t,
                                        "y": 0.35 + 0.3 * (t * 7.0).sin().abs(),
                                        "radius_x": 0.18, "radius_y": 0.14, "angle": 0.0,
                                        "feather": 50.0});
        let mask = answered(editor, asset, agent, "mask.create-radial", radial)["mask"].clone();
        adjust(editor, &mask);
    }
    first
}

/// The Presence fields each of the paint harness's masks holds with `--mask-presence`.
fn harness_presence() -> Value {
    serde_json::json!({"clarity": 50.0, "texture": 40.0})
}

/// The generated 24 MP JPEG at `photograph` opened in an editor of a catalog named for `name`, the
/// paint harness's layout of `masks` masks with `presence`'s fields committed over it
/// ([`harness_layout`]), in Mask mode with the first mask open, so its exposure slider drafts the
/// first mask's masked Basic layer, as the harness's stroke on it changes that layer first; shown
/// at Fit in the harness's view once its first frame is in, the surface stood in for.
fn harness_editor(
    photograph: &std::path::Path,
    name: &str,
    masks: usize,
    presence: &Value,
) -> (Editor, std::path::PathBuf) {
    use super::message::{mask::MaskMessage, view::ViewMessage};
    let catalog = catalog(name);
    let (mut editor, asset, agent) = crate::app::testing::real_photo_at(&catalog, photograph);
    harness_view(&mut editor, VIEWS[0], luxforge_core::Zoom::Fit);
    let first = harness_layout(&mut editor, &asset, agent, masks, presence);
    let mode = serde_json::json!({"mode": luxforge_core::MASK_MODE});
    let _ = editor.update(Message::View(ViewMessage::SetMode(
        luxforge_core::MASK_MODE.into(),
    )));
    crate::app::tasks::call(&editor.owner, editor.client, "workspace.set", mode)
        .expect("Mask mode");
    let (session, _) = crate::app::tasks::call(
        &editor.owner,
        editor.client,
        "session.state",
        serde_json::json!({}),
    )
    .expect("the session");
    let session = serde_json::from_value(session).expect("a session");
    let _ = editor.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
    let _ = editor.update(Message::Mask(MaskMessage::Select(
        first.as_str().expect("a mask").into(),
    )));
    harness_view(&mut editor, VIEWS[0], luxforge_core::Zoom::Fit);
    deliver_until(&mut editor, "the first frame", |editor| {
        editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
    });
    assert_eq!(editor.presentation.dimensions, Some((6000, 4000)));
    editor.gpu.surface = Some(SurfaceReport::default());
    (editor, catalog)
}

/// Cancel `editor`'s open draft, as the Cancel button does.
fn cancel_draft(editor: &mut Editor) {
    let _ = editor.update(Message::Draft(
        crate::app::message::draft::DraftMessage::Cancel,
    ));
}

/// One view of a 100% drag: a window at 2×, its side panels, and where it is scrolled to.
#[derive(Clone, Copy)]
struct View {
    name: &'static str,
    window: (f32, f32),
    panels: bool,
    centred: bool,
}

/// The views a 100% drag over the paint harness's layout is planned in: the harness's own, its
/// 1728 × 1080 window with both side panels open, as the harness has them, scrolled to the top
/// left as a view zoomed to 100% opens; the same window scrolled to the centre; that window with
/// both panels closed, the largest region it holds; and the corpus's largest window
/// ([`LARGEST_WINDOW`](super::gpu_qualification::LARGEST_WINDOW)), whose region on a 6000 × 4000
/// stage is 3026 × 1826 at (1487, 1087).
const VIEWS: [View; 4] = [
    View {
        name: "the harness's (panels open, top left)",
        window: (1728.0, 1080.0),
        panels: true,
        centred: false,
    },
    View {
        name: "panels open, centred",
        window: (1728.0, 1080.0),
        panels: true,
        centred: true,
    },
    View {
        name: "panels closed, centred",
        window: (1728.0, 1080.0),
        panels: false,
        centred: true,
    },
    View {
        name: "the corpus's",
        window: super::gpu_qualification::LARGEST_WINDOW,
        panels: false,
        centred: true,
    },
];

/// `editor`'s view as `view` shows it, at `zoom`.
fn harness_view(editor: &mut Editor, view: View, zoom: luxforge_core::Zoom) {
    editor.view_state.window = view.window;
    editor.view_state.scale_factor = 2.0;
    editor.session.workspace.state_panel = view.panels;
    editor.session.workspace.tools_panel = view.panels;
    editor.session.preview.view.zoom = zoom;
    let surface = crate::layout::photo_surface(view.window, view.panels, view.panels, false);
    let (width, height) = editor.presentation.dimensions.unwrap_or((0, 0));
    // Logical pixels an output pixel takes at 100% at 2×.
    let scale = 0.5;
    editor.view_state.local_pan = if view.centred {
        (
            ((width as f32 * scale - surface.0) / 2.0).max(0.0),
            ((height as f32 * scale - surface.1) / 2.0).max(0.0),
        )
    } else {
        (0.0, 0.0)
    };
}

/// `rect` grown by `by` on every side, clamped to a stage of `stage`.
fn grown(rect: Region, by: u32, stage: (u32, u32)) -> Region {
    let (x0, y0) = (rect.x0.saturating_sub(by), rect.y0.saturating_sub(by));
    let (x1, y1) = ((rect.x1() + by).min(stage.0), (rect.y1() + by).min(stage.1));
    Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

/// The smallest rectangle holding `a` and `b`.
fn union(a: Region, b: Region) -> Region {
    let (x0, y0) = (a.x0.min(b.x0), a.y0.min(b.y0));
    Region {
        x0,
        y0,
        width: a.x1().max(b.x1()) - x0,
        height: a.y1().max(b.y1()) - y0,
    }
}

/// The part of `a` inside `b`, if any.
fn intersection(a: Region, b: Region) -> Option<Region> {
    let (x0, y0) = (a.x0.max(b.x0), a.y0.max(b.y0));
    let (x1, y1) = (a.x1().min(b.x1()), a.y1().min(b.y1()));
    (x1 > x0 && y1 > y0).then(|| Region {
        x0,
        y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// A rectangle as `WxH at (x, y), M MP`.
fn shown(rect: Region) -> String {
    format!(
        "{}x{} at ({}, {}), {:.1} MP",
        rect.width,
        rect.height,
        rect.x0,
        rect.y0,
        rect.pixels() as f64 / 1e6
    )
}

/// A megabyte figure of `bytes`.
fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / 1e6
}

/// How a walk back from a region grows each masked link's needed rectangle.
#[derive(Clone, Copy, PartialEq)]
enum Walk {
    /// By the link's halo everywhere, as the planner grows it.
    Halo,
    /// Only where the link's mask's bounds reach it, since outside them its output is its input
    /// there.
    Bounded,
    /// [`Walk::Bounded`], but by the halo everywhere for the first link, whose mask the gesture
    /// paints, so its bounds change under it.
    BoundedButFirst,
}

/// What each link of a chain of spatial links needs exact, walking back from `region` as `walk`
/// says: the rectangle each link's output must hold, in chain order, and the rectangle the first
/// link's input must hold.
fn needed(
    plan: &luxforge_core::GpuPlan,
    region: Region,
    stage: (u32, u32),
    walk: Walk,
) -> (Vec<Region>, Region) {
    let mut outputs = Vec::new();
    let mut needed = region;
    for (link, spatial) in plan.spatial.iter().enumerate().rev() {
        outputs.push(needed);
        let halo = spatial.halos.iter().sum();
        let bounded = walk == Walk::Bounded || walk == Walk::BoundedButFirst && link > 0;
        match spatial.mask.as_ref().filter(|_| bounded) {
            Some(mask) => {
                if let Some(inside) = intersection(needed, mask.bounds) {
                    needed = union(needed, grown(inside, halo, stage));
                }
            }
            None => needed = grown(needed, halo, stage),
        }
    }
    outputs.reverse();
    (outputs, needed)
}

/// Why `editor-latency --mode paint --zoom 100 --masks 10` (and 16) with `--mask-presence` takes
/// the CPU path naming `budget-exceeded` on every tick, through the real planning path: the
/// generated 24 MP JPEG (6000 × 4000) opened in the editor, the harness's layout of N masks
/// committed over it, a drag of the first mask's exposure in each of [`VIEWS`], whose plan starts,
/// as every gesture's does, from the stack's first content layer. At 100% the window the boundary
/// request names is the region grown by every Presence link's summed halo (Texture's and
/// Clarity's, 8 + 199 px at this stage), once a link: N links grow it N halos on every side, to
/// most or all of the stage at 10 and 16 masks. The plan converts, so the desktop charges the
/// chain as the slot does, each link's intermediate the window's size, and its figure is the
/// slot's own charge for that plan over that window less the links' words and blocks buffers. For
/// each N it prints the window, `region_charge`'s `(boundary, slot)`, the chain's breakdown, the
/// slot's own charge and the budget; and, as estimates for a proposal, what a walk that grows a
/// masked link's needed rectangle only where its mask's bounds reach would hold, and what each
/// link's intermediate and kept planes would take sized to what that link's output must hold. At
/// Fit the plan is the proxy's and no halo grows its boundary.
///
/// ```text
/// LUXFORGE_GENERATED_FIXTURES=fixtures/generated cargo test --release -p luxforge-app \
///     --bin luxforge gpu_window_the_paint_harness_masks_at_100 -- --ignored --nocapture
/// ```
#[test]
#[ignore = "the generated 24 MP JPEG: set LUXFORGE_GENERATED_FIXTURES to the generated JPEGs"]
fn gpu_window_the_paint_harness_masks_at_100_grow_the_window_by_every_links_halo() {
    use luxforge_gpu::{GPU_PREVIEW_BUDGET, GpuBoundary, chain_charge};
    let test = "gpu_window_the_paint_harness_masks_at_100_grow_the_window_by_every_links_halo";
    let Some(qualifier) = headless(test) else {
        return;
    };
    let generated = std::env::var("LUXFORGE_GENERATED_FIXTURES").expect("the generated JPEGs");
    let photograph = std::path::Path::new(&generated).join("24mp.jpg");
    eprintln!(
        "{test}: the budget {} B ({:.1} MB)",
        GPU_PREVIEW_BUDGET,
        megabytes(GPU_PREVIEW_BUDGET)
    );
    for masks in [1usize, 3, 10, 16] {
        let name = format!("harness-{masks}");
        let (mut editor, catalog) = harness_editor(&photograph, &name, masks, &harness_presence());
        // At Fit: the proxy's plan, whose boundary is the proxy stage, which no halo grows.
        let _ = slide(&mut editor, "set-basic", "exposure", 0.7);
        {
            let (plan, request) = editor.gpu.planned().expect("a Fit plan");
            assert_eq!(plan.spatial.len(), masks, "a Presence link a mask");
            assert!(
                plan.spatial.iter().all(|spatial| spatial.mask.is_some()),
                "every Presence layer masked"
            );
            assert_eq!(
                plan.content.len(),
                masks,
                "every masked Basic layer before them"
            );
            assert_eq!(request.key.region(), None);
            eprintln!(
                "{test}: {masks} masks at Fit: the proxy's boundary stage {}x{}, window {:?}, \
                 region_charge {:?}",
                plan.boundary.stage.width,
                plan.boundary.stage.height,
                request.window,
                editor.gpu.region_charge()
            );
        }
        cancel_draft(&mut editor);
        for (number, view) in VIEWS.into_iter().enumerate() {
            harness_view(
                &mut editor,
                view,
                luxforge_core::Zoom::Percent { value: 100.0 },
            );
            let _ = slide(
                &mut editor,
                "set-basic",
                "exposure",
                0.8 + 0.1 * number as f64,
            );
            let (plan, request) = editor.gpu.planned().expect("a 100% plan");
            let rect = request.key.region().expect("a region");
            let window = request.window.expect("the region's window");
            let stage = (plan.boundary.stage.width, plan.boundary.stage.height);
            assert_eq!(stage, (6000, 4000), "the exact stage");
            assert_eq!(plan.spatial.len(), masks);
            // Every Presence link's summed halo, Texture's and Clarity's: the same for each.
            let halos: Vec<u32> = plan
                .spatial
                .iter()
                .map(|spatial| spatial.halos.iter().sum())
                .collect();
            assert!(halos.iter().all(|halo| *halo == halos[0]), "{halos:?}");
            let total: u32 = halos.iter().sum();
            assert_eq!(
                window,
                grown(rect, total, stage),
                "{masks} masks: the region grown by every link's halo"
            );
            assert_eq!(
                needed(plan, rect, stage, Walk::Halo).1,
                window,
                "the planner's walk"
            );
            let (boundary, slot) = editor.gpu.region_charge().expect("a region's charge");
            let steps = super::gpu_plan::plan_steps(plan);
            let converted = steps.is_ok();
            let format = request.format;
            let size = (window.width, window.height);
            let origin = (window.x0, window.y0);
            let chain = chain_charge(&steps.expect("convertible steps"), size, origin, format);
            assert_eq!(
                super::gpu_preview::chain_charge(plan, window, request.format),
                chain.total()
            );
            // The slot's own charge for the plan converted over a boundary of that window.
            let held = GpuBoundary::new(
                std::sync::Arc::new(vec![0u8; window.pixels() as usize * 8]),
                window.width,
                window.height,
                1,
                format,
            )
            .expect("a boundary");
            let surface = super::gpu_plan::surface_plan_over(plan, held, origin, None, Some(rect))
                .expect("a runnable plan");
            let charged = qualifier.charged_bytes(&surface).expect("a charge");
            assert!(slot <= charged, "{slot} B of {charged} B");
            let summary = editor.gpu.summary();
            let over = &summary["drag"]["over_budget"];
            assert_eq!(over.is_null(), slot <= GPU_PREVIEW_BUDGET, "{summary}");
            // Estimates for a proposal, at the window's own rates a texel: each link's
            // intermediate and kept planes sized to what its output must hold, the content
            // link's to the window, and the pool and the rest as they are.
            let texels = window.pixels() as f64;
            let rest = slot - chain.total();
            let kept = chain.kept.iter().copied().max().unwrap_or(0) as f64 / texels;
            let sized = |outputs: &[Region], input: Region| -> f64 {
                let pixels = |rect: &Region| rect.pixels() as f64;
                let scale = input.pixels() as f64 / texels;
                let intermediates: f64 = outputs[..outputs.len() - 1]
                    .iter()
                    .map(|rect| 8.0 * pixels(rect))
                    .sum::<f64>()
                    + 8.0 * pixels(&input);
                let planes: f64 = outputs.iter().map(|rect| kept * pixels(rect)).sum();
                (rest as f64
                    + boundary as f64 * (scale - 1.0)
                    + chain.pool as f64 * scale
                    + intermediates
                    + planes)
                    / 1e6
            };
            let (outputs, _) = needed(plan, rect, stage, Walk::Halo);
            let (bounded, input) = needed(plan, rect, stage, Walk::Bounded);
            let (painted, around) = needed(plan, rect, stage, Walk::BoundedButFirst);
            eprintln!(
                "{test}: {masks} masks at 100%, {}: region {}; each link's halo {} px; window \
                 {}; region_charge (boundary {:.1} MB, slot {:.1} MB); steps converted \
                 {converted}; chain {:.1} MB: {} intermediates of {:.1} MB, kept {:.1} MB a \
                 Presence link, pool {:.1} MB; the slot's own charge {:.1} MB, {:.1} KB more \
                 (the links' buffers); over the budget: {over}; boundaries derived: {}",
                view.name,
                shown(rect),
                halos[0],
                shown(window),
                megabytes(boundary),
                megabytes(slot),
                megabytes(chain.total()),
                chain.intermediates.len(),
                chain.intermediates.first().copied().map_or(0.0, megabytes),
                megabytes(chain.kept.iter().copied().max().unwrap_or(0)),
                megabytes(chain.pool),
                megabytes(charged),
                (charged - slot) as f64 / 1e3,
                editor.gpu.ticks().2,
            );
            eprintln!(
                "{test}: {masks} masks at 100%, {}: estimates: each link sized to its output \
                 {:.1} MB; a mask-bounded walk's window {} ({:.1} MB at the window's rates), \
                 with each link sized to its output {:.1} MB; bounded but for the painted mask's \
                 link {} ({:.1} MB), with each link sized {:.1} MB",
                view.name,
                sized(&outputs, window),
                shown(input),
                (slot as f64 * input.pixels() as f64 / texels) / 1e6,
                sized(&bounded, input),
                shown(around),
                (slot as f64 * around.pixels() as f64 / texels) / 1e6,
                sized(&painted, around),
            );
            cancel_draft(&mut editor);
        }
        finish(editor, catalog);
    }
}

/// How many of the paint harness's masks a 100% drag of the first mask's exposure holds within the
/// GPU-preview budget, in the harness's view and the corpus's ([`VIEWS`]): for every N from 1 to 16
/// masks of the harness's layout, the window the drag's boundary names, the slot the
/// desktop holds it to and whether that fits, and the largest N that does in each view. It holds
/// only what does not depend on the figures: the window is the region grown by every link's halo,
/// the slot never falls as N grows, and a tick derives no boundary exactly when its slot passes
/// the budget. With Dehaze beside Clarity and Texture in every masked Presence layer the same drag
/// changes the input of every Dehaze layer, so each tick computes every layer's light from the
/// whole stage over the source, a light a layer, its light links charged beside the slot.
///
/// ```text
/// LUXFORGE_GENERATED_FIXTURES=fixtures/generated cargo test --release -p luxforge-app \
///     --bin luxforge gpu_window_the_paint_harness_masks_at_100_fit -- --ignored --nocapture
/// ```
#[test]
#[ignore = "the generated 24 MP JPEG: set LUXFORGE_GENERATED_FIXTURES to the generated JPEGs"]
fn gpu_window_the_paint_harness_masks_at_100_fit_the_budget_up_to_a_count() {
    use luxforge_gpu::GPU_PREVIEW_BUDGET;
    let test = "gpu_window_the_paint_harness_masks_at_100_fit_the_budget_up_to_a_count";
    let generated = std::env::var("LUXFORGE_GENERATED_FIXTURES").expect("the generated JPEGs");
    let photograph = std::path::Path::new(&generated).join("24mp.jpg");
    let views = [VIEWS[0], VIEWS[3]];
    let at_100 = luxforge_core::Zoom::Percent { value: 100.0 };
    // Each view's rows: N, the window and the slot.
    let mut rows: [Vec<(usize, Region, u64)>; 2] = Default::default();
    for masks in 1..=16 {
        let name = format!("budget-{masks}");
        let (mut editor, catalog) = harness_editor(&photograph, &name, masks, &harness_presence());
        for (number, view) in views.into_iter().enumerate() {
            harness_view(&mut editor, view, at_100.clone());
            let _ = slide(
                &mut editor,
                "set-basic",
                "exposure",
                0.8 + 0.1 * number as f64,
            );
            let (plan, request) = editor.gpu.planned().expect("a 100% plan");
            let rect = request.key.region().expect("a region");
            let window = request.window.expect("the region's window");
            let stage = (plan.boundary.stage.width, plan.boundary.stage.height);
            let halo = plan
                .spatial
                .iter()
                .flat_map(|spatial| spatial.halos.iter())
                .sum();
            assert_eq!(
                window,
                grown(rect, halo, stage),
                "{masks} masks, {}",
                view.name
            );
            let (_, slot) = editor.gpu.region_charge().expect("a region's charge");
            let over = slot > GPU_PREVIEW_BUDGET;
            assert_eq!(
                editor.gpu.ticks().2 == 0,
                over,
                "{masks} masks, {}: a boundary is derived exactly when the slot fits",
                view.name
            );
            assert_eq!(
                editor.gpu.summary()["drag"]["over_budget"].is_null(),
                !over,
                "{masks} masks, {}",
                view.name
            );
            if let Some(&(_, _, before)) = rows[number].last() {
                assert!(
                    before <= slot,
                    "{masks} masks, {}: {before} B, then {slot} B",
                    view.name
                );
            }
            rows[number].push((masks, window, slot));
            cancel_draft(&mut editor);
        }
        finish(editor, catalog);
    }
    for (view, rows) in views.iter().zip(&rows) {
        eprintln!("{test}: {}, the budget {GPU_PREVIEW_BUDGET} B:", view.name);
        for (masks, window, slot) in rows {
            eprintln!(
                "{test}:   {masks:>2} masks: window {}, slot {:.1} MB, fits {}",
                shown(*window),
                megabytes(*slot),
                *slot <= GPU_PREVIEW_BUDGET
            );
        }
        let largest = rows
            .iter()
            .take_while(|(_, _, slot)| *slot <= GPU_PREVIEW_BUDGET)
            .last()
            .map(|(masks, _, _)| *masks);
        eprintln!(
            "{test}: {}: the largest N that fits: {largest:?}",
            view.name
        );
    }
    // With Dehaze 25 beside them: a light a layer, computed every tick, charged with the slot.
    let mut dehaze = harness_presence();
    dehaze["dehaze"] = serde_json::json!(25.0);
    for masks in [1, 2] {
        let name = format!("dehaze-{masks}");
        let (mut editor, catalog) = harness_editor(&photograph, &name, masks, &dehaze);
        for (number, view) in views.into_iter().enumerate() {
            harness_view(&mut editor, view, at_100.clone());
            let _ = slide(
                &mut editor,
                "set-basic",
                "exposure",
                0.8 + 0.1 * number as f64,
            );
            let (plan, _) = editor.gpu.planned().expect("a 100% plan");
            assert_eq!(plan.lights.len(), masks, "{masks} masks, {}", view.name);
            let (_, slot) = editor.gpu.region_charge().expect("a region's charge");
            eprintln!(
                "{test}: with Dehaze 25, {masks} masks, {}: {} lights, slot {:.1} MB, fits {}",
                view.name,
                plan.lights.len(),
                megabytes(slot),
                slot <= GPU_PREVIEW_BUDGET
            );
            cancel_draft(&mut editor);
        }
        finish(editor, catalog);
    }
}

/// Measurement, not a gate: a 60 MP RAW's planes (9504 × 6336, 12 bytes a pixel, the source the
/// surface holds and charges to the GPU-preview budget) under the recipe's cap of masked Presence
/// layers, each of all three fields through a radial mask of its own. At Fit the slot of its view
/// plan — the frame a drag draws, and the reduced stage a 100% drag past the budget falls back to —
/// beside the source within the 2 GiB budget, its light links sharing one tile texture; at 100%
/// over the largest view's region, what the
/// region's slot takes against what the budget leaves it beside the source. Allocates the planes,
/// 722 MB. `cargo test --release -p luxforge-app sixty_mp_raw -- --ignored --nocapture`.
#[test]
#[ignore = "allocates a 60 MP RAW's planes"]
fn gpu_window_a_sixty_mp_raw_at_the_masked_presence_cap_fits_the_budget_beside_its_source() {
    use luxforge_core::{
        AssetId, Component, ComponentMode, EntryId, Evaluation, GpuView, HistoryEntry,
        MAX_MASKED_SPATIAL_LAYERS, Mask, Snapshot, SnapshotId,
    };
    let (width, height) = (9504u32, 6336u32);
    let planes = vec![0.18f32; 3 * (width * height) as usize];
    let image = LinearImage::new(width, height, planes).expect("finite planes");
    let mut recipe = Recipe::default();
    for index in 0..MAX_MASKED_SPATIAL_LAYERS {
        let t = index as f64 / MAX_MASKED_SPATIAL_LAYERS as f64;
        let mut mask = Mask::new(format!("Mask {}", index + 1));
        let name = mask.next_component_name("radial");
        mask.components.push(Component::new(
            name,
            ComponentMode::Add,
            "radial",
            serde_json::json!({"x": 0.1 + 0.8 * t, "y": 0.5, "radius_x": 0.12,
                               "radius_y": 0.2, "angle": 0.0, "feather": 40.0}),
        ));
        recipe.layers.push(Layer {
            mask: Some(mask.id.clone()),
            ..Layer::new(
                luxforge_core::PRESENCE_EFFECT,
                serde_json::json!({"texture": 40, "clarity": 50, "dehaze": 15}),
            )
        });
        recipe.masks.push(mask);
    }
    let asset = AssetId::new();
    let entry = HistoryEntry {
        id: EntryId::new(),
        asset_id: asset.clone(),
        sequence: 1,
        action_id: "set-presence".into(),
        label: "Presence".into(),
        parameters: serde_json::json!({}),
        actor: "test".into(),
        timestamp_ms: 0,
        request_id: None,
        base_revision: 0,
        result_revision: 1,
        snapshot: Snapshot {
            id: SnapshotId::new(),
            asset_id: asset,
            recipe: recipe.clone(),
        },
        undo_parent: None,
        restore_target: None,
    };
    let evaluation = Evaluation::new(
        std::sync::Arc::new(ModuleRegistry::builtin()),
        RenderContext::new(),
        PreviewSource::Raw {
            image,
            settings: LinearSettings::default(),
        },
        entry,
        recipe,
        None,
    );
    let source = u64::from(width) * u64::from(height) * 12;
    let budget = luxforge_gpu::GPU_PREVIEW_BUDGET;
    let megabytes = |bytes: u64| bytes as f64 / 1e6;
    let planned = |view: GpuView| {
        let rest = luxforge_core::qualification::rest_plan(&evaluation, view).expect("a rest plan");
        match (rest.view.answer, rest.view.boundary) {
            (GpuAnswer::Plan(plan), Some(request)) => (plan, request),
            (answer, _) => panic!("no plan: {:?}", answer.fallback()),
        }
    };
    let (plan, request) = planned(GpuView::Fit(super::gpu_qualification::fit_bounds()));
    assert_eq!(plan.spatial.len(), MAX_MASKED_SPATIAL_LAYERS);
    let fit = super::gpu_preview::reduced_charge(&plan, &request);
    let lights = (
        super::gpu_preview::light_charge(&plan, request.format),
        plan.lights.len(),
    );
    let region = super::gpu_qualification::largest_view((width, height), 100.0).unwrap();
    let (plan, request) = planned(GpuView::Region {
        rect: region,
        magnification: 1.0,
    });
    let (_, at_100) = super::gpu_preview::region_charge(&plan, &request).expect("a region");
    let verdict = if fit + source <= budget {
        "within"
    } else {
        "past"
    };
    eprintln!(
        "60 MP RAW, {MAX_MASKED_SPATIAL_LAYERS} masked Presence layers: source {:.1} MB; at Fit \
         the slot {:.1} MB ({:.1} MB of it its {} light links), with the source {:.1} MB, {verdict} \
         the {:.1} MB budget; at 100% the region's slot {:.1} MB against the {:.1} MB the budget \
         leaves it",
        megabytes(source),
        megabytes(fit),
        megabytes(lights.0),
        lights.1,
        megabytes(fit + source),
        megabytes(budget),
        megabytes(at_100),
        megabytes(budget - source)
    );
    assert!(fit + source <= budget, "the Fit slot beside the source");
}
