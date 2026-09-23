//! Current contracts of the `mask.*` host command family, end to end.
//!
//! Every command is driven twice over the same fixture: once from an independent JSON client through
//! the owner loop, and once from inside the process against [`EditorService`] directly, the way a
//! desktop gesture reaches it. The two must produce the same stacks, the same history labels and the
//! same refusals, because they are the same code reached two ways and not two implementations.
//!
//! Identities are random by construction, so parity is asserted over the shape of a stack — names,
//! values, modes, kinds, payloads and the effects bound to each mask — and over the labels history
//! rows show. Where an identity matters it is resolved by the name the command gave it, which is what
//! a client does.
use crate::{
    ApiRequest, AssetId, ClientId, Component, ComponentId, ComponentMode, EditorService, EntryId,
    Error, ErrorKind, HistoryEntry, Layer, LayerId, Mask, MaskId, Mutation, OwnerHandle, Recipe,
    Snapshot, SnapshotId,
    mask::commands::{self, MaskTarget},
};
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lightwell-mask-commands-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

fn mutation(revision: u64, request: &str) -> Value {
    json!({"expected_revision": revision, "request_id": request, "actor": "contracts"})
}

/// One way of reaching the command family. Both implementations answer the same questions, so the
/// journey below is written once and run twice.
trait Driver {
    /// `mask.list` as a JSON value.
    fn list(&mut self) -> Value;
    /// One command. `Ok` is its result; `Err` is its structured error.
    fn run(
        &mut self,
        method: &str,
        target: &MaskTarget,
        parameters: Value,
        request: &str,
    ) -> Result<Value, Value>;
    fn revision(&mut self) -> u64;
    /// Every entry as `[action_id, label]`, oldest first.
    fn labels(&mut self) -> Vec<Value>;
}

/// The command family reached from an independent JSON client over the owner loop.
struct Json {
    owner: OwnerHandle,
    client: ClientId,
    join: Option<std::thread::JoinHandle<()>>,
    asset: Value,
}

impl Json {
    fn open(catalog: &Path, source: &Path) -> Self {
        let (owner, join) = OwnerHandle::start(catalog).unwrap();
        let client = owner.register();
        let asset = {
            let queued = Self::call(&owner, client, "catalog.import", json!({"path": source}))
                .expect("import queues");
            let id = queued["job_id"].as_str().unwrap().to_owned();
            loop {
                let status = Self::call(&owner, client, "job.status", json!({"job_id": id}))
                    .expect("a job this client owns");
                match status["state"].as_str() {
                    Some("ready") => break status["asset"]["asset"]["id"].clone(),
                    Some("queued" | "preparing") => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    other => panic!("unexpected import job {other:?}"),
                }
            }
        };
        Self {
            owner,
            client,
            join: Some(join),
            asset,
        }
    }

    fn call(
        owner: &OwnerHandle,
        client: ClientId,
        method: &str,
        params: Value,
    ) -> Result<Value, Value> {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: method.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .unwrap();
        match response.error {
            // Both ways report a refusal as its code and its detail, which is what parity is about.
            Some(error) => Err(json!({"code": error.code, "detail": error.message})),
            None => Ok(response.result.unwrap()),
        }
    }

    fn send(&self, method: &str, params: Value) -> Result<Value, Value> {
        Self::call(&self.owner, self.client, method, params)
    }
}

impl Drop for Json {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Driver for Json {
    fn list(&mut self) -> Value {
        self.send("mask.list", json!({"asset_id": self.asset}))
            .expect("mask.list answers")
    }

    fn run(
        &mut self,
        method: &str,
        target: &MaskTarget,
        parameters: Value,
        request: &str,
    ) -> Result<Value, Value> {
        let revision = self.revision();
        let mut params = match parameters {
            Value::Object(object) => object,
            Value::Null => Map::new(),
            other => panic!("parameters must be an object: {other}"),
        };
        params.insert("asset_id".into(), self.asset.clone());
        params.insert("mutation".into(), mutation(revision, request));
        if let Some(mask) = &target.mask {
            params.insert("mask".into(), json!(mask.as_str()));
        }
        if let Some(component) = &target.component {
            params.insert("component".into(), json!(component.as_str()));
        }
        if let Some(name) = &target.name {
            params.insert("name".into(), json!(name));
        }
        self.send(method, Value::Object(params))
    }

    fn revision(&mut self) -> u64 {
        self.send("asset.state", json!({"asset_id": self.asset}))
            .expect("asset.state answers")["revision"]
            .as_u64()
            .unwrap()
    }

