//! Batch preset and export (`docs/design/catalog.md`, "The catalog"): a preset applied to many
//! photographs, or many photographs exported into one folder, as N single calls would do it, one
//! photograph at a time, with one report ([`BatchReport`]) that names every photograph left out and
//! why.
//!
//! - **Apply preset** applies a library preset to each photograph through exactly the path
//!   `edit.apply-preset` takes, so each gets the one entry, `Preset: <name>`, a single call would
//!   write, under a request identity derived from the batch's and the photograph's
//!   ([`request_id`]), so a retry never applies it twice.
//! - **Export** writes each photograph's current entry as `export.jpeg` writes it, into one
//!   existing folder, under the name the export's naming rule gives it there ([`destination`]),
//!   never replacing a file.
//!
//! What is common to both lives here; the owner's half (the job, and the calls on the owner) is
//! `api/owner/library/batch.rs`.
use crate::{
    AssetId, Error, MutationOutcome, SkippedSetting,
    atomic_file::file_error,
    catalog_types::{BatchReport, BatchSettingsSkipped, BatchSkip},
    editor::{ActionResult, library_rows},
    export::publish::{self, Destination},
    jobs::JobControl,
    presets::PresetRecord,
};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// A photograph in Removed: a batch leaves it as it is until it is put back.
pub(crate) const REMOVED: &str = "removed";
/// A photograph with a draft open on it in the caller's session.
pub(crate) const DRAFT_OPEN: &str = "draft-open";
/// A photograph whose history the caller's session is previewing.
pub(crate) const HISTORY_SELECTED: &str = "history-selected";
/// A photograph none of the preset's settings apply to.
pub(crate) const NOT_APPLICABLE: &str = "not-applicable";
/// A photograph that already has every setting of the preset that applies to it.
pub(crate) const UNCHANGED: &str = "unchanged";

/// The longest request identity an envelope carries.
const MAX_REQUEST_ID: usize = 128;

/// The request identity of one photograph's part of a batch: the batch's request identity and the
/// photograph's, `<request_id>/<asset_id>`, so the history entry names the batch it came from and a
/// retry of the batch finds what its first attempt did. A batch identity too long to fit beside the
/// photograph's within the envelope's 128 bytes is replaced by 32 hex digits of its SHA-256.
pub(crate) fn request_id(batch: &str, asset: &AssetId) -> String {
    let asset = asset.as_str();
    if batch.len() + 1 + asset.len() <= MAX_REQUEST_ID {
        return format!("{batch}/{asset}");
    }
    let digest = Sha256::digest(batch.as_bytes());
    let hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{hex}/{asset}")
}

/// A photograph a batch leaves out because the single call would refuse it, with that refusal's
/// code and words.
pub(crate) fn refused(asset: &AssetId, error: &Error) -> BatchSkip {
    skip(asset, error.kind.code(), error.detail.clone())
}

pub(crate) fn skip(asset: &AssetId, code: &str, reason: impl Into<String>) -> BatchSkip {
    BatchSkip {
        asset_id: asset.clone(),
        code: code.to_owned(),
        reason: reason.into(),
    }
}

/// The skip of a photograph in Removed, or `None` for one that is not. An unknown photograph is
/// refused by name: its targets resolved it, so it was deleted since.
pub(crate) fn removed(
    connection: &Connection,
    asset: &AssetId,
) -> Result<Option<BatchSkip>, Error> {
    match library_rows::asset_removed(connection, asset)? {
        None => Err(Error::validation(format!("unknown asset {asset}"))),
        Some(None) => Ok(None),
        Some(Some(_)) => Ok(Some(skip(
            asset,
            REMOVED,
            "it is in Removed; put it back to include it",
        ))),
    }
}

/// One preset as every photograph of a batch receives it: the `edit.apply-preset` parameters a
/// client sends after reading the preset from the library (its `settings`, `name` and `id` as
/// `preset-id`), and the batch's envelope.
#[derive(Clone, Debug)]
pub(crate) struct PresetApply {
    pub parameters: Value,
    pub name: String,
    pub request_id: String,
    pub actor: String,
    /// How many fields the preset sets, over all its actions.
    fields: usize,
}

