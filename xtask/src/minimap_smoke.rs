//! The `minimap` smoke scenario: the high-zoom minimap ([`docs/design/minimap.md`]) across what the
//! `zoom` journey leaves out — the threshold reached by the API and by a pinch, every corner, the
//! window resized, the interface scaled, the panels hidden and the filmstrip collapsed, edits,
//! history, a straightened crop, a mask, the window hidden and shown again, Select and Develop
//! again, a switch to another photograph and back, and the reference renderer in the GPU stage's
//! place — over any photograph: the generated 24 MP JPEG by default, the 60 MP one or a supplied RAW
//! file through `--source`.
//!
//! Two launches per photograph, each a whole smoke run of its own named for it:
//!
//! - `gpu`, the ordinary editor, walks [`GPU_PLAN`]: the threshold inclusive at exactly 200%,
//!   fractional at 199.9% and 200.1%, through `view.set` on the desktop's own session and through a
//!   pinch that crosses it each way, then 800%; a drag of the viewport rectangle into each corner of
//!   the stage and a click; the tools and state panels hidden, the filmstrip collapsed, the window
//!   made smaller and the interface 125%, with a click under each; an exposure, the Original
//!   selected in History and current again; a 1:1 crop straightened by 6°; a radial mask with an
//!   exposure of its own and the mask mode; the window hidden and shown; the 480 × 320 orientation
//!   fixture opened at 200%, where the stage fits the canvas on both axes, and the photograph opened
//!   again; Select, where the overview is let go, and Develop again, where it is made anew; and Fit.
//! - `reference`, with `--no-gpu-render`, walks [`REFERENCE_PLAN`]: the overview made only by the
//!   reference's reduction of the frame it retains, through navigation, edits, a crop, a mask and a
//!   resize.
//!
//! Every frame holds the minimap to the design: shown exactly from 200% of the view's own zoom; its
//! card, image area and stage where the geometry worked here puts them for the frame's canvas,
//! display and interface scale; its rectangle the region the frame's own pan shows; a gesture's pan
//! the one worked here from the point it took, held by the desktop's session; a gesture editing
//! nothing; the stage the photograph's own, the straightened crop's square or the switched
//! photograph's; and the overview shown of that stage. Wherever the card shows a ready overview of a
//! plain canvas, the thumbnail and the main photograph agree: the middle of the visible region,
//! averaged in linear light on both, is the same colour within [`AGREEMENT`] codes, so an overview of
//! another photograph, another entry or an earlier edit cannot pass. A `ready` step after an edit,
//! a history selection or an open shows a newer overview than the last one ready; navigation, zoom
//! above the threshold, resize, panels and interface scale reuse it with no build, sweep or upload;
//! the hidden window does no overview work and keeps the overview it holds; the reference launch's
//! overviews are all the reference's. The original is unchanged and the editor's log carries no validation error or panic.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step},
    smoke::Scenario,
    zoom_smoke::{
        GEOMETRY_SLACK, MINIMAP_AREA, MINIMAP_FROM, MINIMAP_INSET, MINIMAP_PADDING, MINIMAP_SHARE,
        PAN_SLACK, View, card_pixels, near, pair, rect, session_pan,
    },
    *,
};
use luxforge_core::{MASK_MODE, POINTER_MODE};
use luxforge_evidence::{
    self as script, DevelopStep, MinimapPoint, MinimapStep, PinchStep, PreviewStep, SelectStep,
    SelectWorkspace, ViewStep, WorkspaceStep,
};

pub const SCENARIO: &str = "minimap";
/// The photograph the journey switches to: 480 × 320, which fits the canvas at 200% on both axes.
pub const SWITCH: &str = "fixtures/s0/orientation-1.jpg";
const SWITCH_SIZE: (f64, f64) = (480.0, 320.0);
/// The straightened crop: the largest square inside the photograph turned by this many degrees.
const STRAIGHTEN_DEGREES: f64 = 6.0;
/// How far, in stage pixels, the straightened square may lie from the one worked here.
const SQUARE_SLACK: f64 = 2.0;
/// The window the launch opens at and the smaller one it is resized to, in points.
const WINDOW: (f32, f32) = (1440.0, 900.0);
const SMALL_WINDOW: (f32, f32) = (1000.0, 700.0);
/// How far, per channel in 8-bit codes, the thumbnail's and the photograph's means over the middle
/// of the visible region may differ: the thumbnail is the stage reduced some twenty-fold, so a
/// boundary across that region sits up to one thumbnail pixel from where the photograph has it.
const AGREEMENT: f64 = 10.0;
/// The share of the visible region, centred, that the agreement averages.
const AGREEMENT_SHARE: f64 = 0.4;
/// The fewest thumbnail pixels a mean is compared over: a region at 800% covers a handful.
const MIN_THUMBNAIL_PIXELS: u64 = 64;
/// The physical pixels at the canvas's bottom where the mode strip floats over the photograph.
const STRIP_INSET: f64 = 150.0;

/// What a frame's step does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Do {
    Open,
    Wait(u64),
    View(Option<f64>),
    /// `view.set` on the desktop's own session, as an API client sets zoom.
    ApiZoom(f64),
    /// A trackpad pinch over the canvas by this factor.
    Pinch(f64),
    Gesture(MinimapStep),
    Panels {
        state: bool,
        tools: bool,
    },
    Window((f32, f32)),
    Interface(u16),
    Exposure(f64),
    Straighten,
    Radial,
    MaskedExposure,
    Mode(&'static str),
    History(Option<u64>),
    /// The filmstrip collapsed or expanded, as `Cmd+Option+F` does.
    Strip,
    /// Select shown, or Develop again.
    Workspace(SelectWorkspace),
    Switch,
    Return,
    Visibility(&'static str),
}

/// The stage a frame navigates.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    /// The photograph's own output.
    Photo,
    /// The straightened 1:1 crop of it.
    Square,
    /// The switched-to fixture.
    Switched,
}

