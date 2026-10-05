//! Native viewport journeys. The first preserves the review's once-uncommitted functional script
//! as a replayable smoke scenario; the second holds a global estimate behind an earlier spatial
//! layer at 100%, which a drag draws on the GPU and the exact region refines with the layers
//! before it kept whole; the third observes a Fit refit before evidence is allowed to tick or
//! capture again. A missing GPU draw counter is an error, never a zero.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_evidence::{self as script, ViewIdleStep, ViewStep, WorkspaceStep};

pub const REGION: &str = "viewport-region";
pub const CHAINED: &str = "viewport-chained-estimate";
pub const IDLE: &str = "viewport-idle-fit";

const REGION_SCRIPT: &str = include_str!("../scenarios/viewport-region-mask-crop.json");
const CHAINED_SCRIPT: &str = include_str!("../scenarios/viewport-chained-estimate.json");
const REGION_NAMES: [&str; 29] = [
    "panel-hidden",
    "performance-expanded",
    "linear-mask",
    "masked-basic",
    "radial-mask",
    "masked-presence",
    "rotated-crop",
    "mask-workspace",
    "mask-selected",
    "zoom-100",
    "clipping-on",
    "mask-overlay-on",
    "first-draft",
    "first-pan",
    "first-pause",
    "second-pan",
    "second-pause",
    "release",
    "release-pause",
    "settled-pan",
    "settled-pause",
    "mask-overlay-off",
    "clipping-off",
    "cancel-draft",
    "cancelled",
    "history-preview",
    "history-pan",
    "history-current",
    "final-pause",
];
const CHAINED_NAMES: [&str; 12] = [
    "panel-hidden",
    "performance-expanded",
    "linear-mask",
    "first-presence",
    "radial-mask",
    "second-presence",
    "zoom-100",
    "draft",
    "pan",
    "pan-pause",
    "release",
    "release-pause",
];

/// The open, then one step per request of a committed script, each named, with `expect` applied to
/// every scripted step.
fn scripted(source: &str, names: &[&str], expect: impl Fn(Step) -> Step) -> Plan {
    let requests: Vec<script::Step> =
        serde_json::from_str(source).expect("committed evidence script");
    assert_eq!(
        requests.len(),
        names.len(),
        "every request needs a stable name"
    );
    let mut steps = vec![Step::opened("opened")];
    steps.extend(
        requests
            .into_iter()
            .zip(names)
            .map(|(request, name)| expect(Step::new(*name, request))),
    );
    Plan::new(steps)
}

/// The region journey, whose first step hides the state panel, which no later step opens again.
pub fn region_plan(_: &[PathBuf]) -> Plan {
    scripted(REGION_SCRIPT, &REGION_NAMES, |step| {
        step.workspace("state_panel", json!(false))
    })
}
pub fn chained_plan(_: &[PathBuf]) -> Plan {
    scripted(CHAINED_SCRIPT, &CHAINED_NAMES, |step| step)
}

pub fn idle_plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened"),
        Step::new("panel-hidden", WorkspaceStep::default().state_panel(false)).commits(0),
        Step::new(
            "clipping-off",
            WorkspaceStep::default()
                .clip_shadows(false)
                .clip_highlights(false)
                .mask_overlay("off"),
        )
        .commits(0),
        Step::new("zoom-100", ViewStep::Percent(100.0)).commits(0),
        // An ordinary 100% view with the panel hidden and every overlay off.
        Step::new("settled-100", script::Step::wait(1000))
            .commits(0)
            .percent(100.0)
            .workspace("state_panel", json!(false))
            .workspace("clip_shadows", json!(false))
            .workspace("clip_highlights", json!(false))
            .workspace("mask_overlay", json!("off")),
        Step::new(
            "idle-fit",
            ViewIdleStep {
                view: ViewStep::Fit,
                ms: 1000,
            },
        )
        .commits(0),
    ])
}

fn gpu(frame: &Frame, name: &str) -> Result<u64> {
    frame.state()["surface"]["gpu"][name]
        .as_u64()
        .ok_or_else(|| format!("{} lacks surface.gpu.{name}", frame["file"]).into())
}

