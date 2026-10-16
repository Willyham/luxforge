//! The `zoom` smoke scenario: the photograph at percentage zooms, over the generated 24 MP and
//! 60 MP JPEGs, one launch each.
//!
//! Every zoom draws the photograph through the photo surface, which hands the renderer only the part
//! of the zoomed box that is on screen. This scenario is the rendered proof of that. At 50% (the
//! view's reduction stretched over the exact stage's box), 100%, 120%, 800% and 1600% — where the box
//! is far larger than any GPU viewport — and panned to the centre and the far corner at 1600%, each
//! captured frame is checked sample by sample against the source pixel the zoom and the pan put
//! under it, and where a quadrant boundary is in view its position is measured against the one the
//! geometry predicts. The texture on screen is checked to be the reduction below 100% and the exact
//! frame from 100% up, in the state and in the `preview_displayed` event that put it there; at
//! 100% and above the 60 MP render is wider than the device's texture limit and is drawn from
//! tiles. Three `wait` steps, at Fit, at 100% and at 1600%, prove that idling rebuilds the view
//! without writing the texture, asking for a frame or changing a pixel; the Fit one follows a
//! shorter wait that lets the launch's own refit to the display scale land first. A wgpu
//! validation error or a panic in either launch fails the run.
//!
//! The high-zoom minimap ([`docs/design/minimap.md`]) is checked on every frame: hidden below
//! 200% and at Fit, shown from 200% up at the place, size and stage the geometry worked here puts
//! it, its viewport rectangle where the frame's own zoom, pan and canvas put it, and its thumbnail
//! the fixture's quadrants. At 200% the journey then navigates from it with real mouse events
//! routed through the window's widgets: clicks that centre the view, the same pan sent as
//! `view.set` on the desktop's own session, a drag that keeps its grab and clamps at the stage's
//! edges and is followed past the card, an inert letterbox, a wheel that pans and a pinch that
//! does not zoom. Each lands on the pan worked here, and the photograph under it is checked
//! sample by sample as every other zoom is. Then with a crop draft (where Escape ends a held drag
//! and leaves the draft), the neutral picker, an armed brush and Compare on the canvas, minimap
//! gestures navigate and edit nothing: no commit, the draft unchanged, the mode kept.
use crate::{
    scenario::{Checked, Checks, Fixture, Frame, Plan, Run, Step, plan::only},
    smoke::Scenario,
    *,
};
use luxforge_core::{MASK_MODE, POINTER_MODE};
use luxforge_evidence::{
    self as script, CompareStep, DraftStep, MaskStep, MinimapPoint, MinimapStep, PaintStep,
    PinchStep, ViewStep, WorkspaceStep,
};

pub const SCENARIO: &str = "zoom";

/// How long each idle `wait` step lasts: four ticks of the evidence run's 250 ms timer, each of
/// which rebuilds the view with nothing new to show.
const WAIT_MS: u64 = 1000;
/// How long the settling wait after the open lasts: long enough for the refit the display scale
/// asks for once it is known, which is not idle behaviour and so is not what the idle check sees.
const SETTLE_MS: u64 = 500;

/// A pixel shows a fixture colour when every channel is within this many codes of it.
const TOLERANCE: u8 = 10;
/// How far, in source pixels, a sample must stay from anything the fixture draws besides its flat
/// quadrants — the quadrant boundaries, the white centre line and arrow, the black dashes, the
/// image's own edge — to be expected to show a flat quadrant colour. It clears JPEG ringing and
/// chroma subsampling, and the reduction's averaging below 100%.
const FEATURE_MARGIN: f64 = 12.0;
/// The sample grid's spacing, in physical pixels.
const SAMPLE_STEP: usize = 12;
/// Physical pixels left out at the canvas's right and bottom edges, where the scrollbars float over
/// the photograph, and at its bottom, where the mode strip does.
const SCROLLBAR_INSET: u32 = 30;
const STRIP_INSET: u32 = 150;

