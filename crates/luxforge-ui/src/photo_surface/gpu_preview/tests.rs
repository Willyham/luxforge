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
fn identity() -> GpuProgram {
    GpuProgram::new(
        "identity",
        "fn identity(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
         return rgb;\n}\n",
    )
}

/// Scales by its one uniform word.
fn scale(factor: f32) -> GpuProgram {
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

fn plan(boundary: &GpuBoundary, programs: Vec<GpuProgram>) -> GpuPlan {
    GpuPlan {
        boundary: boundary.clone(),
        texels: TexelMap::IDENTITY,
        steps: programs.into_iter().map(GpuStep::Colour).collect(),
    }
}

// ---- Without a device -------------------------------------------------------------------------

/// What a caller's tests check of every program it hands over: the program alone, against the
/// prelude and the convention. The stage checks each program the same way before it compiles.
#[test]
fn a_step_is_checked_against_the_convention_on_its_own() {
    for program in [identity(), scale(0.5), swap(true), stripe(0.5, 4)] {
        validate_step(&GpuStep::Colour(program.clone()))
            .unwrap_or_else(|error| panic!("{}: {error}", program.entry));
    }
    let colour = |entry: &'static str, source: &'static str| {
        validate_step(&GpuStep::Colour(GpuProgram::new(entry, source))).unwrap_err()
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
            "binding or global",
        ),
        (
            "a private global",
            colour(
                "bad",
                "var<private> bad_state: f32;\n\
                 fn bad(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n\
                 return rgb;\n}\n",
            ),
            "binding or global",
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
        ("a reserved name", colour("lf_bad", ""), "lf_"),
    ] {
        assert!(error.contains(expected), "{what}: {error}");
    }
}

#[test]
fn assembly_includes_a_shared_program_once_and_refuses_what_cannot_be_chained() {
    // Two layers of one unit: one source, two calls, each with its own base indices.
    let steps: Vec<GpuStep> = [scale(0.5), swap(true), scale(2.0)]
        .into_iter()
        .map(GpuStep::Colour)
        .collect();
    let source = assemble(&steps).expect("an assembled shader");
    assert_eq!(source.matches("fn scale(").count(), 1);
    assert!(source.contains("rgb = scale(rgb, pos, lf_words[4u], lf_words[5u]);"));
    assert!(source.contains("rgb = swap(rgb, pos, lf_words[6u], lf_words[7u]);"));
    assert!(source.contains("rgb = scale(rgb, pos, lf_words[8u], lf_words[9u]);"));
    validate(&source).expect("the chain validates");
    validate(&assemble(&[]).expect("no steps")).expect("an empty chain is the identity");

    let refused = |program: GpuProgram| assemble(&[GpuStep::Colour(program)]);
    for (entry, why) in [
        ("lf_mine", "lf_"),
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
    let error = assemble(&[GpuStep::Colour(scale(0.5)), GpuStep::Colour(other)]).unwrap_err();
    assert!(error.contains("different sources"), "{error}");
}

#[test]
fn the_words_are_the_map_then_each_programs_bases_then_their_words() {
    let boundary = GpuBoundary::from_linear(1, 1, 1, [[0.0; 4]]).unwrap();
    let mut chain = plan(&boundary, vec![scale(0.5), stripe(0.25, 3), swap(true)]);
    chain.texels = TexelMap {
        origin: [2.0, 5.0],
        step: [1.0, 0.5],
    };
    let (mut words, mut blocks) = (Vec::new(), Vec::new());
    pack(&chain, &mut words, &mut blocks);
    let header = MAP_WORDS + 2 * 3;
    assert_eq!(
        words,
        [
            2f32.to_bits(),
            5f32.to_bits(),
            1f32.to_bits(),
            0.5f32.to_bits(),
            // scale: its word after the header, no block.
            header as u32,
            0,
            // stripe: no words, the first two block words.
            header as u32 + 1,
            0,
            // swap: one word, after the stripe's block.
            header as u32 + 1,
            2,
            0.5f32.to_bits(),
            1,
        ]
    );
    assert_eq!(blocks, [0.25f32.to_bits(), 3]);
    // Every block empty still binds one word.
    pack(&plan(&boundary, vec![identity()]), &mut words, &mut blocks);
    assert_eq!(blocks, [0]);
}

#[test]
fn a_boundary_holds_exactly_its_half_float_texels() {
    let boundary =
        GpuBoundary::from_linear(2, 1, 9, [[0.5, -2.0, 1.0e5, 1.0], [0.0, 1.0, 0.1, 1.0]])
            .expect("two texels");
    assert_eq!((boundary.size(), boundary.version()), ((2, 1), 9));
    let halves: Vec<f32> = (*boundary.texels)
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
        GpuBoundary::from_linear(2, 1, 1, [[0.0; 4]]).is_none(),
        "too few"
    );
    assert!(
        GpuBoundary::from_linear(1, 1, 1, [[0.0; 4]; 2]).is_none(),
        "too many"
    );
    assert!(GpuBoundary::new(Arc::new(vec![0u8; 15]), 2, 1, 1).is_none());
    assert!(GpuBoundary::new(Arc::new(vec![0u8; 16]), 2, 1, 1).is_some());
    assert!(GpuBoundary::new(Arc::new(Vec::<u8>::new()), 0, 0, 1).is_none());
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
fn headless(test: &str) -> Option<(wgpu::Device, wgpu::Queue)> {
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
fn own_pipeline(device: &wgpu::Device, queue: &wgpu::Queue) -> PhotoPipeline {
    PhotoPipeline::with_figures(
        device,
        queue,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        Arc::default(),
    )
}

fn diagnostics(pipeline: &PhotoPipeline, surface: SurfaceId) -> SurfaceDiagnostics {
    pipeline.figures.diagnostics_for(surface)
}

const ID: SurfaceId = SurfaceId::new(0);
const SIDE: u32 = 64;

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
fn primitive(surface: SurfaceId, plan: Option<GpuPlan>) -> PhotoPrimitive {
    PhotoPrimitive {
        surface,
        layers: vec![(Layer::Photo, cpu_frame())],
        viewport: None,
        region_overlays: [None, None],
        gpu: plan,
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
fn paint(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &mut PhotoPipeline,
    primitive: &PhotoPrimitive,
) -> Vec<u8> {
    let bounds = Rectangle::new(iced::Point::ORIGIN, Size::new(SIDE as f32, SIDE as f32));
    let viewport = Viewport::with_physical_size(Size::new(SIDE, SIDE), 1.0);
    primitive.prepare(pipeline, device, queue, &bounds, &viewport);
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gpu preview readback target"),
        size: wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
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
        size: u64::from(SIDE * SIDE * 4),
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
                bytes_per_row: Some(SIDE * 4),
                rows_per_image: Some(SIDE),
            },
        },
        wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
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
fn assert_cpu_frame(bytes: &[u8]) {
    let [r, g, b, a] = CPU_RGBA;
    for (index, pixel) in bytes.chunks_exact(4).enumerate() {
        assert_eq!(pixel, [b, g, r, a], "CPU-path pixel {index}");
    }
}

/// Wait for `pipeline`'s retirements, which its worker finishes when the GPU is done with them.
fn settle(pipeline: &PhotoPipeline) {
    wait_until("the pipeline's retirements", || {
        pipeline.figures.retirement_pending.load(Ordering::Acquire) == 0
    });
}

/// One 8-bit code's linear value, as the boundary holds it: the nearest half float.
fn held(code: u8) -> f32 {
    half::f16::from_f32(srgb::decode(code) as f32).to_f32()
}

/// A 64 × 64 boundary of three different permutations of every code's linear value, its last row
/// out of range — negative, past white, the largest half float, a subnormal — and the codes the
/// independent reference encodes each texel to.
fn boundary_with_codes(version: u64) -> (GpuBoundary, Vec<[u8; 3]>) {
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
fn assert_codes(bytes: &[u8], codes: &[[u8; 3]]) {
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

/// The calling convention end to end: two layers of one unit sharing its function with their own
/// words, a program reading its words, one reading its block and the stage position through the
/// texel map, chained in order.
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
    let drawn = paint(&device, &queue, &mut pipeline, &primitive(ID, Some(chain)));
    // Halving and doubling are exact, so the chain is the swap and the stripes.
    let expected: Vec<[u8; 3]> = codes
        .iter()
        .enumerate()
        .map(|(index, [r, g, b])| {
            let (x, y) = (index as u32 % SIDE, index as u32 / SIDE);
            if (x + 2 + y + 5) % 4 == 0 {
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

/// Every allocation is charged, and the in-use figure returns to zero once the slot is released —
/// when the plan stops, and when the surface closes.
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
    let slot = pipeline.surfaces[&ID].gpu.as_ref().expect("a slot");
    let texture = |texture: &wgpu::Texture| {
        let size = texture.size();
        u64::from(size.width)
            * u64::from(size.height)
            * u64::from(
                texture
                    .format()
                    .block_copy_size(None)
                    .expect("a plain format"),
            )
    };
    let held = texture(&slot.boundary)
        + texture(&slot.output.tiles[0].texture)
        + slot.output.tiles[0].uniform.size()
        + slot.words.buffer.size()
        + slot.blocks.buffer.size();
    assert_eq!(held, SLOT_BYTES);
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_in_use_bytes, held);

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
        &primitive(ID, Some(identity_plan)),
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
        ("reserved name", vec![GpuProgram::new("lf_mine", "")]),
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
    let wide = GpuBoundary::from_linear(SIDE + 1, 1, 1, std::iter::repeat_n([0.5; 4], 65))
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
