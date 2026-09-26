//! Presets: import and the library. Import is format detection, Lightwell's own preset document,
//! Lightroom Classic XMP presets and sidecars, legacy `.lrtemplate` presets, the Lightroom mapping
//! table and the per-setting report. The library keeps presets in the catalog.
//!
//! The importer is a pure function over text and the module registry. It reads no file, opens no
//! catalog and renders nothing, so the catalog owner can run it within the request limit: a 1 MiB
//! document is text work, not frame work. The library, the API methods and the desktop build on
//! it; `docs/design/presets.md` is the contract.
mod document;
mod library;
mod lrtemplate;
mod mapping;
mod report;
mod value;
mod xmp;

#[cfg(test)]
mod library_tests;
#[cfg(test)]
mod tests;

pub use library::{
    IMPORTED_PRESET_GROUP, MAX_PRESET_GROUP, MAX_PRESETS, PresetRecord, PresetSummary,
    PresetUpdate, USER_PRESET_GROUP,
};
pub use lrtemplate::{MAX_TEMPLATE_DEPTH, MAX_TEMPLATE_VALUES};
pub use report::{ImportReport, MappedSetting, ReportCounts, ReportedSetting};
pub use xmp::{MAX_XMP_ATTRIBUTE_PAIRS, MAX_XMP_DEPTH, MAX_XMP_NAMESPACES, MAX_XMP_NODES};

#[cfg(test)]
use crate::ErrorKind;
use crate::{
    Error, MAX_SETTINGS_ACTIONS, MAX_SETTINGS_FIELDS, ModuleRegistry, check_parameters, valid_name,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The largest preset text accepted, in bytes: the desktop's file limit and the request bound.
pub const MAX_PRESET_BYTES: usize = 1024 * 1024;
/// The `format` marker of a Lightwell preset document.
pub const PRESET_DOCUMENT_FORMAT: &str = "lightwell.preset";
/// The only preset document version this build reads and writes.
pub const PRESET_DOCUMENT_VERSION: u64 = 1;
/// The name an import takes when neither the file nor its file name gives one.
pub const IMPORTED_PRESET_NAME: &str = "Imported preset";

const FORMAT_LIGHTWELL: &str = "lightwell";
const FORMAT_XMP: &str = "lightroom-xmp";
const FORMAT_TEMPLATE: &str = "lightroom-template";

/// A parsed preset: what the library stores for an import, before the request's own name and
/// group override the file's.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ImportedPreset {
    /// From the file: `crs:Name`, the template's `title` or `internalName`, or the document's
    /// `name`; then the file name without its extension; then [`IMPORTED_PRESET_NAME`]. Trimmed.
    pub name: String,
    /// `crs:Group` or the document's `group`, trimmed, when the file names one. The library falls
    /// back to the request's group and then to `Imported`.
    pub group: Option<String>,
    /// A settings set of the mapped values, sorted by key. Empty when nothing mapped, which
    /// [`parse_preset`] refuses and [`inspect_preset`] reports.
    pub settings: Map<String, Value>,
    pub origin: PresetOrigin,
    pub report: ImportReport,
}

/// Where a preset came from, stored on its library record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PresetOrigin {
    /// Created in Lightwell or read from a Lightwell preset document. An empty struct rather than
    /// a unit variant, because serde ignores unknown keys after a unit variant's tag and this
    /// shape refuses them like the others.
    Lightwell {},
    LightroomXmp {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_name: Option<String>,
        /// `crs:UUID`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uuid: Option<String>,
        /// `crs:ProcessVersion`, as written.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_version: Option<String>,
        /// `crs:PresetType`, which is `Normal` for every preset that imports.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset_type: Option<String>,
    },
    LightroomTemplate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_name: Option<String>,
        /// The template's `id`, or `value.uuid` when it has none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uuid: Option<String>,
    },
}

/// A Lightwell preset document ready to save, as `preset.export` returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PresetExport {
    /// `<name>.lwpreset`, with characters no common file system allows replaced by `_`.
    pub file_name: String,
    pub content: String,
}

