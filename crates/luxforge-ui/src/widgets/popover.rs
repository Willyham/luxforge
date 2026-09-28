//! A menu dropped under the control that opened it, over whatever lies below.
//!
//! The anchor is laid out and drawn in place, exactly as it would be on its own. While `open`, the
//! menu is drawn as an overlay whose top-left corner sits [`POPOVER_GAP`] under the anchor's
//! bottom-left corner, moved back inside the window when it would leave it. A press anywhere
//! outside both the menu and the anchor publishes `on_dismiss` and goes no further, so a click
//! elsewhere closes the menu without also acting on what was under it; a press on the anchor is the
//! anchor's own, which is how the control that opened the menu closes it again. The widget holds no
//! state: whether the menu is open is the caller's.
use super::decorator::{Decoration, decorate};
use iced::advanced::{Clipboard, Layout, Shell, layout, mouse, overlay, renderer, widget::Tree};
use iced::{Element, Event, Point, Rectangle, Size, Vector, touch};

/// The space between the anchor's bottom edge and the menu's top edge.
pub(crate) const POPOVER_GAP: f32 = 4.0;

/// `anchor`, with `menu` dropped under it while `menu` is `Some`. A press outside both publishes
/// `on_dismiss`.
pub fn popover<'a, M: Clone + 'a>(
    anchor: impl Into<Element<'a, M>>,
    menu: Option<Element<'a, M>>,
    on_dismiss: M,
) -> Element<'a, M> {
    let open = menu.is_some();
    decorate(
        anchor,
        Popover {
            menu: menu.unwrap_or_else(|| iced::widget::Space::new().into()),
            open,
            on_dismiss,
        },
    )
}

/// The popover as a decoration of its anchor: the anchor is measured, laid out, drawn and given
/// events as it would be on its own, through the shared forwarding; the menu is a second child
/// the anchor never sees, drawn only as the overlay while open.
struct Popover<'a, M> {
    menu: Element<'a, M>,
    open: bool,
    on_dismiss: M,
}

impl<'a, M: Clone> Decoration<'a, M, iced::Theme, iced::Renderer> for Popover<'a, M> {
    type State = ();

    fn children(&self, anchor: &Element<'a, M>) -> Vec<Tree> {
        vec![Tree::new(anchor), Tree::new(&self.menu)]
    }

    fn diff(&self, anchor: &Element<'a, M>, tree: &mut Tree) {
        tree.diff_children(&[anchor.as_widget(), self.menu.as_widget()]);
    }

    fn overlay<'b>(
        &'b mut self,
        anchor: &'b mut Element<'a, M>,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, iced::Theme, iced::Renderer>> {
        let [anchor_tree, menu_tree] = tree.children.as_mut_slice() else {
            return None;
        };
        let anchor =
            anchor
                .as_widget_mut()
                .overlay(anchor_tree, layout, renderer, viewport, translation);
        let menu = self.open.then(|| {
            overlay::Element::new(Box::new(Menu {
                menu: &mut self.menu,
                tree: menu_tree,
                anchor: layout.bounds() + translation,
                on_dismiss: self.on_dismiss.clone(),
            }))
        });
        match (anchor, menu) {
            (None, None) => None,
            (Some(only), None) | (None, Some(only)) => Some(only),
            (Some(anchor), Some(menu)) => {
                Some(overlay::Group::with_children(vec![anchor, menu]).overlay())
            }
        }
    }
}

/// The open menu, as the overlay that draws it over the window.
struct Menu<'a, 'b, M> {
    menu: &'b mut Element<'a, M>,
    tree: &'b mut Tree,
    /// The anchor's bounds in window coordinates.
    anchor: Rectangle,
    on_dismiss: M,
}

/// Where a menu of `size` goes under `anchor` in a window of `bounds`: its left edge on the
/// anchor's, `POPOVER_GAP` below it, then moved back inside the window on either axis.
fn place(anchor: Rectangle, size: Size, bounds: Size) -> Point {
    let x = anchor.x.min(bounds.width - size.width).max(0.0);
    let y = (anchor.y + anchor.height + POPOVER_GAP)
        .min(bounds.height - size.height)
        .max(0.0);
    Point::new(x, y)
}

impl<M: Clone> overlay::Overlay<M, iced::Theme, iced::Renderer> for Menu<'_, '_, M> {
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let node = self.menu.as_widget_mut().layout(
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
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let Some(menu) = layout.children().next() else {
            return;
        };
        self.menu.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            menu,
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
        let Some(menu) = layout.children().next() else {
            return;
        };
        self.menu.as_widget_mut().update(
            self.tree,
            event,
            menu,
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        );
        let pressed = matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(_))
                | Event::Touch(touch::Event::FingerPressed { .. })
        );
        if pressed && !cursor.is_over(layout.bounds()) && !cursor.is_over(self.anchor) {
            shell.publish(self.on_dismiss.clone());
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
        let Some(menu) = layout.children().next() else {
            return mouse::Interaction::None;
        };
        // Anything but `None` over the menu keeps the widgets under it from seeing the pointer.
        match self.menu.as_widget().mouse_interaction(
            self.tree,
            menu,
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

    #[test]
    fn the_menu_drops_under_the_anchor_and_stays_inside_the_window() {
        let window = Size::new(800.0, 600.0);
        let menu = Size::new(200.0, 60.0);
        let anchor = Rectangle::new(Point::new(100.0, 10.0), Size::new(28.0, 28.0));
        assert_eq!(
            place(anchor, menu, window),
            Point::new(100.0, 38.0 + POPOVER_GAP)
        );
        let right = Rectangle::new(Point::new(760.0, 10.0), Size::new(28.0, 28.0));
        assert_eq!(place(right, menu, window).x, 600.0);
        let bottom = Rectangle::new(Point::new(100.0, 580.0), Size::new(28.0, 28.0));
        assert_eq!(place(bottom, menu, window).y, 540.0);
    }
}
