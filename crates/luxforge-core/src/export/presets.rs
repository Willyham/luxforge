//! Export presets (`docs/design/export-settings.md`, built-in presets and API): Web, Print and
//! Master are built in, and the person's own live in `export-presets.json` beside
//! `preferences.json` in the host's configuration directory, outside every catalog.
//!
//! The document is one [`JsonDocument`]: bounded, format-marked and changed through one locked
//! read-modify-write, so a file this build cannot read is refused with `incompatible`, naming it,
//! and kept unchanged. Every stored value is checked as a request's is: each preset's settings by
//! [`ExportSettings::check`], its name by [`check_name`], at most [`MAX_PRESETS`] presets, names
//! unique ignoring case across built-in and stored presets alike, and identities unique.
use super::settings::{
    Chroma, ExportFolder, ExportSettings, FormatSettings, MetadataLevel, OutputSize,
};
use crate::{
    Error,
    capabilities::document::JsonDocument,
    tiles::{
        output::Depth,
        sharpen::{Sharpen, SharpenAmount, SharpenMedium},
    },
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The file's name beside the preferences.
pub const FILE: &str = "export-presets.json";
/// The most presets of the person's own.
pub const MAX_PRESETS: usize = 100;
/// The longest preset name, in characters.
pub const MAX_NAME: usize = 64;
/// The longest preset identity a request names.
pub const MAX_ID: usize = 64;
/// The file's bound: a hundred presets, each with a chosen folder of up to 4 KiB, fit well within.
const MAX_BYTES: u64 = 1024 * 1024;
/// The document's format marker.
const FORMAT: u32 = 1;
/// The prefix of a stored preset's identity.
const STORED_PREFIX: &str = "export-preset-";

/// The built-in presets' identities.
pub const WEB: &str = "luxforge.web";
pub const PRINT: &str = "luxforge.print";
pub const MASTER: &str = "luxforge.master";

/// One preset as `export.presets.list` answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPreset {
    pub preset_id: String,
    pub name: String,
    pub builtin: bool,
    pub settings: ExportSettings,
}

/// Web, Print and Master with the design's values, each saving to the original's folder: Web
/// sharpens for the screen and Print for glossy paper, both at Standard, and Master not at all.
pub fn builtin() -> [ExportPreset; 3] {
    let preset = |preset_id: &str, name: &str, settings| ExportPreset {
        preset_id: preset_id.to_owned(),
        name: name.to_owned(),
        builtin: true,
        settings,
    };
    [
        preset(
            WEB,
            "Web",
            ExportSettings {
                format: FormatSettings::Jpeg {
                    quality: 80,
                    chroma: Chroma::Half,
                    limit_bytes: None,
                },
                size: OutputSize::LongEdge { pixels: 2048 },
                optimise: true,
                sharpen: Some(Sharpen {
                    medium: SharpenMedium::Screen,
                    amount: SharpenAmount::Standard,
                }),
                metadata: MetadataLevel::Copyright,
                pixels_per_inch: None,
                folder: ExportFolder::Original,
                suffix: "-web".into(),
            },
        ),
        preset(
            PRINT,
            "Print",
            ExportSettings {
                format: FormatSettings::Jpeg {
                    quality: 95,
                    chroma: Chroma::Full,
                    limit_bytes: None,
                },
                size: OutputSize::Full,
                optimise: false,
                sharpen: Some(Sharpen {
                    medium: SharpenMedium::Glossy,
                    amount: SharpenAmount::Standard,
                }),
                metadata: MetadataLevel::NoLocation,
                pixels_per_inch: Some(300),
                folder: ExportFolder::Original,
                suffix: "-print".into(),
            },
        ),
        preset(
            MASTER,
            "Master",
            ExportSettings {
                format: FormatSettings::Tiff {
                    bit_depth: Depth::Sixteen,
                },
                size: OutputSize::Full,
                optimise: true,
                sharpen: None,
                metadata: MetadataLevel::All,
                pixels_per_inch: Some(300),
                folder: ExportFolder::Original,
                suffix: "-master".into(),
            },
        ),
    ]
}

/// A stored preset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    preset_id: String,
    name: String,
    settings: ExportSettings,
}

impl Stored {
    fn preset(&self) -> ExportPreset {
        ExportPreset {
            preset_id: self.preset_id.clone(),
            name: self.name.clone(),
            builtin: false,
            settings: self.settings.clone(),
        }
    }
}

