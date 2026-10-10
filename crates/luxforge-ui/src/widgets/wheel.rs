//! A hue and saturation wheel: the handle's angle is a hue on the familiar RGB colour wheel and its
//! distance from the centre a saturation, as a fraction of the rim.
//!
//! Orientation: red (0°) points right, at three o'clock, and hue grows counter-clockwise, so green
//! (120°) is up and to the left and blue (240°) down and to the left — the angle of the handle as a
//! person reads it on screen, with screen `y` pointing down.
//!
//! The widget owns no value and no parameter. It reports where the handle now stands, as a hue in
//! degrees and a radius fraction, with the gesture's helpers already applied: a press puts the
//! handle under the pointer; Shift keeps the hue the handle had when it went down while the
//! saturation follows the pointer, Cmd (Ctrl elsewhere) keeps the saturation the same way while
//! the hue follows, and
//! Option/Alt moves the handle a tenth of the pointer's travel from where it stood. The modifiers
//! are read only while this wheel holds a gesture. At the centre, where no angle exists, the hue
//! the handle already had is kept. The disc is drawn from the declared colour wheel alone, never
//! from a picture, and tessellated once per theme; the handle and its spoke are a separate, small
//! layer drawn from the hue and radius. The host maps the two numbers onto its declared
//! parameters, drafts them together and commits once on [`WheelEvent::Release`].

use super::curve_editor::invalidate_on_version_change;
use super::double_click::ClickRun;
use crate::theme;
use crate::widgets::text::control_label;
use crate::{Element, Theme, Token};
use iced::alignment::Horizontal;
use iced::widget::canvas::{self, Action, Event, Path, Stroke};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{column, container, row, text};
use iced::{
    Alignment, Color, Length, Point, Rectangle, Renderer, Vector, keyboard,
    mouse::{self, Cursor},
};
use std::cell::Cell;
use std::rc::Rc;

/// The share of the pointer's travel a fine (Option/Alt) drag moves the handle by.
pub const FINE: f32 = 0.1;
/// Within this fraction of the radius of the centre the handle has no angle, and keeps its hue.
pub const CENTRE: f32 = 1e-3;
/// The disc's diameter in the compact style, small enough for two to share a panel row.
pub const COMPACT_DIAMETER: f32 = 104.0;
/// The disc's diameter in the large style an individual view draws.
pub const LARGE_DIAMETER: f32 = 184.0;
/// How far outside the rim a press still lands on the disc, and the crosshair still shows.
const RIM_SLOP: f32 = 4.0;
/// Hue wedges and saturation rings the disc is tessellated into, once per theme.
const WEDGES: usize = 72;
const RINGS: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub struct WheelModel {
    pub label: String,
    /// What the label line shows at its right, already formatted: `120° · 40`.
    pub readout: String,
    /// The handle's hue in degrees, `0..=360`.
    pub hue: f32,
    /// The handle's distance from the centre, as a fraction of the rim, `0..=1`.
    pub radius: f32,
    pub large: bool,
    /// The wheel's gesture is open: the handle is drawn in the accent.
    pub dragging: bool,
    pub enabled: bool,
}

/// What a wheel reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WheelEvent {
    /// The handle now stands at `hue` degrees, `[0, 360)`, and `radius`, a fraction of the rim.
    Moved { hue: f32, radius: f32 },
    /// The gesture ended.
    Release,
    /// A double-click on the disc: reset the wheel.
    Reset,
}

/// The modifiers a wheel gesture honours.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WheelModifiers {
    /// Shift: keep the hue the handle has when it goes down.
    pub constrain_hue: bool,
    /// Cmd on macOS, Ctrl elsewhere: keep the saturation the handle has when it goes down.
    pub constrain_saturation: bool,
    /// Option/Alt: fine adjustment.
    pub fine: bool,
}

impl WheelModifiers {
    pub fn of(modifiers: keyboard::Modifiers) -> Self {
        Self {
            constrain_hue: modifiers.shift(),
            constrain_saturation: modifiers.command(),
            fine: modifiers.alt(),
        }
    }
}

/// The hue, in degrees `[0, 360)`, of a point `offset` from the centre in screen units (`y` down),
/// or `None` within [`CENTRE`] of it, where there is no angle.
pub fn hue_at(offset: [f32; 2]) -> Option<f32> {
    let [x, y] = offset;
    if !(x.is_finite() && y.is_finite()) || x.hypot(y) <= CENTRE {
        return None;
    }
    let degrees = (-y).atan2(x).to_degrees().rem_euclid(360.0);
    // `rem_euclid` can round a tiny negative angle up to exactly 360.
    Some(if degrees >= 360.0 { 0.0 } else { degrees })
}

