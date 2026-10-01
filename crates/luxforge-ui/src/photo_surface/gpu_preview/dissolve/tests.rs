//! The settle dissolve's own tests: its clock and the widget's redraw requests without a device,
//! then the dissolve drawn on a headless device the test creates and read back through the
//! photograph's real draw.
//!
//! A headless test with no adapter prints that it was skipped and asserts nothing. It is not GPU
//! evidence: the skip is the report.
use super::super::super::{
    Frame, Layer, PhotoPrimitive, PhotoSurface, Placement, photo_surface, viewport_surface,
};
use super::super::tests::{
    ID, SIDE, diagnostics, headless, held, identity, own_pipeline, paint, plan, scale, settle,
};
use super::*;
use crate::photo_surface::{DrawingPath, GpuBoundary, GpuPlan};
use iced::advanced::{
    Layout, Renderer as _, Shell, Widget, clipboard, image, layout, mouse, renderer,
    widget::Tree,
};
use iced::{Background, Color, Event, Length, Rectangle, Size, Transformation, Vector, window};
use luxforge_reference::srgb;
use std::{any::Any, sync::Arc};

// ---- The clock, without a device --------------------------------------------------------------

#[test]
fn the_share_rises_linearly_over_the_recorded_150_ms() {
    assert_eq!(DISSOLVE_DURATION, Duration::from_millis(150));
    let dissolve = Dissolve::start(4, 9);
    let at = |ms: u64| dissolve.share(dissolve.started + Duration::from_millis(ms));
    assert_eq!(at(0), 0.0);
    assert!((at(75) - 0.5).abs() < 1e-6, "{}", at(75));
    assert!((at(30) - 0.2).abs() < 1e-6, "{}", at(30));
    assert_eq!(at(150), 1.0);
    assert_eq!(at(10_000), 1.0, "and stays whole after it ends");
    let before = dissolve
        .started
        .checked_sub(Duration::from_millis(5))
        .expect("an earlier instant");
    assert_eq!(dissolve.share(before), 0.0, "never before it begins");
}

#[test]
fn a_dissolve_runs_only_into_the_frame_it_names_on_a_whole_photograph() {
    let frame = solid(3, [0, 0, 0, 255]);
    let dissolve = Dissolve::start(7, 3);
    let mid = dissolve.started + Duration::from_millis(75);
    let running = dissolving(Some(dissolve), true, Some(&frame), mid).expect("a running dissolve");
    assert_eq!(running.dissolve, dissolve);
    assert_eq!(
        running.drawn(11),
        DrawnDissolve {
            from: 7,
            to: 3,
            gpu_boundary: 11,
            share: 5000,
        }
    );
    assert_eq!(running.drawn(11).progress(), 0.5);
    assert!(dissolving(None, true, Some(&frame), mid).is_none());
    assert!(
        dissolving(Some(dissolve), false, Some(&frame), mid).is_none(),
        "a plan, a percentage view or a crop stage draws none"
    );
    assert!(
        dissolving(Some(dissolve), true, Some(&solid(4, [0; 4])), mid).is_none(),
        "only into the frame it names"
    );
    assert!(dissolving(Some(dissolve), true, None, mid).is_none());
    assert!(
        dissolving(
            Some(dissolve),
            true,
            Some(&frame),
            dissolve.started + DISSOLVE_DURATION
        )
        .is_none(),
        "an ended dissolve is the frame alone"
    );
}

// ---- The widget's redraws, without a device ---------------------------------------------------

/// A renderer that keeps the photo surface's primitives and draws nothing, so a test reads what
/// the widget would hand the GPU and asks for nothing more.
#[derive(Default)]
struct Recorder {
    drawn: Vec<PhotoPrimitive>,
}

impl iced::advanced::Renderer for Recorder {
    fn start_layer(&mut self, _bounds: Rectangle) {}
    fn end_layer(&mut self) {}
    fn start_transformation(&mut self, _transformation: Transformation) {}
    fn end_transformation(&mut self) {}
    fn fill_quad(&mut self, _quad: renderer::Quad, _background: impl Into<Background>) {}
    fn reset(&mut self, _bounds: Rectangle) {}
    fn allocate_image(
        &mut self,
        _handle: &image::Handle,
        _callback: impl FnOnce(Result<image::Allocation, image::Error>) + Send + 'static,
    ) {
    }
}

