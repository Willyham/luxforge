//! The preset library: named settings sets in the catalog's `presets` table, beside history, so
//! they share its single owner, its atomic writes and its backup.
//!
//! Everything here is catalog and text work for the owner thread. A record is a settings set of at
//! most 16 × 64 fields plus its import report, an import parses at most
//! [`MAX_PRESET_BYTES`](super::MAX_PRESET_BYTES) of text, which is kept verbatim, and capture reads
//! the stored payloads of one entry. Nothing opens a source, decodes, renders or hashes pixels.
//! Writes are short `BEGIN IMMEDIATE` transactions through the catalog's one `write` helper.
//! `docs/design/presets.md` is the contract.
use super::{
    ImportReport, PresetExport, PresetOrigin, ReportCounts, export_document, inspect_preset,
    parse_preset, validate_settings,
};
use crate::{
    AssetId, EditorService, EntryId, Error, ErrorKind, MAX_PRESET_NAME, MAX_SETTINGS_ACTIONS,
    MAX_SETTINGS_FIELDS, ModuleRegistry, MutationOutcome, PresetId,
    editor::{decode, encode, in_target, now_ms, write},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashSet;

/// The most presets one library holds; creating or importing one more is a `resource-limit`
/// error.
pub const MAX_PRESETS: usize = 1_000;
/// The longest group name, in characters.
pub const MAX_PRESET_GROUP: usize = 64;
/// The group a preset created in Lightwell takes when the request names none.
pub const USER_PRESET_GROUP: &str = "User presets";
/// The group an import takes when neither the request nor the file names one.
pub const IMPORTED_PRESET_GROUP: &str = "Imported";

/// The longest actor, in bytes, as for versions and mutations.
const MAX_ACTOR: usize = 128;

/// One library preset as a client reads it. `preset.list` returns the [`PresetSummary`] form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetRecord<R = ImportReport> {
    pub id: PresetId,
    /// Trimmed, 1 to [`MAX_PRESET_NAME`] characters and no control characters.
    pub name: String,
    /// Trimmed, 1 to [`MAX_PRESET_GROUP`] characters and no control characters. The (group, name)
    /// pair is unique in the library, ignoring case.
    pub group: String,
    /// Checked against the registry, every action and field, when it was stored.
    pub settings: Map<String, Value>,
    pub origin: PresetOrigin,
    /// The import report, or `None` for a preset created in Lightwell.
    pub report: Option<R>,
    /// Who created the record or last changed it.
    pub actor: String,
    pub created_ms: i64,
    /// `created_ms` until an update changes the name, the group or the settings.
    pub updated_ms: i64,
    /// The actions of `settings` this registry cannot apply: unknown, not a field patch or provided
    /// by an unavailable module. Computed whenever the record is read and never stored, so a
    /// module disabled after the preset was saved is reported rather than silently skipped.
    pub unavailable: Vec<String>,
}

/// A `preset.list` row: the record with its report reduced to the four counts.
pub type PresetSummary = PresetRecord<ReportCounts>;

impl PresetRecord {
    /// The listing form of this record.
    pub fn summary(self) -> PresetSummary {
        PresetRecord {
            id: self.id,
            name: self.name,
            group: self.group,
            settings: self.settings,
            origin: self.origin,
            report: self.report.as_ref().map(ImportReport::counts),
            actor: self.actor,
            created_ms: self.created_ms,
            updated_ms: self.updated_ms,
            unavailable: self.unavailable,
        }
    }
}

/// What `preset.update` did, with the record as it now stands.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetUpdate {
    pub outcome: MutationOutcome,
    pub preset: PresetRecord,
}

/// The `record_json` column: the record without the columns beside it and without the computed
/// `unavailable` list.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    settings: Map<String, Value>,
    origin: PresetOrigin,
    report: Option<ImportReport>,
    actor: String,
    created_ms: i64,
    updated_ms: i64,
}

#[derive(Serialize)]
struct Written<'a> {
    settings: &'a Map<String, Value>,
    origin: &'a PresetOrigin,
    report: Option<&'a ImportReport>,
    actor: &'a str,
    created_ms: i64,
    updated_ms: i64,
}

