//! The GPU stage's own tests: the plain-data plan and its assembly without a device, then the
//! stage on a headless device the test creates, read back through the photograph's real draw.
//!
//! A headless test with no adapter prints that it was skipped and asserts nothing. It is not GPU
//! evidence: the skip is the report, and `cargo test` counting it as passed does not make it one.
//! Each test builds its own pipeline with figures of its own, so none reads another's counts.
use super::super::{Frame, Layer, PhotoPipeline, PhotoPrimitive, SurfaceDiagnostics, SurfaceId};
use super::*;
use iced::widget::shader::{Pipeline as _, Primitive as _, Viewport};
use iced::{Rectangle, Size, Vector};
use luxforge_reference::srgb;
use luxforge_testbase::{wait_for, wait_until};
use std::time::Duration;

/// The identity program: a pointwise colour program that returns its input.
pub(super) fn identity() -> GpuProgram {
    GpuProgram::new(
        "identity",
        "fn identity(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
         return rgb;\n}\n",
    )
}

/// Scales by its one uniform word.
pub(super) fn scale(factor: f32) -> GpuProgram {
    GpuProgram {
        words: vec![factor.to_bits()],
        ..GpuProgram::new(
            "scale",
            "fn scale(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
             return rgb * lf_f32(words);\n}\n",
        )
    }
}

/// Swaps red and blue when its one word is 1.
fn swap(on: bool) -> GpuProgram {
    GpuProgram {
        words: vec![u32::from(on)],
        ..GpuProgram::new(
            "swap",
            "fn swap(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
             if lf_word(words) == 1u {\n        return rgb.bgr;\n    }\n    return rgb;\n}\n",
        )
    }
}

/// Paints the grey of its block's first word on every stage pixel whose `x + y` is a multiple of
/// its block's second word.
fn stripe(grey: f32, period: u32) -> GpuProgram {
    GpuProgram {
        block: Arc::from([grey.to_bits(), period]),
        ..GpuProgram::new(
            "stripe",
            "fn stripe(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
             let at = u32(pos.x) + u32(pos.y);\n    \
             if at % lf_block_word(block + 1u) == 0u {\n        \
             return vec3<f32>(lf_block_f32(block));\n    }\n    return rgb;\n}\n",
        )
    }
}

pub(super) fn plan(boundary: &GpuBoundary, programs: Vec<GpuProgram>) -> GpuPlan {
    GpuPlan {
        boundary: boundary.clone(),
        texels: TexelMap::IDENTITY,
        steps: programs.into_iter().map(GpuStep::colour).collect(),
        region: None,
    }
}

// ---- Without a device -------------------------------------------------------------------------

/// What a caller's tests check of every program it hands over: the program alone, against the
/// prelude and the convention. The stage checks each program the same way before it compiles.
#[test]
fn a_step_is_checked_against_the_convention_on_its_own() {
    // The core names its entries lf_<module>_<unit>, as the Basic exposure program does, with its
    // helpers and constants after the entry.
    let core_named = GpuProgram::new(
        "lf_basic_exposure",
        "const lf_basic_exposure_floor: f32 = 0.0;\n\
         fn lf_basic_exposure_gain(words: u32) -> f32 { return lf_f32(words); }\n\
         fn lf_basic_exposure(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
         -> vec3<f32> {\n    return rgb * lf_basic_exposure_gain(words) + \
         lf_basic_exposure_floor;\n}\n",
    );
    for program in [
        identity(),
        scale(0.5),
        swap(true),
        stripe(0.5, 4),
        core_named,
    ] {
        validate_step(&GpuStep::colour(program.clone()))
            .unwrap_or_else(|error| panic!("{}: {error}", program.entry));
    }
    let colour = |entry: &'static str, source: &'static str| {
        validate_step(&GpuStep::colour(GpuProgram::new(entry, source))).unwrap_err()
    };
    let signature = "fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32)";
    for (what, error, expected) in [
        (
            "a binding of its own",
            colour(
                "bad",
                "@group(0) @binding(3) var<storage, read> bad_more: array<u32>;\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb * f32(bad_more[0]);\n}\n",
            ),
            "binding, global variable",
        ),
        (
            "a private global",
            colour(
                "bad",
                "var<private> bad_state: f32;\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb;\n}\n",
            ),
            "binding, global variable",
        ),
        (
            "an entry point",
            colour(
                "bad",
                "fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb;\n}\n\
                 @compute @workgroup_size(1) fn bad_main() {}\n",
            ),
            "entry point",
        ),
        (
            "a coverage signature for a colour step",
            colour(
                "bad",
                "fn bad(pos: vec2<f32>, rgb: vec3<f32>, words: u32, block: u32) -> f32 {\n\
                 return 1.0;\n}\n",
            ),
            signature,
        ),
        (
            "no entry function",
            colour("bad", "fn bad_helper(x: f32) -> f32 { return x; }\n"),
            "names no function",
        ),
        (
            "a parse error",
            colour(
                "bad",
                "fn bad(rgb: vec3<f32>) -> vec3<f32> { return rgb }\n",
            ),
            "expected",
        ),
        (
            "a helper not named after its entry",
            colour(
                "bad",
                "fn helper(x: f32) -> f32 { return x; }\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb * helper(1.0);\n}\n",
            ),
            "does not start with its entry's name",
        ),
        (
            "a constant not named after its entry",
            colour(
                "bad",
                "const half_gain: f32 = 0.5;\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb * half_gain;\n}\n",
            ),
            "does not start with its entry's name",
        ),
        (
            "a named type",
            colour(
                "bad",
                "struct bad_pair { a: f32, b: f32 }\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 let p = bad_pair(1.0, 2.0);\n    return rgb * p.a;\n}\n",
            ),
            "declares the type",
        ),
        (
            "an override",
            colour(
                "bad",
                "override bad_gain: f32 = 1.0;\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb * bad_gain;\n}\n",
            ),
            "override",
        ),
        (
            "one of the surface's names",
            colour("lf_fragment", ""),
            "surface's own",
        ),
    ] {
        assert!(error.contains(expected), "{what}: {error}");
    }
}

#[test]
fn assembly_includes_a_shared_program_once_and_refuses_what_cannot_be_chained() {
    // Two layers of one unit: one source, two calls, each with its own base indices and its own
    // position map.
    let steps: Vec<GpuStep> = [scale(0.5), swap(true), scale(2.0)]
        .into_iter()
        .map(GpuStep::colour)
        .collect();
    let source = assemble(&steps).expect("an assembled shader");
    assert_eq!(source.matches("fn scale(").count(), 1);
    let call = |entry: &str, base: usize| {
        let word = |k: usize| format!("lf_f32({}u)", base + 2 + k);
        format!(
            "rgb = {entry}(rgb, vec2<f32>({} * stage.x + {} * stage.y + {}, \
             {} * stage.x + {} * stage.y + {}), lf_words[{base}u], lf_words[{}u]);",
            word(0),
            word(1),
            word(2),
            word(3),
            word(4),
            word(5),
            base + 1
        )
    };
    for (entry, base) in [("scale", 6), ("swap", 14), ("scale", 22)] {
        assert!(
            source.contains(&call(entry, base)),
            "{entry} at {base}:\n{source}"
        );
    }
    validate(&source).expect("the chain validates");
    validate(&assemble(&[]).expect("no steps")).expect("an empty chain is the identity");

    let refused = |program: GpuProgram| assemble(&[GpuStep::colour(program)]);
    for (entry, why) in [
        ("lf_boundary", "surface's own"),
        ("lf_word", "surface's own"),
        ("", "needs a name"),
        ("9lives", "identifier"),
        ("__hidden", "identifier"),
        ("two words", "identifier"),
    ] {
        let error = refused(GpuProgram::new(entry.to_owned(), "")).unwrap_err();
        assert!(error.contains(why), "{entry:?}: {error}");
    }
    let mut other = scale(0.5);
    other.source = Cow::Borrowed("fn scale(rgb: vec3<f32>) -> vec3<f32> { return rgb; }\n");
    let error = assemble(&[GpuStep::colour(scale(0.5)), GpuStep::colour(other)]).unwrap_err();
    assert!(error.contains("different sources"), "{error}");
}

