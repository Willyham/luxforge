//! The high-zoom minimap's geometry: when it is shown, where its overview of the stage sits in
//! its card, which part of that stage the main viewport shows, and the pan a click or drag in the
//! overview asks for ([minimap design](../../../../docs/design/minimap.md#geometry-and-command-parity)).
//!
//! Everything here is plain arithmetic over numbers the editor already holds — the stored zoom,
//! the stage the canvas scrolls over, the canvas region, the display scale and the scrollable's
//! offset — so the minimap's rectangle and its navigation are one projection, in both directions,
//! and the widget owns only pointer capture. Placement reuses the canvas's own: [`CanvasView`]'s
//! percentage scale and Fit, the scrollable's overhang ([`overhang`]) and the photo surface's
//! physical-pixel snap ([`snapped`]). Nothing here holds editing state or calls the owner: the
//! pan it answers reaches the session through `view.set`, as every other pan does.
use super::{
    Surfaces,
    canvas::snapped,
    canvas_view::{CanvasView, overhang},
};
use crate::state::canvas::{CanvasModel, PhotoView, ZoomView};
use iced::{Point, Rectangle, Size};

/// The least percentage zoom the minimap is shown at. 200% itself shows it.
pub(crate) const THRESHOLD: f32 = 200.0;

/// The percentage a view at `zoom` shows the minimap at, or `None` when it shows none: the stored
/// value, never the rounded text the title bar shows, decides, and Fit never shows it whatever
/// percentage it comes to.
pub(crate) fn eligible(zoom: ZoomView) -> Option<f32> {
    match zoom {
        ZoomView::Percent(value) if value >= THRESHOLD => Some(value),
        _ => None,
    }
}

/// The stage a percentage view scrolls over, as the canvas draws it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NavStage {
    /// Its size in stage pixels.
    pub(crate) size: (f64, f64),
    /// Whether its drawn edges are snapped to the physical pixel grid: the photograph's are
    /// ([`snapped`]); a crop draft's turned input stage is placed unsnapped, where the crop canvas
    /// draws it.
    pub(crate) snapped: bool,
}

impl NavStage {
    /// The photograph's output stage: the selected entry after its geometry, or the stage
    /// Compare's sides share.
    pub(crate) fn photo((width, height): (u32, u32)) -> Self {
        Self {
            size: (f64::from(width), f64::from(height)),
            snapped: true,
        }
    }

    /// A crop draft's box: its input stage's bounding box at the draft angle, empty corners and
    /// all, rather than the committed cropped output.
    pub(crate) fn crop_box(size: (f64, f64)) -> Self {
        Self {
            size,
            snapped: false,
        }
    }

    /// The stage the canvas draws now, chosen exactly as the photo area chooses it: a crop draft
    /// with its own stage pixels draws its box, the photograph otherwise, and nothing while no
    /// photograph is drawn.
    pub(crate) fn drawn(model: &CanvasModel, surfaces: &Surfaces<'_>) -> Option<Self> {
        match (&model.photo, surfaces.draft, surfaces.stage) {
            (PhotoView::Empty(_), _, _) => None,
            (PhotoView::Draft, Some(draft), Some(_)) => Some(Self::crop_box(draft.box_size())),
            (PhotoView::Plain | PhotoView::Draft, _, _) => {
                surfaces.photo?;
                model.dimensions.map(Self::photo)
            }
        }
    }
}

/// The main viewport at a percentage zoom: a stage seen through the canvas's scrollable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Viewport {
    stage: NavStage,
    /// Stage pixels to logical pixels: `percent / 100 / scale_factor`.
    scale: f32,
    /// Physical pixels per logical pixel: the system's factor times the interface size.
    scale_factor: f32,
    /// The canvas region the scrollable fills, in the window's logical pixels.
    canvas: Rectangle,
    /// The scrollable's offset, which is the session's pan: logical pixels from the zoomed stage's
    /// top-left corner to the viewport's, as given, before the scrollable clamps it.
    pan: (f32, f32),
}