    fn labels(&mut self) -> Vec<Value> {
        let listed = self
            .send(
                "history.list",
                json!({"asset_id": self.asset, "limit": 100}),
            )
            .expect("history.list answers");
        let mut labels: Vec<Value> = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| json!([entry["action_id"], entry["label"]]))
            .collect();
        labels.reverse();
        labels
    }
}

/// The command family reached inside the process, the way a desktop gesture reaches it.
struct Direct {
    service: EditorService,
    asset: AssetId,
}

impl Direct {
    fn open(catalog: &Path, source: &Path) -> Self {
        let mut service = EditorService::open(catalog).unwrap();
        let asset = service.import(source).unwrap().asset.id;
        Self { service, asset }
    }

    fn current(&self) -> EntryId {
        self.service.state(&self.asset).unwrap().current_entry.id
    }
}

impl Driver for Direct {
    fn list(&mut self) -> Value {
        let entry = self.current();
        serde_json::to_value(self.service.mask_listing(&self.asset, &entry).unwrap()).unwrap()
    }

    fn run(
        &mut self,
        method: &str,
        target: &MaskTarget,
        parameters: Value,
        request: &str,
    ) -> Result<Value, Value> {
        let revision = self.revision();
        let command = commands::find(method).expect("a declared command");
        self.service
            .apply_mask_command(
                &self.asset,
                Mutation {
                    expected_revision: revision,
                    request_id: request.to_owned(),
                    actor: "contracts".to_owned(),
                },
                command,
                parameters,
                target.clone(),
            )
            .map(|result| serde_json::to_value(result).unwrap())
            .map_err(|error| json!({"code": error.kind.code(), "detail": error.detail}))
    }

    fn revision(&mut self) -> u64 {
        self.service.state(&self.asset).unwrap().revision
    }

    fn labels(&mut self) -> Vec<Value> {
        let mut labels: Vec<Value> = self
            .service
            .history(&self.asset, None, 100)
            .unwrap()
            .entries
            .iter()
            .map(|entry| json!([entry.action_id, entry.label]))
            .collect();
        labels.reverse();
        labels
    }
}

