//! `hardening` and `measure`: how the editor fails and recovers, and its app-cold launch figures.
//! Every editor process either starts is made through the scenario library's [`Run`] and
//! [`Launch`], watched through its one [`launch::watch`] loop and listed in the run's
//! `result.json`.
use crate::{
    scenario::{
        Launch, Run,
        launch::{self, Guard, Poll, Watched, Watcher},
    },
    *,
};
use std::time::{Duration, Instant};

/// Hardening's initialization launch: an evidence directory that cannot be created, which the
/// editor refuses with exit code 2.
fn initialization(obstacle: &Path) -> Launch {
    Launch::named("initialization")
        .evidence_dir(&obstacle.join("evidence"))
        .deadline(Duration::from_secs(5))
        .exits(2)
}

/// Hardening's degraded launch: a data root that is a plain file, and a catalog elsewhere.
fn degraded(out: &Path, obstacle: &Path, fixture: &Path) -> Launch {
    Launch::ordinary("diagnostics-unavailable")
        .data_root(obstacle)
        .catalog(&out.join("degraded.sqlite"))
        .open_all(&[fixture.into()])
}

/// An ordinary launch of one photograph with its data under `data`.
fn ordinary(name: &str, data: &Path, source: &Path) -> Launch {
    Launch::ordinary(name)
        .data_root(data)
        .open_all(&[source.into()])
}

/// One measured launch: `source` opened `count` times, its evidence in `<out>/<name>`.
fn measured(name: &str, source: &Path, count: usize) -> Launch {
    Launch::named(name).open_all(&vec![source.to_path_buf(); count])
}

/// Watch `child` until the file at `path` holds `needle`, polling every 10 ms for at most
/// `deadline`, and return the file's text then.
fn await_log(child: &mut Guard, path: &Path, needle: &str, deadline: Duration) -> Result<String> {
    let late = format!("Deadline waiting for {needle}");
    let poll = Poll {
        every: Duration::from_millis(10),
        from: Instant::now(),
        deadline,
        late: &late,
    };
    let read = || fs::read_to_string(path).unwrap_or_default();
    let reached = || {
        let text = read();
        Ok(text.contains(needle).then_some(text))
    };
    match launch::watch(child, poll, reached, |_| Ok(()))? {
        Watched::Reached(text) => Ok(text),
        Watched::Exited(_) => Err(format!("Child exited before {needle}: {}", read()).into()),
    }
}

/// The watcher of an ordinary launch, which never exits on its own: wait within the launch's
/// deadline for `needle` in the file at `path`, then hand the running editor and the file's text to
/// `then`, and let the launch end it.
fn logged(
    path: PathBuf,
    needle: &'static str,
    then: impl FnOnce(&mut Guard, String) -> Result<Value> + 'static,
) -> Watcher {
    Box::new(move |child, _, deadline| {
        let text = await_log(child, &path, needle, deadline)?;
        Ok((None, then(child, text)?))
    })
}

/// The one focus check: with the editor running, the frontmost application is not it. Automated
/// launches run hidden in a background-only bundle, so this holds whatever else the owner is doing
/// on the desktop, and no other run re-measures it. The frontmost pid, or null off macOS.
fn not_frontmost(child: &Guard) -> Result<Value> {
    #[cfg(target_os = "macos")]
    {
        let front = crate::launch::frontmost_pid()?;
        ensure(
            front != child.child.id(),
            format!("The automated launch (pid {front}) is the frontmost application"),
        )?;
        Ok(json!(front))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = child;
        Ok(Value::Null)
    }
}