fn checked_blank_draws(frame: &Frame, index: usize) -> Result {
    let blanks = gpu(frame, "blank_photo_draws")?;
    ensure(
        blanks == 0,
        format!("Frame {index} encoded {blanks} blank photograph draws"),
    )
}

fn all_draws(launch: &Checked) -> Result<Value> {
    let mut last_blank = 0;
    let mut last_stale = 0;
    let mut maximum_residency = 0;
    for (index, frame) in launch.frames.iter().enumerate() {
        checked_blank_draws(frame, index)?;
        let blank = gpu(frame, "blank_photo_draws")?;
        let stale = gpu(frame, "stale_photo_draws")?;
        ensure(
            blank >= last_blank && stale >= last_stale,
            format!("Frame {index} reset draw counters"),
        )?;
        let diagnostics = frame.state()["surface"]["gpu"]
            .as_object()
            .ok_or("Missing photo surface GPU diagnostics")?;
        ensure(
            diagnostics
                .get("drawn_stale_photo")
                .is_some_and(Value::is_boolean)
                && diagnostics.contains_key("drawn_fallback_content"),
            format!("Frame {index} lacks drawn photo identity diagnostics"),
        )?;
        let resident = gpu(frame, "full_resident_bytes")?
            .checked_add(gpu(frame, "region_resident_bytes")?)
            .and_then(|bytes| bytes.checked_add(gpu(frame, "retiring_bytes").ok()?))
            .ok_or("Photo texture residency overflow")?;
        maximum_residency = maximum_residency.max(resident);
        ensure(
            resident <= 1088 * 1024 * 1024,
            format!("Frame {index} photo texture residency exceeded 1088 MiB"),
        )?;
        ensure(
            gpu(frame, "gpu_retirement_failures")? == 0,
            format!("Frame {index} had a GPU retirement failure"),
        )?;
        last_blank = blank;
        last_stale = stale;
    }
    Ok(
        json!({"blank_photo_draws":last_blank,"stale_photo_draws":last_stale,
        "maximum_photo_residency_bytes":maximum_residency}),
    )
}

/// The events of the step whose frame is `name`: from its `script_step` record to the next.
fn step_events<'a>(launch: &'a Checked, name: &str) -> Result<&'a [Value]> {
    let step = launch.index(name)? as u64;
    let events = &launch.events;
    let start = events
        .iter()
        .position(|event| event["event"] == "script_step" && event["detail"]["step"] == step)
        .ok_or_else(|| format!("no script_step {step} for {name}"))?;
    let end = events[start + 1..]
        .iter()
        .position(|event| event["event"] == "script_step")
        .map_or(events.len(), |offset| start + 1 + offset);
    Ok(&events[start..end])
}

