//! The range mask experiment through the catalog owner: with `masks.range-selections` off, a new
//! luminance or colour range — as a mask, as a component or as a draft's action — is refused as
//! `not-ready` naming the flag, while range components already stored still list, render, edit and
//! delete; on, read live from the person's preferences or forced by the host, both kinds are
//! created as before.
use super::*;
use crate::api::ApiFailure;
use crate::flags::RANGE_MASKS;
use crate::{editor::mutation_json, flags::LaunchFlags};
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};

struct Harness {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    asset: Value,
    revision: u64,
}

impl Harness {
    fn start(name: &str, launch_flags: LaunchFlags) -> Self {
        let dir = temp_dir(&format!("experiment-{name}"))
            .canonicalize()
            .unwrap();
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                preferences_dir: Some(dir.join("config")),
                launch_flags,
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

    /// One edit against the current revision, which it advances.
    fn edit(&mut self, method: &str, mut params: Value) -> ApiResponse {
        params["asset_id"] = self.asset.clone();
        params["mutation"] = mutation_json(self.revision, &format!("{method}-{}", self.revision));
        let response = self.send(method, params);
        if response.error.is_none() {
            self.revision += 1;
        }
        response
    }

    fn edited(&mut self, method: &str, params: Value) -> Value {
        let response = self.edit(method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    /// The edit's refusal, which must be the experiment's.
    fn refused(&mut self, method: &str, params: Value) -> ApiFailure {
        let failure = self.edit(method, params).error.expect("a refusal");
        assert_eq!(failure.code, "not-ready", "{method}: {failure:?}");
        assert_eq!(
            failure.data,
            Some(json!({"flag": RANGE_MASKS})),
            "{method}: {failure:?}"
        );
        assert!(failure.message.contains(RANGE_MASKS), "{failure:?}");
        failure
    }

    fn set_flag(&self, value: bool) {
        self.ok("flags.set", json!({"flag": RANGE_MASKS, "value": value}));
    }

    fn sample(&self) -> Value {
        self.ok(
            "render.sample",
            json!({"asset_id": self.asset, "x": 10, "y": 10}),
        )["rgba"]
            .clone()
    }

    fn stop(mut self) {
        self.owner.stop();
        self.join.take().unwrap().join().unwrap();
    }
}

const LUMINANCE: &str = "mask.create-luminance-range";
fn band() -> Value {
    json!({"low": 0.0, "low_feather": 0.0, "high": 100.0, "high_feather": 0.0})
}

#[test]
fn range_masks_are_created_only_with_the_experiment_on_and_existing_ones_keep_working() {
    let mut harness = Harness::start("live", LaunchFlags::default());

    // The flag is listed, off by default, and read each time it is needed.
    let listed = harness.ok("flags.list", json!({}));
    let flag = listed["flags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|flag| flag["id"] == RANGE_MASKS)
        .expect("the experiment flag is listed")
        .clone();
    assert_eq!(
        (
            &flag["kind"],
            &flag["default"],
            &flag["value"],
            &flag["applies"]
        ),
        (
            &json!("toggle"),
            &json!(false),
            &json!(false),
            &json!("live")
        )
    );
    // Discoverable before it is met: the schema says when the method is refused.
    let schema = harness.ok("schema.list", json!({}));
    assert!(schema.to_string().contains(RANGE_MASKS));

    // Off: a new mask, a draft of one and a new component of either kind are refused by name.
    harness.refused(LUMINANCE, band());
    harness.refused("mask.create-colour-range", json!({"refine": 50.0}));
    let draft = harness.send(
        "draft.begin",
        json!({"asset_id": harness.asset, "action": LUMINANCE}),
    );
    let draft = draft.error.expect("a draft of a new range mask is refused");
    assert_eq!(
        (draft.code.as_str(), draft.data),
        ("not-ready", Some(json!({"flag": RANGE_MASKS})))
    );
    let linear = harness.edited(
        "mask.create-linear",
        json!({"x0": 0.0, "y0": 0.0, "x1": 1.0, "y1": 1.0}),
    );
    harness.refused(
        "mask.add-luminance-range",
        json!({"mask": linear["mask"], "mode": "intersect", "low": 0.0, "low_feather": 0.0,
               "high": 100.0, "high_feather": 0.0}),
    );
    harness.refused(
        "mask.add-colour-range",
        json!({"mask": linear["mask"], "mode": "add", "refine": 50.0}),
    );

    // On: both kinds are created, and a layer applies through the range mask.
    harness.set_flag(true);
    let made = harness.edited(LUMINANCE, band());
    let mask = made["mask"].clone();
    let added = harness.edited(
        "mask.add-colour-range",
        json!({"mask": mask, "mode": "add", "refine": 50.0}),
    );
    harness.edited("edit.set-basic", json!({"mask": mask, "exposure": 1.0}));
    let rendered = harness.sample();

    // Off again: the stored range components still list, render and edit; only new ones wait.
    harness.set_flag(false);
    let listing = harness.ok("mask.list", json!({"asset_id": harness.asset}));
    let kinds: Vec<&Value> = listing["masks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|listed| listed["id"] == mask)
        .expect("the range mask is listed")["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|component| &component["kind"])
        .collect();
    assert_eq!(kinds, [&json!("luminance-range"), &json!("colour-range")]);
    assert_eq!(harness.sample(), rendered, "the range mask still renders");
    harness.edited(
        "mask.set-luminance-range",
        json!({"mask": mask, "component": made["component"], "low": 20.0}),
    );
    harness.edited(
        "mask.add-colour-range-sample",
        json!({"mask": mask, "component": added["component"], "r": 0.2, "g": 0.3, "b": 0.4}),
    );
    harness.refused(
        "mask.add-luminance-range",
        json!({"mask": mask, "mode": "add", "low": 0.0, "low_feather": 0.0,
               "high": 100.0, "high_feather": 0.0}),
    );
    harness.edited(
        "mask.delete-component",
        json!({"mask": mask, "component": added["component"]}),
    );
    harness.edited("mask.delete", json!({"mask": mask}));
    harness.stop();
}

#[test]
fn a_host_that_forces_the_experiment_on_creates_range_masks_whatever_is_stored() {
    let forced = LaunchFlags::resolve(None, &[(RANGE_MASKS, json!(true), "test")]);
    let mut harness = Harness::start("forced", forced);
    harness.set_flag(false);
    let listed = harness.ok("flags.list", json!({}));
    let flag = listed["flags"]
        .as_array()
        .unwrap()
        .iter()
        .find(|flag| flag["id"] == RANGE_MASKS)
        .unwrap()
        .clone();
    assert_eq!(
        (&flag["value"], &flag["active"], &flag["override"]),
        (&json!(false), &json!(true), &json!("test"))
    );
    harness.edited(LUMINANCE, band());
    harness.stop();
}

/// Picking edits a range component that already exists, so it runs whatever the experiment flag
/// says; on the owner each pick defers exactly one read to the tile service and replays once with
/// its answer, committing one entry, on a mask no layer is bound to. The area read a drag measures
/// is a query of its own that writes nothing.
#[test]
fn picks_run_with_the_experiment_off_and_each_defers_one_read() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let mut harness = Harness::start("pick", LaunchFlags::default());
    harness.set_flag(true);
    let made = harness.edited(LUMINANCE, band());
    let mask = made["mask"].clone();
    let colour = harness.edited(
        "mask.add-colour-range",
        json!({"mask": mask, "mode": "intersect", "refine": 50.0}),
    );
    harness.set_flag(false);

    let calls = Arc::new(AtomicU64::new(0));
    let counted = calls.clone();
    harness.owner.hold_tiles(Some(Arc::new(move || {
        counted.fetch_add(1, Ordering::Relaxed);
    })));
    let reads = || calls.load(Ordering::Relaxed);

    let clicked = harness.edited(
        "mask.pick-luminance-range",
        json!({"mask": mask, "component": made["component"], "x": 0.5, "y": 0.5}),
    );
    assert_eq!(clicked["label"], json!("Pick Luminance range 1"));
    assert_eq!(reads(), 1, "a click defers one read and replays once");
    let dragged = harness.edited(
        "mask.pick-colour-range",
        json!({"mask": mask, "component": colour["component"],
               "x": 0.25, "y": 0.25, "width": 0.5, "height": 0.5}),
    );
    assert_eq!(dragged["label"], json!("Pick Colour range 1"));
    assert_eq!(reads(), 2, "a drag defers one area read");
    harness.edited(
        "mask.pick-colour-range",
        json!({"mask": mask, "component": colour["component"], "x": 0.1, "y": 0.9, "add": true}),
    );
    assert_eq!(reads(), 3);

    let listing = harness.ok("mask.list", json!({"asset_id": harness.asset}));
    let components = listing["masks"][0]["components"].as_array().unwrap();
    assert_eq!(
        components[1]["payload"]["samples"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let band = &components[0]["payload"];
    assert_eq!(
        (&band["low_feather"], &band["high_feather"]),
        (&json!(10.0), &json!(10.0))
    );
    assert!(
        listing["masks"][0]["layers"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );

    // The area query is read-only: the revision does not move.
    let area = harness.ok(
        "mask.sample-input-area",
        json!({"asset_id": harness.asset, "mask": mask,
               "x": 0.25, "y": 0.25, "width": 0.5, "height": 0.5}),
    );
    assert_eq!(reads(), 4);
    assert!(area["points"].as_array().unwrap().len() <= crate::mask::commands::AREA_POINTS);
    assert_eq!(
        area["renderer"],
        json!({"record": "reference", "reason": null})
    );
    let state = harness.ok("asset.state", json!({"asset_id": harness.asset}));
    assert_eq!(state["revision"], json!(harness.revision));
    harness.owner.hold_tiles(None);
    harness.stop();
}
