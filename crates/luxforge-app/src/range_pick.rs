//! The range picker: Mask mode's eyedropper over one luminance or colour range component.
//!
//! It is a sub-state of Mask mode and this desktop's view state only. The workspace mode, the open
//! mask, the selected component and the adjustment sections' target stay as they are while it is
//! armed, so the overlay keeps showing the selection and an adjustment made between picks edits the
//! open mask's own layer. Nothing here reads a pixel: a press on the photograph is placed through
//! the content map `render.transform` answered for the entry on screen, and a click or a drag
//! becomes one `mask.pick-<kind>` request, whose pixels the host reads at the mask's input site.
//!
//! **Coordinates.** The canvas reports positions in output-stage pixels. A pick command takes
//! normalized coordinates of the stage the masked layer receives, and a mask is only ever bound
//! before the geometry tail, so that stage is the content stage the map's inverse lands in
//! (`docs/design/masking.md`, Mask space): one normalization, through the held map, covers a crop,
//! a straighten, a quarter turn and a lens warp alike. A drag is the screen rectangle the person
//! drew, so its corners are mapped one by one and sent as their content-space bounding box; under a
//! rotation that box holds a little more than the rectangle on screen did.
use crate::{
    app::draft::GestureId,
    brush::{ContentMap, MapRefusal},
};
use luxforge_core::{AssetId, ComponentId, EntryId, MaskId};
use serde_json::{Map, Value, json};

/// How far, in logical points, a press must travel before its release is a drag over an area rather
/// than a click: a hand that wobbles while clicking still clicks.
pub(crate) const CLICK_TRAVEL: f32 = 3.0;

/// What the pick bar says a click and a drag do, for a kind that sets a band.
pub(crate) const TONE_HINT: &str = "Click a tone \u{b7} drag over an area";
/// The same for a kind that holds colours, which Shift adds to.
pub(crate) const COLOUR_HINT: &str =
    "Click a colour \u{b7} Shift-click to add \u{b7} drag over an area";

/// The armed eyedropper.
#[derive(Clone, Debug)]
pub(crate) struct RangePicker {
    /// The photograph it was armed on: another photograph ends it.
    pub(crate) asset: AssetId,
    pub(crate) mask: MaskId,
    pub(crate) component: ComponentId,
    pub(crate) kind: String,
    /// The kind's generated `mask.pick-<kind>`.
    pub(crate) action: &'static str,
    /// The command declares `add`: the kind holds samples a Shift-pick adds to.
    pub(crate) adds: bool,
    /// The content map a press is placed through, as `render.transform` answered it. The previous
    /// one is kept to draw a drag while a fresh one is asked for; a pick waits for the fresh one.
    pub(crate) map: Option<ContentMap>,
    /// The displayed entry the map was asked for: a new one asks again.
    pub(crate) entry: Option<EntryId>,
    /// The `render.transform` request in flight, by the identity its answer names.
    pub(crate) request: Option<GestureId>,
    /// A press on the photograph that has not come up yet.
    pub(crate) drag: Option<PickDrag>,
    /// A pick made while another was in flight or the map was on its way: the newest only.
    pub(crate) waiting: Option<PickGesture>,
}

impl RangePicker {
    /// The picker armed on one component of `kind`, or none when the host declares no pick for it.
    pub(crate) fn arm(
        asset: AssetId,
        mask: MaskId,
        component: ComponentId,
        kind: &str,
    ) -> Option<Self> {
        let command = luxforge_core::mask::commands::pick(kind)?;
        Some(Self {
            asset,
            mask,
            component,
            kind: kind.to_owned(),
            action: command.method,
            adds: command.action.parameter("add").is_some(),
            map: None,
            entry: None,
            request: None,
            drag: None,
            waiting: None,
        })
    }

    /// What the pick bar says.
    pub(crate) fn hint(&self) -> &'static str {
        if self.adds { COLOUR_HINT } else { TONE_HINT }
    }

    /// The drag in progress as the rectangle the canvas draws, in output-stage pixels, once it has
    /// travelled far enough to be one.
    pub(crate) fn rectangle(&self) -> Option<[f64; 4]> {
        self.drag.filter(|drag| drag.dragging).map(|drag| {
            let (x0, x1) = ordered(drag.from.0, drag.to.0);
            let (y0, y1) = ordered(drag.from.1, drag.to.1);
            [x0, y0, x1, y1]
        })
    }
}

/// One press on the photograph, from the button going down to it coming up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PickDrag {
    /// Where it went down, in canvas logical points, which the click threshold is measured in.
    pub(crate) press: (f32, f32),
    /// Where it went down and where it is now, in output-stage pixels.
    pub(crate) from: (f64, f64),
    pub(crate) to: (f64, f64),
    /// It has travelled [`CLICK_TRAVEL`] or more, so it is a drag for the rest of its life.
    pub(crate) dragging: bool,
}

impl PickDrag {
    pub(crate) fn pressed(view: (f32, f32), at: (f64, f64)) -> Self {
        Self {
            press: view,
            from: at,
            to: at,
            dragging: false,
        }
    }

