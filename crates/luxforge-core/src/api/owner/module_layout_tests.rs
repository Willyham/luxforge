//! The module layout through the catalog owner: `module.layout.read`, the four writes and their
//! `module.layout` events, `module.list`'s new fields, what the preferences file stores, a stored
//! layout this build cannot read, and the layout changing neither `schema.list` nor a render.
//! The layout's rules on their own are `crate::module_layout`'s tests.
use super::*;
use crate::api::ApiFailure;
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};
use std::fs;

struct Harness {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    dir: PathBuf,
}

impl Harness {
    /// An owner over a fresh catalog with the person's preferences in `<dir>/config`, or none.
    fn start(name: &str, preferences: bool) -> Self {
        let dir = temp_dir(&format!("module-layout-{name}"))
            .canonicalize()
            .unwrap();
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                preferences_dir: preferences.then(|| dir.join("config")),
                ..HostConfig::unconfigured()
            },
        )
        .unwrap();
        let client = owner.register();
        Self {
            owner,
            join: Some(join),
            client,
            dir,
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("config").join("preferences.json")
    }

    fn send(&self, id: &str, method: &str, params: Value) -> ApiResponse {
        self.owner
            .call(
                self.client,
                ApiRequest {
                    id: id.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(&self, id: &str, method: &str, params: Value) -> Value {
        let response = self.send(id, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn refused(&self, id: &str, method: &str, params: Value) -> ApiFailure {
        self.send(id, method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to be refused"))
    }

    /// The request ids announced as `module.layout`, in order.
    fn announced(&self) -> Vec<String> {
        let events = self.ok("events", "events.since", json!({"after": 0}));
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["method"] == "module.layout")
            .map(|event| event["request_id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// `module.list`'s `enabled` for each module, by id.
    fn enabled(&self) -> Vec<(String, bool)> {
        self.ok("list", "module.list", json!({}))["modules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|module| {
                (
                    module["id"].as_str().unwrap().to_owned(),
                    module["enabled"].as_bool().unwrap(),
                )
            })
            .collect()
    }

    /// The stored `module_layout`, or `None` with nothing stored.
    fn stored(&self) -> Option<Value> {
        let text = fs::read_to_string(self.file()).ok()?;
        let document: Value = serde_json::from_str(&text).unwrap();
        document.get("module_layout").cloned()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const ADJUSTMENTS: [&str; 12] = [
    "luxforge.crop",
    "luxforge.presets",
    "luxforge.look",
    "luxforge.basic",
    "luxforge.presence",
    "luxforge.curve",
    "luxforge.detail",
    "luxforge.mixer",
    "luxforge.lens",
    "luxforge.perspective",
    "luxforge.vignette",
    "luxforge.ai",
];

/// The adjustments placements: every module above except AI editing, which draws no section.
fn adjustments() -> Vec<&'static str> {
    ADJUSTMENTS
        .into_iter()
        .filter(|id| *id != "luxforge.ai")
        .collect()
}

/// Every write answers the new layout and announces one `module.layout` event under its request
/// when it changed something, and none for a no-op or a refusal; `module.list` follows; only
/// explicit choices are stored; `schema.list` never changes.
#[test]
fn module_layout_writes_answer_the_layout_and_announce_each_change_once() {
    let harness = Harness::start("writes", true);
    let schema = harness.ok("schema-before", "schema.list", json!({}));
    let read = harness.ok("read", "module.layout.read", json!({}));
    assert_eq!(
        read,
        json!({
            "toolsets": [
                {"toolset": "adjustments", "order": adjustments(), "default": adjustments()},
                {
                    "toolset": "tools",
                    "order": ["luxforge.corrections", "luxforge.crop"],
                    "default": ["luxforge.corrections", "luxforge.crop"],
                },
            ],
            "disabled": [],
            "unrecognized": [],
        })
    );
    assert!(
        !harness.file().exists(),
        "reading the layout creates nothing"
    );
    let listed = harness.ok("list", "module.list", json!({}));
    let crop = &listed["modules"][0];
    assert_eq!(
        (
            &crop["id"],
            &crop["toolset"],
            &crop["locked"],
            &crop["origin"],
            &crop["placements"],
            &crop["enabled"],
        ),
        (
            &json!("luxforge.crop"),
            &json!("adjustments"),
            &json!({"reason": "Every photo's geometry"}),
            &json!("built-in"),
            &json!([{"toolset": "adjustments"}, {"toolset": "tools"}]),
            &json!(true),
        )
    );

    let disabled = harness.ok(
        "off-vignette",
        "module.disable",
        json!({"module": "luxforge.vignette"}),
    );
    assert_eq!(disabled["disabled"], json!(["luxforge.vignette"]));
    assert_eq!(
        harness.ok("read", "module.layout.read", json!({})),
        disabled,
        "a write answers as module.layout.read does"
    );
    harness.ok(
        "off-again",
        "module.disable",
        json!({"module": "luxforge.vignette"}),
    );
    assert!(
        harness
            .enabled()
            .iter()
            .all(|(id, enabled)| *enabled == (id != "luxforge.vignette"))
    );
    let registry_order: Vec<String> = harness.enabled().into_iter().map(|(id, _)| id).collect();
    let mut order = adjustments();
    order.reverse();
    let ordered = harness.ok(
        "order",
        "module.order.set",
        json!({"toolset": "adjustments", "order": order}),
    );
    assert_eq!(ordered["toolsets"][0]["order"], json!(order));
    assert_eq!(ordered["toolsets"][0]["default"], json!(adjustments()));
    let listed: Vec<String> = harness.enabled().into_iter().map(|(id, _)| id).collect();
    assert_eq!(
        listed, registry_order,
        "module.list keeps the registry's order"
    );
    assert_eq!(
        harness.stored(),
        Some(json!({"disabled": ["luxforge.vignette"], "order": {"adjustments": order}})),
        "only the explicit choices are stored"
    );
    let preferences = harness.ok("preferences", "preferences.read", json!({}));
    assert_eq!(preferences["module_layout"], harness.stored().unwrap());

    // Refusals, by name, store nothing and announce nothing.
    for (id, method, params, words) in [
        (
            "locked",
            "module.disable",
            json!({"module": "luxforge.basic"}),
            "module luxforge.basic is locked and always on: Carries the RAW development's white balance and exposure",
        ),
        (
            "unknown",
            "module.enable",
            json!({"module": "example.missing"}),
            "module example.missing is not registered",
        ),
        (
            "partial",
            "module.order.set",
            json!({"toolset": "tools", "order": ["luxforge.crop"]}),
            "order leaves out luxforge.corrections: it names every tools placement exactly once",
        ),
        (
            "wrong-toolset",
            "module.order.set",
            json!({"toolset": "tools", "order": ["luxforge.crop", "luxforge.vignette"]}),
            "order names luxforge.vignette, which has no tools placement",
        ),
    ] {
        let error = harness.refused(id, method, params);
        assert_eq!(
            (error.code.as_str(), error.message.as_str()),
            ("validation", words)
        );
    }
    let error = harness.refused(
        "corrections",
        "module.order.set",
        json!({"toolset": "corrections", "order": []}),
    );
    assert_eq!(error.code, "validation");
    assert!(error.message.contains("toolset"), "{}", error.message);

    let enabled = harness.ok(
        "on-vignette",
        "module.enable",
        json!({"module": "luxforge.vignette"}),
    );
    assert_eq!(enabled["disabled"], json!([]));
    harness.ok(
        "tools-order",
        "module.order.set",
        json!({"toolset": "tools", "order": ["luxforge.crop", "luxforge.corrections"]}),
    );
    harness.ok(
        "reset-tools",
        "module.layout.reset",
        json!({"toolset": "tools"}),
    );
    assert_eq!(
        harness.stored(),
        Some(json!({"order": {"adjustments": order}})),
        "a toolset's reset leaves the other toolset"
    );
    let reset = harness.ok("reset", "module.layout.reset", json!({}));
    assert_eq!(reset, read, "a full reset restores the defaults");
    harness.ok("reset-again", "module.layout.reset", json!({}));
    assert_eq!(harness.stored(), None, "the defaults store nothing");
    assert_eq!(
        harness.announced(),
        [
            "off-vignette",
            "order",
            "on-vignette",
            "tools-order",
            "reset-tools",
            "reset"
        ],
        "one event per change, none for a no-op or a refusal"
    );
    assert_eq!(
        harness.ok("schema-after", "schema.list", json!({})),
        schema,
        "schema.list is the same whatever the layout"
    );
}

/// An id the stored layout names that this host does not register is listed as unrecognized and
/// survives every write, a full reset included.
#[test]
fn module_layout_unregistered_ids_survive_every_write() {
    let harness = Harness::start("unregistered", true);
    // The vignette moved to the top, as a person's order stores it, with a module this build no
    // longer has after it.
    let mut order = adjustments();
    order.retain(|id| *id != "luxforge.vignette");
    order.insert(0, "luxforge.vignette");
    let mut stored = order.clone();
    stored.insert(1, "example.retired");
    fs::create_dir_all(harness.file().parent().unwrap()).unwrap();
    fs::write(
        harness.file(),
        serde_json::to_vec(&json!({
            "format": 1,
            "module_layout": {
                "disabled": ["example.gone"],
                "order": {"adjustments": stored, "tools": ["example.tool"]},
            },
        }))
        .unwrap(),
    )
    .unwrap();
    let read = harness.ok("read", "module.layout.read", json!({}));
    assert_eq!(
        read["unrecognized"],
        json!(["example.gone", "example.retired", "example.tool"])
    );
    assert_eq!(read["disabled"], json!([]));
    assert_eq!(read["toolsets"][0]["order"], json!(order));
    harness.ok("off", "module.disable", json!({"module": "luxforge.mixer"}));
    harness.ok(
        "default-order",
        "module.order.set",
        json!({"toolset": "adjustments", "order": adjustments()}),
    );
    harness.ok("reset", "module.layout.reset", json!({}));
    assert_eq!(
        harness.stored(),
        Some(json!({
            "disabled": ["example.gone"],
            "order": {"adjustments": ["example.retired"], "tools": ["example.tool"]},
        }))
    );
}

/// A stored layout this build cannot use is refused by name, by the read and by every write,
/// without rewriting the file; discovery still lists every module, on.
#[test]
fn module_layout_a_malformed_stored_layout_is_refused_without_rewriting_it() {
    let harness = Harness::start("malformed", true);
    fs::create_dir_all(harness.file().parent().unwrap()).unwrap();
    for bytes in [
        br#"{"format":1,"module_layout":{"disabled":["Not An Id"]}}"#.as_slice(),
        br#"{"format":1,"module_layout":{"hidden":[]}}"#,
        br#"{"format":1,"module_layout":{"order":{"masking":["luxforge.ai"]}}}"#,
        br#"{"format":1,"module_layout":{"disabled":["luxforge.mixer","luxforge.mixer"]}}"#,
    ] {
        fs::write(harness.file(), bytes).unwrap();
        let error = harness.refused("read", "module.layout.read", json!({}));
        assert_eq!(error.code, "incompatible", "{}", error.message);
        for (method, params) in [
            ("module.disable", json!({"module": "luxforge.vignette"})),
            ("module.enable", json!({"module": "luxforge.vignette"})),
            (
                "module.order.set",
                json!({"toolset": "adjustments", "order": adjustments()}),
            ),
            ("module.layout.reset", json!({})),
        ] {
            harness.refused(method, method, params);
        }
        assert_eq!(fs::read(harness.file()).unwrap(), bytes);
        assert!(harness.enabled().iter().all(|(_, enabled)| *enabled));
    }
    assert!(harness.announced().is_empty());
}

/// Without a preference directory the layout is the defaults and every write is not ready.
#[test]
fn module_layout_writes_need_a_preference_directory() {
    let harness = Harness::start("unconfigured", false);
    let read = harness.ok("read", "module.layout.read", json!({}));
    assert_eq!(read["disabled"], json!([]));
    let error = harness.refused(
        "off",
        "module.disable",
        json!({"module": "luxforge.vignette"}),
    );
    assert_eq!(error.code, "not-ready");
    assert!(harness.enabled().iter().all(|(_, enabled)| *enabled));
}

/// A photo with a Vignette layer exports the same bytes before and after the person turns the
/// vignette off and reorders both toolsets: the layout is never read by a render.
#[test]
fn module_layout_never_changes_a_render() {
    let harness = Harness::start("render", true);
    let path = harness.dir.join("layout.jpg");
    fs::copy(fixture(), &path).unwrap();
    let asset = super::library::opening::import(&harness.owner, harness.client, &path);
    let revision =
        harness.ok("state", "asset.state", json!({"asset_id": asset}))["revision"].clone();
    harness.ok(
        "vignette",
        "edit.set-vignette",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": revision, "request_id": "vignette", "actor": "test"},
            "amount": -40,
        }),
    );
    let exports = harness.dir.join("exports");
    fs::create_dir_all(&exports).unwrap();
    let export = |name: &str| -> Vec<u8> {
        let destination = exports.join(format!("{name}.jpg"));
        let accepted = harness.ok(
            &format!("export-{name}"),
            "export.write",
            json!({
                "asset_id": asset,
                "settings": crate::export::settings::baseline_json(),
                "destination": destination,
                "mutation": {"request_id": format!("export-{name}"), "actor": "test"},
            }),
        );
        let read = luxforge_testbase::wait_for("the export to settle", || {
            let read = harness.ok("job", "job.read", json!({"job_id": accepted["job_id"]}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        });
        assert_eq!(read["status"], "ready", "{read}");
        fs::read(&destination).unwrap()
    };
    let before = export("before");
    harness.ok(
        "off",
        "module.disable",
        json!({"module": "luxforge.vignette"}),
    );
    let mut order = adjustments();
    order.reverse();
    harness.ok(
        "order",
        "module.order.set",
        json!({"toolset": "adjustments", "order": order}),
    );
    harness.ok(
        "tools",
        "module.order.set",
        json!({"toolset": "tools", "order": ["luxforge.crop", "luxforge.corrections"]}),
    );
    assert!(
        before == export("after"),
        "the export changed with the layout"
    );
}
