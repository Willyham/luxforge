//! Native minimize/hide/restore and the presentation gate on an isolated background editor.
//! The window is physically ordered in but transparent, never activated. Captured frames are
//! renderer readbacks, not screenshots of an opaque foreground window. This scenario proves
//! native callbacks and timer/read admission, then records three thirty-second CPU windows per
//! presentation state. External `ps` readings are contained within the native observation windows.
use crate::{
    scenario::{Checked, Checks, Plan, Run, Step, Tolerance, launch, plan::only},
    *,
};
use luxforge_evidence as script;
use std::{
    process::ExitStatus,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const SCENARIO: &str = "visibility-monitoring";
pub const FIXTURE: &str = "fixtures/generated/24mp.jpg";
pub const READINGS: &str = "visibility-process-readings.json";
const WAIT_MS: u64 = 2_500;
const OBSERVATION_MS: u64 = 30_000;
const POLL_MS: u64 = 500;
const MAX_READINGS: usize = 900;

fn native(name: &str, action: &str) -> Step {
    Step::new(
        name,
        script::Step::WindowVisibility {
            action: action.into(),
        },
    )
    .commits(0)
}

pub fn plan(_: &[PathBuf]) -> Plan {
    let mut steps = vec![
        Step::opened("opened"),
        native("native_visible", "show_window"),
        Step::new("filled", script::Step::wait(WAIT_MS)).commits(0),
        native("window_hidden", "hide_window"),
        Step::new("window_asleep", script::Step::wait(WAIT_MS)).commits(0),
        native("window_restored", "show_window"),
        native("minimized", "minimize"),
        Step::new("minimized_asleep", script::Step::wait(WAIT_MS)).commits(0),
        native("restored", "restore"),
        native("app_hidden", "hide_app"),
        Step::new("app_asleep", script::Step::wait(WAIT_MS)).commits(0),
        native("app_restored", "show_app"),
        Step::new("collapsed", script::Step::performance(false)).commits(0),
        Step::new("collapsed_asleep", script::Step::wait(WAIT_MS)).commits(0),
        Step::new("reopened", script::Step::performance(true)).commits(0),
    ];
    let observe = |name| {
        Step::new(
            name,
            script::Step::Observe(script::IdleStep {
                settle_ms: 1_000,
                ms: OBSERVATION_MS,
            }),
        )
        .commits(0)
    };
    for repeat in 1..=3 {
        steps.extend([
            observe(format!("cpu_expanded_{repeat}")),
            Step::new(
                format!("cpu_collapse_{repeat}"),
                script::Step::performance(false),
            )
            .commits(0),
            observe(format!("cpu_collapsed_{repeat}")),
            native(&format!("cpu_hide_{repeat}"), "hide_window"),
            Step::new(
                format!("cpu_expand_hidden_{repeat}"),
                script::Step::performance(true),
            )
            .commits(0),
            observe(format!("cpu_hidden_{repeat}")),
            native(&format!("cpu_show_{repeat}"), "show_window"),
        ]);
    }
    Plan::new(
        steps
            .into_iter()
            .map(|step| step.workspace("state_panel", json!(true)).no_draft())
            .collect(),
    )
}

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or_default()
}

/// External cumulative CPU and RSS, sampled by the shared process sampler. No sampling request
/// enters the editor. The vector is bounded independently of its launch's 420-second deadline.
pub fn watch(
    child: &mut launch::Guard,
    started: Instant,
    timeout: Duration,
) -> Result<(Option<ExitStatus>, Value)> {
    let root = root()?;
    let pid = child.child.id();
    let mut samples = Vec::new();
    let status = launch::until_exit(
        child,
        launch::Poll {
            every: Duration::from_millis(POLL_MS),
            from: started,
            deadline: timeout,
            late: "Visibility measurement child timed out; killed and reaped",
        },
        |child| {
            ensure(
                samples.len() < MAX_READINGS,
                "External visibility readings exceeded their bound",
            )?;
            let before = wall_ms();
            match stats::usage(&root, pid) {
                Ok((cpu_seconds, rss_mib)) => samples.push(json!({
                    "start_wall_ms":before,"end_wall_ms":wall_ms(),
                    "cpu_seconds":cpu_seconds,"rss_mib":rss_mib,
                })),
                Err(error) if child.child.try_wait()?.is_none() => return Err(error),
                Err(_) => {}
            }
            Ok(())
        },
    )?;
    Ok((
        Some(status),
        json!({"pid":pid,"poll_ms":POLL_MS,
        "cpu_resolution_ms":10,"max_readings":MAX_READINGS,"samples":samples}),
    ))
}

