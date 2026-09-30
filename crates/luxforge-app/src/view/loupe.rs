//! The loupe's region ([burst board](../../../../docs/design/catalog/choosing-from-a-burst.png)),
//! drawn in Select's centre in place of the grid while the loupe is open, from
//! `state/loupe.rs`'s model. Like every view it reads only its model, and what the app lends it for
//! the frame ([`LoupeImages`]): each picture's handle by its own frame's item and preview key, the
//! region's, and the grid's previews for the strip. No handle is made here.
//!
//! Top to bottom, as the board draws it: the info bar, the photograph fitted to the screen (or
//! compare's cells) with the 100% focus check's box and inset over it, the moment's numbered
//! frames and the key hints. The pointer over the photograph is reported as fractions of the
//! picture, which is what the focus check's rectangle is placed by.
use crate::{
    app::{
        loupe::LoupeImages,
        message::{Message, loupe::LoupeMessage, select::SelectMessage},
    },
    state::loupe::{
        BAR_GAP, BOTTOM_INSET, COMPARE_GAP, FocusModel, FrameModel, HINTS_GAP, HINTS_HEIGHT,
        INFO_BAR_HEIGHT, LoupeModel, SIDE_INSET, STRIP_GAP, STRIP_HEIGHT, StripModel, TOP_INSET,
        Travel,
    },
};
use iced::{
    Border, ContentFit, Element, Length, Padding, Size, Theme,
    widget::{Row, Space, column, container, image, mouse_area, stack, text},
};
use luxforge_ui::{
    FocusInsetModel, FrameStripModel, InsetSource, KeyHint, LoupeInfoModel, MomentFrame, caption,
    focus_box, focus_inset, frame_strip, key_hints, loupe_info_bar, theme,
};

fn loupe_message(message: LoupeMessage) -> Message {
    Message::Select(SelectMessage::Loupe(message))
}

/// The loupe over Select's centre.
pub(crate) fn loupe<'a>(model: &'a LoupeModel, images: LoupeImages<'a>) -> Element<'a, Message> {
    let info = loupe_info_bar(&LoupeInfoModel {
        moment: model.info.moment.clone(),
        frame: model.info.frame.clone(),
        exposure: model.info.exposure.clone(),
        source: model.info.source.clone(),
    });
    let photograph: Element<'a, Message> = if !model.compare.is_empty() {
        compare(&model.compare, images)
    } else if let Some(frame) = &model.frame {
        single(frame, model.focus.as_ref(), images)
    } else {
        container(caption(model.info.source.clone()))
            .center(Length::Fill)
            .into()
    };
    let strip: Element<'a, Message> = match &model.strip {
        Some(strip) => strip_view(strip, images),
        None => Space::new().into(),
    };
    let hints: Vec<KeyHint> = model
        .hints
        .iter()
        .map(|(key, action)| KeyHint {
            key: key.clone(),
            action: action.clone(),
        })
        .collect();
    let body = column![
        Space::new().height(Length::Fixed(TOP_INSET)),
        container(info)
            .center_x(Length::Fill)
            .height(Length::Fixed(INFO_BAR_HEIGHT)),
        Space::new().height(Length::Fixed(BAR_GAP)),
        container(photograph)
            .padding(Padding::from([0.0, SIDE_INSET]))
            .width(Length::Fill)
            .height(Length::Fill),
        Space::new().height(Length::Fixed(STRIP_GAP)),
        container(strip)
            .center_x(Length::Fill)
            .height(Length::Fixed(STRIP_HEIGHT)),
        Space::new().height(Length::Fixed(HINTS_GAP)),
        container(key_hints(&hints))
            .center_x(Length::Fill)
            .height(Length::Fixed(HINTS_HEIGHT)),
        Space::new().height(Length::Fixed(BOTTOM_INSET)),
    ];
    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::canvas_surface)
        .into()
}

/// A frame's picture at its fitted size, or what it says instead while it has none.
fn picture<'a>(frame: &'a FrameModel, images: LoupeImages<'a>) -> Element<'a, Message> {
    let (width, height) = (frame.rect.width, frame.rect.height);
    match frame
        .picture
        .as_ref()
        .and_then(|picture| images.picture(picture))
    {
        Some(handle) => image(handle.clone())
            .width(Length::Fixed(width))
            .height(Length::Fixed(height))
            .content_fit(ContentFit::Fill)
            .into(),
        None => container(caption(frame.note.clone().unwrap_or_default()))
            .center_x(Length::Fixed(width))
            .center_y(Length::Fixed(height))
            .style(|_: &Theme| container::Style::default().background(theme::BAR))
            .into(),
    }
}

