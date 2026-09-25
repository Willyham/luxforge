//! The `zoom` smoke scenario: the photograph at percentage zooms, over the generated 24 MP and
//! 60 MP JPEGs, one launch each.
//!
//! Every zoom draws the photograph through the photo surface, which hands the renderer only the part
//! of the zoomed box that is on screen. This scenario is the rendered proof of that. At 50% (the
//! display proxy stretched over the exact stage's box), 100%, 120%, 800% and 1600% — where the box
//! is far larger than any GPU viewport — and panned to the centre and the far corner at 1600%, each
//! captured frame is checked sample by sample against the source pixel the zoom and the pan put
//! under it, and where a quadrant boundary is in view its position is measured against the one the
//! geometry predicts. The texture on screen is checked to be the proxy below 100% and the exact
//! render from 100% up, in the state and in the `preview_displayed` event that put it there; at
//! 100% and above the 60 MP render is wider than the device's texture limit and is drawn from
//! tiles. Three `wait` steps, at Fit, at 100% and at 1600%, prove that idling rebuilds the view
//! without writing the texture, asking for a frame or changing a pixel; the Fit one follows a
//! shorter wait that lets the launch's own refit to the display scale land first. A wgpu
//! validation error or a panic in either launch fails the run.
use crate::{
    scenario::{Checked, Fixture, Plan, Run, Step, plan::only},
    smoke::Scenario,
    *,
};
use lightwell_evidence::{self as script, ViewStep};

pub const SCENARIO: &str = "zoom";

/// How long each idle `wait` step lasts: four ticks of the evidence run's 250 ms timer, each of
/// which rebuilds the view as the editor's own event sync does while a photograph is open.
const WAIT_MS: u64 = 1000;
/// How long the settling wait after the open lasts: long enough for the refit the display scale
/// asks for once it is known, which is not idle behaviour and so is not what the idle check sees.
const SETTLE_MS: u64 = 500;

/// A pixel shows a fixture colour when every channel is within this many codes of it.
const TOLERANCE: u8 = 10;
/// How far, in source pixels, a sample must stay from anything the fixture draws besides its flat
/// quadrants — the quadrant boundaries, the white centre line and arrow, the black dashes, the
/// image's own edge — to be expected to show a flat quadrant colour. It clears JPEG ringing and
/// chroma subsampling, and the proxy's averaging below 100%.
const FEATURE_MARGIN: f64 = 12.0;
/// The sample grid's spacing, in physical pixels.
const SAMPLE_STEP: usize = 12;
/// Physical pixels left out at the canvas's right and bottom edges, where the scrollbars float over
/// the photograph, and at its bottom, where the mode strip does.
const SCROLLBAR_INSET: u32 = 30;
const STRIP_INSET: u32 = 150;

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
}

/// Every frame in capture order: the open frame, then one per script step, each named, with the
/// zoom it must be drawn at. The plan, and so the script and the frame count, is made from this
/// table, and the checks below walk the same table beside the frames the plan held.
const PLAN: [(&str, Kind, Zoom); 13] = [
    ("opened", Kind::Open, Zoom::Fit),
    ("settle", Kind::Settle, Zoom::Fit),
    ("idle-fit", Kind::Wait, Zoom::Fit),
    ("50", Kind::View, Zoom::Percent(50.0)),
    ("100", Kind::View, Zoom::Percent(100.0)),
    ("idle-100", Kind::Wait, Zoom::Percent(100.0)),
    ("120", Kind::View, Zoom::Percent(120.0)),
    ("800", Kind::View, Zoom::Percent(800.0)),
    ("1600", Kind::View, Zoom::Percent(1600.0)),
    ("pan-centre", Kind::Pan(0.5, 0.5), Zoom::Percent(1600.0)),
    ("idle-1600", Kind::Wait, Zoom::Percent(1600.0)),
    ("pan-corner", Kind::Pan(1.0, 1.0), Zoom::Percent(1600.0)),
    ("fit", Kind::View, Zoom::Fit),
];

/// The step each planned frame after the open one is captured for, as the script writes it and as
/// the editor records it back.
fn request(kind: Kind, zoom: Zoom) -> Option<script::Step> {
    match kind {
        Kind::Open => None,
        Kind::Settle => Some(script::Step::wait(SETTLE_MS)),
        Kind::Wait => Some(script::Step::wait(WAIT_MS)),
        Kind::Pan(x, y) => Some(script::Step::pan(x as f32, y as f32)),
        Kind::View => Some(match zoom {
            Zoom::Fit => script::Step::View(ViewStep::Fit),
            Zoom::Percent(value) => script::Step::View(ViewStep::Percent(value as f32)),
        }),
    }
}