/// What the histogram captured after a drafted step may show
/// ([basic-and-histogram](../../docs/design/basic-and-histogram.md)). While the draft moves, the
/// last exact whole-image report stays plotted and marked updating. Once the draft has been quiet
/// for the shared policy, the exact full-frame analysis of the drafted recipe is its report, as the
/// `histogram` scenario checks of a paused draft. The step settles on the draft's first frame, and
/// the capture after it comes whenever the window is read back: on a loaded host that can be after
/// the quiet policy has settled the draft, and the report is then current. That is correct only
/// when the report is this draft revision's, reduced by the settle job the quiet policy asked for
/// after the step's input and adopted before the capture; anything else current in the capture is
/// a whole-image report adopted during the gesture, and fails.
fn drafted_histogram(launch: &Checked, name: &str, revision: u64) -> Result<Value> {
    let histogram = &launch.at(name)?.state()["histogram"];
    if histogram["stale"] == true {
        return Ok(json!({"outcome": "updating"}));
    }
    let events = step_events(launch, name)?;
    let input = events
        .iter()
        .rposition(|event| event["event"] == "slider_draft_set")
        .ok_or_else(|| format!("{name} sent no draft.set"))?;
    let quiet = events[input..]
        .iter()
        .position(|event| event["event"] == "preview_quiet_refine")
        .map(|offset| input + offset);
    let settle = quiet.and_then(|quiet| {
        events[quiet..].iter().find(|event| {
            event["event"] == "preview_view_requested" && event["detail"]["intent"] == "settle"
        })
    });
    let generation = settle.and_then(|settle| settle["detail"]["generation"].as_u64());
    let adopted = events.iter().find(|event| {
        event["event"] == "analysis_adopted"
            && generation.is_some()
            && event["detail"]["generation"].as_u64() == generation
            && event["detail"]["draft_revision"].as_u64() == Some(revision)
    });
    let identity = &histogram["identity"];
    ensure(
        adopted.is_some()
            && identity["draft_revision"].as_u64() == Some(revision)
            && identity["generation"].as_u64() == generation,
        format!(
            "A drafted viewport made the full histogram current during the gesture: {name}'s report \
             is {identity}, and the quiet policy's settle for draft revision {revision} {}",
            match (quiet, generation, adopted) {
                (None, _, _) => "never ran in the step".to_owned(),
                (Some(_), None, _) => "asked for no settle job".to_owned(),
                (Some(_), Some(generation), None) =>
                    format!("(generation {generation}) adopted no analysis of that revision"),
                (Some(_), Some(generation), Some(_)) =>
                    format!("was generation {generation}, which the report does not name"),
            }
        ),
    )?;
    let at = |index: usize| events[index]["elapsed_ms"].as_f64();
    Ok(json!({
        "outcome": "settled by the quiet policy before the capture",
        "draft_revision": revision,
        "settle_generation": generation,
        "quiet_after_input_ms": quiet.and_then(at).zip(at(input)).map(|(quiet, input)| quiet - input),
        "adopted_after_input_ms": adopted
            .and_then(|event| event["elapsed_ms"].as_f64())
            .zip(at(input))
            .map(|(adopted, input)| adopted - input),
    }))
}

fn event<'a>(events: &'a [Value], kind: &str) -> impl Iterator<Item = &'a Value> {
    let kind = kind.to_owned();
    events.iter().filter(move |event| event["event"] == kind)
}

/// How many pixels of the photograph's interior on screen differ between two captures: the part of
/// the canvas the editor records drawing it in, clear of the canvas chrome around its edges.
fn changed_pixels(first: &Frame, second: &Frame) -> Result<u64> {
    let (a, b) = (first.image()?, second.image()?);
    ensure(
        a.dimensions() == b.dimensions(),
        "Overlay captures changed physical size",
    )?;
    let rect = first.visible_photo()?;
    let (left, top, right, bottom) = (
        rect[0] + 60,
        rect[1] + 60,
        rect[2].saturating_sub(60),
        rect[3].saturating_sub(170),
    );
    ensure(
        left < right && top < bottom && right <= a.width() && bottom <= a.height(),
        "The canvas has no safe photo interior for overlay comparison",
    )?;
    let mut count = 0;
    for y in top..bottom {
        for x in left..right {
            if a.get_pixel(x, y) != b.get_pixel(x, y) {
                count += 1;
            }
        }
    }
    Ok(count)
}

