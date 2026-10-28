//! The external modules' view models on their own: the review sheet for each answer
//! `module.package.inspect` gives (a new package, an update, one that changes its payload format,
//! the same package again, a folder module of the same identity, and a refusal), the Uninstall…
//! confirmation, the badges, the pending words, the program's words, and what a write says.
use super::*;
use luxforge_core::module_package::ModuleManifest;
use serde_json::json;

const MANIFEST: &str = r#"
api = "0.1"
id = "org.example.glow"
name = "glow"
title = "Glow"
hint = "Glow the picture"
version = "1.1.0"
authors = ["Example"]
license = "MIT"
repository = "https://example.org/glow"
toolset = "adjustments"

[effect]
stage = "color"
format = 1

[[fields]]
name = "amount"
kind = "number"
min = -100
max = 100
default = 0
label = "Amount"

[[groups]]
label = "Glow"
fields = ["amount"]
"#;

/// What `module.package.inspect` answers for the package, as the core writes it, with `extra`'s
/// keys over it.
fn inspect_answer(extra: Value) -> Value {
    let manifest = ModuleManifest::parse(MANIFEST).expect("the manifest parses");
    let descriptor = manifest.descriptor().clone();
    let mut answer = json!({
        "path": "/photos/glow.lfmodule",
        "bytes": 4096,
        "hash": "sha256:abc",
        "manifest": manifest,
        "program": true,
        "descriptor": descriptor,
        "placements": luxforge_core::module_layout::placements(&descriptor),
        "methods": ["edit.set-glow", "edit.reset-glow"],
        "access": {"network": "none", "files": "none"},
        "reviewed": false,
        "installed": null,
        "already_installed": false,
        "format_change": null,
        "status": null,
    });
    for (key, value) in extra.as_object().expect("an object") {
        answer[key] = value.clone();
    }
    answer
}

fn review(extra: Value) -> Review {
    let inspected = Inspected::parse(inspect_answer(extra)).expect("the answer reads");
    Review {
        state: ReviewState::Inspected(Box::new(inspected)),
        ..Review::new(PathBuf::from("/photos/glow.lfmodule"))
    }
}

fn fact<'a>(model: &'a ReviewModel, label: &str) -> &'a str {
    model
        .facts
        .iter()
        .find(|(name, _)| name == label)
        .map(|(_, value)| value.as_str())
        .unwrap_or_else(|| panic!("no {label} fact in {:?}", model.facts))
}

fn row(state: &str, installed: Option<Value>, running: Option<Value>) -> PackageRow {
    serde_json::from_value(json!({
        "module": "org.example.glow",
        "origin": "community",
        "state": state,
        "reason": (state == "failed").then_some("its component imports log"),
        "installed": installed,
        "running": running,
        "program": {"state": "present"},
    }))
    .expect("a row")
}

/// A new package's sheet names what it adds and where, how it renders, its program, its access,
/// its authorship, its hash and that Luxforge has not reviewed it, and offers Install.
#[test]
fn external_module_review_of_a_new_package_offers_install_with_every_fact() {
    let review = review(json!({}));
    let model = derive_review(&review);
    assert_eq!(model.title, "Install Glow 1.1.0?");
    assert!(
        model
            .lead
            .as_deref()
            .is_some_and(|lead| lead.starts_with("Glow 1.1.0 comes from its author")
                && lead.contains("hasn't reviewed")),
        "{:?}",
        model.lead
    );
    assert_eq!(fact(&model, "Adds"), "A section in Adjustments, last");
    assert!(fact(&model, "Renders").starts_with("In Colour (order"));
    assert!(fact(&model, "Program").starts_with("GPU program"));
    assert_eq!(fact(&model, "Network"), "None");
    assert_eq!(fact(&model, "Files"), "None");
    assert_eq!(fact(&model, "Author"), "Example");
    assert_eq!(fact(&model, "Licence"), "MIT");
    assert_eq!(fact(&model, "Source"), "https://example.org/glow");
    assert_eq!(fact(&model, "Version"), "1.1.0 \u{b7} interface 0.1");
    assert_eq!(fact(&model, "API"), "edit.set-glow, edit.reset-glow");
    assert_eq!(fact(&model, "Package"), "sha256:abc");
    assert_eq!(fact(&model, "File"), "/photos/glow.lfmodule \u{b7} 4 KB");
    assert_eq!(model.note.as_deref(), Some(APPLIES_AT_RELAUNCH));
    assert_eq!(model.warning, None);
    let confirm = model.confirm.expect("Install");
    assert_eq!(
        (confirm.label.as_str(), confirm.destructive, confirm.enabled),
        ("Install", false, true)
    );
    assert_eq!(model.dismiss, "Cancel");
    assert!(review.installable());

    // Without a program the sheet says the CPU reference renders it.
    let model = derive_review(&self::review(json!({"program": false})));
    assert_eq!(
        fact(&model, "Program"),
        "CPU reference only: it renders correctly, but more slowly"
    );

    // While `module.install` is in flight the button is disabled and the sheet says so.
    let mut installing = review;
    installing.installing = true;
    let model = derive_review(&installing);
    assert!(!model.confirm.expect("Install").enabled);
    assert_eq!(model.busy, Some("Installing\u{2026}"));
    assert!(!installing.installable());

    // A refused install keeps the sheet and says why.
    installing.installing = false;
    installing.failed = Some("the store could not be written".into());
    let model = derive_review(&installing);
    assert_eq!(
        model.warning.as_deref(),
        Some("the store could not be written")
    );
    assert!(installing.installable(), "it can be tried again");
}

