//! What Select draws of merges beside the grid ([HDR merge design](../../../../docs/design/hdr-merge.md),
//! "In the desktop"): the merge menu a right-click opens over files, the merge preview sheet, the
//! notice offering Put back merge, and the Info panel's stack band. The pending row, the badges and
//! the originals' notes are the grid's own. Each reads only `state/select_merge.rs`'s model; the
//! sheet borrows its decoded picture and the frames' thumbnails, as the grid does.
use crate::{
    app::message::{Message, select::SelectMessage, select_merge::MergeMessage},
    layout::DIVIDER_WIDTH,
    state::select_merge::{
        MenuModel, MergeAction, PutBackModel, SheetModel, StackBand, Toggle,
        sheet::{
            EXPOSURE_HINT, FileId, FrameRow, LAST_SETTINGS_HINT, OVERLAY_HINT, Picture, Region,
            Tint, fit, hatch, stepped,
        },
    },
};
use iced::{
    Alignment, Color, ContentFit, Length, Padding, Point, Rectangle, Size,
    widget::{
        Column, Space, canvas, checkbox, column, container, image, image::Handle, mouse_area,
        opaque, progress_bar, row, scrollable, text, tooltip,
    },
};
use luxforge_ui::{
    ButtonSize, ButtonTone, Element, FilterOption, FilterSegmentsModel, Icon, IconButtonModel,
    LabelledButtonModel, MenuEntry, MenuItem, NoticeCardModel, Token, Tone as NoticeTone, caption,
    filter_segments, header_icon_button, labelled_button, menu_list, notice_card, popover,
    section_label, text_button, theme, truncated_text, with_tooltip,
};

fn act(action: MergeAction) -> Message {
    Message::Select(SelectMessage::Merge(MergeMessage::Act(action)))
}

/// The merge menu at the pointer: the kind's group, Merge HDR… with its hint, or a result's stack
/// actions; an entry the core refuses says why as its tooltip.
pub(crate) fn menu(model: &MenuModel, viewport: Size) -> Element<'_, Message> {
    let mut entries = Vec::with_capacity(model.entries.len() * 2 + 1);
    let mut lines = 0.0;
    if let Some(heading) = model.heading {
        entries.push(MenuEntry::Heading(heading.to_owned()));
        lines += 1.0;
    }
    for entry in &model.entries {
        entries.push(MenuEntry::Item(MenuItem {
            icon: None,
            label: entry.label.clone(),
            trailing: entry.trailing.clone(),
            on_press: entry.action.clone().map(act),
            reason: entry.reason.clone(),
        }));
        lines += 1.0;
        if let Some(hint) = entry.hint {
            entries.push(MenuEntry::Hint(hint.to_owned()));
            lines += 1.0;
        }
    }
    // Hints wrap to two lines at the menu's width; the menu is kept inside the grid with room
    // for them.
    let height = lines * theme::MENU_ITEM_HEIGHT + 2.0 * theme::MENU_PADDING;
    let x = model.x.min((viewport.width - theme::MENU_WIDTH).max(0.0));
    let y = model.y.min((viewport.height - height).max(0.0));
    let close = || act(MergeAction::CloseMenu);
    iced::widget::stack![
        mouse_area(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
        )
        .on_press(close())
        .on_right_press(close()),
        container(menu_list(entries)).padding(Padding {
            top: y,
            right: 0.0,
            bottom: 0.0,
            left: x,
        }),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The sheet's settings column, beside the picture.
const SIDE_WIDTH: f32 = 330.0;
/// The space the sheet leaves around it, over the window.
const SHEET_MARGIN: f32 = 40.0;
/// A frame's thumbnail.
const THUMB_WIDTH: f32 = 48.0;
const THUMB_HEIGHT: f32 = 32.0;
/// A frame's exposure stepper's value, between its buttons.
const STEPPER_VALUE_WIDTH: f32 = 44.0;
/// The Ref. column: one radio.
const REF_WIDTH: f32 = 28.0;
const RADIO_SIZE: f32 = 14.0;
/// The spacing of a fallback region's hatching, on screen.
const HATCH_SPACING: f32 = 7.0;
/// A deghost overlay label's height, and its text's inset.
const LABEL_HEIGHT: f32 = 20.0;
const LABEL_PAD: f32 = 6.0;
const LABEL_SWATCH: f32 = 9.0;

/// The deghost overlay's tints, drawn over the photograph and so fixed, not themed: violet for an
/// exposure below the reference, blue for the reference, teal above it.
const BELOW: Color = Color::from_rgb8(176, 140, 255);
const REFERENCE: Color = Color::from_rgb8(110, 160, 255);
const ABOVE: Color = Color::from_rgb8(90, 205, 185);
const LABEL_SURFACE: Color = Color::from_rgba(0.08, 0.08, 0.09, 0.9);

fn tint(tint: Tint) -> Color {
    match tint {
        Tint::Below => BELOW,
        Tint::Reference => REFERENCE,
        Tint::Above => ABOVE,
    }
}

fn faded(colour: Color, alpha: f32) -> Color {
    Color { a: alpha, ..colour }
}

/// Auto and Off, as a segmented control.
fn toggle<'a>(value: Toggle, on: impl Fn(Toggle) -> MergeAction + 'a) -> Element<'a, Message> {
    filter_segments(
        &FilterSegmentsModel {
            options: ["Auto", "Off"]
                .map(|label| FilterOption {
                    label: label.into(),
                    count: None,
                })
                .to_vec(),
            selected: usize::from(value == Toggle::Off),
            enabled: true,
        },
        move |index| {
            act(on(if index == 0 {
                Toggle::Auto
            } else {
                Toggle::Off
            }))
        },
    )
}

