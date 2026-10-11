//! Accepting a candidate as a frozen patch through the catalog owner (`accept.rs`), driven by the
//! developer producer (`luxforge.candidates`) as a client drives it: one entry and one event for
//! an accept, a retry answered with the first answer, a second accept of the same candidate and
//! one generated against another input refused, the refusals that pin nothing, a downstream edit
//! that does not block the accept, and a disconnect, a cancel and a commit that land while the
//! source worker publishes, each leaving the candidate live and at most an unreferenced artifact.
use super::OwnerHandle;
use crate::{
    AssetId, EditorService, ModuleRegistry,
    activity::ActivityBoard,
    api::{ApiFailure, ApiRequest, ApiResponse, ClientId},
    editor::mutation_json,
    modules::CandidateProofModule,
};
use luxforge_testbase::{Gate, paths::temp_dir, wait_for};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

const TASK: &str = "task.generate-proof-patches";
const ACCEPT: &str = "edit.accept-correction-patch";

fn registry() -> Arc<ModuleRegistry> {
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(Arc::new(CandidateProofModule::new()))
        .unwrap();
    Arc::new(registry)
}

static NEXT: AtomicU64 = AtomicU64::new(1);

/// One owner over a catalog holding two photographs, serving the producer, whose source worker
/// passes `gate` before each task's work.
struct Harness {
    root: PathBuf,
    catalog: PathBuf,
    asset: AssetId,
    other: AssetId,
    gate: Arc<Gate>,
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
}

impl Harness {
    fn new(name: &str) -> Self {
        let root = temp_dir(&format!("accept-{name}")).canonicalize().unwrap();
        let catalog = root.join("catalog.sqlite");
        let (asset, other) = {
            let mut service = EditorService::open_with(&catalog, registry()).unwrap();
            let mut import = |name: &str| {
                service
                    .import(&luxforge_testbase::paths::fixture(name))
                    .unwrap()
                    .asset
                    .id
            };
            (
                import("s0/orientation-1.jpg"),
                import("s0/orientation-6.jpg"),
            )
        };
        Self::start(root, catalog, asset, other)
    }

    fn start(root: PathBuf, catalog: PathBuf, asset: AssetId, other: AssetId) -> Self {
        let gate = Arc::new(Gate::new());
        let held = gate.clone();
        let (owner, join) = OwnerHandle::start_observed(
            &catalog,
            registry(),
            ActivityBoard::new(),
            Some(Arc::new(move || held.pass())),
        )
        .unwrap();
        let client = owner.register();
        Self {
            root,
            catalog,
            asset,
            other,
            gate,
            owner,
            join: Some(join),
            client,
        }
    }

    fn restart(mut self) -> Self {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
        let root = std::mem::take(&mut self.root);
        Self::start(
            root,
            self.catalog.clone(),
            self.asset.clone(),
            self.other.clone(),
        )
    }

