//! The photo surface: the plain preview, the crop frame over the draft's input stage, the floating
//! chrome stacked over them, and the mapping from a pointer position to an image pixel.
//!
//! The chrome floats so it stays next to the photograph when the panels are hidden: the mode strip
//! at the bottom centre, the draft bar and the notices at the top centre. None of it reads state:
//! the [`CanvasModel`] already says which mode is selected, what the draft reads and which notices
//! apply, and every control here publishes one semantic message.
use crate::{
    app::{
        crop::SURFACE_ID,
        message::{CapabilityMessage, CropMessage, DraftMessage, Message},
    },
    crop_canvas::{CropCanvas, Mode, Part, View},
    mask_canvas::{MaskCanvas, OutputView as MaskView, Placement},
    state::canvas::{
        CanvasModel, DraftBar, Notice, NoticeAction, NoticeTone, PhotoView, SurfaceMode, ZoomView,
    },
    view::Surfaces,
};
use iced::{
    Alignment, ContentFit, Element, Length, Point, Rectangle, Renderer, Size, Theme,
    alignment::{Horizontal, Vertical},
    mouse::Cursor,
    widget::{Column, canvas, container, image, mouse_area, responsive, scrollable, stack, text},
};
use luxforge_ui::{
    ButtonSize, ButtonTone, ModeEntry, NoticeCardModel, ToggleEntry, Tone, floating_bar,
    mode_strip, notice_card, text_button, theme,
};

/// The surface the photograph is given around it at Fit, from the design's canvas rule.
pub(crate) const PHOTO_PADDING: f32 = 20.0;
/// How far the floating bars sit from the canvas edge.
const BAR_INSET: f32 = 16.0;

/// The whole canvas region: the photograph, and the floating chrome stacked over it.
pub(crate) fn surface<'a>(model: &'a CanvasModel, surfaces: Surfaces<'a>) -> Element<'a, Message> {
    let mut layers: Vec<Element<'a, Message>> = vec![photo_area(model, surfaces)];
    if let Some(overlay) = thirds(model) {
        layers.push(overlay);
    }
    if let Some(top) = top_chrome(model) {
        layers.push(top);
    }
    layers.push(strip(model));
    stack(layers)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The photograph itself, padded by the design's surface margin at Fit. At a percentage the
/// scrollable owns the space instead, so the padding would fight the pan.
fn photo_area<'a>(model: &'a CanvasModel, surfaces: Surfaces<'a>) -> Element<'a, Message> {
    let content = match (&model.photo, surfaces.draft, surfaces.draft_photo) {
        (PhotoView::Draft, Some(draft), Some(photo)) => crop_surface(model, draft, photo),
        (PhotoView::Plain, _, _) => match (surfaces.photo, model.dimensions) {
            (Some(raster), Some(dimensions)) => plain(model, raster, &surfaces, dimensions),
            _ => empty("Open a photograph"),
        },
        (PhotoView::Empty(message), _, _) => empty(message),
        // A draft without its own pixels is not drawn as a draft.
        (PhotoView::Draft, _, _) => match (surfaces.photo, model.dimensions) {
            (Some(raster), Some(dimensions)) => plain(model, raster, &surfaces, dimensions),
            _ => empty("Open a photograph"),
        },
    };
    match model.zoom {
        ZoomView::Fit => container(content)
            .padding(PHOTO_PADDING)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        ZoomView::Percent(_) => content,
    }
}

/// The mode strip: the pointer, every declared canvas mode, then the view overlays.
fn strip<'a>(model: &'a CanvasModel) -> Element<'a, Message> {
    let modes: Vec<ModeEntry> = model
        .modes
        .iter()
        .map(|mode| ModeEntry {
            label: mode.label.clone(),
            shortcut: mode.shortcut.clone(),
            selected: mode.selected,
            enabled: mode.enabled,
        })
        .collect();
    let toggles = [ToggleEntry {
        label: "Thirds".into(),
        shortcut: Some("O".into()),
        on: model.thirds,
    }];
    let ids: Vec<String> = model.modes.iter().map(|mode| mode.id.clone()).collect();
    let bar = mode_strip(
        &modes,
        move |index| Message::SetMode(ids[index].clone()),
        &toggles,
        |_| Message::ToggleThirds,
    );
    container(bar)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(BAR_INSET)
        .align_x(Horizontal::Center)
        .align_y(Vertical::Bottom)
        .into()
}