/// An update replaces the installed version; one whose payload format changed says the notice
/// the owner wrote and its button is destructive; the same package again and a folder module of
/// the same identity offer nothing to press.
#[test]
fn external_module_review_of_an_update_a_format_change_the_same_package_and_a_folder() {
    let installed = json!({"title": "Glow", "version": "1.0.0", "format": 1});
    let model = derive_review(&review(json!({"installed": installed})));
    assert_eq!(model.title, "Replace Glow 1.0.0 with 1.1.0?");
    let confirm = model.confirm.expect("Replace");
    assert_eq!(
        (confirm.label.as_str(), confirm.destructive),
        ("Replace", false)
    );

    let notice = "Photos edited with format 1 may not open the same way.";
    let model = derive_review(&review(json!({
        "installed": installed,
        "format_change": {"installed": 1, "package": 2, "notice": notice},
    })));
    assert_eq!(model.warning.as_deref(), Some(notice));
    let confirm = model.confirm.expect("Replace anyway");
    assert_eq!(
        (confirm.label.as_str(), confirm.destructive),
        ("Replace anyway", true)
    );

    let same = review(json!({"installed": installed, "already_installed": true}));
    let model = derive_review(&same);
    assert_eq!(model.title, "Glow 1.1.0 is installed");
    assert_eq!(
        model.note.as_deref(),
        Some("This exact package is already installed.")
    );
    assert!(model.confirm.is_none() && !same.installable());
    assert_eq!(model.dismiss, "Close");

    let folder = review(json!({"installed": {
        "title": "Glow", "path": "/work/glow", "origin": "folder"}}));
    let model = derive_review(&folder);
    assert!(model.confirm.is_none() && !folder.installable());
    assert!(
        model
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("loaded from a folder at /work/glow")),
        "{:?}",
        model.warning
    );
}

/// A package the reader refuses shows the owner's reason, with no Install, only Close; one still
/// being read says so.
#[test]
fn external_module_review_of_a_refused_package_says_why_without_install() {
    let mut review = Review::new(PathBuf::from("/photos/broken.lfmodule"));
    let model = derive_review(&review);
    assert_eq!(model.busy, Some("Reading the package\u{2026}"));
    assert!(model.confirm.is_none());

    review.state = ReviewState::Refused {
        reason: "the package's manifest names api 0.9, and this Luxforge reads 0.1".into(),
        refusal: Some("api".into()),
    };
    let model = derive_review(&review);
    assert_eq!(model.title, "This package can't be installed");
    assert_eq!(
        model.warning.as_deref(),
        Some("the package's manifest names api 0.9, and this Luxforge reads 0.1")
    );
    assert!(model.confirm.is_none() && !review.installable());
    assert_eq!(model.dismiss, "Close");
}

/// Uninstall… says the photos keep their edits and names no count.
#[test]
fn external_module_uninstall_confirmation_says_what_photos_keep() {
    let model = derive_uninstall(&Uninstall {
        module: "org.example.glow".into(),
        title: "Glow".into(),
        writing: false,
        failed: None,
    });
    assert_eq!(model.title, "Uninstall Glow?");
    assert!(
        model.body.starts_with(
            "Photos that use Glow keep their edits, but can't be previewed or exported with them \
             until Glow is installed again."
        ),
        "{}",
        model.body
    );
    assert!(!model.body.chars().any(|c| c.is_ascii_digit()), "no count");
    assert!(!model.busy && model.failed.is_none());
}