    fn send_as(&self, client: ClientId, method: &str, params: Value) -> ApiResponse {
        self.owner
            .call(
                client,
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
        let response = self.send_as(self.client, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn refused(&self, method: &str, params: Value) -> ApiFailure {
        self.send_as(self.client, method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to be refused"))
    }

    fn revision(&self) -> u64 {
        self.ok("asset.state", json!({"asset_id": self.asset}))["revision"]
            .as_u64()
            .unwrap()
    }

    /// The producer's candidates of the photograph, finished: the job's identity.
    fn generate(&self, count: u64, seed: u64) -> Value {
        let started = self.ok(
            TASK,
            json!({"asset_id": self.asset, "count": count, "width": 24, "height": 16, "x": 30,
                   "y": 20, "seed": seed,
                   "mutation": {"request_id": format!("generate-{seed}"), "actor": "test"}}),
        );
        let job = started["job_id"].clone();
        wait_for("the producer to finish", || {
            Some(self.ok("job.read", json!({"job_id": job})))
                .filter(|record| record["status"] == "ready")
        });
        job
    }

    fn accept_params(&self, job: &Value, candidate: u64, revision: u64, request: &str) -> Value {
        json!({"asset_id": self.asset, "mutation": mutation_json(revision, request),
               "job_id": job, "candidate": candidate})
    }

    fn states(&self, job: &Value) -> Vec<String> {
        self.ok("job.read", json!({"job_id": job}))["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["state"].as_str().unwrap().to_owned())
            .collect()
    }

    fn corrections(&self) -> Vec<Value> {
        self.ok("query.corrections", json!({"asset_id": self.asset}))["operations"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn events(&self) -> Vec<String> {
        self.ok("events.since", json!({"after": 0}))["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["method"].as_str().unwrap().to_owned())
            .collect()
    }

    fn publishing(&self) -> Option<Value> {
        let list = self.ok("activity.list", json!({}));
        list["active"].as_array().and_then(|active| {
            active
                .iter()
                .find(|entry| entry["kind"] == "artifacts.publish")
                .map(|entry| entry["job_id"].clone())
        })
    }

    /// Shut the gate once no other source work is running, so only an accept waits at it.
    fn hold(&self) {
        wait_for("the source worker to be idle", || {
            let list = self.ok("activity.list", json!({}));
            Some(()).filter(|_| {
                list["active"].as_array().is_none_or(|active| {
                    active.iter().all(|entry| {
                        !entry["kind"]
                            .as_str()
                            .unwrap_or("")
                            .starts_with("artifacts")
                    })
                })
            })
        });
        self.gate.shut();
    }

    /// The accept job the source worker holds at the gate.
    fn held_accept(&self) -> Value {
        let job = wait_for("the accept to reach the source worker", || {
            self.publishing()
        });
        wait_for("the accept to wait at the gate", || {
            Some(()).filter(|_| self.gate.holding())
        });
        job
    }

    /// Send `params` as an accept from `client` on another thread; `None` when the call was
    /// dropped unanswered.
    fn accept_on_thread(&self, client: ClientId, params: Value) -> JoinHandle<Option<ApiResponse>> {
        let owner = self.owner.clone();
        thread::spawn(move || {
            owner
                .call(
                    client,
                    ApiRequest {
                        id: "held-accept".into(),
                        method: ACCEPT.into(),
                        params,
                        token: None,
                    },
                )
                .ok()
        })
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.gate.open();
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        if !self.root.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

/// Every file under `root`, counted.
fn files(root: &Path) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() { files(&path) } else { 1 }
        })
        .sum()
}

#[test]
fn a_candidate_is_accepted_once_as_one_patch_entry_and_a_retry_answers_the_first_answer() {
    let harness = Harness::new("journey");
    let job = harness.generate(2, 4);
    let revision = harness.revision();
    let before = harness.events().len();
    let mut params = harness.accept_params(&job, 1, revision, "accept-1");
    params["opacity"] = json!(80);
    let accepted = harness.ok(ACCEPT, params.clone());
    assert_eq!(accepted["outcome"], "applied");
    assert_eq!(accepted["revision"], revision + 1);
    assert_eq!(
        accepted["correction"],
        json!({"operation": "correction-1", "name": "Patch 1", "index": 0})
    );
    assert_eq!(harness.states(&job), ["live", "accepted"]);
    let events = harness.events();
    assert_eq!(&events[before..], [ACCEPT], "one event for one entry");
    let rows = harness.corrections();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0]["kind"].clone(),
            rows[0]["rect"].clone(),
            rows[0]["opacity"].clone()
        ),
        (json!("patch"), json!([30, 20, 24, 16]), json!(80))
    );
    let history = harness.ok(
        "history.list",
        json!({"asset_id": harness.asset, "limit": 5}),
    );
    assert_eq!(history["entries"][0]["label"], "Add Patch 1");
    // The retry is the first answer; nothing more is published or committed.
    let retried = harness.ok(ACCEPT, params);
    assert_eq!(retried["deduplicated"], true);
    assert_eq!(retried["correction"], accepted["correction"]);
    assert_eq!(harness.revision(), revision + 1);
    assert_eq!(
        harness.events().len(),
        events.len(),
        "a retry records nothing"
    );
    // The same candidate again, under another request, is no longer live.
    let again = harness.refused(
        ACCEPT,
        harness.accept_params(&job, 1, revision + 1, "accept-again"),
    );
    assert_eq!(again.code, "conflict", "{again:?}");
    // A stale revision is the ordinary conflict, and the other candidate was generated against
    // the stack without the patch, which a new top correction no longer reads.
    let stale = harness.refused(ACCEPT, harness.accept_params(&job, 0, revision, "stale"));
    assert_eq!(stale.code, "conflict", "{stale:?}");
    let other = harness.refused(
        ACCEPT,
        harness.accept_params(&job, 0, revision + 1, "other"),
    );
    assert_eq!(other.code, "conflict", "{other:?}");
    assert_eq!(other.data, Some(json!({"reasons": ["earlier-correction"]})));
    assert_eq!(harness.states(&job), ["live", "accepted"]);
}

/// Patches stack: a candidate generated after an earlier patch was accepted, over a Clone, reads
/// the stack with both and is accepted on top of them as an entry of its own. Two accepts in a row
/// by one actor never collapse: planned on the first's base, the second would be checked against a
/// stack its candidate was not generated against.
#[test]
fn a_candidate_generated_over_an_accepted_patch_is_accepted_on_top_of_it() {
    let harness = Harness::new("stacked");
    harness.ok(
        "edit.add-correction",
        json!({"asset_id": harness.asset, "mutation": mutation_json(harness.revision(), "clone"),
               "points": [[0.3, 0.4]], "radius": 0.02, "mode": "clone", "feather": 50,
               "opacity": 100, "source-x": 0.42, "source-y": 0.46}),
    );
    let first = harness.generate(1, 7);
    harness.ok(
        ACCEPT,
        harness.accept_params(&first, 0, harness.revision(), "first"),
    );
    let second = harness.generate(2, 23);
    let mut params = harness.accept_params(&second, 1, harness.revision(), "second");
    params["opacity"] = json!(70);
    let accepted = harness.ok(ACCEPT, params);
    assert_eq!(accepted["outcome"], "applied");
    let rows = harness.corrections();
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter().all(|row| row.get("stale").is_none()),
        "{rows:?}"
    );
    let history = harness.ok(
        "history.list",
        json!({"asset_id": harness.asset, "limit": 5}),
    );
    let labels: Vec<&str> = history["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["label"].as_str())
        .collect();
    assert_eq!(&labels[..2], ["Add Patch 3", "Add Patch 2"], "{history}");
}

#[test]
fn what_is_not_a_live_candidate_of_this_photograph_is_refused_before_anything_is_published() {
    let harness = Harness::new("refusals");
    let job = harness.generate(1, 2);
    let revision = harness.revision();
    let refused = |params: Value| harness.refused(ACCEPT, params);
    let never = refused(harness.accept_params(&job, 3, revision, "never"));
    assert_eq!(never.code, "validation", "{never:?}");
    let unknown =
        refused(harness.accept_params(&json!(crate::JobId::new()), 0, revision, "unknown"));
    assert_eq!(unknown.code, "validation", "{unknown:?}");
    let mut out_of_range = harness.accept_params(&job, 0, revision, "range");
    out_of_range["opacity"] = json!(101);
    assert_eq!(refused(out_of_range).code, "validation");
    // Another photograph's accept of this photograph's candidate.
    let mut elsewhere = harness.accept_params(&job, 0, 0, "elsewhere");
    elsewhere["asset_id"] = json!(harness.other);
    let elsewhere = refused(elsewhere);
    assert_eq!(elsewhere.code, "validation", "{elsewhere:?}");
    assert!(
        elsewhere.message.contains("another photograph"),
        "{elsewhere:?}"
    );
    // A caller that may not wait is told so before anything is pinned.
    let unparked = harness
        .owner
        .call_unparked(
            harness.client,
            ApiRequest {
                id: "unparked".into(),
                method: ACCEPT.into(),
                params: harness.accept_params(&job, 0, revision, "unparked"),
                token: None,
            },
        )
        .unwrap()
        .error
        .unwrap();
    assert_eq!(unparked.code, "not-ready");
    assert_eq!(unparked.data, Some(json!({"reason": "accept-publishes"})));
    // A released candidate.
    harness.ok(
        "job.release-candidates",
        json!({"job_id": job, "mutation": {"request_id": "release", "actor": "test"}}),
    );
    let released = refused(harness.accept_params(&job, 0, revision, "released"));
    assert_eq!(released.code, "conflict", "{released:?}");
    assert_eq!(harness.revision(), revision, "nothing was committed");
    assert!(harness.corrections().is_empty());
    assert!(harness.publishing().is_none());
}

#[test]
fn a_downstream_edit_after_generation_leaves_the_candidate_acceptable() {
    let harness = Harness::new("downstream");
    let job = harness.generate(1, 3);
    harness.ok(
        "edit.set-basic",
        json!({"asset_id": harness.asset, "mutation": mutation_json(harness.revision(), "exposure"),
               "exposure": 0.5}),
    );
    let revision = harness.revision();
    let accepted = harness.ok(ACCEPT, harness.accept_params(&job, 0, revision, "accept"));
    assert_eq!(accepted["outcome"], "applied");
    assert!(harness.corrections()[0].get("stale").is_none());
}

#[test]
fn a_disconnect_or_cancel_while_the_candidate_is_published_commits_nothing() {
    let harness = Harness::new("cancel");
    let job = harness.generate(1, 5);
    let revision = harness.revision();
    // The caller leaves while its accept waits on the source worker.
    harness.hold();
    let leaving = harness.owner.register();
    let call =
        harness.accept_on_thread(leaving, harness.accept_params(&job, 0, revision, "leaving"));
    harness.held_accept();
    assert_eq!(harness.states(&job), ["live"]);
    harness.owner.disconnect(leaving);
    harness.gate.open();
    assert!(
        call.join().unwrap().is_none(),
        "a gone caller is not answered"
    );
    wait_for("the accept job to end", || {
        Some(()).filter(|_| harness.publishing().is_none())
    });
    assert_eq!(harness.revision(), revision, "nothing was committed");
    assert_eq!(harness.states(&job), ["live"]);
    // Its own client cancels the accept job: the call is answered and nothing is committed.
    harness.hold();
    let call = harness.accept_on_thread(
        harness.client,
        harness.accept_params(&job, 0, revision, "cancelled"),
    );
    let accept_job = harness.held_accept();
    harness.ok("job.cancel", json!({"job_id": accept_job}));
    harness.gate.open();
    let answer = call.join().unwrap().expect("the caller is answered");
    assert!(answer.error.is_some(), "{answer:?}");
    assert_eq!(harness.revision(), revision);
    assert_eq!(harness.states(&job), ["live"]);
    // Then the accept goes through.
    let accepted = harness.ok(ACCEPT, harness.accept_params(&job, 0, revision, "accepted"));
    assert_eq!(accepted["outcome"], "applied");
    assert_eq!(harness.states(&job), ["accepted"]);
}

#[test]
fn a_commit_while_the_candidate_is_published_refuses_the_accept_and_leaves_a_collectable_artifact()
{
    let harness = Harness::new("race");
    let job = harness.generate(1, 6);
    let revision = harness.revision();
    harness.hold();
    let call = harness.accept_on_thread(
        harness.client,
        harness.accept_params(&job, 0, revision, "raced"),
    );
    harness.held_accept();
    let other = harness.owner.register();
    let response = harness.send_as(
        other,
        "edit.set-basic",
        json!({"asset_id": harness.asset, "mutation": mutation_json(revision, "between"),
               "exposure": 0.25}),
    );
    assert!(response.error.is_none(), "{:?}", response.error);
    harness.gate.open();
    let answer = call.join().unwrap().expect("the caller is answered");
    let error = answer.error.expect("the accept is refused");
    assert_eq!(error.code, "conflict", "{error:?}");
    assert_eq!(
        harness.states(&job),
        ["live"],
        "the candidate is still there"
    );
    assert!(harness.corrections().is_empty());
    let objects = harness.catalog.with_extension("artifacts").join("objects");
    assert_eq!(
        files(&objects),
        1,
        "the published bytes are an unreferenced object"
    );
    // This process holds it live; the next one's collection removes it.
    let harness = harness.restart();
    harness.ok(
        "artifact.collect",
        json!({"mutation": {"request_id": "collect", "actor": "test"}}),
    );
    wait_for("the collection", || {
        Some(()).filter(|_| files(&objects) == 0)
    });
}