/// One planned frame: its name, step, the zoom it is drawn at where the step decides it, and
/// whether a `ready` step must show a newer overview than the last one ready.
struct Row {
    name: &'static str,
    action: Do,
    zoom: Option<Zoom>,
    fresh: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Zoom {
    Fit,
    Percent(f64),
}

const fn row(name: &'static str, action: Do, zoom: Option<Zoom>) -> Row {
    Row {
        name,
        action,
        zoom,
        fresh: false,
    }
}

const fn at(name: &'static str, action: Do, percent: f64) -> Row {
    row(name, action, Some(Zoom::Percent(percent)))
}

const fn ready(name: &'static str, percent: f64, fresh: bool) -> Row {
    Row {
        name,
        action: Do::Gesture(MinimapStep::Ready),
        zoom: Some(Zoom::Percent(percent)),
        fresh,
    }
}

const fn gesture(name: &'static str, step: MinimapStep) -> Row {
    at(name, Do::Gesture(step), 200.0)
}

const fn image(x: f32, y: f32) -> MinimapPoint {
    MinimapPoint::Image([x, y])
}

const CENTRE: MinimapPoint = MinimapPoint::Rectangle([0.5, 0.5]);

/// The ordinary editor's journey.
const GPU_PLAN: [Row; 65] = [
    row("opened", Do::Open, Some(Zoom::Fit)),
    row("settle", Do::Wait(500), Some(Zoom::Fit)),
    at("199.9", Do::View(Some(199.9)), 199.9),
    at("200", Do::View(Some(200.0)), 200.0),
    ready("ready", 200.0, true),
    at("200.1", Do::View(Some(200.1)), 200.1),
    at("api-199.9", Do::ApiZoom(199.9), 199.9),
    at("api-200", Do::ApiZoom(200.0), 200.0),
    at("180", Do::View(Some(180.0)), 180.0),
    at("pinch-up", Do::Pinch(1.2), 216.0),
    at("pinch-down", Do::Pinch(1.0 / 1.2), 180.0),
    at("800", Do::View(Some(800.0)), 800.0),
    at("200-again", Do::View(Some(200.0)), 200.0),
    gesture("tl-press", MinimapStep::Press(CENTRE)),
    gesture("tl-move", MinimapStep::Move(image(-0.5, -0.5))),
    gesture("tl-release", MinimapStep::Release(image(-0.5, -0.5))),
    gesture("tr-press", MinimapStep::Press(CENTRE)),
    gesture("tr-move", MinimapStep::Move(image(1.5, -0.5))),
    gesture("tr-release", MinimapStep::Release(image(1.5, -0.5))),
    gesture("bl-press", MinimapStep::Press(CENTRE)),
    gesture("bl-move", MinimapStep::Move(image(-0.5, 1.5))),
    gesture("bl-release", MinimapStep::Release(image(-0.5, 1.5))),
    gesture("br-press", MinimapStep::Press(CENTRE)),
    gesture("br-move", MinimapStep::Move(image(1.5, 1.5))),
    gesture("br-release", MinimapStep::Release(image(1.5, 1.5))),
    gesture("centre", MinimapStep::Click(image(0.5, 0.5))),
    at(
        "panels-hidden",
        Do::Panels {
            state: false,
            tools: false,
        },
        200.0,
    ),
    gesture("panels-click", MinimapStep::Click(image(0.3, 0.3))),
    at(
        "panels-shown",
        Do::Panels {
            state: true,
            tools: true,
        },
        200.0,
    ),
    at("strip-collapsed", Do::Strip, 200.0),
    gesture("strip-click", MinimapStep::Click(image(0.6, 0.7))),
    at("strip-expanded", Do::Strip, 200.0),
    at("small-window", Do::Window(SMALL_WINDOW), 200.0),
    gesture("small-click", MinimapStep::Click(image(0.7, 0.6))),
    at("window-back", Do::Window(WINDOW), 200.0),
    at("interface-125", Do::Interface(125), 200.0),
    gesture("interface-click", MinimapStep::Click(image(0.4, 0.6))),
    at("interface-100", Do::Interface(100), 200.0),
    at("exposure", Do::Exposure(1.0), 200.0),
    ready("exposure-ready", 200.0, true),
    at("original", Do::History(Some(0)), 200.0),
    ready("original-ready", 200.0, true),
    at("current", Do::History(None), 200.0),
    at("straighten", Do::Straighten, 200.0),
    ready("straighten-ready", 200.0, true),
    gesture("straighten-click", MinimapStep::Click(image(0.25, 0.75))),
    at("radial", Do::Radial, 200.0),
    at("masked", Do::MaskedExposure, 200.0),
    ready("masked-ready", 200.0, true),
    at("mask-mode", Do::Mode(MASK_MODE), 200.0),
    gesture("mask-click", MinimapStep::Click(image(0.6, 0.4))),
    at("mask-left", Do::Mode(POINTER_MODE), 200.0),
    // A hidden window does no overview work and keeps what it holds.
    at("shown", Do::Visibility("show_window"), 200.0),
    at("hidden", Do::Visibility("hide_window"), 200.0),
    at("hidden-wait", Do::Wait(1000), 200.0),
    at("reshown", Do::Visibility("show_window"), 200.0),
    // The view keeps its zoom across an open.
    at("switch", Do::Switch, 200.0),
    ready("switch-ready", 200.0, true),
    gesture("switch-click", MinimapStep::Click(image(0.2, 0.2))),
    at("return", Do::Return, 200.0),
    ready("return-ready", 200.0, true),
    // Select lets the overview go, and the pipeline the source; Develop again hands the source
    // back and makes the overview anew, after another photograph was opened and this one again.
    at("select", Do::Workspace(SelectWorkspace::Select), 200.0),
    at("develop", Do::Workspace(SelectWorkspace::Develop), 200.0),
    ready("recovered", 200.0, true),
    row("fit", Do::View(None), Some(Zoom::Fit)),
];

/// The reference renderer's journey, with the GPU stage refused.
const REFERENCE_PLAN: [Row; 19] = [
    row("opened", Do::Open, Some(Zoom::Fit)),
    row("settle", Do::Wait(500), Some(Zoom::Fit)),
    at("200", Do::View(Some(200.0)), 200.0),
    ready("ready", 200.0, true),
    gesture("click", MinimapStep::Click(image(0.3, 0.6))),
    gesture("br-press", MinimapStep::Press(CENTRE)),
    gesture("br-move", MinimapStep::Move(image(1.5, 1.5))),
    gesture("br-release", MinimapStep::Release(image(1.5, 1.5))),
    at("exposure", Do::Exposure(1.0), 200.0),
    ready("exposure-ready", 200.0, true),
    at("straighten", Do::Straighten, 200.0),
    ready("straighten-ready", 200.0, true),
    gesture("straighten-click", MinimapStep::Click(image(0.7, 0.3))),
    at("radial", Do::Radial, 200.0),
    at("masked", Do::MaskedExposure, 200.0),
    ready("masked-ready", 200.0, true),
    at("small-window", Do::Window(SMALL_WINDOW), 200.0),
    at("window-back", Do::Window(WINDOW), 200.0),
    row("fit", Do::View(None), Some(Zoom::Fit)),
];

/// The step a planned frame after the open one is captured for, over the photograph the launch
/// opened.
fn request(action: Do, source: &Path) -> Option<script::Step> {
    let call = |method: &str, params: Value| Some(script::Step::call(method, params));
    match action {
        Do::Open => None,
        Do::Wait(ms) => Some(script::Step::wait(ms)),
        Do::View(None) => Some(script::Step::View(ViewStep::Fit)),
        Do::View(Some(percent)) => Some(script::Step::View(ViewStep::Percent(percent as f32))),
        Do::ApiZoom(percent) => call(
            "view.set",
            json!({"zoom": {"mode": "percent", "value": percent}}),
        ),
        Do::Pinch(factor) => Some(
            PinchStep {
                delta: factor.ln(),
                x: 0.4,
                y: 0.4,
            }
            .into(),
        ),
        Do::Gesture(step) => Some(script::Step::Minimap(step)),
        Do::Panels { state, tools } => Some(script::Step::Workspace(WorkspaceStep {
            state_panel: Some(state),
            tools_panel: Some(tools),
            ..WorkspaceStep::default()
        })),
        Do::Window((width, height)) => Some(script::Step::WindowSize { width, height }),
        Do::Interface(size) => {
            let mut fields = serde_json::Map::new();
            fields.insert("interface_size".into(), json!(size));
            Some(script::Step::Preference(fields))
        }
        Do::Exposure(ev) => call("edit.set-basic", json!({"exposure": ev})),
        Do::Straighten => call(
            "edit.crop-fit",
            json!({"aspect": "1:1", "angle": STRAIGHTEN_DEGREES}),
        ),
        Do::Radial => call(
            "mask.create-radial",
            json!({"x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.3, "angle": 0.0,
                "feather": 50.0}),
        ),
        Do::MaskedExposure => call("edit.set-basic", json!({"mask": 0, "exposure": -1.5})),
        Do::Mode(mode) => Some(script::Step::Workspace(WorkspaceStep::default().mode(mode))),
        Do::History(Some(sequence)) => Some(script::Step::Preview(PreviewStep::Sequence(sequence))),
        Do::History(None) => Some(script::Step::Preview(PreviewStep::Current)),
        Do::Strip => Some(script::Step::Develop(DevelopStep::Strip)),
        Do::Workspace(workspace) => Some(script::Step::Select(SelectStep::Switch(workspace))),
        Do::Switch => Some(script::Step::Open {
            path: SWITCH.into(),
        }),
        Do::Return => Some(script::Step::Open {
            path: source.to_string_lossy().into_owned(),
        }),
        Do::Visibility(action) => Some(script::Step::WindowVisibility {
            action: action.into(),
        }),
    }
}

/// How many revisions a step commits.
fn commits(action: Do) -> u64 {
    match action {
        Do::Exposure(_) | Do::Straighten | Do::Radial | Do::MaskedExposure => 1,
        _ => 0,
    }
}

fn plan_of(rows: &[Row], sources: &[PathBuf]) -> Plan {
    let source = sources.first().map_or(Path::new(""), PathBuf::as_path);
    Plan::new(
        rows.iter()
            .map(|row| {
                let mut step = match request(row.action, source) {
                    None => Step::opened(row.name),
                    // An open's history is the photograph's, not the frame before's.
                    Some(request) if matches!(row.action, Do::Switch | Do::Return) => {
                        Step::new(row.name, request)
                    }
                    Some(request) => Step::new(row.name, request).commits(commits(row.action)),
                };
                if let Do::Mode(mode) = row.action {
                    step = step.mode(mode);
                }
                match row.zoom {
                    Some(Zoom::Fit) => step.fit(),
                    Some(Zoom::Percent(value)) => step.percent(value),
                    None => step,
                }
            })
            .collect(),
    )
}

pub fn gpu_plan(sources: &[PathBuf]) -> Plan {
    plan_of(&GPU_PLAN, sources)
}

pub fn reference_plan(sources: &[PathBuf]) -> Plan {
    plan_of(&REFERENCE_PLAN, sources)
}

/// One run per photograph, named for it, each an ordinary smoke run of both launches with its own
/// `result.json`; the scenario passes when every one does, and every one always runs.
pub fn run(mut run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    run.note(
        "One run per photograph, each in its own directory named for it with two launches: `gpu`, the ordinary editor, and `reference`, with `--no-gpu-render`. The default is the generated 24 MP JPEG (`cargo xtask generate-fixtures --output fixtures/generated`); `--source` names others, such as the 60 MP JPEG or a supplied RAW file.",
    );
    let mut failures = Vec::new();
    for source in sources {
        let name = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or("A minimap source has no name")?
            .to_owned();
        let shown = source
            .strip_prefix(run.root())
            .unwrap_or(&source)
            .to_string_lossy()
            .into_owned();
        let outcome = run
            .child(&name)
            .and_then(|child| smoke::launch_all(child, scenario, vec![source.clone()]));
        run.record_launches(&run.out().join(&name), json!({"source": shown}))?;
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

/// Both launches against their plans.
pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [gpu, reference] = launches else {
        return Err("The minimap scenario makes two launches".into());
    };
    run.sources_unchanged()?;
    verify_launch(gpu, &GPU_PLAN, false)?;
    verify_launch(reference, &REFERENCE_PLAN, true)
}

/// What a frame's minimap overview lifecycle has counted.
fn counters(frame: &Frame) -> Value {
    frame["state"]["surface"]["gpu"]["minimap_overview"]["lifecycle"].clone()
}

/// The lifecycle's work counters: builds, sweeps and uploads, which reuse never moves.
fn work(frame: &Frame) -> Result<[u64; 4]> {
    let lifecycle = counters(frame);
    let read = |name: &str| {
        lifecycle[name]
            .as_u64()
            .ok_or_else(|| format!("no overview {name} counter: {lifecycle}"))
    };
    Ok([
        read("gpu_builds")?,
        read("cpu_builds")?,
        read("sweeps")?,
        read("uploads")?,
    ])
}

/// The photograph's stage, oriented as the editor shows it: the source's own size at the first
/// percentage frame, where the stage is unedited.
fn photo_stage(launch: &Checked, rows: &[Row]) -> Result<(f64, f64)> {
    let first = rows
        .iter()
        .position(|row| row.name == "ready")
        .ok_or("no ready row")?;
    let minimap = &launch.frames[first]["state"]["minimap"];
    let stage = pair(&minimap["stage"]["size"], "first stage")?;
    let source = pair(
        &launch.frames[0]["state"]["source_dimensions"],
        "source dimensions",
    )?;
    ensure(
        (near(stage.0, source.0, 0.5) && near(stage.1, source.1, 0.5))
            || (near(stage.0, source.1, 0.5) && near(stage.1, source.0, 0.5)),
        format!("the unedited stage {stage:?} is not the source's {source:?}, turned or not"),
    )?;
    Ok(stage)
}

/// The largest square inside a `width` × `height` photograph turned by `degrees`, centred: its
/// corners turned back must stay inside, so its side is the shorter side over `cos + sin`.
fn straightened_square((width, height): (f64, f64), degrees: f64) -> f64 {
    let angle = degrees.to_radians();
    width.min(height) / (angle.cos() + angle.sin().abs())
}

fn verify_launch(launch: &Checked, rows: &[Row], reference: bool) -> Result {
    let (evidence, events) = (&launch.evidence, &launch.events);
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
    ensure(frames.len() == rows.len(), "A frame for every planned row")?;
    let photo = photo_stage(launch, rows)?;
    let square = straightened_square(photo, STRAIGHTEN_DEGREES);
    let mut checks = Checks::new();
    let mut stage = Stage::Photo;
    let mut display_scale: Option<f64> = None;
    let mut interface = 1.0;
    let mut grab: Option<Grab> = None;
    let mut last_ready: Option<u64> = None;
    let mut mode = POINTER_MODE;
    for (index, (row, frame)) in rows.iter().zip(frames).enumerate() {
        let what = format!(
            "{} step {:?} (frame {index})",
            launch.evidence.display(),
            row.name
        );
        let fail = |error: String| format!("{what}: {error}");
        let state = &frame["state"];
        ensure(
            state["phase"] == "ready",
            fail(format!("not a ready frame: phase {}", state["phase"])),
        )?;
        match row.action {
            Do::Straighten | Do::Return => stage = Stage::Square,
            Do::Switch => stage = Stage::Switched,
            Do::Interface(size) => interface = f64::from(size) / 100.0,
            Do::Mode(next) => mode = next,
            _ => {}
        }
        // The History step selecting the Original shows the photograph before its crop; nothing in
        // the journey selects an entry with one.
        let expected_stage = match stage {
            Stage::Photo => photo,
            Stage::Square => (square, square),
            Stage::Switched => SWITCH_SIZE,
        };
        let stage_slack = if stage == Stage::Square {
            SQUARE_SLACK
        } else {
            0.5
        };
        let view_zoom = &state["surface"]["view"]["zoom"];
        let percent = match view_zoom["mode"].as_str() {
            Some("fit") => None,
            Some("percent") => Some(
                view_zoom["value"]
                    .as_f64()
                    .ok_or_else(|| fail("no zoom value".into()))?,
            ),
            _ => return Err(fail(format!("no view zoom: {view_zoom}")).into()),
        };
        let minimap = &state["minimap"];
        // Select shows no photograph, so no minimap whatever the view's zoom.
        let shown = percent.is_some_and(|percent| percent >= MINIMAP_FROM)
            && row.action != Do::Workspace(SelectWorkspace::Select);
        ensure(
            minimap["shown"] == json!(shown),
            fail(format!(
                "the card is {} at {view_zoom}",
                if shown { "not shown" } else { "shown" }
            )),
        )?;
        if !shown {
            ensure(
                !matches!(row.action, Do::Gesture(_)),
                fail("a minimap step with no card".into()),
            )?;
            checks.note(
                frame,
                "no minimap",
                json!({"step": row.name, "zoom": view_zoom}),
            );
            continue;
        }
        let percent = percent.expect("shown at a percentage");
        let scale_factor = minimap["scale_factor"]
            .as_f64()
            .ok_or_else(|| fail("no scale factor".into()))?;
        // The display's own scale, read once with the interface at 100%; the interface size
        // multiplies it.
        let display = *display_scale.get_or_insert(scale_factor / interface);
        ensure(
            near(scale_factor, display * interface, 1e-4),
            fail(format!(
                "the scale factor is {scale_factor}, the display's {display} at an interface of {interface}"
            )),
        )?;
        let [left, top, right, bottom]: [f64; 4] =
            serde_json::from_value(frame["canvas_rect"].clone())
                .map_err(|_| fail("no canvas rectangle".into()))?;
        let canvas = [
            left / scale_factor,
            top / scale_factor,
            (right - left) / scale_factor,
            (bottom - top) / scale_factor,
        ];
        let recorded = rect(&minimap["canvas"], "minimap canvas")?;
        ensure(
            canvas.iter().zip(recorded).all(|(a, b)| near(*a, b, 1.0)),
            fail(format!(
                "the card's canvas is {recorded:?}, the frame's {canvas:?}"
            )),
        )?;
        let size = pair(&minimap["stage"]["size"], "stage size")?;
        ensure(
            near(size.0, expected_stage.0, stage_slack)
                && near(size.1, expected_stage.1, stage_slack),
            fail(format!(
                "the stage is {size:?}, expected {expected_stage:?} ({stage:?})"
            )),
        )?;
        let surface_stage = pair(&state["surface"]["dimensions"], "presented dimensions")
            .ok()
            .or_else(|| pair(&state["preview_dimensions"], "preview dimensions").ok());
        // The card's stage, which the checks below read, is the one the photograph's surface draws.
        let stage_size = size;
        let pan = pair(&minimap["pan"], "card pan")?;
        let view = View {
            stage: stage_size,
            scale: percent / 100.0 / scale_factor,
            canvas,
            pan,
        };
        let scale = minimap["scale"]
            .as_f64()
            .ok_or_else(|| fail("no scale".into()))?;
        ensure(
            near(scale, view.scale, 1e-5),
            fail(format!(
                "the card's scale is {scale}, {percent}% at {scale_factor} is {}",
                view.scale
            )),
        )?;
        // Placement: the stage fitted into the image area, the card hugging it at the canvas's
        // lower right.
        let area = (
            MINIMAP_AREA.0.min(MINIMAP_SHARE * canvas[2]),
            MINIMAP_AREA.1.min(MINIMAP_SHARE * canvas[3]),
        );
        let fit = (area.0 / stage_size.0).min(area.1 / stage_size.1);
        let thumbnail = (stage_size.0 * fit, stage_size.1 * fit);
        let card = rect(&minimap["card"], "card")?;
        let image_rect = rect(&minimap["image"], "image")?;
        let card_right = canvas[0] + canvas[2] - MINIMAP_INSET;
        let card_bottom = canvas[1] + canvas[3] - MINIMAP_INSET;
        ensure(
            near(image_rect[2], thumbnail.0, GEOMETRY_SLACK)
                && near(image_rect[3], thumbnail.1, GEOMETRY_SLACK)
                && near(card[2], thumbnail.0 + 2.0 * MINIMAP_PADDING, GEOMETRY_SLACK)
                && near(card[3], thumbnail.1 + 2.0 * MINIMAP_PADDING, GEOMETRY_SLACK)
                && near(card[0] + card[2], card_right, GEOMETRY_SLACK)
                && card[1] + card[3] <= card_bottom + GEOMETRY_SLACK
                && near(image_rect[0], card[0] + MINIMAP_PADDING, GEOMETRY_SLACK)
                && near(image_rect[1], card[1] + MINIMAP_PADDING, GEOMETRY_SLACK),
            fail(format!(
                "the card {card:?} and image {image_rect:?} are not a {thumbnail:?} thumbnail padded {MINIMAP_PADDING} and inset {MINIMAP_INSET} in canvas {canvas:?}"
            )),
        )?;
        let shows = minimap["state"]
            .as_str()
            .ok_or_else(|| fail("no state".into()))?;
        let navigable = minimap["navigable"] == json!(true);
        let visible = view.visible();
        let expected_rectangle = [
            image_rect[0] + visible[0] / stage_size.0 * image_rect[2],
            image_rect[1] + visible[1] / stage_size.1 * image_rect[3],
            (visible[2] - visible[0]) / stage_size.0 * image_rect[2],
            (visible[3] - visible[1]) / stage_size.1 * image_rect[3],
        ];
        let rectangle = if navigable {
            let rectangle = rect(&minimap["rectangle"], "rectangle")?;
            ensure(
                rectangle
                    .iter()
                    .zip(expected_rectangle)
                    .all(|(a, b)| near(*a, b, GEOMETRY_SLACK)),
                fail(format!(
                    "the rectangle is {rectangle:?}, the pan shows {expected_rectangle:?}"
                )),
            )?;
            Some(rectangle)
        } else {
            ensure(
                minimap["rectangle"].is_null() && matches!(shows, "pending" | "unavailable"),
                fail(format!(
                    "a card {shows} that cannot navigate shows a rectangle"
                )),
            )?;
            None
        };
        if matches!(shows, "ready" | "updating") {
            let overview = pair(&minimap["overview"]["stage"], "overview stage")?;
            ensure(
                near(overview.0, stage_size.0, 0.5) && near(overview.1, stage_size.1, 0.5),
                fail(format!(
                    "the overview shown is of a {overview:?} stage, not this {stage_size:?} one"
                )),
            )?;
        }
        // A fitting axis: the stage is drawn whole and centred, and the rectangle covers it.
        let fits = (
            stage_size.0 * view.scale <= canvas[2],
            stage_size.1 * view.scale <= canvas[3],
        );
        if let Some(rectangle) = rectangle {
            if fits.0 {
                ensure(
                    near(rectangle[0], image_rect[0], GEOMETRY_SLACK)
                        && near(rectangle[2], image_rect[2], GEOMETRY_SLACK),
                    fail("a fitting axis's rectangle is not the whole image".into()),
                )?;
            }
            if fits.1 {
                ensure(
                    near(rectangle[1], image_rect[1], GEOMETRY_SLACK)
                        && near(rectangle[3], image_rect[3], GEOMETRY_SLACK),
                    fail("a fitting axis's rectangle is not the whole image".into()),
                )?;
            }
        }

        // A gesture: the pan worked here from the point it took, held by the session too.
        let mut gesture = Value::Null;
        if let Do::Gesture(step) = row.action
            && step != MinimapStep::Ready
        {
            gesture = check_gesture(frame, &frames[index - 1], step, &view, &mut grab)
                .map_err(|error| fail(error.to_string()))?;
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
                    edited(&state[field]) == edited(&previous[field]),
                    fail(format!(
                        "a minimap gesture changed {field}: {} became {}",
                        previous[field], state[field]
                    )),
                )?;
            }
        }

        // Ready: the stage's own overview, newer than the last one ready where the content
        // changed since.
        let version = minimap["overview"]["version"].as_u64();
        if row.action == Do::Gesture(MinimapStep::Ready) {
            ensure(
                shows == "ready" && navigable,
                fail(format!("a ready step's card shows {shows}")),
            )?;
            if row.fresh
                && let Some(last) = last_ready
            {
                ensure(
                    version != Some(last),
                    fail(format!(
                        "the overview is still version {last} after the content changed"
                    )),
                )?;
            }
            last_ready = version;
        }

        // Reuse: a view, layout or navigation change with a ready overview before and after does
        // no overview work.
        let reuses = matches!(
            row.action,
            Do::View(_)
                | Do::ApiZoom(_)
                | Do::Pinch(_)
                | Do::Gesture(_)
                | Do::Panels { .. }
                | Do::Strip
                | Do::Window(_)
                | Do::Interface(_)
        );
        let before = &frames[index - 1];
        let was_ready = before["state"]["minimap"]["state"] == "ready";
        let mut reused = Value::Null;
        if reuses && was_ready && shows == "ready" {
            let (then, now) = (work(before)?, work(frame)?);
            ensure(
                then == now,
                fail(format!(
                    "a {:?} with the overview ready did overview work: builds, sweeps and uploads {then:?} became {now:?}",
                    row.action
                )),
            )?;
            reused = json!(true);
        }
        // Hidden: no overview work while the window is hidden, and shown again the overview it
        // held, with none made.
        if row.name == "hidden-wait" || row.name == "reshown" {
            ensure(
                work(before)? == work(frame)?
                    && shows == "ready"
                    && version == before["state"]["minimap"]["overview"]["version"].as_u64(),
                fail(format!(
                    "the window hidden and shown again did overview work or changed the overview: {} became {}",
                    counters(before),
                    counters(frame)
                )),
            )?;
        }
        // The reference launch's overviews are all the reference's.
        if reference {
            let lifecycle = counters(frame);
            ensure(
                lifecycle["gpu_builds"] == 0 && lifecycle["sweeps"] == 0,
                fail(format!(
                    "the GPU made an overview with its stage refused: {lifecycle}"
                )),
            )?;
            if shows == "ready" {
                ensure(
                    lifecycle["settled"]["renderer"] == "reference",
                    fail(format!(
                        "the ready overview is not the reference's: {lifecycle}"
                    )),
                )?;
            }
        }

        // The thumbnail and the photograph agree, wherever both show a plain canvas.
        let agreement = if shows == "ready" && mode == POINTER_MODE {
            agree(frame, &view, image_rect, scale_factor)
                .map_err(|error| fail(error.to_string()))?
        } else {
            Value::Null
        };
        checks.note(
            frame,
            "the minimap's placement, stage, rectangle, gesture, overview and its agreement with the photograph",
            json!({
                "step": row.name,
                "action": format!("{:?}", row.action),
                "zoom": percent,
                "scale_factor": scale_factor,
                "stage": [stage_size.0, stage_size.1],
                "expected_stage": [expected_stage.0, expected_stage.1],
                "surface_stage": surface_stage.map(|(w, h)| [w, h]),
                "fits": [fits.0, fits.1],
                "state": shows,
                "card": card,
                "image": image_rect,
                "rectangle": rectangle,
                "expected_rectangle": expected_rectangle,
                "pan": [pan.0, pan.1],
                "session_pan": session_pan(frame).ok().map(|(x, y)| [x, y]),
                "gesture": gesture,
                "overview": minimap["overview"],
                "lifecycle": counters(frame),
                "reused": reused,
                "agreement": agreement,
            }),
        );
    }
    // Select let the overview go, and Develop again made it anew.
    if rows.iter().any(|row| row.name == "select") {
        let (select, recovered) = (launch.index("select")?, launch.index("recovered")?);
        let between = |from: usize, to: usize| {
            let captured: Vec<usize> = events
                .iter()
                .enumerate()
                .filter(|(_, event)| event["event"] == "frame_captured")
                .map(|(index, _)| index)
                .collect();
            events[captured[from - 1] + 1..=captured[to]]
                .iter()
                .filter(|event| event["event"] == "overview_released")
                .count()
        };
        let released = between(select, select);
        ensure(
            released >= 1,
            format!("{}: Select let no overview go", launch.evidence.display()),
        )?;
        let (then, now) = (work(&frames[select])?, work(&frames[recovered])?);
        ensure(
            now.iter().zip(then).any(|(now, then)| *now > then),
            format!(
                "{}: Develop again showed an overview it did not make: {then:?} then {now:?}",
                launch.evidence.display()
            ),
        )?;
        checks.note(
            &frames[recovered],
            "the overview Select let go, made again",
            json!({"released_at_select": released, "select": counters(&frames[select]),
                "recovered": counters(&frames[recovered])}),
        );
    }
    checks.write(
        evidence,
        "minimap",
        json!({
            "renderer": if reference { "reference (--no-gpu-render)" } else { "ordinary" },
            "photo_stage": [photo.0, photo.1],
            "straightened_square": square,
            "switched_stage": [SWITCH_SIZE.0, SWITCH_SIZE.1],
            "geometry_slack_points": GEOMETRY_SLACK,
            "pan_slack_points": PAN_SLACK,
            "agreement_codes": AGREEMENT,
            "agreement_share": AGREEMENT_SHARE,
            "scope": "Minimap placement, stage, rectangle, navigation, lifecycle counters and the thumbnail's agreement with the photograph, read back from the window renderer and the editor's state; gestures are mouse events routed through the laid-out widgets, not the OS pointer; not display scanout, colour calibration or timing",
        }),
    )
}

