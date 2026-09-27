//! A menu dropped under the control that opened it, over whatever lies below.
//!
//! The anchor is laid out and drawn in place, exactly as it would be on its own. While `open`, the
//! menu is drawn as an overlay whose top-left corner sits [`POPOVER_GAP`] under the anchor's
//! bottom-left corner, moved back inside the window when it would leave it. A press anywhere
//! outside both the menu and the anchor publishes `on_dismiss` and goes no further, so a click
//! elsewhere closes the menu without also acting on what was under it; a press on the anchor is the
//! anchor's own, which is how the control that opened the menu closes it again. The widget holds no
//! state: whether the menu is open is the caller's.
use iced::advanced::{
    Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer,
    widget::{Operation, Tree},
};
use iced::{Element, Event, Length, Point, Rectangle, Size, Vector, touch};

/// The space between the anchor's bottom edge and the menu's top edge.
pub const POPOVER_GAP: f32 = 4.0;

/// `anchor`, with `menu` dropped under it while `menu` is `Some`. A press outside both publishes
/// `on_dismiss`.
pub fn popover<'a, M: Clone + 'a>(
    anchor: impl Into<Element<'a, M>>,
    menu: Option<Element<'a, M>>,
    on_dismiss: M,
) -> Element<'a, M> {
    let open = menu.is_some();
    Element::new(Popover {
        anchor: anchor.into(),
        menu: menu.unwrap_or_else(|| iced::widget::Space::new().into()),
        open,
        on_dismiss,
    })
}

struct Popover<'a, M> {
    anchor: Element<'a, M>,
    menu: Element<'a, M>,
    open: bool,
    on_dismiss: M,
}

impl<M: Clone> Widget<M, iced::Theme, iced::Renderer> for Popover<'_, M> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.anchor), Tree::new(&self.menu)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[self.anchor.as_widget(), self.menu.as_widget()]);
    }

    fn size(&self) -> Size<Length> {
        self.anchor.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.anchor.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.anchor
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.anchor.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &iced::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.anchor.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.anchor.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.anchor
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, iced::Theme, iced::Renderer>> {
        let [anchor_tree, menu_tree] = tree.children.as_mut_slice() else {
            return None;
        };
        let anchor = self.anchor.as_widget_mut().overlay(
            anchor_tree,
            layout,
            renderer,
            viewport,
            translation,
        );
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
