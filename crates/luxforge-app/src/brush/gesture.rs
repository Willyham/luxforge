//! What one press on the photograph is, by the Corrections workspace's rules
//! (`docs/design/corrections-workspace.md`, "The canvas"): which target catches it, whether it is a
//! click or a drag, what its modifiers make of it, where a set source puts the next path's source,
//! and what a pin, source or size-handle drag changes. Plain data and arithmetic: the canvas feeds
//! it positions in its own logical points and in normalized content coordinates through the core's
//! map ([`super::ContentMap`]), and the app turns what it answers into one draft per gesture.
use super::StrokeCapture;
use luxforge_core::path::COORDINATE_STEPS_PER_UNIT;

/// A pointer that moves less than this many logical points between press and release clicked.
pub(crate) const CLICK_SLOP: f32 = 3.0;
/// A pin or a handle within this many logical points of the pointer catches a press.
pub(crate) const HIT_RADIUS: f32 = 9.0;

/// A position on the canvas in logical points, as the pointer reports it.
pub(crate) type ViewPoint = (f32, f32);

fn distance(a: ViewPoint, b: ViewPoint) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// The modifiers held at a press. `command` is `⌘` on macOS and Ctrl elsewhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modifiers {
    pub(crate) shift: bool,
    pub(crate) command: bool,
    pub(crate) alt: bool,
}

/// What a press catches, in the workspace's hit order: the selected operation's handles, then pins
/// from the top of the stack down; anything else adds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hit<H, P> {
    Handle(H),
    Pin(P),
    Add,
}

/// The target a press at `pointer` catches. `handles` are the selected operation's handles and
/// `pins` every pin from the top of the stack down, each where the canvas draws it. With targets
/// hidden there are none and every press adds; an `⌥` or `⌘` press bypasses the pins, because both
/// are gestures that add. A handle or pin catches within [`HIT_RADIUS`]; among pins the topmost
/// within reach wins, not the nearest.
pub(crate) fn hit<H: Copy, P: Copy>(
    pointer: ViewPoint,
    modifiers: Modifiers,
    shown: bool,
    handles: impl IntoIterator<Item = (H, ViewPoint)>,
    pins: impl IntoIterator<Item = (P, ViewPoint)>,
) -> Hit<H, P> {
    if !shown {
        return Hit::Add;
    }
    let near = |at: &ViewPoint| distance(pointer, *at) <= HIT_RADIUS;
    if let Some((handle, _)) = handles.into_iter().find(|(_, at)| near(at)) {
        return Hit::Handle(handle);
    }
    if modifiers.alt || modifiers.command {
        return Hit::Add;
    }
    pins.into_iter()
        .find(|(_, at)| near(at))
        .map_or(Hit::Add, |(pin, _)| Hit::Pin(pin))
}

/// The source the next added path samples from: automatic, a point an `⌥`-click set and the next
/// press has not yet measured, or the offset that press measured, which following presses keep —
/// the aligned clone a pattern needs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) enum SourceChoice {
    #[default]
    Auto,
    Pending([f64; 2]),
    Offset([f64; 2]),
}

impl SourceChoice {
    /// An `⌥`-click at `at` sets the source for the presses that follow.
    pub(crate) fn set(&mut self, at: [f64; 2]) {
        *self = Self::Pending(at);
    }

    /// **Auto**, or Escape with nothing selected and no press held.
    pub(crate) fn clear(&mut self) {
        *self = Self::Auto;
    }

    /// The absolute source of a path whose first stored point is `anchor`: none when automatic;
    /// the set point itself the first time, which also measures the offset every later path keeps.
    pub(crate) fn source_for(&mut self, anchor: [f64; 2]) -> Option<[f64; 2]> {
        match *self {
            Self::Auto => None,
            Self::Pending(at) => {
                *self = Self::Offset([at[0] - anchor[0], at[1] - anchor[1]]);
                Some(at)
            }
            Self::Offset([dx, dy]) => Some([anchor[0] + dx, anchor[1] + dy]),
        }
    }

    /// Where the accent crosshair follows the brush: the set point until a press measures it, then
    /// the pointer plus the kept offset.
    pub(crate) fn crosshair(&self, pointer: [f64; 2]) -> Option<[f64; 2]> {
        match *self {
            Self::Auto => None,
            Self::Pending(at) => Some(at),
            Self::Offset([dx, dy]) => Some([pointer[0] + dx, pointer[1] + dy]),
        }
    }

