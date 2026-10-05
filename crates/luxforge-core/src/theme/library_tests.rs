//! The theme library's store: its round trip and file shape, the name rules, every limit, a stored
//! record this build cannot read kept byte for byte, export back to the same tokens, deletion's
//! refusals and the launch read. The methods over it are the owner's tests
//! (`api/owner/theme_tests.rs`).
use super::library::*;
use super::*;
use crate::{ErrorKind, MutationOutcome};
use luxforge_testbase::paths::temp_path;
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::PathBuf};

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
        folder: None,
        name: None,
    }
}

/// An Omarchy theme folder's files, sent with the folder's name.
fn omarchy_input<'a>(
    files: &'a BTreeMap<String, String>,
    folder: Option<&'a str>,
) -> ThemeInput<'a> {
    ThemeInput {
        format: ThemeFormat::Omarchy,
        content: None,
        files: Some(files),
        folder,
        name: None,
    }
}

fn named<'a>(content: &'a str, name: &'a str) -> ThemeInput<'a> {
    ThemeInput {
        name: Some(name),
        ..luxforge(content)
    }
}

/// The built-in themes' names, in their order: Luxforge Dark, then the bundled Omarchy themes.
const BUILT_IN: [&str; 7] = [
    LUXFORGE_DARK_NAME,
    "Tokyo Night",
    "Catppuccin",
    "Catppuccin Latte",
    "Gruvbox",
    "Nord",
    "Everforest",
];

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
    assert_eq!(names(&listing), BUILT_IN);
    assert_eq!(listing.themes[0].id(), LUXFORGE_DARK_ID);
    assert!(listing.themes.iter().all(|theme| theme.built_in));
    assert!(listing.unrecognized.is_empty());
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
        [BUILT_IN.as_slice(), &["aurora", "Night"]].concat(),
        "the built-in themes first, then by name ignoring case"
    );
    let read = store.theme(first.id()).unwrap();
    assert_eq!(read, first);
    assert!(first.id().starts_with("theme-") && first.id() != second.id());
    assert_eq!(first.origin, ThemeOrigin::Luxforge {});
    assert_eq!(first.actor.as_deref(), Some("tester"));
    assert_eq!(first.created_ms, first.updated_ms);
    assert_eq!(
        first.source,
        BTreeMap::from([("Night.lftheme".to_owned(), text.clone())]),
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
    assert_eq!(names(&unconfigured.list().unwrap()), BUILT_IN);
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
    let files = BTreeMap::from([("colors.toml".to_owned(), huge.clone())]);
    let error = store
        .import(omarchy_input(&files, Some("huge")), "tester")
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
    assert_eq!(
        store.list().unwrap().themes.len(),
        MAX_THEMES + BUILT_IN.len()
    );
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
    let empty = BTreeMap::new();
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
                folder: None,
                name: None,
            },
            ErrorKind::Validation,
            "format omarchy takes the theme folder's files as files, and no content",
        ),
        (
            ThemeInput {
                folder: Some("nord"),
                ..luxforge("{}")
            },
            ErrorKind::Validation,
            "format luxforge takes no folder",
        ),
        (
            omarchy_input(&empty, Some("nord")),
            unsupported,
            "the theme has neither colors.toml nor alacritty.toml",
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
    assert_eq!(names(&listing), [BUILT_IN.as_slice(), &["Good"]].concat());
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

// Omarchy themes.

fn rgb8(colour: omarchy::Rgb8) -> Rgba {
    let [r, g, b] = colour.0;
    Rgba::rgb(r, g, b)
}

/// The theme maps the six values Omarchy's resolver gives it to roles, and nothing else: every
/// other role and token derives. Its Omarchy report is the reader's, and it meets every floor and
/// the chroma bound after its reported moves.
fn assert_mapped(theme: &Theme, palette: &omarchy::Palette, what: &str) {
    let mode = match palette.mode {
        omarchy::Mode::Dark => Mode::Dark,
        omarchy::Mode::Light => Mode::Light,
    };
    let control = (palette.lighter_background != palette.background)
        .then(|| rgb8(palette.lighter_background));
    let roles = Roles {
        surround: Some(rgb8(palette.dark_background)),
        control,
        mode: Some(mode),
        ..Roles::new(
            rgb8(palette.background),
            rgb8(palette.foreground),
            rgb8(palette.accent),
        )
    };
    assert_eq!(theme.roles, roles, "{what}");
    assert!(theme.tokens.is_empty(), "{what}");
    assert_eq!(theme.resolved.mode, mode, "{what}");
    let report = theme.resolved.report.omarchy.as_ref().expect(what);
    assert_eq!(
        (report.form, report.mode, report.mode_source),
        (palette.form, mode, palette.mode_source),
        "{what}"
    );
    let keys: Vec<(Token, &str, Rgba)> = report
        .roles
        .iter()
        .map(|role| (role.role, role.key.as_str(), role.value))
        .collect();
    let mut expected = vec![(
        Token::Surround,
        "dark_background",
        rgb8(palette.colours["dark_background"]),
    )];
    expected.push((
        Token::Background,
        "background",
        rgb8(palette.colours["background"]),
    ));
    if let Some(control) = control {
        expected.push((Token::Control, "lighter_background", control));
    }
    expected.push((
        Token::Text,
        "foreground",
        rgb8(palette.colours["foreground"]),
    ));
    expected.push((Token::Accent, "accent", rgb8(palette.colours["accent"])));
    assert_eq!(keys, expected, "{what}: each role and the key it came from");
    let derived = (control.is_none()).then(|| OmarchyDerived {
        role: Token::Control,
        key: "lighter_background".into(),
        value: rgb8(palette.lighter_background),
        reason: DerivedReason::EqualsBackground,
    });
    assert_eq!(report.derived, Vec::from_iter(derived), "{what}");
    let unused: Vec<(&str, &str, &str, omarchy::UnusedReason)> = report
        .unused
        .iter()
        .map(|u| (u.file.as_str(), u.key.as_str(), u.value.as_str(), u.reason))
        .collect();
    let read: Vec<(&str, &str, &str, omarchy::UnusedReason)> = palette
        .unused
        .iter()
        .map(|u| (u.file, u.key.as_str(), u.value.as_str(), u.reason))
        .collect();
    assert_eq!(unused, read, "{what}");
    super::tests::assert_legible(&theme.resolved, what);
}

#[test]
fn each_synthetic_omarchy_folder_maps_its_resolved_values_to_roles() {
    let store = ThemeStore::new(None);
    let folders = omarchy::tests::synthetic_folders();
    assert_eq!(folders.len(), 9);
    for (folder, files) in &folders {
        let palette = omarchy::read(files).unwrap();
        let inspected = store.inspect(omarchy_input(files, Some(folder)));
        // The two mode fixtures on either side of Omarchy's brightness threshold are a mid grey
        // with near-black text, which no lightness brings to its floor: refused by name.
        if folder.starts_with("brightness-") {
            let error = inspected.unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation);
            assert!(
                error
                    .detail
                    .starts_with("text #101010 cannot reach 7:1 against the surround"),
                "{folder}: {}",
                error.detail
            );
            continue;
        }
        let theme = inspected.unwrap_or_else(|error| panic!("{folder}: {error}"));
        assert_mapped(&theme, &palette, folder);
        assert_eq!(Some(theme.name.clone()), omarchy::theme_name(folder));
        assert_eq!(
            theme.origin,
            ThemeOrigin::Omarchy {
                folder: folder.clone(),
                form: palette.form.as_str().into(),
                commit: None,
            }
        );
        assert_eq!(theme.origin.resolution(), Resolution::Import);
        // The text of each file read, verbatim: the palette's file and the marker.
        let read: Vec<&str> = theme.source.keys().map(String::as_str).collect();
        let palette_file = match palette.form {
            omarchy::Form::Alacritty => omarchy::ALACRITTY_TOML,
            _ => omarchy::COLORS_TOML,
        };
        let mut expected = vec![palette_file];
        if files.contains_key(omarchy::LIGHT_MODE) {
            expected.push(omarchy::LIGHT_MODE);
        }
        expected.sort();
        assert_eq!(read, expected, "{folder}");
        for (file, text) in &theme.source {
            assert_eq!(&files[file], text, "{folder}: {file}");
        }
    }

    // The documented ones, in full.
    let report = |name: &str| {
        let (_, files) = folders.iter().find(|(folder, _)| folder == name).unwrap();
        let theme = store.inspect(omarchy_input(files, Some(name))).unwrap();
        (
            serde_json::to_value(&theme.resolved.report.omarchy).unwrap(),
            theme,
        )
    };
    let (omarchy4, theme) = report("omarchy4");
    assert_eq!(
        omarchy4,
        json!({
            "form": "omarchy4", "mode": "dark", "mode_source": "mode-key",
            "roles": [
                {"role": "surround", "key": "dark_background", "value": "#14171d"},
                {"role": "background", "key": "background", "value": "#1b1f27"},
                {"role": "control", "key": "lighter_background", "value": "#252a35"},
                {"role": "text", "key": "foreground", "value": "#c9ced8"},
                {"role": "accent", "key": "accent", "value": "#5e9ce0"},
            ],
            "derived": [],
            "unused": [
                {"file": "colors.toml", "key": "hyprland_active_border",
                 "value": "rgba(5e9ce0ee) rgba(98c379ee) 45deg", "reason": "gradient"},
                {"file": "colors.toml", "key": "hyprland_inactive_border",
                 "value": "rgb(252a35)", "reason": "gradient"},
                {"file": "colors.toml", "key": "active_tab_background", "value": "#2b3140",
                 "reason": "unknown-key"},
            ],
        })
    );
    assert_eq!(theme.name, "Omarchy4");
    assert_eq!(
        theme.resolved.report.given,
        [
            "surround",
            "background",
            "control",
            "text",
            "accent",
            "mode"
        ]
    );
    let (omarchy3, theme) = report("omarchy3");
    assert_eq!(
        omarchy3["derived"],
        json!([{"role": "control", "key": "lighter_background", "value": "#2e3440",
                "reason": "equals-background"}])
    );
    assert_eq!(omarchy3["mode_source"], "background-brightness");
    assert_eq!(omarchy3["unused"][0]["reason"], "replaced");
    assert!(
        theme
            .resolved
            .report
            .derived
            .contains(&"control".to_owned())
    );
    let (alacritty, theme) = report("alacritty-light");
    assert_eq!(
        (&alacritty["form"], &alacritty["mode_source"]),
        (&json!("alacritty"), &json!("light-mode-file"))
    );
    assert_eq!(theme.resolved.mode, Mode::Light);
}

