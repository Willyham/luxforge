//! Analysis, the picker and the selection of objects through the catalog owner over the fake
//! runtime's analysis models, as a client drives them: the flag and each model installed through
//! the ordinary grant and install job, `task.analyse` filling the cache off the owner and answering
//! from it after, `query.pick-object` on the query worker for hover, a click and a stroke,
//! `task.select-object` publishing what the query picked, the cache's invalidation by a repair edit
//! and by removing a model, and every refusal by name.
use super::*;
use crate::modules::ai::analysis::models::{
    FAKE_DEPTH, FAKE_PEOPLE, FAKE_SEGMENTER_DECODER, FAKE_SEGMENTER_ENCODER, FAKE_SEMANTIC,
    FAKE_SUBJECT,
};

const ANALYSE: &str = "task.analyse";
const PICK: &str = "query.pick-object";
const SELECT: &str = "task.select-object";

trait Analysis {
    fn analysis_flag(&self, on: bool);
    /// A task, waiting through any preparation its read asks for: its finished job.
    fn task(&self, method: &str, params: Value) -> Value;
    fn pick(&self, params: Value) -> Result<Value, ApiFailure>;
    fn edit_revision(&self, asset: &AssetId) -> u64;
}

impl Analysis for Owner {
    fn analysis_flag(&self, on: bool) {
        self.ok("flags.set", json!({"flag": "ai.analysis", "value": on}));
    }

    fn task(&self, method: &str, params: Value) -> Value {
        for _ in 0..4 {
            let response = self.call(self.edit, method, params.clone());
            match response.error {
                None => {
                    let queued = response.result.expect("a result");
                    return self.finished(&queued["job_id"]);
                }
                Some(error) if error.code == "preparation-required" => {
                    let job = error.job_id.expect("a preparation names its job");
                    let status = self.finished(&json!(job));
                    assert_eq!(status["status"], "ready", "{status}");
                }
                Some(error) => panic!("{method}: {error:?}"),
            }
        }
        panic!("{method} kept asking for preparation");
    }

    fn pick(&self, params: Value) -> Result<Value, ApiFailure> {
        let response = self.call(self.edit, PICK, params);
        match response.error {
            None => Ok(response.result.expect("a result")),
            Some(error) => Err(error),
        }
    }

    fn edit_revision(&self, asset: &AssetId) -> u64 {
        self.ok("asset.state", json!({"asset_id": asset}))["revision"]
            .as_u64()
            .unwrap()
    }
}

/// The owner with the analysis flag on and the fake segmenter installed.
fn segmenter_ready(fixture: &Fixture) -> Owner {
    let owner = fixture.start();
    owner.analysis_flag(true);
    owner.install(&FAKE_SEGMENTER_ENCODER.file);
    owner.install(&FAKE_SEGMENTER_DECODER.file);
    owner
}

fn requirement_kinds(error: &ApiFailure) -> Vec<String> {
    error.data.as_ref().unwrap()["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{} {}",
                r["kind"].as_str().unwrap(),
                r["id"].as_str().unwrap()
            )
        })
        .collect()
}

