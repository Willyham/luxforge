//! The high-zoom minimap's geometry (`docs/design/minimap.md`): where the card sits in the canvas,
//! where the visible region of the stage falls on its thumbnail, and the pan a click or a drag on
//! the thumbnail asks for. Pure arithmetic over the same placement the photo surface lays the
//! photograph out by ([`CanvasView::scrolled`]), so the rectangle drawn and the navigation it
//! drives agree with the main view by construction; nothing here holds or changes editing state.
//!
//! Every length is in canvas-local logical pixels, the units of the scrollable the photograph pans
//! in, whose offset is `view.set`'s pan. A stage is whatever the main view is navigating: the
//! output stage, or a crop draft's rotated input-stage box. The widget that draws the card and
//! captures the pointer consumes this and publishes the resulting pan through the existing view
//! command; it decides nothing itself.
//!
//! The rectangle is the geometric viewport, unsnapped: the photo surface snaps the photograph's
//! edges to the physical pixel grid, which moves them by under half a physical pixel, a quarter of
//! a stage pixel at 200% and less above, far below one pixel of a 256-pixel overview.
#![cfg_attr(not(test), allow(dead_code))]

use crate::view::canvas_view::CanvasView;
use iced::{Point, Rectangle, Size};
use luxforge_core::Zoom;

/// The largest image area of the card, in logical pixels: the thumbnail is aspect-fitted inside
/// it, centred, and the letterboxing around it is inert.
pub(crate) const IMAGE_AREA: Size = Size::new(180.0, 120.0);

/// The card's distance from the viewport's right and bottom edges, and from the mode strip when it
/// is lifted over it: the chrome inset every floating card keeps.
pub(crate) const INSET: f32 = luxforge_ui::theme::CHROME_INSET;

/// The card's padding around its image area.
pub(crate) const PADDING: f32 = 4.0;

/// The smallest image-area height drawn; a canvas that leaves less shows no minimap.
pub(crate) const MIN_IMAGE_HEIGHT: f32 = 48.0;

/// The most of the viewport's width, and of its height, the card takes before it shrinks.
const MAX_SHARE: f32 = 0.4;

/// What the main view shows at a percentage zoom that shows the minimap: `stage` pixels drawn at
/// `percent` on a display of `scale_factor` physical pixels a logical pixel, in a `viewport` of
/// logical pixels scrolled by `pan`, the scrollable's offset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MainView {
    view: CanvasView,
    stage: (f64, f64),
    viewport: Size,
    pan: (f32, f32),
}

/// A rectangle of stage pixels, `[x0, x1) × [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StageRect {
    pub(crate) x0: f64,
    pub(crate) y0: f64,
    pub(crate) x1: f64,
    pub(crate) y1: f64,
}

impl StageRect {
    fn centre(self) -> (f64, f64) {
        ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }
}

impl MainView {
    /// The main view at `zoom`, where it shows the minimap ([`Zoom::shows_overview`]): `None` at
    /// Fit, below 200%, and for a degenerate stage, viewport, display scale or pan, which leave no
    /// actionable geometry.
    pub(crate) fn new(
        zoom: &Zoom,
        stage: (f64, f64),
        viewport: Size,
        scale_factor: f32,
        pan: (f32, f32),
    ) -> Option<Self> {
        let Zoom::Percent { value } = *zoom else {
            return None;
        };
        if !zoom.shows_overview()
            || !(viewport.width.is_finite() && viewport.height.is_finite())
            || viewport.width <= 0.0
            || viewport.height <= 0.0
            || !(pan.0.is_finite() && pan.1.is_finite())
        {
            return None;
        }
        let view = CanvasView::scrolled(value, scale_factor, stage, viewport, pan)?;
        (view.origin.x.is_finite() && view.origin.y.is_finite()).then_some(Self {
            view,
            stage,
            viewport,
            pan,
        })
    }

    /// The part of the stage the viewport shows, the viewport's edges mapped back to stage pixels
    /// and clipped to the stage: all of an axis that fits. The area under floating chrome is part
    /// of it.
    pub(crate) fn visible(&self) -> StageRect {
        let (x0, y0) = self.view.stage_point(Point::ORIGIN);
        let (x1, y1) = self
            .view
            .stage_point(Point::new(self.viewport.width, self.viewport.height));
        StageRect {
            x0: x0.clamp(0.0, self.stage.0),
            y0: y0.clamp(0.0, self.stage.1),
            x1: x1.clamp(0.0, self.stage.0),
            y1: y1.clamp(0.0, self.stage.1),
        }
    }