/// A labelled line of the sheet: its label at the left, its control at the right.
fn line<'a>(label: &str, control: Element<'a, Message>) -> Element<'a, Message> {
    row![
        text(label.to_owned())
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::TextLabel)),
        Space::new().width(Length::Fill),
        control,
    ]
    .align_y(Alignment::Center)
    .into()
}

fn rule<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Length::Fill)
        .height(Length::Fixed(DIVIDER_WIDTH))
        .style(theme::divider_surface)
        .into()
}

/// The merge preview sheet, over the whole window: the title with the frames' files, camera and
/// time; the picture the newest revision drew, with the deghost overlay while it is shown, or the
/// preview's progress or refusal in its place; the status line under it; the frames with their
/// exposure steppers and Ref., Alignment, Deghosting and Show deghost overlay; the folder and the
/// output; and the footer with Cancel and the primary Merge. `picture` is the revision's decoded picture, and `thumbnail` a frame's held preview.
pub(crate) fn sheet<'a>(
    model: &'a SheetModel,
    picture: Option<&'a Handle>,
    thumbnail: impl Fn(FileId) -> Option<&'a Handle>,
) -> Element<'a, Message> {
    let header = row![
        text(model.title.clone())
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::TextBright))
            .wrapping(text::Wrapping::None),
        container(truncated_text(
            model.subtitle.clone(),
            theme::SIZE_IDENTITY,
            theme::FONT,
            Token::TextSecondary,
        ))
        .width(Length::Fill),
        header_icon_button(
            &IconButtonModel {
                icon: Icon::Close,
                tooltip: "Cancel".into(),
                enabled: true,
                selected: false,
            },
            Some(act(MergeAction::SheetClose)),
        ),
    ]
    .spacing(theme::SPACING * 1.5)
    .align_y(Alignment::Center)
    .padding([theme::SPACING * 1.5, theme::SPACING * 2.0]);

    let status = row![
        text(model.status.clone())
            .size(theme::SIZE_CAPTION)
            .style(theme::ink(if model.status_refused {
                Token::Error
            } else {
                Token::TextSecondary
            })),
        Space::new().width(Length::Fill),
    ]
    .push(match model.note {
        Some(note) => caption(note),
        None => Space::new().into(),
    })
    .align_y(Alignment::Center);
    let left = column![picture_area(model, picture), status]
        .spacing(theme::SPACING * 1.5)
        .padding(theme::SPACING * 2.0)
        .width(Length::Fill)
        .height(Length::Fill);

    let body = row![
        left,
        container(Space::new())
            .width(Length::Fixed(DIVIDER_WIDTH))
            .height(Length::Fill)
            .style(theme::divider_surface),
        side(model, thumbnail),
    ]
    .height(Length::Fill);

    let merge = text_button(
        "Merge",
        ButtonTone::Primary,
        ButtonSize::Regular,
        model.merge().map(act),
    );
    let merge = match &model.reason {
        Some(reason) => with_tooltip(merge, reason.clone(), tooltip::Position::Top),
        None => merge,
    };
    let footer = row![
        caption(LAST_SETTINGS_HINT),
        Space::new().width(Length::Fill),
        text_button(
            "Cancel",
            ButtonTone::Control,
            ButtonSize::Regular,
            Some(act(MergeAction::SheetClose)),
        ),
        merge,
    ]
    .spacing(theme::BUTTON_ROW_SPACING)
    .align_y(Alignment::Center)
    .padding([theme::SPACING * 1.5, theme::SPACING * 2.0]);

    let card = container(column![header, rule(), body, rule(), footer])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::sheet_surface);
    // The sheet is modal: its scrim takes every press beside it, so nothing reaches the grid.
    iced::widget::stack![
        opaque(
            container(Space::new())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::scrim_surface)
        ),
        container(card).padding(SHEET_MARGIN),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The picture area: the revision's picture with the overlay, or the preview's progress or
