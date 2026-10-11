//! The `corrections` smoke scenario: the Corrections workspace's journeys
//! (`docs/design/corrections-workspace.md#acceptance`) on the real editor, background-only, with
//! correlated state, log and capture.
//!
//! Launch 1 enters the mode with `Q` and adds, one entry each, a spot with an automatic source, a
//! painted stroke, a `⇧`-click straight stroke, a live-painted Clone after an `⌥`-click (captured
//! with the button still down, the repair drawn through the draft) and a `⌘`-drag; walks the Escape
//! order; selects by pin and by row; drags a pin, a source and a spot's size handle; asks `/` for a
//! new source; reorders with `⌥↑` and by dragging a row; hides and shows the pins with `H`; deletes
//! with `⌫`; meets the no-source notice and finishes the held stroke with `⌥`-click; stacks a Clone
//! whose source is an earlier repair; previews history; exports a JPEG; and leaves. Launch 2
//! reopens the catalog, shows the photograph's rendered preview in Select, removes it from the
//! library and puts it back, and returns to Develop with its corrections intact. Launches 3 and 4
//! fill a fresh photograph to the declared limit from an agent and show the press it refuses.
//! Launch 5 reopens launch 1's catalog (as launch 2 did) and checks the overlay through a 16:9 crop, a straighten and
//! a Perspective warp: after each, the core's own `render.transform` places every pin, the
//! desktop's retained map is held to it, and the capture with the pins shown is held against the
//! capture with them hidden, whose changed pixels must centre on the places the core puts the pins
//! and, for a selected spot, around the outline the core's map warps to. Launches 6 to 8 are the
//! Locate journey: a photograph developed from a scratch copy gets a Clone, the copy is moved
//! while the editor is not running, Select's Missing originals locates it at the moved file (the
//! same bytes), and a last launch opens it there to find the Clone intact and rendered.
//!
//! The fixture is the four flat quadrants, so what a repair does is legible in the capture: a
//! Clone from the gold quadrant into the blue one turns that patch gold, and a Clone whose source
//! is that repaired patch turns its destination gold rather than blue — the stacking principle,
//! read from pixels.
use crate::{
    resolve_missing_smoke::{ask, settle, with_owner},
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance},
    smoke::{APP, LaunchSpec, Scenario, launch_of, launch_planned_then},
    *,
};
use luxforge_evidence::{
    self as script, CatalogStep, CorrectionDrag, CorrectionHandle, CorrectionStep, ExportFile,
    ExportStep, MissingStep, PressModifier, PressStep, PreviewStep, SelectStep, SelectWorkspace,
};

pub const SCENARIO: &str = "corrections";
/// The golden four-quadrant fixture, unrotated: content and output coordinates coincide, so the
/// normalized positions the script presses at are the ones the capture is measured at.
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
pub const NOTE: &str = "Eight launches. The first walks every Corrections gesture and key on one photograph and exports it; the second reopens its catalog, shows the rendered preview in Select, removes and puts back the photograph and returns to Develop; the third and fourth fill a fresh photograph to the declared limit from an agent and press once more; the fifth reopens the first's catalog and checks the overlay through a crop, a straighten and a Perspective warp against the core's own render.transform and the capture with the pins hidden. The sixth develops a copy of the fixture the run makes in locate/original and adds a Clone; the run then moves that copy to locate/moved and records it missing with source.check; the seventh locates it there from Select's Missing originals; the eighth opens it at the new place.";

pub(crate) const MODULE: &str = "luxforge.corrections";
pub const CORRECTIONS_EFFECT: &str = "luxforge.corrections.repair";

/// The Size rail positions the script sets: a radius of 0.04 content heights (26 px across on the
/// 320 px fixture), and the top of the range, which leaves no candidate inside the frame.
pub(crate) const SIZE: f64 = 0.7051;
const HUGE: f64 = 1.0;

/// Where things are, in normalized content coordinates.
const SPOT: [f64; 2] = [0.30, 0.30];
const STROKE: [[f64; 2]; 3] = [[0.62, 0.30], [0.72, 0.30], [0.82, 0.30]];
const STRAIGHT_END: [f64; 2] = [0.82, 0.40];
pub(crate) const GOLD_SOURCE: [f64; 2] = [0.75, 0.80];
/// The live-painted Clone: from the gold quadrant into the blue one.
pub(crate) const CLONE_FROM: [f64; 2] = [0.25, 0.80];
pub(crate) const CLONE_TO: [[f64; 2]; 2] = [[0.30, 0.80], [0.35, 0.80]];
/// A patch inside the Clone's destination, well within its full coverage and clear of the arrow
/// drawn along `y = 0.8` while it is selected.
pub(crate) const CLONED: [f64; 2] = [0.29, 0.812];
const PULL_AT: [f64; 2] = [0.40, 0.25];
const PULL_TO: [[f64; 2]; 2] = [[0.55, 0.50], [0.70, 0.75]];
/// The stacked Clone: its source is the earlier Clone's repaired patch, its destination is green.
const STACK_SOURCE: [f64; 2] = [0.27, 0.80];
const STACK_AT: [f64; 2] = [0.70, 0.42];
/// A patch inside the stacked spot's full coverage, clear of its pin ring and of the arrow that
/// reaches the pin from the lower left.
const STACKED: [f64; 2] = [0.70 + 3.5 / 480.0, 0.42 - 3.5 / 320.0];

