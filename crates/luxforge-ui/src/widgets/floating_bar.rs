//! A floating bar surface holding arbitrary children, and the draft bar built on it.

use super::button_row::{ButtonSize, ButtonTone, LabelledButtonModel, labelled_button};
use super::icon_button::{Icon, icon};
use crate::theme;
use iced::widget::text::Wrapping;
use iced::widget::{Row, Space, container, text, tooltip};
use iced::{Alignment, Element, Length, Theme};

/// Renders a floating, bordered bar holding `children` laid out in a row, in order: the canvas
/// chrome's surface, [`theme::DRAFT_BAR_HEIGHT`] tall.
pub fn floating_bar<'a, M: Clone + 'a>(children: Vec<Element<'a, M>>) -> Element<'a, M> {
    let mut content = Row::new()
        .spacing(theme::DRAFT_BAR_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill);

    for child in children {
        content = content.push(child);
    }

    container(content)
        .padding(theme::DRAFT_BAR_PADDING)
        .height(Length::Fixed(theme::DRAFT_BAR_HEIGHT))
        .style(|_: &Theme| theme::chrome_surface(theme::CHROME_BORDER, theme::CHROME_RADIUS))
        .into()
}

/// Plain data for the draft bar a canvas mode shows while its draft is open.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftBarModel {
    /// The bar's lead, in the accent: the mode's name, or the name of the mask a gesture edits.
    pub title: String,
    /// What the gesture edits, after the lead: its kind's icon when the library draws one, then its
    /// name and mode, such as `Radial 1 · Add`. `None` where the lead already says it all, as the
    /// crop's does.
    pub subject: Option<DraftSubject>,
    /// One line of the draft's own numbers.
    pub readout: String,
    /// Why Apply is refused, stated on hover; `None` while Apply is enabled.
    pub apply_reason: Option<String>,
    /// How the draft ends.
    pub finish: DraftFinish,
}

/// What a draft bar's gesture edits: an icon, when there is one, and a name.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftSubject {
    pub icon: Option<Icon>,
    pub label: String,
}

/// The buttons that end a draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DraftFinish {
    /// Cancel with its `esc` hint and Apply as the primary button with its return hint: a draft
    /// that waits for a decision.
    #[default]
    Apply,
    /// Done alone, as the primary button with the `esc` hint, sending the cancel message: a gesture
    /// whose every stroke already committed on release, so there is nothing left to apply and
    /// nothing to discard, only a tool to put down.
    Done,
}

/// The draft bar: the lead in the accent, what the gesture edits, the readout, a rule, then the
/// buttons [`DraftFinish`] names. Apply is disabled when `on_apply` is `None`, and says why on hover
/// when the model gives a reason; Done ignores `on_apply` and sends `on_cancel`.
pub fn draft_bar<'a, M: Clone + 'a>(
    model: &DraftBarModel,
    on_cancel: M,
    on_apply: Option<M>,
) -> Element<'a, M> {
    let title = text(model.title.clone())
        .size(theme::SIZE_CONTROL)
        .font(theme::FONT_SEMIBOLD)
        .wrapping(Wrapping::None)
        .color(theme::ACCENT);
    // With a subject the subject takes the label ink and the numbers step back, as the mask board
    // sets them; alone, the numbers are the line the lead names.
    let readout_ink = if model.subject.is_some() {
        theme::TEXT_SECONDARY
    } else {
        theme::TEXT_LABEL
    };
    let readout = text(model.readout.clone())
        .size(theme::SIZE_CONTROL)
        .wrapping(Wrapping::None)
        .color(readout_ink);
    let rule = container(Space::new())
        .width(Length::Fixed(theme::BORDER_WIDTH))
        .height(Length::Fixed(theme::STRIP_RULE_HEIGHT))
        .style(|_: &Theme| container::Style::default().background(theme::STRIP_RULE));
    let button = |label: &str, key: &str, tone: ButtonTone, enabled: bool| LabelledButtonModel {
        label: label.to_owned(),
        icon: None,
        key_hint: Some(key.to_owned()),
        tone,
        size: ButtonSize::Regular,
        fill: false,
        enabled,
    };
    let mut children: Vec<Element<'a, M>> = vec![title.into()];
    if let Some(subject) = &model.subject {
        children.push(subject_view(subject));
    }
    children.push(readout.into());
    children.push(rule.into());
    match model.finish {
        DraftFinish::Done => children.push(labelled_button(
            &button("Done", "esc", ButtonTone::Primary, true),
            Some(on_cancel),
        )),
        DraftFinish::Apply => {
            children.push(labelled_button(
                &button("Cancel", "esc", ButtonTone::Control, true),
                Some(on_cancel),
            ));
            let enabled = on_apply.is_some();
            let apply = labelled_button(
                &button("Apply", "return", ButtonTone::Primary, enabled),
                on_apply,
            );
            children.push(match &model.apply_reason {
                // A refused Apply says why on hover instead of going quiet.
                Some(reason) => tooltip(
                    apply,
                    container(
                        text(reason.clone())
                            .size(theme::SIZE_CAPTION)
                            .color(theme::TEXT_PRIMARY),
                    )
                    .padding(theme::TOOLTIP_PADDING)
                    .style(theme::bar_surface),
                    tooltip::Position::Bottom,
                )
                .into(),
                None => apply,
            });
        }
    }
    floating_bar(children)
}

/// The subject: the kind's icon and the name beside it, in the label ink.
fn subject_view<'a, M: 'a>(subject: &DraftSubject) -> Element<'a, M> {
    let mut row = Row::new()
        .spacing(theme::DRAFT_BAR_SUBJECT_SPACING)
        .align_y(Alignment::Center);
    if let Some(glyph) = subject.icon {
        row = row.push(icon(glyph, theme::BUTTON_ICON_SIZE, theme::TEXT_LABEL));
    }
    row.push(
        text(subject.label.clone())
            .size(theme::SIZE_CONTROL)
            .wrapping(Wrapping::None)
            .color(theme::TEXT_LABEL),
    )
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every form the bar takes builds: the crop's lead alone, a gesture's lead with a subject with
    /// and without an icon, a refused Apply, and Done in place of Apply.
    #[test]
    fn every_draft_bar_form_builds() {
        for subject in [
            None,
            Some(DraftSubject {
                icon: Some(Icon::Mask),
                label: "Radial 1 \u{b7} Add".into(),
            }),
            Some(DraftSubject {
                icon: None,
                label: "Brush 1 \u{b7} Subtract".into(),
            }),
        ] {
            for finish in [DraftFinish::Apply, DraftFinish::Done] {
                for apply_reason in [None, Some("Paint a stroke first".to_owned())] {
                    let model = DraftBarModel {
                        title: "Face".into(),
                        subject: subject.clone(),
                        readout: "0.1800 \u{d7} 0.2400".into(),
                        apply_reason: apply_reason.clone(),
                        finish,
                    };
                    let _: Element<'_, ()> = draft_bar(&model, (), Some(()));
                    let _: Element<'_, ()> = draft_bar(&model, (), None);
                }
            }
        }
    }
}