/// refusal in its place.
fn picture_area<'a>(model: &'a SheetModel, picture: Option<&'a Handle>) -> Element<'a, Message> {
    let content: Element<'a, Message> = match &model.picture {
        Picture::Ready {
            width,
            height,
            regions,
            updating,
            ..
        } => {
            let mut layers = iced::widget::Stack::new()
                .width(Length::Fill)
                .height(Length::Fill);
            layers = layers.push(match picture {
                Some(handle) => Element::from(
                    image(handle.clone())
                        .content_fit(ContentFit::Contain)
                        .width(Length::Fill)
                        .height(Length::Fill),
                ),
                None => container(caption("Drawing the preview\u{2026}"))
                    .center(Length::Fill)
                    .into(),
            });
            if picture.is_some() && !regions.is_empty() {
                layers = layers.push(
                    canvas(Overlay {
                        regions,
                        size: (*width as f32, *height as f32),
                    })
                    .width(Length::Fill)
                    .height(Length::Fill),
                );
            }
            if let Some(words) = updating {
                layers = layers.push(
                    container(
                        container(
                            text(words.clone())
                                .size(theme::SIZE_CAPTION)
                                .style(theme::ink(Token::Text)),
                        )
                        .padding(theme::TOOLTIP_PADDING)
                        .style(theme::bar_surface),
                    )
                    .padding(theme::SPACING)
                    .width(Length::Fill)
                    .align_right(Length::Fill),
                );
            }
            layers.into()
        }
        Picture::Preparing { message, fraction } => container(
            column![
                text(message.clone())
                    .size(theme::SIZE_CONTROL)
                    .style(theme::ink(Token::TextLabel)),
                container(work_bar(*fraction)).width(Length::Fixed(160.0)),
            ]
            .spacing(theme::SPACING)
            .align_x(Alignment::Center),
        )
        .center(Length::Fill)
        .into(),
        Picture::Refused(reason) => container(
            text(reason.clone())
                .size(theme::SIZE_CONTROL)
                .style(theme::ink(Token::Error)),
        )
        .padding(theme::SPACING * 3.0)
        .center(Length::Fill)
        .into(),
    };
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::canvas_surface(theme::CANVAS))
        .into()
}

