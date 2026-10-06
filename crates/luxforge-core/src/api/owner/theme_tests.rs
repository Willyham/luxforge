//! The theme methods through the catalog owner: every method listed by `schema.list`, an import
//! chosen through `preferences.set` and read back with its events, retries answered once, every
//! refusal by name, and no theme method touching a photograph's history. The store's own rules
//! are the library's tests (`theme/library_tests.rs`).
use super::*;
use crate::theme::{LUXFORGE_DARK_ID, Roles, ThemeDocument, Tokens, luxforge_dark, omarchy};
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};
use std::fs;

struct Harness {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    dir: PathBuf,
}

impl Harness {
    fn start(name: &str) -> Self {
        let dir = temp_dir(&format!("themes-{name}")).canonicalize().unwrap();
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

    fn library(&self) -> PathBuf {
        self.dir.join("config").join("themes.json")
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

    /// The refusal's code and message.
    fn refused(&self, method: &str, params: Value) -> (String, String) {
        let failure = self
            .send("refused", method, params.clone())
            .error
            .unwrap_or_else(|| panic!("{method} {params} was accepted"));
        (failure.code, failure.message)
    }

    /// Every event the log holds, as (method, request id).
    fn events(&self) -> Vec<(String, String)> {
        let events = self.ok("events", "events.since", json!({"after": 0}));
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                (
                    event["method"].as_str().unwrap().to_owned(),
                    event["request_id"].as_str().unwrap().to_owned(),
                )
            })
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

fn mutation(request_id: &str) -> Value {
    json!({"request_id": request_id, "actor": "test"})
}

fn night(name: &str) -> String {
    ThemeDocument {
        name: name.into(),
        roles: Roles::new(
            "#1e2030".parse().unwrap(),
            "#c0caf5".parse().unwrap(),
            "#7aa2f7".parse().unwrap(),
        ),
        tokens: Tokens::new(),
    }
    .write()
}

fn import(content: &str, request_id: &str) -> Value {
    json!({"format": "luxforge", "content": content, "mutation": mutation(request_id)})
}

/// The built-in themes' names, in their order: Luxforge Dark, then the bundled Omarchy themes.
const BUILT_IN: [&str; 7] = [
    "Luxforge Dark",
    "Tokyo Night",
    "Catppuccin",
    "Catppuccin Latte",
    "Gruvbox",
    "Nord",
    "Everforest",
];

const METHODS: [&str; 6] = [
    "theme.list",
    "theme.read",
    "theme.inspect",
    "theme.import",
    "theme.export",
    "theme.delete",
];

#[test]
fn schema_list_lists_every_theme_method_and_the_mutating_ones_take_the_request_envelope() {
    let harness = Harness::start("schema");
    let schema = harness.ok("schema", "schema.list", json!({}));
    for method in METHODS {
        let entry = &schema["methods"][method];
        assert!(entry.is_object(), "{method} is listed");
        let mutates = matches!(method, "theme.import" | "theme.delete");
        let envelope = if mutates {
            json!("request")
        } else {
            Value::Null
        };
        assert_eq!(entry["mutation"], envelope, "{method}: {entry}");
    }
    let parameters = |method: &str| -> Vec<String> {
        schema["methods"][method]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|parameter| parameter["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        parameters("theme.import"),
        ["format", "content", "files", "folder", "name"]
    );
    assert_eq!(parameters("theme.delete"), ["theme_id"]);
    assert!(parameters("preferences.set").contains(&"theme".to_owned()));
}

#[test]
fn an_import_is_listed_chosen_announced_and_read_back_and_a_retry_changes_nothing_twice() {
    let harness = Harness::start("round-trip");
    let listed = harness.ok("list", "theme.list", json!({}));
    assert_eq!(listed["active"], LUXFORGE_DARK_ID);
    assert_eq!(listed["unrecognized"], json!([]));
    let dark = &listed["themes"][0];
    assert_eq!(
        (
            dark["id"].clone(),
            dark["name"].clone(),
            dark["built_in"].clone()
        ),
        (json!(LUXFORGE_DARK_ID), json!("Luxforge Dark"), json!(true))
    );
    assert_eq!(dark["origin"], json!({"kind": "built-in"}));
    assert_eq!(
        dark["swatches"],
        json!({"surround": "#19191b", "background": "#202023", "surface": "#232326",
               "text": "#e8e8ea", "accent": "#e2b46a"})
    );
    assert_eq!(dark["adjusted"], false);
    assert_eq!(dark["report"]["moved"], 0);
    let preferences = harness.ok("read", "preferences.read", json!({}));
    assert_eq!(
        (
            preferences["theme"].clone(),
            preferences["canvas_background"].clone()
        ),
        (json!(LUXFORGE_DARK_ID), json!("theme"))
    );

    let text = night("Night");
    let inspected = harness.ok(
        "inspect",
        "theme.inspect",
        json!({"format": "luxforge", "content": text, "name": "Dusk"}),
    );
    assert_eq!(inspected["theme"]["id"], Value::Null);
    assert_eq!(inspected["theme"]["name"], "Dusk");
    assert!(!harness.library().exists(), "an inspection stores nothing");

    let imported = harness.ok("import", "theme.import", import(&text, "import-1"));
    assert_eq!(imported["deduplicated"], false);
    let id = imported["theme"]["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("theme-"));
    assert_eq!(imported["report"], inspected["report"]);
    assert_eq!(imported["theme"]["actor"], "test");
    assert!(
        imported["theme"].get("source").is_none(),
        "only theme.read answers the source"
    );
    // The same request again is answered once, stores nothing and announces nothing.
    let stored = fs::read(harness.library()).unwrap();
    let retried = harness.ok("import-again", "theme.import", import(&text, "import-1"));
    assert_eq!(retried["deduplicated"], true);
    assert_eq!(retried["theme"], imported["theme"]);
    assert_eq!(fs::read(harness.library()).unwrap(), stored);
    let (code, message) = harness.refused("theme.import", import(&night("Other"), "import-1"));
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "conflict",
            "request_id was already used with different input"
        )
    );

    let listed = harness.ok("list", "theme.list", json!({}));
    let names: Vec<&str> = listed["themes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|theme| theme["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, [BUILT_IN.as_slice(), &["Night"]].concat());
    assert_eq!(listed["themes"][7]["built_in"], false);
    assert_eq!(listed["themes"][7]["origin"], json!({"kind": "luxforge"}));

    let read = harness.ok("read", "theme.read", json!({"theme_id": id}));
    assert_eq!(read["theme"]["source"], json!({"Night.lftheme": text}));
    assert_eq!(read["theme"]["resolved"]["background"], "#1e2030");
    assert_eq!(
        read["theme"]["resolved"].as_object().unwrap().len(),
        crate::theme::TOKEN_COUNT
    );
    let exported = harness.ok("export", "theme.export", json!({"theme_id": id}));
    assert_eq!(
        exported,
        json!({"file_name": "Night.lftheme", "content": text})
    );
    assert_eq!(
        harness.ok(
            "export-dark",
            "theme.export",
            json!({"theme_id": LUXFORGE_DARK_ID})
        )["content"],
        luxforge_dark().write()
    );

    // Chosen, announced, and read back; choosing it again announces nothing.
    let set = harness.ok("choose", "preferences.set", json!({"theme": id}));
    assert_eq!(set["theme"], id.as_str());
    harness.ok("choose-again", "preferences.set", json!({"theme": id}));
    assert_eq!(
        harness.ok("read", "preferences.read", json!({}))["theme"],
        id.as_str()
    );
    assert_eq!(
        harness.ok("list", "theme.list", json!({}))["active"],
        id.as_str()
    );
    let launch = crate::preferences::LaunchPreferences::read(Some(harness.dir.join("config")));
    assert_eq!(launch.theme.id, id);
    assert_eq!(launch.theme.name, "Night");
    assert!(launch.theme.problem.is_none());

    // The active theme cannot be deleted; after Luxforge Dark is chosen again, it can, once.
    let (code, message) = harness.refused(
        "theme.delete",
        json!({"theme_id": id, "mutation": mutation("delete-active")}),
    );
    assert_eq!(code, "conflict");
    assert_eq!(
        message,
        format!("theme {id} is the active theme; choose another before deleting it")
    );
    let reset = harness.ok(
        "dark",
        "preferences.set",
        json!({"theme": LUXFORGE_DARK_ID}),
    );
    assert_eq!(reset["theme"], LUXFORGE_DARK_ID);
    let file: Value = serde_json::from_slice(
        &fs::read(harness.dir.join("config").join("preferences.json")).unwrap(),
    )
    .unwrap();
    assert!(
        file.get("theme").is_none(),
        "Luxforge Dark is stored as no choice: {file}"
    );
    let deleted = harness.ok(
        "delete",
        "theme.delete",
        json!({"theme_id": id, "mutation": mutation("delete-1")}),
    );
    assert_eq!(
        deleted,
        json!({"outcome": "applied", "deleted": true, "deduplicated": false})
    );
    let retried = harness.ok(
        "delete-retry",
        "theme.delete",
        json!({"theme_id": id, "mutation": mutation("delete-1")}),
    );
    assert_eq!(retried["deduplicated"], true);
    let absent = harness.ok(
        "delete-absent",
        "theme.delete",
        json!({"theme_id": id, "mutation": mutation("delete-2")}),
    );
    assert_eq!(absent["outcome"], "no-op");

    let announced: Vec<(String, String)> = harness
        .events()
        .into_iter()
        .filter(|(method, _)| method.starts_with("theme.") || method == "preferences.set")
        .collect();
    let expected: Vec<(String, String)> = [
        ("theme.import", "import"),
        ("preferences.set", "choose"),
        ("preferences.set", "dark"),
        ("theme.delete", "delete"),
    ]
    .into_iter()
    .map(|(method, id)| (method.to_owned(), id.to_owned()))
    .collect();
    assert_eq!(announced, expected);
}

#[test]
fn every_refusal_is_by_name_and_changes_nothing() {
    let harness = Harness::start("refused");
    let text = night("Night");
    harness.ok("seed", "theme.import", import(&text, "seed"));
    let stored = fs::read(harness.library()).unwrap();
    let unknown = "theme-ffffffffffffffffffffffffffffffff";
    let wrong_marker = json!({"format": "luxforge.preset", "version": 1}).to_string();
    let wrong_version = text.replace("\"version\": 1", "\"version\": 2");
    for (method, params, code, refusal) in [
        (
            "theme.read",
            json!({"theme_id": unknown}),
            "validation",
            format!("unknown theme {unknown}"),
        ),
        (
            "theme.export",
            json!({"theme_id": unknown}),
            "validation",
            format!("unknown theme {unknown}"),
        ),
        (
            "preferences.set",
            json!({"theme": unknown}),
            "validation",
            format!("unknown theme {unknown}"),
        ),
        (
            "preferences.set",
            json!({"theme": "omarchy.kanagawa"}),
            "validation",
            "unknown theme omarchy.kanagawa".to_owned(),
        ),
        (
            "theme.import",
            import(&text, "duplicate"),
            "conflict",
            "a theme named \"Night\" already exists; choose another name".to_owned(),
        ),
        (
            "theme.import",
            json!({"format": "luxforge", "content": text, "name": "LUXFORGE DARK",
                   "mutation": mutation("built-in-name")}),
            "conflict",
            "a theme named \"Luxforge Dark\" already exists".to_owned(),
        ),
        (
            "theme.import",
            import("{", "malformed"),
            "unsupported-input",
            "malformed JSON".to_owned(),
        ),
        (
            "theme.import",
            import(&wrong_marker, "marker"),
            "unsupported-input",
            "not a Luxforge theme document".to_owned(),
        ),
        (
            "theme.import",
            import(&wrong_version, "version"),
            "unsupported-input",
            "Luxforge theme document version 2 is not supported".to_owned(),
        ),
        (
            "theme.import",
            json!({"format": "omarchy", "files": {"colors.toml": "accent = \"#7aa2f7\""},
                   "folder": "night", "mutation": mutation("omarchy")}),
            "unsupported-input",
            "colors.toml has no background".to_owned(),
        ),
        (
            "theme.import",
            json!({"format": "omarchy", "files": {"neovim.lua": ""}, "folder": "night",
                   "mutation": mutation("omarchy-file")}),
            "validation",
            "\"neovim.lua\" is not read".to_owned(),
        ),
        (
            "theme.import",
            json!({"format": "luxforge", "content": text, "folder": "night",
                   "mutation": mutation("luxforge-folder")}),
            "validation",
            "format luxforge takes no folder".to_owned(),
        ),
        (
            "theme.delete",
            json!({"theme_id": "omarchy.nord", "mutation": mutation("delete-nord")}),
            "validation",
            "Nord is built in and cannot be deleted".to_owned(),
        ),
        (
            "theme.import",
            json!({"format": "base16", "content": text, "mutation": mutation("format")}),
            "validation",
            "parameter format must be one of luxforge, omarchy".to_owned(),
        ),
        (
            "theme.import",
            json!({"format": "luxforge", "content": "x".repeat(64 * 1024 + 1),
                   "mutation": mutation("long")}),
            "validation",
            "parameter content".to_owned(),
        ),
        (
            "theme.import",
            json!({"format": "luxforge", "content": text}),
            "validation",
            "missing field `mutation`".to_owned(),
        ),
        (
            "theme.delete",
            json!({"theme_id": LUXFORGE_DARK_ID, "mutation": mutation("delete-dark")}),
            "validation",
            "Luxforge Dark is built in and cannot be deleted".to_owned(),
        ),
    ] {
        let (failed, message) = harness.refused(method, params.clone());
        assert_eq!(failed, code, "{method} {params}: {message}");
        assert!(message.starts_with(&refusal), "{method}: {message}");
        assert_eq!(fs::read(harness.library()).unwrap(), stored, "{method}");
    }
    let announced: Vec<_> = harness.events().into_iter().map(|(_, id)| id).collect();
    assert_eq!(announced, ["seed"], "a refusal announces nothing");
}

/// A theme lives outside every catalog: importing, choosing, exporting and deleting one leaves a
/// photograph's revision and history as they were.
#[test]
fn no_theme_method_changes_an_asset_revision() {
    let harness = Harness::start("assets");
    let path = harness.dir.join("photo.jpg");
    fs::copy(fixture(), &path).unwrap();
    let queued = harness.ok(
        "develop",
        "pick.develop",
        json!({
            "targets": {"kind": "paths", "paths": [path]},
            "into": [],
            "confirm_removable": true,
            "mutation": mutation("photo"),
        }),
    );
    let job = queued["job_id"].clone();
    let status = luxforge_testbase::wait_for("the development to settle", || {
        let status = harness.ok("job", "job.read", json!({"job_id": job}));
        (!matches!(status["status"].as_str(), Some("queued" | "running"))).then_some(status)
    });
    assert_eq!(status["status"], "ready", "{status}");
    let asset = status["result"]["developed"][0]["asset_id"].clone();
    let state = || harness.ok("state", "asset.state", json!({"asset_id": asset}));
    let before = state();
    let history = harness.ok("history", "history.list", json!({"asset_id": asset}));

    let imported = harness.ok("import", "theme.import", import(&night("Night"), "theme"));
    let id = imported["theme"]["id"].clone();
    harness.ok("list", "theme.list", json!({}));
    harness.ok("read", "theme.read", json!({"theme_id": id}));
    harness.ok("export", "theme.export", json!({"theme_id": id}));
    harness.ok(
        "choose",
        "preferences.set",
        json!({"theme": id, "canvas_background": "grey"}),
    );
    harness.ok("dark", "preferences.set", json!({"theme": null}));
    harness.ok(
        "delete",
        "theme.delete",
        json!({"theme_id": id, "mutation": mutation("delete")}),
    );

    let after = state();
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(after["current_entry"], before["current_entry"]);
    assert_eq!(
        harness.ok("history", "history.list", json!({"asset_id": asset})),
        history
    );
    // No theme event names an asset.
    let events = harness.ok("events", "events.since", json!({"after": 0}));
    for event in events["events"].as_array().unwrap() {
        if event["method"].as_str().unwrap().starts_with("theme.") {
            assert!(event.get("asset_id").is_none_or(Value::is_null), "{event}");
        }
    }
}

// Omarchy themes.

/// A synthetic Omarchy theme folder of `theme/omarchy/testdata`, by name.
fn synthetic(name: &str) -> std::collections::BTreeMap<String, String> {
    omarchy::tests::synthetic_folders()
        .into_iter()
        .find(|(folder, _)| folder == name)
        .unwrap()
        .1
}

/// Each synthetic folder through `theme.inspect` gives the roles Omarchy's resolver gives it, and
/// its report; or its refusal by name. Nothing is stored.
#[test]
fn synthetic_omarchy_folders_give_their_roles_and_report_through_inspect() {
    let harness = Harness::start("omarchy-inspect");
    for (folder, files) in omarchy::tests::synthetic_folders() {
        let params = json!({"format": "omarchy", "files": files, "folder": folder});
        if folder.starts_with("brightness-") {
            // A mid grey with near-black text, which no lightness brings to 7:1.
            let (code, message) = harness.refused("theme.inspect", params);
            assert_eq!(code, "validation", "{folder}");
            assert!(
                message.starts_with("text #101010 cannot reach 7:1"),
                "{message}"
            );
            continue;
        }
        let answer = harness.ok(&folder, "theme.inspect", params);
        let palette = omarchy::read(&files).unwrap();
        let theme = &answer["theme"];
        let report = &answer["report"]["omarchy"];
        assert_eq!(theme["report"], answer["report"]);
        assert_eq!(
            theme["origin"],
            json!({"kind": "omarchy", "folder": folder, "form": palette.form.as_str()})
        );
        assert_eq!(report["form"], palette.form.as_str());
        assert_eq!(report["mode_source"], json!(palette.mode_source));
        assert_eq!(theme["mode"], json!(palette.mode));
        let control = palette.lighter_background != palette.background;
        let mut roles = vec![
            json!({"role": "surround", "key": "dark_background",
                   "value": palette.dark_background.to_string()}),
            json!({"role": "background", "key": "background",
                   "value": palette.background.to_string()}),
        ];
        if control {
            roles.push(json!({"role": "control", "key": "lighter_background",
                              "value": palette.lighter_background.to_string()}));
        }
        roles.push(json!({"role": "text", "key": "foreground",
                          "value": palette.foreground.to_string()}));
        roles.push(json!({"role": "accent", "key": "accent",
                          "value": palette.accent.to_string()}));
        assert_eq!(report["roles"], json!(roles), "{folder}");
        assert_eq!(
            theme["roles"]["control"],
            if control {
                json!(palette.lighter_background.to_string())
            } else {
                Value::Null
            },
            "{folder}"
        );
        assert_eq!(theme["roles"]["text"], palette.foreground.to_string());
        assert_eq!(
            report["unused"].as_array().unwrap().len(),
            palette.unused.len()
        );
        assert_eq!(
            report["derived"].as_array().unwrap().len(),
            usize::from(!control)
        );
    }
    assert!(!harness.library().exists(), "an inspection stores nothing");
}

/// Every way an Omarchy folder cannot be read is refused by name through `theme.inspect` and
/// `theme.import`, with the reader's coded error as data, and nothing is stored.
#[test]
fn an_unreadable_omarchy_folder_is_refused_by_name() {
    let harness = Harness::start("omarchy-refused");
    const BASE: &str = "background = \"#1b1f27\"\nforeground = \"#c9ced8\"\n";
    let colors = |text: &str| json!({"colors.toml": text});
    let alacritty = "[colors.normal]\nblack = \"#000000\"\nred = \"#ff0000\"\n";
    for (files, code, message, data) in [
        (
            colors(&format!("{BASE}accent = \"#5e9ce0\"\nred = \"ff0000\"\n")),
            "unsupported-input",
            "colors.toml: red is \"ff0000\", not a #rrggbb colour",
            json!({"code": "not-hex", "file": "colors.toml", "key": "red", "value": "ff0000"}),
        ),
        (
            colors(&format!(
                "{BASE}accent = \"rgba(33ccffee) rgba(00ff99ee) 45deg\"\n"
            )),
            "unsupported-input",
            "colors.toml: accent is \"rgba(33ccffee) rgba(00ff99ee) 45deg\"",
            json!({"code": "not-hex", "file": "colors.toml", "key": "accent",
                   "value": "rgba(33ccffee) rgba(00ff99ee) 45deg"}),
        ),
        (
            colors(&format!("{BASE}accent = #5e9ce0\n")),
            "unsupported-input",
            "colors.toml is not TOML at line 3, column 10",
            json!({"code": "malformed-toml", "file": "colors.toml", "line": 3, "column": 10,
                   "message": "string values must be quoted, expected literal string"}),
        ),
        (
            colors("foreground = \"#c9ced8\"\naccent = \"#5e9ce0\"\n"),
            "unsupported-input",
            "colors.toml has no background",
            json!({"code": "missing-key", "file": "colors.toml", "key": "background"}),
        ),
        (
            colors("background = \"#1b1f27\"\naccent = \"#5e9ce0\"\n"),
            "unsupported-input",
            "colors.toml has no foreground",
            json!({"code": "missing-key", "file": "colors.toml", "key": "foreground"}),
        ),
        (
            colors(&format!("{BASE}blue = \"#5e9ce0\"\n")),
            "unsupported-input",
            "colors.toml has no accent",
            json!({"code": "missing-key", "file": "colors.toml", "key": "accent"}),
        ),
        (
            colors(&format!("{BASE}accent = \"#5e9ce0\"\nmode = \"auto\"\n")),
            "unsupported-input",
            "colors.toml: mode is \"auto\", not \"light\" or \"dark\"",
            json!({"code": "invalid-mode", "file": "colors.toml", "key": "mode",
                   "value": "auto"}),
        ),
        (
            json!({"alacritty.toml": alacritty}),
            "unsupported-input",
            "alacritty.toml has no colors.normal.green",
            json!({"code": "missing-key", "file": "alacritty.toml",
                   "key": "colors.normal.green"}),
        ),
        (
            json!({"light.mode": ""}),
            "unsupported-input",
            "the theme has neither colors.toml nor alacritty.toml",
            json!({"code": "no-palette"}),
        ),
        (
            json!({"colors.toml": format!("{BASE}accent = \"#5e9ce0\"\n"), "README.md": "x"}),
            "validation",
            "\"README.md\" is not read; an Omarchy theme is read from colors.toml, \
             alacritty.toml and light.mode only",
            json!({"code": "file-not-allowed", "file": "README.md"}),
        ),
    ] {
        for (method, params) in [
            (
                "theme.inspect",
                json!({"format": "omarchy", "files": files, "folder": "broken"}),
            ),
            (
                "theme.import",
                json!({"format": "omarchy", "files": files, "folder": "broken",
                       "mutation": mutation("broken")}),
            ),
        ] {
            let failure = harness.send("refused", method, params).error.unwrap();
            assert_eq!(failure.code, code, "{}", failure.message);
            assert!(failure.message.starts_with(message), "{}", failure.message);
            assert_eq!(failure.data, Some(data.clone()), "{message}");
        }
    }
    // A file past 64 KiB, and a folder with neither a folder name nor a name.
    let oversized = format!("{BASE}accent = \"#5e9ce0\"\n{}", " ".repeat(64 * 1024));
    let (code, message) = harness.refused(
        "theme.inspect",
        json!({"format": "omarchy", "files": {"colors.toml": oversized}, "folder": "big"}),
    );
    assert_eq!(code, "resource-limit");
    assert!(message.starts_with("file \"colors.toml\" is "), "{message}");
    let (code, message) = harness.refused(
        "theme.inspect",
        json!({"format": "omarchy", "files": colors(&format!("{BASE}accent = \"#5e9ce0\"\n"))}),
    );
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "validation",
            "format omarchy takes the theme folder's name as folder, or a name for the theme"
        )
    );
    assert!(!harness.library().exists(), "nothing was stored");
}