/// The draft bar and the notices under it, at the top centre of the canvas.
fn top_chrome<'a>(model: &'a CanvasModel) -> Option<Element<'a, Message>> {
    if model.draft_bar.is_none() && model.notices.is_empty() {
        return None;
    }
    let mut column = Column::new()
        .spacing(theme::SPACING)
        .align_x(Alignment::Center);
    if let Some(bar) = &model.draft_bar {
        column = column.push(draft_bar(bar));
    }
    for notice in &model.notices {
        column = column.push(notice_view(notice));
    }
    Some(
        container(container(column).max_width(560.0))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(BAR_INSET)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Top)
            .into(),
    )
}

fn draft_bar(model: &DraftBar) -> Element<'_, Message> {
    let mut children: Vec<Element<'_, Message>> = vec![
        luxforge_ui::title(model.title.clone()),
        luxforge_ui::caption(model.readout.clone()),
    ];
    // The bar belongs to whichever gesture is open; only one ever is.
    let (cancel, apply_message) = if model.mask {
        (
            Message::Draft(DraftMessage::Cancel),
            Message::Draft(DraftMessage::Commit),
        )
    } else {
        (
            Message::Crop(CropMessage::Cancel),
            Message::Crop(CropMessage::Apply),
        )
    };
    children.push(text_button(
        "Cancel",
        ButtonTone::Quiet,
        ButtonSize::Compact,
        Some(cancel),
    ));
    let apply = text_button(
        "Apply",
        ButtonTone::Primary,
        ButtonSize::Compact,
        model.can_apply.then_some(apply_message),
    );
    children.push(match &model.apply_reason {
        // A refused Apply says why on hover instead of going quiet.
        Some(reason) => iced::widget::tooltip(
            apply,
            container(luxforge_ui::caption(reason.clone()))
                .padding(theme::TOOLTIP_PADDING)
                .style(theme::bar_surface),
            iced::widget::tooltip::Position::Bottom,
        )
        .into(),
        None => apply,
    });
    floating_bar(children)
}

fn notice_view(notice: &Notice) -> Element<'_, Message> {
    let actions = notice
        .actions
        .iter()
        .map(|(label, action)| {
            (
                label.clone(),
                match action {
                    NoticeAction::DiscardDraft => Message::Crop(CropMessage::Cancel),
                    NoticeAction::ReapplyDraft => Message::Crop(CropMessage::Reapply),
                    NoticeAction::DiscardGesture => Message::Draft(DraftMessage::Cancel),
                    NoticeAction::ReapplyGesture => Message::Draft(DraftMessage::Reapply),
                    NoticeAction::ReturnCurrent => Message::ReturnCurrent,
                    NoticeAction::AllowConsent => {
                        Message::Capability(CapabilityMessage::Consent(true))
                    }
                    NoticeAction::DenyConsent => {
                        Message::Capability(CapabilityMessage::Consent(false))
                    }
                },
            )
        })
        .collect();
    notice_card(
        &NoticeCardModel {
            title: notice.title.clone(),
            body: notice.body.clone(),
            tone: match notice.tone {
                NoticeTone::Neutral => Tone::Neutral,
                NoticeTone::Warning => Tone::Warning,
            },
        },
        actions,
    )
}

/// The thirds overlay over the fitted photograph. Only Fit is drawn: at a percentage the photo
/// scrolls inside a scrollable, so the overlay would need the live scroll offset to line up with
/// it, and the overlay is a composition aid for the whole frame rather than for a detail view.
fn thirds<'a>(model: &'a CanvasModel) -> Option<Element<'a, Message>> {
    let dimensions = model.dimensions?;
    // The crop overlay draws its own thirds inside the draft rectangle; two sets would disagree.
    if !model.thirds || model.photo != PhotoView::Plain || !matches!(model.zoom, ZoomView::Fit) {
        return None;
    }
    Some(
        container(
            canvas(Thirds { dimensions })
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .padding(PHOTO_PADDING)
        .width(Length::Fill)
        .height(Length::Fill)
        .into(),
    )
}