/// A bar for the preview's progress, filled as far as its fraction says; empty while unknown.
fn work_bar<'a>(fraction: Option<f32>) -> Element<'a, Message> {
    progress_bar(0.0..=1.0, fraction.unwrap_or(0.0).clamp(0.0, 1.0))
        .length(Length::Fill)
        .girth(Length::Fixed(theme::SHEET_BAR_HEIGHT))
        .style(|theme: &luxforge_ui::Theme| progress_bar::Style {
            background: theme.palette().rail.into(),
            bar: theme.palette().accent.into(),
            border: iced::Border {
                radius: (theme::SHEET_BAR_HEIGHT / 2.0).into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
        })
        .into()
}

/// The settings column: frames, alignment, deghosting and the overlay; the folder and the output
/// at its foot.
fn side<'a>(
    model: &'a SheetModel,
    thumbnail: impl Fn(FileId) -> Option<&'a Handle>,
) -> Element<'a, Message> {
    let mut settings = Column::new()
        .spacing(theme::SPACING * 1.5)
        .push(section_label("Frames"));
    if model.reading {
        settings = settings.push(caption("Reading the frames\u{2026}"));
    } else if !model.frames.is_empty() {
        settings = settings.push(row![
            Space::new().width(Length::Fill),
            container(caption("Exposure")).center_x(Length::Fixed(
                STEPPER_VALUE_WIDTH + 2.0 * theme::HEADER_BUTTON_SIZE
            )),
            container(caption("Ref.")).center_x(Length::Fixed(REF_WIDTH)),
        ]);
    }
    for frame in &model.frames {
        settings = settings.push(frame_row(frame, frame.file_id.and_then(&thumbnail)));
    }
    settings = settings
        .push(caption(EXPOSURE_HINT))
        .push(rule())
        .push(line(
            "Alignment",
            toggle(model.alignment, MergeAction::Alignment),
        ))
        .push(rule())
        .push(line(
            "Deghosting",
            toggle(model.deghost, MergeAction::Deghost),
        ))
        .push(
            checkbox(model.overlay)
                .label("Show deghost overlay")
                .text_size(theme::SIZE_CONTROL)
                .size(14)
                .on_toggle(|shown| act(MergeAction::Overlay(shown))),
        )
        .push(caption(OVERLAY_HINT));

    let folder_label = row![
        text(model.folder.clone())
            .size(theme::SIZE_CONTROL)
            .style(theme::ink(Token::Text)),
    ]
    .push(match model.folder_note {
        Some(note) => caption(note),
        None => Space::new().into(),
    })
    .push(Space::new().width(Length::Fill))
    .push(text("\u{25be}").size(theme::SIZE_CAPTION))
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    let folder_button = iced::widget::button(folder_label)
        .width(Length::Fill)
        .padding([theme::SPACING * 0.75, theme::SPACING])
        .style(theme::button_control)
        .on_press(act(MergeAction::FolderMenu(model.folder_choices.is_none())));
    let folder_menu = model.folder_choices.as_ref().map(|choices| {
        menu_list(
            choices
                .iter()
                .map(|(label, pick)| {
                    MenuEntry::Item(MenuItem {
                        icon: None,
                        label: label.clone(),
                        trailing: None,
                        on_press: Some(act(MergeAction::Folder(pick.clone()))),
                        reason: None,
                    })
                })
                .collect(),
        )
    });
    let mut foot = column![
        rule(),
        section_label("Folder"),
        popover(
            folder_button,
            folder_menu,
            act(MergeAction::FolderMenu(false))
        ),
    ]
    .spacing(theme::SPACING);
    if let Some(output) = &model.output {
        foot = foot.push(rule()).push(section_label("Output")).push(
            text(output.clone())
                .size(theme::SIZE_CONTROL)
                .style(theme::ink(Token::Text)),
        );
    }
    column![
        scrollable(container(settings).padding(Padding {
            top: 0.0,
            right: theme::SPACING,
            bottom: theme::SPACING,
            left: 0.0,
        }))
        .height(Length::Fill),
        foot,
    ]
    .spacing(theme::SPACING)
    .padding(theme::SPACING * 2.0)
    .width(Length::Fixed(SIDE_WIDTH))
    .height(Length::Fill)
    .into()
}

