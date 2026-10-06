//! The theme library as this desktop holds it, and the Settings sheet's Appearance tab drawn from
//! it ([design](../../../../docs/design/ui-themes.md#appearance-tab)).
//!
//! The library is the host's, read through `theme.list` as any client reads it: once at launch,
//! whenever the Settings sheet opens, after each of this desktop's imports and deletes, and on
//! another client's `theme.*` event. It is held whether or not the sheet is open, because the
//! palette lists one entry per theme. Which theme is active is a preference, held by the desktop's
//! one preference writer; what is on screen is [`DrawnTheme`], the identity of the theme the
//! desktop last built. The theme value Iced draws with lives in the update layer, not here.
use super::{MenuTarget, palette::PaletteAction, preferences::PreferenceWriter};
use crate::coalesce::Coalesce;
use luxforge_core::theme::{LUXFORGE_DARK_ID, LUXFORGE_DARK_NAME, Mode, Rgba, ThemeOrigin};
use serde::Deserialize;
use serde_json::Value;

/// A theme's five swatches, as `theme.list` answers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct Swatches {
    pub(crate) surround: Rgba,
    pub(crate) background: Rgba,
    pub(crate) surface: Rgba,
    pub(crate) text: Rgba,
    pub(crate) accent: Rgba,
}

/// One theme as `theme.list` lists it. Fields the desktop does not draw, such as the report's
/// counts, are not read.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(crate) struct ListedTheme {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) mode: Mode,
    pub(crate) origin: ThemeOrigin,
    pub(crate) built_in: bool,
    pub(crate) swatches: Swatches,
    /// Resolving moved one of the theme's own inks to its floor.
    pub(crate) adjusted: bool,
}

/// A stored theme this build cannot read, kept by the host and listed with its reason.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct UnrecognizedTheme {
    pub(crate) id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) reason: String,
}

/// The library as one `theme.list` answered it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(crate) struct ThemeList {
    pub(crate) themes: Vec<ListedTheme>,
    /// The active theme's id when the list was read.
    pub(crate) active: String,
    pub(crate) unrecognized: Vec<UnrecognizedTheme>,
}

impl ThemeList {
    /// The listed theme `id` names.
    pub(crate) fn find(&self, id: &str) -> Option<&ListedTheme> {
        self.themes.iter().find(|theme| theme.id == id)
    }
}

/// A `theme.list` answer as the library it lists.
pub(crate) fn parse_list(answer: Value) -> Result<ThemeList, String> {
    serde_json::from_value(answer).map_err(|error| format!("unreadable theme list: {error}"))
}

/// The theme on screen, as plain data: the theme the preferences chose when it was built, and the
/// theme drawn for it, which is Luxforge Dark when the chosen one cannot be shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DrawnTheme {
    /// The id the preferences chose: what the drawing answers for, so a choice that fell back is
    /// not read again until another is made.
    pub(crate) chosen: String,
    /// The id of the theme drawn.
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) mode: Mode,
    /// Why the chosen theme is not drawn, naming it.
    pub(crate) problem: Option<String>,
}

impl Default for DrawnTheme {
    fn default() -> Self {
        Self {
            chosen: LUXFORGE_DARK_ID.to_owned(),
            id: LUXFORGE_DARK_ID.to_owned(),
            name: LUXFORGE_DARK_NAME.to_owned(),
            mode: Mode::Dark,
            problem: None,
        }
    }
}

/// What the status bar says when the theme `chosen` cannot be shown, in the core's own words for
/// the launch read's problem, so a launch and a later change say the same.
pub(crate) fn fallback_note(chosen: &str, reason: &str) -> String {
    format!("The theme {chosen} cannot be shown, so {LUXFORGE_DARK_NAME} is: {reason}")
}

/// What became of one theme of an Omarchy folder import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FolderOutcome {
    /// Stored under this id and name.
    Imported { id: String, name: String },
    /// A theme the library already holds has this name: one built in, or one imported before. It
    /// is never renamed or replaced.
    Conflict { name: String, built_in: bool },
    /// Not imported, for this reason: a file the desktop could not read, or the host's refusal.
    Failed(String),
}

impl FolderOutcome {
    /// The outcome's name as a frame's state records it.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Imported { .. } => "imported",
            Self::Conflict { built_in: true, .. } => "built-in",
            Self::Conflict {
                built_in: false, ..
            } => "already-imported",
            Self::Failed(_) => "failed",
        }
    }
}