impl<'a> Written<'a> {
    fn of(record: &'a PresetRecord) -> Self {
        Self {
            settings: &record.settings,
            origin: &record.origin,
            report: record.report.as_ref(),
            actor: &record.actor,
            created_ms: record.created_ms,
            updated_ms: record.updated_ms,
        }
    }
}

/// The columns every record read selects, in this order.
const RECORD_COLUMNS: &str = "id,name,group_name,record_json";

/// One row's record columns, before the JSON is decoded.
struct Columns {
    id: String,
    name: String,
    group: String,
    record_json: String,
}

fn columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<Columns> {
    Ok(Columns {
        id: row.get(0)?,
        name: row.get(1)?,
        group: row.get(2)?,
        record_json: row.get(3)?,
    })
}

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn unknown_preset(preset_id: &PresetId) -> Error {
    validation(format!("unknown preset {preset_id}"))
}

/// Trimmed text of 1 to `max` characters with no control character.
fn printable(what: &str, value: &str, max: usize) -> Result<String, Error> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(validation(format!(
            "preset {what} must contain 1..={max} printable characters"
        )));
    }
    Ok(value.to_owned())
}

fn checked_name(name: &str) -> Result<String, Error> {
    printable("name", name, MAX_PRESET_NAME)
}

fn checked_group(group: &str) -> Result<String, Error> {
    printable("group", group, MAX_PRESET_GROUP)
}

fn checked_actor(actor: &str) -> Result<(), Error> {
    if actor.is_empty() || actor.len() > MAX_ACTOR {
        return Err(validation("actor must contain 1..128 characters"));
    }
    Ok(())
}

/// The actions of a settings set that this registry cannot apply, in key order.
fn unavailable_actions(registry: &ModuleRegistry, settings: &Map<String, Value>) -> Vec<String> {
    settings
        .keys()
        .filter(|action_id| {
            !registry
                .action(action_id)
                .is_some_and(|(module, action)| action.patch && module.descriptor().is_available())
        })
        .cloned()
        .collect()
}

/// A stored row as a record, with `unavailable` computed against this registry. A record the
/// current shape cannot read is `incompatible` and is left as it is.
fn record(registry: &ModuleRegistry, columns: Columns) -> Result<PresetRecord, Error> {
    let stored: Stored = decode("invalid preset record", columns.record_json)?;
    Ok(PresetRecord {
        id: PresetId::parse(columns.id)?,
        name: columns.name,
        group: columns.group,
        unavailable: unavailable_actions(registry, &stored.settings),
        settings: stored.settings,
        origin: stored.origin,
        report: stored.report,
        actor: stored.actor,
        created_ms: stored.created_ms,
        updated_ms: stored.updated_ms,
    })
}

fn load(
    connection: &Connection,
    registry: &ModuleRegistry,
    preset_id: &PresetId,
) -> Result<PresetRecord, Error> {
    let columns = connection
        .query_row(
            &format!("SELECT {RECORD_COLUMNS} FROM presets WHERE id=?1"),
            [preset_id.as_str()],
            columns,
        )
        .optional()?
        .ok_or_else(|| unknown_preset(preset_id))?;
    record(registry, columns)
}

/// Refuse a library that already holds [`MAX_PRESETS`] presets.
fn ensure_room(tx: &Transaction<'_>) -> Result<(), Error> {
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM presets", [], |row| row.get(0))?;
    if count >= MAX_PRESETS as i64 {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!("the preset library holds at most {MAX_PRESETS} presets; delete one first"),
        ));
    }
    Ok(())
}

/// Refuse a (group, name) pair another preset holds, ignoring case. The table's `NOCASE`
/// uniqueness folds ASCII only, so the comparison is made here with full Unicode lowercasing; it
/// reads the two short columns of at most [`MAX_PRESETS`] rows and decodes no record. `except` is
/// the preset being renamed, which may change the case of its own name.
fn ensure_unique(
    tx: &Transaction<'_>,
    group: &str,
    name: &str,
    except: Option<&PresetId>,
) -> Result<(), Error> {
    let (group_key, name_key) = (group.to_lowercase(), name.to_lowercase());
    let mut statement = tx.prepare("SELECT id,name,group_name FROM presets")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (id, existing_name, existing_group) = row?;
        if except.is_some_and(|except| except.as_str() == id) {
            continue;
        }
        if existing_group.to_lowercase() == group_key && existing_name.to_lowercase() == name_key {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "group {existing_group:?} already holds a preset named {existing_name:?}; \
                     choose another name or group"
                ),
            ));
        }
    }
    Ok(())
}

