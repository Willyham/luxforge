//! The core's usage producers through the catalog owner, with an in-memory started collector:
//! a mixed import, develop, reopen and export session, retries, no-ops, cancelled drafts, mask
//! commands, settings sets, reads and the off state (`docs/engineering/usage-statistics-coverage.md`).
use super::*;
use crate::usage::{
    Collector, EventKind, Feature, ImageId, ImageKey, Operation, Outcome as Ended, SessionClock,
    SessionStart, Tool, Trigger,
};
use std::{fs, path::Path};

const ACTOR: &str = "usage-test";

struct Harness {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    collector: Arc<Collector>,
    dir: PathBuf,
    next: AtomicU64,
}

impl Harness {
    fn new(name: &str) -> Self {
        let dir = luxforge_testbase::paths::temp_path(&format!("usage-events-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let collector = Arc::new(Collector::new());
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                usage: collector.clone(),
                ..HostConfig::unconfigured()
            },
        )
        .unwrap();
        let client = owner.register();
        Self {
            owner,
            join: Some(join),
            client,
            collector,
            dir,
            next: AtomicU64::new(1),
        }
    }

    /// Opt in: a session with a known key.
    fn start(&self) {
        self.collector.start(
            Trigger::OptIn,
            SessionStart::parse("2026-10-09T14:03:12Z").unwrap(),
            SessionClock::manual().0,
            key(),
        );
    }

    /// Every event recorded since the last call, `session.start` left out.
    fn events(&self) -> Vec<EventKind> {
        let Some(session) = self.collector.session() else {
            return Vec::new();
        };
        let Some(window) = session.detach() else {
            return Vec::new();
        };
        window
            .events()
            .iter()
            .map(|event| event.kind)
            .filter(|kind| !matches!(kind, EventKind::SessionStart { .. }))
            .collect()
    }