    /// [`Self::crosshair`] where a press of `radius` at the pointer would sample, on a content
    /// stage of `content` pixels: a kept offset is carried as the press carries it, inside the
    /// frame and turned to the other side by an edge ([`luxforge_core::SourceFit::Turn`]).
    pub(crate) fn crosshair_within(
        &self,
        pointer: [f64; 2],
        radius: f64,
        content: (u32, u32),
    ) -> Option<[f64; 2]> {
        let source = self.crosshair(pointer)?;
        if !matches!(self, Self::Offset(_)) {
            return Some(source);
        }
        Some(
            luxforge_core::fit_correction_source(
                &[pointer],
                radius,
                source,
                content,
                luxforge_core::SourceFit::Turn,
            )
            .unwrap_or(source),
        )
    }
}

/// What a press that caught no target is.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PressKind {
    /// A click is a spot, a drag a stroke.
    Paint,
    /// `⇧`-click: a straight stroke from where the last path this window placed ended.
    Straight([f64; 2]),
    /// `⌘`-drag: a spot at the press whose source the drag pulls out to the pointer.
    Pull,
    /// `⌥`-click: sets the source for the presses that follow.
    SetSource,
}

/// One press on the photograph that adds, from press to release.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BrushGesture {
    kind: PressKind,
    press_view: ViewPoint,
    /// The farthest the pointer has been from the press, in logical points.
    travel: f32,
    press: [f64; 2],
    pointer: [f64; 2],
    capture: StrokeCapture,
}

/// What a move did to the gesture, so the caller sends only what changed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Moved {
    Nothing,
    /// The path grew or, for a straight stroke, its end moved.
    Path,
    /// A `⌘`-drag's pulled source moved here.
    Source([f64; 2]),
}

/// What a released press adds or sets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Released {
    /// One new path: a spot (one point) or a stroke. `source` is the source a `⌘`-drag pulled
    /// out; otherwise the window's [`SourceChoice`] decides.
    Add {
        path: StrokeCapture,
        pulled: Option<[f64; 2]>,
    },
    /// `⌥`-click: the source for the presses that follow, at this point.
    SetSource([f64; 2]),
}

impl BrushGesture {
    /// A press that caught no target, at `view` on the canvas and `content` in normalized content
    /// coordinates. `last_end` is where the last path this window placed in this mode ended.
    pub(crate) fn press(
        view: ViewPoint,
        content: [f64; 2],
        modifiers: Modifiers,
        last_end: Option<[f64; 2]>,
    ) -> Self {
        let kind = if modifiers.alt {
            PressKind::SetSource
        } else if modifiers.command {
            PressKind::Pull
        } else if let (true, Some(from)) = (modifiers.shift, last_end) {
            PressKind::Straight(from)
        } else {
            PressKind::Paint
        };
        let capture = match kind {
            PressKind::Straight(from) => StrokeCapture::of(&[from, content]),
            PressKind::SetSource => StrokeCapture::new(),
            PressKind::Pull => StrokeCapture::of(&[content]),
            PressKind::Paint => {
                let mut capture = StrokeCapture::new();
                capture.press((content[0], content[1]));
                capture
            }
        };
        Self {
            kind,
            press_view: view,
            travel: 0.0,
            press: content,
            pointer: content,
            capture,
        }
    }

    /// The pointer moved with the button down.
    pub(crate) fn moved(&mut self, view: ViewPoint, content: [f64; 2]) -> Moved {
        self.travel = self.travel.max(distance(self.press_view, view));
        self.pointer = content;
        match self.kind {
            PressKind::Paint => {
                if self.capture.paint((content[0], content[1])) {
                    Moved::Path
                } else {
                    Moved::Nothing
                }
            }
            PressKind::Straight(from) => {
                self.capture = StrokeCapture::of(&[from, content]);
                Moved::Path
            }
            PressKind::Pull if self.dragged() => Moved::Source(content),
            PressKind::Pull | PressKind::SetSource => Moved::Nothing,
        }
    }

    /// Whether the pointer has left the click slop: a drag rather than a click.
    pub(crate) fn dragged(&self) -> bool {
        self.travel >= CLICK_SLOP
    }

    /// The path the gesture would add now, for its live draft and overlay: the stroke so far, the
    /// straight stroke to the pointer, or the spot.
    pub(crate) fn path(&self) -> &StrokeCapture {
        &self.capture
    }

    /// The `⌘`-drag's pulled source, once the pointer has left the click slop.
    pub(crate) fn pulled(&self) -> Option<[f64; 2]> {
        (self.kind == PressKind::Pull && self.dragged()).then_some(self.pointer)
    }

    /// Whether this press sets the source rather than adding.
    pub(crate) fn sets_source(&self) -> bool {
        self.kind == PressKind::SetSource
    }

