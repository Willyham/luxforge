//! The empty Develop canvas: the invitation to open a photograph under a piece of art, drawn by one
//! shader of its own (`empty_canvas.wgsl`) in the theme's colours.
//!
//! The art is a lens seen from the front, stopping down through f/1.4 to f/8 and back, a tunnel of
//! film rushing away through its aperture, bokeh the aperture's own shape drifting behind it and the
//! word LUXFORGE in pixel letters under it, which a glint crosses every few seconds. Each 2 pt art pixel is an ordered-dithered level of one of two ramps,
//! one towards the theme's brightest text and one towards its accent, with the canvas's own fill at
//! the ground level, which the shader leaves transparent so the fill shows through exactly.
//!
//! - **Bounded motion.** The art's clock is the widget's own, advanced by the redraws it asks for,
//!   about thirty a second, so it costs the editor no message and no view rebuild. It plays while
//!   the pointer has moved over the canvas within [`AWAKE`], then eases to a stop over [`EASE`] and
//!   asks for nothing more; the next pointer movement over it eases it back in from where it
//!   stopped. Idle means asleep.
//! - **Held still.** A launch that draws its window on a software adapter or none, an evidence run
//!   and a hidden launch hold the art still for the life of the process ([`hold_still`]): it draws
//!   one composed frame and never asks for another.
use crate::{Element, Palette, Theme, Token, theme};
use iced::{
    Color, Event, Length, Point, Rectangle, Size,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, mouse, renderer,
        widget::{Tree, tree},
    },
    widget::{
        container, responsive,
        shader::{self, Viewport},
        stack, text,
    },
    window,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// How often the art redraws while it plays: about thirty times a second, enough for the motion to
/// read as smooth without redrawing the window at the display's rate.
pub const FRAME: Duration = Duration::from_millis(33);

/// How long the art plays after the pointer last moved over the canvas, or after it first appears.
pub const AWAKE: Duration = Duration::from_secs(30);

/// How long the art takes to ease from full speed to a stop, and back.
pub const EASE: Duration = Duration::from_millis(1500);

/// The longest step one redraw advances the art by, so a late frame never jumps it.
const MAX_STEP: Duration = Duration::from_millis(100);

/// The art's time in a frame held still: the aperture half closed, no glint on the word.
const STILL_TIME: f32 = 7.6;

/// The size of one art pixel, in logical pixels.
const ART_PIXEL: f32 = 2.0;

/// The gap between the lens and the word under it.
const WORD_GAP: f32 = 26.0;

/// The word's height in font pixels, its drop shadow's row included, and the most art pixels a
/// side of a font pixel.
const WORD_ROWS: f32 = 8.0;
const FONT_MAX: f32 = 3.0;

/// The word's width in font pixels: five a letter and one between letters.
const WORD_COLUMNS: f32 = 47.0;

/// The lens radius each art pixel a side of the word's font pixels needs: the word is never wider
/// than the lens.
const RADIUS_PER_FONT_PIXEL: f32 = 60.0;

/// The invitation's line, about [`theme::SIZE_TITLE`] high, and the gap between it and the word.
const INVITATION_HEIGHT: f32 = 18.0;
const INVITATION_GAP: f32 = 22.0;

/// The lens's radius as a share of the room the canvas gives it, and its bounds.
const LENS_SHARE: f32 = 0.3;
const LENS_MAX: f32 = 230.0;
const LENS_MIN: f32 = 48.0;

static STILL: AtomicBool = AtomicBool::new(false);

/// Hold the empty canvas's art still for the life of the process: it draws one composed frame and
/// asks for no redraw. Call it once at launch, before the window opens, for a launch whose window
/// is not drawn by a hardware GPU, and for evidence and hidden launches.
pub fn hold_still() {
    STILL.store(true, Ordering::Release);
}

/// Whether the process holds the art still ([`hold_still`]).
pub fn held_still() -> bool {
    STILL.load(Ordering::Acquire)
}

