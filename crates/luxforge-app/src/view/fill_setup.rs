//! The setup of AI editing, drawn from a [`SetupLine`]: the Corrections band's setup line. Each
//! line is its sentence, wrapping in the panel's width, a thin bar while its install runs, and its detail with the one button beside it. The view decides
//! nothing: a press publishes the line's [`SetupAction`] through the caller's message.
use crate::{
    app::message::Message,
    state::fill_setup::{SetupAction, SetupLine},
};
use iced::{
    Alignment, Length,
    widget::{Column, container, progress_bar, row, text},
};
use luxforge_ui::{Element, Token, caption, theme};

/// A setup button: its label, whether it is the accent kind (Cancel is not), and what a press
/// sends, `None` while it cannot be pressed.
pub(crate) type Button<'b, 'a> = &'b dyn Fn(&str, bool, Option<Message>) -> Element<'a, Message>;

/// One setup line. `enabled` is the module's: nothing else is in flight for it. Cancel stops work
/// already running, so it stays pressable regardless.
pub(crate) fn line<'a>(
    line: &'a SetupLine,
    enabled: bool,
    send: &dyn Fn(SetupAction) -> Message,
    button: Button<'_, 'a>,
) -> Element<'a, Message> {
    let mut body = Column::new().spacing(4.0).width(Length::Fill).push(
        text(line.text.clone())
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::Text))
            .width(Length::Fill),
    );
    if let Some(fraction) = line.progress {
        body = body.push(progress_bar(0.0..=1.0, fraction as f32).girth(Length::Fixed(4.0)));
    }
    let press = line.action.as_ref().map(|(label, action)| {
        let cancel = action.always_pressable();
        let press = (enabled || cancel).then(|| send(action.clone()));
        button(label, !cancel, press)
    });
    let detail = line
        .detail
        .as_ref()
        .map(|detail| container(caption(detail.clone())).width(Length::Fill));
    body = match (detail, press) {
        (Some(detail), Some(press)) => body.push(
            row![detail, press]
                .spacing(theme::SPACING / 2.0)
                .align_y(Alignment::Center),
        ),
        (Some(detail), None) => body.push(detail),
        (None, Some(press)) => body.push(press),
        (None, None) => body,
    };
    body.into()
}
