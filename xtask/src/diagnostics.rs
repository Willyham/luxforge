use crate::*;
use std::time::{Duration, Instant};
fn await_log(
    child: &mut scenario::launch::Guard,
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
    let mut result = json!({"status":"failed","launch_mode":launch::MODE,"binary_sha256":hash(bin)?,"checks":[]});
    let checked = (|| -> Result {
        let fixture = root.join("fixtures/s0/orientation-6.jpg");
        let before = hash(&fixture)?;
        {
            // Not the editor: xtask's own hanging child, so it carries no editor arguments.
            let mut child = scenario::launch::spawn(
                root,
                &std::env::current_exe()?,
                &["__hang".into()],
                &out.join("hung.log"),
            )?;
            ensure(
                scenario::launch::wait(&mut child, Duration::from_millis(200)).is_err(),
                "Hung child accepted",
            )?;
        }
        let obstacle = out.join("not-a-directory");
        fs::write(&obstacle, "preserve")?;
        {
            let mut child = scenario::launch::spawn_editor(
                root,
                bin,
                &[
                    "--evidence-dir".into(),
                    obstacle.join("evidence").into_os_string(),
                ],
                &out.join("initialization.log"),
            )?;
            let status = scenario::launch::wait(&mut child, Duration::from_secs(5))?;
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
            let mut child = scenario::launch::spawn_editor(
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
        }
        let isolated = out.join("abrupt");
        let events = isolated.join("logs/events.jsonl");
        {
            let mut child = scenario::launch::spawn_editor(
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
            // The one focus check: with the editor running, the frontmost application is not it.
            // Automated launches run hidden in a background-only bundle, so this holds whatever
            // else the owner is doing on the desktop, and no other run re-measures it.
            #[cfg(target_os = "macos")]
            {
                let front = launch::frontmost_pid()?;
                ensure(
                    front != child.child.id(),
                    format!("The automated launch (pid {front}) is the frontmost application"),
                )?;
                result["frontmost_pid_while_running"] = json!(front);
            }
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
            "Abrupt termination retains startup; only the catalog under config, no cache or source mutation",
            "A running automated launch is never the frontmost application (macOS)"
        ]);
        Ok(())
    })();
    match &checked {
        Ok(()) => result["status"] = json!("passed"),
        Err(e) => result["error"] = json!(e.to_string()),
    };
    write_json(&out.join("result.json"), &result)?;
    checked
}
/// One measurement's distribution, in the one shape [`stats::Distribution`] gives every timing
/// tool. Formerly its own interpolated median with `p95 = sorted[n*95/100]` (no ceiling), a
/// different statistic from the nearest-rank percentile the other timing tools already used; see
/// `xtask/src/stats.rs` for the golden vectors this changes.
fn distribution(values: &[f64]) -> Value {
    stats::distribution_json(values.to_vec())
}
pub fn measure(root: &Path, out: &Path, bin: &Path, samples: usize) -> Result {
    ensure(
        cfg!(target_os = "macos"),
        "Measurement currently supports native macOS ps only",
    )?;
    ensure((1..=1000).contains(&samples), "Samples must be 1..1000")?;
    ensure(!out.exists(), "Measurement output must be new")?;
    fs::create_dir_all(out)?;
    let mut report = json!({"status":"in_progress","launch_mode":launch::MODE,"platform":host(root)?,"binary_sha256":hash(bin)?,"lockfile_sha256":hash(&root.join("Cargo.lock"))?,"method":"App-cold editor launches with an isolated evidence catalog; filesystem cache not purged. On macOS, launch timing includes a temporary background bundle and binary copy and the window is created invisible, so this is neither foreground activation timing nor the cost of compositing a visible window. open_to_raster_ms spans import, refresh and render. Frame observation upper bound includes polling/readback, not scanout. RSS sampled about every 50 ms; GPU memory not separated.","runs":[]});
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
                let mut child = scenario::launch::spawn_editor(
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
                    if let Ok((_, r)) = stats::usage(root, child.child.id()) {
                        rss.push(json!([start.elapsed().as_secs_f64() * 1000.0, r]));
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
                let mut row = json!({"workload":name,"index":index,"exit_code":status.code(),"launch_to_observed_frame_ms":first.unwrap_or(start.elapsed().as_secs_f64()*1000.0),"sampled_peak_rss_mib":rss.iter().filter_map(|r|r[1].as_f64()).fold(0.0,f64::max),"rss_samples":rss});
                report["runs"].as_array_mut().unwrap().push(row.clone());
                write_json(&out.join("measurements.json"), &report)?;
                ensure(status.success(), "Measurement child failed")?;
                let app = read_json(&evidence.join("result.json"))?;
                let events = scenario::events(&evidence.join("events.jsonl"))?;
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
                        let columns = scenario::columns(frame)?;
                        pixel_checks.push(scenario::pixels::fixture(
                            &image::open(
                                evidence.join(frame["file"].as_str().ok_or("Missing frame")?),
                            )?
                            .to_rgb8(),
                            &scenario::Expect {
                                aspect: Some(if name == "24mp" { 1.5 } else { 5.0 / 3.0 }),
                                columns,
                                ..scenario::Expect::fit(1)
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
                // No upload figure: the photograph is drawn by a surface that writes its own
                // texture during the frame that draws it, so `render_ready` times no upload step.
                for (key, event) in [
                    ("open_to_raster_ms", "decoded"),
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
        let mut child = scenario::launch::spawn_editor(
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
        let window = stats::idle_window(root, child.child.id())?;
        report["idle"] = window.to_json();
        report["idle"]["method"] = json!(
            "ps CPU delta, 30 seconds after readiness plus one-second settle; child then terminated, not clean-close evidence"
        );
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
                values[key] = distribution(&data);
            }
            summary[name] = values;
        }
        report["summary"] = summary;
        Ok(())
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
