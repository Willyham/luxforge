//! The steps that launch the release editor: ordinary background launches over a data root of the
//! output's own (`--data-root`), each a copy of a template the API installed into, so every launch
//! reads the store, the compiled-code cache and the catalog exactly as installed, and never what an
//! earlier launch left. The launch's own events log (`logs/events.jsonl` under its data root) gives
//! the editor's clock; the harness's `ps` samples give its RSS.
use super::{
    ACTOR, FIVE, Options, Report, copy_tree,
    store::{self, Built, DataRoot, Packages},
};
use crate::{
    scenario::{
        Launch, Run,
        launch::{self as watch, Poll, Watched, Watcher},
    },
    *,
};
use luxforge_testkit::client;
use std::time::{Duration, Instant};

/// How often a launch is polled for its log and sampled for its RSS.
const POLL: Duration = Duration::from_millis(20);
/// The deadline of one launch, the idle window included.
const DEADLINE: Duration = Duration::from_secs(120);
/// The workloads of `launch`: no external module, the example installed and unused, five
/// installed.
const LAUNCHES: [(&str, usize); 3] = [("none", 0), ("one_unused", 1), ("five", FIVE)];

/// The first event called `name` in `events`, at or after `from` ms on the editor's clock: its
/// `elapsed_ms` and the event.
fn event_at<'a>(events: &'a [Value], name: &str, from: f64) -> Option<(f64, &'a Value)> {
    events.iter().find_map(|event| {
        let at = event["elapsed_ms"].as_f64()?;
        (event["event"] == name && at >= from).then_some((at, event))
    })
}

