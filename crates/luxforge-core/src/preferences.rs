//! Small user preferences outside the catalog, shared by the desktop and command clients
//! (`docs/design/preferences.md`). The existing bounded, format-marked and locked JSON writer
//! preserves unsupported documents.
//!
//! Only explicit choices are stored: a preference nobody set has no stored value and follows its
//! default, so a changed default reaches everyone who never chose, and `null` in `preferences.set`
//! removes a stored value. Every stored value is checked by the same rules a write is, so a file
//! holding a value this build cannot use is refused without being rewritten.
use crate::{
    Error, MaskOverlayColour, WorkspaceState,
    capabilities::document::JsonDocument,
    mask::commands::ADD_STROKE,
    theme::{LUXFORGE_DARK_ID, LaunchTheme, MAX_THEME_ID},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 16 * 1024;

/// The interface sizes a person can choose, in percent of the system's own scale.
pub const INTERFACE_SIZES: [u16; 4] = [100, 110, 125, 150];

/// The interface size nobody chose: the system's own scale.
pub const DEFAULT_INTERFACE_SIZE: u16 = 100;

/// The smallest and largest remembered window width or height, in the system's points.
pub const WINDOW_SIZE_RANGE: std::ops::RangeInclusive<f32> = 320.0..=16_384.0;

/// The colour around the photograph on the canvas.
///
/// A name, not a colour, as [`MaskOverlayColour`] is: Dark, Black and Grey resolve to fixed greys
/// in `crates/luxforge-ui/src/theme.rs` whatever the theme, and Theme to the active theme's
/// surround. It changes what the workspace draws, never a rendered or exported byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CanvasBackground {
    /// Luxforge Dark's canvas, `#19191b`, in every theme.
    Dark,
    Black,
    /// An 18% grey, for judging tone the way a print is judged.
    Grey,
    /// The active theme's surround, held neutral; Luxforge Dark's is Dark's `#19191b`.
    #[default]
    Theme,
}

impl CanvasBackground {
    /// Every choice, in the order the General row offers them. The accepted vocabulary is read
    /// from here, so a choice and its spelling cannot drift apart.
    pub const ALL: [Self; 4] = [Self::Dark, Self::Black, Self::Grey, Self::Theme];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Black => "black",
            Self::Grey => "grey",
            Self::Theme => "theme",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|background| background.as_str() == value)
    }
}

/// The look a new RAW photograph's Original starts from: what [`crate::ToolModule::original`] reads
/// when the host builds the Original of a photograph it is bringing in. It changes which layers a
/// photograph created from then on starts with, never a saved recipe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RawLook {
    /// Luxforge's own look over the development.
    #[default]
    Standard,
    /// The bare development, with no look.
    Neutral,
}

impl RawLook {
    /// Every choice, in the order the General row offers them. The accepted vocabulary is read
    /// from here, so a choice and its spelling cannot drift apart.
    pub const ALL: [Self; 2] = [Self::Standard, Self::Neutral];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Neutral => "neutral",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|look| look.as_str() == value)
    }
}

/// The five workspace switches the desktop remembers across launches. The canvas mode, the mask
/// overlay mode, zoom and the GPU preview are not remembered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePreference {
    pub state_panel: bool,
    pub tools_panel: bool,
    pub thirds: bool,
    pub clip_shadows: bool,
    pub clip_highlights: bool,
}

impl Default for WorkspacePreference {
    /// The workspace's own defaults ([`WorkspaceState::default`]).
    fn default() -> Self {
        let workspace = WorkspaceState::default();
        Self {
            state_panel: workspace.state_panel,
            tools_panel: workspace.tools_panel,
            thirds: workspace.thirds,
            clip_shadows: workspace.clip_shadows,
            clip_highlights: workspace.clip_highlights,
        }
    }
}

/// The remembered brush: the three numbers of the Masks panel's brush that outlive a stroke, each
/// within the range `mask.add-stroke` declares for it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushPreference {
    pub size: f64,
    pub feather: f64,
    pub flow: f64,
}

/// The remembered window frame in the system's points, whatever the interface size: a size within
/// [`WINDOW_SIZE_RANGE`] and a finite position.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowFrame {
    pub width: f32,
    pub height: f32,
    pub x: f32,
    pub y: f32,
}