#[test]
#[slow]
fn analyse_once_then_pick_and_select_from_the_cache() {
    let fixture = Fixture::new("analysis-journey");
    let [asset, _] = fixture.assets.clone();
    let owner = segmenter_ready(&fixture);
    let status = owner.status(AI_MODULE);
    let tasks = &status["analysis"]["tasks"];
    assert_eq!(tasks[0]["id"], "task.analyse", "{status}");
    assert_eq!(tasks[0]["offered"], true, "{status}");
    // The scene and depth tasks are offered once their models are installed.
    assert_eq!(tasks[3]["offered"], false);

    // Nothing analysed yet: the picker names the task that would analyse it.
    let point = json!({"asset_id": asset, "point": [[0.5, 0.5]]});
    let error = owner.pick(point.clone()).unwrap_err();
    assert_eq!(error.code, "not-ready", "{error:?}");
    assert_eq!(requirement_kinds(&error), ["analysis analyse"]);

    // The analysis runs off the owner and fills the cache; a repeat answers from it.
    let runs = fixture.controls().runs();
    let job = owner.task(ANALYSE, json!({"asset_id": asset}));
    assert_eq!(job["status"], "ready", "{job}");
    let result = &job["result"]["result"];
    assert_eq!(result["cached"], false, "{result}");
    assert_eq!(result["input"], json!([480, 320]));
    assert_eq!(result["guide"], json!([480, 320]));
    assert_eq!(
        result["models"][0]["id"],
        FAKE_SEGMENTER_ENCODER.file.resource
    );
    assert_eq!(fixture.controls().runs(), runs + 1);
    let again = owner.task(ANALYSE, json!({"asset_id": asset}));
    assert_eq!(again["result"]["result"]["cached"], true, "{again}");
    assert_eq!(
        fixture.controls().runs(),
        runs + 1,
        "a cached analysis runs nothing"
    );
    let cache = &owner.status(AI_MODULE)["runtime"]["analysis_cache"];
    assert_eq!(
        cache["entries"], 3,
        "the input, the guide and the embedding: {cache}"
    );
    assert!(cache["hits"].as_u64().unwrap() >= 2, "{cache}");

    // Hover and a click are one query; the same prompt answers the same candidates.
    let hover = owner.pick(point.clone()).unwrap();
    assert_eq!(hover["decision"], "one", "{hover}");
    assert_eq!(hover["candidates"].as_array().unwrap().len(), 3);
    let chosen = hover["chosen"][0].as_u64().unwrap() as usize;
    let preview = &hover["candidates"][chosen]["preview"];
    assert!(preview["width"].as_u64().unwrap() * preview["height"].as_u64().unwrap() <= 1 << 16);
    for (position, candidate) in hover["candidates"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            candidate["preview"].is_null(),
            position != chosen,
            "{candidate}"
        );
    }
    assert_eq!(owner.pick(point.clone()).unwrap(), hover);
    assert_eq!(owner.status(AI_MODULE)["runtime"]["query_worker"], true);

    // A stroke is the prototype's prompts and decision.
    let stroke = json!({"asset_id": asset, "stroke": [[0.3, 0.5], [0.7, 0.5]], "radius": 0.03});
    let painted = owner.pick(stroke.clone()).unwrap();
    assert!(
        ["one", "several", "nothing"].contains(&painted["decision"].as_str().unwrap()),
        "{painted}"
    );
    assert!(
        painted["candidates"][0]["stroke_coverage"].is_number(),
        "{painted}"
    );
    assert_eq!(owner.pick(stroke).unwrap(), painted);

    // Selecting the hovered object publishes the candidate the query chose; again, the same
    // artifact.
    let runs = fixture.controls().runs();
    let selected = owner.task(SELECT, point.clone());
    assert_eq!(selected["status"], "ready", "{selected}");
    let selection = &selected["result"]["result"];
    assert_eq!(selection["class"], "object");
    assert_eq!(selection["model"], FAKE_SEGMENTER_ENCODER.file.resource);
    assert_eq!(
        (selection["width"].clone(), selection["height"].clone()),
        (json!(480), json!(320))
    );
    assert_eq!(
        selection["candidate"]["bounds"], hover["candidates"][chosen]["bounds"],
        "{selection}"
    );
    assert_eq!(
        fixture.controls().runs(),
        runs + 1,
        "the decoder only: {selection}"
    );
    let twice = owner.task(SELECT, point.clone());
    assert_eq!(twice["result"]["result"]["artifact"], selection["artifact"]);

    // What it answers is what mask.create-model takes, current for the photograph as it is.
    let revision = owner.edit_revision(&asset);
    let mut create = json!({"asset_id": asset, "mutation": mutation_json(revision, "select")});
    for field in [
        "artifact",
        "class",
        "model",
        "model_version",
        "input_fingerprint",
        "prefix_hash",
    ] {
        create[field] = selection[field].clone();
    }
    let created = owner.ok("mask.create-model", create);
    assert!(created["mask"].is_string(), "{created}");

    // An edit after the repair stage keeps the analysis; a correction makes it stale.
    let revision = owner.edit_revision(&asset);
    owner.ok(
        "edit.set-basic",
        json!({"asset_id": asset, "mutation": mutation_json(revision, "exposure"), "exposure": 0.5}),
    );
    assert_eq!(owner.pick(point.clone()).unwrap()["decision"], "one");
    let revision = owner.edit_revision(&asset);
    owner.ok(
        "edit.add-correction",
        json!({"asset_id": asset, "mutation": mutation_json(revision, "heal"),
               "points": [[0.1, 0.1], [0.12, 0.1]], "radius": 0.02,
               "source-x": 0.3, "source-y": 0.3}),
    );
    let stale = owner.pick(point.clone()).unwrap_err();
    assert_eq!(requirement_kinds(&stale), ["analysis analyse"], "{stale:?}");
    let job = owner.task(ANALYSE, json!({"asset_id": asset}));
    assert_eq!(job["result"]["result"]["cached"], false, "{job}");
    assert_eq!(owner.pick(point.clone()).unwrap()["decision"], "one");

    // Removing a model forgets everything the module computed. The pick below waits for the
    // removal to finish, so it reads the model as removed rather than racing the transfer lane.
    let removed = owner.ok(
        "module.resource.remove",
        json!({"module_id": AI_MODULE, "resource_id": FAKE_SEGMENTER_DECODER.file.resource}),
    );
    owner.finished(&removed["job_id"]);
    let cache = &owner.status(AI_MODULE)["runtime"]["analysis_cache"];
    assert_eq!(cache["entries"], 0, "{cache}");
    assert_eq!(cache["bytes"], 0, "{cache}");
    let error = owner.pick(point).unwrap_err();
    assert_eq!(error.code, "not-ready");
    assert_eq!(
        requirement_kinds(&error),
        [format!("resource {}", FAKE_SEGMENTER_DECODER.file.resource)]
    );
    owner.stop();
}