/// The Basic module, whose canvas mode is the neutral picker.
const BASIC_MODULE: &str = "luxforge.basic";
/// The design's minimap: an image area of at most 180 × 120 points and at most this share of the
/// canvas on each axis, the stage fitted into it, padded by 4 points and inset 12 from the
/// canvas's right and bottom edges.
pub(crate) const MINIMAP_AREA: (f64, f64) = (180.0, 120.0);
pub(crate) const MINIMAP_SHARE: f64 = 0.35;
pub(crate) const MINIMAP_PADDING: f64 = 4.0;
pub(crate) const MINIMAP_INSET: f64 = 12.0;
/// How far, in points, the card's chrome shadow (`luxforge_ui::theme::CHROME_SHADOW`: a 16-point
/// blur offset 4 points down) darkens the photograph beyond its bounds, with a little to spare.
const CARD_SHADOW: f64 = 22.0;
/// The minimap appears from this percentage up.
pub(crate) const MINIMAP_FROM: f64 = 200.0;
/// How far, in logical points, the minimap's recorded geometry may lie from the geometry worked
/// here, and a pan from the one a gesture asks for: the scrollable draws whole logical pixels.
pub(crate) const GEOMETRY_SLACK: f64 = 0.75;
pub(crate) const PAN_SLACK: f64 = 1.0;
/// A wheel's pan passes through the scrollable's own clamping, which rounds its extent.
const WHEEL_SLACK: f64 = 1.5;
/// The thumbnail is the stage reduced to under 200 points: a sample shows its quadrant's colour
/// within this many codes per channel.
const THUMBNAIL_TOLERANCE: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Zoom {
    Fit,
    Percent(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Open,
    Settle,
    View,
    Wait,
    Pan(f64, f64),
    Pinch(f64),
    /// A gesture on the minimap: mouse events routed through the window's widgets onto its card,
    /// or the same-client `view.set` a click is equivalent to.
    Minimap(MinimapStep),
    /// A tool put on the canvas, or taken off it, around minimap gestures.
    Tool(Tool),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tool {
    CropStart,
    CropCancel,
    /// Escape through the key table, which a held minimap drag takes first.
    Escape,
    /// Escape through the key table with no drag held: the armed brush is put down.
    Disarm,
    Mode(&'static str),
    ArmBrush,
    CompareTap,
}

/// What is on the canvas at a frame besides the photograph. Only a plain canvas is checked sample
/// by sample against the source; with a tool, the frame is held to the minimap's own checks and to
/// having edited nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Over {
    Nothing,
    Crop,
    Picker,
    Brush,
    Compare,
}

const fn minimap(step: MinimapStep) -> Kind {
    Kind::Minimap(step)
}

const fn image(x: f32, y: f32) -> MinimapPoint {
    MinimapPoint::Image([x, y])
}

const AT_200: Zoom = Zoom::Percent(200.0);

/// Every frame in capture order: the open frame, then one per script step, each named, with the
/// zoom it must be drawn at. The plan, and so the script and the frame count, is made from this
/// table, and the checks below walk the same table beside the frames the plan held.
const PLAN: [(&str, Kind, Zoom, Over); 47] = [
    ("opened", Kind::Open, Zoom::Fit, Over::Nothing),
    ("settle", Kind::Settle, Zoom::Fit, Over::Nothing),
    ("idle-fit", Kind::Wait, Zoom::Fit, Over::Nothing),
    ("50", Kind::View, Zoom::Percent(50.0), Over::Nothing),
    ("100", Kind::View, Zoom::Percent(100.0), Over::Nothing),
    (
        "pinch-in",
        Kind::Pinch(1.2),
        Zoom::Percent(120.0),
        Over::Nothing,
    ),
    (
        "pinch-out",
        Kind::Pinch(1.0 / 1.2),
        Zoom::Percent(100.0),
        Over::Nothing,
    ),
    ("idle-100", Kind::Wait, Zoom::Percent(100.0), Over::Nothing),
    ("120", Kind::View, Zoom::Percent(120.0), Over::Nothing),
    ("800", Kind::View, Zoom::Percent(800.0), Over::Nothing),
    ("1600", Kind::View, Zoom::Percent(1600.0), Over::Nothing),
    (
        "pan-centre",
        Kind::Pan(0.5, 0.5),
        Zoom::Percent(1600.0),
        Over::Nothing,
    ),
    (
        "idle-1600",
        Kind::Wait,
        Zoom::Percent(1600.0),
        Over::Nothing,
    ),
    (
        "pan-corner",
        Kind::Pan(1.0, 1.0),
        Zoom::Percent(1600.0),
        Over::Nothing,
    ),
    // The minimap's threshold, then navigation from it at 200%.
    ("199.9", Kind::View, Zoom::Percent(199.9), Over::Nothing),
    ("200", Kind::View, AT_200, Over::Nothing),
    (
        "mm-click",
        minimap(MinimapStep::Click(image(0.25, 0.3))),
        AT_200,
        Over::Nothing,
    ),
    (
        "mm-click-2",
        minimap(MinimapStep::Click(image(0.7, 0.6))),
        AT_200,
        Over::Nothing,
    ),
    (
        "mm-api",
        minimap(MinimapStep::Api(image(0.25, 0.3))),
        AT_200,
        Over::Nothing,
    ),
    (
        "mm-grab",
        minimap(MinimapStep::Press(MinimapPoint::Rectangle([0.75, 0.25]))),
        AT_200,
        Over::Nothing,
    ),
    (
        "mm-drag",
        minimap(MinimapStep::Move(image(0.5, 0.5))),
        AT_200,
        Over::Nothing,
    ),
    (
        "mm-drag-out",
        minimap(MinimapStep::Move(image(1.6, 1.4))),
        AT_200,
        Over::Nothing,
    ),
    (
        "mm-release",
        minimap(MinimapStep::Release(image(1.6, 1.4))),
        AT_200,
        Over::Nothing,
    ),
    (
        "mm-letterbox",
        minimap(MinimapStep::Click(MinimapPoint::Card([0.01, 0.5]))),
        AT_200,
        Over::Nothing,
    ),
    (
        "mm-wheel",
        minimap(MinimapStep::Wheel([0.0, 120.0])),
        AT_200,
        Over::Nothing,
    ),
    (
        "mm-pinch",
        minimap(MinimapStep::Pinch(0.25)),
        AT_200,
        Over::Nothing,
    ),
    // A crop draft: a held drag ended by Escape, which leaves the draft open.
    ("crop", Kind::Tool(Tool::CropStart), AT_200, Over::Crop),
    (
        "crop-press",
        minimap(MinimapStep::Press(image(0.3, 0.3))),
        AT_200,
        Over::Crop,
    ),
    ("crop-escape", Kind::Tool(Tool::Escape), AT_200, Over::Crop),
    (
        "crop-move",
        minimap(MinimapStep::Move(image(0.6, 0.6))),
        AT_200,
        Over::Crop,
    ),
    (
        "crop-release",
        minimap(MinimapStep::Release(image(0.6, 0.6))),
        AT_200,
        Over::Crop,
    ),
    (
        "crop-click",
        minimap(MinimapStep::Click(image(0.7, 0.4))),
        AT_200,
        Over::Crop,
    ),
    (
        "crop-cancel",
        Kind::Tool(Tool::CropCancel),
        AT_200,
        Over::Nothing,
    ),
    // The neutral picker, an armed brush and Compare.
    (
        "picker",
        Kind::Tool(Tool::Mode(BASIC_MODULE)),
        AT_200,
        Over::Picker,
    ),
    (
        "picker-click",
        minimap(MinimapStep::Click(image(0.3, 0.7))),
        AT_200,
        Over::Picker,
    ),
    (
        "picker-left",
        Kind::Tool(Tool::Mode(POINTER_MODE)),
        AT_200,
        Over::Nothing,
    ),
    (
        "mask",
        Kind::Tool(Tool::Mode(MASK_MODE)),
        AT_200,
        Over::Brush,
    ),
    ("brush", Kind::Tool(Tool::ArmBrush), AT_200, Over::Brush),
    (
        "brush-click",
        minimap(MinimapStep::Click(image(0.6, 0.3))),
        AT_200,
        Over::Brush,
    ),
    ("brush-down", Kind::Tool(Tool::Disarm), AT_200, Over::Brush),
    (
        "brush-left",
        Kind::Tool(Tool::Mode(POINTER_MODE)),
        AT_200,
        Over::Nothing,
    ),
    (
        "compare",
        Kind::Tool(Tool::CompareTap),
        AT_200,
        Over::Compare,
    ),
    // No pan is taken under Compare: a pan there, from the minimap or the scrollable alike, leaves
    // the capture waiting for its photograph (seen at 150% with no card shown), so the card's
    // gestures here are the ones that move nothing — the letterbox, and a grab let go in place.
    (
        "compare-letterbox",
        minimap(MinimapStep::Click(MinimapPoint::Card([0.01, 0.5]))),
        AT_200,
        Over::Compare,
    ),
    (
        "compare-grab",
        minimap(MinimapStep::Press(MinimapPoint::Rectangle([0.5, 0.5]))),
        AT_200,
        Over::Compare,
    ),
    (
        "compare-let-go",
        minimap(MinimapStep::Release(MinimapPoint::Rectangle([0.5, 0.5]))),
        AT_200,
        Over::Compare,
    ),
    (
        "compare-exit",
        Kind::Tool(Tool::CompareTap),
        AT_200,
        Over::Nothing,
    ),
    ("fit", Kind::View, Zoom::Fit, Over::Nothing),
];

/// The step each planned frame after the open one is captured for, as the script writes it and as
/// the editor records it back.
fn request(kind: Kind, zoom: Zoom) -> Option<script::Step> {
    match kind {
        Kind::Open => None,
        Kind::Settle => Some(script::Step::wait(SETTLE_MS)),
        Kind::Wait => Some(script::Step::wait(WAIT_MS)),
        Kind::Pan(x, y) => Some(script::Step::pan(x as f32, y as f32)),
        Kind::Pinch(factor) => Some(
            PinchStep {
                delta: factor.ln(),
                x: 0.37,
                y: 0.42,
            }
            .into(),
        ),
        Kind::View => Some(match zoom {
            Zoom::Fit => script::Step::View(ViewStep::Fit),
            Zoom::Percent(value) => script::Step::View(ViewStep::Percent(value as f32)),
        }),
        Kind::Minimap(step) => Some(script::Step::Minimap(step)),
        Kind::Tool(tool) => Some(match tool {
            Tool::CropStart => script::Step::Draft(DraftStep::Start),
            Tool::CropCancel => script::Step::Draft(DraftStep::Cancel),
            Tool::Escape | Tool::Disarm => script::Step::Key {
                key: script::KEY_ESCAPE.into(),
            },
            Tool::Mode(mode) => script::Step::Workspace(WorkspaceStep::default().mode(mode)),
            Tool::ArmBrush => script::Step::Mask(MaskStep::Paint(PaintStep::NewMask)),
            Tool::CompareTap => script::Step::Compare(CompareStep::Tap),
        }),
    }
}

/// The canvas mode a frame must be in, where the journey sets or relies on one.
fn mode(kind: Kind, over: Over) -> Option<&'static str> {
    match (kind, over) {
        (Kind::Tool(Tool::Mode(mode)), _) => Some(mode),
        (Kind::Minimap(_), Over::Nothing | Over::Compare) => Some(POINTER_MODE),
        (Kind::Minimap(_), Over::Picker) => Some(BASIC_MODULE),
        (Kind::Minimap(_) | Kind::Tool(Tool::ArmBrush), Over::Brush) => Some(MASK_MODE),
        _ => None,
    }
}

/// Every frame of one launch: the photograph opened unedited, then each zoom, pan, wait, minimap
/// gesture and tool, none of which commits anything, each at the zoom its row names.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(
        PLAN.iter()
            .map(|(name, kind, zoom, over)| {
                let mut step = match request(*kind, *zoom) {
                    None => Step::opened(*name).label("Original"),
                    Some(request) => Step::new(*name, request).commits(0),
                };
                if let Some(mode) = mode(*kind, *over) {
                    step = step.mode(mode);
                }
                if *kind == Kind::Tool(Tool::CropCancel) {
                    step = step.no_draft();
                }
                match zoom {
                    Zoom::Fit => step.fit(),
                    Zoom::Percent(value) => step.percent(*value),
                }
            })
            .collect(),
    )
}

