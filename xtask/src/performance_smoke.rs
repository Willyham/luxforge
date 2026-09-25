//! The `performance` smoke scenario: the state panel's Performance section on the real editor, over
//! the generated 60 MP JPEG at 1440 × 900.
//!
//! Its frames, in [`plan`] order: the photograph opened with the section open, as every launch
//! starts it, and sampling; a 3.6 s wait, by which the one-second sampler has read at least four
//! times; a 3° straighten; a Presence Clarity commit over it, whose exact render at 60 MP runs long
//! enough to be listed as long work; a wait after which that render is listed as finished; the
//! section collapsed; a 2.5 s wait in which nothing more is read; and the section opened again,
//! captured on its first read of a fresh window.
//!
//! Each frame is checked against its own recorded answers, re-derived here without the editor's
//! code: the memory figure against the recorded `resources.read`, the CPU and GPU figures against a
//! rate computed from the last two recorded reads, the series lengths against the sample count, and
//! the job rows and heading caption against the recorded `activity.list` under the section's display
//! rules. The runner also reads the editor's memory from outside while it runs — `ps` for resident
//! memory and `footprint` for the physical footprint Activity Monitor shows — and compares each
//! frame's recorded figures with its own readings taken at the same wall-clock moment. The collapsed
//! frames prove the section asleep: the reads asked for and the samples held stay exactly where
//! they were, and the reopened frame proves that opening clears the window and reads at once.
//! Everything compared is written to `app/performance-checks.json`.
//!
//! With `--source RAW` the heavy step is a RAW temperature commit instead, which redevelops the
//! mosaic; that run is not part of `rendered`, because no RAW photograph is checked in.
use crate::{
    scenario::{Checked, Plan, Run, Step, launch::Guard, plan::only},
    *,
};
use lightwell_evidence::{self as script};
use std::{
    process::ExitStatus,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const SCENARIO: &str = "performance";
pub const FIXTURE: &str = "fixtures/generated/60mp.jpg";
/// Where the runner's own readings of the editor's memory are written, beside `app/`.
pub const READINGS: &str = "process-readings.json";
/// Long enough for three more ticks of the one-second timer after the photograph opens.
const FILL_WAIT_MS: u64 = 3_600;
/// Long enough for the heavy render's exact phase to end and a read to see it in `recent`.
const FINISHED_WAIT_MS: u64 = 2_500;
/// Two and a half sampler intervals, in which a collapsed section must read nothing.
const ASLEEP_WAIT_MS: u64 = 2_500;
/// The straighten under the heavy edit. Clarity alone renders its exact phase in about 0.6 s on
/// the owner's M4, too close to the section's 0.5 s threshold to be listed reliably; over a 3°
/// straighten, whose interpolation it then renders through, it takes about 0.9 s.
const ANGLE: f64 = 3.0;
/// The heavy edit on the JPEG: full Presence Clarity, a spatial operation over the whole 60 MP
/// frame at its exact phase. Texture or Dehaze beside it would run for seconds more, which is more
/// than the scenario needs.
const CLARITY: f64 = 100.0;
/// The heavy edit on a RAW source: a temperature commit, which redevelops the retained mosaic.
const KELVIN: f64 = 4200.0;
/// The section's own display rules, restated here so the runner does not borrow them.
const LONG_JOB_MS: u64 = 500;
const RECENT_JOB_MS: u64 = 10_000;
const MAX_JOB_ROWS: usize = 4;
const WINDOW: u64 = 60;
const DASH: &str = "\u{2013}";
/// How often the runner reads the editor's resident memory with `ps` (about 4 ms a read), and how
/// many of those polls pass between two `footprint` reads (about 40 ms a read).
const POLL: Duration = Duration::from_millis(100);
const FOOTPRINT_EVERY: usize = 2;
/// The furthest the runner's readings either side of a sample may be from it: a few polls, so a
/// missed poll under load does not fail the run, while a bracket still spans well under a second.
const BRACKET_MS: u64 = 1_000;
/// How far outside the runner's two bracketing readings a recorded figure may lie. `ps`, `footprint`
/// and `resources.read` all read the same kernel counters, so while the editor idles they agree to
/// the byte; while it works, a figure read between two of the runner's readings lies between them
/// unless the process allocated and freed again inside that interval. The slack, 512 pages of
/// 16 KiB, covers small movements of that kind; `performance-checks.json` records the differences.
const MEMORY_SLACK: u64 = 8 << 20;

/// A RAW source is anything the editor opens that is not a JPEG.
fn is_raw(sources: &[PathBuf]) -> bool {
    sources.first().is_some_and(|source| {
        !source
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
            })
    })
}