    fn respond(&self, request_id: &str, method: &str, params: Value) -> ApiResponse {
        self.owner
            .call(
                self.client,
                ApiRequest {
                    id: request_id.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        let id = format!("{method}-{}", self.next.fetch_add(1, Ordering::Relaxed));
        let response = self.respond(&id, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn settle(&self, job: &Value) -> Value {
        luxforge_testbase::wait_for("the job to settle", || {
            let record = self.ok("job.read", json!({"job_id": job}));
            (!matches!(record["status"].as_str(), Some("queued" | "running"))).then_some(record)
        })
    }

    fn request(&self, request_id: &str) -> Value {
        json!({"request_id": request_id, "actor": ACTOR})
    }

    fn revision(&self, asset: &Value) -> u64 {
        self.ok("asset.state", json!({"asset_id": asset}))["revision"]
            .as_u64()
            .unwrap()
    }

    fn asset_mutation(&self, asset: &Value, request_id: &str) -> Value {
        json!({"expected_revision": self.revision(asset), "request_id": request_id, "actor": ACTOR})
    }

    /// A copy of a fixture photograph in the test's own folder.
    fn photo(&self, fixture: &str, name: &str) -> PathBuf {
        let path = self.dir.join(name);
        fs::copy(fixture_path(fixture), &path).unwrap();
        path
    }

    /// `pick.develop` of `paths`, settled: its record.
    fn develop(&self, request_id: &str, paths: &[&Path]) -> Value {
        let started = self.ok(
            "pick.develop",
            json!({
                "targets": {"kind": "paths", "paths": paths},
                "into": [],
                "confirm_removable": true,
                "mutation": self.request(request_id),
            }),
        );
        self.settle(&started["job_id"])
    }

    /// Prepare and adopt `asset` as this client's open photograph, as an open does.
    fn open(&self, asset: &Value) -> Value {
        let queued = self.ok("source.prepare", json!({"asset_id": asset}));
        let record = self.settle(&queued["job_id"]);
        assert_eq!(record["status"], "ready", "{record}");
        self.ok("job.adopt", json!({"job_id": queued["job_id"]}))["asset"].clone()
    }

    fn set_basic(&self, asset: &Value, request_id: &str, exposure: f64) -> ApiResponse {
        let mutation = self.asset_mutation(asset, request_id);
        self.respond(
            request_id,
            "edit.set-basic",
            json!({"asset_id": asset, "exposure": exposure, "mutation": mutation}),
        )
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn key() -> ImageKey {
    ImageKey::from_bytes([0x42; 32])
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0")
        .join(name)
}

/// The image ID the test's key gives the photograph whose state is `state`.
fn image_of(state: &Value) -> ImageId {
    key()
        .image_id_of_hex(
            state["asset"]["fingerprint"]
                .as_str()
                .unwrap_or_else(|| state["fingerprint"].as_str().expect("a fingerprint")),
        )
        .expect("a fingerprint")
}

fn interaction(feature: Feature, image: Option<ImageId>) -> EventKind {
    EventKind::Interaction { feature, image }
}

fn commit(tool: Tool, image: ImageId) -> EventKind {
    EventKind::ToolCommit { tool, image }
}

/// The operation and outcome of an `operation.end`, with its count and image.
fn ended(kind: &EventKind) -> Option<(Operation, Ended, Option<u32>, Option<ImageId>)> {
    match kind {
        EventKind::OperationEnd(end) => {
            Some((end.operation(), end.outcome(), end.count(), end.image()))
        }
        _ => None,
    }
}

#[test]
fn usage_nothing_is_recorded_while_collection_is_off() {
    let harness = Harness::new("off");
    let photo = harness.photo("orientation-1.jpg", "one.jpg");
    let developed = harness.develop("develop-off", &[&photo]);
    let asset = developed["result"]["developed"][0]["asset_id"].clone();
    harness.open(&asset);
    assert!(harness.set_basic(&asset, "off-edit", 0.5).error.is_none());
    assert!(harness.collector.session().is_none(), "off: no session");
    // Opting in records nothing that happened before it.
    harness.start();
    assert_eq!(harness.events(), []);
}

/// The mixed session: only newly committed originals count as added, a linked duplicate and a
/// failed path add nothing; preparation, adoption, commits, a retry, a no-op, a drag, a cancelled
/// drag, an undo, a reopen and an export record exactly their events, in order; reads record
/// nothing.
#[test]
fn usage_a_mixed_import_develop_reopen_export_session_records_its_events_in_order() {
    let harness = Harness::new("mixed");
    harness.start();
    let one = harness.photo("orientation-1.jpg", "one.jpg");
    let two = harness.photo("orientation-3.jpg", "two.jpg");
    let duplicate = harness.photo("orientation-1.jpg", "duplicate.jpg");
    let broken = harness.photo("invalid.jpg", "broken.jpg");
    let developed = harness.develop("develop-mixed", &[&one, &two, &duplicate, &broken]);
    assert_eq!(developed["status"], "ready", "{developed}");
    let outcomes: Vec<&str> = developed["result"]["developed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pick| pick["outcome"].as_str().unwrap())
        .collect();
    let created = outcomes
        .iter()
        .filter(|outcome| **outcome == "created")
        .count();
    assert_eq!(created, 2, "{developed}");
    assert_eq!(
        harness.events(),
        [
            interaction(Feature::DevelopPicks, None),
            EventKind::ImagesAdded {
                count: crate::usage::AddedCount::new(2).unwrap()
            },
        ]
    );

    let asset = developed["result"]["developed"][0]["asset_id"].clone();
    let state = harness.open(&asset);
    let image = image_of(&state);
    let opened = harness.events();
    assert_eq!(opened.len(), 2, "{opened:?}");
    assert_eq!(
        ended(&opened[0]),
        Some((
            Operation::SourcePreparation,
            Ended::Completed,
            None,
            Some(image)
        ))
    );
    assert_eq!(opened[1], EventKind::ImageOpen { image });

    // A commit, its retry and a no-op to the value it already has.
    let first = harness.set_basic(&asset, "exposure", 0.5);
    assert!(first.error.is_none(), "{:?}", first.error);
    let mutation = json!({"expected_revision": 0, "request_id": "exposure", "actor": ACTOR});
    let retried = harness.respond(
        "exposure",
        "edit.set-basic",
        json!({"asset_id": asset, "exposure": 0.5, "mutation": mutation}),
    );
    assert_eq!(retried.result.unwrap()["deduplicated"], json!(true));
    assert!(harness.set_basic(&asset, "same", 0.5).error.is_none());
    assert_eq!(harness.events(), [commit(Tool::Basic, image)]);

    // Ten drag updates and one commit; a cancelled drag commits nothing.
    let draft = harness.ok(
        "draft.begin",
        json!({"asset_id": asset, "action": "set-basic"}),
    );
    for tick in 1..=10 {
        harness.ok(
            "draft.set",
            json!({"draft_id": draft["draft_id"], "fields": {"exposure": 0.5 + f64::from(tick) / 10.0}}),
        );
    }
    let mutation = harness.asset_mutation(&asset, "drag");
    harness.ok(
        "draft.commit",
        json!({"draft_id": draft["draft_id"], "mutation": mutation}),
    );
    let cancelled = harness.ok(
        "draft.begin",
        json!({"asset_id": asset, "action": "set-basic"}),
    );
    harness.ok(
        "draft.set",
        json!({"draft_id": cancelled["draft_id"], "fields": {"exposure": -1.0}}),
    );
    harness.ok("draft.cancel", json!({"draft_id": cancelled["draft_id"]}));
    assert_eq!(harness.events(), [commit(Tool::Basic, image)]);

    // An undo is history's interaction, not a tool commit.
    let mutation = harness.asset_mutation(&asset, "undo");
    harness.ok(
        "history.undo",
        json!({"asset_id": asset, "mutation": mutation}),
    );
    assert_eq!(
        harness.events(),
        [interaction(Feature::HistoryUndo, Some(image))]
    );

    // Reads, polls and the statistics' own reads record nothing.
    harness.ok("session.state", json!({}));
    harness.ok("asset.state", json!({"asset_id": asset}));
    harness.ok("history.list", json!({"asset_id": asset}));
    harness.ok("events.since", json!({"after": 0}));
    harness.ok("activity.list", json!({}));
    harness.ok("usage.read", json!({}));
    assert_eq!(harness.events(), []);

    // A reopen: another preparation and open of the same image, and nothing added.
    harness.open(&asset);
    let reopened = harness.events();
    assert_eq!(reopened.len(), 2, "{reopened:?}");
    assert_eq!(
        ended(&reopened[0]).map(|(operation, outcome, _, image)| (operation, outcome, image)),
        Some((Operation::SourcePreparation, Ended::Completed, Some(image)))
    );
    assert_eq!(reopened[1], EventKind::ImageOpen { image });

    // The open photograph's export: started, then written.
    let destination = harness.dir.join("export.jpg");
    let accepted = harness.ok(
        "export.jpeg",
        json!({"asset_id": asset, "destination": destination, "mutation": harness.request("export")}),
    );
    let record = harness.settle(&accepted["job_id"]);
    assert_eq!(record["status"], "ready", "{record}");
    let exported = harness.events();
    assert_eq!(exported.len(), 2, "{exported:?}");
    assert_eq!(exported[0], interaction(Feature::Export, Some(image)));
    assert_eq!(
        ended(&exported[1])
            .map(|(operation, outcome, count, image)| (operation, outcome, count, image)),
        Some((Operation::Export, Ended::Completed, Some(1), Some(image)))
    );

    // A refused export (nothing may exist at its destination) is no interaction and no outcome.
    let refused = harness.respond(
        "export-again",
        "export.jpeg",
        json!({"asset_id": asset, "destination": destination, "mutation": harness.request("export-again")}),
    );
    assert!(refused.error.is_some());
    assert_eq!(harness.events(), []);
}

/// A preset's application and a paste each record one interaction beside one commit per group they
/// changed; mask commands record their interactions and no tool commit; a settings set that changes
/// nothing records nothing.
#[test]
fn usage_settings_sets_and_mask_commands_record_their_interactions_and_changed_groups() {
    let harness = Harness::new("settings");
    let photo = harness.photo("orientation-1.jpg", "one.jpg");
    let developed = harness.develop("develop-settings", &[&photo]);
    let asset = developed["result"]["developed"][0]["asset_id"].clone();
    harness.start();
    let state = harness.open(&asset);
    let image = image_of(&state);
    harness.events();

    let settings = json!({"set-basic": {"exposure": 0.5}, "set-vignette": {"amount": -20}});
    let mutation = harness.asset_mutation(&asset, "preset");
    harness.ok(
        "edit.apply-settings",
        json!({"asset_id": asset, "settings": settings, "mutation": mutation,
               "origin": {"kind": "preset", "name": "Moody"}}),
    );
    assert_eq!(
        harness.events(),
        [
            commit(Tool::Basic, image),
            commit(Tool::Vignette, image),
            interaction(Feature::PresetApply, Some(image)),
        ]
    );
    let mutation = harness.asset_mutation(&asset, "paste");
    harness.ok(
        "edit.apply-settings",
        json!({"asset_id": asset, "settings": {"set-basic": {"exposure": 1.0}}, "mutation": mutation,
               "origin": {"kind": "paste", "source": "other.jpg"}}),
    );
    assert_eq!(
        harness.events(),
        [
            commit(Tool::Basic, image),
            interaction(Feature::SettingsPaste, Some(image)),
        ]
    );
    // The same paste again changes nothing.
    let mutation = harness.asset_mutation(&asset, "paste-again");
    harness.ok(
        "edit.apply-settings",
        json!({"asset_id": asset, "settings": {"set-basic": {"exposure": 1.0}}, "mutation": mutation,
               "origin": {"kind": "paste", "source": "other.jpg"}}),
    );
    assert_eq!(harness.events(), []);

    let mutation = harness.asset_mutation(&asset, "mask");
    let created = harness.ok(
        "mask.create-linear",
        json!({"asset_id": asset, "x0": 0.5, "y0": 0.1, "x1": 0.5, "y1": 0.6, "mutation": mutation}),
    );
    let mutation = harness.asset_mutation(&asset, "rename");
    harness.ok(
        "mask.rename",
        json!({"asset_id": asset, "mask": created["mask"], "name": "Sky", "mutation": mutation}),
    );
    assert_eq!(
        harness.events(),
        [
            interaction(Feature::MaskCreate, Some(image)),
            interaction(Feature::MaskRename, Some(image)),
        ]
    );
}

/// Catalog organization, a preference and a library undo record their interactions once; a
/// retried request is answered from the request table and records nothing more.
#[test]
fn usage_catalog_organization_records_once_and_a_retry_records_nothing() {
    let harness = Harness::new("organize");
    harness.start();
    let params = json!({"name": "Trips", "mutation": harness.request("folder")});
    harness.ok("folder.create", params.clone());
    let retried = harness.respond("again", "folder.create", params);
    assert!(retried.error.is_none(), "{:?}", retried.error);
    harness.ok(
        "collection.create",
        json!({"name": "Best", "kind": "group", "mutation": harness.request("collection")}),
    );
    harness.ok(
        "library.undo",
        json!({"mutation": harness.request("library-undo")}),
    );
    assert_eq!(
        harness.events(),
        [
            interaction(Feature::FolderCreate, None),
            interaction(Feature::CollectionCreate, None),
            interaction(Feature::LibraryUndo, None),
        ]
    );
}

/// Instrumentation carries no argument, label or identity but the keyed image ID: no asset ID,
/// fingerprint, file name, request ID or actor appears in any recorded event.
#[test]
fn usage_events_carry_no_arguments_labels_or_identities() {
    let harness = Harness::new("privacy");
    harness.start();
    let photo = harness.photo("orientation-1.jpg", "Holiday-Alice.jpg");
    let developed = harness.develop("private-request", &[&photo]);
    let asset = developed["result"]["developed"][0]["asset_id"].clone();
    let state = harness.open(&asset);
    assert!(
        harness
            .set_basic(&asset, "private-edit", 0.75)
            .error
            .is_none()
    );
    let events = harness.events();
    assert!(!events.is_empty());
    let debug = format!("{events:?}");
    let fingerprint = state["asset"]["fingerprint"].as_str().unwrap();
    for secret in [
        asset.as_str().unwrap(),
        fingerprint,
        &fingerprint[..32],
        "Holiday",
        "Alice",
        "private-request",
        "private-edit",
        ACTOR,
        "0.75",
    ] {
        assert!(!debug.contains(secret), "{secret} appears in {debug}");
    }
}
