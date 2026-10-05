//! The Settings sheet in a developer launch: opened from the palette at General, which shows Auto
//! collapse history on by default, then at Experiments, each flag kind changed through
//! its own control, a number the field refuses, a Reset, developer mode turned off for the next
//! launch while `--developer` holds this one, the sheet closed with Escape and opened again over
//! what the host stored. Then the General rows' preferences, each through its row's own control:
//! the canvas background set to grey and back to dark, checked in the canvas beside the
//! photograph with the panels unchanged; the interface size set to 125%, checked in the title
//! bar's drawn height against 100%, with the sheet drawn at that size, and back to 100%; the mask
//! overlay colour set to white; the lens switch turned off; and a catalog folder chosen inside the
//! evidence directory, whose relaunch note the Catalog row shows until Use Default. Nothing is
//! committed, and the photograph is untouched throughout.
//!
//! No RAW photograph is checked in (see `fixtures/README.md`), so the scenario imports none after
//! the lens switch: that an import with the switch off commits no lens entry is proved by the
//! core's owner tests (`first_open_skips_the_lens_module_while_the_switch_is_off_and_asks_it_when_on`
//! and `an_owner_import_follows_the_lens_preference_at_start_and_after_a_change`). Here the switch
//! is checked to reach the row and the stored preferences.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, pixels, plan::only},
    smoke::{self, APP, Scenario},
    *,
};
use luxforge_evidence::{self as script};

/// The refusal a number off its step leaves in the status bar.
const NUMBER_REFUSED: &str = "Proof number takes a number from 0 to 100 in steps of 5";

/// The folder chosen for the catalog, inside the launch's evidence directory.
const CHOSEN: &str = "chosen-catalog";
/// The catalog file a chosen folder holds, and an evidence run's own catalog.
const CATALOG_FILE: &str = "catalog.sqlite";
/// The evidence directory the table's plan names, for `smoke --list`, the plan checks and the
/// script dumps: a run plans over its own launch's evidence directory instead.
const LISTED_EVIDENCE: &str = "/out/app";

/// The canvas backgrounds the scenario sets, as the capture must show them.
const DARK: [u8; 3] = [0x19, 0x19, 0x1b];
const GREY: [u8; 3] = [0x77, 0x77, 0x77];
/// What the canvas colour may differ by in any channel: nothing, since it is a flat fill.
const CANVAS_TOLERANCE: f64 = 0.0;

/// The table's plan.
pub fn plan(_: &[PathBuf]) -> Plan {
    planned(Path::new(LISTED_EVIDENCE))
}

/// The row's own run: the table's launch, its catalog folder chosen inside its own evidence
/// directory.
pub fn run(run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    smoke::launch_planned(run, scenario, sources, |run, _| {
        Ok(vec![planned(&evidence(run)?)])
    })
}

/// The launch's evidence directory as an absolute path, which the catalog step must name: in a
/// replay, the recorded run's, whose script named it.
fn evidence(run: &Run) -> Result<PathBuf> {
    let out = run
        .recorded(run.out())
        .unwrap_or_else(|| run.out().to_path_buf());
    Ok(std::path::absolute(out)?.join(APP.name))
}

/// The catalog file the catalog step chooses, inside `evidence`.
fn chosen(evidence: &Path) -> PathBuf {
    evidence.join(CHOSEN).join(CATALOG_FILE)
}

/// One General row changed through its own control.
fn preference(name: &str, field: &str, value: Value) -> Step {
    Step::new(name, script::Step::preference([(field, value)])).commits(0)
}