#[test]
fn the_words_are_the_map_then_each_steps_bases_and_position_then_their_words() {
    let boundary = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        1,
        1,
        1,
        [[0.0; 4]],
    )
    .unwrap();
    let mut chain = plan(&boundary, vec![scale(0.5), stripe(0.25, 3), swap(true)]);
    chain.texels = TexelMap {
        origin: [2.0, 5.0],
        step: [1.0, 0.5],
    };
    // The stripe's pos is the stage turned a quarter, 40 rows down.
    let turned = PositionMap {
        a: 0,
        b: -1,
        tx: 40,
        c: 1,
        d: 0,
        ty: 0,
    };
    chain.steps[1] = GpuStep::Colour {
        program: stripe(0.25, 3),
        position: turned,
    };
    let (mut words, mut blocks) = (Vec::new(), Vec::new());
    pack(&chain, &mut words, &mut blocks);
    let header = (MAP_WORDS + STEP_WORDS * 3) as u32;
    let unmoved = [1f32, 0.0, 0.0, 0.0, 1.0, 0.0].map(f32::to_bits);
    let mut expected = vec![
        2f32.to_bits(),
        5f32.to_bits(),
        1f32.to_bits(),
        0.5f32.to_bits(),
        // A whole frame's output starts at the stage's first pixel.
        0,
        0,
    ];
    // scale: its word after the header, no block.
    expected.extend([header, 0]);
    expected.extend(unmoved);
    // stripe: no words, the first two block words, its own map.
    expected.extend([header + 1, 0]);
    expected.extend([0f32, -1.0, 40.0, 1.0, 0.0, 0.0].map(f32::to_bits));
    // swap: one word, after the stripe's block.
    expected.extend([header + 1, 2]);
    expected.extend(unmoved);
    expected.extend([0.5f32.to_bits(), 1]);
    assert_eq!(words, expected);
    assert_eq!(blocks, [0.25f32.to_bits(), 3]);
    // Every block empty still binds one word.
    pack(&plan(&boundary, vec![identity()]), &mut words, &mut blocks);
    assert_eq!(blocks, [0]);
    // A region's output starts at its rectangle: in the boundary's texels with no tail, in the
    // stage with one.
    let mut region = plan(&boundary, vec![identity()]);
    region.texels.origin = [3.0, 4.0];
    region.region = Some(GpuRegion {
        rect: [10, 20, 30, 40],
        stage: (64, 64),
    });
    pack(&region, &mut words, &mut blocks);
    assert_eq!(words[4..MAP_WORDS], [7, 16]);
    region.steps.insert(
        0,
        GpuStep::Geometry(GpuTail::affine(
            (20, 20),
            [0, 0, 1, 1],
            false,
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        )),
    );
    pack(&region, &mut words, &mut blocks);
    assert_eq!(words[4..MAP_WORDS], [10, 20]);
}

#[test]
fn a_boundary_holds_exactly_its_half_float_texels() {
    let boundary = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        2,
        1,
        9,
        [[0.5, -2.0, 1.0e5, 1.0], [0.0, 1.0, 0.1, 1.0]],
    )
    .expect("two texels");
    assert_eq!((boundary.size(), boundary.version()), ((2, 1), 9));
    let halves: Vec<f32> = (**boundary.texels.as_ref().expect("its texels"))
        .as_ref()
        .chunks_exact(2)
        .map(|bytes| half::f16::from_bits(u16::from_le_bytes([bytes[0], bytes[1]])).to_f32())
        .collect();
    assert_eq!(
        halves,
        [
            0.5,
            -2.0,
            f32::INFINITY,
            1.0,
            0.0,
            1.0,
            half::f16::from_f32(0.1).to_f32(),
            1.0
        ]
    );
    assert!(
        GpuBoundary::from_linear(
            crate::photo_surface::BoundaryFormat::Half,
            2,
            1,
            1,
            [[0.0; 4]]
        )
        .is_none(),
        "too few"
    );
    assert!(
        GpuBoundary::from_linear(
            crate::photo_surface::BoundaryFormat::Half,
            1,
            1,
            1,
            [[0.0; 4]; 2]
        )
        .is_none(),
        "too many"
    );
    let half = crate::photo_surface::BoundaryFormat::Half;
    let float = crate::photo_surface::BoundaryFormat::Float;
    assert!(GpuBoundary::new(Arc::new(vec![0u8; 15]), 2, 1, 1, half).is_none());
    assert!(GpuBoundary::new(Arc::new(vec![0u8; 16]), 2, 1, 1, half).is_some());
    assert!(GpuBoundary::new(Arc::new(vec![0u8; 16]), 2, 1, 1, float).is_none());
    assert!(GpuBoundary::new(Arc::new(vec![0u8; 32]), 2, 1, 1, float).is_some());
    assert!(GpuBoundary::new(Arc::new(Vec::<u8>::new()), 0, 0, 1, half).is_none());
}

/// An `rgba32float` boundary holds every value as the `f32` it is, a near-black one's sign and a
/// value past the half range included, sixteen bytes a texel.
#[test]
fn a_float_boundary_holds_its_values_exactly() {
    let values = [[-3.0e-8, 1.0e5, 0.1, 1.0], [2.5e-9, -0.0, 7.0, 1.0]];
    let boundary =
        GpuBoundary::from_linear(crate::photo_surface::BoundaryFormat::Float, 2, 1, 4, values)
            .expect("two texels");
    assert_eq!(boundary.bytes(), 32);
    let held: Vec<u32> = (**boundary.texels.as_ref().expect("its texels"))
        .as_ref()
        .chunks_exact(4)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect();
    let expected: Vec<u32> = values
        .iter()
        .flatten()
        .map(|value| value.to_bits())
        .collect();
    assert_eq!(held, expected);
}

#[test]
fn the_budget_refuses_an_allocation_past_it_and_names_itself() {
    let figures = Figures::default();
    figures.budget.store(100, Ordering::Release);
    figures.charge(60).expect("within the budget");
    assert_eq!(
        figures.charge(41),
        Err(GpuFallback::BudgetExceeded {
            requested: 41,
            in_use: 60,
            budget: 100
        })
    );
    assert_eq!(figures.in_use(), 60, "a refusal charges nothing");
    figures.charge(40).expect("exactly the budget");
    figures.discharge(100);
    assert_eq!((figures.in_use(), figures.peak()), (0, 100));
    assert_eq!(
        GpuFallback::BudgetExceeded {
            requested: 0,
            in_use: 0,
            budget: 0
        }
        .as_str(),
        "budget-exceeded"
    );
}

#[test]
fn a_device_without_fragment_storage_or_an_srgb_target_has_no_stage() {
    let srgb = wgpu::TextureFormat::Bgra8UnormSrgb;
    assert!(supported(&wgpu::Limits::default(), srgb));
    assert!(!supported(&wgpu::Limits::downlevel_webgl2_defaults(), srgb));
    assert!(!supported(
        &wgpu::Limits::default(),
        wgpu::TextureFormat::Bgra8Unorm
    ));
}

// ---- On a headless device ---------------------------------------------------------------------

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    wait_for("the GPU request", || {
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => Some(value),
            std::task::Poll::Pending => None,
        }
    })
}

/// A device of this host's default adapter, or `None` after printing the skip.
pub(super) fn headless(test: &str) -> Option<(wgpu::Device, wgpu::Queue)> {
    headless_with(test, wgpu::Limits::default())
}

fn headless_with(test: &str, limits: wgpu::Limits) -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let Some(adapter) =
        block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()
    else {
        eprintln!("skipped: no GPU adapter; {test} ran nothing and is not GPU evidence");
        return None;
    };
    eprintln!("{test}: adapter {:?}", adapter.get_info());
    let descriptor = wgpu::DeviceDescriptor {
        required_limits: limits,
        ..wgpu::DeviceDescriptor::default()
    };
    let device = block_on(adapter.request_device(&descriptor));
    if device.is_err() {
        eprintln!("skipped: no device for the adapter; {test} ran nothing and is not GPU evidence");
    }
    device.ok()
}

/// A pipeline counting into figures of its own, drawing to an sRGB target as the desktop's does.
pub(super) fn own_pipeline(device: &wgpu::Device, queue: &wgpu::Queue) -> PhotoPipeline {
    PhotoPipeline::with_figures(
        device,
        queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        Arc::default(),
    )
}

pub(super) fn diagnostics(pipeline: &PhotoPipeline, surface: SurfaceId) -> SurfaceDiagnostics {
    pipeline.figures.diagnostics_for(surface)
}

