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
//!
//! The fixture is the four flat quadrants, so what a repair does is legible in the capture: a
//! Clone from the gold quadrant into the blue one turns that patch gold, and a Clone whose source
//! is that repaired patch turns its destination gold rather than blue — the stacking principle,
//! read from pixels.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance},
    *,
};
use luxforge_evidence::{
    self as script, CatalogStep, CorrectionHandle, CorrectionStep, CorrectionDrag, ExportFile, ExportStep,
    PressModifier, PressStep, PreviewStep, SelectStep, SelectWorkspace,
};

pub const SCENARIO: &str = "corrections";
/// The golden four-quadrant fixture, unrotated: content and output coordinates coincide, so the
/// normalized positions the script presses at are the ones the capture is measured at.
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
pub const NOTE: &str = "Four launches. The first walks every Corrections gesture and key on one photograph and exports it; the second reopens its catalog, shows the rendered preview in Select, removes and puts back the photograph and returns to Develop; the third and fourth fill a fresh photograph to the declared limit from an agent and press once more.";

const MODULE: &str = "luxforge.corrections";
pub const CORRECTIONS_EFFECT: &str = "luxforge.corrections.repair";

/// The Size rail positions the script sets: a radius of 0.04 content heights (26 px across on the
/// 320 px fixture), and the top of the range, which leaves no candidate inside the frame.
const SIZE: f64 = 0.7051;
const HUGE: f64 = 1.0;

/// Where things are, in normalized content coordinates.
const SPOT: [f64; 2] = [0.30, 0.30];
const STROKE: [[f64; 2]; 3] = [[0.62, 0.30], [0.72, 0.30], [0.82, 0.30]];
const STRAIGHT_END: [f64; 2] = [0.82, 0.40];
const GOLD_SOURCE: [f64; 2] = [0.75, 0.80];
/// The live-painted Clone: from the gold quadrant into the blue one.
const CLONE_FROM: [f64; 2] = [0.25, 0.80];
const CLONE_TO: [[f64; 2]; 2] = [[0.30, 0.80], [0.35, 0.80]];
/// A patch inside the Clone's destination, well within its full coverage and clear of the arrow
/// drawn along `y = 0.8` while it is selected.
const CLONED: [f64; 2] = [0.29, 0.812];
const PULL_AT: [f64; 2] = [0.40, 0.25];
const PULL_TO: [[f64; 2]; 2] = [[0.55, 0.50], [0.70, 0.75]];
/// The stacked Clone: its source is the earlier Clone's repaired patch, its destination is green.
const STACK_SOURCE: [f64; 2] = [0.27, 0.80];
const STACK_AT: [f64; 2] = [0.70, 0.42];
/// A patch inside the stacked spot's full coverage, clear of its pin ring and of the arrow that
/// reaches the pin from the lower left.
const STACKED: [f64; 2] = [0.70 + 3.5 / 480.0, 0.42 - 3.5 / 320.0];

const PATCH_HALF: i64 = 3;
/// How close a repaired patch's mean must be to its source colour, per channel, in 8-bit codes:
/// the flat quadrants are JPEG-compressed, so a margin, not a tuned threshold.
const NEAR: f64 = 30.0;
const GOLD: [f64; 3] = [234.0, 195.0, 30.0];
const BLUE: [f64; 3] = [40.0, 70.0, 220.0];

fn key(key: &str) -> script::Step {
    script::Step::Key { key: key.into() }
}

fn press(at: [f64; 2]) -> PressStep {
    PressStep {
        at,
        to: Vec::new(),
        modifiers: Vec::new(),
        release: true,
        interval_ms: None,
    }
}

fn correction(step: CorrectionStep) -> script::Step {
    step.into()
}

fn add(at: [f64; 2]) -> script::Step {
    correction(CorrectionStep::Press(press(at)))
}

