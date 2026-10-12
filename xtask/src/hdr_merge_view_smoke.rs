//! The `hdr-merge-view` smoke scenario: a photo-sized HDR merge edited with Clarity, panned at 100%
//! and dragged at 100% and at Fit in the real editor at 1440 × 900, every frame the surface draws
//! counted ([HDR merge design](../../docs/design/hdr-merge.md), "Preparation and editing";
//! [GPU previews](../../docs/design/gpu-preview.md), "The picture at rest").
//!
//! The run writes a 24 MP still bracket with `luxforge_testkit::hdr_bracket` (three DNGs at 0, −2
//! and +2 EV), makes a catalog with its folder indexed and merges it through the core's own
//! `merge.create` into a catalog folder of its own, so the launch opens a derived source: `f32`
//! linear planes holding values past diffuse white. The launch, with `LUXFORGE_TEST_CAMERAS=1`,
//! opens the result from that folder in Develop, commits Clarity, zooms to 100% and pans across
//! the photograph, drags Clarity at 100% while panning and releases it, then drags it at Fit and
//! releases it.
//!
//! Every frame the photo surface drew between two captures is counted, not only the captured ones:
//! no draw may leave the canvas showing through part of the photograph the view shows
//! (`uncovered_photo_draws`), draw no photograph (`blank_photo_draws`), or draw the CPU frame or
//! nothing where the GPU's picture had been and was still asked for (`gpu_lapsed_draws`).
use crate::{
    generate_catalog,
    gpu_preview_smoke::{PRESENCE, quiet_for},
    scenario::{Checked, Checks, Frame, Launch, Plan, Run, Step},
    smoke::Scenario,
    *,
};
use luxforge_core::{ApiRequest, ClientId, EditorService, OwnerHandle};
use luxforge_evidence::{
    self as script, CatalogStep, DevelopStep, SelectStep, SliderStep, ViewStep,
};
use luxforge_testkit::hdr_bracket::{Bracket, BracketSpec, PhotoSize};

pub const SCENARIO: &str = "hdr-merge-view";
pub const NOTE: &str = "The run first writes a 24 MP still bracket with \
    `luxforge_testkit::hdr_bracket::Bracket::write_photo` (three DNGs at 0, −2 and +2 EV, seed 1, \
    under `generated/Brackets/Still`), makes `generated/catalog.sqlite` with `Brackets` indexed and \
    merges the bracket through the core's `merge.create` into the catalog folder `Merges` \
    (`hdr-merge-view-expected.json`). The launch sets `LUXFORGE_TEST_CAMERAS=1` and opens that \
    catalog with `--catalog`.";
const GENERATED: &str = "generated";
const EXPECTED: &str = "hdr-merge-view-expected.json";
const BRACKETS: &str = "Brackets";
const STILL: &str = "Still";
const MERGES: &str = "Merges";
const FRAMES: usize = 3;
const CLARITY: &str = "clarity";
/// The committed Clarity, then a drag at 100% and one at Fit.
const COMMITTED: f64 = 40.0;
const DRAG_100: [f64; 4] = [45.0, 50.0, 55.0, 60.0];
const DRAG_FIT: [f64; 4] = [65.0, 70.0, 75.0, 80.0];
const DRAG_INTERVAL_MS: u64 = 60;
/// Where the view is scrolled to at 100%, one step at a time, as fractions of its range.
const PANS: [[f32; 2]; 6] = [
    [0.2, 0.2],
    [0.35, 0.3],
    [0.5, 0.45],
    [0.65, 0.6],
    [0.8, 0.75],
    [0.95, 0.95],
];
/// The scroll offsets the drag at 100% is sent at, one per value.
const DRAG_PANS: [[f32; 2]; 4] = [[0.9, 0.9], [0.7, 0.75], [0.5, 0.6], [0.3, 0.4]];
/// Long enough for a release's picture at rest, its tiles and its dissolve.
const SETTLE_MS: u64 = 2500;