/// The stored preferences: each field holds the person's explicit choice, or nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    /// Whether the Performance section is expanded; it is unless the person collapsed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) performance_expanded: Option<bool>,
    /// Whether an edit that sets the same control as the entry before it collapses that entry in
    /// history ([`crate::EditorService::set_auto_collapse`]). On unless the person turned it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) auto_collapse_history: Option<bool>,
    /// Whether a first preparation commits a new RAW photo's detected lens profile
    /// ([`crate::EditorService::set_auto_lens_profile`]). On unless the person turned it off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) auto_lens_profile: Option<bool>,
    /// The look a new RAW photograph's Original starts from
    /// ([`crate::EditorService::set_raw_look`]). Standard unless the person chose another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) raw_look: Option<RawLook>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mask_overlay_colour: Option<MaskOverlayColour>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) canvas_background: Option<CanvasBackground>,
    /// One of [`INTERFACE_SIZES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) interface_size: Option<u16>,
    /// The catalog file the desktop opens at its next launch; the default catalog when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) catalog: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) workspace: Option<WorkspacePreference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) brush: Option<BrushPreference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<WindowFrame>,
    /// The folder `export.plan` suggests while it exists; the original's folder when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) export_folder: Option<PathBuf>,
    /// The feature flags the person chose, by identity, exactly as stored: a value no flag claims,
    /// or one that does not fit its flag, is kept for [`crate::flags`] to report.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) flags: BTreeMap<String, Value>,
    /// The active theme's id; Luxforge Dark when absent. `preferences.set` stores only an id the
    /// theme library holds, and Luxforge Dark's as absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) theme: Option<String>,
}

/// What the Settings sheet's General rows show, compared before and after a write to decide
/// whether `preferences.set` announces it.
type GeneralRows<'a> = (
    bool,
    bool,
    RawLook,
    MaskOverlayColour,
    CanvasBackground,
    u16,
    Option<&'a Path>,
);

impl Preferences {
    pub(crate) fn performance_expanded(&self) -> bool {
        self.performance_expanded.unwrap_or(true)
    }

    pub(crate) fn auto_collapse_history(&self) -> bool {
        self.auto_collapse_history.unwrap_or(true)
    }

    pub(crate) fn auto_lens_profile(&self) -> bool {
        self.auto_lens_profile.unwrap_or(true)
    }

    pub(crate) fn raw_look(&self) -> RawLook {
        self.raw_look.unwrap_or_default()
    }

    pub(crate) fn mask_overlay_colour(&self) -> MaskOverlayColour {
        self.mask_overlay_colour.unwrap_or_default()
    }

    pub(crate) fn canvas_background(&self) -> CanvasBackground {
        self.canvas_background.unwrap_or_default()
    }

    pub(crate) fn interface_size(&self) -> u16 {
        self.interface_size.unwrap_or(DEFAULT_INTERFACE_SIZE)
    }

    pub(crate) fn workspace(&self) -> WorkspacePreference {
        self.workspace.unwrap_or_default()
    }

    /// The active theme's id, Luxforge Dark's when none is stored.
    pub(crate) fn theme(&self) -> &str {
        self.theme.as_deref().unwrap_or(LUXFORGE_DARK_ID)
    }

