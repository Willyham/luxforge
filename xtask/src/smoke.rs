use crate::{
    basic_smoke as basic, controls_smoke as controls, crop_smoke as crop, gallery_smoke as gallery,
    histogram_smoke as histogram, mixer_smoke as mixer, presence_smoke as presence,
    vignette_smoke as vignette, workspace_smoke as workspace, *,
};
use std::{
    process::{Child, Stdio},
    time::{Duration, Instant},
};
/// Every rendered scenario, in the order `verify --tier rendered` runs them. One list: `main.rs`
/// and `verify` both reach a scenario through [`dispatch`], so a new scenario is named here once.
pub const SCENARIOS: [&str; 22] = [
    "empty",
    "load",
    "replacement",
    "invalid",
    "repeated",
    "alternating",
    "large24",
    "large60",
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
    "gallery",
    "controls",
    "unavailable",
];

/// Run one scenario, including the two that are not a single launch: a module can only be disabled
/// at startup, and persistence across a restart needs a second process.
pub fn dispatch(root: &Path, out: &Path, scenario: &str, bin: &Path, timeout: Duration) -> Result {
    match scenario {
        "unavailable" => workspace::run_unavailable(root, out, bin, timeout),
        "basic-restart" => basic::run_restart(root, out, bin, timeout),
        _ => run(root, out, scenario, bin, timeout),
    }
}

pub struct Guard {
    pub child: Child,
    _launch: launch::Background,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
pub fn spawn(root: &Path, bin: &Path, args: &[OsString], log: &Path) -> Result<Guard> {
    let launch = launch::Background::new(bin)?;
    let f = fs::File::create(log)?;
    Ok(Guard {
        child: Command::new(&launch.executable)
            .args(args)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(f.try_clone()?)
            .stderr(f)
            .spawn()?,
        _launch: launch,
    })
}

/// Launch the editor itself: the caller's arguments with the hidden-window flag the harness always
/// passes. Children that are not the editor (the probe binary, xtask's own test children) use
/// [`spawn`] directly.
pub fn spawn_editor(root: &Path, bin: &Path, args: &[OsString], log: &Path) -> Result<Guard> {
    spawn(root, bin, &launch::editor_args(args), log)
}

pub fn wait(child: &mut Guard, timeout: Duration) -> Result<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.child.try_wait()? {
            return Ok(status);
        }
        ensure(
            start.elapsed() < timeout,
            "Child timed out; killed and reaped",
        )?;
        std::thread::sleep(Duration::from_millis(10));
    }
}
/// What a captured frame must show. Defaults describe the fixture at Fit; a crop changes the ratio
/// the displayed image has and, when it is straightened, where its quadrants land.
pub struct Expect {
    /// Which EXIF orientation's quadrant order the fixture was saved with.
    pub orientation: u8,
    /// The displayed ratio, or the fixture's own when `None`.
    pub aspect: Option<f64>,
    /// The physical x range of the editor's photo surface; the whole width without it.
    pub columns: Option<[u32; 2]>,
    /// How far the measured ratio may differ from the expected one.
    pub tolerance: f64,
    /// The smallest fraction of the capture height the image may occupy. A wide crop fills less of
    /// the surface than the fixture does.
    pub min_height: f64,
    /// Whether the image must be centred in the photo surface.
    pub centred: bool,
    /// Whether the four quarter points must show the four quadrant colours. A straightened crop
    /// rotates the quadrant boundaries, so it only requires all four colours to be present.
    pub quadrants: bool,
}

impl Expect {
    pub fn fit(orientation: u8) -> Self {
        Self {
            orientation,
            aspect: None,
            columns: None,
            tolerance: 0.015,
            min_height: 0.5,
            centred: true,
            quadrants: true,
        }
    }
}