impl iced_wgpu::primitive::Renderer for Recorder {
    fn draw_primitive(&mut self, _bounds: Rectangle, primitive: impl iced_wgpu::primitive::Primitive) {
        let primitive: Box<dyn Any> = Box::new(primitive);
        if let Ok(photo) = primitive.downcast::<PhotoPrimitive>() {
            self.drawn.push(*photo);
        }
    }
}

const BOUNDS: Size = Size::new(64.0, 64.0);

/// One redraw as Iced runs it: the redraw event at `now` through the widget's update, then its
/// draw. Returns whether the widget asked for another frame and the primitive it drew.
fn redraw(surface: &mut PhotoSurface, now: Instant) -> (bool, PhotoPrimitive) {
    let node = layout::Node::new(BOUNDS);
    let viewport = Rectangle::new(iced::Point::ORIGIN, BOUNDS);
    let mut tree = Tree::empty();
    let mut renderer = Recorder::default();
    let mut messages: Vec<()> = Vec::new();
    let mut shell = Shell::new(&mut messages);
    Widget::<(), (), Recorder>::update(
        surface,
        &mut tree,
        &Event::Window(window::Event::RedrawRequested(now)),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &renderer,
        &mut clipboard::Null,
        &mut shell,
        &viewport,
    );
    let asked = match shell.redraw_request() {
        window::RedrawRequest::NextFrame => true,
        window::RedrawRequest::Wait => false,
        window::RedrawRequest::At(at) => panic!("a dissolve asks for the next frame, not {at:?}"),
    };
    assert!(messages.is_empty(), "the clock publishes no message");
    renderer.with_layer(viewport, |renderer| {
        Widget::<(), (), Recorder>::draw(
            surface,
            &tree,
            renderer,
            &(),
            &renderer::Style {
                text_color: Color::WHITE,
            },
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &viewport,
        );
    });
    let drawn = renderer.drawn.pop().expect("the photograph's primitive");
    (asked, drawn)
}

fn whole(frame: &Frame) -> PhotoSurface {
    photo_surface(ID, frame, Placement::Contain, Length::Fill, Length::Fill)
}

/// The acceptance check for idle: the widget asks for the next frame on every redraw while the
/// dissolve runs, draws it at that redraw's share, and the redraw that finds it ended asks for
/// nothing and draws the frame alone; with no dissolve no redraw asks for another.
#[test]
fn the_widget_asks_for_redraws_only_while_its_dissolve_runs() {
    let frame = solid(3, [255, 0, 0, 255]);
    let dissolve = Dissolve::start(9, 3);
    let at = |ms: u64| dissolve.started + Duration::from_millis(ms);
    let mut surface = whole(&frame).dissolve(Some(dissolve));
    let mut shares = Vec::new();
    for ms in [0, 16, 33, 75, 133, 149] {
        let (asked, drawn) = redraw(&mut surface, at(ms));
        assert!(asked, "a redraw {ms} ms in asks for the next frame");
        let frame = drawn.dissolve.expect("drawn as a dissolve");
        assert_eq!(frame.dissolve, dissolve);
        shares.push(frame.share);
    }
    assert!(
        shares.windows(2).all(|pair| pair[0] < pair[1]),
        "{shares:?}"
    );
    assert!((shares[3] - 0.5).abs() < 1e-6, "{shares:?}");
    for ms in [150, 151, 400, 60_000] {
        let (asked, drawn) = redraw(&mut surface, at(ms));
        assert!(!asked, "ended {ms} ms in: no further redraw");
        assert!(drawn.dissolve.is_none(), "the frame alone");
    }
    // An idle surface with no dissolve never asks for one either.
    let (asked, drawn) = redraw(&mut whole(&frame), at(30));
    assert!(!asked && drawn.dissolve.is_none());
}

/// An input during a dissolve hands the surface its next GPU plan: the dissolve is cancelled, the
/// plan drawn, and no redraw is asked for on the dissolve's account.
#[test]
fn a_plan_cancels_the_dissolve_and_asks_for_no_redraw() {
    let frame = solid(3, [255, 0, 0, 255]);
    let dissolve = Dissolve::start(9, 3);
    let gpu = plan(&black_boundary(5), vec![identity()]);
    let mut surface = whole(&frame)
        .gpu_preview(Some(&gpu))
        .dissolve(Some(dissolve));
    let (asked, drawn) = redraw(&mut surface, dissolve.started + Duration::from_millis(40));
    assert!(!asked);
    assert!(drawn.dissolve.is_none());
    assert_eq!(
        drawn.gpu.as_ref().map(|plan| plan.boundary.version()),
        Some(5),
        "the plan's GPU frame is drawn"
    );
}

