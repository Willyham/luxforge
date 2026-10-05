//! A panel that drops under a control while the pointer rests on it: the title bar's zoom stops.
//!
//! The anchor is laid out, drawn and given events exactly as it would be on its own. Once the
//! pointer has rested on it for [`OPEN_DELAY`], the panel is drawn as an overlay centred
//! [`POPOVER_GAP`] under it, moved back inside the window when it would leave it. The panel stays
//! while the pointer is on the anchor or the panel, or holds a press that started in the panel, and
//! closes [`CLOSE_DELAY`] after the pointer has left both, so crossing the gap between them, or
//! overshooting the panel's edge for a moment, never closes it.
//!
//! The host can also show the panel without the pointer, as a keyboard zoom shows the zoom stops
//! moving: each change of its `reveal` count shows the panel for [`REVEAL_LINGER`], or keeps it
//! that much longer, and a pointer that comes onto the anchor or the panel meanwhile keeps it as a
//! hover would.
//!
//! Whether the panel is showing is the widget's own pointer state, as a tooltip's is: it holds no
//! value and publishes nothing itself, and the panel's own controls publish what they change.

use super::POPOVER_GAP;
use super::decorator::{Decoration, decorate};
use crate::{Element, Theme};
use iced::advanced::{Clipboard, Layout, Shell, layout, mouse, overlay, renderer, widget::Tree};
use iced::{Event, Point, Rectangle, Size, Vector, window};
use std::time::{Duration, Instant};

/// How long the pointer rests on the anchor before the panel drops, so passing over the anchor on
/// the way somewhere else never flashes it.
pub const OPEN_DELAY: Duration = Duration::from_millis(200);
/// How long the panel stays after the pointer has left both it and the anchor.
pub const CLOSE_DELAY: Duration = Duration::from_millis(300);
/// How long a reveal shows the panel with the pointer elsewhere.
pub const REVEAL_LINGER: Duration = Duration::from_millis(1200);

/// `anchor`, with `panel` dropped under it while the pointer rests on either, or for a moment
/// after each change of `reveal`. A disabled hover panel never opens, and closes at once if it was
/// open.
pub fn hover_panel<'a, M: 'a>(
    anchor: impl Into<Element<'a, M>>,
    panel: impl Into<Element<'a, M>>,
    enabled: bool,
    reveal: u64,
) -> Element<'a, M> {
    decorate(
        anchor,
        HoverPanel {
            panel: panel.into(),
            enabled,
            reveal,
        },
    )
}

struct HoverPanel<'a, M> {
    panel: Element<'a, M>,
    enabled: bool,
    reveal: u64,
}

/// Where the panel is in its life, and since when.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Closed,
    /// The pointer came to rest on the anchor at this instant.
    Opening(Instant),
    Open,
    /// The pointer left the anchor and the panel at this instant.
    Closing(Instant),
    /// The host revealed the panel; it shows until this instant unless the pointer comes to it.
    Revealed(Instant),
}

/// What the pointer is doing that keeps the panel showing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Pointer {
    pub(crate) over_anchor: bool,
    pub(crate) over_panel: bool,
    /// A press that started in the panel is still held, wherever the pointer has since gone.
    pub(crate) pressing: bool,
}

#[derive(Debug, Default)]
struct Hover {
    phase: Phase,
    pointer: Pointer,
    /// The reveal count last seen; `None` until the first look, which only records it, so a
    /// widget built fresh never reveals itself.
    seen: Option<u64>,
}

/// The phase a reveal at `now` moves `phase` to: shown for the linger from now, unless the pointer
/// already keeps it open.
pub(crate) fn reveal(phase: Phase, pointer: Pointer, now: Instant) -> Phase {
    let inside = pointer.over_anchor || pointer.over_panel || pointer.pressing;
    if phase == Phase::Open && inside {
        Phase::Open
    } else {
        Phase::Revealed(now + REVEAL_LINGER)
    }
}

