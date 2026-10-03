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
//!   planner's windows of crops at the stage's corners reach every edge of it. A spatial step
//!   (Texture and Clarity) over the window, which the planner grows by its filters' margin, draws
//!   the same frame too.
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
                    super::gpu_plan::boundary_format(format),
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
fn deliver_until(editor: &mut Editor, what: &str, mut done: impl FnMut(&Editor) -> bool) {
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
    mut params: Value,
) -> Editor {
    let revision = editor.document.state.as_ref().unwrap().revision;
    params["asset_id"] = serde_json::json!(asset);
    params["mutation"] = serde_json::json!({
        "expected_revision": revision, "request_id": "window-crop", "actor": "agent"
    });
    crate::app::tasks::call(&editor.owner, agent, method, params).expect("the edit is accepted");
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

/// On the corpus's Z6 and Air 2S, whose lens profile the crop is fused with, a Fit drag of Basic
/// over a 16:9 crop straightened by 7°, the corpus's, and by 45° asks for a windowed proxy's
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
