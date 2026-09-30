//! The loupe's chrome, as the choosing-from-a-burst board draws it: the moment's numbered frames
//! under the photograph (`.mstrip .fr`), the info bar over it, the 100% focus check's inset and the
//! pointer's region box, and the key hints (`.hint`).
//!
//! Image content is always the caller's: each frame and the inset's region take an
//! [`image::Handle`] the caller made once and holds, so drawing one never uploads it again. A long
//! burst is drawn as the caller's window of frames ([`visible_window`]), never one element per
//! frame of the whole burst.

use super::icon_button::{Icon, IconButtonModel, icon, sized_icon_button};
use crate::theme;
use iced::widget::image::{self, Handle};
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{Row, Space, button, column, container, row, stack, text, tooltip};
use iced::{Alignment, Border, ContentFit, Element, Length, Point, Rectangle, Size, Theme};
use std::ops::Range;

/// One frame of a moment under the loupe.
#[derive(Debug, Clone, PartialEq)]
pub struct MomentFrame {
    /// The frame's grid preview, or `None` while it is still being read: the frame is drawn empty.
    pub image: Option<Handle>,
    pub picked: bool,
}

/// Plain data for the moment's frame strip: a window of its frames.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameStripModel {
    /// The frames to draw, in order; frame `first + i` of the moment is `frames[i]`.
    pub frames: Vec<MomentFrame>,
    /// The moment's index of `frames[0]`, from 0; the frames are numbered from 1.
    pub first: usize,
    /// The moment's index of the active frame, when it is in the window.
    pub active: Option<usize>,
}

/// Renders the frame strip: the previous-moment chevron, the frames and the next-moment chevron,
/// [`theme::MOMENT_STRIP_SPACING`] apart. Pressing a frame publishes `on_frame` with its index in
/// the moment; a chevron whose message is `None` is disabled.
pub fn frame_strip<'a, M: Clone + 'a>(
    model: &FrameStripModel,
    on_frame: impl Fn(usize) -> M + 'a,
    on_previous: Option<M>,
    on_next: Option<M>,
) -> Element<'a, M> {
    let mut strip = Row::new()
        .spacing(theme::MOMENT_STRIP_SPACING)
        .align_y(Alignment::Center)
        .push(chevron_button(
            Icon::ChevronLeft,
            "Previous moment (\u{2191})",
            on_previous,
        ));
    for (offset, frame) in model.frames.iter().enumerate() {
        let index = model.first + offset;
        strip = strip.push(moment_frame(
            frame,
            index,
            model.active == Some(index),
            on_frame(index),
        ));
    }
    strip
        .push(chevron_button(
            Icon::ChevronRight,
            "Next moment (\u{2193})",
            on_next,
        ))
        .into()
}

/// One frame: its image fitted into [`theme::MOMENT_IMAGE_WIDTH`] ×
/// [`theme::MOMENT_IMAGE_HEIGHT`], its number at the lower left and, when picked, the pick check
/// at the upper right; active, it is raised with the accent outline.
fn moment_frame<'a, M: Clone + 'a>(
    frame: &MomentFrame,
    index: usize,
    active: bool,
    on_press: M,
) -> Element<'a, M> {
    let mut layers = stack![
        container(fitted_image(
            frame.image.clone(),
            theme::MOMENT_IMAGE_WIDTH,
            theme::MOMENT_IMAGE_HEIGHT,
        ))
        .center(Length::Fill),
        container(frame_number(index + 1))
            .padding(theme::FRAME_NUMBER_INSET)
            .align_bottom(Length::Fill),
    ];
    if frame.picked {
        let inset = theme::PICK_CHECK_INSET - theme::CELL_PICK_RING_WIDTH;
        layers = layers.push(
            container(pick_check())
                .padding([inset, inset])
                .align_right(Length::Fill),
        );
    }
    button(layers)
        .padding(0)
        .width(Length::Fixed(theme::MOMENT_FRAME_WIDTH))
        .height(Length::Fixed(theme::MOMENT_FRAME_HEIGHT))
        .style(theme::image_cell(
            active,
            theme::MOMENT_FRAME_RADIUS,
            theme::MOMENT_ACTIVE_OUTLINE,
        ))
        .on_press(on_press)
        .into()
}

