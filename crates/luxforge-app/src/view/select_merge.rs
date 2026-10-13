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
            EXPOSURE_HINT, FRAMES_HINT, FileId, FrameRow, LAST_SETTINGS_HINT, OVERLAY_HINT,
            PanoramaSettings, Picture, Region, StitchOverlay, Tint, WARP_HINT, fit, hatch,
            stepped,
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
use luxforge_core::catalog_types::{MAX_BOUNDARY_WARP, PanoramaProjection};
use luxforge_ui::{
    ButtonSize, ButtonTone, Element, FilterOption, FilterSegmentsModel, Icon, IconButtonModel,
    LabelledButtonModel, MenuEntry, MenuItem, NoticeCardModel, SliderModel, Token,
    Tone as NoticeTone, ValueEdit, caption, filter_segments, header_icon_button, labelled_button,
    menu_list, notice_card, popover, section_label, slider, text_button, theme, truncated_text,
    with_tooltip,
};

fn act(action: MergeAction) -> Message {
    Message::Select(SelectMessage::Merge(MergeMessage::Act(action)))
}

/// The merge menu at the pointer: each kind's group (HDR, Panorama) with its Merge HDR… or Merge
/// panorama… and its hint, or a result's stack actions; an entry the core refuses says why as its
/// tooltip.
pub(crate) fn menu(model: &MenuModel, viewport: Size) -> Element<'_, Message> {
    let mut entries = Vec::new();
    let mut lines = 0.0;
    for (at, group) in model.groups.iter().enumerate() {
        if at > 0 {
            entries.push(MenuEntry::Separator);
            lines += 0.5;
        }
        if let Some(heading) = group.heading {
            entries.push(MenuEntry::Heading(heading.to_owned()));
            lines += 1.0;
        }
        for entry in &group.entries {
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

/// A panorama's overlays, drawn over the photograph and so fixed, not themed, but for the crop,
/// which is the accent: the dimming outside the crop, the covered region's edge, and the frame
/// outlines in white with their numbered circles.
const OUTSIDE_CROP: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.55);
const COVERED_EDGE: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.35);
const OUTLINE: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.85);
/// A crop corner tick's length and width, and a frame number's circle, on screen.
const TICK_LENGTH: f32 = 12.0;
const TICK_WIDTH: f32 = 3.0;
const NUMBER_RADIUS: f32 = 10.0;
/// A panorama frame's thumbnail in the frame strip, and the strip's spacing.
const STRIP_THUMB: f32 = 40.0;
/// A legend chip's swatch.
const LEGEND_SWATCH: f32 = 10.0;

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

    let mut status = row![
        text(model.status.clone())
            .size(theme::SIZE_CAPTION)
            .style(theme::ink(if model.status_refused {
                Token::Error
            } else {
                Token::TextSecondary
            })),
        Space::new().width(Length::Fill),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center);
    match (&model.panorama, &model.picture) {
        // A panorama's legend: the overlays shown, as the board's chips.
        (
            Some(settings),
            Picture::Ready {
                stitch: Some(_), ..
            },
        ) => {
            if settings.crop {
                status = status
                    .push(legend(Swatch::Crop, "Crop"))
                    .push(legend(Swatch::Outside, "Outside the crop"));
            }
            if settings.outlines {
                status = status.push(legend(Swatch::Frames, &settings.frames_legend));
            }
        }
        _ => {
            if let Some(note) = model.note {
                status = status.push(caption(note));
            }
        }
    }
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
            stitch,
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
            if let (Some(_), Some(stitch)) = (picture, stitch) {
                layers = layers.push(
                    canvas(StitchCanvas {
                        stitch,
                        size: (*width as f32, *height as f32),
                    })
                    .width(Length::Fill)
                    .height(Length::Fill),
                );
            }
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
    let settings = match &model.panorama {
        Some(panorama) => panorama_settings(model, panorama, thumbnail),
        None => hdr_settings(model, thumbnail),
    };
    let foot = foot(model);
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

/// HDR's settings: the frames with their exposure steppers and Ref., Alignment, Deghosting and
/// Show deghost overlay.
fn hdr_settings<'a>(
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
    settings.into()
}

