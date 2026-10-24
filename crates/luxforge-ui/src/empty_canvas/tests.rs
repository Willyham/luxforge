use super::*;

/// The clock plays at the frame cadence, eases to a stop once [`AWAKE`] passes with no pointer
/// movement and then asks for nothing; a movement plays it on from the time it stopped at.
#[test]
fn the_art_plays_eases_to_sleep_and_wakes_where_it_stopped() {
    let start = Instant::now();
    let mut clock = Clock::default();
    let mut now = start;
    let mut frames = 0;
    while let Some(next) = clock.advance(now) {
        assert_eq!(next, now + FRAME, "it asks for the next frame a frame away");
        now = next;
        frames += 1;
        assert!(frames < 10_000, "the art never stopped");
    }
    let played = now.duration_since(start);
    assert!(
        played >= AWAKE && played <= AWAKE + EASE + FRAME,
        "it stops within its ease of {AWAKE:?}: {played:?}"
    );
    // Full speed until it begins to ease, then less than full speed through the ease, and at rest
    // it no longer sways.
    let stopped = clock.seconds();
    assert!(stopped > AWAKE.as_secs_f32() && stopped < (AWAKE + EASE).as_secs_f32());
    assert_eq!(clock.sway(), 0.0);

    // Asleep, any other redraw leaves it still and asks for nothing.
    assert_eq!(clock.advance(now + Duration::from_secs(5)), None);
    assert_eq!(clock.seconds(), stopped);

    // The pointer wakes it: it asks to be redrawn, then plays on from where it stopped.
    assert!(clock.wake(), "a sleeping clock needs a redraw to start");
    let woken = now + Duration::from_secs(60);
    assert_eq!(clock.advance(woken), Some(woken + FRAME));
    assert_eq!(clock.seconds(), stopped, "no time passes while it sleeps");
    assert!(
        !clock.wake(),
        "a playing clock already has its next frame asked for"
    );
    let later = clock.advance(woken + FRAME).expect("it plays on");
    assert!(clock.seconds() > stopped);
    assert!(
        clock.sway() > 0.0 && clock.sway() < 1.0,
        "the sway eases back in"
    );
    assert_eq!(later, woken + FRAME * 2);
}

/// A late redraw advances the art by [`MAX_STEP`] at most, so it never jumps.
#[test]
fn a_late_redraw_never_jumps_the_art() {
    let start = Instant::now();
    let mut clock = Clock::default();
    clock.advance(start);
    clock.advance(start + Duration::from_secs(3));
    assert!((clock.seconds() - MAX_STEP.as_secs_f32()).abs() < 1e-6);
}

/// The lockup and the invitation are centred together, top to bottom and inside the canvas, with
/// the brand's clear space around the lockup; a canvas too small for a legible lockup draws the
/// invitation alone.
#[test]
fn the_invitation_sits_under_the_lockup_inside_the_canvas() {
    for size in [
        Size::new(1200.0, 800.0),
        Size::new(700.0, 1000.0),
        Size::new(2400.0, 1400.0),
        Size::new(400.0, 400.0),
    ] {
        let composition = Composition::of(size).expect("room for the lockup");
        let clear = MARK_SHARE * composition.width / 4.0;
        assert!(composition.width <= LOCKUP_MAX && composition.width >= LOCKUP_MIN);
        assert_eq!(
            composition.origin.x + composition.width / 2.0,
            size.width / 2.0
        );
        assert!(composition.origin.x >= clear && composition.origin.y >= clear);
        assert!(composition.invitation_top >= composition.origin.y + composition.height() + clear);
        assert!(composition.invitation_top + INVITATION_HEIGHT <= size.height);
    }
    assert_eq!(Composition::of(Size::new(300.0, 400.0)), None);
    assert_eq!(Composition::of(Size::new(800.0, 160.0)), None);
}

/// At any scale the mark's side is whole pixels and its left edge and baseline lie on pixel
/// edges, within a pixel of where the composition puts the lockup.
#[test]
fn the_mark_lies_on_the_pixel_grid() {
    let composition = Composition::of(Size::new(1013.0, 777.0)).expect("the lockup");
    let [x0, base, side, _] = LOCKUP.mark;
    for scale in [1.0, 1.25, 2.0, 3.0] {
        let (origin, unit) = composition.placement(scale);
        for edge in [side * unit, origin.x + x0 * unit, origin.y + base * unit] {
            assert!((edge - edge.round()).abs() < 1e-3, "{edge} at {scale}×");
        }
        assert!((origin.x - composition.origin.x * scale).abs() <= 1.0);
        assert!((unit * LOCKUP.width - composition.width * scale).abs() <= LOCKUP.width / side);
    }
}

