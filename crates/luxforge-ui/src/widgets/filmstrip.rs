//! Develop's filmstrip, as the development-set board draws it (`.fstrip`, `.fthumb`): a 92 pt strip
//! under the canvas holding the development set — a header with the previous and next buttons, the
//! set's name and a caption, and a row of cells, the active one outlined in the accent.
//!
//! Cells take the caller's image handles, made once and held, and the strip draws only the
//! caller's window of the set ([`crate::visible_window`], [`filmstrip_capacity`]), whatever the
//! set's size.

use super::icon_button::{Icon, IconButtonModel, sized_icon_button};
use super::loupe::fitted_image;
use crate::theme;
use crate::{Element, Token};
use iced::widget::image::Handle;
use iced::widget::text::Wrapping;
use iced::widget::{Row, Space, button, column, container, row, text, tooltip};
use iced::{Alignment, Length, Padding};

/// Plain data for the filmstrip: the set's name and a window of its photographs.
#[derive(Debug, Clone, PartialEq)]
pub struct FilmstripModel {
    /// The set's name: `Development set`.
    pub title: String,
    /// A right-aligned caption; `None` shows the active photograph's place in the set (`3 of 18`).
    pub caption: Option<String>,
    /// The cells to draw, in order; photograph `first + i` of the set is `cells[i]`, `None` while
    /// its preview is not read yet.
    pub cells: Vec<Option<Handle>>,
    /// The set's index of `cells[0]`, from 0.
    pub first: usize,
    /// How many photographs the set holds.
    pub total: usize,
    /// The set's index of the photograph Develop shows.
    pub active: Option<usize>,
    /// Selection flags for the visible cells.
    pub selected: Vec<bool>,
}

/// A cell press, with modifiers and a secondary-menu position in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilmstripPress {
    pub index: usize,
    pub command: bool,
    pub shift: bool,
    pub context: Option<iced::Point>,
}

struct CellPress<'a, M> {
    index: usize,
    publish: Box<dyn Fn(FilmstripPress) -> M + 'a>,
}
impl<'a, M: 'a, Theme, Renderer: iced::advanced::renderer::Renderer>
    super::decorator::Decoration<'a, M, Theme, Renderer> for CellPress<'a, M>
{
    type State = iced::keyboard::Modifiers;
    fn update(
        &mut self,
        content: &mut iced::Element<'a, M, Theme, Renderer>,
        tree: &mut iced::advanced::widget::Tree,
        event: &iced::Event,
        layout: iced::advanced::Layout<'_>,
        cursor: iced::mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, M>,
        viewport: &iced::Rectangle,
    ) {
        if let iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers)) = event {
            *tree.state.downcast_mut::<Self::State>() = *modifiers;
        }
        if let iced::Event::Mouse(iced::mouse::Event::ButtonPressed(button)) = event
            && matches!(
                button,
                iced::mouse::Button::Left | iced::mouse::Button::Right
            )
            && let Some(at) = cursor.position_over(layout.bounds())
        {
            let modifiers = *tree.state.downcast_ref::<Self::State>();
            let context = (*button == iced::mouse::Button::Right
                || (cfg!(target_os = "macos") && modifiers.control()))
            .then_some(at);
            shell.publish((self.publish)(FilmstripPress {
                index: self.index,
                command: modifiers.command(),
                shift: modifiers.shift(),
                context,
            }));
            shell.capture_event();
            return;
        }
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
}

