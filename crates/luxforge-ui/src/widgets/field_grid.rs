//! A component's fields in two columns under its row, as the mask-panels board draws `.fields`: a
//! radial's `x 0.52  y 0.31`, `radius x 0.180  radius y 0.240`, `angle −12°  feather 60`.
//!
//! Each field is a label and a compact value box that edits exactly as a [`crate::number_field`]
//! does — a press opens it for typing, the caller parses and commits, a double-click on the label
//! resets — at [`theme::GRID_ROW_HEIGHT`] with a [`theme::GRID_FIELD_WIDTH`] box. The caller passes
//! the fields in order and they are laid out two to a row, left then right; an odd last field
//! leaves the right column empty. A field's invalid message is drawn under its row.

use super::double_click::double_click_when;
use super::number_field::{BoxSize, NumberFieldModel, invalid, outside_unit, sized_field_box};
use super::text::error_caption;
use crate::theme;
use crate::{Element, Token};
use iced::widget::text::Wrapping;
use iced::widget::{Column, Row, Space, container, mouse_area, row, text};
use iced::{Alignment, Length, Padding};

/// One field of a grid, with the messages a number field publishes.
pub struct GridField<'a, M> {
    pub model: NumberFieldModel,
    /// A press on the box: open it for typing.
    pub on_edit_start: M,
    /// The typed text changed.
    pub on_text: Box<dyn Fn(String) -> M + 'a>,
    /// Return in the box: commit.
    pub on_submit: M,
    /// A double-click on the label: reset.
    pub on_reset: M,
    /// A right press on the field: the caller's context menu (Copy as JSON request).
    pub on_menu: Option<M>,
}

/// How many rows `fields` fields take.
#[cfg(test)]
pub(crate) const fn field_grid_rows(fields: usize) -> usize {
    fields.div_ceil(2)
}

/// The grid's height for `fields` fields with no invalid message showing, its padding included.
#[cfg(test)]
pub(crate) fn field_grid_height(fields: usize) -> f32 {
    let rows = field_grid_rows(fields);
    if rows == 0 {
        return 0.0;
    }
    rows as f32 * theme::GRID_ROW_HEIGHT
        + (rows - 1) as f32 * theme::GRID_ROW_SPACING
        + theme::GRID_PADDING_TOP
        + theme::GRID_PADDING_BOTTOM
}

/// Renders the fields two to a row, inset under their component row.
pub fn field_grid<'a, M: Clone + 'a>(fields: Vec<GridField<'a, M>>) -> Element<'a, M> {
    let mut rows = Column::new().spacing(theme::GRID_ROW_SPACING);
    let mut fields = fields.into_iter();
    while let Some(left) = fields.next() {
        let right = fields.next();
        let errors: Vec<String> = std::iter::once(&left)
            .chain(right.as_ref())
            .filter_map(|field| invalid(&field.model))
            .collect();
        let right: Element<'a, M> = match right {
            Some(field) => cell(field),
            None => Space::new().width(Length::Fill).into(),
        };
        rows = rows.push(
            row![cell(left), right]
                .spacing(theme::GRID_COLUMN_SPACING)
                .width(Length::Fill),
        );
        for message in errors {
            rows = rows.push(error_caption(message));
        }
    }
    container(rows)
        .padding(Padding {
            top: theme::GRID_PADDING_TOP,
            right: 0.0,
            bottom: theme::GRID_PADDING_BOTTOM,
            left: theme::COMPONENT_DETAIL_INDENT,
        })
        .width(Length::Fill)
        .into()
}

/// One field: its label at the left of its column and its box at the right.
fn cell<'a, M: Clone + 'a>(field: GridField<'a, M>) -> Element<'a, M> {
    let GridField {
        model,
        on_edit_start,
        on_text,
        on_submit,
        on_reset,
        on_menu,
    } = field;
    // The grid is the compact form of a field list, and the design sets its labels in lower case
    // (`radius x`) so a pair of columns reads as coordinates rather than as a stack of titles. It is
    // display only: the label a menu, a tooltip or the evidence names is the caller's.
    let label = double_click_when(
        text(model.label.to_lowercase())
            .size(theme::SIZE_GRID_FIELD)
            .wrapping(Wrapping::None)
            .style(theme::ink(if model.enabled {
                Token::TextLabel
            } else {
                Token::TextTertiary
            })),
        on_reset,
        model.enabled,
    );
    let mut line = Row::new()
        .push(container(label).width(Length::Fill))
        .push(sized_field_box(
            &model,
            BoxSize::GRID,
            on_edit_start,
            on_text,
            on_submit,
        ));
    if let Some(unit) = outside_unit(&model.unit) {
        line = line.push(unit);
    }
    let line = line
        .spacing(theme::GRID_LABEL_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fixed(theme::GRID_ROW_HEIGHT))
        .width(Length::Fill);
    match on_menu {
        Some(menu) => mouse_area(line).on_right_press(menu).into(),
        None => line.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ValueEdit;

    #[test]
    fn fields_go_two_to_a_row() {
        assert_eq!(field_grid_rows(0), 0);
        assert_eq!(field_grid_rows(1), 1);
        assert_eq!(field_grid_rows(4), 2);
        assert_eq!(field_grid_rows(6), 3);
        assert_eq!(field_grid_rows(7), 4);
    }

    /// mask-panels.html: a radial's six fields are three 22 pt rows 2 pt apart, 2 pt above and 4 pt
    /// under.
    #[test]
    fn a_radials_fields_take_three_rows() {
        assert_eq!(field_grid_height(6), 3.0 * 22.0 + 2.0 * 2.0 + 6.0);
        assert_eq!(field_grid_height(0), 0.0);
    }

    #[test]
    fn every_state_builds() {
        let field = |label: &str, edit: ValueEdit, unit: Option<&str>| GridField {
            model: NumberFieldModel {
                id: None,
                label: label.into(),
                display: "0.52".into(),
                edit,
                unit: unit.map(Into::into),
                enabled: true,
            },
            on_edit_start: 0u8,
            on_text: Box::new(|_| 1),
            on_submit: 2,
            on_reset: 3,
            on_menu: Some(4),
        };
        let _: Element<'_, u8> = field_grid(vec![
            field("x", ValueEdit::Display, None),
            field("y", ValueEdit::Display, None),
            field("angle", ValueEdit::Display, Some("\u{b0}")),
            field(
                "feather",
                ValueEdit::Editing {
                    text: "600".into(),
                    invalid: Some("Range is 0 to 100".into()),
                },
                None,
            ),
            field("size", ValueEdit::Display, Some("px")),
        ]);
        let _: Element<'_, u8> = field_grid(Vec::new());
    }
}