/// One frame: its thumbnail, name, exposure and shutter, its exposure stepper and its Ref. radio.
fn frame_row<'a>(frame: &'a FrameRow, thumbnail: Option<&'a Handle>) -> Element<'a, Message> {
    let thumb: Element<'a, Message> = match thumbnail {
        Some(handle) => image(handle.clone())
            .content_fit(ContentFit::Cover)
            .width(Length::Fixed(THUMB_WIDTH))
            .height(Length::Fixed(THUMB_HEIGHT))
            .into(),
        None => container(Space::new())
            .width(Length::Fixed(THUMB_WIDTH))
            .height(Length::Fixed(THUMB_HEIGHT))
            .style(theme::control_surface)
            .into(),
    };
    let names = column![
        truncated_text(
            frame.name.clone(),
            theme::SIZE_CONTROL,
            theme::FONT,
            Token::Text
        ),
        text(frame.detail.clone())
            .size(theme::SIZE_CAPTION)
            .style(theme::ink(Token::TextSecondary))
            .wrapping(text::Wrapping::None),
    ]
    .spacing(2.0)
    .width(Length::Fill);
    let step = |by: i32| {
        frame.offset.map(|offset| {
            act(MergeAction::Exposure {
                path: frame.path.clone(),
                offset: Some(stepped(offset, by)),
            })
        })
    };
    let icon = |icon, tooltip: &str, by| {
        header_icon_button(
            &IconButtonModel {
                icon,
                tooltip: tooltip.into(),
                enabled: frame.offset.is_some(),
                selected: false,
            },
            step(by),
        )
    };
    // The value puts an override back to Auto when pressed.
    let value = iced::widget::button(
        container(
            text(frame.value.clone())
                .size(theme::SIZE_CONTROL)
                .style(theme::ink(if frame.overridden {
                    Token::Accent
                } else {
                    Token::Text
                })),
        )
        .center_x(Length::Fill),
    )
    .padding(0)
    .width(Length::Fixed(STEPPER_VALUE_WIDTH))
    .style(theme::button_bare)
    .on_press_maybe(frame.overridden.then(|| {
        act(MergeAction::Exposure {
            path: frame.path.clone(),
            offset: None,
        })
    }));
    let stepper = row![
        icon(Icon::Minus, "A third of a stop darker", -1),
        value,
        icon(Icon::Plus, "A third of a stop brighter", 1),
    ]
    .align_y(Alignment::Center);
    let radio = iced::widget::button(
        canvas(Radio {
            selected: frame.reference,
        })
        .width(Length::Fixed(RADIO_SIZE))
        .height(Length::Fixed(RADIO_SIZE)),
    )
    .padding(0)
    .style(theme::button_bare)
    .on_press_maybe((!frame.reference).then(|| act(MergeAction::Reference(frame.path.clone()))));
    row![
        thumb,
        names,
        stepper,
        container(with_tooltip(
            radio,
            "Reference".to_owned(),
            tooltip::Position::Top
        ))
        .center_x(Length::Fixed(REF_WIDTH)),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .into()
}

/// Ref.'s radio: a ring, filled with the accent's dot while it is the reference.
struct Radio {
    selected: bool,
}

impl canvas::Program<Message, luxforge_ui::Theme> for Radio {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        theme: &luxforge_ui::Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let palette = theme.palette();
        let centre = frame.center();
        let radius = bounds.width.min(bounds.height) / 2.0 - 1.0;
        let ring = if self.selected {
            palette.accent
        } else {
            palette.text_tertiary
        };
        frame.stroke(
            &canvas::Path::circle(centre, radius),
            canvas::Stroke::default().with_color(ring).with_width(1.2),
        );
        if self.selected {
            frame.fill(&canvas::Path::circle(centre, radius * 0.45), palette.accent);
        }
        vec![frame.into_geometry()]
    }
}

/// The deghost overlay over the picture fitted in its area: each moving region tinted by the
/// exposure it comes from, hatched when it is a fallback, and labelled at its top left.
struct Overlay<'a> {
    regions: &'a [Region],
    /// The revision's picture size, which the regions' pixels are in.
    size: (f32, f32),
}

