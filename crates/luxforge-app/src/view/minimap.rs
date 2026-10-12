//! The high-zoom minimap's card ([minimap design](../../../../docs/design/minimap.md)): an overview
//! of the whole stage the canvas navigates, at the canvas's lower right, with the main viewport's
//! rectangle over it.
//!
//! - **Placement.** [`card`] lays the card out from the canvas region's size alone: an image area
//!   of at most [`IMAGE_AREA`] aspect-fitted to the stage and hugged by the card, [`INSET`] from the
//!   canvas's right and bottom edges, clear of the scrollbars under it. On a small canvas it shrinks
//!   with the canvas, and where it would meet the mode strip it rises above it; where it cannot fit
//!   there is none. It is a layer of the canvas's own stack, never the scrollable's content, so it
//!   enlarges nothing that scrolls.
//! - **Drawing.** Image pixels only: the overview the photograph's surface keeps
//!   ([`luxforge_ui::overview_image`]), turned as the crop stage is while a crop draft is open, and
//!   the viewport's rectangle in a light line between two dark ones. A quiet placeholder while the
//!   stage's overview is made, *Updating* over a held one, *After* while Compare is shown.
//! - **Input.** [`MinimapInput`] owns pointer capture and nothing else: a left press on the card
//!   holds every pointer event until the release, wherever the pointer goes, so nothing under the
//!   card — a brush, a picker, the crop frame, Compare's divider, the scrollable — ever sees one;
//!   the wheel over the card is handed on as the scrollable's own pan. What a press or drag means is
//!   the editor's ([`crate::app::minimap`]), through the semantic view messages it publishes.
use crate::app::message::{Message, view::ViewMessage};
use iced::{
    Color, Event, Length, Padding, Point, Rectangle, Renderer, Size, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, mouse, renderer,
        widget::{Operation, Tree, tree},
    },
    alignment::{Horizontal, Vertical},
    widget::{
        canvas::{self, Frame, Geometry, Path, Stroke},
        container, stack, text,
    },
};
use luxforge_ui::{Element, Theme, Token, theme};

/// The largest image area, in logical points: the stage is aspect-fitted inside it.
pub(crate) const IMAGE_AREA: Size = Size::new(180.0, 120.0);
/// Between the card's edge and its image area.
pub(crate) const PADDING: f32 = 4.0;
/// From the canvas's right and bottom edges, clear of the scrollbars there.
pub(crate) const INSET: f32 = theme::CHROME_INSET;
/// The share of the canvas's width and height the image area may take on a small canvas.
const SHARE: f32 = 0.35;
/// The shortest image side a card is drawn for: smaller than this it is no overview.
const SMALLEST: f32 = 24.0;
/// The card's corner radius.
const RADIUS: f32 = 6.0;

/// Where the card sits in the canvas region, in the region's logical points, and its image area
/// inside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Card {
    /// The card's bounds, from the canvas region's top-left corner.
    pub(crate) bounds: Rectangle,
    /// The image area's size; it sits [`PADDING`] inside the card.
    pub(crate) area: Size,
}

/// The card for a canvas region of `canvas` showing a stage of `stage` pixels, with `modes` tools
/// in the mode strip at the region's bottom centre. `None` for a degenerate stage or a canvas too
/// small to hold one.
pub(crate) fn card(canvas: Size, stage: (f64, f64), modes: usize) -> Option<Card> {
    let (width, height) = (stage.0 as f32, stage.1 as f32);
    let usable = width.is_finite()
        && height.is_finite()
        && width > 0.0
        && height > 0.0
        && canvas.width.is_finite()
        && canvas.height.is_finite();
    if !usable {
        return None;
    }
    let bound = Size::new(
        IMAGE_AREA.width.min(canvas.width * SHARE),
        IMAGE_AREA.height.min(canvas.height * SHARE),
    );
    let scale = (bound.width / width).min(bound.height / height);
    let area = Size::new(width * scale, height * scale);
    if !(area.width.max(area.height) >= SMALLEST && area.width.min(area.height) >= 1.0) {
        return None;
    }
    let size = Size::new(area.width + 2.0 * PADDING, area.height + 2.0 * PADDING);
    let x = canvas.width - INSET - size.width;
    let mut bottom = canvas.height - INSET;
    // The mode strip, centred along the bottom: tools of a fixed width, their spacing and padding.
    let tools = modes.max(1) as f32;
    let strip_padding = theme::STRIP_PADDING + theme::BORDER_WIDTH;
    let strip_width = tools * theme::STRIP_TOOL_WIDTH
        + (tools - 1.0) * theme::STRIP_SPACING
        + 2.0 * strip_padding;
    let strip_right = (canvas.width + strip_width) / 2.0;
    let strip_top = canvas.height - theme::CHROME_INSET - theme::STRIP_HEIGHT;
    if x < strip_right + theme::CHROME_STACK_SPACING {
        bottom = strip_top - theme::CHROME_STACK_SPACING;
    }
    let y = bottom - size.height;
    (x >= INSET && y >= INSET).then(|| Card {
        bounds: Rectangle::new(Point::new(x, y), size),
        area,
    })
}