/// What applying a preset did to one photograph.
#[derive(Debug)]
pub(crate) enum Applied {
    /// Its own new entry, and the settings left out because they do not apply to it.
    Done(Vec<SkippedSetting>),
    Skipped(BatchSkip),
}

impl PresetApply {
    pub(crate) fn new(preset: PresetRecord, request_id: String, actor: String) -> Self {
        let fields = preset
            .settings
            .values()
            .map(|fields| fields.as_object().map_or(0, Map::len))
            .sum();
        Self {
            parameters: json!({
                "settings": preset.settings,
                "name": preset.name,
                "preset-id": preset.id,
            }),
            name: preset.name,
            request_id,
            actor,
            fields,
        }
    }

    /// What one `edit.apply-preset` answer means for the batch: a new entry is done; a no-op is
    /// left out, as `not-applicable` when every setting of the preset was skipped for this
    /// photograph and `unchanged` when it already had the ones that apply.
    pub(crate) fn outcome(&self, asset: &AssetId, result: ActionResult) -> Applied {
        if result.mutation.outcome != MutationOutcome::NoOp {
            return Applied::Done(result.skipped);
        }
        let skipped: usize = result
            .skipped
            .iter()
            .map(|setting| match &setting.parameter {
                Some(_) => 1,
                None => self.fields_of(&setting.action),
            })
            .sum();
        if !result.skipped.is_empty() && skipped >= self.fields {
            let mut reasons: Vec<&str> = Vec::new();
            for setting in &result.skipped {
                if !reasons.contains(&setting.reason.as_str()) {
                    reasons.push(&setting.reason);
                }
            }
            return Applied::Skipped(skip(
                asset,
                NOT_APPLICABLE,
                format!(
                    "none of {}'s settings apply to it: {}",
                    self.name,
                    reasons.join("; ")
                ),
            ));
        }
        Applied::Skipped(skip(
            asset,
            UNCHANGED,
            format!("it already has {}'s settings", self.name),
        ))
    }

    fn fields_of(&self, action: &str) -> usize {
        self.parameters["settings"][action]
            .as_object()
            .map_or(0, Map::len)
    }
}