/// What a record says of the edit, less the Masks panel's row thumbnails, which arrive on their
/// own after the mask is made.
fn edited(record: &Value) -> Value {
    let mut record = record.clone();
    if let Some(masks) = record.get_mut("masks").and_then(Value::as_array_mut) {
        for mask in masks {
            if let Some(mask) = mask.as_object_mut() {
                mask.remove("thumbnail");
            }
        }
    }
    record
}

/// A minimap drag the journey holds: the stage point the press took and the pan it left.
#[derive(Clone, Copy, Debug)]
struct Grab {
    at: (f64, f64),
    pan: (f64, f64),
}

/// A gesture's pan against the one worked here from the point it took, and the session's.
fn check_gesture(
    frame: &Frame,
    previous: &Frame,
    step: MinimapStep,
    view: &View,
    grab: &mut Option<Grab>,
) -> Result<Value> {
    let record = &frame["step"];
    let before = &record["minimap_before"];
    let image = rect(&before["image"], "image before the gesture")?;
    let rectangle = rect(&before["rectangle"], "rectangle before the gesture")?;
    let stage = view.stage;
    let from = match &previous["state"]["minimap"]["pan"] {
        Value::Null => session_pan(previous)?,
        card => pair(card, "card pan before")?,
    };
    let was = View {
        pan: from,
        stage,
        scale: view.scale,
        canvas: view.canvas,
    };
    let from = was.clamp(from);
    let stage_at = |point: (f64, f64)| {
        (
            (point.0 - image[0]) / image[2] * stage.0,
            (point.1 - image[1]) / image[3] * stage.1,
        )
    };
    let contains = |[x, y, w, h]: [f64; 4], (px, py): (f64, f64)| {
        px >= x && px < x + w && py >= y && py < y + h
    };
    let point = pair(&record["window_point"], "window point")?;
    let expected = match step {
        MinimapStep::Press(_) | MinimapStep::Click(_) => {
            let at = stage_at(point);
            let expected = if !contains(image, point) || contains(rectangle, point) {
                from
            } else {
                was.centred(at)
            };
            *grab = match step {
                MinimapStep::Press(_) if contains(image, point) => Some(Grab { at, pan: expected }),
                _ => None,
            };
            expected
        }
        MinimapStep::Move(_) | MinimapStep::Release(_) => {
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
            expected
        }
        other => return Err(format!("the journey makes no {other:?} gesture").into()),
    };
    let pan = view.pan;
    let session = session_pan(frame)?;
    ensure(
        near(pan.0, expected.0, PAN_SLACK) && near(pan.1, expected.1, PAN_SLACK),
        format!("the gesture left the pan at {pan:?}, worked here {expected:?} from {from:?}"),
    )?;
    ensure(
        near(session.0, expected.0, PAN_SLACK) && near(session.1, expected.1, PAN_SLACK),
        format!("the session's pan is {session:?}, the gesture's {expected:?}"),
    )?;
    ensure(
        frame["state"]["minimap"]["dragging"] == json!(grab.is_some()),
        format!(
            "dragging is {}, the journey holds {grab:?}",
            frame["state"]["minimap"]["dragging"]
        ),
    )?;
    let routed: Vec<String> =
        serde_json::from_value(record["routed_messages"].clone()).unwrap_or_default();
    let foreign: Vec<&String> = routed
        .iter()
        .filter(|message| !message.starts_with("View(Minimap"))
        .collect();
    ensure(
        foreign.is_empty(),
        format!("events routed onto the card published {foreign:?}"),
    )?;
    Ok(json!({
        "step": format!("{step:?}"),
        "window_point": [point.0, point.1],
        "from": [from.0, from.1],
        "expected": [expected.0, expected.1],
        "pan": [pan.0, pan.1],
        "session": [session.0, session.1],
        "routed": routed,
    }))
}