/// What the card shows of the stage's overview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shows {
    /// The overview is being made: a quiet placeholder, and no navigation yet.
    Pending,
    /// The stage's own overview.
    Ready,
    /// The last settled overview of the same stage, while its replacement is made.
    Updating,
    /// No overview could be made; navigation still works.
    Unavailable,
}

impl Shows {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Updating => "updating",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Everything the card draws, in plain data the editor derives ([`crate::app::minimap`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MinimapModel {
    pub(crate) card: Card,
    /// Where the stage's thumbnail lies in the image area.
    pub(crate) image: Rectangle,
    /// The overview to draw, when one is held.
    pub(crate) overview: Option<luxforge_ui::OverviewImage>,
    /// A crop draft's turned input stage, relative to the thumbnail.
    pub(crate) turn: Option<luxforge_ui::Turn>,
    /// The viewport's rectangle in the image area, while navigation may act on it.
    pub(crate) rectangle: Option<Rectangle>,
    pub(crate) shows: Shows,
    /// Compare's divider is shown: the overview is of its fixed After side.
    pub(crate) after: bool,
    /// A drag holds the rectangle.
    pub(crate) dragging: bool,
}

/// The card, placed in a layer that fills the canvas region.
pub(crate) fn layer<'a>(model: MinimapModel) -> Element<'a, Message> {
    let area = model.card.area;
    let image = model.image;
    let mut layers: Vec<Element<'a, Message>> = Vec::new();
    // The ground under the thumbnail: what shows while there is no image, and in a crop draft's
    // empty corners.
    layers.push(
        container(iced::widget::Space::new())
            .width(Length::Fixed(image.width))
            .height(Length::Fixed(image.height))
            .style(|active: &Theme| {
                container::Style::default().background(active.palette().get(Token::Surround))
            })
            .padding(0)
            .into(),
    );
    if let Some(overview) = model.overview {
        layers.push(
            luxforge_ui::overview_image(
                overview,
                Length::Fixed(image.width),
                Length::Fixed(image.height),
            )
            .turned(model.turn)
            .into(),
        );
    }
    let placed = container(stack(layers))
        .padding(Padding {
            top: image.y,
            left: image.x,
            right: 0.0,
            bottom: 0.0,
        })
        .width(Length::Fixed(area.width))
        .height(Length::Fixed(area.height));
    let outline = iced::widget::canvas(Outline {
        image,
        rectangle: model.rectangle,
    })
    .width(Length::Fixed(area.width))
    .height(Length::Fixed(area.height));
    let mut content: Vec<Element<'a, Message>> = vec![placed.into(), outline.into()];
    let caption = |words: &'static str, ink: Token| {
        container(
            text(words)
                .size(theme::SIZE_SMALL_CAPTION)
                .style(theme::ink(ink)),
        )
        .padding([1.0, 4.0])
        .style(|active: &Theme| {
            theme::chrome_surface(active, Token::ChromeBorder, theme::BORDER_WIDTH * 3.0)
        })
    };
    if model.after {
        content.push(
            container(caption("After", Token::TextBright))
                .padding(3)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Left)
                .align_y(Vertical::Top)
                .into(),
        );
    }
    let quiet = match model.shows {
        Shows::Pending | Shows::Updating => Some("Updating"),
        Shows::Unavailable => Some("Overview unavailable"),
        Shows::Ready => None,
    };
    if let Some(words) = quiet {
        content.push(
            container(caption(words, Token::TextSecondary))
                .padding(3)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Horizontal::Left)
                .align_y(Vertical::Bottom)
                .into(),
        );
    }
    let bounds = model.card.bounds;
    let card = container(
        stack(content)
            .width(Length::Fixed(area.width))
            .height(Length::Fixed(area.height)),
    )
    .padding(PADDING)
    .width(Length::Fixed(bounds.width))
    .height(Length::Fixed(bounds.height))
    .style(|active: &Theme| theme::chrome_surface(active, Token::ChromeBorder, RADIUS));
    let input = Element::new(MinimapInput {
        content: card.into(),
        inset: PADDING,
        dragging: model.dragging,
        rectangle: model.rectangle,
    });
    container(input)
        .padding(Padding {
            top: bounds.y,
            left: bounds.x,
            right: 0.0,
            bottom: 0.0,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Left)
        .align_y(Vertical::Top)
        .into()
}

/// The viewport's rectangle: a light line between two dark ones, so it reads over any photograph,
/// kept inside the thumbnail so a rectangle that is the whole stage still shows.
struct Outline {
    image: Rectangle,
    rectangle: Option<Rectangle>,
}

impl canvas::Program<Message, Theme> for Outline {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        active: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let Some(rectangle) = self.rectangle else {
            return Vec::new();
        };
        let mut frame = Frame::new(renderer, bounds.size());
        let keep = 1.5;
        let left = rectangle
            .x
            .clamp(self.image.x + keep, self.image.x + self.image.width - keep);
        let top = rectangle
            .y
            .clamp(self.image.y + keep, self.image.y + self.image.height - keep);
        let right = (rectangle.x + rectangle.width)
            .clamp(self.image.x + keep, self.image.x + self.image.width - keep);
        let bottom = (rectangle.y + rectangle.height)
            .clamp(self.image.y + keep, self.image.y + self.image.height - keep);
        let path = Path::rectangle(
            Point::new(left, top),
            Size::new((right - left).max(1.0), (bottom - top).max(1.0)),
        );
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(Color {
                    a: 0.75,
                    ..theme::CANVAS_BLACK
                })
                .with_width(3.0),
        );
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(active.palette().get(Token::TextBright))
                .with_width(1.0),
        );
        vec![frame.into_geometry()]
    }
}