/// The heavy step for this source: Presence Clarity on the JPEG, a temperature commit on a RAW.
fn heavy_step(raw: bool) -> script::Step {
    if raw {
        script::Step::call("edit.set-raw-temperature", json!({"kelvin":KELVIN}))
    } else {
        script::Step::call("edit.set-presence", json!({"clarity":CLARITY}))
    }
}

/// Every frame, in order, over the source the run opens. What each step commits is planned here:
/// nothing but the two edits commits anything. What the section shows, `verify` checks.
pub fn plan(sources: &[PathBuf]) -> Plan {
    let raw = is_raw(sources);
    let heavy = Step::new("heavy", heavy_step(raw)).commits(1);
    // The JPEG's Clarity commit is labelled by the module; a RAW temperature's label is the
    // photograph's own, so it is left to the commit count.
    let heavy = if raw {
        heavy
    } else {
        heavy.label(format!("Clarity +{CLARITY}"))
    };
    Plan::new(vec![
        // The photograph opened with the section open and sampling, as every launch starts it.
        Step::opened("opened"),
        // The window fills; the section has sampled since the photograph opened.
        Step::new("filled", script::Step::wait(FILL_WAIT_MS)).commits(0),
        // The straighten, then the heavy edit over it, each one entry, captured on its exact frame.
        Step::new(
            "straightened",
            script::Step::call("edit.crop-fit", json!({"aspect":"16:9","angle":ANGLE})),
        )
        .commits(1)
        .label("Crop 16:9"),
        heavy,
        // Its render listed as finished.
        Step::new("finished", script::Step::wait(FINISHED_WAIT_MS)).commits(0),
        // Collapsed, then asleep.
        Step::new("collapsed", script::Step::performance(false)).commits(0),
        Step::new("asleep", script::Step::wait(ASLEEP_WAIT_MS)).commits(0),
        // Opened again, captured on the first read of a fresh window.
        Step::new("reopened", script::Step::performance(true)).commits(0),
    ])
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

/// The editor's resident memory as `ps` reports it, in bytes.
fn ps_resident(pid: u32) -> Option<u64> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let kib: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some(kib * 1024)
}