/// Check the captured frame shows the fixture, at Fit unless the expectation says otherwise.
pub fn pixels(path: &Path, expect: &Expect) -> Result<Value> {
    ensure(
        (1..=8).contains(&expect.orientation),
        "Orientation must be 1..8",
    )?;
    let img = image::open(path)?.to_rgb8();
    let (w, h) = img.dimensions();
    let [surface_left, surface_right] = expect.columns.unwrap_or([0, w]);
    ensure(
        surface_left < surface_right && surface_right <= w,
        "Invalid surface columns",
    )?;
    let surface_width = surface_right - surface_left;
    let colors = crate::fixtures::ORDERS[(expect.orientation - 1) as usize]
        .map(|i| crate::fixtures::COLORS[i]);
    let matches = |p: &[u8], c: [u8; 3]| p.iter().zip(c).all(|(a, b)| a.abs_diff(b) <= 8);
    let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
    let mut counts = [0u32; 4];
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let pixel = &img.get_pixel(x, y).0;
            if let Some(index) = colors.iter().position(|c| matches(pixel, *c)) {
                counts[index] += 1;
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 4);
                bottom = bottom.max(y + 4);
            }
        }
    }
    ensure(
        counts.iter().sum::<u32>() > 0,
        "No fixture pixels: blank or wrong render",
    )?;
    let width = right - left;
    let height = bottom - top;
    ensure(
        left >= surface_left && right <= surface_right,
        "Image outside the photo surface",
    )?;
    ensure(
        width as f64 > surface_width as f64 * 0.2
            && height as f64 > h as f64 * expect.min_height.max(0.0),
        "Fixture too small",
    )?;
    let aspect = expect.aspect.unwrap_or(if expect.orientation >= 5 {
        2.0 / 3.0
    } else {
        3.0 / 2.0
    });
    ensure(aspect.is_finite() && aspect > 0.0, "Invalid aspect")?;
    let measured = width as f64 / height as f64;
    ensure(
        (measured - aspect).abs() < expect.tolerance,
        format!("Incorrect displayed aspect ratio {measured:.4}, expected {aspect:.4}"),
    )?;
    ensure(
        !expect.centred
            || ((left + right) as f64 / 2.0 - (surface_left + surface_right) as f64 / 2.0).abs()
                <= 5.0,
        "Image not centered",
    )?;
    let mut actual = Vec::new();
    if expect.quadrants {
        for ((fx, fy), color) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)]
            .into_iter()
            .zip(colors)
        {
            let x = (left as f64 + fx * width as f64).round() as u32;
            let y = (top as f64 + fy * height as f64).round() as u32;
            ensure(x < w && y < h, "Pixel bounds")?;
            let p = img.get_pixel(x, y).0;
            ensure(matches(&p, color), "Wrong orientation/color")?;
            actual.push(p);
        }
    } else {
        // A straightened crop moves the quadrant boundaries, so prove real content instead: every
        // quadrant colour is still present in quantity.
        ensure(
            counts.iter().all(|count| *count >= 32),
            format!("A quadrant colour is missing from the crop: sampled counts {counts:?}"),
        )?;
    }
    Ok(
        json!({"status":"passed","physical_size":[w,h],"surface_columns":[surface_left,surface_right],"image_bounds":[left,top,right,bottom],"measured_aspect":measured,"expected_aspect":aspect,"aspect_tolerance":expect.tolerance,"quadrant_sample_counts":counts,"corner_rgb":actual,"tolerance_per_channel":8,"scope":"Displayed geometry and sRGB interiors; not monitor calibration or native picker"}),
    )
}
/// The inclusive `(start, end)` of the longest contiguous run of matching positions, or `None` if
/// none matched at all. Used to find a photograph's own drawn extent by its widest row and tallest
/// column: unlike the leftmost-to-rightmost span between any two matches, a contiguous run is never
/// fooled by scattered chrome (title-bar text, an icon) that happens to span a wide gap of
/// unmatched background between its own characters or glyphs.
pub fn longest_run(positions: impl Iterator<Item = (u32, bool)>) -> Option<(u32, u32)> {
    let mut best: Option<(u32, u32)> = None;
    let mut run_start = None;
    for (position, matched) in positions {
        if matched {
            let start = *run_start.get_or_insert(position);
            if best.is_none_or(|(best_start, best_end)| position - start > best_end - best_start) {
                best = Some((start, position));
            }
        } else {
            run_start = None;
        }
    }
    best
}
pub fn columns(frame: &Value) -> Result<Option<[u32; 2]>> {
    match frame.get("surface_columns") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let pair: [u32; 2] = serde_json::from_value(value.clone())?;
            Ok(Some(pair))
        }
    }
}
pub fn events(path: &Path) -> Result<Vec<Value>> {
    fs::read_to_string(path)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
/// The run's own result and log, with the lifecycle, run identity and frame count checked. `frames`
/// is how many captures the scenario must have produced.
pub fn preamble(evidence: &Path, frames: usize) -> Result<(Value, Vec<Value>)> {
    let app = read_json(&evidence.join("result.json"))?;
    let events = events(&evidence.join("events.jsonl"))?;
    ensure(
        events.first().is_some_and(|e| e["event"] == "startup")
            && events.last().is_some_and(|e| e["event"] == "shutdown"),
        "Missing lifecycle",
    )?;
    ensure(app["status"] == "captured", "Unsuccessful app result")?;
    ensure(
        app["run_id"].as_str().is_some_and(|s| !s.is_empty())
            && events.iter().all(|e| e["run_id"] == app["run_id"]),
        "Wrong log run identity",
    )?;
    ensure(
        app["frames"]
            .as_array()
            .is_some_and(|captured| captured.len() == frames),
        "Missing/stale frames",
    )?;
    Ok((app, events))
}

/// A frame's provenance: whose run it belongs to, that it names a backend, that it came from a
/// renderer readback, and that the state file written beside it says the same thing.
pub fn frame_identity(evidence: &Path, app: &Value, frame: &Value) -> Result<PathBuf> {
    let state = &frame["state"];
    ensure(state["run_id"] == app["run_id"], "Wrong frame run identity")?;
    ensure(
        ["backend", "adapter"]
            .iter()
            .all(|k| state["backend"][k].as_str().is_some_and(|s| !s.is_empty())),
        "Missing backend",
    )?;
    ensure(
        frame["capture_provenance"] == "window-renderer-readback",
        "Wrong capture provenance",
    )?;
    let name = Path::new(frame["file"].as_str().ok_or("Missing capture filename")?);
    ensure(
        name.components()
            .all(|c| matches!(c, std::path::Component::Normal(_))),
        "Unsafe capture path",
    )?;
    let number = name
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("frame-"))
        .ok_or("Unexpected capture filename")?;
    ensure(
        &read_json(&evidence.join(format!("state-{number}.json")))? == frame,
        "The state file beside the frame disagrees with the run result",
    )?;
    Ok(evidence.join(name))
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
        let path = frame_identity(evidence, &app, frame)?;
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
            let img = image::open(&path)?.to_rgb8();
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
            pixels(
                &path,
                &Expect {
                    aspect: match scenario {
                        "large24" => Some(1.5),
                        "large60" => Some(5.0 / 3.0),
                        _ => None,
                    },
                    columns: columns(frame)?,
                    ..Expect::fit(orientation)
                },
            )?;
            ensure(
                events.iter().any(|e| {
                    e["event"] == "render_ready" && e["generation"] == state["displayed_generation"]
                }),
                "Missing upload readiness",
            )?;
        }
    }
    Ok(app)
}
pub fn run(root: &Path, out: &Path, scenario: &str, bin: &Path, timeout: Duration) -> Result {
    ensure(!out.exists(), "Smoke output must be new")?;
    let fixture = root.join("fixtures/s0/orientation-6.jpg");
    let sources = match scenario {
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
        _ => return Err("Unknown smoke scenario".into()),
    };
    fs::create_dir_all(out)?;
    let evidence = out.join("app");
    let mut args = vec!["--evidence-dir".into(), evidence.clone().into_os_string()];
    if matches!(scenario, "gallery" | "controls") {
        args.push("--developer".into());
    }
    for p in &sources {
        args.extend(["--open".into(), p.as_os_str().into()]);
    }
    if let Some(script) = gallery::script(scenario).or_else(|| controls::script(scenario)) {
        let window = if scenario == "gallery" {
            gallery::WINDOW
        } else {
            controls::WINDOW
        };
        let file = out.join("script.json");
        write_json(&file, &script)?;
        args.extend([
            "--evidence-script".into(),
            file.into_os_string(),
            "--window-size".into(),
            window[0].into(),
            window[1].into(),
        ]);
    } else if let Some(script) = crop::script(scenario)? {
        // The crop frames need room for the overlay at Fit and at 100%.
        let file = out.join("script.json");
        write_json(&file, &script)?;
        args.extend([
            "--evidence-script".into(),
            file.into_os_string(),
            "--window-size".into(),
            "1280".into(),
            "800".into(),
        ]);
    } else if let Some(script) = workspace::script(scenario)
        .or_else(|| basic::script(scenario))
        .or_else(|| basic::panel_script(scenario))
    {
        let file = out.join("script.json");
        write_json(&file, &script)?;
        args.extend([
            "--evidence-script".into(),
            file.into_os_string(),
            "--window-size".into(),
            workspace::WINDOW[0].into(),
            workspace::WINDOW[1].into(),
        ]);
    } else if let Some(script) = histogram::script(scenario)
        .or_else(|| histogram::crop_script(scenario))
        .or_else(|| presence::script(scenario))
        .or_else(|| mixer::script(scenario))
        .or_else(|| vignette::script(scenario))
    {
        let file = out.join("script.json");
        write_json(&file, &script)?;
        args.extend([
            "--evidence-script".into(),
            file.into_os_string(),
            "--window-size".into(),
            histogram::WINDOW[0].into(),
            histogram::WINDOW[1].into(),
        ]);
    }
    // The recorded command is what actually runs, hidden-window flag included.
    let args = launch::editor_args(&args);
    let command = std::iter::once(bin.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut result = json!({"scenario":scenario,"status":"failed","command":command,"launch_mode":launch::MODE,"platform":format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH)});
    let check = (|| -> Result {
        let mut hashes = serde_json::Map::new();
        for p in &sources {
            hashes.insert(
                p.file_name().unwrap().to_string_lossy().into_owned(),
                json!(hash(p)?),
            );
        }
        result["fixture_hashes"] = json!(hashes);
        result["binary_sha256"] = json!(hash(bin)?);
        result["lockfile_sha256"] = json!(hash(&root.join("Cargo.lock"))?);
        let mut child = spawn(root, bin, &args, &out.join("subprocess.log"))?;
        let status = wait(&mut child, timeout)?;
        result["exit_code"] = json!(status.code());
        ensure(status.success(), format!("Application exit {status}"))?;
        let app = verify(&evidence, scenario, sources.len())?;
        for p in &sources {
            ensure(
                json!(hash(p)?)
                    == result["fixture_hashes"][p.file_name().unwrap().to_string_lossy().as_ref()],
                "Source changed",
            )?;
        }
        result["backend"] =
            app["frames"].as_array().unwrap().last().unwrap()["state"]["backend"].clone();
        Ok(())
    })();
    match &check {
        Ok(()) => result["status"] = json!("passed"),
        Err(e) => result["error"] = json!(e.to_string()),
    };
    write_json(&out.join("result.json"), &result)?;
    fs::write(
        out.join("reproduce.md"),
        format!(
            "# Smoke run\n\nScenario: {scenario}. Status: {}.\n\nLaunch mode: {}. Reproduce with `cargo xtask smoke --scenario {scenario} --output NEW_DIR --binary PATH`; on macOS this copies the binary into a temporary background-only bundle and the editor runs with `--hidden-window`, so its window is never placed on the desktop. Running the argument array directly bypasses that focus protection.\n\nArgument array:\n\n```json\n{}\n```\n\nActual renderer readback; native dialog/focus verified separately. Synthetic fixtures only.\n",
            result["status"],
            launch::MODE,
            serde_json::to_string_pretty(&command)?
        ),
    )?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    check
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_binary_retains_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("evidence ü space");
        assert!(
            run(
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
    #[test]
    fn blank_frame_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("blank.png");
        image::RgbImage::new(960, 640).save(&p).unwrap();
        assert!(
            pixels(&p, &Expect::fit(6))
                .unwrap_err()
                .to_string()
                .contains("blank")
        );
        assert!(
            pixels(
                &p,
                &Expect {
                    columns: Some([10, 5]),
                    ..Expect::fit(6)
                }
            )
            .unwrap_err()
            .to_string()
            .contains("surface columns")
        );
    }
    #[test]
    #[ignore]
    fn sleeping_child() {
        std::thread::sleep(Duration::from_secs(30));
    }
    #[test]
    fn timeout_reaps_child() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = spawn(
            &root().unwrap(),
            &std::env::current_exe().unwrap(),
            &[
                "--ignored".into(),
                "--exact".into(),
                "smoke::tests::sleeping_child".into(),
            ],
            &tmp.path().join("child.log"),
        )
        .unwrap();
        assert!(
            wait(&mut child, Duration::from_millis(50))
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        child.child.kill().unwrap();
        assert!(child.child.wait().is_ok());
    }
}
