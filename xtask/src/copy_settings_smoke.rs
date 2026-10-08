//! Native clipboard and selection journey; each gesture uses the desktop command path.
use crate::{
    scenario::{Checked, Checks, Launch, Plan, Run, Step, Tolerance, plan::only},
    smoke::Scenario,
    *,
};
use luxforge_evidence::{self as script, CopySettingsStep as Copy, DevelopStep, SelectStep};
const EXPECTED: &str = "copy-settings-expected.json";

fn plan(expected: &Value) -> Plan {
    let copy = |name: &str, step: Copy| Step::new(name, step);
    let develop = |name: &str, step| Step::new(name, script::Step::Develop(step));
    let rows = expected["rows"].as_array().unwrap();
    let first = rows[0]["file_name"].as_str().unwrap();
    let mut steps = vec![
        Step::opened("opened"),
        Step::new("select", script::Step::key("g")),
        Step::new("all", SelectStep::Source("All photographs".into())),
        Step::new(
            "first",
            SelectStep::Click {
                position: 0,
                shift: false,
                command: false,
            },
        ),
        develop("open", DevelopStep::Active),
        Step::new(
            "source-exposure",
            script::Step::call("edit.set-basic", json!({"exposure":0.6})),
        ),
        copy("quick-copy", Copy::Copy),
        copy("chooser", Copy::Chooser),
        copy("none", Copy::None),
        copy(
            "tone",
            Copy::Check {
                group: "Basic · Tone".into(),
                checked: true,
            },
        ),
        copy("chosen", Copy::Chosen),
        develop("target", DevelopStep::Cell(1)),
        develop("baseline", DevelopStep::Settle),
        copy("paste", Copy::Paste).label(format!("Paste settings from {first}")),
        Step::new("undo", script::Step::api("history.undo")).label("Original"),
        copy("previous", Copy::Previous).label(format!("Paste settings from {first}")),
        copy("select-all", Copy::SelectAll),
        copy("filmstrip-confirmation", Copy::Paste),
        copy("filmstrip-pasted", Copy::Confirm),
        copy("filmstrip-report", Copy::Report),
        copy("close-report", Copy::Cancel),
    ];
    if let Some(raw) = rows.iter().position(|row| row["raw"] == true) {
        let jpeg = rows.iter().position(|row| row["raw"] != true).unwrap();
        steps.extend([
            develop("jpeg-source", DevelopStep::Cell(jpeg)),
            develop("jpeg-ready", DevelopStep::Settle),
            copy("jpeg-chooser", Copy::Chooser),
            copy(
                "jpeg-white-balance",
                Copy::Check {
                    group: "Basic · White balance".into(),
                    checked: true,
                },
            ),
            copy("jpeg-copied", Copy::Chosen),
            develop("raw-target", DevelopStep::Cell(raw)),
            develop("raw-ready", DevelopStep::Settle),
            copy("jpeg-to-raw", Copy::Paste),
        ]);
    }
    // Give the Select batch a fresh setting, so it proves successful edits and reported
    // kind-specific skips rather than merely repeating the filmstrip batch as no-ops.
    steps.extend([
        Step::new(
            "select-source-exposure",
            script::Step::call("edit.set-basic", json!({"exposure":0.9})),
        ),
        copy("select-source-copied", Copy::Copy),
        Step::new("select-again", script::Step::key("g")),
    ]);
    if let Some(raw) = rows.iter().position(|row| row["raw"] == true) {
        steps.extend([
            Step::new(
                "raw-active",
                SelectStep::Click {
                    position: raw as u32,
                    shift: false,
                    command: false,
                },
            ),
            copy("raw-chooser", Copy::Chooser),
            copy(
                "white-balance",
                Copy::Check {
                    group: "Basic · White balance".into(),
                    checked: true,
                },
            ),
            copy("raw-copied", Copy::Chosen),
        ]);
    }
    steps.extend([
        Step::new(
            "selection-start",
            SelectStep::Click {
                position: 0,
                shift: false,
                command: false,
            },
        ),
        Step::new(
            "selection-end",
            SelectStep::Click {
                position: (rows.len() - 1) as u32,
                shift: true,
                command: false,
            },
        ),
        copy("select-confirmation", Copy::Paste),
        copy("select-pasted", Copy::Confirm),
        copy("select-report", Copy::Report),
    ]);
    Plan::new(steps)
}