fn planned(evidence: &Path) -> Plan {
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
        // The display preferences with the sheet closed, so the canvas and the title bar are
        // drawn undimmed.
        Step::new("closed-again", script::Step::key(script::KEY_ESCAPE)).commits(0),
        preference("grey", "canvas_background", json!("grey")),
        preference("dark", "canvas_background", json!("dark")),
        preference("size-125", "interface_size", json!(125)),
        // The sheet at 125%, which must fit the window.
        Step::new(
            "general-125",
            script::PaletteStep::Run("settings general".into()),
        )
        .commits(0),
        preference("size-100", "interface_size", json!(100)),
        preference("mask-white", "mask_overlay_colour", json!("white")),
        preference("lens-off", "auto_lens_profile", json!(false)),
        preference(
            "catalog-chosen",
            "catalog",
            json!(chosen(evidence).to_string_lossy()),
        ),
        preference("catalog-default", "catalog", Value::Null),
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

/// The session workspace the frame records, less the mask overlay colour a General row sets.
fn session_workspace(frame: &Frame) -> Value {
    let mut workspace = frame.state()["workspace"].clone();
    if let Some(fields) = workspace.as_object_mut() {
        fields.remove("mask_overlay_colour");
    }
    workspace
}

fn preferences(frame: &Frame) -> &Value {
    &frame.state()["preferences"]
}

/// The General row the frame drew for `field`.
fn general_row<'a>(frame: &'a Frame, field: &str) -> Result<&'a Value> {
    settings(frame)["general"]["rows"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == field))
        .ok_or_else(|| {
            format!(
                "The frame shows no General {field} row: {}",
                settings(frame)["general"]
            )
            .into()
        })
}

/// The frame's canvas region, `[left, top, right, bottom]` physical pixels.
fn canvas_rect(frame: &Frame) -> Result<[u32; 4]> {
    serde_json::from_value(frame["canvas_rect"].clone())
        .map_err(|_| "The frame records no canvas rectangle".into())
}

/// The canvas beside the photograph: midway across the gap between the canvas's left edge and the
/// photograph's, at the photograph's vertical middle. The largest channel difference from
/// `expected` over a 5 × 5 patch there, and the patch's centre.
fn beside_photo(frame: &Frame, expected: [u8; 3]) -> Result<(f64, [u32; 2])> {
    let [canvas_left, ..] = canvas_rect(frame)?;
    let [left, top, _, bottom] = frame.photo()?;
    ensure(
        left >= canvas_left + 12,
        format!(
            "No canvas beside the photograph: it starts at {left}, the canvas at {canvas_left}"
        ),
    )?;
    let centre = [(canvas_left + left) / 2, (top + bottom) / 2];
    let image = frame.image()?;
    let mut worst = 0u8;
    for y in centre[1] - 2..=centre[1] + 2 {
        for x in centre[0] - 2..=centre[0] + 2 {
            let pixel = image.get_pixel(x, y).0;
            for (a, b) in pixel.iter().zip(expected) {
                worst = worst.max(a.abs_diff(b));
            }
        }
    }
    Ok((f64::from(worst), centre))
}

/// The pixels outside the canvas region, the panels and the bars, that differ between two
/// captures of the same size.
fn panels_changed(a: &Frame, b: &Frame) -> Result<u64> {
    let [left, top, right, bottom] = canvas_rect(a)?;
    ensure(
        canvas_rect(b)? == [left, top, right, bottom],
        "The canvas region moved",
    )?;
    let (a, b) = (a.image()?, b.image()?);
    ensure(
        a.dimensions() == b.dimensions(),
        "The captures differ in size",
    )?;
    Ok(a.enumerate_pixels()
        .filter(|(x, y, pixel)| {
            let inside = (left..right).contains(x) && (top..bottom).contains(y);
            !inside && **pixel != *b.get_pixel(*x, *y)
        })
        .count() as u64)
}

/// The pixels inside `photo`, less its outermost row and column on each side, that differ between
/// two captures.
fn photo_changed(a: &Frame, b: &Frame, photo: [u32; 4]) -> Result<u64> {
    let [left, top, right, bottom] = photo;
    let (a, b) = (a.image()?, b.image()?);
    Ok((top + 1..bottom - 1)
        .flat_map(|y| (left + 1..right - 1).map(move |x| (x, y)))
        .filter(|(x, y)| a.get_pixel(*x, *y) != b.get_pixel(*x, *y))
        .count() as u64)
}