pub fn hardening(root: &Path, out: &Path, bin: &Path) -> Result {
    let mut run = Run::tool(root, out, "hardening", bin, Duration::from_secs(10))?;
    run.record("checks", json!([]));
    run.check(|run| {
        let fixture = root.join("fixtures/s0/orientation-6.jpg");
        let before = hash(&fixture)?;
        {
            // Not the editor: xtask's own hanging child, so it carries no editor arguments.
            let mut child = launch::spawn(
                root,
                &std::env::current_exe()?,
                &["__hang".into()],
                &out.join("hung.log"),
            )?;
            ensure(
                launch::wait(&mut child, Duration::from_millis(200)).is_err(),
                "Hung child accepted",
            )?;
        }
        let obstacle = out.join("not-a-directory");
        fs::write(&obstacle, "preserve")?;
        run.launch(initialization(&obstacle))?;
        ensure(
            fs::read_to_string(out.join("initialization.log"))?
                .contains("Cannot create evidence directory"),
            "Missing initialization diagnosis",
        )?;
        ensure(
            fs::read_to_string(&obstacle)? == "preserve",
            "Obstacle modified",
        )?;
        // The data root is a plain file, so its log path cannot be created; the editor must still
        // import and render with an explicit catalog elsewhere, its events on its console.
        let console = out.join("diagnostics-unavailable.log");
        let text = run
            .launch(
                degraded(out, &obstacle, &fixture)
                    .deadline(Duration::from_secs(10))
                    .watch(logged(console, "\"event\":\"decoded\"", |_, text| {
                        Ok(json!(text))
                    })),
            )?
            .watched;
        ensure(
            text.as_str()
                .is_some_and(|text| text.contains("viewing continues")),
            "Missing degraded-diagnostics message",
        )?;
        let isolated = out.join("abrupt");
        let events = isolated.join("logs/events.jsonl");
        let front = run
            .launch(
                ordinary("abrupt", &isolated, &fixture)
                    .deadline(Duration::from_secs(10))
                    .watch(logged(
                        events.clone(),
                        "\"event\":\"startup\"",
                        |child, _| not_frontmost(child),
                    )),
            )?
            .watched;
        if !front.is_null() {
            run.record("frontmost_pid_while_running", front);
        }
        // An abrupt kill may leave an incomplete final line; the first complete startup must survive.
        let text = fs::read_to_string(events)?;
        let first: Value = serde_json::from_str(text.lines().next().ok_or("Missing startup")?)?;
        ensure(first["event"] == "startup", "Wrong retained event")?;
        run.provenance(std::slice::from_ref(&first))?;
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
        run.record(
            "checks",
            json!([
                "Actual hung child killed and reaped",
                "Evidence initialization fails without changing existing files",
                "Normal decode survives unavailable diagnostics; child then terminated",
                "Abrupt termination retains startup; only the catalog under config, no cache or source mutation",
                "A running automated launch is never the frontmost application (macOS)"
            ]),
        );
        Ok(())
    })
}
/// One measurement's distribution, in the one shape [`stats::Distribution`] gives every timing
/// tool. Formerly its own interpolated median with `p95 = sorted[n*95/100]` (no ceiling), a
/// different statistic from the nearest-rank percentile the other timing tools already used; see
/// `xtask/src/stats.rs` for the golden vectors this changes.
fn distribution(values: &[f64]) -> Value {
    stats::distribution_json(values.to_vec())
}
/// Watch a measured launch until it exits, from just before its spawn, so its launch time includes
/// the background bundle's copy: its RSS every 50 ms, and the first poll at which its events hold a
/// captured frame.
fn observed(root: &Path, evidence: &Path) -> Watcher {
    let (root, events) = (root.to_path_buf(), evidence.join("events.jsonl"));
    Box::new(move |child, started, deadline| {
        let mut rss = Vec::new();
        let mut first = None;
        let poll = Poll {
            every: Duration::from_millis(50),
            from: started,
            deadline,
            late: "Measurement deadline exceeded",
        };
        let status = launch::until_exit(child, poll, |child| {
            if let Ok((_, r)) = stats::usage(&root, child.child.id()) {
                rss.push(json!([started.elapsed().as_secs_f64() * 1000.0, r]));
            }
            if first.is_none()
                && fs::read_to_string(&events)
                    .unwrap_or_default()
                    .contains("frame_captured")
            {
                first = Some(started.elapsed().as_secs_f64() * 1000.0);
            }
            Ok(())
        })?;
        Ok((
            Some(status),
            json!({"exit_code":status.code(),"launch_to_observed_frame_ms":first.unwrap_or(started.elapsed().as_secs_f64()*1000.0),"sampled_peak_rss_mib":rss.iter().filter_map(|r|r[1].as_f64()).fold(0.0,f64::max),"rss_samples":rss}),
        ))
    })
}
pub fn measure(root: &Path, out: &Path, bin: &Path, samples: usize) -> Result {
    ensure(
        cfg!(target_os = "macos"),
        "Measurement currently supports native macOS ps only",
    )?;
    ensure((1..=1000).contains(&samples), "Samples must be 1..1000")?;
    let mut run = Run::tool(root, out, "measure", bin, Duration::from_secs(35))?;
    let mut report = json!({"status":"in_progress","method":"App-cold editor launches with an isolated evidence catalog; filesystem cache not purged. On macOS, launch timing includes a temporary background bundle and binary copy and the window is created invisible, so this is neither foreground activation timing nor the cost of compositing a visible window. open_to_raster_ms spans import, refresh and render. Frame observation upper bound includes polling/readback, not scanout. RSS sampled about every 50 ms; GPU memory not separated.","runs":[]});
    let checked = (|| -> Result {
        for name in ["empty", "24mp", "60mp", "repeated60mp"] {
            for index in 0..if name == "repeated60mp" { 1 } else { samples } {
                let label = format!("{name}-{index:02}");
                let evidence = out.join(&label);
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
                let watched = run
                    .launch(measured(&label, &source, count).watch(observed(root, &evidence)))?
                    .watched;
                let mut row = json!({"workload":name,"index":index});
                launch::stamp(&mut row, &watched);
                report["runs"].as_array_mut().unwrap().push(row.clone());
                write_json(&out.join("measurements.json"), &report)?;
                let app = read_json(&evidence.join("result.json"))?;
                let events = scenario::events(&evidence.join("events.jsonl"))?;
                if report["profile"].is_null() {
                    launch::stamp(&mut report, &run.provenance(&events)?);
                }
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
                            &scenario::Fixture {
                                aspect: Some(if name == "24mp" { 1.5 } else { 5.0 / 3.0 }),
                                columns,
                                ..scenario::Fixture::fit(1)
                            },
                        )?);
                    }
                }
                if count > 0 {
                    ensure(before == hash(&source)?, "Source modified")?;
                } else {
                    smoke::check_empty(&evidence)?;
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
        let idle_root = root.to_path_buf();
        report["idle"] = run
            .launch(
                ordinary("idle", &data, &root.join("fixtures/generated/60mp.jpg"))
                    .deadline(Duration::from_secs(10))
                    .watch(logged(
                        data.join("logs/events.jsonl"),
                        "\"event\":\"render_ready\"",
                        move |child, _| {
                            Ok(stats::idle_window(&idle_root, child.child.id())?.to_json())
                        },
                    )),
            )?
            .watched;
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
    run.finish(checked, |_| Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every launch `measure` and `hardening` make, as its argument list, into
    /// `$SCRIPT_DUMP/measure/` and `$SCRIPT_DUMP/hardening/`, for runs written to `/out` from a
    /// checkout at `/root`: the proof that a change to how launches are made leaves the arguments
    /// they pass the same.
    #[test]
    #[ignore]
    fn dump_scripts() {
        let dir =
            PathBuf::from(std::env::var("SCRIPT_DUMP").expect("SCRIPT_DUMP names a directory"));
        let (root, out) = (Path::new("/root"), Path::new("/out"));
        let put = |tool: &str, name: &str, launch: Launch| {
            let dir = dir.join(tool);
            fs::create_dir_all(&dir).unwrap();
            write_json(
                &dir.join(format!("{name}-arguments.json")),
                &json!(launch.command(out)),
            )
            .unwrap();
        };
        let (twenty_four, sixty) = (
            root.join("fixtures/generated/24mp.jpg"),
            root.join("fixtures/generated/60mp.jpg"),
        );
        put("measure", "empty-00", measured("empty-00", &sixty, 0));
        put("measure", "24mp-00", measured("24mp-00", &twenty_four, 1));
        put("measure", "60mp-00", measured("60mp-00", &sixty, 1));
        put(
            "measure",
            "repeated60mp-00",
            measured("repeated60mp-00", &sixty, 16),
        );
        put(
            "measure",
            "idle",
            ordinary("idle", &out.join("idle-data"), &sixty),
        );
        let (obstacle, fixture) = (
            out.join("not-a-directory"),
            root.join("fixtures/s0/orientation-6.jpg"),
        );
        put("hardening", "initialization", initialization(&obstacle));
        put(
            "hardening",
            "diagnostics-unavailable",
            degraded(out, &obstacle, &fixture),
        );
        put(
            "hardening",
            "abrupt",
            ordinary("abrupt", &out.join("abrupt"), &fixture),
        );
    }
}