/// A frame's number, 10 pt semibold, over a dark copy a point lower and to the right, which stands
/// in for the board's soft text shadow so a light sky never swallows it.
fn frame_number<'a, M: 'a>(number: usize) -> Element<'a, M> {
    let label = number.to_string();
    let shadow = text(label.clone())
        .size(theme::SIZE_FRAME_NUMBER)
        .font(theme::FONT_SEMIBOLD)
        .wrapping(Wrapping::None)
        .color(iced::Color {
            a: 0.7,
            ..iced::Color::BLACK
        });
    stack![
        container(shadow).padding(iced::Padding::default().left(1.0).top(1.0)),
        text(label)
            .size(theme::SIZE_FRAME_NUMBER)
            .font(theme::FONT_SEMIBOLD)
            .wrapping(Wrapping::None)
            .color(theme::TEXT_PRIMARY),
    ]
    .into()
}

/// A pick's check (`.tk`), the grid cell's own: the accent disc and its dark check, ringed so it
/// reads over any photograph. The ring is the disc's border, [`theme::CELL_PICK_RING_WIDTH`] outside the
/// [`theme::CELL_PICK_SIZE`] disc.
pub(crate) fn pick_check<'a, M: 'a>() -> Element<'a, M> {
    let size = theme::CELL_PICK_SIZE + 2.0 * theme::CELL_PICK_RING_WIDTH;
    container(icon(
        Icon::Check,
        theme::CELL_CHECK_SIZE,
        theme::PRIMARY_INK,
    ))
    .center(Length::Fixed(size))
    .style(move |_: &Theme| {
        container::Style::default()
            .background(theme::ACCENT)
            .border(Border {
                color: theme::CELL_PICK_RING,
                width: theme::CELL_PICK_RING_WIDTH,
                radius: (size / 2.0).into(),
            })
    })
    .into()
}

/// A caller's image fitted into `width` × `height` and centred, scaled down but never up, or an
/// empty box of that size while there is none.
pub(crate) fn fitted_image<'a, M: 'a>(
    handle: Option<Handle>,
    width: f32,
    height: f32,
) -> Element<'a, M> {
    match handle {
        Some(handle) => image::Image::new(handle)
            .width(Length::Fixed(width))
            .height(Length::Fixed(height))
            .content_fit(ContentFit::ScaleDown)
            .into(),
        None => Space::new()
            .width(Length::Fixed(width))
            .height(Length::Fixed(height))
            .into(),
    }
}

/// A [`theme::ICON_BUTTON_SIZE`] button holding a 12 pt chevron, disabled without a message.
fn chevron_button<'a, M: Clone + 'a>(
    glyph: Icon,
    tooltip_text: &str,
    on_press: Option<M>,
) -> Element<'a, M> {
    let enabled = on_press.is_some();
    sized_icon_button(
        &IconButtonModel {
            icon: glyph,
            tooltip: tooltip_text.to_owned(),
            enabled,
            selected: false,
        },
        on_press,
        theme::ICON_BUTTON_SIZE,
        theme::SMALL_ICON_SIZE,
        if enabled {
            theme::TEXT_SECONDARY
        } else {
            theme::TEXT_FAINT
        },
        tooltip::Position::Top,
    )
}

/// Which frames of `total` to draw, at most `capacity` of them, keeping `active` in view and as
/// near the middle as the ends allow. A 1,000-frame burst is drawn as the few frames that fit.
pub fn visible_window(total: usize, active: usize, capacity: usize) -> Range<usize> {
    let count = total.min(capacity);
    let active = active.min(total.saturating_sub(1));
    let start = active
        .saturating_sub(count.saturating_sub(1) / 2)
        .min(total - count);
    start..start + count
}

