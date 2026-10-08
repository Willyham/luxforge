//! Auto's declared control and per-photo preset, correlated with independent query answers.
use crate::{
    scenario::{Checked, Checks, Plan, Run, Step, plan::only},
    *,
};
use luxforge_core::{BASIC_EFFECT, auto_tone::FIELDS};
use luxforge_evidence::{self as script, ControlsStep, PresetCreateStep, PreviewStep, SectionStep};

pub fn plan(_: &[PathBuf]) -> Plan {
    let press = || {
        script::Step::Controls(ControlsStep::Action {
            action: "auto-tone".into(),
            background: false,
        })
    };
    let form = |submit| PresetCreateStep {
        auto_tone: true,
        name: "Auto per photo".into(),
        group: None,
        groups: vec![],
        submit,
    };
    Plan::new(vec![
        Step::opened("opened").no_draft().no_layer(BASIC_EFFECT),
        Step::new("prediction", script::Step::api("query.auto-tone")).commits(0),
        Step::new(
            "busy",
            script::Step::Controls(ControlsStep::Action {
                action: "auto-tone".into(),
                background: true,
            }),
        )
        .commits(0),
        Step::new(
            "button",
            script::Step::Controls(ControlsStep::AnalysisReady {
                action: "auto-tone".into(),
            }),
        )
        .commits(1)
        .label("Auto tone")
        .no_draft(),
        Step::new("shortcut-repeat", script::Step::key("Command+U"))
            .commits(0)
            .label("Auto tone"),
        Step::new("undo", script::Step::api("history.undo"))
            .commits(1)
            .no_layer(BASIC_EFFECT),
        Step::new("shortcut", script::Step::key("Command+U"))
            .commits(1)
            .label("Auto tone"),
        Step::new("history", PreviewStep::Sequence(0)).commits(0),
        Step::new("history-disabled", press()).commits(0),
        Step::new("current", PreviewStep::Current).commits(0),
        Step::new("compare", script::Step::Compare(script::CompareStep::Tap)).commits(0),
        Step::new("compare-disabled", press()).commits(0),
        Step::new(
            "compare-end",
            script::Step::Compare(script::CompareStep::Tap),
        )
        .commits(0),
        Step::new(
            "presets",
            script::Step::Section(SectionStep {
                module: "luxforge.presets".into(),
                expanded: true,
            }),
        )
        .commits(0),
        Step::new("form", form(false)).commits(0),
        Step::new("created", form(true)).commits(0),
        Step::new("undo-again", script::Step::api("history.undo"))
            .commits(1)
            .no_layer(BASIC_EFFECT),
        Step::new("preset", script::Step::preset("Auto per photo"))
            .commits(1)
            .label("Preset: Auto per photo"),
        Step::new("predicted-again", script::Step::api("query.auto-tone")).commits(0),
        // The palette lists Auto with the chord Basic's descriptor declares beside its label.
        Step::new(
            "palette",
            script::PaletteStep::Query("auto".into()),
        )
        .commits(0),
    ])
}

pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let prediction = &launch.at("prediction")?["step"]["result"];
    let mut checks = Checks::new();
    // RAW opening can already have an automatic lens-profile entry. Undo must restore
    // the exact entry Auto started from, including its development and other layers.
    for name in ["undo", "undo-again"] {
        ensure(
            launch.at(name)?.entry()? == launch.at("opened")?.entry()?,
            format!("{name}: Auto did not return to the opened entry"),
        )?;
    }
    let busy = &launch.at("busy")?.state()["section_controls"]["luxforge.basic"];
    ensure(
        busy.as_array().is_some_and(|controls| {
            controls.iter().any(|control| {
                control["action"] == "auto-tone"
                    && control["label"] == "Auto…"
                    && control["runnable"] == false
            })
        }),
        "the in-flight Auto button is not visibly busy and disabled",
    )?;

    for name in ["button", "shortcut-repeat", "shortcut", "preset"] {
        let frame = launch.at(name)?;
        let payload = frame
            .payload(BASIC_EFFECT)
            .ok_or("Auto committed no Basic layer")?;
        for field in FIELDS {
            let expected = prediction["values"][field]
                .as_f64()
                .ok_or("Auto query did not report a value")?;
            ensure(
                (payload[field].as_f64().unwrap_or(0.) - expected).abs() < 1e-9,
                format!("{name}: {field} differs from the query"),
            )?;
            let shown = frame.field("set-basic", field)?.parse::<f64>()?;
            ensure(
                (shown - expected).abs() < 1e-9,
                format!("{name}: {field} slider differs from its committed value"),
            )?;
        }
        checks.note(
            frame,
            "all eight committed and displayed values equal the independent query",
            prediction.clone(),
        );
    }
    ensure(
        prediction == &launch.at("predicted-again")?["step"]["result"],
        "Auto prediction changed after repeated Auto and preset application",
    )?;
    let explanation =
        &launch.at("button")?.state()["control_ui"]["analysis_reports"]["auto-tone"][2];
    ensure(
        explanation == prediction,
        "the button explanation is not the query's report",
    )?;
    let form = &launch.at("form")?.state()["presets"]["form"];
    ensure(
        form["auto_tone"] == true && form["checked"] == json!([]),
        "Auto preset form also captured concrete fields",
    )?;
    ensure(
        form["disabled"]
            .as_array()
            .is_some_and(|rows| rows.len() == 2),
        "Auto did not disable Tone and Colour",
    )?;
    checks.note(
        launch.at("form")?,
        "Auto checked; concrete Tone and Colour unchecked and disabled",
        form.clone(),
    );
    checks.write(&launch.evidence, "auto-tone", json!({"scope":"native hidden editor; eight fields, button/query/shortcut/preset parity, repeat, undo and historical refusal"}))
}