impl Viewport {
    /// The view of `stage` at `percent` with display scale `scale_factor` in `canvas` at `pan`.
    /// `None` for a degenerate or non-finite input, which has no actionable geometry.
    pub(crate) fn new(
        stage: NavStage,
        percent: f32,
        scale_factor: f32,
        canvas: Rectangle,
        pan: (f32, f32),
    ) -> Option<Self> {
        let (width, height) = stage.size;
        let finite_rect = [canvas.x, canvas.y, canvas.width, canvas.height]
            .iter()
            .all(|value| value.is_finite());
        if !(width.is_finite()
            && height.is_finite()
            && width > 0.0
            && height > 0.0
            && scale_factor.is_finite()
            && scale_factor > 0.0
            && finite_rect
            && canvas.width > 0.0
            && canvas.height > 0.0
            && pan.0.is_finite()
            && pan.1.is_finite())
        {
            return None;
        }
        let scale = CanvasView::percent(percent, scale_factor)?.scale;
        let content = (width as f32 * scale, height as f32 * scale);
        (content.0.is_finite() && content.1.is_finite() && content.0 > 0.0 && content.1 > 0.0)
            .then_some(Self {
                stage,
                scale,
                scale_factor,
                canvas,
                pan,
            })
    }

    pub(crate) fn stage(&self) -> NavStage {
        self.stage
    }

    /// Stage pixels to logical pixels.
    pub(crate) fn scale(&self) -> f32 {
        self.scale
    }

    /// The pan this view was given.
    pub(crate) fn pan(&self) -> (f32, f32) {
        self.pan
    }

    /// The zoomed stage's size in logical pixels: the scrollable's content.
    fn content(&self) -> (f32, f32) {
        (
            self.stage.size.0 as f32 * self.scale,
            self.stage.size.1 as f32 * self.scale,
        )
    }

    /// How far the scrollable can move on each axis; zero on an axis the stage fits.
    pub(crate) fn pan_range(&self) -> (f32, f32) {
        let (width, height) = self.content();
        (
            overhang(width, self.canvas.width),
            overhang(height, self.canvas.height),
        )
    }

    /// The pan the scrollable actually holds: the given pan clamped to [`Self::pan_range`], so
    /// zero on an axis the stage fits.
    pub(crate) fn effective_pan(&self) -> (f32, f32) {
        let (x, y) = self.pan_range();
        (self.pan.0.clamp(0.0, x), self.pan.1.clamp(0.0, y))
    }

    /// The part of the stage the viewport shows, as `[x0, y0, x1, y1]` stage pixels within the
    /// stage: the geometric viewport, floating chrome included, where the main surface draws it.
    /// On an axis the stage fits, all of it. Otherwise the scrollable moves its content by its
    /// offset rounded to whole logical pixels and clips to its bounds snapped to whole physical
    /// pixels, and the photograph's edges are snapped as the photo surface snaps them; the visible
    /// part is that clip mapped back through the drawn edges. `None` when nothing is shown.
    pub(crate) fn visible(&self) -> Option<[f64; 4]> {
        let (width, height) = self.content();
        let (range_x, range_y) = self.pan_range();
        // The scrollable translates its content by its offset rounded to whole logical pixels
        // (`iced_widget::scrollable`'s translation), from the canvas's own corner on an axis
        // that overhangs.
        let left = self.canvas.x - self.pan.0.clamp(0.0, range_x).round();
        let top = self.canvas.y - self.pan.1.clamp(0.0, range_y).round();
        let drawn = Rectangle::new(Point::new(left, top), Size::new(width, height));
        let image = if self.stage.snapped {
            snapped(drawn, self.scale_factor).map(|edge| edge as f64)
        } else {
            [left, top, left + width, top + height].map(|edge| f64::from(edge * self.scale_factor))
        };
        // The clip rectangle, snapped as the toolkit snaps a clip to its scissor.
        let clip = [
            self.canvas.x,
            self.canvas.y,
            self.canvas.x + self.canvas.width,
            self.canvas.y + self.canvas.height,
        ]
        .map(|edge| f64::from((edge * self.scale_factor).round()));
        let axis = |range: f32, stage: f64, near: f64, far: f64, from: f64, to: f64| {
            if range <= 0.0 {
                return Some((0.0, stage));
            }
            let (shown_near, shown_far) = (near.max(from), far.min(to));
            (far > near && shown_far > shown_near).then(|| {
                let at =
                    |physical: f64| ((physical - near) / (far - near) * stage).clamp(0.0, stage);
                (at(shown_near), at(shown_far))
            })
        };
        let (x0, x1) = axis(
            range_x,
            self.stage.size.0,
            image[0],
            image[2],
            clip[0],
            clip[2],
        )?;
        let (y0, y1) = axis(
            range_y,
            self.stage.size.1,
            image[1],
            image[3],
            clip[1],
            clip[3],
        )?;
        Some([x0, y0, x1, y1])
    }