/// The empty canvas: the art, filling the canvas, with `invitation` centred under its lens. The
/// art's ground is `ground`, resolved from the palette the canvas is drawn with, and must be the
/// fill the canvas region draws under it.
pub fn empty_canvas<'a, Message: 'a>(
    invitation: &'a str,
    ground: impl Fn(&Palette) -> Color + Copy + 'a,
) -> Element<'a, Message> {
    responsive(move |size| {
        let line = text(invitation)
            .size(theme::SIZE_TITLE)
            .style(theme::ink(Token::TextSecondary));
        let Some(composition) = Composition::of(size) else {
            return container(line).center(Length::Fill).into();
        };
        let invitation = container(line)
            .width(Length::Fill)
            .padding(iced::Padding {
                top: composition.invitation_top,
                ..iced::Padding::ZERO
            })
            .align_x(iced::alignment::Horizontal::Center);
        stack![
            Element::new(Art {
                ground,
                composition,
                still: held_still(),
            }),
            invitation,
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    })
    .into()
}

/// Where the lens, the word and the invitation sit in a canvas, in its logical pixels: the three
/// centred together, top to bottom, the lens a share of the room the other two leave.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Composition {
    pub centre: Point,
    pub radius: f32,
    /// The top of the word.
    pub word_top: f32,
    /// The art pixels a side of one of the word's font pixels.
    pub font: f32,
    /// The top of the invitation's line.
    pub invitation_top: f32,
}

impl Composition {
    /// The composition in a canvas of `size`, or `None` when the lens would be too small to read,
    /// where the invitation is drawn alone.
    pub fn of(size: Size) -> Option<Self> {
        let below = |font: f32| {
            WORD_GAP + WORD_ROWS * font * ART_PIXEL + INVITATION_GAP + INVITATION_HEIGHT
        };
        let room = size.width.min(size.height - below(FONT_MAX));
        let radius = (room * LENS_SHARE).min(LENS_MAX);
        if radius < LENS_MIN {
            return None;
        }
        let font = (radius / RADIUS_PER_FONT_PIXEL)
            .floor()
            .clamp(1.0, FONT_MAX);
        let top = ((size.height - 2.0 * radius - below(font)) / 2.0).max(0.0);
        let word_top = top + 2.0 * radius + WORD_GAP;
        Some(Self {
            centre: Point::new(size.width / 2.0, top + radius),
            radius,
            word_top,
            font,
            invitation_top: word_top + WORD_ROWS * font * ART_PIXEL + INVITATION_GAP,
        })
    }
}

/// The art's clock: the time it has played, advanced by each redraw at the speed it plays at, which
/// eases to zero once [`AWAKE`] passes with no pointer movement over the canvas.
#[derive(Debug)]
struct Clock {
    /// The art's time, in seconds.
    time: f64,
    /// The share of full speed it plays at.
    speed: f32,
    /// The redraw that last advanced it, while it plays; `None` once it has stopped.
    last: Option<Instant>,
    /// When it starts to ease to a stop; `None` before its first redraw.
    until: Option<Instant>,
    /// The pointer moved over the canvas since the last redraw.
    woken: bool,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            time: 0.0,
            speed: 1.0,
            last: None,
            until: None,
            woken: false,
        }
    }
}

impl Clock {
    /// Advance to the redraw at `now`: the time the next redraw is wanted at, or `None` once the
    /// art has eased to a stop.
    fn advance(&mut self, now: Instant) -> Option<Instant> {
        if self.woken || self.until.is_none() {
            self.until = Some(now + AWAKE);
            self.woken = false;
        }
        let step = self.last.map_or(0.0, |last| {
            now.saturating_duration_since(last)
                .min(MAX_STEP)
                .as_secs_f32()
        });
        let playing = self.until.is_some_and(|until| now < until);
        let target = if playing { 1.0 } else { 0.0 };
        let reach = step / EASE.as_secs_f32();
        let speed = if target > self.speed {
            (self.speed + reach).min(target)
        } else {
            (self.speed - reach).max(target)
        };
        self.time += f64::from(step * (self.speed + speed) * 0.5);
        self.speed = speed;
        if speed == 0.0 && !playing {
            self.last = None;
            return None;
        }
        self.last = Some(now);
        Some(now + FRAME)
    }