    /// The pan that puts stage point `centre` at the viewport's centre, clamped to the scrollable's
    /// range so the photograph's edge never leaves the viewport's. An axis that fits has nothing to
    /// scroll and keeps the pan it has, so navigation there changes nothing; a non-finite point
    /// changes nothing either.
    pub(crate) fn pan_centring(&self, centre: (f64, f64)) -> (f32, f32) {
        let scale = f64::from(self.view.scale);
        let axis = |stage: f64, available: f32, centre: f64, pan: f32| {
            // Finite: the stage, the scale and the viewport are, or there is no view.
            let range = stage * scale - f64::from(available);
            if range <= 0.0 || !centre.is_finite() {
                return pan;
            }
            (centre * scale - f64::from(available) / 2.0).clamp(0.0, range) as f32
        };
        (
            axis(self.stage.0, self.viewport.width, centre.0, self.pan.0),
            axis(self.stage.1, self.viewport.height, centre.1, self.pan.1),
        )
    }
}

/// Where the minimap is laid out inside the viewport, in viewport-local logical pixels: the card,
/// its image area, and the thumbnail of the stage fitted inside that area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub(crate) card: Rectangle,
    pub(crate) area: Rectangle,
    pub(crate) image: Rectangle,
    stage: (f64, f64),
}

impl Placement {
    /// The card for a stage of `stage` pixels in a `viewport`, beside a mode strip of `strip`
    /// logical pixels at the viewport's bottom centre, [`INSET`] above its edge: anchored at the
    /// lower right, [`INSET`] in; shrunk where the viewport is small, so it takes at most
    /// [`MAX_SHARE`] of either side; lifted over the strip where the two would overlap. `None`
    /// where the image area would be shorter than [`MIN_IMAGE_HEIGHT`] or the card would not fit,
    /// and for a degenerate stage or viewport.
    pub(crate) fn new(viewport: Size, stage: (f64, f64), strip: Size) -> Option<Self> {
        let finite = |size: Size| size.width.is_finite() && size.height.is_finite();
        if !(finite(viewport)
            && finite(strip)
            && stage.0.is_finite()
            && stage.1.is_finite()
            && stage.0 > 0.0
            && stage.1 > 0.0
            && viewport.width > 0.0
            && viewport.height > 0.0)
        {
            return None;
        }
        let shrink = 1f32
            .min((viewport.width * MAX_SHARE - 2.0 * PADDING) / IMAGE_AREA.width)
            .min((viewport.height * MAX_SHARE - 2.0 * PADDING) / IMAGE_AREA.height);
        let area = Size::new(IMAGE_AREA.width * shrink, IMAGE_AREA.height * shrink);
        if area.height < MIN_IMAGE_HEIGHT {
            return None;
        }
        let card = Size::new(area.width + 2.0 * PADDING, area.height + 2.0 * PADDING);
        let left = viewport.width - INSET - card.width;
        let strip_right = (viewport.width + strip.width) / 2.0;
        let mut bottom = viewport.height - INSET;
        if strip.width > 0.0 && strip.height > 0.0 && left < strip_right + INSET {
            bottom -= strip.height + INSET;
        }
        let top = bottom - card.height;
        if left < INSET || top < INSET {
            return None;
        }
        let card = Rectangle::new(Point::new(left, top), card);
        let area = Rectangle::new(Point::new(left + PADDING, top + PADDING), area);
        let fit = (f64::from(area.width) / stage.0).min(f64::from(area.height) / stage.1);
        let size = Size::new((stage.0 * fit) as f32, (stage.1 * fit) as f32);
        let image = Rectangle::new(
            Point::new(
                area.x + (area.width - size.width) / 2.0,
                area.y + (area.height - size.height) / 2.0,
            ),
            size,
        );
        Some(Self {
            card,
            area,
            image,
            stage,
        })
    }

    /// Thumbnail logical pixels a stage pixel.
    fn scale(&self) -> f64 {
        f64::from(self.image.width) / self.stage.0
    }

    /// `rect` of the stage on the thumbnail.
    pub(crate) fn rectangle(&self, rect: StageRect) -> Rectangle {
        let scale = self.scale();
        Rectangle::new(
            Point::new(
                self.image.x + (rect.x0 * scale) as f32,
                self.image.y + (rect.y0 * scale) as f32,
            ),
            Size::new(
                ((rect.x1 - rect.x0) * scale) as f32,
                ((rect.y1 - rect.y0) * scale) as f32,
            ),
        )
    }

