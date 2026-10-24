//! The empty Develop canvas: the invitation to open a photograph under the Luxforge lockup, drawn
//! by one shader of its own (`empty_canvas.wgsl`).
//!
//! The art is the brand pack's stacked lockup (`packaging/brand`): the Midtone mark over the
//! wordmark, whose outline is read from the pack's own drawing, never retyped. When it first
//! appears the mark's two channels rise from the tonal axis and their light adds to the arch where
//! they overlap, and the word fades in under them; then the channels sway a little, out of step, as
//! a histogram does while the exposure moves. The mark is the house mix, never remixed: on a dark
//! ground the colour drawing with the word in Bright, on a light one the paper drawing, whose arch
//! is the ground itself, with the word in Ink. Every pixel the lockup does not cover is left
//! transparent, so the canvas's fill shows through it exactly.
//!
//! - **Bounded motion.** The art's clock is the widget's own, advanced by the redraws it asks for,
//!   about thirty a second, so it costs the editor no message and no view rebuild. It plays while
//!   the pointer has moved over the canvas within [`AWAKE`], then eases to a stop over [`EASE`] and
//!   asks for nothing more; the sway eases out with it, so the art comes to rest as the mark
//!   exactly. The next pointer movement over it eases it back in from where it stopped. Idle means
//!   asleep.
//! - **Held still.** A launch that draws its window on a software adapter or none, an evidence run
//!   and a hidden launch hold the art still for the life of the process ([`hold_still`]): it draws
//!   the lockup at rest, once, and never asks for another frame.
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
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use wgpu::util::DeviceExt as _;

/// How often the art redraws while it plays: about thirty times a second, enough for the motion to
/// read as smooth without redrawing the window at the display's rate.
pub const FRAME: Duration = Duration::from_millis(33);

/// How long the art plays after the pointer last moved over the canvas, or after it first appears.
pub const AWAKE: Duration = Duration::from_secs(30);

/// How long the art takes to ease from full speed to a stop, and back.
pub const EASE: Duration = Duration::from_millis(1500);

/// The longest step one redraw advances the art by, so a late frame never jumps it.
const MAX_STEP: Duration = Duration::from_millis(100);

/// The art's time in a frame held still: the channels risen and the word in.
const STILL_TIME: f32 = 4.0;

/// The lockup's width as a share of the canvas's width, and of the height the invitation leaves
/// it, and its bounds. The share keeps far more than the brand's clear space, a quarter of the
/// mark's width, on every side; the least keeps the mark well above the 32 px its drawing needs.
const WIDTH_SHARE: f32 = 0.3;
const HEIGHT_SHARE: f32 = 0.42;
const LOCKUP_MAX: f32 = 320.0;
const LOCKUP_MIN: f32 = 110.0;

/// The invitation's line, about [`theme::SIZE_TITLE`] high, and the least gap between it and the
/// word; a larger lockup's clear space is wider.
const INVITATION_HEIGHT: f32 = 18.0;
const INVITATION_GAP: f32 = 28.0;

/// The stacked lockup, as the brand pack draws it.
const LOCKUP_SVG: &str = include_str!("../../../packaging/brand/lockup/lockup-stacked-dark.svg");

/// The mark's construction (`packaging/brand/midtone.py`): its width as a share of the word's in
/// the stacked lockup, the quarters' radius as a share of the square's side, and the paper
/// drawing's tonal axis as a share of the mark's height.
const MARK_SHARE: f32 = 0.42;
const R0: f32 = 0.94;
const AXIS: f32 = 0.045;

/// The brand's colours (`packaging/brand/README.md`): the house mix's channels, Blue and
/// Chartreuse, and the Light their overlap makes; the channels' fills on paper; and the word's
/// Bright on dark grounds and Ink on light ones.
const BLUE: Color = Color::from_rgb8(0x4a, 0x75, 0xbf);
const CHARTREUSE: Color = Color::from_rgb8(0x9d, 0xc5, 0x70);
const LIGHT: Color = Color::from_rgb8(0xee, 0xf5, 0xf9);
const BLUE_ON_PAPER: Color = Color::from_rgb8(0x46, 0x6e, 0xb2);
const CHARTREUSE_ON_PAPER: Color = Color::from_rgb8(0x90, 0xb4, 0x68);
const BRIGHT: Color = Color::from_rgb8(0xf0, 0xf0, 0xf2);
const INK: Color = Color::from_rgb8(0x17, 0x18, 0x1b);