/// A panorama's settings, as the boards draw them: Projection with Auto's choice, Boundary Warp,
/// the overlay switches, and the frame strip in the stack's order, where a click on a frame makes
/// it the reference.
fn panorama_settings<'a>(
    model: &'a SheetModel,
    settings: &'a PanoramaSettings,
    thumbnail: impl Fn(FileId) -> Option<&'a Handle>,
) -> Element<'a, Message> {
    let projection = filter_segments(
        &FilterSegmentsModel {
            options: PanoramaProjection::ALL
                .map(|projection| FilterOption {
                    label: crate::state::select_merge::projection_name(projection).into(),
                    count: None,
                })
                .to_vec(),
            selected: PanoramaProjection::ALL
                .iter()
                .position(|projection| *projection == settings.projection)
                .unwrap_or(0),
            enabled: true,
        },
        |index| {
            act(MergeAction::Projection(
                PanoramaProjection::ALL[index.min(PanoramaProjection::ALL.len() - 1)],
            ))
        },
    );
    let mut column = Column::new()
        .spacing(theme::SPACING * 1.5)
        .push(section_label("Projection"))
        .push(projection);
    if let Some(resolved) = &settings.resolved {
        column = column.push(caption(resolved.clone()));
    }
    let max = f64::from(MAX_BOUNDARY_WARP);
    let warp = slider(
        &SliderModel {
            id: None,
            label: "Boundary Warp".into(),
            min: 0.0,
            max,
            soft_min: 0.0,
            soft_max: max,
            value: f64::from(settings.boundary_warp),
            step: 1.0,
            shift_step: 10.0,
            fine_step: 1.0,
            zero: Some(0.0),
            rail: luxforge_ui::RailDecoration::Plain,
            over_range: None,
            unit: None,
            display: settings.boundary_warp.to_string(),
            edit: ValueEdit::Display,
            dragging: false,
            enabled: true,
        },
        move |fraction| {
            act(MergeAction::BoundaryWarp(
                (fraction.clamp(0.0, 1.0) * max).round() as u8,
            ))
        },
        act(MergeAction::BoundaryWarpReleased),
        act(MergeAction::BoundaryWarpReleased),
        |_| act(MergeAction::BoundaryWarpReleased),
        act(MergeAction::BoundaryWarpReleased),
        act(MergeAction::BoundaryWarpReset),
    );
    column = column
        .push(rule())
        .push(warp)
        .push(caption(WARP_HINT))
        .push(rule())
        .push(section_label("Show"))
        .push(
            checkbox(settings.outlines)
                .label("Frame outlines")
                .text_size(theme::SIZE_CONTROL)
                .size(14)
                .on_toggle(|shown| act(MergeAction::FrameOutlines(shown))),
        )
        .push(
            checkbox(settings.crop)
                .label("Crop and coverage")
                .text_size(theme::SIZE_CONTROL)
                .size(14)
                .on_toggle(|shown| act(MergeAction::CropCoverage(shown))),
        )
        .push(rule());
    let mut heading = row![section_label("Frames"), Space::new().width(Length::Fill)]
        .align_y(Alignment::Center);
    if let Some(reference) = &settings.reference {
        heading = heading.push(caption(reference.clone()));
    }
    column = column.push(heading);
    if model.reading {
        column = column.push(caption("Reading the frames\u{2026}"));
    } else {
        let strip = model
            .frames
            .iter()
            .map(|frame| strip_frame(frame, frame.file_id.and_then(&thumbnail)));
        column = column.push(
            iced::widget::Row::with_children(strip.collect::<Vec<_>>())
                .spacing(theme::SPACING * 0.75)
                .wrap()
                .vertical_spacing(theme::SPACING),
        );
    }
    column.push(caption(FRAMES_HINT)).into()
}