fn linear(code: u8) -> f64 {
    luxforge_reference::srgb::decode(code)
}

fn encoded(value: f64) -> f64 {
    255.0 * luxforge_reference::srgb::encode_clamped(value)
}

/// The mean colour of `capture` over `[x0, y0, x1, y1]` in physical pixels, averaged in linear
/// light and encoded again, and how many pixels it took.
fn mean(capture: &image::RgbImage, [x0, y0, x1, y1]: [f64; 4]) -> ([f64; 3], u64) {
    let (width, height) = capture.dimensions();
    let clampx = |v: f64| (v.max(0.0) as u32).min(width);
    let clampy = |v: f64| (v.max(0.0) as u32).min(height);
    let mut sum = [0.0; 3];
    let mut count = 0u64;
    for y in clampy(y0.ceil())..clampy(y1.floor()) {
        for x in clampx(x0.ceil())..clampx(x1.floor()) {
            let pixel = capture.get_pixel(x, y).0;
            for channel in 0..3 {
                sum[channel] += linear(pixel[channel]);
            }
            count += 1;
        }
    }
    let n = count.max(1) as f64;
    (
        [
            encoded(sum[0] / n),
            encoded(sum[1] / n),
            encoded(sum[2] / n),
        ],
        count,
    )
}

/// The thumbnail and the photograph over the middle of the visible region: the same stage region
/// placed on the canvas by the frame's own zoom and pan, and on the thumbnail by its image area,
/// each averaged in linear light, within [`AGREEMENT`] codes per channel. A region the card covers
/// on the canvas, or the mode strip floats over, is not compared, and says so.
fn agree(frame: &Frame, view: &View, image: [f64; 4], scale_factor: f64) -> Result<Value> {
    let visible = view.visible();
    let inset = (1.0 - AGREEMENT_SHARE) / 2.0;
    let region = [
        visible[0] + (visible[2] - visible[0]) * inset,
        visible[1] + (visible[3] - visible[1]) * inset,
        visible[2] - (visible[2] - visible[0]) * inset,
        visible[3] - (visible[3] - visible[1]) * inset,
    ];
    // The canvas: the stage's origin in logical pixels is the canvas's corner, plus the centring
    // of a fitting axis, less the pan.
    let pan = view.clamp(view.pan);
    let origin = (
        view.canvas[0] + ((view.canvas[2] - view.stage.0 * view.scale) / 2.0).max(0.0) - pan.0,
        view.canvas[1] + ((view.canvas[3] - view.stage.1 * view.scale) / 2.0).max(0.0) - pan.1,
    );
    let on_canvas = [
        (origin.0 + region[0] * view.scale) * scale_factor,
        (origin.1 + region[1] * view.scale) * scale_factor,
        (origin.0 + region[2] * view.scale) * scale_factor,
        (origin.1 + region[3] * view.scale) * scale_factor,
    ];
    let on_thumbnail = [
        (image[0] + region[0] / view.stage.0 * image[2]) * scale_factor,
        (image[1] + region[1] / view.stage.1 * image[3]) * scale_factor,
        (image[0] + region[2] / view.stage.0 * image[2]) * scale_factor,
        (image[1] + region[3] / view.stage.1 * image[3]) * scale_factor,
    ];
    let card = card_pixels(frame)?;
    let canvas_bottom = (view.canvas[1] + view.canvas[3]) * scale_factor;
    let covered = card.is_some_and(|[l, t, r, b]| {
        on_canvas[0] < r && on_canvas[2] > l && on_canvas[1] < b && on_canvas[3] > t
    }) || on_canvas[3] > canvas_bottom - STRIP_INSET;
    if covered {
        return Ok(
            json!({"compared": false, "why": "the card or the mode strip covers the region on the canvas",
            "region": region, "canvas": on_canvas}),
        );
    }
    let capture = frame.image()?;
    let (photo, photo_pixels) = mean(capture, on_canvas);
    let (thumbnail, thumbnail_pixels) = mean(capture, on_thumbnail);
    // At a high zoom the region is a few thumbnail pixels, too few for a mean to stand for it.
    if thumbnail_pixels < MIN_THUMBNAIL_PIXELS {
        return Ok(
            json!({"compared": false, "why": "the region covers too few thumbnail pixels",
            "region": region, "pixels": [photo_pixels, thumbnail_pixels]}),
        );
    }
    let difference = (0..3)
        .map(|channel| (photo[channel] - thumbnail[channel]).abs())
        .fold(0.0, f64::max);
    ensure(
        difference <= AGREEMENT,
        format!(
            "the thumbnail's mean {thumbnail:.1?} over the middle of the visible region is {difference:.1} codes from the photograph's {photo:.1?}"
        ),
    )?;
    Ok(json!({
        "compared": true,
        "region": region,
        "canvas": on_canvas,
        "thumbnail": on_thumbnail,
        "photo_mean": photo,
        "thumbnail_mean": thumbnail,
        "difference": difference,
        "pixels": [photo_pixels, thumbnail_pixels],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each launch's script is one step per planned frame after the open, within the script limit.
    #[test]
    fn each_script_is_one_step_per_planned_frame() {
        let sources = [PathBuf::from("/photo.jpg")];
        for (plan, rows) in [
            (gpu_plan(&sources), GPU_PLAN.len()),
            (reference_plan(&sources), REFERENCE_PLAN.len()),
        ] {
            assert_eq!(plan.len(), rows);
            let script = plan.script();
            let steps = script.as_array().expect("an array");
            assert_eq!(steps.len(), rows - 1);
            assert!(steps.len() <= script::MAX_SCRIPT_STEPS);
        }
        let script = gpu_plan(&sources).script();
        let at = |name: &str| GPU_PLAN.iter().position(|row| row.name == name).unwrap() - 1;
        assert_eq!(script[at("ready")], json!({"minimap": "ready"}));
        assert_eq!(
            script[at("small-window")],
            json!({"window_size": {"width": 1000.0, "height": 700.0}})
        );
        assert_eq!(
            script[at("return")],
            json!({"open": {"path": "/photo.jpg"}})
        );
    }

    /// The straightened square of a 6000 × 4000 photograph at 6° is the short side over cos + sin.
    #[test]
    fn the_straightened_square_fits_the_turned_photograph() {
        let side = straightened_square((6000.0, 4000.0), 6.0);
        assert!(
            (side - 4000.0 / (0.994_521_9 + 0.104_528_5)).abs() < 1e-3,
            "{side}"
        );
        // Its corners, turned back by the angle, stay inside the photograph.
        let angle = 6f64.to_radians();
        let half = side / 2.0;
        let corner = half * angle.cos() + half * angle.sin();
        assert!(corner <= 2000.0 + 1e-9);
    }

    /// Linear light round-trips through the encoding.
    #[test]
    fn the_encoding_round_trips() {
        for code in [0u8, 1, 10, 128, 254, 255] {
            assert!(
                (encoded(linear(code)) - f64::from(code)).abs() < 1e-6,
                "{code}"
            );
        }
    }
}