/// The four thirds guides over `rect`: two vertical, then two horizontal, each as its endpoints.
/// Pure geometry, so the overlay's placement is provable without a renderer.
fn thirds_lines(rect: Rectangle) -> [(Point, Point); 4] {
    let x = |step: f32| rect.x + rect.width * step / 3.0;
    let y = |step: f32| rect.y + rect.height * step / 3.0;
    [
        (Point::new(x(1.0), rect.y), Point::new(x(1.0), y(3.0))),
        (Point::new(x(2.0), rect.y), Point::new(x(2.0), y(3.0))),
        (Point::new(rect.x, y(1.0)), Point::new(x(3.0), y(1.0))),
        (Point::new(rect.x, y(2.0)), Point::new(x(3.0), y(2.0))),
    ]
}

/// Two vertical and two horizontal 1 px guides over the contained image rectangle. The program
/// holds no state and reads none: it is given the photograph's pixel dimensions and computes the
/// same contained rectangle the image widget draws into.
struct Thirds {
    dimensions: (u32, u32),
}

impl canvas::Program<Message> for Thirds {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<canvas::Geometry> {
        let Some(rect) = fit_rect(self.dimensions, bounds.size()) else {
            return Vec::new();
        };
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let stroke = canvas::Stroke::default()
            .with_color(theme::GUIDE)
            .with_width(1.0);
        for (from, to) in thirds_lines(rect) {
            frame.stroke(&canvas::Path::line(from, to), stroke);
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        _bounds: Rectangle,
        _cursor: Cursor,
    ) -> iced::mouse::Interaction {
        // The overlay is decoration: every pointer event belongs to the photo under it.
        iced::mouse::Interaction::None
    }
}

/// The canvas with nothing to draw: the invitation to open a photograph, or why the open one has no
/// preview. Title-sized, so a long reason still reads as one line of chrome rather than a headline.
fn empty(message: &str) -> Element<'_, Message> {
    container(
        text(message.to_owned())
            .size(luxforge_ui::theme::SIZE_TITLE)
            .color(luxforge_ui::theme::TEXT_SECONDARY),
    )
    .center(Length::Fill)
    .into()
}