/// The editor's physical footprint as macOS's `footprint` tool reports it — the kernel ledger
/// Activity Monitor's Memory column shows — or why it could not be read.
fn footprint(pid: u32) -> std::result::Result<u64, String> {
    let out = Command::new("footprint")
        .args(["-f", "bytes", "--noCategories", "-p", &pid.to_string()])
        .output()
        .map_err(|error| format!("footprint could not run: {error}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("phys_footprint:")
                .and_then(|rest| rest.trim().strip_suffix('B'))
                .and_then(|bytes| bytes.trim().parse().ok())
        })
        .ok_or_else(|| {
            format!(
                "footprint printed no phys_footprint (exit {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            )
        })
}

/// Wait for the editor to exit as [`crate::scenario::launch::wait`] does, reading its memory from
/// outside the process meanwhile: `ps` every [`POLL`] and `footprint` every [`FOOTPRINT_EVERY`]
/// polls, each stamped with the wall-clock middle of the read. `footprint` needs no privileges for a process
/// of the same user; if it fails once it is not tried again and the reason is recorded.
pub fn watch(child: &mut Guard, timeout: Duration) -> Result<(ExitStatus, Value)> {
    let pid = child.child.id();
    let start = Instant::now();
    let mut readings = Vec::new();
    let mut footprint_error: Option<String> = None;
    let mut polls = 0usize;
    let status = loop {
        if let Some(status) = child.child.try_wait()? {
            break status;
        }
        ensure(
            start.elapsed() < timeout,
            "Child timed out; killed and reaped",
        )?;
        let before = wall_ms();
        if let Some(resident) = ps_resident(pid) {
            let after = wall_ms();
            readings.push(json!({"tool":"ps","wall_ms":(before + after) / 2,"span_ms":after - before,"resident_bytes":resident}));
        }
        if footprint_error.is_none() && polls.is_multiple_of(FOOTPRINT_EVERY) {
            let before = wall_ms();
            match footprint(pid) {
                Ok(bytes) => {
                    let after = wall_ms();
                    readings.push(json!({"tool":"footprint","wall_ms":(before + after) / 2,"span_ms":after - before,"footprint_bytes":bytes}));
                }
                // A process that has just exited is not a reason to stop trying.
                Err(error) if child.child.try_wait()?.is_none() => footprint_error = Some(error),
                Err(_) => {}
            }
        }
        polls += 1;
        std::thread::sleep(POLL);
    };
    Ok((
        status,
        json!({"pid":pid,"footprint_error":footprint_error,"readings":readings}),
    ))
}

// -- Independent re-derivations of what the section shows. --

/// Bytes in Activity Monitor's units: whole MB below one GiB, two decimals below ten, then one,
/// with the unit chosen from the rounded figure and every rounding half up.
fn bytes_text(bytes: u64) -> (String, &'static str) {
    let bytes = u128::from(bytes);
    let rounded = |scale: u128, per: u128| (bytes * scale * 2 + per) / (per * 2);
    let megabytes = rounded(1, 1 << 20);
    if megabytes < 1024 {
        (megabytes.to_string(), "MB")
    } else if rounded(100, 1 << 30) < 1000 {
        let hundredths = rounded(100, 1 << 30);
        (
            format!("{}.{:02}", hundredths / 100, hundredths % 100),
            "GB",
        )
    } else {
        let tenths = rounded(10, 1 << 30);
        (format!("{}.{}", tenths / 10, tenths % 10), "GB")
    }
}

/// A percentage: one decimal while it rounds below ten, whole numbers from there.
fn percent_text(percent: f64) -> String {
    let percent = percent.max(0.0);
    if (percent * 10.0).round() < 100.0 {
        let tenths = (percent * 10.0).round() as u64;
        format!("{}.{}", tenths / 10, tenths % 10)
    } else {
        (percent.round() as u64).to_string()
    }
}

/// A duration: tenths of a second while it rounds below ten seconds, whole seconds below a minute,
/// then minutes and seconds.
fn elapsed_text(ms: u64) -> String {
    if (ms + 50) / 100 < 100 {
        let tenths = (ms + 50) / 100;
        format!("{}.{} s", tenths / 10, tenths % 10)
    } else if (ms + 500) / 1000 < 60 {
        format!("{} s", (ms + 500) / 1000)
    } else {
        let seconds = (ms + 500) / 1000;
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

/// The rate of a cumulative nanosecond counter between two reads, in percent of one unit.
fn rate(previous: &Value, latest: &Value, pointer: &str) -> Option<f64> {
    let elapsed = latest["monotonic_ns"]
        .as_u64()?
        .checked_sub(previous["monotonic_ns"].as_u64()?)
        .filter(|elapsed| *elapsed > 0)?;
    let spent = latest
        .pointer(pointer)?
        .as_u64()?
        .saturating_sub(previous.pointer(pointer)?.as_u64()?);
    Some(100.0 * spent as f64 / elapsed as f64)
}

/// A figure matches a rate when it is that rate's text. A rate within a nanopercent of a rounding
/// boundary may round either way depending on the order of two floating-point operations, so both
/// neighbours are accepted there.
fn shows_rate(text: &str, rate: f64) -> bool {
    [rate, rate - 1e-9, rate + 1e-9]
        .iter()
        .any(|candidate| percent_text(*candidate) == text)
}

/// The job rows, the `+N more` caption and the heading caption the display rules give for one
/// recorded `activity.list`.
fn expected_jobs(activity: &Value) -> Result<(Vec<Value>, Value, Value)> {
    let active = activity["active"]
        .as_array()
        .ok_or("activity.list has no active list")?;
    let long: Vec<&Value> = active
        .iter()
        .filter(|job| {
            job["elapsed_ms"]
                .as_u64()
                .is_some_and(|ms| ms >= LONG_JOB_MS)
        })
        .collect();
    if !long.is_empty() {
        let rows = long
            .iter()
            .take(MAX_JOB_ROWS)
            .map(|job| {
                let mut parts = Vec::new();
                if let Some(detail) = job["detail"].as_str() {
                    parts.push(detail.to_owned());
                }
                if let Some(phase) = job["phase"].as_str() {
                    parts.push(format!("{phase} phase"));
                }
                let progress = match (job["progress"]["done"].as_u64(), job["progress"]["total"].as_u64()) {
                    (Some(done), Some(total)) if total > 0 => json!(done as f64 / total as f64),
                    _ => Value::Null,
                };
                json!({
                    "label": job["label"],
                    "trailing": elapsed_text(job["elapsed_ms"].as_u64().unwrap_or_default()),
                    "detail": if parts.is_empty() { Value::Null } else { json!(parts.join(" \u{b7} ")) },
                    "running": true,
                    "progress": progress,
                })
            })
            .collect();
        let more = if long.len() > MAX_JOB_ROWS {
            json!(format!("+{} more", long.len() - MAX_JOB_ROWS))
        } else {
            Value::Null
        };
        let caption = if long.len() == 1 {
            json!("1 job")
        } else {
            json!(format!("{} jobs", long.len()))
        };
        return Ok((rows, more, caption));
    }
    let recent = activity["recent"]
        .as_array()
        .ok_or("activity.list has no recent list")?;
    let finished = recent.iter().find(|job| {
        job["duration_ms"]
            .as_u64()
            .is_some_and(|ms| ms >= LONG_JOB_MS)
            && job["ended_ms_ago"]
                .as_u64()
                .is_some_and(|ms| ms <= RECENT_JOB_MS)
    });
    let row = match finished {
        Some(job) => {
            let how = match job["outcome"].as_str() {
                Some("cancelled") => "Cancelled",
                Some("failed") => "Failed",
                _ => "Finished",
            };
            let ago = ((job["ended_ms_ago"].as_u64().unwrap_or_default() + 500) / 1000).max(1);
            json!({
                "label": job["label"],
                "trailing": elapsed_text(job["duration_ms"].as_u64().unwrap_or_default()),
                "detail": format!("{how} {ago} s ago"),
                "running": false,
                "progress": Value::Null,
            })
        }
        None => json!({
            "label": "No background work",
            "trailing": "",
            "detail": Value::Null,
            "running": false,
            "progress": Value::Null,
        }),
    };
    Ok((vec![row], Value::Null, Value::Null))
}

fn performance(frame: &Value) -> &Value {
    &frame["state"]["performance"]
}

fn count(frame: &Value, key: &str) -> Result<u64> {
    performance(frame)[key]
        .as_u64()
        .ok_or_else(|| format!("Frame records no performance {key}").into())
}

/// A frame whose section is collapsed: the heading alone, no caption, nothing sampling.
fn expect_collapsed(step: &str, frame: &Value) -> Result {
    let section = performance(frame);
    ensure(
        section["expanded"] == json!(false)
            && section["sampling"] == json!(false)
            && section["caption"].is_null()
            && section["rows"].as_array().is_some_and(Vec::is_empty)
            && section["jobs"].as_array().is_some_and(Vec::is_empty),
        format!("Step {step:?}: the section is not collapsed to its heading: {section}"),
    )
}

/// Check one expanded frame's rows against its own recorded answers, and return what was compared.
fn expect_expanded(step: &str, frame: &Value) -> Result<Value> {
    let section = performance(frame);
    ensure(
        section["expanded"] == json!(true) && section["sampling"] == json!(true),
        format!("Step {step:?}: the section is not expanded and sampling: {section}"),
    )?;
    ensure(
        section["error"].is_null(),
        format!("Step {step:?}: the last read failed: {}", section["error"]),
    )?;
    let samples = count(frame, "samples")?;
    ensure(
        samples >= 1,
        format!("Step {step:?}: no sample was adopted"),
    )?;
    ensure(
        count(frame, "reads_requested")? >= samples,
        format!("Step {step:?}: more samples than reads requested"),
    )?;
    let latest = &section["resources"];
    let rows = section["rows"]
        .as_array()
        .ok_or_else(|| format!("Step {step:?}: no rows"))?;
    ensure(
        rows.iter()
            .map(|row| row["label"].clone())
            .collect::<Vec<_>>()
            == [json!("Memory"), json!("CPU"), json!("GPU")],
        format!("Step {step:?}: the rows are not Memory, CPU and GPU: {rows:?}"),
    )?;

    // Memory: the figure is the recorded footprint in Activity Monitor's units.
    let bytes = latest["memory"]["bytes"]
        .as_u64()
        .ok_or_else(|| format!("Step {step:?}: the read has no memory figure"))?;
    let (value, unit) = bytes_text(bytes);
    ensure(
        rows[0]["value"] == json!(value)
            && rows[0]["unit"] == json!(unit)
            && rows[0]["available"] == json!(true),
        format!(
            "Step {step:?}: memory shows {} {} for {bytes} bytes, expected {value} {unit}",
            rows[0]["value"], rows[0]["unit"]
        ),
    )?;
    ensure(
        rows[0]["series_len"] == json!(samples.min(WINDOW)),
        format!(
            "Step {step:?}: memory series {} for {samples} samples",
            rows[0]["series_len"]
        ),
    )?;

    // The platform: on the owner's Mac the footprint, GPU time and unified GPU allocations are all
    // reported, and the tooltips say so.
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ensure(
            latest["memory"]["kind"] == json!("footprint")
                && latest["gpu"]["time_ns"].is_u64()
                && latest["gpu"]["allocated_bytes"].is_u64()
                && latest["gpu"]["unified_memory"] == json!(true),
            format!(
                "Step {step:?}: the M4 read lacks footprint, GPU time or GPU allocations: {latest}"
            ),
        )?;
        ensure(
            rows[0]["tooltip"].as_str().is_some_and(|tooltip| {
                tooltip.starts_with("Memory footprint, as Activity Monitor's Memory column")
                    && tooltip.contains("of GPU allocations")
            }),
            format!("Step {step:?}: memory tooltip {}", rows[0]["tooltip"]),
        )?;
    }
    let cores = latest["cpu"]["logical_cpus"].as_u64().unwrap_or_default();
    ensure(
        rows[1]["tooltip"]
            .as_str()
            .is_some_and(|tooltip| tooltip.contains(&format!("{cores} cores: {}%", cores * 100))),
        format!(
            "Step {step:?}: CPU tooltip {} for {cores} cores",
            rows[1]["tooltip"]
        ),
    )?;

    // CPU and GPU: a dash until a second sample, then the rate between the last two reads.
    let mut rates = json!({});
    for (row, pointer, name) in [
        (&rows[1], "/cpu/time_ns", "cpu"),
        (&rows[2], "/gpu/time_ns", "gpu"),
    ] {
        if samples < 2 {
            ensure(
                row["value"] == json!(DASH)
                    && row["unit"] == json!("")
                    && row["series_len"] == json!(0),
                format!(
                    "Step {step:?}: {name} shows {} before a second sample",
                    row["value"]
                ),
            )?;
            continue;
        }
        let previous = &section["previous_resources"];
        let computed = rate(previous, latest, pointer)
            .ok_or_else(|| format!("Step {step:?}: no {name} rate from the recorded reads"))?;
        let text = row["value"].as_str().unwrap_or_default();
        ensure(
            shows_rate(text, computed) && row["unit"] == json!("%"),
            format!(
                "Step {step:?}: {name} shows {text} {}, the recorded counters give {computed}",
                row["unit"]
            ),
        )?;
        ensure(
            row["series_len"] == json!((samples - 1).min(WINDOW)),
            format!(
                "Step {step:?}: {name} series {} for {samples} samples",
                row["series_len"]
            ),
        )?;
        rates[name] = json!({"shown":text,"computed":computed});
    }

    // Jobs: exactly what the display rules give for the recorded board, and a single job line
    // without a detail keeps that line's room, so one job line is always the same height.
    let (rows_expected, more, caption) = expected_jobs(&section["activity"])?;
    ensure(
        section["jobs"] == json!(rows_expected)
            && section["more"] == more
            && section["caption"] == caption,
        format!(
            "Step {step:?}: jobs {} more {} caption {}; the recorded board gives {} more {more} caption {caption}",
            section["jobs"],
            section["more"],
            section["caption"],
            json!(rows_expected)
        ),
    )?;
    let reserve = rows_expected.len() == 1 && rows_expected[0]["detail"].is_null();
    ensure(
        section["reserve_detail"] == json!(reserve),
        format!(
            "Step {step:?}: reserve_detail is {}, expected {reserve}",
            section["reserve_detail"]
        ),
    )?;
    Ok(json!({
        "samples": samples,
        "memory": {"bytes": bytes, "shown": format!("{value} {unit}")},
        "rates": rates,
        "jobs": section["jobs"],
        "caption": section["caption"],
    }))
}

/// Compare one frame's recorded figure with the runner's own readings of the same counter: the last
/// one taken at or before the moment the editor read it and the first one at or after. The figure
/// must lie between the two, widened by [`MEMORY_SLACK`], and both must be within
/// [`BRACKET_MS`] of that moment.
fn compare_memory(
    step: &str,
    what: &str,
    recorded: u64,
    at: u64,
    readings: &[Value],
    tool: &str,
    key: &str,
) -> Result<Value> {
    let of_tool: Vec<(u64, u64)> = readings
        .iter()
        .filter(|reading| reading["tool"] == tool)
        .filter_map(|reading| Some((reading["wall_ms"].as_u64()?, reading[key].as_u64()?)))
        .collect();
    let before = of_tool.iter().rev().find(|(ms, _)| *ms <= at).copied();
    let after = of_tool.iter().find(|(ms, _)| *ms >= at).copied();
    let (Some(before), Some(after)) = (before, after) else {
        return Err(format!(
            "Step {step:?}: the runner has no {tool} reading on both sides of the sample"
        )
        .into());
    };
    ensure(
        at - before.0 <= BRACKET_MS && after.0 - at <= BRACKET_MS,
        format!(
            "Step {step:?}: the runner's {tool} readings are {} ms before and {} ms after the sample",
            at - before.0,
            after.0 - at
        ),
    )?;
    let low = before.1.min(after.1).saturating_sub(MEMORY_SLACK);
    let high = before.1.max(after.1) + MEMORY_SLACK;
    ensure(
        (low..=high).contains(&recorded),
        format!(
            "Step {step:?}: recorded {what} {recorded} is outside the runner's {tool} readings {}..={} around it, even with {MEMORY_SLACK} bytes of slack",
            before.1, after.1
        ),
    )?;
    let nearest = if at - before.0 <= after.0 - at {
        before
    } else {
        after
    };
    Ok(json!({
        "recorded": recorded,
        "before": {"ms_before": at - before.0, "bytes": before.1},
        "after": {"ms_after": after.0 - at, "bytes": after.1},
        "difference_to_nearest": recorded as i64 - nearest.1 as i64,
        "equals_nearest": recorded == nearest.1,
    }))
}

/// What the section shows at each step, against its own recorded answers and the runner's readings,
/// once the plan has held.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let evidence = &launch.evidence;
    ensure(
        !launch
            .events
            .iter()
            .any(|event| event["event"] == "performance_read_failed"),
        "A sampler read failed",
    )?;
    let watched = read_json(
        &evidence
            .parent()
            .ok_or("The evidence directory has no parent")?
            .join(READINGS),
    )?;
    let readings = watched["readings"].as_array().cloned().unwrap_or_default();
    let pid = watched["pid"]
        .as_u64()
        .ok_or("The runner recorded no pid")?;
    let mut checks = Vec::new();

    for (step, frame) in launch.names().iter().zip(&launch.frames) {
        ensure(
            frame["state"]["workspace"]["state_panel"] == json!(true),
            format!("Step {step:?}: the state panel is hidden"),
        )?;
        ensure(
            performance(frame)["pid"].as_u64() == Some(pid),
            format!("Step {step:?}: the frame's pid is not the process the runner watched"),
        )?;
    }

    // Opened: the section open and sampling from the launch, its figures from its own answers. Its
    // first reads may land before the runner's first reading of the new process, so its memory is
    // compared from the next frame on.
    let opened = launch.at("opened")?;
    let compared = expect_expanded("opened", opened)?;
    checks.push(json!({"frame":opened["file"],"shows":"the photograph opened with the Performance section open and sampling","compared":compared}));

    // Filled through finished, and reopened: each frame against its own answers, and, except the
    // reopened frame, against the runner's readings. The reopened frame is captured on its first
    // read, so that read coincides with the capture's own readback of the whole window (about
    // 20 MB at 2880 × 1800, allocated and freed between two of the runner's polls), which is the
    // harness's memory, not the section's.
    let mut gpu_times = Vec::new();
    let reopened = launch.index("reopened")?;
    for index in (launch.index("filled")?..=launch.index("finished")?).chain([reopened]) {
        let (step, frame) = (launch.names()[index].as_str(), &launch.frames[index]);
        let mut compared = expect_expanded(step, frame)?;
        if index == reopened {
            if let Some(time) = performance(frame)["resources"]["gpu"]["time_ns"].as_u64() {
                gpu_times.push(time);
            }
            checks.push(json!({"frame":frame["file"],"compared":compared}));
            continue;
        }
        let section = performance(frame);
        let at = section["wall_ms"]
            .as_u64()
            .ok_or_else(|| format!("Step {step:?}: no wall-clock time for the sample"))?;
        let resident = section["resources"]["memory"]["resident_bytes"]
            .as_u64()
            .ok_or_else(|| format!("Step {step:?}: no resident memory"))?;
        compared["resident"] = compare_memory(
            step,
            "resident memory",
            resident,
            at,
            &readings,
            "ps",
            "resident_bytes",
        )?;
        if watched["footprint_error"].is_null() {
            let bytes = section["resources"]["memory"]["bytes"]
                .as_u64()
                .unwrap_or_default();
            compared["footprint"] = compare_memory(
                step,
                "footprint",
                bytes,
                at,
                &readings,
                "footprint",
                "footprint_bytes",
            )?;
        }
        if let Some(time) = section["resources"]["gpu"]["time_ns"].as_u64() {
            gpu_times.push(time);
        }
        checks.push(json!({"frame":frame["file"],"compared":compared}));
    }
    let filled = count(launch.at("filled")?, "samples")?;
    ensure(
        filled >= 4,
        format!("Step \"filled\": {filled} samples after {FILL_WAIT_MS} ms"),
    )?;
    ensure(
        gpu_times.windows(2).all(|pair| pair[0] <= pair[1]),
        format!("GPU time decreased across frames: {gpu_times:?}"),
    )?;

    // Finished: the heavy edit's long work is listed as finished — its render for Clarity, and
    // whichever of the redevelopment and its render ended last for a RAW temperature.
    let (heavy, finished) = (launch.at("heavy")?, launch.at("finished")?);
    let jobs = performance(finished)["jobs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let raw = heavy["step"]["request"]["api"]["method"] == json!("edit.set-raw-temperature");
    ensure(
        jobs.len() == 1
            && jobs[0]["running"] == json!(false)
            && jobs[0]["detail"]
                .as_str()
                .is_some_and(|detail| detail.starts_with("Finished "))
            && (raw || jobs[0]["label"] == json!("Rendering preview")),
        format!("Step \"finished\": the heavy edit's work is not listed as finished: {jobs:?}"),
    )?;
    let listed = performance(finished)["activity"]["recent"]
        .as_array()
        .and_then(|recent| {
            recent.iter().find(|job| {
                job["duration_ms"]
                    .as_u64()
                    .is_some_and(|ms| ms >= LONG_JOB_MS)
                    && job["ended_ms_ago"]
                        .as_u64()
                        .is_some_and(|ms| ms <= RECENT_JOB_MS)
            })
        })
        .cloned()
        .unwrap_or(Value::Null);

    // Collapsed, then asleep: nothing more is read or adopted.
    let (collapsed, asleep) = (launch.at("collapsed")?, launch.at("asleep")?);
    expect_collapsed("collapsed", collapsed)?;
    expect_collapsed("asleep", asleep)?;
    let asked = count(collapsed, "reads_requested")?;
    let held = count(collapsed, "samples")?;
    ensure(
        count(asleep, "reads_requested")? == asked && count(asleep, "samples")? == held,
        format!(
            "Step \"asleep\": the collapsed section read again: {} reads and {} samples, against {asked} and {held}",
            count(asleep, "reads_requested")?,
            count(asleep, "samples")?
        ),
    )?;
    ensure(
        performance(asleep)["in_flight"] == json!(false),
        "Step \"asleep\": a read is still in flight while collapsed",
    )?;
    checks.push(json!({"frames":[collapsed["file"],asleep["file"]],"shows":"collapsed, then asleep","reads_requested":asked,"samples":held,"asleep_ms":ASLEEP_WAIT_MS}));

    // Reopened: a fresh window, read at once. The frame is captured on that first read, so it holds
    // exactly one sample and one read more than the collapsed section had asked for.
    let reopened = launch.at("reopened")?;
    ensure(
        count(reopened, "samples")? == 1 && count(reopened, "reads_requested")? == asked + 1,
        format!(
            "Step \"reopened\": reopening did not start a fresh window with one read: {} samples and {} reads, after {asked}",
            count(reopened, "samples")?,
            count(reopened, "reads_requested")?
        ),
    )?;
    checks.push(json!({"frame":reopened["file"],"shows":"opened again: a fresh window, read at once","reads_requested":asked + 1,"samples":1}));

    let ps_count = readings
        .iter()
        .filter(|reading| reading["tool"] == "ps")
        .count();
    let footprint_count = readings
        .iter()
        .filter(|reading| reading["tool"] == "footprint")
        .count();
    write_json(
        &evidence.join("performance-checks.json"),
        &json!({
            "checks": checks,
            "edits": [launch.at("straightened")?["step"]["request"], heavy["step"]["request"]],
            "heavy_work_listed": listed,
            "gpu_time_ns": gpu_times,
            "runner": {
                "pid": pid,
                "ps_readings": ps_count,
                "footprint_readings": footprint_count,
                "footprint_error": watched["footprint_error"],
                "poll_ms": POLL.as_millis() as u64,
                "footprint_every_polls": FOOTPRINT_EVERY,
            },
            "tolerance": {
                "bracket_ms": BRACKET_MS,
                "memory_slack_bytes": MEMORY_SLACK,
                "rule": "the recorded figure lies between the runner's last reading of the same counter at or before the sample's wall-clock time and its first at or after, widened by the slack; both readings within the bracket distance of the sample",
            },
            "scope": "Displayed text against the frame's own recorded resources.read and activity.list, re-derived without the editor's code; resident memory and footprint against ps and footprint run by the runner on the same pid. A consistency check of the section against the process, not a measurement of the sampler's cost.",
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runners_formatters_follow_the_design() {
        assert_eq!(bytes_text(812 << 20), ("812".to_owned(), "MB"));
        assert_eq!(bytes_text((1 << 30) - 1), ("1.00".to_owned(), "GB"));
        assert_eq!(bytes_text((1 << 30) * 142 / 100), ("1.42".to_owned(), "GB"));
        assert_eq!(bytes_text((1 << 30) * 124 / 10), ("12.4".to_owned(), "GB"));
        assert_eq!(percent_text(3.24), "3.2");
        assert_eq!(percent_text(9.96), "10");
        assert_eq!(percent_text(419.6), "420");
        assert_eq!(elapsed_text(760), "0.8 s");
        assert_eq!(elapsed_text(12_400), "12 s");
        assert_eq!(elapsed_text(64_000), "1 min 4 s");
        assert!(shows_rate("3.2", 3.2499999999));
    }

    #[test]
    fn the_runners_job_rules_follow_the_design() {
        let (rows, more, caption) = expected_jobs(&json!({
            "active": [
                {"label":"Developing RAW","detail":"DSC_0412.NEF","elapsed_ms":1204},
                {"label":"Rendering preview","phase":"exact","elapsed_ms":700},
                {"label":"Measuring histogram","elapsed_ms":20}
            ],
            "recent": []
        }))
        .unwrap();
        assert_eq!(caption, json!("2 jobs"));
        assert_eq!(more, Value::Null);
        assert_eq!(rows[0]["detail"], json!("DSC_0412.NEF"));
        assert_eq!(rows[1]["detail"], json!("exact phase"));
        assert_eq!(rows[1]["trailing"], json!("0.7 s"));
        let (rows, _, caption) = expected_jobs(&json!({
            "active": [],
            "recent": [
                {"label":"Rendering preview","outcome":"completed","duration_ms":300,"ended_ms_ago":10},
                {"label":"Developing RAW","outcome":"completed","duration_ms":1610,"ended_ms_ago":4020}
            ]
        }))
        .unwrap();
        assert_eq!(caption, Value::Null);
        assert_eq!(rows[0]["detail"], json!("Finished 4 s ago"));
        assert_eq!(rows[0]["trailing"], json!("1.6 s"));
        let (rows, ..) = expected_jobs(&json!({"active": [], "recent": []})).unwrap();
        assert_eq!(rows[0]["label"], json!("No background work"));
    }

    /// The plan scripts one step per frame after the open, and its heavy step is Clarity on the
    /// JPEG and a temperature commit on a RAW, over the straighten either way.
    #[test]
    fn the_plan_picks_the_heavy_step_by_its_source() {
        let method = |plan: &Plan, step: &str| {
            let at = plan.index(step).expect("a planned step");
            plan.steps()[at].script().expect("a scripted step")["api"]["method"].clone()
        };
        let jpeg = plan(&[PathBuf::from("a/60mp.jpg")]);
        assert_eq!(jpeg.script().as_array().map(Vec::len), Some(jpeg.len() - 1));
        assert_eq!(method(&jpeg, "straightened"), json!("edit.crop-fit"));
        assert_eq!(method(&jpeg, "heavy"), json!("edit.set-presence"));
        let raw = plan(&[PathBuf::from("a/x.RAF")]);
        assert_eq!(method(&raw, "straightened"), json!("edit.crop-fit"));
        assert_eq!(method(&raw, "heavy"), json!("edit.set-raw-temperature"));
    }
}