/// The document: the person's presets in the order they were created.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    presets: Vec<Stored>,
}

impl Document {
    /// Every rule a stored document meets that its shape cannot say, the same for a write and a
    /// file read back. The settings were checked as they were read.
    fn check(&self) -> Result<(), String> {
        if self.presets.len() > MAX_PRESETS {
            return Err(format!("more than {MAX_PRESETS} export presets"));
        }
        for (index, preset) in self.presets.iter().enumerate() {
            check_name(&preset.name).map_err(|error| error.detail)?;
            if !is_stored_id(&preset.preset_id) {
                return Err(format!(
                    "{} is not an export preset identity",
                    preset.preset_id
                ));
            }
            for earlier in &self.presets[..index] {
                if earlier.preset_id == preset.preset_id {
                    return Err(format!("{} is stored twice", preset.preset_id));
                }
                if same_name(&earlier.name, &preset.name) {
                    return Err(format!("the name {} is stored twice", preset.name));
                }
            }
            if builtin()
                .iter()
                .any(|builtin| same_name(&builtin.name, &preset.name))
            {
                return Err(format!("the name {} is a built-in preset's", preset.name));
            }
        }
        Ok(())
    }

    fn find(&self, preset_id: &str) -> Option<usize> {
        self.presets
            .iter()
            .position(|preset| preset.preset_id == preset_id)
    }

    /// Refuse `name` when another preset, built-in or stored other than `except`, has it.
    fn name_free(&self, name: &str, except: Option<&str>) -> Result<(), Error> {
        let builtin = builtin();
        let taken = builtin.iter().any(|preset| same_name(&preset.name, name))
            || self.presets.iter().any(|preset| {
                Some(preset.preset_id.as_str()) != except && same_name(&preset.name, name)
            });
        if taken {
            return Err(
                Error::conflict(format!("an export preset is already named {name}"))
                    .with_data(serde_json::json!({"field": "name"})),
            );
        }
        Ok(())
    }
}

/// Names compare ignoring case.
fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn is_stored_id(preset_id: &str) -> bool {
    preset_id
        .strip_prefix(STORED_PREFIX)
        .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn is_builtin(preset_id: &str) -> bool {
    [WEB, PRINT, MASTER].contains(&preset_id)
}

/// A preset name is 1–64 characters with no control character and no leading or trailing space.
pub fn check_name(name: &str) -> Result<(), Error> {
    let count = name.chars().count();
    let refused = |reason: &str| {
        Error::validation(format!("name {reason}")).with_data(serde_json::json!({"field": "name"}))
    };
    if count == 0 || count > MAX_NAME {
        return Err(refused(&format!("must be 1 to {MAX_NAME} characters")));
    }
    if name.chars().any(char::is_control) {
        return Err(refused("must not contain a control character"));
    }
    if name.trim() != name {
        return Err(refused("must not start or end with a space"));
    }
    Ok(())
}

/// The person's export presets, in the host's configuration directory when it has one. It holds no
/// state: every call reads the file again.
pub(crate) struct ExportPresetStore(Option<JsonDocument<Document>>);

impl ExportPresetStore {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self(
            dir.map(|dir| JsonDocument::new(dir, FILE, MAX_BYTES, FORMAT).checked(Document::check)),
        )
    }

    fn document(&self) -> Result<&JsonDocument<Document>, Error> {
        self.0
            .as_ref()
            .ok_or_else(|| Error::not_ready("no user preference directory is configured"))
    }

    /// Every preset: the built-in ones first, then the person's by name ignoring case. A host with
    /// no configuration directory has only the built-in ones.
    pub(crate) fn list(&self) -> Result<Vec<ExportPreset>, Error> {
        let mut stored = match &self.0 {
            Some(document) => document.read()?.unwrap_or_default().presets,
            None => Vec::new(),
        };
        stored.sort_by_key(|preset| (preset.name.to_lowercase(), preset.preset_id.clone()));
        Ok(builtin()
            .into_iter()
            .chain(stored.iter().map(Stored::preset))
            .collect())
    }

    /// Store a new preset of the person's own.
    pub(crate) fn create(
        &self,
        name: &str,
        settings: ExportSettings,
    ) -> Result<ExportPreset, Error> {
        check_name(name)?;
        settings.check()?;
        self.document()?.transact(|document| {
            if document.presets.len() >= MAX_PRESETS {
                return Err(Error::resource_limit(format!(
                    "at most {MAX_PRESETS} export presets are kept; delete one first"
                )));
            }
            document.name_free(name, None)?;
            let stored = Stored {
                preset_id: format!("{STORED_PREFIX}{}", uuid::Uuid::new_v4().simple()),
                name: name.to_owned(),
                settings,
            };
            document.presets.push(stored.clone());
            Ok(stored.preset())
        })
    }

    /// Rename a preset of the person's own, replace its settings, or both. A built-in preset is
    /// refused with `validation`, as is an identity no preset has.
    pub(crate) fn update(
        &self,
        preset_id: &str,
        name: Option<&str>,
        settings: Option<ExportSettings>,
    ) -> Result<ExportPreset, Error> {
        refuse_builtin(preset_id, "updated")?;
        if let Some(name) = name {
            check_name(name)?;
        }
        if let Some(settings) = &settings {
            settings.check()?;
        }
        self.document()?.transact(|document| {
            let index = document.find(preset_id).ok_or_else(|| unknown(preset_id))?;
            if let Some(name) = name {
                document.name_free(name, Some(preset_id))?;
                document.presets[index].name = name.to_owned();
            }
            if let Some(settings) = settings {
                document.presets[index].settings = settings;
            }
            Ok(document.presets[index].preset())
        })
    }

    /// Delete a preset of the person's own: `true` when it was there. A built-in preset is refused
    /// with `validation`.
    pub(crate) fn delete(&self, preset_id: &str) -> Result<bool, Error> {
        refuse_builtin(preset_id, "deleted")?;
        self.document()?.transact(|document| {
            Ok(match document.find(preset_id) {
                Some(index) => {
                    document.presets.remove(index);
                    true
                }
                None => false,
            })
        })
    }
}