/// One frame of a panorama's strip: its thumbnail, framed in the accent while it is the
/// reference, and its number under it; a press makes it the reference.
fn strip_frame<'a>(frame: &'a FrameRow, thumbnail: Option<&'a Handle>) -> Element<'a, Message> {
    let thumb: Element<'a, Message> = match thumbnail {
        Some(handle) => image(handle.clone())
            .content_fit(ContentFit::Cover)
            .width(Length::Fixed(STRIP_THUMB))
            .height(Length::Fixed(STRIP_THUMB))
            .into(),
        None => container(Space::new())
            .width(Length::Fixed(STRIP_THUMB))
            .height(Length::Fixed(STRIP_THUMB))
            .style(theme::control_surface)
            .into(),
    };
    let reference = frame.reference;
    let framed = container(thumb)
        .padding(2.0)
        .style(move |theme: &luxforge_ui::Theme| container::Style {
            border: iced::Border {
                color: if reference {
                    theme.palette().accent
                } else {
                    Color::TRANSPARENT
                },
                width: 1.5,
                radius: 3.0.into(),
            },
            ..container::Style::default()
        });
    let number = text(frame.number.map(|number| number.to_string()).unwrap_or_default())
        .size(theme::SIZE_CAPTION)
        .style(theme::ink(if reference {
            Token::Accent
        } else {
            Token::TextSecondary
        }));
    let content = column![framed, number]
        .spacing(2.0)
        .align_x(Alignment::Center);
    let button = iced::widget::button(content)
        .padding(0)
        .style(theme::button_bare)
        .on_press_maybe((!reference).then(|| act(MergeAction::Reference(frame.path.clone()))));
    with_tooltip(button, frame.name.clone(), tooltip::Position::Top)
}

/// The folder and the output at the settings' foot.
fn foot(model: &SheetModel) -> Element<'_, Message> {
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
    foot.into()
}

/// A legend chip's swatch: the crop's accent square, the dimmed outside, a frame's circle.
#[derive(Clone, Copy)]
enum Swatch {
    Crop,
    Outside,
    Frames,
}

/// One chip of a panorama's overlay legend.
fn legend<'a>(swatch: Swatch, label: &str) -> Element<'a, Message> {
    container(
        row![
            canvas(swatch)
                .width(Length::Fixed(LEGEND_SWATCH))
                .height(Length::Fixed(LEGEND_SWATCH)),
            text(label.to_owned())
                .size(theme::SIZE_CAPTION)
                .style(theme::ink(Token::Text))
                .wrapping(text::Wrapping::None),
        ]
        .spacing(theme::SPACING * 0.75)
        .align_y(Alignment::Center),
    )
    .padding([2.0, theme::SPACING])
    .style(theme::control_surface)
    .into()
}

impl canvas::Program<Message, luxforge_ui::Theme> for Swatch {
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
        let side = bounds.width.min(bounds.height) - 1.0;
        let square = canvas::Path::rounded_rectangle(
            Point::new(0.5, 0.5),
            Size::new(side, side),
            2.0.into(),
        );
        match self {
            Swatch::Crop => frame.stroke(
                &square,
                canvas::Stroke::default()
                    .with_color(theme.palette().accent)
                    .with_width(1.5),
            ),
            Swatch::Outside => frame.fill(&square, Color::BLACK),
            Swatch::Frames => frame.stroke(
                &canvas::Path::circle(frame.center(), side / 2.0),
                canvas::Stroke::default()
                    .with_color(theme.palette().text)
                    .with_width(1.2),
            ),
        }
        vec![frame.into_geometry()]
    }
}

/// A panorama's overlays over the picture fitted in its area: outside the crop dimmed, the
/// covered region's edge, the crop in the accent with its corner ticks, and each frame's outline
/// in white with its number in a circle near its top.
struct StitchCanvas<'a> {
    stitch: &'a StitchOverlay,
    /// The revision's picture size, which the overlay's pixels are in.
    size: (f32, f32),
}