/// The mark the construction places agrees with the brand's drawing of the lockup, and the
/// word's outline lies inside the drawing, under the mark.
#[test]
fn the_lockup_is_the_brand_drawing() {
    let [x0, base, side, radius] = LOCKUP.mark;
    let paths: Vec<&str> = LOCKUP_SVG
        .split(" d=\"")
        .skip(1)
        .map(|tail| tail.split('"').next().expect("a path"))
        .collect();
    let leading = |d: &str, count: usize| -> Vec<f32> {
        d.split(|c: char| c.is_ascii_alphabetic() || c == ',' || c == ' ')
            .filter(|number| !number.is_empty())
            .take(count)
            .map(|number| number.parse().expect("a number"))
            .collect()
    };
    let close = |a: f32, b: f32| assert!((a - b).abs() < 0.01, "{a} against the drawing's {b}");
    // The shadows' quarter: M x0,base V base-radius; the highlights': M x0+side,base H x0+side-radius.
    let shadows = leading(paths[0], 3);
    close(x0, shadows[0]);
    close(base, shadows[1]);
    close(base - radius, shadows[2]);
    let highlights = leading(paths[1], 3);
    close(x0 + side, highlights[0]);
    close(base, highlights[1]);
    close(x0 + side - radius, highlights[2]);

    let word = &LOCKUP.word;
    assert!(word.contours.len() >= 8, "a contour a letter at least");
    for &([left, top, right, bottom], [first, count]) in &word.contours {
        assert!(left >= 0.0 && right <= LOCKUP.width);
        assert!(top > base && bottom <= LOCKUP.height);
        assert!(count > 0 && (first + count) as usize <= word.segments.len());
    }
}

/// The colours are the brand's own, as its drawings of the lockup on dark and on paper fill it.
#[test]
fn the_colours_are_the_brand_drawings() {
    let fills = |svg: &str| -> Vec<Color> {
        svg.split(" fill=\"#")
            .skip(1)
            .map(|tail| {
                let hex = u32::from_str_radix(&tail[..6], 16).expect("a hex colour");
                Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
            })
            .collect()
    };
    assert_eq!(fills(LOCKUP_SVG), [BLUE, CHARTREUSE, LIGHT, BRIGHT]);
    let paper = include_str!("../../../../packaging/brand/lockup/lockup-stacked-paper.svg");
    assert_eq!(
        fills(paper),
        [
            BLUE_ON_PAPER,
            CHARTREUSE_ON_PAPER,
            BLUE_ON_PAPER,
            CHARTREUSE_ON_PAPER,
            INK
        ]
    );
}

/// Dark and mid grounds take the colour drawing, light ones the paper drawing.
#[test]
fn light_grounds_take_the_paper_drawing() {
    for dark in [
        theme::CANVAS,
        theme::CANVAS_BLACK,
        theme::CANVAS_GREY,
        Palette::luxforge_dark().surround,
    ] {
        assert!(!on_paper(dark), "{dark:?}");
    }
    for light in [
        Color::from_rgb8(0xf3, 0xf4, 0xf5),
        Color::from_rgb8(0xfb, 0xf1, 0xc7),
        Color::from_rgb8(0xa0, 0xa0, 0xa0),
    ] {
        assert!(on_paper(light), "{light:?}");
    }
}

/// Lines become one segment each, a contour closes back to its start, a curve's segments stay
/// within the tolerance of it, and a command the outlines do not use is refused.
#[test]
fn an_outline_flattens_into_closed_contours() {
    let square = Outline::flatten("M0 0H10V10L0 10Z").expect("a square");
    assert_eq!(square.segments.len(), 4);
    assert_eq!(square.contours, [([0.0, 0.0, 10.0, 10.0], [0, 4])]);

    let curve = Outline::flatten("M0 0Q50 100 100 0Z").expect("a curve");
    let steps = curve.segments.len() - 1;
    assert!(steps > 4, "the curve is flattened into {steps} steps");
    for &[x0, y0, x1, y1] in &curve.segments[..steps] {
        // The curve is y = 2x(1 - x/100) here; each chord's middle lies within the tolerance.
        let (x, y) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
        let curve_y = 2.0 * x * (1.0 - x / 100.0);
        assert!(
            (curve_y - y).abs() <= TOLERANCE * 1.01,
            "{x}: {y} against {curve_y}"
        );
    }
    assert_eq!(curve.segments.last(), Some(&[100.0, 0.0, 0.0, 0.0]));

    assert!(Outline::flatten("M0 0C1 1 2 2 3 3Z").is_err());
    assert!(Outline::flatten("M0 0L5").is_err());
}