/// The folder a batch export writes into: an absolute path to an existing folder. A relative path
/// or a file is `validation`; a folder that is not there or cannot be read is `read-error`.
pub(crate) fn export_folder(path: &Path) -> Result<PathBuf, Error> {
    if !path.is_absolute() {
        return Err(Error::validation(
            "the export destination must be an absolute path to a folder",
        ));
    }
    let metadata = fs::metadata(path).map_err(|error| file_error(path.display(), error.kind()))?;
    if !metadata.is_dir() {
        return Err(Error::validation(format!(
            "{} is not a folder",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

/// Where one photograph is exported in `folder`: the export's naming rule
/// ([`publish::suggest`]) from its original's file stem, `<stem>-edited.jpg`, else `-edited-2.jpg`
/// and so on, checked as `export.jpeg` checks its destination. Named just before its file is
/// written, one photograph at a time, so two originals of one name get two names. When all 64
/// names the rule reads are taken, `conflict`.
pub(crate) fn destination(folder: &Path, original: &Path) -> Result<Destination, Error> {
    let stem = original
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            Error::validation(format!(
                "{} has no file name to export under",
                original.display()
            ))
        })?;
    let path = publish::suggest(folder, stem).ok_or_else(|| {
        Error::conflict(format!(
            "{stem}-edited.jpg and the next 63 names the export gives it are taken in {}",
            folder.display()
        ))
    })?;
    Destination::check(&path)
}

/// A batch's report as it grows, one photograph at a time: kept on the job's worker, published on
/// the activity board as "3 of 18" with its fraction, and mirrored an item at a time into the answer
/// so far that `job.read` reports while the job runs, so each photograph costs the same whatever the
/// batch's size.
pub(crate) struct Progress<'a> {
    control: &'a JobControl,
    report: BatchReport,
    total: usize,
    handled: usize,
}

impl<'a> Progress<'a> {
    /// A batch of `total` photographs, none handled yet.
    pub(crate) fn new(control: &'a JobControl, total: usize) -> Self {
        let progress = Self {
            control,
            report: BatchReport::default(),
            total,
            handled: 0,
        };
        control.update_partial(|partial| *partial = json!({"done": [], "skipped": []}));
        progress.show(0.0);
        progress
    }

    /// A photograph done, with the file it wrote or the settings it was done without.
    pub(crate) fn done(
        &mut self,
        asset: AssetId,
        written: Option<PathBuf>,
        settings: Vec<SkippedSetting>,
    ) {
        self.push("done", json!(asset));
        if let Some(path) = &written {
            self.push("written", json!(path));
        }
        if !settings.is_empty() {
            let skipped = BatchSettingsSkipped {
                asset_id: asset.clone(),
                settings,
            };
            self.push("settings_skipped", json!(skipped));
            self.report.settings_skipped.push(skipped);
        }
        self.report.done.push(asset);
        self.report.written.extend(written);
        self.handled += 1;
        self.show(0.0);
    }

    /// A photograph left out, and why.
    pub(crate) fn skip(&mut self, skip: BatchSkip) {
        self.push("skipped", json!(skip));
        self.report.skipped.push(skip);
        self.handled += 1;
        self.show(0.0);
    }

    /// How far the photograph being handled has got, from 0 to 1, for the board's fraction.
    pub(crate) fn within(&self, fraction: f64) {
        self.show(fraction.clamp(0.0, 1.0));
    }

    pub(crate) fn finish(self) -> BatchReport {
        self.report
    }

    fn show(&self, within: f64) {
        let fraction = if self.total == 0 {
            1.0
        } else {
            (self.handled as f64 + within) / self.total as f64
        };
        self.control.set_progress(
            Some(fraction.min(1.0)),
            &format!("{} of {}", self.handled, self.total),
        );
    }

    fn push(&self, field: &str, item: Value) {
        self.control
            .update_partial(|partial| match partial[field].as_array_mut() {
                Some(items) => items.push(item),
                None => partial[field] = json!([item]),
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_request_identity_names_the_batch_and_the_photograph_within_the_envelope() {
        let asset = AssetId::new();
        assert_eq!(request_id("batch-1", &asset), format!("batch-1/{asset}"));
        let long = "r".repeat(MAX_REQUEST_ID);
        let derived = request_id(&long, &asset);
        assert!(derived.len() <= MAX_REQUEST_ID, "{derived}");
        assert!(derived.ends_with(&format!("/{asset}")));
        assert_eq!(derived, request_id(&long, &asset), "the same every time");
        assert_ne!(derived, request_id(&"s".repeat(MAX_REQUEST_ID), &asset));
        assert_ne!(derived, request_id(&long, &AssetId::new()));
    }

    #[test]
    fn a_batch_export_folder_is_an_existing_absolute_folder() {
        let folder = luxforge_testbase::paths::temp_dir("batch-folder");
        assert_eq!(export_folder(&folder).unwrap(), folder);
        let relative = export_folder(Path::new("exports")).unwrap_err();
        assert_eq!(relative.kind, crate::ErrorKind::Validation);
        let file = folder.join("a.jpg");
        fs::write(&file, b"x").unwrap();
        let not_a_folder = export_folder(&file).unwrap_err();
        assert_eq!(not_a_folder.kind, crate::ErrorKind::Validation);
        let gone = export_folder(&folder.join("gone")).unwrap_err();
        assert_eq!(gone.kind, crate::ErrorKind::FileAccess);
    }

    #[test]
    fn a_batch_export_names_each_file_by_the_export_rule_in_its_folder() {
        let folder = luxforge_testbase::paths::temp_dir("batch-names");
        let original = Path::new("/Volumes/Card/DCIM/DSC_0042.NEF");
        let first = destination(&folder, original).unwrap();
        assert_eq!(first.path(), folder.join("DSC_0042-edited.jpg"));
        fs::write(first.path(), b"x").unwrap();
        let second = destination(&folder, original).unwrap();
        assert_eq!(second.path(), folder.join("DSC_0042-edited-2.jpg"));
        fs::write(second.path(), b"x").unwrap();
        for n in 3..=64 {
            fs::write(folder.join(format!("DSC_0042-edited-{n}.jpg")), b"x").unwrap();
        }
        let full = destination(&folder, original).unwrap_err();
        assert_eq!(full.kind, crate::ErrorKind::Conflict);
    }
}
