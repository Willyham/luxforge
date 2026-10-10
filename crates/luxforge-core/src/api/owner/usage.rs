//! The catalog owner's usage producers: the `interaction` a host method's successful answer is, and
//! the `image.open` of an adopted photograph (`docs/engineering/usage-statistics-coverage.md`).
//!
//! Recorded at the one place every method is answered ([`Owner::answer`]), after the handler and
//! only for an answer that stands: a retry the request table or a catalog's request log answered,
//! a no-op, a refusal and a parked or deferred call record nothing. A committed recipe change's
//! `tool.commit` and a mask command's interaction are the editor service's
//! ([`crate::usage::produce`]); a job's terminal outcome is the job table's. While collection is
//! off every producer here costs one atomic load.
use super::Owner;
use crate::usage::{EventKind, Feature, Generation};
use serde_json::Value;

/// When a host method's answer is a deliberate operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Emit {
    /// When it changed something it announced: not a no-op or a retry.
    OnChange,
    /// Whenever it succeeds: it starts a job, or it is a deliberate operation that changes nothing
    /// the event log announces.
    OnSuccess,
}

/// The interaction a host method is, and when it records it. `params` decides between a
/// preset's application and a paste. Every other host method is an exemption the coverage
/// inventory names, or a producer elsewhere.
pub(crate) fn host_feature(method: &str, params: &Value) -> Option<(Feature, Emit)> {
    use Emit::{OnChange, OnSuccess};
    use Feature as F;
    Some(match method {
        "asset.move" => (F::PhotoMove, OnChange),
        "asset.remove" => (F::PhotoRemove, OnChange),
        "asset.restore" => (F::PhotoRestore, OnChange),
        "asset.send-back" => (F::PhotoSendBack, OnChange),
        "catalog.empty-removed" => (F::RemovedEmpty, OnChange),
        "collection.add" => (F::CollectionAdd, OnChange),
        "collection.create" => (F::CollectionCreate, OnChange),
        "collection.create-smart" => (F::CollectionSmartCreate, OnChange),
        "collection.update-smart" => (F::CollectionSmartUpdate, OnChange),
        "collection.delete" => (F::CollectionDelete, OnChange),
        "collection.move" => (F::CollectionMove, OnChange),
        "collection.remove" => (F::CollectionRemove, OnChange),
        "collection.rename" => (F::CollectionRename, OnChange),
        "folder.create" => (F::FolderCreate, OnChange),
        "folder.delete" => (F::FolderDelete, OnChange),
        "folder.merge" => (F::FolderMerge, OnChange),
        "folder.move" => (F::FolderMove, OnChange),
        "folder.rename" => (F::FolderRename, OnChange),
        "history.undo" => (F::HistoryUndo, OnChange),
        "history.redo" => (F::HistoryRedo, OnChange),
        "history.restore" => (F::HistoryRestore, OnChange),
        "index.add-folder" => (F::IndexedFolderAdd, OnChange),
        "index.remove-folder" => (F::IndexedFolderRemove, OnChange),
        "library.undo" => (F::LibraryUndo, OnChange),
        "library.redo" => (F::LibraryRedo, OnChange),
        "pick.set" => (F::PickSet, OnChange),
        "preset.create" => (F::PresetCreate, OnChange),
        "preset.delete" => (F::PresetDelete, OnChange),
        "preset.import" => (F::PresetImport, OnChange),
        "preset.update" => (F::PresetUpdate, OnChange),
        "source.locate" => (F::LocateOriginal, OnChange),
        "source.relink" => (F::OriginalsRelink, OnChange),
        "theme.delete" => (F::ThemeDelete, OnChange),
        "theme.import" => (F::ThemeImport, OnChange),
        "version.create" => (F::VersionCreate, OnChange),
        "version.delete" => (F::VersionDelete, OnChange),
        // Only a deliberate preference is announced; the remembered workspace, brush, window and
        // Performance section state a desktop stores as it goes are not.
        "preferences.set" | "flags.set" => (F::PreferencesChange, OnChange),
        // A single photograph's settings: one interaction beside its tool commits.
        "edit.apply-settings" => (settings_feature(params), OnChange),
        // Jobs, recorded when accepted; their outcomes are the job's own events.
        "batch.apply-settings" => (settings_feature(params), OnSuccess),
        "export.jpeg" => (F::Export, OnSuccess),
        "batch.export" => (F::ExportBatch, OnSuccess),
        "pick.develop" => (F::DevelopPicks, OnSuccess),
        "source.find" => (F::OriginalsFind, OnSuccess),
        // Writing a file the desktop saves: a deliberate operation the log does not announce.
        "preset.export" => (F::PresetExport, OnSuccess),
        "theme.export" => (F::ThemeExport, OnSuccess),
        _ => return None,
    })
}

/// A settings set's interaction: a paste's, or a preset's (a library preset named by
/// `preset_id`, or settings whose origin is a preset).
fn settings_feature(params: &Value) -> Feature {
    if params["origin"]["kind"] == "paste" {
        Feature::SettingsPaste
    } else {
        Feature::PresetApply
    }
}

impl Owner {
    /// Record the interaction a host method's standing answer is, with the image of the open
    /// photograph it acted on. `changed` says whether the call announced a change.
    pub(super) fn usage_answered(
        &self,
        generation: Generation,
        method: &str,
        params: &Value,
        changed: bool,
    ) {
        let Some((feature, emit)) = host_feature(method, params) else {
            return;
        };
        if emit == Emit::OnChange && !changed {
            return;
        }
        let usage = &self.service.usage;
        let image = params["asset_id"]
            .as_str()
            .and_then(|asset| usage.open_image(generation, asset));
        usage.record(generation, EventKind::Interaction { feature, image });
    }

    /// A client adopted `state`'s photograph as its open one: its image ID, computed now from the
    /// fingerprint the adoption holds and kept for the events that act on it, and `image.open`.
    pub(super) fn usage_adopted(&mut self, state: &crate::EditorState) {
        let usage = &mut self.service.usage;
        let Some(generation) = usage.on() else {
            return;
        };
        if let Some(image) = usage.adopt(generation, &state.asset.id, &state.asset.fingerprint) {
            usage.record(generation, EventKind::ImageOpen { image });
        }
    }
}