/// The unit-disc offset (`y` down) of a handle at `hue` degrees and `radius`.
pub fn point_at(hue: f32, radius: f32) -> [f32; 2] {
    let angle = hue.to_radians();
    [radius * angle.cos(), -radius * angle.sin()]
}

/// One gesture's memory: the handle's last hue and radius, the pointer's last position for fine
/// adjustment, and the hue or saturation a held modifier keeps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grab {
    /// The handle's last hue, kept at the centre.
    hue: f32,
    radius: f32,
    /// The handle's last position on the unit disc.
    handle: [f32; 2],
    /// The pointer's last position while fine adjustment is on, and whether it is.
    anchor: [f32; 2],
    fine: bool,
    /// The hue Shift keeps and the radius Cmd/Ctrl keeps: the handle's own when the modifier went
    /// down, so pressing one never moves the handle.
    hue_lock: Option<f32>,
    radius_lock: Option<f32>,
}

impl Grab {
    /// A gesture beginning on a handle at `hue` and `radius`, pressed at `pointer` (unit-disc
    /// offset, `y` down). Without fine adjustment the handle jumps under the pointer, as a slider's
    /// handle jumps to a press on its rail; with it the handle stays where it was. A modifier held
    /// at the press keeps the handle's hue or saturation from before it.
    pub fn new(hue: f32, radius: f32, pointer: [f32; 2], fine: bool) -> Self {
        Self {
            hue,
            radius,
            handle: point_at(hue, radius),
            anchor: pointer,
            fine,
            hue_lock: None,
            radius_lock: None,
        }
    }

    /// The pointer moved to `pointer` with `modifiers` held: the hue and radius the handle now
    /// stands for, the handle clamped inside the rim.
    pub fn moved(&mut self, pointer: [f32; 2], modifiers: WheelModifiers) -> (f32, f32) {
        self.hue_lock = modifiers
            .constrain_hue
            .then(|| self.hue_lock.unwrap_or(self.hue));
        self.radius_lock = modifiers
            .constrain_saturation
            .then(|| self.radius_lock.unwrap_or(self.radius));
        let target = if modifiers.fine {
            if !self.fine {
                // Fine adjustment starts here: the handle stays, and moves by a tenth from now.
                self.anchor = pointer;
                self.fine = true;
            }
            let [dx, dy] = [pointer[0] - self.anchor[0], pointer[1] - self.anchor[1]];
            self.anchor = pointer;
            [self.handle[0] + dx * FINE, self.handle[1] + dy * FINE]
        } else {
            self.fine = false;
            self.anchor = pointer;
            pointer
        };
        let length = target[0].hypot(target[1]);
        let clamped = if length > 1.0 {
            [target[0] / length, target[1] / length]
        } else {
            target
        };
        let hue = self
            .hue_lock
            .unwrap_or_else(|| hue_at(clamped).unwrap_or(self.hue));
        let radius = self
            .radius_lock
            .unwrap_or_else(|| clamped[0].hypot(clamped[1]).min(1.0));
        self.hue = hue;
        self.radius = radius;
        self.handle = point_at(hue, radius);
        (hue, radius)
    }
}

/// A wheel row: the label line with its readout, then the disc. `on_event` receives what the
/// wheel reports.
pub fn wheel<'a, M: Clone + 'a>(
    model: &WheelModel,
    on_event: impl Fn(WheelEvent) -> M + 'a,
) -> Element<'a, M> {
    let readout = text(model.readout.clone())
        .size(theme::SIZE_CONTROL)
        .line_height(LineHeight::Absolute(theme::SLIDER_LABEL_HEIGHT.into()))
        .wrapping(Wrapping::None)
        .align_x(Horizontal::Right)
        .style(theme::ink(if model.enabled {
            Token::Text
        } else {
            Token::TextTertiary
        }));
    let header = row![
        container(control_label(model.label.clone(), model.enabled)).width(Length::Fill),
        container(readout).padding(iced::Padding::default().right(theme::VALUE_INSET)),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .width(Length::Fill);
    let diameter = if model.large {
        LARGE_DIAMETER
    } else {
        COMPACT_DIAMETER
    };
    let disc = iced::widget::canvas(WheelCanvas {
        model: model.clone(),
        on_event: Rc::new(on_event),
    })
    .width(Length::Fill)
    .height(Length::Fixed(diameter + 8.0));
    column![header, disc]
        .spacing(theme::SLIDER_GAP)
        .width(Length::Fill)
        .into()
}