    /// The values the General rows show, defaults filled in.
    pub(crate) fn general(&self) -> GeneralRows<'_> {
        (
            self.auto_collapse_history(),
            self.auto_lens_profile(),
            self.raw_look(),
            self.mask_overlay_colour(),
            self.canvas_background(),
            self.interface_size(),
            self.catalog.as_deref(),
        )
    }

    /// Every rule a stored value meets that its type cannot say, refused by the field's name: the
    /// same check for a write and for a file read back.
    fn check(&self) -> Result<(), String> {
        if let Some(size) = self.interface_size
            && !INTERFACE_SIZES.contains(&size)
        {
            return Err(format!(
                "interface_size must be one of {}",
                INTERFACE_SIZES.map(|size| size.to_string()).join(", ")
            ));
        }
        if let Some(brush) = &self.brush {
            for (name, value) in [
                ("size", brush.size),
                ("feather", brush.feather),
                ("flow", brush.flow),
            ] {
                check_brush(name, value)?;
            }
        }
        if let Some(window) = &self.window {
            for (name, value) in [("width", window.width), ("height", window.height)] {
                if !value.is_finite() || !WINDOW_SIZE_RANGE.contains(&value) {
                    return Err(format!(
                        "window.{name} must be a number within {}..={}",
                        WINDOW_SIZE_RANGE.start(),
                        WINDOW_SIZE_RANGE.end()
                    ));
                }
            }
            for (name, value) in [("x", window.x), ("y", window.y)] {
                if !value.is_finite() {
                    return Err(format!("window.{name} must be a finite number"));
                }
            }
        }
        if let Some(theme) = &self.theme
            && (theme.is_empty()
                || theme.len() > MAX_THEME_ID
                || theme.chars().any(char::is_control))
        {
            return Err(format!(
                "theme must be a theme id of 1..={MAX_THEME_ID} printable characters"
            ));
        }
        for (name, path) in [
            ("catalog", &self.catalog),
            ("export_folder", &self.export_folder),
        ] {
            match path {
                Some(path) if path.as_os_str().is_empty() => {
                    return Err(format!("{name} must not be empty"));
                }
                Some(path) if !path.is_absolute() => {
                    return Err(format!("{name} must be an absolute path"));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// One brush number against the range `mask.add-stroke` declares for it, so the remembered brush
/// can never hold a value the stroke command would refuse.
fn check_brush(name: &str, value: f64) -> Result<(), String> {
    let declared = crate::mask::commands::find(ADD_STROKE)
        .and_then(|command| command.action.parameter(name))
        .map(|parameter| &parameter.kind);
    let Some(crate::ParameterKind::Number { min, max }) = declared else {
        return Err(format!(
            "brush.{name} has no range declared by {ADD_STROKE}"
        ));
    };
    if !value.is_finite() || value < *min || value > *max {
        return Err(format!(
            "brush.{name} must be a number within {min}..={max}"
        ));
    }
    Ok(())
}

/// Decode one structured preference from a request, naming the field when it does not fit.
pub(crate) fn decode<T: DeserializeOwned>(name: &str, value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|error| Error::validation(format!("{name}: {error}")))
}

/// The preferences one `preferences.set` changes. For each field, `None` leaves it as it is,
/// `Some(None)` removes its stored value so it follows its default, and `Some(Some(value))` stores
/// `value`.
#[derive(Clone, Debug, Default)]
pub(crate) struct PreferenceChange {
    pub(crate) performance_expanded: Option<Option<bool>>,
    pub(crate) auto_collapse_history: Option<Option<bool>>,
    pub(crate) auto_lens_profile: Option<Option<bool>>,
    pub(crate) raw_look: Option<Option<RawLook>>,
    pub(crate) mask_overlay_colour: Option<Option<MaskOverlayColour>>,
    pub(crate) canvas_background: Option<Option<CanvasBackground>>,
    pub(crate) interface_size: Option<Option<u16>>,
    pub(crate) catalog: Option<Option<PathBuf>>,
    pub(crate) workspace: Option<Option<WorkspacePreference>>,
    pub(crate) brush: Option<Option<BrushPreference>>,
    pub(crate) window: Option<Option<WindowFrame>>,
    pub(crate) export_folder: Option<Option<PathBuf>>,
    pub(crate) theme: Option<Option<String>>,
}

impl PreferenceChange {
    fn apply(self, preferences: &mut Preferences) {
        fn put<T>(stored: &mut Option<T>, change: Option<Option<T>>) {
            if let Some(value) = change {
                *stored = value;
            }
        }
        put(
            &mut preferences.performance_expanded,
            self.performance_expanded,
        );
        put(
            &mut preferences.auto_collapse_history,
            self.auto_collapse_history,
        );
        put(&mut preferences.auto_lens_profile, self.auto_lens_profile);
        put(&mut preferences.raw_look, self.raw_look);
        put(
            &mut preferences.mask_overlay_colour,
            self.mask_overlay_colour,
        );
        put(&mut preferences.canvas_background, self.canvas_background);
        put(&mut preferences.interface_size, self.interface_size);
        put(&mut preferences.catalog, self.catalog);
        put(&mut preferences.workspace, self.workspace);
        put(&mut preferences.brush, self.brush);
        put(&mut preferences.window, self.window);
        put(&mut preferences.export_folder, self.export_folder);
        put(&mut preferences.theme, self.theme);
    }
}

pub(crate) struct PreferenceStore(Option<JsonDocument<Preferences>>);

impl PreferenceStore {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self(dir.map(|dir| {
            JsonDocument::new(dir, "preferences.json", MAX_BYTES, 1).checked(Preferences::check)
        }))
    }

    pub(crate) fn read(&self) -> Result<Preferences, Error> {
        match &self.0 {
            Some(document) => Ok(document.read()?.unwrap_or_default()),
            None => Ok(Preferences::default()),
        }
    }

    /// Store the preferences `change` names, leaving the rest as they were, and answer them all
    /// as they were before the write and as they are after it. A value that breaks a rule is
    /// refused by name and nothing is written.
    pub(crate) fn set(
        &self,
        change: PreferenceChange,
    ) -> Result<(Preferences, Preferences), Error> {
        self.0
            .as_ref()
            .ok_or_else(|| Error::not_ready("no user preference directory is configured"))?
            .transact(|preferences| {
                let before = preferences.clone();
                change.apply(preferences);
                preferences.check().map_err(Error::validation)?;
                Ok((before, preferences.clone()))
            })
    }

    /// Store one flag's value, or remove it for `None`, leaving every other stored flag as it
    /// was. Returns whether the stored value changed; an unchanged one writes nothing.
    pub(crate) fn set_flag(&self, id: &str, value: Option<Value>) -> Result<bool, Error> {
        self.0
            .as_ref()
            .ok_or_else(|| Error::not_ready("no user preference directory is configured"))?
            .transact(|preferences| {
                let before = preferences.flags.get(id).cloned();
                match &value {
                    Some(value) => preferences.flags.insert(id.to_owned(), value.clone()),
                    None => preferences.flags.remove(id),
                };
                Ok(before != value)
            })
    }
}

/// The preferences the desktop needs before its catalog owner starts: which catalog to open, the
/// window's frame, the interface size and the theme it draws from its first frame. Read once at
/// launch, beside [`crate::flags::LaunchFlags::resolve`].
#[derive(Clone, Debug)]
pub struct LaunchPreferences {
    /// The stored catalog file, or `None` for the default catalog. Whether its folder exists is
    /// the launch's to check.
    pub catalog: Option<PathBuf>,
    /// The stored window frame, or `None` for the default size placed by the system.
    pub window: Option<WindowFrame>,
    /// The stored interface size, or [`DEFAULT_INTERFACE_SIZE`].
    pub interface_size: u16,
    /// The active theme, from `themes.json` beside the preferences: Luxforge Dark when none is
    /// chosen, and in place of a chosen one that is missing or unreadable, whose own problem
    /// names it. The stored choice is not changed.
    pub theme: LaunchTheme,
    /// Why the preferences could not be read, when they could not: every field took its default,
    /// and nothing was written.
    pub problem: Option<Error>,
}

impl LaunchPreferences {
    /// Read the launch preferences from `preferences.json` under `preferences_dir`, if any. A file
    /// that cannot be read leaves every field at its default and is kept as [`Self::problem`].
    /// `themes.json` is read only when a theme other than Luxforge Dark is chosen.
    pub fn read(preferences_dir: Option<PathBuf>) -> Self {
        let (preferences, problem) = match PreferenceStore::new(preferences_dir.clone()).read() {
            Ok(preferences) => (preferences, None),
            Err(error) => (Preferences::default(), Some(error)),
        };
        Self {
            theme: LaunchTheme::read(preferences_dir, preferences.theme.as_deref()),
            interface_size: preferences.interface_size(),
            catalog: preferences.catalog,
            window: preferences.window,
            problem,
        }
    }
}

/// The file the default catalog's folder holds, as a chosen catalog folder does.
pub const CATALOG_FILE: &str = "catalog.sqlite";

/// The catalog an ordinary desktop launch opens, from [`LaunchPreferences::catalog`] and the
/// default ([design](../../../docs/design/preferences.md#behaviour)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSelection {
    /// The catalog file selected.
    pub path: PathBuf,
    /// The stored catalog whose folder did not exist, so the default was selected instead. The
    /// stored location is kept for the next launch.
    pub missing: Option<PathBuf>,
}

impl CatalogSelection {
    /// The explicit catalog, then the stored location when its folder exists, then the default. A
    /// stored catalog whose folder is missing, as with an unplugged drive, selects the default and
    /// is kept as [`Self::missing`]; a folder without a catalog is still selected, since the
    /// desktop creates one there. `None` when nothing names a catalog and there is no default.
    /// Selecting creates nothing.
    pub fn select(
        explicit: Option<PathBuf>,
        default: Option<PathBuf>,
        stored: Option<PathBuf>,
    ) -> Option<Self> {
        if let Some(path) = explicit {
            return Some(Self {
                path,
                missing: None,
            });
        }
        let (path, missing) = match stored {
            Some(stored) if stored.parent().is_some_and(Path::is_dir) => (stored, None),
            Some(stored) => (default?, Some(stored)),
            None => (default?, None),
        };
        Some(Self { path, missing })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use luxforge_testbase::paths::temp_path;

    fn every_field() -> PreferenceChange {
        PreferenceChange {
            performance_expanded: Some(Some(false)),
            auto_collapse_history: Some(Some(false)),
            auto_lens_profile: Some(Some(false)),
            raw_look: Some(Some(RawLook::Neutral)),
            mask_overlay_colour: Some(Some(MaskOverlayColour::White)),
            canvas_background: Some(Some(CanvasBackground::Grey)),
            interface_size: Some(Some(125)),
            catalog: Some(Some(PathBuf::from("/Volumes/Photos/catalog.sqlite"))),
            workspace: Some(Some(WorkspacePreference {
                state_panel: false,
                tools_panel: true,
                thirds: true,
                clip_shadows: true,
                clip_highlights: false,
            })),
            brush: Some(Some(BrushPreference {
                size: 0.05,
                feather: 20.0,
                flow: 75.0,
            })),
            window: Some(Some(WindowFrame {
                width: 1280.0,
                height: 800.0,
                x: -40.5,
                y: 25.0,
            })),
            export_folder: Some(Some(PathBuf::from("/Users/someone/Exports"))),
            theme: Some(Some(STORED_THEME.to_owned())),
        }
    }

    /// A stored theme's id; the store checks its shape, and `preferences.set` that the library
    /// holds it.
    const STORED_THEME: &str = "theme-0123456789abcdef";

    #[test]
    fn preferences_survive_reopen_and_refuse_unsupported_data_without_rewriting_it() {
        let root = temp_path("preferences");
        let store = PreferenceStore::new(Some(root.clone()));
        let defaults = store.read().unwrap();
        assert!(defaults.performance_expanded());
        assert!(
            defaults.auto_collapse_history(),
            "history collapses by default"
        );
        assert!(!root.exists(), "reading defaults creates nothing");
        store
            .set(PreferenceChange {
                performance_expanded: Some(Some(false)),
                ..PreferenceChange::default()
            })
            .unwrap();
        let reopened = PreferenceStore::new(Some(root.clone())).read().unwrap();
        assert!(!reopened.performance_expanded());
        assert!(
            reopened.auto_collapse_history(),
            "one preference's write leaves the others as they were"
        );
        assert_eq!(
            reopened.auto_collapse_history, None,
            "a preference nobody chose is not stored"
        );
        store
            .set(PreferenceChange {
                auto_collapse_history: Some(Some(false)),
                ..PreferenceChange::default()
            })
            .unwrap();
        let reopened = PreferenceStore::new(Some(root.clone())).read().unwrap();
        assert!(!reopened.performance_expanded() && !reopened.auto_collapse_history());
        let path = root.join("preferences.json");
        for bytes in [
            b"{\"format\":2,\"performance_expanded\":true}".as_slice(),
            b"{\"format\":1,\"performance_expanded\":false,\"unknown\":1}",
            b"{\"format\":1,\"interface_size\":120}",
            b"{\"format\":1,\"canvas_background\":\"white\"}",
            b"{\"format\":1,\"raw_look\":\"camera\"}",
            b"{\"format\":1,\"catalog\":\"relative/catalog.sqlite\"}",
            b"{\"format\":1,\"brush\":{\"size\":0.1,\"feather\":101,\"flow\":100}}",
            b"{\"format\":1,\"window\":{\"width\":100,\"height\":800,\"x\":0,\"y\":0}}",
            b"{\"format\":1,\"workspace\":{\"thirds\":true}}",
            b"{\"format\":1,\"theme\":\"\"}",
            b"not json",
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(store.read().unwrap_err().kind, ErrorKind::Incompatible);
            assert!(store.set(PreferenceChange::default()).is_err());
            assert!(store.set(every_field()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            let launch = LaunchPreferences::read(Some(root.clone()));
            assert_eq!(
                launch.problem.map(|problem| problem.kind),
                Some(ErrorKind::Incompatible)
            );
            assert_eq!(
                (launch.catalog, launch.window, launch.interface_size),
                (None, None, DEFAULT_INTERFACE_SIZE)
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Today's files, written before the newer preferences existed, still read.
    #[test]
    fn a_file_from_before_the_new_preferences_still_reads() {
        let root = temp_path("preferences-today");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("preferences.json"),
            br#"{"format":1,"performance_expanded":true,"auto_collapse_history":false,"flags":{"developer":true}}"#,
        )
        .unwrap();
        let read = PreferenceStore::new(Some(root.clone())).read().unwrap();
        assert_eq!(read.performance_expanded, Some(true));
        assert!(!read.auto_collapse_history());
        assert!(read.auto_lens_profile());
        assert_eq!(read.raw_look(), RawLook::Standard);
        assert_eq!(read.workspace(), WorkspacePreference::default());
        assert_eq!((read.brush, read.window), (None, None));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn every_preference_round_trips_and_null_resets_it() {
        let root = temp_path("preferences-round-trip");
        let store = PreferenceStore::new(Some(root.clone()));
        let (before, after) = store.set(every_field()).unwrap();
        assert_eq!(before, Preferences::default());
        let reopened = PreferenceStore::new(Some(root.clone())).read().unwrap();
        assert_eq!(reopened, after);
        assert!(!reopened.performance_expanded());
        assert!(!reopened.auto_collapse_history());
        assert!(!reopened.auto_lens_profile());
        assert_eq!(reopened.raw_look(), RawLook::Neutral);
        assert_eq!(reopened.mask_overlay_colour(), MaskOverlayColour::White);
        assert_eq!(reopened.canvas_background(), CanvasBackground::Grey);
        assert_eq!(reopened.interface_size(), 125);
        assert_eq!(
            reopened.catalog.as_deref(),
            Some(Path::new("/Volumes/Photos/catalog.sqlite"))
        );
        assert!(reopened.workspace().thirds && !reopened.workspace().state_panel);
        assert_eq!(reopened.brush.unwrap().feather, 20.0);
        assert_eq!(reopened.window.unwrap().x, -40.5);
        assert_eq!(
            reopened.export_folder.as_deref(),
            Some(Path::new("/Users/someone/Exports"))
        );
        assert_eq!(reopened.theme(), STORED_THEME);
        let launch = LaunchPreferences::read(Some(root.clone()));
        assert!(launch.problem.is_none());
        assert_eq!(launch.catalog, reopened.catalog);
        assert_eq!(launch.window, reopened.window);
        assert_eq!(launch.interface_size, 125);
        // The chosen theme is not in the library, so Luxforge Dark is drawn and the problem
        // names the choice, which stays stored.
        assert_eq!(launch.theme.id, LUXFORGE_DARK_ID);
        let problem = launch.theme.problem.unwrap();
        assert!(
            problem.detail.contains(STORED_THEME) && problem.detail.contains("unknown theme"),
            "{}",
            problem.detail
        );
        assert_eq!(
            PreferenceStore::new(Some(root.clone()))
                .read()
                .unwrap()
                .theme(),
            STORED_THEME
        );

        // An absent field is left as it is; null removes every stored value, so each follows its
        // default again, and the file holds nothing but its marker.
        let reset = PreferenceChange {
            performance_expanded: Some(None),
            auto_collapse_history: Some(None),
            auto_lens_profile: Some(None),
            raw_look: Some(None),
            mask_overlay_colour: Some(None),
            canvas_background: Some(None),
            interface_size: Some(None),
            catalog: Some(None),
            workspace: Some(None),
            brush: Some(None),
            window: Some(None),
            export_folder: Some(None),
            theme: Some(None),
        };
        let (_, after) = store.set(reset).unwrap();
        assert_eq!(after, Preferences::default());
        assert_eq!(after.theme(), LUXFORGE_DARK_ID);
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(root.join("preferences.json")).unwrap())
                .unwrap(),
            serde_json::json!({"format": 1})
        );
        let launch = LaunchPreferences::read(Some(root.clone()));
        assert_eq!(
            (launch.catalog, launch.window, launch.interface_size),
            (None, None, DEFAULT_INTERFACE_SIZE)
        );
        assert_eq!(launch.theme.id, LUXFORGE_DARK_ID);
        assert!(launch.theme.problem.is_none());
        std::fs::remove_dir_all(root).unwrap();
        let unconfigured = LaunchPreferences::read(None);
        assert!(unconfigured.problem.is_none() && unconfigured.catalog.is_none());
        assert_eq!(
            unconfigured.theme.resolved.tokens,
            crate::theme::Palette::luxforge_dark()
        );
    }

    #[test]
    fn every_bad_value_is_refused_by_name_and_writes_nothing() {
        let root = temp_path("preferences-refused");
        let store = PreferenceStore::new(Some(root.clone()));
        store
            .set(PreferenceChange {
                interface_size: Some(Some(110)),
                ..PreferenceChange::default()
            })
            .unwrap();
        let path = root.join("preferences.json");
        let stored = std::fs::read(&path).unwrap();
        let brush = |size, feather, flow| PreferenceChange {
            brush: Some(Some(BrushPreference {
                size,
                feather,
                flow,
            })),
            ..PreferenceChange::default()
        };
        let window = |width, height, x, y| PreferenceChange {
            window: Some(Some(WindowFrame {
                width,
                height,
                x,
                y,
            })),
            ..PreferenceChange::default()
        };
        for (change, refusal) in [
            (
                PreferenceChange {
                    interface_size: Some(Some(120)),
                    ..PreferenceChange::default()
                },
                "interface_size must be one of 100, 110, 125, 150",
            ),
            (
                brush(0.0, 50.0, 100.0),
                "brush.size must be a number within",
            ),
            (
                brush(3.0, 50.0, 100.0),
                "brush.size must be a number within",
            ),
            (
                brush(0.1, -1.0, 100.0),
                "brush.feather must be a number within 0..=100",
            ),
            (
                brush(0.1, 50.0, 100.5),
                "brush.flow must be a number within 0..=100",
            ),
            (
                brush(0.1, f64::NAN, 100.0),
                "brush.feather must be a number within",
            ),
            (
                window(319.0, 800.0, 0.0, 0.0),
                "window.width must be a number within 320..=16384",
            ),
            (
                window(1440.0, 16_385.0, 0.0, 0.0),
                "window.height must be a number within 320..=16384",
            ),
            (
                window(1440.0, 900.0, f32::INFINITY, 0.0),
                "window.x must be a finite number",
            ),
            (
                window(1440.0, 900.0, 0.0, f32::NAN),
                "window.y must be a finite number",
            ),
            (
                PreferenceChange {
                    catalog: Some(Some(PathBuf::from("photos/catalog.sqlite"))),
                    ..PreferenceChange::default()
                },
                "catalog must be an absolute path",
            ),
            (
                PreferenceChange {
                    catalog: Some(Some(PathBuf::new())),
                    ..PreferenceChange::default()
                },
                "catalog must not be empty",
            ),
            (
                PreferenceChange {
                    export_folder: Some(Some(PathBuf::from("Exports"))),
                    ..PreferenceChange::default()
                },
                "export_folder must be an absolute path",
            ),
            (
                PreferenceChange {
                    export_folder: Some(Some(PathBuf::new())),
                    ..PreferenceChange::default()
                },
                "export_folder must not be empty",
            ),
            (
                PreferenceChange {
                    theme: Some(Some(String::new())),
                    ..PreferenceChange::default()
                },
                "theme must be a theme id of 1..=96 printable characters",
            ),
            (
                PreferenceChange {
                    theme: Some(Some("theme-\n".into())),
                    ..PreferenceChange::default()
                },
                "theme must be a theme id",
            ),
        ] {
            let error = store.set(change).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "{}", error.detail);
            assert!(error.detail.starts_with(refusal), "{}", error.detail);
            assert_eq!(std::fs::read(&path).unwrap(), stored);
        }
        // The edges of every range are accepted.
        let Some(crate::ParameterKind::Number { min, max }) =
            crate::mask::commands::find(ADD_STROKE)
                .and_then(|command| command.action.parameter("size"))
                .map(|parameter| parameter.kind.clone())
        else {
            panic!("{ADD_STROKE} declares the brush size");
        };
        for change in [
            brush(min, 0.0, 0.0),
            brush(max, 100.0, 100.0),
            window(320.0, 16_384.0, -16_384.0, 0.0),
        ] {
            store.set(change).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_the_general_rows_count_as_general() {
        let defaults = Preferences::default();
        let mut changed = defaults.clone();
        PreferenceChange {
            performance_expanded: Some(Some(false)),
            workspace: Some(Some(WorkspacePreference {
                thirds: true,
                ..WorkspacePreference::default()
            })),
            brush: Some(Some(BrushPreference {
                size: 0.2,
                feather: 0.0,
                flow: 50.0,
            })),
            window: Some(Some(WindowFrame {
                width: 800.0,
                height: 600.0,
                x: 0.0,
                y: 0.0,
            })),
            export_folder: Some(Some(PathBuf::from("/tmp"))),
            // A default chosen explicitly shows the same as the default.
            auto_lens_profile: Some(Some(true)),
            raw_look: Some(Some(RawLook::Standard)),
            ..PreferenceChange::default()
        }
        .apply(&mut changed);
        assert_eq!(changed.general(), defaults.general());
        for change in [
            PreferenceChange {
                auto_collapse_history: Some(Some(false)),
                ..PreferenceChange::default()
            },
            PreferenceChange {
                auto_lens_profile: Some(Some(false)),
                ..PreferenceChange::default()
            },
            PreferenceChange {
                raw_look: Some(Some(RawLook::Neutral)),
                ..PreferenceChange::default()
            },
            PreferenceChange {
                mask_overlay_colour: Some(Some(MaskOverlayColour::White)),
                ..PreferenceChange::default()
            },
            PreferenceChange {
                canvas_background: Some(Some(CanvasBackground::Black)),
                ..PreferenceChange::default()
            },
            PreferenceChange {
                interface_size: Some(Some(150)),
                ..PreferenceChange::default()
            },
            PreferenceChange {
                catalog: Some(Some(PathBuf::from("/tmp/catalog.sqlite"))),
                ..PreferenceChange::default()
            },
        ] {
            let mut changed = defaults.clone();
            change.apply(&mut changed);
            assert_ne!(changed.general(), defaults.general());
        }
    }

    #[test]
    fn the_canvas_background_spells_each_choice_once() {
        for background in CanvasBackground::ALL {
            assert_eq!(
                CanvasBackground::parse(background.as_str()),
                Some(background)
            );
            assert_eq!(
                serde_json::to_value(background).unwrap(),
                background.as_str()
            );
        }
        assert_eq!(CanvasBackground::parse("white"), None);
        assert_eq!(CanvasBackground::default(), CanvasBackground::Theme);
    }

    #[test]
    fn the_raw_look_spells_each_choice_once() {
        for look in RawLook::ALL {
            assert_eq!(RawLook::parse(look.as_str()), Some(look));
            assert_eq!(serde_json::to_value(look).unwrap(), look.as_str());
        }
        assert_eq!(RawLook::parse("camera"), None);
        assert_eq!(RawLook::default(), RawLook::Standard);
    }

    #[test]
    fn the_catalog_is_the_explicit_one_the_stored_location_or_the_default() {
        let root = temp_path("catalog-selection");
        let default = root.join("config").join(CATALOG_FILE);
        let present = root.join("Photos").join(CATALOG_FILE);
        let missing = root.join("Unplugged").join(CATALOG_FILE);
        let select = |explicit: Option<&Path>, stored: Option<&Path>| {
            CatalogSelection::select(
                explicit.map(Path::to_path_buf),
                Some(default.clone()),
                stored.map(Path::to_path_buf),
            )
        };
        // Nothing exists yet: the stored folder is missing, so the default is selected.
        assert_eq!(
            select(None, Some(&present)),
            Some(CatalogSelection {
                path: default.clone(),
                missing: Some(present.clone())
            })
        );
        assert!(!root.exists(), "selecting creates nothing");
        std::fs::create_dir_all(present.parent().unwrap()).unwrap();
        // A stored location whose folder exists is selected, though it holds no catalog yet.
        assert_eq!(select(None, Some(&present)).unwrap().path, present);
        let named = root.join("named.sqlite");
        assert_eq!(select(Some(&named), Some(&present)).unwrap().path, named);
        assert_eq!(
            select(None, Some(&missing)).unwrap(),
            CatalogSelection {
                path: default.clone(),
                missing: Some(missing)
            }
        );
        assert_eq!(select(None, None).unwrap().path, default);
        // Nowhere to keep the default and nothing naming a catalog.
        assert_eq!(CatalogSelection::select(None, None, None), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