/// A dissolve into a frame other than the one the surface holds, or over a percentage view, is
/// not drawn and keeps nothing awake.
#[test]
fn a_dissolve_into_another_frame_or_a_percentage_view_asks_for_nothing() {
    let frame = solid(3, [255, 0, 0, 255]);
    let mid = Instant::now();
    let (asked, drawn) = redraw(
        &mut whole(&frame).dissolve(Some(Dissolve::start(9, 4))),
        mid,
    );
    assert!(!asked && drawn.dissolve.is_none());
    let mut percent = viewport_surface(
        ID,
        Some((&frame, 1)),
        None,
        1,
        frame.size(),
        Placement::Fill,
        Length::Fixed(BOUNDS.width),
        Length::Fixed(BOUNDS.height),
    )
    .dissolve(Some(Dissolve::start(9, 3)));
    let (asked, drawn) = redraw(&mut percent, mid);
    assert!(!asked && drawn.dissolve.is_none());
}

// ---- On a headless device ---------------------------------------------------------------------

/// A solid `SIDE` × `SIDE` frame of `rgba`, at `version`.
fn solid(version: u64, rgba: [u8; 4]) -> Frame {
    let pixels: Vec<u8> = std::iter::repeat_n(rgba, (SIDE * SIDE) as usize)
        .flatten()
        .collect();
    Frame::new(Arc::new(pixels), SIDE, SIDE, version).expect("a whole frame")
}

/// A boundary whose every texel is linear black but for a full blue channel.
fn black_boundary(version: u64) -> GpuBoundary {
    GpuBoundary::from_linear(
        SIDE,
        SIDE,
        version,
        std::iter::repeat_n([0.0, 0.0, held(255), 1.0], (SIDE * SIDE) as usize),
    )
    .expect("a whole boundary")
}

/// The CPU frame every device test dissolves into: full red.
const CPU_RED: [u8; 4] = [255, 0, 0, 255];
const CPU_VERSION: u64 = 3;

/// Surface `ID`'s photograph over the whole target: the CPU frame, with `plan` or a dissolve at
/// `share` from the GPU frame the caller calls `from`.
fn primitive(plan: Option<GpuPlan>, dissolve: Option<(u64, f32)>) -> PhotoPrimitive {
    PhotoPrimitive {
        surface: ID,
        layers: vec![(Layer::Photo, solid(CPU_VERSION, CPU_RED))],
        viewport: None,
        region_overlays: [None, None],
        gpu: plan,
        dissolve: dissolve.map(|(from, share)| DissolveFrame {
            dissolve: Dissolve::start(from, CPU_VERSION),
            share,
        }),
        offset: Vector::new(0.0, 0.0),
        size: Size::new(SIDE as f32, SIDE as f32),
        clip_size: Size::new(SIDE as f32, SIDE as f32),
        bright: None,
        angle: 0.0,
        snap: true,
    }
}

/// Every pixel of a BGRA readback is `rgb`, to within one code.
fn assert_every_pixel(bytes: &[u8], [r, g, b]: [u8; 3], what: &str) {
    for (index, pixel) in bytes.chunks_exact(4).enumerate() {
        let close = |drawn: u8, wanted: u8| drawn.abs_diff(wanted) <= 1;
        assert!(
            close(pixel[2], r) && close(pixel[1], g) && close(pixel[0], b) && pixel[3] == 255,
            "{what}: pixel {index} is BGRA {pixel:?}, wanted RGB {:?}",
            [r, g, b]
        );
    }
}

/// The codes a linear-light mix of the GPU frame (linear blue) and the CPU frame (linear red) at
/// `share` encodes to, by the independent reference.
fn linear_mix(share: f32) -> [u8; 3] {
    let share = f64::from(share);
    [srgb::code(share), 0, srgb::code(1.0 - share)]
}

