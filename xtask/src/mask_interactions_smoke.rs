//! Native regressions for unplaced creation, truthful live coverage and selected brush targets.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, Tolerance, plan::only},
    *,
};
use luxforge_evidence::{
    self as script, BrushStep, MaskStep, PaintStep, Reference, ViewStep, WorkspaceStep,
};
use luxforge_reference::mask::{self as reference, Linear, Radial, Stage};

pub const SCENARIO: &str = "mask-interactions";
pub const FIXTURE: &str = "docs/design/develop-workspace/html/sapa.jpg";
pub const NOTE: &str = "Creates disjoint masks, reads live and committed coverage from native renderer captures, switches targets after arming a brush, checks deliberate overlay hiding, and compares live linear/radial gradients at Fit and 100% under an 8-degree crop with the independent analytic coverage reference.";

const TRANSFORMED_LINEAR: [[f64; 2]; 2] = [[0.35, 0.35], [0.65, 0.65]];
const TRANSFORMED_RADIAL: [[f64; 2]; 2] = [[0.5, 0.5], [0.75, 0.75]];

fn quiet(name: &str, step: impl Into<script::Step>) -> Step {
    Step::new(name, step).commits(0).mode("mask")
}
fn commit(name: &str, step: impl Into<script::Step>) -> Step {
    Step::new(name, step).commits(1).mode("mask").no_draft()
}
fn named(name: &str) -> Reference {
    Reference::name(name)
}
fn rename(step: &str, name: &str) -> Step {
    commit(
        step,
        script::Step::call("mask.rename", json!({"mask":{"name":"Mask 1"},"name":name})),
    )
}

pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened").masks(0).no_draft(),
        quiet("mask-mode", WorkspaceStep::default().mode("mask")),
        quiet("linear-unplaced", MaskStep::New("linear".into()))
            .no_draft()
            .masks(0),
        quiet(
            "linear-live",
            MaskStep::Sweep {
                from: [0.5, 0.32],
                to: [0.5, 0.12],
            },
        ),
        quiet(
            "linear-black",
            WorkspaceStep::default().mask_overlay("mask-on-black"),
        ),
        commit("linear-committed", MaskStep::Release).masks(1),
        rename("name-a", "A"),
        quiet("radial-unplaced", MaskStep::New("radial".into()))
            .no_draft()
            .masks(1),
        quiet(
            "radial-live",
            MaskStep::Sweep {
                from: [0.5, 0.75],
                to: [0.75, 0.95],
            },
        ),
        commit("radial-committed", MaskStep::Release).masks(2),
        rename("name-b", "B"),
        quiet("select-a", MaskStep::Select(named("A"))).open_mask(Some("A")),
        quiet("select-b", MaskStep::Select(named("B"))).open_mask(Some("B")),
        quiet("arm-b", MaskStep::Paint(PaintStep::NewBrush)),
        quiet("switch-a-disarms", MaskStep::Select(named("A"))).open_mask(Some("A")),
        quiet("arm-a", MaskStep::Paint(PaintStep::NewBrush)),
        commit(
            "stroke-a",
            MaskStep::stroke([[0.42, 0.45], [0.58, 0.45]], true),
        )
        .open_mask(Some("A")),
        quiet("brush-done", MaskStep::Apply),
        quiet("new-brush", MaskStep::Paint(PaintStep::NewMask)),
        quiet(
            "brush-settings",
            MaskStep::Brush(BrushStep {
                size: Some(0.10),
                feather: Some(100.0),
                flow: Some(25.0),
                ..Default::default()
            }),
        ),
        quiet(
            "brush-live",
            MaskStep::stroke([[0.25, 0.55], [0.65, 0.55], [0.25, 0.55]], false),
        ),
        commit("brush-committed", MaskStep::Apply).masks(3),
        quiet("new-gradient", MaskStep::New("radial".into()))
            .no_draft()
            .masks(3),
        quiet("manual-hide", script::Step::Key { key: "o".into() }),
        quiet(
            "hidden-live",
            MaskStep::Sweep {
                from: [0.5, 0.5],
                to: [0.72, 0.72],
            },
        ),
        quiet("manual-show", script::Step::Key { key: "o".into() }),
        quiet("cancel-gradient", MaskStep::Cancel)
            .no_draft()
            .masks(3),
        commit(
            "rotated-crop",
            script::Step::call("edit.crop-fit", json!({"aspect":"16:9","angle":8.0})),
        ),
        quiet("rotated-transform", script::Step::api("render.transform")),
        quiet(
            "rotated-fit-black",
            WorkspaceStep::default().mask_overlay("mask-on-black"),
        )
        .fit(),
        quiet("rotated-fit-linear-new", MaskStep::New("linear".into())).no_draft(),
        quiet(
            "rotated-fit-linear-live",
            MaskStep::Sweep {
                from: TRANSFORMED_LINEAR[0],
                to: TRANSFORMED_LINEAR[1],
            },
        )
        .fit(),
        quiet("rotated-fit-linear-cancel", MaskStep::Cancel)
            .no_draft()
            .masks(3),
        quiet("rotated-fit-radial-new", MaskStep::New("radial".into())).no_draft(),
        quiet(
            "rotated-fit-radial-live",
            MaskStep::Sweep {
                from: TRANSFORMED_RADIAL[0],
                to: TRANSFORMED_RADIAL[1],
            },
        )
        .fit(),
        quiet("rotated-fit-radial-cancel", MaskStep::Cancel)
            .no_draft()
            .masks(3),
        quiet("rotated-100", ViewStep::Percent(100.0)).percent(100.0),
        quiet("rotated-100-linear-new", MaskStep::New("linear".into())).no_draft(),
        quiet(
            "rotated-100-linear-live",
            MaskStep::Sweep {
                from: TRANSFORMED_LINEAR[0],
                to: TRANSFORMED_LINEAR[1],
            },
        )
        .percent(100.0),
        quiet("rotated-100-linear-cancel", MaskStep::Cancel)
            .no_draft()
            .masks(3),
        quiet("rotated-100-radial-new", MaskStep::New("radial".into())).no_draft(),
        quiet(
            "rotated-100-radial-live",
            MaskStep::Sweep {
                from: TRANSFORMED_RADIAL[0],
                to: TRANSFORMED_RADIAL[1],
            },
        )
        .percent(100.0),
        commit("rotated-100-radial-apply", MaskStep::Release)
            .masks(4)
            .percent(100.0),
    ])
}