#[test]
fn an_omarchy_theme_is_named_after_its_folder_unless_named_and_refused_with_neither() {
    let dir = Dir::new("omarchy-names");
    let store = dir.store();
    let files = BTreeMap::from([(
        "colors.toml".to_owned(),
        "background = \"#1b1f27\"\nforeground = \"#c9ced8\"\naccent = \"#5e9ce0\"\n".to_owned(),
    )]);
    let input = |folder, name| ThemeInput {
        name,
        ..omarchy_input(&files, folder)
    };
    let named = |folder, name| store.inspect(input(folder, name)).map(|theme| theme.name);
    assert_eq!(named(Some("deep-sea"), None).unwrap(), "Deep Sea");
    assert_eq!(
        named(Some("omarchy-deep-sea-theme"), None).unwrap(),
        "Deep Sea"
    );
    assert_eq!(named(Some("deep-sea"), Some("Abyss")).unwrap(), "Abyss");
    assert_eq!(named(Some("omarchy-"), Some("Abyss")).unwrap(), "Abyss");
    let unnamed = store.inspect(input(None, Some("Abyss"))).unwrap();
    assert_eq!(unnamed.name, "Abyss");
    assert_eq!(
        unnamed.origin,
        ThemeOrigin::Omarchy {
            folder: String::new(),
            form: "omarchy4".into(),
            commit: None
        },
        "no folder is kept as none"
    );
    for (folder, name, refusal) in [
        (
            None,
            None,
            "format omarchy takes the theme folder's name as folder, or a name for the theme",
        ),
        (
            Some("omarchy-"),
            None,
            "the folder \"omarchy-\" gives the theme no name; give the theme a name",
        ),
        (Some(""), None, "folder \"\" is not a theme folder's name"),
        (
            Some(".."),
            None,
            "folder \"..\" is not a theme folder's name",
        ),
        (
            Some("themes/nord"),
            None,
            "folder \"themes/nord\" is not a theme folder's name",
        ),
        (
            Some("themes\\nord"),
            Some("Nord 2"),
            "folder \"themes\\\\nord\" is not a theme folder's name",
        ),
        (Some("tab\tbed"), None, "folder \"tab\\tbed\" is not"),
        (
            Some(&"n".repeat(MAX_THEME_FOLDER + 1)),
            None,
            "folder \"nnn",
        ),
    ] {
        for error in [
            store.inspect(input(folder, name)).unwrap_err(),
            store.import(input(folder, name), "tester").unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Validation, "{error}");
            assert!(error.detail.starts_with(refusal), "{}", error.detail);
        }
    }
    assert!(!dir.file().exists(), "nothing was stored");
}

