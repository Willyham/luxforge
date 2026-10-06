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
    Layout, Renderer as _, Shell, Widget, clipboard, image, layout, mouse, renderer, widget::Tree,
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
    let version = Some(frame.version());
    let running = dissolving(Some(dissolve), true, version, mid).expect("a running dissolve");
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
    assert!(dissolving(None, true, version, mid).is_none());
    assert!(
        dissolving(Some(dissolve), false, version, mid).is_none(),
        "a plan drawn beside it or a crop stage draws none"
    );
    assert!(
        dissolving(Some(dissolve), true, Some(4), mid).is_none(),
        "only into the frame it names"
    );
    assert!(dissolving(Some(dissolve), true, None, mid).is_none());
    assert!(
        dissolving(
            Some(dissolve),
            true,
            version,
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
    fn draw_primitive(
        &mut self,
        _bounds: Rectangle,
        primitive: impl iced_wgpu::primitive::Primitive,
    ) {
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

/// A plan held behind the CPU frame keeps the dissolve: the settle holds the drawn plan for the
/// next tick, and the dissolve runs and asks for its frames as with no plan.
#[test]
fn a_held_plan_keeps_the_dissolve_and_its_redraws() {
    let frame = solid(3, [255, 0, 0, 255]);
    let dissolve = Dissolve::start(9, 3);
    let gpu = plan(&black_boundary(5), vec![identity()]);
    let mut surface = whole(&frame)
        .gpu_preview(Some(&gpu))
        .gpu_hold(true)
        .dissolve(Some(dissolve));
    let (asked, drawn) = redraw(&mut surface, dissolve.started + Duration::from_millis(40));
    assert!(asked, "a running dissolve asks for the next frame");
    assert!(drawn.dissolve.is_some() && drawn.gpu_options.hold);
    let (asked, drawn) = redraw(&mut surface, dissolve.started + DISSOLVE_DURATION);
    assert!(
        !asked && drawn.dissolve.is_none(),
        "and nothing once it ends"
    );
}

/// A dissolve into a frame other than the one the surface draws is not drawn and keeps nothing
/// awake. A percentage view dissolves into the whole frame it draws its current content from, and
/// into no other.
#[test]
fn a_dissolve_into_another_frame_asks_for_nothing_and_a_percentage_view_into_its_own() {
    let frame = solid(3, [255, 0, 0, 255]);
    let mid = Instant::now();
    let (asked, drawn) = redraw(
        &mut whole(&frame).dissolve(Some(Dissolve::start(9, 4))),
        mid,
    );
    assert!(!asked && drawn.dissolve.is_none());
    let percent = |full: Option<(&Frame, u64)>, to: u64| {
        viewport_surface(
            ID,
            full,
            1,
            STAGE,
            Placement::Fill,
            Length::Fixed(BOUNDS.width),
            Length::Fixed(BOUNDS.height),
        )
        .dissolve(Some(Dissolve::start(9, to)))
    };
    for (what, full, to, runs) in [
        ("its whole frame", Some((&frame, 1)), 3, true),
        (
            "a whole frame of another content",
            Some((&frame, 0)),
            3,
            false,
        ),
        ("another version", Some((&frame, 1)), 6, false),
        ("another frame", None, 4, false),
    ] {
        let (asked, drawn) = redraw(&mut percent(full, to), mid);
        assert_eq!(asked, runs, "{what}");
        assert_eq!(drawn.dissolve.is_some(), runs, "{what}");
    }
}

/// While its last frame said the plan's boundary is still uploading, the widget asks for the next
/// frame, whose `prepare` writes the next chunks; once it is not — drawn, another fallback, or a
/// boundary derived from the source, which no boundary upload is for — it asks for nothing, so an
/// idle editor stays asleep.
#[test]
fn the_widget_asks_for_frames_only_while_its_plans_boundary_uploads() {
    use super::super::super::{SurfaceDiagnostics, SurfaceId, process_figures};
    let id = SurfaceId::new(901);
    let report = |fallback: Option<crate::photo_surface::GpuFallback>| {
        process_figures()
            .draws
            .lock()
            .expect("surface draw identities lock")
            .insert(
                id,
                SurfaceDiagnostics {
                    gpu_fallback: fallback,
                    ..Default::default()
                },
            );
    };
    let frame = solid(3, [255, 0, 0, 255]);
    let boundary = black_boundary(5);
    let held = plan(&boundary, vec![identity()]);
    let source =
        crate::photo_surface::GpuSource::codes(1, Arc::new(vec![0u8; 16]), 2, 2).expect("a source");
    let derived = plan(
        &GpuBoundary::derived(
            &source,
            crate::photo_surface::Derivation::Cut { origin: (0, 0) },
            2,
            2,
            6,
        )
        .expect("a derived boundary"),
        vec![identity()],
    );
    let now = Instant::now();
    let surface = |plan: &GpuPlan| {
        photo_surface(id, &frame, Placement::Contain, Length::Fill, Length::Fill)
            .gpu_preview(Some(plan))
    };
    let uploading = crate::photo_surface::GpuFallback::BoundaryUploading {
        uploaded: 1,
        bytes: 2,
    };
    for (what, fallback, plan, asks) in [
        ("drawn", None, &held, false),
        ("uploading", Some(uploading), &held, true),
        ("a derived boundary", Some(uploading), &derived, false),
        (
            "compiling",
            Some(crate::photo_surface::GpuFallback::Compiling),
            &held,
            false,
        ),
    ] {
        report(fallback);
        let (asked, _) = redraw(&mut surface(plan), now);
        assert_eq!(asked, asks, "{what}");
    }
    process_figures()
        .draws
        .lock()
        .expect("surface draw identities lock")
        .remove(&id);
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
        crate::photo_surface::BoundaryFormat::Half,
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
        gpu_options: Default::default(),
        dissolve: dissolve.map(|(from, share)| DissolveFrame {
            dissolve: Dissolve::start(from, CPU_VERSION),
            share,
        }),
        source: None,
        rest: None,
        offset: Vector::new(0.0, 0.0),
        size: Size::new(SIDE as f32, SIDE as f32),
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

/// A settle that keeps the plan held behind the CPU frame dissolves from the GPU frame the slot
/// holds: the held plan stays ready for the next tick, the dissolve's end leaves the CPU frame
/// with the slot still held, and a tick drawn on the GPU mid-dissolve cancels it.
#[test]
fn a_dissolve_runs_behind_a_held_plan_and_a_drawn_one_cancels_it() {
    let test = "a_dissolve_runs_behind_a_held_plan_and_a_drawn_one_cancels_it";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let boundary = black_boundary(5);
    let held = |share: Option<f32>| {
        let mut primitive = primitive(
            Some(plan(&boundary, vec![identity()])),
            share.map(|share| (42, share)),
        );
        primitive.gpu_options.hold = true;
        primitive
    };
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(Some(plan(&boundary, vec![identity()])), None),
    );
    let slot_bytes = diagnostics(&pipeline, ID).gpu_preview_in_use_bytes;

    let drawn = paint(&device, &queue, &mut pipeline, &held(Some(0.5)));
    assert_every_pixel(&drawn, linear_mix(0.5), "the dissolve behind the held plan");
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(
        seen.drawn_dissolve
            .map(|dissolve| (dissolve.from, dissolve.to, dissolve.gpu_boundary)),
        Some((42, CPU_VERSION, 5))
    );
    assert_eq!(seen.drawn_path, Some(DrawingPath::Cpu));
    assert_eq!(
        seen.gpu_ready_boundary,
        Some(5),
        "the held plan stays ready for the next tick"
    );

    // Ended: the CPU frame alone, the slot still held for the open draft.
    let ended = paint(&device, &queue, &mut pipeline, &held(None));
    assert_every_pixel(&ended, [255, 0, 0], "the CPU frame once it ends");
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_dissolve, None);
    assert_eq!(seen.gpu_preview_in_use_bytes, slot_bytes);

    // Again, cancelled by the next tick drawn on the GPU.
    paint(&device, &queue, &mut pipeline, &held(Some(0.25)));
    assert!(
        diagnostics(&pipeline, ID).drawn_dissolve.is_none(),
        "the CPU frame was drawn last, so there is no GPU frame to dissolve from"
    );
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(Some(plan(&boundary, vec![identity()])), None),
    );
    paint(&device, &queue, &mut pipeline, &held(Some(0.25)));
    assert!(diagnostics(&pipeline, ID).drawn_dissolve.is_some());
    let next = primitive(Some(plan(&boundary, vec![scale(0.5)])), Some((42, 0.3)));
    let drawn = paint(&device, &queue, &mut pipeline, &next);
    assert_every_pixel(
        &drawn,
        [0, 0, srgb::code(0.5)],
        "the next tick, drawn alone",
    );
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_dissolve, None);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
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

// ---- At a percentage zoom, on a headless device ----------------------------------------------

/// The 128 × 128 stage a percentage view shows the middle 64 × 64 of at 100%.
const STAGE: (u32, u32) = (2 * SIDE, 2 * SIDE);
/// That middle, `[x0, y0, x1, y1]`.
const MIDDLE: [u32; 4] = [SIDE / 2, SIDE / 2, SIDE / 2 + SIDE, SIDE / 2 + SIDE];

/// The CPU's whole frame of the stage, full red, at `version`.
fn whole_red(version: u64) -> Frame {
    let pixels: Vec<u8> = std::iter::repeat_n(CPU_RED, (STAGE.0 * STAGE.1) as usize)
        .flatten()
        .collect();
    Frame::new(Arc::new(pixels), STAGE.0, STAGE.1, version).expect("a whole frame")
}

/// The GPU plan of the middle region: the blue boundary held for it, drawn at its rectangle.
fn region_plan() -> GpuPlan {
    let mut region = plan(&black_boundary(5), vec![identity()]);
    region.texels.origin = [MIDDLE[0] as f32, MIDDLE[1] as f32];
    region.region = Some(crate::photo_surface::GpuRegion {
        rect: MIDDLE,
        stage: STAGE,
        full_stage: STAGE,
    });
    region
}

/// Surface `ID`'s percentage view of the stage at 100%, panned to the middle, which fills the
/// target: the CPU's whole frame of the stage, with `plan` or a dissolve at `share`.
fn percent(plan: Option<GpuPlan>, dissolve: Option<(u64, f32)>, hold: bool) -> PhotoPrimitive {
    let mut primitive = primitive(plan, None);
    primitive.layers = Vec::new();
    primitive.viewport = Some(super::super::super::ViewportFrames {
        full: Some((whole_red(CPU_VERSION), 1)),
        current_content: 1,
        full_stage: STAGE,
    });
    primitive.offset = Vector::new(-(MIDDLE[0] as f32), -(MIDDLE[1] as f32));
    primitive.size = Size::new(STAGE.0 as f32, STAGE.1 as f32);
    primitive.gpu_options.hold = hold;
    primitive.dissolve = dissolve.map(|(from, share)| DissolveFrame {
        dissolve: Dissolve::start(from, CPU_VERSION),
        share,
    });
    primitive
}

/// At a percentage zoom, after the GPU frame of the visible region, a dissolve draws the view's
/// whole frame of the settled content over that GPU frame at its share, in linear light, records
/// both identities and keeps the slot until it ends; behind a held plan as with none. Once it ends
/// the whole frame is drawn alone and the slot retires.
#[test]
fn a_percentage_view_dissolves_its_gpu_region_into_its_whole_frame() {
    let test = "a_percentage_view_dissolves_its_gpu_region_into_its_whole_frame";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let shown = paint(
        &device,
        &queue,
        &mut pipeline,
        &percent(Some(region_plan()), None, false),
    );
    assert_every_pixel(&shown, [0, 0, 255], "the GPU region frame");
    assert_eq!(
        diagnostics(&pipeline, ID).drawn_path,
        Some(DrawingPath::Gpu)
    );
    let slot_bytes = diagnostics(&pipeline, ID).gpu_preview_in_use_bytes;
    for (share, plan, hold) in [
        (0.25, None, false),
        (0.5, Some(region_plan()), true),
        (0.75, None, false),
    ] {
        let drawn = paint(
            &device,
            &queue,
            &mut pipeline,
            &percent(plan, Some((42, share)), hold),
        );
        assert_every_pixel(
            &drawn,
            linear_mix(share),
            &format!("the dissolve at {share}"),
        );
        let seen = diagnostics(&pipeline, ID);
        assert_eq!(
            seen.drawn_dissolve,
            Some(DrawnDissolve {
                from: 42,
                to: CPU_VERSION,
                gpu_boundary: 5,
                share: (share * 10_000.0) as u16,
            }),
            "at {share}"
        );
        assert_eq!(seen.drawn_path, Some(DrawingPath::Cpu));
        assert_eq!(seen.drawn_full_version, Some(CPU_VERSION));
        assert_eq!(seen.gpu_preview_in_use_bytes, slot_bytes);
    }
    let ended = paint(&device, &queue, &mut pipeline, &percent(None, None, false));
    assert_every_pixel(&ended, [255, 0, 0], "the CPU frame after the dissolve");
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_dissolve, None);
    assert_eq!(seen.drawn_gpu_boundary, None);
    settle(&pipeline);
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_in_use_bytes, 0);
}

