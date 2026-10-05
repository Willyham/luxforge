//! The theme library's store: its round trip and file shape, the name rules, every limit, a stored
//! record this build cannot read kept byte for byte, export back to the same tokens, deletion's
//! refusals and the launch read. The methods over it are the owner's tests
//! (`api/owner/theme_tests.rs`).
use super::library::*;
use super::*;
use crate::{ErrorKind, MutationOutcome};
use luxforge_testbase::paths::temp_path;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

/// A directory of its own, removed at the end.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        Self(temp_path(&format!("themes-{name}")))
    }

    fn store(&self) -> ThemeStore {
        ThemeStore::new(Some(self.0.clone()))
    }

    fn file(&self) -> PathBuf {
        self.0.join("themes.json")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn rgb(text: &str) -> Rgba {
    Rgba::parse(text).unwrap()
}

/// A Luxforge theme document of three roles, which meets every floor.
fn night(name: &str) -> String {
    ThemeDocument {
        name: name.into(),
        roles: Roles::new(rgb("#1e2030"), rgb("#c0caf5"), rgb("#7aa2f7")),
        tokens: Tokens::new(),
    }
    .write()
}

fn luxforge(content: &str) -> ThemeInput<'_> {
    ThemeInput {
        format: ThemeFormat::Luxforge,
        content: Some(content),
        files: None,
        name: None,
    }
}

fn named<'a>(content: &'a str, name: &'a str) -> ThemeInput<'a> {
    ThemeInput {
        name: Some(name),
        ..luxforge(content)
    }
}

fn names(listing: &Listing) -> Vec<&str> {
    listing
        .themes
        .iter()
        .map(|theme| theme.name.as_str())
        .collect()
}

/// `themes.json` as the store writes it, holding `records`.
fn library_text(records: &[Value]) -> Vec<u8> {
    serde_json::to_vec_pretty(&json!({"format": 1, "themes": records})).unwrap()
}

fn stored(dir: &Dir) -> Vec<Value> {
    let value: Value = serde_json::from_slice(&fs::read(dir.file()).unwrap()).unwrap();
    value["themes"].as_array().unwrap().clone()
}

#[test]
fn an_import_is_stored_outside_any_catalog_and_listed_after_the_built_ins() {
    let dir = Dir::new("round-trip");
    let store = dir.store();
    let listing = store.list().unwrap();
    assert_eq!(names(&listing), [LUXFORGE_DARK_NAME]);
    assert_eq!(listing.themes[0].id(), LUXFORGE_DARK_ID);
    assert!(listing.themes[0].built_in && listing.unrecognized.is_empty());
    assert!(!dir.0.exists(), "listing creates nothing");

    let text = night("Night");
    let first = store.import(luxforge(&text), "tester").unwrap();
    let second = store.import(named(&text, "  aurora  "), "tester").unwrap();
    assert_eq!(
        second.name, "aurora",
        "trimmed, and the request's name wins"
    );
    let listing = store.list().unwrap();
    assert_eq!(
        names(&listing),
        [LUXFORGE_DARK_NAME, "aurora", "Night"],
        "the built-in theme first, then by name ignoring case"
    );
    let read = store.theme(first.id()).unwrap();
    assert_eq!(read, first);
    assert!(first.id().starts_with("theme-") && first.id() != second.id());
    assert_eq!(first.origin, ThemeOrigin::Luxforge {});
    assert_eq!(first.actor.as_deref(), Some("tester"));
    assert_eq!(first.created_ms, first.updated_ms);
    assert_eq!(
        first.source,
        std::collections::BTreeMap::from([("Night.lftheme".to_owned(), text.clone())]),
        "the document's text is kept verbatim"
    );
    let (_, expected) = ThemeDocument::read(&text).unwrap();
    assert_eq!(read.resolved, expected);

    // Each record holds exactly its fields, with only the explicit tokens.
    let records = stored(&dir);
    assert_eq!(records.len(), 2);
    let keys: Vec<&str> = records[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut expected_keys = [
        "id",
        "name",
        "mode",
        "roles",
        "tokens",
        "origin",
        "report",
        "source",
        "actor",
        "created_ms",
        "updated_ms",
    ];
    expected_keys.sort();
    assert_eq!(keys, expected_keys);
    assert_eq!(records[0]["tokens"], json!({}));
    assert_eq!(records[0]["origin"], json!({"kind": "luxforge"}));
    assert_eq!(records[0]["mode"], "dark");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(dir.file()).unwrap()).unwrap()["format"],
        1
    );

    // Inspecting stores nothing and names nothing.
    let before = fs::read(dir.file()).unwrap();
    let inspected = store.inspect(named(&text, "Dusk")).unwrap();
    assert_eq!((inspected.id, inspected.actor), (None, None));
    assert_eq!(inspected.name, "Dusk");
    assert_eq!(fs::read(dir.file()).unwrap(), before);

    // With no directory only the built-in themes are held, and nothing can be stored.
    let unconfigured = ThemeStore::new(None);
    assert_eq!(names(&unconfigured.list().unwrap()), [LUXFORGE_DARK_NAME]);
    let error = unconfigured.import(luxforge(&text), "tester").unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotReady);
}