pub(crate) const PATCH_HALF: i64 = 3;
/// How close a repaired patch's mean must be to its source colour, per channel, in 8-bit codes:
/// the flat quadrants are JPEG-compressed, so a margin, not a tuned threshold.
pub(crate) const NEAR: f64 = 30.0;
const GOLD: [f64; 3] = [234.0, 195.0, 30.0];
const BLUE: [f64; 3] = [40.0, 70.0, 220.0];

pub(crate) fn key(key: &str) -> script::Step {
    script::Step::Key { key: key.into() }
}

pub(crate) fn press(at: [f64; 2]) -> PressStep {
    PressStep {
        at,
        to: Vec::new(),
        modifiers: Vec::new(),
        release: true,
        interval_ms: None,
    }
}

pub(crate) fn correction(step: CorrectionStep) -> script::Step {
    step.into()
}

pub(crate) fn add(at: [f64; 2]) -> script::Step {
    correction(CorrectionStep::Press(press(at)))
}

pub(crate) fn with(at: [f64; 2], modifier: PressModifier) -> script::Step {
    correction(CorrectionStep::Press(PressStep {
        modifiers: vec![modifier],
        ..press(at)
    }))
}

fn drag(handle: CorrectionHandle, to: &[[f64; 2]]) -> script::Step {
    correction(CorrectionStep::Drag(CorrectionDrag {
        handle,
        to: to.to_vec(),
        interval_ms: None,
    }))
}

pub(crate) fn field(name: &str, fraction: f64) -> script::Step {
    correction(CorrectionStep::Field {
        name: name.into(),
        fraction,
    })
}

/// Launch 1: every gesture and key of the workspace.
pub fn launch1(_: &[PathBuf]) -> Plan {
    let mut pulled = press(PULL_AT);
    pulled.modifiers = vec![PressModifier::Command];
    pulled.to = PULL_TO.to_vec();
    let mut painted = press(STROKE[0]);
    painted.to = STROKE[1..].to_vec();
    let mut held = press(CLONE_FROM);
    held.to = vec![CLONE_TO[0]];
    held.release = false;
    Plan::new(vec![
        Step::opened("opened")
            .no_layer(CORRECTIONS_EFFECT)
            .no_draft(),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
        Step::new("size", field("radius", SIZE)).commits(0),
        // A click away from every pin: a spot with an automatic source, one entry, selected.
        Step::new("spot", add(SPOT))
            .commits(1)
            .label("Add Heal 1")
            .status_starts("Added Heal 1 \u{b7} source picked"),
        // A drag paints a stroke.
        Step::new("stroke", correction(CorrectionStep::Press(painted)))
            .commits(1)
            .label("Add Heal 2"),
        // `⇧`-click: a straight stroke from where the last stroke ended.
        Step::new("straight", with(STRAIGHT_END, PressModifier::Shift))
            .commits(1)
            .label("Add Heal 3"),
        // `⌥`-click sets the source and writes nothing; Clone is chosen for the next press.
        Step::new("source-set", with(GOLD_SOURCE, PressModifier::Alt))
            .commits(0)
            .status_starts("Source set"),
        Step::new(
            "clone-kind",
            correction(CorrectionStep::Kind("clone".into())),
        )
        .commits(0),
        // The Clone painted with the button still down: the live repair through the draft.
        Step::new("clone-held", correction(CorrectionStep::Press(held)))
            .commits(0)
            .status_starts("Clone 4 draft"),
        Step::new(
            "clone-released",
            correction(CorrectionStep::Release(CLONE_TO[1])),
        )
        .commits(1)
        .label("Add Clone 4")
        .no_draft(),
        // Escape, one level at a time: deselect, then clear the set source.
        Step::new("escape-deselect", key("Escape")).commits(0),
        Step::new("escape-source", key("Escape"))
            .commits(0)
            .status_starts("Corrections \u{b7} Clone"),
        // `⌘`-drag: a spot at the press, its source pulled out to the release.
        Step::new("pulled", correction(CorrectionStep::Press(pulled)))
            .commits(1)
            .label("Add Clone 5"),
        // Selection by pin and by row. Rows are the stack top first.
        Step::new("by-pin", correction(CorrectionStep::Pin(4)))
            .commits(0)
            .status_starts("Heal 1 selected"),
        Step::new("by-row", correction(CorrectionStep::Row(Some(3))))
            .commits(0)
            .status_starts("Heal 2 selected"),
        Step::new("reselect", correction(CorrectionStep::Row(Some(4)))).commits(0),
        // The selected correction's handles, one entry each.
        Step::new(
            "pin-drag",
            drag(CorrectionHandle::Pin, &[[0.31, 0.31], [0.32, 0.32]]),
        )
        .commits(1)
        .label("Move Heal 1"),
        Step::new(
            "source-drag",
            drag(CorrectionHandle::Source, &[[0.38, 0.20], [0.40, 0.18]]),
        )
        .commits(1)
        .label("Move Heal 1 source"),
        Step::new(
            "resize",
            drag(CorrectionHandle::Size, &[[0.32 + 0.06 / 1.5, 0.32]]),
        )
        .commits(1)
        .label("Resize Heal 1"),
        // `/`: the next-ranked automatic source.
        Step::new("new-source", key("/"))
            .commits(1)
            .label("New source Heal 1"),
        // `⌥↑` moves it up the stack; a row dragged to the bottom moves another.
        Step::new("option-up", key("Option+ArrowUp"))
            .commits(1)
            .label("Reorder Heal 1"),
        Step::new(
            "row-drag",
            correction(CorrectionStep::Reorder { row: 0, to: 4 }),
        )
        .commits(1)
        .label("Reorder Clone 5"),
        // `H` hides the pins and outlines, and shows them again.
        Step::new("hidden", key("h"))
            .commits(0)
            .status_starts("Pins and outlines hidden"),
        Step::new("shown", key("h")).commits(0),
        // `⌫` deletes the selected correction.
        Step::new("deleted", key("Backspace"))
            .commits(1)
            .label("Delete Heal 1")
            .status_starts("Deleted Heal 1"),
        // No automatic source fits a quarter-frame brush: the stroke stays drafted under the
        // notice, and an `⌥`-click finishes it with that source.
        Step::new("heal-kind", correction(CorrectionStep::Kind("heal".into()))).commits(0),
        Step::new("huge", field("radius", HUGE)).commits(0),
        Step::new("no-source", add([0.5, 0.5]))
            .commits(0)
            .notice("No automatic source fits"),
        Step::new("finished", with([0.25, 0.30], PressModifier::Alt))
            .commits(1)
            .label("Add Heal 6"),
        Step::new("clear-source", correction(CorrectionStep::SourceAuto)).commits(0),
        Step::new("size-again", field("radius", SIZE)).commits(0),
        // A Clone whose source is the earlier Clone's repaired patch.
        Step::new(
            "stack-kind",
            correction(CorrectionStep::Kind("clone".into())),
        )
        .commits(0),
        Step::new("stack-source", with(STACK_SOURCE, PressModifier::Alt)).commits(0),
        Step::new("stacked", add(STACK_AT))
            .commits(1)
            .label("Add Clone 7"),
        // The history before the first Clone, then the current state again.
        Step::new("history-before-clone", PreviewStep::Sequence(3)).commits(0),
        Step::new("history-current", PreviewStep::Current).commits(0),
        // A real JPEG export of the current state.
        Step::new(
            "exported",
            ExportStep::File(ExportFile {
                name: "corrections.jpg".into(),
                keep_metadata: false,
                reference: false,
                background: false,
            }),
        )
        .commits(0),
        // Escape twice (the set source, then the mode) leaves Corrections.
        Step::new("escape-clear", key("Escape")).commits(0),
        Step::new("escape-leave", key("Escape"))
            .commits(0)
            .mode("pointer"),
    ])
}