/// The drawn height of the title bar with the rule under it, in the capture's pixels: up the
/// column through the middle of the photograph from just above its top edge, over the canvas
/// surface between them, to the first row that is not the canvas surface, which is the rule's
/// lowest.
fn title_bar_height(frame: &Frame) -> Result<u32> {
    let [left, top, right, _] = frame.photo()?;
    let image = frame.image()?;
    let x = (left + right) / 2;
    let mut y = top
        .checked_sub(2)
        .ok_or("The photograph touches the window's top")?;
    ensure(
        image.get_pixel(x, y).0 == DARK,
        format!("The canvas above the photograph is not the dark surface at {x},{y}"),
    )?;
    while y > 0 && image.get_pixel(x, y - 1).0 == DARK {
        y -= 1;
    }
    ensure(y > 0, "No title bar above the canvas")?;
    Ok(y)
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
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
        // The sheet is the desktop's own view state, and a flag or a preference touches no
        // photograph. The mask overlay colour is the session's, checked on its own below.
        ensure(
            session_workspace(frame) == session_workspace(opened)
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

    general_preferences(run, launch, &mut checks)?;
    checks.write(
        &launch.evidence,
        "settings",
        json!({"canvas": {"dark": DARK, "grey": GREY, "tolerance_per_channel": CANVAS_TOLERANCE,
               "scope": "a 5 × 5 patch of the canvas beside the photograph"}}),
    )
}

/// The General rows' steps: what each frame draws and records of the preference it set.
fn general_preferences(run: &Run, launch: &Checked, checks: &mut Checks) -> Result {
    let closed = launch.at("closed-again")?;
    ensure(
        settings(closed)["open"].is_null(),
        "Escape left the sheet open",
    )?;
    for step in ["closed-again", "grey", "dark", "size-125"] {
        ensure(
            settings(launch.at(step)?)["open"].is_null(),
            format!("The sheet is open at step {step:?}, dimming the canvas"),
        )?;
    }
    for step in launch.names().iter().skip_while(|step| *step != "grey") {
        let frame = launch.at(step)?;
        ensure(
            preferences(frame)["error"].is_null()
                && preferences(frame)["writing"].is_null()
                && preferences(frame)["waiting"].is_null(),
            format!(
                "Step {step:?} left a preference write outstanding or failed: {}",
                preferences(frame)
            ),
        )?;
    }

    // The canvas background: grey beside the photograph, the panels and bars as they were, and
    // dark again.
    let (before, _) = beside_photo(closed, DARK)?;
    checks.compare(
        closed,
        "the dark canvas beside the photograph before the change",
        before,
        0.0,
        Tolerance::Within(CANVAS_TOLERANCE),
    )?;
    let grey = launch.at("grey")?;
    ensure(
        preferences(grey)["display"]["canvas_background"] == "grey"
            && preferences(grey)["applied"]["canvas_background"] == "grey"
            && preferences(grey)["stored"]["canvas_background"] == "grey",
        format!(
            "The grey canvas was not applied and stored: {}",
            preferences(grey)
        ),
    )?;
    let (difference, at) = beside_photo(grey, GREY)?;
    checks.compare(
        grey,
        "the grey canvas beside the photograph, largest channel difference from #777777",
        difference,
        0.0,
        Tolerance::Within(CANVAS_TOLERANCE),
    )?;
    let changed = panels_changed(closed, grey)?;
    checks.compare(
        grey,
        "pixels outside the canvas region changed by the grey canvas",
        changed as f64,
        0.0,
        Tolerance::Within(0.0),
    )?;
    let photo = grey.photo()?;
    ensure(
        photo == closed.photo()?,
        "The grey canvas moved the photograph",
    )?;
    let changed = photo_changed(closed, grey, photo)?;
    checks.compare(
        grey,
        "pixels of the photograph changed by the grey canvas",
        changed as f64,
        0.0,
        Tolerance::Within(0.0),
    )?;
    checks.note(
        grey,
        "the grey canvas around the unedited photograph",
        json!({"beside_photo": at, "photo": photo}),
    );
    let dark = launch.at("dark")?;
    let (difference, _) = beside_photo(dark, DARK)?;
    checks.compare(
        dark,
        "the dark canvas beside the photograph, largest channel difference from #19191b",
        difference,
        0.0,
        Tolerance::Within(CANVAS_TOLERANCE),
    )?;
    ensure(
        preferences(dark)["display"]["canvas_background"] == "dark",
        "The dark canvas was not applied",
    )?;

    // The interface size: the combined scale factor, and the title bar drawn 1.25 times as tall.
    let size = launch.at("size-125")?;
    let display = &preferences(size)["display"];
    let system = display["system_scale_factor"]
        .as_f64()
        .ok_or("The frame records no system scale factor")?;
    let scale = display["scale_factor"]
        .as_f64()
        .ok_or("The frame records no scale factor")?;
    ensure(
        display["interface_size"] == 125 && preferences(size)["applied"]["interface_size"] == 125,
        format!("The frame does not record the interface size at 125%: {display}"),
    )?;
    checks.compare(
        size,
        "the scale factor against the system's × 1.25",
        scale,
        system * 1.25,
        Tolerance::Within(1e-6),
    )?;
    let (at_100, at_125) = (title_bar_height(dark)?, title_bar_height(size)?);
    checks.compare(
        size,
        "the title bar's drawn height with its rule at 125%, against 1.25 × its height at 100%",
        f64::from(at_125),
        f64::from(at_100) * 1.25,
        Tolerance::Within(1.0),
    )?;
    for (frame, height) in [(dark, at_100), (size, at_125)] {
        ensure(
            canvas_rect(frame)?[1] == height,
            format!(
                "The canvas the frame records starts at row {}, not where the title bar is drawn to, {height}",
                canvas_rect(frame)?[1]
            ),
        )?;
    }
    checks.note(
        size,
        "the workspace at 125%",
        json!({"display": display, "title_bar_rows": {"100": at_100, "125": at_125}}),
    );
    let sheet = launch.at("general-125")?;
    ensure(
        settings(sheet)["open"] == "general"
            && preferences(sheet)["display"]["interface_size"] == 125,
        "The General tab did not open at 125%",
    )?;
    checks.note(
        sheet,
        "the General tab at 125%, for visual review",
        settings(sheet)["general"].clone(),
    );
    let back = launch.at("size-100")?;
    ensure(
        preferences(back)["display"]["interface_size"] == 100
            && preferences(back)["display"]["scale_factor"].as_f64() == Some(system)
            && preferences(back)["stored"]["interface_size"] == 100,
        format!(
            "The interface size did not return to 100%: {}",
            preferences(back)
        ),
    )?;

    // The mask overlay colour and the lens switch, as the rows and the preferences show them.
    let white = launch.at("mask-white")?;
    ensure(
        general_row(white, "mask_overlay_colour")?["control"] == json!({"choice": "white"})
            && preferences(white)["applied"]["mask_overlay_colour"] == "white"
            && preferences(white)["stored"]["mask_overlay_colour"] == "white"
            && white.state()["workspace"]["mask_overlay_colour"] == "white",
        format!(
            "The mask overlay colour is not white in the row, the preferences and the session: {} {}",
            general_row(white, "mask_overlay_colour")?,
            preferences(white)
        ),
    )?;
    let lens = launch.at("lens-off")?;
    ensure(
        general_row(lens, "auto_lens_profile")?["control"] == json!({"toggle": false})
            && preferences(lens)["applied"]["auto_lens_profile"] == false
            && preferences(lens)["stored"]["auto_lens_profile"] == false,
        format!(
            "The lens switch is not off in the row and the preferences: {}",
            preferences(lens)
        ),
    )?;
    checks.note(
        lens,
        "the lens switch off; that a RAW import then commits no lens entry is the core's owner tests'",
        general_row(lens, "auto_lens_profile")?.clone(),
    );

    // The catalog: chosen inside the evidence directory for the next launch, then Use Default.
    let evidence = evidence(run)?;
    let file = chosen(&evidence);
    let this_launch = "This launch uses the evidence run's catalog";
    let relaunch = format!("Relaunch to use {}", file.display());
    let catalog = launch.at("catalog-chosen")?;
    let row = &general_row(catalog, "catalog")?["control"];
    ensure(
        row["catalog"] == json!(evidence.join(CATALOG_FILE).to_string_lossy())
            && row["stored"] == json!(file.to_string_lossy())
            && row["notes"] == json!([this_launch, relaunch])
            && preferences(catalog)["stored"]["catalog"] == json!(file.to_string_lossy()),
        format!("The Catalog row does not show the chosen folder for the next launch: {row}"),
    )?;
    checks.note(
        catalog,
        "a catalog folder chosen for the next launch",
        row.clone(),
    );
    let default = launch.at("catalog-default")?;
    let row = &general_row(default, "catalog")?["control"];
    ensure(
        row["stored"].is_null()
            && row["notes"] == json!([this_launch])
            && preferences(default)["stored"]["catalog"].is_null(),
        format!("Use Default left a stored location or its relaunch note: {row}"),
    )?;
    ensure(
        !file.exists(),
        "Choosing a catalog folder created a catalog before the next launch",
    )?;
    checks.note(default, "Use Default", row.clone());
    Ok(())
}