/// Renders the filmstrip, [`theme::FILMSTRIP_HEIGHT`] tall and filling its width. Pressing a cell
/// publishes `on_select` with its index in the set; a header button whose message is `None` is
/// disabled.
pub fn filmstrip<'a, M: Clone + 'a>(
    model: &FilmstripModel,
    on_select: impl Fn(FilmstripPress) -> M + 'a,
    on_previous: Option<M>,
    on_next: Option<M>,
) -> Element<'a, M> {
    let on_select = std::rc::Rc::new(on_select);
    let mut header = row![
        step_button(
            Icon::ChevronLeft,
            "Previous photograph (\u{2190})",
            on_previous
        ),
        step_button(Icon::ChevronRight, "Next photograph (\u{2192})", on_next),
        text(model.title.clone())
            .size(theme::SIZE_CAPTION)
            .wrapping(Wrapping::None)
            .style(theme::ink(Token::TextLabel)),
        Space::new().width(Length::Fill),
    ]
    .spacing(theme::FILMSTRIP_HEADER_SPACING)
    .align_y(Alignment::Center);
    if let Some(caption) = filmstrip_caption(model) {
        header = header.push(
            text(caption)
                .size(theme::SIZE_CAPTION)
                .wrapping(Wrapping::None)
                .style(theme::ink(Token::TextIdentity)),
        );
    }
    let cells = Row::with_children(model.cells.iter().enumerate().map(|(offset, cell)| {
        let index = model.first + offset;
        let active = model.active == Some(index);
        let cell: Element<'a, M> = button(
            container(fitted_image(
                cell.clone(),
                theme::FILMSTRIP_IMAGE_WIDTH,
                theme::FILMSTRIP_IMAGE_HEIGHT,
            ))
            .center(Length::Fill),
        )
        .on_press(on_select(FilmstripPress {
            index,
            command: false,
            shift: false,
            context: None,
        }))
        .padding(0)
        .width(Length::Fixed(theme::FILMSTRIP_CELL_WIDTH))
        .height(Length::Fixed(theme::FILMSTRIP_CELL_HEIGHT))
        .style(theme::image_cell(
            active || model.selected.get(offset).copied().unwrap_or(false),
            theme::FILMSTRIP_CELL_RADIUS,
            if active {
                theme::FILMSTRIP_ACTIVE_OUTLINE
            } else {
                1.0
            },
        ))
        .into();
        let on_select = on_select.clone();
        super::decorator::decorate(
            cell,
            CellPress {
                index,
                publish: Box::new(move |press| on_select(press)),
            },
        )
    }))
    .spacing(theme::FILMSTRIP_CELL_SPACING)
    .align_y(Alignment::Center);
    let strip = column![
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::BORDER_WIDTH))
            .style(theme::select_bar_rule),
        container(header)
            .padding([0.0, theme::FILMSTRIP_PADDING])
            .width(Length::Fill)
            .height(Length::Fixed(theme::FILMSTRIP_HEADER_HEIGHT)),
        container(cells)
            .padding(Padding {
                top: 0.0,
                right: theme::FILMSTRIP_PADDING,
                bottom: theme::FILMSTRIP_BOTTOM,
                left: theme::FILMSTRIP_PADDING,
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .center_y(Length::Fill)
            .clip(true),
    ];
    container(strip)
        .width(Length::Fill)
        .height(Length::Fixed(theme::FILMSTRIP_HEIGHT))
        .style(theme::select_bar_surface)
        .into()
}

/// A 20 pt header button holding a 12 pt chevron, disabled without a message.
fn step_button<'a, M: Clone + 'a>(glyph: Icon, label: &str, on_press: Option<M>) -> Element<'a, M> {
    let enabled = on_press.is_some();
    sized_icon_button(
        &IconButtonModel {
            icon: glyph,
            tooltip: label.to_owned(),
            enabled,
            selected: false,
        },
        on_press,
        theme::HEADER_BUTTON_SIZE,
        theme::SMALL_ICON_SIZE,
        if enabled {
            Token::TextSecondary
        } else {
            Token::TextFaint
        },
        tooltip::Position::Top,
    )
}

/// The header's right-aligned caption: the caller's, or else the active photograph's place in the
/// set, or nothing when no photograph is active.
pub(crate) fn filmstrip_caption(model: &FilmstripModel) -> Option<String> {
    model.caption.clone().or_else(|| {
        model
            .active
            .filter(|&active| active < model.total)
            .map(|active| {
                format!(
                    "{} of {}",
                    super::title_actions::group_digits(active + 1),
                    super::title_actions::group_digits(model.total)
                )
            })
    })
}

