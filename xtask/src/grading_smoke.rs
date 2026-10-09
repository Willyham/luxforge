//! The `grading` smoke scenario: the Colour mixer's Grading tab on the real editor, over the mixer
//! scenario's generated hue wheel, whose dark backdrop is shadow and whose white centre is
//! highlight.
//!
//! It opens the Grading tab (session state only), drags the Shadows wheel of the 3-way view as one
//! draft and commits it as one entry, types a luminance, drags through the wheel's centre to keep
//! the hue at zero saturation, lets another client restore the saturation and set a Global tint,
//! switches to the Global view and back without a frame or an entry, and resets Grading while an
//! HSL edit stays. Its checks
//! correlate the history, the stored payload, the wheels' models, the session's view selection and
//! the rendered backdrop's colour.
use crate::{
    scenario::{
        Checked, Checks, Frame, Plan, Run, Step, Tolerance, asked_no_frame, drawn_wheel, pixels,
        plan::only, selected_view,
    },
    *,
};
use luxforge_core::MIXER_EFFECT;
use luxforge_evidence::{self as script, ControlsStep, SliderEnd, TabStep};

const MODULE: &str = "luxforge.mixer";
const BASIC_MODULE: &str = "luxforge.basic";
const ACTION: &str = "set-mixer";
const SHADOWS_HUE: &str = "grade-shadows-hue";
const SHADOWS_SATURATION: &str = "grade-shadows-saturation";
const SHADOWS_LUMINANCE: &str = "grade-shadows-luminance";
const GLOBAL_HUE: &str = "grade-global-hue";
const GLOBAL_SATURATION: &str = "grade-global-saturation";
const GRADING: &str = "Grading";
/// The mixer scenario's generated hue wheel.
pub const FIXTURE: &str = crate::mixer_smoke::FIXTURE;

/// The module's tab row: HSL then Grading.
fn module_tab(index: usize) -> script::Step {
    script::Step::Tab(TabStep {
        module: MODULE.into(),
        group: Vec::new(),
        index,
    })
}

/// The Grading tab's own row: 3-way, Shadows, Midtones, Highlights, Global.
fn grading_view(index: usize) -> script::Step {
    script::Step::Tab(TabStep {
        module: MODULE.into(),
        group: vec![GRADING.into()],
        index,
    })
}

/// One gesture on the Shadows wheel through `positions`, offsets from its centre in radii, screen
/// y down: `[0.5, 0]` is red at saturation 50 and `[-0.25, -0.433]` green (120°) at 50.
fn shadows_wheel(positions: Vec<[f32; 2]>, finish: SliderEnd) -> script::Step {
    script::Step::Controls(ControlsStep::Wheel {
        action: ACTION.into(),
        hue: SHADOWS_HUE.into(),
        positions,
        shift: false,
        command: false,
        option: false,
        finish,
    })
}

pub fn plan(_: &[PathBuf]) -> Plan {
    let view = |name: &str, script: script::Step| Step::new(name, script).commits(0);
    Plan::new(vec![
        Step::opened("opened")
            .collapsed(MODULE)
            .no_draft()
            .no_layer(MIXER_EFFECT),
        view(
            "basic-collapsed",
            script::Step::section(BASIC_MODULE, false),
        )
        .collapsed(BASIC_MODULE),
        view("expanded", script::Step::section(MODULE, true)).expanded(MODULE),
        // The Grading tab, starting on its 3-way view.
        view("grading", module_tab(1)),
        // A Shadows wheel drag, left open: both fields in one draft.
        view(
            "drag",
            shadows_wheel(vec![[0.5, 0.0], [-0.25, -0.433]], SliderEnd::Open),
        )
        .draft(
            ACTION,
            json!({SHADOWS_HUE: 120.0, SHADOWS_SATURATION: 50.0}),
        )
        .no_layer(MIXER_EFFECT),
        // Released: one entry for the two fields.
        Step::new(
            "release",
            shadows_wheel(vec![[-0.25, -0.433]], SliderEnd::Release),
        )
        .no_draft()
        .commits(1)
        .label("Shadows tint")
        .payload(
            MIXER_EFFECT,
            json!({SHADOWS_HUE: 120.0, SHADOWS_SATURATION: 50.0}),
        ),
        // A typed luminance on the same wheel's rail.
        Step::new(
            "luminance",
            script::Step::field(ACTION, SHADOWS_LUMINANCE, "40", true),
        )
        .commits(1)
        .label("Shadows luminance +40")
        .same_layer(MIXER_EFFECT, "release"),
        // Through the centre: the hue is kept as a setting at zero saturation.
        Step::new(
            "centre",
            shadows_wheel(vec![[0.0, 0.0]], SliderEnd::Release),
        )
        .commits(1)
        .label("Shadows tint")
        .payload(
            MIXER_EFFECT,
            json!({SHADOWS_HUE: 120.0, SHADOWS_LUMINANCE: 40.0}),
        )
        .field(ACTION, SHADOWS_HUE, "120")
        .field(ACTION, SHADOWS_SATURATION, "0"),
        // The tint back, for the reset to undo, from another client: the same control again by
        // the same actor would collapse into the centre entry (auto collapse history).
        Step::new(
            "retint",
            script::Step::agent(format!("edit.{ACTION}"), json!({SHADOWS_SATURATION: 50.0})),
        )
        .commits(1)
        .label("Shadows saturation 50"),
        // The Global view and back: view state alone.
        view("global-view", grading_view(4)),
        // Another client's Global tint moves the Global wheel.
        Step::new(
            "external",
            script::Step::agent(
                format!("edit.{ACTION}"),
                json!({GLOBAL_HUE: 220.0, GLOBAL_SATURATION: 30.0}),
            ),
        )
        .commits(1)
        .label("Global tint"),
        view("three-way-view", grading_view(0)),
        // An HSL edit, which Reset Grading keeps.
        Step::new(
            "hsl",
            script::Step::agent(format!("edit.{ACTION}"), json!({"red-hue": 20.0})),
        )
        .commits(1)
        .label("Red hue +20"),
        Step::new("reset-grading", script::Step::reset(MODULE, Some(GRADING)))
            .commits(1)
            .label(format!("Reset {GRADING}"))
            .payload(MIXER_EFFECT, json!({"red-hue": 20.0}))
            .same_layer(MIXER_EFFECT, "release")
            .field(ACTION, SHADOWS_SATURATION, "0"),
    ])
}

