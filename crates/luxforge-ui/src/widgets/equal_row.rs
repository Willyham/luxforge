//! A row whose cards are all as tall as the tallest: cards side by side whose backgrounds and
//! borders end together however much text each wraps.
//!
//! iced's own row cannot do it inside a scrollable: a child that fills the cross axis is laid out
//! against the tallest of the children laid out before it, not after. This one lays the children
//! out once, as an ordinary row does, then stretches each card's own bounds to the row's height
//! and leaves what is inside it where it was laid out, at the top: a container draws its
//! background and border to its bounds, so the cards end together. Laying each card out a second
//! time at the taller height was tried: the wgpu renderer then drew none of the cards' text. A centred child, such as a chevron between cards, keeps its size and is
//! centred in the row's height.

use iced::advanced::widget::{Operation, Tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::{Alignment, Element, Event, Length, Padding, Point, Rectangle, Size, Vector};

/// An empty row filling the width, its children `spacing` apart.
pub fn equal_row<'a, Message, Theme, Renderer>(
    spacing: f32,
) -> EqualRow<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    EqualRow {
        children: Vec::new(),
        centred: Vec::new(),
        spacing,
    }
}

pub struct EqualRow<'a, Message, Theme, Renderer> {
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    /// Whether each child is centred rather than stretched.
    centred: Vec<bool>,
    spacing: f32,
}

impl<'a, Message, Theme, Renderer> EqualRow<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    /// A card, stretched to the row's height.
    #[must_use]
    pub fn push(mut self, card: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        self.children.push(card.into());
        self.centred.push(false);
        self
    }

    /// A child that keeps its own size, centred in the row's height.
    #[must_use]
    pub fn push_centred(mut self, child: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        self.children.push(child.into());
        self.centred.push(true);
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for EqualRow<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let row = layout::flex::resolve(
            layout::flex::Axis::Horizontal,
            renderer,
            limits,
            Length::Fill,
            Length::Shrink,
            Padding::ZERO,
            self.spacing,
            Alignment::Start,
            &mut self.children,
            &mut tree.children,
        );
        let height = row.size().height;
        let children = row
            .children()
            .iter()
            .zip(&self.centred)
            .map(|(laid, &centred)| {
                let bounds = laid.bounds();
                if centred {
                    laid.clone()
                        .move_to(Point::new(bounds.x, (height - bounds.height) / 2.0))
                } else {
                    layout::Node::with_children(
                        Size::new(bounds.width, height),
                        laid.children().to_vec(),
                    )
                    .move_to(Point::new(bounds.x, 0.0))
                }
            })
            .collect();
        layout::Node::with_children(row.size(), children)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for ((child, state), layout) in self
                .children
                .iter_mut()
                .zip(&mut tree.children)
                .zip(layout.children())
            {
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        for ((child, state), layout) in self
            .children
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            child.as_widget_mut().update(
                state, event, layout, cursor, renderer, clipboard, shell, viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .map(|((child, state), layout)| {
                child
                    .as_widget()
                    .mouse_interaction(state, layout, cursor, viewport, renderer)
            })
            .max()
            .unwrap_or_default()
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
        for ((child, state), layout) in self
            .children
            .iter()
            .zip(&tree.children)
            .zip(layout.children())
            .filter(|(_, layout)| layout.bounds().intersects(viewport))
        {
            child
                .as_widget()
                .draw(state, renderer, theme, style, layout, cursor, viewport);
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        overlay::from_children(
            &mut self.children,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<EqualRow<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(row: EqualRow<'a, Message, Theme, Renderer>) -> Self {
        Element::new(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::{Space, container};

    type Probe<'a> = Element<'a, (), iced::Theme, ()>;

    /// Lays `row` out in a `width`-wide limit with unbounded height, as a scrollable column does.
    fn laid(mut row: EqualRow<'_, (), iced::Theme, ()>, width: f32) -> layout::Node {
        let mut tree = Tree {
            tag: iced::advanced::widget::tree::Tag::stateless(),
            state: iced::advanced::widget::tree::State::None,
            children: Widget::<(), iced::Theme, ()>::children(&row),
        };
        Widget::layout(
            &mut row,
            &mut tree,
            &(),
            &layout::Limits::new(Size::ZERO, Size::new(width, f32::INFINITY)),
        )
    }

    #[test]
    fn equal_row_stretches_each_card_to_the_tallest_keeping_its_content_and_centres_the_rest() {
        let card = |height: f32| -> Probe<'_> {
            container(Space::new().height(Length::Fixed(height)))
                .width(Length::FillPortion(1))
                .into()
        };
        let chevron: Probe<'_> = Space::new()
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(10.0))
            .into();
        let row = equal_row(10.0)
            .push(card(40.0))
            .push_centred(chevron)
            .push(card(90.0));
        let node = laid(row, 340.0);
        assert_eq!(node.size(), Size::new(340.0, 90.0));
        let bounds: Vec<Rectangle> = node.children().iter().map(layout::Node::bounds).collect();
        assert_eq!(
            bounds,
            [
                Rectangle::new(Point::new(0.0, 0.0), Size::new(150.0, 90.0)),
                Rectangle::new(Point::new(160.0, 40.0), Size::new(20.0, 10.0)),
                Rectangle::new(Point::new(190.0, 0.0), Size::new(150.0, 90.0)),
            ]
        );
        // The short card's content stays as it was laid out, at the top.
        assert_eq!(
            node.children()[0].children()[0].bounds(),
            Rectangle::new(Point::ORIGIN, Size::new(0.0, 40.0))
        );
    }
}
