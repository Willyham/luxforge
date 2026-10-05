//! Import Omarchy theme… against a real owner: a chosen folder classified as one theme, a set or
//! neither; the bound on a set's subfolders and on each file; only the reader's files read, and
//! `alacritty.toml` only without a `colors.toml`; each theme named after its folder and imported
//! with its own report, a conflict listed as built in or already imported and a file that cannot
//! be read failing its own theme, not the set; the request an independent JSON client sends; the
//! tab's and the status bar's account of it; and the evidence steps that drive it and a second
//! client's `preferences.set`. Tasks are run here as the plain functions they wrap.
use super::{
    Editor,
    message::{
        Message, evidence::EvidenceMessage, preferences::PreferenceMessage,
        settings::SettingsMessage, sync::SyncMessage, theme::ThemeMessage,
    },
    tasks::{self, call},
    theme_folder::{
        Folder, MAX_SET, classify, import_folder_now, import_params, omarchy_folders, read_files,
    },
    themes_tests::{Host, answer_list, answer_read, finish, fixture, without_request_id},
};
use crate::state::{settings::SettingsTab, themes::FolderOutcome};
use luxforge_core::theme::MAX_THEME_FILE_BYTES;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The synthetic set: Dusk (dark), Linen (light, in a cloned repository's folder), a theme named
/// like the bundled Nord, one with no accent and a folder of notes that is no theme.
fn set() -> PathBuf {
    fixture("fixtures/themes/omarchy")
}

fn dusk() -> PathBuf {
    set().join("dusk")
}

/// A folder of its own under the system's temporary directory, removed by [`Temp::drop`].
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let path = luxforge_testbase::paths::temp_path(name);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    /// A theme folder `name` holding these files.
    fn theme(&self, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let folder = self.0.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        for (file, bytes) in files {
            std::fs::write(folder.join(file), bytes).unwrap();
        }
        folder
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A valid `colors.toml` padded with a comment to exactly `bytes` bytes.
fn padded_colours(bytes: usize) -> Vec<u8> {
    let mut text = std::fs::read(dusk().join("colors.toml")).unwrap();
    text.extend(b"# ");
    text.resize(bytes - 1, b'x');
    text.push(b'\n');
    text
}

/// What each theme of an import became, by folder and outcome kind.
fn outcomes(themes: &[crate::state::themes::FolderTheme]) -> Vec<(&str, &'static str)> {
    themes
        .iter()
        .map(|theme| (theme.folder.as_str(), theme.outcome.kind()))
        .collect()
}

#[test]
fn a_folder_is_one_theme_a_set_of_them_or_neither() {
    assert_eq!(classify(&dusk()).unwrap(), Folder::Theme(dusk()));
    // A light.mode alone makes a folder a theme, which its import then refuses for no palette.
    let temp = Temp::new("theme-folder-classes");
    let marker = temp.theme("marker", &[("light.mode", b"")]);
    assert_eq!(classify(&marker).unwrap(), Folder::Theme(marker.clone()));
    // A set lists the subfolders that hold a theme, in name order; the notes folder is none.
    assert_eq!(
        classify(&set()).unwrap(),
        Folder::Set(
            ["dusk", "no-accent", "nord", "omarchy-linen-theme"]
                .map(|name| set().join(name))
                .to_vec()
        )
    );
    // Neither: the themes fixture folder holds a Luxforge document and a set, but no theme of its
    // own and no subfolder that holds one.
    let error = classify(&fixture("fixtures/themes")).unwrap_err();
    assert_eq!(
        error,
        "unsupported-input: the folder themes is not an Omarchy theme: neither it nor any folder \
         in it holds colors.toml, alacritty.toml or light.mode"
    );
    assert!(
        classify(&temp.0.join("missing"))
            .unwrap_err()
            .starts_with("read-error: cannot read the folder missing")
    );
}

#[test]
fn a_set_is_at_most_256_subfolders() {
    let temp = Temp::new("theme-folder-bound");
    temp.theme("a-theme", &[("colors.toml", b"accent = \"#ffffff\"")]);
    for index in 1..MAX_SET {
        std::fs::create_dir(temp.0.join(format!("folder-{index:03}"))).unwrap();
    }
    // A file beside them is not a folder, so it does not count.
    std::fs::write(temp.0.join("notes.txt"), "notes").unwrap();
    assert_eq!(
        classify(&temp.0).unwrap(),
        Folder::Set(vec![temp.0.join("a-theme")]),
        "256 subfolders are a set"
    );
    std::fs::create_dir(temp.0.join("folder-256")).unwrap();
    let name = temp.0.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        classify(&temp.0).unwrap_err(),
        format!(
            "resource-limit: the folder {name} holds more than 256 folders; a set of Omarchy \
             themes is at most 256"
        )
    );
}

#[test]
fn only_the_readers_files_are_read_each_within_64_kib() {
    let temp = Temp::new("theme-folder-files");
    let largest = padded_colours(MAX_THEME_FILE_BYTES);
    let at_bound = temp.theme(
        "at-bound",
        &[
            ("colors.toml", &largest),
            // Beside a colors.toml the reader takes no alacritty.toml, so it is not read even
            // past the bound; nor is anything else a theme ships.
            ("alacritty.toml", &vec![b'#'; MAX_THEME_FILE_BYTES * 2]),
            ("hyprland.conf", b"never read"),
            ("light.mode", b""),
        ],
    );
    let files = read_files(&at_bound).unwrap();
    assert_eq!(
        files.keys().collect::<Vec<_>>(),
        ["colors.toml", "light.mode"]
    );
    assert_eq!(files["colors.toml"].len(), MAX_THEME_FILE_BYTES);

    let past = temp.theme(
        "past-bound",
        &[("colors.toml", &padded_colours(MAX_THEME_FILE_BYTES + 1))],
    );
    assert_eq!(
        read_files(&past).unwrap_err(),
        "resource-limit: colors.toml is 65537 bytes; a theme file is at most 64 KiB (65536 bytes)"
    );
    // An alacritty.toml is read where there is no colors.toml.
    let terminal = temp.theme("terminal", &[("alacritty.toml", b"[colors]")]);
    assert_eq!(
        read_files(&terminal).unwrap().keys().collect::<Vec<_>>(),
        ["alacritty.toml"]
    );
    let binary = temp.theme("binary", &[("colors.toml", &[0xff, 0xfe, 0x00])]);
    assert_eq!(
        read_files(&binary).unwrap_err(),
        "unsupported-input: colors.toml is not UTF-8 text"
    );
}

/// The request is the one an independent JSON client sends for the same folder, and the owner
/// resolves the desktop's import as it resolves that client's.
#[test]
fn each_theme_is_the_import_an_independent_client_sends() {
    let host = Host::start("theme-folder-parity");
    let files = read_files(&dusk()).unwrap();
    let colours = std::fs::read_to_string(dusk().join("colors.toml")).unwrap();
    assert_eq!(
        without_request_id(import_params(&files, "dusk")),
        json!({"format": "omarchy", "files": {"colors.toml": colours}, "folder": "dusk",
               "mutation": {"actor": "desktop"}})
    );
    let desktop = host.owner.register();
    let imported = import_folder_now(&host.owner, desktop, &dusk()).unwrap();
    let FolderOutcome::Imported { id, name } = &imported.report.themes[0].outcome else {
        panic!("{:?}", imported.report);
    };
    assert_eq!(name, "Dusk");
    let independent = call(
        &host.owner,
        host.agent,
        "theme.inspect",
        json!({"format": "omarchy", "files": {"colors.toml": colours}, "folder": "dusk"}),
    )
    .unwrap()
    .0;
    let read = call(
        &host.owner,
        host.agent,
        "theme.read",
        json!({"theme_id": id}),
    )
    .unwrap()
    .0;
    assert_eq!(read["theme"]["resolved"], independent["theme"]["resolved"]);
    assert_eq!(read["theme"]["origin"]["folder"], "dusk");
    assert_eq!(imported.answers[0]["report"], independent["report"]);
    host.stop();
}

/// A set imports each theme on its own: named after its folder, each with its own report; the
/// theme named like a bundled one is listed as already built in and the one with no accent fails
/// with the reader's reason, and neither stops the rest. Importing a folder again lists its
/// theme as already imported.
#[test]
fn a_set_imports_each_theme_with_its_own_report() {
    let host = Host::start("theme-folder-set");
    let desktop = host.owner.register();
    let imported = import_folder_now(&host.owner, desktop, &set()).unwrap();
    assert_eq!(imported.report.chosen, "omarchy");
    assert_eq!(
        outcomes(&imported.report.themes),
        [
            ("dusk", "imported"),
            ("no-accent", "failed"),
            ("nord", "built-in"),
            ("omarchy-linen-theme", "imported")
        ]
    );
    let names: Vec<_> = imported
        .report
        .themes
        .iter()
        .filter_map(|theme| match &theme.outcome {
            FolderOutcome::Imported { name, .. } | FolderOutcome::Conflict { name, .. } => {
                Some(name.as_str())
            }
            FolderOutcome::Failed(_) => None,
        })
        .collect();
    assert_eq!(names, ["Dusk", "Nord", "Linen"]);
    assert_eq!(
        imported.report.themes[1].outcome,
        FolderOutcome::Failed("unsupported-input: colors.toml has no accent".into())
    );
    // Each imported theme has its own report, which is the one the host keeps for it.
    for answer in imported
        .answers
        .iter()
        .filter(|answer| answer["outcome"] == "imported")
    {
        let id = answer["theme"]["id"].as_str().unwrap();
        let read = call(
            &host.owner,
            host.agent,
            "theme.read",
            json!({"theme_id": id}),
        )
        .unwrap()
        .0;
        assert_eq!(answer["report"], read["theme"]["report"], "{id}");
    }
    assert_eq!(imported.answers[0]["report"]["omarchy"]["form"], "omarchy4");
    assert_eq!(
        imported.answers[3]["report"]["omarchy"]["mode_source"],
        "light-mode-file"
    );
    assert_eq!(imported.requests.len(), 2);
    let (list, _) = imported.list.unwrap();
    let modes: Vec<_> = list
        .themes
        .iter()
        .filter(|theme| !theme.built_in)
        .map(|theme| (theme.name.as_str(), theme.mode))
        .collect();
    assert_eq!(
        modes,
        [
            ("Dusk", luxforge_core::theme::Mode::Dark),
            ("Linen", luxforge_core::theme::Mode::Light)
        ]
    );
    assert_eq!(
        imported.report.summary(),
        "Imported 2 of 4 Omarchy themes from \u{201c}omarchy\u{201d}: 1 already built in, 1 failed"
    );

    let again = import_folder_now(&host.owner, desktop, &dusk()).unwrap();
    assert_eq!(
        again.report.themes[0].outcome,
        FolderOutcome::Conflict {
            name: "Dusk".into(),
            built_in: false
        }
    );
    assert_eq!(
        again.report.summary(),
        "\u{201c}Dusk\u{201d} is already imported, so it was not imported"
    );
    assert!(again.requests.is_empty());
    host.stop();
}

/// A file that cannot be read fails its own theme with the reason; the rest of the set is
/// imported.
#[test]
fn a_file_that_cannot_be_read_fails_its_theme_not_the_set() {
    let host = Host::start("theme-folder-unreadable");
    let desktop = host.owner.register();
    let temp = Temp::new("theme-folder-unreadable-set");
    let colours = std::fs::read(dusk().join("colors.toml")).unwrap();
    temp.theme("evening", &[("colors.toml", &colours)]);
    temp.theme(
        "oversized",
        &[("colors.toml", &padded_colours(MAX_THEME_FILE_BYTES + 1))],
    );
    let imported = import_folder_now(&host.owner, desktop, &temp.0).unwrap();
    assert_eq!(
        outcomes(&imported.report.themes),
        [("evening", "imported"), ("oversized", "failed")]
    );
    assert_eq!(
        imported.report.themes[1].outcome,
        FolderOutcome::Failed(
            "resource-limit: colors.toml is 65537 bytes; a theme file is at most 64 KiB (65536 \
             bytes)"
                .into()
        )
    );
    assert!(imported.answers[0]["report"].is_object());
    assert_eq!(
        imported.answers[1],
        json!({"folder": "oversized", "outcome": "failed",
               "reason": "resource-limit: colors.toml is 65537 bytes; a theme file is at most \
                          64 KiB (65536 bytes)"})
    );
    // A folder that is no theme at all is refused as a whole, naming it.
    assert!(
        import_folder_now(
            &host.owner,
            desktop,
            &temp.0.join("evening").join("missing")
        )
        .unwrap_err()
        .starts_with("read-error: cannot read the folder missing")
    );
    host.stop();
}

#[test]
fn the_picker_opens_at_omarchys_theme_folders_in_order() {
    assert_eq!(
        omarchy_folders(Some(Path::new("/home/a"))),
        [
            PathBuf::from("/home/a/.config/omarchy/themes"),
            PathBuf::from("/usr/share/omarchy/themes"),
            PathBuf::from("/home/a/.local/share/omarchy/themes"),
        ]
    );
    assert_eq!(
        omarchy_folders(None),
        [PathBuf::from("/usr/share/omarchy/themes")]
    );
}

/// The dialog's answer starts the import task; its answer lists every theme's outcome in the tab,
/// says it in a line in the status bar whose Copy copies every answer, adopts the library after it
/// and hands the stored themes' requests to the event sync.
#[test]
fn the_tab_and_the_status_bar_show_what_each_theme_became() {
    let mut host = Host::start("theme-folder-tab");
    let mut editor = host.launch();
    answer_list(&mut editor);
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Appearance,
    )));
    let _ = editor.update(Message::Theme(ThemeMessage::OmarchyPicked(Some(set()))));
    assert!(editor.themes.pending, "the import task was started");
    assert!(
        !editor.workspace.settings.appearance.can_import,
        "one import at a time"
    );
    assert_eq!(editor.status.text, "Importing Omarchy themes\u{2026}");
    let imported = import_folder_now(&editor.owner, editor.client, &set()).map(Box::new);
    let _ = editor.update(Message::Theme(ThemeMessage::OmarchyImported(imported)));
    assert!(!editor.themes.pending);
    let summary =
        "Imported 2 of 4 Omarchy themes from \u{201c}omarchy\u{201d}: 1 already built in, 1 failed";
    assert_eq!(editor.status.text, summary);
    let copied: Value = serde_json::from_str(&editor.status.copy.as_ref().unwrap().1).unwrap();
    assert_eq!(copied.as_array().unwrap().len(), 4);
    let (shown, lines) = editor
        .workspace
        .settings
        .appearance
        .folder
        .clone()
        .expect("the tab shows the import");
    assert_eq!(shown, summary);
    let lines: Vec<_> = lines
        .iter()
        .map(|line| (line.text.as_str(), line.failed))
        .collect();
    assert_eq!(
        lines,
        [
            ("Imported: Dusk, Linen", false),
            ("Already built in: Nord", false),
            (
                "no-accent: unsupported-input: colors.toml has no accent",
                true
            ),
        ]
    );
    let rows: Vec<_> = editor
        .workspace
        .settings
        .appearance
        .rows
        .iter()
        .map(|row| row.origin.as_str())
        .filter(|origin| origin.starts_with("Omarchy"))
        .collect();
    assert_eq!(
        rows,
        ["Omarchy \u{b7} dusk", "Omarchy \u{b7} omarchy-linen-theme"]
    );
    assert_eq!(editor.sync.own_requests.len(), 2);
    let frame = editor.theme_summary();
    assert_eq!(frame["folder_import"]["summary"], summary);
    assert_eq!(
        frame["folder_import"]["themes"][2],
        json!({"folder": "nord", "outcome": "built-in", "name": "Nord", "id": null,
               "reason": null})
    );

    // A folder refused as a whole is the tab's refusal, and the next import clears the account.
    let _ = editor.update(Message::Theme(ThemeMessage::OmarchyPicked(Some(fixture(
        "fixtures/themes",
    )))));
    assert!(editor.workspace.settings.appearance.folder.is_none());
    let refused =
        import_folder_now(&editor.owner, editor.client, &fixture("fixtures/themes")).map(Box::new);
    let _ = editor.update(Message::Theme(ThemeMessage::OmarchyImported(refused)));
    assert!(
        editor
            .workspace
            .settings
            .appearance
            .refusal
            .as_deref()
            .is_some_and(|refusal| refusal.starts_with("unsupported-input: the folder themes"))
    );
    finish(editor, host);
}