    pub(crate) fn moved(&mut self, view: (f32, f32), at: (f64, f64)) {
        self.to = at;
        self.dragging |= travelled(self.press, view);
    }

    /// The pick this press makes when it comes up at `at`: a click where it went down, or the
    /// rectangle it was dragged over. `add` is the Shift held at the release, for a kind that adds.
    pub(crate) fn released(mut self, view: (f32, f32), at: (f64, f64), add: bool) -> PickGesture {
        self.moved(view, at);
        PickGesture {
            shape: if self.dragging {
                PickShape::Area(self.from, self.to)
            } else {
                PickShape::Click(self.from)
            },
            add,
        }
    }
}

/// Whether a press moved from `from` to `to` has travelled far enough to be a drag.
pub(crate) fn travelled(from: (f32, f32), to: (f32, f32)) -> bool {
    (to.0 - from.0).hypot(to.1 - from.1) >= CLICK_TRAVEL
}

/// What was picked, in output-stage pixels: one point, or two opposite corners of a rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PickShape {
    Click((f64, f64)),
    Area((f64, f64), (f64, f64)),
}

/// One pick, before it is placed in the content stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PickGesture {
    pub(crate) shape: PickShape,
    pub(crate) add: bool,
}

/// One pick in the command's own terms: a point, or the top-left corner and size of a rectangle, in
/// the content stage's normalized coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PickArea {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) size: Option<(f64, f64)>,
    pub(crate) add: bool,
}

impl PickArea {
    /// A click at the centre of the frame: what Copy as JSON request shows before any pick is made.
    pub(crate) const CENTRE: Self = Self {
        x: 0.5,
        y: 0.5,
        size: None,
        add: false,
    };

    /// The fields the pick command takes for this pick. `add` is sent only by a command that
    /// declares it, and always by one that does, so the request says whether it replaces.
    pub(crate) fn fields(&self, adds: bool) -> Map<String, Value> {
        let mut fields = Map::from_iter([
            ("x".to_owned(), json!(self.x)),
            ("y".to_owned(), json!(self.y)),
        ]);
        if let Some((width, height)) = self.size {
            fields.insert("width".to_owned(), json!(width));
            fields.insert("height".to_owned(), json!(height));
        }
        if adds {
            fields.insert("add".to_owned(), json!(self.add));
        }
        fields
    }
}

/// Place one pick in the content stage through `map`.
///
/// A click off the photograph is refused rather than moved to its edge. A drag is clipped to the
/// photograph first, so a rectangle that starts off the frame picks what it covers of it, and one
/// that covers none of it is refused; its four corners are then mapped and sent as their bounding
/// box, clamped to the frame.
pub(crate) fn place(map: &ContentMap, gesture: &PickGesture) -> Result<PickArea, MapRefusal> {
    match gesture.shape {
        PickShape::Click((x, y)) => {
            if !map.visible(x, y) {
                return Err(MapRefusal::Outside);
            }
            let (x, y) = map.locate(x, y)?;
            Ok(PickArea {
                x: x.clamp(0.0, 1.0),
                y: y.clamp(0.0, 1.0),
                size: None,
                add: gesture.add,
            })
        }
        PickShape::Area(from, to) => {
            let (width, height) = map.output();
            let (x0, x1) = ordered(from.0, to.0);
            let (y0, y1) = ordered(from.1, to.1);
            if x1 < 0.0 || y1 < 0.0 || x0 > width || y0 > height {
                return Err(MapRefusal::Outside);
            }
            let (x0, x1) = (x0.clamp(0.0, width), x1.clamp(0.0, width));
            let (y0, y1) = (y0.clamp(0.0, height), y1.clamp(0.0, height));
            let (mut low, mut high) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
            for (x, y) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
                let (x, y) = map.locate(x, y)?;
                let (x, y) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
                low = [low[0].min(x), low[1].min(y)];
                high = [high[0].max(x), high[1].max(y)];
            }
            Ok(PickArea {
                x: low[0],
                y: low[1],
                size: Some((high[0] - low[0], high[1] - low[1])),
                add: gesture.add,
            })
        }
    }
}