/// Use only reads wholly inside the app's declared observation, so restored captures and the
/// boundary diagnostic messages cannot inflate the quiet interval's external CPU estimate.
pub(crate) fn external_window(start: u64, end: u64, samples: &[Value]) -> Result<Value> {
    ensure(end > start, "The native observation window did not advance")?;
    let held: Vec<&Value> = samples
        .iter()
        .filter(|sample| {
            sample["start_wall_ms"]
                .as_u64()
                .is_some_and(|at| at >= start)
                && sample["end_wall_ms"].as_u64().is_some_and(|at| at <= end)
        })
        .collect();
    let first = held
        .first()
        .ok_or("No external readings inside native observation")?;
    let last = held
        .last()
        .ok_or("No external readings inside native observation")?;
    let midpoint = |sample: &Value| -> Result<f64> {
        let before = sample["start_wall_ms"]
            .as_u64()
            .ok_or("Missing external read start")?;
        let after = sample["end_wall_ms"]
            .as_u64()
            .ok_or("Missing external read end")?;
        ensure(after >= before, "External read clock moved backwards")?;
        Ok((before as f64 + after as f64) / 2.0)
    };
    let first_at = midpoint(first)?;
    let last_at = midpoint(last)?;
    let elapsed_s = (last_at - first_at) / 1_000.0;
    ensure(
        held.len() >= 2 && elapsed_s >= (end - start) as f64 / 1_000.0 - 2.0,
        "External contained CPU window is too short",
    )?;
    let before = first["cpu_seconds"]
        .as_f64()
        .ok_or("Missing external CPU start")?;
    let after = last["cpu_seconds"]
        .as_f64()
        .ok_or("Missing external CPU end")?;
    ensure(
        after >= before,
        "External cumulative process CPU moved backwards",
    )?;
    let excluded_start_ms = first_at - start as f64;
    let excluded_end_ms = end as f64 - last_at;
    // The observation is thirty seconds and reads are every 500 ms plus ps runtime. Record the
    // actual gap instead of claiming precisely 500 ms when a host delayed the sampler.
    Ok(
        json!({"cpu_percent_one_core":100.0 * (after - before) / elapsed_s,
        "duration_s":elapsed_s,"process_cpu_ms":1_000.0 * (after - before),
        "native_start_wall_ms":start,"native_end_wall_ms":end,
        "excluded_start_ms":excluded_start_ms,"excluded_end_ms":excluded_end_ms,
        "edge_within_poll":excluded_start_ms <= POLL_MS as f64 && excluded_end_ms <= POLL_MS as f64,
        "samples":held.len(),"first":first,"last":last,
        "cpu_resolution_ms":10,"poll_ms":POLL_MS}),
    )
}

fn number(frame: &Value, section: &str, key: &str) -> Result<u64> {
    frame["state"][section][key]
        .as_u64()
        .ok_or_else(|| format!("Frame has no {section}.{key} counter").into())
}

fn native_state(frame: &Value, allowed: bool) -> Result {
    let visibility = &frame["state"]["visibility"];
    ensure(
        visibility["supported"] == true
            && visibility["ready"] == true
            && visibility["unavailable"].is_null()
            && visibility["evidence_invisible_window_override"] == false
            && visibility["sampling_allowed"] == allowed
            && number(frame, "visibility", "native_sequence")? > 0,
        format!("Native visibility is unavailable, overridden or wrong: {visibility}"),
    )
}

fn changed(before: &Value, after: &Value) -> Result {
    ensure(
        number(after, "visibility", "native_sequence")?
            > number(before, "visibility", "native_sequence")?
            && number(after, "visibility", "transitions")?
                > number(before, "visibility", "transitions")?,
        "Native operation produced no adopted visibility callback",
    )
}

fn asleep(hidden: &Value, later: &Value, fact: &str) -> Result<Value> {
    for frame in [hidden, later] {
        native_state(frame, false)?;
        ensure(
            frame["state"]["visibility"][fact] == true,
            format!("Hidden frame has no native {fact} fact"),
        )?;
        let section = &frame["state"]["performance"];
        ensure(
            section["expanded"] == true && section["sampling"] == false,
            "Hiding changed saved disclosure or retained the Performance timer",
        )?;
        ensure(
            frame["state"]["long_work"]["timers"] == json!({"throttle":false,"refresh":false}),
            "A hidden window retained a long-work display timer",
        )?;
    }
    let reads = number(hidden, "performance", "reads_requested")?;
    let samples = number(hidden, "performance", "samples")?;
    ensure(
        number(later, "performance", "reads_requested")? == reads
            && number(later, "performance", "samples")? == samples,
        "A hidden window admitted another resource read or adopted an old sample",
    )?;
    ensure(
        later["state"]["performance"]["in_flight"] == false,
        "A resource read remained in flight after the hidden window's quiet interval",
    )?;
    ensure(
        number(hidden, "visibility", "native_sequence")?
            == number(later, "visibility", "native_sequence")?,
        "Unchanged hidden native facts unexpectedly generated another callback",
    )?;
    Ok(
        json!({"fact":fact,"asleep_ms":WAIT_MS,"reads_requested":reads,"samples":samples,
        "native_sequence":number(later,"visibility","native_sequence")?,
        "long_work":later["state"]["long_work"]}),
    )
}