pub fn verify_region(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let at = |name| launch.at(name).map(Frame::state);
    let setup = at("zoom-100")?;
    let layers = setup["stack"]["layers"]
        .as_array()
        .ok_or("No stack layers")?;
    let masks = setup["masks"]["masks"].as_array().ok_or("No masks")?;
    ensure(
        masks.len() >= 2 && layers.iter().filter(|l| !l["mask"].is_null()).count() >= 2,
        "Two distinct masks must own edited layers",
    )?;
    ensure(
        layers.iter().any(|l| {
            l["effect"].as_str().is_some_and(|s| s.ends_with(".crop"))
                && l["payload"]["angle"]
                    .as_f64()
                    .is_some_and(|a| (a - 7.0).abs() < 0.001)
        }),
        "The region journey did not render through the rotated crop",
    )?;
    let reads = at("performance-expanded")?["performance"]["reads_requested"].clone();
    for name in &REGION_NAMES[9..21] {
        let state = at(name)?;
        ensure(
            state["performance"]["reads_requested"] == reads
                && state["performance"]["sampling"] == false,
            format!("{name} woke the hidden Performance sampler"),
        )?;
    }
    for name in ["settled-pause", "final-pause"] {
        let state = at(name)?;
        ensure(
            state["surface"]["quiet_timer_armed"] == false
                && state["surface"]["desired_view_dirty"] == false
                && state["surface"]["detail_updating"] == false,
            format!("{name} left viewport work pending"),
        )?;
    }
    ensure(
        gpu(launch.at("settled-pause")?, "retiring_bytes")? == 0,
        "GPU retirement remained pending after settle",
    )?;
    let first_revision = at("first-draft")?["displayed_draft_revision"]
        .as_u64()
        .ok_or("The first draft had no displayed revision")?;
    let first_histogram = drafted_histogram(launch, "first-draft", first_revision)?;
    let regions: Vec<_> = event(&launch.events, "preview_displayed")
        .filter(|e| e["detail"]["path"] == "region")
        .collect();
    for quality in ["interactive", "exact"] {
        ensure(
            regions.iter().any(|e| {
                e["detail"]["draft_revision"] == first_revision && e["detail"]["quality"] == quality
            }),
            format!("First draft never displayed a {quality} region"),
        )?;
    }
    let captured_region = [
        "first-draft",
        "first-pan",
        "first-pause",
        "second-pan",
        "second-pause",
    ]
    .iter()
    .any(|name| {
        let h = &at(name).unwrap()["histogram"];
        !h["overlay"].is_null()
            && !h["overlay"]["region"].is_null()
            && h["overlay"]["source_assigned"] == true
    });
    // When the quiet policy settled the first draft before its capture, its whole frame and that
    // frame's exact overlay replaced the region grid, and later views of the same draft reuse the
    // whole frame: no drafted capture can show the grid. Its own event shows it was assigned to a
    // region the draft displayed.
    let region_generations: Vec<_> = regions
        .iter()
        .filter(|e| e["detail"]["draft_revision"] == first_revision)
        .map(|e| e["detail"]["generation"].clone())
        .collect();
    let region_grid = event(&launch.events, "clipping_overlay").any(|e| {
        e["detail"]["approximate"] == true
            && region_generations.contains(&e["detail"]["generation"])
    });
    ensure(
        captured_region || (first_histogram["outcome"] != "updating" && region_grid),
        "No drafted capture assigned viewport clipping coverage",
    )?;
    let final_state = at("release-pause")?;
    let histogram = &final_state["histogram"];
    ensure(
        histogram["stale"] == false
            && histogram["identity"]["draft_revision"].is_null()
            && histogram["identity"]["width"] == final_state["preview_dimensions"][0]
            && histogram["identity"]["height"] == final_state["preview_dimensions"][1]
            && histogram["overlay"]["approximate"] == false,
        "Release lacks exact full-stage histogram and clipping overlay",
    )?;
    // The pan writes no photograph texture. Released, the photograph at rest was the GPU's region
    // plan, which the pan leaves: the full photograph texture already held is drawn; drawn by the
    // CPU at release, the same full texture is drawn again.
    let released = &final_state["surface"]["gpu"];
    let settled = &at("settled-pause")?["surface"]["gpu"];
    let full_reused = if released["picture"] == "view" || released["picture"] == "rest" {
        !settled["drawn_full_version"].is_null()
    } else {
        released["drawn_full_version"] == settled["drawn_full_version"]
    };
    ensure(
        gpu(launch.at("settled-pause")?, "photo_writes")?
            == gpu(launch.at("release-pause")?, "photo_writes")?
            && full_reused,
        "Settled pan did not reuse the full photograph texture",
    )?;
    let tint_pixels = changed_pixels(launch.at("settled-pause")?, launch.at("mask-overlay-off")?)?;
    let clipping_state = at("mask-overlay-off")?;
    let clipping = &clipping_state["histogram"]["overlay"];
    ensure(
        clipping["source_assigned"] == true
            && clipping["drawn"] == true
            && clipping["version"].as_u64().is_some()
            && clipping["version"] == clipping_state["surface"]["gpu"]["drawn_clipping_version"]
            && clipping["generation"] == clipping_state["surface"]["generation"],
        "Mask-off capture lacked the current clipping frame's GPU draw",
    )?;
    let unclipped_state = at("clipping-off")?;
    ensure(
        unclipped_state["histogram"]["overlay"].is_null()
            && unclipped_state["surface"]["gpu"]["drawn_clipping_version"].is_null(),
        "Clipping-off capture still drew a clipping frame",
    )?;
    let clipping_pixels =
        changed_pixels(launch.at("mask-overlay-off")?, launch.at("clipping-off")?)?;
    ensure(
        tint_pixels > 1000 && clipping_pixels > 50,
        format!("Overlay pixels did not change: mask {tint_pixels}, clipping {clipping_pixels}"),
    )?;
    let before_cancel = at("clipping-off")?;
    let after_cancel = at("cancelled")?;
    ensure(
        after_cancel["displayed_draft_revision"].is_null()
            && after_cancel["stack"]["revision"] == before_cancel["stack"]["revision"]
            && after_cancel["stack"]["displayed"]["snapshot"]
                == before_cancel["stack"]["displayed"]["snapshot"],
        "Cancel failed to restore the committed snapshot",
    )?;
    ensure(
        at("history-preview")?["stack"]["displayed"]["snapshot"]
            != before_cancel["stack"]["displayed"]["snapshot"]
            && at("history-current")?["stack"]["displayed"]["snapshot"]
                == before_cancel["stack"]["displayed"]["snapshot"],
        "History preview or Return to current drew the wrong snapshot",
    )?;
    let mut checks = Checks::new();
    checks.note(
        launch.at("first-draft")?,
        "the first draft's histogram: updating, or settled for its own revision by the quiet policy",
        json!({"histogram": first_histogram, "region_grid_captured": captured_region,
            "region_grid_logged": region_grid}),
    );
    checks.note(
        launch.at("clipping-off")?,
        "the mask and clipping overlays each changed the photograph's pixels",
        json!({"mask_overlay_changed_pixels":tint_pixels,"clipping_overlay_changed_pixels":clipping_pixels}),
    );
    checks.write(run.out(), REGION, json!({"gpu": all_draws(launch)?}))
}