struct WheelCanvas<'a, M> {
    model: WheelModel,
    on_event: Rc<dyn Fn(WheelEvent) -> M + 'a>,
}

#[derive(Default)]
struct WheelState {
    /// The colour disc and its rim, which depend on the theme alone; the cache redraws for a new
    /// size by itself.
    disc: canvas::Cache,
    disc_key: Cell<Option<u64>>,
    /// The spoke, the handle and the disabled veil, drawn from the model's own values.
    overlay: canvas::Cache,
    overlay_key: Cell<Option<OverlayKey>>,
    grab: Option<Grab>,
    modifiers: keyboard::Modifiers,
    clicks: ClickRun,
}

/// What the overlay's drawing depends on: the handle's hue and radius (as bits, so the key is
/// exact), whether it is dragged, whether the wheel is enabled, and the theme's generation.
type OverlayKey = (u32, u32, bool, bool, u64);

impl<M> WheelCanvas<'_, M> {
    /// The disc's centre and radius inside `bounds`' own coordinates.
    fn disc(&self, bounds: Rectangle) -> (Point, f32) {
        let radius = ((bounds.width.min(bounds.height)) / 2.0 - 4.0).max(1.0);
        (Point::new(bounds.width / 2.0, bounds.height / 2.0), radius)
    }

    /// Whether `point`, in `bounds`' own coordinates, is on the disc or within [`RIM_SLOP`] of its
    /// rim: where a press grabs the handle and the pointer shows a crosshair.
    fn on_disc(&self, point: Point, bounds: Rectangle) -> bool {
        let (centre, radius) = self.disc(bounds);
        let offset = point - centre;
        offset.x.hypot(offset.y) <= radius + RIM_SLOP
    }

    /// `point`, in `bounds`' own coordinates, as a unit-disc offset.
    fn unit(&self, point: Point, bounds: Rectangle) -> [f32; 2] {
        let (centre, radius) = self.disc(bounds);
        [(point.x - centre.x) / radius, (point.y - centre.y) / radius]
    }
}