#[test]
#[slow]
fn every_refusal_is_named_and_runs_nothing() {
    let fixture = Fixture::new("analysis-refusals");
    let [asset, _] = fixture.assets.clone();
    let owner = fixture.start();
    let point = json!({"asset_id": asset, "point": [[0.5, 0.5]]});
    // The flag off and nothing installed.
    let error = owner.pick(point.clone()).unwrap_err();
    assert_eq!(error.code, "not-ready");
    let kinds = requirement_kinds(&error);
    assert!(kinds.contains(&"flag ai.analysis".to_owned()), "{kinds:?}");
    assert!(
        kinds.contains(&format!(
            "resource {}",
            FAKE_SEGMENTER_DECODER.file.resource
        )),
        "{kinds:?}"
    );
    let response = owner.call(owner.edit, ANALYSE, json!({"asset_id": asset}));
    let error = response.error.expect("refused");
    assert_eq!(error.code, "not-ready", "{error:?}");
    owner.analysis_flag(true);
    owner.install(&FAKE_SEGMENTER_ENCODER.file);
    owner.install(&FAKE_SEGMENTER_DECODER.file);
    // Malformed prompts: none, two, and a stroke without its radius.
    for params in [
        json!({"asset_id": asset}),
        json!({"asset_id": asset, "point": [[0.5, 0.5]], "box": [[0.1, 0.1], [0.9, 0.9]]}),
        json!({"asset_id": asset, "stroke": [[0.3, 0.5], [0.7, 0.5]]}),
    ] {
        let job = owner.task(ANALYSE, json!({"asset_id": asset}));
        assert_eq!(job["status"], "ready");
        let error = owner.pick(params.clone()).unwrap_err();
        assert_eq!(error.code, "validation", "{params}: {error:?}");
    }
    // Selecting a candidate the prompt did not answer fails the job by name and publishes nothing.
    let job = owner.task(
        SELECT,
        json!({"asset_id": asset, "point": [[0.5, 0.5]], "candidate": 63}),
    );
    assert_eq!(job["status"], "failed", "{job}");
    assert_eq!(job["error"]["code"], "validation", "{job}");
    owner.stop();
}

#[test]
#[slow]
fn scene_and_depth_analyses_run_once_their_models_are_installed() {
    let fixture = Fixture::new("analysis-scene");
    let [asset, _] = fixture.assets.clone();
    let owner = fixture.start();
    owner.analysis_flag(true);
    for model in [FAKE_SEMANTIC, FAKE_SUBJECT, FAKE_PEOPLE, FAKE_DEPTH] {
        owner.install(&model.file);
    }
    let scene = owner.task("task.analyse-scene", json!({"asset_id": asset}));
    assert_eq!(scene["status"], "ready", "{scene}");
    let result = &scene["result"]["result"];
    assert_eq!(result["cached"], false);
    let classes = result["classes"].as_array().unwrap();
    assert!(!classes.is_empty(), "{result}");
    let total: f64 = classes.iter().map(|c| c["area"].as_f64().unwrap()).sum();
    assert!(total <= 1.0001, "{result}");
    assert_eq!(result["models"].as_array().unwrap().len(), 3);
    let again = owner.task("task.analyse-scene", json!({"asset_id": asset}));
    assert_eq!(again["result"]["result"]["cached"], true);
    let depth = owner.task("task.analyse-depth", json!({"asset_id": asset}));
    assert_eq!(depth["status"], "ready", "{depth}");
    let range = &depth["result"]["result"]["range"];
    assert!(
        range["near"].is_number() && range["far"].is_number(),
        "{depth}"
    );
    owner.stop();
}