/// One theme folder of an import and what became of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderTheme {
    /// The theme folder's own name, which names the theme.
    pub(crate) folder: String,
    pub(crate) outcome: FolderOutcome,
}

/// One Import Omarchy theme…: the folder chosen and each theme it held, in folder-name order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderReport {
    /// The chosen folder's own name.
    pub(crate) chosen: String,
    pub(crate) themes: Vec<FolderTheme>,
}

impl FolderReport {
    /// The one line the status bar and the tab say of the import.
    pub(crate) fn summary(&self) -> String {
        if let [theme] = self.themes.as_slice() {
            return match &theme.outcome {
                FolderOutcome::Imported { name, .. } => {
                    format!("Imported the Omarchy theme \u{201c}{name}\u{201d}")
                }
                FolderOutcome::Conflict { name, built_in } => format!(
                    "\u{201c}{name}\u{201d} is {}, so it was not imported",
                    already(*built_in)
                ),
                FolderOutcome::Failed(reason) => format!(
                    "Could not import the Omarchy theme {}: {reason}",
                    theme.folder
                ),
            };
        }
        let count = |kind: &str| {
            self.themes
                .iter()
                .filter(|theme| theme.outcome.kind() == kind)
                .count()
        };
        let mut summary = format!(
            "Imported {} of {} Omarchy themes from \u{201c}{}\u{201d}",
            count("imported"),
            self.themes.len(),
            self.chosen
        );
        let rest: Vec<String> = [
            ("built-in", "already built in"),
            ("already-imported", "already imported"),
            ("failed", "failed"),
        ]
        .into_iter()
        .filter_map(|(kind, label)| {
            let n = count(kind);
            (n > 0).then(|| format!("{n} {label}"))
        })
        .collect();
        if !rest.is_empty() {
            summary.push_str(": ");
            summary.push_str(&rest.join(", "));
        }
        summary
    }

    /// The tab's lines under the summary: the imported themes, those already built in and those
    /// already imported, each group named once, then one line per failure with its reason. A
    /// folder of one theme has none, since its summary says it all.
    pub(crate) fn lines(&self) -> Vec<FolderLine> {
        if self.themes.len() == 1 {
            return Vec::new();
        }
        let names = |built_in: Option<bool>| -> Vec<&str> {
            self.themes
                .iter()
                .filter_map(|theme| match (&theme.outcome, built_in) {
                    (FolderOutcome::Imported { name, .. }, None) => Some(name.as_str()),
                    (FolderOutcome::Conflict { name, built_in }, Some(wanted))
                        if *built_in == wanted =>
                    {
                        Some(name.as_str())
                    }
                    _ => None,
                })
                .collect()
        };
        let mut lines: Vec<FolderLine> = [
            ("Imported", names(None)),
            ("Already built in", names(Some(true))),
            ("Already imported", names(Some(false))),
        ]
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(label, names)| FolderLine {
            text: format!("{label}: {}", names.join(", ")),
            failed: false,
        })
        .collect();
        lines.extend(self.themes.iter().filter_map(|theme| match &theme.outcome {
            FolderOutcome::Failed(reason) => Some(FolderLine {
                text: format!("{}: {reason}", theme.folder),
                failed: true,
            }),
            _ => None,
        }));
        lines
    }
}

fn already(built_in: bool) -> &'static str {
    if built_in {
        "already built in"
    } else {
        "already imported"
    }
}

/// One line the Appearance tab shows of the last folder import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderLine {
    pub(crate) text: String,
    /// A theme that failed, drawn as an error.
    pub(crate) failed: bool,
}

/// The library and the theme on screen.
#[derive(Clone, Debug, Default)]
pub(crate) struct Themes {
    /// The library as the last `theme.list` answered it, or `None` until one has.
    pub(crate) list: Option<ThemeList>,
    /// Why the last `theme.list` failed, until the next one succeeds.
    pub(crate) error: Option<String>,
    /// The event sequence the held listing was read at, so a listing read earlier never replaces
    /// one read later.
    pub(crate) sequence: u64,
    /// One `theme.list` in flight, and one more wanted behind it.
    pub(crate) listing: Coalesce<()>,
    /// The theme on screen.
    pub(crate) drawn: DrawnTheme,
    /// The theme whose `theme.read` is in flight, to be drawn when it answers.
    pub(crate) reading: Option<String>,
    /// An import or a delete is in flight; they go one at a time.
    pub(crate) pending: bool,
    /// Why the last import or delete was refused, shown in the tab until the next one is made.
    pub(crate) refusal: Option<String>,
    /// What the last Import Omarchy theme… made of each theme it found, shown in the tab until the
    /// next import or delete.
    pub(crate) folder: Option<FolderReport>,
}