fn refuse_builtin(preset_id: &str, done: &str) -> Result<(), Error> {
    if is_builtin(preset_id) {
        return Err(Error::validation(format!(
            "{preset_id} is a built-in export preset and cannot be {done}; duplicate it instead"
        ))
        .with_data(serde_json::json!({"field": "preset_id"})));
    }
    Ok(())
}

fn unknown(preset_id: &str) -> Error {
    Error::validation(format!("unknown export preset {preset_id}"))
        .with_data(serde_json::json!({"field": "preset_id"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use luxforge_test_macros::slow;
    use luxforge_testbase::paths::temp_path;
    use std::fs;

    fn store() -> (PathBuf, ExportPresetStore) {
        let root = temp_path("export-presets");
        (root.clone(), ExportPresetStore::new(Some(root)))
    }

    #[test]
    fn the_built_in_presets_take_the_designs_values_and_are_valid() {
        let [web, print, master] = builtin();
        for preset in [&web, &print, &master] {
            preset.settings.check().unwrap();
            assert!(preset.builtin);
            assert_eq!(preset.settings.folder, ExportFolder::Original);
        }
        assert_eq!(
            web.settings.to_json(),
            serde_json::json!({
                "format": "jpeg", "size": {"fit": "long_edge", "pixels": 2048}, "quality": 80,
                "optimise": true, "chroma": "420", "limit_bytes": null,
                "sharpen": {"medium": "screen", "amount": "standard"},
                "metadata": "copyright", "pixels_per_inch": null, "folder": "original",
                "suffix": "-web",
            })
        );
        assert_eq!(
            print.settings.to_json(),
            serde_json::json!({
                "format": "jpeg", "size": {"fit": "full"}, "quality": 95, "optimise": false,
                "chroma": "444", "limit_bytes": null,
                "sharpen": {"medium": "glossy", "amount": "standard"}, "metadata": "no_location",
                "pixels_per_inch": 300, "folder": "original", "suffix": "-print",
            })
        );
        assert_eq!(
            master.settings.to_json(),
            serde_json::json!({
                "format": "tiff", "size": {"fit": "full"}, "optimise": true, "bit_depth": 16,
                "sharpen": null, "metadata": "all", "pixels_per_inch": 300,
                "folder": "original", "suffix": "-master",
            })
        );
    }

    #[test]
    fn presets_round_trip_through_create_list_update_and_delete() {
        let (root, store) = store();
        assert_eq!(store.list().unwrap().len(), 3, "built-ins only");
        assert!(!root.exists(), "listing creates nothing");
        let mut settings = ExportSettings::baseline(MetadataLevel::Copyright);
        settings.suffix = "-mine".into();
        let created = store.create("Zebra", settings.clone()).unwrap();
        let second = store.create("apple", settings.clone()).unwrap();
        assert!(!created.builtin);
        assert!(is_stored_id(&created.preset_id), "{}", created.preset_id);
        let names: Vec<String> = store.list().unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Web", "Print", "Master", "apple", "Zebra"]);

        let mut changed = settings.clone();
        changed.optimise = true;
        let updated = store
            .update(&created.preset_id, Some("Yak"), Some(changed.clone()))
            .unwrap();
        assert_eq!(
            (updated.name.as_str(), &updated.settings),
            ("Yak", &changed)
        );
        // Reopened, as another process or the next launch reads it.
        let reopened = ExportPresetStore::new(Some(root.clone()));
        let listed = reopened.list().unwrap();
        assert_eq!(listed[4], updated);
        assert_eq!(listed[3], second);

        assert!(reopened.delete(&created.preset_id).unwrap());
        assert!(
            !reopened.delete(&created.preset_id).unwrap(),
            "already gone"
        );
        assert_eq!(reopened.list().unwrap().len(), 4);
    }

    #[test]
    fn built_in_presets_refuse_update_and_delete() {
        let (_, store) = store();
        for preset_id in [WEB, PRINT, MASTER] {
            let error = store.update(preset_id, Some("Mine"), None).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation);
            let error = store.delete(preset_id).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation);
        }
        assert_eq!(store.list().unwrap(), builtin());
    }

    #[test]
    fn names_are_unique_ignoring_case_and_bounded() {
        let (_, store) = store();
        let settings = ExportSettings::baseline(MetadataLevel::None);
        let first = store.create("Mine", settings.clone()).unwrap();
        for taken in ["mine", "MINE", "web", "Print"] {
            let error = store.create(taken, settings.clone()).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Conflict, "{taken}");
        }
        // A preset keeps its own name in another case.
        store.update(&first.preset_id, Some("MINE"), None).unwrap();
        for bad in ["", " padded", "x".repeat(65).as_str(), "tab\there"] {
            let error = store.create(bad, settings.clone()).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation, "{bad:?}");
        }
        assert_eq!(
            store
                .update("export-preset-0123", Some("x"), None)
                .unwrap_err()
                .kind,
            ErrorKind::Validation
        );
    }

    /// A hundred durable writes.
    #[test]
    #[slow]
    fn the_person_keeps_at_most_100_presets() {
        let (_, store) = store();
        let settings = ExportSettings::baseline(MetadataLevel::None);
        for n in 0..MAX_PRESETS {
            store
                .create(&format!("Preset {n}"), settings.clone())
                .unwrap();
        }
        let error = store.create("One too many", settings).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(store.list().unwrap().len(), MAX_PRESETS + 3);
    }

    #[test]
    fn an_unreadable_or_unsupported_document_is_kept_unchanged_and_refused_by_name() {
        let (root, store) = store();
        fs::create_dir_all(&root).unwrap();
        let path = root.join(FILE);
        for contents in [
            "not json".to_owned(),
            r#"{"format": 2, "presets": []}"#.to_owned(),
            r#"{"format": 1, "presets": [{"preset_id": "export-preset-00000000000000000000000000000000", "name": "x", "settings": {"format": "png"}}]}"#.to_owned(),
        ] {
            fs::write(&path, &contents).unwrap();
            let error = store.list().unwrap_err();
            assert_eq!(error.kind, ErrorKind::Incompatible, "{contents}");
            assert!(error.detail.contains(FILE), "{}", error.detail);
            let error = store
                .create("Mine", ExportSettings::baseline(MetadataLevel::None))
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Incompatible);
            assert_eq!(fs::read_to_string(&path).unwrap(), contents, "kept");
        }
    }

    #[test]
    fn a_host_without_a_configuration_directory_lists_only_the_built_in_presets() {
        let store = ExportPresetStore::new(None);
        assert_eq!(store.list().unwrap(), builtin());
        let error = store
            .create("Mine", ExportSettings::baseline(MetadataLevel::None))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotReady);
    }
}