pub(super) const ID: SurfaceId = SurfaceId::new(0);
pub(super) const SIDE: u32 = 64;

/// The CPU frame every surface here is built with, solid, so a CPU-path draw is told apart from
/// any GPU output at every pixel.
const CPU_RGBA: [u8; 4] = [10, 200, 30, 255];

fn cpu_frame() -> Frame {
    let pixels: Vec<u8> = std::iter::repeat_n(CPU_RGBA, (SIDE * SIDE) as usize)
        .flatten()
        .collect();
    Frame::new(Arc::new(pixels), SIDE, SIDE, 1).expect("a whole frame")
}

/// Surface `surface`'s photograph, drawn over the whole 64 × 64 target, with `plan` when given.
pub(super) fn primitive(surface: SurfaceId, plan: Option<GpuPlan>) -> PhotoPrimitive {
    PhotoPrimitive {
        surface,
        layers: vec![(Layer::Photo, cpu_frame())],
        viewport: None,
        region_overlays: [None, None],
        gpu: plan,
        gpu_options: Default::default(),
        dissolve: None,
        offset: Vector::new(0.0, 0.0),
        size: Size::new(SIDE as f32, SIDE as f32),
        clip_size: Size::new(SIDE as f32, SIDE as f32),
        bright: None,
        angle: 0.0,
        snap: true,
    }
}

fn wait(device: &wgpu::Device, index: wgpu::SubmissionIndex) {
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(index),
            timeout: None,
        })
        .expect("device wait");
}

/// One frame as Iced renders it — `prepare`, then `draw` into a 64 × 64 sRGB BGRA target — read
/// back. The readback, and the wait for it, are the test's: the surface never reads a pixel.
pub(super) fn paint(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &mut PhotoPipeline,
    primitive: &PhotoPrimitive,
) -> Vec<u8> {
    paint_into(device, queue, pipeline, primitive, (SIDE, SIDE))
}

/// [`paint`] into a target of `(width, height)`, `width` a multiple of 64 so its rows copy out
/// unpadded.
fn paint_into(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &mut PhotoPipeline,
    primitive: &PhotoPrimitive,
    (width, height): (u32, u32),
) -> Vec<u8> {
    // These tests are about drawing, so the plan's sequence is compiled first, as a frame after
    // its compile finds it; `compile_tests` is about compiling.
    if let Some(plan) = &primitive.gpu {
        pipeline.compile_now(device, plan);
    }
    draw_into(device, queue, pipeline, primitive, (width, height))
}

/// [`paint`] as the frame finds the stage, whatever its pipelines' state.
pub(super) fn paint_prepared(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &mut PhotoPipeline,
    primitive: &PhotoPrimitive,
) -> Vec<u8> {
    draw_into(device, queue, pipeline, primitive, (SIDE, SIDE))
}

fn draw_into(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &mut PhotoPipeline,
    primitive: &PhotoPrimitive,
    (width, height): (u32, u32),
) -> Vec<u8> {
    let bounds = Rectangle::new(iced::Point::ORIGIN, Size::new(width as f32, height as f32));
    let viewport = Viewport::with_physical_size(Size::new(width, height), 1.0);
    primitive.prepare(pipeline, device, queue, &bounds, &viewport);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gpu preview readback target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gpu preview readback"),
        size: u64::from(width * height * 4),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("gpu preview readback pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLUE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        assert!(primitive.draw(pipeline, &mut pass));
    }
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let index = queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("readback receiver")
        });
    wait(device, index);
    receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("readback callback")
        .expect("readback mapping");
    let bytes = readback.slice(..).get_mapped_range().to_vec();
    readback.unmap();
    bytes
}

/// The CPU frame, as the BGRA target holds it.
pub(super) fn assert_cpu_frame(bytes: &[u8]) {
    let [r, g, b, a] = CPU_RGBA;
    for (index, pixel) in bytes.chunks_exact(4).enumerate() {
        assert_eq!(pixel, [b, g, r, a], "CPU-path pixel {index}");
    }
}

/// Wait for `pipeline`'s retirements, which its worker finishes when the GPU is done with them.
pub(super) fn settle(pipeline: &PhotoPipeline) {
    wait_until("the pipeline's retirements", || {
        pipeline.figures.retirement_pending.load(Ordering::Acquire) == 0
    });
}

/// One 8-bit code's linear value, as the boundary holds it: the nearest half float.
pub(super) fn held(code: u8) -> f32 {
    half::f16::from_f32(srgb::decode(code) as f32).to_f32()
}

/// A 64 × 64 boundary of three different permutations of every code's linear value, its last row
/// out of range — negative, past white, the largest half float, a subnormal — and the codes the
/// independent reference encodes each texel to.
pub(super) fn boundary_with_codes(version: u64) -> (GpuBoundary, Vec<[u8; 3]>) {
    let mut values = Vec::with_capacity((SIDE * SIDE) as usize);
    for index in 0..SIDE * SIDE {
        let (x, y) = (index % SIDE, index / SIDE);
        values.push(if y == SIDE - 1 {
            match x % 4 {
                0 => [-1.0, 1.5, 65504.0],
                1 => [-1.0e-4, 1.0001, 4.0],
                2 => [1.0e-6, -65504.0, 0.0],
                _ => [-0.0, 2.0, 1.0],
            }
        } else {
            let code = |a: u32, b: u32| ((index * a + b) % 256) as u8;
            [held(code(1, 0)), held(code(7, 3)), held(code(13, 5))]
        });
    }
    let boundary = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        SIDE,
        SIDE,
        version,
        values.iter().map(|[r, g, b]| [*r, *g, *b, 1.0]),
    )
    .expect("a whole boundary");
    // The texel the GPU reads is the half float, so the reference encodes that value.
    let codes = values
        .iter()
        .map(|rgb| rgb.map(|value| srgb::code(f64::from(half::f16::from_f32(value).to_f32()))))
        .collect();
    (boundary, codes)
}

/// Every texel's expected codes, as the BGRA target holds them.
pub(super) fn assert_codes(bytes: &[u8], codes: &[[u8; 3]]) {
    for (index, (pixel, [r, g, b])) in bytes.chunks_exact(4).zip(codes).enumerate() {
        assert_eq!(
            pixel,
            [*b, *g, *r, 255],
            "texel ({}, {})",
            index as u32 % SIDE,
            index as u32 / SIDE
        );
    }
}

/// What a 64 × 64 slot with the smallest buffers is charged: the boundary at eight bytes a texel,
/// the output at four, its placement uniform and the two buffers.
const SLOT_BYTES: u64 = 64 * 64 * 8 + 64 * 64 * 4 + UNIFORM_SIZE as u64 + 2 * MIN_BUFFER;

/// The acceptance check: the identity program over an uploaded `rgba16float` boundary reads back,
/// through the photograph's own draw, equal in 8-bit codes to the boundary's own encoding by the
/// independent sRGB reference, and the surface says the GPU drew it.
#[test]
fn an_identity_program_reads_back_the_boundarys_own_encoding() {
    let test = "an_identity_program_reads_back_the_boundarys_own_encoding";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(7);
    let drawn = paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(plan(&boundary, vec![identity()]))),
    );
    assert_codes(&drawn, &codes);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert_eq!(seen.gpu_fallback, None);
    assert_eq!(seen.drawn_gpu_boundary, Some(7));
    assert_eq!(seen.drawn_full_version, None, "the CPU frame was not drawn");
    assert_eq!(seen.gpu_preview_in_use_bytes, SLOT_BYTES);
    assert_eq!(seen.gpu_preview_peak_bytes, SLOT_BYTES);
    assert_eq!(seen.gpu_preview_budget_bytes, GPU_PREVIEW_BUDGET);
    eprintln!("{test}: {} texels equal", codes.len());
}

