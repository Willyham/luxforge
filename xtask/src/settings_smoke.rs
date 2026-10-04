//! The Settings sheet in a developer launch: opened from the palette at General, which shows Auto
//! collapse history on by default, then at Experiments, each flag kind changed through
//! its own control, a number the field refuses, a Reset, developer mode turned off for the next
//! launch while `--developer` holds this one, the sheet closed with Escape and opened again over
//! what the host stored. Nothing is committed, and the photograph is untouched throughout.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, pixels, plan::only},
    *,
};
use luxforge_evidence::{self as script};

/// The refusal a number off its step leaves in the status bar.
const NUMBER_REFUSED: &str = "Proof number takes a number from 0 to 100 in steps of 5";

pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new(
            "general",
            script::PaletteStep::Run("settings general".into()),
        )
        .commits(0),
        Step::new("sheet", script::PaletteStep::Run("experiments".into())).commits(0),
        Step::new(
            "choice",
            script::Step::flag("proof.choice", Some(json!("third"))),
        )
        .commits(0),
        Step::new(
            "number",
            script::Step::flag("proof.number", Some(json!(75))),
        )
        .commits(0),
        Step::new(
            "number-refused",
            script::Step::flag("proof.number", Some(json!(33))),
        )
        .commits(0)
        .status(NUMBER_REFUSED),
        Step::new("reset", script::Step::flag("proof.choice", None)).commits(0),
        Step::new(
            "developer-off",
            script::Step::flag("developer", Some(json!(false))),
        )
        .commits(0),
        Step::new("closed", script::Step::key(script::KEY_ESCAPE)).commits(0),
        Step::new("reopened", script::Step::settings(true)).commits(0),
    ])
}

fn settings(frame: &Frame) -> &Value {
    &frame.state()["settings"]
}

/// The row the frame drew for `id`.
fn row<'a>(frame: &'a Frame, id: &str) -> Result<&'a Value> {
    settings(frame)["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .ok_or_else(|| format!("The frame shows no {id} row: {}", settings(frame)).into())
}

/// The flag as the host last listed it to the sheet.
fn listed<'a>(frame: &'a Frame, id: &str) -> Result<&'a Value> {
    settings(frame)["flags"]["flags"]
        .as_array()
        .and_then(|flags| flags.iter().find(|flag| flag["id"] == id))
        .ok_or_else(|| format!("The host listed no {id} flag").into())
}

/// The capture's colour at a fraction of its width and height.
fn colour_at(frame: &Frame, at: [f64; 2]) -> Result<[u8; 3]> {
    let image = frame.image()?;
    let (width, height) = image.dimensions();
    let x = (f64::from(width) * at[0]) as u32;
    let y = (f64::from(height) * at[1]) as u32;
    Ok(image.get_pixel(x.min(width - 1), y.min(height - 1)).0)
}

/// Inside the sheet, under its one tab: the empty foot of the tab rail, which the sheet draws on
/// the Bar surface over the dimmed workspace. The sheet is 720 by 480 points, centred in the
/// 1440 by 900 window, and its rail is 160 points wide.
const RAIL_FOOT: [f64; 2] = [(360.0 + 80.0) / 1440.0, (690.0 - 40.0) / 900.0];
const BAR: [u8; 3] = [0x23, 0x23, 0x26];

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 3)
}

pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let opened = launch.at("opened")?;
    let mut checks = Checks::new();
    checks.note(
        opened,
        "the unedited photograph before the sheet",
        pixels::identity_photo(opened)?,
    );
    ensure(
        settings(opened)["open"].is_null(),
        "The sheet was open at launch",
    )?;
    let behind = colour_at(opened, RAIL_FOOT)?;
    ensure(
        !near(behind, BAR),
        "The workspace behind the sheet is already the sheet's colour, so the check proves nothing",
    )?;

    for step in launch.names().iter().skip(1) {
        let frame = launch.at(step)?;
        // The sheet is the desktop's own view state, and a flag touches no photograph.
        ensure(
            frame.state()["workspace"] == opened.state()["workspace"]
                && frame.state()["stack"] == opened.state()["stack"],
            format!("Step {step:?} changed the session workspace or the stack"),
        )?;
        ensure(
            settings(frame)["writes_outstanding"] == 0 && settings(frame)["error"].is_null(),
            format!(
                "Step {step:?} left a write outstanding or failed: {}",
                settings(frame)
            ),
        )?;
    }

    let general = launch.at("general")?;
    let collapse = settings(general)["general"]["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == "auto_collapse_history"));
    ensure(
        settings(general)["open"] == "general"
            && settings(general)["general"]["error"].is_null()
            && collapse
                == Some(
                    &json!({"id": "auto_collapse_history", "control": {"toggle": true},
                                "saving": false}),
                ),
        format!(
            "The palette did not open General with Auto collapse history on: {}",
            settings(general)
        ),
    )?;
    let drawn = colour_at(general, RAIL_FOOT)?;
    ensure(
        near(drawn, BAR),
        format!("The General tab is not drawn: the rail's foot is {drawn:?}, not the Bar surface"),
    )?;
    checks.note(
        general,
        "the General tab with Auto collapse history on by default",
        settings(general)["general"].clone(),
    );

    let sheet = launch.at("sheet")?;
    let ids: Vec<_> = settings(sheet)["rows"]
        .as_array()
        .ok_or("The sheet drew no rows")?
        .iter()
        .map(|row| row["id"].clone())
        .collect();
    ensure(
        settings(sheet)["open"] == "experiments"
            && ids
                == [
                    json!("developer"),
                    json!("proof.choice"),
                    json!("proof.number"),
                ],
        format!("The palette opened another sheet: {}", settings(sheet)),
    )?;
    ensure(
        row(sheet, "developer")?["notes"] == json!(["On for this launch: --developer"]),
        "The developer row does not say --developer holds this launch",
    )?;
    let drawn = colour_at(sheet, RAIL_FOOT)?;
    ensure(
        near(drawn, BAR),
        format!("The sheet is not drawn: the rail's foot is {drawn:?}, not the Bar surface"),
    )?;
    checks.note(
        sheet,
        "the Experiments tab over the dimmed workspace",
        json!({"rows": ids, "rail_foot": drawn}),
    );

    let choice = launch.at("choice")?;
    ensure(
        row(choice, "proof.choice")?["control"] == json!({"choice": "third"})
            && listed(choice, "proof.choice")?["value"] == "third"
            && listed(choice, "proof.choice")?["stored"] == true,
        "The choice did not reach the host",
    )?;
    let number = launch.at("number")?;
    ensure(
        row(number, "proof.number")?["control"] == json!({"number": "75", "invalid": false})
            && listed(number, "proof.number")?["value"].as_f64() == Some(75.0),
        "The number did not reach the host",
    )?;
    let refused = launch.at("number-refused")?;
    ensure(
        row(refused, "proof.number")?["control"] == json!({"number": "33", "invalid": true})
            && listed(refused, "proof.number")?["value"].as_f64() == Some(75.0),
        "A number off its step was sent, or the field forgot what was typed",
    )?;
    let reset = launch.at("reset")?;
    ensure(
        row(reset, "proof.choice")?["control"] == json!({"choice": "first"})
            && row(reset, "proof.choice")?["can_reset"] == false
            && listed(reset, "proof.choice")?["stored"] == false,
        "Reset left a stored value",
    )?;
    let developer = launch.at("developer-off")?;
    let flag = listed(developer, "developer")?;
    ensure(
        flag["value"] == false
            && flag["active"] == true
            // The launch's own switch forced it; the sheet's note, checked above, names it.
            && flag["override"].is_string()
            && developer.state()["developer"] == true,
        format!("Developer mode's next launch or this one is wrong: {flag}"),
    )?;
    checks.note(
        developer,
        "developer mode off for the next launch, held on for this one",
        flag.clone(),
    );

    let closed = launch.at("closed")?;
    ensure(
        settings(closed)["open"].is_null(),
        "Escape left the sheet open",
    )?;
    let after = colour_at(closed, RAIL_FOOT)?;
    ensure(
        near(after, behind),
        format!("The sheet is still drawn after Escape: {after:?}, not {behind:?}"),
    )?;
    checks.note(
        closed,
        "the unedited photograph after the sheet",
        pixels::identity_photo(closed)?,
    );

    let reopened = launch.at("reopened")?;
    ensure(
        listed(reopened, "proof.number")?["value"].as_f64() == Some(75.0)
            && listed(reopened, "proof.choice")?["stored"] == false
            && listed(reopened, "developer")?["value"] == false,
        "Reopening read back other values than the host stored",
    )?;
    checks.write(&launch.evidence, "settings", json!({}))
}