    /// The pointer moved over the canvas: play on. Whether the art had stopped, so a redraw must be
    /// asked for to start it again.
    fn wake(&mut self) -> bool {
        self.woken = true;
        self.last.is_none()
    }

    /// The art's time, as the shader reads it.
    fn seconds(&self) -> f32 {
        self.time as f32
    }
}

/// The art itself, filling its bounds.
struct Art<G> {
    ground: G,
    composition: Composition,
    still: bool,
}

impl<Message, Renderer, G> Widget<Message, Theme, Renderer> for Art<G>
where
    Renderer: iced_wgpu::primitive::Renderer,
    G: Fn(&Palette) -> Color,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Clock>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Clock::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::atomic(limits, Length::Fill, Length::Fill)
    }

    /// Each redraw's own time advances the clock, which asks for the next while the art plays;
    /// the pointer moving over the art plays it on. It never captures an event.
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if self.still {
            return;
        }
        let clock = tree.state.downcast_mut::<Clock>();
        match event {
            Event::Window(window::Event::RedrawRequested(now)) => {
                if let Some(next) = clock.advance(*now) {
                    shell.request_redraw_at(window::RedrawRequest::At(next));
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. })
                if cursor.is_over(layout.bounds()) && clock.wake() =>
            {
                shell.request_redraw();
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let palette = theme.palette();
        let time = if self.still {
            STILL_TIME
        } else {
            tree.state.downcast_ref::<Clock>().seconds()
        };
        let bounds = layout.bounds();
        renderer.draw_primitive(
            bounds,
            ArtPrimitive {
                size: bounds.size(),
                composition: self.composition,
                time,
                ground: (self.ground)(palette),
                accent: palette.accent,
                ink: palette.text_bright,
                slot: AtomicUsize::new(0),
            },
        );
    }
}

impl<'a, Message: 'a, G: Fn(&Palette) -> Color + 'a> From<Art<G>> for Element<'a, Message> {
    fn from(art: Art<G>) -> Self {
        Element::new(art)
    }
}

/// The word under the lens, letter by letter: each letter's 5×7 rows, top to bottom, separated by
/// spaces, `#` lit.
const WORD: [(u8, &str); 8] = [
    (b'L', "#.... #.... #.... #.... #.... #.... #####"),
    (b'U', "#...# #...# #...# #...# #...# #...# .###."),
    (b'X', "#...# #...# .#.#. ..#.. .#.#. #...# #...#"),
    (b'F', "##### #.... #.... ####. #.... #.... #...."),
    (b'O', ".###. #...# #...# #...# #...# #...# .###."),
    (b'R', "####. #...# #...# ####. #.#.. #..#. #...#"),
    (b'G', ".###. #...# #.... #.### #...# #...# .####"),
    (b'E', "##### #.... #.... ####. #.... #.... #####"),
];

/// The word as the shader reads it: two words a letter, two letters a vector.
const LETTERS: [[u32; 4]; 4] = letters();

const fn letters() -> [[u32; 4]; 4] {
    let mut words = [[0u32; 4]; 4];
    let mut letter = 0;
    while letter < WORD.len() {
        // Six bytes a row: its five pixels and the space before the next.
        let bits = WORD[letter].1.as_bytes();
        assert!(bits.len() == 7 * 6 - 1, "a letter is seven rows of five");
        let mut at = 0;
        while at < bits.len() {
            let (row, column) = (at / 6, at % 6);
            if bits[at] == b'#' {
                let word = letter % 2 * 2 + row / 4;
                words[letter / 2][word] |= 1 << ((row % 4) * 5 + column);
            }
            at += 1;
        }
        letter += 1;
    }
    words
}

/// One frame of the art: what its uniform block is made from once the scale factor is known.
#[derive(Debug)]
struct ArtPrimitive {
    size: Size,
    composition: Composition,
    time: f32,
    ground: Color,
    accent: Color,
    ink: Color,
    /// The pipeline's uniform slot `prepare` wrote this frame's block into, which `draw` binds.
    slot: AtomicUsize,
}