/// The acceptance check: after a GPU frame, a dissolve draws the CPU frame over that GPU frame at
/// its share, blended in linear light (a gamma-space blend would be 128, not 188, at one half);
/// both identities, the GPU boundary and the progress are recorded; the slot is kept, charged,
/// for the dissolve and released once it ends, when the CPU frame is drawn alone.
#[test]
fn a_dissolve_blends_the_gpu_frame_into_the_cpu_frame_in_linear_light() {
    let test = "a_dissolve_blends_the_gpu_frame_into_the_cpu_frame_in_linear_light";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let gpu = plan(&black_boundary(5), vec![identity()]);
    let shown = paint(&device, &queue, &mut pipeline, &primitive(Some(gpu), None));
    assert_every_pixel(&shown, [0, 0, 255], "the GPU frame");
    let slot_bytes = diagnostics(&pipeline, ID).gpu_preview_in_use_bytes;
    assert!(slot_bytes > 0);

    for share in [0.25, 0.5, 0.75] {
        let drawn = paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(None, Some((42, share))),
        );
        let wanted = linear_mix(share);
        assert_every_pixel(&drawn, wanted, &format!("the dissolve at {share}"));
        let seen = diagnostics(&pipeline, ID);
        assert_eq!(
            seen.drawn_dissolve,
            Some(DrawnDissolve {
                from: 42,
                to: CPU_VERSION,
                gpu_boundary: 5,
                share: (share * 10_000.0) as u16,
            })
        );
        assert_eq!(
            seen.drawn_path,
            Some(DrawingPath::Cpu),
            "the CPU frame, over the GPU one"
        );
        assert_eq!(seen.drawn_full_version, Some(CPU_VERSION));
        assert_eq!(seen.drawn_gpu_boundary, Some(5));
        assert!(seen.gpu_preview_frame_us.is_some());
        assert_eq!(
            seen.gpu_preview_in_use_bytes, slot_bytes,
            "the slot is kept for the dissolve, and nothing more is allocated"
        );
    }
    assert_eq!(linear_mix(0.5), [188, 0, 188], "not the gamma mix of 128");

    // Ended: the CPU frame alone, and the slot retires.
    let ended = paint(&device, &queue, &mut pipeline, &primitive(None, None));
    assert_every_pixel(&ended, [255, 0, 0], "the CPU frame after the dissolve");
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_dissolve, None);
    assert_eq!(seen.drawn_gpu_boundary, None);
    assert_eq!(seen.gpu_preview_frame_us, None);
    settle(&pipeline);
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_in_use_bytes, 0);
    eprintln!("{test}: linear-light dissolve read back at 0.25, 0.5 and 0.75");
}

/// An input during a dissolve hands the surface a plan again: the dissolve is cancelled and the
/// next GPU frame is drawn alone, from the same slot.
#[test]
fn an_input_mid_dissolve_cancels_it_and_draws_the_next_gpu_frame() {
    let test = "an_input_mid_dissolve_cancels_it_and_draws_the_next_gpu_frame";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let boundary = black_boundary(5);
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(Some(plan(&boundary, vec![scale(1.0)])), None),
    );
    let slot_bytes = diagnostics(&pipeline, ID).gpu_preview_in_use_bytes;
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(None, Some((42, 0.5))),
    );
    assert!(diagnostics(&pipeline, ID).drawn_dissolve.is_some());
    // The next tick: the same boundary at half its blue. The widget hands no dissolve beside a
    // plan; one handed anyway is ignored.
    let next = primitive(Some(plan(&boundary, vec![scale(0.5)])), Some((42, 0.6)));
    let drawn = paint(&device, &queue, &mut pipeline, &next);
    assert_every_pixel(&drawn, [0, 0, srgb::code(0.5)], "the next GPU frame alone");
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_dissolve, None);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert_eq!(seen.drawn_full_version, None, "the CPU frame was not drawn");
    assert_eq!(seen.gpu_preview_in_use_bytes, slot_bytes, "the same slot");
    assert_eq!(seen.gpu_preview_peak_bytes, slot_bytes);
}

/// A dissolve with no GPU frame to start from — none was drawn, or the stage fell back — draws
/// the CPU frame alone and holds nothing.
#[test]
fn with_no_gpu_frame_to_dissolve_from_the_cpu_frame_is_drawn_alone() {
    let test = "with_no_gpu_frame_to_dissolve_from_the_cpu_frame_is_drawn_alone";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let drawn = paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(None, Some((42, 0.5))),
    );
    assert_every_pixel(&drawn, [255, 0, 0], "no GPU frame was shown");
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_dissolve, None);
    assert_eq!(seen.gpu_preview_in_use_bytes, 0);

    // A frame the stage could not draw is the CPU's, so there is nothing to dissolve from either.
    pipeline.set_gpu_budget(1);
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(Some(plan(&black_boundary(5), vec![identity()])), None),
    );
    assert!(diagnostics(&pipeline, ID).gpu_fallback.is_some());
    let drawn = paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(None, Some((42, 0.5))),
    );
    assert_every_pixel(&drawn, [255, 0, 0], "after a fallback");
    assert_eq!(diagnostics(&pipeline, ID).drawn_dissolve, None);
}