/// Launch 2: the reopened catalog, the rendered preview in Select, Remove and Put back, and the
/// corrections back in Develop.
pub fn launch2(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("reopened").label("Add Clone 7"),
        Step::new("select", SelectStep::Switch(SelectWorkspace::Select)).commits(0),
        Step::new("all", SelectStep::Source("All photographs".into())).commits(0),
        Step::new(
            "selected",
            SelectStep::Click {
                position: 0,
                shift: false,
                command: false,
            },
        )
        .commits(0),
        Step::new("remove-asked", script::Step::Catalog(CatalogStep::Remove)).commits(0),
        Step::new("removed", script::Step::Catalog(CatalogStep::Confirm))
            .commits(0)
            .status_starts("Removed "),
        Step::new("removed-view", SelectStep::Source("Removed".into())).commits(0),
        Step::new(
            "removed-selected",
            SelectStep::Click {
                position: 0,
                shift: false,
                command: false,
            },
        )
        .commits(0),
        Step::new("put-back", script::Step::Catalog(CatalogStep::PutBack))
            .commits(0)
            .status_starts("Put back "),
        Step::new("develop", SelectStep::Switch(SelectWorkspace::Develop)).commits(0),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
    ])
}

/// How many corrections launch 3 adds from an agent; launch 4 adds the rest of the limit.
const FILL_FIRST: usize = 60;
const LIMIT: usize = 100;

fn agent_add(index: usize) -> script::Step {
    let x = 0.05 + 0.009 * (index % 100) as f64;
    script::Step::agent(
        "edit.add-correction",
        json!({
            "points": [[x, 0.5]],
            "radius": 0.002,
            "source-x": x,
            "source-y": 0.2,
        }),
    )
}

/// Launch 3: a fresh photograph filled with corrections from an agent.
pub fn launch3(_: &[PathBuf]) -> Plan {
    let mut steps = vec![Step::opened("opened").no_layer(CORRECTIONS_EFFECT)];
    for index in 0..FILL_FIRST {
        steps.push(
            Step::new(format!("agent-{index}"), agent_add(index))
                .commits(1)
                .label(format!("Add Heal {}", index + 1)),
        );
    }
    Plan::new(steps)
}