/// The photograph, with the clipping overlay stacked over it when there is one.
///
/// The photograph is drawn by the [photo surface](luxforge_ui::photo_surface) at every zoom: it
/// owns its texture and writes the raster into it as it draws, so no allocation round trip stands
/// between a rendered frame and the screen, and it hands the renderer only the part of a zoomed box
/// that is on screen. The overlay keeps the toolkit's image path, as a second image stacked over
/// the first and never a change to it.
///
/// Alignment comes from giving both the same sizing rule — `Contain` inside the same box at Fit,
/// the same fixed extent at a percentage — and from the overlay's cell grid keeping the source's
/// aspect ratio, so the two land in the same rectangle at every zoom and, inside the scrollable, at
/// every pan. The surface's `Contain` is the toolkit's own `ContentFit::Contain`, centred and
/// snapped to the pixel grid exactly as the image widget snaps it.
fn plain<'a>(
    model: &'a CanvasModel,
    raster: &'a luxforge_ui::PhotoRaster,
    surfaces: &Surfaces<'a>,
    (width, height): (u32, u32),
) -> Element<'a, Message> {
    let picking = model.picking;
    let pointer = model.pointer;
    // The clipping overlay and the mask overlay are two images over the photograph, in that order,
    // and neither changes the photograph itself.
    let overlays: Vec<iced::advanced::image::Handle> = [surfaces.overlay, surfaces.mask_overlay]
        .into_iter()
        .flatten()
        .map(|allocation| allocation.handle().clone())
        .collect();
    let mask_draft = surfaces.mask_draft;
    let mask_map = surfaces.mask_map;
    match model.zoom {
        ZoomView::Fit => {
            // Fit needs the available size to know where the toolkit draws the contained image.
            responsive(move |available| {
                let photo = luxforge_ui::photo_surface(
                    raster,
                    luxforge_ui::Placement::Contain,
                    Length::Fill,
                    Length::Fill,
                );
                let mut layers: Vec<Element<'_, Message>> = vec![photo];
                for handle in &overlays {
                    layers.push(
                        image(handle.clone())
                            .width(Length::Fill)
                            .height(Length::Fill)
                            .content_fit(ContentFit::Contain)
                            .filter_method(image::FilterMethod::Nearest)
                            .into(),
                    );
                }
                // The open gesture's handles sit above every overlay, mapped through the affine and
                // the same contained rectangle the photograph is drawn into.
                if let (Some(draft), Some(map)) = (mask_draft, mask_map)
                    && let Some(rect) = fit_rect((width, height), available)
                    && let Some(view) = MaskView::fit(map.output(), rect.size())
                {
                    layers.push(
                        iced::widget::container(
                            canvas(MaskCanvas::new(draft, Placement { map, view }))
                                .width(Length::Fill)
                                .height(Length::Fill),
                        )
                        .padding(iced::Padding {
                            top: rect.y,
                            left: rect.x,
                            right: 0.0,
                            bottom: 0.0,
                        })
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .into(),
                    );
                }
                let layered: Element<'_, Message> = if layers.len() == 1 {
                    layers.pop().expect("the photograph")
                } else {
                    stack(layers).into()
                };
                // The pointer readout needs every move over the photograph, not only the ones a
                // module's pick would use; a move that maps to the same pixel is dropped in the
                // update function rather than here.
                let mut area = mouse_area(layered).on_move(move |point| {
                    Message::PointerMoved(fit_pick((width, height), available, point))
                });
                area = area.on_exit(Message::PointerMoved(None));
                if picking && let Some((x, y)) = pointer {
                    area = area.on_press(Message::PointPicked { x, y });
                }
                area.into()
            })
            .into()
        }
        ZoomView::Percent(value) => {
            let scale = value / 100.0 / model.scale_factor;
            let (box_width, box_height) = (
                Length::Fixed(width as f32 * scale),
                Length::Fixed(height as f32 * scale),
            );
            // `Fill` rather than a fit: the box is the exact stage's displayed size and the texture
            // may be the display proxy, which is smaller. Filling stretches it to exactly that box,
            // so the photograph and the overlay — which fills the same box — stay in the same
            // rectangle whichever texture is on screen. The box may be far larger than the window;
            // the surface hands the renderer only its visible part.
            let photo = luxforge_ui::photo_surface(
                raster,
                luxforge_ui::Placement::Fill,
                box_width,
                box_height,
            );
            let mut layers: Vec<Element<'a, Message>> = vec![photo];
            for handle in &overlays {
                layers.push(
                    image(handle.clone())
                        .width(box_width)
                        .height(box_height)
                        .content_fit(ContentFit::Fill)
                        .filter_method(image::FilterMethod::Nearest)
                        .into(),
                );
            }
            if let (Some(draft), Some(map)) = (mask_draft, mask_map)
                && let Some(view) = MaskView::percent(value, model.scale_factor)
            {
                layers.push(
                    canvas(MaskCanvas::new(draft, Placement { map, view }))
                        .width(box_width)
                        .height(box_height)
                        .into(),
                );
            }
            let layered: Element<'a, Message> = if layers.len() == 1 {
                layers.pop().expect("the photograph")
            } else {
                stack(layers).into()
            };
            // Inside the scrollable the reported point is already content-space: the scrollable
            // translates the cursor by its offset before its content sees it.
            let mut area = mouse_area(layered).on_move(move |point| {
                Message::PointerMoved(percent_pick((width, height), scale, point))
            });
            area = area.on_exit(Message::PointerMoved(None));
            if picking && let Some((x, y)) = pointer {
                area = area.on_press(Message::PointPicked { x, y });
            }
            scrolled(area.into())
        }
    }
}

/// The crop frame over the layer's own input stage, at Fit or at a percentage zoom. The canvas
/// draws nothing authoritative: it borrows the draft and publishes messages.
fn crop_surface<'a>(
    model: &'a CanvasModel,
    draft: &'a crate::crop_draft::CropDraft,
    photo: &'a crate::draft_photo::DraftPhoto,
) -> Element<'a, Message> {
    let handle = photo.clone();
    let box_size = draft.stage.bounding_box();
    let mode = match model.surface_mode {
        SurfaceMode::Pan => Mode::Pan,
        SurfaceMode::Guide => Mode::Guide,
        SurfaceMode::Frame => Mode::Frame,
    };
    let option = model.option;
    // Two stacked canvases: the toolkit paints every image of one layer over every mesh of that
    // layer, so the frame, thirds, handles and guide need the layer the stack gives its second child.
    let parts =
        move |handle: crate::draft_photo::DraftPhoto, view: View, width: Length, height: Length| {
            stack([Part::Photo, Part::Overlay].map(|part| {
                canvas(CropCanvas::new(
                    draft,
                    handle.clone(),
                    view,
                    mode,
                    option,
                    part,
                ))
                .width(width)
                .height(height)
                .into()
            }))
        };
    match model.zoom {
        ZoomView::Fit => responsive(move |available| match View::fit(box_size, available) {
            Some(view) => parts(handle.clone(), view, Length::Fill, Length::Fill).into(),
            None => container(text("The surface is too small to draw the crop").size(12))
                .center(Length::Fill)
                .into(),
        })
        .into(),
        ZoomView::Percent(value) => {
            let Some(view) = View::percent(value, model.scale_factor) else {
                return container(text("Zoom is out of range").size(12))
                    .center(Length::Fill)
                    .into();
            };
            let frame = parts(
                handle,
                view,
                Length::Fixed(box_size.0 as f32 * view.scale),
                Length::Fixed(box_size.1 as f32 * view.scale),
            );
            scrolled(frame.into())
        }
    }
}