/// The launch's frames: the result opened in Develop, Clarity committed, the pans at 100%, the drag
/// there, and the drag at Fit.
pub fn plan(expected: &Value) -> Result<Plan> {
    let position = expected["position"]
        .as_u64()
        .ok_or("The expected answers place no result")?;
    let mut steps = vec![
        Step::opened("opened"),
        Step::new("select", script::Step::key("g")),
        Step::new(
            "folder",
            script::Step::Catalog(CatalogStep::Source(MERGES.to_owned())),
        ),
        Step::new(
            "result",
            script::Step::Select(SelectStep::Click {
                position: position as u32,
                shift: false,
                command: false,
            }),
        ),
        Step::new("develop", script::Step::Develop(DevelopStep::Active)),
        Step::new(
            "clarity",
            script::Step::call("edit.set-presence", json!({ CLARITY: COMMITTED })),
        )
        .commits(1),
        quiet_for("clarity-settled", SETTLE_MS),
        Step::new("zoom-100", ViewStep::Percent(100.0))
            .commits(0)
            .no_draft(),
    ];
    for (index, [x, y]) in PANS.into_iter().enumerate() {
        steps.push(Step::new(format!("pan-{index}"), script::Step::Pan { x, y }).commits(0));
    }
    steps.extend([
        Step::new(
            "drag-100",
            SliderStep::new(PRESENCE, CLARITY, DRAG_100)
                .paced(DRAG_INTERVAL_MS)
                .pan_path(DRAG_PANS.to_vec()),
        )
        .commits(0),
        Step::new(
            "release-100",
            SliderStep::new(PRESENCE, CLARITY, [DRAG_100[3]]).release(),
        )
        .commits(1)
        .no_draft(),
        quiet_for("settled-100", SETTLE_MS),
        Step::new("pan-after", script::Step::Pan { x: 0.1, y: 0.1 }).commits(0),
        Step::new("fit", ViewStep::Fit).commits(0).no_draft(),
        quiet_for("fit-settled", SETTLE_MS),
        Step::new(
            "drag-fit",
            SliderStep::new(PRESENCE, CLARITY, DRAG_FIT).paced(DRAG_INTERVAL_MS),
        )
        .commits(0),
        Step::new(
            "release-fit",
            SliderStep::new(PRESENCE, CLARITY, [DRAG_FIT[3]]).release(),
        )
        .commits(1)
        .no_draft(),
        quiet_for("settled-fit", SETTLE_MS),
    ]);
    Ok(Plan::new(steps))
}

/// One request through the runner's own client.
fn ask(owner: &OwnerHandle, client: ClientId, method: &str, params: Value) -> Result<Value> {
    let response = owner
        .call(
            client,
            ApiRequest {
                id: format!("hdr-merge-view-{method}"),
                method: method.into(),
                params,
                token: None,
            },
        )
        .map_err(|error| format!("{method}: {error}"))?;
    match response.error {
        Some(error) => Err(format!("{method}: {}: {}", error.code, error.message).into()),
        None => Ok(response.result.unwrap_or(Value::Null)),
    }
}