    /// The stage point under `point`, wherever it lies: a captured drag reads points outside the
    /// thumbnail too.
    fn stage_point(&self, point: Point) -> (f64, f64) {
        let scale = self.scale();
        (
            f64::from(point.x - self.image.x) / scale,
            f64::from(point.y - self.image.y) / scale,
        )
    }

    /// The stage point under `point` on the thumbnail; `None` in the letterboxing, outside the
    /// card, and for a non-finite point.
    pub(crate) fn stage_at(&self, point: Point) -> Option<(f64, f64)> {
        let inside = point.x >= self.image.x
            && point.x <= self.image.x + self.image.width
            && point.y >= self.image.y
            && point.y <= self.image.y + self.image.height;
        inside.then(|| self.stage_point(point))
    }
}

/// A press on the thumbnail and the drag that may follow it: the stage offset from the visible
/// region's centre to where it was grabbed, which a drag keeps under the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Navigation {
    grab: (f64, f64),
}

impl Navigation {
    /// A press at `point`: on the viewport rectangle it grabs it where it was pressed, leaving the
    /// pan as it is; elsewhere on the thumbnail it centres the view there, and a drag continues
    /// from the centre. `None` in the letterboxing, outside the thumbnail, and for a non-finite
    /// point: nothing to navigate. The pan answered is the one to ask `view.set` for.
    pub(crate) fn press(
        placement: &Placement,
        main: &MainView,
        point: Point,
    ) -> Option<(Self, (f32, f32))> {
        let at = placement.stage_at(point)?;
        let visible = main.visible();
        let on_rectangle =
            at.0 >= visible.x0 && at.0 <= visible.x1 && at.1 >= visible.y0 && at.1 <= visible.y1;
        if on_rectangle {
            let centre = visible.centre();
            let grab = (at.0 - centre.0, at.1 - centre.1);
            return Some((Self { grab }, main.pan));
        }
        Some((Self { grab: (0.0, 0.0) }, main.pan_centring(at)))
    }

