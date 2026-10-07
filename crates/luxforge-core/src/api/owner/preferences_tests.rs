//! `preferences.read` and `preferences.set` through the catalog owner: every preference answered
//! with its default, each one set and reset by `null`, every bad value refused by name without a
//! write, and only a General row's change announced. The lens switch reaching the catalog writer
//! is the first-open tests'.
use super::*;
use luxforge_testbase::paths::temp_dir;
use std::fs;

struct Harness {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    dir: PathBuf,
}

impl Harness {
    fn start(name: &str) -> Self {
        let dir = temp_dir(&format!("preferences-{name}"))
            .canonicalize()
            .unwrap();
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                preferences_dir: Some(dir.join("config")),
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

    /// The request ids `preferences.set` announced, in order.
    fn announced(&self) -> Vec<String> {
        let events = self.ok("events", "events.since", json!({"after": 0}));
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["method"] == "preferences.set")
            .map(|event| event["request_id"].as_str().unwrap().to_owned())
            .collect()
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

fn defaults() -> Value {
    json!({
        "performance_expanded": true,
        "auto_collapse_history": true,
        "auto_lens_profile": true,
        "raw_look": "standard",
        "mask_overlay_colour": "green",
        "canvas_background": "theme",
        "interface_size": 100,
        "catalog": null,
        "workspace": {
            "state_panel": true,
            "tools_panel": true,
            "thirds": false,
            "clip_shadows": false,
            "clip_highlights": false,
        },
        "brush": null,
        "window": null,
        "export_folder": null,
        "theme": "luxforge.dark",
    })
}

#[test]
fn preferences_read_answers_every_default_and_set_round_trips_every_field() {
    let harness = Harness::start("round-trip");
    assert_eq!(
        harness.ok("read", "preferences.read", json!({})),
        defaults()
    );
    assert!(!harness.file().exists(), "reading creates nothing");
    let schema = harness.ok("schema", "schema.list", json!({}));
    let declared: Vec<_> = schema["methods"]["preferences.set"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|parameter| parameter["name"].as_str().unwrap().to_owned())
        .collect();
    let answered: Vec<_> = defaults().as_object().unwrap().keys().cloned().collect();
    let mut sorted = declared.clone();
    sorted.sort();
    assert_eq!(sorted, answered, "every preference can be set");

    let catalog = harness.dir.join("Photos").join("catalog.sqlite");
    let exports = harness.dir.join("Exports");
    let chosen = json!({
        "performance_expanded": false,
        "auto_collapse_history": false,
        "auto_lens_profile": false,
        "raw_look": "neutral",
        "mask_overlay_colour": "white",
        "canvas_background": "grey",
        "interface_size": 125,
        "catalog": catalog,
        "workspace": {
            "state_panel": false,
            "tools_panel": true,
            "thirds": true,
            "clip_shadows": true,
            "clip_highlights": true,
        },
        "brush": {"size": 0.05, "feather": 20.0, "flow": 75.0},
        "window": {"width": 1280.0, "height": 800.0, "x": -40.5, "y": 25.0},
        "export_folder": exports,
        "theme": "luxforge.dark",
    });
    assert_eq!(
        harness.ok("set", "preferences.set", chosen.clone()),
        chosen,
        "a path need not exist"
    );
    assert_eq!(harness.ok("read", "preferences.read", json!({})), chosen);
    assert!(!catalog.exists() && !exports.exists(), "nothing is created");

    // An absent field is left as it was; null resets each one to its default.
    let mut kept = chosen.clone();
    kept["interface_size"] = json!(150);
    assert_eq!(
        harness.ok("one", "preferences.set", json!({"interface_size": 150})),
        kept
    );
    let nulls: serde_json::Map<String, Value> = defaults()
        .as_object()
        .unwrap()
        .keys()
        .map(|key| (key.clone(), Value::Null))
        .collect();
    assert_eq!(
        harness.ok("reset", "preferences.set", Value::Object(nulls)),
        defaults()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(harness.file()).unwrap()).unwrap(),
        json!({"format": 1}),
        "a reset stores nothing"
    );
}

#[test]
fn every_bad_preference_is_refused_by_name_and_writes_nothing() {
    let harness = Harness::start("refused");
    harness.ok("seed", "preferences.set", json!({"interface_size": 110}));
    let stored = fs::read(harness.file()).unwrap();
    let workspace = json!({
        "state_panel": true, "tools_panel": true, "thirds": false,
        "clip_shadows": false, "clip_highlights": false,
    });
    for (params, refusal) in [
        (
            json!({"auto_lens_profile": "yes"}),
            "parameter auto_lens_profile must be a boolean",
        ),
        (
            json!({"performance_expanded": 1}),
            "parameter performance_expanded must be a boolean",
        ),
        (
            json!({"mask_overlay_colour": "red"}),
            "parameter mask_overlay_colour must be one of green, white",
        ),
        (
            json!({"canvas_background": "white"}),
            "parameter canvas_background must be one of dark, black, grey, theme",
        ),
        (
            json!({"raw_look": "camera"}),
            "parameter raw_look must be one of standard, neutral",
        ),
        (
            json!({"interface_size": 120}),
            "interface_size must be one of 100, 110, 125, 150",
        ),
        (
            json!({"interface_size": 200}),
            "parameter interface_size must be an integer within 100..=150",
        ),
        (
            json!({"catalog": "photos/catalog.sqlite"}),
            "catalog must be an absolute path",
        ),
        (json!({"catalog": ""}), "catalog must not be empty"),
        (
            json!({"export_folder": "Exports"}),
            "export_folder must be an absolute path",
        ),
        (json!({"export_folder": 3}), "parameter export_folder"),
        (
            json!({"workspace": {"thirds": true}}),
            "workspace: missing field",
        ),
        (
            json!({"workspace": {"mode": "pointer"}}),
            "workspace: unknown field `mode`",
        ),
        (
            json!({"brush": {"size": 0.1, "feather": 50}}),
            "brush: missing field `flow`",
        ),
        (
            json!({"brush": {"size": 0.1, "feather": 101, "flow": 100}}),
            "brush.feather must be a number within 0..=100",
        ),
        (
            json!({"brush": {"size": 0.0, "feather": 50, "flow": 100}}),
            "brush.size must be a number within",
        ),
        (
            json!({"window": {"width": 200, "height": 800, "x": 0, "y": 0}}),
            "window.width must be a number within 320..=16384",
        ),
        (
            json!({"window": {"width": 1440, "height": 900, "x": 1e39, "y": 0}}),
            "window.x must be a finite number",
        ),
        (json!({"window": "1440x900"}), "window: invalid type"),
        (json!({"theme": "dark"}), "unknown theme dark"),
        (
            json!({"colour_theme": "dark"}),
            "unknown field `colour_theme`",
        ),
        // One bad field refuses the whole write.
        (
            json!({"workspace": workspace, "interface_size": 99}),
            "parameter interface_size must be an integer within 100..=150",
        ),
    ] {
        let failure = harness
            .send("bad", "preferences.set", params.clone())
            .error
            .unwrap_or_else(|| panic!("{params} was accepted"));
        assert_eq!(failure.code, "validation", "{params}: {}", failure.message);
        assert!(
            failure.message.starts_with(refusal),
            "{params}: {}",
            failure.message
        );
        assert_eq!(fs::read(harness.file()).unwrap(), stored);
    }
    assert_eq!(harness.announced(), ["seed"], "a refusal announces nothing");
}

#[test]
fn only_a_change_to_a_general_row_is_announced() {
    let harness = Harness::start("events");
    let exports = harness.dir.join("Exports");
    for (id, params) in [
        ("performance", json!({"performance_expanded": false})),
        (
            "workspace",
            json!({"workspace": {
                "state_panel": false, "tools_panel": true, "thirds": true,
                "clip_shadows": false, "clip_highlights": false,
            }}),
        ),
        (
            "brush",
            json!({"brush": {"size": 0.2, "feather": 10, "flow": 50}}),
        ),
        (
            "window",
            json!({"window": {"width": 900, "height": 700, "x": 10, "y": 20}}),
        ),
        ("export", json!({"export_folder": exports})),
        // Choosing a default explicitly shows nothing new.
        ("lens-default", json!({"auto_lens_profile": true})),
        ("collapse", json!({"auto_collapse_history": false})),
        ("collapse-again", json!({"auto_collapse_history": false})),
        ("lens", json!({"auto_lens_profile": false})),
        ("look-default", json!({"raw_look": "standard"})),
        ("look", json!({"raw_look": "neutral"})),
        ("colour", json!({"mask_overlay_colour": "white"})),
        ("background", json!({"canvas_background": "black"})),
        ("size", json!({"interface_size": 110})),
        (
            "catalog",
            json!({"catalog": harness.dir.join("elsewhere.sqlite")}),
        ),
        ("catalog-reset", json!({"catalog": null})),
        ("export-reset", json!({"export_folder": null})),
    ] {
        harness.ok(id, "preferences.set", params);
    }
    assert_eq!(
        harness.announced(),
        [
            "collapse",
            "lens",
            "look",
            "colour",
            "background",
            "size",
            "catalog",
            "catalog-reset"
        ]
    );
}