/// Read a job until it ends.
fn settle(owner: &OwnerHandle, client: ClientId, job: &Value) -> Result<Value> {
    let started = std::time::Instant::now();
    loop {
        let read = ask(owner, client, "job.read", json!({ "job_id": job }))?;
        if !matches!(read["status"].as_str(), Some("queued" | "running")) {
            return Ok(read);
        }
        ensure(
            started.elapsed() < std::time::Duration::from_secs(600),
            format!("A job did not end: {read}"),
        )?;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Write the bracket, make the catalog with its folder indexed, merge it into its own catalog
/// folder and find the result's place there. Answers the expected record and every frame.
fn make(generated: &Path) -> Result<(Value, Vec<PathBuf>)> {
    let still = generated.join(BRACKETS).join(STILL);
    let frames = Bracket::new(&BracketSpec {
        frames: FRAMES,
        seed: 1,
        ..BracketSpec::default()
    })
    .write_photo(&still, PhotoSize::Mp24)?;
    let catalog = generated.join(generate_catalog::CATALOG);
    drop(EditorService::open(&catalog).map_err(|error| format!("a new catalog: {error}"))?);
    let brackets = generated.join(BRACKETS).canonicalize()?;
    let frames: Vec<PathBuf> = frames
        .iter()
        .map(|path| path.canonicalize())
        .collect::<std::io::Result<_>>()?;
    let (owner, join) = OwnerHandle::start(&catalog)
        .map_err(|error| format!("the core cannot open the catalog: {error}"))?;
    let client = owner.register();
    let made = (|| -> Result<Value> {
        let mutation = |id: &str| json!({"request_id": id, "actor": "setup"});
        let added = ask(
            &owner,
            client,
            "index.add-folder",
            json!({"path": brackets, "mutation": mutation("setup-brackets")}),
        )?;
        if !added["job_id"].is_null() {
            let read = settle(&owner, client, &added["job_id"])?;
            ensure(
                read["status"] == "ready",
                format!("the core could not list the bracket: {read}"),
            )?;
        }
        let created = ask(
            &owner,
            client,
            "folder.create",
            json!({"name": MERGES, "mutation": mutation("setup-folder")}),
        )?;
        let folder_id = created["folder"]["id"].clone();
        let targets = json!({"kind": "paths", "paths": frames});
        let plan = ask(
            &owner,
            client,
            "merge.plan",
            json!({"kind": "hdr", "targets": targets}),
        )?;
        ensure(
            plan["suitable"] == true,
            format!("the core refused the bracket: {plan}"),
        )?;
        let answer = ask(
            &owner,
            client,
            "merge.create",
            json!({"kind": "hdr", "targets": targets, "plan_id": plan["plan_id"],
                "folder": {"kind": "existing", "folder_id": folder_id},
                "mutation": mutation("setup-merge")}),
        )?;
        let job = answer["job"]["job_id"]
            .clone()
            .as_str()
            .map(|id| json!(id))
            .or_else(|| answer["job_id"].as_str().map(|id| json!(id)))
            .ok_or_else(|| format!("merge.create answered no job: {answer}"))?;
        let read = settle(&owner, client, &job)?;
        ensure(
            read["status"] == "ready",
            format!("the merge did not finish: {read}"),
        )?;
        let result = read["result"]["result_asset_id"].clone();
        ask(
            &owner,
            client,
            "browse.view",
            json!({"source": {"kind": "catalog-folder", "folder_id": folder_id}}),
        )?;
        let rows = ask(
            &owner,
            client,
            "browse.rows",
            json!({"from": 0, "count": 16}),
        )?;
        let position = rows["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["asset_id"] == result)
            .map(|row| row["position"].clone())
            .ok_or_else(|| format!("the result is not in its folder: {rows}"))?;
        Ok(
            json!({"result": result, "folder_id": folder_id, "position": position,
            "merge": read["result"], "plan": plan}),
        )
    })();
    owner.stop();
    let _ = join.join();
    Ok((made?, frames))
}

/// Make the bracket, the catalog and the merge (a replay reads the recorded answers), launch the
/// editor over them and check every frame.
pub fn run(mut run: Run, scenario: &'static Scenario, _: Vec<PathBuf>) -> Result {
    let generated = run.out().join(GENERATED);
    let catalog = generated.join(generate_catalog::CATALOG);
    let expected_file = run.out().join(EXPECTED);
    if let Some(note) = scenario.note {
        run.note(note);
    }
    run.check(|run| {
        let frames = if run.replaying() {
            Vec::new()
        } else {
            fs::create_dir_all(&generated)?;
            let (expected, frames) = make(&generated)?;
            write_json(&expected_file, &expected)?;
            frames
        };
        let expected = read_json(
            &run.recorded(&expected_file)
                .unwrap_or(expected_file.clone()),
        )?;
        if !run.replaying() {
            run.hash(&frames)?;
        }
        let plan = plan(&expected)?;
        let mut launch = Launch::named("view")
            .catalog(&catalog)
            .script("view-script.json", plan.script())
            .test_cameras();
        if let Some(window) = scenario.window {
            launch = launch.window(window);
        }
        let launched = run.launch(launch)?;
        let checked = plan.check(&launched.dir)?;
        (scenario.verify)(run, &[checked])?;
        if !run.replaying() {
            run.sources_unchanged()?;
        }
        Ok(())
    })
}

fn gpu(frame: &Frame, name: &str) -> Result<u64> {
    frame.state()["surface"]["gpu"][name]
        .as_u64()
        .ok_or_else(|| format!("{} lacks surface.gpu.{name}", frame["file"]).into())
}

/// What each counter grew by over the frames from `from` to the last, frame by frame.
fn counted(launch: &Checked, from: &str, counter: &str) -> Result<(u64, Vec<Value>)> {
    let start = launch.index(from)?;
    let names = launch.names();
    let mut before = gpu(launch.at(&names[start])?, counter)?;
    let first = before;
    let mut grew = Vec::new();
    for name in &names[start + 1..] {
        let now = gpu(launch.at(name)?, counter)?;
        ensure(now >= before, format!("{name}: {counter} went back"))?;
        if now > before {
            grew.push(json!({"step": name, "draws": now - before}));
        }
        before = now;
    }
    Ok((before - first, grew))
}

pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = crate::scenario::plan::only(launches)?;
    let mut checks = Checks::new();
    let develop = launch.at("develop")?;
    let source = &develop.state()["develop"]["document"];
    let mut counts = serde_json::Map::new();
    let mut failures = Vec::new();
    // From the open on: the photograph is the merge's, drawn by the GPU.
    for counter in [
        "uncovered_photo_draws",
        "blank_photo_draws",
        "gpu_lapsed_draws",
    ] {
        let (total, grew) = counted(launch, "develop", counter)?;
        counts.insert(counter.into(), json!({"total": total, "steps": grew}));
        if total > 0 {
            failures.push(format!("{counter} grew by {total}: {grew:?}"));
        }
    }
    // Where a pan reveals an edge before the region's re-plan lands, the backdrop fills it.
    let (backdrops, grew) = counted(launch, "zoom-100", "backdrop_draws")?;
    counts.insert(
        "backdrop_draws".into(),
        json!({"total": backdrops, "steps": grew}),
    );
    if backdrops == 0 {
        failures.push("no pan at 100% drew the backdrop".into());
    }
    let counts = Value::Object(counts);
    for name in ["clarity-settled", "pan-5", "settled-100", "settled-fit"] {
        let frame = launch.at(name)?;
        checks.note(
            frame,
            "the photograph the GPU presents",
            json!({"picture": frame.state()["surface"]["gpu"]["picture"],
                "drawing_path": frame.state()["surface"]["gpu"]["drawing_path"],
                "drawn_backdrop": frame.state()["surface"]["gpu"]["drawn_backdrop"]}),
        );
    }
    checks.write(
        &launch.evidence,
        SCENARIO,
        json!({"document": source, "counts": counts}),
    )?;
    ensure(failures.is_empty(), failures.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hdr_merge_view_plan_is_well_formed() {
        let plan = plan(&json!({"position": 0})).unwrap();
        plan.validate().unwrap();
        assert!(plan.scripted());
        let row = crate::smoke::find(SCENARIO).unwrap();
        assert!(row.rendered() && !row.takes_source());
    }
}
