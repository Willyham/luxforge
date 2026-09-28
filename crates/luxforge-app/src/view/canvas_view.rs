//! The one view transform every canvas over the photograph draws through, and the one ellipse
//! builder its figures share.
//!
//! The crop frame and the mask handles are drawn by two canvases over two different stages — the
//! crop layer's rotated box and the output stage — but both put that stage on screen the same two
//! ways the photo surface itself does: Fit centres it at the largest scale that shows all of it, and
//! a percentage zoom draws it at its own size with the origin at the corner while the surrounding
//! scrollable handles the offset. [`CanvasView`] is that mapping, in both directions, and the hit
//! radius through it. It holds no editing state and answers no host question: a pointer is mapped
//! here, locally, on every move, which is what keeps a runtime hop off the input path
//! ([performance rule 12](../../docs/engineering/performance-rules.md)).
use iced::{Point, Rectangle, Size, Vector, widget::canvas::Path};

/// Where a stage is drawn inside a canvas, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CanvasView {
    /// Stage pixels to logical pixels.
    pub(crate) scale: f32,
    /// Where stage (0, 0) sits inside the canvas, in logical pixels.
    pub(crate) origin: Vector,
}

impl CanvasView {
    /// Fit: the largest scale that shows the whole stage, centred in `available`. A degenerate stage
    /// or surface has no view rather than a nonsense one.
    pub(crate) fn fit(stage: (f64, f64), available: Size) -> Option<Self> {
        let (width, height) = (stage.0 as f32, stage.1 as f32);
        if !(width > 0.0 && height > 0.0 && available.width > 0.0 && available.height > 0.0) {
            return None;
        }
        let scale = (available.width / width).min(available.height / height);
        (scale.is_finite() && scale > 0.0).then_some(Self {
            scale,
            origin: Vector::new(
                (available.width - width * scale) / 2.0,
                (available.height - height * scale) / 2.0,
            ),
        })
    }

    /// A percentage zoom: `value / 100 / display scale`, the same arithmetic the plain image path
    /// uses, so 100% keeps one stage pixel per physical pixel.
    pub(crate) fn percent(value: f32, scale_factor: f32) -> Option<Self> {
        let scale = value / 100.0 / scale_factor;
        (scale.is_finite() && scale > 0.0).then_some(Self {
            scale,
            origin: Vector::new(0.0, 0.0),
        })
    }

    /// Canvas-local logical pixels to stage pixels.
    pub(crate) fn stage_point(self, point: Point) -> (f64, f64) {
        (
            f64::from((point.x - self.origin.x) / self.scale),
            f64::from((point.y - self.origin.y) / self.scale),
        )
    }

    /// Stage pixels to canvas-local logical pixels.
    pub(crate) fn canvas_point(self, x: f64, y: f64) -> Point {
        Point::new(
            self.origin.x + x as f32 * self.scale,
            self.origin.y + y as f32 * self.scale,
        )
    }

    /// A stage-pixel rectangle as the canvas-local one it is drawn at.
    pub(crate) fn canvas_rect(self, x: f64, y: f64, width: f64, height: f64) -> Rectangle {
        Rectangle::new(
            self.canvas_point(x, y),
            Size::new(width as f32 * self.scale, height as f32 * self.scale),
        )
    }

    /// A hit radius of `radius` logical pixels in stage pixels, so a handle is the same size on
    /// screen at every zoom.
    pub(crate) fn tolerance(self, radius: f32) -> f64 {
        f64::from(radius / self.scale)
    }
}

/// How many segments one drawn ellipse is built from. Fixed, so a figure costs the same at every
/// zoom, and fine enough that the boundary reads as a curve on a full-screen ellipse.
const ELLIPSE_STEPS: usize = 96;