/// The active frame fitted to the screen, the focus check's box and inset over it, and the pointer
/// over it reported as fractions of the picture.
fn single<'a>(
    frame: &'a FrameModel,
    focus: Option<&'a FocusModel>,
    images: LoupeImages<'a>,
) -> Element<'a, Message> {
    let (width, height) = (frame.rect.width, frame.rect.height);
    let mut layers = stack![picture(frame, images)];
    if let Some(focus) = focus {
        let region_box = focus.region_box;
        layers = layers.push(
            container(focus_box(Size::new(region_box.width, region_box.height))).padding(Padding {
                top: region_box.y,
                left: region_box.x,
                ..Padding::ZERO
            }),
        );
        let inset = focus_inset(&FocusInsetModel {
            region: focus
                .region
                .as_ref()
                .and_then(|region| images.region(region))
                .cloned(),
            source: if focus.developed {
                InsetSource::Development
            } else {
                InsetSource::CameraPreview
            },
        });
        layers = layers.push(container(inset).padding(Padding {
            top: (focus.inset.y - frame.rect.y).max(0.0),
            left: (focus.inset.x - frame.rect.x).max(0.0),
            ..Padding::ZERO
        }));
    }
    let area = mouse_area(
        layers
            .width(Length::Fixed(width))
            .height(Length::Fixed(height)),
    )
    .on_move(move |point| {
        loupe_message(LoupeMessage::Pointer(Some((
            point.x / width.max(1.0),
            point.y / height.max(1.0),
        ))))
    })
    .on_exit(loupe_message(LoupeMessage::Pointer(None)));
    container(area).center(Length::Fill).into()
}

/// Compare: the moment's frames side by side at one zoom, each its own frame's picture, the active
/// one outlined in the accent, each numbered.
fn compare<'a>(cells: &'a [FrameModel], images: LoupeImages<'a>) -> Element<'a, Message> {
    Row::with_children(cells.iter().map(|cell| {
        let active = cell.active;
        // The active frame's accent outline, drawn over the picture's edge so the picture keeps
        // its fitted size.
        let outline = container(Space::new())
            .width(Length::Fixed(cell.rect.width))
            .height(Length::Fixed(cell.rect.height))
            .style(move |_: &Theme| {
                container::Style::default().border(Border {
                    color: if active {
                        theme::ACCENT
                    } else {
                        iced::Color::TRANSPARENT
                    },
                    width: theme::BORDER_WIDTH * 2.0,
                    radius: 0.0.into(),
                })
            });
        let number = container(
            text(cell.number.to_string())
                .size(theme::SIZE_CAPTION)
                .font(theme::FONT_SEMIBOLD)
                .color(theme::TEXT_PRIMARY),
        )
        .padding([2.0, 6.0])
        .style(theme::tag_surface(4.0));
        container(stack![
            picture(cell, images),
            outline,
            container(number).padding(Padding {
                top: 8.0,
                left: 8.0,
                ..Padding::ZERO
            })
        ])
        .center(Length::Fill)
        .into()
    }))
    .spacing(COMPARE_GAP)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The moment's numbered frames, each the grid's preview of its file, with the moments either side.
fn strip_view<'a>(strip: &'a StripModel, images: LoupeImages<'a>) -> Element<'a, Message> {
    let model = FrameStripModel {
        frames: strip
            .frames
            .iter()
            .map(|frame| MomentFrame {
                image: frame
                    .item
                    .as_ref()
                    .and_then(|item| images.thumbnail(item))
                    .cloned(),
                picked: frame.picked,
            })
            .collect(),
        first: strip.first as usize,
        active: Some(strip.active as usize),
    };
    frame_strip(
        &model,
        |index| loupe_message(LoupeMessage::Jump(index as u32)),
        strip
            .previous
            .then(|| loupe_message(LoupeMessage::Moment(Travel::Back))),
        strip
            .next
            .then(|| loupe_message(LoupeMessage::Moment(Travel::Forward))),
    )
}

#[cfg(test)]
mod tests {
    use crate::state::loupe::{
        FOCUS_INSET_HEIGHT, FOCUS_REGION, INFO_BAR_HEIGHT, STRIP_CHEVRON, STRIP_FRAME_WIDTH,
        STRIP_HEIGHT, STRIP_SPACING,
    };
    use luxforge_ui::theme;

    /// The loupe's geometry is laid out with the widgets' own sizes, so the photograph's area, the
    /// strip's capacity and the focus check's pixels are what the view draws.
    #[test]
    fn loupe_geometry_is_the_widgets_own() {
        assert_eq!(INFO_BAR_HEIGHT, theme::DRAFT_BAR_HEIGHT);
        assert_eq!(STRIP_HEIGHT, theme::MOMENT_FRAME_HEIGHT);
        assert_eq!(STRIP_FRAME_WIDTH, theme::MOMENT_FRAME_WIDTH);
        assert_eq!(STRIP_SPACING, theme::MOMENT_STRIP_SPACING);
        assert_eq!(STRIP_CHEVRON, theme::ICON_BUTTON_SIZE);
        assert_eq!(
            FOCUS_REGION,
            (theme::FOCUS_INSET_WIDTH, theme::FOCUS_REGION_HEIGHT)
        );
        assert_eq!(
            FOCUS_INSET_HEIGHT,
            theme::FOCUS_REGION_HEIGHT + theme::FOCUS_FOOTER_HEIGHT
        );
    }
}