/// One launch per photograph the row opens, each an ordinary smoke run with the scenario's launch
/// and checks in its own directory, named for the photograph; the scenario passes when both do.
/// Both always run, so one failing launch never hides the other's evidence.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    run.note(
        "Two launches, one per fixture, each an ordinary smoke run with its own `result.json` in its own directory: `24mp/` over the generated 24 MP JPEG and `60mp/` over the generated 60 MP one. Generate them first with `cargo xtask generate-fixtures --output fixtures/generated`.",
    );
    let mut failures = Vec::new();
    for source in sources {
        let name = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or("A zoom fixture has no name")?
            .to_owned();
        let fixture = source
            .strip_prefix(run.root())
            .unwrap_or(&source)
            .to_string_lossy()
            .into_owned();
        let outcome = run
            .child(&name)
            .and_then(|child| smoke::launch_all(child, scenario, vec![source.clone()]));
        run.record_launches(&run.out().join(&name), json!({"fixture": fixture}))?;
        if let Err(error) = outcome {
            failures.push(format!("{name}: {error}"));
        }
    }
    let outcome: Result = if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    };
    run.finish(outcome, |_| Ok(()))
}

/// The fixture's colour at source pixel `(x, y)` when that pixel is a flat quadrant interior, or
/// `None` when it is near a drawn feature or outside the image. Mirrors the generator in
/// `fixtures.rs`: four quadrants split at half the width and height, a three-pixel white line at
/// `w / 2 - 1 ..= w / 2 + 1` from row 30 to `h - 30` with an arrow head in rows 30 to 55, and black
/// dashes in columns 20 to 150 within 20 rows of the middle.
fn flat_colour((x, y): (f64, f64), (width, height): (u32, u32)) -> Option<[u8; 3]> {
    let (w, h) = (f64::from(width), f64::from(height));
    let m = FEATURE_MARGIN;
    let centre = (w / 2.0).floor();
    let middle = (h / 2.0).floor();
    let near = [
        x < m || y < m || x > w - m || y > h - m,
        (x - (centre + 0.5)).abs() < 1.5 + m,
        (y - middle).abs() < m,
        y < 55.0 + m && (x - centre).abs() < 16.0 + m,
        x < 150.0 + m && (y - middle).abs() < 20.0 + m,
    ];
    if near.iter().any(|near| *near) {
        return None;
    }
    let index = usize::from(y >= middle) * 2 + usize::from(x >= centre);
    Some(crate::fixtures::COLORS[index])
}

fn matches(pixel: [u8; 3], colour: [u8; 3]) -> bool {
    pixel
        .iter()
        .zip(colour)
        .all(|(a, b)| a.abs_diff(b) <= TOLERANCE)
}

/// Where a percentage frame maps captured pixels to source pixels: where the editor records drawing
/// the photograph's top-left corner, in physical pixels — the canvas's corner less the pan, at every
/// zoom this scenario takes, since the zoomed box is larger than the canvas on both axes — and the
/// zoom as physical pixels per source pixel.
struct Mapping {
    origin: (f64, f64),
    zoom: f64,
}

impl Mapping {
    fn source(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            (x - self.origin.0) / self.zoom,
            (y - self.origin.1) / self.zoom,
        )
    }

    fn screen(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (self.origin.0 + x * self.zoom, self.origin.1 + y * self.zoom)
    }
}

fn canvas_rect(frame: &Value) -> Result<[u32; 4]> {
    let rect: [u32; 4] = serde_json::from_value(frame["canvas_rect"].clone())
        .map_err(|_| "Frame records no canvas rectangle")?;
    ensure(
        rect[0] + 2 * SCROLLBAR_INSET < rect[2] && rect[1] + STRIP_INSET + 20 < rect[3],
        format!("Canvas rectangle {rect:?} is too small to sample"),
    )?;
    Ok(rect)
}

/// Check a percentage frame against the source pixels the zoom and pan put on screen: every sample
/// on a grid over the canvas that lands in a flat quadrant interior must show that quadrant's
/// colour, and any quadrant boundary in view must sit where the geometry puts it. The minimap's
/// card, `[left, top, right, bottom]` in physical pixels where it is shown, floats over the
/// photograph and is left out.
fn check_mapping(
    frame: &Frame,
    zoom: f64,
    stage: (u32, u32),
    card: Option<[f64; 4]>,
) -> Result<Value> {
    let over_card = |x: f64, y: f64| {
        card.is_some_and(|[left, top, right, bottom]| {
            x >= left && x < right && y >= top && y < bottom
        })
    };
    let image = frame.image()?;
    let [left, top, right, bottom] = canvas_rect(frame)?;
    let photo = frame.photo_rect()?;
    let mapping = Mapping {
        origin: (photo[0] as f64, photo[1] as f64),
        zoom: zoom / 100.0,
    };
    let (w, h) = (f64::from(stage.0), f64::from(stage.1));
    ensure(
        w * mapping.zoom > f64::from(right - left) && h * mapping.zoom > f64::from(bottom - top),
        "The zoomed box does not cover the canvas; this check assumes it does",
    )?;
    // The recorded rectangle is the zoomed box, to the pixel its edges snap to.
    ensure(
        ((photo[2] - photo[0]) as f64 - w * mapping.zoom).abs() <= 1.0
            && ((photo[3] - photo[1]) as f64 - h * mapping.zoom).abs() <= 1.0,
        format!("The photograph is drawn in {photo:?}, not a {zoom}% box of {stage:?}"),
    )?;
    let pixel = |x: u32, y: u32| image.get_pixel(x, y).0;
    let (mut checked, mut matched) = (0u32, 0u32);
    let mut colours = [0u32; 4];
    let mut mismatches = Vec::new();
    for y in (top + 8..bottom - STRIP_INSET).step_by(SAMPLE_STEP) {
        for x in (left + 8..right - SCROLLBAR_INSET).step_by(SAMPLE_STEP) {
            if over_card(f64::from(x), f64::from(y)) {
                continue;
            }
            let source = mapping.source((f64::from(x) + 0.5, f64::from(y) + 0.5));
            let Some(expected) = flat_colour(source, stage) else {
                continue;
            };
            checked += 1;
            let actual = pixel(x, y);
            if matches(actual, expected) {
                matched += 1;
                if let Some(index) = crate::fixtures::COLORS.iter().position(|c| *c == expected) {
                    colours[index] += 1;
                }
            } else if mismatches.len() < 8 {
                mismatches.push(json!({"screen":[x,y],"source":[source.0,source.1],"expected":expected,"actual":actual}));
            }
        }
    }
    ensure(
        checked >= 200,
        format!("Only {checked} samples landed in flat quadrant interiors"),
    )?;
    ensure(
        matched == checked,
        format!(
            "{} of {checked} samples do not show the source pixel the zoom and pan put there (blank, stale or misplaced): {}",
            checked - matched,
            Value::Array(mismatches.clone())
        ),
    )?;

    // The vertical boundary is the white line: its centre is source column w / 2 + 0.5.
    let slack = 3.0 + mapping.zoom;
    let centre = (w / 2.0).floor();
    let middle = (h / 2.0).floor();
    let inside = |x: f64, y: f64| {
        x >= f64::from(left + 8)
            && x < f64::from(right - SCROLLBAR_INSET)
            && y >= f64::from(top + 8)
            && y < f64::from(bottom - STRIP_INSET)
            && !over_card(x, y)
    };
    let mut boundaries = serde_json::Map::new();
    let line_x = mapping.screen((centre + 0.5, 0.0)).0;
    // A row that shows the line away from its arrow, its ends, the dashes and the middle boundary.
    let row = (top + 8..bottom - STRIP_INSET).find(|y| {
        let source_y = mapping.source((0.0, f64::from(*y) + 0.5)).1;
        source_y > 55.0 + FEATURE_MARGIN
            && source_y < h - 30.0 - FEATURE_MARGIN
            && (source_y - middle).abs() > 20.0 + FEATURE_MARGIN
    });
    if let Some(y) = row
        && inside(line_x - slack - 3.0 * mapping.zoom, f64::from(y))
        && inside(line_x + slack + 3.0 * mapping.zoom, f64::from(y))
    {
        let side = if mapping.source((0.0, f64::from(y) + 0.5)).1 >= middle {
            2
        } else {
            0
        };
        let (left_colour, right_colour) = (
            crate::fixtures::COLORS[side],
            crate::fixtures::COLORS[side + 1],
        );
        let reach = (3.0 * mapping.zoom + slack + 4.0).ceil() as i64;
        let run: Vec<u32> = (line_x as i64 - reach..=line_x as i64 + reach)
            .map(|x| x as u32)
            .filter(|x| {
                let p = pixel(*x, y);
                !matches(p, left_colour) && !matches(p, right_colour)
            })
            .collect();
        let (first, last) = (
            *run.first()
                .ok_or("The white centre line is not where the zoom and pan put it")?,
            *run.last().expect("a non-empty run"),
        );
        let measured = (f64::from(first) + f64::from(last) + 1.0) / 2.0;
        ensure(
            (measured - line_x).abs() <= slack,
            format!(
                "The white centre line is at x {measured}, expected {line_x:.2} within {slack:.1}"
            ),
        )?;
        boundaries.insert(
            "vertical".into(),
            json!({"row":y,"measured_x":measured,"expected_x":line_x,"tolerance":slack}),
        );
    }
    // The horizontal boundary sits at source row h / 2 on both halves; measure it in a column that
    // is a flat quadrant on both sides of it.
    let middle_y = mapping.screen((0.0, middle)).1;
    let column = (left + 8..right - SCROLLBAR_INSET).find(|x| {
        let source_x = mapping.source((f64::from(*x) + 0.5, 0.0)).0;
        source_x > 150.0 + 20.0 + FEATURE_MARGIN
            && source_x < w - FEATURE_MARGIN
            && (source_x - (centre + 0.5)).abs() > 1.5 + FEATURE_MARGIN
    });
    if let Some(x) = column
        && inside(f64::from(x), middle_y - slack - 2.0 * mapping.zoom)
        && inside(f64::from(x), middle_y + slack + 2.0 * mapping.zoom)
    {
        let side = usize::from(mapping.source((f64::from(x) + 0.5, 0.0)).0 >= centre);
        let (upper, lower) = (
            crate::fixtures::COLORS[side],
            crate::fixtures::COLORS[side + 2],
        );
        let reach = (2.0 * mapping.zoom + slack + 4.0).ceil() as i64;
        let rows: Vec<u32> = (middle_y as i64 - reach..=middle_y as i64 + reach)
            .map(|y| y as u32)
            .collect();
        let last_upper = rows
            .iter()
            .copied()
            .filter(|y| matches(pixel(x, *y), upper))
            .max()
            .ok_or("No upper quadrant above the middle boundary")?;
        let first_lower = rows
            .iter()
            .copied()
            .filter(|y| matches(pixel(x, *y), lower))
            .min()
            .ok_or("No lower quadrant below the middle boundary")?;
        let measured = (f64::from(last_upper) + f64::from(first_lower) + 1.0) / 2.0;
        ensure(
            (measured - middle_y).abs() <= slack,
            format!(
                "The middle boundary is at y {measured}, expected {middle_y:.2} within {slack:.1}"
            ),
        )?;
        boundaries.insert(
            "horizontal".into(),
            json!({"column":x,"measured_y":measured,"expected_y":middle_y,"tolerance":slack}),
        );
    }
    Ok(json!({
        "samples": checked,
        "per_quadrant": colours,
        "photo_rect": photo,
        "boundaries": boundaries,
        "minimap_card_left_out": card,
    }))
}