/// Launch 4: the rest of the limit, then a press, refused before any draft.
pub fn launch4(_: &[PathBuf]) -> Plan {
    let mut steps = vec![Step::opened("reopened").label(format!("Add Heal {FILL_FIRST}"))];
    for index in FILL_FIRST..LIMIT {
        steps.push(
            Step::new(format!("agent-{index}"), agent_add(index))
                .commits(1)
                .label(format!("Add Heal {}", index + 1)),
        );
    }
    steps.push(Step::new("mode", key("q")).commits(0).mode(MODULE));
    steps.push(
        Step::new("refused", add([0.5, 0.85]))
            .commits(0)
            .no_draft()
            .notice(format!("{LIMIT} corrections is this photograph's limit")),
    );
    Plan::new(steps)
}

/// The photograph's geometries launch 5 checks the overlay through, each applied on top of the
/// last: a 16:9 crop, the crop straightened by 8 degrees, and a Perspective warp.
const GEOMETRIES: [&str; 4] = ["plain", "crop", "straighten", "perspective"];

fn geometry_edit(name: &str) -> Option<script::Step> {
    match name {
        // The source's detected lens profile, which expanding the Lens section turns on: a
        // nonlinear distortion the overlay is drawn through.
        "lens" => Some(script::Step::Section(script::SectionStep {
            module: "luxforge.lens".into(),
            expanded: true,
        })),
        "crop" => Some(script::Step::call(
            "edit.crop-fit",
            json!({"aspect": "16:9", "angle": 0.0}),
        )),
        "straighten" => Some(script::Step::call(
            "edit.crop-fit",
            json!({"aspect": "16:9", "angle": 8.0}),
        )),
        "perspective" => Some(script::Step::call(
            "edit.set-perspective",
            json!({"horizontal": 20, "vertical": -10}),
        )),
        _ => None,
    }
}

/// Launch 5: launch 1's catalog reopened, the overlay through a crop, a straighten and a
/// Perspective warp. For each geometry: its edit (one entry), the core's `render.transform`, the
/// pins hidden with `H` and shown again, then the top correction selected and put down. The hidden
/// frame is the render with no overlay: the shown and selected frames are held against it.
pub fn launch5(_: &[PathBuf]) -> Plan {
    geometry_launch("Add Clone 7", &GEOMETRIES, 0, Vec::new())
}

/// A reopened catalog whose current entry is `label`, the mode entered, then each of
/// `geometries` in turn with the overlay checks' frames.
/// `row` is the stack row (top first) of the spot each geometry selects, whose outline is read.
pub(crate) fn geometry_launch(
    label: &str,
    geometries: &[&str],
    row: usize,
    tail: Vec<Step>,
) -> Plan {
    let mut steps = vec![
        Step::opened("reopened").label(label),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
    ];
    for &name in geometries {
        if let Some(edit) = geometry_edit(name) {
            // The lens section commits only when the source has a profile to turn on.
            let step = Step::new(name, edit);
            steps.push(if name == "lens" {
                step
            } else {
                step.commits(1)
            });
        }
        steps.push(
            Step::new(format!("{name}-map"), script::Step::api("render.transform")).commits(0),
        );
        steps.push(Step::new(format!("{name}-hidden"), key("h")).commits(0));
        steps.push(Step::new(format!("{name}-shown"), key("h")).commits(0));
        steps.push(
            Step::new(
                format!("{name}-selected"),
                correction(CorrectionStep::Row(Some(row))),
            )
            .commits(0),
        );
        steps.push(Step::new(format!("{name}-deselected"), key("Escape")).commits(0));
    }
    steps.extend(tail);
    Plan::new(steps)
}

/// The folder it was moved from, as Select's Missing originals names the group.
const MOVED_FROM: &str = "original";

/// Launch 6: a photograph developed from the run's own copy of the fixture, with one Clone from the
/// gold quadrant into the blue one.
pub(crate) fn locate_clone() -> Plan {
    let mut painted = press(CLONE_FROM);
    painted.to = CLONE_TO.to_vec();
    Plan::new(vec![
        Step::opened("opened")
            .no_layer(CORRECTIONS_EFFECT)
            .no_draft(),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
        Step::new("size", field("radius", SIZE)).commits(0),
        Step::new("source-set", with(GOLD_SOURCE, PressModifier::Alt))
            .commits(0)
            .status_starts("Source set"),
        Step::new(
            "clone-kind",
            correction(CorrectionStep::Kind("clone".into())),
        )
        .commits(0),
        Step::new("cloned", correction(CorrectionStep::Press(painted)))
            .commits(1)
            .label("Add Clone 1"),
    ])
}