fn current_entry(owner: &Owner, asset: &AssetId) -> Value {
    owner.ok("asset.state", json!({"asset_id": asset}))["current_entry"]["id"].clone()
}

#[test]
#[slow]
fn a_pick_answers_about_the_current_entry_the_analysis_reads() {
    let fixture = Fixture::new("analysis-entries");
    let [asset, _] = fixture.assets.clone();
    let owner = segmenter_ready(&fixture);
    let point = json!({"asset_id": asset, "point": [[0.5, 0.5]]});
    // The refusal names the input the analysis must be of, and the analysis says which it is.
    let refused = owner.pick(point.clone()).unwrap_err();
    let data = refused.data.clone().unwrap();
    assert_eq!(data["task"], "task.analyse", "{data}");
    let job = owner.task(ANALYSE, json!({"asset_id": asset}));
    let analysed = &job["result"]["result"]["analysed"];
    assert_eq!(analysed, &data["analysis"], "{job}");
    // A correction moves the repair stage on; the entry before it is analysed no longer.
    let before = current_entry(&owner, &asset);
    let revision = owner.edit_revision(&asset);
    owner.ok(
        "edit.add-correction",
        json!({"asset_id": asset, "mutation": mutation_json(revision, "heal"),
               "points": [[0.1, 0.1], [0.12, 0.1]], "radius": 0.02,
               "source-x": 0.3, "source-y": 0.3}),
    );
    owner.task(ANALYSE, json!({"asset_id": asset}));
    let current = current_entry(&owner, &asset);
    assert_eq!(owner.pick(point.clone()).unwrap()["decision"], "one");
    // Previewing the entry before it, or naming it, is refused by name, never `not-ready`: the
    // analysis a task can run is of the current entry, so asking for it again would not help.
    owner.ok_as(
        owner.edit,
        "preview.select",
        json!({"asset_id": asset, "entry_id": before}),
    );
    for params in [
        point.clone(),
        with(point.clone(), json!({"entry_id": before})),
    ] {
        let error = owner.pick(params).unwrap_err();
        assert_eq!(error.code, "validation", "{error:?}");
        let data = error.data.unwrap();
        assert_eq!(data["current_entry_id"], current, "{data}");
        assert_eq!(data["entry_id"], before, "{data}");
    }
    // Naming the current entry answers while the session previews another.
    let named = owner
        .pick(with(point.clone(), json!({"entry_id": current})))
        .unwrap();
    assert_eq!(named["decision"], "one");
    owner.ok_as(owner.edit, "preview.return-current", json!({}));
    assert_eq!(owner.pick(point).unwrap(), named);
    owner.stop();
}

#[test]
#[slow]
fn the_query_worker_stops_whenever_its_models_are_released() {
    let fixture = Fixture::new("analysis-worker");
    let [asset, _] = fixture.assets.clone();
    let mut host = fixture.host(true);
    host.runtime_idle = Duration::from_millis(150);
    let owner = Owner::start(
        &fixture.root.join("catalog.sqlite"),
        registry(&fixture.gate),
        host,
    );
    owner.analysis_flag(true);
    owner.install(&FAKE_SEGMENTER_ENCODER.file);
    owner.install(&FAKE_SEGMENTER_DECODER.file);
    owner.task(ANALYSE, json!({"asset_id": asset}));
    let point = json!({"asset_id": asset, "point": [[0.5, 0.5]]});
    let worker = |owner: &Owner| owner.status(AI_MODULE)["runtime"]["query_worker"] == true;
    let held =
        |owner: &Owner| owner.status(AI_MODULE)["runtime"]["analysis_cache"]["held_bytes"].clone();
    // A pick starts the worker and leaves the cache its working copy of the embedding.
    owner.pick(point.clone()).unwrap();
    assert!(worker(&owner));
    assert_eq!(held(&owner), json!(4 << 20));
    // The idle unload releases the decoder and stops the worker; the analysis stays.
    wait_for("the idle decoder to stop the worker", || {
        Some(()).filter(|_| !worker(&owner))
    });
    owner.pick(point.clone()).unwrap();
    assert!(worker(&owner));
    // So does the memory signal, from another thread.
    assert!(owner.handle.memory_signal() > 0);
    assert!(!worker(&owner));
    owner.pick(point.clone()).unwrap();
    // And an unload of one of the module's models.
    owner.ok(
        "module.runtime.unload",
        json!({"module_id": AI_MODULE, "resource_id": FAKE_SEGMENTER_DECODER.file.resource}),
    );
    assert!(!worker(&owner));
    owner.pick(point).unwrap();
    // Removing a model stops it and forgets the analysis with its working copy.
    owner.ok(
        "module.resource.remove",
        json!({"module_id": AI_MODULE, "resource_id": FAKE_SEGMENTER_ENCODER.file.resource}),
    );
    assert!(!worker(&owner));
    assert_eq!(held(&owner), json!(0));
    owner.stop();
}