impl EditorService {
    /// Every library preset, sorted by group and then by name, ignoring case. Each report is
    /// reduced to its counts and no imported text is read.
    pub fn presets(&self) -> Result<Vec<PresetSummary>, Error> {
        let mut statement = self
            .connection
            .prepare(&format!("SELECT {RECORD_COLUMNS} FROM presets"))?;
        let rows = statement.query_map([], columns)?;
        let mut presets = Vec::new();
        for row in rows {
            presets.push(record(self.registry(), row?)?.summary());
        }
        presets
            .sort_by_cached_key(|preset| (preset.group.to_lowercase(), preset.name.to_lowercase()));
        Ok(presets)
    }

    /// One library preset with its full report, and the imported file's text as it was sent, or
    /// `None` for a preset created in Lightwell.
    pub fn preset(&self, preset_id: &PresetId) -> Result<(PresetRecord, Option<String>), Error> {
        let (columns, source_text) = self
            .connection
            .query_row(
                &format!("SELECT {RECORD_COLUMNS},source_text FROM presets WHERE id=?1"),
                [preset_id.as_str()],
                |row| Ok((columns(row)?, row.get::<_, Option<String>>(4)?)),
            )
            .optional()?
            .ok_or_else(|| unknown_preset(preset_id))?;
        Ok((record(self.registry(), columns)?, source_text))
    }

    /// Store a settings set as a Lightwell preset, in `group` or [`USER_PRESET_GROUP`]. Every
    /// action and field is checked against the registry; a duplicate (group, name) pair is a
    /// `conflict` and a full library a `resource-limit` error, and neither writes anything.
    pub fn create_preset(
        &mut self,
        name: &str,
        group: Option<&str>,
        settings: &Map<String, Value>,
        actor: &str,
    ) -> Result<PresetRecord, Error> {
        checked_actor(actor)?;
        let name = checked_name(name)?;
        let group = checked_group(group.unwrap_or(USER_PRESET_GROUP))?;
        validate_settings(self.registry(), settings)?;
        let now = now_ms();
        let record = PresetRecord {
            id: PresetId::new(),
            name,
            group,
            settings: settings.clone(),
            origin: PresetOrigin::Lightwell {},
            report: None,
            actor: actor.to_owned(),
            created_ms: now,
            updated_ms: now,
            unavailable: Vec::new(),
        };
        self.insert_preset(&record, None)?;
        Ok(record)
    }

    /// Change a preset's name, group or settings. Nothing that differs is a no-op, which writes
    /// nothing and keeps `updated_ms`; a change records `actor` and the time. A rename onto
    /// another preset's (group, name) pair is a `conflict`. The origin and the import report stay
    /// what they were: they describe where the preset came from.
    pub fn update_preset(
        &mut self,
        preset_id: &PresetId,
        actor: &str,
        name: Option<&str>,
        group: Option<&str>,
        settings: Option<&Map<String, Value>>,
    ) -> Result<PresetUpdate, Error> {
        checked_actor(actor)?;
        let name = name.map(checked_name).transpose()?;
        let group = group.map(checked_group).transpose()?;
        let registry = self.registry().clone();
        if let Some(settings) = settings {
            validate_settings(&registry, settings)?;
        }
        write(&mut self.connection, |tx| {
            let mut preset = load(tx, &registry, preset_id)?;
            let changed = name.as_ref().is_some_and(|name| *name != preset.name)
                || group.as_ref().is_some_and(|group| *group != preset.group)
                || settings.is_some_and(|settings| *settings != preset.settings);
            if !changed {
                // The transaction commits having written nothing.
                return Ok(PresetUpdate {
                    outcome: MutationOutcome::NoOp,
                    preset,
                });
            }
            if let Some(name) = name {
                preset.name = name;
            }
            if let Some(group) = group {
                preset.group = group;
            }
            if let Some(settings) = settings {
                preset.settings = settings.clone();
                preset.unavailable = unavailable_actions(&registry, &preset.settings);
            }
            ensure_unique(tx, &preset.group, &preset.name, Some(preset_id))?;
            preset.actor = actor.to_owned();
            preset.updated_ms = now_ms();
            tx.execute(
                "UPDATE presets SET name=?2,group_name=?3,record_json=?4 WHERE id=?1",
                params![
                    preset_id.as_str(),
                    preset.name,
                    preset.group,
                    encode(&Written::of(&preset))?
                ],
            )?;
            Ok(PresetUpdate {
                outcome: MutationOutcome::Applied,
                preset,
            })
        })
    }