#[test]
fn names_are_checked_and_unique_ignoring_case_across_every_theme() {
    let dir = Dir::new("names");
    let store = dir.store();
    let text = night("Écran");
    store.import(luxforge(&text), "tester").unwrap();
    let before = fs::read(dir.file()).unwrap();
    let long = "n".repeat(MAX_THEME_NAME + 1);
    for (name, kind, refusal) in [
        (
            "luxforge DARK",
            ErrorKind::Conflict,
            "a theme named \"Luxforge Dark\" already exists",
        ),
        (
            "éCRAN",
            ErrorKind::Conflict,
            "a theme named \"Écran\" already exists",
        ),
        (
            "   ",
            ErrorKind::Validation,
            "theme name must contain 1..=64 printable characters",
        ),
        (
            "tab\there",
            ErrorKind::Validation,
            "theme name must contain 1..=64",
        ),
        (
            long.as_str(),
            ErrorKind::Validation,
            "theme name must contain",
        ),
    ] {
        let error = store.import(named(&text, name), "tester").unwrap_err();
        assert_eq!(error.kind, kind, "{name}: {error}");
        assert!(
            error.detail.starts_with(refusal),
            "{name}: {}",
            error.detail
        );
        assert_eq!(
            fs::read(dir.file()).unwrap(),
            before,
            "{name} stored nothing"
        );
    }
    // A document named after the built-in theme is a conflict too, and nothing is renamed.
    let dark = luxforge_dark().write();
    let error = store.import(luxforge(&dark), "tester").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    store
        .import(named(&dark, "Luxforge Dark copy"), "tester")
        .unwrap();
    // 64 characters is a whole name.
    store
        .import(named(&text, &"n".repeat(MAX_THEME_NAME)), "tester")
        .unwrap();
    let error = store.import(named(&text, "x"), "").unwrap_err();
    assert_eq!(error.detail, "actor must contain 1..128 characters");
}

