//! The `pick` smoke scenario: Remove's hover picking (`docs/design/ai-editing.md`, "Picking:
//! hover, click, box and brush") on the real editor, background-only, with correlated state, log
//! and capture, over the developer fake segmenter.
//!
//! One developer launch on the four-quadrant fixture. A second client turns on `ai.generative`
//! (which offers Remove) and `ai.analysis` (which offers the picker), and the fake segmenter's two
//! files are granted and installed through the API. Remove is chosen, and the pointer moves over
//! the top-left quadrant: the first pick is refused for want of an analysis, the desktop runs
//! `task.analyse` once, asks again, and draws the picked object, tinted over its bounds. The fake
//! decoder's masks follow colour similarity, so the object is the quadrant: the tint shows inside
//! it and nothing changes in the quadrant beside it. A move to the top-right quadrant picks that
//! one, answered from the same analysis, and a move off the photograph clears the tint. No step
//! commits anything.
use crate::{
    corrections_smoke::{MODULE, NEAR, PATCH_HALF, correction, key},
    scenario::{Checked, Checks, Frame, Plan, Step, Tolerance, plan::only},
    *,
};
use luxforge_evidence::{
    self as script, CapabilityAction, CapabilityStep, CorrectionStep, RemoveStep,
};

pub const SCENARIO: &str = "pick";
pub const ABOUT: &str = "Remove's hover picking on the developer fake segmenter: the picker offered once ai.analysis is on and the segmenter installed, the first hover running task.analyse once and then picking, the hovered quadrant tinted over its bounds with its neighbour unchanged, a second quadrant picked from the same analysis, and the tint cleared off the photograph";

const AI: &str = "luxforge.ai";
const ORIGIN: &str = "https://developer.luxforge.invalid";
const SEGMENTER: [&str; 2] = ["fake-segmenter-encoder", "fake-segmenter-decoder"];
/// The two hovered positions, in the top-left and top-right quadrants.
const LEFT: [f64; 2] = [0.25, 0.25];
const RIGHT: [f64; 2] = [0.75, 0.25];
/// The tint moves a hovered patch by at least this much per channel, in 8-bit codes.
const TINTED: f64 = 8.0;

fn remove(step: RemoveStep) -> script::Step {
    correction(CorrectionStep::Remove(step))
}

/// Every frame, in order.
pub fn plan(_: &[PathBuf]) -> Plan {
    let flag = |name: &str, flag: &str| {
        Step::new(
            name,
            script::Step::agent("flags.set", json!({"flag": flag, "value": true})),
        )
        .commits(0)
    };
    let api = |name: &str, method: &str, params: Value| {
        Step::new(name, script::Step::call(method, params)).commits(0)
    };
    let mut steps = vec![
        Step::opened("opened").no_draft(),
        flag("generative", "ai.generative"),
        flag("analysis", "ai.analysis"),
    ];
    for resource in SEGMENTER {
        steps.push(api(
            &format!("{resource}-allowed"),
            "module.permission.grant",
            json!({"module_id": AI, "capability": format!("download-{resource}"),
                   "scope": {"resource": resource, "version": "1", "origin": ORIGIN}}),
        ));
        steps.push(api(
            &format!("{resource}-installing"),
            "module.resource.install",
            json!({"module_id": AI, "resource_id": resource}),
        ));
        steps.push(
            Step::new(
                format!("{resource}-installed"),
                CapabilityStep::new(AI, CapabilityAction::Settle),
            )
            .commits(0),
        );
    }
    steps.extend([
        Step::new("mode", key("q")).commits(0).mode(MODULE),
        Step::new("remove", correction(CorrectionStep::Kind("remove".into()))).commits(0),
        Step::new("left", remove(RemoveStep::Hover(Some(LEFT)))).commits(0),
        Step::new("right", remove(RemoveStep::Hover(Some(RIGHT)))).commits(0),
        Step::new("off", remove(RemoveStep::Hover(None))).commits(0),
    ]);
    Plan::new(steps)
}