impl canvas::Program<Message, luxforge_ui::Theme> for Overlay<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &luxforge_ui::Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let (origin, scale) = fit(self.size, (bounds.width, bounds.height));
        if scale <= 0.0 {
            return Vec::new();
        }
        let at = |[x, y]: [f32; 2]| Point::new(origin[0] + x * scale, origin[1] + y * scale);
        for region in self.regions {
            let colour = tint(region.tint);
            if region.outline.len() >= 3 {
                let outline = canvas::Path::new(|path| {
                    path.move_to(at(region.outline[0]));
                    for point in &region.outline[1..] {
                        path.line_to(at(*point));
                    }
                    path.close();
                });
                frame.fill(&outline, faded(colour, 0.38));
                if region.fallback {
                    let hatching = canvas::Stroke::default()
                        .with_color(faded(colour, 0.9))
                        .with_width(1.0);
                    for (from, to) in hatch(&region.outline, HATCH_SPACING / scale) {
                        frame.stroke(&canvas::Path::line(at(from), at(to)), hatching);
                    }
                }
                frame.stroke(
                    &outline,
                    canvas::Stroke::default()
                        .with_color(faded(colour, 0.95))
                        .with_width(1.5),
                );
            }
            // The label sits just above the region's top left, inside the picture.
            let corner = at([region.bounds[0], region.bounds[1]]);
            let width = region.label.chars().count() as f32 * 6.4 + LABEL_SWATCH + 3.0 * LABEL_PAD;
            let x = corner
                .x
                .clamp(origin[0], (bounds.width - width).max(origin[0]));
            let y = (corner.y - LABEL_HEIGHT - 4.0).max(origin[1]);
            frame.fill(
                &canvas::Path::rounded_rectangle(
                    Point::new(x, y),
                    Size::new(width, LABEL_HEIGHT),
                    4.0.into(),
                ),
                LABEL_SURFACE,
            );
            frame.fill(
                &canvas::Path::rounded_rectangle(
                    Point::new(x + LABEL_PAD, y + (LABEL_HEIGHT - LABEL_SWATCH) / 2.0),
                    Size::new(LABEL_SWATCH, LABEL_SWATCH),
                    2.0.into(),
                ),
                colour,
            );
            frame.fill_text(canvas::Text {
                content: region.label.clone(),
                position: Point::new(x + 2.0 * LABEL_PAD + LABEL_SWATCH, y + LABEL_HEIGHT / 2.0),
                color: theme::PHOTO_LABEL,
                size: theme::SIZE_CAPTION.into(),
                font: theme::FONT_SEMIBOLD,
                align_y: iced::alignment::Vertical::Center,
                ..canvas::Text::default()
            });
        }
        vec![frame.into_geometry()]
    }
}

/// The notice for a merge found in Removed: Put back merge, or Not now.
pub(crate) fn put_back(model: &PutBackModel) -> Element<'_, Message> {
    notice_card(
        &NoticeCardModel {
            icon: Icon::Trash,
            title: "This merge is in Removed".into(),
            body: model.body.clone(),
            tone: NoticeTone::Warning,
        },
        vec![
            ("Not now".to_owned(), act(MergeAction::DismissPutBack)),
            (
                "Put back merge".to_owned(),
                act(MergeAction::PutBack(model.merge_id.clone())),
            ),
        ],
    )
}

/// The Info panel's stack band: "HDR · 3 exposures" (or an original's place in its stack), and
/// Expand or Collapse stack with `S` and Remove merge.
pub(crate) fn stack_band(band: &StackBand) -> Element<'_, Message> {
    let mut content = column![
        text(band.title.clone())
            .size(theme::SIZE_TITLE)
            .font(theme::FONT_SEMIBOLD)
            .style(theme::ink(Token::Text)),
    ]
    .spacing(theme::SPACING);
    if let Some(note) = &band.note {
        content = content.push(caption(note.clone()));
    }
    let expanded = band.expanded;
    let mut actions = row![labelled_button(
        &LabelledButtonModel {
            label: if expanded {
                "Collapse stack"
            } else {
                "Expand stack"
            }
            .into(),
            icon: None,
            key_hint: Some("S".into()),
            tone: ButtonTone::Control,
            size: ButtonSize::Regular,
            fill: true,
            enabled: true,
        },
        Some(act(MergeAction::Expand(band.merge_id.clone(), band.toggle))),
    )]
    .spacing(theme::BUTTON_ROW_SPACING);
    if band.leader {
        actions = actions.push(labelled_button(
            &LabelledButtonModel {
                label: "Remove merge".into(),
                icon: None,
                key_hint: None,
                tone: ButtonTone::Control,
                size: ButtonSize::Regular,
                fill: true,
                enabled: true,
            },
            Some(act(MergeAction::Remove(band.merge_id.clone()))),
        ));
    }
    content.push(actions).width(Length::Fill).into()
}