pub fn verify(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    let opened = launch.at("opened")?.state();
    let unchanged_end = &launch.at("cancel-gradient")?["file"];
    for frame in &launch.frames {
        let state = frame.state();
        ensure(
            state["surface"]["version"] == opened["surface"]["version"]
                && state["surface"]["gpu"]["upload_bytes"]
                    == opened["surface"]["gpu"]["upload_bytes"],
            "Unbound coverage, selection or presentation uploaded unchanged photograph pixels",
        )?;
        if &frame["file"] == unchanged_end {
            break;
        }
    }
    for name in ["linear-unplaced", "radial-unplaced", "new-gradient"] {
        let state = launch.at(name)?.state();
        ensure(
            state["mask_tool"]["placed"] == json!(false),
            format!("{name} placed initial geometry: {}", state["mask_draft"]),
        )?;
        ensure(
            state["draft_bar"]["done"] == json!(true),
            format!("{name} offers something other than Done before placement"),
        )?;
    }
    let disarmed = launch.at("switch-a-disarms")?;
    ensure(
        disarmed.state()["masks"]["brush"]["armed"] == json!(false),
        "Changing target retained the old armed brush",
    )?;
    for (step, upper, lower) in [("select-a", true, false), ("select-b", false, true)] {
        let frame = launch.at(step)?;
        ensure(
            frame.state()["mask_overlay"]["coverage"]["adopted"]["mask"]
                == frame.state()["masks"]["selected"],
            format!(
                "{step} rendered a different target: {}",
                frame.state()["mask_overlay"]
            ),
        )?;
        // Opening a mask selects its gradient, whose handles rest on the canvas: the lower probe
        // sits inside B's ellipse but off its centre, where B's anchor grip is drawn.
        for (at, covered) in [([0.5, 0.06], upper), ([0.56, 0.78], lower)] {
            let read = frame.luminance_at(at, 5)?;
            ensure(
                if covered { read > 240.0 } else { read < 15.0 },
                format!("{step}: {at:?} reads {read}, covered={covered}"),
            )?;
        }
        checks.note(
            frame,
            "selected coverage changes to the visible mask",
            json!({"target":frame.state()["masks"]["selected"]}),
        );
    }
    for (live, committed, probes) in [
        (
            "linear-black",
            "linear-committed",
            vec![[0.65, 0.06], [0.65, 0.18], [0.65, 0.75]],
        ),
        (
            "radial-live",
            "radial-committed",
            vec![[0.58, 0.80], [0.53, 0.90], [0.53, 0.22]],
        ),
        (
            "brush-live",
            "brush-committed",
            vec![[0.45, 0.55], [0.45, 0.60], [0.45, 0.72]],
        ),
    ] {
        let a = launch.at(live)?;
        let b = launch.at(committed)?;
        ensure(
            !a.state()["mask_overlay"]["coverage"]["adopted"]["request"]["identity"]["draft"]
                .is_null(),
            format!("{live} captured no accepted candidate identity"),
        )?;
        for at in probes {
            checks.compare(
                b,
                &format!("{live} coverage at {at:?} survives commit"),
                b.luminance_at(at, 3)?,
                a.luminance_at(at, 3)?,
                Tolerance::Within(2.0),
            )?;
        }
    }
    let brush = launch.at("brush-live")?;
    checks.compare(
        brush,
        "retracing retains 25 percent flow",
        brush.luminance_at([0.45, 0.55], 3)?,
        64.0,
        Tolerance::Within(3.0),
    )?;
    checks.compare(
        brush,
        "feather is evaluated during the stroke",
        brush.luminance_at([0.45, 0.60], 3)?,
        32.0,
        Tolerance::Within(5.0),
    )?;
    for name in ["manual-hide", "hidden-live"] {
        let overlay = &launch.at(name)?.state()["mask_overlay"];
        ensure(
            overlay["effective"] == json!("off") && overlay["forced"] == json!(false),
            format!("{name} ignored explicit hide: {overlay}"),
        )?;
    }
    ensure(
        launch.at("manual-show")?.state()["mask_overlay"]["effective"] == json!("tint"),
        "O did not restore the tint",
    )?;
    let stack = &launch.at("stroke-a")?.state()["masks"];
    ensure(
        stack["components"].as_array().is_some_and(|c| c.len() == 2),
        format!("Stroke went to the wrong selected mask: {stack}"),
    )?;
    let tail = Tail::read(&launch.at("rotated-transform")?["step"]["result"])?;
    ensure(
        {
            let a = tail.mapping.to_output(0.0, 0.0).unwrap();
            let x = tail.mapping.to_output(1.0, 0.0).unwrap();
            let y = tail.mapping.to_output(0.0, 1.0).unwrap();
            (x.1 - a.1).abs() > 0.01 && (y.0 - a.0).abs() > 0.01
        },
        "Transformed gradient acceptance did not rotate the content map",
    )?;
    for name in [
        "rotated-fit-linear-live",
        "rotated-fit-radial-live",
        "rotated-100-linear-live",
        "rotated-100-radial-live",
    ] {
        analytic_candidate(&mut checks, launch.at(name)?, &tail)?;
    }
    ensure(
        launch.at("rotated-100-radial-apply")?.state()["masks"]["selected"]
            != launch.at("rotated-100-radial-new")?.state()["masks"]["selected"],
        "Apply did not select the transformed newly created mask",
    )?;
    checks.write(run.out(), SCENARIO, json!({"scope":"Native background Metal renderer readbacks and correlated target/history state; no scanout claim"}))
}