/// The canvas region of two captures, compared byte for byte, less `card` where it is given.
fn same_canvas(
    a: &image::RgbImage,
    b: &image::RgbImage,
    rect: [u32; 4],
    card: Option<[f64; 4]>,
) -> Result<bool> {
    ensure(
        a.dimensions() == b.dimensions(),
        "Two captures of one run have different sizes",
    )?;
    let [left, top, right, bottom] = rect;
    let over_card = |x: u32, y: u32| {
        card.is_some_and(|[l, t, r, b]| {
            f64::from(x) >= l && f64::from(x) < r && f64::from(y) >= t && f64::from(y) < b
        })
    };
    Ok((top..bottom)
        .all(|y| (left..right).all(|x| over_card(x, y) || a.get_pixel(x, y) == b.get_pixel(x, y))))
}

fn number(value: &Value, what: &str) -> Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| format!("Frame records no {what}").into())
}

/// Every frame against the zoom its step asks for: the texture on screen, the source pixels the zoom
/// and pan put under each sample, the minimap and what its gestures did, and, for the idle steps, a
/// view rebuilt with no new frame.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let (evidence, events) = (&launch.evidence, &launch.events);
    // A validation error aborts the editor; say so plainly if its log carries one.
    let log = fs::read_to_string(
        evidence
            .parent()
            .ok_or("No run directory")?
            .join("subprocess.log"),
    )
    .unwrap_or_default();
    for marker in ["Validation Error", "wgpu error", "panicked"] {
        ensure(
            !log.contains(marker),
            format!("The editor's log reports {marker:?}"),
        )?;
    }
    // The original is untouched by everything the journey did, the minimap's gestures among it.
    run.sources_unchanged()?;
    let frames = &launch.frames;
    let captures: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event["event"] == "frame_captured")
        .map(|(index, _)| index)
        .collect();
    ensure(
        captures.len() == frames.len(),
        "Every frame needs its own frame_captured event",
    )?;
    let stage: (u32, u32) = {
        let dims: [u32; 2] =
            serde_json::from_value(launch.at("opened")?["state"]["source_dimensions"].clone())
                .map_err(|_| "The opened frame records no source dimensions")?;
        (dims[0], dims[1])
    };
    let mut checks = Checks::new();
    if std::env::consts::OS == "macos" {
        ensure(
            events
                .iter()
                .any(|event| event["event"] == "trackpad_input_ready"),
            "The native trackpad monitor was not installed",
        )?;
    }
    let mut grab: Option<Grab> = None;
    for (index, ((name, kind, zoom, over), frame)) in PLAN.iter().zip(frames).enumerate() {
        let what = format!("step {name:?} (frame {index}, {kind:?} at {zoom:?} over {over:?})");
        let state = &frame["state"];
        let image = frame.image()?;
        ensure(
            state["phase"] == "ready" && state["source_dimensions"] == json!([stage.0, stage.1]),
            format!("{what}: not a ready frame of the opened photograph"),
        )?;
        let surface = &state["surface"];
        let plain = *over == Over::Nothing;
        if matches!(kind, Kind::Pinch(_)) {
            let before = &frames[index - 1];
            let previous = before.photo_rect()?;
            let current = frame.photo_rect()?;
            let [left, top, right, bottom]: [u32; 4] =
                serde_json::from_value(frame["canvas_rect"].clone())?;
            let cursor = [
                f64::from(left) + f64::from(right - left) * 0.37,
                f64::from(top) + f64::from(bottom - top) * 0.42,
            ];
            let old_percent = before["state"]["surface"]["view"]["zoom"]["value"]
                .as_f64()
                .ok_or("No pre-pinch zoom")?;
            let new_percent = surface["view"]["zoom"]["value"]
                .as_f64()
                .ok_or("No pinched zoom")?;
            for axis in 0..2 {
                let a = (cursor[axis] - previous[axis] as f64) * 100.0 / old_percent;
                let b = (cursor[axis] - current[axis] as f64) * 100.0 / new_percent;
                checks.compare(
                    frame,
                    "the pinch keeps the source point under the pointer",
                    a,
                    b,
                    crate::scenario::Tolerance::Within(2.0),
                )?;
            }
            ensure(
                events.iter().any(|event| {
                    event["event"] == "view_pinched"
                        && event["detail"]["params"]["zoom"]["value"] == json!(new_percent)
                }),
                "The pinched frame has no correlated input request",
            )?;
        }

        // With a tool on the canvas the surface draws the tool's own picture — a crop draft's box,
        // a mask's overlay, Compare's halves — so the texture and sample checks are a plain
        // canvas's.
        let texture = if plain {
            check_texture(frame, *kind, *zoom, stage, events)
                .map_err(|error| format!("{what}: {error}"))?
        } else {
            Value::Null
        };
        let minimap = check_minimap(frame, index, frames, *kind, *zoom, *over, stage, &mut grab)
            .map_err(|error| format!("{what}: minimap: {error}"))?;
        let card = card_pixels(frame)?;

        let pixels = match zoom {
            Zoom::Fit => frame
                .fixture(Fixture {
                    aspect: Some(f64::from(stage.0) / f64::from(stage.1)),
                    ..Fixture::fit(1)
                })
                .map_err(|error| format!("{what}: {error}"))?,
            Zoom::Percent(value) if plain => check_mapping(frame, *value, stage, card)
                .map_err(|error| format!("{what}: {error}"))?,
            Zoom::Percent(_) => Value::Null,
        };

        let version = number(&surface["version"], "surface version")?;
        let writes = number(&surface["texture_writes"], "texture write count")?;
        let views = number(&surface["views"], "view count")?;
        let mut idle = Value::Null;
        if index > 0 {
            let previous = &frames[index - 1]["state"]["surface"];
            let (previous_version, previous_writes, previous_views) = (
                number(&previous["version"], "surface version")?,
                number(&previous["texture_writes"], "texture write count")?,
                number(&previous["views"], "view count")?,
            );
            // The texture is written once per new raster the surface draws and never on a redraw:
            // an unchanged version means an unchanged count, whatever else happened between.
            // Compare's After is drawn by a surface of its own whose writes are counted beside the
            // photograph's, and leaving it hands the photograph back: a frame entering or in
            // Compare, leaving it, or the first after is not held to it.
            let comparing = *over == Over::Compare
                || PLAN[index - 1].3 == Over::Compare
                || PLAN[index - 1].1 == Kind::Tool(Tool::CompareTap);
            if comparing {
                // Its counts are recorded below as they stand.
            } else if version == previous_version {
                ensure(
                    writes == previous_writes,
                    format!(
                        "{what}: the texture was written {} times with no new raster",
                        writes - previous_writes
                    ),
                )?;
            } else {
                ensure(
                    writes > previous_writes
                        && writes - previous_writes <= version - previous_version,
                    format!(
                        "{what}: {} new rasters but {} texture writes",
                        version - previous_version,
                        writes.saturating_sub(previous_writes)
                    ),
                )?;
            }
            if *kind == Kind::Wait {
                let between = &events[captures[index - 1] + 1..captures[index]];
                let rendered: Vec<&Value> = between
                    .iter()
                    .filter(|event| {
                        matches!(
                            event["event"].as_str(),
                            Some("preview_displayed" | "preview_proxy_requested" | "render_ready")
                        )
                    })
                    .collect();
                ensure(
                    rendered.is_empty(),
                    format!("{what}: idling displayed or asked for a frame: {rendered:?}"),
                )?;
                ensure(
                    version == previous_version && writes == previous_writes,
                    format!("{what}: idling changed the surface's raster or wrote its texture"),
                )?;
                ensure(
                    views >= previous_views + 2,
                    format!(
                        "{what}: the view was rebuilt {} times while idling; the check needs at least two",
                        views - previous_views
                    ),
                )?;
                // The minimap's overview is work of its own that may land while the picture idles;
                // its card is held to the idle check too once it showed the same overview before.
                let settled_card = {
                    let (a, b) = (&frames[index - 1]["state"]["minimap"], &state["minimap"]);
                    a["state"] == b["state"] && a["overview"] == b["overview"]
                };
                let rect = canvas_rect(frame)?;
                ensure(
                    same_canvas(
                        frames[index - 1].image()?,
                        image,
                        rect,
                        if settled_card { None } else { card },
                    )?,
                    format!("{what}: the canvas changed while idling"),
                )?;
                idle = json!({"views_rebuilt":views - previous_views,"canvas_identical":true,
                    "minimap_card_compared":settled_card,"events_between":between.len()});
            }
        }
        checks.note(
            frame,
            "the texture on screen, the source pixels under the samples, the minimap and, idle, a view rebuilt with no new frame",
            json!({
            "step": name,
            "kind": format!("{kind:?}"),
            "over": format!("{over:?}"),
            "texture": texture,
            "version": version,
            "texture_writes": writes,
            "views": views,
            "pan": [surface["view"]["pan_x"], surface["view"]["pan_y"]],
            "pixels": pixels,
            "minimap": minimap,
            "idle": idle,
            }),
        );
    }
    checks.write(
        evidence,
        "zoom",
        json!({
            "stage": [stage.0, stage.1],
            "tolerance_per_channel": TOLERANCE,
            "thumbnail_tolerance_per_channel": THUMBNAIL_TOLERANCE,
            "feature_margin_source_pixels": FEATURE_MARGIN,
            "sample_step_physical_pixels": SAMPLE_STEP,
            "minimap_geometry_slack_points": GEOMETRY_SLACK,
            "minimap_pan_slack_points": PAN_SLACK,
            "scope": "Displayed geometry, texture identity, idle stability and minimap placement and navigation read back from the window renderer and the editor's state; minimap gestures are mouse events routed through the laid-out widgets, not the OS pointer; not display scanout or colour calibration",
        }),
    )
}