fn ordered(a: f64, b: f64) -> (f64, f64) {
    if a <= b { (a, b) } else { (b, a) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(size: (u32, u32)) -> ContentMap {
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        ContentMap::from_affine(size, size, identity, identity).expect("a drawable stage")
    }

    /// A quarter turn clockwise of a 480 × 320 content stage, cropped to its left 300 × 480 after
    /// the turn: output `(x', y') = (H - y, x)` less the crop's origin of 20 on x.
    fn turned_and_cropped() -> ContentMap {
        let (w, h) = (480.0_f64, 320.0_f64);
        let crop = 20.0;
        ContentMap::from_affine(
            (480, 320),
            (300, 480),
            // Content to output.
            [0.0, -1.0, h - crop, 1.0, 0.0, 0.0],
            // Output to content: x = y', y = H - crop - x'.
            [0.0, 1.0, 0.0, -1.0, 0.0, h - crop],
        )
        .filter(|map| map.content() == (w as u32, h as u32))
        .expect("a drawable stage")
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// A press that comes up within 3 pt of where it went down is a click there; from 3 pt on it is
    /// a drag, and stays one even if it comes back.
    #[test]
    fn a_press_is_a_click_until_it_travels_three_points() {
        let mut press = PickDrag::pressed((10.0, 10.0), (100.0, 50.0));
        press.moved((12.0, 11.0), (101.0, 50.5));
        assert!(!press.dragging);
        let click = press.released((12.5, 10.0), (101.2, 50.0), false);
        assert_eq!(click.shape, PickShape::Click((100.0, 50.0)));

        let mut press = PickDrag::pressed((10.0, 10.0), (100.0, 50.0));
        press.moved((13.0, 10.0), (101.5, 50.0));
        assert!(press.dragging, "3 pt is a drag");
        let back = press.released((10.0, 10.0), (100.0, 50.0), true);
        assert_eq!(
            back,
            PickGesture {
                shape: PickShape::Area((100.0, 50.0), (100.0, 50.0)),
                add: true
            },
            "once a drag, always a drag"
        );
    }

    /// With no geometry the output stage is the content stage: a click is its own position over the
    /// stage's size.
    #[test]
    fn a_click_is_normalized_by_the_content_stage() {
        let map = identity((480, 320));
        let click = PickGesture {
            shape: PickShape::Click((120.0, 80.0)),
            add: false,
        };
        assert_eq!(
            place(&map, &click),
            Ok(PickArea {
                x: 0.25,
                y: 0.25,
                size: None,
                add: false
            })
        );
        let off = PickGesture {
            shape: PickShape::Click((-1.0, 80.0)),
            add: false,
        };
        assert_eq!(place(&map, &off), Err(MapRefusal::Outside));
    }

    /// **Under a rotated crop** a click and a drag land on the content positions the output pixels
    /// show, normalized by the content stage the pick command reads — not the output stage, which
    /// is turned and smaller — and a drag is its corners' content-space bounding box.
    #[test]
    fn picks_under_a_rotated_crop_are_placed_in_the_content_stage() {
        let map = turned_and_cropped();
        // Output (60, 120) shows content x = 120, y = 320 - 20 - 60 = 240.
        let click = place(
            &map,
            &PickGesture {
                shape: PickShape::Click((60.0, 120.0)),
                add: true,
            },
        )
        .expect("inside the photograph");
        assert!(close(click.x, 120.0 / 480.0), "{click:?}");
        assert!(close(click.y, 240.0 / 320.0), "{click:?}");
        assert!(click.add && click.size.is_none());

        // Dragged from output (100, 300) up and left to (40, 200): content x spans 200..300 and y
        // spans 320 - 20 - 100 = 200 to 320 - 20 - 40 = 260.
        let area = place(
            &map,
            &PickGesture {
                shape: PickShape::Area((100.0, 300.0), (40.0, 200.0)),
                add: false,
            },
        )
        .expect("inside the photograph");
        assert!(close(area.x, 200.0 / 480.0), "{area:?}");
        assert!(close(area.y, 200.0 / 320.0), "{area:?}");
        let (width, height) = area.size.expect("a drag has a size");
        assert!(close(width, 100.0 / 480.0), "{area:?}");
        assert!(close(height, 60.0 / 320.0), "{area:?}");

        // A drag that starts off the photograph picks the part of it that it covers.
        let clipped = place(
            &map,
            &PickGesture {
                shape: PickShape::Area((-50.0, -50.0), (30.0, 40.0)),
                add: false,
            },
        )
        .expect("it covers a corner");
        assert!(
            close(clipped.x, 0.0) && close(clipped.y, 270.0 / 320.0),
            "{clipped:?}"
        );
        let (width, height) = clipped.size.unwrap();
        assert!(
            close(width, 40.0 / 480.0) && close(height, 30.0 / 320.0),
            "{clipped:?}"
        );
        // One wholly off it picks nothing.
        assert_eq!(
            place(
                &map,
                &PickGesture {
                    shape: PickShape::Area((310.0, 10.0), (400.0, 90.0)),
                    add: false,
                },
            ),
            Err(MapRefusal::Outside)
        );
    }

    /// The command's fields: a click names a point, a drag its corner and size, and `add` travels
    /// only on a command that declares it.
    #[test]
    fn a_pick_sends_the_fields_its_command_declares() {
        let click = PickArea {
            x: 0.25,
            y: 0.5,
            size: None,
            add: false,
        };
        assert_eq!(
            Value::Object(click.fields(false)),
            json!({"x": 0.25, "y": 0.5})
        );
        assert_eq!(
            Value::Object(click.fields(true)),
            json!({"x": 0.25, "y": 0.5, "add": false})
        );
        let drag = PickArea {
            size: Some((0.1, 0.2)),
            add: true,
            ..click
        };
        assert_eq!(
            Value::Object(drag.fields(true)),
            json!({"x": 0.25, "y": 0.5, "width": 0.1, "height": 0.2, "add": true})
        );
    }
}
