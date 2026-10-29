//! The grain experiment through the catalog owner (`docs/design/settings-and-flags.md`, "Gated
//! grain"): with `grain` off, `edit.set-grain`, a draft of it and a settings set carrying it —
//! a preset applied, settings pasted, a batch or a draft's set — are refused as `not-ready` naming
//! the flag and write nothing, while `edit.reset-grain`, undo and a stored grain layer's render are
//! the same whatever the flag says.
use super::*;
use crate::api::ApiFailure;
use crate::editor::mutation_json;
use crate::flags::{GRAIN, LaunchFlags};
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};

struct Harness {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    asset: Value,
    revision: u64,
}

impl Harness {
    fn start(name: &str) -> Self {
        let dir = temp_dir(&format!("grain-flag-{name}"))
            .canonicalize()
            .unwrap();
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                preferences_dir: Some(dir.join("config")),
                launch_flags: LaunchFlags::default(),
                ..HostConfig::unconfigured()
            },
        )
        .unwrap();
        let client = owner.register();
        let asset =
            super::library::opening::open(&owner, client, &fixture())["asset"]["id"].clone();
        Self {
            owner,
            join: Some(join),
            client,
            asset,
            revision: 0,
        }
    }

    fn send(&self, method: &str, params: Value) -> ApiResponse {
        self.owner
            .call(
                self.client,
                ApiRequest {
                    id: method.into(),
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

    /// One edit against the current revision, which follows its answer.
    fn edit(&mut self, method: &str, mut params: Value) -> ApiResponse {
        params["asset_id"] = self.asset.clone();
        params["mutation"] = mutation_json(self.revision, &format!("{method}-{}", self.revision));
        let response = self.send(method, params);
        if let Some(revision) = response
            .result
            .as_ref()
            .and_then(|result| result["revision"].as_u64())
        {
            self.revision = revision;
        }
        response
    }

    fn edited(&mut self, method: &str, params: Value) -> Value {
        let response = self.edit(method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn set_flag(&self, value: bool) {
        self.ok("flags.set", json!({"flag": GRAIN, "value": value}));
    }

    /// What the photograph holds now: its revision as the owner reads it, its described stack
    /// and its rendered bytes at a spread of points, each read through the owner.
    fn picture(&self) -> (Value, Value, Vec<Value>) {
        let state = self.ok("asset.state", json!({"asset_id": self.asset}));
        let described = self.ok("recipe.describe", json!({"asset_id": self.asset}));
        let samples = [(3, 3), (40, 20), (90, 60), (150, 100), (210, 140)]
            .into_iter()
            .map(|(x, y)| {
                self.ok(
                    "render.sample",
                    json!({"asset_id": self.asset, "x": x, "y": y}),
                )["rgba"]
                    .clone()
            })
            .collect();
        (
            state["revision"].clone(),
            described["layers"].clone(),
            samples,
        )
    }

    fn stop(mut self) {
        self.owner.stop();
        self.join.take().unwrap().join().unwrap();
    }
}

/// The refusal must be the grain experiment's: `not-ready`, naming the flag in its words and its
/// data.
fn assert_gated(what: &str, failure: Option<ApiFailure>) {
    let failure = failure.unwrap_or_else(|| panic!("{what} is refused while grain is off"));
    assert_eq!(
        (failure.code.as_str(), failure.data.clone()),
        ("not-ready", Some(json!({"flag": GRAIN}))),
        "{what}: {failure:?}"
    );
    assert!(
        failure.message.contains("flag grain"),
        "{what}: {failure:?}"
    );
}

#[test]
fn grain_is_added_only_with_the_flag_on_and_stored_grain_renders_and_resets_either_way() {
    let mut harness = Harness::start("live");

    // Listed, off by default and live; the module and its patch say what they are offered under.
    let listed = harness.ok("flags.list", json!({}));
    let flag = listed["flags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|flag| flag["id"] == GRAIN)
        .expect("the grain flag is listed")
        .clone();
    assert_eq!(
        (
            &flag["title"],
            &flag["kind"],
            &flag["default"],
            &flag["value"],
            &flag["applies"]
        ),
        (
            &json!("Grain"),
            &json!("toggle"),
            &json!(false),
            &json!(false),
            &json!("live")
        )
    );
    let schema = harness.ok("schema.list", json!({}));
    assert_eq!(schema["methods"]["edit.set-grain"]["flags"], json!([GRAIN]));
    assert!(
        schema["methods"]["edit.set-grain"]["notes"]
            .as_str()
            .unwrap()
            .contains("Offered only while the experiment flag grain is on")
    );
    assert!(schema["methods"]["edit.reset-grain"].get("flags").is_none());
    let modules = harness.ok("module.list", json!({}));
    let grain = modules["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == "luxforge.grain")
        .expect("the Grain module is listed with the flag off");
    assert_eq!(grain["flags"], json!([GRAIN]));

    // Off: every way of adding grain is refused by name, and none writes anything.
    let before = harness.picture();
    let grain_set = json!({"set-grain": {"amount": 30}});
    let refusals = [
        ("edit.set-grain", json!({"amount": 40.0})),
        (
            "edit.apply-settings",
            json!({"settings": grain_set, "origin": {"kind": "preset", "name": "Film"}}),
        ),
        (
            "edit.apply-settings",
            json!({"settings": {"set-basic": {"exposure": 0.5}, "set-grain": {"amount": 30}},
                   "origin": {"kind": "paste", "source": "DSC_0001.NEF"}}),
        ),
    ];
    for (method, params) in refusals {
        assert_gated(method, harness.edit(method, params).error);
    }
    assert_gated(
        "draft.begin of set-grain",
        harness
            .send(
                "draft.begin",
                json!({"asset_id": harness.asset, "action": "set-grain"}),
            )
            .error,
    );
    assert_gated(
        "batch.apply-settings",
        harness
            .send(
                "batch.apply-settings",
                json!({"targets": {"kind": "assets", "asset_ids": [harness.asset]},
                       "settings": grain_set, "origin": {"kind": "paste", "source": "a.jpg"},
                       "mutation": {"request_id": "batch", "actor": "test"}}),
            )
            .error,
    );
    // A preset's hover preview opens; the set it would preview and commit is refused.
    let draft = harness.ok(
        "draft.begin",
        json!({"asset_id": harness.asset, "action": "apply-settings"}),
    );
    assert_gated(
        "draft.set of a settings set carrying grain",
        harness
            .send(
                "draft.set",
                json!({"draft_id": draft["draft_id"], "fields": {
                    "settings": grain_set, "origin": {"kind": "preset", "name": "Film"}}}),
            )
            .error,
    );
    harness.ok("draft.cancel", json!({"draft_id": draft["draft_id"]}));
    assert_eq!(harness.picture(), before, "nothing was written");
    // Removing grain is not offering it.
    harness.edited("edit.reset-grain", json!({}));

    // On: grain is added, and a draft of it commits.
    harness.set_flag(true);
    harness.edited("edit.set-grain", json!({"amount": 60.0, "size": 10.0}));
    let grained = harness.picture();
    assert_ne!(grained.2, before.2, "the grain renders");

    // Off again: the stored layer renders, samples and describes exactly as it did, a draft of it
    // begun while on is refused at its commit, and reset and undo still run.
    let draft = harness.ok(
        "draft.begin",
        json!({"asset_id": harness.asset, "action": "set-grain"}),
    );
    harness.ok(
        "draft.set",
        json!({"draft_id": draft["draft_id"], "fields": {"amount": 20.0}}),
    );
    harness.set_flag(false);
    assert_eq!(harness.picture(), grained, "a flag never changes pixels");
    assert_gated(
        "draft.commit of set-grain",
        harness
            .send(
                "draft.commit",
                json!({"draft_id": draft["draft_id"],
                       "mutation": mutation_json(harness.revision, "commit")}),
            )
            .error,
    );
    harness.ok("draft.cancel", json!({"draft_id": draft["draft_id"]}));
    assert_eq!(harness.picture(), grained);
    harness.edited("edit.reset-grain", json!({}));
    assert_eq!(harness.picture().2, before.2, "the reset removes the grain");
    harness.edited("history.undo", json!({}));
    assert_eq!(
        harness.picture().1,
        grained.1,
        "undo replays the stored grain"
    );
    assert_eq!(harness.picture().2, grained.2);
    harness.stop();
}