fn not_a_preset() -> Error {
    Error::unsupported_input("not a Lightwell, Lightroom XMP or .lrtemplate preset")
}

fn duplicate_setting(name: &str) -> Error {
    Error::unsupported_input(format!("the preset sets {name} more than once"))
}

/// Text trimmed, or `None` when nothing is left.
fn trimmed(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// A file name without its directory or its last extension, trimmed.
fn file_stem(file_name: &str) -> Option<String> {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    let stem = match base.rfind('.') {
        Some(dot) if dot > 0 => &base[..dot],
        _ => base,
    };
    trimmed(stem)
}

/// The first candidate that is not blank, trimmed, or [`IMPORTED_PRESET_NAME`].
fn preset_name(candidates: impl IntoIterator<Item = Option<String>>) -> String {
    candidates
        .into_iter()
        .flatten()
        .find_map(|name| trimmed(&name))
        .unwrap_or_else(|| IMPORTED_PRESET_NAME.to_owned())
}

/// Whether `text`, after leading whitespace, starts `s = {`.
fn is_template(text: &str) -> bool {
    fn space(text: &str) -> &str {
        text.trim_start_matches([' ', '\t', '\r', '\n'])
    }
    text.strip_prefix('s')
        .and_then(|rest| space(rest).strip_prefix('='))
        .is_some_and(|rest| space(rest).starts_with('{'))
}

/// Detect a preset's format and read it, returning its report whatever it maps.
///
/// This is `preset.inspect`: a file that maps nothing still returns its full report, with empty
/// settings. Every error is structured: `unsupported-input` for an unrecognized format, a
/// Lightroom profile, a template that is not a Develop preset, malformed syntax (a template names
/// the byte offset, XML the line and column) or a setting written twice; `resource-limit` for
/// text over [`MAX_PRESET_BYTES`], nesting past [`MAX_XMP_DEPTH`] or [`MAX_TEMPLATE_DEPTH`], more
/// than [`MAX_XMP_NODES`] XML nodes or [`MAX_TEMPLATE_VALUES`] template values; and the
/// [`validate_settings`] errors for a Lightwell document whose settings this registry refuses.
pub fn inspect_preset(
    content: &str,
    file_name: Option<&str>,
    registry: &ModuleRegistry,
) -> Result<ImportedPreset, Error> {
    if content.len() > MAX_PRESET_BYTES {
        return Err(Error::resource_limit(format!(
            "a preset is at most {MAX_PRESET_BYTES} bytes; this one is {}",
            content.len()
        )));
    }
    let body = content.strip_prefix('\u{feff}').unwrap_or(content);
    let body = body.trim_start_matches([' ', '\t', '\r', '\n']);
    let start = content.len() - body.len();
    let stem = || file_name.and_then(file_stem);
    let file_name_owned = file_name.map(str::to_owned);
    if body.starts_with('{') {
        return document::read(body, file_name, registry);
    }
    if body.starts_with('<') {
        let settings = xmp::read(body)?;
        let text = |name: &str| {
            settings
                .iter()
                .find(|setting| setting.name == name)
                .and_then(|setting| setting.value.text())
                .map(str::to_owned)
        };
        let mapped = mapping::map(FORMAT_XMP, &settings, registry)?;
        return Ok(ImportedPreset {
            name: preset_name([text("Name"), stem()]),
            group: text("Group").as_deref().and_then(trimmed),
            settings: mapped.settings,
            origin: PresetOrigin::LightroomXmp {
                file_name: file_name_owned,
                uuid: text("UUID"),
                process_version: text("ProcessVersion"),
                preset_type: text("PresetType"),
            },
            report: mapped.report,
        });
    }
    if is_template(body) {
        let template = lrtemplate::read(content, start)?;
        let mapped = mapping::map(FORMAT_TEMPLATE, &template.settings, registry)?;
        return Ok(ImportedPreset {
            name: preset_name([template.title, template.internal_name, stem()]),
            group: None,
            settings: mapped.settings,
            origin: PresetOrigin::LightroomTemplate {
                file_name: file_name_owned,
                uuid: template.id.or(template.uuid),
            },
            report: mapped.report,
        });
    }
    Err(not_a_preset())
}

/// Detect, read and map a preset for import: [`inspect_preset`], refusing a file that maps
/// nothing with `unsupported-input` and the four report counts.
pub fn parse_preset(
    content: &str,
    file_name: Option<&str>,
    registry: &ModuleRegistry,
) -> Result<ImportedPreset, Error> {
    let preset = inspect_preset(content, file_name, registry)?;
    if preset.settings.is_empty() {
        return Err(Error::unsupported_input(format!(
            "the preset has no setting Lightwell can apply ({})",
            preset.report.counts()
        )));
    }
    Ok(preset)
}

/// Check a settings set against the registry, without a stack: 1 to 16 actions of 1 to 64 fields
/// each, every action registered, declared `patch: true` and provided by an available module,
/// and every field passing that action's own parameter check. The library runs this when a set
/// is created, updated or imported; the host checks again when one is applied.
pub fn validate_settings(
    registry: &ModuleRegistry,
    settings: &Map<String, Value>,
) -> Result<(), Error> {
    if settings.is_empty() || settings.len() > MAX_SETTINGS_ACTIONS {
        return Err(Error::validation(format!(
            "a settings set names 1 to {MAX_SETTINGS_ACTIONS} actions; this one names {}",
            settings.len()
        )));
    }
    for (action_id, fields) in settings {
        if !valid_name(action_id) {
            return Err(Error::validation(format!(
                "invalid action identity {action_id}"
            )));
        }
        let Some(fields) = fields
            .as_object()
            .filter(|fields| !fields.is_empty() && fields.len() <= MAX_SETTINGS_FIELDS)
        else {
            return Err(Error::validation(format!(
                "the settings of {action_id} must be an object of 1 to {MAX_SETTINGS_FIELDS} fields"
            )));
        };
        let (module, action) = registry
            .action(action_id)
            .ok_or_else(|| Error::validation(format!("unknown action {action_id}")))?;
        if !action.patch {
            return Err(Error::validation(format!(
                "{action_id} is not a field-patch action"
            )));
        }
        if !module.descriptor().is_available() {
            return Err(Error::incompatible(format!(
                "unavailable module {}",
                module.descriptor().id
            )));
        }
        check_parameters(action, &Value::Object(fields.clone()))?;
    }
    Ok(())
}

/// Characters Windows, macOS or Linux refuse in a file name.
fn forbidden_in_file_name(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
        )
}