/// A stack's masks with every random identity dropped, which is what two independent runs can be
/// compared on.
fn shape(listing: &Value) -> Value {
    let masks: Vec<Value> = listing["masks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|mask| {
            json!({
                "index": mask["index"],
                "name": mask["name"],
                "amount": mask["amount"],
                "invert": mask["invert"],
                "components": mask["components"].as_array().unwrap().iter().map(|component| json!({
                    "index": component["index"],
                    "name": component["name"],
                    "mode": component["mode"],
                    "invert": component["invert"],
                    "kind": component["kind"],
                    "payload": component["payload"],
                    "available": component["available"],
                })).collect::<Vec<_>>(),
                "layers": mask["layers"].as_array().unwrap().iter().map(|layer| json!([layer["effect"], layer["title"]])).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!(masks)
}

/// The mask of `name`, resolved the way a client resolves one: from the listing.
fn mask_of(listing: &Value, name: &str) -> MaskTarget {
    let mask = listing["masks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mask| mask["name"] == json!(name))
        .unwrap_or_else(|| panic!("no mask named {name} in {listing}"));
    MaskTarget {
        mask: Some(MaskId::parse(mask["id"].as_str().unwrap()).unwrap()),
        component: None,
        name: None,
    }
}

fn component_of(listing: &Value, mask: &str, component: &str) -> MaskTarget {
    let found = listing["masks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["name"] == json!(mask))
        .unwrap_or_else(|| panic!("no mask named {mask}"));
    let id = found["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["name"] == json!(component))
        .unwrap_or_else(|| panic!("no component named {component} in {mask}"))["id"]
        .as_str()
        .unwrap();
    MaskTarget {
        mask: Some(MaskId::parse(found["id"].as_str().unwrap()).unwrap()),
        component: Some(ComponentId::parse(id).unwrap()),
        name: None,
    }
}

fn linear(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!({"kind": "linear", "x0": x0, "y0": y0, "x1": x1, "y1": y1})
}

/// Every command of the family, in one journey, driven through whichever way `driver` reaches it.
/// It returns the resulting stack shape, the history labels, the result of the destructive delete and
/// every refusal it collected, so the two ways can be compared field by field.
fn journey(driver: &mut dyn Driver) -> (Value, Vec<Value>, Value, Vec<Value>) {
    let mut refusals = Vec::new();
    // A mask never exists empty, so its first component arrives with it and is always `add`: the
    // command declares no mode and a request that sends one is refused by the generic check.
    refusals.push(
        driver
            .run(
                "mask.create",
                &MaskTarget::default(),
                json!({"kind": "linear", "mode": "subtract", "x0": 0, "y0": 0, "x1": 0, "y1": 1}),
                "refused-mode",
            )
            .expect_err("a first component is always add"),
    );
    refusals.push(
        driver
            .run(
                "mask.create",
                &MaskTarget::default(),
                linear(0.0, 0.0, 0.0, 3.0),
                "refused-range",
            )
            .expect_err("a stored position is bounded"),
    );
    driver
        .run(
            "mask.create",
            &MaskTarget::default(),
            linear(0.0, 0.0, 0.0, 1.0),
            "create-1",
        )
        .expect("a new mask");
    let listing = driver.list();
    let sky = mask_of(&listing, "Mask 1");
    let first = component_of(&listing, "Mask 1", "Linear 1");
    // A mask never exists empty, so while it has one component that component is not deletable.
    refusals.push(
        driver
            .run("mask.delete-component", &first, Value::Null, "refused-last")
            .expect_err("a mask is never empty"),
    );
    // A field patch over one component's geometry, and the same patch again, which changes nothing.
    driver
        .run("mask.set-component", &first, json!({"x1": 0.5}), "patch-1")
        .expect("a geometry patch");
    let repeated = driver
        .run("mask.set-component", &first, json!({"x1": 0.5}), "patch-2")
        .expect("a patch that changes nothing");
    assert_eq!(
        repeated["outcome"],
        json!("no-op"),
        "setting a field to what it already holds writes no entry"
    );
    driver
        .run(
            "mask.add-component",
            &sky,
            json!({"kind": "linear", "mode": "subtract", "x0": 0.1, "y0": 0.1, "x1": 0.9, "y1": 0.9}),
            "add-2",
        )
        .expect("a second component");
    let listing = driver.list();
    let second = component_of(&listing, "Mask 1", "Linear 2");
    driver
        .run(
            "mask.set-component-mode",
            &second,
            json!({"mode": "intersect"}),
            "mode-2",
        )
        .expect("a mode change");
    driver
        .run(
            "mask.set-component-invert",
            &second,
            json!({"invert": true}),
            "invert-2",
        )
        .expect("a component inversion");
    refusals.push(
        driver
            .run(
                "mask.reorder-component",
                &second,
                json!({"index": 0}),
                "reorder-refused",
            )
            .expect_err("the first component of a mask is always add"),
    );
    driver
        .run("mask.set-amount", &sky, json!({"amount": 60}), "amount-1")
        .expect("a whole-mask amount");
    driver
        .run("mask.set-invert", &sky, json!({"invert": true}), "invert-1")
        .expect("a whole-mask inversion");
    driver
        .run(
            "mask.rename",
            &MaskTarget {
                name: Some("Sky".into()),
                ..sky.clone()
            },
            Value::Null,
            "rename-1",
        )
        .expect("a rename");
    driver
        .run("mask.duplicate", &sky, Value::Null, "duplicate-1")
        .expect("a duplicate");
    let listing = driver.list();
    let copy = mask_of(&listing, "Mask 1");
    driver
        .run("mask.reorder", &copy, json!({"index": 0}), "reorder-1")
        .expect("a mask move");
    // The freed ordinal is never reused: deleting `Linear 2` and adding another linear gives
    // `Linear 3`, so a row naming `Linear 2` can only ever mean the component it was written about.
    let listing = driver.list();
    let second = component_of(&listing, "Sky", "Linear 2");
    driver
        .run("mask.delete-component", &second, Value::Null, "drop-2")
        .expect("a component delete");
    let listing = driver.list();
    driver
        .run(
            "mask.add-component",
            &mask_of(&listing, "Sky"),
            json!({"kind": "linear", "mode": "add", "x0": 0.2, "y0": 0.2, "x1": 0.8, "y1": 0.8}),
            "add-3",
        )
        .expect("a third component");
    refusals.push(
        driver
            .run(
                "mask.set-invert",
                &MaskTarget {
                    mask: Some(MaskId::new()),
                    ..MaskTarget::default()
                },
                json!({"invert": true}),
                "refused-unknown",
            )
            .expect_err("an unknown mask"),
    );
    let listing = driver.list();
    let deleted = driver
        .run(
            "mask.delete",
            &mask_of(&listing, "Mask 1"),
            Value::Null,
            "delete-1",
        )
        .expect("a mask delete");
    (shape(&driver.list()), driver.labels(), deleted, refusals)
}

#[test]
fn every_command_is_identical_from_an_independent_json_client_and_from_inside() {
    let dir = temp("parity");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture(), &source).unwrap();
    let (json_shape, json_labels, json_deleted, json_refusals) = {
        let mut driver = Json::open(&dir.join("json.sqlite"), &source);
        journey(&mut driver)
    };
    let (direct_shape, direct_labels, direct_deleted, direct_refusals) = {
        let mut driver = Direct::open(&dir.join("direct.sqlite"), &source);
        journey(&mut driver)
    };
    assert_eq!(json_shape, direct_shape, "the same stack either way");
    assert_eq!(json_labels, direct_labels, "the same history either way");
    assert_eq!(
        json_deleted["label"], direct_deleted["label"],
        "the same destructive report either way"
    );
    // Identities are random by construction, so a refusal that names one is compared with the
    // identity replaced by its prefix; the identity itself is asserted below.
    assert_eq!(
        anonymous(&json_refusals),
        anonymous(&direct_refusals),
        "the same errors either way"
    );

    // Only the family's own entries: the import wrote the first one.
    let mask_entries: Vec<&Value> = json_labels
        .iter()
        .filter(|entry| entry[0].as_str().unwrap().starts_with("mask."))
        .collect();
    assert_eq!(
        json_labels.len(),
        mask_entries.len() + 1,
        "one entry per command that changed something, and the import's own"
    );
    // The labels the design's history-granularity table states, in the order the journey wrote them.
    assert_eq!(
        mask_entries
            .iter()
            .map(|entry| entry[1].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "Add linear",
            "Update Linear 1",
            "Add subtract linear",
            "Linear 2 intersect",
            "Linear 2 inverted",
            "Amount 60",
            "Inverted",
            "Rename Mask 1 to Sky",
            "Duplicate Sky",
            "Move Mask 1 to 1",
            "Sky · Delete Linear 2",
            "Sky · Add linear",
            "Delete Mask 1"
        ]
    );
    // Every entry of the family stores its own durable identity, so a row says which command wrote it.
    assert_eq!(
        mask_entries
            .iter()
            .map(|entry| entry[0].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "mask.create",
            "mask.set-component",
            "mask.add-component",
            "mask.set-component-mode",
            "mask.set-component-invert",
            "mask.set-amount",
            "mask.set-invert",
            "mask.rename",
            "mask.duplicate",
            "mask.reorder",
            "mask.delete-component",
            "mask.add-component",
            "mask.delete"
        ]
    );
    // One mask survives: the renamed original, with its amount, inversion and two components. The
    // duplicate — which was the one holding `Mask 1` after the rename — was deleted.
    assert_eq!(
        json_shape,
        json!([{
            "index": 0,
            "name": "Sky",
            "amount": 60.0,
            "invert": true,
            "components": [
                {"index": 0, "name": "Linear 1", "mode": "add", "invert": false,
                 "kind": "linear", "payload": {"x0": 0.0, "y0": 0.0, "x1": 0.5, "y1": 1.0},
                 "available": true},
                {"index": 1, "name": "Linear 3", "mode": "add", "invert": false,
                 "kind": "linear", "payload": {"x0": 0.2, "y0": 0.2, "x1": 0.8, "y1": 0.8},
                 "available": true},
            ],
            "layers": [],
        }])
    );
    // Every refusal, with its exact message.
    assert_eq!(
        json_refusals
            .iter()
            .map(|error| error["detail"].as_str().unwrap())
            .take(4)
            .collect::<Vec<_>>(),
        [
            "unknown parameter mode for action mask.create",
            "parameter y1 must be a number within -1..=2",
            "mask Mask 1 has one component; delete the mask rather than its last component",
            "mask Mask 1 begins with a intersect component; the first component of a mask is always add",
        ]
    );
    assert!(
        json_refusals
            .iter()
            .all(|error| error["code"] == json!("validation")),
        "every one of them is a validation refusal: {json_refusals:?}"
    );
    // The unknown mask names the identity it could not find.
    assert!(
        json_refusals[4]["detail"]
            .as_str()
            .unwrap()
            .starts_with("unknown mask mask-"),
        "{}",
        json_refusals[4]
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Refusals with any identity they name replaced by its prefix, so two independent runs of the same
/// journey can be compared on what they refused rather than on the uuid they happened to mint.
fn anonymous(refusals: &[Value]) -> Vec<Value> {
    refusals
        .iter()
        .map(|error| {
            let detail = error["detail"].as_str().unwrap();
            let anonymized = detail
                .split(' ')
                .map(|word| match word.split_once('-') {
                    Some((prefix @ ("mask" | "component"), _)) => prefix,
                    _ => word,
                })
                .collect::<Vec<_>>()
                .join(" ");
            json!({"code": error["code"], "detail": anonymized})
        })
        .collect()
}

#[test]
fn a_gradient_drag_is_one_entry_and_a_drag_that_returns_to_its_start_is_none() {
    let dir = temp("draft");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture(), &source).unwrap();
    let mut client = Json::open(&dir.join("draft.sqlite"), &source);
    let asset = client.asset.clone();
    client
        .run(
            "mask.create",
            &MaskTarget::default(),
            linear(0.0, 0.0, 0.0, 1.0),
            "create",
        )
        .expect("a mask to drag");
    let listing = client.list();
    let target = component_of(&listing, "Mask 1", "Linear 1");
    let begin = json!({
        "asset_id": asset,
        "action": "mask.set-component",
        "mask": target.mask.as_ref().unwrap().as_str(),
        "component": target.component.as_ref().unwrap().as_str(),
    });
    // One drag: pointer-down, many moves, one release, one entry.
    let draft = client.send("draft.begin", begin.clone()).expect("a draft");
    let draft_id = draft["draft_id"].clone();
    for y in [0.9, 0.8, 0.7, 0.6] {
        client
            .send(
                "draft.set",
                json!({"draft_id": draft_id, "fields": {"y1": y}}),
            )
            .expect("a drafted field");
    }
    let before = client.labels().len();
    let revision = client.revision();
    let committed = client
        .send(
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "drag")}),
        )
        .expect("a drafted commit");
    assert_eq!(committed["outcome"], json!("applied"));
    assert_eq!(committed["label"], json!("Update Linear 1"));
    let after = client.labels();
    assert_eq!(
        after.len(),
        before + 1,
        "one entry per gesture, never one per pointer move"
    );
    assert_eq!(
        client.list()["masks"][0]["components"][0]["payload"],
        json!({"x0": 0.0, "y0": 0.0, "x1": 0.0, "y1": 0.6}),
        "the drafted geometry is what committed"
    );
    // A drag that wanders and returns to where it began commits nothing and ends the draft anyway.
    let draft = client
        .send("draft.begin", begin)
        .expect("a second gesture may begin");
    let draft_id = draft["draft_id"].clone();
    for y in [0.4, 0.2, 0.6] {
        client
            .send(
                "draft.set",
                json!({"draft_id": draft_id, "fields": {"y1": y}}),
            )
            .expect("a drafted field");
    }
    let revision = client.revision();
    let nothing = client
        .send(
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "return")}),
        )
        .expect("a no-op commit still answers");
    assert_eq!(nothing["outcome"], json!("no-op"));
    assert_eq!(
        nothing["revision"],
        json!(revision),
        "the stack did not move"
    );
    assert!(
        nothing.get("label").is_none(),
        "a no-op wrote no entry, so there is no label: {nothing}"
    );
    assert_eq!(client.labels(), after, "no entry and no event");
    assert!(
        client
            .send("session.state", json!({}))
            .expect("the session answers")["draft"]
            .is_null(),
        "the gesture is over either way"
    );
    // A draft of a module action takes no mask target, and a rename is not a gesture.
    assert_eq!(
        client
            .send(
                "draft.begin",
                json!({"asset_id": asset, "action": "set-basic", "mask": target.mask.as_ref().unwrap().as_str()}),
            )
            .expect_err("a module action takes no target")["detail"],
        json!("action set-basic takes no mask target")
    );
    assert_eq!(
        client
            .send(
                "draft.begin",
                json!({"asset_id": asset, "action": "mask.rename", "mask": target.mask.as_ref().unwrap().as_str()}),
            )
            .expect_err("a rename is not a gesture")["detail"],
        json!("missing required field name for mask.rename")
    );
    drop(client);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_external_commit_conflicts_a_mask_draft_and_discard_or_reapply_resolves_it() {
    let dir = temp("conflict");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture(), &source).unwrap();
    let mut mine = Json::open(&dir.join("conflict.sqlite"), &source);
    let asset = mine.asset.clone();
    let other = mine.owner.register();
    mine.run(
        "mask.create",
        &MaskTarget::default(),
        linear(0.0, 0.0, 0.0, 1.0),
        "create",
    )
    .expect("a mask to drag");
    let listing = mine.list();
    let target = component_of(&listing, "Mask 1", "Linear 1");
    let begin = json!({
        "asset_id": asset,
        "action": "mask.set-component",
        "mask": target.mask.as_ref().unwrap().as_str(),
        "component": target.component.as_ref().unwrap().as_str(),
    });
    let draft = mine.send("draft.begin", begin.clone()).expect("a draft");
    let draft_id = draft["draft_id"].clone();
    let base = draft["base_revision"].as_u64().unwrap();
    mine.send(
        "draft.set",
        json!({"draft_id": draft_id, "fields": {"y1": 0.3}}),
    )
    .expect("a drafted field");
    // Another client commits under the draft.
    let amount = {
        let mut params = Map::new();
        params.insert("asset_id".into(), asset.clone());
        params.insert("mutation".into(), mutation(base, "external"));
        params.insert("mask".into(), json!(target.mask.as_ref().unwrap().as_str()));
        params.insert("amount".into(), json!(40));
        Value::Object(params)
    };
    Json::call(&mine.owner, other, "mask.set-amount", amount).expect("another client may commit");
    let read = mine
        .send("draft.read", json!({"draft_id": draft_id}))
        .expect("the draft is still held");
    assert_eq!(
        read["conflicted"],
        json!(true),
        "the asset moved under the draft"
    );
    let revision = mine.revision();
    assert_eq!(
        mine.send(
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "conflicted")}),
        )
        .expect_err("a conflicted draft does not commit")["detail"],
        json!("the asset changed under this draft; discard it or reapply it")
    );
    // Reapply rebases this client's own fields over what the other client committed.
    let reapplied = mine
        .send("draft.reapply", json!({"draft_id": draft_id}))
        .expect("a reapply");
    assert_eq!(reapplied["conflicted"], json!(false));
    assert_eq!(reapplied["base_revision"], json!(revision));
    let committed = mine
        .send(
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation(revision, "reapplied")}),
        )
        .expect("a reapplied commit");
    assert_eq!(committed["label"], json!("Update Linear 1"));
    let listed = mine.list();
    assert_eq!(
        listed["masks"][0]["amount"],
        json!(40.0),
        "the other client's amount survived"
    );
    assert_eq!(
        listed["masks"][0]["components"][0]["payload"]["y1"],
        json!(0.3),
        "and so did this client's own drafted field"
    );
    // Discard leaves the stack exactly as it is.
    let before = mine.labels();
    let draft = mine.send("draft.begin", begin).expect("a third gesture");
    let draft_id = draft["draft_id"].clone();
    mine.send(
        "draft.set",
        json!({"draft_id": draft_id, "fields": {"y1": 0.9}}),
    )
    .expect("a drafted field");
    assert_eq!(
        mine.send("draft.cancel", json!({"draft_id": draft_id}))
            .expect("a discard")["cancelled"],
        json!(true)
    );
    assert_eq!(mine.labels(), before, "a discarded draft wrote nothing");
    assert_eq!(
        mine.list()["masks"][0]["components"][0]["payload"]["y1"],
        json!(0.3)
    );
    drop(mine);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_retried_command_returns_its_original_result_and_a_reused_id_conflicts() {
    let dir = temp("retry");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture(), &source).unwrap();
    let mut client = Json::open(&dir.join("retry.sqlite"), &source);
    let asset = client.asset.clone();
    let created = client
        .run(
            "mask.create",
            &MaskTarget::default(),
            linear(0.0, 0.0, 0.0, 1.0),
            "once",
        )
        .expect("a new mask");
    let revision = client.revision();
    let listing = client.list();
    let sky = mask_of(&listing, "Mask 1");
    // Every mutation of the family deduplicates by the same input hash.
    for (method, target, parameters, request) in [
        (
            "mask.set-amount",
            sky.clone(),
            json!({"amount": 60}),
            "amount",
        ),
        (
            "mask.rename",
            MaskTarget {
                name: Some("Sky".into()),
                ..sky.clone()
            },
            Value::Null,
            "rename",
        ),
    ] {
        let mut params = match parameters.clone() {
            Value::Object(object) => object,
            _ => Map::new(),
        };
        params.insert("asset_id".into(), asset.clone());
        params.insert("mutation".into(), mutation(client.revision(), request));
        params.insert("mask".into(), json!(target.mask.as_ref().unwrap().as_str()));
        if let Some(name) = &target.name {
            params.insert("name".into(), json!(name));
        }
        let first = client
            .send(method, Value::Object(params.clone()))
            .expect("the command applies");
        let again = client
            .send(method, Value::Object(params.clone()))
            .expect("a retry answers");
        assert_eq!(again["outcome"], first["outcome"]);
        assert_eq!(again["revision"], first["revision"]);
        assert_eq!(again["current_entry_id"], first["current_entry_id"]);
        assert_eq!(again["created_entry_id"], first["created_entry_id"]);
        assert_eq!(
            again["label"], first["label"],
            "the stored label reads back"
        );
        assert_eq!(again["deduplicated"], json!(true));
        assert_eq!(first["deduplicated"], json!(false));
        // The same request id with different input is a conflict, and nothing is written.
        let before = client.labels();
        if target.name.is_some() {
            params.insert("name".into(), json!("Other"));
        } else {
            params.insert("amount".into(), json!(20));
        }
        let conflict = client
            .send(method, Value::Object(params))
            .expect_err("a reused request id with different input");
        assert_eq!(
            conflict["detail"],
            json!("request_id was already used with different input")
        );
        assert_eq!(client.labels(), before, "nothing was written");
    }
    // A retry of the creating command reads its mask and component back from its own entry.
    let mut params = Map::new();
    params.insert("asset_id".into(), asset.clone());
    params.insert("mutation".into(), mutation(0, "once"));
    for (name, value) in [
        ("kind", json!("linear")),
        ("x0", json!(0.0)),
        ("y0", json!(0.0)),
        ("x1", json!(0.0)),
        ("y1", json!(1.0)),
    ] {
        params.insert(name.into(), value);
    }
    let retried = client
        .send("mask.create", Value::Object(params))
        .expect("a retry of the create");
    assert_eq!(retried["mask"], created["mask"]);
    assert_eq!(retried["component"], created["component"]);
    assert_eq!(retried["label"], created["label"]);
    assert_eq!(retried["deduplicated"], json!(true));
    // A stale revision is refused before anything is planned.
    let mut params = Map::new();
    params.insert("asset_id".into(), asset);
    params.insert("mutation".into(), mutation(0, "stale"));
    params.insert("mask".into(), json!(sky.mask.as_ref().unwrap().as_str()));
    params.insert("invert".into(), json!(true));
    let current = client.revision();
    let stale = client
        .send("mask.set-invert", Value::Object(params))
        .expect_err("a stale revision");
    assert_eq!(
        stale["detail"],
        json!(format!("stale revision 0; current revision is {current}"))
    );
    assert!(current > revision, "the journey moved the asset on");
    drop(client);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn deleting_a_mask_carrying_layers_of_two_effects_says_what_it_removed() {
    let dir = temp("delete");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture(), &source).unwrap();
    let catalog = dir.join("delete.sqlite");
    // The `mask` target on a module action is not delivered yet, so the only way to hold a stack whose
    // layers are bound to a mask is to write one. It is a stack the host itself validates and
    // compiles on the way out, which is what the delete below proves.
    let (mask, unmasked, masked) = {
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&source).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let mut mask = Mask::new("Mask 1");
        let name = mask.next_component_name("linear");
        mask.components.push(Component::new(
            name,
            ComponentMode::Add,
            "linear",
            json!({"x0": 0.0, "y0": 0.0, "x1": 0.0, "y1": 1.0}),
        ));
        let layer = |effect: &str, bound: bool| Layer {
            id: LayerId::new(),
            effect_id: effect.to_owned(),
            effect_format: crate::EFFECT_FORMAT,
            payload: json!({}),
            mask: bound.then(|| mask.id.clone()),
        };
        let global = layer(crate::BASIC_EFFECT, false);
        let layers = vec![
            global.clone(),
            layer(crate::BASIC_EFFECT, true),
            layer(crate::PRESENCE_EFFECT, true),
        ];
        let masked: Vec<LayerId> = layers[1..].iter().map(|layer| layer.id.clone()).collect();
        let entry = HistoryEntry {
            id: EntryId::new(),
            sequence: state.current_entry.sequence + 1,
            label: "Mask 1 exposure +0.50".into(),
            undo_parent: Some(state.current_entry.id.clone()),
            base_revision: state.revision,
            result_revision: state.revision + 1,
            snapshot: Snapshot {
                id: SnapshotId::new(),
                asset_id: asset.clone(),
                recipe: Recipe {
                    format: crate::RECIPE_FORMAT,
                    layers,
                    masks: vec![mask.clone()],
                },
            },
            ..state.current_entry.clone()
        };
        drop(service);
        plant(&catalog, &entry);
        (mask, global.id, masked)
    };
    let mut client = Json::open(&catalog, &source);
    let listed = client.list();
    assert_eq!(
        listed["masks"][0]["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| layer["title"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Basic", "Presence"],
        "mask.list reports the layers bound to the mask"
    );
    let removed = client
        .run(
            "mask.delete",
            &MaskTarget {
                mask: Some(mask.id.clone()),
                ..MaskTarget::default()
            },
            Value::Null,
            "delete",
        )
        .expect("a destructive delete");
    assert_eq!(
        removed["label"],
        json!("Delete Mask 1 with Basic, Presence"),
        "the history label names exactly what went with the mask"
    );
    assert_eq!(
        removed["removed_layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| json!([layer["id"], layer["effect"], layer["title"]]))
            .collect::<Vec<_>>(),
        masked
            .iter()
            .zip([crate::BASIC_EFFECT, crate::PRESENCE_EFFECT])
            .map(|(id, effect)| {
                let title = if effect == crate::BASIC_EFFECT {
                    "Basic"
                } else {
                    "Presence"
                };
                json!([id.as_str(), effect, title])
            })
            .collect::<Vec<_>>(),
        "and the result names them by identity too"
    );
    assert!(
        client.list()["masks"].as_array().unwrap().is_empty(),
        "the mask is gone"
    );
    let described = client
        .send("recipe.describe", json!({"asset_id": client.asset.clone()}))
        .expect("the recipe answers");
    assert_eq!(
        described["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| layer["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        [unmasked.as_str().to_owned()],
        "the global layer of the same effect stayed exactly where it was"
    );
    // The entry that removed them keeps the complete stack that preceded it, so undo restores both.
    let revision = client.revision();
    client
        .send(
            "history.undo",
            json!({"asset_id": client.asset.clone(), "mutation": mutation(revision, "undo")}),
        )
        .expect("undo walks back one entry");
    assert_eq!(
        client.list()["masks"][0]["layers"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "originals are sacred and so is history: the removed layers come back"
    );
    drop(client);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Write one entry and make it current without going through a mutation, which is the only way to
/// hold a stack whose layers are bound to a mask until the `mask` target on module actions arrives.
fn plant(catalog: &Path, entry: &HistoryEntry) {
    let connection = Connection::open(catalog).unwrap();
    connection
        .execute(
            "INSERT INTO entries (id,asset_id,sequence,action_id,undo_parent_id,entry_json)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                entry.id.as_str(),
                entry.asset_id.as_str(),
                entry.sequence as i64,
                entry.action_id,
                entry.undo_parent.as_ref().map(EntryId::as_str),
                serde_json::to_string(entry).unwrap(),
            ],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE asset_state SET current_entry_id=?2, revision=?3 WHERE asset_id=?1",
            params![
                entry.asset_id.as_str(),
                entry.id.as_str(),
                entry.result_revision as i64
            ],
        )
        .unwrap();
}

#[test]
fn a_mask_command_and_a_module_action_can_never_collide() {
    // A mask command's identity carries a dot, which an action identity may not, so the two families
    // are disjoint by construction — and a module that declares one anyway is refused by name rather
    // than shadowing the host.
    let mut registry = crate::ModuleRegistry::builtin();
    let error = registry
        .register(std::sync::Arc::new(Colliding))
        .expect_err("a module may not declare a host mask command");
    assert_eq!(error.kind, ErrorKind::Validation);
    assert_eq!(
        error.detail,
        "test.collide declares mask.create, which is a host mask command"
    );
}

/// A module whose one action names a host mask command, which is the case the registry refuses.
struct Colliding;

impl crate::ToolModule for Colliding {
    fn descriptor(&self) -> &crate::ModuleDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<crate::ModuleDescriptor> =
            std::sync::LazyLock::new(|| crate::ModuleDescriptor {
                id: "test.collide".into(),
                title: "Collide".into(),
                hint: None,
                effects: Vec::new(),
                actions: vec![crate::ActionDescriptor {
                    id: "mask.create".into(),
                    title: "Create".into(),
                    notes: String::new(),
                    summary: None,
                    patch: false,
                    parameters: Vec::new(),
                }],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: true,
                collapsed: false,
                availability: crate::Availability::Available,
            });
        &DESCRIPTOR
    }
    fn parse(&self, _: &str, _: &Map<String, Value>) -> Result<crate::ActionInput, Error> {
        unreachable!("registration is refused before anything is parsed")
    }
    fn plan(
        &self,
        _: &crate::ActionInput,
        _: &crate::StageContext<'_>,
    ) -> Result<crate::ActionPlan, Error> {
        unreachable!("registration is refused before anything is planned")
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Ok(())
    }
    fn describe_layer(&self, _: &str, _: u32, _: &Value) -> Result<String, Error> {
        Ok(String::new())
    }
    fn compile(
        &self,
        _: &str,
        _: u32,
        _: &Value,
        _: crate::Stage,
    ) -> Result<crate::Processing, Error> {
        unreachable!("registration is refused before anything is compiled")
    }
}
