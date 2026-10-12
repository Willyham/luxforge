//! The `move-to-applications` smoke scenario
//! ([design](../../docs/design/auto-update.md#moving-into-applications)): the question a macOS
//! release build asks when it runs outside Applications, drawn in a background launch.
//!
//! A development build never asks, and no evidence run asks unless told to, so the launch passes
//! `--evidence-move-to-applications`, which shows the question once the workspace is usable,
//! wherever the run's app is, and makes its buttons move nothing. The run's app is the harness's
//! background-only bundle under the temporary folder, so it classifies as `elsewhere`, with the
//! Trash as what a move would do with the original. The checks read the question's state in the
//! frame — its phase, words, buttons and switch, where the app runs and what a move would clean
//! up — the log's classification and showing in order, and the capture's pixels: the card drawn
//! over the bottom of the workspace where the opened frame had none.
use crate::{
    scenario::{Checked, Checks, Plan, Run, Step, plan::only},
    *,
};
use luxforge_evidence as script;

pub const SCENARIO: &str = "move-to-applications";
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
pub const NOTE: &str = "The launch passes `--evidence-move-to-applications`, an evidence-only \
     flag: the question is shown once the workspace is usable wherever the run's app is (here the \
     harness's background-only bundle under the temporary folder), and its buttons move nothing.";

const TITLE: &str = "Move Luxforge to Applications?";
const BODY: &str = "Luxforge can only update itself from the Applications folder.";
/// How long the question's step waits after the open's capture: the classification runs off the
/// update loop and answers in milliseconds.
const SETTLE_MS: u64 = 1500;
/// How far two captures' pixels must differ for one to count as changed.
const CHANGED: i32 = 24;

pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("question", script::Step::wait(SETTLE_MS)).commits(0),
    ])
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let opened = launch.at("opened")?;
    let question = launch.at("question")?;
    let before = &opened.state()["move_to_applications"];
    let state = &question.state()["move_to_applications"];
    let mut checks = Checks::new();
    checks.note(question, "the move question", state.clone());

    ensure(
        before["asker"] == "evidence" && before["notice"].is_null(),
        format!("The opened frame already shows the question: {before}"),
    )?;
    ensure(
        state["asker"] == "evidence"
            && state["phase"] == "shown"
            && state["location"] == "elsewhere"
            && state["cleanup"] == "trash"
            && state["dont_ask"] == false
            && state["failure"].is_null(),
        format!("The question is not shown for an app elsewhere: {state}"),
    )?;
    let bundle = state["bundle"].as_str().unwrap_or_default();
    ensure(
        bundle.ends_with(".app") && state["original"] == state["bundle"],
        format!("The question does not name the running bundle: {state}"),
    )?;
    let notice = &state["notice"];
    ensure(
        notice["title"] == TITLE
            && notice["body"] == BODY
            && notice["buttons"] == json!(["Move and relaunch", "Not now"])
            && notice["dont_ask"] == false,
        format!("The question does not say the design's words: {notice}"),
    )?;
    // No other startup notice is up beside it, and the photograph is the open's.
    ensure(
        question.state()["stack"] == opened.state()["stack"]
            && question.state()["workspace"] == opened.state()["workspace"],
        "Showing the question changed the session or the stack",
    )?;

    // The log: classified, then shown, once each; nothing moved.
    let named = |name: &str| {
        launch
            .events
            .iter()
            .position(|event| event["event"] == name)
    };
    let count = |name: &str| {
        launch
            .events
            .iter()
            .filter(|event| event["event"] == name)
            .count()
    };
    let classified = named("move_question_classified");
    let shown = named("move_question_shown");
    ensure(
        matches!((classified, shown), (Some(a), Some(b)) if a < b)
            && count("move_question_classified") == 1
            && count("move_question_shown") == 1,
        format!("The log does not classify then show the question once: {classified:?}, {shown:?}"),
    )?;
    for moved in [
        "move_to_applications_moved",
        "move_to_applications_requested",
        "after_close",
    ] {
        ensure(count(moved) == 0, format!("An evidence run logged {moved}"))?;
    }
    let shown_event = &launch.events[shown.unwrap_or_default()]["detail"];
    ensure(
        shown_event["phase"] == "shown" && shown_event["location"] == "elsewhere",
        format!("The shown event disagrees with the frame: {shown_event}"),
    )?;

    // The card is drawn over the bottom of the workspace: the lower middle of the window changed
    // from the opened frame's, and nothing above it did.
    let (a, b) = (opened.image()?, question.image()?);
    ensure(
        a.dimensions() == b.dimensions(),
        "The two captures differ in size",
    )?;
    let (width, height) = a.dimensions();
    let differ = |x: u32, y: u32| {
        let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
        (0..3).any(|c| (i32::from(p[c]) - i32::from(q[c])).abs() > CHANGED)
    };
    let count_in = |x0: u32, x1: u32, y0: u32, y1: u32| {
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| differ(x, y))
            .count()
    };
    let card = count_in(width / 3, 2 * width / 3, height / 2, height);
    let top = count_in(0, width, 0, height / 4);
    checks.note(
        question,
        "the card's pixels",
        json!({"changed_lower_middle": card, "changed_top_quarter": top}),
    );
    ensure(
        card > ((width / 3) * (height / 2) / 50) as usize,
        format!("The question's card is not drawn: {card} pixels changed in the lower middle"),
    )?;
    ensure(
        top == 0,
        format!("{top} pixels changed in the window's top quarter"),
    )?;
    checks.write(
        &launch.evidence,
        run.scenario(),
        json!({"changed_threshold": CHANGED, "settle_ms": SETTLE_MS}),
    )
}