impl<M: Clone> canvas::Program<M, Theme> for WheelCanvas<'_, M> {
    type State = WheelState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<M>> {
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) = event {
            state.modifiers = *modifiers;
            return None;
        }
        if !self.model.enabled {
            // A wheel disabled under a live gesture lets it go as a release does, so the host's
            // draft is never left open by a gesture nothing can end any more.
            return state
                .grab
                .take()
                .map(|_| Action::publish((self.on_event)(WheelEvent::Release)));
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                if !self.on_disc(point, bounds) {
                    return None;
                }
                if state.clicks.double(point) {
                    state.grab = None;
                    return Some(Action::publish((self.on_event)(WheelEvent::Reset)).and_capture());
                }
                let modifiers = WheelModifiers::of(state.modifiers);
                let pointer = self.unit(point, bounds);
                let mut grab =
                    Grab::new(self.model.hue, self.model.radius, pointer, modifiers.fine);
                let (hue, radius) = grab.moved(pointer, modifiers);
                state.grab = Some(grab);
                Some(
                    Action::publish((self.on_event)(WheelEvent::Moved { hue, radius }))
                        .and_capture(),
                )
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.grab.is_some() => {
                let position = cursor.position()?;
                let point = Point::new(position.x - bounds.x, position.y - bounds.y);
                let pointer = self.unit(point, bounds);
                let modifiers = WheelModifiers::of(state.modifiers);
                let grab = state.grab.as_mut()?;
                let (hue, radius) = grab.moved(pointer, modifiers);
                Some(
                    Action::publish((self.on_event)(WheelEvent::Moved { hue, radius }))
                        .and_capture(),
                )
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if state.grab.is_some() =>
            {
                state.grab = None;
                Some(Action::publish((self.on_event)(WheelEvent::Release)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<canvas::Geometry> {
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return Vec::new();
        }
        let (centre, radius) = self.disc(bounds);
        let model = &self.model;
        let generation = theme.generation();
        invalidate_on_version_change(&state.disc_key, generation, || state.disc.clear());
        let disc = state.disc.draw(renderer, bounds.size(), |frame| {
            // The disc: hue wedges, each a run of annular cells from grey at the centre to the hue
            // at the rim, drawn from the declared colour wheel and nothing else. A cell covers its
            // own ring alone, reaching half a point into the ring inside it so no seam shows, so
            // the disc is filled about once rather than once per ring.
            for wedge in 0..WEDGES {
                let from = wedge as f32 * 360.0 / WEDGES as f32;
                let to = from + 360.0 / WEDGES as f32 + 0.6;
                let middle = (from + to) / 2.0;
                for ring in 0..RINGS {
                    let inner = (radius * ring as f32 / RINGS as f32 - 0.5).max(0.0);
                    let outer = radius * (ring + 1) as f32 / RINGS as f32;
                    let saturation = (ring as f32 + 0.5) / RINGS as f32;
                    let rgb =
                        crate::hsv_to_rgb([f64::from(middle) / 360.0, f64::from(saturation), 0.82]);
                    let corner = |hue: f32, distance: f32| {
                        let [x, y] = point_at(hue, distance);
                        Point::new(centre.x + x, centre.y + y)
                    };
                    let path = Path::new(|builder| {
                        builder.move_to(corner(from, inner));
                        builder.line_to(corner(from, outer));
                        builder.line_to(corner(middle, outer));
                        builder.line_to(corner(to, outer));
                        builder.line_to(corner(to, inner));
                        builder.line_to(corner(middle, inner));
                        builder.close();
                    });
                    frame.fill(&path, Color::from_rgb8(rgb[0], rgb[1], rgb[2]));
                }
            }
            frame.stroke(
                &Path::circle(centre, radius),
                Stroke::default()
                    .with_color(theme.palette().border)
                    .with_width(1.0),
            );
        });
        let key = (
            model.hue.to_bits(),
            model.radius.to_bits(),
            model.dragging,
            model.enabled,
            generation,
        );
        invalidate_on_version_change(&state.overlay_key, key, || state.overlay.clear());
        let overlay = state.overlay.draw(renderer, bounds.size(), |frame| {
            let palette = theme.palette();
            // The handle and its spoke from the centre.
            let [x, y] = point_at(model.hue, model.radius.clamp(0.0, 1.0) * radius);
            let handle = centre + Vector::new(x, y);
            let ink = if model.dragging {
                palette.accent
            } else {
                palette.text
            };
            frame.stroke(
                &Path::line(centre, handle),
                Stroke::default()
                    .with_color(Color { a: 0.6, ..ink })
                    .with_width(1.0),
            );
            frame.fill(
                &Path::circle(handle, 5.0),
                Color {
                    a: 0.35,
                    ..palette.background
                },
            );
            frame.stroke(
                &Path::circle(handle, 5.0),
                Stroke::default().with_color(ink).with_width(2.0),
            );
            if !model.enabled {
                frame.fill(
                    &Path::circle(centre, radius + 1.0),
                    Color {
                        a: 0.58,
                        ..palette.background
                    },
                );
            }
        });
        vec![disc, overlay]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if !self.model.enabled {
            return mouse::Interaction::None;
        }
        if state.grab.is_some() {
            return mouse::Interaction::Grabbing;
        }
        match cursor.position_in(bounds) {
            Some(point) if self.on_disc(point, bounds) => mouse::Interaction::Crosshair,
            _ => mouse::Interaction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Size;
    use iced::widget::canvas::Program;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    /// Red points right, green up and to the left, blue down and to the left: the RGB wheel read
    /// counter-clockwise on a screen whose `y` points down.
    #[test]
    fn the_angle_is_the_rgb_wheel_counter_clockwise_from_three_oclock() {
        assert!(close(hue_at([1.0, 0.0]).unwrap(), 0.0));
        assert!(close(hue_at([0.0, -1.0]).unwrap(), 90.0));
        let green = point_at(120.0, 1.0);
        assert!(green[0] < 0.0 && green[1] < 0.0, "green is up and left");
        assert!(close(hue_at(green).unwrap(), 120.0));
        let blue = point_at(240.0, 1.0);
        assert!(blue[0] < 0.0 && blue[1] > 0.0, "blue is down and left");
        assert!(close(hue_at(blue).unwrap(), 240.0));
        for hue in [0.0, 1.0, 45.0, 179.5, 300.0, 359.0] {
            assert!(close(hue_at(point_at(hue, 0.5)).unwrap(), hue), "{hue}");
        }
        // Just below the seam is 359.x, never 360 and never negative.
        let below = hue_at([1.0, 1e-7]).unwrap();
        assert!((0.0..360.0).contains(&below), "{below}");
    }

    /// The centre has no angle: a drag through it keeps the hue the handle had.
    #[test]
    fn the_centre_keeps_the_last_hue() {
        assert_eq!(hue_at([0.0, 0.0]), None);
        assert_eq!(hue_at([CENTRE / 2.0, 0.0]), None);
        let mut grab = Grab::new(200.0, 0.5, point_at(200.0, 0.5), false);
        let (hue, radius) = grab.moved([0.0, 0.0], WheelModifiers::default());
        assert_eq!((hue, radius), (200.0, 0.0));
        let (hue, _) = grab.moved([0.0, 0.0], WheelModifiers::default());
        assert_eq!(hue, 200.0, "still the last hue");
    }

    /// Crossing the seam goes from 359 to 1 directly, not round the wheel.
    #[test]
    fn a_drag_across_the_seam_wraps() {
        let mut grab = Grab::new(359.0, 0.8, point_at(359.0, 0.8), false);
        let (hue, radius) = grab.moved(point_at(1.0, 0.8), WheelModifiers::default());
        assert!(close(hue, 1.0) && close(radius, 0.8), "{hue} {radius}");
        let (hue, _) = grab.moved(point_at(359.0, 0.8), WheelModifiers::default());
        assert!(close(hue, 359.0), "{hue}");
    }

    /// Past the rim the handle stays on the rim, at the pointer's angle.
    #[test]
    fn the_radius_is_clamped_at_the_rim() {
        let mut grab = Grab::new(0.0, 0.0, [0.0, 0.0], false);
        let (hue, radius) = grab.moved(point_at(90.0, 3.0), WheelModifiers::default());
        assert!(close(hue, 90.0));
        assert_eq!(radius, 1.0);
    }

    /// Shift keeps the starting hue while saturation follows; Cmd/Ctrl keeps the starting
    /// saturation while hue follows; Option moves a tenth of the pointer's travel.
    #[test]
    fn modifiers_constrain_and_refine_the_gesture() {
        let shift = WheelModifiers {
            constrain_hue: true,
            ..WheelModifiers::default()
        };
        let mut grab = Grab::new(30.0, 0.2, point_at(30.0, 0.2), false);
        let (hue, radius) = grab.moved(point_at(150.0, 0.7), shift);
        assert!(close(hue, 30.0) && close(radius, 0.7), "{hue} {radius}");

        let command = WheelModifiers {
            constrain_saturation: true,
            ..WheelModifiers::default()
        };
        let mut grab = Grab::new(30.0, 0.2, point_at(30.0, 0.2), false);
        let (hue, radius) = grab.moved(point_at(150.0, 0.7), command);
        assert!(close(hue, 150.0) && close(radius, 0.2), "{hue} {radius}");

        let fine = WheelModifiers {
            fine: true,
            ..WheelModifiers::default()
        };
        // Pressed with Option: the handle does not jump to the pointer.
        let mut grab = Grab::new(0.0, 0.5, [0.9, 0.0], true);
        let (hue, radius) = grab.moved([0.9, 0.0], fine);
        assert!(close(hue, 0.0) && close(radius, 0.5), "{hue} {radius}");
        // A pointer travel of 0.5 to the right moves the handle 0.05.
        let (hue, radius) = grab.moved([1.4, 0.0], fine);
        assert!(close(hue, 0.0) && close(radius, 0.55), "{hue} {radius}");
        // Releasing Option puts the handle back under the pointer.
        let (_, radius) = grab.moved([0.3, 0.0], WheelModifiers::default());
        assert!(close(radius, 0.3));
    }

    fn canvas() -> WheelCanvas<'static, WheelEvent> {
        WheelCanvas {
            model: WheelModel {
                label: "Wheel".into(),
                readout: String::new(),
                hue: 90.0,
                radius: 0.5,
                large: false,
                dragging: false,
                enabled: true,
            },
            on_event: Rc::new(|event| event),
        }
    }

    fn published(action: Option<Action<WheelEvent>>) -> Option<WheelEvent> {
        action.and_then(|action| action.into_inner().0)
    }

    /// A press moves the handle under the pointer and opens the gesture, a move reports the new
    /// position, and the release ends it; Shift read from the keyboard constrains the hue.
    #[test]
    fn a_press_drag_and_release_report_hue_and_radius() {
        let canvas = canvas();
        let mut state = WheelState::default();
        let bounds = Rectangle::new(Point::new(10.0, 20.0), Size::new(108.0, 108.0));
        // The disc's centre is (64, 74) on screen and its radius 50.
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let at = |x: f32, y: f32| Cursor::Available(Point::new(x, y));
        let Some(WheelEvent::Moved { hue, radius }) =
            published(canvas.update(&mut state, &press, bounds, at(114.0, 74.0)))
        else {
            panic!("a press moves the handle");
        };
        assert!(close(hue, 0.0) && close(radius, 1.0), "{hue} {radius}");
        let shift = Event::Keyboard(keyboard::Event::ModifiersChanged(
            keyboard::Modifiers::SHIFT,
        ));
        assert!(
            canvas
                .update(&mut state, &shift, bounds, at(114.0, 74.0))
                .is_none()
        );
        let moved = Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(64.0, 49.0),
        });
        let Some(WheelEvent::Moved { hue, radius }) =
            published(canvas.update(&mut state, &moved, bounds, at(64.0, 49.0)))
        else {
            panic!("a drag moves the handle");
        };
        assert!(close(hue, 0.0), "Shift keeps the starting hue: {hue}");
        assert!(close(radius, 0.5), "{radius}");
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        assert_eq!(
            published(canvas.update(&mut state, &release, bounds, Cursor::Unavailable)),
            Some(WheelEvent::Release)
        );
        // A second press at the same place soon after is a double-click: the wheel's reset.
        // `mouse::Click` needs the second press strictly later than the first, so the test waits
        // only until the clock has moved.
        let _ = canvas.update(&mut state, &press, bounds, at(70.0, 74.0));
        let pressed = std::time::Instant::now();
        let _ = canvas.update(&mut state, &release, bounds, Cursor::Unavailable);
        luxforge_testbase::wait_until("the clock moving past the first press", || {
            std::time::Instant::now() > pressed
        });
        assert_eq!(
            published(canvas.update(&mut state, &press, bounds, at(70.0, 74.0))),
            Some(WheelEvent::Reset)
        );
    }

    /// A disabled wheel publishes nothing, and a press outside the disc is not the wheel's.
    #[test]
    fn a_disabled_wheel_or_a_press_outside_the_disc_does_nothing() {
        let mut disabled = canvas();
        disabled.model.enabled = false;
        let mut state = WheelState::default();
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(200.0, 108.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        assert!(
            disabled
                .update(
                    &mut state,
                    &press,
                    bounds,
                    Cursor::Available(Point::new(100.0, 54.0))
                )
                .is_none()
        );
        let enabled = canvas();
        assert!(
            enabled
                .update(
                    &mut state,
                    &press,
                    bounds,
                    Cursor::Available(Point::new(2.0, 54.0))
                )
                .is_none(),
            "the corner beside the disc"
        );
    }

    /// A wheel disabled while it holds a gesture lets the gesture go with a release, once, so the
    /// host's draft is not left open.
    #[test]
    fn a_wheel_disabled_under_a_gesture_releases_it() {
        let enabled = canvas();
        let mut state = WheelState::default();
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(108.0, 108.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let at = Cursor::Available(Point::new(80.0, 54.0));
        assert!(matches!(
            published(enabled.update(&mut state, &press, bounds, at)),
            Some(WheelEvent::Moved { .. })
        ));
        let mut disabled = canvas();
        disabled.model.enabled = false;
        let moved = Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(90.0, 54.0),
        });
        assert_eq!(
            published(disabled.update(&mut state, &moved, bounds, at)),
            Some(WheelEvent::Release)
        );
        assert!(state.grab.is_none());
        assert!(disabled.update(&mut state, &moved, bounds, at).is_none());
    }

    /// The crosshair shows exactly where a press grabs the handle: on the disc and just past its
    /// rim, and nowhere else.
    #[test]
    fn the_crosshair_marks_where_a_press_lands() {
        let canvas = canvas();
        let state = WheelState::default();
        // The disc's centre is (54, 54) and its radius 50.
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(108.0, 108.0));
        // The centre, the rim, and just past the rim on either side.
        for x in [54.0, 104.0, 107.9, 2.0] {
            let point = Point::new(x, 54.0);
            assert!(canvas.on_disc(point, bounds), "{x}");
            assert_eq!(
                canvas.mouse_interaction(&state, bounds, Cursor::Available(point)),
                mouse::Interaction::Crosshair,
                "{x}"
            );
        }
        let corner = Point::new(4.0, 4.0);
        assert!(!canvas.on_disc(corner, bounds));
        assert_eq!(
            canvas.mouse_interaction(&state, bounds, Cursor::Available(corner)),
            mouse::Interaction::None
        );
    }
}
