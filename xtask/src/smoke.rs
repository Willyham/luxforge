use crate::{
    basic_smoke as basic, capabilities_smoke as capabilities, controls_smoke as controls,
    crop_smoke as crop, gallery_smoke as gallery, histogram_smoke as histogram,
    mask_brush_smoke as mask_brush, mask_combine_smoke as mask_combine,
    mask_range_smoke as mask_range, mask_smoke as mask, mixer_smoke as mixer,
    performance_smoke as performance, presence_smoke as presence, presets_smoke as presets,
    raw_panel_smoke as raw_panel,
    scenario::{Expect, Frame, Launch, Run, preamble},
    vignette_smoke as vignette, workspace_smoke as workspace, zoom_smoke as zoom, *,
};
use std::{borrow::Borrow, time::Duration};
/// Every rendered scenario, in the order `verify --tier rendered` runs them. One list: `main.rs`
/// and `verify` both reach a scenario through [`dispatch`], so a new scenario is named here once.
pub const SCENARIOS: [&str; 30] = [
    "empty",
    "load",
    "replacement",
    "invalid",
    "repeated",
    "alternating",
    "large24",
    "large60",
    "zoom",
    "crop",
    "crop-draft",
    "workspace",
    "basic",
    "basic-panel",
    "basic-crop",
    "basic-restart",
    "histogram",
    "presence",
    "mixer",
    "vignette",
    "presets",
    mask::SCENARIO,
    mask_combine::SCENARIO,
    mask_brush::SCENARIO,
    mask_range::SCENARIO,
    "performance",
    "gallery",
    "controls",
    "capabilities",
    "unavailable",
];

/// What one scenario runs: a scenario of its own shape, or one plain launch over its sources.
enum Plan {
    Own(fn(Run) -> Result),
    Plain(Vec<PathBuf>),
}

/// The plan for `scenario`, or the reason it has none. The scenarios that are not a single plain
/// launch are named here: a module can only be disabled at startup, persistence across a restart
/// and a mask reopened in a new process need a second launch, `zoom` runs its script over the 24 MP
/// and the 60 MP photograph in turn, and the capability scenario runs its proof endpoint in this
/// process. `sources`, when given, replaces a plain scenario's own fixtures.
fn plan(root: &Path, scenario: &str, sources: Option<Vec<PathBuf>>) -> Result<Plan> {
    Ok(match scenario {
        "unavailable" => Plan::Own(workspace::unavailable),
        "basic-restart" => Plan::Own(basic::restart),
        zoom::SCENARIO => Plan::Own(zoom::run),
        "capabilities" => Plan::Own(capabilities::run),
        // Two launches over one catalog: the mask is drawn and edited in the first, reopened in
        // the second.
        mask::SCENARIO => Plan::Own(mask::run),
        // Four components in three modes in one mask, its coverage read off the overlay.
        mask_combine::SCENARIO => Plan::Own(mask_combine::run),
        // Three launches: strokes painted, erased and deleted in the first, a subtracting brush in
        // the second, and painting carried on in the third over the edge, at 100% and under a
        // rotated crop.
        mask_brush::SCENARIO => Plan::Own(mask_brush::run),
        // Two launches over one catalog: the range selections typed, picked and combined in the
        // first, and their own limits taken one at a time in the second.
        mask_range::SCENARIO => Plan::Own(mask_range::run),
        _ => Plan::Plain(match sources {
            Some(sources) => sources,
            None => sources_for(root, scenario)?,
        }),
    })
}

fn execute(run: Run, plan: Plan) -> Result {
    match plan {
        Plan::Own(scenario) => scenario(run),
        Plan::Plain(sources) => plain(run, sources),
    }
}

/// Run one scenario into `out`.
pub fn dispatch(root: &Path, out: &Path, scenario: &str, bin: &Path, timeout: Duration) -> Result {
    let plan = plan(root, scenario, None)?;
    execute(Run::start(root, out, scenario, bin, timeout)?, plan)
}