/// Only `colors.toml`, `alacritty.toml` and `light.mode` are taken: any other name, beside a
/// readable palette, is refused by name with the reader's coded error, and nothing is stored.
#[test]
fn no_omarchy_file_but_the_three_the_reader_takes_is_accepted() {
    let dir = Dir::new("omarchy-files");
    let store = dir.store();
    let colors = "background = \"#1b1f27\"\nforeground = \"#c9ced8\"\naccent = \"#5e9ce0\"\n";
    for name in [
        "neovim.lua",
        "backgrounds/1.png",
        "preview.png",
        "icons.theme",
        "hyprland.conf",
        "Colors.toml",
        "colors.toml.bak",
        " colors.toml",
        "./colors.toml",
        "../colors.toml",
        "nord/colors.toml",
        "light.mode/x",
        "",
    ] {
        let files = BTreeMap::from([
            ("colors.toml".to_owned(), colors.to_owned()),
            (name.to_owned(), String::new()),
        ]);
        for error in [
            store
                .inspect(omarchy_input(&files, Some("deep-sea")))
                .unwrap_err(),
            store
                .import(omarchy_input(&files, Some("deep-sea")), "tester")
                .unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::Validation, "{name:?}: {error}");
            assert_eq!(
                error.detail,
                format!(
                    "{name:?} is not read; an Omarchy theme is read from colors.toml, \
                     alacritty.toml and light.mode only"
                )
            );
            assert_eq!(
                error.data.as_deref(),
                Some(&json!({"code": "file-not-allowed", "file": name}))
            );
        }
    }
    assert!(!dir.file().exists(), "nothing was stored");
    // All three together: colors.toml is read, and alacritty.toml beside it neither read nor kept.
    let files = BTreeMap::from([
        ("colors.toml".to_owned(), colors.to_owned()),
        ("alacritty.toml".to_owned(), "[".to_owned()),
        ("light.mode".to_owned(), String::new()),
    ]);
    let theme = store
        .import(omarchy_input(&files, Some("deep-sea")), "tester")
        .unwrap();
    assert_eq!(
        theme.source.keys().collect::<Vec<_>>(),
        ["colors.toml", "light.mode"]
    );
    assert_eq!(theme.resolved.mode, Mode::Light);
}