/// Launch 7: nothing open. Select's Missing originals lists the photograph whose original moved,
/// Find in a folder… searches an empty folder (so the photograph is not found, which is what offers
/// Locate…), and Locate… relinks the photograph to the moved file by `source.locate`.
fn locate_select(folder: &str, moved: &str, file: &str) -> Plan {
    let missing = |name: &str, step: MissingStep| Step::new(name, script::Step::Missing(step));
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("select", script::Step::key("g")),
        Step::new(
            "missing",
            script::Step::Select(SelectStep::Source("Missing originals".into())),
        ),
        missing(
            "found",
            MissingStep::Find {
                group: MOVED_FROM.into(),
                folder: folder.into(),
                stop: false,
            },
        )
        .status_starts("Searched "),
        missing(
            "located",
            MissingStep::Locate {
                file: file.into(),
                path: moved.into(),
            },
        )
        .status_starts(format!("Located {file}")),
    ])
}

/// Launch 8: the catalog opened over the moved file, which Locate relinked to.
fn locate_open() -> Plan {
    Plan::new(vec![
        Step::opened("opened").label("Add Clone 1"),
        Step::new("mode", key("q")).commits(0).mode(MODULE),
    ])
}

/// Launches 6 to 8, made after the row's listed launches: the Locate journey, with the original
/// moved by the run between launch 6 and launch 7.
pub(crate) fn locate(
    run: &mut Run,
    scenario: &Scenario,
    fixture: &Path,
    checked: &mut Vec<Checked>,
    clone: Plan,
) -> Result {
    let file = fixture
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("The located original has no file name")?
        .to_owned();
    let dir = run.out().join("locate");
    let paths_file = dir.join("paths.json");
    if !run.replaying() {
        let original = dir.join("original");
        let moved = dir.join("moved");
        let empty = dir.join("empty");
        fs::create_dir_all(&original)?;
        fs::create_dir_all(&moved)?;
        fs::create_dir_all(&empty)?;
        fs::copy(fixture, original.join(&file))?;
        write_json(
            &paths_file,
            &json!({
                "original": original.canonicalize()?.join(&file),
                "moved": moved.canonicalize()?.join(&file),
                "empty": empty.canonicalize()?,
            }),
        )?;
    }
    let recorded = read_json(&paths_file)?;
    let path = |key: &str| -> Result<PathBuf> {
        recorded[key]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| format!("locate/paths.json names no {key}").into())
    };
    let (original, moved, empty) = (path("original")?, path("moved")?, path("empty")?);
    let launch =
        |name: &'static str, script: &'static str, catalog: Option<&'static str>| LaunchSpec {
            name,
            script,
            catalog,
            ..APP
        };
    let steps: [(LaunchSpec, Plan, Vec<PathBuf>); 3] = [
        (
            launch("launch6", "script6.json", None),
            clone,
            vec![original.clone()],
        ),
        (
            launch("launch7", "script7.json", Some("launch6")),
            locate_select(&empty.to_string_lossy(), &moved.to_string_lossy(), &file),
            Vec::new(),
        ),
        (
            launch("launch8", "script8.json", Some("launch6")),
            locate_open(),
            vec![moved.clone()],
        ),
    ];
    for (index, (spec, plan, sources)) in steps.iter().enumerate() {
        if index == 1 && !run.replaying() {
            // The original moves while the editor is not running, and the core records it missing.
            fs::rename(&original, &moved)?;
            let catalog = run.out().join("launch6").join("catalog.sqlite");
            with_owner(&catalog, |owner, client| {
                let started = ask(
                    owner,
                    client,
                    "source.check",
                    json!({"targets": {"kind": "paths", "paths": [&original]}}),
                )?;
                let settled = settle(owner, client, &started["job_id"])?;
                ensure(
                    settled["status"] == "ready"
                        && settled["result"]["rows"][0]["availability"] == "missing",
                    format!("source.check after the move: {settled}"),
                )
            })?;
        }
        let evidence = run
            .launch(launch_of(scenario, spec, plan, sources, run.out()))?
            .dir;
        checked.push(plan.check(&evidence)?);
    }
    Ok(())
}

/// The row's own run: its listed launches, then the Locate journey.
pub fn run(run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let fixture = sources
        .first()
        .cloned()
        .ok_or("The corrections scenario opens one fixture")?;
    launch_planned_then(
        run,
        scenario,
        sources,
        |_, sources| {
            Ok(scenario
                .launches
                .iter()
                .map(|spec| (spec.plan)(sources))
                .collect())
        },
        |run, checked| locate(run, scenario, &fixture, checked, locate_clone()),
    )
}

pub(crate) fn rgb(frame: &Frame, at: [f64; 2]) -> Result<[f64; 3]> {
    frame.rgb_at(at, PATCH_HALF)
}

/// Each channel of `read` within [`NEAR`] of `expected`.
fn near(
    checks: &mut Checks,
    frame: &Frame,
    what: &str,
    read: [f64; 3],
    expected: [f64; 3],
) -> Result {
    for (channel, (read, expected)) in ["red", "green", "blue"]
        .iter()
        .zip(read.iter().zip(expected))
    {
        checks.compare(
            frame,
            &format!("{what}, {channel}"),
            *read,
            expected,
            Tolerance::Within(NEAR),
        )?;
    }
    Ok(())
}

/// The workspace's recorded band in a frame.
pub(crate) fn band(frame: &Frame) -> &Value {
    &frame.state()["corrections"]["band"]
}