/// The shader parses and validates as wgpu will compile it, with no device, and its blocks are
/// laid out exactly as [`ArtPrimitive::uniforms`] and [`Outline::block`] write them.
#[test]
fn the_shader_validates_and_its_blocks_match_the_uniforms() {
    use wgpu::naga;
    let source = include_str!("../empty_canvas.wgsl");
    let module = naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
    let block = |name: &str| {
        let (_, block) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("the {name} block"));
        let naga::TypeInner::Struct { members, span } = &block.inner else {
            panic!("{name} is a struct");
        };
        let offsets: Vec<u32> = members.iter().map(|member| member.offset).collect();
        (*span as usize, offsets)
    };
    let (span, offsets) = block("Art");
    assert_eq!(span, UNIFORM_BYTES);
    assert_eq!(offsets, [0, 16, 32, 48, 64, 80, 96, 112]);
    let (span, offsets) = block("Word");
    assert_eq!(span, WORD_BYTES);
    assert_eq!(offsets, [0, CONTOURS as u32 * 16, CONTOURS as u32 * 32]);
    assert_eq!(LOCKUP.word.block().len(), WORD_BYTES);
    let primitive = ArtPrimitive {
        size: Size::new(800.0, 600.0),
        composition: Composition::of(Size::new(800.0, 600.0)).expect("the lockup"),
        time: 1.0,
        sway: 1.0,
        ground: Color::BLACK,
        slot: AtomicUsize::new(0),
    };
    assert_eq!(primitive.uniforms(2.0, true).len(), UNIFORM_BYTES);
}

/// The art drawn on this host's GPU through its real pipeline, read back. A host without an
/// adapter prints that it was skipped and asserts nothing; that is not GPU evidence.
mod gpu {
    use super::*;
    use iced::widget::shader::{Pipeline as _, Primitive as _};
    use luxforge_test_macros::slow;
    use luxforge_testbase::wait_for;

    /// A row of the readback is a whole number of wgpu's 256-byte copy rows.
    const WIDTH: u32 = 512;
    const HEIGHT: u32 = 420;
    /// The canvas's fill under the art, as a test target's clear holds it: the dark canvas, and
    /// paper.
    const DARK: [u8; 4] = [0x19, 0x19, 0x1b, 0xff];
    const PAPER: [u8; 4] = [0xf3, 0xf4, 0xf5, 0xff];

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

