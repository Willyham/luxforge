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
        "luxforge-mask-commands-{}-{}-{name}",
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
            let queued = Self::call(&owner, client, "catalog.import", json!({"path": source, "mutation": {"request_id": format!("import-{}", uuid::Uuid::new_v4().simple()), "actor": "test"}}))
                .expect("import queues");
            let id = queued["job_id"].as_str().unwrap().to_owned();
            loop {
                let status = Self::call(&owner, client, "job.status", json!({"job_id": id}))
                    .expect("a job this client owns");
                match status["status"].as_str() {
                    Some("ready") => break status["asset"]["asset"]["id"].clone(),
                    Some("queued" | "running") => {
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
        if let Some(stroke) = &target.stroke {
            params.insert("stroke".into(), json!(stroke.as_str()));
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
        // The same one action path a module action takes, with the same request an independent
        // client sends: the target's identities are declared parameters beside the values.
        self.service
            .run_action(
                &self.asset,
                Mutation {
                    expected_revision: revision,
                    request_id: request.to_owned(),
                    actor: "contracts".to_owned(),
                },
                method,
                target.request(parameters),
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
        ..MaskTarget::default()
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
        ..MaskTarget::default()
    }
}

fn linear(x0: f64, y0: f64, x1: f64, y1: f64) -> Value {
    json!({"x0": x0, "y0": y0, "x1": x1, "y1": y1})
}

fn radial(x: f64, y: f64, radius: f64, feather: f64) -> Value {
    json!({"x": x, "y": y, "radius_x": radius, "radius_y": radius, "angle": 0.0, "feather": feather})
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
                "mask.create-linear",
                &MaskTarget::default(),
                json!({"mode": "subtract", "x0": 0, "y0": 0, "x1": 0, "y1": 1}),
                "refused-mode",
            )
            .expect_err("a first component is always add"),
    );
    refusals.push(
        driver
            .run(
                "mask.create-linear",
                &MaskTarget::default(),
                linear(0.0, 0.0, 0.0, 3.0),
                "refused-range",
            )
            .expect_err("a stored position is bounded"),
    );
    // A radius is not a linear gradient's field, and a linear gradient's endpoint is not a radial's:
    // each generated method declares exactly its own kind's parameters, so the other kind's are
    // simply unknown to it rather than passed through a range that does not fit them.
    refusals.push(
        driver
            .run(
                "mask.create-linear",
                &MaskTarget::default(),
                json!({"x0": 0, "y0": 0, "x1": 0, "y1": 1, "radius_x": 0.4}),
                "refused-foreign",
            )
            .expect_err("a radius is not part of a linear gradient"),
    );
    refusals.push(
        driver
            .run(
                "mask.create-radial",
                &MaskTarget::default(),
                radial(0.5, 0.5, 0.0, 40.0),
                "refused-radius",
            )
            .expect_err("a radius takes the study's distance range"),
    );
    driver
        .run(
            "mask.create-linear",
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
        .run("mask.set-linear", &first, json!({"x1": 0.5}), "patch-1")
        .expect("a geometry patch");
    let repeated = driver
        .run("mask.set-linear", &first, json!({"x1": 0.5}), "patch-2")
        .expect("a patch that changes nothing");
    assert_eq!(
        repeated["outcome"],
        json!("no-op"),
        "setting a field to what it already holds writes no entry"
    );
    driver
        .run(
            "mask.add-linear",
            &sky,
            json!({"mode": "subtract", "x0": 0.1, "y0": 0.1, "x1": 0.9, "y1": 0.9}),
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
            "mask.add-linear",
            &mask_of(&listing, "Sky"),
            json!({"mode": "add", "x0": 0.2, "y0": 0.2, "x1": 0.8, "y1": 0.8}),
            "add-3",
        )
        .expect("a third component");
    // A radial joins the same mask, intersecting, and is then patched on a radius, an angle and a
    // feather — the three fields the delivered single geometry table could not express at all.
    let listing = driver.list();
    driver
        .run(
            "mask.add-radial",
            &mask_of(&listing, "Sky"),
            json!({"mode": "intersect", "x": 0.5, "y": 0.5, "radius_x": 0.3, "radius_y": 0.3,
                   "angle": 0.0, "feather": 40.0}),
            "add-radial",
        )
        .expect("a radial component of a mask that already holds two linear ones");
    let listing = driver.list();
    let ellipse = component_of(&listing, "Sky", "Radial 1");
    driver
        .run(
            "mask.set-radial",
            &ellipse,
            json!({"radius_y": 0.45, "angle": -30.0}),
            "patch-radial",
        )
        .expect("a radial geometry patch");
    driver
        .run(
            "mask.set-component-invert",
            &ellipse,
            json!({"invert": true}),
            "invert-radial",
        )
        .expect("a radial inversion, after the fact");
    // One kind's patch may not reach another kind's component, and the refusal names both.
    refusals.push(
        driver
            .run(
                "mask.set-radial",
                &component_of(&listing, "Sky", "Linear 1"),
                json!({"radius_x": 0.2}),
                "refused-mismatch",
            )
            .expect_err("a radial's patch on a linear component"),
    );
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
            "Sky · Add intersect radial",
            "Sky · Update Radial 1",
            "Sky · Radial 1 inverted",
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
            "mask.create-linear",
            "mask.set-linear",
            "mask.add-linear",
            "mask.set-component-mode",
            "mask.set-component-invert",
            "mask.set-amount",
            "mask.set-invert",
            "mask.rename",
            "mask.duplicate",
            "mask.reorder",
            "mask.delete-component",
            "mask.add-linear",
            "mask.add-radial",
            "mask.set-radial",
            "mask.set-component-invert",
            "mask.delete"
        ]
    );
    // One mask survives: the renamed original, with its amount, inversion and three components of
    // two kinds. The duplicate — which was the one holding `Mask 1` after the rename — was deleted.
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
                {"index": 2, "name": "Radial 1", "mode": "intersect", "invert": true,
                 "kind": "radial", "payload": {"x": 0.5, "y": 0.5, "radius_x": 0.3,
                 "radius_y": 0.45, "angle": -30.0, "feather": 40.0},
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
            .take(7)
            .collect::<Vec<_>>(),
        [
            "unknown parameter mode for action mask.create-linear",
            "parameter y1 must be a number within -1..=2",
            // One kind's field is not a parameter of another kind's method at all, which is what
            // generating a method per kind buys: a radius cannot arrive on a linear gradient, and a
            // radius that does arrive takes the study's distance range rather than a position's.
            "unknown parameter radius_x for action mask.create-linear",
            "parameter radius_x must be a number within 0.0001..=64",
            "mask Mask 1 has one component; delete the mask rather than its last component",
            "mask Mask 1 begins with a intersect component; the first component of a mask is always add",
            "component Linear 1 is a linear component; patch it with mask.set-linear",
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
        json_refusals[7]["detail"]
            .as_str()
            .unwrap()
            .starts_with("unknown mask mask-"),
        "{}",
        json_refusals[7]
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
            "mask.create-linear",
            &MaskTarget::default(),
            linear(0.0, 0.0, 0.0, 1.0),
            "create",
        )
        .expect("a mask to drag");
    let listing = client.list();
    let target = component_of(&listing, "Mask 1", "Linear 1");
    let begin = json!({
        "asset_id": asset,
        "action": "mask.set-linear",
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
    // A draft of a maskable module action takes the mask — that is what makes a masked slider
    // preview what it will commit — but never a component, and an action of a module with no
    // maskable effect takes neither.
    let mask_id = target.mask.as_ref().unwrap().as_str().to_owned();
    let opened = client
        .send(
            "draft.begin",
            json!({"asset_id": asset, "action": "set-basic", "mask": mask_id}),
        )
        .expect("a maskable module action drafts through a mask");
    assert_eq!(opened["target"]["mask"], json!(mask_id));
    assert!(
        opened["target"].get("component").is_none(),
        "a module action's draft carries the mask alone: {opened}"
    );
    client
        .send("draft.cancel", json!({"draft_id": opened["draft_id"]}))
        .expect("the gesture ends");
    assert_eq!(
        client
            .send(
                "draft.begin",
                json!({"asset_id": asset, "action": "set-basic", "mask": mask_id, "component": target.component.as_ref().unwrap().as_str()}),
            )
            .expect_err("a module edits through the whole mask")["detail"],
        json!("action set-basic takes no mask component")
    );
    assert_eq!(
        client
            .send(
                "draft.begin",
                json!({"asset_id": asset, "action": "crop", "mask": mask_id}),
            )
            .expect_err("an unmaskable action takes no target")["detail"],
        json!("action crop does not accept a mask target")
    );
    // A gesture names every object its command addresses when it begins: a patch without its
    // component, and a stroke deletion — whose stroke a draft cannot carry — are refused there.
    assert_eq!(
        client
            .send(
                "draft.begin",
                json!({"asset_id": asset, "action": "mask.set-linear", "mask": target.mask.as_ref().unwrap().as_str()}),
            )
            .expect_err("a patch addresses a component")["detail"],
        json!("missing required parameter component for action mask.set-linear")
    );
    assert_eq!(
        client
            .send(
                "draft.begin",
                json!({"asset_id": asset, "action": "mask.delete-stroke", "mask": target.mask.as_ref().unwrap().as_str(), "component": target.component.as_ref().unwrap().as_str()}),
            )
            .expect_err("a stroke deletion is not a gesture")["detail"],
        json!("missing required parameter stroke for action mask.delete-stroke")
    );
    assert_eq!(
        client
            .send(
                "draft.begin",
                json!({"asset_id": asset, "action": "mask.create-linear", "mask": target.mask.as_ref().unwrap().as_str()}),
            )
            .expect_err("a create addresses no mask")["detail"],
        json!("unknown parameter mask for action mask.create-linear")
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
        "mask.create-linear",
        &MaskTarget::default(),
        linear(0.0, 0.0, 0.0, 1.0),
        "create",
    )
    .expect("a mask to drag");
    let listing = mine.list();
    let target = component_of(&listing, "Mask 1", "Linear 1");
    let begin = json!({
        "asset_id": asset,
        "action": "mask.set-linear",
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
            "mask.create-linear",
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
        ("x0", json!(0.0)),
        ("y0", json!(0.0)),
        ("x1", json!(0.0)),
        ("y1", json!(1.0)),
    ] {
        params.insert(name.into(), value);
    }
    let retried = client
        .send("mask.create-linear", Value::Object(params))
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
            artifacts: Vec::new(),
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
                    ..Recipe::default()
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

    // And the other half of the relation, over the restored stack: a duplicate copies the layers
    // bound to the mask as well as its components, because a mask without its adjustments is not a
    // useful copy. The copies are legal because `single_layer` is per target and the two masks are
    // two targets, and the whole stack compiles on the way out — which is what `mask.list`
    // answering at all proves.
    client
        .run(
            "mask.duplicate",
            &MaskTarget {
                mask: Some(mask.id.clone()),
                ..MaskTarget::default()
            },
            Value::Null,
            "duplicate",
        )
        .expect("a duplicate of a mask that holds layers");
    let listed = client.list();
    let masks = listed["masks"].as_array().expect("two masks");
    assert_eq!(masks.len(), 2);
    for report in masks {
        assert_eq!(
            report["layers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|layer| layer["title"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["Basic", "Presence"],
            "each mask holds its own copy of the adjustments: {report}"
        );
    }
    assert_ne!(
        masks[0]["layers"][0]["id"], masks[1]["layers"][0]["id"],
        "a copied layer takes a new identity"
    );
    // The durable processing order follows the ordering rule: the global layer first, then each
    // effect's masked layers in the order the mask list shows.
    let described = client
        .send("recipe.describe", json!({"asset_id": client.asset.clone()}))
        .expect("the recipe answers");
    let order: Vec<Value> = described["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|layer| json!([layer["effect"], layer["mask"]]))
        .collect();
    assert_eq!(
        order,
        json!([
            [crate::BASIC_EFFECT, Value::Null],
            [crate::BASIC_EFFECT, masks[0]["id"]],
            [crate::BASIC_EFFECT, masks[1]["id"]],
            [crate::PRESENCE_EFFECT, masks[0]["id"]],
            [crate::PRESENCE_EFFECT, masks[1]["id"]],
        ])
        .as_array()
        .unwrap()
        .clone(),
        "each copy sits after the layer it was copied from, which is the ordering rule"
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
            "INSERT INTO entries (id,asset_id,sequence,action_id,label,actor,timestamp_ms,
                                  undo_parent_id,restore_target_id,entry_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                entry.id.as_str(),
                entry.asset_id.as_str(),
                entry.sequence as i64,
                entry.action_id,
                entry.label,
                entry.actor,
                entry.timestamp_ms,
                entry.undo_parent.as_ref().map(EntryId::as_str),
                entry.restore_target.as_ref().map(EntryId::as_str),
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

/// An independent client discovers the mask family exactly as it discovers a module's actions: from
/// `module.list`, where the host publishes one descriptor in the module shape, whose actions and
/// queries are the methods `schema.list` lists with the same parameters — the identities among them,
/// of the identity kind — and whose identities are refused by the one generic check, from either
/// side, with the same words.
#[test]
fn the_mask_family_is_discovered_as_a_host_descriptor_and_checked_like_any_action() {
    let dir = temp("discovery");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture(), &source).unwrap();
    let mut client = Json::open(&dir.join("json.sqlite"), &source);
    let listed = client
        .send("module.list", json!({}))
        .expect("module.list answers");
    let host = listed["host"].as_array().expect("the host's descriptors");
    assert_eq!(host.len(), 1);
    let masks = &host[0];
    assert_eq!(masks["id"], json!(commands::HOST_MODULE));
    assert!(
        listed["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|module| module["id"] != json!(commands::HOST_MODULE)),
        "the host is listed beside the modules, not as one"
    );
    let schema = client
        .send("schema.list", json!({}))
        .expect("schema.list answers");
    let methods = &schema["methods"];
    let actions = masks["actions"].as_array().unwrap();
    assert_eq!(actions.len(), commands::all().len());
    for action in actions.iter().chain(masks["queries"].as_array().unwrap()) {
        let method = action["id"].as_str().unwrap();
        assert_eq!(
            methods[method]["parameters"], action["parameters"],
            "{method} is listed with the parameters its descriptor declares"
        );
    }
    assert_eq!(schema["host"], listed["host"]);
    // The identities are declared parameters of the identity kind, first and in a fixed order.
    let patch = actions
        .iter()
        .find(|action| action["id"] == json!("mask.set-linear"))
        .unwrap();
    assert_eq!(
        patch["parameters"][0],
        json!({"name": "mask", "kind": "identity", "of": "mask", "required": true,
               "default": null, "unit": null, "step": null, "precision": null,
               "notes": "the mask this command addresses"})
    );
    assert_eq!(patch["parameters"][1]["of"], json!("component"));
    // A maskable module action declares its host `mask` field as the same kind, as its target.
    assert_eq!(
        methods["edit.set-basic"]["target"]["kind"],
        json!("identity")
    );
    assert_eq!(methods["edit.set-basic"]["target"]["of"], json!("mask"));

    // One generic check refuses a malformed identity, from an independent client and from inside,
    // on a mask command and on a masked module action alike.
    let wrong = ComponentId::new();
    let revision = client.revision();
    let json_refusals = [
        client
            .send(
                "mask.set-amount",
                json!({"asset_id": client.asset, "mutation": mutation(revision, "a"),
                       "mask": wrong.as_str(), "amount": 20}),
            )
            .expect_err("a component is not a mask"),
        client
            .send(
                "edit.set-basic",
                json!({"asset_id": client.asset, "mutation": mutation(revision, "b"),
                       "mask": wrong.as_str(), "exposure": 0.5}),
            )
            .expect_err("a component is not a mask"),
    ];
    drop(client);
    let mut direct = Direct::open(&dir.join("direct.sqlite"), &source);
    let direct_refusals = ["mask.set-amount", "set-basic"].map(|action| {
        let parameters = match action {
            "mask.set-amount" => json!({"mask": wrong.as_str(), "amount": 20}),
            _ => json!({"mask": wrong.as_str(), "exposure": 0.5}),
        };
        let error = direct
            .service
            .run_action(
                &direct.asset,
                Mutation {
                    expected_revision: 1,
                    request_id: action.to_owned(),
                    actor: "contracts".to_owned(),
                },
                action,
                parameters,
            )
            .expect_err("a component is not a mask");
        json!({"code": error.kind.code(), "detail": error.detail})
    });
    for refusals in [&json_refusals, &direct_refusals] {
        for refusal in refusals {
            assert_eq!(
                refusal,
                &json!({"code": "validation", "detail": "parameter mask must be a mask identity"})
            );
        }
    }
    drop(direct);
    std::fs::remove_dir_all(dir).unwrap();
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
        "test.collide declares mask.create-linear, which is a host mask command"
    );
}

/// A module whose one action names a host mask command — a *generated* one, because a generated
/// geometry method carries the same dotted identity every other command does and is protected by the
/// same check.
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
                    id: "mask.create-linear".into(),
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
                layout: crate::ModuleLayout::Stacked,
                availability: crate::Availability::Available,
                ..crate::ModuleDescriptor::default()
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

/// One painted stroke as a request: the path and the brush it was drawn with.
fn stroke(points: Value, size: f64, feather: f64, flow: f64, erase: bool) -> Value {
    json!({"points": points, "size": size, "feather": feather, "flow": flow, "erase": erase})
}

/// The strokes one component holds, in stored order, by content address.
fn strokes_of(listing: &Value, mask: &str, component: &str) -> Vec<String> {
    listing["masks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["name"] == json!(mask))
        .unwrap_or_else(|| panic!("no mask named {mask} in {listing}"))["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["name"] == json!(component))
        .unwrap_or_else(|| panic!("no component named {component}"))["payload"]["strokes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|address| address.as_str().unwrap().to_owned())
        .collect()
}

/// One stroke's content address as a target's `stroke` field.
fn at(target: &MaskTarget, address: &str) -> MaskTarget {
    MaskTarget {
        stroke: Some(crate::path::StrokeId::parse(address.to_owned()).unwrap()),
        ..target.clone()
    }
}

/// The whole brush journey, run once per driver: the first stroke draws a mask, later strokes update
/// the component it made, a second brush joins the same mask in the mode it was given, and one
/// stroke is deleted from the middle.
fn painted(driver: &mut dyn Driver) -> (Value, Vec<Value>, Vec<Value>) {
    let mut refusals = Vec::new();
    // The first stroke on nothing: a mask, a brush component and the stroke, as one entry.
    driver
        .run(
            commands::ADD_STROKE,
            &MaskTarget::default(),
            stroke(
                json!([[0.2, 0.2], [0.4, 0.4], [0.6, 0.4]]),
                0.1,
                50.0,
                100.0,
                false,
            ),
            "paint-1",
        )
        .expect("the first stroke draws a mask");
    let listing = driver.list();
    let brush = component_of(&listing, "Mask 1", "Brush 1");
    // Every later stroke on that component is one entry of its own, which is what makes undo walk
    // back one stroke at a time.
    for (index, path) in [json!([[0.3, 0.7], [0.5, 0.7]]), json!([[0.7, 0.2]])]
        .into_iter()
        .enumerate()
    {
        driver
            .run(
                commands::ADD_STROKE,
                &brush,
                stroke(path, 0.05, 20.0, 60.0, false),
                &format!("paint-more-{index}"),
            )
            .expect("a further stroke");
    }
    // A second brush on the same mask, in the mode the gesture chose before it started.
    let mask = mask_of(&driver.list(), "Mask 1");
    let mut subtract = stroke(json!([[0.5, 0.5], [0.55, 0.55]]), 0.08, 0.0, 100.0, false);
    subtract
        .as_object_mut()
        .unwrap()
        .insert("mode".into(), json!("subtract"));
    driver
        .run(commands::ADD_STROKE, &mask, subtract, "paint-subtract")
        .expect("a subtract brush");
    // An erase stroke inside the first brush: a property of the stroke, not of the component.
    driver
        .run(
            commands::ADD_STROKE,
            &brush,
            stroke(json!([[0.35, 0.35], [0.45, 0.4]]), 0.04, 30.0, 100.0, true),
            "paint-erase",
        )
        .expect("an erase stroke");
    let held = strokes_of(&driver.list(), "Mask 1", "Brush 1");
    // A forward edit: one entry appended, the named stroke gone, every other stroke where it was.
    driver
        .run(
            commands::DELETE_STROKE,
            &at(&brush, &held[1]),
            Value::Null,
            "unpaint",
        )
        .expect("a stroke is deleted");
    let after = strokes_of(&driver.list(), "Mask 1", "Brush 1");
    assert_eq!(
        after,
        [held[0].clone(), held[2].clone(), held[3].clone()],
        "only the named stroke goes and the rest keep their order"
    );

    // A stroke appended to a component that exists takes no mode: a component's mode is changed by
    // the command that changes one, and an ignored field is never an answer.
    let mut moded = stroke(json!([[0.1, 0.1]]), 0.05, 10.0, 50.0, false);
    moded
        .as_object_mut()
        .unwrap()
        .insert("mode".into(), json!("intersect"));
    refusals.push(
        driver
            .run(commands::ADD_STROKE, &brush, moded, "refuse-mode")
            .expect_err("a mode on an appended stroke"),
    );
    // A gradient is not painted on: its geometry is declared, and the refusal names the method that
    // does edit it.
    driver
        .run(
            "mask.create-linear",
            &MaskTarget::default(),
            linear(0.0, 0.0, 0.0, 1.0),
            "a-gradient",
        )
        .expect("a gradient mask");
    let gradient = component_of(&driver.list(), "Mask 2", "Linear 1");
    refusals.push(
        driver
            .run(
                commands::ADD_STROKE,
                &gradient,
                stroke(json!([[0.5, 0.5]]), 0.05, 10.0, 50.0, false),
                "refuse-kind",
            )
            .expect_err("a stroke on a gradient"),
    );
    // A stroke the component does not hold, and the last stroke of a component, are both named
    // refusals rather than something approximate.
    refusals.push(
        driver
            .run(
                commands::DELETE_STROKE,
                &at(&brush, &"0".repeat(32)),
                Value::Null,
                "refuse-absent",
            )
            .expect_err("a stroke that is not there"),
    );
    let two = component_of(&driver.list(), "Mask 1", "Brush 2");
    let only = strokes_of(&driver.list(), "Mask 1", "Brush 2");
    refusals.push(
        driver
            .run(
                commands::DELETE_STROKE,
                &at(&two, &only[0]),
                Value::Null,
                "refuse-last",
            )
            .expect_err("a component's only stroke"),
    );
    (shape(&driver.list()), driver.labels(), refusals)
}

/// The brush's two commands, end to end and identically from both clients: the design's history
/// granularity, the strokes-by-address payload, the forward delete and every refusal it states.
#[test]
fn painting_is_one_entry_a_stroke_and_a_delete_is_a_forward_edit() {
    let dir = temp("brush");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture(), &source).unwrap();
    let (json_shape, json_labels, json_refusals) = {
        let mut driver = Json::open(&dir.join("json.sqlite"), &source);
        painted(&mut driver)
    };
    let (direct_shape, direct_labels, direct_refusals) = {
        let mut driver = Direct::open(&dir.join("direct.sqlite"), &source);
        painted(&mut driver)
    };
    assert_eq!(json_shape, direct_shape, "the same stack either way");
    assert_eq!(json_labels, direct_labels, "the same history either way");
    assert_eq!(
        anonymous(&json_refusals),
        anonymous(&direct_refusals),
        "the same refusals either way"
    );

    // The design's granularity table, in the order the journey painted it. A mask's name prefixes a
    // label once the stack holds more than one, which is the delivered rule and not the brush's.
    let painted_entries: Vec<Value> = json_labels
        .iter()
        .filter(|entry| entry[0].as_str().unwrap().starts_with("mask."))
        .map(|entry| entry[1].clone())
        .collect();
    assert_eq!(
        painted_entries,
        [
            json!("Add brush"),
            json!("Update Brush 1"),
            json!("Update Brush 1"),
            json!("Add subtract brush"),
            json!("Update Brush 1"),
            json!("Delete a stroke from Brush 1"),
            // The delivered label rule, not the brush's: once the stack holds a second mask, every
            // row names the mask it belongs to.
            json!("Mask 2 · Add linear"),
        ],
        "one entry a stroke, named for what that stroke did"
    );
    // No coordinate is ever written into a component payload: a payload holds addresses and nothing
    // else, which is what keeps one entry a stroke from copying every earlier stroke.
    let payload = &json_shape[0]["components"][0]["payload"];
    assert_eq!(
        payload.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["strokes"],
        "a brush payload carries the reserved strokes field and nothing else"
    );
    assert!(
        payload["strokes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|address| address.as_str().is_some_and(|text| text.len() == 32)),
        "every stroke is a content address, never a position"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Decimation happens where the store says it does, and it is idempotent, so a desktop that
/// decimates before it posts and an agent that posts the path it captured reach the same stored
/// stroke — the same address, and therefore the same coverage.
#[test]
fn a_decimated_path_and_the_path_it_came_from_are_the_same_stored_stroke() {
    let dir = temp("decimation");
    let source = dir.join("orientation-1.jpg");
    std::fs::copy(fixture(), &source).unwrap();
    // A path a pointer produces: many positions along a straight run, which two grid steps of
    // tolerance reduce to its two ends.
    let captured: Vec<[f64; 2]> = (0..=64)
        .map(|step| [0.2 + f64::from(step) * 0.005, 0.3])
        .collect();
    let decimated = crate::path::decimate(&captured).unwrap();
    assert_eq!(decimated.len(), 2, "a straight run keeps its ends");
    let address = |path: &[[f64; 2]], request: &str| -> String {
        let mut driver = Direct::open(&dir.join(format!("{request}.sqlite")), &source);
        driver
            .run(
                commands::ADD_STROKE,
                &MaskTarget::default(),
                stroke(json!(path), 0.1, 50.0, 100.0, false),
                request,
            )
            .expect("a stroke");
        strokes_of(&driver.list(), "Mask 1", "Brush 1")[0].clone()
    };
    assert_eq!(
        address(&captured, "raw"),
        address(&decimated, "decimated"),
        "the same path, however much of it the client posted"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