#[test]
fn an_omarchy_import_round_trips_with_its_source_and_report() {
    let dir = Dir::new("omarchy-round-trip");
    let store = dir.store();
    let (_, files) = omarchy::tests::synthetic_folders()
        .into_iter()
        .find(|(folder, _)| folder == "omarchy4")
        .unwrap();
    let imported = store
        .import(omarchy_input(&files, Some("deep-sea")), "tester")
        .unwrap();
    assert_eq!(imported.name, "Deep Sea");
    let listing = store.list().unwrap();
    let listed = listing.themes.last().unwrap();
    assert_eq!(listed, &imported);
    let read = store.theme(imported.id()).unwrap();
    assert_eq!(read, imported, "the Omarchy report is kept with the record");
    assert_eq!(read.source, files);
    assert_eq!(
        read.resolved.report.omarchy.as_ref().unwrap().unused.len(),
        3
    );
    let record = stored(&dir).remove(0);
    assert_eq!(
        record["origin"],
        json!({"kind": "omarchy", "folder": "deep-sea", "form": "omarchy4"})
    );
    assert_eq!(record["report"]["omarchy"]["form"], "omarchy4");
    assert_eq!(read.summary()["report"]["unused"], 3);
    assert_eq!(
        read.summary()["report"]["derived"],
        read.resolved.report.derived.len()
    );
    // Exported, it is a Luxforge document of the tokens it resolved to.
    let exported = store.export(imported.id()).unwrap();
    let (_, resolved) = ThemeDocument::read(&exported.content).unwrap();
    assert_eq!(resolved.tokens, imported.resolved.tokens);
}