/// Every frame of one launch: the photograph opened unedited, then each zoom, pan and wait, none of
/// which commits anything.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(
        PLAN.iter()
            .map(|(name, kind, zoom)| match request(*kind, *zoom) {
                None => Step::opened(*name).label("Original"),
                Some(request) => Step::new(*name, request).commits(0),
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

/// Where a percentage frame maps captured pixels to source pixels: the canvas's top-left corner in
/// physical pixels, the pan in physical pixels and the zoom as physical pixels per source pixel.
/// At every zoom this scenario takes, the zoomed box is larger than the canvas on both axes, so the
/// scrollable anchors it at the canvas's corner and the pan is the whole offset.
struct Mapping {
    origin: (f64, f64),
    pan: (f64, f64),
    zoom: f64,
}

impl Mapping {
    fn source(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            (x - self.origin.0 + self.pan.0) / self.zoom,
            (y - self.origin.1 + self.pan.1) / self.zoom,
        )
    }

    fn screen(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            self.origin.0 - self.pan.0 + x * self.zoom,
            self.origin.1 - self.pan.1 + y * self.zoom,
        )
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
/// colour, and any quadrant boundary in view must sit where the geometry puts it.
fn check_mapping(
    image: &image::RgbImage,
    frame: &Value,
    zoom: f64,
    stage: (u32, u32),
) -> Result<Value> {
    let [left, top, right, bottom] = canvas_rect(frame)?;
    let scale = frame["scale"].as_f64().ok_or("Frame records no scale")?;
    let view = &frame["state"]["surface"]["view"];
    let pan = (
        view["pan_x"].as_f64().ok_or("Frame records no pan")? * scale,
        view["pan_y"].as_f64().ok_or("Frame records no pan")? * scale,
    );
    let mapping = Mapping {
        origin: (f64::from(left), f64::from(top)),
        pan,
        zoom: zoom / 100.0,
    };
    let (w, h) = (f64::from(stage.0), f64::from(stage.1));
    ensure(
        w * mapping.zoom > f64::from(right - left) && h * mapping.zoom > f64::from(bottom - top),
        "The zoomed box does not cover the canvas; this check assumes it does",
    )?;
    let pixel = |x: u32, y: u32| image.get_pixel(x, y).0;
    let (mut checked, mut matched) = (0u32, 0u32);
    let mut colours = [0u32; 4];
    let mut mismatches = Vec::new();
    for y in (top + 8..bottom - STRIP_INSET).step_by(SAMPLE_STEP) {
        for x in (left + 8..right - SCROLLBAR_INSET).step_by(SAMPLE_STEP) {
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
        "pan_physical": [pan.0, pan.1],
        "boundaries": boundaries,
    }))
}

/// The canvas region of two captures, compared byte for byte.
fn same_canvas(a: &image::RgbImage, b: &image::RgbImage, rect: [u32; 4]) -> Result<bool> {
    ensure(
        a.dimensions() == b.dimensions(),
        "Two captures of one run have different sizes",
    )?;
    let [left, top, right, bottom] = rect;
    Ok((top..bottom).all(|y| (left..right).all(|x| a.get_pixel(x, y) == b.get_pixel(x, y))))
}

fn number(value: &Value, what: &str) -> Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| format!("Frame records no {what}").into())
}

