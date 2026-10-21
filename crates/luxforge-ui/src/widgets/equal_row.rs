//! A row whose cards are all as tall as the tallest: cards side by side whose backgrounds and
//! borders end together however much text each wraps, and whose feet, such as a step's hint line,
//! sit at the bottom of every card.
//!
//! iced's own row cannot do it inside a scrollable: a child that fills the cross axis is laid out
//! against the tallest of the children laid out before it, not after. This one lays the children
//! out once, as an ordinary row does, then stretches each card's own bounds to the row's height
//! and leaves what is inside it where it was laid out, at the top: a container draws its
//! background and border to its bounds, so the cards end together. Laying each card out a second
//! time at the taller height was tried: the wgpu renderer then drew none of the cards' text. A
//! card's foot is its own child, laid out once at the card's width less the foot's padding and
//! placed at the stretched card's bottom, so it needs no second layout either. A centred child,
//! such as a chevron between cards, keeps its size and is centred in the row's height.

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
        kinds: Vec::new(),
        spacing,
        foot_padding: Padding::ZERO,
    }
}

/// How an item of the row is placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Stretched to the row's height.
    Card,
    /// Stretched, with the next foot at its bottom.
    Footed,
    /// Its own size, centred in the row's height.
    Centred,
}

/// The row's children are its items in order, then the footed cards' feet in order.
pub struct EqualRow<'a, Message, Theme, Renderer> {
    children: Vec<Element<'a, Message, Theme, Renderer>>,
    kinds: Vec<Kind>,
    spacing: f32,
    foot_padding: Padding,
}

impl<'a, Message, Theme, Renderer> EqualRow<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn item(mut self, child: Element<'a, Message, Theme, Renderer>, kind: Kind) -> Self {
        self.children.insert(self.kinds.len(), child);
        self.kinds.push(kind);
        self
    }

    /// A card, stretched to the row's height.
    #[must_use]
    pub fn push(self, card: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        self.item(card.into(), Kind::Card)
    }

    /// A card with `foot` at its bottom: the foot is inset by the row's
    /// [foot padding](Self::foot_padding) at the sides and bottom, and the card grows so its foot
    /// sits at least the padding's top under the card's own content, which is taken to end in the
    /// padding's bottom.
    #[must_use]
    pub fn push_footed(
        self,
        card: impl Into<Element<'a, Message, Theme, Renderer>>,
        foot: impl Into<Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        let mut row = self.item(card.into(), Kind::Footed);
        row.children.push(foot.into());
        row
    }

    /// A child that keeps its own size, centred in the row's height.
    #[must_use]
    pub fn push_centred(self, child: impl Into<Element<'a, Message, Theme, Renderer>>) -> Self {
        self.item(child.into(), Kind::Centred)
    }

    /// Where a footed card's foot sits: `top` under the card's content, the rest inside its edges.
    #[must_use]
    pub fn foot_padding(mut self, padding: Padding) -> Self {
        self.foot_padding = padding;
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
        let items = self.kinds.len();
        let (row_items, feet) = self.children.split_at_mut(items);
        let (row_trees, foot_trees) = tree.children.split_at_mut(items);
        let row = layout::flex::resolve(
            layout::flex::Axis::Horizontal,
            renderer,
            limits,
            Length::Fill,
            Length::Shrink,
            Padding::ZERO,
            self.spacing,
            Alignment::Start,
            row_items,
            row_trees,
        );
        // Each foot at its card's width, and the height every card then needs.
        let padding = self.foot_padding;
        let mut height = row.size().height;
        let mut feet = feet.iter_mut().zip(foot_trees);
        let laid_feet: Vec<(Rectangle, layout::Node)> = row
            .children()
            .iter()
            .zip(&self.kinds)
            .filter(|(_, kind)| **kind == Kind::Footed)
            .filter_map(|(card, _)| {
                let (foot, state) = feet.next()?;
                let card = card.bounds();
                let width = (card.width - padding.left - padding.right).max(0.0);
                let node = foot.as_widget_mut().layout(
                    state,
                    renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(width, f32::INFINITY)),
                );
                height = height.max(card.height + padding.top + node.size().height);
                Some((card, node))
            })
            .collect();
        let items = row.children().iter().zip(&self.kinds).map(|(laid, kind)| {
            let bounds = laid.bounds();
            if *kind == Kind::Centred {
                laid.clone()
                    .move_to(Point::new(bounds.x, (height - bounds.height) / 2.0))
            } else {
                layout::Node::with_children(
                    Size::new(bounds.width, height),
                    laid.children().to_vec(),
                )
                .move_to(Point::new(bounds.x, 0.0))
            }
        });
        let feet = laid_feet.into_iter().map(|(card, node)| {
            let y = height - padding.bottom - node.size().height;
            node.move_to(Point::new(card.x + padding.left, y))
        });
        layout::Node::with_children(
            Size::new(row.size().width, height),
            items.chain(feet).collect(),
        )
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

    fn card(height: f32) -> Probe<'static> {
        container(Space::new().height(Length::Fixed(height)))
            .width(Length::FillPortion(1))
            .into()
    }

    fn block(width: f32, height: f32) -> Probe<'static> {
        Space::new()
            .width(Length::Fixed(width))
            .height(Length::Fixed(height))
            .into()
    }

    #[test]
    fn equal_row_stretches_each_card_to_the_tallest_keeping_its_content_and_centres_the_rest() {
        let row = equal_row(10.0)
            .push(card(40.0))
            .push_centred(block(20.0, 10.0))
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

    #[test]
    fn equal_row_grows_for_a_card_and_its_foot_and_sets_every_foot_at_its_card_bottom() {
        let row = equal_row(10.0)
            .foot_padding(Padding {
                top: 6.0,
                right: 5.0,
                bottom: 4.0,
                left: 5.0,
            })
            .push_footed(card(40.0), block(30.0, 30.0))
            .push(card(70.0))
            .push_footed(card(50.0), block(20.0, 12.0));
        let node = laid(row, 320.0);
        // The first card needs 40 + 6 + 30: taller than the plain card's 70 and the last's
        // 50 + 6 + 12.
        assert_eq!(node.size(), Size::new(320.0, 76.0));
        let bounds: Vec<Rectangle> = node.children().iter().map(layout::Node::bounds).collect();
        assert_eq!(
            bounds,
            [
                Rectangle::new(Point::new(0.0, 0.0), Size::new(100.0, 76.0)),
                Rectangle::new(Point::new(110.0, 0.0), Size::new(100.0, 76.0)),
                Rectangle::new(Point::new(220.0, 0.0), Size::new(100.0, 76.0)),
                // Each foot inset 5 from its card's left and 4 from the row's bottom.
                Rectangle::new(Point::new(5.0, 42.0), Size::new(30.0, 30.0)),
                Rectangle::new(Point::new(225.0, 60.0), Size::new(20.0, 12.0)),
            ]
        );
    }
}