pub fn run(run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    let generated = run.out().join("generated");
    let expected_path = run.out().join(EXPECTED);
    run.check(|run| {
        ensure(
            sources.len() <= 1,
            "copy-settings accepts at most one RAW --source",
        )?;
        if !run.replaying() {
            fs::create_dir_all(&generated)?;
            write_json(
                &expected_path,
                &crate::filmstrip_smoke::make(&generated, sources.first().map(PathBuf::as_path))?,
            )?;
        }
        let expected = read_json(&expected_path)?;
        let plan = plan(&expected);
        let mut launch = Launch::app()
            .catalog(&generated.join(crate::generate_catalog::CATALOG))
            .script("script.json", plan.script());
        if let Some(window) = scenario.window {
            launch = launch.window(window);
        }
        run.hash(&sources)?;
        let evidence = run.launch(launch)?.dir;
        let checked = plan.check(&evidence)?;
        (scenario.verify)(run, &[checked])?;
        run.sources_unchanged()?;
        Ok(())
    })
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let expected = read_json(&run.out().join(EXPECTED))?;
    let count = expected["rows"].as_array().unwrap().len();
    let mut checks = Checks::new();
    for name in [
        "quick-copy",
        "chosen",
        "paste",
        "undo",
        "previous",
        "filmstrip-confirmation",
        "filmstrip-pasted",
        "select-confirmation",
        "select-pasted",
        "select-report",
    ] {
        let frame = launch.at(name)?;
        ensure(
            frame.state()["copy_settings"]["pending"] == false,
            format!("{name} captured before completion"),
        )?;
        checks.note(frame, name, json!({"clipboard":frame.state()["copy_settings"],"develop":frame.state()["develop"],"stack":frame.state()["stack"],"status":frame.status()?}));
    }
    let chosen = launch.at("chosen")?;
    ensure(
        chosen.state()["copy_settings"]["clipboard"]["groups"] == json!(["Basic · Tone"]),
        "chooser did not select only Tone",
    )?;
    for name in ["filmstrip-confirmation", "select-confirmation"] {
        ensure(
            launch.at(name)?.state()["copy_settings"]["confirmation"]["count"] == json!(count),
            format!("{name}: wrong target count"),
        )?;
    }
    for name in ["paste", "undo", "previous", "filmstrip-pasted"] {
        ensure(
            launch.at(name)?.state()["copy_settings"]["clipboard"]
                == chosen.state()["copy_settings"]["clipboard"],
            format!("{name} changed the copied snapshot"),
        )?;
    }
    for name in ["source-exposure", "paste", "previous"] {
        ensure(
            launch
                .at(name)?
                .payload(luxforge_core::BASIC_EFFECT)
                .and_then(|payload| payload["exposure"].as_f64())
                == Some(0.6),
            format!("{name}: stored exposure differs from the copied field"),
        )?;
    }
    ensure(
        launch.at("undo")?.payload(luxforge_core::BASIC_EFFECT)
            == launch.at("baseline")?.payload(luxforge_core::BASIC_EFFECT),
        "undo did not restore the complete Basic payload",
    )?;
    for name in ["filmstrip-pasted", "select-pasted"] {
        let batch = &launch.at(name)?.state()["select"]["catalog"]["batch"];
        ensure(
            batch["end"]["status"] == "ready",
            format!("{name}: batch not complete: {batch}"),
        )?;
        let report = &batch["end"]["report"];
        let accounted = report["done"].as_array().map_or(0, Vec::len)
            + report["skipped"].as_array().map_or(0, Vec::len);
        ensure(
            accounted == count,
            format!("{name}: report accounts for {accounted} of {count}"),
        )?;
    }
    let baseline = crate::filmstrip_smoke::drawn_luminance(launch.at("baseline")?)?;
    let selected_report =
        &launch.at("select-pasted")?.state()["select"]["catalog"]["batch"]["end"]["report"];
    ensure(
        selected_report["done"].as_array().map_or(0, Vec::len) == count - 1,
        "Select paste did not change every target other than its source",
    )?;
    let paste = launch.at("paste")?;
    checks.compare(
        paste,
        "paste brightens the target",
        crate::filmstrip_smoke::drawn_luminance(paste)?,
        baseline,
        Tolerance::Above(2.0),
    )?;
    let undo = launch.at("undo")?;
    checks.compare(
        undo,
        "undo restores the target pixels",
        crate::filmstrip_smoke::drawn_luminance(undo)?,
        baseline,
        Tolerance::Within(1.5),
    )?;
    let previous = launch.at("previous")?;
    checks.compare(
        previous,
        "previous reproduces paste",
        crate::filmstrip_smoke::drawn_luminance(previous)?,
        crate::filmstrip_smoke::drawn_luminance(paste)?,
        Tolerance::Within(1.5),
    )?;
    if expected["raw"].is_string() {
        let raw_groups = &launch.at("raw-chooser")?.state()["copy_settings"]["chooser"]["groups"];
        ensure(
            raw_groups.as_array().is_some_and(|groups| {
                groups.iter().any(|group| {
                    group["label"] == "Basic · White balance" && group["custom"] == false
                })
            }),
            "the RAW source's as-shot white balance was not labelled Original",
        )?;
        ensure(
            selected_report["settings_skipped"]
                .as_array()
                .map_or(0, Vec::len)
                == 3,
            "Select report did not retain the three edited JPEGs' white-balance skips",
        )?;
        ensure(
            launch.at("raw-copied")?.state()["copy_settings"]["clipboard"]["kind"] == "raw",
            "RAW source not captured",
        )?;
        ensure(
            launch.at("select-confirmation")?.state()["copy_settings"]["confirmation"]["skip"]
                .as_str()
                .is_some_and(|text| text.contains("3 photographs")),
            "confirmation does not identify three JPEG white-balance skips",
        )?;
        ensure(
            launch
                .at("jpeg-to-raw")?
                .status()?
                .contains("White balance skipped:"),
            "JPEG white balance on RAW did not report its skip",
        )?;
        run.record(
            "raw",
            json!({"status":"checked", "directions":["RAW to JPEG","JPEG to RAW"]}),
        );
    } else {
        run.record("raw", json!({"status":"pending","reason":"run with a RAW --source to verify mixed white balance"}));
    }
    checks.write(
        run.out(),
        "copy-settings",
        json!({"scope":"native rendered clipboard, selection, history and pixel direction"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copy_settings_native_plans_validate_with_and_without_raw() {
        for count in [3, 4] {
            let rows: Vec<_> = (0..count)
                .map(|index| json!({"file_name":format!("{index}.jpg"),"raw":index == 3}))
                .collect();
            plan(&json!({"rows":rows})).validate().unwrap();
        }
    }
}