/// A row carries its origin badge and its state badge; the pending words say what the next
/// launch does; the detail offers Uninstall… for an installed package and quotes the loader.
#[test]
fn external_module_badges_pending_words_and_detail() {
    assert_eq!(origin_badge(ModuleOrigin::BuiltIn), None);
    assert_eq!(origin_badge(ModuleOrigin::Community), Some(COMMUNITY));
    assert_eq!(origin_badge(ModuleOrigin::Folder), Some(FROM_FOLDER));
    assert_eq!(
        missing_module("org.example.glow.adjust"),
        "org.example.glow"
    );
    assert_eq!(missing_module("other.effect"), "other.effect");

    let v = |version: &str| Some(json!({"title": "Glow", "version": version}));
    let installed = row("pending-relaunch", v("1.0.0"), None);
    assert_eq!(
        row_badges(ModuleOrigin::Community, Some(&installed)),
        [
            RowBadge {
                label: COMMUNITY,
                tone: Tone::Neutral
            },
            RowBadge {
                label: RELAUNCH_TO_APPLY,
                tone: Tone::Accent
            },
        ]
    );
    assert_eq!(
        installed.pending().as_deref(),
        Some("Installed: it loads when Luxforge relaunches.")
    );
    let updated = row("pending-relaunch", v("1.1.0"), v("1.0.0"));
    assert_eq!(
        updated.pending().as_deref(),
        Some("Version 1.1.0 replaces 1.0.0 when Luxforge relaunches.")
    );
    let removed = row("pending-relaunch", None, v("1.0.0"));
    assert_eq!(
        removed.pending().as_deref(),
        Some("Uninstalled: it is removed when Luxforge relaunches.")
    );
    let loaded = row("loaded", v("1.0.0"), v("1.0.0"));
    assert_eq!(loaded.pending(), None);
    assert_eq!(state_badge(&loaded), None);

    let failed = row("failed", v("1.0.0"), None);
    assert_eq!(
        state_badge(&failed),
        Some(RowBadge {
            label: DIDNT_LOAD,
            tone: Tone::Error
        })
    );
    let detail = external_detail(&failed, false);
    assert_eq!(detail.reason.as_deref(), Some("its component imports log"));
    assert!(detail.uninstall && !detail.remove);
    assert_eq!(detail.trust, TRUST);

    let detail = external_detail(&updated, false);
    assert_eq!(
        detail.version.as_deref(),
        Some("Version 1.0.0 \u{b7} 1.1.0 at relaunch")
    );

    let mut list = PackageList::parse(json!({
        "modules": [], "pending_relaunch": false, "store": {"state": "ready"}}))
    .unwrap();
    assert!(list.writable() && list.problem().is_none());
    list.adopt(installed);
    assert!(list.pending_relaunch && list.pending() == 1);
    let unreadable = PackageList::parse(json!({
        "modules": [], "pending_relaunch": false,
        "store": {"state": "unreadable", "reason": "modules.json is not JSON"}}))
    .unwrap();
    assert!(!unreadable.writable());
    assert_eq!(
        unreadable.problem().as_deref(),
        Some("The installed modules can't be read: modules.json is not JSON")
    );
}

/// The program's words follow what the owner reports; a state it adds later is shown as it is,
/// with its reason, with no change here.
#[test]
fn external_module_program_words_follow_the_reported_state() {
    let words = |value: Value| ProgramView::of(&value).words();
    assert!(words(json!(true)).starts_with("GPU program, checked"));
    assert!(words(json!(false)).starts_with("CPU reference only"));
    assert!(words(json!({"state": "present"})).starts_with("GPU program, checked"));
    assert_eq!(
        words(json!({"state": "disqualified", "reason": "differs by 0.02 at pixel 4,7"})),
        "GPU program disqualified: differs by 0.02 at pixel 4,7"
    );
    assert_eq!(
        words(json!({"state": "qualified"})),
        "GPU program qualified"
    );
}

/// Each write names exactly one method and its parameters, and says what the owner answered.
#[test]
fn external_module_writes_name_one_method_and_say_the_answer() {
    let install = PackageWrite::Install(PathBuf::from("/photos/glow.lfmodule"));
    assert_eq!(install.method(), "module.install");
    assert_eq!(install.params(), json!({"path": "/photos/glow.lfmodule"}));
    let status = json!({"module": "org.example.glow", "origin": "community",
        "state": "pending-relaunch", "installed": {"title": "Glow", "version": "1.1.0"},
        "running": null, "program": {"state": "present"}});
    let answer = json!({"module": "org.example.glow", "outcome": "installed",
        "installed": {"version": "1.1.0"}, "status": status});
    assert_eq!(
        install.said(&answer),
        "Installed Glow 1.1.0 \u{b7} relaunch to apply"
    );
    let uninstall = PackageWrite::Uninstall("org.example.glow".into());
    assert_eq!(uninstall.method(), "module.uninstall");
    assert_eq!(uninstall.params(), json!({"module": "org.example.glow"}));
    assert_eq!(
        uninstall.said(&answer),
        "Uninstalled Glow \u{b7} relaunch to apply"
    );
    assert_eq!(
        PackageWrite::FolderLoad(PathBuf::from("/work/glow")).method(),
        "module.folder.load"
    );
    assert_eq!(
        PackageWrite::FolderRemove("org.example.glow".into()).method(),
        "module.folder.remove"
    );
}