#[test]
fn an_omarchy_import_is_listed_and_read_back_with_its_source_and_report() {
    let harness = Harness::start("omarchy-round-trip");
    let files = synthetic("omarchy4");
    let params = json!({"format": "omarchy", "files": files, "folder": "deep-sea"});
    let inspected = harness.ok("inspect", "theme.inspect", params.clone());
    let mut import = params;
    import["mutation"] = mutation("import");
    let imported = harness.ok("import", "theme.import", import);
    assert_eq!(imported["report"], inspected["report"]);
    assert_eq!(imported["theme"]["name"], "Deep Sea");
    let id = imported["theme"]["id"].as_str().unwrap().to_owned();

    let listed = harness.ok("list", "theme.list", json!({}));
    let row = &listed["themes"][BUILT_IN.len()];
    assert_eq!(row["id"], id.as_str());
    assert_eq!(
        row["origin"],
        json!({"kind": "omarchy", "folder": "deep-sea", "form": "omarchy4"})
    );
    assert_eq!(row["built_in"], false);
    assert_eq!(row["report"]["unused"], 3);
    assert_eq!(
        row["report"]["derived"],
        json!(imported["report"]["derived"].as_array().unwrap().len())
    );

    let read = harness.ok("read", "theme.read", json!({"theme_id": id}));
    assert_eq!(read["theme"]["source"], json!(files));
    assert_eq!(read["theme"]["report"], imported["report"]);
    assert_eq!(read["theme"]["report"]["omarchy"]["form"], "omarchy4");
    assert_eq!(
        read["theme"]["report"]["omarchy"]["unused"][0]["key"],
        "hyprland_active_border"
    );
    let set = harness.ok("choose", "preferences.set", json!({"theme": id}));
    assert_eq!(set["theme"], id.as_str());
}

