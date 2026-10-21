//! The `select-introduction` smoke scenario
//! ([design](../../docs/design/select-introduction.md)): the Select introduction over a new
//! catalog, drawn in a background launch.
//!
//! No automated launch shows the sheet by itself, so the launch passes
//! `--evidence-select-introduction`, which shows it as a person's launch does: the first time
//! Select is shown, here once the opened frame is captured, while the run's own preferences say it
//! was not closed. Nothing is opened, so the new catalog starts in Select. Its frames: Select
//! without the sheet; the sheet, with the design's words, its keys and Add a folder…, since nothing
//! is indexed; Return closing it; the stored preference; the title bar's ? showing it again; the
//! same sheet at 150% interface size and in a light theme; and Escape closing it, which stores
//! nothing more. The checks read the introduction's state in each frame against the design, the
//! preferences the desktop holds, the log's shown and closed events in order, and the captures'
//! pixels: the sheet drawn over the centre where the opened frame had none.
use crate::{
    scenario::{Checked, Checks, Plan, Run, Step, plan::only},
    *,
};
use luxforge_evidence::{self as script, SelectStep};

pub const SCENARIO: &str = "select-introduction";
pub const NOTE: &str = "The launch passes `--evidence-select-introduction`, an evidence-only \
     flag: the run shows the Select introduction the first time Select is shown, as a launch with \
     its window shown does, while the run's own preferences say it was not closed.";

const TITLE: &str = "Select, then develop";
/// The light theme the sheet is captured in: one of the bundled Omarchy themes.
const LIGHT: &str = "omarchy.catppuccin-latte";
/// How long a step that only waits gives the desktop: the sheet appears on the first message after
/// the opened frame, and the preference write answers in milliseconds.
const SETTLE_MS: u64 = 1000;
/// How far two captures' pixels must differ for one to count as changed: in a dark theme the
/// sheet's surface and the scrim over the grid move a pixel by only a few levels.
const CHANGED: i32 = 4;