/// The `theme_import_omarchy` step drives the import task and records what each theme became with
/// its report; the `agent` step sends `preferences.set` through a second client and is captured
/// once the event sync has followed it to the theme drawn.
#[test]
fn the_steps_import_a_folder_and_a_second_client_chooses_a_theme() {
    let mut host = Host::start("theme-folder-steps");
    let mut editor = host.launch();
    answer_list(&mut editor);
    super::testing::attach_script(
        &mut editor,
        &format!(
            r#"[{{"theme_import_omarchy":{{"path":{path}}}}},
                {{"agent":{{"method":"preferences.set","params":{{"theme":"omarchy.nord"}}}}}}]"#,
            path = json!(set()),
        ),
    );
    let evidence = |editor: &Editor| {
        let evidence = editor.evidence.as_ref().expect("evidence");
        (evidence.awaiting, evidence.capture_pending)
    };
    let next = |editor: &mut Editor| {
        editor.evidence.as_mut().unwrap().capture_pending = false;
        editor.next_step()
    };

    let _ = next(&mut editor);
    assert_eq!(
        evidence(&editor),
        (Some(super::evidence::Settle::Themes), false)
    );
    let imported = import_folder_now(&editor.owner, editor.client, &set()).map(Box::new);
    let _ = editor.update(Message::Theme(ThemeMessage::OmarchyImported(imported)));
    assert!(evidence(&editor).1, "captured once every theme answered");
    let record = editor.evidence.as_ref().unwrap().current.clone().unwrap();
    assert_eq!(
        record["status"], "sent",
        "a conflict or a failure is no failed step"
    );
    let themes = record["folder_import"]["themes"].as_array().unwrap();
    assert_eq!(themes.len(), 4);
    assert!(themes[0]["report"].is_object() && themes[2]["outcome"] == "built-in");

    // The second client's change reaches the desktop only through the event sync.
    let _ = next(&mut editor);
    assert_eq!(
        evidence(&editor),
        (Some(super::evidence::Settle::AgentHost), false)
    );
    let agent = editor.evidence.as_ref().unwrap().agent.unwrap();
    let answered = call(
        &editor.owner,
        agent,
        "preferences.set",
        json!({"theme": "omarchy.nord"}),
    );
    let _ = editor.update(Message::Evidence(EvidenceMessage::AgentHostAnswered(
        answered,
    )));
    assert!(!evidence(&editor).1, "waits for the sync to follow");
    let _ = editor.update(Message::Sync(SyncMessage::Changed));
    let polled = tasks::sync_now(
        &editor.owner,
        editor.client,
        None,
        editor.sync.sequence,
        &[],
        None,
    );
    let _ = editor.update(Message::Sync(SyncMessage::Synced(polled)));
    assert!(!evidence(&editor).1, "waits for the preferences");
    let read = call(&editor.owner, editor.client, "preferences.read", json!({}))
        .and_then(|(answer, _)| crate::state::preferences::parse(answer));
    let _ = editor.update(Message::Preferences(PreferenceMessage::Read(read)));
    if editor.themes.listing.in_flight() {
        answer_list(&mut editor);
    }
    assert!(!evidence(&editor).1, "waits for the theme to be drawn");
    answer_read(&mut editor);
    assert!(evidence(&editor).1, "captured once the theme is drawn");
    assert_eq!(editor.themes.drawn.id, "omarchy.nord");
    let record = editor.evidence.as_ref().unwrap().current.clone().unwrap();
    assert_eq!(record["actor"], "evidence-agent");
    finish(editor, host);
}