/// One closed ellipse as a canvas path: `radii` about `centre`, turned by `angle` radians, all in the
/// caller's own space, with every point taken through `map` to the canvas.
///
/// The ellipse is built in the space the caller's geometry is defined in and mapped point by point,
/// so a figure that is a circle in that space stays one under whatever `map` does — a mask's ellipse
/// is a circle in mask space and follows a crop or a quarter turn with the picture. A dashed ellipse
/// is drawn as alternate segments rather than with a dash pattern, so it reads as the softer of two
/// figures at every zoom. [`ELLIPSE_STEPS`] segments is what a bounded figure costs: the path is
/// rebuilt per frame like every other canvas figure, independent of zoom.
pub(crate) fn ellipse(
    centre: (f64, f64),
    radii: (f64, f64),
    angle: f64,
    dashed: bool,
    map: impl Fn(f64, f64) -> Point,
) -> Path {
    let (ca, sa) = (angle.cos(), angle.sin());
    let point = |step: usize| {
        let t = step as f64 / ELLIPSE_STEPS as f64 * std::f64::consts::TAU;
        let (a, b) = (radii.0 * t.cos(), radii.1 * t.sin());
        map(centre.0 + (ca * a - sa * b), centre.1 + (sa * a + ca * b))
    };
    Path::new(|builder| {
        builder.move_to(point(0));
        for step in 1..=ELLIPSE_STEPS {
            if dashed && step % 2 == 0 {
                builder.move_to(point(step));
            } else {
                builder.line_to(point(step));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_centres_the_stage_and_maps_pointers_through_it_both_ways() {
        // A 200x100 stage in a 400x400 surface: scale 2, centred vertically like the image path.
        let view = CanvasView::fit((200.0, 100.0), Size::new(400.0, 400.0)).expect("a view");
        assert_eq!(view.scale, 2.0);
        assert_eq!(view.origin, Vector::new(0.0, 100.0));
        assert_eq!(view.stage_point(Point::new(0.0, 100.0)), (0.0, 0.0));
        assert_eq!(view.stage_point(Point::new(400.0, 300.0)), (200.0, 100.0));
        assert_eq!(
            view.stage_point(Point::new(200.0, 200.0)),
            (100.0, 50.0),
            "the centre of the surface is the centre of the stage"
        );
        // The mapping is invertible, so a drawn handle sits where a press on it lands.
        for (x, y) in [(0.0, 0.0), (37.0, 91.0), (200.0, 100.0)] {
            assert_eq!(view.stage_point(view.canvas_point(x, y)), (x, y));
        }
        assert_eq!(
            view.canvas_rect(10.0, 20.0, 30.0, 40.0),
            Rectangle::new(Point::new(20.0, 140.0), Size::new(60.0, 80.0))
        );
        // A degenerate stage or surface has no view rather than a nonsense one.
        assert!(CanvasView::fit((0.0, 100.0), Size::new(400.0, 400.0)).is_none());
        assert!(CanvasView::fit((200.0, 100.0), Size::new(0.0, 400.0)).is_none());
    }

    #[test]
    fn a_percentage_zoom_keeps_one_stage_pixel_per_physical_pixel_at_one_hundred() {
        // 100% on a 2x display draws every stage pixel at half a logical pixel, which is one
        // physical pixel: the same arithmetic the plain percent image path uses.
        let view = CanvasView::percent(100.0, 2.0).expect("a percent view");
        assert_eq!(view.scale, 0.5);
        assert_eq!(view.origin, Vector::new(0.0, 0.0));
        assert_eq!(view.stage_point(Point::new(50.0, 25.0)), (100.0, 50.0));
        // Inside the scrollable the reported point is already content space, so no pan enters here.
        let view = CanvasView::percent(200.0, 1.0).expect("a percent view");
        assert_eq!(view.stage_point(Point::new(317.0, 9.0)), (158.5, 4.5));
        assert!(CanvasView::percent(0.0, 1.0).is_none());
        assert!(CanvasView::percent(100.0, 0.0).is_none());
        assert!(CanvasView::percent(f32::NAN, 1.0).is_none());
    }

    #[test]
    fn the_hit_radius_is_constant_on_screen_at_every_zoom() {
        let zoomed = CanvasView::percent(400.0, 1.0).expect("a percent view");
        assert_eq!(zoomed.tolerance(8.0), 2.0);
        let shrunk = CanvasView::percent(25.0, 1.0).expect("a percent view");
        assert_eq!(shrunk.tolerance(8.0), 32.0);
    }
}