/// How many cells fit a filmstrip `width` points wide, inside its insets.
pub fn filmstrip_capacity(width: f32) -> usize {
    let room = width - 2.0 * theme::FILMSTRIP_PADDING + theme::FILMSTRIP_CELL_SPACING;
    let pitch = theme::FILMSTRIP_CELL_WIDTH + theme::FILMSTRIP_CELL_SPACING;
    if room <= 0.0 || !room.is_finite() {
        0
    } else {
        (room / pitch).floor() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_settings_filmstrip_presses_keep_modifiers_and_secondary_position() {
        use super::super::decorator::Decorated;
        use iced::advanced::{Layout, Shell, Widget, layout, widget::Tree};
        use iced::{
            Event, Point, Rectangle, Size,
            keyboard::{Event as K, Modifiers},
            mouse::{Button, Cursor, Event as Mouse},
        };
        let mut widget: Decorated<'_, FilmstripPress, _, iced::Theme, ()> = Decorated {
            content: iced::Element::new(Space::new().width(78).height(54)),
            decoration: CellPress {
                index: 7,
                publish: Box::new(|press| press),
            },
        };
        let mut tree = Tree::new(&widget as &dyn Widget<FilmstripPress, iced::Theme, ()>);
        let node = layout::Node::new(Size::new(78.0, 54.0));
        let mut send = |event: Event, at: Point| {
            let mut messages = Vec::new();
            widget.update(
                &mut tree,
                &event,
                Layout::new(&node),
                Cursor::Available(at),
                &(),
                &mut iced::advanced::clipboard::Null,
                &mut Shell::new(&mut messages),
                &Rectangle::new(Point::ORIGIN, Size::new(78.0, 54.0)),
            );
            messages
        };
        let at = Point::new(20.0, 20.0);
        send(
            Event::Keyboard(K::ModifiersChanged(Modifiers::COMMAND | Modifiers::SHIFT)),
            at,
        );
        assert_eq!(
            send(Event::Mouse(Mouse::ButtonPressed(Button::Left)), at),
            [FilmstripPress {
                index: 7,
                command: true,
                shift: true,
                context: None
            }]
        );
        send(Event::Keyboard(K::ModifiersChanged(Modifiers::empty())), at);
        assert_eq!(
            send(Event::Mouse(Mouse::ButtonPressed(Button::Right)), at),
            [FilmstripPress {
                index: 7,
                command: false,
                shift: false,
                context: Some(at)
            }]
        );
        assert!(
            send(
                Event::Mouse(Mouse::ButtonPressed(Button::Left)),
                Point::new(100.0, 100.0)
            )
            .is_empty()
        );
    }

    fn model(caption: Option<&str>, active: Option<usize>) -> FilmstripModel {
        FilmstripModel {
            title: "Development set".into(),
            caption: caption.map(Into::into),
            cells: vec![None; 3],
            first: 40,
            total: 1200,
            active,
            selected: Vec::new(),
        }
    }

    #[test]
    fn the_caption_is_the_callers_or_the_active_place_in_the_set() {
        assert_eq!(
            filmstrip_caption(&model(Some("From Konstanz"), Some(41))),
            Some("From Konstanz".to_owned())
        );
        assert_eq!(
            filmstrip_caption(&model(None, Some(41))),
            Some("42 of 1,200".to_owned())
        );
        assert_eq!(filmstrip_caption(&model(None, None)), None);
        assert_eq!(filmstrip_caption(&model(None, Some(1200))), None);
    }

    /// development-set.png: the strip between the side panels at 1440 pt holds eight cells and
    /// more room; each cell is 78 pt, 4 pt apart, inside 10 pt insets.
    #[test]
    fn capacity_is_whole_cells_inside_the_insets() {
        assert_eq!(filmstrip_capacity(0.0), 0);
        assert_eq!(filmstrip_capacity(20.0), 0);
        assert_eq!(filmstrip_capacity(98.0), 1, "10 + 78 + 10");
        assert_eq!(filmstrip_capacity(97.9), 0);
        assert_eq!(filmstrip_capacity(180.0), 2, "10 + 78 + 4 + 78 + 10");
        assert_eq!(filmstrip_capacity(900.0), 10);
        assert_eq!(filmstrip_capacity(f32::NAN), 0);
    }

    #[test]
    fn every_state_builds() {
        for (on_previous, on_next) in [(Some(()), Some(())), (None, None)] {
            let _: Element<'_, ()> =
                filmstrip(&model(None, Some(41)), |_| (), on_previous, on_next);
        }
    }
}
