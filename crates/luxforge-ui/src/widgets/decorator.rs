//! The one forwarding implementation behind the widgets that wrap a single content element to
//! intercept one gesture: the double-click, the slider guard, keyboard focus and the popover.
//!
//! A decoration implements [`Decoration`] with only the methods it customizes. [`Decorated`] is the
//! one `iced::advanced::Widget` over it: its size, size hint and layout are always the content's,
//! and every [`Decoration`] method the decoration leaves alone forwards straight to the content,
//! whose tree is the first child. So wrapping never changes how the content is measured, laid out,
//! operated, drawn or given events unless the decoration says so.
//!
//! A trait with forwarding defaults rather than a macro that emits the forwarding methods: with a
//! trait a decoration cannot forget to forward a method, since iced's own defaults for `update`,
//! `operate` and `mouse_interaction` would otherwise silently cut the content off, and each
//! decoration reads as an ordinary impl of the methods it changes.

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::{Element, Event, Length, Rectangle, Size, Vector};

/// What a wrapper adds to its content. Each method receives the content and the wrapper's whole
/// tree (its own [`Decoration::State`], and the content's tree as the first child); the defaults
/// forward to the content.
pub(super) trait Decoration<'a, M, Theme, Renderer: renderer::Renderer> {
    /// The wrapper's own state in the widget tree: `()` for a wrapper that keeps none.
    type State: Default + 'static;

    fn children(&self, content: &Element<'a, M, Theme, Renderer>) -> Vec<Tree> {
        vec![Tree::new(content)]
    }

    fn diff(&self, content: &Element<'a, M, Theme, Renderer>, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(content));
    }

    fn operate(
        &mut self,
        content: &mut Element<'a, M, Theme, Renderer>,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "iced's `Widget::update` plus the content"
    )]
    fn update(
        &mut self,
        content: &mut Element<'a, M, Theme, Renderer>,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        content.as_widget_mut().update(
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

    fn mouse_interaction(
        &self,
        content: &Element<'a, M, Theme, Renderer>,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        content
            .as_widget()
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "iced's `Widget::draw` plus the content"
    )]
    fn draw(
        &self,
        content: &Element<'a, M, Theme, Renderer>,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        content: &'b mut Element<'a, M, Theme, Renderer>,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

/// `content` with `decoration` applied: the one widget every decoration is drawn through.
pub(super) struct Decorated<'a, M, D, Theme = iced::Theme, Renderer = iced::Renderer> {
    pub(super) content: Element<'a, M, Theme, Renderer>,
    pub(super) decoration: D,
}

/// `content` wrapped in `decoration`, as an element.
pub(super) fn decorate<'a, M: 'a, D: Decoration<'a, M, iced::Theme, iced::Renderer> + 'a>(
    content: impl Into<Element<'a, M>>,
    decoration: D,
) -> Element<'a, M> {
    Element::new(Decorated {
        content: content.into(),
        decoration,
    })
}

impl<'a, M, D, Theme, Renderer> Widget<M, Theme, Renderer> for Decorated<'a, M, D, Theme, Renderer>
where
    Renderer: renderer::Renderer,
    D: Decoration<'a, M, Theme, Renderer>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<D::State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(D::State::default())
    }

    fn children(&self) -> Vec<Tree> {
        self.decoration.children(&self.content)
    }

    fn diff(&self, tree: &mut Tree) {
        self.decoration.diff(&self.content, tree);
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
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.decoration
            .operate(&mut self.content, tree, layout, renderer, operation);
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
        self.decoration.update(
            &mut self.content,
            tree,
            event,
            layout,
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
        self.decoration
            .mouse_interaction(&self.content, tree, layout, cursor, viewport, renderer)
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
        self.decoration.draw(
            &self.content,
            tree,
            renderer,
            theme,
            style,
            layout,
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
        self.decoration.overlay(
            &mut self.content,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