/// The most contours and segments the wordmark's flattened outline may have, as
/// `empty_canvas.wgsl`'s `Word` holds them.
const CONTOURS: usize = 16;
const SEGMENTS: usize = 640;

/// How far a flattened curve of the wordmark may stray from the drawing, in lockup units: a tenth
/// of a physical pixel at the largest lockup on a 2× display.
const TOLERANCE: f32 = 0.1;

static STILL: AtomicBool = AtomicBool::new(false);

/// Hold the empty canvas's art still for the life of the process: it draws the lockup at rest,
/// once, and asks for no redraw. Call it once at launch, before the window opens, for a launch
/// whose window is not drawn by a hardware GPU, and for evidence and hidden launches.
pub fn hold_still() {
    STILL.store(true, Ordering::Release);
}

/// Whether the process holds the art still ([`hold_still`]).
pub fn held_still() -> bool {
    STILL.load(Ordering::Acquire)
}

/// The empty canvas: the art, filling the canvas, with `invitation` centred under the lockup. The
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

/// Where the lockup and the invitation sit in a canvas, in its logical pixels: the two centred
/// together, top to bottom, the lockup a share of the room the invitation leaves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Composition {
    /// The lockup's top left.
    pub origin: Point,
    /// The lockup's width; its height is the drawing's at that width.
    pub width: f32,
    /// The top of the invitation's line.
    pub invitation_top: f32,
}

impl Composition {
    /// The composition in a canvas of `size`, or `None` when the lockup would be too small to
    /// read, where the invitation is drawn alone.
    pub fn of(size: Size) -> Option<Self> {
        let aspect = LOCKUP.height / LOCKUP.width;
        // The room under the lockup, its gap as wide as the largest lockup's clear space.
        let below = invitation_gap(LOCKUP_MAX) + INVITATION_HEIGHT;
        let width = (size.width * WIDTH_SHARE)
            .min((size.height - below) * HEIGHT_SHARE / aspect)
            .min(LOCKUP_MAX);
        if width < LOCKUP_MIN {
            return None;
        }
        let height = width * aspect;
        let top = ((size.height - height - below) / 2.0).max(0.0);
        Some(Self {
            origin: Point::new((size.width - width) / 2.0, top),
            width,
            invitation_top: top + height + invitation_gap(width),
        })
    }

    /// The lockup's height.
    pub fn height(&self) -> f32 {
        self.width * LOCKUP.height / LOCKUP.width
    }

    /// Where the lockup falls at `scale` physical pixels a logical one: its top left in physical
    /// pixels and its physical pixels a lockup unit. The mark's side is a whole number of pixels,
    /// and its left edge and baseline lie on pixel edges, so its straight edges are crisp.
    fn placement(&self, scale: f32) -> (Point, f32) {
        let [x0, base, side, _] = LOCKUP.mark;
        let unit = (self.width * scale / LOCKUP.width * side).round().max(1.0) / side;
        let left = (self.origin.x * scale + x0 * unit).round() - x0 * unit;
        let top = (self.origin.y * scale + base * unit).round() - base * unit;
        (Point::new(left, top), unit)
    }
}

/// The gap between a lockup `width` wide and the invitation: its clear space, a quarter of the
/// mark's width, and never less than [`INVITATION_GAP`].
fn invitation_gap(width: f32) -> f32 {
    INVITATION_GAP.max(MARK_SHARE * width / 4.0)
}

/// The stacked lockup the art draws, in its drawing's units.
#[derive(Debug)]
struct Lockup {
    width: f32,
    height: f32,
    /// The mark: its left corner's x, its baseline's y, the square's side and the quarters' radius.
    mark: [f32; 4],
    word: Outline,
}

