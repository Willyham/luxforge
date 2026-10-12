//! The unsupported-catalog sheet ([design](../../../../docs/design/auto-update.md#launching-past-an-unsupported-catalog)):
//! drawn on the window's panel background when the launch's catalog was made by another version,
//! as Develop's confirmation draws a sheet — a title, the body, a rule and the footer's two
//! buttons, Quit and the one primary action, Start a new catalog. It reads only
//! `state/unsupported_catalog.rs`'s model.
use crate::{
    app::message::{Message, unsupported_catalog::UnsupportedCatalogMessage as Sheet},
    state::unsupported_catalog::{QUIT_LABEL, SheetModel},
};
use iced::{
    Alignment, Length,
    widget::{Column, Space, column, container, row, text},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Element, LabelledButtonModel, Token, labelled_button, theme,
    truncated_text,
};

/// The sheet's width, as Develop's confirmation.
const WIDTH: f32 = 460.0;
const HEADER_HEIGHT: f32 = 42.0;
const FOOTER_HEIGHT: f32 = 48.0;
const INSET: f32 = 18.0;
const BODY_SPACING: f32 = 10.0;

fn sheet(message: Sheet) -> Message {
    Message::UnsupportedCatalog(message)
}

fn rule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(theme::BORDER_WIDTH))
        .style(theme::divider_surface)
        .into()
}

fn body_text<'a>(content: impl Into<String>, ink: Token) -> Element<'a, Message> {
    text(content.into())
        .size(theme::SIZE_NOTICE_BODY)
        .style(theme::ink(ink))
        .width(Length::Fill)
        .into()
}

fn button<'a>(
    label: &str,
    tone: ButtonTone,
    enabled: bool,
    message: Sheet,
) -> Element<'a, Message> {
    labelled_button(
        &LabelledButtonModel {
            label: label.into(),
            icon: None,
            key_hint: None,
            tone,
            size: ButtonSize::Regular,
            fill: false,
            enabled,
        },
        enabled.then(|| sheet(message)),
    )
}

/// The whole window: the sheet centred on the panel background.
pub(crate) fn screen<'a>(model: SheetModel) -> Element<'a, Message> {
    let header = container(
        text(model.title)
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextBright)),
    )
    .padding([0.0, INSET])
    .center_y(Length::Fixed(HEADER_HEIGHT));
    let mut body = Column::new()
        .spacing(BODY_SPACING)
        .push(truncated_text(
            model.catalog,
            theme::SIZE_CAPTION,
            theme::FONT,
            Token::TextTertiary,
        ))
        .push(body_text(model.formats, Token::Text))
        .push(body_text(model.unchanged, Token::TextSecondary))
        .push(
            row![
                body_text(model.releases, Token::TextSecondary),
                button(
                    model.link,
                    ButtonTone::Quiet,
                    model.quit_enabled,
                    Sheet::OpenReleases
                ),
            ]
            .spacing(theme::SPACING)
            .align_y(Alignment::Center),
        )
        .push(rule())
        .push(body_text(model.new_catalog, Token::TextSecondary));
    if let Some(problem) = model.problem {
        body = body.push(
            text(problem)
                .size(theme::SIZE_CAPTION)
                .color(theme::CLIPPING_HIGHLIGHT)
                .width(Length::Fill),
        );
    }
    let footer = container(
        row![
            Space::new().width(Length::Fill),
            button(
                QUIT_LABEL,
                ButtonTone::Control,
                model.quit_enabled,
                Sheet::Quit
            ),
            button(
                model.start,
                ButtonTone::Primary,
                model.start_enabled,
                Sheet::Start
            ),
        ]
        .spacing(theme::SHEET_SPACING)
        .align_y(Alignment::Center),
    )
    .padding([0.0, INSET])
    .center_y(Length::Fixed(FOOTER_HEIGHT))
    .width(Length::Fill)
    .style(theme::panel_surface);
    let card = container(column![
        header,
        rule(),
        container(body).padding([14.0, INSET]),
        rule(),
        footer,
    ])
    .width(Length::Fixed(WIDTH))
    .style(theme::sheet_surface)
    .clip(true);
    container(card)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(theme::SPACING * 2.0)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(theme::panel_surface)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::unsupported_catalog::{Phase, Refusal};
    use std::path::PathBuf;

    /// Every phase of the sheet builds: offered, starting, failed and with no name left.
    #[test]
    fn unsupported_catalog_sheet_views_build() {
        let refusal = Refusal {
            path: PathBuf::from("/Photos/catalog.sqlite"),
            format: Some(16),
            expected: 1,
            marked: true,
            detail: "catalog format 16 is not supported".into(),
        };
        let next = PathBuf::from("/Photos/catalog (format 1).sqlite");
        for (next, phase) in [
            (Some(next.as_path()), Phase::Offered),
            (Some(next.as_path()), Phase::Starting),
            (Some(next.as_path()), Phase::Failed("denied".into())),
            (None, Phase::Offered),
        ] {
            let _ = screen(SheetModel::new(&refusal, next, &phase, Some("no browser")));
        }
    }
}