pub fn plan(_: &[PathBuf]) -> Plan {
    // Nothing is open, so no frame records a revision to count commits from.
    let step = Step::new;
    Plan::new(vec![
        Step::opened("opened"),
        step("introduced", script::Step::wait(SETTLE_MS)),
        step("closed", script::Step::key("Return")),
        step("stored", script::Step::wait(SETTLE_MS)),
        step("reopened", script::Step::Select(SelectStep::Introduction)),
        step(
            "large",
            script::Step::preference([("interface_size", json!(150))]),
        ),
        step("light", script::Step::theme(LIGHT)),
        step("escaped", script::Step::key("Escape")),
    ])
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let frame = |name: &str| launch.at(name);
    let introduction =
        |name: &str| -> Result<Value> { Ok(frame(name)?.state()["select_introduction"].clone()) };
    let seen = |name: &str| -> Result<Value> {
        Ok(frame(name)?.state()["preferences"]["stored"]["select_introduction_seen"].clone())
    };
    let mut checks = Checks::new();

    let opened = introduction("opened")?;
    ensure(
        opened["first"] == "waiting"
            && opened["sheet"].is_null()
            && frame("opened")?.state()["select"]["shown"] == "select",
        format!("The opened frame is not Select without the sheet: {opened}"),
    )?;

    let shown = introduction("introduced")?;
    checks.note(frame("introduced")?, "the sheet", shown.clone());
    let sheet = &shown["sheet"];
    ensure(
        shown["first"] == "shown" && shown["reopened"] == false,
        format!("The sheet is not shown the first time Select is: {shown}"),
    )?;
    ensure(
        sheet["title"] == TITLE
            && sheet["buttons"] == json!(["Add a folder\u{2026}", "Start browsing"])
            && sheet["steps"]
                .as_array()
                .is_some_and(|steps| steps.len() == 3)
            && sheet["steps"][1]["keys"] == json!(["Space", "Z", "P", "\u{2193}"])
            && sheet["stacked"] == false,
        format!("The sheet does not say the design's words: {sheet}"),
    )?;
    let words = sheet["words"]
        .as_array()
        .map(|words| {
            words
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    ensure(
        words.contains("Luxforge doesn\u{2019}t import")
            && words.contains("Your files are never moved, copied or changed."),
        format!("The sheet's words are not the design's: {words}"),
    )?;
    ensure(
        seen("introduced")? != true,
        "Showing the sheet stored the preference",
    )?;

    let closed = introduction("closed")?;
    ensure(
        closed["first"] == "closed" && closed["closed"] == "return" && closed["sheet"].is_null(),
        format!("Return did not close the sheet: {closed}"),
    )?;
    ensure(
        seen("stored")? == true,
        format!(
            "Closing the sheet did not store select_introduction_seen: {}",
            frame("stored")?.state()["preferences"]
        ),
    )?;

    let reopened = introduction("reopened")?;
    ensure(
        reopened["reopened"] == true
            && reopened["first"] == "closed"
            && reopened["sheet"]["title"] == TITLE,
        format!("The ? did not show the sheet again: {reopened}"),
    )?;
    for name in ["large", "light"] {
        let state = introduction(name)?;
        ensure(
            state["sheet"]["title"] == TITLE,
            format!("The sheet is not drawn in the {name} frame: {state}"),
        )?;
    }
    let large = frame("large")?.state();
    ensure(
        large["preferences"]["display"]["interface_size"] == 150,
        format!(
            "The large frame is not at 150%: {}",
            large["preferences"]["display"]
        ),
    )?;
    ensure(
        frame("light")?.state()["theme"]["id"] == LIGHT,
        format!(
            "The light frame is not in {LIGHT}: {}",
            frame("light")?.state()["theme"]
        ),
    )?;
    let escaped = introduction("escaped")?;
    ensure(
        escaped["sheet"].is_null() && escaped["reopened"] == false && escaped["closed"] == "return",
        format!("Escape did not close the sheet shown again: {escaped}"),
    )?;

    // The log: shown, closed and stored; shown again and closed, storing nothing.
    let events: Vec<&Value> = launch
        .events
        .iter()
        .filter(|event| {
            event["event"]
                .as_str()
                .is_some_and(|name| name.starts_with("select_introduction_"))
        })
        .collect();
    let summary: Vec<Value> = events
        .iter()
        .map(|event| json!([event["event"], event["detail"]]))
        .collect();
    ensure(
        summary
            == [
                json!(["select_introduction_shown", {"reopened": false}]),
                json!(["select_introduction_closed", {"how": "return", "stored": true}]),
                json!(["select_introduction_shown", {"reopened": true}]),
                json!(["select_introduction_closed", {"how": "escape", "stored": false}]),
            ],
        format!("The log does not show, close, show again and close the sheet: {summary:?}"),
    )?;

    // The sheet is drawn over the middle of the window and the scrim darkens the sources panel
    // beside it; both are gone once it is closed.
    let (width, height) = frame("opened")?.image()?.dimensions();
    let middle = [width / 3, height / 3, 2 * width / 3, 2 * height / 3];
    let sources = [0, height / 3, width / 8, 2 * height / 3];
    let region = |a: &str, b: &str, [x0, y0, x1, y1]: [u32; 4]| -> Result<(usize, f64)> {
        let (a, b) = (frame(a)?.image()?, frame(b)?.image()?);
        ensure(
            a.dimensions() == b.dimensions(),
            "Two captures differ in size",
        )?;
        let pixels: Vec<(i32, i32)> = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let (p, q) = (a.get_pixel(x, y), b.get_pixel(x, y));
                (0..3).map(move |c| (i32::from(p[c]), i32::from(q[c])))
            })
            .collect();
        let changed = pixels
            .chunks(3)
            .filter(|pixel| pixel.iter().any(|(p, q)| (p - q).abs() > CHANGED))
            .count();
        let darker =
            pixels.iter().map(|(p, q)| f64::from(p - q)).sum::<f64>() / pixels.len() as f64;
        Ok((changed, darker))
    };
    let area = |[x0, y0, x1, y1]: [u32; 4]| ((x1 - x0) * (y1 - y0)) as usize;
    let (drawn, _) = region("opened", "introduced", middle)?;
    let (gone, _) = region("opened", "closed", middle)?;
    let (_, dimmed) = region("opened", "introduced", sources)?;
    let (_, undimmed) = region("opened", "closed", sources)?;
    let area = area(middle);
    checks.note(
        frame("introduced")?,
        "the sheet's pixels",
        json!({
            "changed_middle": drawn,
            "changed_middle_after_closing": gone,
            "middle": area,
            "sources_darker_by": dimmed,
            "sources_darker_after_closing_by": undimmed,
        }),
    );
    ensure(
        drawn > area / 2,
        format!("The sheet is not drawn: {drawn} of {area} middle pixels changed"),
    )?;
    ensure(
        dimmed > 2.0,
        format!("The scrim does not darken the sources panel: {dimmed:.2} levels darker"),
    )?;
    ensure(
        gone < area / 100 && undimmed.abs() < 0.5,
        format!(
            "The sheet is still drawn after Return: {gone} of {area} middle pixels differ, \
             the sources panel {undimmed:.2} levels darker"
        ),
    )?;
    checks.write(
        &launch.evidence,
        run.scenario(),
        json!({"changed_threshold": CHANGED, "settle_ms": SETTLE_MS}),
    )
}