    /// Remove a preset: `Applied` when it existed and `NoOp` when it is absent. History entries
    /// that applied it keep their settings, because an entry stores what was applied.
    pub fn delete_preset(&mut self, preset_id: &PresetId) -> Result<MutationOutcome, Error> {
        let removed = write(&mut self.connection, |tx| {
            Ok(tx.execute("DELETE FROM presets WHERE id=?1", [preset_id.as_str()])?)
        })?;
        Ok(if removed == 0 {
            MutationOutcome::NoOp
        } else {
            MutationOutcome::Applied
        })
    }

    /// The preset as a Lightwell preset document, which [`EditorService::import_preset`] reads
    /// back to the same name, group and settings.
    pub fn export_preset(&self, preset_id: &PresetId) -> Result<PresetExport, Error> {
        let preset = load(&self.connection, self.registry(), preset_id)?;
        Ok(export_document(
            &preset.name,
            Some(&preset.group),
            &preset.settings,
        ))
    }

    /// `preset.inspect`: `{preset, report}` for what an import of this text would store, with
    /// nothing stored. The preset has the shape of a record with `id`, `actor`, `created_ms` and
    /// `updated_ms` null, the file's name, and the file's group or [`IMPORTED_PRESET_GROUP`]. A
    /// file that maps nothing still returns its report, with empty settings. The name and group
    /// are the file's, before any request override and the library's name rules, which only an
    /// import applies.
    pub fn inspect_import(&self, content: &str, file_name: Option<&str>) -> Result<Value, Error> {
        let imported = inspect_preset(content, file_name, self.registry())?;
        let group = imported
            .group
            .unwrap_or_else(|| IMPORTED_PRESET_GROUP.to_owned());
        let unavailable = unavailable_actions(self.registry(), &imported.settings);
        Ok(json!({
            "preset": {
                "id": null,
                "name": imported.name,
                "group": group,
                "settings": imported.settings,
                "origin": imported.origin,
                "report": imported.report,
                "actor": null,
                "created_ms": null,
                "updated_ms": null,
                "unavailable": unavailable,
            },
            "report": imported.report,
        }))
    }

    /// Import a preset file's text as a library preset. The request's `name` and `group` override
    /// the file's; the group falls back to [`IMPORTED_PRESET_GROUP`]. The text is kept verbatim so
    /// a later importer can map what this one could not. A parse error, a file that maps nothing,
    /// a duplicate or a full library stores nothing.
    pub fn import_preset(
        &mut self,
        content: &str,
        file_name: Option<&str>,
        name: Option<&str>,
        group: Option<&str>,
        actor: &str,
    ) -> Result<PresetRecord, Error> {
        checked_actor(actor)?;
        let imported = parse_preset(content, file_name, self.registry())?;
        let name = checked_name(name.unwrap_or(&imported.name))?;
        let group = checked_group(
            group
                .or(imported.group.as_deref())
                .unwrap_or(IMPORTED_PRESET_GROUP),
        )?;
        validate_settings(self.registry(), &imported.settings)?;
        let now = now_ms();
        let record = PresetRecord {
            id: PresetId::new(),
            name,
            group,
            unavailable: unavailable_actions(self.registry(), &imported.settings),
            settings: imported.settings,
            origin: imported.origin,
            report: Some(imported.report),
            actor: actor.to_owned(),
            created_ms: now,
            updated_ms: now,
        };
        self.insert_preset(&record, Some(content))?;
        Ok(record)
    }

    fn insert_preset(
        &mut self,
        record: &PresetRecord,
        source_text: Option<&str>,
    ) -> Result<(), Error> {
        write(&mut self.connection, |tx| {
            ensure_room(tx)?;
            ensure_unique(tx, &record.group, &record.name, None)?;
            tx.execute(
                "INSERT INTO presets (id,name,group_name,record_json,source_text) VALUES (?1,?2,?3,?4,?5)",
                params![
                    record.id.as_str(),
                    record.name,
                    record.group,
                    encode(&Written::of(record))?,
                    source_text
                ],
            )?;
            Ok(())
        })
    }