/// Plain data for the loupe's info bar: every part the caller's words.
#[derive(Debug, Clone, PartialEq)]
pub struct LoupeInfoModel {
    /// The lead, semibold in the accent: `Moment 4 of 37 · burst`.
    pub moment: String,
    /// The frame's place and time offset: `Frame 3 of 6 · +0.52 s`.
    pub frame: String,
    /// `1/2000 s · f/5.6 · ISO 100 · 28 mm`; `None` draws neither it nor the rule before it.
    pub exposure: Option<String>,
    /// What the picture is, in [`theme::TEXT_IDENTITY`]: `Camera preview · 8368 × 5584`.
    pub source: String,
}

/// Renders the loupe's info bar: the canvas chrome's 34 pt floating surface holding the moment,
/// the frame, a rule, the exposure, a rule and the picture's source.
pub fn loupe_info_bar<'a, M: 'a>(model: &LoupeInfoModel) -> Element<'a, M> {
    let part = |content: String, colour| {
        text(content)
            .size(theme::SIZE_CONTROL)
            .wrapping(Wrapping::None)
            .color(colour)
    };
    let mut content = Row::new()
        .spacing(theme::DRAFT_BAR_SPACING)
        .align_y(Alignment::Center)
        .height(Length::Fill)
        .push(
            text(model.moment.clone())
                .size(theme::SIZE_CONTROL)
                .font(theme::FONT_SEMIBOLD)
                .wrapping(Wrapping::None)
                .color(theme::ACCENT),
        )
        .push(part(model.frame.clone(), theme::TEXT_LABEL));
    if let Some(exposure) = &model.exposure {
        content = content
            .push(bar_rule())
            .push(part(exposure.clone(), theme::TEXT_LABEL));
    }
    content = content
        .push(bar_rule())
        .push(part(model.source.clone(), theme::TEXT_IDENTITY));
    container(content)
        .padding([0.0, theme::LOUPE_INFO_PADDING])
        .height(Length::Fixed(theme::DRAFT_BAR_HEIGHT))
        .style(|_: &Theme| theme::chrome_surface(theme::CHROME_BORDER, theme::CHROME_RADIUS))
        .into()
}

/// The 1 × 16 pt rule between the info bar's groups (`.vs`).
fn bar_rule<'a, M: 'a>() -> Element<'a, M> {
    container(Space::new())
        .width(Length::Fixed(theme::BORDER_WIDTH))
        .height(Length::Fixed(theme::STRIP_RULE_HEIGHT))
        .style(|_: &Theme| container::Style::default().background(theme::STRIP_RULE))
        .into()
}

/// Where the 100% region's pixels come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsetSource {
    /// The camera's embedded preview, full size.
    CameraPreview,
    /// A neutral Luxforge development of the frame, made because the camera's preview is smaller
    /// than the sensor. The inset always says so.
    Development,
}

/// The inset's footer: its lead, semibold, and its caption. A development is labelled in the lead.
pub(crate) fn inset_labels(source: InsetSource) -> (&'static str, &'static str) {
    match source {
        InsetSource::CameraPreview => ("100%", "Focus check under the pointer"),
        InsetSource::Development => ("100% \u{b7} Luxforge development", "Focus check"),
    }
}

/// Plain data for the 100% focus check's inset.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusInsetModel {
    /// The region under the pointer at 100%, [`theme::FOCUS_INSET_WIDTH`] ×
    /// [`theme::FOCUS_REGION_HEIGHT`] points of it; `None` while it is being prepared, drawn empty.
    pub region: Option<Handle>,
    pub source: InsetSource,
}