    /// The pan a drag to `point` asks for, the grab kept under the pointer and clamped to the
    /// photograph's edges wherever the pointer goes; a non-finite point keeps the pan.
    pub(crate) fn drag(&self, placement: &Placement, main: &MainView, point: Point) -> (f32, f32) {
        let at = placement.stage_point(point);
        main.pan_centring((at.0 - self.grab.0, at.1 - self.grab.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn percent(value: f32) -> Zoom {
        Zoom::Percent { value }
    }

    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 1e-3
    }

    fn assert_rect(actual: Rectangle, expected: [f32; 4]) {
        let actual_edges = [actual.x, actual.y, actual.width, actual.height];
        assert!(
            actual_edges
                .iter()
                .zip(expected)
                .all(|(a, e)| (a - e).abs() < 1e-3),
            "{actual_edges:?} != {expected:?}"
        );
    }

    fn assert_stage(actual: StageRect, expected: [f64; 4]) {
        let edges = [actual.x0, actual.y0, actual.x1, actual.y1];
        assert!(
            edges.iter().zip(expected).all(|(a, e)| close(*a, e)),
            "{edges:?} != {expected:?}"
        );
    }

    const VIEWPORT: Size = Size::new(1000.0, 700.0);
    const NO_STRIP: Size = Size::new(0.0, 0.0);

    #[test]
    fn the_minimap_is_eligible_from_two_hundred_percent_of_the_stored_value() {
        let stage = (6000.0, 4000.0);
        let main = |zoom: Zoom| MainView::new(&zoom, stage, VIEWPORT, 1.0, (0.0, 0.0));
        assert!(
            main(Zoom::Fit).is_none(),
            "Fit never, however large it draws"
        );
        assert!(main(percent(199.9)).is_none());
        assert!(main(percent(200.0)).is_some());
        assert!(main(percent(200.1)).is_some());
        assert!(main(percent(1600.0)).is_some());
    }

    /// A 6000x4000 stage at 200% on a 1x display draws 12000x8000 logical pixels in a 1000x700
    /// viewport: every figure below is worked by hand, not through the code under test.
    #[test]
    fn the_rectangle_follows_the_viewport_at_the_centre_and_every_edge() {
        let stage = (6000.0, 4000.0);
        let placement = Placement::new(VIEWPORT, stage, NO_STRIP).unwrap();
        // The card hugs the lower right, 12 in: 188x128 with a 4-pixel padding round 180x120,
        // which a 3:2 stage fills exactly at 0.03 thumbnail pixels a stage pixel.
        assert_rect(placement.card, [800.0, 560.0, 188.0, 128.0]);
        assert_rect(placement.image, [804.0, 564.0, 180.0, 120.0]);
        for (pan, visible, drawn) in [
            (
                (0.0, 0.0),
                [0.0, 0.0, 500.0, 350.0],
                [804.0, 564.0, 15.0, 10.5],
            ),
            (
                (5500.0, 3650.0),
                [2750.0, 1825.0, 3250.0, 2175.0],
                [886.5, 618.75, 15.0, 10.5],
            ),
            (
                (11000.0, 7300.0),
                [5500.0, 3650.0, 6000.0, 4000.0],
                [969.0, 673.5, 15.0, 10.5],
            ),
            (
                (11000.0, 0.0),
                [5500.0, 0.0, 6000.0, 350.0],
                [969.0, 564.0, 15.0, 10.5],
            ),
            (
                (0.0, 7300.0),
                [0.0, 3650.0, 500.0, 4000.0],
                [804.0, 673.5, 15.0, 10.5],
            ),
            // A pan past the overhang is where the scrollable holds it: at the far edge.
            (
                (90000.0, -50.0),
                [5500.0, 0.0, 6000.0, 350.0],
                [969.0, 564.0, 15.0, 10.5],
            ),
        ] {
            let main = MainView::new(&percent(200.0), stage, VIEWPORT, 1.0, pan).unwrap();
            assert_stage(main.visible(), visible);
            assert_rect(placement.rectangle(main.visible()), drawn);
        }
    }

    #[test]
    fn a_click_centres_the_view_there_and_clamps_at_the_edges() {
        let stage = (6000.0, 4000.0);
        let placement = Placement::new(VIEWPORT, stage, NO_STRIP).unwrap();
        let main = MainView::new(&percent(200.0), stage, VIEWPORT, 1.0, (0.0, 0.0)).unwrap();
        // The thumbnail's centre is stage (3000, 2000): at 2 logical pixels a stage pixel its
        // centre lies 500 and 350 in from the viewport's corner at a pan of (5500, 3650).
        let (_, pan) = Navigation::press(&placement, &main, Point::new(894.0, 624.0)).unwrap();
        assert_eq!(pan, (5500.0, 3650.0));
        // From there, near the top-left corner the view clamps to the origin; near the far one
        // to the overhang, 12000 - 1000 and 8000 - 700.
        let main = MainView::new(&percent(200.0), stage, VIEWPORT, 1.0, pan).unwrap();
        let (_, pan) = Navigation::press(&placement, &main, Point::new(805.0, 565.0)).unwrap();
        assert_eq!(pan, (0.0, 0.0));
        let (_, pan) = Navigation::press(&placement, &main, Point::new(983.0, 683.0)).unwrap();
        assert_eq!(pan, (11000.0, 7300.0));
        // A press on the card's padding or outside it navigates nothing.
        assert!(Navigation::press(&placement, &main, Point::new(801.0, 600.0)).is_none());
        assert!(Navigation::press(&placement, &main, Point::new(500.0, 300.0)).is_none());
        assert!(Navigation::press(&placement, &main, Point::new(f32::NAN, 600.0)).is_none());
    }

    #[test]
    fn a_drag_keeps_its_grab_under_the_pointer_and_stays_bounded() {
        let stage = (6000.0, 4000.0);
        let placement = Placement::new(VIEWPORT, stage, NO_STRIP).unwrap();
        let start = (2000.0, 1000.0);
        let main = MainView::new(&percent(200.0), stage, VIEWPORT, 1.0, start).unwrap();
        // The rectangle covers stage [1000, 1500) x [500, 850), thumbnail x 834..849, y 579..589.5;
        // grabbed 3 thumbnail pixels right of and 2 above its centre (841.5, 584.25).
        let (navigation, pan) =
            Navigation::press(&placement, &main, Point::new(844.5, 582.25)).unwrap();
        assert_eq!(pan, start, "grabbing the rectangle does not move it");
        // Moved 30 right and 15 down on the thumbnail, 1000 and 500 stage pixels: the pan moves
        // by twice that at 200%.
        let pan = navigation.drag(&placement, &main, Point::new(874.5, 597.25));
        assert!(
            close(f64::from(pan.0), 4000.0) && close(f64::from(pan.1), 2000.0),
            "{pan:?}"
        );
        // Dragged far beyond the card, the pan stops at the photograph's edges.
        assert_eq!(
            navigation.drag(&placement, &main, Point::new(5000.0, 5000.0)),
            (11000.0, 7300.0)
        );
        assert_eq!(
            navigation.drag(&placement, &main, Point::new(-5000.0, -5000.0)),
            (0.0, 0.0)
        );
        // A pointer the platform reports as not a number keeps the pan.
        assert_eq!(
            navigation.drag(&placement, &main, Point::new(f32::NAN, f32::INFINITY)),
            start
        );
        // Moving the view to the pan the drag asked for puts the grab point back under the
        // pointer: the rectangle's centre is 3 left of and 2 below it.
        let pointer = Point::new(874.5, 597.25);
        let pan = navigation.drag(&placement, &main, pointer);
        let moved = MainView::new(&percent(200.0), stage, VIEWPORT, 1.0, pan).unwrap();
        let drawn = placement.rectangle(moved.visible());
        assert!((drawn.center_x() - (pointer.x - 3.0)).abs() < 1e-3);
        assert!((drawn.center_y() - (pointer.y + 2.0)).abs() < 1e-3);
    }

    /// A portrait stage at 400% on a 2x display is letterboxed in the card, and an axis that fits
    /// the viewport stays centred and keeps its pan whatever is clicked.
    #[test]
    fn portrait_letterboxing_is_inert_and_a_fitting_axis_keeps_its_pan() {
        // 4000x6000 at 400% on 2x: 2 logical pixels a stage pixel, 8000x12000 of content.
        let stage = (4000.0, 6000.0);
        let placement = Placement::new(VIEWPORT, stage, NO_STRIP).unwrap();
        // 120 high and 80 wide, centred in the 180x120 area: 50 of letterbox each side.
        assert_rect(placement.image, [854.0, 564.0, 80.0, 120.0]);
        let main = MainView::new(&percent(400.0), stage, VIEWPORT, 2.0, (100.0, 200.0)).unwrap();
        assert_stage(main.visible(), [50.0, 100.0, 550.0, 450.0]);
        assert_rect(
            placement.rectangle(main.visible()),
            [855.0, 566.0, 10.0, 7.0],
        );
        assert!(Navigation::press(&placement, &main, Point::new(820.0, 620.0)).is_none());
        assert!(Navigation::press(&placement, &main, Point::new(950.0, 620.0)).is_none());
        // 600x4000 at 200% on 2x is 600 wide, narrower than the viewport: centred, all of it
        // visible, and the pan across is kept, however stale.
        let narrow = (600.0, 4000.0);
        let placement = Placement::new(VIEWPORT, narrow, NO_STRIP).unwrap();
        let main = MainView::new(&percent(200.0), narrow, VIEWPORT, 2.0, (37.0, 0.0)).unwrap();
        assert_stage(main.visible(), [0.0, 0.0, 600.0, 700.0]);
        let (_, pan) = Navigation::press(
            &placement,
            &main,
            Point::new(
                placement.image.x + 1.0,
                placement.image.y + placement.image.height - 1.0,
            ),
        )
        .unwrap();
        assert_eq!(
            pan,
            (37.0, 3300.0),
            "x keeps its pan; y clamps to 4000 - 700"
        );
        // A stage that fits on both axes shows a full rectangle and navigation changes nothing.
        let small = (300.0, 200.0);
        let placement = Placement::new(VIEWPORT, small, NO_STRIP).unwrap();
        let main = MainView::new(&percent(200.0), small, VIEWPORT, 1.0, (0.0, 0.0)).unwrap();
        assert_stage(main.visible(), [0.0, 0.0, 300.0, 200.0]);
        assert_rect(
            placement.rectangle(main.visible()),
            [804.0, 564.0, 180.0, 120.0],
        );
        let (_, pan) = Navigation::press(&placement, &main, Point::new(810.0, 570.0)).unwrap();
        assert_eq!(pan, (0.0, 0.0));
    }

    /// The display's physical pixels a logical pixel — the system's scale times the interface
    /// size — divide the zoom, and a crop draft's rotated box, a non-integer stage, maps the same
    /// way.
    #[test]
    fn interface_scale_and_a_crop_box_map_through_the_effective_scale() {
        // 250% at 2x and a 125% interface: 2.5 physical pixels a logical pixel, so one stage
        // pixel a logical pixel; the viewport is 800x560 logical pixels.
        let viewport = Size::new(800.0, 560.0);
        let stage = (4123.5, 2950.25);
        let main = MainView::new(&percent(250.0), stage, viewport, 2.5, (1000.0, 500.0)).unwrap();
        assert_stage(main.visible(), [1000.0, 500.0, 1800.0, 1060.0]);
        let edge = MainView::new(&percent(250.0), stage, viewport, 2.5, (9e9, 9e9)).unwrap();
        assert_stage(edge.visible(), [3323.5, 2390.25, 4123.5, 2950.25]);
        // At 1x the same 250% is 2.5 logical pixels a stage pixel: a third of the region.
        let main = MainView::new(&percent(250.0), stage, viewport, 1.0, (1000.0, 500.0)).unwrap();
        assert_stage(main.visible(), [400.0, 200.0, 720.0, 424.0]);
        // Centring stage (2000, 1500) at 2.5: 5000 - 400 and 3750 - 280.
        assert_eq!(main.pan_centring((2000.0, 1500.0)), (4600.0, 3470.0));
        // The card: 0.4 of 560 is 224, roomy; a 4123.5x2950.25 box is fitted by height.
        let placement = Placement::new(viewport, stage, NO_STRIP).unwrap();
        assert_rect(placement.card, [600.0, 420.0, 188.0, 128.0]);
        let width = (4123.5 * 120.0 / 2950.25) as f32;
        assert_rect(
            placement.image,
            [604.0 + (180.0 - width) / 2.0, 424.0, width, 120.0],
        );
    }

    #[test]
    fn the_card_shrinks_and_lifts_clear_of_the_mode_strip_on_a_small_canvas() {
        let stage = (6000.0, 4000.0);
        let strip = Size::new(300.0, 44.0);
        // A wide canvas leaves the card clear of the strip's half: it stays at the bottom.
        let wide = Placement::new(Size::new(1400.0, 800.0), stage, strip).unwrap();
        assert_rect(wide.card, [1200.0, 660.0, 188.0, 128.0]);
        // 0.4 of 400 is 160: an image area of 152 x 101.33, whose card would reach under the strip
        // (its left, 228, is inside the strip's right edge, 350, plus the inset), so it lifts by
        // the strip and an inset.
        let small = Placement::new(Size::new(400.0, 300.0), stage, strip).unwrap();
        let area_height = 120.0 * 152.0 / 180.0;
        assert_rect(
            small.card,
            [228.0, 232.0 - (area_height + 8.0), 160.0, area_height + 8.0],
        );
        // Too small for a 48-pixel image area, or no room above a tall strip: no minimap.
        assert!(Placement::new(Size::new(200.0, 120.0), stage, NO_STRIP).is_none());
        assert!(Placement::new(Size::new(400.0, 150.0), stage, strip).is_some());
        assert!(Placement::new(Size::new(400.0, 150.0), stage, Size::new(300.0, 100.0)).is_none());
    }

    #[test]
    fn degenerate_geometry_is_not_actionable() {
        let stage = (6000.0, 4000.0);
        let zoom = percent(200.0);
        assert!(MainView::new(&zoom, (0.0, 4000.0), VIEWPORT, 1.0, (0.0, 0.0)).is_none());
        assert!(MainView::new(&zoom, (f64::NAN, 4000.0), VIEWPORT, 1.0, (0.0, 0.0)).is_none());
        assert!(MainView::new(&zoom, stage, Size::new(0.0, 700.0), 1.0, (0.0, 0.0)).is_none());
        assert!(MainView::new(&zoom, stage, VIEWPORT, 0.0, (0.0, 0.0)).is_none());
        assert!(MainView::new(&zoom, stage, VIEWPORT, f32::NAN, (0.0, 0.0)).is_none());
        assert!(MainView::new(&zoom, stage, VIEWPORT, 1.0, (f32::NAN, 0.0)).is_none());
        assert!(Placement::new(VIEWPORT, (0.0, 4000.0), NO_STRIP).is_none());
        assert!(Placement::new(Size::new(f32::INFINITY, 700.0), stage, NO_STRIP).is_none());
        assert!(Placement::new(Size::new(0.0, 0.0), stage, NO_STRIP).is_none());
        // A centre that is not a number keeps the pan rather than drifting.
        let main = MainView::new(&zoom, stage, VIEWPORT, 1.0, (5.0, 6.0)).unwrap();
        assert_eq!(main.pan_centring((f64::NAN, f64::INFINITY)), (5.0, 6.0));
    }
}