/// Run one plain scenario over the given sources: a scenario whose photograph cannot be checked in
/// (`raw-panel`) is handed its source instead.
pub fn run_sources(
    root: &Path,
    out: &Path,
    scenario: &str,
    bin: &Path,
    timeout: Duration,
    sources: Vec<PathBuf>,
) -> Result {
    let plan = plan(root, scenario, Some(sources))?;
    execute(Run::start(root, out, scenario, bin, timeout)?, plan)
}

/// Rerun a recorded run's checks without launching: `recorded` is copied into `out` and the
/// scenario's own code runs over the copy, each launch being the one recorded there. `sources`
/// names a `--source` run's source again.
pub fn verify_only(
    root: &Path,
    recorded: &Path,
    out: &Path,
    scenario: &str,
    sources: Option<Vec<PathBuf>>,
) -> Result {
    let plan = plan(root, scenario, sources)?;
    execute(Run::replay(root, recorded, out, scenario)?, plan)
}

/// The longest plausible render of one preview phase on the fixtures a scenario opens, in
/// milliseconds. A release render of the 60 MP fixture's exact phase is well under a second; the
/// bound exists to catch a figure that is not a render time at all, such as the time since the last
/// request, which grows with the length of the run.
pub const RENDER_MS_BOUND: f64 = 5000.0;

/// The status bar's wording of one frame's render time, exactly as the editor's
/// `state::status::RenderTime` formats it, so a captured frame's text is checked against its own
/// figure rather than against a copy of the text. `approximate` is a frame that approximates a
/// drafted RAW white balance.
pub fn render_text(ms: f64, proxy: bool, approximate: bool) -> String {
    let figure = if ms < 0.5 {
        "<1".to_owned()
    } else {
        format!("{}", ms.round() as i64)
    };
    let phase = match (proxy, approximate) {
        (true, true) => " (proxy, approximate)",
        (true, false) => " (proxy)",
        (false, true) => " (approximate)",
        (false, false) => "",
    };
    format!("Rendered in {figure} ms{phase}")
}

/// Every presented frame's render time, from its `preview_displayed` event: the preview worker's
/// own time for the phase on screen. Each must be a finite number of milliseconds in
/// `0..RENDER_MS_BOUND`, and there must be at least one. Then, for every captured frame, the status
/// bar either says the renderer is busy or states a figure that one of those events carried, in
/// exactly the editor's wording, with `(proxy)` exactly when the frame on screen is the proxy and
/// `approximate` exactly when it approximates a drafted RAW white balance. Returns the evidence
/// record.
pub fn expect_render_times<F: Borrow<Value>>(events: &[Value], frames: &[F]) -> Result<Value> {
    let mut displayed = Vec::new();
    for event in events.iter().filter(|e| e["event"] == "preview_displayed") {
        let detail = &event["detail"];
        let ms = detail["render_ms"]
            .as_f64()
            .ok_or_else(|| format!("A preview_displayed event carries no render_ms: {detail}"))?;
        ensure(
            ms.is_finite() && (0.0..RENDER_MS_BOUND).contains(&ms),
            format!(
                "Generation {} reports a render of {ms} ms, outside 0..{RENDER_MS_BOUND}",
                detail["generation"]
            ),
        )?;
        displayed.push(json!({"generation":detail["generation"],"proxy":detail["proxy"],"reason":detail["reason"],"render_ms":ms}));
    }
    ensure(
        !displayed.is_empty(),
        "No preview_displayed event: nothing reported a render time",
    )?;
    let figures: Vec<f64> = displayed
        .iter()
        .filter_map(|d| d["render_ms"].as_f64())
        .collect();
    let mut shown = Vec::new();
    for frame in frames {
        let frame: &Value = frame.borrow();
        let bar = &frame["state"]["status_bar"];
        let text = bar["render"]
            .as_str()
            .ok_or("A frame records no status bar render text")?;
        if text == "Rendering\u{2026}" {
            shown.push(json!({"frame":frame["file"],"render":text}));
            continue;
        }
        let Some(ms) = bar["render_ms"].as_f64() else {
            ensure(
                text == "Idle",
                format!(
                    "{}: the status bar says {text:?} with no render time",
                    frame["file"]
                ),
            )?;
            continue;
        };
        ensure(
            figures.contains(&ms),
            format!(
                "{}: the status bar's {ms} ms is no presented frame's own render time",
                frame["file"]
            ),
        )?;
        let proxy = bar["render_proxy"] == json!(true);
        ensure(
            proxy == (frame["state"]["proxy"]["presented"] == json!(true)),
            format!(
                "{}: the status bar's proxy label disagrees with the frame on screen",
                frame["file"]
            ),
        )?;
        let approximate = bar["render_approximate"] == json!(true);
        ensure(
            approximate == (frame["state"]["approximate_white_balance"] == json!(true)),
            format!(
                "{}: the status bar's approximate label disagrees with the frame on screen",
                frame["file"]
            ),
        )?;
        ensure(
            text == render_text(ms, proxy, approximate),
            format!(
                "{}: the status bar says {text:?} for {ms} ms",
                frame["file"]
            ),
        )?;
        shown.push(json!({"frame":frame["file"],"render":text,"render_ms":ms,"proxy":proxy,"approximate":approximate}));
    }
    Ok(json!({"bound_ms":RENDER_MS_BOUND,"preview_displayed":displayed,"status_bar":shown}))
}