    /// [`Self::visible`] as fractions of the stage.
    pub(crate) fn normalized(&self) -> Option<[f64; 4]> {
        let [x0, y0, x1, y1] = self.visible()?;
        let (width, height) = self.stage.size;
        Some([x0 / width, y0 / height, x1 / width, y1 / height])
    }

    /// The stage point at the viewport's centre for its effective pan, before any snapping: the
    /// point [`Self::centred_pan`] would keep there. The stage's centre on an axis it fits.
    pub(crate) fn centre(&self) -> (f64, f64) {
        let (range_x, range_y) = self.pan_range();
        let (pan_x, pan_y) = self.effective_pan();
        let axis = |range: f32, pan: f32, extent: f32, stage: f64| {
            if range <= 0.0 {
                stage / 2.0
            } else {
                (f64::from(pan) + f64::from(extent) / 2.0) / f64::from(self.scale)
            }
        };
        (
            axis(range_x, pan_x, self.canvas.width, self.stage.size.0),
            axis(range_y, pan_y, self.canvas.height, self.stage.size.1),
        )
    }

    /// The pan that centres the viewport on stage point `point` as nearly as the stage's edges
    /// allow, clamped to [`Self::pan_range`]: an axis the stage fits stays centred, at zero. `None`
    /// for a non-finite point.
    pub(crate) fn centred_pan(&self, point: (f64, f64)) -> Option<(f32, f32)> {
        if !(point.0.is_finite() && point.1.is_finite()) {
            return None;
        }
        let (range_x, range_y) = self.pan_range();
        let axis = |at: f64, extent: f32, range: f32| {
            (at * f64::from(self.scale) - f64::from(extent) / 2.0).clamp(0.0, f64::from(range))
                as f32
        };
        Some((
            axis(point.0, self.canvas.width, range_x),
            axis(point.1, self.canvas.height, range_y),
        ))
    }
}

/// The overview's thumbnail of a stage inside the minimap's card: aspect-fitted and centred, its
/// letterbox inert. Card-local logical pixels throughout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Overview {
    stage: (f64, f64),
    view: CanvasView,
}

impl Overview {
    /// Fit `stage` into a card of `card`, as Fit places a photograph. `None` for a degenerate
    /// stage or card.
    pub(crate) fn place(stage: (f64, f64), card: Size) -> Option<Self> {
        if !(stage.0.is_finite()
            && stage.1.is_finite()
            && card.width.is_finite()
            && card.height.is_finite())
        {
            return None;
        }
        Some(Self {
            stage,
            view: CanvasView::fit(stage, card)?,
        })
    }

    /// Where the thumbnail is drawn in the card.
    pub(crate) fn image(&self) -> Rectangle {
        self.view.canvas_rect(0.0, 0.0, self.stage.0, self.stage.1)
    }

    /// The viewport rectangle over the thumbnail, in the card, or `None` when `viewport` is of
    /// another stage — an overview of an old stage never answers for a new one — or shows nothing.
    pub(crate) fn rectangle(&self, viewport: &Viewport) -> Option<Rectangle> {
        if viewport.stage.size != self.stage {
            return None;
        }
        let [x0, y0, x1, y1] = viewport.visible()?;
        Some(self.view.canvas_rect(x0, y0, x1 - x0, y1 - y0))
    }

    /// The stage point under card point `point`, or `None` in the letterbox around the thumbnail.
    pub(crate) fn stage_point(&self, point: Point) -> Option<(f64, f64)> {
        let (x, y) = self.unbounded(point)?;
        ((0.0..=self.stage.0).contains(&x) && (0.0..=self.stage.1).contains(&y)).then_some((x, y))
    }

    /// The stage point under `point`, wherever it is: a captured drag keeps moving outside the
    /// thumbnail and its pan clamps instead.
    fn unbounded(&self, point: Point) -> Option<(f64, f64)> {
        let (x, y) = self.view.stage_point(point);
        (x.is_finite() && y.is_finite()).then_some((x, y))
    }