fn fresh(before: &Value, after: &Value) -> Result<Value> {
    native_state(after, true)?;
    let section = &after["state"]["performance"];
    let reads = number(after, "performance", "reads_requested")?;
    ensure(
        section["expanded"] == true
            && section["sampling"] == true
            && section["error"].is_null()
            && section["in_flight"] == false
            && number(after, "performance", "samples")? == 1
            && reads == number(before, "performance", "reads_requested")? + 1
            && section["previous_resources"].is_null(),
        format!("Restore did not start exactly one fresh resource read: {section}"),
    )?;
    let rows = section["rows"].as_array().ok_or("Missing resource rows")?;
    ensure(
        rows.len() == 3
            && rows[0]["label"] == "Memory"
            && rows[0]["series_len"] == 1
            && rows[1]["label"] == "CPU"
            && rows[2]["label"] == "GPU"
            && rows[1..]
                .iter()
                .all(|row| row["value"] == "\u{2013}" && row["series_len"] == 0),
        "A restored resource rate included samples from before the hidden interval",
    )?;
    Ok(json!({"reads_requested":reads,"samples":1,"rows":rows}))
}

fn observation(mode: &str, ended: &Value, samples: &[Value]) -> Result<Value> {
    let before = &ended["before"];
    let after = &ended["after"];
    let start = before["wall_ms"]
        .as_u64()
        .ok_or("Observation has no start wall clock")?;
    let end = ended["wall_ms"]
        .as_u64()
        .ok_or("Observation has no end wall clock")?;
    ensure(
        ended["window_ms"] == OBSERVATION_MS,
        "Unexpected presentation observation duration",
    )?;
    let visible = mode != "hidden";
    let expanded = mode != "collapsed";
    for state in [before, after] {
        native_state(&json!({"state":state}), visible)?;
        ensure(
            state["performance"]["expanded"] == expanded
                && state["performance"]["sampling"] == (visible && expanded)
                && state["long_work"]["timers"] == json!({"throttle":false,"refresh":false}),
            format!("Observation's {mode} state or timers changed: {state}"),
        )?;
    }
    let reads_before = before["performance"]["reads_requested"]
        .as_u64()
        .ok_or("No baseline resource reads")?;
    let reads_after = after["performance"]["reads_requested"]
        .as_u64()
        .ok_or("No final resource reads")?;
    let reads = reads_after
        .checked_sub(reads_before)
        .ok_or("Resource read count moved backwards")?;
    ensure(
        if mode == "expanded" {
            (28..=32).contains(&reads)
        } else {
            reads == 0
        },
        format!("Observation's {mode} state admitted {reads} resource reads"),
    )?;
    ensure(
        before["visibility"]["native_sequence"] == after["visibility"]["native_sequence"],
        "Native facts changed during a CPU observation",
    )?;
    let delta = |key: &str| {
        after[key]
            .as_u64()
            .zip(before[key].as_u64())
            .map(|(after, before)| after.saturating_sub(before))
    };
    Ok(
        json!({"mode":mode,"external":external_window(start,end,samples)?,
        "internal":ended,"resource_reads_delta":reads,
        "updates_delta":delta("updates"),"full_updates_delta":delta("full_updates"),
        "views_delta":delta("views")}),
    )
}

pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    ensure(
        !launch.events.iter().any(|event| {
            matches!(
                event["event"].as_str(),
                Some("performance_read_failed" | "window_visibility_unavailable")
            )
        }),
        "A native observer or resource read failed",
    )?;
    let base = launch.at("native_visible")?;
    native_state(base, true)?;
    changed(launch.at("opened")?, base)?;
    ensure(
        number(launch.at("filled")?, "performance", "samples")? >= 3,
        "The visible one-second sampler did not fill during the visible interval",
    )?;
    let mut checks = Checks::new();
    for (before, hidden, quiet, restored, fact) in [
        (
            "filled",
            "window_hidden",
            "window_asleep",
            "window_restored",
            "window_hidden",
        ),
        (
            "window_restored",
            "minimized",
            "minimized_asleep",
            "restored",
            "minimized",
        ),
        (
            "restored",
            "app_hidden",
            "app_asleep",
            "app_restored",
            "app_hidden",
        ),
    ] {
        changed(launch.at(before)?, launch.at(hidden)?)?;
        let detail = asleep(launch.at(hidden)?, launch.at(quiet)?, fact)?;
        checks.note(
            launch.at(quiet)?,
            "native hidden interval admitted no resource reads",
            detail,
        );
        changed(launch.at(quiet)?, launch.at(restored)?)?;
        checks.note(
            launch.at(restored)?,
            "native restore begins a fresh sample window",
            fresh(launch.at(quiet)?, launch.at(restored)?)?,
        );
    }
    let collapsed = launch.at("collapsed")?;
    let quiet = launch.at("collapsed_asleep")?;
    for frame in [collapsed, quiet] {
        native_state(frame, true)?;
        ensure(
            frame["state"]["performance"]["expanded"] == false
                && frame["state"]["performance"]["sampling"] == false,
            "Visible collapsed Performance section retained sampling",
        )?;
    }
    ensure(
        number(collapsed, "performance", "reads_requested")?
            == number(quiet, "performance", "reads_requested")?,
        "The collapsed section read resources while idle",
    )?;
    checks.note(
        launch.at("reopened")?,
        "reopening after native transitions retains existing disclosure semantics",
        fresh(quiet, launch.at("reopened")?)?,
    );

    let external = read_json(
        &launch
            .evidence
            .parent()
            .ok_or("Evidence has no parent")?
            .join(READINGS),
    )?;
    let samples = external["samples"]
        .as_array()
        .ok_or("No external process readings")?;
    ensure(
        samples.len() <= MAX_READINGS && external["poll_ms"] == POLL_MS,
        "External readings have wrong scope or exceeded their bound",
    )?;
    let started: Vec<&Value> = launch
        .events
        .iter()
        .filter(|event| event["event"] == "presentation_observation_started")
        .collect();
    let ended: Vec<&Value> = launch
        .events
        .iter()
        .filter(|event| event["event"] == "presentation_observation_ended")
        .collect();
    ensure(
        started.len() == 9 && ended.len() == 9,
        "Expected nine native presentation observation windows",
    )?;
    let mut observations = Vec::with_capacity(9);
    for (index, (start, end)) in started.iter().zip(&ended).enumerate() {
        ensure(
            start["detail"] == end["detail"]["before"],
            "Observation start and end baseline differ",
        )?;
        let repeat = index / 3 + 1;
        let mode = ["expanded", "collapsed", "hidden"][index % 3];
        let frame = launch.at(&format!("cpu_{mode}_{repeat}"))?;
        ensure(
            frame["state"]["performance"]["pid"] == external["pid"],
            "The external watcher observed another process",
        )?;
        let mut detail = observation(mode, &end["detail"], samples)?;
        detail["repeat"] = json!(repeat);
        checks.note(
            frame,
            "contained external CPU window and presentation counters",
            detail.clone(),
        );
        observations.push(detail);
        if mode == "hidden" {
            let restored = launch.at(&format!("cpu_show_{repeat}"))?;
            changed(frame, restored)?;
            let _ = fresh(frame, restored)?;
        }
    }
    let cpu_rows = ["expanded", "collapsed", "hidden"].map(|mode| {
        stats::row(
            &format!("visibility.{mode}.cpu_percent_one_core"),
            "% of one core",
            observations
                .iter()
                .filter(|window| window["mode"] == mode)
                .filter_map(|window| window["external"]["cpu_percent_one_core"].as_f64()),
        )
    });

    // Native presentation state cannot alter an edit, source size or displayed photograph.
    // Sample three separated interior patches through the shared recorded photo locator.
    let patches = [[0.2, 0.25], [0.5, 0.5], [0.8, 0.75]];
    let reference = patches
        .iter()
        .map(|at| base.rgb_at(*at, 3))
        .collect::<Result<Vec<_>>>()?;
    for (name, frame) in launch.names().iter().zip(&launch.frames) {
        ensure(
            frame["state"]["stack"] == base["state"]["stack"]
                && frame["state"]["source_dimensions"] == base["state"]["source_dimensions"]
                && frame["state"]["geometry"] == base["state"]["geometry"],
            format!("Step {name:?}: native visibility altered the original or recipe"),
        )?;
        for (index, at) in patches.iter().enumerate() {
            let rgb = frame.rgb_at(*at, 3)?;
            for (channel, value) in rgb.iter().enumerate() {
                checks.compare(
                    frame,
                    &format!("unchanged photo patch {index} channel {channel}"),
                    *value,
                    reference[index][channel],
                    Tolerance::Within(1.0),
                )?;
            }
        }
        checks.note(
            frame,
            "native callback and presentation counters",
            json!({
                "step":name,"visibility":frame["state"]["visibility"],
                "reads_requested":frame["state"]["performance"]["reads_requested"],
                "long_work":frame["state"]["long_work"],
            }),
        );
    }
    checks.write(&launch.evidence, SCENARIO, json!({
        "native_platform":"macOS AppKit", "wait_ms":WAIT_MS,
        "scope":"Native lifecycle callbacks and presentation read/timer admission; transparent isolated background window. Three thirty-second windows per state after a one-second settle, external ps every500ms; CPU percent of one core over contained sample spans. Boundary evidence reads/logs are excluded from the external interval; expanded Performance sampling remains instrumented. No guaranteed zero CPU or job-lifecycle claim.",
        "other_platforms":"Native Windows/Linux visibility facts are unsupported and unqualified.",
        "photo_patch_tolerance":1.0,
        "cpu_observations":observations,"rows":cpu_rows,
        "external_readings_file":READINGS,"max_external_readings":MAX_READINGS,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(sequence: u64, reads: u64, hidden: bool) -> Value {
        json!({"state":{
            "visibility":{"supported":true,"ready":true,"unavailable":null,
                "evidence_invisible_window_override":false,"sampling_allowed":!hidden,
                "window_hidden":hidden,"native_sequence":sequence,"transitions":sequence},
            "performance":{"expanded":true,"sampling":!hidden,"reads_requested":reads,
                "samples":1,"in_flight":false,"error":null,"previous_resources":null,
                "rows":[{"label":"Memory","series_len":1},
                    {"label":"CPU","series_len":0,"value":"\u{2013}"},
                    {"label":"GPU","series_len":0,"value":"\u{2013}"}]},
            "long_work":{"timers":{"throttle":false,"refresh":false}}
        }})
    }

    #[test]
    fn visibility_checks_refuse_hidden_read_leaks_and_synthetic_only_facts() {
        let hidden = state(2, 3, true);
        let mut quiet = hidden.clone();
        assert!(asleep(&hidden, &quiet, "window_hidden").is_ok());
        quiet["state"]["performance"]["reads_requested"] = json!(4);
        assert!(asleep(&hidden, &quiet, "window_hidden").is_err());
        quiet = hidden.clone();
        quiet["state"]["long_work"]["timers"]["refresh"] = json!(true);
        assert!(asleep(&hidden, &quiet, "window_hidden").is_err());
        quiet = hidden.clone();
        quiet["state"]["visibility"]["native_sequence"] = json!(0);
        assert!(native_state(&quiet, false).is_err());
        assert!(changed(&hidden, &hidden).is_err());
        assert!(changed(&hidden, &state(3, 4, false)).is_ok());
    }

    #[test]
    fn visibility_checks_refuse_rates_that_span_hidden_time_or_multiple_restore_reads() {
        let before = state(2, 3, true);
        let mut restored = state(3, 4, false);
        assert!(fresh(&before, &restored).is_ok());
        restored["state"]["performance"]["rows"][1]["value"] = json!("1.0");
        assert!(fresh(&before, &restored).is_err());
        restored = state(3, 4, false);
        restored["state"]["performance"]["previous_resources"] = json!({"cpu":{"time_ns":4}});
        assert!(fresh(&before, &restored).is_err());
        restored = state(3, 5, false);
        assert!(fresh(&before, &restored).is_err());
    }

    #[test]
    fn visibility_cpu_window_excludes_reads_that_cross_observation_boundaries() {
        let sample =
            |start, end, cpu| json!({"start_wall_ms":start,"end_wall_ms":end,"cpu_seconds":cpu});
        let samples = [
            sample(4_998, 5_002, 0.0),
            sample(5_004, 5_008, 1.0),
            sample(34_992, 34_996, 1.3),
            sample(34_998, 35_002, 100.0),
        ];
        let report = external_window(5_000, 35_000, &samples).unwrap();
        assert_eq!(report["samples"], 2);
        assert_eq!(report["first"], samples[1]);
        assert_eq!(report["last"], samples[2]);
        assert!((report["cpu_percent_one_core"].as_f64().unwrap() - 30.0 / 29.988).abs() < 1e-9);
        assert!(
            external_window(5_000, 35_000, &samples[..2]).is_err(),
            "one read cannot give CPU duration"
        );
        assert!(external_window(35_000, 5_000, &samples).is_err());
    }
}