/// The phase after `phase` at `now` with the pointer as it is, and the instant to look again at, if
/// the phase is waiting on a delay.
pub(crate) fn step(phase: Phase, pointer: Pointer, now: Instant) -> (Phase, Option<Instant>) {
    let inside = pointer.over_anchor || pointer.over_panel || pointer.pressing;
    match phase {
        Phase::Closed if pointer.over_anchor => (Phase::Opening(now), Some(now + OPEN_DELAY)),
        Phase::Closed => (Phase::Closed, None),
        Phase::Opening(_) if !pointer.over_anchor => (Phase::Closed, None),
        Phase::Opening(since) if now >= since + OPEN_DELAY => (Phase::Open, None),
        Phase::Opening(since) => (phase, Some(since + OPEN_DELAY)),
        Phase::Open if inside => (Phase::Open, None),
        Phase::Open => (Phase::Closing(now), Some(now + CLOSE_DELAY)),
        Phase::Closing(_) if inside => (Phase::Open, None),
        Phase::Closing(since) if now >= since + CLOSE_DELAY => (Phase::Closed, None),
        Phase::Closing(since) => (phase, Some(since + CLOSE_DELAY)),
        Phase::Revealed(_) if inside => (Phase::Open, None),
        Phase::Revealed(until) if now >= until => (Phase::Closed, None),
        Phase::Revealed(until) => (phase, Some(until)),
    }
}

impl Hover {
    fn showing(&self) -> bool {
        matches!(
            self.phase,
            Phase::Open | Phase::Closing(_) | Phase::Revealed(_)
        )
    }

    /// Move on from what the pointer now does, asking for a frame when the panel appears or goes,
    /// and for one at the end of whichever delay is running.
    fn advance<M>(&mut self, shell: &mut Shell<'_, M>) {
        let was_showing = self.showing();
        let (phase, wake) = step(self.phase, self.pointer, Instant::now());
        self.phase = phase;
        if !self.showing() {
            self.pointer.over_panel = false;
            self.pointer.pressing = false;
        }
        if self.showing() != was_showing {
            shell.invalidate_layout();
            shell.request_redraw();
        }
        if let Some(at) = wake {
            shell.request_redraw_at(at);
        }
    }
}