/// Drawn magnified, as a Fit frame smaller than the canvas is, the GPU stage's output is sampled
/// exactly as the CPU frame of its codes: it is held in the same size bucket with the same edge
/// texels past the frame, so the filter weighs the same texels the same way. Otherwise the jump at
/// settlement would carry a sampling difference no program made. The geometry is the
/// `gpu-identity` scenario's: a 320 × 480 frame drawn 1006 × 1508 pixels large, where an output
/// held at exactly its own size drew a few hundred pixels a code or two off the CPU frame.
#[test]
fn a_magnified_gpu_frame_draws_exactly_as_the_cpu_frame_of_its_codes() {
    let test = "a_magnified_gpu_frame_draws_exactly_as_the_cpu_frame_of_its_codes";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (width, height) = (320u32, 480u32);
    let target = (1024, 1536);
    let drawn = Size::new(1006.0, 1508.0);
    let code = |index: u32, a: u32, b: u32| ((index * a + b) % 256) as u8;
    let values: Vec<[f32; 3]> = (0..width * height)
        .map(|index| {
            [
                held(code(index, 7, 0)),
                held(code(index, 13, 5)),
                held(code(index, 29, 11)),
            ]
        })
        .collect();
    let boundary = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        width,
        height,
        1,
        values.iter().map(|[r, g, b]| [*r, *g, *b, 1.0]),
    )
    .expect("a boundary");
    let codes: Vec<u8> = values
        .iter()
        .flat_map(|rgb| {
            let [r, g, b] = rgb.map(|value| srgb::code(f64::from(value)));
            [r, g, b, 255]
        })
        .collect();
    let frame = Frame::new(Arc::new(codes), width, height, 1).expect("the codes as a frame");
    let placed = |surface, plan| {
        let mut primitive = primitive(surface, plan);
        primitive.layers = vec![(Layer::Photo, frame.clone())];
        primitive.offset = Vector::new(9.0, 14.0);
        primitive.size = drawn;
        primitive.clip_size = Size::new(target.0 as f32, target.1 as f32);
        primitive
    };
    let cpu = placed(SurfaceId::new(1), None);
    let gpu = placed(SurfaceId::new(2), Some(plan(&boundary, vec![identity()])));
    let cpu_drawn = paint_into(&device, &queue, &mut pipeline, &cpu, target);
    let gpu_drawn = paint_into(&device, &queue, &mut pipeline, &gpu, target);
    assert_eq!(
        diagnostics(&pipeline, SurfaceId::new(2)).drawn_path,
        Some(DrawingPath::Gpu)
    );
    let differing = cpu_drawn
        .chunks_exact(4)
        .zip(gpu_drawn.chunks_exact(4))
        .filter(|(cpu, gpu)| cpu != gpu)
        .count();
    assert_eq!(
        differing, 0,
        "pixels the GPU frame draws unlike the CPU frame"
    );
}

/// The calling convention end to end: two layers of one unit sharing its function with their own
/// words, a program reading its words, one reading its block and the stage position through the
/// texel map and its own position map, chained in order.
#[test]
fn a_chain_passes_each_program_its_words_block_and_stage_position() {
    let test = "a_chain_passes_each_program_its_words_block_and_stage_position";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(3);
    let mut chain = plan(
        &boundary,
        vec![
            scale(0.5),
            swap(true),
            scale(2.0),
            stripe(srgb::decode(200) as f32, 4),
        ],
    );
    chain.texels = TexelMap {
        origin: [2.0, 5.0],
        step: [1.0, 1.0],
    };
    // The stripes' own position map mirrors the stage: pos = (100 - x, y).
    chain.steps[3] = GpuStep::Colour {
        program: stripe(srgb::decode(200) as f32, 4),
        position: PositionMap {
            a: -1,
            b: 0,
            tx: 100,
            c: 0,
            d: 1,
            ty: 0,
        },
    };
    let drawn = paint(&device, &queue, &mut pipeline, &primitive(ID, Some(chain)));
    // Halving and doubling are exact, so the chain is the swap and the stripes.
    let expected: Vec<[u8; 3]> = codes
        .iter()
        .enumerate()
        .map(|(index, [r, g, b])| {
            let (x, y) = (index as u32 % SIDE, index as u32 / SIDE);
            if (100 - (x + 2) + y + 5) % 4 == 0 {
                [200; 3]
            } else {
                [*b, *g, *r]
            }
        })
        .collect();
    assert_codes(&drawn, &expected);
    assert_eq!(
        diagnostics(&pipeline, ID).drawn_path,
        Some(DrawingPath::Gpu)
    );
}

/// What `texture` takes, measured from the texture itself.
fn texture_bytes(texture: &wgpu::Texture) -> u64 {
    let size = texture.size();
    u64::from(size.width)
        * u64::from(size.height)
        * u64::from(
            texture
                .format()
                .block_copy_size(None)
                .expect("a plain format"),
        )
}

/// What `slot` holds, measured from its resources themselves: its boundary, output, placement
/// uniform, words and blocks; each earlier link's intermediate, words, blocks, kept planes and
/// parameters; the last link's kept planes and parameters; and the pool's textures, once.
fn held_bytes(slot: &GpuSlot) -> u64 {
    let planes = |spatial: Option<&SpatialSlot>| {
        spatial.map_or(0, |spatial| {
            let (kept, parameters) = spatial.planes.resources();
            kept.into_iter().map(texture_bytes).sum::<u64>() + parameters.size()
        })
    };
    texture_bytes(&slot.boundary)
        + texture_bytes(&slot.output.tiles[0].texture)
        + slot.output.tiles[0].uniform.size()
        + slot.words.buffer.size()
        + slot.blocks.buffer.size()
        + slot
            .chain
            .iter()
            .map(|link| {
                texture_bytes(&link.texture)
                    + link.words.buffer.size()
                    + link.blocks.buffer.size()
                    + planes(link.spatial.as_deref())
            })
            .sum::<u64>()
        + planes(slot.spatial.as_deref())
        + slot
            .pool
            .textures()
            .into_iter()
            .map(|(_, texture)| texture_bytes(texture))
            .sum::<u64>()
}

/// Every allocation is charged, and the in-use figure returns to zero once the slot is released —
/// when the plan stops, and when the surface closes. A chain of spatial links is charged each
/// link's intermediate, buffers and kept planes, and the pool of scratch textures its links share,
/// once, which the evidence names apart and which returns to zero with the slot.
#[test]
fn every_allocation_is_charged_and_a_released_slot_returns_the_budget_to_zero() {
    let test = "every_allocation_is_charged_and_a_released_slot_returns_the_budget_to_zero";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, _) = boundary_with_codes(1);
    let identity_plan = plan(&boundary, vec![identity()]);
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(identity_plan.clone())),
    );
    // What the slot holds, measured from the resources themselves.
    let held = held_bytes(pipeline.surfaces[&ID].gpu.as_ref().expect("a slot"));
    assert_eq!(held, SLOT_BYTES);
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_in_use_bytes, held);
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_scratch_bytes, 0);

    // The plan stops: the next frame is the CPU's and the slot retires.
    assert_cpu_frame(&paint(&device, &queue, &mut pipeline, &primitive(ID, None)));
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(
        (seen.drawn_path, seen.gpu_fallback),
        (Some(DrawingPath::Cpu), None)
    );
    settle(&pipeline);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.gpu_preview_in_use_bytes, 0);
    assert_eq!(seen.gpu_preview_peak_bytes, SLOT_BYTES);

    // The surface closes while its slot is held: the frame's end releases it.
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(identity_plan.clone())),
    );
    assert_eq!(
        diagnostics(&pipeline, ID).gpu_preview_in_use_bytes,
        SLOT_BYTES
    );
    pipeline.trim();
    pipeline.trim();
    assert!(!pipeline.surfaces.contains_key(&ID));
    settle(&pipeline);
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_in_use_bytes, 0);

    // Three spatial links after a colour step, colour steps between them: four links, three of
    // them with kept planes, and one pool texture their copies take in turn.
    let spatial = || GpuStep::Spatial(Box::new(spatial::test_spatial()));
    let chained = GpuPlan {
        steps: vec![
            GpuStep::colour(scale(0.5)),
            spatial(),
            GpuStep::colour(scale(1.5)),
            spatial(),
            GpuStep::colour(swap(true)),
            spatial(),
        ],
        ..identity_plan
    };
    for close in [false, true] {
        paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(ID, Some(chained.clone())),
        );
        let slot = pipeline.surfaces[&ID].gpu.as_ref().expect("a slot");
        let held = held_bytes(slot);
        let seen = diagnostics(&pipeline, ID);
        assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
        assert_eq!(seen.gpu_preview_in_use_bytes, held);
        assert_eq!(slot_charge(&device, &chained), Ok(held));
        let charge = chain_charge(&chained.steps, (SIDE, SIDE), (0, 0), BoundaryFormat::Half);
        assert_eq!(charge.intermediates, [64 * 64 * 8; 3]);
        assert_eq!(charge.pool, 64 * 64 * 8, "one copy's texture for all three");
        assert_eq!(slot.pool.textures().len(), 1);
        assert_eq!(seen.gpu_preview_scratch_bytes, charge.pool);
        assert_eq!(
            held,
            SLOT_BYTES + 3 * 2 * MIN_BUFFER + charge.total(),
            "the slot, every earlier link's buffers and the chain"
        );
        // The plan stops, or the surface closes: the pool goes with the slot.
        if close {
            pipeline.trim();
            pipeline.trim();
            assert!(!pipeline.surfaces.contains_key(&ID));
        } else {
            paint(&device, &queue, &mut pipeline, &primitive(ID, None));
        }
        settle(&pipeline);
        let seen = diagnostics(&pipeline, ID);
        assert_eq!(
            (
                seen.gpu_preview_in_use_bytes,
                seen.gpu_preview_scratch_bytes
            ),
            (0, 0)
        );
    }
}