#[test]
fn the_bundled_themes_are_built_in_read_by_the_same_reader_and_never_stored() {
    let dir = Dir::new("bundled");
    let store = dir.store();
    let listing = store.list().unwrap();
    assert_eq!(names(&listing), BUILT_IN);
    assert_eq!(built_in_themes(), listing.themes.as_slice());
    for (theme, bundled) in listing.themes[1..].iter().zip(omarchy::BUNDLED) {
        assert_eq!(theme.id(), format!("omarchy.{}", bundled.slug));
        assert!(theme.built_in);
        assert_eq!(
            theme.origin,
            ThemeOrigin::Omarchy {
                folder: bundled.slug.into(),
                form: "omarchy4".into(),
                commit: Some(omarchy::OMARCHY_COMMIT.into()),
            }
        );
        assert_eq!(
            theme.source,
            BTreeMap::from([("colors.toml".to_owned(), bundled.colors_toml.to_owned())])
        );
        assert_eq!((theme.actor.as_ref(), theme.created_ms), (None, None));
        // Omarchy's values, before the reported moves, and every floor met after them.
        assert_mapped(theme, &bundled.read().unwrap(), bundled.slug);
        assert_eq!(store.theme(theme.id()).unwrap(), *theme);
        // An import of the same folder resolves to the same tokens and report.
        let files = theme.source.clone();
        let inspected = store
            .inspect(omarchy_input(&files, Some(bundled.slug)))
            .unwrap();
        assert_eq!(inspected.resolved, theme.resolved, "{}", bundled.slug);
        // It cannot be deleted, and its name is taken, ignoring case.
        let error = store.delete(theme.id(), LUXFORGE_DARK_ID).unwrap_err();
        assert_eq!(
            (error.kind, error.detail),
            (
                ErrorKind::Validation,
                format!("{} is built in and cannot be deleted", theme.name)
            )
        );
        for input in [
            omarchy_input(&files, Some(bundled.slug)),
            ThemeInput {
                name: Some(&theme.name.to_uppercase()),
                ..omarchy_input(&files, None)
            },
        ] {
            let error = store.import(input, "tester").unwrap_err();
            assert_eq!(error.kind, ErrorKind::Conflict, "{error}");
            assert_eq!(
                error.detail,
                format!(
                    "a theme named {:?} already exists; choose another name",
                    theme.name
                )
            );
        }
    }
    assert!(!dir.file().exists(), "a bundled theme is never stored");
    // Under another name it imports as a theme of its own.
    let files = built_in_themes()[5].source.clone();
    let copy = store
        .import(
            ThemeInput {
                name: Some("Nord copy"),
                ..omarchy_input(&files, Some("nord"))
            },
            "tester",
        )
        .unwrap();
    assert!(!copy.built_in && copy.id().starts_with("theme-"));
    assert_eq!(copy.resolved, built_in_themes()[5].resolved);
    let launch = LaunchTheme::read(Some(dir.0.clone()), Some("omarchy.nord"));
    assert_eq!(
        (launch.id.as_str(), launch.name.as_str()),
        ("omarchy.nord", "Nord")
    );
    assert!(launch.problem.is_none());
}