impl<'a, M> Decoration<'a, M, Theme, iced::Renderer> for HoverPanel<'a, M> {
    type State = Hover;

    fn children(&self, anchor: &Element<'a, M>) -> Vec<Tree> {
        vec![Tree::new(anchor), Tree::new(&self.panel)]
    }

    fn diff(&self, anchor: &Element<'a, M>, tree: &mut Tree) {
        tree.diff_children(&[anchor.as_widget(), self.panel.as_widget()]);
    }

    fn update(
        &mut self,
        anchor: &mut Element<'a, M>,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        anchor.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        let hover = tree.state.downcast_mut::<Hover>();
        if !self.enabled {
            if hover.showing() {
                shell.invalidate_layout();
                shell.request_redraw();
            }
            *hover = Hover::default();
            return;
        }
        match hover.seen {
            None => hover.seen = Some(self.reveal),
            Some(seen) if seen != self.reveal => {
                hover.seen = Some(self.reveal);
                hover.phase = reveal(hover.phase, hover.pointer, Instant::now());
                // Reveal from the phase just set, so the linger is scheduled now.
                hover.advance(shell);
                shell.invalidate_layout();
                shell.request_redraw();
            }
            Some(_) => {}
        }
        if matches!(
            event,
            Event::Mouse(mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft)
                | Event::Window(window::Event::RedrawRequested(_))
        ) {
            // Over the open panel the anchor's cursor is unavailable, so this is false there.
            hover.pointer.over_anchor = cursor.is_over(layout.bounds());
            hover.advance(shell);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        anchor: &'b mut Element<'a, M>,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, iced::Renderer>> {
        let Tree {
            state, children, ..
        } = tree;
        let [anchor_tree, panel_tree] = children.as_mut_slice() else {
            return None;
        };
        let anchor =
            anchor
                .as_widget_mut()
                .overlay(anchor_tree, layout, renderer, viewport, translation);
        let hover = state.downcast_mut::<Hover>();
        let panel = (self.enabled && hover.showing()).then(|| {
            overlay::Element::new(Box::new(Panel {
                panel: &mut self.panel,
                tree: panel_tree,
                hover,
                anchor: layout.bounds() + translation,
            }))
        });
        match (anchor, panel) {
            (None, None) => None,
            (Some(only), None) | (None, Some(only)) => Some(only),
            (Some(anchor), Some(panel)) => {
                Some(overlay::Group::with_children(vec![anchor, panel]).overlay())
            }
        }
    }
}

/// The showing panel, as the overlay that draws it over the window.
struct Panel<'a, 'b, M> {
    panel: &'b mut Element<'a, M>,
    tree: &'b mut Tree,
    hover: &'b mut Hover,
    /// The anchor's bounds in window coordinates.
    anchor: Rectangle,
}

/// Where a panel of `size` goes under `anchor` in a window of `bounds`: centred on the anchor,
/// `POPOVER_GAP` below it, then moved back inside the window on either axis.
fn place(anchor: Rectangle, size: Size, bounds: Size) -> Point {
    let x = (anchor.center_x() - size.width / 2.0)
        .min(bounds.width - size.width)
        .max(0.0);
    let y = (anchor.y + anchor.height + POPOVER_GAP)
        .min(bounds.height - size.height)
        .max(0.0);
    Point::new(x, y)
}

impl<M> overlay::Overlay<M, Theme, iced::Renderer> for Panel<'_, '_, M> {
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let node = self.panel.as_widget_mut().layout(
            self.tree,
            renderer,
            &layout::Limits::new(Size::ZERO, bounds),
        );
        let size = node.size();
        layout::Node::with_children(size, vec![node]).move_to(place(self.anchor, size, bounds))
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let Some(panel) = layout.children().next() else {
            return;
        };
        self.panel.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            panel,
            cursor,
            &layout.bounds(),
        );
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
    ) {
        let Some(panel) = layout.children().next() else {
            return;
        };
        let over = cursor.is_over(layout.bounds());
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft) => {
                self.hover.pointer.over_panel = over;
            }
            Event::Mouse(mouse::Event::ButtonPressed(_)) if over => {
                self.hover.pointer.pressing = true;
            }
            Event::Mouse(mouse::Event::ButtonReleased(_)) => {
                self.hover.pointer.pressing = false;
            }
            _ => {}
        }
        self.panel.as_widget_mut().update(
            self.tree,
            event,
            panel,
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        );
        // A press on the panel's own padding is the panel's, never the photograph's under it.
        if over
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(_) | mouse::Event::ButtonReleased(_))
            )
        {
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        if !cursor.is_over(layout.bounds()) {
            return mouse::Interaction::None;
        }
        let Some(panel) = layout.children().next() else {
            return mouse::Interaction::None;
        };
        // Anything but `None` over the panel keeps the widgets under it from seeing the pointer.
        match self.panel.as_widget().mouse_interaction(
            self.tree,
            panel,
            cursor,
            &layout.bounds(),
            renderer,
        ) {
            mouse::Interaction::None => mouse::Interaction::Idle,
            interaction => interaction,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON_ANCHOR: Pointer = Pointer {
        over_anchor: true,
        over_panel: false,
        pressing: false,
    };
    const AWAY: Pointer = Pointer {
        over_anchor: false,
        over_panel: false,
        pressing: false,
    };

    /// The panel drops only after the pointer has rested on the anchor for the open delay, and a
    /// pointer that passes over the anchor sooner never opens it.
    #[test]
    fn the_panel_opens_after_the_pointer_rests_on_the_anchor() {
        let t0 = Instant::now();
        let (phase, wake) = step(Phase::Closed, ON_ANCHOR, t0);
        assert_eq!(phase, Phase::Opening(t0));
        assert_eq!(wake, Some(t0 + OPEN_DELAY));
        let early = t0 + OPEN_DELAY / 2;
        assert_eq!(
            step(phase, ON_ANCHOR, early),
            (phase, Some(t0 + OPEN_DELAY))
        );
        assert_eq!(step(phase, AWAY, early), (Phase::Closed, None));
        assert_eq!(step(phase, ON_ANCHOR, t0 + OPEN_DELAY), (Phase::Open, None));
        assert_eq!(step(Phase::Closed, AWAY, t0), (Phase::Closed, None));
    }

    /// The panel stays while the pointer is on either it or the anchor or holds a press from the
    /// panel, and closes only after the close delay away from all three.
    #[test]
    fn the_panel_stays_while_the_pointer_is_on_it_and_closes_after_a_grace() {
        let t0 = Instant::now();
        for keeping in [
            ON_ANCHOR,
            Pointer {
                over_panel: true,
                ..AWAY
            },
            Pointer {
                pressing: true,
                ..AWAY
            },
        ] {
            assert_eq!(step(Phase::Open, keeping, t0), (Phase::Open, None));
            assert_eq!(
                step(Phase::Closing(t0), keeping, t0 + CLOSE_DELAY / 2),
                (Phase::Open, None),
                "coming back within the grace keeps it"
            );
        }
        let (phase, wake) = step(Phase::Open, AWAY, t0);
        assert_eq!(phase, Phase::Closing(t0));
        assert_eq!(wake, Some(t0 + CLOSE_DELAY));
        assert_eq!(
            step(phase, AWAY, t0 + CLOSE_DELAY / 2),
            (phase, Some(t0 + CLOSE_DELAY))
        );
        assert_eq!(step(phase, AWAY, t0 + CLOSE_DELAY), (Phase::Closed, None));
    }

    /// A reveal shows the panel for the linger with the pointer elsewhere, a later one extends it,
    /// and a pointer that comes to the panel or the anchor meanwhile keeps it as a hover does. A
    /// reveal never cuts short a panel the pointer is holding open.
    #[test]
    fn a_reveal_shows_the_panel_for_a_moment() {
        let t0 = Instant::now();
        for from in [
            Phase::Closed,
            Phase::Opening(t0),
            Phase::Closing(t0),
            Phase::Open,
        ] {
            assert_eq!(reveal(from, AWAY, t0), Phase::Revealed(t0 + REVEAL_LINGER));
        }
        assert_eq!(reveal(Phase::Open, ON_ANCHOR, t0), Phase::Open);
        let phase = Phase::Revealed(t0 + REVEAL_LINGER);
        assert_eq!(
            step(phase, AWAY, t0 + REVEAL_LINGER / 2),
            (phase, Some(t0 + REVEAL_LINGER))
        );
        assert_eq!(step(phase, AWAY, t0 + REVEAL_LINGER), (Phase::Closed, None));
        let later = t0 + REVEAL_LINGER / 2;
        assert_eq!(
            reveal(phase, AWAY, later),
            Phase::Revealed(later + REVEAL_LINGER)
        );
        let on_panel = Pointer {
            over_panel: true,
            ..AWAY
        };
        assert_eq!(step(phase, on_panel, t0), (Phase::Open, None));
        assert_eq!(step(phase, ON_ANCHOR, t0), (Phase::Open, None));
    }

    #[test]
    fn the_panel_centres_under_the_anchor_and_stays_inside_the_window() {
        let window = Size::new(800.0, 600.0);
        let panel = Size::new(300.0, 50.0);
        let anchor = Rectangle::new(Point::new(380.0, 10.0), Size::new(40.0, 24.0));
        assert_eq!(
            place(anchor, panel, window),
            Point::new(250.0, 34.0 + POPOVER_GAP)
        );
        let left = Rectangle::new(Point::new(20.0, 10.0), Size::new(40.0, 24.0));
        assert_eq!(place(left, panel, window).x, 0.0);
        let right = Rectangle::new(Point::new(760.0, 10.0), Size::new(40.0, 24.0));
        assert_eq!(place(right, panel, window).x, 500.0);
    }
}