/// A redraw of an unchanged plan encodes no pass; new words or a new boundary version do.
#[test]
fn an_unchanged_plan_encodes_nothing_and_new_words_encode_one_pass() {
    let test = "an_unchanged_plan_encodes_nothing_and_new_words_encode_one_pass";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(1);
    let swapped = |on| primitive(ID, Some(plan(&boundary, vec![swap(on)])));
    let passes = |pipeline: &PhotoPipeline| diagnostics(pipeline, ID).gpu_preview_passes;
    paint(&device, &queue, &mut pipeline, &swapped(false));
    assert_codes(
        &paint(&device, &queue, &mut pipeline, &swapped(false)),
        &codes,
    );
    assert_eq!(passes(&pipeline), 1);
    let drawn = paint(&device, &queue, &mut pipeline, &swapped(true));
    let reversed: Vec<[u8; 3]> = codes.iter().map(|[r, g, b]| [*b, *g, *r]).collect();
    assert_codes(&drawn, &reversed);
    assert_eq!(passes(&pipeline), 2);
    let (next, next_codes) = boundary_with_codes(2);
    let drawn = paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(plan(&next, vec![swap(false)]))),
    );
    assert_codes(&drawn, &next_codes);
    assert_eq!(passes(&pipeline), 3);
    assert_eq!(pipeline.figures.preview.compiles.load(Ordering::Relaxed), 1);
}

/// No adapter able to run the stage: the frame is the CPU's, named, and nothing is charged.
#[test]
fn without_an_adapter_for_the_stage_the_frame_is_the_cpus_and_says_so() {
    let test = "without_an_adapter_for_the_stage_the_frame_is_the_cpus_and_says_so";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue).without_gpu_stage();
    let (boundary, _) = boundary_with_codes(1);
    for _ in 0..2 {
        let drawn = paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(ID, Some(plan(&boundary, vec![identity()]))),
        );
        assert_cpu_frame(&drawn);
        let seen = diagnostics(&pipeline, ID);
        assert_eq!(seen.drawn_path, Some(DrawingPath::Cpu));
        assert_eq!(seen.gpu_fallback, Some(GpuFallback::NoAdapter));
        assert_eq!(seen.drawn_full_version, Some(1));
        assert_eq!(seen.gpu_preview_in_use_bytes, 0);
    }
    assert_eq!(pipeline.figures.preview.compiles.load(Ordering::Relaxed), 0);
}

/// A device lost during a gesture: the next frame is the CPU's and names the loss, the slot goes,
/// and nothing waits for the device to come back.
#[test]
fn a_lost_device_makes_the_next_frame_the_cpus_without_waiting() {
    let test = "a_lost_device_makes_the_next_frame_the_cpus_without_waiting";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(1);
    let gesture = primitive(ID, Some(plan(&boundary, vec![identity()])));
    assert_codes(&paint(&device, &queue, &mut pipeline, &gesture), &codes);
    // The handler the device's lost callback runs.
    pipeline.simulate_device_loss();
    for _ in 0..2 {
        assert_cpu_frame(&paint(&device, &queue, &mut pipeline, &gesture));
        let seen = diagnostics(&pipeline, ID);
        assert_eq!(seen.drawn_path, Some(DrawingPath::Cpu));
        assert_eq!(seen.gpu_fallback, Some(GpuFallback::DeviceLost));
        assert!(pipeline.surfaces[&ID].gpu.is_none());
    }
    settle(&pipeline);
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_in_use_bytes, 0);
}

/// The lost callback is the device's own: destroying the device sets the reason the stage names,
/// and the stage creates nothing on it. Only the stage is run: the photograph's own texture writes
/// are not made for a destroyed device, and nothing is drawn or read back on it.
#[test]
fn a_destroyed_devices_callback_names_the_loss() {
    let test = "a_destroyed_devices_callback_names_the_loss";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    device.destroy();
    wait_until("the lost callback", || {
        let _ = device.poll(wgpu::PollType::Poll);
        pipeline.gpu.lost.load(Ordering::Acquire)
    });
    let (boundary, _) = boundary_with_codes(1);
    let mut surface = pipeline.new_surface();
    pipeline.prepare_gpu(
        &mut surface,
        &device,
        &queue,
        Some(&plan(&boundary, vec![identity()])),
        None,
        None,
    );
    assert_eq!(surface.gpu_outcome, Some(Err(GpuFallback::DeviceLost)));
    assert!(surface.gpu.is_none());
    assert_eq!(pipeline.figures.preview.in_use(), 0);
    assert_eq!(pipeline.figures.preview.compiles.load(Ordering::Relaxed), 0);
}

/// A program that does not compile, an entry name the surface keeps, or two programs claiming
/// one name: the frame is the CPU's and names the pipeline, and the failed sequence is not
/// compiled again on the next tick. A valid plan afterwards draws.
#[test]
fn a_failed_pipeline_makes_the_frame_the_cpus_and_is_not_compiled_again() {
    let test = "a_failed_pipeline_makes_the_frame_the_cpus_and_is_not_compiled_again";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(1);
    let mut conflicting = identity();
    conflicting.source = Cow::Borrowed(
        "fn identity(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
         return rgb.bgr;\n}\n",
    );
    let failing: [(&str, Vec<GpuProgram>); 4] = [
        (
            "parse",
            vec![GpuProgram::new(
                "broken",
                "fn broken(rgb: vec3<f32>) -> vec3<f32> { return rgb }",
            )],
        ),
        (
            "type",
            vec![GpuProgram::new(
                "grey",
                "fn grey(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> f32 {\n    \
                 return rgb.g;\n}\n",
            )],
        ),
        ("surface name", vec![GpuProgram::new("lf_vertex", "")]),
        ("conflicting sources", vec![identity(), conflicting]),
    ];
    for (index, (what, programs)) in failing.into_iter().enumerate() {
        let failed = plan(&boundary, programs);
        for _ in 0..2 {
            assert_cpu_frame(&paint(
                &device,
                &queue,
                &mut pipeline,
                &primitive(ID, Some(failed.clone())),
            ));
            let seen = diagnostics(&pipeline, ID);
            assert_eq!(seen.drawn_path, Some(DrawingPath::Cpu), "{what}");
            assert_eq!(
                seen.gpu_fallback,
                Some(GpuFallback::PipelineFailed),
                "{what}"
            );
            assert_eq!(seen.gpu_preview_in_use_bytes, 0, "{what}");
        }
        assert_eq!(
            pipeline.figures.preview.compiles.load(Ordering::Relaxed),
            index as u64 + 1,
            "{what} is compiled once"
        );
        let message = pipeline.gpu.failure(&failed.steps).expect("a kept failure");
        eprintln!("{test}: {what}: {message}");
    }
    assert_codes(
        &paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(ID, Some(plan(&boundary, vec![identity()]))),
        ),
        &codes,
    );
    assert_eq!(diagnostics(&pipeline, ID).gpu_fallback, None);
}