/// The card's pointer capture ([module docs](self)).
struct MinimapInput<'a> {
    content: Element<'a, Message>,
    /// From the card's edge to its image area, which the published points are relative to.
    inset: f32,
    /// The editor holds a drag: moves are published.
    dragging: bool,
    /// The viewport's rectangle in the image area, for the pointer shown over it.
    rectangle: Option<Rectangle>,
}

#[derive(Default)]
struct Capture {
    /// The left button went down on the card and has not come up.
    pressed: bool,
    modifiers: iced::keyboard::Modifiers,
}

/// What a wheel step scrolls the scrollable by, as the scrollable itself counts it: lines are 60
/// logical points, and Shift turns a vertical wheel horizontal (macOS swaps the axes itself).
pub(crate) fn wheel_offset(delta: mouse::ScrollDelta, shift: bool) -> Vector {
    match delta {
        mouse::ScrollDelta::Lines { x, y } => {
            let (x, y) = if cfg!(target_os = "macos") && shift {
                (y, x)
            } else {
                (x, y)
            };
            let movement = if shift {
                Vector::new(y, x)
            } else {
                Vector::new(x, y)
            };
            movement * -60.0
        }
        mouse::ScrollDelta::Pixels { x, y } => Vector::new(-x, -y),
    }
}

impl MinimapInput<'_> {
    fn local(&self, bounds: Rectangle, point: Point) -> Point {
        Point::new(
            point.x - bounds.x - self.inset,
            point.y - bounds.y - self.inset,
        )
    }
}