    /// A press at card point `point`. In the letterbox, of another stage or with no visible
    /// rectangle it is inert (`None`). Inside the viewport rectangle it grabs it where it was
    /// pressed and moves nothing; elsewhere on the thumbnail it centres the viewport there and can
    /// continue as a drag.
    pub(crate) fn press(&self, viewport: &Viewport, point: Point) -> Option<Press> {
        let rectangle = self.rectangle(viewport)?;
        let at = self.stage_point(point)?;
        let inside = (rectangle.x..=rectangle.x + rectangle.width).contains(&point.x)
            && (rectangle.y..=rectangle.y + rectangle.height).contains(&point.y);
        if inside {
            let centre = viewport.centre();
            return Some(Press {
                grab: Grab {
                    offset: (at.0 - centre.0, at.1 - centre.1),
                },
                pan: None,
            });
        }
        let pan = viewport.centred_pan(at)?;
        Some(Press {
            grab: Grab { offset: (0.0, 0.0) },
            pan: (pan != viewport.pan).then_some(pan),
        })
    }
}

/// What a press in the overview did: the grab its drag keeps, and the pan it asks for, if any.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Press {
    pub(crate) grab: Grab,
    pub(crate) pan: Option<(f32, f32)>,
}

/// Where a drag holds the viewport rectangle: stage pixels from the rectangle's centre to the
/// pressed point, so the rectangle moves with the pointer rather than jumping to centre on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Grab {
    offset: (f64, f64),
}

impl Grab {
    /// The pan a drag held by this grab asks for with the pointer at card point `point` — which may
    /// lie outside the thumbnail or the card — clamped to the stage's edges; `None` when it is the
    /// viewport's pan already, the overview is of another stage, or the point is not finite.
    pub(crate) fn pan(
        &self,
        overview: &Overview,
        viewport: &Viewport,
        point: Point,
    ) -> Option<(f32, f32)> {
        if viewport.stage.size != overview.stage {
            return None;
        }
        let (x, y) = overview.unbounded(point)?;
        let pan = viewport.centred_pan((x - self.offset.0, y - self.offset.1))?;
        (pan != viewport.pan).then_some(pan)
    }
}

#[cfg(test)]
mod tests {
    //! Every expected coordinate below is worked by hand from the layout — the canvas region, the
    //! zoomed stage's size, the scrollable's offset, the physical grid and the card — rather than
    //! by running the module's own formulas again.
    use super::*;
    use crate::crop_draft::CropDraft;

    /// Within a hundredth of a pixel: the card's scale is a decimal fraction.
    fn near(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 0.01
    }

    fn assert_rect(actual: [f64; 4], expected: [f64; 4]) {
        assert!(
            actual.iter().zip(expected).all(|(a, e)| near(*a, e)),
            "{actual:?} is not {expected:?}"
        );
    }

    fn assert_pan(actual: Option<(f32, f32)>, expected: (f32, f32)) {
        let (x, y) = actual.expect("a pan");
        assert!(
            near(f64::from(x), f64::from(expected.0)) && near(f64::from(y), f64::from(expected.1)),
            "{:?} is not {expected:?}",
            (x, y)
        );
    }

    fn assert_frame(actual: Rectangle, expected: [f32; 4]) {
        let got = [actual.x, actual.y, actual.width, actual.height];
        assert!(
            got.iter()
                .zip(expected)
                .all(|(a, e)| near(f64::from(*a), f64::from(e))),
            "{got:?} is not {expected:?}"
        );
    }

    fn canvas(x: f32, y: f32, width: f32, height: f32) -> Rectangle {
        Rectangle::new(Point::new(x, y), Size::new(width, height))
    }

    const CARD: Size = Size {
        width: 180.0,
        height: 120.0,
    };

    /// A 6000 × 4000 photograph at 200% on a 1× display in a 1000 × 800 canvas at (100, 50): two
    /// logical pixels per stage pixel, so 12000 × 8000 of content overhanging by 11000 × 7200.
    fn landscape(pan: (f32, f32)) -> Viewport {
        Viewport::new(
            NavStage::photo((6000, 4000)),
            200.0,
            1.0,
            canvas(100.0, 50.0, 1000.0, 800.0),
            pan,
        )
        .expect("a view")
    }

    #[test]
    fn the_stored_percentage_decides_and_fit_never_shows_the_minimap() {
        assert_eq!(eligible(ZoomView::Fit), None);
        assert_eq!(eligible(ZoomView::Percent(199.9)), None);
        // The title bar rounds 199.9 to 200%, which never decides.
        assert_eq!(crate::state::title::percent_text(199.9), "200%");
        assert_eq!(eligible(ZoomView::Percent(199.99)), None);
        assert_eq!(eligible(ZoomView::Percent(200.0)), Some(200.0));
        assert_eq!(eligible(ZoomView::Percent(200.1)), Some(200.1));
        assert_eq!(eligible(ZoomView::Percent(1600.0)), Some(1600.0));
        assert_eq!(eligible(ZoomView::Percent(f32::NAN)), None);
    }

