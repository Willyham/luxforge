//! The crop frame canvas: it draws the draft and turns pointer events into draft gestures.
//!
//! The canvas owns no editing state. It borrows the draft for one `view` call, maps pointer
//! positions into box space and publishes messages; every change to the draft happens in
//! [`crate::app`]'s update, so the same gestures are reachable from the API without
//! simulating a pointer.
//!
//! The rotation drawn here is the GPU's display filter, not the reference sampler: the committed
//! render is the reference. The canvas never rasterizes a pixel itself.
use crate::{
    app::message::{CropMessage, CropPointer, Message},
    canvas_view::CanvasView,
    crop_draft::{Corner, CropDraft, Handle, edge_midpoint},
    draft_photo::{self, DraftPhoto},
};
use iced::{
    Color, Point, Radians, Rectangle, Renderer, Size, Theme,
    mouse::{self, Cursor},
    widget::{
        canvas::{self, Action, Event, Frame, Geometry, Image, Path, Stroke},
        image,
    },
};
use lightwell_core::{BoxRect, Edge};

/// The hit radius of a handle and the drawn size of one, in logical pixels.
const HIT_RADIUS: f32 = 8.0;
const HANDLE_SIZE: f32 = 8.0;
/// How much of the source stays visible outside the crop rectangle.
const DIM_OPACITY: f32 = 0.35;

/// What a drag on the image does right now. The app decides, from Space and the panel's guide
/// toggle, so the mode is observable state rather than a hidden canvas mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Handles, sides and moving the composition.
    Frame,
    /// Space is held: the drag scrolls the surrounding scrollable and never touches the draft.
    Pan,
    /// The panel's Straighten toggle is on: the drag draws a levelling guide.
    Guide,
}

/// Which half of the crop frame a canvas draws. Iced's renderer paints every image of one layer
/// over every mesh of that layer, whatever order they were built in, so the frame, thirds, handles
/// and guide have to live in a canvas of their own that the host stacks above the photo. The two
/// parts share this program, this view transform and this draft; only the overlay handles pointers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Photo,
    Overlay,
}

/// The canvas's own ephemeral pointer bookkeeping. It holds no editing state: only where a Space
/// drag last was, so the next move can be expressed as a scroll delta.
#[derive(Debug, Default)]
pub(crate) struct Interaction {
    pan_from: Option<Point>,
}

/// The crop frame over one truncated preview. Borrowed from the app for the duration of `view`.
pub(crate) struct CropCanvas<'a> {
    draft: &'a CropDraft,
    /// The crop layer's input stage as the preview worker rendered it, in the tiles it was uploaded
    /// as.
    photo: DraftPhoto,
    /// The crop box as it is drawn in this canvas: the shared canvas view, over box pixels.
    view: CanvasView,
    mode: Mode,
    /// Option (Alt) is held, so a handle scales uniformly about the centre.
    option: bool,
    part: Part,
}

impl<'a> CropCanvas<'a> {
    pub(crate) fn new(
        draft: &'a CropDraft,
        photo: DraftPhoto,
        view: CanvasView,
        mode: Mode,
        option: bool,
        part: Part,
    ) -> Self {
        Self {
            draft,
            photo,
            view,
            mode,
            option,
            part,
        }
    }

    /// The unrotated image rectangle: the input stage at the display scale, centred on the box
    /// centre. Iced rotates an image about its bounds centre with the same matrix the geometry
    /// contract uses, so a positive angle turns the image clockwise on screen with no sign flip;
    /// each tile is placed so that turning it about its own centre turns the stage about this
    /// rectangle's ([`draft_photo::placed`]).
    fn image_bounds(&self) -> Rectangle {
        let (box_width, box_height) = self.draft.stage.bounding_box();
        let centre = self.view.canvas_point(box_width / 2.0, box_height / 2.0);
        let size = Size::new(
            f64::from(self.draft.stage.width) as f32 * self.view.scale,
            f64::from(self.draft.stage.height) as f32 * self.view.scale,
        );
        Rectangle::new(
            Point::new(centre.x - size.width / 2.0, centre.y - size.height / 2.0),
            size,
        )
    }

    /// Every tile of the stage, rotated with it, at this opacity.
    fn draw_photo(&self, frame: &mut Frame, opacity: f32) {
        let bounds = self.image_bounds();
        let angle = self.draft.stage.angle.to_radians() as f32;
        let stage = (self.photo.width, self.photo.height);
        for (rect, handle) in self.photo.tiles.iter() {
            frame.draw_image(
                draft_photo::placed(bounds, stage, *rect, angle),
                Image {
                    handle: handle.clone(),
                    filter_method: image::FilterMethod::Linear,
                    rotation: Radians(angle),
                    border_radius: 0.0.into(),
                    opacity,
                    snap: false,
                },
            );
        }
    }

    /// What a press at this canvas point would grab.
    fn handle_at(&self, point: Point) -> Handle {
        match self.mode {
            Mode::Guide => Handle::Guide,
            Mode::Pan => Handle::Move,
            Mode::Frame => self.draft.hit(
                self.view.stage_point(point),
                self.view.tolerance(HIT_RADIUS),
            ),
        }
    }