/// What the mapping makes of Omarchy's 22 built-in themes at the pinned commit: the figures the
/// design's table "What the 22 built-in themes become" records.
#[test]
fn the_22_built_in_omarchy_themes_become_the_recorded_figures() {
    let store = ThemeStore::new(None);
    let folders = omarchy::tests::built_in_folders();
    assert_eq!(folders.len(), 22);
    let mut neutralised = Vec::new();
    let mut text = Vec::new();
    let mut accent = Vec::new();
    let mut error = Vec::new();
    let mut secondary = Vec::new();
    let mut tertiary = Vec::new();
    let mut close = Vec::new();
    let mut control = Vec::new();
    for (slug, files) in &folders {
        let theme = store
            .inspect(omarchy_input(files, Some(slug)))
            .unwrap_or_else(|error| panic!("{slug}: {error}"));
        assert_mapped(&theme, &omarchy::read(files).unwrap(), slug);
        let report = &theme.resolved.report;
        let slug = slug.as_str();
        if report.surround.before != report.surround.after {
            neutralised.push((slug, report.surround.chroma));
        }
        for moved in &report.moved {
            let list = match moved.ink {
                Token::Text => &mut text,
                Token::Accent => &mut accent,
                Token::Error => &mut error,
                ink => panic!("{slug}: {ink} moved"),
            };
            list.push((slug, moved.difference));
        }
        for tier in &report.shortened {
            match tier.tier {
                Token::TextSecondary => secondary.push(slug),
                Token::TextTertiary => tertiary.push(slug),
                other => panic!("{slug}: {other} shortened"),
            }
        }
        if report.accent.close {
            close.push((slug, report.accent.nearest, report.accent.distance));
        }
        if report.omarchy.as_ref().unwrap().derived.len() == 1 {
            control.push(slug);
        }
        if slug == "retro-82" {
            assert_eq!(
                (report.surround.before, report.surround.after),
                (rgb("#031222"), rgb("#0e1215"))
            );
        }
    }
    assert_eq!(
        neutralised,
        [
            ("catppuccin", 0.0237),
            ("ethereal", 0.0324),
            ("everforest", 0.0127),
            ("flexoki-light", 0.0149),
            ("hackerman", 0.0155),
            ("kanagawa", 0.0138),
            ("lumon", 0.0197),
            ("nord", 0.0183),
            ("osaka-jade", 0.0148),
            ("retro-82", 0.0402),
            ("rose-pine", 0.0104),
            ("tokyo-night", 0.0162),
        ]
    );
    assert_eq!(
        text,
        [
            ("catppuccin-latte", 3.41),
            ("everforest", 2.2),
            ("gruvbox", 2.26),
            ("rose-pine", 3.83),
            ("tokyo-night", 0.55),
        ]
    );
    assert_eq!(accent, [("rose-pine", 2.11), ("white", 1.93)]);
    assert_eq!(
        error,
        [
            ("catppuccin-latte", 1.92),
            ("everforest", 0.79),
            ("flexoki-light", 0.96),
            ("nord", 2.79),
            ("white", 10.91),
        ]
    );
    assert_eq!(
        secondary,
        [
            "catppuccin-latte",
            "everforest",
            "gruvbox",
            "miasma",
            "nord",
            "osaka-jade",
            "rose-pine",
            "tokyo-night",
        ]
    );
    assert_eq!(
        tertiary,
        [
            "catppuccin",
            "catppuccin-latte",
            "everforest",
            "flexoki-light",
            "gruvbox",
            "kanagawa",
            "lupine",
            "matte-black",
            "miasma",
            "nord",
            "osaka-jade",
            "ristretto",
            "rose-pine",
            "solitude",
            "tokyo-night",
            "white",
        ]
    );
    use ReservedColour::{ClippingBlue, ClippingRed, MaskGreen, MaskWhite};
    assert_eq!(
        close,
        [
            ("catppuccin", ClippingBlue, 12.85),
            ("catppuccin-latte", ClippingBlue, 12.55),
            ("ethereal", ClippingBlue, 11.03),
            ("hackerman", MaskGreen, 10.59),
            ("kanagawa", MaskWhite, 14.87),
            ("lupine", ClippingBlue, 13.44),
            ("nord", ClippingBlue, 9.72),
            ("ristretto", ClippingRed, 13.9),
            ("tokyo-night", ClippingBlue, 8.74),
        ]
    );
    assert_eq!(control, ["last-horizon", "solitude"]);
}