/// The longest file-name stem written, in bytes, so the name with its extension fits the common
/// 255-byte limit.
const MAX_STEM_BYTES: usize = 200;

/// `<name>.lwpreset`, safe to create on every desktop platform.
fn export_file_name(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|character| {
            if forbidden_in_file_name(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    // Windows drops trailing dots and spaces, and a leading dot hides a file elsewhere.
    let edges = |character: char| character == '.' || character.is_whitespace();
    let mut stem = replaced.trim_matches(edges).to_owned();
    if stem.len() > MAX_STEM_BYTES {
        let mut cut = MAX_STEM_BYTES;
        while !stem.is_char_boundary(cut) {
            cut -= 1;
        }
        stem.truncate(cut);
        stem = stem.trim_end_matches(edges).to_owned();
    }
    if stem.is_empty() {
        stem = "preset".to_owned();
    }
    let device = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (device.len() == 4
            && (device.starts_with("COM") || device.starts_with("LPT"))
            && device.as_bytes()[3].is_ascii_digit()
            && device.as_bytes()[3] != b'0');
    if reserved {
        // Windows reserves the device name whatever extension follows it.
        stem.insert(device.len(), '_');
    }
    format!("{stem}.lwpreset")
}

/// A Lightwell preset document for these settings, pretty-printed, and its file name. The
/// document reads back through [`parse_preset`] to the same name, group and settings.
pub fn export_document(
    name: &str,
    group: Option<&str>,
    settings: &Map<String, Value>,
) -> PresetExport {
    PresetExport {
        file_name: export_file_name(name),
        content: document::write(name, group, settings),
    }
}
