//! When the queue reports a GPU frame's pass complete, on a headless device the test creates.
//!
//! A headless test with no adapter prints that it was skipped and asserts nothing. It is not GPU
//! evidence: the skip is the report.
use super::super::super::{PhotoPipeline, PhotoPrimitive};
use super::super::tests::{
    ID, SIDE, diagnostics, headless, identity, own_pipeline, paint, plan, primitive, scale, settle,
};
use super::super::{GpuBoundary, GpuProgram};
use super::*;
use iced::widget::shader::{Primitive as _, Viewport};
use iced::{Rectangle, Size};
use luxforge_testbase::{Distribution, wait_until};
use std::time::Duration;

/// `primitive`'s `prepare`, as Iced runs it for one frame, and nothing else: no draw, no submit of
/// the frame's own and no poll.
fn prepare(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &mut PhotoPipeline,
    primitive: &PhotoPrimitive,
) {
    let bounds = Rectangle::new(iced::Point::ORIGIN, Size::new(SIDE as f32, SIDE as f32));
    let viewport = Viewport::with_physical_size(Size::new(SIDE, SIDE), 1.0);
    primitive.prepare(pipeline, device, queue, &bounds, &viewport);
}

fn clock(pipeline: &PhotoPipeline) -> Arc<PassClock> {
    pipeline
        .surfaces
        .get(&ID)
        .and_then(|surface| surface.gpu.as_ref())
        .expect("a GPU-preview slot")
        .clock()
}

/// wgpu runs a pass's callback from the next submit that finds it complete, so nothing polls or
/// waits for the figure: a pass prepared and left alone stays unreported however long the GPU has
/// been done, and the next submit — Iced's own, every frame it draws — reports it. The figure then
/// runs to that submit, which is why it is at most a frame late while frames keep coming.
#[test]
fn a_pass_is_reported_complete_by_a_later_submit_without_a_poll() {
    let test = "a_pass_is_reported_complete_by_a_later_submit_without_a_poll";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let boundary = GpuBoundary::from_linear(
        SIDE,
        SIDE,
        1,
        std::iter::repeat_n([0.2, 0.4, 0.6, 1.0], (SIDE * SIDE) as usize),
    )
    .expect("a boundary");
    prepare(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(plan(&boundary, vec![identity()]))),
    );
    let clock = clock(&pipeline);
    assert_eq!(clock.newest(), None, "not reported by its own submit");
    // Time passing is what this waits for: with no submit or poll, nobody is told.
    let quiet = Instant::now();
    wait_until("200 ms with no submit or poll", || {
        quiet.elapsed() >= Duration::from_millis(200)
    });
    assert_eq!(
        clock.newest(),
        None,
        "nothing polls: the GPU finished long ago and nobody has been told"
    );
    queue.submit([]);
    assert_eq!(
        clock.newest(),
        Some(1),
        "the next submit's maintenance reports it"
    );
    let figure = clock.figure().expect("a figure");
    assert!(
        figure >= 200_000,
        "the figure runs to the submit that learned of it: {figure} µs"
    );
    eprintln!("{test}: reported by a later submit after {figure} µs");
}

/// A frame drawn and read back reports its pass's figure, live, beside the interface thread's
/// own; a newer pass replaces it, and a released slot reports none.
#[test]
fn the_drawn_gpu_frame_reports_its_completion_figure() {
    let test = "the_drawn_gpu_frame_reports_its_completion_figure";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let boundary = GpuBoundary::from_linear(
        SIDE,
        SIDE,
        1,
        std::iter::repeat_n([0.2, 0.4, 0.6, 1.0], (SIDE * SIDE) as usize),
    )
    .expect("a boundary");
    for (pass, factor) in [(1, 1.0), (2, 0.5)] {
        paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(ID, Some(plan(&boundary, vec![scale(factor)]))),
        );
        let seen = diagnostics(&pipeline, ID);
        let done = seen
            .gpu_preview_done_us
            .expect("reported by the readback's wait");
        let prepared = seen
            .gpu_preview_frame_us
            .expect("the interface thread's figure");
        assert!(done >= prepared, "{done} µs against {prepared} µs");
        assert_eq!(clock(&pipeline).newest(), Some(pass));
    }
    paint(&device, &queue, &mut pipeline, &primitive(ID, None));
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_done_us, None);
    settle(&pipeline);
}