static LOCKUP: LazyLock<Lockup> = LazyLock::new(|| {
    Lockup::parse(LOCKUP_SVG).unwrap_or_else(|error| panic!("the brand's stacked lockup: {error}"))
});

impl Lockup {
    /// The lockup from its drawing: the view box's size, the mark from the construction, centred on
    /// the word at the top, and the word from the drawing's last path, its outline.
    fn parse(svg: &str) -> Result<Self, String> {
        let view_box = attribute(svg, "viewBox").ok_or("no view box")?;
        let numbers: Vec<f32> = view_box
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|error| format!("the view box: {error}"))?;
        let &[0.0, 0.0, width, height] = numbers.as_slice() else {
            return Err(format!("the view box {view_box:?}"));
        };
        let word = svg
            .rsplit(" d=\"")
            .next()
            .and_then(|tail| tail.split('"').next())
            .ok_or("no word")?;
        let side = MARK_SHARE * width;
        Ok(Self {
            width,
            height,
            mark: [(width - side) / 2.0, R0 * side, side, R0 * side],
            word: Outline::flatten(word)?,
        })
    }
}

/// The value of the first attribute `name` in `svg`.
fn attribute<'a>(svg: &'a str, name: &str) -> Option<&'a str> {
    let at = svg.find(&format!(" {name}=\""))? + name.len() + 3;
    svg[at..].split('"').next()
}

/// An outline flattened to line segments, contour by contour.
#[derive(Debug, Default)]
struct Outline {
    /// Each contour's bounds, its least x and y then its greatest, and its first segment and count
    /// of segments.
    contours: Vec<([f32; 4], [u32; 2])>,
    /// Each segment's start and end.
    segments: Vec<[f32; 4]>,
}

impl Outline {
    /// The outline of the path data `d`, which uses the absolute `M`, `L`, `H`, `V`, `Q` and `Z`
    /// commands only, as the brand's outlines do, each curve flattened within [`TOLERANCE`].
    fn flatten(d: &str) -> Result<Self, String> {
        let tokens = path_tokens(d)?;
        let mut outline = Self::default();
        let (mut at, mut command) = (0, None);
        let (mut current, mut start, mut first) = ([0.0f32; 2], [0.0f32; 2], 0);
        while at < tokens.len() {
            if let PathToken::Command(next) = tokens[at] {
                command = Some(next);
                at += 1;
            }
            match command {
                Some(b'Z') => {
                    outline.close(&mut current, start, first);
                    first = outline.segments.len();
                    command = None;
                }
                Some(b'M') => {
                    outline.close(&mut current, start, first);
                    first = outline.segments.len();
                    let [x, y] = numbers(&tokens, &mut at)?;
                    (current, start) = ([x, y], [x, y]);
                    // Pairs after a move's first are lines.
                    command = Some(b'L');
                }
                Some(b'L') => {
                    let to = numbers(&tokens, &mut at)?;
                    outline.line(&mut current, to);
                }
                Some(b'H') => {
                    let [x] = numbers(&tokens, &mut at)?;
                    let to = [x, current[1]];
                    outline.line(&mut current, to);
                }
                Some(b'V') => {
                    let [y] = numbers(&tokens, &mut at)?;
                    let to = [current[0], y];
                    outline.line(&mut current, to);
                }
                Some(b'Q') => {
                    let [cx, cy, x, y] = numbers(&tokens, &mut at)?;
                    let (from, control, to) = (current, [cx, cy], [x, y]);
                    let bend = (from[0] - 2.0 * cx + x).hypot(from[1] - 2.0 * cy + y);
                    // A quadratic strays from the chords of n equal steps by at most bend / 4n².
                    let steps = (bend / (4.0 * TOLERANCE)).sqrt().ceil().clamp(1.0, 64.0) as u32;
                    for step in 1..=steps {
                        let t = step as f32 / steps as f32;
                        let point = |axis: usize| {
                            let u = 1.0 - t;
                            u * u * from[axis] + 2.0 * u * t * control[axis] + t * t * to[axis]
                        };
                        outline.line(&mut current, [point(0), point(1)]);
                    }
                }
                Some(other) => return Err(format!("the path command {:?}", other as char)),
                None => return Err("a number before any command".into()),
            }
        }
        outline.close(&mut current, start, first);
        if outline.contours.len() > CONTOURS || outline.segments.len() > SEGMENTS {
            return Err(format!(
                "{} contours and {} segments, more than the shader's {CONTOURS} and {SEGMENTS}",
                outline.contours.len(),
                outline.segments.len()
            ));
        }
        Ok(outline)
    }