/// The texture on screen at a plain canvas: the reference's exact frame reduced to the view
/// wherever the stage is drawn smaller than itself, the exact frame itself from 100% up.
fn check_texture(
    frame: &Frame,
    kind: Kind,
    zoom: Zoom,
    stage: (u32, u32),
    events: &[Value],
) -> Result<Value> {
    let state = &frame["state"];
    let surface = &state["surface"];
    let reduced = match zoom {
        Zoom::Fit => true,
        Zoom::Percent(value) => value < 100.0,
    };
    let raster: [u32; 2] = serde_json::from_value(surface["raster"].clone())
        .map_err(|_| "no raster on the surface")?;
    // Where the GPU presents the stack at rest (`docs/design/gpu-first.md`, stage 2), a zoom
    // renders nothing on the CPU: the GPU's picture of the view is the photograph, drawn over
    // the frame the surface already holds, and the CPU texture checks below do not apply.
    let gpu_picture = matches!(
        state["surface"]["gpu"]["picture"].as_str(),
        Some("rest" | "view")
    );
    if gpu_picture && kind != Kind::Open {
        ensure(
            state["surface"]["gpu"]["drawing_path"] == "gpu",
            format!(
                "the GPU's picture at rest is handed but the surface drew {}",
                state["surface"]["gpu"]["drawing_path"]
            ),
        )?;
    }
    // The open's picture the GPU presents at rest is exempt as well: the reference then draws
    // nothing to reduce, and the frame is checked sample by sample all the same.
    let gpu_opened =
        gpu_picture && kind == Kind::Open && state["surface"]["gpu"]["drawing_path"] == "gpu";
    ensure(
        (gpu_picture && kind != Kind::Open)
            || gpu_opened
            || state["reference"]["reduced"] == json!(reduced),
        format!("reduction presented is {}", state["reference"]["reduced"]),
    )?;
    // A reduction is the stage scaled into the bounds the view asks for now — never one left
    // over from a previous zoom — and the exact frame is the stage itself. The opened frame is
    // the exception: the open request is sent before the display scale is known, so its
    // reduction is made for the bounds at scale 1 and the refit that follows is what the
    // settling wait lets land.
    let cpu_picture = !(gpu_opened || (gpu_picture && kind != Kind::Open));
    let expected_raster = if !cpu_picture || (reduced && kind == Kind::Open) {
        raster
    } else if reduced {
        let bounds = &state["reference"]["bounds"];
        let (width, height) = (
            bounds["width"].as_f64().ok_or("No view bounds")?,
            bounds["height"].as_f64().ok_or("No view bounds")?,
        );
        let scale = (width / f64::from(stage.0))
            .min(height / f64::from(stage.1))
            .min(1.0);
        [
            (f64::from(stage.0) * scale).round() as u32,
            (f64::from(stage.1) * scale).round() as u32,
        ]
    } else {
        [stage.0, stage.1]
    };
    ensure(
        !cpu_picture
            || (raster == expected_raster
                && (!reduced || (raster[0] < stage.0 && raster[1] < stage.1))),
        format!(
            "the surface holds a {raster:?} raster of a {stage:?} stage, expected {expected_raster:?}"
        ),
    )?;
    // Every raster handed to the surface is one `preview_displayed` and one new version, so
    // the event that put this frame's raster on screen is the version-th of them; a picture the
    // GPU presents hands no raster.
    let version = number(&surface["version"], "surface version")?;
    let displayed = events
        .iter()
        .filter(|event| event["event"] == "preview_displayed" && event["detail"]["path"] != "gpu")
        .nth(
            usize::try_from(version)?
                .checked_sub(1)
                .ok_or("Version zero")?,
        )
        .ok_or_else(|| format!("no preview_displayed for version {version}"))?;
    let detail = &displayed["detail"];
    ensure(
        !cpu_picture
            || (detail["reduced"] == json!(reduced)
                && detail["path"] == "surface"
                && detail["dimensions"] == json!([stage.0, stage.1])),
        format!("the raster on screen was displayed as {detail}"),
    )?;
    Ok(json!({
        "reduced": reduced,
        "raster": raster,
        "displayed_generation": detail["generation"],
    }))
}

