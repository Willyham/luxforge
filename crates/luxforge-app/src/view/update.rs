//! The update offer: a card in the same chrome as the workspace's other notices, at the bottom of
//! the workspace beside the usage statistics notice ([`super::usage`]), so it covers none of the
//! photograph's controls. It is not modal: it takes no focus and holds no key, and the rest of the
//! window stays usable around it ([design](../../../../docs/design/auto-update.md#the-desktop)).
use crate::{
    app::message::{Message, update::UpdateMessage},
    state::update::{NoticeButton, UpdateNotice},
};
use iced::{
    Alignment, Border, Length,
    widget::{Space, column, container, progress_bar, row, text},
};
use luxforge_ui::{ButtonSize, ButtonTone, Element, Icon, Theme, Token, icon, text_button, theme};

/// The download bar's height, the progress sheet's.
const BAR_HEIGHT: f32 = 5.0;

/// The offer's card: its title and lines, the download's bar while it runs, a refusal in the error
/// ink, and its buttons in a row under them, Install and relaunch the primary one.
pub(crate) fn card(model: &UpdateNotice) -> Element<'_, Message> {
    let mut words = column![
        text(model.title.clone())
            .size(theme::SIZE_NOTICE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextBright))
    ]
    .spacing(2.0)
    .width(Length::Fill);
    for line in &model.lines {
        words = words.push(
            text(line.clone())
                .size(theme::SIZE_NOTICE_BODY)
                .style(theme::ink(Token::TextSecondary)),
        );
    }
    if let Some(fraction) = model.progress {
        words = words.push(bar(fraction));
    }
    if let Some(error) = &model.error {
        words = words.push(
            text(error.clone())
                .size(theme::SIZE_NOTICE_BODY)
                .style(theme::ink(Token::Error)),
        );
    }
    let mut buttons = row![Space::new().width(Length::Fill)]
        .spacing(theme::BUTTON_ROW_SPACING)
        .align_y(Alignment::Center);
    for button in &model.buttons {
        let tone = match button {
            NoticeButton::Install | NoticeButton::Download => ButtonTone::Primary,
            _ => ButtonTone::Control,
        };
        // A call in flight holds the buttons that send one; the page and Later stay live.
        let sends = matches!(button, NoticeButton::Install | NoticeButton::Skip);
        let message =
            (!(sends && model.busy)).then_some(Message::Update(UpdateMessage::Notice(*button)));
        buttons = buttons.push(text_button(
            button.label(),
            tone,
            ButtonSize::Regular,
            message,
        ));
    }
    let content = row![
        icon(Icon::Information, theme::ICON_SIZE, Token::TextSecondary),
        column![words, buttons]
            .spacing(theme::NOTICE_SPACING)
            .width(Length::Fill),
    ]
    .spacing(theme::NOTICE_SPACING)
    .align_y(Alignment::Start);
    container(content)
        .padding(theme::NOTICE_PADDING)
        .width(Length::Fixed(theme::NOTICE_WIDTH))
        .style(|theme: &Theme| theme::chrome_surface(theme, Token::StripRule, theme::CHROME_RADIUS))
        .into()
}

/// The download's bar: filled as far as it has come, or empty while its length is unknown.
fn bar<'a>(fraction: Option<f32>) -> Element<'a, Message> {
    progress_bar(0.0..=1.0, fraction.unwrap_or(0.0))
        .length(Length::Fill)
        .girth(Length::Fixed(BAR_HEIGHT))
        .style(|theme: &Theme| progress_bar::Style {
            background: theme.palette().rail.into(),
            bar: theme.palette().accent.into(),
            border: Border {
                radius: (BAR_HEIGHT / 2.0).into(),
                width: 0.0,
                color: iced::Color::TRANSPARENT,
            },
        })
        .into()
}
