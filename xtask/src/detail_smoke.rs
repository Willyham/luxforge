//! Detail's real editor journeys: generated controls, the picture at rest at Fit and percentage
//! views.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, plan::only},
    *,
};
use luxforge_evidence::{
    self as script, BrushStep, DoubleClickStep, MaskStep, PaintStep, PresetCreateStep, Reference,
    SliderStep, ViewStep, WorkspaceStep,
};
const MODULE: &str = "luxforge.detail";
use luxforge_core::DETAIL_EFFECT as EFFECT;
const ACTION: &str = "set-detail";
pub const FIXTURE: &str = "fixtures/generated/detail.jpg";

fn release(name: &str, parameter: &str, value: f64) -> Step {
    Step::new(name, SliderStep::new(ACTION, parameter, [value]).release())
        .commits(1)
        .no_draft()
}
fn moderate(name: &str) -> Step {
    Step::new(
        name,
        script::Step::call(
            "edit.set-detail",
            json!({"sharpening":50.0,"luminance":40.0,"colour":40.0}),
        ),
    )
    .commits(1)
    .payload(
        EFFECT,
        json!({"sharpening":50.0,"luminance":40.0,"colour":40.0}),
    )
}
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened")
            .collapsed(MODULE)
            .no_layer(EFFECT)
            .field(ACTION, "radius", "1.0"),
        Step::new(
            "basic-collapsed",
            script::Step::section("luxforge.basic", false),
        )
        .commits(0),
        Step::new("expanded", script::Step::section(MODULE, true))
            .expanded(MODULE)
            .commits(0),
        release("ancillary", "radius", 2.0).payload(EFFECT, json!({"radius":2.0})),
        Step::new(
            "drag",
            SliderStep::new(ACTION, "sharpening", [20.0, 40.0, 60.0]),
        )
        .commits(0)
        .draft(ACTION, json!({"sharpening":60.0})),
        Step::new(
            "cancel",
            SliderStep::new(ACTION, "sharpening", [60.0]).cancel(),
        )
        .commits(0)
        .no_draft(),
        release("sharpen", "sharpening", 50.0).same_layer(EFFECT, "ancillary"),
        Step::new(
            "typed",
            script::Step::Field(script::FieldStep {
                action: ACTION.into(),
                parameter: "radius".into(),
                text: "1.0".into(),
                submit: true,
            }),
        )
        .commits(1)
        .field(ACTION, "radius", "1.0"),
        release("sharpen-detail", "sharpen-detail", 50.0).field(ACTION, "sharpen-detail", "50"),
        release("sharpen-masking", "sharpen-masking", 30.0).field(ACTION, "sharpen-masking", "30"),
        release("luminance", "luminance", 30.0).field(ACTION, "luminance", "30"),
        release("luminance-detail", "luminance-detail", 65.0).field(
            ACTION,
            "luminance-detail",
            "65",
        ),
        release("colour", "colour", 30.0).field(ACTION, "colour", "30"),
        release("colour-detail", "colour-detail", 65.0).field(ACTION, "colour-detail", "65"),
        Step::new(
            "field-reset",
            DoubleClickStep {
                action: ACTION.into(),
                parameter: "sharpen-detail".into(),
                value: 60.0,
                gap_ms: 120,
            },
        )
        .commits(2)
        .no_draft()
        .field(ACTION, "sharpen-detail", "25"),
        Step::new(
            "sharpening-reset",
            script::Step::reset(MODULE, Some("Sharpening")),
        )
        .commits(1)
        .payload(
            EFFECT,
            json!({"luminance":30.0,"luminance-detail":65.0,"colour":30.0,"colour-detail":65.0}),
        )
        .same_layer(EFFECT, "ancillary"),
        Step::new(
            "noise-reset",
            script::Step::reset(MODULE, Some("Noise reduction")),
        )
        .commits(1)
        .payload(EFFECT, json!({}))
        .same_layer(EFFECT, "ancillary"),
        release("sharpen-restored", "sharpening", 50.0).payload(EFFECT, json!({"sharpening":50.0})),
        moderate("moderate").same_layer(EFFECT, "ancillary"),
        Step::new(
            "preset-created",
            PresetCreateStep {
                name: "Detail moderate".into(),
                group: None,
                groups: vec![
                    "Detail · Sharpening".into(),
                    "Detail · Noise reduction".into(),
                ],
                submit: true,
            },
        )
        .commits(0),
        Step::new("undo", script::Step::call("history.undo", json!({})))
            .commits(1)
            .payload(EFFECT, json!({"sharpening":50.0})),
        Step::new("redo", script::Step::call("history.redo", json!({})))
            .commits(1)
            .payload(
                EFFECT,
                json!({"sharpening":50.0,"luminance":40.0,"colour":40.0}),
            ),
        Step::new("picker-key", script::Step::key("w"))
            .commits(0)
            .mode("luxforge.basic"),
        Step::new("picker-escape", script::Step::key("Escape"))
            .commits(0)
            .mode(luxforge_core::POINTER_MODE),
        Step::new(
            "compare",
            script::Step::from_value(json!({"compare":"tap"})).unwrap(),
        )
        .commits(0),
        Step::new(
            "compare-back",
            script::Step::from_value(json!({"compare":"tap"})).unwrap(),
        )
        .commits(0)
        .payload(
            EFFECT,
            json!({"sharpening":50.0,"luminance":40.0,"colour":40.0}),
        )
        .same_layer(EFFECT, "ancillary"),
        Step::new(
            "compare-restored",
            script::Step::from_value(json!({"preview":"current"})).unwrap(),
        )
        .commits(0)
        .no_draft()
        .payload(
            EFFECT,
            json!({"sharpening":50.0,"luminance":40.0,"colour":40.0}),
        )
        .same_layer(EFFECT, "ancillary"),
        Step::new("reset", script::Step::reset(MODULE, None))
            .commits(1)
            .payload(EFFECT, json!({}))
            .same_layer(EFFECT, "ancillary"),
        Step::new("reset-noop", script::Step::reset(MODULE, None))
            .commits(0)
            .payload(EFFECT, json!({})),
        Step::new("preset-applied", script::Step::preset("Detail moderate"))
            .commits(1)
            .payload(
                EFFECT,
                json!({"sharpening":50.0,"luminance":40.0,"colour":40.0}),
            )
            .same_layer(EFFECT, "ancillary"),
        Step::new(
            "picker-basic-expanded",
            script::Step::section("luxforge.basic", true),
        )
        .commits(0)
        .expanded("luxforge.basic"),
        Step::new(
            "picker-warm",
            SliderStep::new("set-basic", "temperature", [40.0]).release(),
        )
        .commits(1),
        Step::new(
            "picker-mode",
            WorkspaceStep::default().mode("luxforge.basic"),
        )
        .commits(0)
        .mode("luxforge.basic"),
        Step::new("neutral-through-detail", script::Step::pick(600, 400))
            .commits(1)
            .field("set-basic", "temperature", "0")
            .field("set-basic", "tint", "0")
            .payload(luxforge_core::BASIC_EFFECT, json!({}))
            .mode(luxforge_core::POINTER_MODE)
            .no_draft(),
        Step::new("mask-mode", WorkspaceStep::default().mode("mask"))
            .commits(0)
            .mode("mask"),
        Step::new("mask-new", MaskStep::New("linear".into())).commits(0),
        Step::new(
            "mask-sweep",
            MaskStep::Sweep {
                from: [0.5, 0.3],
                to: [0.5, 0.7],
            },
        )
        .commits(0),
        Step::new("mask-apply", MaskStep::Release)
            .commits(1)
            .masks(1),
        release("masked-detail", "sharpening", 30.0),
        Step::new("overlay", WorkspaceStep::default().mask_overlay("tint")).commits(0),
        Step::new("overlay-off", WorkspaceStep::default().mask_overlay("off")).commits(0),
        Step::new(
            "brush-settings",
            MaskStep::Brush(BrushStep {
                size: Some(0.06),
                feather: Some(40.0),
                flow: Some(100.0),
                erase: Some(false),
                limit_to_colour: Some(false),
                ..Default::default()
            }),
        )
        .commits(0),
        Step::new("brush-armed", MaskStep::Paint(PaintStep::NewBrush)).commits(0),
        Step::new(
            "brush-painted",
            MaskStep::stroke([[0.25, 0.20], [0.40, 0.20]], true),
        )
        .commits(1)
        .masks(1)
        .no_draft(),
        Step::new("brush-first-done", MaskStep::Apply).commits(0),
        Step::new(
            "brush-before",
            WorkspaceStep::default().mask_overlay("mask-on-black"),
        )
        .commits(0),
        Step::new(
            "brush-colour-settings",
            MaskStep::Brush(BrushStep {
                erase: Some(true),
                limit_to_colour: Some(true),
                colour_refine: Some(50.0),
                ..Default::default()
            }),
        )
        .commits(0),
        Step::new(
            "brush-colour-armed",
            MaskStep::Paint(PaintStep::Component(Reference::Index(1))),
        )
        .commits(0),
        Step::new(
            "brush-colour-through-detail",
            MaskStep::stroke([[0.25, 0.20], [0.40, 0.20]], true),
        )
        .commits(1)
        .masks(1)
        .no_draft(),
        Step::new("brush-done", MaskStep::Apply).commits(0),
        Step::new(
            "brush-after",
            WorkspaceStep::default().mask_overlay("mask-on-black"),
        )
        .commits(0),
        Step::new("brush-list", script::Step::api("mask.list")).commits(0),
        Step::new(
            "brush-input",
            script::Step::call(
                "mask.sample-input",
                json!({"mask":{"name":"Mask 1"},"x":600,"y":320}),
            ),
        )
        .commits(0),
        Step::new(
            "brush-overlay-off",
            WorkspaceStep::default().mask_overlay("off"),
        )
        .commits(0),
    ])
}
pub fn fit_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_layer(EFFECT),
        moderate("moderate"),
        Step::new(
            "downstream-drag",
            SliderStep::new("set-basic", "exposure", [0.2, 0.4, 0.6]),
        )
        .commits(0)
        .draft("set-basic", json!({"exposure":0.6}))
        .field("set-basic", "exposure", "0.60"),
        Step::new("quiet", script::Step::Wait { ms: 10000 })
            .commits(0)
            .draft("set-basic", json!({"exposure":0.6})),
        Step::new(
            "release",
            SliderStep::new("set-basic", "exposure", [0.6]).release(),
        )
        .commits(1)
        .payload(luxforge_core::BASIC_EFFECT, json!({"exposure":0.6}))
        .no_draft(),
        Step::new(
            "panels",
            script::Step::from_value(json!({"workspace":{"state_panel":false}})).unwrap(),
        )
        .commits(0),
        Step::new(
            "panels-restored",
            script::Step::from_value(json!({"workspace":{"state_panel":true}})).unwrap(),
        )
        .commits(0),
        Step::new(
            "original",
            script::Step::from_value(json!({"preview":{"sequence":0}})).unwrap(),
        )
        .commits(0),
        Step::new(
            "current",
            script::Step::from_value(json!({"preview":"current"})).unwrap(),
        )
        .commits(0),
    ])
}
pub fn zoom_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        moderate("moderate"),
        Step::new("100", ViewStep::Percent(100.0))
            .commits(0)
            .percent(100.0),
        Step::new(
            "pan",
            script::Step::from_value(json!({"pan":{"x":0.25,"y":0.25}})).unwrap(),
        )
        .commits(0),
        Step::new("200", ViewStep::Percent(200.0))
            .commits(0)
            .percent(200.0),
        Step::new(
            "detail-drag",
            SliderStep::new(ACTION, "sharpening", [60.0, 70.0]),
        )
        .commits(0)
        .draft(ACTION, json!({"sharpening":70.0})),
        Step::new("quiet", script::Step::Wait { ms: 10000 }).commits(0),
        release("release", "sharpening", 70.0),
        Step::new("fit", ViewStep::Fit).commits(0).fit(),
    ])
}
pub fn raw_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        moderate("moderate"),
        release("amount", "sharpening", 60.0),
        Step::new("100", ViewStep::Percent(100.0))
            .commits(0)
            .percent(100.0),
        Step::new("fit", ViewStep::Fit).commits(0).fit(),
        Step::new(
            "white-balance",
            script::Step::call("edit.set-raw", json!({"temperature":6500.0})),
        )
        .commits(1),
        Step::new(
            "as-shot",
            script::Step::call("edit.set-raw", json!({"white-balance":"as-shot"})),
        )
        .commits(1),
        Step::new(
            "crop",
            script::Step::call("edit.crop-fit", json!({"aspect":"4:3","angle":5.0})),
        )
        .commits(1),
        Step::new(
            "rotate",
            script::Step::call("edit.transform", json!({"transform":"rotate-right"})),
        )
        .commits(1),
        Step::new("reset", script::Step::reset(MODULE, None))
            .commits(1)
            .payload(EFFECT, json!({})),
    ])
}
fn pixel_difference(a: &Frame, b: &Frame) -> Result<(f64, u8)> {
    let ar = a.photo()?;
    let br = b.photo()?;
    ensure(
        ar[2] - ar[0] == br[2] - br[0] && ar[3] - ar[1] == br[3] - br[1],
        "photo sizes differ",
    )?;
    let (ai, bi) = (a.image()?, b.image()?);
    let mut sum = 0.0;
    let mut n = 0;
    let mut maximum = 0;
    for y in 0..ar[3] - ar[1] {
        for x in 0..ar[2] - ar[0] {
            let ap = ai.get_pixel(ar[0] + x, ar[1] + y).0;
            let bp = bi.get_pixel(br[0] + x, br[1] + y).0;
            for c in 0..3 {
                let difference = ap[c].abs_diff(bp[c]);
                maximum = maximum.max(difference);
                sum += f64::from(difference);
                n += 1;
            }
        }
    }
    Ok((sum / f64::from(n), maximum))
}
fn difference(a: &Frame, b: &Frame) -> Result<f64> {
    Ok(pixel_difference(a, b)?.0)
}
fn current_photo(frame: &Frame) -> Result {
    frame.visible_photo()?;
    let gpu = &frame.state()["surface"]["gpu"];
    ensure(
        gpu["drawn_photo_blank"] == false
            && gpu["drawn_stale_photo"] == false
            && gpu["blank_photo_draws"] == 0
            && gpu["stale_photo_draws"] == 0,
        format!("Detail's GPU draw was blank or stale: {gpu}"),
    )
}
/// The picture at rest at Fit is the GPU's render of the current stack — the stack at full
/// resolution in tiles reduced to the view, or its view plan where the view draws it at its own
/// size — the status bar naming it, over the current stack's adopted analysis.
fn rest_fit(frame: &Frame) -> Result {
    current_photo(frame)?;
    let state = frame.state();
    let gpu = &state["surface"]["gpu"];
    let render = &state["status_bar"]["render"];
    ensure(
        gpu["drawing_path"] == "gpu"
            && (gpu["picture"] == "rest" || gpu["picture"] == "view")
            && render
                .as_str()
                .is_some_and(|text| text.starts_with("GPU render"))
            && state["approximate_white_balance"] == false
            && state["surface"]["detail_updating"] == false
            && state["histogram"]["stale"] == false
            && state["stack"]["displayed"]["entry"] == state["stack"]["entry"],
        format!(
            "Fit's picture at rest is not the GPU's render of the current stack: path {}, picture \
             {}, {render}",
            gpu["drawing_path"], gpu["picture"]
        ),
    )
}
fn displayed_dimensions(frame: &Frame) -> Result<[u64; 2]> {
    let dimensions = &frame.state()["stack"]["displayed"]["dimensions"];
    let width = dimensions[0].as_u64().ok_or("no displayed stage width")?;
    let height = dimensions[1].as_u64().ok_or("no displayed stage height")?;
    ensure(width > 0 && height > 0, "the displayed stage is empty")?;
    Ok([width, height])
}
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    let opened = launch.at("opened")?;
    opened.module_available(MODULE)?;
    for name in ["ancillary", "noise-reset", "reset", "reset-noop"] {
        let frame = launch.at(name)?;
        checks.compare(
            frame,
            "neutral strengths retain original rendered pixels",
            difference(opened, frame)?,
            0.0,
            Tolerance::Within(0.0),
        )?;
    }
    let frame = launch.at("moderate")?;
    checks.compare(
        frame,
        "active Detail changes rendered pixels",
        difference(opened, frame)?,
        0.0,
        Tolerance::Beyond(0.001),
    )?;
    for (name, reference) in [("undo", "sharpen-restored"), ("redo", "moderate")] {
        checks.compare(
            launch.at(name)?,
            "native history restores the expected Detail photograph",
            difference(launch.at(name)?, launch.at(reference)?)?,
            0.0,
            Tolerance::Within(0.0),
        )?;
    }
    let comparison = &launch.at("compare")?.state()["comparison"];
    ensure(
        launch.at("compare")?.state()["compare"] == true
            && comparison["position"] == 0.5
            && comparison["after_entry"] == json!(launch.at("redo")?.entry()?),
        format!("Compare did not bind to the restored Detail entry: {comparison}"),
    )?;
    let back = launch.at("compare-back")?;
    ensure(
        back.state()["compare"] == false
            && back.state()["comparison"].is_null()
            && back.entry()? == launch.at("redo")?.entry()?,
        "Compare did not exit without changing the current Detail entry",
    )?;
    if back.state()["stack"]["displayed"]["entry"] != json!(back.entry()?) {
        ensure(
            back.state()["stack"]["displayed"]["entry"] == json!(opened.entry()?)
                && back.state()["reference"]["reduced"] == false
                && back.state()["histogram"]["stale"] == true
                && back.status()?.contains("Rendering selected history state"),
            "Compare's retained Original was not clearly marked updating",
        )?;
        checks.note(
            back,
            "Compare has exited while the coherent Original awaits restored current pixels",
            back.state()["reference"].clone(),
        );
    }
    let restored = launch.at("compare-restored")?;
    rest_fit(restored)?;
    ensure(
        restored.state()["compare"] == false
            && restored.state()["comparison"].is_null()
            && restored.entry()? == launch.at("redo")?.entry()?,
        "returning to current after Compare changed its entry or comparison state",
    )?;
    checks.compare(
        restored,
        "returning to current after Compare restores exact Detail pixels",
        difference(frame, restored)?,
        0.0,
        Tolerance::Within(0.0),
    )?;
    checks.compare(
        launch.at("preset-applied")?,
        "native preset restores the captured Detail pixels",
        difference(frame, launch.at("preset-applied")?)?,
        0.0,
        Tolerance::Within(0.0),
    )?;
    let picked = launch.at("neutral-through-detail")?;
    checks.compare(
        picked,
        "neutral picker through active Detail restores the uncast photograph",
        difference(launch.at("preset-applied")?, picked)?,
        0.0,
        Tolerance::Within(0.0),
    )?;
    for name in ["masked-detail", "overlay", "overlay-off"] {
        let frame = launch.at(name)?;
        current_photo(frame)?;
        checks.note(
            frame,
            "masked Detail and its overlay share the accepted recipe",
            frame.state()["masks"].clone(),
        );
    }
    let masked = launch.at("masked-detail")?;
    ensure(
        masked.state()["stack"]["layers"]
            .as_array()
            .is_some_and(|layers| {
                layers.iter().any(|layer| {
                    layer["effect"] == EFFECT
                        && !layer["mask"].is_null()
                        && layer["payload"] == json!({"sharpening":30.0})
                })
            }),
        "masked Detail did not store its strength on the masked layer",
    )?;
    let constrained = launch.at("brush-colour-through-detail")?;
    ensure(
        constrained.state()["masks"]["brush"]["limit_to_colour"] == true,
        "the brush did not retain its colour limit",
    )?;
    let stroke = &constrained.component(1)?["strokes"][1];
    ensure(
        stroke["summary"]
            .as_str()
            .is_some_and(|text| text.contains("colour-held")),
        "the stored stroke row is not colour-held",
    )?;
    let listed = &launch.at("brush-list")?["step"]["result"]["masks"][0]["components"][1]["strokes"]
        [1]["settings"]["colour"];
    let input = &launch.at("brush-input")?["step"]["result"];
    ensure(
        input["renderer"] == json!({"record": "gpu", "reason": null}),
        format!("mask.sample-input through Detail is not the GPU's: {input}"),
    )?;
    let mut expected = [0_u8; 3];
    for (code, channel) in expected.iter_mut().zip(["r", "g", "b"]) {
        *code = luxforge_reference::srgb::code(
            input[channel]
                .as_f64()
                .ok_or("the mask input has no linear channel")?,
        );
    }
    ensure(
        listed["seed"] == json!(expected),
        format!("the stored brush seed {listed} differs from its Detail input {input}"),
    )?;
    checks.compare(
        launch.at("brush-after")?,
        "the colour-held erase changes the rendered brush coverage",
        difference(launch.at("brush-before")?, launch.at("brush-after")?)?,
        0.0,
        Tolerance::Beyond(0.01),
    )?;
    current_photo(launch.at("brush-overlay-off")?)?;
    checks.note(
        constrained,
        "colour-held brush reads the masked Detail input",
        json!({"stored":listed,"input":input}),
    );
    checks.write(&launch.evidence,"detail",json!({"scope":"All eight sliders, typed values, field/group/module resets, history, Compare, presets, neutral pick and colour-held brush through Detail, masked coverage and captured photo correlation. Native Copy as JSON and focused keyboard increments remain unproven; photographic qualification remains incomplete and is assessed separately."}))
}
pub fn verify_fit(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    let moving = launch.at("downstream-drag")?;
    current_photo(moving)?;
    moving.displays_draft()?;
    checks.compare(
        moving,
        "the downstream exposure draft changes the Detail photograph",
        difference(launch.at("moderate")?, moving)?,
        0.0,
        Tolerance::Beyond(0.001),
    )?;
    let drawn_on_gpu = moving.drawn_on_gpu();
    if drawn_on_gpu {
        checks.note(
            moving,
            "the GPU preview drew the downstream draft over Detail from the held boundary",
            moving.state()["surface"]["gpu"]["gpu_preview"].clone(),
        );
    } else {
        // A tick the GPU did not draw: the reference renderer's whole frame of the draft, never
        // a moving approximation.
        current_photo(moving)?;
        ensure(
            !moving.status()?.contains("approximate"),
            "the downstream draft was labelled approximate",
        )?;
        checks.note(
            moving,
            "the reference renderer drew the downstream draft's whole frame",
            moving.state()["reference"].clone(),
        );
    }
    launch.at("quiet")?.displays_draft()?;
    for name in [
        "moderate",
        "quiet",
        "release",
        "panels",
        "panels-restored",
        "current",
    ] {
        let frame = launch.at(name)?;
        if name == "quiet" {
            continue;
        }
        rest_fit(frame)?;
        checks.note(
            frame,
            "the picture at rest at Fit is the GPU's, the stack at full resolution reduced to the view",
            frame.state()["surface"]["gpu"]["rest"].clone(),
        );
    }
    checks.compare(
        launch.at("current")?,
        "returning to the current version draws the release's picture at rest again, byte for byte",
        difference(launch.at("release")?, launch.at("current")?)?,
        0.0,
        Tolerance::Within(0.0),
    )?;
    checks.write(&launch.evidence,"detail-fit",json!({"moving_drawn_on_gpu":drawn_on_gpu,"scope":"Downstream draft pixels in motion, on the GPU or the reference's whole frame, the picture at rest on the GPU and view-bound changes; reducer and stale adoption are exact unit proofs."}))
}
pub fn verify_zoom(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    for name in ["100", "pan", "200", "quiet", "release", "fit"] {
        let frame = launch.at(name)?;
        current_photo(frame)?;
        checks.note(
            frame,
            "Detail percentage view presents matched photo content",
            frame.state()["surface"].clone(),
        );
    }
    for name in ["quiet", "release"] {
        let frame = launch.at(name)?;
        let state = frame.state();
        let surface = &state["surface"];
        ensure(
            surface["detail_updating"] == false
                && (surface["gpu"]["drawing_path"] == "gpu"
                    || state["reference"]["reduced"] == false),
            format!("{name} did not present full Detail at 100%: {surface}"),
        )?;
        ensure(
            state["approximate_white_balance"] == false,
            "An approximate frame was accepted as full Detail at 100%",
        )?;
    }
    ensure(
        launch.at("release")?.state()["histogram"]["stale"] == false,
        "Release did not adopt its exact full-stage histogram",
    )?;
    // Back at Fit the picture at rest is the GPU's again, planned for the view it returns to.
    rest_fit(launch.at("fit")?)?;
    checks.write(&launch.evidence,"detail-zoom",json!({"scope":"Native percentage views and pan with correlated state, the GPU's region or the reference's whole frame; the GPU's agreement with the reference is the release gate's."}))
}
pub fn verify_raw(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    for name in [
        "opened",
        "moderate",
        "amount",
        "100",
        "fit",
        "white-balance",
        "as-shot",
        "crop",
        "rotate",
        "reset",
    ] {
        let frame = launch.at(name)?;
        current_photo(frame)?;
        checks.note(
            frame,
            "RAW Detail controls and presentation",
            frame.state()["surface"].clone(),
        );
    }
    for name in [
        "amount",
        "fit",
        "white-balance",
        "as-shot",
        "crop",
        "rotate",
    ] {
        rest_fit(launch.at(name)?)?;
    }
    let moderate = launch.at("moderate")?;
    let moderate_state = moderate.state();
    if moderate_state["histogram"]["stale"] == false {
        rest_fit(moderate)?;
    } else {
        let status = moderate.status()?;
        ensure(
            moderate_state["approximate_white_balance"] == false,
            "the in-flight moderate RAW frame approximated its white balance",
        )?;
        ensure(
            moderate_state["stack"]["displayed"]["entry"] != moderate_state["stack"]["entry"]
                && status.contains("Rendering selected history state"),
            "a retained RAW photograph was not marked updating",
        )?;
        checks.note(
            moderate,
            "moderate RAW capture is still updating; exact acceptance is checked at Amount and Fit",
            moderate_state["reference"].clone(),
        );
    }
    let expected_detail = json!({"sharpening":60.0,"luminance":40.0,"colour":40.0});
    for name in [
        "amount",
        "100",
        "fit",
        "white-balance",
        "as-shot",
        "crop",
        "rotate",
    ] {
        let frame = launch.at(name)?;
        ensure(
            frame.payload(EFFECT) == Some(&expected_detail)
                && frame.layer_id(EFFECT) == launch.at("moderate")?.layer_id(EFFECT),
            format!("{name} did not retain the active Detail layer and all three strengths"),
        )?;
    }
    let percentage = launch.at("100")?.state();
    ensure(
        percentage["surface"]["detail_updating"] == false
            && percentage["approximate_white_balance"] == false
            && (percentage["surface"]["gpu"]["drawing_path"] == "gpu"
                || percentage["reference"]["reduced"] == false),
        "RAW 100% did not adopt exact Detail content",
    )?;
    let opened = launch.at("opened")?;
    let fit = launch.at("fit")?;
    checks.compare(
        fit,
        "active Detail changes the supplied RAW photograph",
        difference(opened, fit)?,
        0.0,
        Tolerance::Beyond(0.001),
    )?;
    let white_balance = launch.at("white-balance")?;
    let custom = white_balance
        .payload("luxforge.raw")
        .ok_or("the supplied photograph has no RAW development layer")?;
    let original = opened
        .payload("luxforge.raw")
        .ok_or("the opened photograph has no RAW development layer")?;
    ensure(
        custom["wb_mode"] == "custom"
            && custom["temperature_kelvin"] == 6500.0
            && custom["gains"] != original["gains"],
        format!("RAW white balance did not adopt 6500 K: {custom}"),
    )?;
    checks.compare(
        white_balance,
        "6500 K changes the exact RAW photograph through Detail",
        difference(fit, white_balance)?,
        0.0,
        Tolerance::Beyond(0.001),
    )?;
    let as_shot = launch.at("as-shot")?;
    ensure(
        as_shot.payload("luxforge.raw") == Some(original),
        "As shot did not restore the opened RAW gains and white-balance mode",
    )?;
    checks.compare(
        as_shot,
        "As shot restores the preceding exact Fit photograph",
        difference(fit, as_shot)?,
        0.0,
        Tolerance::Within(0.0),
    )?;
    // The Amount capture precedes the view's 100%-then-Fit cycle. Native texture sampling can
    // round that earlier draw by one code; the returned Fit above remains a byte-exact oracle.
    let (amount_mean, amount_maximum) = pixel_difference(launch.at("amount")?, as_shot)?;
    ensure(
        amount_maximum <= 1,
        format!("As shot differs from the pre-zoom Amount draw by {amount_maximum} GPU codes"),
    )?;
    checks.note(
        as_shot,
        "As shot agrees with the pre-zoom Amount draw within one GPU code per channel",
        json!({"mean_codes":amount_mean,"maximum_codes":amount_maximum,"scope":"native display readback; no core image tolerance"}),
    );
    let crop = launch.at("crop")?;
    let section = &crop.state()["crop"]["section"];
    let cropped = displayed_dimensions(crop)?;
    let uncropped = displayed_dimensions(as_shot)?;
    ensure(
        section["chosen"] == "4:3"
            && section["rail"] == 5.0
            && section["angle"] == "5.0"
            && cropped != uncropped
            && cropped[0] * cropped[1] < uncropped[0] * uncropped[1]
            && (cropped[0] as f64 / cropped[1] as f64 - 4.0 / 3.0).abs() < 0.001,
        format!(
            "RAW crop did not adopt the angled 4:3 stage: {section}, {uncropped:?} -> {cropped:?}"
        ),
    )?;
    let rotated = launch.at("rotate")?;
    ensure(
        displayed_dimensions(rotated)? == [cropped[1], cropped[0]]
            && rotated
                .payload(luxforge_core::ORIENTATION_EFFECT)
                .is_some_and(|payload| payload["turns"] == 1 && payload["mirror"] == false),
        "RAW rotation did not adopt the rotated cropped stage",
    )?;
    checks.write(
        &launch.evidence,
        "raw-detail",
        json!({"scope":"Supplied RAW Detail pixel changes, exact Fit and percentage content, 6500 K and restored As shot pixels, angled crop and rotated stage; no controlled quality claim or pending-work stress qualification."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detail_journeys_are_valid_evidence_scripts() {
        for create in [plan, fit_plan, zoom_plan, raw_plan] {
            let plan = create(&[]);
            let _script =
                script::parse(&plan.script().to_string()).expect("all scripted journeys validate");
        }
    }
}