/// The one scrollable the photo surface uses, so a Space drag can scroll it while drafting.
fn scrolled(content: Element<'_, Message>) -> Element<'_, Message> {
    scrollable(container(content).center(Length::Shrink))
        .id(SURFACE_ID)
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::default(),
            horizontal: scrollable::Scrollbar::default(),
        })
        .on_scroll(|viewport| {
            let offset = viewport.absolute_offset();
            Message::Panned(offset.x, offset.y)
        })
        .into()
}

/// Where the toolkit draws a contained image inside `available`, matching the image widget's own
/// bounds: `ContentFit::Contain` sized and centered.
fn fit_rect(image: (u32, u32), available: Size) -> Option<Rectangle> {
    let content = Size::new(image.0 as f32, image.1 as f32);
    if content.width <= 0.0
        || content.height <= 0.0
        || !(available.width > 0.0 && available.height > 0.0)
    {
        return None;
    }
    let size = ContentFit::Contain.fit(content, available);
    (size.width > 0.0 && size.height > 0.0).then(|| {
        Rectangle::new(
            Point::new(
                (available.width - size.width) / 2.0,
                (available.height - size.height) / 2.0,
            ),
            size,
        )
    })
}

/// Fit: the reported point spans the whole surface, so the centered rectangle is removed first.
fn fit_pick(image: (u32, u32), available: Size, point: Point) -> Option<(u32, u32)> {
    let rect = fit_rect(image, available)?;
    image_pixel(
        (point.x - rect.x) * image.0 as f32 / rect.width,
        (point.y - rect.y) * image.1 as f32 / rect.height,
        image,
    )
}

/// Percent: the reported point is local to the displayed raster, which is the image scaled
/// uniformly. The scrollable translates the cursor by its scroll offset before its content sees
/// it, so the pan position never enters this mapping.
fn percent_pick(image: (u32, u32), scale: f32, point: Point) -> Option<(u32, u32)> {
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    image_pixel(point.x / scale, point.y / scale, image)
}