    /// A device of this host's default adapter, never one of wgpu's OpenGL backend: an instance
    /// of that backend holds an EGL display as long as the device lives, and a thread holding a
    /// clone of the device may drop the last one while the test process exits
    /// (`luxforge_gpu::adapters::open`).
    fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all() - wgpu::Backends::GL,
            ..wgpu::InstanceDescriptor::default()
        });
        let adapter =
            block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    fn colour([r, g, b, _]: [u8; 4]) -> Color {
        Color::from_rgb8(r, g, b)
    }

    fn primitive(ground: [u8; 4], time: f32, sway: f32) -> ArtPrimitive {
        let size = Size::new(WIDTH as f32, HEIGHT as f32);
        ArtPrimitive {
            size,
            composition: Composition::of(size).expect("room for the lockup"),
            time,
            sway,
            ground: colour(ground),
            slot: AtomicUsize::new(0),
        }
    }

    /// Prepare and draw `primitive` as the toolkit does, over a target cleared to its ground, and
    /// read the RGBA codes back.
    fn paint(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &mut ArtPipeline,
        primitive: &ArtPrimitive,
    ) -> Vec<u8> {
        let bounds = Rectangle::new(Point::ORIGIN, primitive.size);
        let viewport = Viewport::with_physical_size(Size::new(WIDTH, HEIGHT), 1.0);
        primitive.prepare(pipeline, device, queue, &bounds, &viewport);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("empty canvas readback target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("empty canvas readback"),
            size: u64::from(WIDTH * HEIGHT * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let ground = primitive.ground;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let linear = |code: f32| luxforge_reference::srgb::decode((code * 255.0).round() as u8);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("empty canvas readback pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: linear(ground.r),
                            g: linear(ground.g),
                            b: linear(ground.b),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
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
                    bytes_per_row: Some(WIDTH * 4),
                    rows_per_image: Some(HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        let index = queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .expect("device wait");
        receiver
            .recv()
            .expect("readback callback")
            .expect("the readback maps");
        let codes = readback.slice(..).get_mapped_range().to_vec();
        readback.unmap();
        pipeline.trim();
        codes
    }

    fn pixel(codes: &[u8], x: u32, y: u32) -> [u8; 4] {
        let at = ((y * WIDTH + x) * 4) as usize;
        [codes[at], codes[at + 1], codes[at + 2], codes[at + 3]]
    }

    /// The code at a point of the mark, given in shares of its side across from its left corner
    /// and of its radius up from its baseline.
    fn mark_pixel(codes: &[u8], primitive: &ArtPrimitive, across: f32, up: f32) -> [u8; 4] {
        let (origin, unit) = primitive.composition.placement(1.0);
        let [x0, base, side, radius] = LOCKUP.mark;
        let x = origin.x + (x0 + across * side) * unit;
        let y = origin.y + (base - up * radius) * unit;
        pixel(codes, x as u32, y as u32)
    }

    fn near(code: [u8; 4], expected: Color) -> bool {
        let expected = [expected.r, expected.g, expected.b].map(|v| (v * 255.0).round() as i32);
        code[..3]
            .iter()
            .zip(expected)
            .all(|(&code, expected)| (i32::from(code) - expected).abs() <= 1)
    }

    /// On the dark canvas the art leaves the ground untouched around the lockup, draws the mark's
    /// shoulders in Blue and Chartreuse with the Light arch between them and the word under them,
    /// is the same frame for the same time, rises as time passes, and at rest is the same mark
    /// whatever its time.
    #[test]
    #[slow]
    fn the_art_draws_the_lockup_over_an_untouched_ground_and_rises() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no adapter; the empty canvas's art drew nothing");
            return;
        };
        let mut pipeline = ArtPipeline::new(&device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb);
        let rest = primitive(DARK, STILL_TIME, 0.0);
        let still = paint(&device, &queue, &mut pipeline, &rest);
        for (x, y) in [
            (0, 0),
            (WIDTH - 1, 0),
            (0, HEIGHT - 1),
            (WIDTH - 1, HEIGHT - 1),
        ] {
            assert_eq!(
                pixel(&still, x, y),
                DARK,
                "the corner ({x}, {y}) is the ground"
            );
        }
        let at = |across, up| mark_pixel(&still, &rest, across, up);
        assert!(
            near(at(0.04, 0.5), BLUE),
            "the shadows' shoulder: {:?}",
            at(0.04, 0.5)
        );
        assert!(
            near(at(0.96, 0.5), CHARTREUSE),
            "the highlights': {:?}",
            at(0.96, 0.5)
        );
        assert!(near(at(0.5, 0.3), LIGHT), "the arch: {:?}", at(0.5, 0.3));
        assert_eq!(
            at(0.5, 0.95),
            DARK,
            "the notch above the arch is the ground"
        );

        let (origin, unit) = rest.composition.placement(1.0);
        let word_top = (origin.y + LOCKUP.mark[1] * unit) as u32 + 1;
        let lettered = (word_top..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| pixel(&still, x, y) != DARK)
            .count();
        assert!(lettered > 800, "the word is drawn: {lettered} pixels");

        let again = paint(&device, &queue, &mut pipeline, &rest);
        assert_eq!(still, again, "the same time draws the same frame");
        let later = paint(&device, &queue, &mut pipeline, &primitive(DARK, 40.0, 0.0));
        assert_eq!(
            still, later,
            "at rest the art is the mark whatever its time"
        );
        let swaying = paint(&device, &queue, &mut pipeline, &primitive(DARK, 40.0, 1.0));
        assert_ne!(still, swaying, "the channels sway as it plays");
        let early = paint(&device, &queue, &mut pipeline, &primitive(DARK, 0.4, 1.0));
        let risen = paint(&device, &queue, &mut pipeline, &primitive(DARK, 0.8, 1.0));
        assert_ne!(early, risen, "the channels rise as its time passes");
        let first = paint(&device, &queue, &mut pipeline, &primitive(DARK, 0.0, 1.0));
        assert!(
            first.chunks(4).all(|code| code == DARK),
            "nothing has risen at its first frame"
        );
    }

    /// On paper the arch is the paper itself, closed by the tonal axis, and the shoulders take
    /// their on-paper fills.
    #[test]
    #[slow]
    fn on_paper_the_arch_is_the_ground() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no adapter; the empty canvas's art drew nothing");
            return;
        };
        let mut pipeline = ArtPipeline::new(&device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb);
        let rest = primitive(PAPER, STILL_TIME, 0.0);
        let codes = paint(&device, &queue, &mut pipeline, &rest);
        let at = |across, up| mark_pixel(&codes, &rest, across, up);
        assert!(near(at(0.04, 0.5), BLUE_ON_PAPER), "{:?}", at(0.04, 0.5));
        assert!(
            near(at(0.96, 0.5), CHARTREUSE_ON_PAPER),
            "{:?}",
            at(0.96, 0.5)
        );
        assert_eq!(at(0.5, 0.3), PAPER, "the arch is the paper");
        assert!(
            near(at(0.3, 0.02), BLUE_ON_PAPER),
            "the axis: {:?}",
            at(0.3, 0.02)
        );
        assert!(
            near(at(0.7, 0.02), CHARTREUSE_ON_PAPER),
            "the axis: {:?}",
            at(0.7, 0.02)
        );
    }
}