#[test]
fn the_bundled_themes_are_listed_after_luxforge_dark_and_can_be_chosen_but_not_taken() {
    let harness = Harness::start("bundled");
    let listed = harness.ok("list", "theme.list", json!({}));
    let rows = listed["themes"].as_array().unwrap();
    let names: Vec<&str> = rows
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, BUILT_IN);
    for (row, bundled) in rows[1..].iter().zip(omarchy::BUNDLED) {
        assert_eq!(row["id"], bundled.id());
        assert_eq!(row["built_in"], true);
        assert_eq!(
            row["origin"],
            json!({"kind": "omarchy", "folder": bundled.slug, "form": "omarchy4",
                   "commit": omarchy::OMARCHY_COMMIT})
        );
        let read = harness.ok("read", "theme.read", json!({"theme_id": bundled.id()}));
        assert_eq!(
            read["theme"]["source"],
            json!({"colors.toml": bundled.colors_toml})
        );
        let palette = bundled.read().unwrap();
        let roles = &read["theme"]["report"]["omarchy"]["roles"];
        assert_eq!(roles[1]["value"], palette.background.to_string());
        assert_eq!(
            read["theme"]["roles"]["text"],
            palette.foreground.to_string()
        );
        assert_eq!(read["theme"]["roles"]["accent"], palette.accent.to_string());
        assert_eq!(
            (
                read["theme"]["actor"].clone(),
                read["theme"]["created_ms"].clone()
            ),
            (Value::Null, Value::Null)
        );
    }
    assert!(!harness.library().exists(), "listing stores nothing");

    // Chosen like any theme, announced, and drawn from launch.
    let set = harness.ok(
        "choose",
        "preferences.set",
        json!({"theme": "omarchy.nord"}),
    );
    assert_eq!(set["theme"], "omarchy.nord");
    assert_eq!(
        harness.ok("list", "theme.list", json!({}))["active"],
        "omarchy.nord"
    );
    let launch = crate::preferences::LaunchPreferences::read(Some(harness.dir.join("config")));
    assert_eq!(
        (launch.theme.id.as_str(), launch.theme.name.as_str()),
        ("omarchy.nord", "Nord")
    );
    assert!(launch.theme.problem.is_none());

    // Never deleted, active or not, and its name never taken.
    let (code, message) = harness.refused(
        "theme.delete",
        json!({"theme_id": "omarchy.nord", "mutation": mutation("delete")}),
    );
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("validation", "Nord is built in and cannot be deleted")
    );
    let nord = omarchy::BUNDLED[4];
    assert_eq!(nord.slug, "nord");
    for params in [
        json!({"format": "omarchy", "files": {"colors.toml": nord.colors_toml},
               "folder": "nord", "mutation": mutation("nord")}),
        json!({"format": "omarchy", "files": {"colors.toml": nord.colors_toml},
               "name": "tokyo NIGHT", "mutation": mutation("tokyo")}),
        json!({"format": "luxforge", "content": night("Everforest"),
               "mutation": mutation("everforest")}),
    ] {
        let (code, message) = harness.refused("theme.import", params);
        assert_eq!(code, "conflict", "{message}");
        assert!(message.starts_with("a theme named \""), "{message}");
    }
    assert!(!harness.library().exists(), "nothing was stored");
    let exported = harness.ok(
        "export",
        "theme.export",
        json!({"theme_id": "omarchy.nord"}),
    );
    assert_eq!(exported["file_name"], "Nord.lftheme");
    let announced: Vec<_> = harness.events().into_iter().map(|(_, id)| id).collect();
    assert_eq!(announced, ["choose"]);
}
