//! The photo surface: the plain preview, the crop frame over the draft's input stage, and the
//! mapping from a pointer position to an image pixel.
use crate::{
    app::{crop::SURFACE_ID, message::Message},
    crop_canvas::{CropCanvas, Mode, Part, View},
    state::canvas::{CanvasModel, PhotoView, SurfaceMode, ZoomView},
    view::Surfaces,
};
use iced::{
    ContentFit, Element, Length, Point, Rectangle, Size,
    widget::{canvas, container, image, mouse_area, responsive, scrollable, stack, text},
};
use iced_runtime::image as image_memory;

pub(crate) fn surface<'a>(model: &'a CanvasModel, surfaces: Surfaces<'a>) -> Element<'a, Message> {
    match (&model.photo, surfaces.draft, surfaces.draft_photo) {
        (PhotoView::Draft, Some(draft), Some(allocation)) => crop_surface(model, draft, allocation),
        (PhotoView::Plain, _, _) => match (surfaces.photo, model.dimensions) {
            (Some(allocation), Some(dimensions)) => plain(model, allocation, dimensions),
            _ => empty("Open a photograph"),
        },
        (PhotoView::Empty(message), _, _) => empty(message),
        // A draft without its own pixels is not drawn as a draft.
        (PhotoView::Draft, _, _) => match (surfaces.photo, model.dimensions) {
            (Some(allocation), Some(dimensions)) => plain(model, allocation, dimensions),
            _ => empty("Open a photograph"),
        },
    }
}

fn empty(message: &str) -> Element<'_, Message> {
    container(text(message.to_owned()).size(24))
        .center(Length::Fill)
        .into()
}

fn plain<'a>(
    model: &'a CanvasModel,
    allocation: &'a image_memory::Allocation,
    (width, height): (u32, u32),
) -> Element<'a, Message> {
    let picking = model.picking;
    let pointer = model.pointer;
    match model.zoom {
        ZoomView::Fit => {
            let handle = allocation.handle().clone();
            // Fit needs the available size to know where the toolkit draws the contained image.
            responsive(move |available| {
                let photo = image(handle.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(ContentFit::Contain);
                if !picking {
                    return photo.into();
                }
                let mut area = mouse_area(photo).on_move(move |point| {
                    Message::PointerMoved(fit_pick((width, height), available, point))
                });
                if let Some((x, y)) = pointer {
                    area = area.on_press(Message::PointPicked { x, y });
                }
                area.into()
            })
            .into()
        }
        ZoomView::Percent(value) => {
            let scale = value / 100.0 / model.scale_factor;
            let photo = image(allocation.handle().clone())
                .width(Length::Fixed(width as f32 * scale))
                .height(Length::Fixed(height as f32 * scale));
            // Inside the scrollable the reported point is already content-space: the scrollable
            // translates the cursor by its offset before its content sees it.
            let photo: Element<'a, Message> = if picking {
                let mut area = mouse_area(photo).on_move(move |point| {
                    Message::PointerMoved(percent_pick((width, height), scale, point))
                });
                if let Some((x, y)) = pointer {
                    area = area.on_press(Message::PointPicked { x, y });
                }
                area.into()
            } else {
                photo.into()
            };
            scrolled(photo)
        }
    }
}

/// The crop frame over the layer's own input stage, at Fit or at a percentage zoom. The canvas
/// draws nothing authoritative: it borrows the draft and publishes messages.
fn crop_surface<'a>(
    model: &'a CanvasModel,
    draft: &'a crate::crop_draft::CropDraft,
    allocation: &'a image_memory::Allocation,
) -> Element<'a, Message> {
    let handle = allocation.handle().clone();
    let box_size = draft.stage.bounding_box();
    let mode = match model.surface_mode {
        SurfaceMode::Pan => Mode::Pan,
        SurfaceMode::Guide => Mode::Guide,
        SurfaceMode::Frame => Mode::Frame,
    };
    let option = model.option;
    // Two stacked canvases: the toolkit paints every image of one layer over every mesh of that
    // layer, so the frame, thirds, handles and guide need the layer the stack gives its second child.
    let parts = move |handle: image::Handle, view: View, width: Length, height: Length| {
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