/// The uniform block's size: six vectors and the word's four.
const UNIFORM_BYTES: usize = (6 + 4) * 16;

/// The most art primitives one frame draws: one per window's canvas, with room to spare.
const SLOTS: usize = 4;

impl ArtPrimitive {
    /// The uniform block, as `empty_canvas.wgsl`'s `Art` lays it out, at `scale` physical pixels a
    /// logical one, for a target that stores linear light when `linear`.
    fn uniforms(&self, scale: f32, linear: bool) -> Vec<u8> {
        let cell = (ART_PIXEL * scale).round().max(1.0);
        let composition = self.composition;
        // The word centred under the lens, on the art pixels' grid.
        let word = [
            (composition.centre.x * scale / cell - WORD_COLUMNS * composition.font / 2.0).round(),
            (composition.word_top * scale / cell).round(),
            composition.font,
            0.0,
        ];
        let colour = |colour: Color| [colour.r, colour.g, colour.b, colour.a];
        let vectors: [[f32; 4]; 6] = [
            [
                self.size.width * scale,
                self.size.height * scale,
                cell,
                if linear { 1.0 } else { 0.0 },
            ],
            [
                composition.centre.x * scale,
                composition.centre.y * scale,
                composition.radius * scale,
                self.time,
            ],
            word,
            colour(self.ground),
            colour(self.accent),
            colour(self.ink),
        ];
        let mut bytes = Vec::with_capacity(UNIFORM_BYTES);
        for value in vectors.iter().flatten() {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for word in LETTERS.iter().flatten() {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }
}

impl shader::Primitive for ArtPrimitive {
    type Pipeline = ArtPipeline;

    fn prepare(
        &self,
        pipeline: &mut ArtPipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let slot = pipeline.used.min(SLOTS - 1);
        pipeline.used += 1;
        while pipeline.slots.len() <= slot {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("luxforge.empty_canvas.uniforms"),
                size: UNIFORM_BYTES as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("luxforge.empty_canvas.group"),
                layout: &pipeline.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
            pipeline.slots.push((buffer, group));
        }
        let uniforms = self.uniforms(viewport.scale_factor(), pipeline.linear);
        queue.write_buffer(&pipeline.slots[slot].0, 0, &uniforms);
        self.slot.store(slot, Ordering::Relaxed);
    }

    fn draw(&self, pipeline: &ArtPipeline, render_pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some((_, group)) = pipeline.slots.get(self.slot.load(Ordering::Relaxed)) else {
            return true;
        };
        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_bind_group(0, group, &[]);
        render_pass.draw(0..3, 0..1);
        true
    }
}

/// The art's pipeline: its program and up to [`SLOTS`] uniform blocks, one per primitive a frame
/// draws, reused from frame to frame.
struct ArtPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    slots: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    /// The slots this frame has prepared.
    used: usize,
    /// The target stores linear light: the toolkit chose an sRGB format, so it gamma corrects.
    linear: bool,
}

impl shader::Pipeline for ArtPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("luxforge.empty_canvas.layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("luxforge.empty_canvas.shader"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "empty_canvas.wgsl"
            ))),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("luxforge.empty_canvas.pipeline_layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("luxforge.empty_canvas.pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Every art pixel is opaque or, on the ground, transparent: the canvas's fill
                    // shows through the ground untouched.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        Self {
            pipeline,
            layout,
            slots: Vec::new(),
            used: 0,
            linear: format.is_srgb(),
        }
    }

    /// The end of a frame: the next frame's primitives take the slots from the first again.
    fn trim(&mut self) {
        self.used = 0;
    }
}

#[cfg(test)]
mod tests {
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
        // Full speed until it begins to ease, then less than full speed through the ease.
        let stopped = clock.seconds();
        assert!(stopped > AWAKE.as_secs_f32() && stopped < (AWAKE + EASE).as_secs_f32());

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