/// A percentage view whose last GPU frame was a whole frame's — none of its own region — draws its
/// whole frame alone, as does one whose dissolve names a frame it has not drawn yet.
#[test]
fn a_percentage_view_dissolves_only_from_its_own_region_into_a_frame_it_holds() {
    let test = "a_percentage_view_dissolves_only_from_its_own_region_into_a_frame_it_holds";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    // A whole frame's GPU output, at Fit.
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(Some(plan(&black_boundary(5), vec![identity()])), None),
    );
    let drawn = paint(
        &device,
        &queue,
        &mut pipeline,
        &percent(None, Some((42, 0.5)), false),
    );
    assert_every_pixel(&drawn, [255, 0, 0], "no region GPU frame to dissolve from");
    assert_eq!(diagnostics(&pipeline, ID).drawn_dissolve, None);
    // The region's GPU frame, then a dissolve into a version the view does not hold.
    paint(
        &device,
        &queue,
        &mut pipeline,
        &percent(Some(region_plan()), None, false),
    );
    let mut elsewhere = percent(None, Some((42, 0.5)), false);
    if let Some(frame) = &mut elsewhere.dissolve {
        frame.dissolve.to = CPU_VERSION + 1;
    }
    let drawn = paint(&device, &queue, &mut pipeline, &elsewhere);
    assert_every_pixel(&drawn, [255, 0, 0], "not into a frame it does not hold");
    assert_eq!(diagnostics(&pipeline, ID).drawn_dissolve, None);
}
