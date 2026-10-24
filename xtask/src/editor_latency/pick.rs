//! `editor-latency --mode pick`: a range pick's time from the press on the photograph to the
//! overlay adopted for the entry it committed ([range masks](../../../docs/design/range-masks.md)).
//!
//! One launch over a generated JPEG (`generate-fixtures`: the 24 MP and 60 MP workloads). Mask mode
//! is entered and a colour range created from New mask, which arms
//! the eyedropper and shows the overlay as Tint with no adjustment bound, so every pick reads the
//! mask's unbound input site. Then `--samples` pairs, each:
//!
//! - **a whole-frame drag**, a pick step from the output stage's top-left pixel to its bottom-right
//!   one: the area read reduced to at most 4,096 points (on the GPU, the tile service's; with
//!   `--reference-renderer`, the reference renderer's whole prefix), its medoid stored as the sample
//!   and Refine lowered;
//! - **a click** on the fixture's red quadrant, which replaces the sample with that colour, so the
//!   next drag changes the component again.
//!
//! Each pick is timed from the `script_step` that pressed it to the `range_pick` that sent its
//! command and to the first `mask_coverage_ready` for the entry the pick committed: the overlay
//! adopted for it. Every pick must commit exactly one entry, or
//! the run fails. A last API step asks `mask.sample-input-area` over the whole frame, whose answer
//! names the renderer the picks read from.
use super::*;

/// The most pairs one launch takes: two steps each, beside the three setup and attribution steps,
/// inside the evidence script's 64.
pub(super) const PICK_PAIRS_MAX: usize = 30;

/// The click's place, as a fraction of the output stage: the middle of the generated fixtures'
/// red top-left quadrant, a colour no whole-frame medoid of the four quadrants holds.
const CLICK_AT: [f64; 2] = [0.25, 0.25];