/// Every frame against the zoom its step asks for: the texture on screen, the source pixels the zoom
/// and pan put under each sample, and, for the idle steps, a view rebuilt with no new frame.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
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
    let mut checks = Vec::new();
    for (index, ((name, kind, zoom), frame)) in PLAN.iter().zip(frames).enumerate() {
        let what = format!("step {name:?} (frame {index}, {kind:?} at {zoom:?})");
        let state = &frame["state"];
        let image = frame.image()?;
        ensure(
            state["phase"] == "ready" && state["source_dimensions"] == json!([stage.0, stage.1]),
            format!("{what}: not a ready frame of the opened photograph"),
        )?;
        let surface = &state["surface"];
        let expected_zoom = match zoom {
            Zoom::Fit => json!({"mode":"fit"}),
            Zoom::Percent(value) => json!({"mode":"percent","value":value}),
        };
        ensure(
            surface["view"]["zoom"] == expected_zoom,
            format!("{what}: the session's zoom is {}", surface["view"]["zoom"]),
        )?;

        // The texture on screen: the proxy wherever the stage is drawn smaller than itself, the
        // exact render from 100% up.
        let proxy = match zoom {
            Zoom::Fit => true,
            Zoom::Percent(value) => *value < 100.0,
        };
        let raster: [u32; 2] = serde_json::from_value(surface["raster"].clone())
            .map_err(|_| format!("{what}: no raster on the surface"))?;
        ensure(
            state["proxy"]["presented"] == json!(proxy),
            format!("{what}: proxy presented is {}", state["proxy"]["presented"]),
        )?;
        // A proxy is the stage scaled into the bounds the view asks for now — never one left over
        // from a previous zoom — and the exact render is the stage itself. The opened frame is the
        // exception: the open request is sent before the display scale is known, so its proxy is
        // made for the bounds at scale 1 and the refit that follows is what the settling wait
        // lets land.
        let expected_raster = if proxy && *kind == Kind::Open {
            raster
        } else if proxy {
            let bounds = &state["proxy"]["bounds"];
            let (width, height) = (
                bounds["width"].as_f64().ok_or("No proxy bounds")?,
                bounds["height"].as_f64().ok_or("No proxy bounds")?,
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
            raster == expected_raster && (!proxy || (raster[0] < stage.0 && raster[1] < stage.1)),
            format!(
                "{what}: the surface holds a {raster:?} raster of a {stage:?} stage, expected {expected_raster:?}"
            ),
        )?;
        // Every raster handed to the surface is one `preview_displayed` and one new version, so
        // the event that put this frame's raster on screen is the version-th of them.
        let version = number(&surface["version"], "surface version")?;
        let displayed = events
            .iter()
            .filter(|event| event["event"] == "preview_displayed")
            .nth(
                usize::try_from(version)?
                    .checked_sub(1)
                    .ok_or("Version zero")?,
            )
            .ok_or_else(|| format!("{what}: no preview_displayed for version {version}"))?;
        let detail = &displayed["detail"];
        ensure(
            detail["proxy"] == json!(proxy)
                && detail["path"] == "surface"
                && detail["dimensions"] == json!([stage.0, stage.1])
                && detail["proxy_dimensions"] == if proxy { json!(raster) } else { Value::Null },
            format!("{what}: the raster on screen was displayed as {detail}"),
        )?;

        let pixels = match zoom {
            Zoom::Fit => frame
                .fixture(Fixture {
                    aspect: Some(f64::from(stage.0) / f64::from(stage.1)),
                    ..Fixture::fit(1)
                })
                .map_err(|error| format!("{what}: {error}"))?,
            Zoom::Percent(value) => check_mapping(image, frame, *value, stage)
                .map_err(|error| format!("{what}: {error}"))?,
        };

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
            if version == previous_version {
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
                let rect = canvas_rect(frame)?;
                ensure(
                    same_canvas(frames[index - 1].image()?, image, rect)?,
                    format!("{what}: the canvas changed while idling"),
                )?;
                idle = json!({"views_rebuilt":views - previous_views,"canvas_identical":true,"events_between":between.len()});
            }
        }
        checks.push(json!({
            "frame": frame["file"],
            "step": name,
            "kind": format!("{kind:?}"),
            "zoom": expected_zoom,
            "proxy": proxy,
            "raster": raster,
            "displayed_generation": displayed["detail"]["generation"],
            "version": version,
            "texture_writes": writes,
            "views": views,
            "pan": [surface["view"]["pan_x"], surface["view"]["pan_y"]],
            "pixels": pixels,
            "idle": idle,
        }));
    }
    write_json(
        &evidence.join("zoom-checks.json"),
        &json!({
            "stage": [stage.0, stage.1],
            "checks": checks,
            "tolerance_per_channel": TOLERANCE,
            "feature_margin_source_pixels": FEATURE_MARGIN,
            "sample_step_physical_pixels": SAMPLE_STEP,
            "scope": "Displayed geometry, texture identity and idle stability read back from the window renderer; not display scanout or colour calibration",
        }),
    )?;
    Ok(())
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
        assert_eq!(steps[8], script::Step::pan(0.5, 0.5).to_value());
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

    /// The mapping from a captured pixel to a source pixel and back is one affine map.
    #[test]
    fn the_mapping_round_trips() {
        let mapping = Mapping {
            origin: (482.0, 90.0),
            pan: (47102.0, 31172.0),
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