#[test]
#[slow]
fn admission_reads_install_markers_once_and_loads_only_under_the_flag() {
    let fixture = Fixture::new("analysis-admission");
    let [asset, _] = fixture.assets.clone();
    let owner = segmenter_ready(&fixture);
    owner.analysis_flag(false);
    // An analysis model loads only while its flag is on, as its tasks run only then.
    let load = json!({"module_id": AI_MODULE, "resource_id": FAKE_SEGMENTER_DECODER.file.resource});
    let refused = owner.fail("module.runtime.load", load.clone());
    assert_eq!(refused.code, "not-ready", "{refused:?}");
    assert_eq!(requirement_kinds(&refused), ["flag ai.analysis"]);
    owner.analysis_flag(true);
    owner.ok("module.runtime.load", load);
    owner.task(ANALYSE, json!({"asset_id": asset}));
    let point = json!({"asset_id": asset, "point": [[0.5, 0.5]]});
    owner.pick(point.clone()).unwrap();
    // A pick reads no marker: with the decoder's marker gone behind the editor's back, picks are
    // still admitted from what the host read before ...
    let marker = fixture
        .root
        .join("resources")
        .join(AI_MODULE)
        .join(FAKE_SEGMENTER_DECODER.file.resource)
        .join(FAKE_SEGMENTER_DECODER.file.version)
        .join(crate::capabilities::resources::INSTALLED_FILE);
    fs::remove_file(&marker).unwrap();
    for _ in 0..3 {
        owner.pick(point.clone()).unwrap();
    }
    // ... until a read of the disk, here the resource list, finds it gone.
    owner.ok("module.resource.list", json!({"module_id": AI_MODULE}));
    let error = owner.pick(point).unwrap_err();
    assert_eq!(
        requirement_kinds(&error),
        [format!("resource {}", FAKE_SEGMENTER_DECODER.file.resource)]
    );
    owner.stop();
}

#[test]
#[slow]
fn a_changed_original_is_never_analysed_or_selected_from() {
    let fixture = Fixture::new("analysis-changed");
    // A copy of a fixture the catalog does not hold yet, so the test can change it.
    let original = fixture.root.join("original.jpg");
    fs::copy(
        luxforge_testbase::paths::fixture("s0/orientation-3.jpg"),
        &original,
    )
    .unwrap();
    let asset = EditorService::open_with(
        &fixture.root.join("catalog.sqlite"),
        registry(&fixture.gate),
    )
    .unwrap()
    .import(&original)
    .unwrap()
    .asset
    .id;
    let owner = segmenter_ready(&fixture);
    let job = owner.task(ANALYSE, json!({"asset_id": asset}));
    assert_eq!(job["status"], "ready", "{job}");
    let point = json!({"asset_id": asset, "point": [[0.5, 0.5]]});
    assert_eq!(owner.pick(point.clone()).unwrap()["decision"], "one");
    // Other bytes at the original's path: every task reads the original through the host-planned
    // read, which verifies it, so none answers from the analysis of the bytes it had.
    fs::copy(
        luxforge_testbase::paths::fixture("s0/jpeg-scan-444-interleaved.jpg"),
        &original,
    )
    .unwrap();
    for (method, params) in [
        (ANALYSE, json!({"asset_id": asset})),
        (SELECT, point.clone()),
    ] {
        let response = owner.call(owner.edit, method, params);
        let error = match response.error {
            Some(error) if error.code == "preparation-required" => {
                let job = owner.finished(&json!(error.job_id.expect("a preparation job")));
                assert_eq!(job["status"], "failed", "{method}: {job}");
                job["error"]["code"].as_str().unwrap().to_owned()
            }
            Some(error) => error.code,
            None => panic!("{method} was queued over a changed original"),
        };
        assert_eq!(error, "source-unavailable", "{method}");
    }
    owner.stop();
}