    /// The lens, the word and the invitation are centred together, top to bottom and inside the
    /// canvas, the word no wider than the lens; a canvas too small for a lens draws the invitation
    /// alone.
    #[test]
    fn the_word_and_the_invitation_sit_under_the_lens_inside_the_canvas() {
        for size in [
            Size::new(1200.0, 800.0),
            Size::new(700.0, 1000.0),
            Size::new(2400.0, 1400.0),
            Size::new(260.0, 300.0),
        ] {
            let composition = Composition::of(size).expect("room for a lens");
            assert_eq!(composition.centre.x, size.width / 2.0);
            assert!(composition.radius <= LENS_MAX && composition.radius >= LENS_MIN);
            assert!(composition.centre.y - composition.radius >= 0.0);
            let word_height = WORD_ROWS * composition.font * ART_PIXEL;
            let word_width = WORD_COLUMNS * composition.font * ART_PIXEL;
            assert!(composition.word_top > composition.centre.y + composition.radius);
            assert!(word_width < 2.0 * composition.radius);
            assert!(composition.invitation_top > composition.word_top + word_height);
            assert!(composition.invitation_top + INVITATION_HEIGHT <= size.height);
        }
        assert_eq!(Composition::of(Size::new(120.0, 400.0)), None);
        assert_eq!(Composition::of(Size::new(800.0, 160.0)), None);
    }

    /// The shader parses and validates as wgpu will compile it, with no device, and its uniform
    /// block is laid out exactly as [`ArtPrimitive::uniforms`] writes it.
    #[test]
    fn the_shader_validates_and_its_block_matches_the_uniforms() {
        use wgpu::naga;
        let source = include_str!("empty_canvas.wgsl");
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{}", error.emit_to_string(source)));
        let (_, block) = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some("Art"))
            .expect("the Art block");
        let naga::TypeInner::Struct { members, span } = &block.inner else {
            panic!("Art is a struct");
        };
        assert_eq!(*span as usize, UNIFORM_BYTES);
        let offsets: Vec<u32> = members.iter().map(|member| member.offset).collect();
        assert_eq!(offsets, [0, 16, 32, 48, 64, 80, 96]);
        let primitive = ArtPrimitive {
            size: Size::new(800.0, 600.0),
            composition: Composition::of(Size::new(800.0, 600.0)).expect("a lens"),
            time: 1.0,
            ground: Color::BLACK,
            accent: Color::WHITE,
            ink: Color::WHITE,
            slot: AtomicUsize::new(0),
        };
        assert_eq!(primitive.uniforms(2.0, true).len(), UNIFORM_BYTES);
    }

    /// The word spells LUXFORGE, and each letter is packed row by row as drawn: `G`'s fourth row
    /// is `#.###`.
    #[test]
    fn the_word_spells_luxforge_packed_row_by_row() {
        assert_eq!(WORD.map(|(letter, _)| letter), *b"LUXFORGE");
        let g = WORD.iter().position(|(c, _)| *c == b'G').expect("a G");
        let [low, high] = [LETTERS[g / 2][g % 2 * 2], LETTERS[g / 2][g % 2 * 2 + 1]];
        assert_eq!(low & 0b11111, 0b01110, "the top row: .###.");
        assert_eq!((low >> 15) & 0b11111, 0b11101, "the fourth row: #.###");
        assert_eq!((high >> 10) & 0b11111, 0b11110, "the last row: .####");
    }
}

/// The art drawn on this host's GPU through its real pipeline, read back. A host without an
/// adapter prints that it was skipped and asserts nothing; that is not GPU evidence.
#[cfg(test)]
mod gpu_tests {
    use super::*;
    use iced::widget::shader::{Pipeline as _, Primitive as _};
    use luxforge_test_macros::slow;
    use luxforge_testbase::wait_for;

    const WIDTH: u32 = 320;
    const HEIGHT: u32 = 300;
    /// The canvas's fill under the art, as a test target's clear holds it.
    const GROUND: [u8; 4] = [0x19, 0x19, 0x1b, 0xff];

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