/// A minimap drag the journey holds: the stage point the press took and the pan it left.
#[derive(Clone, Copy, Debug)]
struct Grab {
    at: (f64, f64),
    pan: (f64, f64),
}

pub(crate) fn pair(value: &Value, what: &str) -> Result<(f64, f64)> {
    let [a, b]: [f64; 2] =
        serde_json::from_value(value.clone()).map_err(|_| format!("no {what}: {value}"))?;
    Ok((a, b))
}

pub(crate) fn rect(value: &Value, what: &str) -> Result<[f64; 4]> {
    serde_json::from_value(value.clone()).map_err(|_| format!("no {what}: {value}").into())
}

pub(crate) fn near(a: f64, b: f64, slack: f64) -> bool {
    (a - b).abs() <= slack
}

/// The minimap's card in physical pixels, `[left, top, right, bottom]` with a margin for the
/// chrome shadow it casts on the photograph, where it is shown.
pub(crate) fn card_pixels(frame: &Frame) -> Result<Option<[f64; 4]>> {
    let minimap = &frame["state"]["minimap"];
    if minimap["shown"] != json!(true) {
        return Ok(None);
    }
    let [x, y, w, h] = rect(&minimap["card"], "minimap card")?;
    let scale = minimap["scale_factor"].as_f64().ok_or("no scale factor")?;
    let margin = CARD_SHADOW * scale;
    Ok(Some([
        x * scale - margin,
        y * scale - margin,
        (x + w) * scale + margin,
        (y + h) * scale + margin,
    ]))
}

/// The pan the session holds at a frame: logical pixels from the zoomed stage's corner.
pub(crate) fn session_pan(frame: &Frame) -> Result<(f64, f64)> {
    let view = &frame["state"]["surface"]["view"];
    Ok((
        view["pan_x"].as_f64().ok_or("no session pan")?,
        view["pan_y"].as_f64().ok_or("no session pan")?,
    ))
}

/// The view a frame shows, worked from its own zoom, pan and canvas rather than from anything the
/// minimap records: the stage in logical pixels per stage pixel, the canvas, and the pan clamped
/// as the scrollable clamps it.
pub(crate) struct View {
    pub(crate) stage: (f64, f64),
    pub(crate) scale: f64,
    pub(crate) canvas: [f64; 4],
    pub(crate) pan: (f64, f64),
}

impl View {
    pub(crate) fn range(&self) -> (f64, f64) {
        (
            (self.stage.0 * self.scale - self.canvas[2]).max(0.0),
            (self.stage.1 * self.scale - self.canvas[3]).max(0.0),
        )
    }

    pub(crate) fn clamp(&self, (x, y): (f64, f64)) -> (f64, f64) {
        let (rx, ry) = self.range();
        (x.clamp(0.0, rx), y.clamp(0.0, ry))
    }

    /// The pan that centres the canvas on stage point `at`, as far as the stage's edges allow.
    pub(crate) fn centred(&self, at: (f64, f64)) -> (f64, f64) {
        self.clamp((
            at.0 * self.scale - self.canvas[2] / 2.0,
            at.1 * self.scale - self.canvas[3] / 2.0,
        ))
    }

    /// The stage region the canvas shows, `[x0, y0, x1, y1]` in stage pixels.
    pub(crate) fn visible(&self) -> [f64; 4] {
        let pan = self.clamp(self.pan);
        let axis = |extent: f64, view: f64, pan: f64| {
            let origin = ((view - extent * self.scale) / 2.0).max(0.0) - pan;
            (
                ((0.0 - origin) / self.scale).clamp(0.0, extent),
                ((view - origin) / self.scale).clamp(0.0, extent),
            )
        };
        let (x0, x1) = axis(self.stage.0, self.canvas[2], pan.0);
        let (y0, y1) = axis(self.stage.1, self.canvas[3], pan.1);
        [x0, y0, x1, y1]
    }
}