/// The events logged under `root` so far; a line the editor is still writing is left out.
fn logged(root: &DataRoot) -> Vec<Value> {
    fs::read_to_string(root.events())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Watch a launch until `done` finds what it waits for in its events, sampling its RSS on every
/// poll, then hand over the time the harness saw it, the peak RSS until then and what `done`
/// found, to `then`, which may go on watching the running editor.
fn watched(
    root: &Path,
    data: DataRoot,
    done: impl Fn(&[Value]) -> Option<Value> + 'static,
    then: impl FnOnce(&mut watch::Guard, f64, f64, Value) -> Result<Value> + 'static,
) -> Watcher {
    let root = root.to_path_buf();
    Box::new(move |child, started, deadline| {
        let mut peak = 0.0_f64;
        let poll = Poll {
            every: POLL,
            from: started,
            deadline,
            late: "the launch did not reach what module-measure waits for within its deadline",
        };
        let reached = watch::watch(
            child,
            poll,
            || {
                Ok(done(&logged(&data))
                    .map(|found| (started.elapsed().as_secs_f64() * 1000.0, found)))
            },
            |child| {
                if let Ok((_, rss)) = stats::usage(&root, child.child.id()) {
                    peak = peak.max(rss);
                }
                Ok(())
            },
        )?;
        match reached {
            Watched::Reached((ms, found)) => Ok((None, then(child, ms, peak, found)?)),
            Watched::Exited(status) => Err(format!(
                "the editor exited ({status}) before module-measure saw what it waits for"
            )
            .into()),
        }
    })
}

/// The editor's first `render_ready`, on its clock.
fn ready(events: &[Value]) -> Option<Value> {
    event_at(events, "render_ready", 0.0).map(|(at, _)| json!(at))
}

/// One ordinary background launch of the editor over `data`, opening `source`.
fn ordinary(name: &str, data: &DataRoot, source: &Path, watcher: Watcher) -> Launch {
    Launch::ordinary(name)
        .data_root(&data.dir)
        .open_all(&[source.to_path_buf()])
        .deadline(DEADLINE)
        .watch(watcher)
}

/// The editor's provenance, from the first launch's events, into the report's header once.
fn provenance(report: &mut Report, run: &mut Run, data: &DataRoot) -> Result {
    if report.header["editor"].is_null() {
        let header = run.provenance(&logged(data))?;
        report.header["editor"] = header;
    }
    Ok(())
}

/// `launch`: [`LAUNCHES`]' templates installed through the API, then `launches` rounds of one
/// launch each, the workloads' order rotating each round, each launch over a fresh copy of its
/// template opening the 24 MP JPEG: the harness's time from spawn to seeing the first
/// `render_ready` in the log, the editor's own time to it, the peak RSS until then, and the idle
/// window after it (a one-second settle, then thirty seconds sampled every 500 ms).
pub(super) fn launch(
    report: &mut Report,
    root: &Path,
    out: &Path,
    packages: &Packages,
    options: &Options,
) -> Result {
    let start = report.load();
    let templates = out.join("launch-templates");
    let mut listed = serde_json::Map::new();
    for (name, count) in LAUNCHES {
        let template = DataRoot::new(&templates.join(name));
        let chosen: Vec<&Built> = packages.all[..count].iter().collect();
        listed.insert(name.into(), store::install(&template, &chosen)?);
    }
    let source = options.source(24).canonicalize()?;
    let mut run = Run::tool(
        root,
        &out.join("launch"),
        super::TOOL,
        &options.binary,
        DEADLINE,
    )?;
    run.hash(std::slice::from_ref(&source))?;
    let mut figures: Vec<[Vec<f64>; 6]> = LAUNCHES.iter().map(|_| Default::default()).collect();
    let mut launches = Vec::new();
    let checked = (|| -> Result {
        for round in 0..options.launches {
            for offset in 0..LAUNCHES.len() {
                let at = (round + offset) % LAUNCHES.len();
                let (name, _) = LAUNCHES[at];
                let label = format!("{name}-{round:02}");
                let data = DataRoot::new(&run.out().join(&label));
                copy_tree(&templates.join(name), &data.dir)?;
                let ps = root.to_path_buf();
                let watcher = watched(root, data.clone(), ready, move |child, ms, peak, at| {
                    let idle = stats::idle_window(&ps, child.child.id())?;
                    Ok(json!({"observed_ready_ms": ms, "app_ready_ms": at,
                        "peak_rss_to_ready_mib": peak, "idle_cpu_percent_one_core":
                        idle.cpu_percent_one_core, "idle_rss_mib_peak": idle.rss_mib_peak,
                        "idle_rss_mib_end": idle.rss_mib_end, "idle_s": idle.duration_s}))
                });
                let watched = run
                    .launch(ordinary(&label, &data, &source, watcher))?
                    .watched;
                provenance(report, &mut run, &data)?;
                for (index, key) in [
                    "observed_ready_ms",
                    "app_ready_ms",
                    "peak_rss_to_ready_mib",
                    "idle_rss_mib_peak",
                    "idle_rss_mib_end",
                    "idle_cpu_percent_one_core",
                ]
                .iter()
                .enumerate()
                {
                    let value = watched[key]
                        .as_f64()
                        .ok_or_else(|| format!("{label} recorded no {key}"))?;
                    figures[at][index].push(value);
                }
                launches.push(json!({"label": label, "workload": name, "watched": watched}));
            }
        }
        run.sources_unchanged()
    })();
    let outcome = match &checked {
        Ok(()) => Ok(()),
        Err(error) => Err(error.to_string().into()),
    };
    run.finish(outcome, |_| Ok(()))?;
    let load = report.span(start);
    let scope = format!(
        "ordinary background launches of the release editor (hidden window), each over a fresh \
         copy of a data root the API installed into (the compiled-code cache as the install left \
         it), opening the 24 MP JPEG with no layer; {} rounds, the workloads' order rotating; \
         RSS from ps every 20 ms until ready, then the idle window's 500 ms samples; GPU memory \
         not separated",
        options.launches
    );
    for (at, (name, _)) in LAUNCHES.iter().enumerate() {
        for (index, (metric, unit, what)) in [
            (
                "observed_ready_ms",
                "ms",
                "spawn (bundle copy included on macOS) to the harness seeing the first \
                 render_ready in the log, polled every 20 ms",
            ),
            (
                "app_ready_ms",
                "ms",
                "the editor's own clock from its start to its first render_ready",
            ),
            (
                "peak_rss_to_ready_mib",
                "MiB",
                "peak RSS sampled from spawn to ready",
            ),
            (
                "idle_rss_mib_peak",
                "MiB",
                "peak RSS in the idle window after ready",
            ),
            ("idle_rss_mib_end", "MiB", "RSS at the idle window's end"),
            (
                "idle_cpu_percent_one_core",
                "% of one core",
                "process CPU over the idle window",
            ),
        ]
        .iter()
        .enumerate()
        {
            report.row(
                &format!("launch.{name}.{metric}"),
                unit,
                figures[at][index].clone(),
                &format!("{what}; {scope}"),
                &load,
            );
        }
    }
    report.details.insert(
        "launch".into(),
        json!({"stores": listed, "launches": launches}),
    );
    report.write()?;
    checked
}

/// What `desktop-first-use` waits for: the view's probe answered and, when the program was
/// enabled, the GPU's picture at rest drawn after the answer.
fn probed(events: &[Value]) -> Option<Value> {
    let (answered, event) = event_at(events, "gpu_program_probed", 0.0)?;
    let enabled = event["detail"]["state"]["state"] == "enabled";
    if enabled && event_at(events, "gpu_rest_drawn", answered).is_none() {
        return None;
    }
    Some(json!(answered))
}

/// The figures of one first-use launch, on the editor's clock, from its events.
pub(crate) fn first_use_figures(events: &[Value]) -> Result<Value> {
    let ready = event_at(events, "render_ready", 0.0)
        .ok_or("no render_ready")?
        .0;
    let (asked, ask) =
        event_at(events, "gpu_program_probe", 0.0).ok_or("the view never asked for a probe")?;
    let (answered, answer) =
        event_at(events, "gpu_program_probed", asked).ok_or("the view's probe was not answered")?;
    let state = answer["detail"]["state"].clone();
    let drawn = event_at(events, "gpu_rest_drawn", answered).map(|(at, _)| at);
    let warm = event_at(events, "gpu_preview_warm", answered).map(|(at, _)| at);
    Ok(json!({
        "program": ask["detail"]["program"],
        "state": state,
        "app_ready_ms": ready,
        "probe_asked_ms": asked,
        "probe_answered_ms": answered,
        "probe_ms": answered - asked,
        "gpu_drawn_ms": drawn,
        "answer_to_gpu_drawn_ms": drawn.map(|at| at - answered),
        "warm_after_answer_ms": warm,
    }))
}

/// `desktop-first-use`: a template with the example installed through the API and, in the next
/// launch's owner, the 24 MP JPEG opened and given a Split toning layer; then `launches` ordinary
/// launches over fresh copies of it, the program unprobed (no probe cache), each opening that
/// photo: the editor's clock to its first `render_ready`, to the view's probe ask and its answer,
/// and to the GPU's picture at rest drawn after an enabled answer; and the peak RSS until then.
pub(super) fn first_use(
    report: &mut Report,
    root: &Path,
    out: &Path,
    packages: &Packages,
    options: &Options,
) -> Result {
    let start = report.load();
    let template = DataRoot::new(&out.join("first-use-template"));
    let built = packages.first();
    let listed = store::install(&template, &[built])?;
    let source = options.source(24).canonicalize()?;
    {
        let owner = store::owner(&template)?;
        let client = owner.client();
        let opened = owner.open(client, &source)?;
        let asset = opened["asset"]["id"].clone();
        let mut params = json!({"highlight-saturation": 40, "shadow-saturation": 30,
            "balance": 20, "asset_id": asset});
        params["mutation"] = client::mutation(
            owner.revision(client, &asset)?,
            &client::request_id(ACTOR),
            ACTOR,
        );
        let method = format!("edit.set-{}", built.package.manifest.name);
        let answer = owner.call(client, &method, params)?;
        ensure(
            answer["outcome"] == "applied",
            format!("{method} answered {answer}"),
        )?;
        owner.close()?;
    }
    ensure(
        !template.probe_cache().exists(),
        "the template holds a probe result",
    )?;
    let mut run = Run::tool(
        root,
        &out.join("desktop-first-use"),
        super::TOOL,
        &options.binary,
        DEADLINE,
    )?;
    run.hash(std::slice::from_ref(&source))?;
    let keys = [
        "app_ready_ms",
        "probe_asked_ms",
        "probe_ms",
        "answer_to_gpu_drawn_ms",
        "peak_rss_mib",
    ];
    let mut figures: [Vec<f64>; 5] = Default::default();
    let mut launches = Vec::new();
    let checked = (|| -> Result {
        for index in 0..options.launches {
            let label = format!("first-use-{index:02}");
            let data = DataRoot::new(&run.out().join(&label));
            copy_tree(&template.dir, &data.dir)?;
            let started = Instant::now();
            let watcher = watched(root, data.clone(), probed, move |_, _, peak, _| {
                Ok(json!({"peak_rss_mib": peak}))
            });
            let watched = run
                .launch(ordinary(&label, &data, &source, watcher))?
                .watched;
            let elapsed = started.elapsed().as_secs_f64();
            provenance(report, &mut run, &data)?;
            let mut row = first_use_figures(&logged(&data))?;
            row["peak_rss_mib"] = watched["peak_rss_mib"].clone();
            row["probe_cache"] = json!(
                fs::read_dir(data.probe_cache())
                    .map(|entries| entries.filter_map(|e| e.ok()).count())
                    .unwrap_or(0)
            );
            ensure(
                row["state"]["state"] == "enabled",
                format!("{label}: the probe left the program {}", row["state"]),
            )?;
            for (at, key) in keys.iter().enumerate() {
                if let Some(value) = row[key].as_f64() {
                    figures[at].push(value);
                }
            }
            launches.push(json!({"label": label, "figures": row, "launch_s": elapsed}));
        }
        run.sources_unchanged()
    })();
    let outcome = match &checked {
        Ok(()) => Ok(()),
        Err(error) => Err(error.to_string().into()),
    };
    run.finish(outcome, |_| Ok(()))?;
    let load = report.span(start);
    let scope = format!(
        "ordinary background launches of the release editor (hidden window), each over a fresh \
         copy of a data root holding the example installed through the API and the 24 MP JPEG \
         with a Split toning layer, its compiled code cached and its program unprobed; the \
         editor's own clock from its events; {} launches",
        options.launches
    );
    for (at, (key, unit, what)) in [
        (
            "app_ready_ms",
            "ms",
            "start to the first render_ready, the reference's picture",
        ),
        (
            "probe_asked_ms",
            "ms",
            "start to the view's probe ask (gpu_program_probe)",
        ),
        (
            "probe_ms",
            "ms",
            "the view's ask to its answer (gpu_program_probed): queued on the GPU tile worker, \
             the harness compiled on the window's adapter and every value set run",
        ),
        (
            "answer_to_gpu_drawn_ms",
            "ms",
            "the enabled answer to the GPU's picture at rest drawn with the program \
             (gpu_rest_drawn): the plan again, the surface's programs compiled and the draw",
        ),
        (
            "peak_rss_mib",
            "MiB",
            "peak RSS from spawn to the GPU's picture, ps every 20 ms",
        ),
    ]
    .iter()
    .enumerate()
    {
        report.row(
            &format!("desktop_first_use.{key}"),
            unit,
            figures[at].clone(),
            &format!("{what}; {scope}"),
            &load,
        );
    }
    report.details.insert(
        "desktop_first_use".into(),
        json!({"store": listed, "launches": launches}),
    );
    report.write()?;
    checked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, at: f64, detail: Value) -> Value {
        json!({"event": name, "elapsed_ms": at, "detail": detail})
    }

    /// The figures read the editor's clock in order: the probe after its ask, the GPU's picture
    /// after the answer, never an earlier event of the same name.
    #[test]
    fn first_use_figures_follow_the_events_in_order() {
        let events = [
            event("startup", 0.0, json!({})),
            event("gpu_rest_drawn", 400.0, json!({})),
            event("render_ready", 900.0, json!({})),
            event(
                "gpu_program_probe",
                950.0,
                json!({"program": "lf_ext_0123"}),
            ),
            event(
                "gpu_program_probed",
                1200.0,
                json!({"program": "lf_ext_0123", "state": {"state": "enabled"}}),
            ),
            event("gpu_preview_warm", 1210.0, json!({})),
            event("gpu_rest_drawn", 1500.0, json!({})),
        ];
        let figures = first_use_figures(&events).unwrap();
        assert_eq!(figures["app_ready_ms"], 900.0);
        assert_eq!(figures["probe_ms"], 250.0);
        assert_eq!(figures["answer_to_gpu_drawn_ms"], 300.0);
        assert_eq!(figures["warm_after_answer_ms"], 1210.0);
        assert_eq!(figures["state"]["state"], "enabled");
        assert_eq!(probed(&events), Some(json!(1200.0)));
        // Enabled but not drawn yet: still waiting.
        assert_eq!(probed(&events[..6]), None);
        // Disabled: done at the answer, with no GPU picture to wait for.
        let disabled = [
            event("render_ready", 900.0, json!({})),
            event("gpu_program_probe", 950.0, json!({})),
            event(
                "gpu_program_probed",
                1200.0,
                json!({"state": {"state": "disabled"}}),
            ),
        ];
        assert_eq!(probed(&disabled), Some(json!(1200.0)));
        assert!(first_use_figures(&disabled).unwrap()["answer_to_gpu_drawn_ms"].is_null());
        assert!(first_use_figures(&events[..2]).is_err());
    }
}