/// Evidence records the parser's explicit default `finish: open` on picker/curve steps. Match a
/// script's shorter spelling to that same parsed request without weakening any other field.
pub fn script_request_matches(recorded: &Value, scripted: &Value) -> bool {
    let mut normalized = recorded.clone();
    for kind in ["picker", "curve"] {
        if scripted
            .get(kind)
            .is_some_and(|step| step.get("finish").is_none())
            && let Some(object) = normalized.get_mut(kind).and_then(Value::as_object_mut)
        {
            object.remove("finish");
        }
    }
    normalized == *scripted
}

pub fn verify(evidence: &Path, scenario: &str, count: usize) -> Result<Value> {
    if let Some(frames) = gallery::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        gallery::verify(evidence, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = controls::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        controls::verify(evidence, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = crop::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        crop::verify(evidence, scenario, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = workspace::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        workspace::verify(evidence, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = basic::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        basic::verify_scenario(evidence, scenario, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = histogram::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        if scenario == "basic-crop" {
            histogram::verify_crop(&root()?, evidence, &app, &events)?;
        } else {
            histogram::verify(&root()?, evidence, &app, &events)?;
        }
        return Ok(app);
    }
    if let Some(frames) = presence::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        presence::verify(evidence, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = mixer::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        mixer::verify(evidence, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = vignette::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        vignette::verify(evidence, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = presets::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        presets::verify(evidence, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = zoom::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        zoom::verify(evidence, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = raw_panel::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        raw_panel::verify(evidence, &app, &events)?;
        return Ok(app);
    }
    if let Some(frames) = performance::frames(scenario) {
        let (app, events) = preamble(evidence, frames)?;
        performance::verify(evidence, &app, &events)?;
        return Ok(app);
    }
    let (app, events) = preamble(evidence, count.max(1))?;
    let frames = app["frames"].as_array().ok_or("Missing frames")?;
    for (index, frame) in frames.iter().enumerate() {
        let state = &frame["state"];
        let generation = if count == 0 { 0 } else { index + 1 };
        let orientation =
            if scenario.starts_with("large") || (scenario == "alternating" && index % 2 == 1) {
                1
            } else {
                6
            };
        ensure(state["run_id"] == app["run_id"], "Wrong frame run identity")?;
        ensure(
            state["requested_generation"] == generation,
            "Wrong requested generation",
        )?;
        let captured = Frame::identified(evidence, &app, frame)?;
        if matches!(scenario, "empty" | "invalid") {
            ensure(
                state["phase"]
                    == if scenario == "empty" {
                        "empty"
                    } else {
                        "error"
                    }
                    && state["displayed_generation"] == 0,
                "Wrong empty/error state",
            )?;
            if scenario == "invalid" {
                ensure(state["error_code"] == "invalid-input", "Wrong error")?;
            }
            let img = captured.image()?;
            let colors: std::collections::BTreeSet<_> =
                img.pixels().map(|p| p.0).take(20_000_000).collect();
            ensure(colors.len() > 10, "Blank empty UI")?;
        } else {
            let displayed = if matches!(scenario, "repeated" | "alternating") {
                generation
            } else {
                1
            };
            ensure(
                state["displayed_generation"] == displayed,
                "Stale displayed image",
            )?;
            let dims = match scenario {
                "large24" => [6000, 4000],
                "large60" => [10000, 6000],
                _ => {
                    if orientation == 1 {
                        [480, 320]
                    } else {
                        [320, 480]
                    }
                }
            };
            ensure(
                state["source_dimensions"] == json!(dims),
                "Wrong dimensions",
            )?;
            let failed = scenario == "replacement" && index == 1;
            ensure(
                state["phase"] == if failed { "error" } else { "ready" },
                "Wrong phase",
            )?;
            if failed {
                ensure(
                    state["error_code"] == "invalid-input",
                    "Wrong replacement error",
                )?;
            }
            captured.fixture(Expect {
                aspect: match scenario {
                    "large24" => Some(1.5),
                    "large60" => Some(5.0 / 3.0),
                    _ => None,
                },
                ..Expect::fit(orientation)
            })?;
            ensure(
                events.iter().any(|e| {
                    e["event"] == "render_ready" && e["generation"] == state["displayed_generation"]
                }),
                "Missing upload readiness",
            )?;
        }
    }
    if scenario.starts_with("large") {
        // A photo-sized source at Fit is shown as its display proxy, so the status bar's figure is
        // the proxy phase's own render time and says so. A capture can land while a refit or the
        // exact phase is still running, when the bar says "Rendering…"; the figure behind it is
        // still recorded, and it must be the proxy's.
        let record = expect_render_times(&events, frames)?;
        ensure(
            frames.iter().all(|frame| {
                let bar = &frame["state"]["status_bar"];
                bar["render_proxy"] == json!(true)
                    && bar["render"].as_str().is_some_and(|text| {
                        text.ends_with("(proxy)") || text == "Rendering\u{2026}"
                    })
            }),
            "A photo-sized frame at Fit does not report the proxy's render time",
        )?;
        write_json(&evidence.join("render-times.json"), &record)?;
    }
    Ok(app)
}

/// The fixtures a plain scenario opens, in order.
fn sources_for(root: &Path, scenario: &str) -> Result<Vec<PathBuf>> {
    let fixture = root.join("fixtures/s0/orientation-6.jpg");
    Ok(match scenario {
        "empty" => vec![],
        "load" => vec![fixture],
        "replacement" => vec![fixture, root.join("fixtures/s0/invalid.jpg")],
        "invalid" => vec![root.join("fixtures/s0/invalid.jpg")],
        "repeated" => vec![fixture; 8],
        "alternating" => (0..4)
            .flat_map(|_| [fixture.clone(), root.join("fixtures/s0/orientation-1.jpg")])
            .collect(),
        "large24" => vec![root.join("fixtures/generated/24mp.jpg")],
        "large60" => vec![root.join("fixtures/generated/60mp.jpg")],
        // The crop scenarios drive the editor's crop workflow through an evidence script.
        "crop" | "crop-draft" => vec![root.join("fixtures/s0/orientation-1.jpg")],
        // `workspace` drives the panels, canvas mode, thirds, preview and palette; `basic` drives
        // the generated Exposure slider's whole gesture. Both need the window size the design's
        // layout constants are written against.
        "workspace" | "basic" | "gallery" => {
            vec![root.join("fixtures/s0/orientation-1.jpg")]
        }
        scenario if controls::source(scenario).is_some() => {
            vec![root.join(controls::source(scenario).expect("controls fixture"))]
        }
        // `basic-panel` drives the rest of the Basic section and the neutral picker. It opens the
        // greyscale fixture because the picker needs both a genuinely neutral patch to sample and
        // a clipped one to be refused on, and that fixture has each: uniform grey quadrants and a
        // white cross at code 255.
        "basic-panel" => vec![root.join("fixtures/s0/greyscale.jpg")],
        // `histogram` drives the inspector, the clipping overlays and the pointer readout over its
        // own fixture, whose clipped pixels are known from the generator.
        scenario if histogram::source(scenario).is_some() => {
            vec![root.join(histogram::source(scenario).expect("the scenario's fixture"))]
        }
        // `presence` drives the section over a generated fixture holding a gradient, a step edge,
        // a fine checker and a flat field, none of which the golden fixtures have on their own.
        scenario if presence::source(scenario).is_some() => {
            vec![root.join(presence::source(scenario).expect("the presence fixture"))]
        }
        // `mixer` drives the Colour mixer section over a generated hue wheel, so a hue rotation's
        // continuity across the spectrum can be inspected; the golden fixtures hold only four flat
        // quadrant colours.
        scenario if mixer::source(scenario).is_some() => {
            vec![root.join(mixer::source(scenario).expect("the mixer fixture"))]
        }
        // `vignette` drives the section over the ordinary quadrant fixture.
        scenario if vignette::source(scenario).is_some() => {
            vec![root.join(vignette::source(scenario).expect("the vignette fixture"))]
        }
        // `presets` imports, applies, creates and deletes library presets over the quadrant
        // fixture, whose flat colours each preset moves.
        scenario if presets::source(scenario).is_some() => {
            vec![root.join(presets::source(scenario).expect("the presets fixture"))]
        }
        // `performance` samples the editor while a heavy edit renders, so it opens the generated
        // 60 MP JPEG, whose exact render runs long enough to be listed as long work.
        scenario if performance::source(scenario).is_some() => {
            vec![root.join(performance::source(scenario).expect("the performance fixture"))]
        }
        raw_panel::SCENARIO => {
            return Err("The raw-panel scenario needs --source RAW_FILE".into());
        }
        _ => return Err("Unknown smoke scenario".into()),
    })
}

/// One plain launch of a scenario over `sources`, with its script and window, checked by
/// [`verify`].
pub fn plain(run: Run, sources: Vec<PathBuf>) -> Result {
    let scenario = run.scenario().to_owned();
    let mut launch = Launch::app();
    if matches!(scenario.as_str(), "gallery" | "controls") {
        launch = launch.developer();
    }
    launch = launch.open_all(&sources);
    if let Some(script) = gallery::script(&scenario).or_else(|| controls::script(&scenario)) {
        let window = if scenario == "gallery" {
            gallery::WINDOW
        } else {
            controls::WINDOW
        };
        launch = launch.script("script.json", script).window(window);
    } else if let Some(script) = crop::script(&scenario)? {
        // The crop frames need room for the overlay at Fit and at 100%.
        launch = launch.script("script.json", script).window(["1280", "800"]);
    } else if let Some(script) = workspace::script(&scenario)
        .or_else(|| basic::script(&scenario))
        .or_else(|| basic::panel_script(&scenario))
        .or_else(|| raw_panel::script(&scenario))
        .or_else(|| performance::script(&scenario, &sources))
    {
        launch = launch
            .script("script.json", script)
            .window(workspace::WINDOW);
    } else if let Some(script) = histogram::script(&scenario)
        .or_else(|| histogram::crop_script(&scenario))
        .or_else(|| presence::script(&scenario))
        .or_else(|| mixer::script(&scenario))
        .or_else(|| vignette::script(&scenario))
        .or_else(|| presets::script(&scenario))
        .or_else(|| zoom::script(&scenario))
    {
        launch = launch
            .script("script.json", script)
            .window(histogram::WINDOW);
    }
    // `performance` compares the memory the editor reports with readings the runner takes of the
    // same process while it runs, so it waits by watching.
    if performance::frames(&scenario).is_some() {
        launch = launch.watch(performance::READINGS, Box::new(performance::watch));
    }
    run.check(|run| {
        run.hash(&sources)?;
        let evidence = run.launch(launch)?;
        let app = verify(&evidence, &scenario, sources.len())?;
        run.sources_unchanged()?;
        run.record(
            "backend",
            app["frames"]
                .as_array()
                .and_then(|frames| frames.last())
                .map_or(Value::Null, |frame| frame["state"]["backend"].clone()),
        );
        Ok(())
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The render-time check accepts a presented frame's own figure in the editor's wording and
    /// refuses what the status bar used to show: a figure that is the time since the last request,
    /// a missing one, and a status bar that states a figure no frame reported.
    #[test]
    fn render_times_must_be_each_frames_own_and_plausible() {
        let displayed = |ms: Value| json!({"event":"preview_displayed","detail":{"generation":2,"proxy":true,"render_ms":ms}});
        let frame = |render: &str, ms: f64, proxy: bool| json!({"file":"frame-1.png","state":{"proxy":{"presented":proxy},"status_bar":{"render":render,"render_ms":ms,"render_proxy":proxy}}});
        assert_eq!(render_text(12.4, true, false), "Rendered in 12 ms (proxy)");
        assert_eq!(render_text(0.3, false, false), "Rendered in <1 ms");
        assert_eq!(
            render_text(9.2, true, true),
            "Rendered in 9 ms (proxy, approximate)"
        );
        assert_eq!(
            render_text(140.0, false, true),
            "Rendered in 140 ms (approximate)"
        );
        let good = expect_render_times(
            &[displayed(json!(12.4))],
            &[frame("Rendered in 12 ms (proxy)", 12.4, true)],
        );
        assert!(good.is_ok(), "{good:?}");
        // The old figure: half a million milliseconds since the open.
        let none: [Value; 0] = [];
        assert!(expect_render_times(&[displayed(json!(500_000.0))], &none).is_err());
        assert!(expect_render_times(&[displayed(Value::Null)], &none).is_err());
        assert!(
            expect_render_times(&[], &none).is_err(),
            "nothing was presented"
        );
        // A status bar stating a figure no presented frame carried.
        assert!(
            expect_render_times(
                &[displayed(json!(12.4))],
                &[frame("Rendered in 90 ms (proxy)", 90.0, true)]
            )
            .is_err()
        );
        // The proxy label must match the frame on screen.
        assert!(
            expect_render_times(
                &[displayed(json!(12.4))],
                &[frame("Rendered in 12 ms", 12.4, false)
                    .as_object()
                    .map(|object| {
                        let mut object = object.clone();
                        object["state"]["proxy"]["presented"] = json!(true);
                        Value::Object(object)
                    })
                    .unwrap()]
            )
            .is_err()
        );
    }

    #[test]
    fn missing_binary_retains_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("evidence ü space");
        assert!(
            dispatch(
                &root().unwrap(),
                &out,
                "load",
                &tmp.path().join("absent"),
                Duration::from_millis(100)
            )
            .is_err()
        );
        assert_eq!(
            read_json(&out.join("result.json")).unwrap()["status"],
            "failed"
        );
        assert!(out.join("reproduce.md").is_file());
        assert!(!out.join("app/frame-1.png").exists());
    }
    #[test]
    fn missing_evidence_and_stale_generation_fail() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(verify(tmp.path(), "load", 1).is_err());
        fs::write(tmp.path().join("events.jsonl"),"{\"event\":\"startup\",\"run_id\":\"current\"}\n{\"event\":\"shutdown\",\"run_id\":\"current\"}\n").unwrap();
        let mut app = json!({"status":"captured","run_id":"current","frames":[{"state":{"run_id":"old","requested_generation":1}}]});
        write_json(&tmp.path().join("result.json"), &app).unwrap();
        assert!(
            verify(tmp.path(), "load", 1)
                .unwrap_err()
                .to_string()
                .contains("run identity")
        );
        app["frames"][0]["state"]["run_id"] = json!("current");
        app["frames"][0]["state"]["requested_generation"] = json!(0);
        write_json(&tmp.path().join("result.json"), &app).unwrap();
        assert!(
            verify(tmp.path(), "load", 1)
                .unwrap_err()
                .to_string()
                .contains("generation")
        );
    }
}
