//! Native viewport journeys. The first two preserve the review's once-uncommitted functional
//! scripts as replayable smoke scenarios; the third observes a Fit refit before evidence is
//! allowed to tick or capture again. A missing GPU draw counter is an error, never a zero.
use crate::{
    scenario::{Checked, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_evidence::{self as script, ViewIdleStep, ViewStep, WorkspaceStep};

pub const REGION: &str = "viewport-region";
pub const FALLBACK: &str = "viewport-fallback";
pub const IDLE: &str = "viewport-idle-fit";

const REGION_SCRIPT: &str = include_str!("../scenarios/viewport-region-mask-crop.json");
const FALLBACK_SCRIPT: &str = include_str!("../scenarios/viewport-estimate-after-spatial.json");
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
const FALLBACK_NAMES: [&str; 12] = [
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

fn scripted(source: &str, names: &[&str]) -> Plan {
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
            .map(|(request, name)| Step::new(*name, request)),
    );
    Plan::new(steps)
}

pub fn region_plan(_: &[PathBuf]) -> Plan {
    scripted(REGION_SCRIPT, &REGION_NAMES)
}
pub fn fallback_plan(_: &[PathBuf]) -> Plan {
    scripted(FALLBACK_SCRIPT, &FALLBACK_NAMES)
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
        Step::new("settled-100", script::Step::wait(1000)).commits(0),
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

fn event<'a>(events: &'a [Value], kind: &str) -> impl Iterator<Item = &'a Value> {
    let kind = kind.to_owned();
    events.iter().filter(move |event| event["event"] == kind)
}

fn changed_pixels(first: &Frame, second: &Frame) -> Result<u64> {
    let (a, b) = (first.image()?, second.image()?);
    ensure(
        a.dimensions() == b.dimensions(),
        "Overlay captures changed physical size",
    )?;
    let rect: [u32; 4] = serde_json::from_value(first["canvas_rect"].clone())?;
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
    for name in &REGION_NAMES[..29] {
        let state = at(name)?;
        ensure(
            state["workspace"]["state_panel"] == false,
            format!("{name} unexpectedly opened the state panel"),
        )?;
    }
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
    ensure(
        at("first-draft")?["histogram"]["stale"] == true,
        "A drafted viewport made the full histogram current",
    )?;
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
    ensure(
        [
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
        }),
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
    ensure(
        gpu(launch.at("settled-pause")?, "photo_writes")?
            == gpu(launch.at("release-pause")?, "photo_writes")?
            && final_state["surface"]["gpu"]["drawn_full_version"]
                == at("settled-pause")?["surface"]["gpu"]["drawn_full_version"],
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
    run.record(
        "viewport",
        json!({"case":REGION,"mask_overlay_changed_pixels":tint_pixels,
        "clipping_overlay_changed_pixels":clipping_pixels,"gpu":all_draws(launch)?}),
    );
    Ok(())
}

pub fn verify_fallback(run: &mut Run, launches: &[Checked]) -> Result {
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
        event(&launch.events, "preview_view_fallback").any(|e| {
            e["detail"]["reason"]
                .as_str()
                .is_some_and(|s| s.contains("global estimate behind an earlier spatial layer"))
        }),
        "Viewport refusal did not name estimate-after-spatial fallback",
    )?;
    ensure(
        !event(&launch.events, "preview_displayed")
            .any(|e| e["detail"]["path"] == "region" && !e["detail"]["draft_revision"].is_null()),
        "Declined draft masqueraded as a viewport region",
    )?;
    ensure(
        launch.at("draft")?.state()["histogram"]["stale"] == true
            && launch.at("release-pause")?.state()["histogram"]["stale"] == false
            && launch.at("release-pause")?.state()["histogram"]["identity"]["draft_revision"]
                .is_null(),
        "Fallback did not settle exact full histogram counts",
    )?;
    run.record(
        "viewport",
        json!({"case":FALLBACK,"gpu":all_draws(launch)?}),
    );
    Ok(())
}

pub fn verify_idle(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let before = launch.at("settled-100")?;
    ensure(
        before.state()["surface"]["view"]["zoom"]["mode"] == "percent"
            && before.state()["surface"]["view"]["zoom"]["value"] == 100.0
            && before.state()["workspace"]["state_panel"] == false
            && before.state()["workspace"]["clip_shadows"] == false
            && before.state()["workspace"]["clip_highlights"] == false
            && before.state()["workspace"]["mask_overlay"] == "off"
            && before.state()["performance"]["sampling"] == false,
        "Idle precondition was not an ordinary 100% view with overlays and sampling off",
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
    ensure(
        detail["expected_full_version"].as_u64().is_some()
            && detail["expected_full_version"] == detail["drawn_full_version"]
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
    ensure(
        after.state()["surface"]["gpu"]["drawn_full_version"] == detail["drawn_full_version"],
        "Capture after idle check did not show the checked Fit photo",
    )?;
    run.record(
        "viewport",
        json!({"case":IDLE,"idle_check":detail,"gpu":all_draws(launch)?}),
    );
    Ok(())
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
        fallback_plan(&[]).validate().unwrap();
        idle_plan(&[]).validate().unwrap();
    }
}
