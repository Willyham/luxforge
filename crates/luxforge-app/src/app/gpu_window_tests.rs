//! A boundary held over the window its output reads (`docs/design/gpu-preview.md`, "The held input
//! boundary"): the GPU frame over the window is the frame over the whole boundary stage, and at Fit
//! a crop drawn at its exact stage asks for that window and is held to the bound and the budget
//! before it is rendered.
//!
//! - **The same frame.** For an affine tail (a straightened crop), a projective one (a perspective
//!   warp with the crop) and a lens warp's coordinate grid, on both boundary formats, the surface's
//!   own assembled shader draws the same codes and the same `f32` output over the planner's window
//!   of the boundary stage as over the whole stage, and over that window grown to each edge of the
//!   stage: the tail clamps each tap to the rectangle the CPU's resample reads, then offsets it by
//!   the window's integer origin, so every tap reads the texel it reads in the whole stage. The
//!   planner's windows of crops at the stage's corners reach every edge of it.
//! - **At Fit.** A tight crop of a photograph that fits the display is drawn at its exact stage:
//!   the tick asks for the window its output reads, the boundary held is that window, and a slot
//!   over the budget asks for no boundary and names it.
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
use luxforge_ui::photo_surface::gpu_preview::qualification::{boundary_as, held};

const WIDTH: u32 = 360;
const HEIGHT: u32 = 240;

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
fn source(format: BoundaryFormat) -> PreviewSource {
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
fn whole(source: &PreviewSource) -> Vec<[f32; 3]> {
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
fn cut(pixels: &[[f32; 3]], window: Region) -> Vec<[f32; 3]> {
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
                    .expect("a grid");
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
                        super::gpu_plan::boundary_format(format),
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

// ---- At Fit, at the exact stage -----------------------------------------------------------------

fn catalog(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "luxforge-gpu-window-{name}-{}.sqlite",
        std::process::id()
    ))
}

/// Take up the preview worker's results, as the worker's wake does, until `done`.
fn deliver_until(editor: &mut Editor, what: &str, mut done: impl FnMut(&Editor) -> bool) {
    luxforge_testbase::wait_until(what, || {
        let _ = editor.update(Message::Preview(PreviewMessage::Poll));
        done(editor)
    });
}

/// The fixture photograph with a tight straightened crop committed by another client, as the
/// desktop then shows it: its output fits the display, so Fit draws its exact stage.
fn cropped_photo(catalog: &std::path::Path) -> Editor {
    let (mut editor, asset, agent) = real_photo(catalog);
    let revision = editor.document.state.as_ref().unwrap().revision;
    crate::app::tasks::call(
        &editor.owner,
        agent,
        "edit.crop",
        serde_json::json!({
            "asset_id": asset, "angle": 7.0, "x": 0.3, "y": 0.3, "width": 0.35, "height": 0.35,
            "mutation": {"expected_revision": revision, "request_id": "window-crop",
                "actor": "agent"}
        }),
    )
    .expect("the crop is accepted");
    let refreshed = crate::app::tasks::refresh(
        &editor.owner,
        editor.client,
        asset,
        crate::app::tasks::Scope::Open,
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        refreshed,
    )))));
    editor
}

/// A Fit drag over a tight crop drawn at its exact stage asks for the window its output reads,
/// and holds that window, not the whole photograph; the charge it is held to before rendering is
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
    assert_eq!(ticks[0]["reason"], "boundary-pending", "{ticks:?}");
    let (boundary, slot) = editor
        .gpu
        .region_charge()
        .expect("held to the bound and budget");
    deliver_until(&mut editor, "the boundary", |editor| {
        editor.gpu.holds_boundary()
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
    // A JPEG's boundary is eight bytes a texel; the slot adds the tail's intermediate and the
    // frame.
    assert_eq!(boundary, held_width * held_height * 8);
    assert_eq!(
        slot,
        boundary * 2 + u64::from(width) * u64::from(height) * 4
    );
    let _ = editor.update(Message::Draft(
        crate::app::message::draft::DraftMessage::Cancel,
    ));
    finish(editor, catalog);
}

/// At Fit at the exact stage a slot the GPU-preview budget would not hold asks for no boundary:
/// every tick takes the CPU path naming the budget, with the bytes and the budget they pass.
#[test]
fn gpu_window_an_exact_fit_slot_over_the_budget_keeps_the_cpu_path() {
    let catalog = catalog("budget");
    let mut editor = cropped_photo(&catalog);
    editor.gpu.budget = Some(1);
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
    assert_eq!(editor.gpu.ticks().2, 0, "no boundary is asked for");
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