/// The captured geometry service answer maps content pixels to displayed output pixels. Reading
/// its inverse lets the reference evaluate each renderer-readback pixel in the content stage, even
/// when 100% carries the photograph beyond the canvas. No production coverage evaluator is used.
struct Tail {
    stage: Stage,
    output: [f64; 2],
    mapping: luxforge_core::GeometryMap,
}

impl Tail {
    fn read(answer: &Value) -> Result<Self> {
        Ok(Self {
            stage: Stage::new(
                answer["content"]["width"]
                    .as_u64()
                    .ok_or("No content width")? as u32,
                answer["content"]["height"]
                    .as_u64()
                    .ok_or("No content height")? as u32,
            ),
            output: [
                answer["output"]["width"]
                    .as_f64()
                    .ok_or("No output width")?,
                answer["output"]["height"]
                    .as_f64()
                    .ok_or("No output height")?,
            ],
            mapping: serde_json::from_value::<luxforge_core::MappingDescriptor>(answer.clone())?
                .geometry,
        })
    }

    fn mask_uv(&self, photo: [i64; 4], x: u32, y: u32) -> [f64; 2] {
        let ox =
            (f64::from(x) + 0.5 - photo[0] as f64) / (photo[2] - photo[0]) as f64 * self.output[0];
        let oy =
            (f64::from(y) + 0.5 - photo[1] as f64) / (photo[3] - photo[1]) as f64 * self.output[1];
        let (cx, cy) = self.mapping.to_content(ox, oy).expect("captured geometry");
        let height = f64::from(self.stage.height);
        [cx / height, cy / height]
    }
}

