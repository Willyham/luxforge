use super::{
    AssetRecord, CachedSource, EditorService, EditorState, PreparedFile, RawDevelopment,
    SourceKind, SourceSignature,
    catalog::{insert_entry, now_ms, write},
    catalog_rows::{NewAsset, insert_asset, insert_capture, top_level_folder, upsert_volume},
};
use crate::{
    AssetId, EntryId, Error, ErrorKind, HistoryEntry, LayerId, Preparation, PreparationNeeds,
    Snapshot,
    artifacts::{self, ArtifactRead, VerifiedArtifact},
    atomic_file::file_error,
    catalog_types::{Dimensions, ExifOrientation, HeaderMetadata},
    library::availability,
    open_source_bytes, read_bounded_file,
    source::{PreparedSource, RawPreparation, RawPrepared, SecondDevelopment},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

/// A RAW original's camera interpretation: the decoder's metadata, typed, and checked once where it
/// enters — an asset row read or an import — then shared by every state that carries it.
///
/// It serializes exactly as [`luxforge_raw::RawMetadata`] does, so the catalog row and the API
/// carry the object they always have. Equality is the typed fields' own at their native precision:
/// an `f32` compares as the `f32` it is, whichever decimal spelling it was read from.
#[derive(Clone, Debug, PartialEq)]
pub struct RawInterpretation(Arc<luxforge_raw::RawMetadata>);

/// Equality is reflexive because no field can hold a NaN: the decoder refuses a non-finite
/// calibration, gain, black level or white level, and JSON has no spelling for one.
impl Eq for RawInterpretation {}

impl RawInterpretation {
    /// Accept a decoder's metadata once its correction record agrees with its mode: a mode that
    /// needs DNG corrections carries their record, and no other mode carries one.
    pub(crate) fn new(metadata: luxforge_raw::RawMetadata) -> Result<Self, Error> {
        if metadata.mode.requires_dng_corrections() != metadata.dng_corrections.is_some() {
            return Err(Error::incompatible(
                "RAW correction record differs from mode",
            ));
        }
        Ok(Self(Arc::new(metadata)))
    }
}

impl std::ops::Deref for RawInterpretation {
    type Target = luxforge_raw::RawMetadata;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Serialize for RawInterpretation {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RawInterpretation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(luxforge_raw::RawMetadata::deserialize(deserializer)?)
            .map_err(|error| de::Error::custom(error.detail))
    }
}

impl SourceKind {
    /// How a prepared original is interpreted: a JPEG, or a RAW with its checked interpretation.
    fn of(source: &PreparedSource) -> Result<Self, Error> {
        Ok(match source.metadata() {
            None => Self::Jpeg,
            Some(metadata) => Self::Raw {
                metadata: RawInterpretation::new(metadata.clone())?,
            },
        })
    }
}

/// A source path in the one spelling every catalog lookup uses.
fn canonical_source(path: &Path) -> Result<PathBuf, Error> {
    path.canonicalize()
        .map_err(|error| file_error("cannot resolve source", error.kind()))
}

fn file_access(error: std::io::Error) -> Error {
    file_error("cannot read source", error.kind())
}

/// A source path's canonical spelling and its file's current signature.
fn located_signature(path: &Path) -> Result<(PathBuf, Metadata, SourceSignature), Error> {
    let canonical = canonical_source(path)?;
    let metadata = canonical.metadata().map_err(file_access)?;
    let signature = source_signature(&canonical, &metadata);
    Ok((canonical, metadata, signature))
}

impl SourceSignature {
    /// The file's length in bytes.
    pub(crate) fn byte_len(&self) -> u64 {
        self.byte_len
    }

    /// The file's identity as the catalog records it (`assets.file_identity`).
    pub(crate) fn file_identity(&self) -> &str {
        &self.file_identity
    }
}

/// The one stat every read of an original starts from — a preparation, an evaluation, an export,
/// a rendered preview — and all a present original costs: its signature now, or whether a file is
/// there that is no longer the one developed (`Err(true)`) or none is (`Err(false)`).
fn current_signature(asset: &AssetRecord) -> Result<SourceSignature, bool> {
    match asset.locator.metadata() {
        Ok(metadata) => {
            let signature = source_signature(&asset.locator, &metadata);
            if signature.file_identity == asset.file_identity
                && signature.byte_len == asset.byte_len
            {
                Ok(signature)
            } else {
                Err(true)
            }
        }
        Err(_) => Err(false),
    }
}

/// The signature an asset's original has now, off the owner (a preview worker): refused as
/// `source-unavailable` when the file is gone or is no longer the file that was developed, without
/// recording the observation, which only the owner's [`EditorService::original_signature`] does.
pub(crate) fn original_signature(asset: &AssetRecord) -> Result<SourceSignature, Error> {
    current_signature(asset).map_err(|changed| {
        Error::source_unavailable(if changed {
            "original source fingerprint changed"
        } else {
            "original source is unavailable"
        })
    })
}

impl EditorService {
    /// The signature an asset's original has now. Refused as `source-unavailable`, naming why, when
    /// the file is gone or is no longer the file that was developed ([`availability::refusal`]);
    /// only then is the catalog read, the volume looked at and the observation recorded.
    fn original_signature(&self, asset: &AssetRecord) -> Result<SourceSignature, Error> {
        current_signature(asset)
            .map_err(|changed| availability::refusal(&self.connection, asset, changed))
    }
}

/// Immutable source settings for a known file. The worker receives bounded catalog metadata,
/// then checks the decoded original before developing it at this entry's gains.
#[derive(Clone, Debug)]
pub(crate) struct FilePreparation {
    pub(crate) fingerprint: String,
    pub(crate) raw: Option<RawPreparation>,
}

impl FilePreparation {
    /// The preparation of `recipe`, a stack of `asset` its caller already admitted as one
    /// ([`EditorService::saved_entry`]).
    pub(crate) fn for_recipe(asset: &AssetRecord, recipe: &crate::Recipe) -> Result<Self, Error> {
        let raw = match &asset.source {
            SourceKind::Jpeg => None,
            SourceKind::Raw { metadata } => Some(RawPreparation {
                metadata: metadata.0.as_ref().clone(),
                gains: raw_gains(metadata, recipe)?,
            }),
        };
        Ok(Self {
            fingerprint: asset.fingerprint.clone(),
            raw,
        })
    }

    fn verify_fingerprint(&self, fingerprint: &str) -> Result<(), Error> {
        if self.fingerprint != fingerprint {
            return Err(Error::source_unavailable(
                "original source fingerprint changed",
            ));
        }
        Ok(())
    }
}

/// One source job's preparation: what the catalog owner queues on its source worker, and what the
/// blocking helpers ([`EditorService::import`], [`EditorService::prepare`]) run on the caller's
/// thread. Either way it is the same work, run by [`Self::run`] and completed by the service's one
/// completion, so no caller prepares an original or an artifact any other way.
#[derive(Debug)]
pub(crate) enum SourceWork {
    /// Read and decode the original at `path`, which must still have `signature`, checking a
    /// known file against `target` and developing a known RAW at its entry's gains.
    File {
        path: PathBuf,
        signature: SourceSignature,
        target: Option<Box<FilePreparation>>,
    },
    /// Redevelop the cached RAW mosaic at new gains.
    Develop(Box<RawDevelopment>),
    /// The asset's source is already prepared; only its artifacts need reading.
    Artifacts(AssetId),
}

/// What preparing one import or one set of needs takes: nothing, because the verified cache
/// already answers it, or one source job's work and the artifacts it reads and verifies after it.
#[derive(Debug)]
pub(crate) enum Preparing {
    Ready(Box<EditorState>),
    Work(SourceWork, Vec<ArtifactRead>),
}

/// What one source job prepared, for [`EditorService::complete_preparation`] to adopt.
pub(crate) enum Prepared {
    File(PreparedFile, Vec<VerifiedArtifact>),
    Develop(RawDevelopment, RawPrepared, Vec<VerifiedArtifact>),
    Artifacts(AssetId, Vec<VerifiedArtifact>),
}

impl SourceWork {
    /// The preparation of the original at `path` as it is now, checked against `target` when the
    /// file is already an asset. Fails when the path is not a regular file.
    pub(crate) fn file(path: &Path, target: Option<FilePreparation>) -> Result<Self, Error> {
        let (path, signature) = EditorService::request_signature(path)?;
        Ok(Self::File {
            path,
            signature,
            target: target.map(Box::new),
        })
    }

    /// Whether the work allocates a RAW's float planes, so it waits for earlier planes to be
    /// released first. Artifact work allocates none.
    pub(crate) fn decodes(&self) -> bool {
        matches!(self, Self::File { .. } | Self::Develop(_))
    }

    /// Whether the work redevelops the cached mosaic, so the development it replaces may be
    /// retained beside it ([`EditorService::evict_development`]).
    pub(crate) fn redevelops(&self) -> bool {
        matches!(self, Self::Develop(_))
    }

    /// Run the work: decode the original and develop a RAW, or redevelop a cached mosaic, and then
    /// read and verify `reads`, stopping at the first that is missing, corrupt or cancelled. Reads
    /// no catalog, so the source worker runs it off the owner thread; the blocking helpers run it
    /// on theirs.
    pub(crate) fn run(
        self,
        reads: &[ArtifactRead],
        cancel: &AtomicBool,
    ) -> Result<Prepared, Error> {
        let read = || -> Result<Vec<VerifiedArtifact>, Error> {
            reads
                .iter()
                .map(|read| artifacts::read_verified(read, cancel))
                .collect()
        };
        match self {
            Self::File {
                path,
                signature,
                target,
            } => {
                let prepared = EditorService::prepare_file(&path, target.as_deref(), cancel)?;
                if prepared.signature != signature {
                    return Err(Error::conflict("source changed after job was queued"));
                }
                Ok(Prepared::File(prepared, read()?))
            }
            Self::Develop(request) => {
                let developed = RawPrepared::develop(
                    request.sensor.clone(),
                    request.capture.clone(),
                    request.fingerprint.clone(),
                    request.gains,
                    cancel,
                )?;
                Ok(Prepared::Develop(*request, developed, read()?))
            }
            Self::Artifacts(asset_id) => Ok(Prepared::Artifacts(asset_id, read()?)),
        }
    }
}

impl Prepared {
    /// The developed RAW planes this preparation allocated, if any, so the source worker can hold
    /// them against its memory gate.
    pub(crate) fn linear_mut(&mut self) -> Option<&mut crate::LinearImage> {
        let raw = match self {
            Self::File(file, _) => match &mut file.source {
                PreparedSource::Raw(raw) => raw,
                PreparedSource::Jpeg(_) => return None,
            },
            Self::Develop(_, raw, _) => raw,
            Self::Artifacts(..) => return None,
        };
        raw.linear.as_mut()
    }
}

impl EditorService {
    pub(crate) fn request_signature(path: &Path) -> Result<(PathBuf, SourceSignature), Error> {
        let (canonical, metadata, signature) = located_signature(path)?;
        if !metadata.is_file() {
            return Err(Error::unsupported_input("expected a regular file"));
        }
        Ok((canonical, signature))
    }

    /// The asset already imported from this file, found by either identity a source has — its
    /// canonical path, or the file's own identity, which a hard link keeps — with its fingerprint.
    fn asset_for_source(
        &self,
        canonical: &Path,
        file_identity: &str,
    ) -> Result<Option<(AssetId, String)>, Error> {
        self.connection
            .query_row(
                "SELECT id,fingerprint FROM assets WHERE canonical_locator=?1 OR file_identity=?2 LIMIT 1",
                params![canonical.to_string_lossy(), file_identity],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(id, fingerprint)| Ok((AssetId::parse(id)?, fingerprint)))
            .transpose()
    }

    /// Read, hash and decode an original from the same bounded, stable read-only file handle, under
    /// a cancellation flag: the one read of an original, which only [`SourceWork::run`] makes. A
    /// known file is checked against `target`, and a known RAW is developed at its entry's gains.
    fn prepare_file(
        path: &Path,
        target: Option<&FilePreparation>,
        cancel: &AtomicBool,
    ) -> Result<PreparedFile, Error> {
        let canonical = canonical_source(path)?;
        let mut file = File::open(&canonical).map_err(file_access)?;
        let handle_before = file.metadata().map_err(file_access)?;
        let path_before = canonical.metadata().map_err(file_access)?;
        let signature = source_signature_for_handle(&canonical, &file, &handle_before);
        if signature != source_signature(&canonical, &path_before) {
            return Err(Error::conflict("source changed before preparation"));
        }
        let bytes = read_bounded_file(&mut file)?;
        let (source, fingerprint) = if bytes.starts_with(&[0xff, 0xd8]) {
            let image = open_source_bytes(bytes)?;
            let fingerprint = image.fingerprint.clone();
            if let Some(target) = target {
                target.verify_fingerprint(&fingerprint)?;
                if target.raw.is_some() {
                    return Err(Error::incompatible(
                        "original source interpretation changed",
                    ));
                }
            }
            (PreparedSource::Jpeg(image), fingerprint)
        } else {
            let fingerprint = format!("{:x}", Sha256::digest(&bytes));
            if let Some(target) = target {
                target.verify_fingerprint(&fingerprint)?;
                if target.raw.is_none() {
                    return Err(Error::incompatible(
                        "original source interpretation changed",
                    ));
                }
            }
            let raw = RawPrepared::decode(
                bytes,
                fingerprint.clone(),
                target.and_then(|target| target.raw.as_ref()),
                cancel,
            )?;
            (PreparedSource::Raw(raw), fingerprint)
        };
        let handle_after = file.metadata().map_err(file_access)?;
        let path_after = canonical.metadata().map_err(file_access)?;
        if signature != source_signature_for_handle(&canonical, &file, &handle_after)
            || signature != source_signature(&canonical, &path_after)
        {
            return Err(Error::conflict("source changed during preparation"));
        }
        Ok(PreparedFile {
            canonical,
            signature,
            source,
            fingerprint,
        })
    }

    fn known_file_preparation(&self, path: &Path) -> Result<Option<FilePreparation>, Error> {
        let (canonical, signature) = Self::request_signature(path)?;
        self.asset_for_source(&canonical, &signature.file_identity)?
            .map(|(id, _)| self.file_preparation(&id, None))
            .transpose()
    }

    fn file_preparation(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<FilePreparation, Error> {
        let (asset, entry) = self.saved_entry(asset_id, entry_id)?;
        FilePreparation::for_recipe(&asset, &entry.snapshot.recipe)
    }

    /// One saved entry of an asset, the current one unless `entry_id` names another, shared from
    /// the entry cache, with the asset it belongs to, its stack admitted as one of that asset's
    /// source kind ([`validate_source_recipe`]). What every question about a saved stack starts
    /// from, an evaluation of it included ([`Self::evaluation`]): a cached head and entry read,
    /// nothing copied but the asset record, and `O(layers)` checks.
    pub(crate) fn saved_entry(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<(AssetRecord, Arc<crate::HistoryEntry>), Error> {
        let head = self.head(asset_id)?;
        let entry = self.shared_entry(asset_id, entry_id.unwrap_or(&head.current))?;
        validate_source_recipe(&self.registry, &head.asset, &entry.snapshot.recipe)?;
        Ok((head.asset, entry))
    }

    /// A repeated import can reuse the one verified immutable decode without a new worker job.
    fn cached_import(&self, path: &Path) -> Result<Option<EditorState>, Error> {
        let (canonical, _, signature) = located_signature(path)?;
        let Some((id, _)) = self.asset_for_source(&canonical, &signature.file_identity)? else {
            return Ok(None);
        };
        let state = self.state(&id)?;
        if state.asset.file_identity != signature.file_identity
            || state.asset.byte_len != signature.byte_len
        {
            return Err(Error::source_unavailable(
                "original source fingerprint changed",
            ));
        }
        let cache = self.source_cache.borrow();
        Ok(cache
            .as_ref()
            .filter(|cached| cached.asset_id == state.asset.id && cached.signature == signature)
            .map(|_| state))
    }

    /// Persisted source interpretation and current in-memory readiness, with no decode or frame work.
    pub(crate) fn inspect_source(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<Value, Error> {
        let (asset, entry) = self.saved_entry(asset_id, entry_id)?;
        let cached = self.cached_state(asset_id)?.is_some();
        let needs_development = match development_gains(&asset, &entry.snapshot.recipe)? {
            Some(gains) => cached && self.raw_development(asset_id, gains)?.is_some(),
            None => false,
        };
        Ok(json!({
            "asset_id": asset_id,
            "entry_id": entry.id,
            "fingerprint": asset.fingerprint,
            "width": asset.width,
            "height": asset.height,
            "source": asset.source,
            "readiness": if !cached { "preparation-required" } else if needs_development { "development-required" } else { "ready" },
        }))
    }

    /// Everything evaluating one stack needs prepared, as a `preparation-required` refusal of it
    /// names it: the asset's original, the development at the gains `stack.developed` asks for,
    /// and the artifacts `stack.recipe` references that are not ready. `O(layers)` plus one file
    /// signature per unready artifact; nothing is read or decoded.
    pub(crate) fn preparation_needs(
        &self,
        stack: Evaluated<'_>,
    ) -> Result<PreparationNeeds, Error> {
        Ok(PreparationNeeds {
            asset_id: stack.asset.id.clone(),
            entry_id: stack.entry_id.clone(),
            gains: development_gains(stack.asset, stack.developed)?,
            artifacts: self.unprepared_artifacts(stack.recipe)?,
        })
    }

    /// What one saved entry's stack needs prepared, or the current one's: what `source.prepare`
    /// queues, and what [`Self::prepare`] prepares.
    pub fn entry_needs(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<PreparationNeeds, Error> {
        let (asset, entry) = self.saved_entry(asset_id, entry_id)?;
        self.preparation_needs(Evaluated::exactly(
            &asset,
            &entry.id,
            &entry.snapshot.recipe,
        ))
    }

    /// `result`, with a `preparation-required` refusal that does not yet say what it needs naming
    /// everything `stack` needs ([`Self::preparation_needs`]). Every service method that evaluates a
    /// stack answers through this, at the point where it knows which stack it evaluated, so the
    /// catalog owner prepares exactly that stack whatever the request did or did not name.
    pub(super) fn needing<T>(
        &self,
        stack: Evaluated<'_>,
        result: Result<T, Error>,
    ) -> Result<T, Error> {
        match result {
            Err(error)
                if error.kind == ErrorKind::PreparationRequired && error.preparation.is_none() =>
            {
                let needs = self.preparation_needs(stack)?;
                Err(error.with_preparation(Preparation::Needs(needs)))
            }
            result => result,
        }
    }

    /// The development an asset's cached RAW source needs to hold `gains`: `None` when its planes
    /// or its second slot already hold them, and when the cache holds no RAW source of this asset,
    /// whose preparation develops it.
    pub(crate) fn raw_development(
        &self,
        asset_id: &AssetId,
        gains: [f32; 3],
    ) -> Result<Option<RawDevelopment>, Error> {
        let state = self.state(asset_id)?;
        let cache = self.source_cache.borrow();
        let Some(cached) = cache.as_ref().filter(|cached| cached.asset_id == *asset_id) else {
            return Ok(None);
        };
        let PreparedSource::Raw(raw) = &cached.source else {
            return Ok(None);
        };
        if (gains == raw.gains && raw.linear.is_some()) || cached.second.holds(gains) {
            return Ok(None);
        }
        Ok(Some(RawDevelopment {
            asset_id: asset_id.clone(),
            signature: cached.signature.clone(),
            fingerprint: state.asset.fingerprint,
            sensor: raw.sensor.clone(),
            capture: raw.capture.clone(),
            gains,
            file_name: state
                .asset
                .locator
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
        }))
    }

    /// Release the cache's float planes before the sole source worker allocates another development.
    /// When `retain`, because the work is a redevelopment of this same mosaic, the most recently
    /// used development stays in the second slot, within its byte budget and exempt from the
    /// memory gate ([`SecondDevelopment::evict`]); every other one is dropped. Preview jobs may
    /// still hold the old Arc; the worker's memory gate waits for those to finish.
    pub(crate) fn evict_development(&self, retain: bool) {
        if let Some(cached) = self.source_cache.borrow_mut().as_mut()
            && let PreparedSource::Raw(raw) = &mut cached.source
        {
            cached.second.evict(raw.gains, &mut raw.linear, retain);
        }
    }

    fn install_development(
        &self,
        request: &RawDevelopment,
        developed: RawPrepared,
    ) -> Result<EditorState, Error> {
        let state = self.state(&request.asset_id)?;
        if state.asset.fingerprint != request.fingerprint
            || developed.gains != request.gains
            || developed
                .linear
                .as_ref()
                .is_none_or(|image| image.fingerprint() != request.fingerprint)
        {
            return Err(Error::conflict("RAW development identity changed"));
        }
        let current = Self::request_signature(&state.asset.locator)?.1;
        if current != request.signature {
            return Err(Error::source_unavailable(
                "original source changed during RAW development",
            ));
        }
        let mut cache = self.source_cache.borrow_mut();
        let Some(cached) = cache.as_mut().filter(|cached| {
            cached.asset_id == request.asset_id && cached.signature == request.signature
        }) else {
            return Err(Error::conflict(
                "RAW source cache was replaced during development",
            ));
        };
        let PreparedSource::Raw(previous) = &mut cached.source else {
            return Err(Error::incompatible(
                "RAW development targeted a JPEG source",
            ));
        };
        if !Arc::ptr_eq(&previous.sensor, &request.sensor) {
            return Err(Error::conflict("RAW mosaic changed during development"));
        }
        // A development read back from the second slot while this one was built is the less
        // recently used now, and waits there in turn.
        let replaced = previous.linear.take();
        cached
            .second
            .keep(previous.gains, replaced, developed.linear.as_ref());
        cached.source = PreparedSource::Raw(developed);
        Ok(state)
    }

    pub(crate) fn cached_state(&self, asset_id: &AssetId) -> Result<Option<EditorState>, Error> {
        let state = self.state(asset_id)?;
        let signature = self.original_signature(&state.asset)?;
        let cache = self.source_cache.borrow();
        Ok(cache
            .as_ref()
            .filter(|cached| cached.asset_id == state.asset.id && cached.signature == signature)
            .map(|_| state))
    }

    /// What importing `path` takes: nothing when the verified cache already holds this file, and
    /// otherwise the preparation of the file, checked against the asset it already is, if any.
    /// What `catalog.import` queues, and what [`Self::import`] runs.
    pub(crate) fn importing(&self, path: &Path) -> Result<Preparing, Error> {
        if let Some(state) = self.cached_import(path)? {
            return Ok(Preparing::Ready(Box::new(state)));
        }
        let target = self.known_file_preparation(path)?;
        Ok(Preparing::Work(SourceWork::file(path, target)?, Vec::new()))
    }

    /// What preparing exactly what `needs` names takes — the asset's original, its RAW development
    /// at the named gains and the named artifacts — and nothing re-derived from the request that
    /// was refused. An original the cache does not hold is prepared and developed at the entry's
    /// gains in the same job; one it holds is redeveloped only when its planes do not hold them;
    /// artifacts that became ready since they were named are left out. Needs with nothing left to
    /// prepare are ready at once. What `source.prepare` and a refused evaluation queue, and what
    /// [`Self::prepare`] runs.
    pub(crate) fn preparation(&self, needs: &PreparationNeeds) -> Result<Preparing, Error> {
        let asset_id = &needs.asset_id;
        let reads = self.artifact_reads(&needs.artifacts)?;
        let Some(state) = self.cached_state(asset_id)? else {
            let state = self.state(asset_id)?;
            let target = self.file_preparation(asset_id, Some(&needs.entry_id))?;
            let work = SourceWork::file(&state.asset.locator, Some(target))?;
            return Ok(Preparing::Work(work, reads));
        };
        let development = match needs.gains {
            Some(gains) => self.raw_development(asset_id, gains)?,
            None => None,
        };
        Ok(match development {
            Some(request) => Preparing::Work(SourceWork::Develop(Box::new(request)), reads),
            None if reads.is_empty() => Preparing::Ready(Box::new(state)),
            None => Preparing::Work(SourceWork::Artifacts(state.asset.id), reads),
        })
    }

    /// Import `path` on the caller's thread, blocking: the catalog owner's own import
    /// ([`Self::importing`]), its source job's own work ([`SourceWork::run`]) and its own
    /// completion ([`Self::complete_preparation`]), in turn. For tests and the harness; the catalog
    /// owner queues the work on its source worker instead.
    pub fn import(&mut self, path: &Path) -> Result<EditorState, Error> {
        let preparing = self.importing(path)?;
        self.run_preparation(preparing)
    }

    /// Prepare exactly what `needs` names on the caller's thread, blocking: the catalog owner's own
    /// planning ([`Self::preparation`]), its source job's own work ([`SourceWork::run`]) and its own
    /// completion ([`Self::complete_preparation`]), in turn, so a reopened RAW develops at its
    /// entry's own gains exactly as it does on the owner. `needs` is what a
    /// `preparation-required` refusal names (`Error::needs`) or what [`Self::entry_needs`]
    /// answers. For tests and the harness; the catalog owner queues the work on its source worker
    /// instead.
    pub fn prepare(&mut self, needs: &PreparationNeeds) -> Result<EditorState, Error> {
        let preparing = self.preparation(needs)?;
        self.run_preparation(preparing)
    }

    fn run_preparation(&mut self, preparing: Preparing) -> Result<EditorState, Error> {
        let (work, reads) = match preparing {
            Preparing::Ready(state) => return Ok(*state),
            Preparing::Work(work, reads) => (work, reads),
        };
        // As on the owner: the cache's float planes go before another development is allocated.
        if work.decodes() {
            self.evict_development(work.redevelops());
        }
        let prepared = work.run(&reads, &AtomicBool::new(false))?;
        self.complete_preparation(prepared).map(|(state, _)| state)
    }

    /// Complete one source job's preparation, on the thread that owns the catalog: commit an
    /// import, or adopt a new original, a development and the verified artifacts into the caches.
    /// The one completion, which the catalog owner runs for each source job and the blocking
    /// helpers run after the same work. The bool says whether a new asset was inserted, for event
    /// publication.
    pub(crate) fn complete_preparation(
        &mut self,
        prepared: Prepared,
    ) -> Result<(EditorState, bool), Error> {
        let (completed, verified) = match prepared {
            Prepared::File(file, verified) => (self.import_prepared(file)?, verified),
            Prepared::Develop(request, developed, verified) => (
                (self.install_development(&request, developed)?, false),
                verified,
            ),
            Prepared::Artifacts(asset_id, verified) => ((self.state(&asset_id)?, false), verified),
        };
        self.adopt_artifacts(verified);
        Ok(completed)
    }

    /// Complete an import only after a worker has verified and decoded its exact source bytes.
    /// The bool says whether a new asset was inserted for event publication.
    fn import_prepared(&mut self, prepared: PreparedFile) -> Result<(EditorState, bool), Error> {
        let PreparedFile {
            canonical,
            signature,
            source,
            fingerprint,
        } = prepared;
        let current = canonical
            .metadata()
            .map_err(|e| Error::source_unavailable(e.kind().to_string()))?;
        if source_signature(&canonical, &current) != signature {
            return Err(Error::conflict("source changed before import completion"));
        }
        let identity = signature.file_identity.clone();
        let canonical_text = canonical.to_string_lossy().into_owned();
        if let Some((existing, _)) = self.asset_for_source(&canonical, &identity)? {
            let state = self.state(&existing)?;
            if state.asset.fingerprint != fingerprint {
                return Err(Error::source_unavailable(
                    "original source fingerprint changed",
                ));
            }
            if state.asset.source != SourceKind::of(&source)? {
                return Err(Error::incompatible(
                    "original source interpretation changed",
                ));
            }
            // The original was just read and verified where the catalog looks for it, so it is
            // available, whatever an earlier check or refusal observed. An observation that cannot
            // be recorded is left to the next look; the preparation stands.
            if state.asset.locator == canonical {
                let _ = availability::observed(
                    &self.connection,
                    &state.asset.id,
                    crate::catalog_types::FileAvailability::Available,
                    now_ms(),
                );
            }
            self.source_cache.replace(Some(CachedSource {
                asset_id: state.asset.id.clone(),
                signature,
                source,
                second: SecondDevelopment::default(),
            }));
            return Ok((state, false));
        }
        let (width, height) = source.dimensions();
        let source_kind = SourceKind::of(&source)?;
        let asset = AssetRecord {
            id: AssetId::new(),
            source_root: canonical.parent().unwrap_or(Path::new("")).to_path_buf(),
            locator: canonical.clone(),
            fingerprint: fingerprint.clone(),
            file_identity: identity,
            byte_len: signature.byte_len,
            width,
            height,
            source: source_kind,
        };
        let mut snapshot = Snapshot::original(asset.id.clone());
        // A source with RAW metadata is exactly the source `SourceKind::of` reads as a RAW.
        if let Some(metadata) = source.metadata() {
            snapshot = snapshot.with_layer_inserted(
                0,
                crate::RawPayload::for_as_shot(metadata.as_shot_gains, metadata.cam_xyz)?
                    .layer(LayerId::new()),
            )?;
        }
        let mut entry = HistoryEntry {
            id: EntryId::new(),
            asset_id: asset.id.clone(),
            sequence: 0,
            action_id: "original".into(),
            label: "Original".into(),
            parameters: json!({}),
            actor: "system".into(),
            timestamp_ms: now_ms(),
            request_id: None,
            base_revision: 0,
            result_revision: 0,
            snapshot,
            undo_parent: None,
            restore_target: None,
        };
        // An import is its own write, because it creates the asset a head would name, but it admits
        // its Original exactly as a commit admits the stack it writes.
        self.admit(&asset, &mut entry.snapshot.recipe)?;
        // Until picks are developed into the folders a person chooses (`pick.develop`, which
        // replaces this import), one imported file goes into the top-level catalog folder named
        // after its folder on disk, made when there is none, and records the volume it is on and
        // what the import knows of its capture: its stored size and orientation.
        let now = entry.timestamp_ms;
        let volume = crate::index::volume_of(&canonical, now)?;
        let source_folder = asset.source_root.clone();
        let folder_name = source_folder.file_name().map_or_else(
            || "Imported".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let file_name = canonical
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let header = imported_header(&source, width, height);
        let artifact_root = &self.artifact_root;
        write(&mut self.connection, |tx| {
            upsert_volume(tx, &volume)?;
            let catalog_folder_id = top_level_folder(tx, &folder_name, now)?;
            let row = insert_asset(
                tx,
                &NewAsset {
                    record: &asset,
                    canonical_locator: &canonical_text,
                    catalog_folder_id: &catalog_folder_id,
                    source_folder: &source_folder,
                    volume_id: &volume.id,
                    file_name: &file_name,
                    developed_ms: now,
                    removed_ms: None,
                    availability: crate::catalog_types::FileAvailability::Available,
                    checked_ms: now,
                    develop_moment: None,
                },
            )?;
            insert_capture(tx, row, &header, None)?;
            insert_entry(tx, artifact_root, &entry)?;
            tx.execute(
                "INSERT INTO asset_state VALUES (?1,?2,0,'[]')",
                params![asset.id.as_str(), entry.id.as_str()],
            )?;
            Ok(())
        })?;
        self.source_cache.replace(Some(CachedSource {
            asset_id: asset.id.clone(),
            signature,
            source,
            second: SecondDevelopment::default(),
        }));
        Ok((
            EditorState {
                asset,
                revision: 0,
                current_entry: entry,
                redo: Vec::new(),
            },
            true,
        ))
    }

    /// The asset's prepared original from the verified cache, once its file still has the
    /// signature it was prepared under, holding the development of `recipe`'s white balance when
    /// the second slot has it ([`SecondDevelopment::take_up`]). Nothing is read here: a cache miss
    /// is `preparation-required`, which the evaluation that asked names with everything its stack
    /// needs ([`Self::needing`]), and a source job prepares ([`SourceWork`]).
    pub(super) fn verified_prepared(
        &self,
        asset: &AssetRecord,
        recipe: &crate::Recipe,
    ) -> Result<PreparedSource, Error> {
        let signature = self.original_signature(asset)?;
        let raw_source = matches!(&asset.source, SourceKind::Raw { .. });
        let max_source_bytes = if raw_source {
            luxforge_raw::MAX_SOURCE_BYTES as u64
        } else {
            crate::source::MAX_JPEG_BYTES as u64
        };
        if signature.byte_len > max_source_bytes {
            return Err(Error::source_unavailable(
                "original source fingerprint changed",
            ));
        }
        if let Some(cached) = self.source_cache.borrow_mut().as_mut()
            && cached.asset_id == asset.id
            && cached.signature == signature
        {
            // Which development a stack reads is decided where it is evaluated; an unreadable
            // development layer is refused there, so here it only leaves the cache as it is.
            if let PreparedSource::Raw(raw) = &mut cached.source
                && let Ok(Some(gains)) = development_gains(asset, recipe)
            {
                cached
                    .second
                    .take_up(&mut raw.gains, &mut raw.linear, gains);
            }
            return Ok(cached.source.clone());
        }
        Err(Error::preparation_required("source preparation required"))
    }
}

/// What the single-file import knows of a file's header: the original's stored size (its decoded
/// size, turned back by its orientation) and its EXIF orientation. The typed header read replaces
/// this with everything a header says.
fn imported_header(source: &PreparedSource, width: u32, height: u32) -> HeaderMetadata {
    let orientation = match source {
        PreparedSource::Jpeg(image) => ExifOrientation::new(image.orientation),
        PreparedSource::Raw(_) => source
            .metadata()
            .and_then(|metadata| ExifOrientation::new(metadata.exif_orientation)),
    };
    let transposed = orientation.is_some_and(ExifOrientation::transposes);
    HeaderMetadata {
        dimensions: Some(if transposed {
            Dimensions {
                width: height,
                height: width,
            }
        } else {
            Dimensions { width, height }
        }),
        orientation,
        ..HeaderMetadata::default()
    }
}

/// Admit `recipe` as a stack of `asset`'s source kind.
///
/// Every layer's effect must list the asset's kind among its declared `sources`
/// ([`crate::EffectDescriptor::applies_to`]); a layer of an effect no provider declares is left to
/// the registry's own refusal, which reports it as an unavailable edit. A RAW stack also keeps the
/// RAW source's own invariants, which are not a matter of declaration: exactly one development
/// layer at index zero, whose calibration equals the original's. `O(layers)`, no allocation.
pub(super) fn validate_source_recipe(
    registry: &crate::ModuleRegistry,
    asset: &AssetRecord,
    recipe: &crate::Recipe,
) -> Result<(), Error> {
    let kind = asset.source.tag();
    for layer in &recipe.layers {
        if let Some((module, effect)) = registry.effect(&layer.effect_id)
            && !effect.applies_to(kind)
        {
            return Err(module
                .descriptor()
                .not_applicable_refusal(crate::ErrorKind::Incompatible, kind));
        }
    }
    reject_superseded_fields(registry, kind, recipe)?;
    if let SourceKind::Raw { ref metadata } = asset.source {
        let payload = raw_payload(recipe)?;
        if payload.as_shot_gains != metadata.as_shot_gains || payload.cam_xyz != metadata.cam_xyz {
            return Err(Error::incompatible(
                "RAW source layer calibration differs from original",
            ));
        }
    }
    Ok(())
}

/// Refuse a global layer holding a field another module's control variant supersedes on a photo of
/// `kind` ([`crate::ModuleRegistry::superseded`]): on a RAW photo the global white balance lives
/// only in the source development, so a global Basic layer with a temperature or tint is data no
/// commit could have written. A field holds a value when the module's own `values` for the layer
/// differ from the parameter's declared default; a masked layer is a mask's target, where nothing
/// is superseded. `O(controls + layers × fields)`; nothing is parsed when no variant names `kind`.
fn reject_superseded_fields(
    registry: &crate::ModuleRegistry,
    kind: crate::SourceTag,
    recipe: &crate::Recipe,
) -> Result<(), Error> {
    let superseded: Vec<_> = registry
        .superseded()
        .into_iter()
        .filter(|field| field.source == kind)
        .collect();
    if superseded.is_empty() {
        return Ok(());
    }
    for layer in recipe.layers.iter().filter(|layer| layer.mask.is_none()) {
        let Some((module, _)) = registry.effect(&layer.effect_id) else {
            continue;
        };
        let descriptor = module.descriptor();
        let mut values = None;
        for field in &superseded {
            let Some(parameter) = descriptor
                .action(field.action)
                .and_then(|action| action.parameter(field.parameter))
            else {
                continue;
            };
            let values = match &values {
                Some(values) => values,
                None => values.insert(
                    module
                        .describe(&layer.effect_id, layer.effect_format, &layer.payload)?
                        .values,
                ),
            };
            if let Some(value) = values.get(field.parameter)
                && Some(value) != parameter.default.as_ref()
            {
                return Err(registry.superseded_error(crate::ErrorKind::Incompatible, field));
            }
        }
    }
    Ok(())
}

/// Whether an evaluation may approximate a RAW white balance the developed planes do not hold.
///
/// [`Self::DraftPreview`] is decided in exactly one place, the evaluation builder
/// ([`EditorService::evaluation`]), for the preview of an open draft; `cargo xtask
/// check-repository` keeps it there. Every other evaluation — a committed or historical preview,
/// `render_entry` and every export, `sample_entry` and `sample_draft` and so the readout and
/// `render.sample`, `analysis_plan`, and every pixel a plan or query samples from its stage
/// context — is [`Self::Strict`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RawSettingsMode {
    /// The developed planes must hold the recipe's white balance; anything else is
    /// `preparation-required`, and the mosaic is redeveloped for it.
    Strict,
    /// A white balance the planes do not hold is approximated on them by
    /// [`crate::WhiteBalanceApproximation`], and the frame is labelled approximate. Used only for
    /// the preview of an open draft, so the drag shows something before its release redevelops.
    DraftPreview,
}

pub(super) fn raw_settings(
    raw: &RawPrepared,
    recipe: &crate::Recipe,
    mode: RawSettingsMode,
) -> Result<crate::LinearSettings, Error> {
    let payload = raw_payload(recipe)?;
    let metadata = raw.sensor.metadata();
    resolve_raw_settings(
        &payload,
        metadata.as_shot_gains,
        metadata.rgb_cam,
        raw.gains,
        raw.linear.is_some(),
        mode,
    )
}

/// The linear settings one RAW recipe asks for over planes developed at `developed` gains, which
/// exist when `developed_present`. `rgb_cam` is the camera-to-linear-sRGB matrix the development
/// applied; its fourth column is validated zero at decode and is not read.
///
/// Planes that hold the recipe's gains need no approximation: the settings are exactly the ones a
/// committed render uses. Planes at other gains are `preparation-required` under
/// [`RawSettingsMode::Strict`]; under [`RawSettingsMode::DraftPreview`] they carry the matrix that
/// approximates the recipe's gains on them, and a camera matrix with no usable inverse stays
/// `preparation-required` rather than rendering a frame the matrix cannot describe. Missing planes
/// are `preparation-required` in both modes. `O(1)`: it reads no pixel.
fn resolve_raw_settings(
    payload: &crate::RawPayload,
    as_shot_gains: [f32; 3],
    rgb_cam: [[f32; 4]; 3],
    developed: [f32; 3],
    developed_present: bool,
    mode: RawSettingsMode,
) -> Result<crate::LinearSettings, Error> {
    let gains = development_gains_of(payload, as_shot_gains);
    let required = |detail: String| Error::preparation_required(detail);
    if !developed_present {
        return Err(required("RAW white balance development required".into()));
    }
    let white_balance = if gains == developed {
        None
    } else {
        match mode {
            RawSettingsMode::Strict => {
                return Err(required("RAW white balance development required".into()));
            }
            RawSettingsMode::DraftPreview => {
                let camera_to_srgb =
                    rgb_cam.map(|row| [f64::from(row[0]), f64::from(row[1]), f64::from(row[2])]);
                let approximation =
                    crate::WhiteBalanceApproximation::between(camera_to_srgb, developed, gains)
                        .map_err(|error| {
                            required(format!(
                                "RAW white balance development required: {}",
                                error.detail
                            ))
                        })?;
                Some(approximation)
            }
        }
    };
    Ok(crate::LinearSettings { white_balance })
}

/// The sensor gains one RAW development develops at: the camera's as-shot gains under As shot and
/// the payload's own otherwise. The one place the host resolves the white-balance mode.
fn development_gains_of(payload: &crate::RawPayload, as_shot_gains: [f32; 3]) -> [f32; 3] {
    match payload.wb_mode {
        crate::WhiteBalanceMode::AsShot => as_shot_gains,
        crate::WhiteBalanceMode::Custom => payload.gains,
    }
}

/// One stack an evaluation reads, which is what a `preparation-required` refusal of that
/// evaluation names ([`EditorService::needing`]).
#[derive(Clone, Copy)]
pub(crate) struct Evaluated<'a> {
    pub(crate) asset: &'a AssetRecord,
    /// The saved entry evaluated, or the one a draft or a change was planned over.
    pub(crate) entry_id: &'a EntryId,
    /// The stack evaluated, whose artifacts must be ready.
    pub(crate) recipe: &'a crate::Recipe,
    /// The stack whose white balance a RAW development must hold: `recipe` itself, except for a
    /// drafted preview, which approximates on the development its entry holds.
    pub(crate) developed: &'a crate::Recipe,
}

impl<'a> Evaluated<'a> {
    /// A stack evaluated exactly, which needs a development at its own white balance.
    pub(crate) fn exactly(
        asset: &'a AssetRecord,
        entry_id: &'a EntryId,
        recipe: &'a crate::Recipe,
    ) -> Self {
        Self {
            asset,
            entry_id,
            recipe,
            developed: recipe,
        }
    }
}

/// The sensor gains a RAW asset's stack develops at — the camera's as-shot gains under As shot and
/// the payload's own otherwise — or `None` for a JPEG, which has no development.
fn development_gains(
    asset: &AssetRecord,
    recipe: &crate::Recipe,
) -> Result<Option<[f32; 3]>, Error> {
    match &asset.source {
        SourceKind::Jpeg => Ok(None),
        SourceKind::Raw { metadata } => raw_gains(metadata, recipe).map(Some),
    }
}

/// The sensor gains a RAW stack develops at, read from its source layer.
fn raw_gains(metadata: &RawInterpretation, recipe: &crate::Recipe) -> Result<[f32; 3], Error> {
    let payload = raw_payload(recipe)?;
    Ok(development_gains_of(&payload, metadata.as_shot_gains))
}

fn raw_payload(recipe: &crate::Recipe) -> Result<crate::RawPayload, Error> {
    let Some(layer) = recipe.layers.first() else {
        return Err(Error::incompatible(
            "RAW recipe is missing its required source layer",
        ));
    };
    if !crate::modules::is_raw_development(layer)
        || recipe
            .layers
            .iter()
            .skip(1)
            .any(crate::modules::is_raw_development)
    {
        return Err(Error::incompatible(
            "RAW recipe needs exactly one source layer at index zero",
        ));
    }
    crate::RawPayload::from_layer(layer)
}

#[cfg(not(windows))]
pub(crate) fn source_signature(path: &Path, metadata: &Metadata) -> SourceSignature {
    SourceSignature {
        byte_len: metadata.len(),
        modified: metadata.modified().ok(),
        file_identity: file_identity(metadata, path),
        change_marker: metadata_change_marker(metadata),
    }
}

#[cfg(windows)]
pub(crate) fn source_signature(path: &Path, metadata: &Metadata) -> SourceSignature {
    let file = File::open(path).ok();
    windows_source_signature(path, metadata, file.as_ref())
}

#[cfg(windows)]
fn windows_source_signature(
    path: &Path,
    metadata: &Metadata,
    file: Option<&File>,
) -> SourceSignature {
    use std::os::windows::fs::MetadataExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    // If the filesystem cannot provide change time, no two reads may compare equal: a cached
    // decoded source must not survive a same-length, same-timestamp overwrite.
    static MISSING_CHANGE_TIME: AtomicU64 = AtomicU64::new(0);
    let change_time = file
        .and_then(|file| luxforge_process::file_change_time(file).ok())
        .map_or_else(
            || -1 - i128::from(MISSING_CHANGE_TIME.fetch_add(1, Ordering::Relaxed)),
            i128::from,
        );
    SourceSignature {
        byte_len: metadata.len(),
        modified: metadata.modified().ok(),
        file_identity: file
            .and_then(windows_file_identity)
            .unwrap_or_else(|| format!("path:{}", path.to_string_lossy().to_lowercase())),
        change_marker: Some((i128::from(metadata.last_write_time()), change_time)),
    }
}

/// Use the already-open file's identity when checking the bytes read from that handle. A path
/// may be replaced between reads, so looking up its identity again would miss that replacement.
#[cfg(windows)]
pub(crate) fn source_signature_for_handle(
    path: &Path,
    file: &File,
    metadata: &Metadata,
) -> SourceSignature {
    windows_source_signature(path, metadata, Some(file))
}

#[cfg(not(windows))]
pub(crate) fn source_signature_for_handle(
    path: &Path,
    _: &File,
    metadata: &Metadata,
) -> SourceSignature {
    source_signature(path, metadata)
}

#[cfg(unix)]
fn metadata_change_marker(metadata: &Metadata) -> Option<(i128, i128)> {
    use std::os::unix::fs::MetadataExt;
    Some((
        i128::from(metadata.ctime()),
        i128::from(metadata.ctime_nsec()),
    ))
}

#[cfg(not(any(unix, windows)))]
fn metadata_change_marker(_: &Metadata) -> Option<(i128, i128)> {
    None
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata, _: &Path) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("unix:{}:{}", metadata.dev(), metadata.ino())
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> Option<String> {
    let info = winapi_util::file::information(file).ok()?;
    Some(format!(
        "windows:{}:{}",
        info.volume_serial_number(),
        info.file_index()
    ))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_: &Metadata, canonical: &Path) -> String {
    format!("path:{}", canonical.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModuleRegistry;
    use crate::editor::{
        AnalysisSelection, MutationOutcome,
        test_support::{fixture, mutation, synthetic_raw_metadata, temp},
    };
    use crate::{Draft, PreviewSource, render::testing::render};
    use serde_json::Map;

    #[test]
    fn known_file_preparation_checks_bytes_and_source_kind() {
        let path = fixture();
        let never = AtomicBool::new(false);
        let prepared = EditorService::prepare_file(&path, None, &never).unwrap();
        let target = FilePreparation {
            fingerprint: prepared.fingerprint.clone(),
            raw: None,
        };
        EditorService::prepare_file(&path, Some(&target), &never).expect("matching JPEG target");
        let wrong = FilePreparation {
            fingerprint: "different bytes".into(),
            ..target
        };
        assert_eq!(
            EditorService::prepare_file(&path, Some(&wrong), &never)
                .unwrap_err()
                .kind,
            ErrorKind::SourceUnavailable
        );
    }

    #[test]
    fn raw_interpretation_json_roundtrip_is_strict_at_native_precision() {
        use luxforge_raw::RawMode;
        let metadata = synthetic_raw_metadata();
        // A row read parses the stored text into the type once, here, as `into_record` does.
        let read = |text: String| -> Result<SourceKind, Error> {
            crate::editor::decode("stored source interpretation", text)
        };
        let fresh = SourceKind::Raw {
            metadata: RawInterpretation::new(metadata.clone()).unwrap(),
        };
        // The catalog and the API spell each f32 as its shortest decimal; a JSON value holds its
        // exact f64 widening instead. Both read back to the same f32s, because the comparison is
        // the type's own and not the text's.
        let shortest = serde_json::to_string(&fresh).unwrap();
        let spelled = serde_json::to_value(&fresh).unwrap();
        assert_ne!(shortest, spelled.to_string(), "the two spellings differ");
        let stored = read(shortest).unwrap();
        assert_eq!(stored, fresh);
        assert_eq!(read(spelled.to_string()).unwrap(), fresh);
        for field in ["backend", "default_crop", "cam_xyz"] {
            let mut changed = spelled.clone();
            let metadata = &mut changed["metadata"];
            match field {
                "backend" => metadata["backend"] = json!("other backend"),
                "default_crop" => metadata["default_crop"]["x"] = json!(1),
                "cam_xyz" => metadata["cam_xyz"][0][0] = json!(0.5),
                _ => unreachable!(),
            }
            assert_ne!(read(changed.to_string()).unwrap(), stored, "{field}");
        }
        let mut unknown = spelled.clone();
        unknown["metadata"]["unexpected"] = json!(true);
        assert_eq!(
            read(unknown.to_string()).unwrap_err().kind,
            ErrorKind::Incompatible
        );
        assert!(spelled["metadata"].get("dng_corrections").is_none());
        let mut dng = metadata.clone();
        dng.mode = RawMode::from_id("DjiAir2sDng16").unwrap();
        dng.dng_corrections = Some(luxforge_raw::DngCorrectionMetadata {
            interpretation: "test-stage3-v1".into(),
            applied: [9_u32, 1]
                .map(|id| luxforge_raw::DngOpcodeProvenance {
                    list: 51022,
                    id,
                    version: 0x0103_0000,
                    flags: 0,
                    payload_sha256: format!("{id:064x}"),
                })
                .to_vec(),
            skipped_optional: vec![],
            calibration: luxforge_raw::DngCalibrationMetadata {
                illuminants: [17, 21],
                color_matrix1_sha256: "1".repeat(64),
                color_matrix2_sha256: "2".repeat(64),
                selected: "ColorMatrix2-D65-fixed-XYZ-to-camera".into(),
            },
        });
        let dng_kind = SourceKind::Raw {
            metadata: RawInterpretation::new(dng.clone()).unwrap(),
        };
        let dng_spelled = serde_json::to_value(&dng_kind).unwrap();
        let dng_stored = read(serde_json::to_string(&dng_kind).unwrap()).unwrap();
        assert_eq!(dng_stored, dng_kind);
        let mut changed_correction = dng_spelled.clone();
        changed_correction["metadata"]["dng_corrections"]["applied"][0]["payload_sha256"] =
            json!("changed");
        assert_ne!(read(changed_correction.to_string()).unwrap(), dng_stored);
        // A mode that needs its correction record refuses a row without one, spelled null or left
        // out, when the row is read.
        let mut missing_correction = dng_spelled.clone();
        missing_correction["metadata"]["dng_corrections"] = Value::Null;
        let mut absent_correction = dng_spelled.clone();
        absent_correction["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("dng_corrections");
        for refused in [missing_correction, absent_correction] {
            assert_eq!(
                read(refused.to_string()).unwrap_err().kind,
                ErrorKind::Incompatible
            );
        }
        assert_ne!(stored, SourceKind::Jpeg);
        // And a mode that takes none refuses a record where it enters, so no asset holds one.
        let mut mismatched = metadata.clone();
        mismatched.dng_corrections = dng.dng_corrections.clone();
        assert_eq!(
            RawInterpretation::new(mismatched).unwrap_err().kind,
            ErrorKind::Incompatible
        );
        let mut mismatched = spelled.clone();
        mismatched["metadata"]["dng_corrections"] =
            dng_spelled["metadata"]["dng_corrections"].clone();
        assert_eq!(
            read(mismatched.to_string()).unwrap_err().kind,
            ErrorKind::Incompatible
        );

        let asset = AssetRecord {
            id: AssetId::new(),
            source_root: PathBuf::new(),
            locator: PathBuf::from("test.nef"),
            fingerprint: "test".into(),
            file_identity: "test".into(),
            byte_len: 1,
            width: 32,
            height: 32,
            source: stored,
        };
        let payload =
            crate::RawPayload::for_as_shot(metadata.as_shot_gains, metadata.cam_xyz).unwrap();
        let layer_id = LayerId::new();
        let snapshot = Snapshot::original(asset.id.clone())
            .with_layer_inserted(0, payload.layer(layer_id.clone()))
            .unwrap();
        let registry = crate::ModuleRegistry::builtin();
        validate_source_recipe(&registry, &asset, &snapshot.recipe).unwrap();
        // The same stack on a JPEG is refused by the effect's declared sources, worded from its
        // module's descriptor rather than from its identity.
        let jpeg = AssetRecord {
            source: SourceKind::Jpeg,
            ..asset.clone()
        };
        let (module, _) = registry
            .effect(&snapshot.recipe.layers[0].effect_id)
            .expect("the development's provider");
        let refused = validate_source_recipe(&registry, &jpeg, &snapshot.recipe).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Incompatible);
        assert_eq!(
            refused.detail,
            format!(
                "{} does not apply to a JPEG photo",
                module.descriptor().title
            )
        );
        assert_eq!(
            refused.data.as_deref(),
            Some(&json!({"source": "jpeg", "module_id": module.descriptor().id})),
            "the kind and the module are data, not only prose"
        );
        for calibration in ["as_shot_gains", "cam_xyz"] {
            let mut corrupted = payload.clone();
            if calibration == "as_shot_gains" {
                // As shot is canonical, so the development's gains are the as-shot ones.
                corrupted.as_shot_gains[0] += 0.1;
                corrupted.gains = corrupted.as_shot_gains;
            } else {
                corrupted.cam_xyz[0][0] += 0.1;
            }
            let mut recipe = snapshot.recipe.clone();
            recipe.layers[0] = corrupted.layer(layer_id.clone());
            assert_eq!(
                validate_source_recipe(&registry, &asset, &recipe)
                    .unwrap_err()
                    .kind,
                ErrorKind::Incompatible,
                "{calibration}"
            );
        }

        // On a RAW photo the global white balance is the development's alone: a global Basic
        // layer holding a temperature or tint is refused, by the rule the variants derive, while
        // a global exposure and a masked white balance are admitted. The same layers on a JPEG
        // are Basic's own.
        let basic = |payload: Value, mask: Option<crate::MaskId>| crate::Layer {
            id: LayerId::new(),
            effect_id: crate::BASIC_EFFECT.into(),
            effect_format: crate::EFFECT_FORMAT,
            payload,
            mask,
            artifacts: Vec::new(),
        };
        let with = |layer: crate::Layer| {
            let mut recipe = snapshot.recipe.clone();
            recipe.layers.push(layer);
            recipe
        };
        for (payload, detail, by) in [
            (
                json!({"temperature": 12.0}),
                "on a RAW photo, Temperature is the source development's: set-raw temperature (K)",
                ("set-basic.temperature", "set-raw.temperature"),
            ),
            (
                json!({"tint": -3.0, "exposure": 0.5}),
                "on a RAW photo, Tint is the source development's: set-raw tint",
                ("set-basic.tint", "set-raw.tint"),
            ),
        ] {
            let refused =
                validate_source_recipe(&registry, &asset, &with(basic(payload.clone(), None)))
                    .unwrap_err();
            assert_eq!(refused.kind, ErrorKind::Incompatible, "{payload}");
            assert_eq!(refused.detail, detail, "{payload}");
            assert_eq!(
                refused.data.as_deref(),
                Some(&json!({"source": "raw", "field": by.0, "by": by.1})),
                "{payload}"
            );
            let mut on_jpeg = with(basic(payload.clone(), None));
            on_jpeg.layers.remove(0);
            validate_source_recipe(&registry, &jpeg, &on_jpeg).expect("Basic's own on a JPEG");
        }
        for admitted in [
            with(basic(json!({"exposure": 1.25, "contrast": 10.0}), None)),
            // A field spelled at its default holds nothing.
            with(basic(json!({"temperature": 0.0, "exposure": 1.0}), None)),
            with(basic(
                json!({"temperature": 25.0, "tint": 5.0}),
                Some(crate::MaskId::new()),
            )),
        ] {
            validate_source_recipe(&registry, &asset, &admitted).expect("admitted");
        }
    }

    /// A camera matrix with rows summing to one and strong cross terms, as a real `rgb_cam` has.
    const RGB_CAM: [[f32; 4]; 3] = [
        [1.72, -0.61, -0.11, 0.0],
        [-0.18, 1.49, -0.31, 0.0],
        [0.04, -0.52, 1.48, 0.0],
    ];

    fn custom(gains: [f32; 3]) -> crate::RawPayload {
        crate::RawPayload {
            wb_mode: crate::WhiteBalanceMode::Custom,
            gains,
            as_shot_gains: [2.0, 1.0, 1.5],
            ..crate::RawPayload::default()
        }
    }

    /// Planes that hold the recipe's white balance are evaluated exactly in both modes. Planes at
    /// another white balance are `preparation-required` when strict, and approximated by
    /// `R · diag(g'/g) · R⁻¹` only for a drafted preview. Missing planes, or a camera matrix with no
    /// inverse, are `preparation-required` in both modes: never a silent wrong frame.
    #[test]
    fn only_a_drafted_preview_approximates_a_white_balance_the_planes_do_not_hold() {
        use RawSettingsMode::{DraftPreview, Strict};
        let developed = [2.0_f32, 1.0, 1.5];
        let target = [1.6_f32, 1.0, 2.2];
        let camera = RGB_CAM.map(|row| [row[0], row[1], row[2]].map(f64::from));
        for mode in [Strict, DraftPreview] {
            let held = resolve_raw_settings(
                &custom(developed),
                developed,
                RGB_CAM,
                developed,
                true,
                mode,
            )
            .unwrap();
            assert_eq!(
                held,
                crate::LinearSettings {
                    white_balance: None,
                },
                "{mode:?}: planes that hold the white balance need no approximation"
            );
            // As shot resolves to the camera's gains, which these planes hold.
            let as_shot = crate::RawPayload {
                wb_mode: crate::WhiteBalanceMode::AsShot,
                ..custom(target)
            };
            assert_eq!(
                resolve_raw_settings(&as_shot, developed, RGB_CAM, developed, true, mode)
                    .unwrap()
                    .white_balance,
                None
            );
            let missing = resolve_raw_settings(
                &custom(developed),
                developed,
                RGB_CAM,
                developed,
                false,
                mode,
            )
            .unwrap_err();
            assert_eq!(missing.kind, ErrorKind::PreparationRequired, "{mode:?}");
        }

        let strict =
            resolve_raw_settings(&custom(target), developed, RGB_CAM, developed, true, Strict)
                .unwrap_err();
        assert_eq!(strict.kind, ErrorKind::PreparationRequired);

        let drafted = resolve_raw_settings(
            &custom(target),
            developed,
            RGB_CAM,
            developed,
            true,
            DraftPreview,
        )
        .unwrap();
        assert_eq!(
            drafted.white_balance,
            Some(crate::WhiteBalanceApproximation::between(camera, developed, target).unwrap()),
            "the drafted gains over the developed ones, through the camera matrix"
        );

        let mut singular = RGB_CAM;
        singular[2] = [
            2.0 * singular[0][0],
            2.0 * singular[0][1],
            2.0 * singular[0][2],
            0.0,
        ];
        let error = resolve_raw_settings(
            &custom(target),
            developed,
            singular,
            developed,
            true,
            DraftPreview,
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::PreparationRequired);
        assert!(error.detail.contains("singular"), "{}", error.detail);
    }

    /// On a real RAW file: a drafted temperature previews through the approximation, and every
    /// other evaluation of the same drafted or committed white balance — the draft's point sample,
    /// its analysis, and once committed the preview, render (the export path), sample, analysis and
    /// every pixel a plan samples from its stage context — stays `preparation-required` until the
    /// mosaic is redeveloped, while a plan that samples nothing commits. Run with
    /// LUXFORGE_RAW_FIXTURE pointing to a private qualified NEF, RAF or DNG.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_raw_draft_preview_approximates_and_every_strict_path_refuses() {
        let path = PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("fixture path"));
        let catalog = temp("raw-approximate.sqlite");
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let state = service.import(&path).unwrap();
        let asset = state.asset.id.clone();
        let is_required = |error: Error| error.kind == ErrorKind::PreparationRequired;

        let committed = service.preview_job(&asset, None, None, None, None).unwrap();
        assert!(!committed.evaluation.source().approximate_white_balance());

        let mut draft = Draft::new("set-raw", asset.clone(), state.revision);
        draft.merge(Map::from_iter([("temperature".to_owned(), json!(3200.0))]));
        let drafted = service
            .preview_job(&asset, None, None, Some(&draft), None)
            .expect("a drafted white balance previews");
        assert!(drafted.evaluation.source().approximate_white_balance());
        let PreviewSource::Raw { settings, .. } = drafted.evaluation.source() else {
            panic!("a RAW source");
        };
        let metadata = match &state.asset.source {
            SourceKind::Raw { metadata } => metadata.clone(),
            SourceKind::Jpeg => panic!("a RAW asset"),
        };
        // A temperature drafted from As shot keeps the camera's as-shot tint.
        let [_, as_shot_tint] =
            crate::RawPayload::for_as_shot(metadata.as_shot_gains, metadata.cam_xyz)
                .unwrap()
                .white_balance_controls();
        let target =
            crate::gains_from_temperature_tint(3200.0, as_shot_tint, metadata.cam_xyz).unwrap();
        let camera = metadata
            .rgb_cam
            .map(|row| [row[0], row[1], row[2]].map(f64::from));
        assert_eq!(
            settings.white_balance,
            Some(
                crate::WhiteBalanceApproximation::between(camera, metadata.as_shot_gains, target)
                    .unwrap()
            )
        );
        let rendered = drafted
            .evaluation
            .source()
            .render(
                &service.registry,
                drafted.evaluation.entry().snapshot.id.clone(),
                drafted.evaluation.recipe(),
            )
            .expect("the approximate frame renders");
        let exact = committed
            .evaluation
            .source()
            .render(
                &service.registry,
                committed.evaluation.entry().snapshot.id.clone(),
                committed.evaluation.recipe(),
            )
            .unwrap();
        assert_ne!(
            rendered.rgba, exact.rgba,
            "3200 K is not the as-shot picture"
        );

        // The drafted value's numbers are strict.
        assert!(is_required(
            service
                .point_draft(&asset, &draft, 10, 10)
                .and_then(|plan| plan.evaluate())
                .unwrap_err()
        ));
        assert!(is_required(
            service
                .analysis_plan(&asset, AnalysisSelection::Draft(&draft))
                .unwrap_err()
        ));
        // A drafted exposure is Basic's, over planes that hold the white balance: exact.
        let mut exposure = Draft::new("set-basic", asset.clone(), state.revision);
        exposure.merge(Map::from_iter([("exposure".to_owned(), json!(0.5))]));
        assert!(
            !service
                .preview_job(&asset, None, None, Some(&exposure), None)
                .unwrap()
                .evaluation
                .source()
                .approximate_white_balance()
        );

        // Committed, the same white balance is strict everywhere until it is redeveloped.
        let result = service
            .apply_action(
                &asset,
                mutation(state.revision, "temperature"),
                "set-raw",
                json!({"temperature": 3200.0}),
            )
            .unwrap();
        let current = service.state(&asset).unwrap().current_entry.id;
        assert!(is_required(
            service
                .preview_job(&asset, None, None, None, None)
                .unwrap_err()
        ));
        assert!(is_required(
            service.render_entry(&asset, &current).unwrap_err()
        ));
        assert!(is_required(
            service.sample_entry(&asset, &current, 10, 10).unwrap_err()
        ));
        assert!(is_required(
            service
                .analysis_plan(&asset, AnalysisSelection::Current)
                .unwrap_err()
        ));
        // A plan that samples a pixel reads it from the stage context, which is strict too: a
        // pixel replacement compares the pixel it would replace.
        assert!(is_required(
            service
                .apply_action(
                    &asset,
                    mutation(result.revision, "pixel"),
                    "set-pixel",
                    json!({"x": 10, "y": 10, "rgb": [1, 2, 3]}),
                )
                .unwrap_err()
        ));
        // A plan that reads no pixel asks the context nothing it would refuse, so it commits.
        service
            .apply_action(
                &asset,
                mutation(result.revision, "basic"),
                "set-basic",
                json!({"exposure": 0.5}),
            )
            .expect("a Basic edit plans without the development");
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Run with LUXFORGE_RAW_FIXTURE pointing to a private qualified NEF or RAF.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn raw_import_wb_history_redevelopment_and_reopen_preserve_original() {
        let path = PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("fixture path"));
        let original_hash = format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap()));
        let catalog = temp("raw-history.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let initial = service.import(&path).unwrap();
        assert_eq!(initial.asset.fingerprint, original_hash);
        assert!(matches!(initial.asset.source, SourceKind::Raw { .. }));
        assert_eq!(initial.current_entry.snapshot.recipe.layers.len(), 1);
        let source_layer = initial.current_entry.snapshot.recipe.layers[0].id.clone();
        let original = service
            .preview_job(&initial.asset.id, None, None, None, None)
            .unwrap();
        assert!(matches!(
            original.evaluation.source(),
            PreviewSource::Raw { .. }
        ));
        let as_shot = raw_payload(&initial.current_entry.snapshot.recipe).unwrap();
        assert_eq!(as_shot.as_shot_gains, as_shot.gains);
        // Exposure is Basic's: it leaves the source development as it was.
        let exposure = service
            .apply_action(
                &initial.asset.id,
                mutation(0, "raw-exposure"),
                "set-basic",
                json!({"exposure":1.5}),
            )
            .unwrap();
        let exposed = service.state(&initial.asset.id).unwrap();
        assert_eq!(
            exposed.current_entry.snapshot.recipe.layers[0].id,
            source_layer
        );
        assert_eq!(
            raw_payload(&exposed.current_entry.snapshot.recipe).unwrap(),
            as_shot
        );
        let gain = (f64::from(as_shot.gains[0]) * 1.1).min(16.0);
        let changed = service
            .apply_action(
                &initial.asset.id,
                mutation(exposure.revision, "raw-red"),
                "set-raw-red-gain",
                json!({"gain":gain}),
            )
            .unwrap();
        let state = service.state(&initial.asset.id).unwrap();
        let payload = raw_payload(&state.current_entry.snapshot.recipe).unwrap();
        assert_eq!(
            state.current_entry.snapshot.recipe.layers[0].id,
            source_layer
        );
        assert_eq!(payload.wb_mode, crate::WhiteBalanceMode::Custom);
        assert_eq!(
            payload.gains[2], as_shot.gains[2],
            "partial custom edit retains camera blue gain"
        );
        let refused = service
            .preview_job(&initial.asset.id, None, None, None, None)
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::PreparationRequired);
        // The refusal names the development at the committed white balance.
        let needs = refused.needs().expect("a refusal names what it needs");
        assert_eq!(needs.entry_id, state.current_entry.id);
        assert_eq!(needs.gains, Some(payload.gains));
        service.prepare(needs).unwrap();
        assert!(matches!(
            service
                .preview_job(&initial.asset.id, None, None, None, None)
                .unwrap()
                .evaluation
                .source(),
            PreviewSource::Raw { .. }
        ));
        let undo = service
            .undo(&initial.asset.id, mutation(changed.revision, "undo-red"))
            .unwrap();
        assert_eq!(undo.outcome, MutationOutcome::Navigated);
        let state = service.state(&initial.asset.id).unwrap();
        assert_eq!(
            raw_payload(&state.current_entry.snapshot.recipe)
                .unwrap()
                .wb_mode,
            crate::WhiteBalanceMode::AsShot
        );
        drop(service);
        let reopened = EditorService::open(&catalog).unwrap();
        let state = reopened.state(&initial.asset.id).unwrap();
        assert_eq!(
            state.current_entry.snapshot.recipe.layers[0].id,
            source_layer
        );
        assert_eq!(
            reopened.inspect_source(&initial.asset.id, None).unwrap()["readiness"],
            "preparation-required"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(std::fs::read(&path).unwrap())),
            original_hash
        );
        drop(reopened);
        std::fs::remove_file(catalog).unwrap();
    }

    /// The typed interpretation a reopened catalog reads back is equal to the one the decoder
    /// produces again: a repeated import finds the same asset rather than a changed original, and a
    /// synchronous preparation after reopen accepts the file it decodes. Run with
    /// LUXFORGE_RAW_FIXTURE pointing to a private qualified NEF, RAF or DNG.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_reopened_raw_interpretation_equals_the_decoders_again() {
        let path = PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("fixture path"));
        let catalog = temp("raw-reinterpretation.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let imported = service.import(&path).unwrap();
        drop(service);

        let mut service = EditorService::open(&catalog).unwrap();
        let again = service.import(&path).expect("the same interpretation");
        assert_eq!(again.asset, imported.asset);
        drop(service);

        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.state(&imported.asset.id).unwrap();
        assert_eq!(state.asset.source, imported.asset.source);
        service
            .prepare(&service.entry_needs(&state.asset.id, None).unwrap())
            .expect("a preparation after reopen accepts the decoded interpretation");
        service
            .verified_prepared(&state.asset, &state.current_entry.snapshot.recipe)
            .expect("the prepared original is cached");
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A RAW whose current entry holds a custom white balance, reopened: the blocking helper runs
    /// the owner's one preparation, which decodes the original and develops it at that entry's own
    /// gains in the same job — no as-shot development, no redevelopment and no adoption written by
    /// hand — and the reopened render equals the one the first session made after its own
    /// redevelopment. Run with LUXFORGE_RAW_FIXTURE pointing to a private qualified NEF, RAF or
    /// DNG.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_reopened_raw_with_a_custom_white_balance_develops_at_its_entrys_own_gains() {
        let path = PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("fixture path"));
        let catalog = temp("raw-custom-reopen.sqlite");
        let (asset, gains, rendered) = {
            let mut service = EditorService::open(&catalog).unwrap();
            let initial = service.import(&path).unwrap();
            let asset = initial.asset.id;
            let as_shot = raw_payload(&initial.current_entry.snapshot.recipe).unwrap();
            let gain = (f64::from(as_shot.gains[0]) * 1.1).min(16.0);
            service
                .apply_action(
                    &asset,
                    mutation(0, "raw-red"),
                    "set-raw-red-gain",
                    json!({"gain": gain}),
                )
                .unwrap();
            let current = service.state(&asset).unwrap().current_entry;
            let gains = raw_payload(&current.snapshot.recipe).unwrap().gains;
            assert_ne!(gains, as_shot.gains, "a custom white balance");
            // The first session redevelops the cached mosaic at the new gains.
            let refused = service.render_current(&asset).unwrap_err();
            assert_eq!(refused.needs().unwrap().gains, Some(gains));
            service.prepare(refused.needs().unwrap()).unwrap();
            let rendered = service.render_current(&asset).unwrap();
            (asset, gains, rendered)
        };

        let mut service = EditorService::open(&catalog).unwrap();
        assert_eq!(
            service.inspect_source(&asset, None).unwrap()["readiness"],
            "preparation-required"
        );
        let needs = service.entry_needs(&asset, None).unwrap();
        assert_eq!(needs.gains, Some(gains));
        // One file preparation, targeted at the entry's gains.
        let Preparing::Work(SourceWork::File { target, .. }, _) =
            service.preparation(&needs).unwrap()
        else {
            panic!("a reopened original is prepared from its file");
        };
        assert_eq!(target.unwrap().raw.unwrap().gains, gains);
        service.prepare(&needs).unwrap();
        {
            let cache = service.source_cache.borrow();
            let Some(PreparedSource::Raw(raw)) = cache.as_ref().map(|cached| &cached.source) else {
                panic!("the RAW original is cached");
            };
            assert_eq!(raw.gains, gains, "developed at the entry's own gains");
            assert!(raw.linear.is_some(), "and developed in the same job");
        }
        assert!(service.raw_development(&asset, gains).unwrap().is_none());
        assert_eq!(
            service.inspect_source(&asset, None).unwrap()["readiness"],
            "ready"
        );
        let reopened = service.render_current(&asset).unwrap();
        assert_eq!(
            (reopened.width, reopened.height),
            (rendered.width, rendered.height)
        );
        assert!(
            reopened.rgba == rendered.rgba,
            "the reopened render equals the redeveloped one"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Cold saved-white-balance preparation through the catalog owner, as the [performance
    /// plan](../../../../docs/specs/performance.md#native-development-and-saved-white-balance-preparation)
    /// measures it. One catalog is made first, holding the RAW with a saved custom red gain of
    /// 1.1 × as-shot. Every observation then starts a new owner on it, so the source cache is
    /// empty while the filesystem cache stays warm, and times from immediately before
    /// `catalog.import` until a strict exact-source `PreviewJob` for the current entry is
    /// available: reading, hashing, decoding and developing the original at the saved gains, and
    /// waiting for and adopting that one job. Starting the owner and opening the catalog,
    /// hashing the planes and stopping the owner are outside the clock, and nothing is rendered.
    /// Every observation's planes must hash the same. A measurement, not a gate; the one-minute
    /// load average is read before the first and after the last observation:
    ///
    /// ```text
    /// LUXFORGE_RAW_FIXTURE=/path/to/nikon_z6.NEF [LUXFORGE_RAW_SAMPLES=15] cargo test --release \
    ///   --locked -p luxforge-core --lib cold_saved_white_balance_preparation_timing -- \
    ///   --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a measurement, not a gate; run alone in release"]
    fn cold_saved_white_balance_preparation_timing() {
        use crate::{ApiRequest, OwnerHandle, PreviewRequest};
        use std::time::Instant;
        let path = PathBuf::from(std::env::var("LUXFORGE_RAW_FIXTURE").expect("fixture path"));
        let samples: usize = std::env::var("LUXFORGE_RAW_SAMPLES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(15);
        let load = || {
            std::process::Command::new("/usr/sbin/sysctl")
                .args(["-n", "vm.loadavg"])
                .output()
                .ok()
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
                .unwrap_or_else(|| "unavailable".into())
        };
        let catalog = temp("cold-saved-wb-timing.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (asset, gains) = {
            let mut service = EditorService::open(&catalog).unwrap();
            let initial = service.import(&path).unwrap();
            let asset = initial.asset.id;
            let as_shot = raw_payload(&initial.current_entry.snapshot.recipe).unwrap();
            let gain = (f64::from(as_shot.gains[0]) * 1.1).min(16.0);
            service
                .apply_action(
                    &asset,
                    mutation(0, "raw-red"),
                    "set-raw-red-gain",
                    json!({"gain": gain}),
                )
                .unwrap();
            let current = service.state(&asset).unwrap().current_entry;
            let gains = raw_payload(&current.snapshot.recipe).unwrap().gains;
            assert_ne!(gains, as_shot.gains, "a custom white balance");
            (asset, gains)
        };
        let call = |owner: &OwnerHandle, client, method: &str, params: Value| {
            let response = owner
                .call(
                    client,
                    ApiRequest {
                        id: method.into(),
                        method: method.into(),
                        params,
                        token: None,
                    },
                )
                .unwrap();
            assert!(response.error.is_none(), "{method}: {:?}", response.error);
            response.result.unwrap()
        };
        fn find<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
            match value {
                Value::Object(map) => map
                    .get(key)
                    .or_else(|| map.values().find_map(|v| find(v, key))),
                Value::Array(items) => items.iter().find_map(|v| find(v, key)),
                _ => None,
            }
        }
        let (mut times, mut digest, mut backend) = (Vec::new(), None, None);
        let load_start = load();
        for observation in 0..samples {
            let (owner, join) = OwnerHandle::start(&catalog).unwrap();
            let client = owner.register();
            let started = Instant::now();
            let queued = call(
                &owner,
                client,
                "catalog.import",
                json!({"path": path, "mutation": {
                    "request_id": format!("cold-import-{observation}"), "actor": "timing",
                }}),
            );
            let job = queued["job_id"].clone();
            owner.wait_source(client, None).unwrap();
            let adopted = call(&owner, client, "job.adopt", json!({"job_id": job}));
            let preview = owner
                .preview_job(PreviewRequest::new(client, asset.clone()))
                .expect("a strict exact-source preview job after the one source job");
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            let PreviewSource::Raw { image, .. } = preview.evaluation.source() else {
                panic!("a RAW source");
            };
            assert!(
                !preview.evaluation.source().approximate_white_balance(),
                "exact, not approximated"
            );
            let mut hash = Sha256::new();
            for chunk in image.planes().chunks(4096) {
                let bytes: Vec<u8> = chunk
                    .iter()
                    .flat_map(|v| v.to_bits().to_le_bytes())
                    .collect();
                hash.update(bytes);
            }
            let hash = format!("{:x}", hash.finalize());
            assert_eq!(digest.get_or_insert_with(|| hash.clone()), &hash);
            let named = find(&adopted, "backend").cloned();
            assert_eq!(backend.get_or_insert_with(|| named.clone()), &named);
            times.push(elapsed);
            drop(preview);
            owner.stop();
            join.join().unwrap();
        }
        let load_end = load();
        let distribution = luxforge_testbase::Distribution::of(times.clone()).expect("samples");
        println!(
            "{}",
            json!({
                "source": path,
                "saved_gains": gains,
                "backend": backend,
                "observations": samples,
                "p50_p95_ms": [distribution.p50, distribution.p95],
                "min_max_ms": [distribution.min, distribution.max],
                "times_ms": times,
                "planes_sha256": digest,
                "load_start": load_start,
                "load_end": load_end,
            })
        );
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn aliases_reuse_asset_but_copies_do_not_and_changed_sources_fail() {
        let dir = temp("aliases");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.jpg");
        let hard = dir.join("hard.jpg");
        let copy = dir.join("copy.jpg");
        std::fs::copy(fixture(), &source).unwrap();
        std::fs::hard_link(&source, &hard).unwrap();
        std::fs::copy(&source, &copy).unwrap();
        let mut service = EditorService::open(&dir.join("catalog.sqlite")).unwrap();
        let a = service.import(&source).unwrap().asset.id;
        assert_eq!(service.import(&hard).unwrap().asset.id, a);
        assert_ne!(service.import(&copy).unwrap().asset.id, a);
        let listed: Vec<AssetId> = service
            .assets(None, 100)
            .unwrap()
            .assets
            .into_iter()
            .map(|asset| asset.id)
            .collect();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0], a);
        std::fs::write(&source, b"changed").unwrap();
        assert_eq!(
            service.render_current(&a).unwrap_err().kind,
            ErrorKind::SourceUnavailable
        );
        drop(service);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unchanged_sources_reuse_one_decoded_pixel_allocation() {
        let catalog = temp("source-cache.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&fixture()).unwrap();
        let first = service
            .preview_job(&state.asset.id, None, None, None, None)
            .unwrap();
        let second = service
            .preview_job(&state.asset.id, None, None, None, None)
            .unwrap();
        let (PreviewSource::Jpeg(first_source), PreviewSource::Jpeg(second_source)) =
            (first.evaluation.source(), second.evaluation.source())
        else {
            panic!("JPEG preview expected")
        };
        assert!(std::sync::Arc::ptr_eq(
            &first_source.rgba,
            &second_source.rgba
        ));
        let raster = render(
            service.registry(),
            first_source,
            first.evaluation.entry().snapshot.id.clone(),
            &first.evaluation.entry().snapshot.recipe,
        )
        .unwrap();
        assert!(std::sync::Arc::ptr_eq(&first_source.rgba, &raster.rgba));
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn same_length_source_replacement_invalidates_the_decode_cache() {
        let dir = temp("source-cache-invalidation");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.jpg");
        std::fs::copy(fixture(), &source).unwrap();
        let mut service = EditorService::open(&dir.join("catalog.sqlite")).unwrap();
        let asset = service.import(&source).unwrap().asset.id;
        service.preview_job(&asset, None, None, None, None).unwrap();
        let replacement =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-2.jpg");
        assert_eq!(
            std::fs::metadata(&source).unwrap().len(),
            std::fs::metadata(&replacement).unwrap().len()
        );
        std::fs::copy(replacement, &source).unwrap();
        // The changed signature is a cache miss, and the preparation that reads the file again
        // finds other bytes.
        let refused = service
            .preview_job(&asset, None, None, None, None)
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::PreparationRequired);
        assert_eq!(
            service.prepare(refused.needs().unwrap()).unwrap_err().kind,
            ErrorKind::SourceUnavailable
        );
        drop(service);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