    /// `preset.capture`: a settings set read from one entry's stack. `fields` maps field-patch
    /// actions to an array of their parameter names or to `true` for all of them. Each action reads
    /// the global layers of its module's effects — a masked layer is another target's, and a preset
    /// never reads or writes one: with none, each field takes its declared default; with one, its
    /// value comes from the module's `values` for that layer and a missing value takes the default;
    /// two or more are `validation: ambiguous`. A field with no value and no default is refused
    /// rather than left out.
    ///
    /// This reads stored payloads only, `O(layers × actions)`: no source is opened and nothing is
    /// sampled or rendered, so a JPEG and a RAW entry cost the same.
    pub fn capture_preset(
        &self,
        asset_id: &AssetId,
        entry_id: &EntryId,
        fields: &Map<String, Value>,
    ) -> Result<Map<String, Value>, Error> {
        if fields.is_empty() || fields.len() > MAX_SETTINGS_ACTIONS {
            return Err(validation(format!(
                "fields names 1 to {MAX_SETTINGS_ACTIONS} actions; this one names {}",
                fields.len()
            )));
        }
        let registry = self.registry();
        let entry = self.entry(asset_id, entry_id)?;
        let layers = &entry.snapshot.recipe.layers;
        let mut settings = Map::new();
        for (action_id, wanted) in fields {
            let (module, action) = registry
                .action(action_id)
                .ok_or_else(|| validation(format!("unknown action {action_id}")))?;
            if !action.patch {
                return Err(validation(format!(
                    "{action_id} is not a field-patch action"
                )));
            }
            let descriptor = module.descriptor();
            if !descriptor.is_available() {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    format!("unavailable module {}", descriptor.id),
                ));
            }
            let names: Vec<&str> = match wanted {
                Value::Bool(true) => action
                    .parameters
                    .iter()
                    .map(|parameter| parameter.name.as_str())
                    .collect(),
                Value::Array(names) if !names.is_empty() && names.len() <= MAX_SETTINGS_FIELDS => {
                    let mut seen = HashSet::new();
                    names
                        .iter()
                        .map(|name| {
                            let name = name.as_str().ok_or_else(|| {
                                validation(format!(
                                    "the fields of {action_id} must be parameter names"
                                ))
                            })?;
                            if action.parameter(name).is_none() {
                                return Err(validation(format!(
                                    "unknown parameter {name} for action {action_id}"
                                )));
                            }
                            if !seen.insert(name) {
                                return Err(validation(format!(
                                    "the fields of {action_id} name {name} twice"
                                )));
                            }
                            Ok(name)
                        })
                        .collect::<Result<_, Error>>()?
                }
                _ => {
                    return Err(validation(format!(
                        "the fields of {action_id} must be true or an array of 1 to \
                         {MAX_SETTINGS_FIELDS} parameter names"
                    )));
                }
            };
            // A preset addresses the global layer, so capture reads the layers a preset step of
            // this action plans against: the global target's, never a masked layer's.
            let mut owned = layers.iter().filter(|layer| {
                descriptor.effect(&layer.effect_id).is_some() && in_target(registry, layer, None)
            });
            let values = match (owned.next(), owned.next()) {
                (None, _) => Map::new(),
                (Some(layer), None) => {
                    module.values(&layer.effect_id, layer.effect_format, &layer.payload)?
                }
                (Some(_), Some(_)) => {
                    return Err(validation(format!("ambiguous {} layers", descriptor.title)));
                }
            };
            let mut captured = Map::new();
            for name in names {
                let parameter = action
                    .parameter(name)
                    .expect("every name was matched to a declared parameter above");
                let value = values
                    .get(name)
                    .or(parameter.default.as_ref())
                    .ok_or_else(|| {
                        validation(format!(
                            "parameter {name} of {action_id} has no default and the stack does \
                             not set it"
                        ))
                    })?;
                captured.insert(name.to_owned(), value.clone());
            }
            settings.insert(action_id.clone(), Value::Object(captured));
        }
        // The module's own reading of a payload must be a set the action accepts, or the capture
        // would hand the client a preset it cannot store or apply.
        validate_settings(registry, &settings)?;
        Ok(settings)
    }
}