impl Widget<Message, Theme, Renderer> for MinimapInput<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Capture>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Capture::default())
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
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
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
        let bounds = layout.bounds();
        let over = cursor.position_over(bounds);
        let capture = tree.state.downcast_mut::<Capture>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(button)) if over.is_some() => {
                if *button == mouse::Button::Left
                    && let Some(point) = over
                {
                    capture.pressed = true;
                    shell.publish(Message::View(ViewMessage::MinimapPress(
                        self.local(bounds, point),
                    )));
                }
                // Every button pressed on the card is the card's: nothing under it is pressed.
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if capture.pressed => {
                if self.dragging {
                    shell.publish(Message::View(ViewMessage::MinimapDrag(
                        self.local(bounds, *position),
                    )));
                }
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::ButtonReleased(button)) if capture.pressed => {
                if *button == mouse::Button::Left {
                    capture.pressed = false;
                    shell.publish(Message::View(ViewMessage::MinimapRelease));
                }
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::ButtonReleased(_)) if over.is_some() => {
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if over.is_some() => {
                let offset = wheel_offset(*delta, capture.modifiers.shift());
                shell.publish(Message::View(ViewMessage::MinimapWheel(offset.x, offset.y)));
                shell.capture_event();
                return;
            }
            Event::Touch(_) if over.is_some() || capture.pressed => {
                shell.capture_event();
                return;
            }
            Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers)) => {
                capture.modifiers = *modifiers;
            }
            // The editor ends the drag on focus loss; a release it never sees ends the capture.
            Event::Window(iced::window::Event::Unfocused) => capture.pressed = false,
            _ => {}
        }
        self.content.as_widget_mut().update(
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
        renderer: &mut Renderer,
        active: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            active,
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
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        if tree.state.downcast_ref::<Capture>().pressed {
            return mouse::Interaction::Grabbing;
        }
        let Some(point) = cursor.position_over(bounds) else {
            return mouse::Interaction::None;
        };
        // Over the card the pointer is the card's, so nothing under it shows its own.
        match self.rectangle {
            Some(rectangle) if rectangle.contains(self.local(bounds, point)) => {
                mouse::Interaction::Grab
            }
            _ => mouse::Interaction::Pointer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_sits_at_the_lower_right_clear_of_the_strip_and_hugs_the_stage() {
        // A wide canvas: the card is the whole 180 × 120 area for a 3:2 stage, 12 pt in from the
        // right and bottom edges.
        let card = card(Size::new(1200.0, 800.0), (6000.0, 4000.0), 6).unwrap();
        assert_eq!(card.area, Size::new(180.0, 120.0));
        assert_eq!(card.bounds.width, 188.0);
        assert_eq!(card.bounds.height, 128.0);
        assert_eq!(card.bounds.x, 1200.0 - 12.0 - 188.0);
        assert_eq!(card.bounds.y, 800.0 - 12.0 - 128.0);
        // A portrait stage keeps the 120 pt height and narrows.
        let portrait = super::card(Size::new(1200.0, 800.0), (4000.0, 6000.0), 6).unwrap();
        assert_eq!(portrait.area, Size::new(80.0, 120.0));
        // A narrow canvas: the strip's six tools would reach the card, so it rises above the strip.
        let narrow = super::card(Size::new(480.0, 700.0), (6000.0, 4000.0), 6).unwrap();
        assert!(narrow.area.width <= 480.0 * SHARE + 0.001);
        let strip_top = 700.0 - theme::CHROME_INSET - theme::STRIP_HEIGHT;
        assert!(narrow.bounds.y + narrow.bounds.height <= strip_top);
        // A canvas too small for an overview has none.
        assert!(super::card(Size::new(60.0, 50.0), (6000.0, 4000.0), 6).is_none());
        assert!(super::card(Size::new(1200.0, 800.0), (0.0, 4000.0), 6).is_none());
    }

    #[test]
    fn a_wheel_over_the_card_scrolls_as_the_scrollable_counts_it() {
        assert_eq!(
            wheel_offset(mouse::ScrollDelta::Pixels { x: 3.0, y: -120.0 }, false),
            Vector::new(-3.0, 120.0)
        );
        assert_eq!(
            wheel_offset(mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }, false),
            Vector::new(0.0, 60.0)
        );
    }
}
