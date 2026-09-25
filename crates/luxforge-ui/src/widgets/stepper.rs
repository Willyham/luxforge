//! A value field with explicit decrement and increment buttons: one [`theme::FIELD_ROW_HEIGHT`]
//! row, the label at the left, then the decrement and increment buttons and the value box at the
//! right, as a number field has it.
//!
//! A stepper may also carry a rail, as crop-and-straighten.png draws the angle: the rail sits
//! between the decrement and increment buttons and fills the row, and it is the slider's own rail
//! line ([`super::slider`]), so its geometry, fill, zero tick, handle and halo are the slider's.
//! Dragging it publishes rail fractions the host maps to values, exactly as a slider does.

use super::icon_button::{Icon, IconButtonModel, header_icon_button};
use super::number_field::{NumberFieldModel, ValueEdit, field_box, field_label, outside_unit};
use super::slider::{RailDecoration, RailLine, rail_line};
use crate::theme;
use crate::widgets::text::error_caption;
use iced::widget::{Row, column, container};
use iced::{Alignment, Element, Length};

/// A stepper's optional rail, as plain data: the soft range it spans, the value on it, the step
/// Iced snaps its handle to, the zero its fill grows from, and whether its gesture is live (the
/// handle is accent and haloed).
#[derive(Debug, Clone, PartialEq)]
pub struct StepperRail {
    pub soft_min: f64,
    pub soft_max: f64,
    pub value: f64,
    pub step: f64,
    pub zero: Option<f64>,
    pub dragging: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StepperModel {
    pub field: NumberFieldModel,
    pub decrement_enabled: bool,
    pub increment_enabled: bool,
    pub decrement_tooltip: String,
    pub increment_tooltip: String,
    pub rail: Option<StepperRail>,
}

/// What a stepper's rail publishes: a rail fraction while it is dragged, and its release.
pub struct StepperRailMessages<'a, M> {
    pub on_change: Box<dyn Fn(f64) -> M + 'a>,
    pub on_release: M,
}

/// The rail's width in a stepper row `row_width` points wide with no label: the row less the two
/// buttons, the value box and the four gaps between them. crop-and-straighten.png's 274 pt row
/// leaves a 160 pt rail.
pub fn stepper_rail_width(row_width: f32) -> f32 {
    (row_width
        - 2.0 * theme::HEADER_BUTTON_SIZE
        - theme::FIELD_WIDTH
        - 3.0 * theme::FIELD_UNIT_SPACING)
        .max(0.0)
}

/// A model with a rail but no `rail` messages draws no rail: a rail that cannot be dragged would
/// read as one that can.
#[allow(clippy::too_many_arguments)]
pub fn stepper<'a, M: Clone + 'a>(
    model: &StepperModel,
    on_decrement: M,
    on_increment: M,
    on_edit_start: M,
    on_text: impl Fn(String) -> M + 'a,
    on_submit: M,
    on_reset: M,
    rail: Option<StepperRailMessages<'a, M>>,
) -> Element<'a, M> {
    let invalid = match &model.field.edit {
        ValueEdit::Editing { invalid, .. } => invalid.clone(),
        ValueEdit::Display => None,
    };
    let minus = header_icon_button(
        &IconButtonModel {
            icon: Icon::Minus,
            tooltip: model.decrement_tooltip.clone(),
            enabled: model.field.enabled && model.decrement_enabled,
            selected: false,
        },
        Some(on_decrement),
    );
    let plus = header_icon_button(
        &IconButtonModel {
            icon: Icon::Plus,
            tooltip: model.increment_tooltip.clone(),
            enabled: model.field.enabled && model.increment_enabled,
            selected: false,
        },
        Some(on_increment),
    );
    let rail = model.rail.as_ref().zip(rail).map(|(rail, messages)| {
        rail_line(
            &RailLine {
                soft_min: rail.soft_min,
                soft_max: rail.soft_max,
                value: rail.value,
                step: rail.step,
                shift_step: rail.step * 10.0,
                fine_step: rail.step / 10.0,
                zero: rail.zero,
                decoration: RailDecoration::Plain,
                over_range: None,
                dragging: rail.dragging,
                enabled: model.field.enabled,
            },
            messages.on_change,
            messages.on_release,
            on_reset.clone(),
        )
    });
    let mut line = Row::new();
    line = match rail {
        // The rail takes the row's slack, so a label (if any) keeps only its own width.
        Some(rail) => {
            if !model.field.label.is_empty() {
                line = line.push(field_label(&model.field, on_reset));
            }
            line.push(minus).push(rail).push(plus)
        }
        None => line
            .push(container(field_label(&model.field, on_reset)).width(Length::Fill))
            .push(minus)
            .push(plus),
    };
    line = line.push(field_box(&model.field, on_edit_start, on_text, on_submit));
    if let Some(unit) = outside_unit(&model.field.unit) {
        line = line.push(unit);
    }
    let line = line
        .spacing(theme::FIELD_UNIT_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fixed(theme::FIELD_ROW_HEIGHT))
        .width(Length::Fill);
    let mut body = column![line];
    if let Some(message) = invalid {
        body = body.push(error_caption(message));
    }
    body.into()
}

#[cfg(test)]
mod tests {
    use super::stepper_rail_width;
    use crate::geometry::{fraction_from_value, rail_geometry, zero_fraction};
    use crate::theme;

    /// crop-and-straighten.png, drafting, measured at 2x from the card's content edge: minus
    /// 0..40, rail 52..372, plus 384..424, value box 436..548. At 2.40° on −45..+45 the handle's
    /// centre is at 220 (219.8 here) and the zero sits under the handle, so no fill shows.
    #[test]
    fn the_angle_rail_lands_where_the_reference_draws_it() {
        let row = 274.0;
        let rail = stepper_rail_width(row);
        assert_eq!(rail, 160.0);
        let rail_start = theme::HEADER_BUTTON_SIZE + theme::FIELD_UNIT_SPACING;
        assert_eq!(rail_start, 26.0);
        let value = fraction_from_value(-45.0, 45.0, 2.4);
        let zero = zero_fraction(-45.0, 45.0, Some(0.0));
        let geometry = rail_geometry(rail, theme::THUMB_RADIUS, value, zero);
        assert!(((rail_start + geometry.handle) * 2.0 - 219.8).abs() < 0.1);
        let (from, to) = geometry.fill.expect("a fill from zero to the handle");
        let thumb = theme::THUMB_RADIUS - theme::THUMB_OUTLINE_WIDTH;
        assert!(from >= geometry.handle - thumb && to <= geometry.handle + thumb);
        // Plus and the value box follow the rail at the field spacing.
        let plus = rail_start + rail + theme::FIELD_UNIT_SPACING;
        assert_eq!(plus * 2.0, 384.0);
        assert_eq!(
            (plus + theme::HEADER_BUTTON_SIZE + theme::FIELD_UNIT_SPACING) * 2.0,
            436.0
        );
    }
}