    #[test]
    fn the_rectangle_marks_the_stage_the_viewport_shows() {
        let view = landscape((3000.0, 2000.0));
        assert_eq!(view.pan_range(), (11000.0, 7200.0));
        // The content starts 3000 logical pixels left of the canvas at x = 100, so the canvas's
        // 1000 columns show stage columns 1500..2000; rows 1000..1400 likewise.
        assert_rect(view.visible().unwrap(), [1500.0, 1000.0, 2000.0, 1400.0]);
        assert_rect(view.normalized().unwrap(), [0.25, 0.25, 1.0 / 3.0, 0.35]);
        assert_eq!(view.centre(), (1750.0, 1200.0));
        // 6000 × 4000 is the card's own 3:2, so the thumbnail fills it at 0.03 card pixels per
        // stage pixel.
        let overview = Overview::place((6000.0, 4000.0), CARD).unwrap();
        assert_frame(overview.image(), [0.0, 0.0, 180.0, 120.0]);
        assert_frame(overview.rectangle(&view).unwrap(), [45.0, 30.0, 15.0, 12.0]);
        // At the far corner, and past it, the rectangle holds the stage's last 500 × 400 pixels;
        // a pan below zero holds the first rows.
        assert_rect(
            landscape((11000.0, 7200.0)).visible().unwrap(),
            [5500.0, 3600.0, 6000.0, 4000.0],
        );
        let stale = landscape((20000.0, -50.0));
        assert_eq!(stale.effective_pan(), (11000.0, 0.0));
        assert_rect(stale.visible().unwrap(), [5500.0, 0.0, 6000.0, 400.0]);
    }

    #[test]
    fn a_click_centres_the_viewport_there_clamped_to_the_edges() {
        let view = landscape((3000.0, 2000.0));
        let overview = Overview::place((6000.0, 4000.0), CARD).unwrap();
        // The card's centre is stage (3000, 2000): its 6000 × 4000 logical pixels less half the
        // 1000 × 800 canvas.
        let press = overview.press(&view, Point::new(90.0, 60.0)).unwrap();
        assert_pan(press.pan, (5500.0, 3600.0));
        // Each corner clamps to its edge.
        assert_pan(
            overview.press(&view, Point::new(0.0, 0.0)).unwrap().pan,
            (0.0, 0.0),
        );
        assert_pan(
            overview.press(&view, Point::new(180.0, 120.0)).unwrap().pan,
            (11000.0, 7200.0),
        );
        // The thumbnail's right edge, halfway down: stage (6000, 2000).
        assert_pan(
            overview.press(&view, Point::new(180.0, 60.0)).unwrap().pan,
            (11000.0, 3600.0),
        );
        // A press outside the rectangle can continue as a drag from where it centred.
        let outside = overview.press(&view, Point::new(150.0, 20.0)).unwrap();
        assert_pan(outside.pan, (9500.0, 933.333));
        let moved = landscape(outside.pan.unwrap());
        assert_pan(
            outside.grab.pan(&overview, &moved, Point::new(151.5, 21.5)),
            (9600.0, 1033.333),
        );
    }

    #[test]
    fn a_drag_keeps_its_grab_offset_and_clamps_outside_the_card() {
        let view = landscape((3000.0, 2000.0));
        let overview = Overview::place((6000.0, 4000.0), CARD).unwrap();
        // (48, 33) is inside the 45..60 × 30..42 rectangle: stage (1600, 1100), 150 left of and
        // 100 above its centre (1750, 1200). Grabbing moves nothing.
        let press = overview.press(&view, Point::new(48.0, 33.0)).unwrap();
        assert_eq!(press.pan, None);
        assert_eq!(
            press.grab.pan(&overview, &view, Point::new(48.0, 33.0)),
            None
        );
        // Moved to stage (2600, 2100), the centre follows to (2750, 2200).
        assert_pan(
            press.grab.pan(&overview, &view, Point::new(78.0, 63.0)),
            (5000.0, 4000.0),
        );
        // Captured outside the card, the pan holds at the edges.
        assert_pan(
            press.grab.pan(&overview, &view, Point::new(500.0, 500.0)),
            (11000.0, 7200.0),
        );
        assert_pan(
            press.grab.pan(&overview, &view, Point::new(-100.0, -100.0)),
            (0.0, 0.0),
        );
        assert_eq!(
            press.grab.pan(&overview, &view, Point::new(f32::NAN, 10.0)),
            None
        );
    }