/// Hover picking's record in a frame.
fn hover(frame: &Frame) -> &Value {
    &frame.state()["corrections"]["remove"]["hover"]
}

fn rgb(frame: &Frame, at: [f64; 2]) -> Result<[f64; 3]> {
    frame.rgb_at(at, PATCH_HALF)
}

/// The largest per-channel difference between two patches.
fn apart(one: [f64; 3], two: [f64; 3]) -> f64 {
    one.iter()
        .zip(two)
        .map(|(one, two)| (one - two).abs())
        .fold(0.0, f64::max)
}

/// The frame's overlay bounds, which must hold `at` and stay off `away`.
fn bounds_hold(frame: &Frame, at: [f64; 2], away: [f64; 2]) -> Result {
    let record = hover(frame);
    let bounds: Vec<f64> = record["overlay"]["bounds"]
        .as_array()
        .map(|b| b.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let inside = |p: [f64; 2]| {
        bounds.len() == 4
            && bounds[0] <= p[0]
            && p[0] <= bounds[2]
            && bounds[1] <= p[1]
            && p[1] <= bounds[3]
    };
    ensure(
        inside(at) && !inside(away),
        format!("The hovered object's bounds hold {at:?} and not {away:?}: {record}"),
    )
}

pub fn verify(run: &mut crate::scenario::Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();
    let chosen = launch.at("remove")?;
    ensure(
        hover(chosen)["picking"] == json!(true) && hover(chosen)["overlay"].is_null(),
        format!(
            "The picker offered and nothing hovered yet: {}",
            hover(chosen)
        ),
    )?;
    // The first hover ran the analysis and then answered; the pick sent is an API client's.
    let left = launch.at("left")?;
    let record = hover(left);
    ensure(
        record["answered"] == json!(1)
            && record["asking"].is_null()
            && record["analysing"] == json!(false)
            && record["refused"] == json!(false)
            && record["last_request"]["method"] == json!("query.pick-object")
            && record["last_request"]["params"]["point"] == json!([LEFT]),
        format!("The first hover's pick: {record}"),
    )?;
    bounds_hold(left, LEFT, RIGHT)?;
    checks.compare(
        left,
        "the hovered quadrant's tint, against the photograph before it",
        apart(rgb(left, LEFT)?, rgb(chosen, LEFT)?),
        0.0,
        Tolerance::Above(TINTED),
    )?;
    checks.compare(
        left,
        "the quadrant beside it, against the photograph before it",
        apart(rgb(left, RIGHT)?, rgb(chosen, RIGHT)?),
        0.0,
        Tolerance::AtMost(NEAR),
    )?;
    // The second quadrant, from the same analysis.
    let right = launch.at("right")?;
    ensure(
        hover(right)["answered"] == json!(2) && hover(right)["refused"] == json!(false),
        format!("The second hover's pick: {}", hover(right)),
    )?;
    bounds_hold(right, RIGHT, LEFT)?;
    checks.compare(
        right,
        "the second quadrant's tint, against the photograph before it",
        apart(rgb(right, RIGHT)?, rgb(chosen, RIGHT)?),
        0.0,
        Tolerance::Above(TINTED),
    )?;
    // Off the photograph: no tint.
    let off = launch.at("off")?;
    ensure(
        hover(off)["overlay"].is_null(),
        format!("No overlay off the photograph: {}", hover(off)),
    )?;
    checks.compare(
        off,
        "the photograph off hover, against the photograph before it",
        apart(rgb(off, LEFT)?, rgb(chosen, LEFT)?),
        0.0,
        Tolerance::AtMost(NEAR),
    )?;
    checks.write(
        run.out(),
        SCENARIO,
        json!({
            "tinted_codes": TINTED,
            "near_codes": NEAR,
            "scope": "Mean 8-bit RGB of small patches of the captured photograph over the flat four-quadrant fixture, read as tinted or unchanged against the frame before the first hover; the developer fake segmenter's masks follow colour similarity, so an object is a quadrant: a wiring and overlay claim, not a model-quality or latency claim",
        }),
    )
}
