//! A scrollable's content that never shrinks above what is in view.
//!
//! Iced keeps a scrollable's offset as an absolute distance and clamps it to the content's new
//! height when the content gets shorter. Scrolled near the bottom of a panel, a body that shrinks
//! (a tab with fewer rows, a group or section closing) pulls the offset back, and everything above
//! the change slides down as if a section above it had opened. [`steady_bottom`] holds the content
//! at least as tall as the bottom edge last drawn in view, so the offset needs no clamp and nothing
//! above the change moves. The held space is only ever what was already on screen: scrolling up
//! lets it go on the next layout, and it never adds room below what was last in view.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Vector};
use std::cell::Cell;

/// `content`, as a scrollable's content that keeps the height last drawn in view.
pub fn steady_bottom<'a, M: 'a, Theme: 'a, Renderer: renderer::Renderer + 'a>(
    content: impl Into<Element<'a, M, Theme, Renderer>>,
) -> Element<'a, M, Theme, Renderer> {
    Element::new(SteadyBottom {
        content: content.into(),
    })
}

struct SteadyBottom<'a, M, Theme, Renderer> {
    content: Element<'a, M, Theme, Renderer>,
}

/// The bottom edge of the scrollable's viewport, from the content's top, when it was last drawn.
/// A `Cell` because drawing sees the tree only by reference.
#[derive(Default)]
struct Floor(Cell<f32>);

/// The visible bottom edge, from the content's top, of a content drawn at `top` in `viewport`.
fn visible_bottom(viewport: &Rectangle, top: f32) -> f32 {
    (viewport.y + viewport.height - top).max(0.0)
}

/// The height laid out for content `natural` tall under a held `floor`, within `max`.
fn held_height(natural: f32, floor: f32, max: f32) -> f32 {
    natural.max(floor.min(max))
}

impl<M, Theme, Renderer: renderer::Renderer> Widget<M, Theme, Renderer>
    for SteadyBottom<'_, M, Theme, Renderer>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Floor>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Floor::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let child = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        let floor = tree.state.downcast_ref::<Floor>().0.get();
        let size = child.size();
        let height = held_height(size.height, floor, limits.max().height);
        layout::Node::with_children(Size::new(size.width, height), vec![child])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            content_layout(layout),
            renderer,
            operation,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            content_layout(layout),
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            content_layout(layout),
            cursor,
            viewport,
            renderer,
        )
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        tree.state
            .downcast_ref::<Floor>()
            .0
            .set(visible_bottom(viewport, layout.bounds().y));
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            content_layout(layout),
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            content_layout(layout),
            renderer,
            viewport,
            translation,
        )
    }
}

fn content_layout(layout: Layout<'_>) -> Layout<'_> {
    layout
        .children()
        .next()
        .expect("the held content is the only child")
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::Space;

    fn held(natural: f32) -> (SteadyBottom<'static, (), (), ()>, Tree) {
        let widget = SteadyBottom {
            content: Space::new().width(Length::Fill).height(natural).into(),
        };
        let tree = Tree::new(&widget as &dyn Widget<(), (), ()>);
        (widget, tree)
    }

    fn laid_out(widget: &mut SteadyBottom<'static, (), (), ()>, tree: &mut Tree) -> f32 {
        let limits = layout::Limits::new(Size::ZERO, Size::new(300.0, f32::INFINITY));
        Widget::<(), (), ()>::layout(widget, tree, &(), &limits)
            .size()
            .height
    }

    /// A panel scrolled to the bottom of 1200 px of content in a 500 px viewport has 1200 px in
    /// view; a body that shrinks to 900 px keeps 1200 laid out, so the offset is not clamped.
    #[test]
    fn shrinking_content_keeps_the_bottom_last_in_view() {
        let viewport = Rectangle::new(iced::Point::new(0.0, 740.0), Size::new(300.0, 500.0));
        let top = 40.0;
        assert_eq!(visible_bottom(&viewport, top), 1200.0);

        let (mut widget, mut tree) = held(900.0);
        assert_eq!(laid_out(&mut widget, &mut tree), 900.0, "no floor yet");
        tree.state
            .downcast_ref::<Floor>()
            .0
            .set(visible_bottom(&viewport, top));
        assert_eq!(laid_out(&mut widget, &mut tree), 1200.0);
        let child = Widget::<(), (), ()>::layout(
            &mut widget,
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, Size::new(300.0, f32::INFINITY)),
        );
        assert_eq!(
            child.children()[0].size().height,
            900.0,
            "the content keeps its own height; only the wrapper holds the space"
        );
    }

    #[test]
    fn the_held_space_never_exceeds_the_bottom_in_view_and_lets_go_on_scrolling_up() {
        // Content taller than what was in view is laid out at its own height.
        assert_eq!(held_height(1500.0, 1200.0, f32::INFINITY), 1500.0);
        // Scrolled up 400 px, the bottom in view is 800, below the content's own 900.
        assert_eq!(held_height(900.0, 800.0, f32::INFINITY), 900.0);
        // A bounded parent caps the floor.
        assert_eq!(held_height(900.0, 1200.0, 1000.0), 1000.0);
        // A viewport entirely above the content holds nothing.
        let above = Rectangle::new(iced::Point::new(0.0, -600.0), Size::new(300.0, 500.0));
        assert_eq!(visible_bottom(&above, 0.0), 0.0);
    }
}
