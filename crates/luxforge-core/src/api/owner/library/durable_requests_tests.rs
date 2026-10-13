//! Public request identity across owner restarts, eviction, deletion and asynchronous admission.
use super::{
    super::{OwnerMessage, catalog::CatalogMessage},
    LibraryMessage,
};
use crate::{
    AssetId, EditorService, ModuleRegistry,
    api::{ApiRequest, ApiResponse, ClientAuthority, ClientId, OwnerHandle},
};
use luxforge_test_macros::slow;
use luxforge_testbase::{
    Gate,
    paths::{jpeg, temp_dir},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

fn mutation(id: &str) -> Value {
    json!({"request_id":id,"actor":"durable-client"})
}
fn targets(assets: &[AssetId]) -> Value {
    json!({"kind":"assets","asset_ids":assets})
}

struct Harness {
    dir: PathBuf,
    catalog: PathBuf,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    admin: ClientId,
    assets: Vec<AssetId>,
    originals: Vec<PathBuf>,
}
impl Harness {
    fn new(name: &str, count: usize) -> Self {
        let dir = temp_dir(&format!("durable-{name}")).canonicalize().unwrap();
        let catalog = dir.join("catalog.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let originals: Vec<_> = (0..count)
            .map(|n| super::opening::distinct_copy(&jpeg(), &dir.join(format!("photos/{n}.jpg"))))
            .collect();
        let assets = originals
            .iter()
            .map(|path| service.import(path).unwrap().asset.id)
            .collect();
        drop(service);
        Self::start(dir, catalog, assets, originals)
    }
    fn start(
        dir: PathBuf,
        catalog: PathBuf,
        assets: Vec<AssetId>,
        originals: Vec<PathBuf>,
    ) -> Self {
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(ModuleRegistry::builtin())).unwrap();
        let client = owner.register();
        let admin = owner.register_with(ClientAuthority::Permissions);
        Self {
            dir,
            catalog,
            owner,
            join: Some(join),
            client,
            admin,
            assets,
            originals,
        }
    }
    fn stop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
    fn restart(mut self) -> Self {
        self.stop();
        let dir = std::mem::take(&mut self.dir);
        Self::start(
            dir,
            self.catalog.clone(),
            self.assets.clone(),
            self.originals.clone(),
        )
    }
    fn send_as(&self, client: ClientId, method: &str, params: Value) -> ApiResponse {
        self.owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .unwrap()
    }
    fn send(&self, method: &str, params: Value) -> ApiResponse {
        self.send_as(self.client, method, params)
    }
    fn ok(&self, method: &str, params: Value) -> Value {
        let response = self.send(method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.unwrap()
    }
    fn empty(&self, id: &str) -> Value {
        let response = self.send_as(
            self.admin,
            "catalog.empty-removed",
            json!({"mutation":mutation(id)}),
        );
        assert!(response.error.is_none(), "{:?}", response.error);
        response.result.unwrap()
    }
    fn remove(&self, index: usize, id: &str) {
        self.ok(
            "asset.remove",
            json!({"targets":targets(&[self.assets[index].clone()]),"mutation":mutation(id)}),
        );
    }
    fn revision(&self, index: usize) -> Value {
        self.ok("asset.state", json!({"asset_id":self.assets[index]}))["revision"].clone()
    }
    fn settle(&self, job: &Value) -> Value {
        luxforge_testbase::wait_for("durable request job", || {
            let read = self.ok("job.read", json!({"job_id":job}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }
    fn on_owner<T: Send + 'static>(
        &self,
        run: impl FnOnce(&mut super::super::Owner) -> T + Send + 'static,
    ) -> T {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        self.owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Library(
                LibraryMessage::Run(Box::new(move |owner| {
                    let _ = reply.send(run(owner));
                })),
            )))
            .unwrap();
        answer.recv().unwrap()
    }
    fn hold(&self, nth: usize) -> Arc<Gate> {
        let gate = Arc::new(Gate::new());
        gate.shut();
        let held = gate.clone();
        let n = AtomicUsize::new(0);
        self.owner
            .sender
            .send(OwnerMessage::Catalog(CatalogMessage::Library(
                LibraryMessage::Hold(Some(Arc::new(move |phase| {
                    if phase == crate::library::locate::Phase::NextPhotograph
                        && n.fetch_add(1, Ordering::SeqCst) + 1 == nth
                    {
                        held.pass();
                    }
                }))),
            )))
            .unwrap();
        gate
    }
    fn commit_draft(&self, id: &str) -> (Value, Value) {
        let draft = self.ok(
            "draft.begin",
            json!({"asset_id":self.assets[0],"action":"set-basic"}),
        );
        let params = json!({"draft_id":draft["draft_id"],"mutation":{"expected_revision":self.revision(0),"request_id":id,"actor":"durable-client"}});
        let answer = self.ok("draft.commit", params.clone());
        (params, answer)
    }
    fn inline_batch(&self, id: &str) -> Value {
        json!({"targets":targets(&self.assets),"settings":{"set-basic":{"exposure":0.5}},"origin":{"kind":"preset","name":"Frozen","preset_id":null},"mutation":mutation(id)})
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.stop();
        if !self.dir.as_os_str().is_empty() {
            std::fs::remove_dir_all(&self.dir).unwrap();
        }
    }
}

#[test]
#[slow]
fn durable_requests_noop_navigation_cannot_consume_new_work_after_restart() {
    let h = Harness::new("noop-navigation", 0);
    let undo = json!({"mutation":mutation("empty-undo")});
    let redo = json!({"mutation":mutation("empty-redo")});
    let first = h.ok("library.undo", undo.clone());
    assert_eq!(first["outcome"], "no-op");
    assert_eq!(h.ok("library.redo", redo.clone())["outcome"], "no-op");
    let folder = h.ok(
        "folder.create",
        json!({"name":"Keep","mutation":mutation("folder")}),
    );
    let h = h.restart();
    let mut expected = first;
    expected["deduplicated"] = json!(true);
    assert_eq!(h.ok("library.undo", undo), expected);
    assert_eq!(
        h.ok("folder.list", json!({}))["folders"][0]["id"],
        folder["folder"]["id"]
    );
    h.ok("library.undo", json!({"mutation":mutation("new-undo")}));
    assert_eq!(h.ok("library.redo", redo)["outcome"], "no-op");
    assert!(
        h.ok("folder.list", json!({}))["folders"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        h.ok("library.redo", json!({"mutation":mutation("new-redo")}))["outcome"],
        "applied"
    );
}

#[test]
#[slow]
fn durable_requests_survive_owner_cache_eviction_and_match_input_before_targets() {
    let h = Harness::new("eviction", 1);
    let first = json!({"mutation":mutation("undo")});
    h.ok("library.undo", first.clone());
    h.ok(
        "folder.create",
        json!({"name":"Keep","mutation":mutation("folder")}),
    );
    let (old_owner, _) = h.commit_draft("old-cache");
    for n in 0..260 {
        h.commit_draft(&format!("evict-{n}"));
    }
    assert_eq!(h.on_owner(|owner| owner.requests.len()), 256);
    assert!(
        h.on_owner(move |owner| owner
            .requests
            .answered(&super::super::requests::RequestKey::of("draft.commit", &old_owner).unwrap())
            .unwrap()
            .is_none()),
        "Owner requests must actually evict the older cached answer"
    );
    assert_eq!(h.ok("library.undo", first.clone())["outcome"], "no-op");
    let changed = h.send("library.redo", first);
    assert_eq!(changed.error.unwrap().code, "conflict");
    assert_eq!(h.ok("folder.list", json!({}))["folders"][0]["name"], "Keep");
}

#[test]
#[slow]
fn durable_requests_empty_removed_retains_first_counts_and_originals() {
    use sha2::{Digest, Sha256};
    let h = Harness::new("empty-removed", 2);
    let hashes: Vec<_> = h
        .originals
        .iter()
        .map(|p| Sha256::digest(std::fs::read(p).unwrap()))
        .collect();
    h.remove(0, "remove-a");
    let first = h.empty("empty");
    assert_eq!(first["deleted"], 1);
    h.remove(1, "remove-b");
    let h = h.restart();
    let retry = h.empty("empty");
    assert_eq!(retry["deleted"], 1);
    assert_eq!(retry["remaining"], first["remaining"]);
    assert_eq!(retry["deduplicated"], true);
    assert_eq!(
        h.send_as(
            h.client,
            "catalog.empty-removed",
            json!({"mutation":mutation("empty")})
        )
        .error
        .unwrap()
        .code,
        "forbidden"
    );
    assert_eq!(
        h.empty("new-empty")["deleted"],
        1,
        "B must remain removed until a new request"
    );
    for (path, hash) in h.originals.iter().zip(hashes) {
        assert_eq!(Sha256::digest(std::fs::read(path).unwrap()), hash);
    }
}

#[test]
#[slow]
fn durable_requests_empty_removed_noop_stays_noop_after_later_removal() {
    let h = Harness::new("empty-noop", 1);
    assert_eq!(h.empty("empty")["deleted"], 0);
    h.remove(0, "remove");
    let h = h.restart();
    assert_eq!(h.empty("empty")["deleted"], 0);
    assert_eq!(h.empty("next")["deleted"], 1);
}

#[test]
#[slow]
fn durable_requests_create_replays_original_metadata_after_delete_and_refuses_other_input() {
    let h = Harness::new("create", 0);
    for (family, extra) in [
        ("folder", json!({})),
        ("collection", json!({"kind":"collection"})),
    ] {
        let mut params = extra;
        params["name"] = json!("Original");
        params["mutation"] = mutation("create");
        let first = h.ok(&format!("{family}.create"), params.clone());
        h.ok(&format!("{family}.rename"),json!({format!("{family}_id"):first[family]["id"],"name":"Later","mutation":mutation("rename")}));
        h.ok(
            &format!("{family}.delete"),
            json!({format!("{family}_id"):first[family]["id"],"mutation":mutation("delete")}),
        );
        let retry = h.ok(&format!("{family}.create"), params.clone());
        assert_eq!(retry[family], first[family]);
        assert_eq!(retry["change"]["deduplicated"], true);
        params["name"] = json!("Other");
        assert_eq!(
            h.send(&format!("{family}.create"), params)
                .error
                .unwrap()
                .code,
            "conflict"
        );
    }
    let h = h.restart();
    assert_eq!(
        h.send(
            "folder.create",
            json!({"name":"Different","mutation":mutation("create")})
        )
        .error
        .unwrap()
        .code,
        "conflict"
    );
}

#[test]
#[slow]
fn durable_requests_version_delete_does_not_delete_a_recreated_name() {
    let h = Harness::new("version", 1);
    let asset = &h.assets[0];
    h.ok(
        "version.create",
        json!({"asset_id":asset,"name":"Saved","mutation":mutation("create")}),
    );
    let delete = json!({"asset_id":asset,"name":"Saved","mutation":mutation("delete")});
    h.ok("version.delete", delete.clone());
    h.ok(
        "version.create",
        json!({"asset_id":asset,"name":"Saved","mutation":mutation("recreate")}),
    );
    let h = h.restart();
    assert_eq!(h.ok("version.delete", delete)["deduplicated"], true);
    assert_eq!(
        h.ok("version.list", json!({"asset_id":h.assets[0]}))["versions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
#[slow]
fn durable_requests_preset_update_returns_first_answer_without_overwriting_newer_settings() {
    let h = Harness::new("preset", 0);
    let preset=h.ok("preset.create",json!({"name":"Initial","settings":{"set-basic":{"exposure":0.1}},"mutation":mutation("create")}));
    let update =
        json!({"preset_id":preset["preset"]["id"],"name":"First","mutation":mutation("update")});
    let first = h.ok("preset.update", update.clone());
    h.ok(
        "preset.update",
        json!({"preset_id":preset["preset"]["id"],"name":"Later","mutation":mutation("later")}),
    );
    let h = h.restart();
    let retry = h.ok("preset.update", update);
    assert_eq!(retry["preset"], first["preset"]);
    assert_eq!(
        h.ok("preset.read", json!({"preset_id":preset["preset"]["id"]}))["preset"]["name"],
        "Later"
    );
}

#[test]
#[slow]
fn durable_requests_batch_completed_replays_without_resolving_deleted_preset_or_assets() {
    let h = Harness::new("batch-complete", 1);
    let preset=h.ok("preset.create",json!({"name":"Warm","settings":{"set-basic":{"exposure":0.5}},"mutation":mutation("preset")}));
    let params = json!({"targets":targets(&h.assets),"preset_id":preset["preset"]["id"],"mutation":mutation("batch")});
    let started = h.ok("batch.apply-settings", params.clone());
    let result = h.settle(&started["job_id"]);
    assert_eq!(result["status"], "ready");
    h.ok(
        "preset.delete",
        json!({"preset_id":preset["preset"]["id"],"mutation":mutation("delete")}),
    );
    h.remove(0, "remove");
    h.empty("empty");
    let h = h.restart();
    let retry = h.ok("batch.apply-settings", params.clone());
    assert_eq!(retry["deduplicated"], true);
    assert_eq!(h.settle(&retry["job_id"])["result"], result["result"]);
    let mut other = params;
    other["targets"] = json!({"kind":"selection"});
    assert_eq!(
        h.send("batch.apply-settings", other).error.unwrap().code,
        "conflict"
    );
}

#[test]
#[slow]
fn durable_requests_interrupted_batch_preserves_partial_commits_and_requires_new_id() {
    for nth in [1, 2] {
        let h = Harness::new(&format!("interrupt-{nth}"), 2);
        let before = [h.revision(0), h.revision(1)];
        let gate = h.hold(nth);
        let params = h.inline_batch("batch");
        let first = h.ok("batch.apply-settings", params.clone());
        gate.wait_reached(1, "interrupted batch");
        assert_eq!(
            h.ok("batch.apply-settings", params.clone())["job_id"],
            first["job_id"]
        );
        let h = h.restart();
        gate.open();
        let retry = h.ok("batch.apply-settings", params);
        let ended = h.settle(&retry["job_id"]);
        assert_eq!(ended["status"], "cancelled");
        assert_eq!(ended["error"]["data"]["reason"], "interrupted");
        assert_eq!(h.revision(1), before[1]);
        if nth == 1 {
            assert_eq!(h.revision(0), before[0]);
        } else {
            assert!(h.revision(0).as_u64().unwrap() > before[0].as_u64().unwrap());
        }
        let fresh = h.ok("batch.apply-settings", h.inline_batch("new-batch"));
        assert_eq!(h.settle(&fresh["job_id"])["status"], "ready");
    }
}

#[test]
#[slow]
fn durable_requests_queued_cancel_and_worker_panic_are_terminal_after_restart() {
    let h = Harness::new("cancel", 2);
    let gate = h.hold(1);
    let params = h.inline_batch("running");
    let running = h.ok("batch.apply-settings", params);
    gate.wait_reached(1, "held running batch");
    let queued_params = h.inline_batch("queued");
    let queued = h.ok("batch.apply-settings", queued_params.clone());
    h.ok("job.cancel", json!({"job_id":queued["job_id"]}));
    h.ok("job.cancel", json!({"job_id":running["job_id"]}));
    gate.open();
    assert_eq!(h.settle(&running["job_id"])["status"], "cancelled");
    let h = h.restart();
    let replay = h.ok("batch.apply-settings", queued_params);
    assert_eq!(h.settle(&replay["job_id"])["status"], "cancelled");
    h.owner
        .sender
        .send(OwnerMessage::Catalog(CatalogMessage::Library(
            LibraryMessage::Hold(Some(Arc::new(|_| panic!("test worker panic")))),
        )))
        .unwrap();
    let params = h.inline_batch("panic");
    let started = h.ok("batch.apply-settings", params.clone());
    assert_eq!(h.settle(&started["job_id"])["error"]["code"], "internal");
    let h = h.restart();
    let replay = h.ok("batch.apply-settings", params);
    assert_eq!(h.settle(&replay["job_id"])["error"]["code"], "internal");
}

#[test]
#[slow]
fn durable_requests_batch_child_collision_is_refused_without_using_an_unrelated_answer() {
    let h = Harness::new("child-collision", 1);
    let request = crate::library::batch::request_id("batch", &h.assets[0]);
    h.ok("edit.set-basic",json!({"asset_id":h.assets[0],"exposure":0.1,"mutation":{"expected_revision":h.revision(0),"actor":"durable-client","request_id":request}}));
    let before = h.revision(0);
    let started = h.ok("batch.apply-settings", h.inline_batch("batch"));
    let ended = h.settle(&started["job_id"]);
    assert_eq!(ended["result"]["skipped"][0]["code"], "conflict");
    assert_eq!(h.revision(0), before);
}

#[test]
#[slow]
fn durable_requests_failed_result_insert_rolls_back_tree_mutation_and_events() {
    let h = Harness::new("rollback-tree", 0);
    h.on_owner(|owner| owner.service.connection.execute_batch("CREATE TRIGGER durable_insert_failure BEFORE INSERT ON catalog_requests BEGIN SELECT RAISE(ABORT,'injected durable insert failure'); END;").unwrap());
    let params = json!({"name":"Rolled back","mutation":mutation("create")});
    assert_eq!(
        h.send("folder.create", params.clone()).error.unwrap().code,
        "catalog"
    );
    assert!(
        h.ok("folder.list", json!({}))["folders"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        h.ok("events.since", json!({"after":0}))["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        h.on_owner(|owner| owner
            .service
            .connection
            .query_row("SELECT COUNT(*) FROM library_changes", [], |row| row
                .get::<_, i64>(0))
            .unwrap()),
        0
    );
    h.on_owner(|owner| {
        owner
            .service
            .connection
            .execute_batch("DROP TRIGGER durable_insert_failure")
            .unwrap()
    });
    assert_eq!(h.ok("folder.create", params)["deduplicated"], false);
}

#[test]
#[slow]
fn durable_requests_failed_empty_removed_result_preserves_records_and_deletion_triggers() {
    let h = Harness::new("rollback-empty", 1);
    h.remove(0, "remove");
    let triggers: Vec<String> = h.on_owner(|owner| {
        owner
            .service
            .connection
            .prepare("SELECT name FROM sqlite_master WHERE type='trigger' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    });
    h.on_owner(|owner| owner.service.connection.execute_batch("CREATE TRIGGER durable_insert_failure BEFORE INSERT ON catalog_requests BEGIN SELECT RAISE(ABORT,'injected durable insert failure'); END;").unwrap());
    let params = json!({"mutation":mutation("empty")});
    assert_eq!(
        h.send_as(h.admin, "catalog.empty-removed", params)
            .error
            .unwrap()
            .code,
        "catalog"
    );
    assert_eq!(
        h.on_owner(|owner| owner
            .service
            .connection
            .query_row(
                "SELECT COUNT(*) FROM assets WHERE removed_ms IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap()),
        1
    );
    assert!(
        h.send("asset.state", json!({"asset_id":h.assets[0]}))
            .error
            .is_none()
    );
    h.on_owner(|owner| {
        owner
            .service
            .connection
            .execute_batch("DROP TRIGGER durable_insert_failure")
            .unwrap()
    });
    let after: Vec<String> = h.on_owner(|owner| {
        owner
            .service
            .connection
            .prepare("SELECT name FROM sqlite_master WHERE type='trigger' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    });
    assert_eq!(after, triggers);
    assert_eq!(h.empty("empty")["deleted"], 1);
}

#[test]
#[slow]
fn durable_requests_locate_completed_answer_survives_permanent_target_deletion() {
    let h = Harness::new("locate-replay", 1);
    let moved = h.dir.join("moved.jpg");
    std::fs::rename(&h.originals[0], &moved).unwrap();
    let params = json!({"asset_id":h.assets[0],"path":moved,"mutation":mutation("locate")});
    let first = h.ok("source.locate", params.clone());
    let settled = h.settle(&first["job_id"]);
    assert_eq!(settled["status"], "ready");
    h.remove(0, "remove");
    h.empty("empty");
    let h = h.restart();
    let replay = h.ok("source.locate", params);
    let result = h.settle(&replay["job_id"]);
    assert_eq!(result["status"], "ready");
    assert_eq!(result["result"]["deduplicated"], true);
    assert_eq!(result["result"]["change"], settled["result"]["change"]);
}

#[test]
#[slow]
fn durable_requests_collect_completed_reconstructs_a_readable_source_job() {
    let h = Harness::new("collect-complete", 0);
    let params = json!({"mutation":mutation("collect")});
    let first = h.ok("artifact.collect", params.clone());
    let settled = h.settle(&first["job_id"]);
    assert_eq!(settled["status"], "ready");
    let h = h.restart();
    let replay = h.ok("artifact.collect", params);
    assert_eq!(replay["deduplicated"], true);
    let settled_again = h.settle(&replay["job_id"]);
    assert_eq!(settled_again["status"], "ready");
    assert_eq!(settled_again["result"], settled["result"]);
}

#[test]
#[slow]
fn durable_requests_collect_live_retry_joins_new_client_and_interruption_never_recollects() {
    let mut h = Harness::new("collect-interrupted", 0);
    h.stop();
    let gate = Arc::new(Gate::new());
    gate.shut();
    let held = gate.clone();
    let (owner, join) =
        OwnerHandle::start_holding_sources(&h.catalog, Arc::new(move || held.pass())).unwrap();
    h.owner = owner;
    h.join = Some(join);
    h.client = h.owner.register();
    h.admin = h.owner.register_with(ClientAuthority::Permissions);
    struct OpenOnDrop(Arc<Gate>);
    impl Drop for OpenOnDrop {
        fn drop(&mut self) {
            self.0.open();
        }
    }
    let _cleanup = OpenOnDrop(gate.clone());
    let params = json!({"mutation":mutation("collect")});
    let first = h.ok("artifact.collect", params.clone());
    gate.wait_reached(1, "held explicit collection");
    let other = h.owner.register();
    let replay = h.send_as(other, "artifact.collect", params.clone());
    assert!(replay.error.is_none(), "{replay:?}");
    assert_eq!(replay.result.unwrap()["job_id"], first["job_id"]);
    assert!(
        h.send_as(other, "job.read", json!({"job_id":first["job_id"]}))
            .error
            .is_none()
    );
    // Stop is enqueued before releasing the worker; its completion cannot be accepted after it.
    // Source workers are joined on shutdown, so release the hold before joining the old owner.
    h.owner.stop();
    gate.open();
    let h = h.restart();
    let replay = h.ok("artifact.collect", params);
    let ended = h.settle(&replay["job_id"]);
    assert_eq!(ended["status"], "cancelled");
    assert_eq!(ended["error"]["data"]["reason"], "interrupted");
}

fn published_batch_failure(late_cancel: bool) {
    let h = Harness::new(
        if late_cancel {
            "batch-sync-cancel"
        } else {
            "batch-sync"
        },
        2,
    );
    let out = h.dir.join("out");
    std::fs::create_dir(&out).unwrap();
    let next = h.hold(2);
    let publication = Arc::new(Gate::new());
    publication.shut();
    if late_cancel {
        let hold = publication.clone();
        h.owner.hold_exports(Some(Arc::new(move |phase| {
            if phase == "published-failure" {
                hold.pass();
            }
        })));
    }
    let params = json!({"targets":targets(&h.assets),"settings":crate::export::settings::baseline_json_in(&out),"mutation":mutation("batch-export")});
    let started = h.ok("batch.export", params.clone());
    next.wait_reached(1, "first exported file committed");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
    crate::atomic_file::sync_faults::inject(&out);
    next.open();
    if late_cancel {
        publication.wait_reached(1, "visible export whose directory sync failed");
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 2);
        h.ok("job.cancel", json!({"job_id":started["job_id"]}));
        publication.open();
    }
    let ended = h.settle(&started["job_id"]);
    assert_eq!(ended["status"], "failed", "{ended}");
    let data = &ended["error"]["data"];
    assert_eq!(data["published"], true);
    assert_eq!(data["durable"], false);
    assert!(PathBuf::from(data["path"].as_str().unwrap()).is_file());
    assert_eq!(data["partial_report"]["done"].as_array().unwrap().len(), 1);
    assert_eq!(
        data["partial_report"]["written"].as_array().unwrap().len(),
        1
    );
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 2);
    let events = h.ok("events.since", json!({"after":0}));
    assert_eq!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["method"] == "batch.export")
            .count(),
        3,
        "both visible files and the terminal job were announced"
    );
    let h = h.restart();
    let replay = h.ok("batch.export", params);
    let replayed = h.settle(&replay["job_id"]);
    assert_eq!(replayed["error"], ended["error"]);
    assert_eq!(
        std::fs::read_dir(&out).unwrap().count(),
        2,
        "retry never creates another suffixed output"
    );
}

#[test]
#[slow]
fn durable_requests_batch_export_directory_sync_failure_retains_file_and_partial_report() {
    published_batch_failure(false);
}

#[test]
#[slow]
fn durable_requests_batch_export_late_cancel_cannot_hide_visible_publication_failure() {
    published_batch_failure(true);
}

#[test]
#[slow]
fn durable_requests_worker_start_failure_is_terminal_and_does_not_restart_work() {
    let h = Harness::new("startup-failure", 1);
    let before = h.revision(0);
    h.owner
        .sender
        .send(OwnerMessage::Catalog(CatalogMessage::Library(
            LibraryMessage::Run(Box::new(|owner| {
                owner.catalog.library.start_failure = Some(crate::Error::resource_limit(
                    "injected worker startup refusal",
                ));
            })),
        )))
        .unwrap();
    let params = h.inline_batch("startup");
    let started = h.ok("batch.apply-settings", params.clone());
    let failed = h.settle(&started["job_id"]);
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error"]["code"], "resource-limit");
    assert_eq!(h.revision(0), before);
    let h = h.restart();
    let replay = h.ok("batch.apply-settings", params);
    assert_eq!(h.settle(&replay["job_id"])["error"], failed["error"]);
    assert_eq!(h.revision(0), before);
}

#[test]
#[slow]
fn durable_requests_index_add_returns_the_reserved_listing_job_and_omits_dead_replay_metadata() {
    let h = Harness::new("index-job", 0);
    let folder = h.dir.join("listed");
    std::fs::create_dir(&folder).unwrap();
    let params = json!({"path":folder,"mutation":mutation("index-add")});
    let first = h.ok("index.add-folder", params.clone());
    assert!(first["job_id"].is_string(), "{first}");
    assert_eq!(h.settle(&first["job_id"])["status"], "ready");
    assert_eq!(
        h.ok("index.add-folder", params.clone())["job_id"],
        first["job_id"]
    );
    let h = h.restart();
    let replay = h.ok("index.add-folder", params);
    assert_eq!(replay["deduplicated"], true);
    assert!(
        replay.get("job_id").is_none(),
        "never return an unreadable older owner's maintenance job"
    );
    assert_eq!(replay["folder"], first["folder"]);
    assert_eq!(replay["change"]["change"], first["change"]["change"]);
}

#[test]
#[slow]
fn durable_requests_sync_and_deferred_answers_never_admit_to_owner_cache() {
    let h = Harness::new("cache-admission", 1);
    let (owner_params, mut owner_answer) = h.commit_draft("owner-answer");
    assert_eq!(h.on_owner(|owner| owner.requests.len()), 1);
    let folder = h.ok(
        "folder.create",
        json!({"name":"Keep","mutation":mutation("folder")}),
    );
    assert_eq!(folder["deduplicated"], false);
    assert_eq!(folder["change"]["deduplicated"], false);
    for n in 0..260 {
        h.ok("folder.rename",json!({"folder_id":folder["folder"]["id"],"name":"Keep","mutation":mutation(&format!("durable-{n}"))}));
    }
    let path = h.dir.join("indexed");
    std::fs::create_dir(&path).unwrap();
    let add = json!({"path":path,"mutation":mutation("index-add")});
    let indexed = h.ok("index.add-folder", add.clone());
    assert_eq!(indexed["deduplicated"], false);
    assert_eq!(h.ok("index.add-folder", add)["deduplicated"], true);
    assert_eq!(
        h.on_owner(|owner| owner.requests.len()),
        1,
        "durable synchronous and deferred answers consume no cache entries"
    );
    owner_answer["deduplicated"] = json!(true);
    assert_eq!(
        h.ok("draft.commit", owner_params),
        owner_answer,
        "durable requests never evict the useful session answer"
    );
}
