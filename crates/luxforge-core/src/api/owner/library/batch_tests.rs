//! Batch preset and export through the catalog owner, as clients call them: each batch against the
//! same N single calls (the entries, labels, actors and events a preset writes; the bytes and names
//! an export writes), every reason a photograph is left out, progress on `job.read` and the activity
//! board, a cancel between photographs and within an export keeping what was finished, and a retry
//! answered once, after a restart too. Every original is a copied fixture in a scratch directory.
use super::super::{OwnerMessage, catalog::CatalogMessage};
use super::LibraryMessage;
use crate::{
    AssetId, ModuleRegistry, RegistryOptions,
    api::{ApiFailure, ApiRequest, ApiResponse, ClientId, OwnerHandle},
    library::locate::Phase,
};
use luxforge_testbase::{
    Gate,
    paths::{fixture, temp_dir},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

/// The actor every client request here is made as.
const ACTOR: &str = "batch-client";

fn envelope(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": ACTOR})
}

fn assets(assets: &[AssetId]) -> Value {
    json!({"kind": "assets", "asset_ids": assets})
}

/// One owner over a catalog in its own scratch directory.
struct Harness {
    dir: PathBuf,
    catalog: PathBuf,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
}

impl Harness {
    fn new(name: &str) -> Self {
        let dir = temp_dir(&format!("batch-{name}")).canonicalize().unwrap();
        let catalog = dir.join("catalog.sqlite");
        Self::start(dir, catalog, ModuleRegistry::builtin())
    }

    fn start(dir: PathBuf, catalog: PathBuf, registry: ModuleRegistry) -> Self {
        let (owner, join) = OwnerHandle::start_with(&catalog, Arc::new(registry)).unwrap();
        let client = owner.register();
        Self {
            dir,
            catalog,
            owner,
            join: Some(join),
            client,
        }
    }

    fn stop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }

    /// The same catalog under a new owner serving `registry`: no session, no prepared source and no
    /// remembered answer survive.
    fn restart(mut self, registry: ModuleRegistry) -> Self {
        self.stop();
        let (dir, catalog) = (std::mem::take(&mut self.dir), self.catalog.clone());
        Self::start(dir, catalog, registry)
    }

    fn send(&self, method: &str, params: Value) -> ApiResponse {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        self.owner
            .call(
                self.client,
                ApiRequest {
                    id: format!("{method}-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        let response = self.send(method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn refused(&self, method: &str, params: Value) -> ApiFailure {
        self.send(method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to be refused"))
    }

    /// Answer `method`, retrying through every preparation it asks for, as a client does.
    fn prepared(&self, method: &str, params: Value) -> Value {
        for _ in 0..4 {
            let response = self.send(method, params.clone());
            match response.error {
                None => return response.result.expect("a result"),
                Some(error) if error.code == "preparation-required" => {
                    let job = json!(error.job_id.expect("a preparation names its job"));
                    assert_eq!(self.settle(&job)["status"], "ready");
                }
                Some(error) => panic!("{method}: {error:?}"),
            }
        }
        panic!("{method} kept asking for preparation");
    }

    /// Read a job until it leaves `queued` and `running`, as a client would.
    fn settle(&self, job: &Value) -> Value {
        luxforge_testbase::wait_for("a job to settle", || {
            let read = self.ok("job.read", json!({"job_id": job}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    /// A copy of the fixture `name` (`s0/…`) at `relative` in the scratch directory, imported.
    fn photograph(&self, name: &str, relative: &str) -> AssetId {
        let path = super::opening::distinct_copy(
            &fixture(&format!("s0/{name}")),
            &self.dir.join(relative),
        );
        serde_json::from_value(super::opening::import(&self.owner, self.client, &path)).unwrap()
    }

    fn revision(&self, asset: &AssetId) -> u64 {
        self.ok("asset.state", json!({"asset_id": asset}))["revision"]
            .as_u64()
            .unwrap()
    }

    /// The photograph's current entry, with its whole stack.
    fn current(&self, asset: &AssetId) -> Value {
        let state = self.ok("asset.state", json!({"asset_id": asset}));
        self.ok(
            "history.inspect",
            json!({"asset_id": asset, "entry_id": state["current_entry"]["id"]}),
        )
    }

    /// Commit a Basic exposure as the photograph's next entry.
    fn expose(&self, asset: &AssetId, exposure: f64) {
        let revision = self.revision(asset);
        self.ok(
            "edit.set-basic",
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": revision, "request_id": format!("expose-{asset}-{revision}"), "actor": ACTOR},
                "exposure": exposure,
            }),
        );
    }

    /// A library preset of `settings`, as `preset.read` answers it.
    fn preset(&self, name: &str, settings: Value) -> Value {
        self.ok(
            "preset.create",
            json!({"name": name, "settings": settings, "mutation": envelope(&format!("preset-{name}"))}),
        )["preset"]
            .clone()
    }

    /// `edit.apply-preset` of `preset` on one photograph, as a client sends it after reading the
    /// preset from the library.
    fn apply(&self, asset: &AssetId, preset: &Value, request_id: &str) -> ApiResponse {
        self.send(
            "edit.apply-preset",
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": self.revision(asset), "request_id": request_id, "actor": ACTOR},
                "settings": preset["settings"],
                "name": preset["name"],
                "preset-id": preset["id"],
            }),
        )
    }

    /// The newest event's sequence.
    fn sequence(&self) -> u64 {
        self.ok("events.since", json!({"after": 0}))["current_sequence"]
            .as_u64()
            .unwrap()
    }

    /// The events after `after` recorded under `method`.
    fn events(&self, after: u64, method: &str) -> Vec<Value> {
        self.ok("events.since", json!({"after": after}))["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["method"] == method)
            .cloned()
            .collect()
    }

    fn post(&self, message: LibraryMessage) {
        self.owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Library(message)))
            .unwrap();
    }

    /// Hold every library job dispatched from now on as it is about to take its `nth` photograph
    /// (from 1), until the gate opens.
    fn hold_before_photograph(&self, nth: usize) -> Arc<Gate> {
        let gate = Arc::new(Gate::new());
        gate.shut();
        let held = gate.clone();
        let reached = AtomicUsize::new(0);
        self.post(LibraryMessage::Hold(Some(Arc::new(move |phase| {
            if phase == Phase::NextPhotograph && reached.fetch_add(1, Ordering::SeqCst) + 1 == nth {
                held.pass();
            }
        }))));
        gate
    }

    /// Put a photograph in Removed, as removing it will.
    fn remove(&self, asset: &AssetId) {
        let asset = asset.clone();
        self.post(LibraryMessage::Run(Box::new(move |owner| {
            owner
                .service
                .connection
                .execute(
                    "UPDATE assets SET removed_ms = 1 WHERE id = ?1",
                    [asset.as_str()],
                )
                .unwrap();
        })));
    }

    /// The running activity of `job`, as `activity.list` answers it.
    fn activity(&self, job: &Value) -> Value {
        let listed = self.ok("activity.list", json!({}));
        listed["active"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["job_id"] == *job)
            .cloned()
            .unwrap_or_else(|| panic!("no activity for {job} in {listed}"))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop();
        if !self.dir.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// An entry's stack without its layers' identities, which every commit makes anew.
fn stack(entry: &Value) -> Value {
    let mut recipe = entry["snapshot"]["recipe"].clone();
    for layer in recipe["layers"].as_array_mut().unwrap() {
        layer.as_object_mut().unwrap().remove("id");
    }
    recipe
}

/// The names in `folder`, sorted.
fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Each photograph of a batch preset gets exactly the entry `edit.apply-preset` gives it: its
/// action, label, parameters, actor, revisions and stack, over an earlier edit too, with the
/// settings that do not apply to it reported as the single call reports them, and one event naming
/// it and its new revision. Its request identity names the batch.
#[test]
fn a_batch_preset_writes_each_photograph_the_entry_a_single_call_writes() {
    let harness = Harness::new("preset-equals");
    let fixtures = ["srgb.jpg", "portrait.jpg", "orientation-6.jpg"];
    let batch: Vec<AssetId> = fixtures
        .iter()
        .map(|name| harness.photograph(name, &format!("batch/{name}")))
        .collect();
    let single: Vec<AssetId> = fixtures
        .iter()
        .map(|name| harness.photograph(name, &format!("single/{name}")))
        .collect();
    harness.expose(&batch[1], 0.25);
    harness.expose(&single[1], 0.25);
    let preset = harness.preset(
        "Warm",
        json!({
            "set-basic": {"exposure": 0.5, "contrast": 12},
            "set-raw": {"white-balance": "as-shot"},
            "set-vignette": {"amount": -18},
        }),
    );

    let before = harness.sequence();
    let started = harness.ok(
        "batch.apply-preset",
        json!({"targets": assets(&batch), "preset_id": preset["id"], "mutation": envelope("batch-1")}),
    );
    assert_eq!(started["deduplicated"], json!(false));
    let settled = harness.settle(&started["job_id"]);
    assert_eq!(settled["status"], "ready", "{settled}");
    assert_eq!(settled["kind"], "batch-preset");
    let report = &settled["result"];
    assert_eq!(report["done"], json!(batch));
    assert_eq!(report["skipped"], json!([]));
    assert!(report.get("written").is_none(), "{report}");

    let answers: Vec<Value> = single
        .iter()
        .enumerate()
        .map(|(index, asset)| {
            let response = harness.apply(asset, &preset, &format!("single-{index}"));
            assert!(response.error.is_none(), "{:?}", response.error);
            response.result.unwrap()
        })
        .collect();
    for (index, (batched, alone)) in batch.iter().zip(&single).enumerate() {
        let (batched_entry, alone_entry) = (harness.current(batched), harness.current(alone));
        for field in [
            "action_id",
            "label",
            "parameters",
            "actor",
            "base_revision",
            "result_revision",
        ] {
            assert_eq!(batched_entry[field], alone_entry[field], "{field}");
        }
        assert_eq!(batched_entry["label"], "Preset: Warm");
        assert_eq!(batched_entry["actor"], ACTOR);
        assert_eq!(batched_entry["request_id"], format!("batch-1/{batched}"));
        assert_eq!(stack(&batched_entry), stack(&alone_entry), "{index}");
        assert_eq!(
            report["settings_skipped"][index],
            json!({"asset_id": batched, "settings": answers[index]["skipped"]}),
        );
        assert_eq!(
            answers[index]["skipped"][0]["action"], "set-raw",
            "a RAW development never applies to a JPEG"
        );
    }
    let events: Vec<(Value, Value)> = harness
        .events(before, "batch.apply-preset")
        .into_iter()
        .map(|event| (event["asset_id"].clone(), event["revision"].clone()))
        .collect();
    let mut expected: Vec<(Value, Value)> = batch
        .iter()
        .map(|asset| (json!(asset), json!(harness.revision(asset))))
        .collect();
    // Then the job's end, naming no photograph.
    expected.push((Value::Null, Value::Null));
    assert_eq!(events, expected);
}

/// A batch preset reports every photograph it leaves out, with a code and a reason, and changes
/// none of them: one in Removed, one with a draft open in the caller's session, one whose history
/// the caller previews, one that already has the preset's settings, one none of them apply to, and
/// one the single call refuses, in that call's words.
#[test]
fn a_batch_preset_reports_every_photograph_it_leaves_out() {
    let harness = Harness::new("preset-skips");
    let [applied, removed, drafted, previewed, unchanged] =
        ["Applied", "Removed", "Drafted", "Previewed", "Unchanged"]
            .map(|name| harness.photograph("srgb.jpg", &format!("photos/{name}.jpg")));
    let warm = harness.preset("Warm", json!({"set-basic": {"exposure": 0.5}}));
    let vignette = harness.preset("Vignette", json!({"set-vignette": {"amount": -20}}));
    let as_shot = harness.preset("As shot", json!({"set-raw": {"white-balance": "as-shot"}}));
    harness.remove(&removed);
    let draft = harness.ok(
        "draft.begin",
        json!({"asset_id": drafted, "action": "set-basic"}),
    );
    harness.expose(&previewed, 0.25);
    let original = harness.ok("history.list", json!({"asset_id": previewed}))["entries"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["id"]
        .clone();
    harness.ok(
        "preview.select",
        json!({"asset_id": previewed, "entry_id": original}),
    );
    let response = harness.apply(&unchanged, &warm, "first");
    assert!(response.error.is_none(), "{:?}", response.error);
    let revisions: Vec<u64> = [&removed, &drafted, &previewed, &unchanged]
        .map(|asset| harness.revision(asset))
        .to_vec();

    let targets = [&applied, &removed, &drafted, &previewed, &unchanged].map(Clone::clone);
    let started = harness.ok(
        "batch.apply-preset",
        json!({"targets": assets(&targets), "preset_id": warm["id"], "mutation": envelope("skips")}),
    );
    let settled = harness.settle(&started["job_id"]);
    assert_eq!(settled["status"], "ready", "{settled}");
    assert_eq!(settled["result"]["done"], json!([applied]));
    assert_eq!(
        settled["result"]["skipped"],
        json!([
            {"asset_id": removed, "code": "removed", "reason": "it is in Removed; put it back to include it"},
            {"asset_id": drafted, "code": "draft-open", "reason": "an unapplied set-basic draft is open on it; apply or cancel it first"},
            {"asset_id": previewed, "code": "history-selected", "reason": "its history is being previewed: return to current or restore the selected history entry before editing"},
            {"asset_id": unchanged, "code": "unchanged", "reason": "it already has Warm's settings"},
        ])
    );
    assert_eq!(
        [&removed, &drafted, &previewed, &unchanged]
            .map(|asset| harness.revision(asset))
            .to_vec(),
        revisions,
        "nothing left out changed"
    );
    assert_eq!(
        harness.ok("session.state", json!({}))["draft"]["draft_id"],
        draft["draft_id"],
        "the draft stays open"
    );

    // Every setting of a RAW-only preset is left out of a JPEG.
    let started = harness.ok(
        "batch.apply-preset",
        json!({"targets": assets(std::slice::from_ref(&applied)), "preset_id": as_shot["id"], "mutation": envelope("as-shot")}),
    );
    let settled = harness.settle(&started["job_id"]);
    let skipped = &settled["result"]["skipped"][0];
    assert_eq!(skipped["code"], "not-applicable", "{settled}");
    let reason = skipped["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("none of As shot's settings apply to it: "),
        "{reason}"
    );
    let alone = harness
        .apply(&applied, &as_shot, "as-shot-alone")
        .result
        .unwrap();
    assert_eq!(alone["outcome"], "no-op");
    assert!(
        reason.ends_with(alone["skipped"][0]["reason"].as_str().unwrap()),
        "{reason}"
    );

    // A refusal of the single call is reported in its words: the vignette's module is disabled.
    let harness = harness.restart(
        ModuleRegistry::assemble(&RegistryOptions {
            disabled: &["luxforge.vignette".to_owned()],
            ..RegistryOptions::default()
        })
        .unwrap(),
    );
    let refusal = harness
        .apply(&applied, &vignette, "vignette-alone")
        .error
        .unwrap();
    assert_eq!(refusal.code, "incompatible");
    let started = harness.ok(
        "batch.apply-preset",
        json!({"targets": assets(std::slice::from_ref(&applied)), "preset_id": vignette["id"], "mutation": envelope("vignette")}),
    );
    let settled = harness.settle(&started["job_id"]);
    assert_eq!(
        settled["result"]["skipped"],
        json!([{"asset_id": applied, "code": refusal.code, "reason": refusal.message}])
    );
    let unknown = harness.refused(
        "batch.apply-preset",
        json!({"targets": assets(std::slice::from_ref(&applied)), "preset_id": crate::PresetId::new(), "mutation": envelope("unknown")}),
    );
    assert_eq!(unknown.code, "validation");
}

/// While a batch preset runs, `job.read` and the activity board say how far it has got and what it
/// has done so far; a cancel between photographs keeps the entries already written and writes no
/// more.
#[test]
fn a_cancelled_batch_preset_keeps_the_photographs_it_finished_and_showed_its_progress() {
    let harness = Harness::new("preset-cancel");
    let photographs: Vec<AssetId> = ["One", "Two", "Three"]
        .iter()
        .map(|name| harness.photograph("srgb.jpg", &format!("photos/{name}.jpg")))
        .collect();
    let warm = harness.preset("Warm", json!({"set-basic": {"exposure": 0.5}}));
    let gate = harness.hold_before_photograph(2);
    let started = harness.ok(
        "batch.apply-preset",
        json!({"targets": assets(&photographs), "preset_id": warm["id"], "mutation": envelope("cancel")}),
    );
    let job = started["job_id"].clone();
    gate.wait_reached(1, "the batch's second photograph");

    let running = harness.ok("job.read", json!({"job_id": job}));
    assert_eq!(running["status"], "running");
    assert_eq!(running["progress"]["message"], "1 of 3", "{running}");
    let fraction = running["progress"]["fraction"].as_f64().unwrap();
    assert!((fraction - 1.0 / 3.0).abs() < 1e-9, "{fraction}");
    assert_eq!(
        running["result"],
        json!({"done": [photographs[0]], "skipped": []}),
        "the report so far"
    );
    let activity = harness.activity(&job);
    assert_eq!(activity["kind"], "batch.apply-preset");
    assert_eq!(activity["label"], "Applying preset");
    assert_eq!(activity["detail"], "Warm · 3 photographs");
    assert_eq!(activity["progress"]["message"], "1 of 3");

    harness.ok("job.cancel", json!({"job_id": job}));
    gate.open();
    let settled = harness.settle(&job);
    assert_eq!(settled["status"], "cancelled", "{settled}");
    assert_eq!(settled["progress"]["message"], "1 of 3");
    assert_eq!(
        photographs
            .iter()
            .map(|asset| harness.revision(asset))
            .collect::<Vec<_>>(),
        [1, 0, 0],
        "the finished photograph keeps its entry, and nothing more is written"
    );
    assert_eq!(harness.current(&photographs[0])["label"], "Preset: Warm");
}

/// A retry of a batch preset is answered with its first job, and after a restart, when the owner no
/// longer remembers it, each photograph is answered as its first attempt was: nothing is applied
/// twice and nothing is announced again. A batch export's retry is answered with its first job.
#[test]
fn a_retried_batch_is_answered_once() {
    let harness = Harness::new("retry");
    let photographs: Vec<AssetId> = ["One", "Two"]
        .iter()
        .map(|name| harness.photograph("srgb.jpg", &format!("photos/{name}.jpg")))
        .collect();
    let warm = harness.preset("Warm", json!({"set-basic": {"exposure": 0.5}}));
    let request = json!({"targets": assets(&photographs), "preset_id": warm["id"], "mutation": envelope("retry-1")});
    let first = harness.ok("batch.apply-preset", request.clone());
    assert_eq!(harness.settle(&first["job_id"])["status"], "ready");
    let sequence = harness.sequence();
    let retried = harness.ok("batch.apply-preset", request.clone());
    assert_eq!(retried["deduplicated"], json!(true));
    assert_eq!(retried["job_id"], first["job_id"]);
    assert_eq!(harness.sequence(), sequence, "the retry records nothing");

    let harness = harness.restart(ModuleRegistry::builtin());
    let sequence = harness.sequence();
    let again = harness.ok("batch.apply-preset", request);
    let settled = harness.settle(&again["job_id"]);
    assert_eq!(settled["status"], "ready", "{settled}");
    assert_eq!(settled["result"]["done"], json!(photographs));
    assert_eq!(
        photographs
            .iter()
            .map(|asset| harness.revision(asset))
            .collect::<Vec<_>>(),
        [1, 1],
        "applied once"
    );
    assert_eq!(
        harness.sequence(),
        sequence + 1,
        "nothing is announced again but the retry's job ending"
    );

    let out = harness.dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let export = json!({"targets": assets(&photographs), "destination": out, "mutation": envelope("export-1")});
    let first = harness.ok("batch.export", export.clone());
    assert_eq!(harness.settle(&first["job_id"])["status"], "ready");
    let retried = harness.ok("batch.export", export);
    assert_eq!(retried["deduplicated"], json!(true));
    assert_eq!(retried["job_id"], first["job_id"]);
    assert_eq!(names(&out), ["One-edited.jpg", "Two-edited.jpg"]);
}

/// A batch export writes each photograph's current entry byte for byte as `export.jpeg` writes it,
/// with and without metadata, preparing each original first, into one folder under the names the
/// export's rule gives them there: two originals of one name get two names, and a file already there
/// is never replaced. Each written file records an event, as a single export's does.
#[test]
fn a_batch_export_writes_the_files_single_exports_write_under_the_export_rule() {
    let harness = Harness::new("export-equals");
    let photographs = [
        harness.photograph("srgb.jpg", "card-a/DSC_0001.jpg"),
        harness.photograph("portrait.jpg", "card-b/DSC_0001.jpg"),
        harness.photograph("orientation-6.jpg", "card-a/Lake.jpg"),
    ];
    harness.expose(&photographs[1], 0.5);
    let out = harness.dir.join("out");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("Lake-edited.jpg"), b"taken").unwrap();
    // A new owner, so no original is prepared and each is prepared through the owner's one path.
    let harness = harness.restart(ModuleRegistry::builtin());

    let before = harness.sequence();
    let started = harness.ok(
        "batch.export",
        json!({"targets": assets(&photographs), "destination": out, "mutation": envelope("export")}),
    );
    let settled = harness.settle(&started["job_id"]);
    assert_eq!(settled["status"], "ready", "{settled}");
    assert_eq!(settled["kind"], "batch-export");
    let written = [
        out.join("DSC_0001-edited.jpg"),
        out.join("DSC_0001-edited-2.jpg"),
        out.join("Lake-edited-2.jpg"),
    ];
    assert_eq!(
        settled["result"],
        json!({"done": photographs, "written": written, "skipped": []})
    );
    assert_eq!(fs::read(out.join("Lake-edited.jpg")).unwrap(), b"taken");
    assert_eq!(
        names(&out),
        [
            "DSC_0001-edited-2.jpg",
            "DSC_0001-edited.jpg",
            "Lake-edited-2.jpg",
            "Lake-edited.jpg"
        ],
        "no temporary file is left"
    );
    assert_eq!(
        harness.events(before, "batch.export").len(),
        4,
        "one event a file written, then the job's end"
    );

    let singles = harness.dir.join("singles");
    fs::create_dir_all(&singles).unwrap();
    for (index, (asset, path)) in photographs.iter().zip(&written).enumerate() {
        let destination = singles.join(format!("{index}.jpg"));
        let started = harness.prepared(
            "export.jpeg",
            json!({"asset_id": asset, "destination": destination, "mutation": envelope(&format!("single-{index}"))}),
        );
        assert_eq!(harness.settle(&started["job_id"])["status"], "ready");
        assert!(
            fs::read(&destination).unwrap() == fs::read(path).unwrap(),
            "{} differs from its single export",
            path.display()
        );
    }

    // With the original's metadata, as a single export keeps it.
    let kept = harness.dir.join("kept");
    fs::create_dir_all(&kept).unwrap();
    let started = harness.ok(
        "batch.export",
        json!({"targets": assets(&photographs[..1]), "destination": kept, "keep_metadata": true, "mutation": envelope("kept")}),
    );
    assert_eq!(harness.settle(&started["job_id"])["status"], "ready");
    let destination = singles.join("kept.jpg");
    let started = harness.prepared(
        "export.jpeg",
        json!({"asset_id": photographs[0], "destination": destination, "keep_metadata": true, "mutation": envelope("single-kept")}),
    );
    assert_eq!(harness.settle(&started["job_id"])["status"], "ready");
    let batched = fs::read(kept.join("DSC_0001-edited.jpg")).unwrap();
    assert!(batched == fs::read(&destination).unwrap());
    assert!(
        batched != fs::read(&written[0]).unwrap(),
        "the metadata is kept"
    );
}

/// A batch export reports every photograph it leaves out, with the code and words the single export
/// refuses it with, and writes the rest: one whose preparation fails, one in Removed, one whose
/// original is missing, and one whose every name is taken in the folder. A folder that is not one is
/// refused before anything starts.
#[test]
fn a_batch_export_reports_every_photograph_it_leaves_out() {
    let harness = Harness::new("export-skips");
    let changed = harness.photograph("orientation-8.jpg", "photos/Changed.jpg");
    let exported = harness.photograph("srgb.jpg", "photos/Keep.jpg");
    let removed = harness.photograph("portrait.jpg", "photos/Removed.jpg");
    let missing = harness.photograph("orientation-3.jpg", "photos/Gone.jpg");
    let crowded = harness.photograph("orientation-6.jpg", "photos/Full.jpg");
    harness.remove(&removed);
    fs::remove_file(harness.dir.join("photos/Gone.jpg")).unwrap();
    let out = harness.dir.join("out");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("Full-edited.jpg"), b"taken").unwrap();
    for n in 2..=64 {
        fs::write(out.join(format!("Full-edited-{n}.jpg")), b"taken").unwrap();
    }
    let gone = harness.refused(
        "export.jpeg",
        json!({"asset_id": missing, "destination": harness.dir.join("gone.jpg"), "mutation": envelope("gone")}),
    );
    assert_eq!(gone.code, "source-unavailable");
    // Other bytes of the same length written in place: the one look every read of an original
    // starts from passes, and preparing it fails, which is the single export's answer.
    let path = harness.dir.join("photos/Changed.jpg");
    let mut bytes = fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    fs::write(&path, &bytes).unwrap();
    let preparing = harness
        .refused(
            "export.jpeg",
            json!({"asset_id": changed, "destination": harness.dir.join("changed.jpg"), "mutation": envelope("changed")}),
        );
    assert_eq!(preparing.code, "preparation-required");
    let failed = harness.settle(&json!(preparing.job_id.unwrap()));
    assert_eq!(failed["status"], "failed", "{failed}");

    let targets = [&changed, &exported, &removed, &missing, &crowded].map(Clone::clone);
    let started = harness.ok(
        "batch.export",
        json!({"targets": assets(&targets), "destination": out, "mutation": envelope("skips")}),
    );
    let settled = harness.settle(&started["job_id"]);
    assert_eq!(settled["status"], "ready", "{settled}");
    let report = &settled["result"];
    assert_eq!(report["done"], json!([exported]));
    assert_eq!(report["written"], json!([out.join("Keep-edited.jpg")]));
    let skipped = report["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 4, "{report}");
    assert_eq!(
        skipped[0],
        json!({"asset_id": changed, "code": failed["error"]["code"], "reason": failed["error"]["message"]})
    );
    assert_eq!(
        skipped[1],
        json!({"asset_id": removed, "code": "removed", "reason": "it is in Removed; put it back to include it"})
    );
    assert_eq!(
        skipped[2],
        json!({"asset_id": missing, "code": gone.code, "reason": gone.message})
    );
    assert_eq!(skipped[3]["asset_id"], json!(crowded));
    assert_eq!(skipped[3]["code"], "conflict");
    assert_eq!(names(&out).len(), 65, "only Keep-edited.jpg was added");
    assert!(fs::read(out.join("Keep-edited.jpg")).is_ok());

    let file = harness.refused(
        "batch.export",
        json!({"targets": assets(&targets), "destination": out.join("Keep-edited.jpg"), "mutation": envelope("file")}),
    );
    assert_eq!(file.code, "validation");
    let relative = harness.refused(
        "batch.export",
        json!({"targets": assets(&targets), "destination": "out", "mutation": envelope("relative")}),
    );
    assert_eq!(relative.code, "validation");
    let absent = harness.refused(
        "batch.export",
        json!({"targets": assets(&targets), "destination": harness.dir.join("absent"), "mutation": envelope("absent")}),
    );
    assert_eq!(absent.code, "read-error");
}

/// A batch export cancelled while it encodes its second photograph keeps the first file, removes
/// the second's temporary file and writes nothing more; while it runs, `job.read` and the activity
/// board show its progress and the phase of the photograph being exported.
#[test]
fn a_cancelled_batch_export_keeps_the_files_it_wrote_and_removes_its_temporary_file() {
    let harness = Harness::new("export-cancel");
    let photographs: Vec<AssetId> = ["One", "Two", "Three"]
        .iter()
        .map(|name| harness.photograph("srgb.jpg", &format!("photos/{name}.jpg")))
        .collect();
    let out = harness.dir.join("out");
    fs::create_dir_all(&out).unwrap();
    let gate = Arc::new(Gate::new());
    gate.shut();
    let held = gate.clone();
    let encoded = AtomicUsize::new(0);
    harness
        .owner
        .hold_exports(Some(Arc::new(move |phase: &'static str| {
            if phase == "encoding" && encoded.fetch_add(1, Ordering::SeqCst) == 1 {
                held.pass();
            }
        })));
    let started = harness.ok(
        "batch.export",
        json!({"targets": assets(&photographs), "destination": out, "mutation": envelope("cancel")}),
    );
    let job = started["job_id"].clone();
    gate.wait_reached(1, "the second photograph's encoding");

    let running = harness.ok("job.read", json!({"job_id": job}));
    assert_eq!(running["status"], "running");
    assert_eq!(running["progress"]["message"], "1 of 3", "{running}");
    assert_eq!(
        running["result"],
        json!({"done": [photographs[0]], "written": [out.join("One-edited.jpg")], "skipped": []})
    );
    let activity = harness.activity(&job);
    assert_eq!(activity["kind"], "batch.export");
    assert_eq!(activity["phase"], "encoding");
    assert_eq!(activity["detail"], "3 photographs to out");
    let staged = names(&out);
    assert_eq!(staged.len(), 2, "{staged:?}");
    assert!(
        staged[0].starts_with(".Two-edited.jpg.") && staged[0].ends_with(".luxforge-export"),
        "{staged:?}"
    );

    harness.ok("job.cancel", json!({"job_id": job}));
    gate.open();
    let settled = harness.settle(&job);
    assert_eq!(settled["status"], "cancelled", "{settled}");
    assert_eq!(
        names(&out),
        ["One-edited.jpg"],
        "the first file stays and the temporary file is gone"
    );
    harness.owner.hold_exports(None);
}