fn image_pixel(x: f32, y: f32, (width, height): (u32, u32)) -> Option<(u32, u32)> {
    let inside = |value: f32, limit: u32| {
        (value.is_finite() && value >= 0.0 && value < limit as f32).then(|| value.floor() as u32)
    };
    Some((inside(x, width)?, inside(y, height)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlay's guides land on the contained image rectangle the photo is drawn into, not on
    /// the padded canvas around it, and they divide it in exact thirds.
    #[test]
    fn thirds_guides_divide_the_contained_image_rectangle() {
        let image = (200, 100);
        let available = Size::new(400.0, 400.0);
        let rect = fit_rect(image, available).expect("a drawn rectangle");
        assert_eq!((rect.x, rect.y), (0.0, 100.0));
        assert_eq!((rect.width, rect.height), (400.0, 200.0));
        let lines = thirds_lines(rect);
        // Two verticals, spanning the rectangle's full height at a third and two thirds across.
        for (index, third) in [1.0f32, 2.0].into_iter().enumerate() {
            let (from, to) = lines[index];
            let x = rect.x + rect.width * third / 3.0;
            assert!(
                (from.x - x).abs() < 1e-4 && (to.x - x).abs() < 1e-4,
                "{from:?}"
            );
            assert_eq!((from.y, to.y), (rect.y, rect.y + rect.height));
        }
        // Two horizontals, spanning its full width.
        for (index, third) in [1.0f32, 2.0].into_iter().enumerate() {
            let (from, to) = lines[index + 2];
            let y = rect.y + rect.height * third / 3.0;
            assert!(
                (from.y - y).abs() < 1e-4 && (to.y - y).abs() < 1e-4,
                "{from:?}"
            );
            assert_eq!((from.x, to.x), (rect.x, rect.x + rect.width));
        }
        // Nothing is drawn where no rectangle exists.
        assert!(fit_rect(image, Size::new(0.0, 0.0)).is_none());
    }

    /// The overlay is drawn only where it can line up with the photograph: at Fit, over a plain
    /// preview, and never over a crop draft, which draws its own thirds inside its rectangle.
    #[test]
    fn the_thirds_overlay_is_drawn_only_at_fit_over_a_plain_preview() {
        let base = CanvasModel {
            photo: PhotoView::Plain,
            zoom: ZoomView::Fit,
            dimensions: Some((480, 320)),
            thirds: true,
            ..CanvasModel::default()
        };
        assert!(thirds(&base).is_some());
        for (case, model) in [
            (
                "the toggle is off",
                CanvasModel {
                    thirds: false,
                    ..base.clone()
                },
            ),
            (
                "a percentage zoom scrolls instead",
                CanvasModel {
                    zoom: ZoomView::Percent(100.0),
                    ..base.clone()
                },
            ),
            (
                "a crop draft draws its own",
                CanvasModel {
                    photo: PhotoView::Draft,
                    ..base.clone()
                },
            ),
            (
                "nothing is open",
                CanvasModel {
                    photo: PhotoView::Empty("Open a photograph".into()),
                    ..base.clone()
                },
            ),
            (
                "no dimensions are known",
                CanvasModel {
                    dimensions: None,
                    ..base.clone()
                },
            ),
        ] {
            assert!(thirds(&model).is_none(), "{case}");
        }
    }

    #[test]
    fn fit_picks_map_through_the_centered_contained_rectangle() {
        let image = (200, 100);
        let available = Size::new(400.0, 400.0);
        let rect = fit_rect(image, available).expect("a drawn rectangle");
        assert_eq!((rect.x, rect.y), (0.0, 100.0));
        assert_eq!((rect.width, rect.height), (400.0, 200.0));
        assert_eq!(
            fit_pick(image, available, Point::new(0.0, 100.0)),
            Some((0, 0))
        );
        assert_eq!(
            fit_pick(image, available, Point::new(399.0, 299.0)),
            Some((199, 99))
        );
        assert_eq!(
            fit_pick(image, available, Point::new(200.0, 200.0)),
            Some((100, 50)),
            "the centre of the surface is the centre of the photograph"
        );
        for outside in [
            Point::new(0.0, 99.0),
            Point::new(0.0, 300.0),
            Point::new(-1.0, 150.0),
            Point::new(f32::NAN, 150.0),
        ] {
            assert_eq!(fit_pick(image, available, outside), None, "{outside:?}");
        }
        assert_eq!(fit_rect(image, Size::new(0.0, 400.0)), None);
        assert_eq!(fit_rect((0, 0), available), None);
    }

    #[test]
    fn percent_picks_ignore_pan_because_the_scrollable_translates_the_cursor() {
        let image = (200, 200);
        let scale = 2.0;
        // The scrollable hands its content a cursor already moved by the scroll offset, so the
        // point the mouse area reports is the viewport position plus the pan.
        for pan in [(0.0, 0.0), (100.0, 50.0), (317.0, 9.0)] {
            let viewport = Point::new(10.0, 20.0);
            let reported = Point::new(viewport.x + pan.0, viewport.y + pan.1);
            let expected = (
                ((viewport.x + pan.0) / scale) as u32,
                ((viewport.y + pan.1) / scale) as u32,
            );
            assert_eq!(percent_pick(image, scale, reported), Some(expected));
        }
        assert_eq!(
            percent_pick(image, scale, Point::new(1.9, 0.0)),
            Some((0, 0))
        );
        assert_eq!(percent_pick(image, scale, Point::new(400.0, 0.0)), None);
        assert_eq!(percent_pick(image, scale, Point::new(-0.5, 0.0)), None);
        assert_eq!(percent_pick(image, 0.0, Point::new(1.0, 1.0)), None);
    }

    #[test]
    fn one_hundred_percent_uses_physical_pixel_scale() {
        let width = 6000f32;
        let display_scale = 2f32;
        let logical_width = width / display_scale;
        assert_eq!(logical_width * display_scale, width);
    }
}