/// Spins on a few transcendentals per channel, to stand for a chain of colour programs.
fn heavy() -> GpuProgram {
    GpuProgram::new(
        "heavy",
        "fn heavy(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {\n    \
         var value = max(rgb, vec3<f32>(0.0));\n    \
         for (var i = 0u; i < 24u; i = i + 1u) {\n        \
         value = pow(value + vec3<f32>(1e-6), vec3<f32>(1.0 / 2.4));\n        \
         value = pow(value, vec3<f32>(2.4)) * exp(-1e-7 * f32(i));\n    }\n    \
         return value;\n}\n",
    )
}

/// Measurement, not a pass: on this host's adapter, for a Fit-sized boundary, how long the
/// interface thread spends preparing a pass, how soon after it the GPU has finished (found by a
/// test-only blocking wait, which the editor never makes), and how often the frame's own submit, made
/// straight after the draw, already finds it finished. Iced encodes the rest of the window between
/// the stage's submit and its own, so in the editor that share is at least this one.
#[test]
#[ignore = "measurement: cargo test -p luxforge-ui --lib gpu_frame_completion -- --ignored --nocapture"]
fn gpu_frame_completion_is_measured() {
    let test = "gpu_frame_completion_is_measured";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let (width, height) = (2400, 1600);
    let boundary = GpuBoundary::from_linear(
        width,
        height,
        1,
        (0..width * height).map(|index| {
            let value = (index % 997) as f32 / 997.0;
            [value, 1.0 - value, value * 0.5, 1.0]
        }),
    )
    .expect("a boundary");
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("completion probe target"),
        size: wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    for (name, programs) in [
        ("identity", vec![identity()]),
        ("heavy", vec![heavy(), scale(1.0)]),
    ] {
        let mut pipeline = own_pipeline(&device, &queue);
        let (mut prepared, mut finished) = (Vec::new(), Vec::new());
        let mut in_frame = 0;
        let samples = 60;
        for sample in 0..=samples {
            // New words every frame, as a drag's ticks have, so every frame encodes a pass.
            let mut programs = programs.clone();
            programs.push(scale(1.0 + sample as f32 * 1e-6));
            let primitive = primitive(ID, Some(plan(&boundary, programs)));
            prepare(&device, &queue, &mut pipeline, &primitive);
            let clock = clock(&pipeline);
            let pass = pipeline
                .surfaces
                .get(&ID)
                .and_then(|surface| surface.gpu.as_ref())
                .expect("a GPU-preview slot")
                .passes;
            // The frame's own submit, straight after the draw.
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            {
                let mut render = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("completion probe frame"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                assert!(primitive.draw(&pipeline, &mut render));
            }
            queue.submit([encoder.finish()]);
            let reported_in_frame = clock.newest() == Some(pass);
            // A blocking wait, which the editor never makes: the device's maintenance runs as
            // soon as the GPU is done, so the callback's figure is when it finished.
            device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .expect("device wait");
            assert_eq!(clock.newest(), Some(pass));
            if sample == 0 {
                // The first frame uploads the boundary and compiles the pipeline.
                continue;
            }
            in_frame += usize::from(reported_in_frame);
            let seen = diagnostics(&pipeline, ID);
            prepared.push(seen.gpu_preview_frame_us.unwrap_or_default() as f64 / 1000.0);
            finished.push(clock.figure().unwrap_or_default() as f64 / 1000.0);
        }
        let prepared = Distribution::of(prepared).expect("frames");
        let finished = Distribution::of(finished).expect("frames");
        eprintln!(
            "{test}: {name} over {width} × {height}, {samples} frames: interface thread p50 \
             {:.3} ms, p95 {:.3} ms; GPU finished (blocking wait) p50 {:.3} ms, p95 {:.3} ms; \
             reported by the frame's own submit in {in_frame} of {samples}",
            prepared.p50, prepared.p95, finished.p50, finished.p95,
        );
        drop(pipeline);
    }
}