/// The run's script and the step numbers (from one) of its drags and its clicks, over an output
/// stage of `width` × `height`.
pub(super) fn script(
    samples: usize,
    width: u32,
    height: u32,
) -> (Vec<script::Step>, Vec<usize>, Vec<usize>) {
    let mut steps = vec![
        script::Step::Workspace(WorkspaceStep::default().mode("mask")),
        script::Step::Mask(MaskStep::New("colour-range".into())),
    ];
    let click = [
        (CLICK_AT[0] * f64::from(width)) as u32,
        (CLICK_AT[1] * f64::from(height)) as u32,
    ];
    let (mut drags, mut clicks) = (Vec::new(), Vec::new());
    for _ in 0..samples {
        steps.push(script::Step::pick_drag(
            [0, 0],
            [width - 1, height - 1],
            false,
        ));
        drags.push(steps.len());
        steps.push(script::Step::pick(click[0], click[1]));
        clicks.push(steps.len());
    }
    steps.push(script::Step::call(
        "mask.sample-input-area",
        json!({"mask": {"name": "Mask 1"}, "x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0}),
    ));
    (steps, drags, clicks)
}

pub(super) fn run(root: &Path, out: &Path, bin: &Path, options: &Options) -> Result {
    ensure(
        (1..=PICK_PAIRS_MAX).contains(&options.samples),
        format!("Pick samples are 1..{PICK_PAIRS_MAX} pairs of a whole-frame drag and a click"),
    )?;
    ensure(
        source_tag(options.source)? == SourceTag::Jpeg,
        "--mode pick measures a generated JPEG (generate-fixtures' 24mp.jpg or 60mp.jpg)",
    )?;
    // A reference whole-frame pick renders the whole prefix: allow seconds for each at 60 MP, inside
    // the editor's own 300 s scripted-evidence deadline.
    let deadline = Duration::from_secs(90 + 6 * options.samples as u64);
    let run = Run::tool(root, out, TOOL, bin, deadline)?;
    run.check(|run| measure(run, options))
}

/// The pick's figures: the press to the command sent and to the overlay adopted for `entry`, the
/// entry it committed.
fn timed(events: &[Value], step: usize, entry: &Value) -> Result<(f64, f64)> {
    let sent = events
        .iter()
        .position(|event| event["event"] == "script_step" && event["detail"]["step"] == json!(step))
        .ok_or_else(|| format!("Pick step {step} was never sent"))?;
    let pressed = elapsed(&events[sent])?;
    let after = &events[sent + 1..];
    let picked = after
        .iter()
        .find(|event| event["event"] == "range_pick")
        .ok_or_else(|| format!("Pick step {step} sent no pick"))?;
    ensure(
        picked["detail"]["error"].is_null(),
        format!("Pick step {step} was refused: {}", picked["detail"]),
    )?;
    let adopted = after
        .iter()
        .find(|event| {
            event["event"] == "mask_coverage_ready"
                && event["detail"]["identity"]["entry_id"] == *entry
        })
        .ok_or_else(|| format!("No overlay was adopted for pick step {step}'s entry"))?;
    Ok((elapsed(picked)? - pressed, elapsed(adopted)? - pressed))
}

fn measure(run: &mut Run, options: &Options) -> Result {
    let (root, out) = (&run.root().to_path_buf(), &run.out().to_path_buf());
    let source = options.source.canonicalize()?;
    let source_hash = hash(&source)?;
    let (width, height) = image::image_dimensions(&source)
        .map_err(|error| format!("{}: {error}", source.display()))?;
    let (steps, drags, clicks) = script(options.samples, width, height);
    ensure(
        steps.len() <= script::MAX_SCRIPT_STEPS,
        "Pick samples exceed the 64-step evidence bound",
    )?;
    let load_start = launch::load_average(root);
    let launch = rendered_by(
        gesture_launch(
            out,
            "pick",
            "pick-script.json",
            &steps,
            &source,
            false,
            options.window,
        ),
        options,
    )
    .watch(sampled(root, "pick"));
    let Launched {
        dir: evidence,
        watched: usage,
    } = run.launch(launch)?;
    let app = read_json(&evidence.join("result.json"))?;
    ensure(
        app["status"] == "captured"
            && app["had_input_errors"] == json!(false)
            && app["script"]
                .as_array()
                .is_some_and(|recorded| recorded.len() == steps.len()),
        format!("A pick step failed or never ran: {}", app["script"]),
    )?;
    let events = scenario::events(&evidence.join("events.jsonl"))?;
    let header = run.provenance(&events)?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    // The opened frame comes first, then one frame per step.
    let stack = |step: usize| -> Result<Value> {
        Ok(frame_at(frames, step, "pick")?["state"]["stack"].clone())
    };
    let mut rows = Vec::new();
    let mut per_pick = Vec::new();
    for (gesture, measured) in [("drag", &drags), ("click", &clicks)] {
        let (mut to_sent, mut to_overlay) = (Vec::new(), Vec::new());
        for &step in measured.iter() {
            let (before, after) = (stack(step - 1)?, stack(step)?);
            let (from, to) = (&before["revision"], &after["revision"]);
            ensure(
                to.as_u64()
                    .zip(from.as_u64())
                    .is_some_and(|(to, from)| to == from + 1),
                format!(
                    "Pick step {step} ({gesture}) moved the revision from {from} to {to}, not by one entry"
                ),
            )?;
            let (sent, adopted) = timed(&events, step, &after["entry"])?;
            to_sent.push(sent);
            to_overlay.push(adopted);
            per_pick.push(json!({"step": step, "gesture": gesture,
                "press_to_pick_sent_ms": sent, "press_to_overlay_adopted_ms": adopted}));
        }
        rows.push(stats::row(
            &format!("{gesture}_press_to_pick_sent"),
            "ms",
            to_sent,
        ));
        rows.push(stats::row(
            &format!("{gesture}_press_to_overlay_adopted"),
            "ms",
            to_overlay,
        ));
    }
    let area = &app["script"][steps.len() - 1]["result"];
    let last = frames.last().ok_or("No frame was captured")?;
    rows.extend(resource_rows(&usage, last));
    let load_end = launch::load_average(root);
    let mut result = json!({
        "status": "passed",
        "source": source,
        "source_sha256": source_hash,
        "source_dimensions": [width, height],
        "backend": last["state"]["backend"],
        "renderer": last["state"]["renderer"],
        "area_renderer": area["renderer"],
        "area_reduction": area["reduction"],
        "area_points": area["points"].as_array().map_or(0, Vec::len),
        "reference_renderer": options.no_gpu_render,
        "mode": "pick",
        "samples": options.samples,
        "method": "Background evidence launch of the release binary, warm filesystem cache, at Fit, with Mask mode entered and one colour range created from New mask (the eyedropper armed, the overlay Tint, nothing bound). Each sample is a pick step dragged from the output stage's top-left pixel to its bottom-right one, then a pick step clicked on the red quadrant, which changes the sample back so the next drag commits again. Times are from the script_step that pressed each pick: to the range_pick that sent its command, and to the first mask_coverage_ready for the entry the pick committed.",
        "rows": rows,
        "picks": per_pick,
        "load": launch::load(load_start),
        "load_average_1m_end": load_end,
        "resources": {"rss_samples": usage["rss_samples"]},
        "scope": "A range pick on an unbound colour range at Fit on this source: the press on the photograph to the pick's command sent, and to the overlay adopted for the entry it committed. A drag reads the whole frame's area at the mask's input site; area_renderer names the renderer that answered it. Not display scanout.",
    });
    report_geometry(&mut result, options);
    stamp(&mut result, &header);
    write_json(&out.join("latency.json"), &result)?;
    ensure(hash(&source)? == source_hash, "The source changed")?;
    println!("PASS editor latency (pick): {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// At its largest the pick script fits the evidence script, alternates a whole-frame drag with
    /// a click on the red quadrant, numbers them as the editor does, and ends with the area read.
    #[test]
    fn a_pick_script_alternates_whole_frame_drags_and_clicks_within_the_script_limit() {
        let (steps, drags, clicks) = script(PICK_PAIRS_MAX, 6000, 4000);
        assert!(steps.len() <= script::MAX_SCRIPT_STEPS, "{}", steps.len());
        assert_eq!(
            (drags.len(), clicks.len()),
            (PICK_PAIRS_MAX, PICK_PAIRS_MAX)
        );
        assert_eq!((drags[0], clicks[0]), (3, 4));
        assert_eq!(
            steps[drags[0] - 1],
            script::Step::pick_drag([0, 0], [5999, 3999], false)
        );
        assert_eq!(steps[clicks[0] - 1], script::Step::pick(1500, 1000));
        assert!(
            serde_json::to_string(steps.last().unwrap())
                .unwrap()
                .contains("mask.sample-input-area")
        );
    }
}