/// Two separately masked Presence layers, Dehaze on the second, at 100%: the drag's ticks are drawn
/// on the GPU, the second layer's light computed from the whole stage by its stand-in with the
/// first layer left out; nothing names a global estimate as a reason to refuse the view's region;
/// and once released the exact visible region is rendered, the layers before Dehaze kept whole so
/// it reduces its own input, with exact full histogram counts.
pub fn verify_chained(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let layers = launch.at("second-presence")?.state()["stack"]["layers"]
        .as_array()
        .ok_or("No stack layers")?;
    let presence: Vec<_> = layers
        .iter()
        .filter(|l| l["effect"] == "luxforge.presence.adjust")
        .collect();
    ensure(
        presence.len() == 2 && presence[0]["mask"] != presence[1]["mask"],
        "Two separately masked Presence layers are required",
    )?;
    ensure(
        !event(&launch.events, "preview_view_fallback").any(|e| {
            e["detail"]["reason"]
                .as_str()
                .is_some_and(|s| s.contains("global estimate"))
        }),
        "A view's region was refused for a global estimate",
    )?;
    let ticks: Vec<_> = event(&launch.events, "gpu_preview_tick").collect();
    let gpu_ticks = ticks
        .iter()
        .filter(|tick| tick["detail"]["path"] == "gpu")
        .count();
    ensure(
        gpu_ticks >= 1,
        format!(
            "The drag drew no tick on the GPU: {:?}",
            ticks
                .iter()
                .map(|tick| (&tick["detail"]["path"], &tick["detail"]["reason"]))
                .collect::<Vec<_>>()
        ),
    )?;
    let lights = &launch.at("pan")?.state()["surface"]["gpu"]["gpu_preview"]["drag"]["lights"];
    let refined = event(&launch.events, "preview_displayed")
        .any(|e| e["detail"]["path"] == "region" && e["detail"]["draft_revision"].is_null());
    ensure(
        refined,
        "The released view's exact region was never displayed",
    )?;
    let revision = launch.at("draft")?.state()["displayed_draft_revision"]
        .as_u64()
        .ok_or("The draft had no displayed revision")?;
    let draft_histogram = drafted_histogram(launch, "draft", revision)?;
    ensure(
        launch.at("release-pause")?.state()["histogram"]["stale"] == false
            && launch.at("release-pause")?.state()["histogram"]["identity"]["draft_revision"]
                .is_null(),
        "The release did not settle exact full histogram counts",
    )?;
    let mut checks = Checks::new();
    checks.note(
        launch.at("draft")?,
        "the draft's histogram: updating, or settled for its own revision by the quiet policy",
        draft_histogram,
    );
    checks.note(
        launch.at("pan")?,
        "the drag on the GPU, its lights, and the exact region after the release",
        json!({"gpu_ticks": gpu_ticks, "ticks": ticks.len(), "lights": lights,
            "exact_region_displayed": refined}),
    );
    checks.write(run.out(), CHAINED, json!({"gpu": all_draws(launch)?}))
}