/// A plan the budget cannot hold is refused before anything is allocated, and names the budget; a
/// second surface's slot cannot take the room the first one holds.
#[test]
fn a_plan_past_the_budget_makes_the_frame_the_cpus_and_names_the_budget() {
    let test = "a_plan_past_the_budget_makes_the_frame_the_cpus_and_names_the_budget";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(1);
    let gesture = |surface| primitive(surface, Some(plan(&boundary, vec![identity()])));
    pipeline.set_gpu_budget(SLOT_BYTES - 1);
    assert_cpu_frame(&paint(&device, &queue, &mut pipeline, &gesture(ID)));
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Cpu));
    assert_eq!(
        seen.gpu_fallback,
        Some(GpuFallback::BudgetExceeded {
            requested: SLOT_BYTES,
            in_use: 0,
            budget: SLOT_BYTES - 1,
        })
    );
    assert_eq!(
        (seen.gpu_preview_in_use_bytes, seen.gpu_preview_peak_bytes),
        (0, 0)
    );
    assert!(
        pipeline.surfaces[&ID].gpu.is_none(),
        "nothing was allocated"
    );

    // Room for one slot: the first surface draws on the GPU, the second is refused beside it.
    pipeline.set_gpu_budget(SLOT_BYTES);
    let other = SurfaceId::new(1);
    assert_codes(&paint(&device, &queue, &mut pipeline, &gesture(ID)), &codes);
    assert_cpu_frame(&paint(&device, &queue, &mut pipeline, &gesture(other)));
    assert_eq!(
        diagnostics(&pipeline, other).gpu_fallback,
        Some(GpuFallback::BudgetExceeded {
            requested: SLOT_BYTES,
            in_use: SLOT_BYTES,
            budget: SLOT_BYTES,
        })
    );
    assert_eq!(
        diagnostics(&pipeline, ID).drawn_path,
        Some(DrawingPath::Gpu)
    );
    assert_eq!(
        diagnostics(&pipeline, other).gpu_preview_in_use_bytes,
        SLOT_BYTES
    );
}

/// A storage block larger than the device allows a storage binding is refused before anything is
/// created, rather than handed to wgpu as an error the default handler would panic on.
#[test]
fn a_block_past_the_storage_binding_limit_makes_the_frame_the_cpus() {
    let test = "a_block_past_the_storage_binding_limit_makes_the_frame_the_cpus";
    let limits = wgpu::Limits {
        max_storage_buffer_binding_size: 2048,
        ..wgpu::Limits::default()
    };
    let Some((device, queue)) = headless_with(test, limits) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(1);
    let mut large = identity();
    large.block = Arc::from(vec![0u32; 750]);
    assert_cpu_frame(&paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(plan(&boundary, vec![large]))),
    ));
    assert_eq!(
        diagnostics(&pipeline, ID).gpu_fallback,
        Some(GpuFallback::BufferLimit {
            bytes: 3000,
            limit: 2048,
        })
    );
    assert_eq!(pipeline.figures.preview.in_use(), 0);
    // Within the limit, the same device draws on the GPU.
    assert_codes(
        &paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(ID, Some(plan(&boundary, vec![identity()]))),
        ),
        &codes,
    );
}

/// A boundary wider than the device allows a texture is refused before anything is created.
#[test]
fn a_boundary_past_the_texture_limit_makes_the_frame_the_cpus() {
    let test = "a_boundary_past_the_texture_limit_makes_the_frame_the_cpus";
    let limits = wgpu::Limits {
        max_texture_dimension_2d: SIDE,
        ..wgpu::Limits::default()
    };
    let Some((device, queue)) = headless_with(test, limits) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let wide = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        SIDE + 1,
        1,
        1,
        std::iter::repeat_n([0.5; 4], 65),
    )
    .expect("a boundary");
    assert_cpu_frame(&paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(plan(&wide, vec![identity()]))),
    ));
    assert_eq!(
        diagnostics(&pipeline, ID).gpu_fallback,
        Some(GpuFallback::TextureLimit {
            width: SIDE + 1,
            height: 1,
            limit: SIDE,
        })
    );
    assert_eq!(pipeline.figures.preview.in_use(), 0);
}

mod blocks;
mod masked;
mod spatial;

/// At a percentage zoom a region plan's frame is the photograph: drawn alone at its rectangle of
/// the whole stage, one texel to one pixel, with no CPU frame of other content composited with
/// it. A whole frame's plan is no frame of a percentage view, so that view draws the CPU's.
#[test]
fn a_percentage_view_draws_a_region_plans_frame_at_its_rectangle() {
    let test = "a_percentage_view_draws_a_region_plans_frame_at_its_rectangle";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(9);
    // The 128 × 128 stage at 100%, panned so its middle 64 × 64 fills the target, and the
    // boundary holding that window of it.
    let stage = (2 * SIDE, 2 * SIDE);
    let viewed = |surface, plan| {
        let mut primitive = primitive(surface, plan);
        primitive.viewport = Some(super::super::ViewportFrames {
            full: None,
            region: None,
            current_content: 1,
            full_stage: stage,
        });
        primitive.offset = Vector::new(-(SIDE as f32) / 2.0, -(SIDE as f32) / 2.0);
        primitive.size = Size::new(stage.0 as f32, stage.1 as f32);
        primitive
    };
    let half = SIDE / 2;
    let mut region = plan(&boundary, vec![identity()]);
    region.texels.origin = [half as f32, half as f32];
    region.region = Some(GpuRegion {
        rect: [half, half, half + SIDE, half + SIDE],
        stage,
    });
    let drawn = paint(&device, &queue, &mut pipeline, &viewed(ID, Some(region)));
    assert_codes(&drawn, &codes);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert_eq!(seen.drawn_gpu_boundary, Some(9));
    assert_eq!(seen.drawn_full_version, None, "no CPU frame was drawn");
    // A whole frame's plan at a percentage view, and a region plan of another stage, run nothing.
    let mut elsewhere = plan(&boundary, vec![identity()]);
    elsewhere.region = Some(GpuRegion {
        rect: [0, 0, SIDE, SIDE],
        stage: (SIDE, SIDE),
    });
    for (index, plan) in [plan(&boundary, vec![identity()]), elsewhere]
        .into_iter()
        .enumerate()
    {
        let surface = SurfaceId::new(10 + index as u64);
        paint(&device, &queue, &mut pipeline, &viewed(surface, Some(plan)));
        let seen = diagnostics(&pipeline, surface);
        assert_ne!(seen.drawn_path, Some(DrawingPath::Gpu), "plan {index}");
        assert_eq!(seen.drawn_gpu_boundary, None, "plan {index}");
    }
    settle(&pipeline);
}