/// The minimap at one frame: shown exactly from 200% up; its card, image and stage where the design
/// puts them for this canvas and stage; its rectangle the region the frame's own pan shows; its
/// thumbnail the fixture; and, for a gesture, the pan worked here from the point the gesture took.
#[allow(clippy::too_many_arguments)]
fn check_minimap(
    frame: &Frame,
    index: usize,
    frames: &[Frame],
    kind: Kind,
    zoom: Zoom,
    over: Over,
    source: (u32, u32),
    grab: &mut Option<Grab>,
) -> Result<Value> {
    let state = &frame["state"];
    let minimap = &state["minimap"];
    let shown = matches!(zoom, Zoom::Percent(value) if value >= MINIMAP_FROM);
    ensure(
        minimap["shown"] == json!(shown),
        format!(
            "the card is {} at {zoom:?}",
            if shown { "not shown" } else { "shown" }
        ),
    )?;
    let counters = &minimap["counters"];
    let before = index
        .checked_sub(1)
        .map(|index| &frames[index]["state"]["minimap"]["counters"]);
    let counted = |name: &str| -> Result<u64> {
        let now = counters[name].as_u64().ok_or("no counters")?;
        let then = before.map_or(Some(0), |before| before[name].as_u64());
        Ok(now - then.ok_or("no counters before")?)
    };
    if !shown {
        ensure(
            minimap["dragging"] == json!(false) && !matches!(kind, Kind::Minimap(_)),
            "no card, yet a gesture or a drag",
        )?;
        return Ok(json!({"shown": false}));
    }
    let Zoom::Percent(percent) = zoom else {
        unreachable!("shown only at a percentage")
    };
    let scale_factor = minimap["scale_factor"].as_f64().ok_or("no scale factor")?;
    let [left, top, right, bottom]: [f64; 4] =
        serde_json::from_value(frame["canvas_rect"].clone()).map_err(|_| "no canvas rectangle")?;
    let canvas = [
        left / scale_factor,
        top / scale_factor,
        (right - left) / scale_factor,
        (bottom - top) / scale_factor,
    ];
    let recorded = rect(&minimap["canvas"], "minimap canvas")?;
    ensure(
        canvas.iter().zip(recorded).all(|(a, b)| near(*a, b, 1.0)),
        format!("the card's canvas is {recorded:?}, the frame's {canvas:?}"),
    )?;
    // The stage: the photograph's output, snapped to the device's pixels; a crop draft's input
    // box, unsnapped — at this draft's angle and size, the source's own size.
    let (size, snapped) = (
        pair(&minimap["stage"]["size"], "stage size")?,
        minimap["stage"]["snapped"].as_bool().ok_or("no stage")?,
    );
    let source = (f64::from(source.0), f64::from(source.1));
    ensure(
        near(size.0, source.0, 0.5)
            && near(size.1, source.1, 0.5)
            && snapped == (over != Over::Crop),
        format!("the stage is {size:?} snapped {snapped} over {over:?}, the source {source:?}"),
    )?;
    // The pan on screen is the scrollable's, which the card follows at once; the session's can
    // trail it by the one coalesced `view.set` in flight, as after a zoom whose scrollable kept its
    // relative offset. A gesture's frame waits for the session, and is held to both below.
    let session = session_pan(frame)?;
    let pan = pair(&minimap["pan"], "card pan")?;
    let view = View {
        stage: source,
        scale: percent / 100.0 / scale_factor,
        canvas,
        pan,
    };
    let scale = minimap["scale"].as_f64().ok_or("no scale")?;
    ensure(
        near(scale, view.scale, 1e-5),
        format!(
            "the card's scale is {scale}, {percent}% at {scale_factor} is {}",
            view.scale
        ),
    )?;
    let session_lag = [pan.0 - session.0, pan.1 - session.1];

    // Placement: the stage fitted into the image area, the card hugging it, inset from the
    // canvas's right edge and at least as far from its bottom (further where it rises above the
    // mode strip).
    let area = (
        MINIMAP_AREA.0.min(MINIMAP_SHARE * canvas[2]),
        MINIMAP_AREA.1.min(MINIMAP_SHARE * canvas[3]),
    );
    let fit = (area.0 / source.0).min(area.1 / source.1);
    let thumbnail = (source.0 * fit, source.1 * fit);
    let card = rect(&minimap["card"], "card")?;
    let image = rect(&minimap["image"], "image")?;
    let card_right = canvas[0] + canvas[2] - MINIMAP_INSET;
    let card_bottom = canvas[1] + canvas[3] - MINIMAP_INSET;
    ensure(
        near(image[2], thumbnail.0, GEOMETRY_SLACK)
            && near(image[3], thumbnail.1, GEOMETRY_SLACK)
            && near(card[2], thumbnail.0 + 2.0 * MINIMAP_PADDING, GEOMETRY_SLACK)
            && near(card[3], thumbnail.1 + 2.0 * MINIMAP_PADDING, GEOMETRY_SLACK)
            && near(card[0] + card[2], card_right, GEOMETRY_SLACK)
            && card[1] + card[3] <= card_bottom + GEOMETRY_SLACK
            && near(image[0], card[0] + MINIMAP_PADDING, GEOMETRY_SLACK)
            && near(image[1], card[1] + MINIMAP_PADDING, GEOMETRY_SLACK),
        format!(
            "the card {card:?} and image {image:?} are not a {thumbnail:?} thumbnail padded {MINIMAP_PADDING} and inset {MINIMAP_INSET} in canvas {canvas:?}"
        ),
    )?;
    let raised = card[1] + card[3] < card_bottom - GEOMETRY_SLACK;

    let shows = minimap["state"].as_str().ok_or("no state")?;
    let navigable = minimap["navigable"] == json!(true);
    ensure(
        minimap["after"] == json!(over == Over::Compare),
        format!("After is {} over {over:?}", minimap["after"]),
    )?;
    // The rectangle: the stage region the frame's own pan shows, in the image.
    let visible = view.visible();
    let expected_rectangle = [
        image[0] + visible[0] / source.0 * image[2],
        image[1] + visible[1] / source.1 * image[3],
        (visible[2] - visible[0]) / source.0 * image[2],
        (visible[3] - visible[1]) / source.1 * image[3],
    ];
    let rectangle = if navigable {
        let rectangle = rect(&minimap["rectangle"], "rectangle")?;
        ensure(
            rectangle
                .iter()
                .zip(expected_rectangle)
                .all(|(a, b)| near(*a, b, GEOMETRY_SLACK)),
            format!("the rectangle is {rectangle:?}, the pan shows {expected_rectangle:?}"),
        )?;
        Some(rectangle)
    } else {
        ensure(
            minimap["rectangle"].is_null() && matches!(shows, "pending" | "unavailable"),
            format!("a card {shows} that cannot navigate shows a rectangle"),
        )?;
        None
    };
    let thumbnail_pixels = if matches!(shows, "ready" | "updating") {
        let overview = &minimap["overview"];
        let stage = pair(&overview["stage"], "overview stage")?;
        ensure(
            near(stage.0, source.0, 0.5)
                && near(stage.1, source.1, 0.5)
                && overview["crop_input"] == json!(over == Over::Crop),
            format!("the overview shown is of {overview}, not this stage"),
        )?;
        check_thumbnail(frame, image, rectangle, scale_factor, shows, over)?
    } else {
        Value::Null
    };

    // A gesture: the pan worked here from the point it took.
    let mut gesture = Value::Null;
    if let Kind::Minimap(step) = kind {
        let step_record = &frame["step"];
        let previous = &frames[index - 1];
        let from = match &previous["state"]["minimap"]["pan"] {
            Value::Null => session_pan(previous)?,
            card => pair(card, "card pan before")?,
        };
        let was = View { pan: from, ..view };
        let from = was.clamp(from);
        let card_before = &step_record["minimap_before"];
        let image_before = rect(&card_before["image"], "image before the gesture")?;
        let rectangle_before = rect(&card_before["rectangle"], "rectangle before the gesture")?;
        let stage_at = |point: (f64, f64)| {
            (
                (point.0 - image_before[0]) / image_before[2] * source.0,
                (point.1 - image_before[1]) / image_before[3] * source.1,
            )
        };
        let contains = |[x, y, w, h]: [f64; 4], (px, py): (f64, f64)| {
            px >= x && px < x + w && py >= y && py < y + h
        };
        let routed: Vec<String> =
            serde_json::from_value(step_record["routed_messages"].clone()).unwrap_or_default();
        let point = step_record["window_point"]
            .as_array()
            .map(|_| pair(&step_record["window_point"], "window point"))
            .transpose()?;
        let (expected, slack) = match step {
            MinimapStep::Press(_) | MinimapStep::Click(_) => {
                let point = point.ok_or("no window point")?;
                let at = stage_at(point);
                let (expected, inert) = if !contains(image_before, point) {
                    (from, true)
                } else if contains(rectangle_before, point) {
                    (from, false)
                } else {
                    (was.centred(at), false)
                };
                ensure(
                    counted("presses")? == 1 && counted("inert")? == u64::from(inert),
                    format!(
                        "press counted {}, inert {}",
                        counted("presses")?,
                        counted("inert")?
                    ),
                )?;
                *grab = match step {
                    MinimapStep::Press(_) if !inert => Some(Grab { at, pan: expected }),
                    _ => None,
                };
                (expected, PAN_SLACK)
            }
            MinimapStep::Move(_) | MinimapStep::Release(_) => {
                let point = point.ok_or("no window point")?;
                let expected = match grab {
                    Some(held) => {
                        let to = stage_at(point);
                        was.clamp((
                            held.pan.0 + (to.0 - held.at.0) * view.scale,
                            held.pan.1 + (to.1 - held.at.1) * view.scale,
                        ))
                    }
                    None => from,
                };
                if matches!(step, MinimapStep::Release(_)) {
                    *grab = None;
                }
                (expected, PAN_SLACK)
            }
            MinimapStep::Wheel([x, y]) => {
                ensure(counted("wheels")? == 1, "the wheel was not the card's")?;
                (
                    was.clamp((from.0 - f64::from(x), from.1 - f64::from(y))),
                    WHEEL_SLACK,
                )
            }
            MinimapStep::Ready => (from, PAN_SLACK),
            MinimapStep::Pinch(_) => {
                ensure(
                    counted("pinches")? == 1,
                    "the pinch was not held by the card",
                )?;
                (from, PAN_SLACK)
            }
            MinimapStep::Api(_) => {
                let at = stage_at(point.ok_or("no window point")?);
                let expected = was.centred(at);
                let params = &step_record["api_params"];
                let sent = (
                    params["pan_x"].as_f64().ok_or("no API pan")?,
                    params["pan_y"].as_f64().ok_or("no API pan")?,
                );
                ensure(
                    near(sent.0, expected.0, PAN_SLACK) && near(sent.1, expected.1, PAN_SLACK),
                    format!("view.set was sent {sent:?}, an API client computes {expected:?}"),
                )?;
                (expected, PAN_SLACK)
            }
        };
        ensure(
            near(pan.0, expected.0, slack) && near(pan.1, expected.1, slack),
            format!("the gesture left the pan at {pan:?}, worked here {expected:?} from {from:?}"),
        )?;
        // The session holds the view the screen shows: the gesture went through `view.set`.
        ensure(
            near(session.0, expected.0, slack) && near(session.1, expected.1, slack),
            format!("the session's pan is {session:?}, the gesture's {expected:?}"),
        )?;
        ensure(
            minimap["dragging"] == json!(grab.is_some()),
            format!(
                "dragging is {}, the journey holds {grab:?}",
                minimap["dragging"]
            ),
        )?;
        // Routed events publish only the minimap's own messages: nothing reaches the tool, the
        // divider or the photograph beneath the card.
        let foreign: Vec<&String> = routed
            .iter()
            .filter(|message| !message.starts_with("View(Minimap"))
            .collect();
        ensure(
            foreign.is_empty(),
            format!("events routed onto the card published {foreign:?}"),
        )?;
        gesture = json!({
            "step": format!("{step:?}"),
            "window_point": point.map(|(x, y)| [x, y]),
            "from": [from.0, from.1],
            "expected": [expected.0, expected.1],
            "pan": [pan.0, pan.1],
            "slack": slack,
            "routed": routed,
        });
    } else if kind == Kind::Tool(Tool::Escape) {
        ensure(
            grab.is_some() && counted("escapes")? == 1 && minimap["dragging"] == json!(false),
            "Escape did not end the held drag",
        )?;
        *grab = None;
    }
    // A gesture changed nothing but the view: the crop or mask draft as it was, the stack and the
    // masks unchanged, Compare's divider where it was.
    if let Kind::Minimap(_) = kind {
        let previous = &frames[index - 1]["state"];
        for field in [
            "draft",
            "mask_draft",
            "stack",
            "masks",
            "comparison",
            "compare",
        ] {
            ensure(
                state[field] == previous[field],
                format!(
                    "a minimap gesture changed {field}: {} became {}",
                    previous[field], state[field]
                ),
            )?;
        }
    }
    if kind == Kind::Tool(Tool::Escape) {
        let previous = &frames[index - 1];
        ensure(
            !frame.draft().is_null() && frame.draft() == previous.draft(),
            "Escape taken by the minimap's drag cancelled the crop draft",
        )?;
    }
    Ok(json!({
        "shown": true,
        "state": shows,
        "navigable": navigable,
        "after": minimap["after"],
        "card": card,
        "raised_above_strip": raised,
        "session_lag": session_lag,
        "image": image,
        "expected_thumbnail": [thumbnail.0, thumbnail.1],
        "rectangle": rectangle,
        "expected_rectangle": expected_rectangle,
        "overview": minimap["overview"],
        "thumbnail_pixels": thumbnail_pixels,
        "gesture": gesture,
    }))
}