/// The backdrop's mean colour: a patch near the photograph's top-left corner, outside the wheel,
/// where the fixture is a dark near-grey.
fn backdrop(frame: &Frame) -> Result<[f64; 3]> {
    let [left, top, _, _] = frame.photo()?;
    pixels::mean_rgb(
        frame.image()?,
        (f64::from(left) + 12.0, f64::from(top) + 12.0),
        4,
    )
}

/// How much greener than the mean of red and blue a colour is.
fn green_lead(rgb: [f64; 3]) -> f64 {
    rgb[1] - (rgb[0] + rgb[2]) / 2.0
}

pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    let at = |step: &str| launch.at(step);
    at("opened")?.module_available(MODULE)?;
    at("drag")?.displays_draft()?;

    // The green shadow tint reaches the dark backdrop while drafted and once committed, and the
    // Grading reset takes it away again.
    let opened = green_lead(backdrop(at("opened")?)?);
    for (step, what) in [("drag", "drafted"), ("release", "committed")] {
        let frame = at(step)?;
        checks.compare(
            frame,
            &format!("the {what} green shadow tint on the dark backdrop (green lead over opened)"),
            green_lead(backdrop(frame)?) - opened,
            0.0,
            Tolerance::Above(3.0),
        )?;
    }
    let reset = at("reset-grading")?;
    checks.compare(
        reset,
        "Reset Grading takes the shadow tint off the backdrop (green lead against opened)",
        (green_lead(backdrop(reset)?) - opened).abs(),
        0.0,
        Tolerance::Under(2.0),
    )?;

    // The wheels' models follow the committed and external values; the centre keeps the hue.
    let wheel = |step: &str, hue: &str| -> Result<Value> {
        drawn_wheel(at(step)?.state(), hue)
            .cloned()
            .ok_or_else(|| format!("The {step} frame draws no {hue} wheel").into())
    };
    ensure(
        wheel("drag", SHADOWS_HUE)?["dragging"] == true,
        "The dragged Shadows wheel is not drawn as dragging",
    )?;
    ensure(
        wheel("centre", SHADOWS_HUE)?["hue"] == json!(120.0)
            && wheel("centre", SHADOWS_HUE)?["saturation"] == json!(0.0),
        "A drag through the centre did not keep the Shadows hue",
    )?;
    ensure(
        wheel("external", GLOBAL_HUE)?["hue"] == json!(220.0),
        "Another client's Global tint did not move the Global wheel",
    )?;

    // View selection is session state: recorded, drawn, and never a frame.
    let global = at("global-view")?.state();
    ensure(
        selected_view(global, MODULE, &[] as &[&str], GRADING)
            && selected_view(global, MODULE, &[GRADING], "Global"),
        format!(
            "The session did not record the Global view: {}",
            global["workspace"]["views"]
        ),
    )?;
    ensure(
        selected_view(at("three-way-view")?.state(), MODULE, &[GRADING], "3-way"),
        "The session did not record the 3-way view",
    )?;
    // The views draw differently: Global's one large wheel replaces the 3-way view's three, and
    // back. A tab row drawn as stacked groups would leave the panel unchanged.
    for (step, before) in [("global-view", "retint"), ("three-way-view", "external")] {
        ensure(
            crate::controls_smoke::sidebar_difference(at(before)?, at(step)?)? >= 100,
            format!("Selecting the view in {step} did not change the tools panel"),
        )?;
    }
    for (step, before) in [("grading", "expanded"), ("global-view", "retint")] {
        ensure(
            asked_no_frame(at(before)?.state(), at(step)?.state()),
            format!("Selecting the view in {step} asked for a frame"),
        )?;
    }

    checks.write(
        &launch.evidence,
        "grading",
        json!({
            "scope": "Mean RGB of a backdrop patch the editor records drawing, the wheels' models and the session's view selection; a correlation of controls, history and rendering, not a colorimetric claim",
        }),
    )?;
    Ok(())
}