/// A region plan's output takes the bucket the CPU's region picture of its rectangle reserves —
/// its footprint and a small margin, where the photograph's square bucket would hold its longer
/// side on both axes — and is charged that. Magnified across its far corner, its frame draws what
/// that picture of the same codes draws, pixel for pixel: the same placement and filter weights,
/// and its edge texels repeated past it. A dissolve from it draws it at its rectangle the same way.
#[test]
fn a_region_plans_output_takes_the_cpu_regions_bucket_and_draws_as_it_does() {
    let test = "a_region_plans_output_takes_the_cpu_regions_bucket_and_draws_as_it_does";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    // A 150 × 90 region of a 400 × 300 stage, whose boundary is the region alone, so the frame's
    // pass repeats its edge texels past it as a picture's upload does.
    let (width, height, stage) = (150, 90, (400, 300));
    let rect = [100, 80, 100 + width, 80 + height];
    let code = |index: u32, a: u32, b: u32| ((index * a + b) % 256) as u8;
    let values: Vec<[f32; 3]> = (0..width * height)
        .map(|index| [code(index, 1, 0), code(index, 7, 3), code(index, 13, 5)].map(held))
        .collect();
    let boundary = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        width,
        height,
        3,
        values.iter().map(|[r, g, b]| [*r, *g, *b, 1.0]),
    )
    .expect("a boundary");
    // The CPU's region picture of the codes the GPU frame's pass computes.
    let pixels: Vec<u8> = values
        .iter()
        .flat_map(|rgb| {
            let [r, g, b] = rgb.map(|value| srgb::code(f64::from(value)));
            [r, g, b, 255]
        })
        .collect();
    let picture = crate::photo_surface::RegionFrame {
        frame: Frame::new(Arc::new(pixels), width, height, 7).expect("a region frame"),
        rect,
        stage,
        full_stage: stage,
        scale: 1.0,
        quality: crate::RegionQuality::Exact,
        content_id: 1,
        generation: 1,
    };
    let mut region = plan(&boundary, vec![identity()]);
    region.texels.origin = [rect[0] as f32, rect[1] as f32];
    region.region = Some(GpuRegion { rect, stage });
    // At 250%, the region's far corner 100 pixels into a 128 × 128 target: the last texels'
    // outer halves are drawn, blending in the column and row past them.
    let target = (2 * SIDE, 2 * SIDE);
    let zoom = 2.5;
    let viewed = |surface, plan: Option<GpuPlan>, dissolve: Option<f32>| {
        let mut primitive = primitive(surface, plan);
        primitive.layers = Vec::new();
        primitive.viewport = Some(super::super::ViewportFrames {
            full: None,
            region: Some(picture.clone()),
            current_content: 1,
            full_stage: stage,
        });
        primitive.size = Size::new(stage.0 as f32 * zoom, stage.1 as f32 * zoom);
        primitive.offset =
            Vector::new(100.0 - rect[2] as f32 * zoom, 100.0 - rect[3] as f32 * zoom);
        primitive.clip_size = Size::new(target.0 as f32, target.1 as f32);
        primitive.dissolve = dissolve.map(|share| DissolveFrame {
            dissolve: Dissolve::start(42, 7),
            share,
        });
        primitive
    };
    let cpu = paint_into(
        &device,
        &queue,
        &mut pipeline,
        &viewed(SurfaceId::new(20), None, None),
        target,
    );
    let gpu = paint_into(
        &device,
        &queue,
        &mut pipeline,
        &viewed(ID, Some(region.clone()), None),
        target,
    );
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    // The two textures: the picture's bucket, the region and two more each way in steps of 64.
    let surfaces = &pipeline.surfaces;
    let output = surfaces[&ID]
        .gpu
        .as_ref()
        .expect("the slot")
        .output()
        .capacity;
    let held_picture = surfaces[&SurfaceId::new(20)].regions.iter().flatten();
    let capacities: Vec<(u32, u32)> = held_picture.map(|picture| picture.capacity).collect();
    assert_eq!((output, capacities), ((192, 128), vec![(192, 128)]));
    assert_eq!(
        seen.gpu_preview_in_use_bytes,
        u64::from(width * height) * 8 + 192 * 128 * 4 + UNIFORM_SIZE as u64 + 2 * MIN_BUFFER,
        "the region's bucket, where the photograph's would be 256 × 256"
    );
    assert_eq!(
        texture_charge(
            (width, height),
            BoundaryFormat::Half,
            (width, height),
            None,
            true,
            8192
        ),
        seen.gpu_preview_in_use_bytes - 2 * MIN_BUFFER,
        "what the desktop holds the slot to"
    );
    // The far corner is drawn, through texels of many values, then the clear colour past it.
    let distinct: std::collections::HashSet<&[u8]> = gpu.chunks_exact(4).collect();
    assert!(distinct.len() > 1000, "{} colours", distinct.len());
    assert_eq!(
        gpu[(110 * target.0 as usize + 110) * 4..][..4],
        [255, 0, 0, 255]
    );
    let differing = gpu
        .chunks_exact(4)
        .zip(cpu.chunks_exact(4))
        .filter(|(gpu, cpu)| gpu != cpu)
        .count();
    assert_eq!(differing, 0, "pixels the GPU frame draws otherwise");
    // A dissolve from the frame into the picture of the same codes, each drawn at the rectangle.
    let dissolved = paint_into(
        &device,
        &queue,
        &mut pipeline,
        &viewed(ID, None, Some(0.5)),
        target,
    );
    assert!(diagnostics(&pipeline, ID).drawn_dissolve.is_some());
    let differing = dissolved
        .chunks_exact(4)
        .zip(cpu.chunks_exact(4))
        .filter(|(dissolved, cpu)| dissolved != cpu)
        .count();
    assert_eq!(differing, 0, "pixels the dissolve draws otherwise");
    settle(&pipeline);
}

/// A boundary whose texels its caller let go once the slot held them draws from that slot, a tick
/// changing only its words, and a plan of another output or tail over it refits the slot around
/// the boundary it holds; a slot that no longer holds it — released by a frame with no plan —
/// falls back naming it, drawing the CPU frame, and the texels uploaded again draw once more. A
/// sequence still compiling leaves the slot, and the boundary it holds, as they were.
#[test]
fn a_resident_boundary_draws_from_the_slot_that_holds_it() {
    let test = "a_resident_boundary_draws_from_the_slot_that_holds_it";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(3);
    let resident = boundary.resident();
    assert!(boundary.holds_texels() && !resident.holds_texels());
    assert_eq!(
        (resident.size(), resident.version(), resident.bytes()),
        (boundary.size(), boundary.version(), boundary.bytes())
    );
    let identity_of =
        |boundary: &GpuBoundary| primitive(ID, Some(plan(boundary, vec![identity()])));
    let drawn = paint(&device, &queue, &mut pipeline, &identity_of(&boundary));
    assert_codes(&drawn, &codes);
    let slot_bytes = diagnostics(&pipeline, ID).gpu_preview_in_use_bytes;
    // The next tick names the resident boundary: drawn from the slot.
    let drawn = paint(&device, &queue, &mut pipeline, &identity_of(&resident));
    assert_codes(&drawn, &codes);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert_eq!(seen.gpu_ready_boundary, Some(3));
    // A sequence the frame finds still compiling keeps the slot and the boundary in it.
    let scaled = plan(&resident, vec![scale(0.5)]);
    paint_prepared(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(scaled.clone())),
    );
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.gpu_fallback, Some(GpuFallback::Compiling));
    assert_eq!(
        seen.gpu_preview_in_use_bytes, slot_bytes,
        "the slot is kept"
    );
    pipeline.compile_now(&device, &scaled);
    paint(&device, &queue, &mut pipeline, &primitive(ID, Some(scaled)));
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(
        seen.drawn_path,
        Some(DrawingPath::Gpu),
        "drawn from the slot it kept"
    );
    assert_eq!(seen.gpu_fallback, None);
    // A plan of another shape over the same boundary — here the identity tail a colour step after
    // a stack's last spatial one brings — refits the slot and keeps the boundary it holds.
    let (width, height) = resident.size();
    let mut tailed = plan(&resident, vec![identity()]);
    tailed.steps.push(GpuStep::Geometry(GpuTail::affine(
        (width, height),
        [0, 0, width, height],
        false,
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
    )));
    tailed.steps.push(GpuStep::colour(identity()));
    pipeline.compile_now(&device, &tailed);
    let drawn = paint(&device, &queue, &mut pipeline, &primitive(ID, Some(tailed)));
    assert_codes(&drawn, &codes);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(
        seen.drawn_path,
        Some(DrawingPath::Gpu),
        "drawn from the kept boundary"
    );
    assert_eq!(seen.gpu_fallback, None);
    assert_eq!(seen.gpu_ready_boundary, Some(3));
    // A frame with no plan lets the slot go; the resident boundary cannot be drawn then.
    assert_cpu_frame(&paint(&device, &queue, &mut pipeline, &primitive(ID, None)));
    let drawn = paint(&device, &queue, &mut pipeline, &identity_of(&resident));
    assert_cpu_frame(&drawn);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.gpu_fallback, Some(GpuFallback::BoundaryReleased));
    assert_eq!(
        seen.gpu_fallback.map(GpuFallback::as_str),
        Some("boundary-released")
    );
    assert_eq!(seen.gpu_ready_boundary, None);
    // Its texels, brought again, draw.
    let drawn = paint(&device, &queue, &mut pipeline, &identity_of(&boundary));
    assert_codes(&drawn, &codes);
    settle(&pipeline);
}

/// A plan over a boundary of another version, handed with a change measured from the plan the
/// slot holds — no change, as a caller comparing only the plans' operations measures it — is
/// evaluated whole: the new boundary's texels differ everywhere, so the frame is all of its codes,
/// not the old boundary's with nothing drawn again.
#[test]
fn a_change_measured_over_another_boundary_is_evaluated_whole() {
    let test = "a_change_measured_over_another_boundary_is_evaluated_whole";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let changed = |boundary: &GpuBoundary, serial: u64, since: Option<(u64, [u32; 4])>| {
        let mut primitive = primitive(ID, Some(plan(boundary, vec![identity()])));
        primitive.gpu_options.change = Some(GpuChange { serial, since });
        primitive
    };
    let (first, codes) = boundary_with_codes(3);
    let drawn = paint(&device, &queue, &mut pipeline, &changed(&first, 1, None));
    assert_codes(&drawn, &codes);
    // The same shape and format, other values: every texel another code.
    let other: Vec<f32> = (0..SIDE * SIDE * 3)
        .map(|index| held(((index * 37 + 11) % 256) as u8))
        .collect();
    let second = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        SIDE,
        SIDE,
        4,
        other
            .chunks_exact(3)
            .map(|rgb| [rgb[0], rgb[1], rgb[2], 1.0]),
    )
    .expect("a whole boundary");
    let other_codes: Vec<[u8; 3]> = other
        .chunks_exact(3)
        .map(|rgb| [0, 1, 2].map(|channel| srgb::code(f64::from(rgb[channel]))))
        .collect();
    let drawn = paint(
        &device,
        &queue,
        &mut pipeline,
        &changed(&second, 2, Some((1, [0; 4]))),
    );
    assert_codes(&drawn, &other_codes);
    settle(&pipeline);
}

