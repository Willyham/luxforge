use crate::*;
use std::time::{Duration, Instant};
fn await_log(
    child: &mut smoke::Guard,
    path: &Path,
    needle: &str,
    timeout: Duration,
) -> Result<String> {
    let start = Instant::now();
    loop {
        let text = fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return Ok(text);
        }
        ensure(
            child.child.try_wait()?.is_none(),
            format!("Child exited before {needle}: {text}"),
        )?;
        ensure(
            start.elapsed() < timeout,
            format!("Deadline waiting for {needle}"),
        )?;
        std::thread::sleep(Duration::from_millis(10));
    }
}
pub fn hardening(root: &Path, out: &Path, bin: &Path) -> Result {
    ensure(!out.exists(), "Hardening output must be new")?;
    fs::create_dir_all(out)?;
    let mut result = json!({"status":"failed","launch_mode":launch::MODE,"binary_sha256":hash(bin)?,"focus_checks":[],"checks":[]});
    let checked = (|| -> Result {
        let fixture = root.join("fixtures/s0/orientation-6.jpg");
        let before = hash(&fixture)?;
        {
            // Not the editor: xtask's own hanging child, so it carries no editor arguments.
            let mut child = smoke::spawn(
                root,
                &std::env::current_exe()?,
                &["__hang".into()],
                &out.join("hung.log"),
            )?;
            ensure(
                smoke::wait(&mut child, Duration::from_millis(200)).is_err(),
                "Hung child accepted",
            )?;
            smoke::note_focus(&mut result, &child)?;
        }
        let obstacle = out.join("not-a-directory");
        fs::write(&obstacle, "preserve")?;
        {
            let mut child = smoke::spawn_editor(
                root,
                bin,
                &[
                    "--evidence-dir".into(),
                    obstacle.join("evidence").into_os_string(),
                ],
                &out.join("initialization.log"),
            )?;
            let status = smoke::wait(&mut child, Duration::from_secs(5))?;
            smoke::note_focus(&mut result, &child)?;
            ensure(status.code() == Some(2), "Wrong initialization failure")?;
            ensure(
                fs::read_to_string(out.join("initialization.log"))?
                    .contains("Cannot create evidence directory"),
                "Missing initialization diagnosis",
            )?;
        }
        ensure(
            fs::read_to_string(&obstacle)? == "preserve",
            "Obstacle modified",
        )?;
        {
            // The data root is a plain file, so its log path cannot be created; the editor must
            // still import and render with an explicit catalog elsewhere.
            let log = out.join("diagnostics-unavailable.log");
            let mut child = smoke::spawn_editor(
                root,
                bin,
                &[
                    "--data-root".into(),
                    obstacle.into_os_string(),
                    "--catalog".into(),
                    out.join("degraded.sqlite").into_os_string(),
                    "--open".into(),
                    fixture.clone().into_os_string(),
                ],
                &log,
            )?;
            let text = await_log(
                &mut child,
                &log,
                "\"event\":\"decoded\"",
                Duration::from_secs(10),
            )?;
            ensure(
                text.contains("viewing continues"),
                "Missing degraded-diagnostics message",
            )?;
            smoke::note_focus(&mut result, &child)?;
        }
        let isolated = out.join("abrupt");
        let events = isolated.join("logs/events.jsonl");
        {
            let mut child = smoke::spawn_editor(
                root,
                bin,
                &[
                    "--data-root".into(),
                    isolated.clone().into_os_string(),
                    "--open".into(),
                    fixture.clone().into_os_string(),
                ],
                &out.join("abrupt.log"),
            )?;
            await_log(
                &mut child,
                &events,
                "\"event\":\"startup\"",
                Duration::from_secs(10),
            )?;
            smoke::note_focus(&mut result, &child)?;
        }
        // An abrupt kill may leave an incomplete final line; the first complete startup must survive.
        let text = fs::read_to_string(events)?;
        let first: Value = serde_json::from_str(text.lines().next().ok_or("Missing startup")?)?;
        ensure(first["event"] == "startup", "Wrong retained event")?;
        // The editor owns a catalog under the data root's config directory and nothing else;
        // no cache directory or other configuration appears.
        ensure(
            !isolated.join("cache").exists(),
            "Unexpected cache directory",
        )?;
        if isolated.join("config").exists() {
            for entry in fs::read_dir(isolated.join("config"))? {
                let name = entry?.file_name().to_string_lossy().into_owned();
                ensure(
                    name.starts_with("catalog."),
                    format!("Unexpected configuration file {name}"),
                )?;
            }
        }
        ensure(before == hash(&fixture)?, "Source modified")?;
        result["checks"] = json!([
            "Actual hung child killed and reaped",
            "Evidence initialization fails without changing existing files",
            "Normal decode survives unavailable diagnostics; child then terminated",
            "Abrupt termination retains startup; only the catalog under config, no cache or source mutation"
        ]);
        launch::focus_verdict(&result)
    })();
    match &checked {
        Ok(()) => result["status"] = json!("passed"),
        Err(e) => result["error"] = json!(e.to_string()),
    };
    write_json(&out.join("result.json"), &result)?;
    checked
}
fn usage(root: &Path, pid: u32) -> Result<(f64, f64)> {
    let raw = output(root, "ps", &["-o", "time=,rss=", "-p", &pid.to_string()])?;
    let fields: Vec<_> = raw.split_whitespace().collect();
    ensure(fields.len() == 2, "Missing ps measurements")?;
    let (min, sec) = fields[0].split_once(':').ok_or("Unexpected ps CPU time")?;
    Ok((
        min.parse::<f64>()? * 60.0 + sec.parse::<f64>()?,
        fields[1].parse::<f64>()? / 1024.0,
    ))
}
fn stats(values: &[f64]) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    let median = if n.is_multiple_of(2) {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    } else {
        sorted[n / 2]
    };
    json!({"median":median,"p95":sorted[(n*95/100).min(n-1)],"first":values[0]})
}
pub fn measure(root: &Path, out: &Path, bin: &Path, samples: usize) -> Result {
    ensure(
        cfg!(target_os = "macos"),
        "Measurement currently supports native macOS ps only",
    )?;
    ensure((1..=1000).contains(&samples), "Samples must be 1..1000")?;
    ensure(!out.exists(), "Measurement output must be new")?;
    fs::create_dir_all(out)?;
    let mut report = json!({"status":"in_progress","launch_mode":launch::MODE,"platform":host(root)?,"binary_sha256":hash(bin)?,"lockfile_sha256":hash(&root.join("Cargo.lock"))?,"method":"App-cold editor launches with an isolated evidence catalog; filesystem cache not purged. On macOS, launch timing includes a temporary background bundle and binary copy and the window is created invisible, so this is neither foreground activation timing nor the cost of compositing a visible window. open_to_raster_ms spans import, refresh and render. Frame observation upper bound includes polling/readback, not scanout. RSS sampled about every 50 ms; GPU memory not separated.","focus_checks":[],"runs":[]});
    let checked = (|| -> Result {
        for name in ["empty", "24mp", "60mp", "repeated60mp"] {
            for index in 0..if name == "repeated60mp" { 1 } else { samples } {
                let evidence = out.join(format!("{name}-{index:02}"));
                let mut args = vec!["--evidence-dir".into(), evidence.clone().into_os_string()];
                let source = root.join("fixtures/generated").join(if name == "24mp" {
                    "24mp.jpg"
                } else {
                    "60mp.jpg"
                });
                let before = if name == "empty" {
                    String::new()
                } else {
                    hash(&source)?
                };
                let count = if name == "empty" {
                    0
                } else if name == "repeated60mp" {
                    16
                } else {
                    1
                };
                for _ in 0..count {
                    args.extend(["--open".into(), source.clone().into_os_string()]);
                }
                let start = Instant::now();
                let mut child = smoke::spawn_editor(
                    root,
                    bin,
                    &args,
                    &out.join(format!("{name}-{index:02}.log")),
                )?;
                let mut rss = Vec::new();
                let mut first = None;
                let status = loop {
                    if let Some(s) = child.child.try_wait()? {
                        break s;
                    }
                    ensure(
                        start.elapsed() < Duration::from_secs(35),
                        "Measurement deadline exceeded",
                    )?;
                    if let Ok((_, r)) = usage(root, child.child.id()) {
                        rss.push(json!([start.elapsed().as_secs_f64(), r]));
                    }
                    if first.is_none()
                        && fs::read_to_string(evidence.join("events.jsonl"))
                            .unwrap_or_default()
                            .contains("frame_captured")
                    {
                        first = Some(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                };
                smoke::note_focus(&mut report, &child)?;
                let mut row = json!({"workload":name,"index":index,"exit_code":status.code(),"launch_to_observed_frame_ms":first.unwrap_or(start.elapsed().as_secs_f64()*1000.0),"sampled_peak_rss_mib":rss.iter().filter_map(|r|r[1].as_f64()).fold(0.0,f64::max),"rss_samples":rss});
                report["runs"].as_array_mut().unwrap().push(row.clone());
                write_json(&out.join("measurements.json"), &report)?;
                ensure(status.success(), "Measurement child failed")?;
                let app = read_json(&evidence.join("result.json"))?;
                let events = smoke::events(&evidence.join("events.jsonl"))?;
                ensure(
                    app["status"] == "captured"
                        && app["run_id"].as_str().is_some_and(|s| !s.is_empty()),
                    "Missing capture result",
                )?;
                ensure(
                    events.first().is_some_and(|e| e["event"] == "startup")
                        && events.last().is_some_and(|e| e["event"] == "shutdown")
                        && events.iter().all(|e| e["run_id"] == app["run_id"]),
                    "Invalid measurement lifecycle",
                )?;
                let frames = app["frames"].as_array().ok_or("Missing frames")?;
                ensure(frames.len() == count.max(1), "Wrong frame count")?;
                let mut pixel_checks = Vec::new();
                for (i, frame) in frames.iter().enumerate() {
                    let state = &frame["state"];
                    ensure(
                        state["run_id"] == app["run_id"]
                            && state["requested_generation"] == if count == 0 { 0 } else { i + 1 },
                        "Stale measurement frame",
                    )?;
                    ensure(
                        frame["capture_provenance"] == "window-renderer-readback",
                        "Capture provenance",
                    )?;
                    if count > 0 {
                        ensure(
                            state["phase"] == "ready" && state["displayed_generation"] == i + 1,
                            "Stale displayed measurement",
                        )?;
                        pixel_checks.push(smoke::pixels(
                            &evidence.join(frame["file"].as_str().ok_or("Missing frame")?),
                            &smoke::Expect {
                                aspect: Some(if name == "24mp" { 1.5 } else { 5.0 / 3.0 }),
                                columns: smoke::columns(frame)?,
                                ..smoke::Expect::fit(1)
                            },
                        )?);
                    }
                }
                if count > 0 {
                    ensure(before == hash(&source)?, "Source modified")?;
                } else {
                    smoke::verify(&evidence, "empty", 0)?;
                }
                row["pixel_checks"] = json!(pixel_checks);
                let last = frames.last().unwrap();
                for k in ["physical_size", "scale"] {
                    row[k] = last[k].clone();
                }
                row["backend"] = last["state"]["backend"].clone();
                for (key, event) in [
                    ("open_to_raster_ms", "decoded"),
                    ("upload_ms", "render_ready"),
                    ("request_to_capture_ms", "frame_captured"),
                ] {
                    row[key] = json!(
                        events
                            .iter()
                            .filter(|e| e["event"] == event)
                            .map(|e| e["detail"][key].clone())
                            .collect::<Vec<_>>()
                    );
                }
                *report["runs"].as_array_mut().unwrap().last_mut().unwrap() = row;
                write_json(&out.join("measurements.json"), &report)?;
            }
        }
        let data = out.join("idle-data");
        let mut child = smoke::spawn_editor(
            root,
            bin,
            &[
                "--data-root".into(),
                data.clone().into_os_string(),
                "--open".into(),
                root.join("fixtures/generated/60mp.jpg").into_os_string(),
            ],
            &out.join("idle.log"),
        )?;
        await_log(
            &mut child,
            &data.join("logs/events.jsonl"),
            "\"event\":\"render_ready\"",
            Duration::from_secs(10),
        )?;
        std::thread::sleep(Duration::from_secs(1));
        let before = usage(root, child.child.id())?;
        let start = Instant::now();
        let mut peak = before.1;
        while start.elapsed() < Duration::from_secs(30) {
            peak = peak.max(usage(root, child.child.id())?.1);
            std::thread::sleep(Duration::from_millis(500));
        }
        let after = usage(root, child.child.id())?;
        let elapsed = start.elapsed().as_secs_f64();
        // The idle process is still running: this is the frontmost application after its whole
        // measured window, which is when a launch that took the desktop would show.
        smoke::note_focus(&mut report, &child)?;
        report["idle"] = json!({"duration_s":elapsed,"cpu_percent_one_core":(after.0-before.0)/elapsed*100.0,"rss_mib_start":before.1,"rss_mib_end":after.1,"rss_mib_peak":peak,"method":"ps CPU delta, 30 seconds after readiness plus one-second settle; child then terminated, not clean-close evidence"});
        let mut summary = json!({});
        for name in ["empty", "24mp", "60mp"] {
            let rows: Vec<_> = report["runs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["workload"] == name)
                .collect();
            let mut values = json!({});
            for key in [
                "launch_to_observed_frame_ms",
                "sampled_peak_rss_mib",
                "open_to_raster_ms",
                "upload_ms",
                "request_to_capture_ms",
            ] {
                let data = rows
                    .iter()
                    .flat_map(|r| {
                        if let Some(a) = r[key].as_array() {
                            a.iter().filter_map(Value::as_f64).collect::<Vec<_>>()
                        } else {
                            r[key].as_f64().into_iter().collect()
                        }
                    })
                    .collect::<Vec<_>>();
                values[key] = stats(&data);
            }
            summary[name] = values;
        }
        report["summary"] = summary;
        launch::focus_verdict(&report)
    })();
    match &checked {
        Ok(()) => report["status"] = json!("passed"),
        Err(e) => {
            report["status"] = json!("failed");
            report["error"] = json!(e.to_string());
        }
    }
    write_json(&out.join("measurements.json"), &report)?;
    checked
}
pub fn probe(root: &Path, out: &Path, candidate: &str) -> Result {
    ensure(
        matches!(candidate, "iced" | "egui"),
        "Candidate must be iced or egui",
    )?;
    ensure(!out.exists(), "Probe output must be new")?;
    fs::create_dir_all(out)?;
    let fixture = root.join("fixtures/s0/orientation-6.jpg");
    let before = hash(&fixture)?;
    let capture = out.join("window.png");
    let bin = root.join(format!(
        "probes/s0/target/release/{candidate}-viewer{}",
        std::env::consts::EXE_SUFFIX
    ));
    let mut result = json!({"candidate":candidate,"status":"failed","launch_mode":launch::MODE,"fixture_sha256":before,"native_dialog_resize_verification":"not-performed"});
    let check = (|| -> Result {
        // A probe is its own binary, not the editor, so it takes none of the editor's arguments.
        let mut child = smoke::spawn(
            root,
            &bin,
            &[
                fixture.clone().into_os_string(),
                capture.clone().into_os_string(),
            ],
            &out.join("process.log"),
        )?;
        let status = smoke::wait(&mut child, Duration::from_secs(30))?;
        result["focus_check"] = child.focus_check();
        ensure(status.success(), "Probe failed")?;
        ensure(before == hash(&fixture)?, "Source changed")?;
        result["pixel_check"] = smoke::pixels(&capture, &smoke::Expect::fit(6))?;
        result["capture_provenance"] = json!("window-renderer-readback");
        launch::focus_verdict(&result)
    })();
    match &check {
        Ok(()) => result["status"] = json!("passed"),
        Err(e) => result["error"] = json!(e.to_string()),
    };
    write_json(&out.join("result.json"), &result)?;
    check
}