fn with(at: [f64; 2], modifier: PressModifier) -> script::Step {
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

fn field(name: &str, fraction: f64) -> script::Step {
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
        Step::opened("opened").no_layer(CORRECTIONS_EFFECT).no_draft(),
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
        Step::new("clone-kind", correction(CorrectionStep::Kind("clone".into()))).commits(0),
        // The Clone painted with the button still down: the live repair through the draft.
        Step::new("clone-held", correction(CorrectionStep::Press(held)))
            .commits(0)
            .status_starts("Clone 4 draft"),
        Step::new("clone-released", correction(CorrectionStep::Release(CLONE_TO[1])))
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
        Step::new("pin-drag", drag(CorrectionHandle::Pin, &[[0.31, 0.31], [0.32, 0.32]]))
            .commits(1)
            .label("Move Heal 1"),
        Step::new("source-drag", drag(CorrectionHandle::Source, &[[0.38, 0.20], [0.40, 0.18]]))
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
        Step::new("row-drag", correction(CorrectionStep::Reorder { row: 0, to: 4 }))
            .commits(1)
            .label("Reorder Clone 5"),
        // `H` hides the pins and outlines, and shows them again.
        Step::new("hidden", key("h")).commits(0).status_starts("Pins and outlines hidden"),
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
        Step::new("stack-kind", correction(CorrectionStep::Kind("clone".into()))).commits(0),
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
        Step::new("escape-leave", key("Escape")).commits(0).mode("pointer"),
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

fn rgb(frame: &Frame, at: [f64; 2]) -> Result<[f64; 3]> {
    frame.rgb_at(at, PATCH_HALF)
}

/// Each channel of `read` within [`NEAR`] of `expected`.
fn near(checks: &mut Checks, frame: &Frame, what: &str, read: [f64; 3], expected: [f64; 3]) -> Result {
    for (channel, (read, expected)) in ["red", "green", "blue"].iter().zip(read.iter().zip(expected)) {
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
fn band(frame: &Frame) -> &Value {
    &frame.state()["corrections"]["band"]
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let [launch1, launch2, launch3, launch4] = launches else {
        return Err(format!("Expected four launches, found {}", launches.len()).into());
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
    near(&mut checks, opened, "the Clone's destination before it", rgb(opened, CLONED)?, BLUE)?;
    near(&mut checks, held, "the live Clone while held", rgb(held, CLONED)?, GOLD)?;
    let released = launch1.at("clone-released")?;
    near(&mut checks, released, "the committed Clone", rgb(released, CLONED)?, GOLD)?;
    // `H` hides the pins: recorded as view state.
    ensure(
        launch1.at("hidden")?.state()["corrections"]["hidden"] == json!(true)
            && launch1.at("shown")?.state()["corrections"]["hidden"] == json!(false),
        "H hides and shows the pins",
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
    near(&mut checks, stacked, "the stacked Clone", rgb(stacked, STACKED)?, GOLD)?;
    // History before the first Clone shows the blue it replaced.
    let before = launch1.at("history-before-clone")?;
    near(&mut checks, before, "the history entry before the Clone", rgb(before, CLONED)?, BLUE)?;
    let current = launch1.at("history-current")?;
    near(&mut checks, current, "back at the current state", rgb(current, CLONED)?, GOLD)?;
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
    let pixel = [f64::from(pixel[0]), f64::from(pixel[1]), f64::from(pixel[2])];
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
    near(&mut checks, back, "the Clone after reopen, Remove and Put back", rgb(back, CLONED)?, GOLD)?;
    // Launches 3 and 4: the limit.
    let filled = launch3.at(&format!("agent-{}", FILL_FIRST - 1))?;
    ensure(
        filled.revision()? > 0,
        "Launch 3 committed its corrections",
    )?;
    let refused = launch4.at("refused")?;
    ensure(
        band(refused)["footer"] == json!(format!("{LIMIT} of {LIMIT}"))
            && band(refused)["notice"]["kind"] == json!("limit"),
        format!("The press at the limit: {}", band(refused)),
    )?;
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "near_codes": NEAR,
            "scope": "Mean 8-bit RGB of small patches of the captured photograph and of the exported JPEG over a flat four-quadrant fixture; repair colours read against the quadrants they were copied from, not a colorimetric claim",
        }),
    )
}