    fn pointer(&self, pointer: CropPointer) -> Action<Message> {
        Action::publish(Message::Crop(CropMessage::Pointer(pointer))).and_capture()
    }
}

/// The pointer position in canvas-local logical pixels, even once a drag has left the bounds.
fn local(cursor: Cursor, bounds: Rectangle) -> Option<Point> {
    cursor
        .position()
        .map(|point| Point::new(point.x - bounds.x, point.y - bounds.y))
}

impl canvas::Program<Message> for CropCanvas<'_> {
    type State = Interaction;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<Action<Message>> {
        if self.part == Part::Photo {
            return None;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                if self.mode == Mode::Pan {
                    state.pan_from = Some(point);
                    return Some(Action::capture());
                }
                let handle = self.handle_at(point);
                let (x, y) = self.view.stage_point(point);
                Some(self.pointer(CropPointer::Begin { handle, x, y }))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let point = local(cursor, bounds)?;
                if let Some(from) = state.pan_from {
                    state.pan_from = Some(point);
                    // A Space drag scrolls the surface; the draft is untouched.
                    return Some(
                        Action::publish(Message::Crop(CropMessage::Pan {
                            dx: from.x - point.x,
                            dy: from.y - point.y,
                        }))
                        .and_capture(),
                    );
                }
                if !self.draft.dragging() {
                    return None;
                }
                let (x, y) = self.view.stage_point(point);
                Some(self.pointer(CropPointer::Drag {
                    x,
                    y,
                    option: self.option,
                }))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if state.pan_from.take().is_some() {
                    return Some(Action::capture());
                }
                if !self.draft.dragging() {
                    return None;
                }
                Some(self.pointer(CropPointer::End))
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let box_rect = &self.draft.rect;
        let rect = self
            .view
            .canvas_rect(box_rect.x, box_rect.y, box_rect.width, box_rect.height);
        if self.part == Part::Photo {
            // The whole stage, dimmed, then the crop rectangle at full opacity over it.
            self.draw_photo(&mut frame, DIM_OPACITY);
            frame.with_clip(rect, |clipped| self.draw_photo(clipped, 1.0));
            return vec![frame.into_geometry()];
        }

        let border = Color::from_rgba(1.0, 1.0, 1.0, 0.9);
        let guides = Color::from_rgba(1.0, 1.0, 1.0, 0.35);
        frame.stroke_rectangle(
            rect.position(),
            rect.size(),
            Stroke::default().with_color(border).with_width(1.0),
        );
        // Thirds: two lines each way, drawn inside the rectangle.
        for step in [1.0f32, 2.0] {
            let x = rect.x + rect.width * step / 3.0;
            let y = rect.y + rect.height * step / 3.0;
            frame.stroke(
                &Path::line(Point::new(x, rect.y), Point::new(x, rect.y + rect.height)),
                Stroke::default().with_color(guides).with_width(1.0),
            );
            frame.stroke(
                &Path::line(Point::new(rect.x, y), Point::new(rect.x + rect.width, y)),
                Stroke::default().with_color(guides).with_width(1.0),
            );
        }
        // Eight handles, drawn at the same size whatever the zoom.
        for point in handle_points(&self.draft.rect) {
            let centre = self.view.canvas_point(point.0, point.1);
            frame.fill_rectangle(
                Point::new(centre.x - HANDLE_SIZE / 2.0, centre.y - HANDLE_SIZE / 2.0),
                Size::new(HANDLE_SIZE, HANDLE_SIZE),
                border,
            );
        }
        if let Some((from, to)) = self.draft.guide_line() {
            frame.stroke(
                &Path::line(
                    self.view.canvas_point(from.0, from.1),
                    self.view.canvas_point(to.0, to.1),
                ),
                Stroke::default()
                    .with_color(Color::from_rgb(1.0, 0.85, 0.2))
                    .with_width(2.0),
            );
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if self.part == Part::Photo {
            return mouse::Interaction::None;
        }
        if state.pan_from.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if self.mode == Mode::Pan {
            return mouse::Interaction::Grab;
        }
        if self.mode == Mode::Guide {
            return mouse::Interaction::Crosshair;
        }
        let Some(point) = cursor.position_in(bounds) else {
            return mouse::Interaction::None;
        };
        cursor_for(self.handle_at(point))
    }
}

/// The cursor one handle deserves.
fn cursor_for(handle: Handle) -> mouse::Interaction {
    match handle {
        Handle::Corner(Corner::TopLeft | Corner::BottomRight) => {
            mouse::Interaction::ResizingDiagonallyDown
        }
        Handle::Corner(Corner::TopRight | Corner::BottomLeft) => {
            mouse::Interaction::ResizingDiagonallyUp
        }
        Handle::Side(Edge::Left | Edge::Right) => mouse::Interaction::ResizingHorizontally,
        Handle::Side(Edge::Top | Edge::Bottom) => mouse::Interaction::ResizingVertically,
        Handle::Move => mouse::Interaction::Move,
        Handle::Guide => mouse::Interaction::Crosshair,
    }
}

/// The eight handle positions in box pixels: four corners and four edge midpoints.
fn handle_points(rect: &BoxRect) -> Vec<(f64, f64)> {
    let mut points: Vec<(f64, f64)> = Corner::ALL
        .into_iter()
        .map(|corner| corner.point(rect))
        .collect();
    points.extend(
        [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom]
            .into_iter()
            .map(|edge| edge_midpoint(edge, rect)),
    );
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightwell_core::CropStage;

    fn stage(width: u32, height: u32, angle: f64) -> CropStage {
        CropStage {
            width,
            height,
            angle,
        }
    }

    #[test]
    fn the_image_is_placed_so_its_rotated_bounds_are_the_box() {
        // Iced rotates about the bounds centre and reports the rotated bounding box, which is
        // exactly the geometry contract's box: place the unrotated stage on the box centre.
        for angle in [0.0, 7.0, -22.5, 45.0] {
            let mut draft = CropDraft::neutral(stage(480, 320, 0.0), 0, 0);
            draft.set_angle(angle);
            assert_eq!(draft.stage.angle, angle);
            let (box_width, box_height) = draft.stage.bounding_box();
            let view = CanvasView::percent(100.0, 1.0).expect("a percent view");
            let canvas = CropCanvas::new(
                &draft,
                DraftPhoto::unallocated(480, 320),
                view,
                Mode::Frame,
                false,
                Part::Photo,
            );
            let bounds = canvas.image_bounds();
            assert_eq!(bounds.width, 480.0, "{angle}");
            assert_eq!(bounds.height, 320.0, "{angle}");
            let centre = bounds.center();
            // Canvas coordinates are f32, so agreement is checked to a thousandth of a pixel.
            assert!(
                (f64::from(centre.x) - box_width / 2.0).abs() < 1e-3
                    && (f64::from(centre.y) - box_height / 2.0).abs() < 1e-3,
                "{angle}: the image centre {centre:?} is not the box centre"
            );
            // The corners of the rotated image land where the box mapping puts them.
            for (u, v) in [(0.0, 0.0), (480.0, 0.0), (0.0, 320.0), (480.0, 320.0)] {
                let (x, y) = draft.stage.to_box(u, v);
                let rotated = rotate_about(
                    Point::new(bounds.x + u as f32, bounds.y + v as f32),
                    centre,
                    angle,
                );
                assert!(
                    (f64::from(rotated.x) - x).abs() < 1e-3
                        && (f64::from(rotated.y) - y).abs() < 1e-3,
                    "{angle}: ({u}, {v}) drew at {rotated:?}, not ({x}, {y})"
                );
            }
        }
    }

    /// The rotation iced's image shader applies: the geometry contract's matrix, unchanged, which is
    /// clockwise on a y-down screen.
    fn rotate_about(point: Point, centre: Point, degrees: f64) -> Point {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let dx = f64::from(point.x - centre.x);
        let dy = f64::from(point.y - centre.y);
        Point::new(
            centre.x + (cos * dx - sin * dy) as f32,
            centre.y + (sin * dx + cos * dy) as f32,
        )
    }

    #[test]
    fn only_the_overlay_part_answers_pointers() {
        use canvas::Program;
        let draft = CropDraft::neutral(stage(480, 320, 0.0), 0, 0);
        let view = CanvasView::percent(100.0, 1.0).expect("a percent view");
        let bounds = Rectangle::new(Point::new(0.0, 0.0), Size::new(480.0, 320.0));
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let cursor = Cursor::Available(Point::new(10.0, 10.0));
        for (part, answers) in [(Part::Photo, false), (Part::Overlay, true)] {
            let canvas = CropCanvas::new(
                &draft,
                DraftPhoto::unallocated(480, 320),
                view,
                Mode::Frame,
                false,
                part,
            );
            let mut state = Interaction::default();
            assert_eq!(
                canvas.update(&mut state, &press, bounds, cursor).is_some(),
                answers,
                "{part:?}"
            );
            assert_eq!(
                canvas.mouse_interaction(&state, bounds, cursor) == mouse::Interaction::None,
                !answers,
                "{part:?}"
            );
        }
    }

    #[test]
    fn every_handle_has_a_drawn_position_and_a_cursor() {
        let rect = BoxRect {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        let points = handle_points(&rect);
        assert_eq!(points.len(), 8);
        assert!(points.contains(&(10.0, 20.0)));
        assert!(points.contains(&(110.0, 70.0)));
        assert!(points.contains(&(10.0, 45.0)), "the left edge midpoint");
        assert!(points.contains(&(60.0, 70.0)), "the bottom edge midpoint");
        assert_eq!(
            cursor_for(Handle::Corner(Corner::TopLeft)),
            mouse::Interaction::ResizingDiagonallyDown
        );
        assert_eq!(
            cursor_for(Handle::Side(Edge::Top)),
            mouse::Interaction::ResizingVertically
        );
        assert_eq!(cursor_for(Handle::Move), mouse::Interaction::Move);
        assert_eq!(cursor_for(Handle::Guide), mouse::Interaction::Crosshair);
    }
}