/// A boundary larger than a frame's upload arrives over several frames' `prepare`s, at most the
/// frame's bound each: until its last chunk the frame is the CPU's, the fallback naming the upload
/// and how far it is, the slot kept and every chunk staged once; the frame that writes the last
/// chunk draws the plan, exactly. A boundary of another version starts over.
#[test]
fn a_boundary_arrives_a_frames_chunks_at_a_time() {
    let test = "a_boundary_arrives_a_frames_chunks_at_a_time";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(3);
    let row = u64::from(SIDE) * 8;
    // Twenty rows a frame: the 64-row boundary arrives over four frames.
    pipeline.set_upload_per_frame(20 * row);
    let plan_of = |boundary: &GpuBoundary| primitive(ID, Some(plan(boundary, vec![identity()])));
    let staged = |pipeline: &PhotoPipeline| diagnostics(pipeline, ID).gpu_preview_staged_bytes;
    let mut before = staged(&pipeline);
    for rows in [20u64, 40, 60] {
        let drawn = paint(&device, &queue, &mut pipeline, &plan_of(&boundary));
        assert_cpu_frame(&drawn);
        let seen = diagnostics(&pipeline, ID);
        assert_eq!(
            seen.gpu_fallback,
            Some(GpuFallback::BoundaryUploading {
                uploaded: rows * row,
                bytes: boundary.bytes(),
            }),
            "after {rows} rows"
        );
        assert_eq!(seen.gpu_ready_boundary, None);
        assert!(seen.gpu_preview_in_use_bytes > 0, "the slot is kept");
        assert_eq!(
            seen.gpu_preview_staged_bytes - before,
            20 * row,
            "one frame's chunks"
        );
        before = seen.gpu_preview_staged_bytes;
    }
    let drawn = paint(&device, &queue, &mut pipeline, &plan_of(&boundary));
    assert_codes(&drawn, &codes);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.gpu_fallback, None);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert_eq!(seen.gpu_ready_boundary, Some(3));
    assert_eq!(
        seen.gpu_preview_staged_bytes - before,
        4 * row,
        "the last rows"
    );
    // Drawn again, nothing more is staged.
    let before = staged(&pipeline);
    assert_codes(
        &paint(&device, &queue, &mut pipeline, &plan_of(&boundary)),
        &codes,
    );
    assert_eq!(staged(&pipeline), before);
    // Another version starts over, from its first row.
    let (next, _) = boundary_with_codes(4);
    assert_cpu_frame(&paint(&device, &queue, &mut pipeline, &plan_of(&next)));
    assert_eq!(
        diagnostics(&pipeline, ID).gpu_fallback,
        Some(GpuFallback::BoundaryUploading {
            uploaded: 20 * row,
            bytes: next.bytes(),
        })
    );
    // At the recorded bound the same boundary, 32 KiB, arrives in its first frame.
    pipeline.set_upload_per_frame(UPLOAD_PER_FRAME);
    let (small, small_codes) = boundary_with_codes(5);
    assert_codes(
        &paint(&device, &queue, &mut pipeline, &plan_of(&small)),
        &small_codes,
    );
    settle(&pipeline);
}

/// A functional measurement of one boundary's arrival, not a timing: the largest `f32` boundary a
/// RAW region with Clarity's margin reads in the largest window, 4705 × 2817 texels (212 MB), held
/// as the desktop holds it, handed to the surface frame by frame until the surface has it, then let
/// go as the desktop lets it go. Each frame records what the surface has staged, what the
/// GPU-preview slot holds and the process's footprint; the process's own peak footprint catches
/// what falls between frames. `LUXFORGE_ARRIVAL=whole` uploads the boundary in its first frame, as
/// before the upload was spread; anything else spreads it at [`UPLOAD_PER_FRAME`]. Run each in its
/// own process, so the peak is the arrival's.
#[test]
#[ignore = "a functional measurement of a boundary's arrival: LUXFORGE_ARRIVAL=spread|whole, one \
            process each"]
fn a_boundary_arrival_measured() {
    let test = "a_boundary_arrival_measured";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let whole = std::env::var("LUXFORGE_ARRIVAL").is_ok_and(|mode| mode == "whole");
    let mut pipeline = own_pipeline(&device, &queue);
    pipeline.set_upload_per_frame(if whole { u64::MAX } else { UPLOAD_PER_FRAME });
    let mut sampler = luxforge_process::Sampler::new();
    let mut footprint = || {
        let memory = sampler.read().memory;
        (
            memory.bytes.expect("the footprint"),
            memory.peak_bytes.expect("the peak footprint"),
        )
    };
    let mb = |bytes: u64| bytes as f64 / 1e6;
    let (width, height) = (4705u32, 2817u32);
    let bytes = u64::from(width) * u64::from(height) * 16;
    // The pipeline is up and the identity sequence compiled before anything is counted.
    let warm = GpuBoundary::from_linear(BoundaryFormat::Float, 4, 4, 1, [[0.5; 4]; 16]).unwrap();
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(plan(&warm, vec![identity()]))),
    );
    paint(&device, &queue, &mut pipeline, &primitive(ID, None));
    settle(&pipeline);
    let (baseline, _) = footprint();
    // The worker's render, which the desktop holds: every page written.
    let texels = Arc::new(vec![1u8; bytes as usize]);
    let boundary = GpuBoundary::new(texels, width, height, 2, BoundaryFormat::Float).unwrap();
    let (held, _) = footprint();
    eprintln!(
        "{test}: {} — boundary {:.1} MB; baseline {:.1} MB, held {:+.1} MB",
        if whole { "whole" } else { "spread" },
        mb(bytes),
        mb(baseline),
        mb(held - baseline)
    );
    let mut frames = 0;
    let mut sampled_peak = held;
    loop {
        let before = diagnostics(&pipeline, ID).gpu_preview_staged_bytes;
        paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(ID, Some(plan(&boundary, vec![identity()]))),
        );
        frames += 1;
        let seen = diagnostics(&pipeline, ID);
        let (now, _) = footprint();
        sampled_peak = sampled_peak.max(now);
        eprintln!(
            "{test}: frame {frames}: staged {:.1} MB, slot {:.1} MB, footprint {:+.1} MB, {:?}",
            mb(seen.gpu_preview_staged_bytes - before),
            mb(seen.gpu_preview_in_use_bytes),
            mb(now - baseline),
            seen.gpu_fallback.map(GpuFallback::as_str)
        );
        if seen.gpu_ready_boundary == Some(2) {
            break;
        }
        assert!(frames < 64, "the boundary never arrived");
    }
    // The desktop lets its copy go once the surface holds the boundary.
    let resident = boundary.resident();
    assert_eq!(
        boundary.texels.as_ref().map(Arc::strong_count),
        Some(1),
        "nothing but the desktop's copy holds the texels"
    );
    drop(boundary);
    let (dropped, _) = footprint();
    eprintln!(
        "{test}: the copy let go: footprint {:+.1} MB",
        mb(dropped - baseline)
    );
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(plan(&resident, vec![identity()]))),
    );
    settle(&pipeline);
    let (after, peak) = footprint();
    let slot = diagnostics(&pipeline, ID).gpu_preview_in_use_bytes;
    eprintln!(
        "{test}: arrived in {frames} frames; sampled peak {:+.1} MB ({:.2} × the boundary), \
         process peak {:+.1} MB ({:.2} ×); after the copy is let go {:+.1} MB, the slot {:.1} MB",
        mb(sampled_peak - baseline),
        (sampled_peak - baseline) as f64 / bytes as f64,
        mb(peak.saturating_sub(baseline)),
        peak.saturating_sub(baseline) as f64 / bytes as f64,
        mb(after.saturating_sub(baseline)),
        mb(slot)
    );
}