    #[test]
    fn a_portrait_thumbnail_is_letterboxed_and_its_letterbox_is_inert() {
        // 3000 × 4500 in 180 × 120: the height binds at 120 / 4500, so the thumbnail is 80 wide,
        // at x = 50.
        let overview = Overview::place((3000.0, 4500.0), CARD).unwrap();
        assert_frame(overview.image(), [50.0, 0.0, 80.0, 120.0]);
        for x in [20.0, 49.5, 130.5, 170.0] {
            assert_eq!(overview.stage_point(Point::new(x, 60.0)), None, "{x}");
        }
        let edge = overview.stage_point(Point::new(50.0, 60.0)).unwrap();
        assert!(near(edge.0, 0.0) && near(edge.1, 2250.0), "{edge:?}");
        let corner = overview.stage_point(Point::new(130.0, 120.0)).unwrap();
        assert!(
            near(corner.0, 3000.0) && near(corner.1, 4500.0),
            "{corner:?}"
        );
        // 400% at 1×: 12000 × 18000 of content in a 1200 × 700 canvas, at the origin.
        let view = Viewport::new(
            NavStage::photo((3000, 4500)),
            400.0,
            1.0,
            canvas(0.0, 0.0, 1200.0, 700.0),
            (0.0, 0.0),
        )
        .unwrap();
        assert_rect(view.visible().unwrap(), [0.0, 0.0, 300.0, 175.0]);
        assert_frame(
            overview.rectangle(&view).unwrap(),
            [50.0, 0.0, 8.0, 175.0 * 120.0 / 4500.0],
        );
        assert_eq!(overview.press(&view, Point::new(20.0, 60.0)), None);
        // Stage (1500, 2250): 6000 × 9000 logical less half the canvas.
        assert_pan(
            overview.press(&view, Point::new(90.0, 60.0)).unwrap().pan,
            (5400.0, 8650.0),
        );
    }

    #[test]
    fn an_axis_that_fits_stays_centred_and_navigation_never_moves_it() {
        // 600 × 4000 at 200% on 1×: 1200 wide in a 1500-wide canvas fits; 8000 tall overhangs.
        let narrow = |pan| {
            Viewport::new(
                NavStage::photo((600, 4000)),
                200.0,
                1.0,
                canvas(0.0, 0.0, 1500.0, 800.0),
                pan,
            )
            .unwrap()
        };
        let stale = narrow((300.0, 1000.0));
        assert_eq!(stale.pan_range(), (0.0, 7200.0));
        assert_eq!(stale.effective_pan(), (0.0, 1000.0));
        assert_rect(stale.visible().unwrap(), [0.0, 500.0, 600.0, 900.0]);
        assert_eq!(stale.centred_pan((600.0, 4000.0)), Some((0.0, 7200.0)));
        // Thumbnail 18 × 120 at x = 81; the rectangle spans its width, rows 15..27.
        let overview = Overview::place((600.0, 4000.0), CARD).unwrap();
        let view = narrow((0.0, 1000.0));
        assert_frame(overview.rectangle(&view).unwrap(), [81.0, 15.0, 18.0, 12.0]);
        let press = overview.press(&view, Point::new(90.0, 21.0)).unwrap();
        assert_eq!(press.pan, None);
        // Sideways motion does nothing; downward motion moves only y.
        assert_eq!(
            press.grab.pan(&overview, &view, Point::new(98.0, 21.0)),
            None
        );
        assert_pan(
            press.grab.pan(&overview, &view, Point::new(84.0, 30.0)),
            (0.0, 1600.0),
        );
        // A photograph that fits both ways shows a full-image rectangle and nothing moves it.
        let small = Viewport::new(
            NavStage::photo((400, 300)),
            200.0,
            1.0,
            canvas(0.0, 0.0, 1000.0, 800.0),
            (0.0, 0.0),
        )
        .unwrap();
        assert_eq!(small.visible(), Some([0.0, 0.0, 400.0, 300.0]));
        let whole = Overview::place((400.0, 300.0), CARD).unwrap();
        assert_frame(whole.rectangle(&small).unwrap(), [10.0, 0.0, 160.0, 120.0]);
        for point in [
            Point::new(10.0, 0.0),
            Point::new(90.0, 60.0),
            Point::new(170.0, 120.0),
        ] {
            let press = whole.press(&small, point).unwrap();
            assert_eq!(press.pan, None);
            assert_eq!(press.grab.pan(&whole, &small, Point::new(0.0, 0.0)), None);
        }
    }