/// Renders the 100% inset: the region over a 24 pt footer naming what it is, with the `Z` that
/// toggles it at the right, on the Bar surface with its outline drawn over the region.
pub fn focus_inset<'a, M: 'a>(model: &FocusInsetModel) -> Element<'a, M> {
    let (lead, caption) = inset_labels(model.source);
    let region: Element<'a, M> = match &model.region {
        Some(handle) => image::Image::new(handle.clone())
            .width(Length::Fill)
            .height(Length::Fixed(theme::FOCUS_REGION_HEIGHT))
            .content_fit(ContentFit::Cover)
            .border_radius(iced::border::top(theme::FOCUS_INSET_RADIUS))
            .into(),
        None => container(Space::new())
            .width(Length::Fill)
            .height(Length::Fixed(theme::FOCUS_REGION_HEIGHT))
            .style(|_: &Theme| {
                container::Style::default()
                    .background(theme::CANVAS)
                    .border(Border {
                        radius: iced::border::top(theme::FOCUS_INSET_RADIUS),
                        ..Border::default()
                    })
            })
            .into(),
    };
    let footer = row![
        text(lead)
            .size(theme::SIZE_CAPTION)
            .font(theme::FONT_SEMIBOLD)
            .wrapping(Wrapping::None)
            .color(theme::TEXT_PRIMARY),
        text(caption)
            .size(theme::SIZE_CAPTION)
            .wrapping(Wrapping::None)
            .color(theme::TEXT_SECONDARY),
        Space::new().width(Length::Fill),
        text("Z")
            .size(theme::SIZE_CAPTION)
            .wrapping(Wrapping::None)
            .color(theme::TEXT_TERTIARY),
    ]
    .spacing(theme::SPACING)
    .align_y(Alignment::Center)
    .padding([0.0, theme::SPACING])
    .height(Length::Fixed(theme::FOCUS_FOOTER_HEIGHT));
    stack![
        container(column![region, footer])
            .width(Length::Fixed(theme::FOCUS_INSET_WIDTH))
            .style(theme::focus_inset_surface),
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::focus_inset_outline),
    ]
    .into()
}

/// Renders the pointer's region box over the loupe's photograph: `size` points, a
/// [`theme::REGION_BOX_WIDTH`] white outline inside a 1 pt dark ring. The caller places it at
/// [`region_box`]'s origin.
pub fn focus_box<'a, M: 'a>(size: Size) -> Element<'a, M> {
    container(
        container(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_: &Theme| {
                container::Style::default().border(Border {
                    color: theme::REGION_BOX,
                    width: theme::REGION_BOX_WIDTH,
                    radius: 0.0.into(),
                })
            }),
    )
    .padding(theme::BORDER_WIDTH)
    .width(Length::Fixed(size.width))
    .height(Length::Fixed(size.height))
    .style(|_: &Theme| {
        container::Style::default().border(Border {
            color: theme::REGION_BOX_RING,
            width: theme::BORDER_WIDTH,
            radius: 0.0.into(),
        })
    })
    .into()
}

/// Where the region box of `size` goes on the photograph drawn in `image`: centred on the pointer,
/// then moved back inside the photograph, and no larger than it.
pub fn region_box(pointer: Point, image: Rectangle, size: Size) -> Rectangle {
    let size = Size::new(size.width.min(image.width), size.height.min(image.height));
    let x = (pointer.x - size.width / 2.0).clamp(image.x, image.x + image.width - size.width);
    let y = (pointer.y - size.height / 2.0).clamp(image.y, image.y + image.height - size.height);
    Rectangle::new(Point::new(x, y), size)
}

/// One key hint: the key's cap and what it does.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyHint {
    pub key: String,
    pub action: String,
}