/// How close the centre of a pin's changed pixels is to where the core puts the pin, in points.
const RING_CENTRE: f64 = 1.5;
/// How many points around a selected spot's outline are read, each of which must change.
const OUTLINE_POINTS: usize = 12;
/// A pin ring's radius in points, and how far past it a pin's own changed pixels are gathered.
const PIN_RING: f64 = 6.0;

/// The pixels of `shown` that differ from `hidden` within `half` of `at`, in capture pixels.
fn changed(shown: &Frame, hidden: &Frame, at: (f64, f64), half: i64) -> Result<Vec<(i64, i64)>> {
    let (a, b) = (shown.image()?, hidden.image()?);
    ensure(
        a.dimensions() == b.dimensions(),
        "The two captures differ in size",
    )?;
    let (width, height) = (i64::from(a.width()), i64::from(a.height()));
    let (cx, cy) = (at.0.round() as i64, at.1.round() as i64);
    let mut pixels = Vec::new();
    for y in (cy - half).max(0)..=(cy + half).min(height - 1) {
        for x in (cx - half).max(0)..=(cx + half).min(width - 1) {
            let (p, q) = (
                a.get_pixel(x as u32, y as u32),
                b.get_pixel(x as u32, y as u32),
            );
            if p.0.iter().zip(q.0).any(|(p, q)| p.abs_diff(q) > 24) {
                pixels.push((x, y));
            }
        }
    }
    Ok(pixels)
}

/// The core's map of a `render.transform` answer.
fn core_map(answer: &Value) -> Result<luxforge_core::GeometryMap> {
    Ok(serde_json::from_value::<luxforge_core::MappingDescriptor>(answer.clone())?.geometry)
}

/// Overlays through the geometry: after each geometry edit, every pin the desktop retains is held to
/// the core's own map, and the capture shows it there.
pub(crate) fn overlay_through_geometry(
    checks: &mut Checks,
    launch: &Checked,
    geometries: &[&str],
    least: usize,
) -> Result {
    for &geometry in geometries {
        let map = launch.at(&format!("{geometry}-map"))?;
        let core = core_map(&map["step"]["result"])?;
        let (content, output) = (
            (
                f64::from(core.content.width),
                f64::from(core.content.height),
            ),
            (f64::from(core.output.width), f64::from(core.output.height)),
        );
        let (hidden, shown, selected) = (
            launch.at(&format!("{geometry}-hidden"))?,
            launch.at(&format!("{geometry}-shown"))?,
            launch.at(&format!("{geometry}-selected"))?,
        );
        ensure(
            hidden.state()["workspace"]["mask_handles"] == json!(false)
                && shown.state()["workspace"]["mask_handles"] == json!(true),
            format!("{geometry}: H did not flip the session's handles key"),
        )?;
        let state = &shown.state()["corrections"];
        ensure(
            state["map"] == json!(true) && state["map_stale"] == json!(false),
            format!("{geometry}: the pins' map was not current: {state}"),
        )?;
        ensure(
            state["output"] == json!([output.0, output.1]),
            format!(
                "{geometry}: the desktop's output {} is not the core's {output:?}",
                state["output"]
            ),
        )?;
        let pins = state["pins"].as_array().ok_or("No pins recorded")?;
        ensure(pins.len() >= least, format!("{geometry}: pins {pins:?}"))?;
        let scale = shown["scale"].as_f64().unwrap_or(1.0);
        let mut placed = 0;
        for pin in pins {
            let operation = pin["operation"].as_str().unwrap_or("?");
            let (Some(x), Some(y)) = (pin["content"][0].as_f64(), pin["content"][1].as_f64())
            else {
                continue;
            };
            let Ok(want) = core.to_output(x * content.0, y * content.1) else {
                ensure(
                    pin["output"].is_null(),
                    format!(
                        "{geometry} {operation}: the core cannot place it but the desktop does"
                    ),
                )?;
                continue;
            };
            let got = (
                pin["output"][0]
                    .as_f64()
                    .ok_or("The desktop placed no pin")?,
                pin["output"][1]
                    .as_f64()
                    .ok_or("The desktop placed no pin")?,
            );
            checks.compare(
                shown,
                &format!("{geometry} {operation}: x in the output stage"),
                got.0,
                want.0,
                Tolerance::Within(1e-6),
            )?;
            checks.compare(
                shown,
                &format!("{geometry} {operation}: y in the output stage"),
                got.1,
                want.1,
                Tolerance::Within(1e-6),
            )?;
            // A pin whose ring is wholly inside the output is read from the capture.
            let rect = shown.photo_rect()?;
            let edge = (PIN_RING + 6.0) * scale * output.0 / (rect[2] - rect[0]) as f64;
            if want.0 < edge
                || want.1 < edge
                || want.0 > output.0 - edge
                || want.1 > output.1 - edge
            {
                continue;
            }
            let at = shown.photo_at([want.0 / output.0, want.1 / output.1])?;
            let pixels = changed(shown, hidden, at, ((PIN_RING + 5.0) * scale).ceil() as i64)?;
            ensure(
                pixels.len() >= (PIN_RING * scale) as usize * 2,
                format!(
                    "{geometry} {operation}: only {} pixels changed around {at:?}",
                    pixels.len()
                ),
            )?;
            let n = pixels.len() as f64;
            let centre = (
                pixels.iter().map(|p| p.0 as f64).sum::<f64>() / n,
                pixels.iter().map(|p| p.1 as f64).sum::<f64>() / n,
            );
            checks.compare(
                shown,
                &format!("{geometry} {operation}: the drawn pin's centre, x (capture pixels)"),
                centre.0,
                at.0,
                Tolerance::Within(RING_CENTRE * scale),
            )?;
            checks.compare(
                shown,
                &format!("{geometry} {operation}: the drawn pin's centre, y (capture pixels)"),
                centre.1,
                at.1,
                Tolerance::Within(RING_CENTRE * scale),
            )?;
            placed += 1;
        }
        ensure(
            placed >= 3,
            format!("{geometry}: only {placed} pins were read from the capture"),
        )?;
        // The selected spot's outline passes through the points the core's map puts a circle of its
        // radius at, whatever the warp does to it.
        let chosen = selected.state()["corrections"]["selected"]
            .as_str()
            .ok_or("No correction was selected")?;
        let pin = pins
            .iter()
            .find(|pin| pin["operation"] == chosen)
            .ok_or("The selected correction has no pin")?;
        let (centre, radius) = (
            [
                pin["content"][0].as_f64().ok_or("No anchor")?,
                pin["content"][1].as_f64().ok_or("No anchor")?,
            ],
            pin["radius"].as_f64().ok_or("No radius")?,
        );
        let mut drawn = 0;
        for index in 0..OUTLINE_POINTS {
            let angle = std::f64::consts::TAU * index as f64 / OUTLINE_POINTS as f64;
            let want = core
                .to_output(
                    centre[0] * content.0 + radius * content.1 * angle.cos(),
                    centre[1] * content.1 + radius * content.1 * angle.sin(),
                )
                .map_err(|error| format!("{geometry}: outline point {index}: {error:?}"))?;
            let at = selected.photo_at([want.0 / output.0, want.1 / output.1])?;
            if !changed(selected, hidden, at, (2.0 * scale).ceil() as i64)?.is_empty() {
                drawn += 1;
            }
        }
        checks.compare(
            selected,
            &format!("{geometry}: outline points drawn where the core's map puts them"),
            f64::from(drawn),
            OUTLINE_POINTS as f64,
            Tolerance::Within(0.0),
        )?;
        checks.note(
            selected,
            &format!(
                "{geometry}: pins and the selected outline sit where the core's map puts them"
            ),
            json!({"pins_read_from_the_capture": placed, "selected": chosen,
                "mapping_sha256": core.mapping_sha256}),
        );
    }
    Ok(())
}