    fn headless() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter =
            block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
    }

    fn primitive(time: f32) -> ArtPrimitive {
        sized(Size::new(WIDTH as f32, HEIGHT as f32), time)
    }

    fn sized(size: Size, time: f32) -> ArtPrimitive {
        let palette = Palette::luxforge_dark();
        ArtPrimitive {
            size,
            composition: Composition::of(size).expect("room for a lens"),
            time,
            ground: palette.surround,
            accent: palette.accent,
            ink: palette.text_bright,
            slot: AtomicUsize::new(0),
        }
    }

    /// Prepare and draw `primitive` as the toolkit does, over a target cleared to the ground, and
    /// read the RGBA codes back.
    fn paint(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &mut ArtPipeline,
        primitive: &ArtPrimitive,
    ) -> Vec<u8> {
        paint_at(device, queue, pipeline, primitive, 1.0)
    }

    /// [`paint`] at `scale` physical pixels a logical one.
    fn paint_at(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &mut ArtPipeline,
        primitive: &ArtPrimitive,
        scale: f32,
    ) -> Vec<u8> {
        let bounds = Rectangle::new(Point::ORIGIN, primitive.size);
        let (width, height) = (
            (primitive.size.width * scale).round() as u32,
            (primitive.size.height * scale).round() as u32,
        );
        let viewport = Viewport::with_physical_size(Size::new(width, height), scale);
        primitive.prepare(pipeline, device, queue, &bounds, &viewport);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("empty canvas readback target"),
            size: wgpu::Extent3d {
                width,
                height,
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
            size: u64::from(width * height * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let linear = luxforge_reference::srgb::decode;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("empty canvas readback pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: linear(GROUND[0]),
                            g: linear(GROUND[1]),
                            b: linear(GROUND[2]),
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

    /// The art leaves the ground untouched at the canvas's edges, draws the lens and the word
    /// where the composition puts them, is the same frame for the same time and moves as time
    /// passes.
    #[test]
    #[slow]
    fn the_art_draws_the_lens_over_an_untouched_ground_and_moves() {
        let Some((device, queue)) = headless() else {
            eprintln!("skipped: no adapter; the empty canvas's art drew nothing");
            return;
        };
        let mut pipeline = ArtPipeline::new(&device, &queue, wgpu::TextureFormat::Rgba8UnormSrgb);
        let still = paint(&device, &queue, &mut pipeline, &primitive(STILL_TIME));
        for (x, y) in [
            (0, 0),
            (WIDTH - 1, 0),
            (0, HEIGHT - 1),
            (WIDTH - 1, HEIGHT - 1),
        ] {
            assert_eq!(
                pixel(&still, x, y),
                GROUND,
                "the corner ({x}, {y}) is the ground"
            );
        }
        let composition = primitive(0.0).composition;
        let inside = (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let (dx, dy) = (
                    x as f32 - composition.centre.x,
                    y as f32 - composition.centre.y,
                );
                (dx * dx + dy * dy).sqrt() < composition.radius * 0.95
            })
            .filter(|&(x, y)| pixel(&still, x, y) != GROUND)
            .count();
        assert!(inside > 1000, "the lens is drawn: {inside} pixels");
        let word = Rectangle::new(
            Point::new(
                composition.centre.x - WORD_COLUMNS * composition.font * ART_PIXEL / 2.0,
                composition.word_top,
            ),
            Size::new(
                WORD_COLUMNS * composition.font * ART_PIXEL,
                7.0 * composition.font * ART_PIXEL,
            ),
        );
        let lettered = (0..HEIGHT)
            .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
            .filter(|&(x, y)| word.contains(Point::new(x as f32 + 0.5, y as f32 + 0.5)))
            .filter(|&(x, y)| pixel(&still, x, y) != GROUND)
            .count();
        assert!(lettered > 200, "the word is drawn: {lettered} pixels");

        let again = paint(&device, &queue, &mut pipeline, &primitive(STILL_TIME));
        assert_eq!(still, again, "the same time draws the same frame");
        let early = paint(&device, &queue, &mut pipeline, &primitive(1.0));
        let later = paint(&device, &queue, &mut pipeline, &primitive(1.5));
        assert_ne!(early, later, "the art moves as its time passes");
    }
}