    #[test]
    fn the_effective_display_scale_divides_the_zoom() {
        // 200% on a 2× display is one logical pixel per stage pixel: a 1000 × 800 canvas shows
        // 1000 × 800 stage pixels, twice what it shows on a 1× display.
        let view = |percent, scale_factor| {
            Viewport::new(
                NavStage::photo((6000, 4000)),
                percent,
                scale_factor,
                canvas(0.0, 0.0, 1000.0, 800.0),
                (2500.0, 1500.0),
            )
            .unwrap()
        };
        assert_rect(
            view(200.0, 2.0).visible().unwrap(),
            [2500.0, 1500.0, 3500.0, 2300.0],
        );
        assert_rect(
            view(200.0, 1.0).visible().unwrap(),
            [1250.0, 750.0, 1750.0, 1150.0],
        );
        // 400% on 2× is 200% on 1× again.
        assert_eq!(view(400.0, 2.0).visible(), view(200.0, 1.0).visible());
        // A click on stage (5000, 2000 / 3) at 200% on 2× asks for that less half the canvas.
        let overview = Overview::place((6000.0, 4000.0), CARD).unwrap();
        assert_pan(
            overview
                .press(&view(200.0, 2.0), Point::new(150.0, 20.0))
                .unwrap()
                .pan,
            (4500.0, 266.667),
        );
    }

    /// At an interface size of 125% on a 2× display there are 2.5 physical pixels to a logical
    /// pixel, so neither the canvas nor the photograph lies on the physical grid. The scrollable
    /// moves its content by whole logical pixels, the photograph's edges snap half to even and the
    /// clip rounds, and the rectangle follows the pixels the surface draws.
    #[test]
    fn the_rectangle_follows_the_snapped_photograph_and_the_unsnapped_crop_box() {
        let view = |stage| {
            Viewport::new(
                stage,
                200.0,
                2.5,
                canvas(241.0, 44.0, 900.0, 700.0),
                (1000.3, 400.6),
            )
            .unwrap()
        };
        // 0.8 logical pixels per stage pixel: 4800 × 3200 of content. Moved by 1000 and 401, it
        // starts at x = −759 and y = −357 logical, which is −1897.5 and −892.5 physical; its far
        // edges are at 10102.5 and 7107.5. The clip is 602.5..2852.5 by 110..1860 physical,
        // rounded to 603..2853 by 110..1860.
        //
        // The photograph snaps half to even: −1898..10102 and −892..7108, two physical pixels per
        // stage pixel, so columns (603 + 1898) / 2 and (2853 + 1898) / 2, rows (110 + 892) / 2
        // and (1860 + 892) / 2.
        let photo = view(NavStage::photo((6000, 4000)));
        assert_rect(photo.visible().unwrap(), [1250.5, 501.0, 2375.5, 1376.0]);
        // A crop box of the same size is placed unsnapped, from −1897.5 and −892.5.
        let crop = view(NavStage::crop_box((6000.0, 4000.0)));
        assert_rect(crop.visible().unwrap(), [1250.25, 501.25, 2375.25, 1376.25]);
    }