/// The Locate journey: the original moved, Select located it, and the Clone is intact and drawn.
fn located(
    checks: &mut Checks,
    run: &Run,
    cloned: &Checked,
    select: &Checked,
    open: &Checked,
) -> Result {
    let made = cloned.at("cloned")?;
    near(
        checks,
        made,
        "the Clone before the original moves",
        rgb(made, CLONED)?,
        GOLD,
    )?;
    let missing = select.at("missing")?;
    ensure(
        missing.state()["missing"]["count"] == json!(1),
        format!(
            "The moved original is the one missing: {}",
            missing.state()["missing"]["count"]
        ),
    )?;
    let located = select.at("located")?;
    ensure(
        located.state()["missing"]["count"] == json!(0),
        format!(
            "Locate left {} missing",
            located.state()["missing"]["count"]
        ),
    )?;
    // The same bytes at the new place: the moved file is the fixture, byte for byte.
    let paths = read_json(&run.out().join("locate").join("paths.json"))?;
    let moved = PathBuf::from(paths["moved"].as_str().ok_or("No moved path")?);
    let fixture = run.sources().first().ok_or("The run opens one fixture")?;
    ensure(
        hash(&moved)? == hash(fixture)?,
        "The moved original is not the fixture's bytes",
    )?;
    let opened = open.at("opened")?;
    near(
        checks,
        opened,
        "the Clone rendered after Locate",
        rgb(opened, CLONED)?,
        GOLD,
    )?;
    let band = band(open.at("mode")?);
    ensure(
        band["active"] == json!(true) && band["count"] == json!(1),
        format!("The corrections after Locate: {band}"),
    )?;
    Ok(())
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [
        launch1,
        launch2,
        launch3,
        launch4,
        launch5,
        launch6,
        launch7,
        launch8,
    ] = launches
    else {
        return Err(format!("Expected eight launches, found {}", launches.len()).into());
    };
    let mut checks = Checks::new();
    // Launch 1. The band and the canvas state at each journey.
    let mode = launch1.at("mode")?;
    ensure(
        band(mode)["active"] == json!(true) && band(mode)["footer"] == json!("0 of 100"),
        format!("The band on entering the mode: {}", band(mode)),
    )?;
    let spot = launch1.at("spot")?;
    ensure(
        spot.state()["corrections"]["selected"] == json!("correction-1"),
        "The new correction is selected on commit",
    )?;
    let held = launch1.at("clone-held")?;
    ensure(
        band(held)["drafting"] == json!("Clone 4")
            && held.state()["draft_bar"]["title"] == json!("Clone 4"),
        format!(
            "The held Clone shows its drafting row and bar: {} {}",
            band(held),
            held.state()["draft_bar"]
        ),
    )?;
    // The live repair through the draft, and the committed one: gold into blue.
    let opened = launch1.at("opened")?;
    near(
        &mut checks,
        opened,
        "the Clone's destination before it",
        rgb(opened, CLONED)?,
        BLUE,
    )?;
    near(
        &mut checks,
        held,
        "the live Clone while held",
        rgb(held, CLONED)?,
        GOLD,
    )?;
    let released = launch1.at("clone-released")?;
    near(
        &mut checks,
        released,
        "the committed Clone",
        rgb(released, CLONED)?,
        GOLD,
    )?;
    // `H` hides the pins through the session's one handles key, `workspace.mask_handles`.
    let (hidden, shown) = (launch1.at("hidden")?, launch1.at("shown")?);
    ensure(
        hidden.state()["corrections"]["hidden"] == json!(true)
            && shown.state()["corrections"]["hidden"] == json!(false)
            && hidden.state()["workspace"]["mask_handles"] == json!(false)
            && shown.state()["workspace"]["mask_handles"] == json!(true),
        format!(
            "H hides and shows the pins through mask_handles: {} {}",
            hidden.state()["workspace"]["mask_handles"],
            shown.state()["workspace"]["mask_handles"]
        ),
    )?;
    // The no-source notice keeps the painted stroke as an open add draft.
    let no_source = launch1.at("no-source")?;
    ensure(
        no_source.state()["corrections"]["draft"]["op"] == json!("add"),
        format!(
            "The refused add keeps its draft: {}",
            no_source.state()["corrections"]["draft"]
        ),
    )?;
    // Stacking: the second Clone sampled the first Clone's repair, gold, not the blue beneath it.
    let stacked = launch1.at("stacked")?;
    near(
        &mut checks,
        stacked,
        "the stacked Clone",
        rgb(stacked, STACKED)?,
        GOLD,
    )?;
    // History before the first Clone shows the blue it replaced.
    let before = launch1.at("history-before-clone")?;
    near(
        &mut checks,
        before,
        "the history entry before the Clone",
        rgb(before, CLONED)?,
        BLUE,
    )?;
    let current = launch1.at("history-current")?;
    near(
        &mut checks,
        current,
        "back at the current state",
        rgb(current, CLONED)?,
        GOLD,
    )?;
    // The exported JPEG carries the repairs.
    let exported = run.out().join("launch1").join("corrections.jpg");
    let image = image::open(&exported)
        .map_err(|error| format!("The export {} does not decode: {error}", exported.display()))?
        .to_rgb8();
    let (width, height) = image.dimensions();
    let pixel = image.get_pixel(
        (CLONED[0] * f64::from(width)) as u32,
        (CLONED[1] * f64::from(height)) as u32,
    );
    let pixel = [
        f64::from(pixel[0]),
        f64::from(pixel[1]),
        f64::from(pixel[2]),
    ];
    for (channel, (read, expected)) in pixel.iter().zip(GOLD).enumerate() {
        ensure(
            (read - expected).abs() <= NEAR,
            format!("The export's Clone channel {channel} reads {read}, expected about {expected}"),
        )?;
    }
    // Launch 2: the reopened corrections in Develop after Remove and Put back.
    let back = launch2.at("mode")?;
    ensure(
        band(back)["count"].as_u64().is_some_and(|count| count > 0),
        format!("The reopened photograph's corrections: {}", band(back)),
    )?;
    near(
        &mut checks,
        back,
        "the Clone after reopen, Remove and Put back",
        rgb(back, CLONED)?,
        GOLD,
    )?;
    // Launches 3 and 4: the limit.
    let filled = launch3.at(&format!("agent-{}", FILL_FIRST - 1))?;
    ensure(filled.revision()? > 0, "Launch 3 committed its corrections")?;
    let refused = launch4.at("refused")?;
    ensure(
        band(refused)["footer"] == json!(format!("{LIMIT} of {LIMIT}"))
            && band(refused)["notice"]["kind"] == json!("limit"),
        format!("The press at the limit: {}", band(refused)),
    )?;
    overlay_through_geometry(&mut checks, launch5, &GEOMETRIES, 4)?;
    located(&mut checks, run, launch6, launch7, launch8)?;
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "near_codes": NEAR,
            "ring_centre_pixels": RING_CENTRE,
            "outline_points": OUTLINE_POINTS,
            "scope": "Mean 8-bit RGB of small patches of the captured photograph and of the exported JPEG over a flat four-quadrant fixture; repair colours read against the quadrants they were copied from, not a colorimetric claim. Overlay placement: the pixels the capture with the pins shown changes against the capture with them hidden, read against the places the core's own render.transform puts the pins and a selected spot's outline, on the native renderer's capture; no scanout claim",
        }),
    )
}