/// Renders the loupe's key hints on one line, [`theme::KEY_HINT_SPACING`] apart: each key in its
/// cap, then its action in [`theme::TEXT_IDENTITY`].
pub fn key_hints<'a, M: 'a>(hints: &[KeyHint]) -> Element<'a, M> {
    Row::with_children(hints.iter().map(|hint| {
        row![
            container(
                text(hint.key.clone())
                    .size(theme::SIZE_SECTION_LABEL)
                    .line_height(LineHeight::Absolute(theme::CAPTION_LINE_HEIGHT.into()))
                    .font(theme::FONT_SEMIBOLD)
                    .wrapping(Wrapping::None)
                    .color(theme::TEXT_LABEL),
            )
            .padding(theme::KEY_CAP_PADDING)
            .style(theme::tag_surface(theme::KEY_CAP_RADIUS)),
            text(hint.action.clone())
                .size(theme::SIZE_CAPTION)
                .wrapping(Wrapping::None)
                .color(theme::TEXT_IDENTITY),
        ]
        .spacing(theme::KEY_CAP_SPACING)
        .align_y(Alignment::Center)
        .into()
    }))
    .spacing(theme::KEY_HINT_SPACING)
    .align_y(Alignment::Center)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window keeps the active frame in view, centred where the ends allow, and never holds more
    /// than fits or more than there are.
    #[test]
    fn a_window_keeps_the_active_frame_in_view() {
        assert_eq!(visible_window(6, 2, 8), 0..6, "a short moment is whole");
        assert_eq!(visible_window(1000, 499, 5), 497..502, "centred");
        assert_eq!(visible_window(1000, 0, 5), 0..5, "held at the start");
        assert_eq!(visible_window(1000, 999, 5), 995..1000, "held at the end");
        assert_eq!(visible_window(1000, 2000, 5), 995..1000, "past the end");
        assert_eq!(visible_window(1000, 500, 4), 499..503);
        assert_eq!(visible_window(0, 0, 5), 0..0, "nothing to draw");
        assert_eq!(visible_window(10, 3, 0), 3..3, "no room");
        for active in 0..40 {
            let window = visible_window(40, active, 7);
            assert_eq!(window.len(), 7);
            assert!(window.contains(&active), "{active} in {window:?}");
        }
    }

    /// A development's inset says so in its lead; the camera's preview is the plain 100%.
    #[test]
    fn a_development_is_always_labelled() {
        assert_eq!(
            inset_labels(InsetSource::CameraPreview),
            ("100%", "Focus check under the pointer")
        );
        let (lead, _) = inset_labels(InsetSource::Development);
        assert!(lead.starts_with("100%") && lead.contains("Luxforge development"));
    }

    /// The region box centres on the pointer and stays on the photograph, at any edge.
    #[test]
    fn the_region_box_follows_the_pointer_inside_the_photograph() {
        let image = Rectangle::new(Point::new(100.0, 50.0), Size::new(960.0, 641.0));
        let size = Size::new(64.0, 44.0);
        assert_eq!(
            region_box(Point::new(600.0, 400.0), image, size),
            Rectangle::new(Point::new(568.0, 378.0), size)
        );
        assert_eq!(
            region_box(Point::new(90.0, 40.0), image, size).position(),
            Point::new(100.0, 50.0),
            "held at the top left"
        );
        assert_eq!(
            region_box(Point::new(2000.0, 2000.0), image, size).position(),
            Point::new(996.0, 647.0),
            "held at the bottom right"
        );
        let tiny = Rectangle::new(Point::ORIGIN, Size::new(40.0, 30.0));
        assert_eq!(
            region_box(Point::new(20.0, 15.0), tiny, size),
            tiny,
            "no larger than the photograph"
        );
    }

    #[test]
    fn every_state_builds() {
        let frames = vec![
            MomentFrame {
                image: None,
                picked: false,
            },
            MomentFrame {
                image: None,
                picked: true,
            },
        ];
        for active in [None, Some(3)] {
            let model = FrameStripModel {
                frames: frames.clone(),
                first: 2,
                active,
            };
            let _: Element<'_, usize> = frame_strip(&model, |index| index, Some(0), None);
        }
        for exposure in [None, Some("1/2000 s \u{b7} f/5.6".to_owned())] {
            let _: Element<'_, ()> = loupe_info_bar(&LoupeInfoModel {
                moment: "Moment 4 of 37 \u{b7} burst".into(),
                frame: "Frame 3 of 6 \u{b7} +0.52 s".into(),
                exposure,
                source: "Camera preview \u{b7} 8368 \u{d7} 5584".into(),
            });
        }
        for source in [InsetSource::CameraPreview, InsetSource::Development] {
            let _: Element<'_, ()> = focus_inset(&FocusInsetModel {
                region: None,
                source,
            });
        }
        let _: Element<'_, ()> = focus_box(Size::new(64.0, 44.0));
        let _: Element<'_, ()> = key_hints(&[KeyHint {
            key: "P".into(),
            action: "pick".into(),
        }]);
    }
}