pub fn verify_idle(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    // The plan holds the precondition's view and overlays; its sampler is off too.
    ensure(
        launch.at("settled-100")?.state()["performance"]["sampling"] == false,
        "Idle precondition was not an ordinary 100% view with sampling off",
    )?;
    let checks: Vec<_> = event(&launch.events, "view_idle_check").collect();
    ensure(
        checks.len() == 1,
        format!("Expected one view_idle_check event, got {}", checks.len()),
    )?;
    let detail = &checks[0]["detail"];
    ensure(
        detail["ready"] == true && detail["passed"] == true,
        format!("Idle Fit failed before evidence capture: {detail}"),
    )?;
    ensure(
        detail["blank_photo_draws_delta"].as_u64() == Some(0)
            && detail["drawn_frames_delta"].as_u64().is_some_and(|n| n > 0)
            && detail["stale_photo_draws_delta"].as_u64().is_some(),
        format!("Idle Fit missed its draw or encoded blank content: {detail}"),
    )?;
    // The photograph drawn at idle Fit: the GPU's picture at rest of the committed stack, which
    // stands in for the presenter's frame, or that frame itself where the GPU does not draw it.
    let at_rest = detail["picture"] == "rest" || detail["picture"] == "view";
    ensure(
        detail["expected_full_version"].as_u64().is_some()
            && (if at_rest {
                !detail["drawn_rest"].is_null() || !detail["drawn_gpu_boundary"].is_null()
            } else {
                detail["expected_full_version"] == detail["drawn_full_version"]
            })
            && detail["drawn_stale_photo"] == false
            && detail["drawn_fallback_content"].is_null(),
        format!("Idle Fit did not draw the requested photograph version: {detail}"),
    )?;
    let recorded = &launch.app["script"][4]["view_idle_check"];
    ensure(
        recorded == detail,
        "The script's pre-capture idle check disagrees with the event",
    )?;
    let after = launch.at("idle-fit")?;
    let drawn = &after.state()["surface"]["gpu"];
    ensure(
        drawn["drawn_full_version"] == detail["drawn_full_version"]
            && drawn["drawn_rest"] == detail["drawn_rest"]
            && drawn["picture"] == detail["picture"],
        "Capture after idle check did not show the checked Fit photo",
    )?;
    Checks::new().write(
        run.out(),
        IDLE,
        json!({"idle_check": detail, "gpu": all_draws(launch)?}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_draw_gate_rejects_missing_and_nonzero_diagnostics() {
        let missing =
            Frame::state_only(&json!({"file":"missing.png","state":{"surface":{"gpu":{}}}}));
        assert!(
            checked_blank_draws(&missing, 0)
                .unwrap_err()
                .to_string()
                .contains("blank_photo_draws")
        );
        let blank = Frame::state_only(
            &json!({"file":"blank.png","state":{"surface":{"gpu":{"blank_photo_draws":1}}}}),
        );
        assert!(
            checked_blank_draws(&blank, 1)
                .unwrap_err()
                .to_string()
                .contains("1 blank photograph draws")
        );
    }

    #[test]
    fn committed_journeys_have_valid_named_steps() {
        region_plan(&[]).validate().unwrap();
        chained_plan(&[]).validate().unwrap();
        idle_plan(&[]).validate().unwrap();
    }
}