fn analytic_candidate(checks: &mut Checks, frame: &Frame, tail: &Tail) -> Result {
    let tool = &frame.state()["mask_tool"];
    let shape = &tool["shape"];
    let number = |name: &str| -> Result<f64> {
        shape[name]
            .as_f64()
            .ok_or_else(|| format!("Candidate lacks {name}: {tool}").into())
    };
    let evaluate: Box<dyn Fn(f64, f64) -> f64> = match tool["kind"].as_str() {
        Some("linear") => {
            let gradient = Linear {
                x0: number("x0")?,
                y0: number("y0")?,
                x1: number("x1")?,
                y1: number("y1")?,
            };
            Box::new(move |u, v| reference::linear_coverage(&gradient, &tail.stage, u, v))
        }
        Some("radial") => {
            let gradient = Radial {
                x: number("x")?,
                y: number("y")?,
                radius_x: number("radius_x")?,
                radius_y: number("radius_y")?,
                angle: number("angle")?,
                feather: number("feather")?,
            };
            Box::new(move |u, v| reference::radial_coverage(&gradient, &tail.stage, u, v))
        }
        _ => return Err(format!("Expected a geometric candidate: {tool}").into()),
    };
    let overlay = &frame.state()["mask_overlay"];
    ensure(
        overlay["effective"] == "mask-on-black"
            && !overlay["coverage"]["adopted"]["request"]["identity"]["draft"].is_null()
            && overlay["coverage"]["pending"] == false,
        format!("Captured no current candidate coverage: {overlay}"),
    )?;
    ensure(
        (tool["aspect"].as_f64().ok_or("Candidate lacks aspect")? - tail.stage.aspect()).abs()
            < 1e-12,
        "Candidate was placed with the output aspect instead of the content aspect",
    )?;
    let photo = frame.photo_rect()?;
    let visible = frame.visible_photo()?;
    let image = frame.image()?;
    let mut references = Vec::new();
    // Patches are in the visible canvas, clear of the shape's diagonal axis and radial grips.
    for at in [
        [0.12, 0.20],
        [0.32, 0.48],
        [0.40, 0.37],
        [0.24, 0.76],
        [0.76, 0.78],
    ] {
        let x = (f64::from(visible[0]) + at[0] * f64::from(visible[2] - visible[0])).round() as u32;
        let y = (f64::from(visible[1]) + at[1] * f64::from(visible[3] - visible[1])).round() as u32;
        let mut actual = 0.0;
        let mut expected = 0.0;
        for py in y - 1..=y + 1 {
            for px in x - 1..=x + 1 {
                let [u, v] = tail.mask_uv(photo, px, py);
                expected += evaluate(u, v) * 255.0;
                let [r, g, b] = image.get_pixel(px, py).0;
                actual += 0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b);
            }
        }
        references.push(expected / 9.0);
        checks.compare(
            frame,
            &format!(
                "live {} at visible {at:?} matches independent transformed reference",
                tool["kind"]
            ),
            actual / 9.0,
            expected / 9.0,
            Tolerance::Within(4.0),
        )?;
    }
    ensure(
        references.iter().any(|value| (20.0..235.0).contains(value))
            && references.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                - references.iter().copied().fold(f64::INFINITY, f64::min)
                > 40.0,
        format!("The reference probes did not exercise gradient falloff: {references:?}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_regression_plan_is_valid() {
        plan(&[]).validate().unwrap();
    }
}