    /// The button came up at `content`. A paint that stayed inside the click slop is a spot at the
    /// press, whatever it jittered through; a `⌘`-click that pulled nothing is a spot with the
    /// window's source.
    pub(crate) fn release(mut self, view: ViewPoint, content: [f64; 2]) -> Released {
        if self.kind != PressKind::SetSource {
            self.moved(view, content);
        }
        match self.kind {
            PressKind::SetSource => Released::SetSource(self.press),
            PressKind::Paint if !self.dragged() => Released::Add {
                path: StrokeCapture::of(&[self.press]),
                pulled: None,
            },
            PressKind::Pull => Released::Add {
                pulled: self.pulled(),
                path: StrokeCapture::of(&[self.press]),
            },
            PressKind::Paint | PressKind::Straight(_) => {
                self.capture.release();
                Released::Add {
                    path: self.capture,
                    pulled: None,
                }
            }
        }
    }
}

/// Which of the selected operation's handles a drag holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handle {
    /// The pin, at the path's first point: moves the destination, the source stays put.
    Pin,
    /// The source pin or the dashed source outline: moves the source.
    Source,
    /// A spot's size handle, on the right of its outline: changes its radius about its centre.
    Size,
}

/// One stored operation as the list query reads it, in the coordinates its path is stored in.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeldOperation {
    pub(crate) operation: String,
    pub(crate) points: Vec<[f64; 2]>,
    pub(crate) radius: f64,
    pub(crate) source: Option<[f64; 2]>,
}

/// What a handle drag changed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HandleChange {
    Path(Vec<[f64; 2]>),
    Source([f64; 2]),
    Radius(f64),
}

/// One handle drag of one stored operation: every move is measured from the press, so a drag away
/// and back changes nothing and writes nothing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HandleEdit {
    handle: Handle,
    held: HeldOperation,
    press: [f64; 2],
    pointer: [f64; 2],
    /// The content stage's `W/H`: a radius is in units of its height on both axes.
    aspect: f64,
    /// The content stage's size, when known: a dragged source stops inside its frame.
    frame: Option<(u32, u32)>,
}

impl HandleEdit {
    pub(crate) fn begin(handle: Handle, held: HeldOperation, press: [f64; 2], aspect: f64) -> Self {
        Self {
            handle,
            held,
            press,
            pointer: press,
            aspect,
            frame: None,
        }
    }

    /// The drag over a content stage of `frame` pixels: a source dragged towards an edge stops
    /// with its whole footprint inside, as the core stores it ([`luxforge_core::SourceFit::Clamp`]).
    pub(crate) fn within(mut self, frame: Option<(u32, u32)>) -> Self {
        self.frame = frame;
        self
    }

    pub(crate) fn drag(&mut self, content: [f64; 2]) {
        if content.iter().all(|value| value.is_finite()) {
            self.pointer = content;
        }
    }

    pub(crate) fn handle(&self) -> Handle {
        self.handle
    }

    pub(crate) fn operation(&self) -> &str {
        &self.held.operation
    }

    /// What the drag changes now, or `None` when it changes nothing. A pin drag translates the
    /// whole path by whole grid steps, so the stored path moves exactly; a source drag moves the
    /// source by the pointer's travel; a size drag changes the radius by how much farther from the
    /// spot's centre the pointer is than the press was, in units of the content height.
    pub(crate) fn change(&self) -> Option<HandleChange> {
        let [dx, dy] = [
            self.pointer[0] - self.press[0],
            self.pointer[1] - self.press[1],
        ];
        match self.handle {
            Handle::Pin => {
                let steps = |delta: f64| (delta * COORDINATE_STEPS_PER_UNIT).round();
                let (sx, sy) = (steps(dx), steps(dy));
                if sx == 0.0 && sy == 0.0 {
                    return None;
                }
                let (dx, dy) = (
                    sx / COORDINATE_STEPS_PER_UNIT,
                    sy / COORDINATE_STEPS_PER_UNIT,
                );
                Some(HandleChange::Path(
                    self.held
                        .points
                        .iter()
                        .map(|[x, y]| [x + dx, y + dy])
                        .collect(),
                ))
            }
            Handle::Source => {
                let held = self.held.source?;
                let mut source = [held[0] + dx, held[1] + dy];
                if let Some(frame) = self.frame {
                    source = luxforge_core::fit_correction_source(
                        &self.held.points,
                        self.held.radius,
                        source,
                        frame,
                        luxforge_core::SourceFit::Clamp,
                    )
                    .unwrap_or(source);
                }
                (source != held).then_some(HandleChange::Source(source))
            }
            Handle::Size => {
                // By the difference from the press, so a press slightly off the handle never
                // makes the radius jump.
                let [cx, cy] = *self.held.points.first()?;
                let reach = |[x, y]: [f64; 2]| ((x - cx) * self.aspect).hypot(y - cy);
                let radius = self.held.radius + reach(self.pointer) - reach(self.press);
                (radius != self.held.radius).then_some(HandleChange::Radius(radius))
            }
        }
    }
}