    fn line(&mut self, current: &mut [f32; 2], to: [f32; 2]) {
        if *current != to {
            self.segments.push([current[0], current[1], to[0], to[1]]);
        }
        *current = to;
    }

    /// Close the contour whose segments start at `first`, back to its `start`.
    fn close(&mut self, current: &mut [f32; 2], start: [f32; 2], first: usize) {
        self.line(current, start);
        let segments = &self.segments[first..];
        if segments.is_empty() {
            return;
        }
        let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for &[x0, y0, x1, y1] in segments {
            bounds = [
                bounds[0].min(x0).min(x1),
                bounds[1].min(y0).min(y1),
                bounds[2].max(x0).max(x1),
                bounds[3].max(y0).max(y1),
            ];
        }
        self.contours
            .push((bounds, [first as u32, segments.len() as u32]));
    }

    /// The outline as `empty_canvas.wgsl`'s `Word` lays it out, unused contours empty.
    fn block(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(WORD_BYTES);
        let empty = ([1e30, 1e30, -1e30, -1e30], [0, 0]);
        let contours = || {
            self.contours
                .iter()
                .copied()
                .chain(std::iter::repeat(empty))
                .take(CONTOURS)
        };
        for (bounds, _) in contours() {
            for value in bounds {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        for (_, [first, count]) in contours() {
            for value in [first, count, 0, 0] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        for segment in self
            .segments
            .iter()
            .chain(std::iter::repeat(&[0.0; 4]))
            .take(SEGMENTS)
        {
            for value in segment {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        bytes
    }
}

#[derive(Clone, Copy, Debug)]
enum PathToken {
    Command(u8),
    Number(f32),
}

/// The commands and numbers of the path data `d`.
fn path_tokens(d: &str) -> Result<Vec<PathToken>, String> {
    let bytes = d.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte.is_ascii_alphabetic() {
            tokens.push(PathToken::Command(byte));
            at += 1;
        } else if byte == b' ' || byte == b',' {
            at += 1;
        } else {
            let start = at;
            if byte == b'-' || byte == b'+' {
                at += 1;
            }
            let mut point = false;
            while at < bytes.len() && (bytes[at].is_ascii_digit() || (bytes[at] == b'.' && !point))
            {
                point |= bytes[at] == b'.';
                at += 1;
            }
            let number = d[start..at]
                .parse()
                .map_err(|_| format!("the path number at {start}: {:?}", &d[start..=start]))?;
            tokens.push(PathToken::Number(number));
        }
    }
    Ok(tokens)
}

/// The `N` numbers at `at` in `tokens`, advancing past them.
fn numbers<const N: usize>(tokens: &[PathToken], at: &mut usize) -> Result<[f32; N], String> {
    let mut values = [0.0; N];
    for value in &mut values {
        let Some(PathToken::Number(number)) = tokens.get(*at) else {
            return Err(format!("{N} numbers at token {at}"));
        };
        *value = *number;
        *at += 1;
    }
    Ok(values)
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

    /// How much the art sways: its speed, so the sway eases out as the clock comes to rest.
    fn sway(&self) -> f32 {
        self.speed
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
        let (time, sway) = if self.still {
            (STILL_TIME, 0.0)
        } else {
            let clock = tree.state.downcast_ref::<Clock>();
            (clock.seconds(), clock.sway())
        };
        let bounds = layout.bounds();
        renderer.draw_primitive(
            bounds,
            ArtPrimitive {
                size: bounds.size(),
                composition: self.composition,
                time,
                sway,
                ground: (self.ground)(theme.palette()),
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

/// Whether `ground` takes the paper drawing: it is light, Ink reading on it at 4.5:1 or more and
/// better than Bright does. A mid grey, where neither reads well, keeps the colour drawing, whose
/// arch is its own light.
fn on_paper(ground: Color) -> bool {
    let contrast = |a: Color, b: Color| {
        let (a, b) = (a.relative_luminance(), b.relative_luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    };
    let ink = contrast(ground, INK);
    ink >= 4.5 && ink > contrast(ground, BRIGHT)
}

/// One frame of the art: what its uniform block is made from once the scale factor is known.
#[derive(Debug)]
struct ArtPrimitive {
    size: Size,
    composition: Composition,
    time: f32,
    /// How much the channels sway, 0 at rest to 1 at full speed.
    sway: f32,
    ground: Color,
    /// The pipeline's uniform slot `prepare` wrote this frame's block into, which `draw` binds.
    slot: AtomicUsize,
}

/// The uniform block's size: eight vectors.
const UNIFORM_BYTES: usize = 8 * 16;

/// The wordmark's block's size: a vector for each contour's bounds and range, and for each segment.
const WORD_BYTES: usize = (2 * CONTOURS + SEGMENTS) * 16;
// Within the 16 KiB a downlevel device binds.
const _: () = assert!(WORD_BYTES <= 16 * 1024);

/// The most art primitives one frame draws: one per window's canvas, with room to spare.
const SLOTS: usize = 4;

impl ArtPrimitive {
    /// The uniform block, as `empty_canvas.wgsl`'s `Art` lays it out, at `scale` physical pixels a
    /// logical one, for a target that stores linear light when `linear`.
    fn uniforms(&self, scale: f32, linear: bool) -> Vec<u8> {
        let (origin, unit) = self.composition.placement(scale);
        let paper = on_paper(self.ground);
        let radius = LOCKUP.mark[3];
        let colour = |colour: Color| [colour.r, colour.g, colour.b, colour.a];
        let (left, right, arch, word) = if paper {
            (BLUE_ON_PAPER, CHARTREUSE_ON_PAPER, Color::TRANSPARENT, INK)
        } else {
            (BLUE, CHARTREUSE, LIGHT, BRIGHT)
        };
        let vectors: [[f32; 4]; 8] = [
            [
                self.size.width * scale,
                self.size.height * scale,
                if linear { 1.0 } else { 0.0 },
                0.0,
            ],
            [
                origin.x,
                origin.y,
                unit,
                if paper { AXIS * radius } else { 0.0 },
            ],
            LOCKUP.mark,
            [self.time, self.sway, 0.0, 0.0],
            colour(left),
            colour(right),
            colour(arch),
            colour(word),
        ];
        let mut bytes = Vec::with_capacity(UNIFORM_BYTES);
        for value in vectors.iter().flatten() {
            bytes.extend_from_slice(&value.to_le_bytes());
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
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: pipeline.word.as_entire_binding(),
                    },
                ],
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

/// The art's pipeline: its program, the wordmark's outline, written once, and up to [`SLOTS`]
/// uniform blocks, one per primitive a frame draws, reused from frame to frame.
struct ArtPipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    word: wgpu::Buffer,
    slots: Vec<(wgpu::Buffer, wgpu::BindGroup)>,
    /// The slots this frame has prepared.
    used: usize,
    /// The target stores linear light: the toolkit chose an sRGB format, so it gamma corrects.
    linear: bool,
}

impl shader::Pipeline for ArtPipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("luxforge.empty_canvas.layout"),
            entries: &[uniform(0), uniform(1)],
        });
        let word = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("luxforge.empty_canvas.word"),
            contents: &LOCKUP.word.block(),
            usage: wgpu::BufferUsages::UNIFORM,
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
                    // The art is premultiplied, transparent wherever the lockup does not reach:
                    // the canvas's fill shows through there untouched.
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
            word,
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
mod tests;
