//! A developed photograph's camera preview, which it shows until its first render: its original's
//! embedded preview (a RAW's) or the original itself (a JPEG's), extracted exactly as a browsed
//! file's grid and loupe tiers are (`extract.rs`), fitted within the photograph's tier (512 px for
//! the grid, 2048 px for the large tier), upright and labelled `embedded`.
//!
//! It is one task of the preview lane's extraction workers (`lane.rs`), keyed
//! `(ViewItem::Photo(row), tier)`, planned on the owner from the catalog's record of the original
//! ([`CameraSource`]) and written as the photograph's `photo_previews` row of the entry current when
//! it was planned (`photos.rs`), which a render then replaces. Before reading the file the task
//! checks it is still the original the catalog recorded — its identity and length, as a 100% region
//! of a photograph does — and refuses anything else as `source-unavailable`, and it checks again
//! before writing. It never develops: a RAW with no usable camera preview (the Canon EOS R5 Mark
//! II's and R8's H.265 previews) ends deferred, `not-ready`, since its render is its first
//! preview.
use super::{
    cache::{Store, intact},
    extract::{FileImages, Found},
    lane::{Ended, Task, lasting, now_ms},
    photos::{self, CAMERA_RENDERER, NewTier},
    rendered::tier_side,
};
use crate::{
    AssetId, EntryId, Error, SourceTag,
    catalog_types::{
        AssetRowId, FileRecord, FileSignature, HeaderState, PreviewInfo, PreviewOrigin,
        PreviewTier, VolumeId,
    },
    editor::source_signature,
};
use std::{fs, path::PathBuf};

/// A photograph's original as the catalog records it, and the entry current when its camera
/// preview was asked for: everything the task reads, planned on the owner from the catalog alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CameraSource {
    pub row: AssetRowId,
    pub asset_id: AssetId,
    pub entry_id: EntryId,
    pub locator: PathBuf,
    /// The directory on disk it is in.
    pub folder: PathBuf,
    pub name: String,
    pub volume_id: VolumeId,
    pub kind: SourceTag,
    pub file_identity: String,
    pub byte_len: u64,
}

/// The signature the lane remembers a photograph's camera-preview failure under: the length the
/// catalog recorded, with no time or identity. The original never legitimately changes — a file
/// that did is `source-unavailable`, which is never remembered — so a failure holds until
/// Luxforge restarts.
pub(crate) fn recorded_signature(source: &CameraSource) -> FileSignature {
    FileSignature {
        len: source.byte_len,
        modified_ns: 0,
        identity: None,
    }
}

/// The original's signature now, when it is still the file the catalog recorded: its identity and
/// length, the rule the editor applies before it prepares an original.
fn original(source: &CameraSource) -> Result<FileSignature, Error> {
    let metadata = fs::metadata(&source.locator).map_err(|_| {
        Error::source_unavailable(format!(
            "the original of {} is not available",
            source.asset_id
        ))
    })?;
    let recorded = source_signature(&source.locator, &metadata);
    if !metadata.is_file()
        || recorded.file_identity() != source.file_identity
        || recorded.byte_len() != source.byte_len
    {
        return Err(Error::source_unavailable(format!(
            "{} is no longer the original of {}",
            source.locator.display(),
            source.asset_id
        )));
    }
    Ok(FileSignature::of(&metadata))
}

/// Make the camera preview `task` asks for, as its extraction worker does: answered from the cache
/// when the photograph already has a render of the tier or a camera preview of it; otherwise the
/// original checked, its preview extracted and fitted within the tier, the original checked again,
/// and the tier written (and, for a large tier, the shared budget kept). `ended` says whether a
/// failure is lasting or a deferral.
pub(crate) fn make(
    store: &mut Store,
    task: &Task,
    source: &CameraSource,
    ended: &mut Ended,
) -> Result<PreviewInfo, Error> {
    let tier = task.key.1;
    let control = &task.control;
    control.checkpoint()?;
    if let Some(served) = served(store, source, tier)? {
        return Ok(served);
    }
    let signature = original(source)?;
    let record = FileRecord {
        path: source.locator.clone(),
        folder: source.folder.clone(),
        name: source.name.clone(),
        volume_id: source.volume_id.clone(),
        signature,
        kind: source.kind,
        header: HeaderState::Pending,
        last_seen_ms: 0,
    };
    let side = tier_side(tier)?;
    let mut images = FileImages::open(&record, control).inspect_err(|error| {
        ended.permanent = lasting(error.kind);
    })?;
    let found = images.preview(&record, side, control);
    drop(images);
    let made = match found {
        Ok(Found::Made(made)) => made,
        Ok(Found::Unusable(why)) => {
            ended.deferred = true;
            return Err(Error::not_ready(format!(
                "{} has no usable camera preview ({why}); its render is its first preview",
                source.name
            )));
        }
        Err(error) => {
            ended.permanent = lasting(error.kind);
            return Err(error);
        }
    };
    control.checkpoint()?;
    if original(source)? != signature {
        return Err(Error::source_unavailable(format!(
            "{} changed while its preview was read",
            source.locator.display()
        )));
    }
    let name = photos::camera_file_name(&source.asset_id, &source.entry_id, tier)?;
    let written = photos::write(
        store,
        &NewTier {
            asset_id: &source.asset_id,
            entry_id: &source.entry_id,
            tier,
            renderer: CAMERA_RENDERER,
            origin: PreviewOrigin::Embedded,
            approximate: false,
            drawn: None,
            name: &name,
            jpeg: &made.jpeg,
            width: made.width,
            height: made.height,
            now_ms: now_ms(),
            control,
        },
    )?;
    match written {
        Some(row) => {
            if tier == PreviewTier::Large {
                store.evict(task.budget, &row.path)?;
            }
            Ok(row.info(&source.asset_id))
        }
        // A render of the tier was written meanwhile, and is what the photograph shows.
        None => served(store, source, tier)?
            .ok_or_else(|| Error::internal("a render refused a camera preview and is gone")),
    }
}

/// The row that already answers the photograph's `tier` better than or as well as a new camera
/// preview would: any render of the tier, or a camera preview of it, whose file is intact.
fn served(
    store: &Store,
    source: &CameraSource,
    tier: PreviewTier,
) -> Result<Option<PreviewInfo>, Error> {
    let rows = photos::rows(store.connection(), &source.asset_id)?;
    let mut candidates: Vec<_> = rows.iter().filter(|row| row.tier == tier).collect();
    candidates.sort_by_key(|row| !row.rendered());
    Ok(candidates
        .into_iter()
        .find(|row| intact(&row.path))
        .map(|row| row.info(&source.asset_id)))
}