    #[test]
    fn the_drawn_stage_is_the_crop_box_while_a_crop_draft_draws_its_own() {
        let pixels = std::sync::Arc::new(vec![0u8; 4]);
        let frame = luxforge_ui::Frame::new(pixels, 1, 1, 1).expect("a one-pixel frame");
        // A 4000 × 3000 stage turned by the angle whose cosine is 0.8 and sine 0.6 has a box of
        // 4000 × 0.8 + 3000 × 0.6 by 4000 × 0.6 + 3000 × 0.8.
        let mut draft = CropDraft::upright(4000, 3000);
        draft.stage.angle = 0.6_f64.atan2(0.8).to_degrees();
        let surfaces = Surfaces {
            comparison: None,
            comparison_waiting: false,
            photo: Some(&frame),
            photo_content: None,
            current_content: 0,
            region_coverage: None,
            stage: Some(&frame),
            stage_rest: None,
            clipping: None,
            coverage: None,
            mask_draft: None,
            mask_map: None,
            mask_handles: false,
            corrections: None,
            draft: Some(&draft),
            gpu: None,
            gpu_hold: false,
            gpu_tag: None,
            gpu_change: None,
            dissolve: None,
            gpu_warm: None,
            gpu_source: None,
            gpu_window: None,
            gpu_rest: None,
            gpu_rest_under: false,
            gpu_counts: None,
            compare_gpu: None,
            compare_source: None,
            compare_change: None,
            compare_rest: None,
            overview: None,
            minimap: None,
        };
        let model = CanvasModel {
            photo: PhotoView::Draft,
            zoom: ZoomView::Percent(400.0),
            dimensions: Some((3000, 2000)),
            ..CanvasModel::default()
        };
        let drawn = NavStage::drawn(&model, &surfaces).unwrap();
        assert!(!drawn.snapped);
        assert!(
            near(drawn.size.0, 5000.0) && near(drawn.size.1, 4800.0),
            "{drawn:?}"
        );
        // Until the draft's stage pixels arrive the photograph is drawn, and so navigated.
        let waiting = Surfaces {
            stage: None,
            ..surfaces
        };
        assert_eq!(
            NavStage::drawn(&model, &waiting),
            Some(NavStage::photo((3000, 2000)))
        );
        let plain = CanvasModel {
            photo: PhotoView::Plain,
            ..model.clone()
        };
        assert_eq!(
            NavStage::drawn(&plain, &surfaces),
            Some(NavStage {
                size: (3000.0, 2000.0),
                snapped: true
            })
        );
        // Nothing drawn, nothing navigated.
        let unrendered = Surfaces {
            photo: None,
            ..waiting
        };
        assert_eq!(NavStage::drawn(&plain, &unrendered), None);
        let empty = CanvasModel {
            photo: PhotoView::Empty("Open a photograph".into()),
            ..model
        };
        assert_eq!(NavStage::drawn(&empty, &surfaces), None);
    }

    #[test]
    fn degenerate_or_non_finite_geometry_is_not_actionable() {
        let stage = NavStage::photo((6000, 4000));
        let room = canvas(0.0, 0.0, 1000.0, 800.0);
        assert!(Viewport::new(NavStage::photo((0, 4000)), 200.0, 1.0, room, (0.0, 0.0)).is_none());
        assert!(
            Viewport::new(
                NavStage::crop_box((f64::NAN, 10.0)),
                200.0,
                1.0,
                room,
                (0.0, 0.0)
            )
            .is_none()
        );
        for percent in [0.0, -200.0, f32::NAN, f32::INFINITY] {
            assert!(
                Viewport::new(stage, percent, 1.0, room, (0.0, 0.0)).is_none(),
                "{percent}"
            );
        }
        for scale_factor in [0.0, -2.0, f32::NAN] {
            assert!(Viewport::new(stage, 200.0, scale_factor, room, (0.0, 0.0)).is_none());
        }
        for region in [
            canvas(0.0, 0.0, 0.0, 800.0),
            canvas(0.0, 0.0, 1000.0, -1.0),
            canvas(f32::NAN, 0.0, 1000.0, 800.0),
        ] {
            assert!(Viewport::new(stage, 200.0, 1.0, region, (0.0, 0.0)).is_none());
        }
        for pan in [(f32::NAN, 0.0), (0.0, f32::INFINITY)] {
            assert!(Viewport::new(stage, 200.0, 1.0, room, pan).is_none());
        }
        assert!(Overview::place((6000.0, 4000.0), Size::new(0.0, 120.0)).is_none());
        assert!(Overview::place((0.0, 4000.0), CARD).is_none());
        assert!(Overview::place((6000.0, 4000.0), Size::new(f32::NAN, 120.0)).is_none());
        let view = Viewport::new(stage, 200.0, 1.0, room, (0.0, 0.0)).unwrap();
        assert_eq!(view.centred_pan((f64::NAN, 0.0)), None);
        let overview = Overview::place((6000.0, 4000.0), CARD).unwrap();
        assert_eq!(overview.press(&view, Point::new(f32::NAN, 1.0)), None);
        // An overview of another stage never answers for this one.
        let old = Overview::place((4000.0, 6000.0), CARD).unwrap();
        assert_eq!(old.rectangle(&view), None);
        assert_eq!(old.press(&view, Point::new(90.0, 60.0)), None);
        let press = overview.press(&view, Point::new(90.0, 60.0)).unwrap();
        assert_eq!(press.grab.pan(&old, &view, Point::new(10.0, 10.0)), None);
    }
}