/// The thumbnail's pixels: each quadrant's colour at its centre, away from the viewport's outline
/// and the captions, and the outline's bright inner edge where the rectangle's left edge lies
/// inside the image.
fn check_thumbnail(
    frame: &Frame,
    image: [f64; 4],
    rectangle: Option<[f64; 4]>,
    scale: f64,
    shows: &str,
    over: Over,
) -> Result<Value> {
    let capture = frame.image()?;
    let pixel = |x: f64, y: f64| {
        let (x, y) = ((x * scale).floor() as u32, (y * scale).floor() as u32);
        capture
            .get_pixel(x.min(capture.width() - 1), y.min(capture.height() - 1))
            .0
    };
    let caption = 20.0;
    let clear = |x: f64, y: f64| {
        let outline = rectangle.is_some_and(|[rx, ry, rw, rh]| {
            let margin = 5.0;
            x > rx - margin && x < rx + rw + margin && y > ry - margin && y < ry + rh + margin
        });
        let after = over == Over::Compare && y < image[1] + caption;
        let quiet = shows != "ready" && y > image[1] + image[3] - caption;
        !outline && !after && !quiet
    };
    let mut samples = Vec::new();
    for (index, (fx, fy)) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)]
        .into_iter()
        .enumerate()
    {
        let (x, y) = (image[0] + image[2] * fx, image[1] + image[3] * fy);
        if !clear(x, y) {
            continue;
        }
        let shown = pixel(x, y);
        let expected = crate::fixtures::COLORS[index];
        ensure(
            shown
                .iter()
                .zip(expected)
                .all(|(a, b)| a.abs_diff(b) <= THUMBNAIL_TOLERANCE),
            format!("the thumbnail shows {shown:?} at ({fx}, {fy}), the fixture {expected:?}"),
        )?;
        samples.push(json!({"at":[fx,fy],"shown":shown}));
    }
    ensure(
        samples.len() >= 2,
        "fewer than two of the thumbnail's quadrants are clear of the outline to sample",
    )?;
    // The outline: a bright inner edge within a few physical pixels of the rectangle's left edge.
    let mut outline = Value::Null;
    if let Some([rx, ry, _, rh]) = rectangle
        && rx > image[0] + 3.0
    {
        let y = ry + rh / 2.0;
        let bright = (-3..=3).any(|offset| {
            pixel(rx + f64::from(offset) / scale, y)
                .iter()
                .all(|channel| *channel >= 200)
        });
        ensure(bright, "the viewport rectangle's bright edge is not drawn")?;
        outline = json!({"left_edge":rx,"row":y,"bright":true});
    }
    Ok(json!({"quadrants": samples, "outline": outline}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script is one step per planned frame after the open one, in order.
    #[test]
    fn the_script_is_one_step_per_planned_frame() {
        let plan = plan(&[]);
        assert_eq!(plan.len(), PLAN.len());
        let script = plan.script();
        let steps = script.as_array().expect("an array");
        assert_eq!(steps.len(), PLAN.len() - 1);
        assert_eq!(steps[0], script::Step::wait(SETTLE_MS).to_value());
        assert_eq!(
            steps[2],
            script::Step::View(ViewStep::Percent(50.0)).to_value()
        );
        assert_eq!(steps[10], script::Step::pan(0.5, 0.5).to_value());
        assert_eq!(
            *steps.last().expect("a last step"),
            script::Step::View(ViewStep::Fit).to_value()
        );
    }

    /// Flat quadrant interiors map to their colour; anything near a drawn feature, or outside the
    /// image, is not sampled.
    #[test]
    fn only_flat_quadrant_interiors_are_sampled() {
        let stage = (6000, 4000);
        let colors = crate::fixtures::COLORS;
        assert_eq!(flat_colour((1000.0, 1000.0), stage), Some(colors[0]));
        assert_eq!(flat_colour((5000.0, 1000.0), stage), Some(colors[1]));
        assert_eq!(flat_colour((1000.0, 3000.0), stage), Some(colors[2]));
        assert_eq!(flat_colour((5000.0, 3000.0), stage), Some(colors[3]));
        for near in [
            (3000.5, 1000.0),
            (2990.0, 1000.0),
            (1000.0, 2005.0),
            (3000.0, 40.0),
            (100.0, 2010.0),
            (5.0, 1000.0),
            (-1.0, 1000.0),
            (6001.0, 1000.0),
        ] {
            assert_eq!(flat_colour(near, stage), None, "{near:?}");
        }
    }

    /// The minimap rows script the gestures they name, between the threshold and the last Fit.
    #[test]
    fn the_minimap_rows_script_their_gestures() {
        let plan = plan(&[]);
        let script = plan.script();
        let steps = script.as_array().expect("an array");
        let at = |name: &str| {
            PLAN.iter()
                .position(|(row, ..)| *row == name)
                .expect("a planned row")
                - 1
        };
        assert_eq!(
            steps[at("mm-click")],
            json!({"minimap": {"click": {"image": [0.25_f32, 0.3_f32]}}})
        );
        assert_eq!(
            steps[at("crop-escape")],
            script::Step::Key {
                key: script::KEY_ESCAPE.into()
            }
            .to_value()
        );
        assert!(steps.len() <= script::MAX_SCRIPT_STEPS);
    }

    /// The independent view geometry: at 400% on a 2× display a 6000 × 4000 stage is 12000 × 8000
    /// logical pixels, and centring an 898 × 830 canvas on its middle pans by half of each less
    /// half the canvas; the pan clamps at the far edges and the visible region is the canvas over
    /// two.
    #[test]
    fn the_view_geometry_centres_clamps_and_shows() {
        let view = View {
            stage: (6000.0, 4000.0),
            scale: 2.0,
            canvas: [241.0, 44.0, 898.0, 830.0],
            pan: (5551.0, 3585.0),
        };
        assert_eq!(view.centred((3000.0, 2000.0)), (5551.0, 3585.0));
        assert_eq!(view.centred((6000.0, 4000.0)), (11102.0, 7170.0));
        assert_eq!(view.centred((0.0, 0.0)), (0.0, 0.0));
        assert_eq!(view.visible(), [2775.5, 1792.5, 3224.5, 2207.5]);
    }

    /// The mapping from a captured pixel to a source pixel and back is one affine map.
    #[test]
    fn the_mapping_round_trips() {
        let mapping = Mapping {
            origin: (482.0 - 47102.0, 90.0 - 31172.0),
            zoom: 16.0,
        };
        for point in [(482.0, 90.0), (1000.5, 700.25)] {
            let source = mapping.source(point);
            let back = mapping.screen(source);
            assert!((back.0 - point.0).abs() < 1e-9 && (back.1 - point.1).abs() < 1e-9);
        }
        assert_eq!(
            mapping.source((482.0, 90.0)),
            (47102.0 / 16.0, 31172.0 / 16.0)
        );
    }
}
