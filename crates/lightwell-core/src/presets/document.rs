//! Lightwell's own preset document, `{"format": "lightwell.preset", "version": 1, "name",
//! "group"?, "settings"}`: what `preset.export` writes and every import reads back one to one.
use super::report::{ImportReport, MappedSetting};
use super::value::bounded;
use super::{
    FORMAT_LIGHTWELL, ImportedPreset, PRESET_DOCUMENT_FORMAT, PRESET_DOCUMENT_VERSION,
    PresetOrigin, file_stem, not_a_preset, preset_name, trimmed, validate_settings,
};
use crate::{Error, ModuleRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[allow(dead_code, reason = "checked before the document is read")]
    format: String,
    #[allow(dead_code, reason = "checked before the document is read")]
    version: u64,
    name: String,
    #[serde(default)]
    group: Option<String>,
    settings: Map<String, Value>,
}

#[derive(Serialize)]
struct Written<'a> {
    format: &'a str,
    version: u64,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<&'a str>,
    settings: &'a Map<String, Value>,
}

fn json_error(error: serde_json::Error) -> Error {
    // serde_json refuses nesting past 128 levels with this message and no category of its own.
    if error.to_string().starts_with("recursion limit exceeded") {
        Error::resource_limit("the JSON nests deeper than 128 levels")
    } else {
        Error::unsupported_input(format!("malformed JSON: {error}"))
    }
}

/// Read a Lightwell preset document. Any JSON object that does not name the Lightwell format is
/// not a preset, and any version other than 1 is refused by number.
pub(super) fn read(
    text: &str,
    file_name: Option<&str>,
    registry: &ModuleRegistry,
) -> Result<ImportedPreset, Error> {
    let value: Value = serde_json::from_str(text).map_err(json_error)?;
    if value.get("format").and_then(Value::as_str) != Some(PRESET_DOCUMENT_FORMAT) {
        return Err(not_a_preset());
    }
    match value.get("version") {
        Some(version) if version.as_u64() == Some(PRESET_DOCUMENT_VERSION) => {}
        Some(version) => {
            return Err(Error::unsupported_input(format!(
                "Lightwell preset document version {version} is not supported; this build reads \
                 version {PRESET_DOCUMENT_VERSION}"
            )));
        }
        None => {
            return Err(Error::unsupported_input(
                "the Lightwell preset document has no version",
            ));
        }
    }
    let document: Document = serde_json::from_value(value).map_err(|error| {
        Error::unsupported_input(format!("malformed Lightwell preset document: {error}"))
    })?;
    validate_settings(registry, &document.settings)?;
    let mut mapped = Vec::new();
    for (action, fields) in &document.settings {
        let Value::Object(fields) = fields else {
            continue;
        };
        for (field, applied) in fields {
            mapped.push(MappedSetting {
                setting: format!("{action}.{field}"),
                value: bounded(&applied.to_string()),
                action: action.clone(),
                field: field.clone(),
                applied: applied.clone(),
            });
        }
    }
    mapped.sort_by(|a, b| a.setting.cmp(&b.setting));
    Ok(ImportedPreset {
        name: preset_name([Some(document.name), file_name.and_then(file_stem)]),
        group: document.group.as_deref().and_then(trimmed),
        settings: document.settings,
        origin: PresetOrigin::Lightwell {},
        report: ImportReport {
            format: FORMAT_LIGHTWELL.into(),
            process_version: None,
            mapped,
            neutral: Vec::new(),
            unsupported: Vec::new(),
            refused: Vec::new(),
        },
    })
}

/// The document text, pretty-printed with a final newline. Settings keys are sorted, because a
/// settings set is a JSON map.
pub(super) fn write(name: &str, group: Option<&str>, settings: &Map<String, Value>) -> String {
    let mut text = serde_json::to_string_pretty(&Written {
        format: PRESET_DOCUMENT_FORMAT,
        version: PRESET_DOCUMENT_VERSION,
        name,
        group,
        settings,
    })
    .expect("a document of strings and JSON values always serializes");
    text.push('\n');
    text
}