/// One theme as the Appearance tab draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThemeRow {
    pub(crate) id: String,
    pub(crate) name: String,
    /// "Dark" or "Light".
    pub(crate) mode: &'static str,
    /// "Built-in", "Omarchy · <folder>" or "Imported file".
    pub(crate) origin: String,
    /// Surround, background, surface, text and accent.
    pub(crate) swatches: [[u8; 3]; 5],
    pub(crate) adjusted: bool,
    /// The theme the preferences choose, with a change not yet answered laid over them.
    pub(crate) active: bool,
    /// A stored theme: its menu offers Delete. A built-in theme's does not.
    pub(crate) deletable: bool,
    /// Its row menu is open.
    pub(crate) menu_open: bool,
}

/// The Appearance tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AppearanceModel {
    pub(crate) rows: Vec<ThemeRow>,
    /// Stored themes this build cannot read, each named with its reason.
    pub(crate) unrecognized: Vec<String>,
    /// The library has not been read yet.
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) refusal: Option<String>,
    /// The last Omarchy folder import: its summary, then its lines.
    pub(crate) folder: Option<(String, Vec<FolderLine>)>,
    /// Import Omarchy theme… and Import theme file… can start: nothing else is in flight and no
    /// dialog is open.
    pub(crate) can_import: bool,
}

/// What the Appearance tab's rows are for these inputs: Luxforge Dark first, then the other
/// built-in themes, then the imported ones by name, as `theme.list` orders them.
pub(crate) fn appearance(
    themes: &Themes,
    preferences: &PreferenceWriter,
    menu: Option<&MenuTarget>,
    picker_open: bool,
) -> AppearanceModel {
    let active = active_theme(themes, preferences);
    let rows = themes
        .list
        .iter()
        .flat_map(|list| &list.themes)
        .map(|theme| ThemeRow {
            id: theme.id.clone(),
            name: theme.name.clone(),
            mode: mode_label(theme.mode),
            origin: origin_label(theme),
            swatches: {
                let Swatches {
                    surround,
                    background,
                    surface,
                    text,
                    accent,
                } = theme.swatches;
                [surround, background, surface, text, accent].map(Rgba::channels)
            },
            adjusted: theme.adjusted,
            active: active == Some(theme.id.as_str()),
            deletable: !theme.built_in,
            menu_open: matches!(menu, Some(MenuTarget::Theme(id)) if *id == theme.id),
        })
        .collect();
    let unrecognized = themes
        .list
        .iter()
        .flat_map(|list| &list.unrecognized)
        .map(|record| {
            let named = match (&record.name, &record.id) {
                (Some(name), Some(id)) => format!("{name} ({id})"),
                (Some(name), None) => name.clone(),
                (None, Some(id)) => id.clone(),
                (None, None) => "A stored theme".to_owned(),
            };
            format!("{named}: {}", record.reason)
        })
        .collect();
    AppearanceModel {
        rows,
        unrecognized,
        loading: themes.list.is_none() && themes.error.is_none(),
        error: themes.error.clone(),
        refusal: themes.refusal.clone(),
        folder: themes
            .folder
            .as_ref()
            .map(|report| (report.summary(), report.lines())),
        can_import: !themes.pending && !picker_open,
    }
}

/// The theme the preferences choose, with a change not yet answered laid over them; the library's
/// own answer while the preferences have not been read.
pub(crate) fn active_theme<'a>(
    themes: &'a Themes,
    preferences: &'a PreferenceWriter,
) -> Option<&'a str> {
    preferences
        .applied_theme()
        .or_else(|| themes.list.as_ref().map(|list| list.active.as_str()))
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Dark => "Dark",
        Mode::Light => "Light",
    }
}

/// Where a row's theme came from. A bundled Omarchy theme is built in like Luxforge Dark: it ships
/// with Luxforge, is never stored and cannot be deleted, so the folder it was read from is the
/// build's business rather than the person's.
fn origin_label(theme: &ListedTheme) -> String {
    if theme.built_in {
        return "Built-in".to_owned();
    }
    match &theme.origin {
        ThemeOrigin::Omarchy { folder, .. } => format!("Omarchy \u{b7} {folder}"),
        ThemeOrigin::BuiltIn {} => "Built-in".to_owned(),
        ThemeOrigin::Luxforge {} => "Imported file".to_owned(),
    }
}