#[test]
fn every_limit_is_refused_by_name_and_stores_nothing() {
    let dir = Dir::new("limits");
    let store = dir.store();
    // A file past 64 KiB.
    let huge = format!("{}{}", night("Huge"), " ".repeat(MAX_THEME_FILE_BYTES));
    let error = store.import(luxforge(&huge), "tester").unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert!(
        error.detail.starts_with("content is ") && error.detail.contains("at most 65536"),
        "{}",
        error.detail
    );
    let files = std::collections::BTreeMap::from([("colors.toml".to_owned(), huge.clone())]);
    let error = store
        .import(
            ThemeInput {
                format: ThemeFormat::Omarchy,
                content: None,
                files: Some(&files),
                name: None,
            },
            "tester",
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert!(
        error.detail.starts_with("file \"colors.toml\" is "),
        "{}",
        error.detail
    );
    // A record past 8 KiB, its source included.
    let padded = format!("{}{}", night("Padded"), " ".repeat(MAX_THEME_BYTES));
    let error = store.import(luxforge(&padded), "tester").unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert!(
        error.detail.contains("a theme is at most 8192"),
        "{}",
        error.detail
    );
    assert!(!dir.file().exists(), "nothing was stored");

    // A full library.
    let text = night("Seed");
    let seed = store.import(luxforge(&text), "tester").unwrap();
    let record = stored(&dir).remove(0);
    let records: Vec<Value> = (0..MAX_THEMES)
        .map(|index| {
            let mut record = record.clone();
            record["id"] = json!(format!("theme-{index:032x}"));
            record["name"] = json!(format!("Theme {index}"));
            record
        })
        .collect();
    fs::write(dir.file(), library_text(&records)).unwrap();
    assert_eq!(store.list().unwrap().themes.len(), MAX_THEMES + 1);
    let full = fs::read(dir.file()).unwrap();
    let error = store
        .import(named(&text, "One more"), "tester")
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert_eq!(
        error.detail,
        "the theme library holds at most 128 themes; delete one first"
    );
    assert_eq!(fs::read(dir.file()).unwrap(), full);
    assert!(seed.id().starts_with("theme-"));

    // A library past 1 MiB is refused when it is read, and kept.
    let mut over = full.clone();
    over.resize(MAX_LIBRARY_BYTES as usize + 1, b' ');
    fs::write(dir.file(), &over).unwrap();
    assert_eq!(store.list().unwrap_err().kind, ErrorKind::ResourceLimit);
    assert_eq!(
        store.import(named(&text, "x"), "tester").unwrap_err().kind,
        ErrorKind::ResourceLimit
    );
    assert_eq!(fs::read(dir.file()).unwrap(), over);
}

#[test]
fn a_malformed_or_foreign_input_is_refused_by_name() {
    let store = Dir::new("malformed").store();
    let unsupported = ErrorKind::UnsupportedInput;
    for (input, kind, refusal) in [
        (luxforge("{"), unsupported, "malformed JSON"),
        (
            luxforge(r#"{"format": "luxforge.preset", "version": 1}"#),
            unsupported,
            "not a Luxforge theme document",
        ),
        (
            ThemeInput {
                files: Some(&Default::default()),
                ..luxforge("{}")
            },
            ErrorKind::Validation,
            "format luxforge takes the document's text as content, and no files",
        ),
        (
            ThemeInput {
                format: ThemeFormat::Omarchy,
                content: Some("x"),
                files: None,
                name: None,
            },
            ErrorKind::Validation,
            "format omarchy takes the theme folder's files as files, and no content",
        ),
        (
            ThemeInput {
                format: ThemeFormat::Omarchy,
                content: None,
                files: Some(&Default::default()),
                name: None,
            },
            unsupported,
            "format omarchy: this build has no Omarchy theme reader yet",
        ),
    ] {
        for error in [
            store.inspect(input).unwrap_err(),
            store.import(input, "tester").unwrap_err(),
        ] {
            assert_eq!(error.kind, kind, "{error}");
            assert!(error.detail.starts_with(refusal), "{}", error.detail);
        }
    }
    // A document that misses a floor is refused by the document's rules.
    let low = ThemeDocument {
        name: "Low".into(),
        roles: Roles::new(rgb("#202023"), rgb("#505055"), rgb("#e2b46a")),
        tokens: Tokens::new(),
    }
    .write();
    let error = store.import(luxforge(&low), "tester").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation, "{error}");
}

/// Records this build cannot read: each is listed with its reason, makes no other unreadable,
/// survives a later write byte for byte and can still be deleted by its id.
#[test]
fn an_unreadable_record_is_listed_and_kept_byte_for_byte() {
    let dir = Dir::new("unrecognized");
    let store = dir.store();
    let good = store.import(luxforge(&night("Good")), "tester").unwrap();
    let record = stored(&dir).remove(0);
    let variant = |id: &str, name: &str, change: &dyn Fn(&mut Value)| {
        let mut record = record.clone();
        record["id"] = json!(id);
        record["name"] = json!(name);
        change(&mut record);
        record
    };
    let bad = [
        variant(
            "theme-00000000000000000000000000000001",
            "Future",
            &|record| {
                record["gradient"] = json!("linear");
            },
        ),
        variant(
            "theme-00000000000000000000000000000002",
            "Unresolved",
            &|record| {
                record["roles"]["text"] = record["roles"]["background"].clone();
            },
        ),
        variant("theme-00000000000000000000000000000003", "good", &|_| {}),
        variant(
            "theme-00000000000000000000000000000004",
            "Other",
            &|record| {
                record["origin"] = json!({"kind": "base16"});
            },
        ),
        variant("luxforge.dark", "Impostor", &|_| {}),
        json!("not a record"),
    ];
    let mut records = vec![record.clone()];
    records.extend(bad.iter().cloned());
    let before = library_text(&records);
    fs::write(dir.file(), &before).unwrap();

    let listing = store.list().unwrap();
    assert_eq!(names(&listing), [LUXFORGE_DARK_NAME, "Good"]);
    let reasons: Vec<(Option<&str>, &str)> = listing
        .unrecognized
        .iter()
        .map(|record| (record.name.as_deref(), record.reason.as_str()))
        .collect();
    assert_eq!(reasons.len(), bad.len(), "{reasons:?}");
    for ((name, reason), expected) in reasons.iter().zip([
        "not a theme record (unknown field `gradient`",
        "it does not resolve:",
        "another theme already holds its id",
        "not a theme record (unknown variant `base16`",
        "\"luxforge.dark\" is not a stored theme's id",
        "not a theme record (invalid type",
    ]) {
        assert!(reason.starts_with(expected), "{name:?}: {reason}");
    }
    let error = store
        .theme("theme-00000000000000000000000000000002")
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert!(
        error.detail.contains("it does not resolve") && error.detail.ends_with("kept unchanged"),
        "{}",
        error.detail
    );
    // Its name is still taken.
    assert_eq!(
        store
            .import(named(&night("x"), "unresolved"), "tester")
            .unwrap_err()
            .kind,
        ErrorKind::Conflict
    );

    // A later write keeps every record exactly: the file up to the end of the last one is what it
    // was, and the new record follows.
    let added = store.import(luxforge(&night("Added")), "tester").unwrap();
    let after = fs::read(dir.file()).unwrap();
    let closing = b"\n  ]\n}";
    assert!(before.ends_with(closing));
    assert!(
        after.starts_with(&before[..before.len() - closing.len()]),
        "{}",
        String::from_utf8_lossy(&after)
    );
    assert_eq!(&stored(&dir)[1..=bad.len()], bad.as_slice());
    assert_eq!(store.theme(added.id()).unwrap().name, "Added");
    assert_eq!(store.theme(good.id()).unwrap(), good);

    // Deleting one by id is the person's explicit choice; the others stay as they were.
    assert_eq!(
        store
            .delete("theme-00000000000000000000000000000001", LUXFORGE_DARK_ID)
            .unwrap(),
        MutationOutcome::Applied
    );
    let left = stored(&dir);
    assert_eq!(left.len(), bad.len() + 1);
    assert_eq!(&left[1..bad.len()], &bad[1..]);
}

#[test]
fn deleting_refuses_a_built_in_or_the_active_theme_and_is_a_no_op_when_absent() {
    let dir = Dir::new("delete");
    let store = dir.store();
    let theme = store.import(luxforge(&night("Night")), "tester").unwrap();
    let before = fs::read(dir.file()).unwrap();
    let error = store.delete(LUXFORGE_DARK_ID, theme.id()).unwrap_err();
    assert_eq!(
        (error.kind, error.detail.as_str()),
        (
            ErrorKind::Validation,
            "Luxforge Dark is built in and cannot be deleted"
        )
    );
    let error = store.delete(theme.id(), theme.id()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(
        error.detail,
        format!(
            "theme {} is the active theme; choose another before deleting it",
            theme.id()
        )
    );
    assert_eq!(fs::read(dir.file()).unwrap(), before);
    assert_eq!(
        store.delete(theme.id(), LUXFORGE_DARK_ID).unwrap(),
        MutationOutcome::Applied
    );
    assert_eq!(
        store.delete(theme.id(), LUXFORGE_DARK_ID).unwrap(),
        MutationOutcome::NoOp
    );
    let error = store.theme(theme.id()).unwrap_err();
    assert_eq!(
        (error.kind, error.detail),
        (
            ErrorKind::Validation,
            format!("unknown theme {}", theme.id())
        )
    );
}

#[test]
fn an_export_imports_back_to_the_same_tokens() {
    let dir = Dir::new("export");
    let store = dir.store();
    let dark = store.export(LUXFORGE_DARK_ID).unwrap();
    assert_eq!(dark.file_name, "Luxforge Dark.lftheme");
    assert_eq!(dark.content, luxforge_dark().write());

    let text = night("Night");
    let night = store.import(luxforge(&text), "tester").unwrap();
    let exported = store.export(night.id()).unwrap();
    assert_eq!(exported.file_name, "Night.lftheme");
    assert_eq!(
        exported.content, text,
        "a document theme is written as it was given"
    );

    // An imported theme whose own inks were moved is written as it resolved, which a document's
    // rules accept, and reads back to the same tokens.
    let mut record = stored(&dir).remove(0);
    record["id"] = json!("theme-0000000000000000000000000000000a");
    record["name"] = json!("Moved");
    record["origin"] = json!({"kind": "omarchy", "folder": "moved", "form": "omarchy-4"});
    record["roles"] = json!({"background": "#202023", "text": "#8090a0", "accent": "#6a4a20"});
    fs::write(dir.file(), library_text(&[record])).unwrap();
    let moved = store
        .theme("theme-0000000000000000000000000000000a")
        .unwrap();
    assert!(moved.resolved.report.adjusted());
    let exported = store.export(moved.id()).unwrap();
    let (_, resolved) = ThemeDocument::read(&exported.content).unwrap();
    assert_eq!(resolved.tokens, moved.resolved.tokens);
    assert_eq!(resolved.mode, moved.resolved.mode);
    let error = store
        .export("theme-ffffffffffffffffffffffffffffffff")
        .unwrap_err();
    assert_eq!(
        error.detail,
        "unknown theme theme-ffffffffffffffffffffffffffffffff"
    );
}

#[test]
fn the_launch_read_draws_the_chosen_theme_or_luxforge_dark_with_the_reason() {
    let dir = Dir::new("launch");
    let store = dir.store();
    let dark = LaunchTheme::read(Some(dir.0.clone()), None);
    assert_eq!(
        (dark.id.as_str(), dark.name.as_str()),
        (LUXFORGE_DARK_ID, LUXFORGE_DARK_NAME)
    );
    assert_eq!(dark.resolved.tokens, Palette::luxforge_dark());
    assert!(dark.problem.is_none());

    let theme = store.import(luxforge(&night("Night")), "tester").unwrap();
    let chosen = LaunchTheme::read(Some(dir.0.clone()), Some(theme.id()));
    assert_eq!(
        (chosen.id.as_str(), chosen.name.as_str()),
        (theme.id(), "Night")
    );
    assert_eq!(chosen.resolved, theme.resolved);
    assert!(chosen.problem.is_none());

    // Missing, unreadable, or in a library that cannot be read: Luxforge Dark, and the reason
    // naming the chosen theme.
    let missing = "theme-ffffffffffffffffffffffffffffffff";
    let mut record = stored(&dir).remove(0);
    record["roles"]["text"] = record["roles"]["background"].clone();
    let unreadable = serde_json::to_vec_pretty(&json!({"format": 1, "themes": [record]})).unwrap();
    for (contents, chosen, kind, reason) in [
        (None, missing, ErrorKind::Validation, "unknown theme"),
        (
            Some(unreadable),
            theme.id(),
            ErrorKind::Incompatible,
            "it does not resolve",
        ),
        (
            Some(b"{\"format\": 2, \"themes\": []}".to_vec()),
            theme.id(),
            ErrorKind::Incompatible,
            "themes format 2 is not supported",
        ),
    ] {
        if let Some(contents) = &contents {
            fs::write(dir.file(), contents).unwrap();
        }
        let launch = LaunchTheme::read(Some(dir.0.clone()), Some(chosen));
        assert_eq!(launch.id, LUXFORGE_DARK_ID);
        assert_eq!(launch.resolved.tokens, Palette::luxforge_dark());
        let problem = launch.problem.unwrap();
        assert_eq!(problem.kind, kind, "{problem}");
        assert!(
            problem.detail.starts_with(&format!(
                "the theme {chosen} cannot be shown, so Luxforge Dark is: "
            )) && problem.detail.contains(reason),
            "{}",
            problem.detail
        );
        if let Some(contents) = contents {
            assert_eq!(
                fs::read(dir.file()).unwrap(),
                contents,
                "nothing is rewritten"
            );
        }
    }
    let unconfigured = LaunchTheme::read(None, Some(missing));
    assert_eq!(unconfigured.id, LUXFORGE_DARK_ID);
    assert!(unconfigured.problem.is_some());
}