impl canvas::Program<Message, luxforge_ui::Theme> for StitchCanvas<'_> {
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
        let (origin, scale) = fit(self.size, (bounds.width, bounds.height));
        if scale <= 0.0 {
            return Vec::new();
        }
        let at = |[x, y]: [f32; 2]| Point::new(origin[0] + x * scale, origin[1] + y * scale);
        let polygon = |points: &[[f32; 2]]| {
            canvas::Path::new(|path| {
                if let Some(first) = points.first() {
                    path.move_to(at(*first));
                    for point in &points[1..] {
                        path.line_to(at(*point));
                    }
                    path.close();
                }
            })
        };
        let picture = Rectangle::new(
            Point::new(origin[0], origin[1]),
            Size::new(self.size.0 * scale, self.size.1 * scale),
        );
        if let Some([x, y, width, height]) = self.stitch.crop {
            let crop = Rectangle::new(at([x, y]), Size::new(width * scale, height * scale));
            // Outside the crop, within the picture: four bands around it.
            for band in [
                Rectangle::new(
                    picture.position(),
                    Size::new(picture.width, (crop.y - picture.y).max(0.0)),
                ),
                Rectangle::new(
                    Point::new(picture.x, crop.y + crop.height),
                    Size::new(
                        picture.width,
                        (picture.y + picture.height - crop.y - crop.height).max(0.0),
                    ),
                ),
                Rectangle::new(
                    Point::new(picture.x, crop.y),
                    Size::new((crop.x - picture.x).max(0.0), crop.height),
                ),
                Rectangle::new(
                    Point::new(crop.x + crop.width, crop.y),
                    Size::new(
                        (picture.x + picture.width - crop.x - crop.width).max(0.0),
                        crop.height,
                    ),
                ),
            ] {
                frame.fill_rectangle(band.position(), band.size(), OUTSIDE_CROP);
            }
            for covered in &self.stitch.covered {
                if covered.len() >= 3 {
                    frame.stroke(
                        &polygon(covered),
                        canvas::Stroke::default()
                            .with_color(COVERED_EDGE)
                            .with_width(1.0),
                    );
                }
            }
            let accent = theme.palette().accent;
            frame.stroke(
                &canvas::Path::rectangle(crop.position(), crop.size()),
                canvas::Stroke::default().with_color(accent).with_width(1.5),
            );
            let tick = canvas::Stroke::default()
                .with_color(accent)
                .with_width(TICK_WIDTH);
            let (left, top) = (crop.x, crop.y);
            let (right, bottom) = (crop.x + crop.width, crop.y + crop.height);
            let length = TICK_LENGTH.min(crop.width / 3.0).min(crop.height / 3.0);
            for (corner, dx, dy) in [
                (Point::new(left, top), 1.0, 1.0),
                (Point::new(right, top), -1.0, 1.0),
                (Point::new(left, bottom), 1.0, -1.0),
                (Point::new(right, bottom), -1.0, -1.0),
            ] {
                frame.stroke(
                    &canvas::Path::new(|path| {
                        path.move_to(Point::new(corner.x + dx * length, corner.y));
                        path.line_to(corner);
                        path.line_to(Point::new(corner.x, corner.y + dy * length));
                    }),
                    tick,
                );
            }
        }
        for (number, outline) in &self.stitch.frames {
            if outline.len() < 3 {
                continue;
            }
            frame.stroke(
                &polygon(outline),
                canvas::Stroke::default().with_color(OUTLINE).with_width(1.0),
            );
            // The number sits in a circle a fifth of the way down the frame, at its centre.
            let (mut low, mut high) = ([f32::MAX; 2], [f32::MIN; 2]);
            for [x, y] in outline {
                low = [low[0].min(*x), low[1].min(*y)];
                high = [high[0].max(*x), high[1].max(*y)];
            }
            let centre = at([
                (low[0] + high[0]) / 2.0,
                low[1] + (high[1] - low[1]) * 0.2,
            ]);
            let circle = canvas::Path::circle(centre, NUMBER_RADIUS);
            frame.fill(&circle, LABEL_SURFACE);
            frame.stroke(
                &circle,
                canvas::Stroke::default().with_color(OUTLINE).with_width(1.2),
            );
            frame.fill_text(canvas::Text {
                content: number.to_string(),
                position: centre,
                color: theme::PHOTO_LABEL,
                size: theme::SIZE_CAPTION.into(),
                font: theme::FONT_SEMIBOLD,
                align_x: iced::widget::text::Alignment::Center,
                align_y: iced::alignment::Vertical::Center,
                ..canvas::Text::default()
            });
        }
        vec![frame.into_geometry()]
    }
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