/// The palette's theme entries: one per listed theme, which chooses it as its row does.
pub(crate) fn palette_entries(themes: &Themes) -> Vec<(String, String, PaletteAction)> {
    themes
        .list
        .iter()
        .flat_map(|list| &list.themes)
        .map(|theme| {
            (
                format!("Theme: {}", theme.name),
                "preferences.set".to_owned(),
                PaletteAction::Theme(theme.id.clone()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::preferences::{PreferenceChange, parse};
    use serde_json::json;

    /// A listing with Luxforge Dark, a bundled Omarchy theme, an imported light theme and one
    /// imported from a folder, and a record this build cannot read.
    pub(crate) fn listed() -> ThemeList {
        let swatches = |surround: &str| {
            json!({"surround": surround, "background": "#202023", "surface": "#232326",
                   "text": "#e8e8ea", "accent": "#e2b46a"})
        };
        parse_list(json!({
            "themes": [
                {"id": "luxforge.dark", "name": "Luxforge Dark", "mode": "dark",
                 "origin": {"kind": "built-in"}, "built_in": true, "swatches": swatches("#19191b"),
                 "report": {"moved": 0}, "adjusted": false},
                {"id": "omarchy.nord", "name": "Nord", "mode": "dark",
                 "origin": {"kind": "omarchy", "folder": "nord", "form": "omarchy-4",
                            "commit": "035ce29f03"},
                 "built_in": true, "swatches": swatches("#24272d"), "adjusted": true},
                {"id": "theme-a", "name": "Paper", "mode": "light", "origin": {"kind": "luxforge"},
                 "built_in": false, "swatches": swatches("#e6e4df"), "adjusted": false},
                {"id": "theme-b", "name": "Retro 82", "mode": "dark",
                 "origin": {"kind": "omarchy", "folder": "retro-82", "form": "omarchy-4"},
                 "built_in": false, "swatches": swatches("#0e1215"), "adjusted": true}
            ],
            "active": "luxforge.dark",
            "unrecognized": [{"id": "theme-c", "name": "Old", "reason": "unknown field `x`"},
                             {"id": null, "name": null, "reason": "not an object"}]
        }))
        .unwrap()
    }

    fn preferences(theme: &str) -> PreferenceWriter {
        PreferenceWriter::new(parse(json!({
            "performance_expanded": true, "auto_collapse_history": true,
            "auto_lens_profile": true, "raw_look": "standard", "mask_overlay_colour": "green",
            "canvas_background": "theme", "interface_size": 100, "catalog": null,
            "workspace": {"state_panel": true, "tools_panel": true, "thirds": false,
                          "clip_shadows": false, "clip_highlights": false},
            "brush": null, "window": null, "export_folder": null, "theme": theme
        })))
    }

    fn themes() -> Themes {
        Themes {
            list: Some(listed()),
            ..Themes::default()
        }
    }

    #[test]
    fn rows_list_every_theme_with_its_swatches_mode_origin_badge_and_check() {
        let model = appearance(&themes(), &preferences("luxforge.dark"), None, false);
        let summary: Vec<_> = model
            .rows
            .iter()
            .map(|row| {
                (
                    row.name.as_str(),
                    row.mode,
                    row.origin.as_str(),
                    row.adjusted,
                    row.active,
                    row.deletable,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("Luxforge Dark", "Dark", "Built-in", false, true, false),
                ("Nord", "Dark", "Built-in", true, false, false),
                ("Paper", "Light", "Imported file", false, false, true),
                (
                    "Retro 82",
                    "Dark",
                    "Omarchy \u{b7} retro-82",
                    true,
                    false,
                    true
                ),
            ]
        );
        assert_eq!(
            model.rows[0].swatches,
            [
                [0x19, 0x19, 0x1b],
                [0x20, 0x20, 0x23],
                [0x23, 0x23, 0x26],
                [0xe8, 0xe8, 0xea],
                [0xe2, 0xb4, 0x6a]
            ]
        );
        assert_eq!(
            model.unrecognized,
            [
                "Old (theme-c): unknown field `x`",
                "A stored theme: not an object"
            ]
        );
        assert!(!model.loading && model.can_import);
    }

    #[test]
    fn the_check_follows_a_choice_at_once_and_the_menu_its_row() {
        let mut writer = preferences("luxforge.dark");
        writer.offer(PreferenceChange {
            theme: Some("theme-a".into()),
            ..PreferenceChange::default()
        });
        let menu = MenuTarget::Theme("theme-b".into());
        let model = appearance(&themes(), &writer, Some(&menu), false);
        let active: Vec<_> = model.rows.iter().filter(|row| row.active).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(
            active[0].id, "theme-a",
            "the choice shows before its answer"
        );
        assert!(model.rows[3].menu_open && !model.rows[2].menu_open);
    }

    #[test]
    fn an_unread_library_is_loading_and_a_busy_one_takes_no_import() {
        let unread = appearance(
            &Themes::default(),
            &preferences("luxforge.dark"),
            None,
            false,
        );
        assert!(unread.loading && unread.rows.is_empty());
        let busy = Themes {
            pending: true,
            ..themes()
        };
        assert!(!appearance(&busy, &preferences("luxforge.dark"), None, false).can_import);
        assert!(!appearance(&themes(), &preferences("luxforge.dark"), None, true).can_import);
        let failed = Themes {
            error: Some("not-ready: no directory".into()),
            ..Themes::default()
        };
        let model = appearance(&failed, &preferences("luxforge.dark"), None, false);
        assert!(!model.loading);
        assert_eq!(model.error.as_deref(), Some("not-ready: no directory"));
    }

    #[test]
    fn the_palette_lists_one_entry_per_theme_that_chooses_it() {
        let entries = palette_entries(&themes());
        let labels: Vec<_> = entries.iter().map(|(label, ..)| label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Theme: Luxforge Dark",
                "Theme: Nord",
                "Theme: Paper",
                "Theme: Retro 82"
            ]
        );
        assert_eq!(entries[2].2, PaletteAction::Theme("theme-a".into()));
        assert_eq!(entries[2].1, "preferences.set");
        assert!(palette_entries(&Themes::default()).is_empty());
    }

    #[test]
    fn a_folder_import_is_said_in_a_line_and_listed_by_outcome() {
        let theme = |folder: &str, outcome| FolderTheme {
            folder: folder.into(),
            outcome,
        };
        let one = |outcome| FolderReport {
            chosen: "nord".into(),
            themes: vec![theme("nord", outcome)],
        };
        let summaries = [
            FolderOutcome::Imported {
                id: "theme-a".into(),
                name: "Nord".into(),
            },
            FolderOutcome::Conflict {
                name: "Nord".into(),
                built_in: true,
            },
            FolderOutcome::Conflict {
                name: "Nord".into(),
                built_in: false,
            },
            FolderOutcome::Failed("unsupported-input: colors.toml has no accent".into()),
        ]
        .map(|outcome| {
            let report = one(outcome);
            assert!(report.lines().is_empty(), "the summary says it all");
            report.summary()
        });
        assert_eq!(
            summaries,
            [
                "Imported the Omarchy theme \u{201c}Nord\u{201d}",
                "\u{201c}Nord\u{201d} is already built in, so it was not imported",
                "\u{201c}Nord\u{201d} is already imported, so it was not imported",
                "Could not import the Omarchy theme nord: unsupported-input: colors.toml has no \
                 accent",
            ]
        );
        let set = FolderReport {
            chosen: "themes".into(),
            themes: vec![
                theme(
                    "a",
                    FolderOutcome::Conflict {
                        name: "A".into(),
                        built_in: false,
                    },
                ),
                theme(
                    "b",
                    FolderOutcome::Imported {
                        id: "theme-b".into(),
                        name: "B".into(),
                    },
                ),
                theme(
                    "c",
                    FolderOutcome::Imported {
                        id: "theme-c".into(),
                        name: "C".into(),
                    },
                ),
            ],
        };
        assert_eq!(
            set.summary(),
            "Imported 2 of 3 Omarchy themes from \u{201c}themes\u{201d}: 1 already imported"
        );
        assert_eq!(
            set.lines(),
            [
                FolderLine {
                    text: "Imported: B, C".into(),
                    failed: false
                },
                FolderLine {
                    text: "Already imported: A".into(),
                    failed: false
                },
            ]
        );
    }

    #[test]
    fn the_fallback_names_the_theme_and_its_reason() {
        assert_eq!(
            fallback_note("theme-x", "unknown theme theme-x"),
            "The theme theme-x cannot be shown, so Luxforge Dark is: unknown theme theme-x"
        );
        assert_eq!(DrawnTheme::default().id, LUXFORGE_DARK_ID);
    }
}
