use super::{
    AssetRecord, CachedSource, EditorService, EditorState, PreparedFile, RawDevelopment,
    SourceKind, SourceSignature,
    catalog::{encode, insert_entry, now_ms, write},
};
use crate::{
    AssetId, EntryId, Error, ErrorKind, HistoryEntry, LayerId, Preparation, PreparationNeeds,
    Snapshot, open_source_bytes, read_bounded_file,
    source::{PreparedSource, RawPreparation, RawPrepared},
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
/// It serializes exactly as [`lightwell_raw::RawMetadata`] does, so the catalog row and the API
/// carry the object they always have. Equality is the typed fields' own at their native precision:
/// an `f32` compares as the `f32` it is, whichever decimal spelling it was read from.
#[derive(Clone, Debug, PartialEq)]
pub struct RawInterpretation(Arc<lightwell_raw::RawMetadata>);

/// Equality is reflexive because no field can hold a NaN: the decoder refuses a non-finite
/// calibration, gain, black level or white level, and JSON has no spelling for one.
impl Eq for RawInterpretation {}

impl RawInterpretation {
    /// Accept a decoder's metadata once its correction record agrees with its mode: a mode that
    /// needs DNG corrections carries their record, and no other mode carries one.
    pub(crate) fn new(metadata: lightwell_raw::RawMetadata) -> Result<Self, Error> {
        if metadata.mode.requires_dng_corrections() != metadata.dng_corrections.is_some() {
            return Err(Error::incompatible(
                "RAW correction record differs from mode",
            ));
        }
        Ok(Self(Arc::new(metadata)))
    }
}

impl std::ops::Deref for RawInterpretation {
    type Target = lightwell_raw::RawMetadata;

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
        Self::new(lightwell_raw::RawMetadata::deserialize(deserializer)?)
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
        .map_err(|e| Error::file_access(format!("cannot resolve source: {}", e.kind())))
}

fn file_access(error: std::io::Error) -> Error {
    Error::file_access(error.kind().to_string())
}

/// A source path's canonical spelling and its file's current signature.
fn located_signature(path: &Path) -> Result<(PathBuf, Metadata, SourceSignature), Error> {
    let canonical = canonical_source(path)?;
    let metadata = canonical.metadata().map_err(file_access)?;
    let signature = source_signature(&canonical, &metadata);
    Ok((canonical, metadata, signature))
}

/// The signature an asset's original has now, refused when the file is gone or is no longer the
/// file that was imported.
fn original_signature(asset: &AssetRecord) -> Result<SourceSignature, Error> {
    let metadata = asset
        .locator
        .metadata()
        .map_err(|_| Error::source_unavailable("original source is unavailable"))?;
    let signature = source_signature(&asset.locator, &metadata);
    if signature.file_identity != asset.file_identity || signature.byte_len != asset.byte_len {
        return Err(Error::source_unavailable(
            "original source fingerprint changed",
        ));
    }
    Ok(signature)
}

/// Immutable source settings for a known file. The worker receives bounded catalog metadata,
/// then checks the decoded original before developing it at this entry's gains.
#[derive(Clone, Debug)]
pub(crate) struct FilePreparation {
    pub(crate) fingerprint: String,
    pub(crate) raw: Option<RawPreparation>,
}

impl FilePreparation {
    fn for_recipe(asset: &AssetRecord, recipe: &crate::Recipe) -> Result<Self, Error> {
        validate_source_recipe(asset, recipe)?;
        let raw = match &asset.source {
            SourceKind::Jpeg => None,
            SourceKind::Raw { metadata } => Some(RawPreparation {
                metadata: metadata.0.as_ref().clone(),
                gains: development_gains(asset, recipe)?.expect("RAW has development gains"),
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

impl EditorService {
    /// The catalog owner disables synchronous source misses; workers prepare these separately.
    pub(crate) fn disable_sync_source(&mut self) {
        self.allow_sync_source = false;
    }

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

    /// Read, hash and decode from the same bounded, stable read-only file handle on a worker.
    pub(crate) fn prepare_file(path: &Path) -> Result<PreparedFile, Error> {
        Self::prepare_file_cancel(path, None, &AtomicBool::new(false))
    }

    /// [`Self::prepare_file`] under a cancellation flag. A known RAW is developed at its entry's gains.
    pub(crate) fn prepare_file_cancel(
        path: &Path,
        target: Option<&FilePreparation>,
        cancel: &AtomicBool,
    ) -> Result<PreparedFile, Error> {
        let canonical = canonical_source(path)?;
        let mut file = File::open(&canonical).map_err(file_access)?;
        let handle_before = file.metadata().map_err(file_access)?;
        let path_before = canonical.metadata().map_err(file_access)?;
        let signature = source_signature(&canonical, &handle_before);
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
        if signature != source_signature(&canonical, &handle_after)
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

    pub(crate) fn known_file_preparation(
        &self,
        path: &Path,
    ) -> Result<Option<FilePreparation>, Error> {
        let (canonical, signature) = Self::request_signature(path)?;
        self.asset_for_source(&canonical, &signature.file_identity)?
            .map(|(id, _)| self.file_preparation(&id, None))
            .transpose()
    }

    pub(crate) fn file_preparation(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<FilePreparation, Error> {
        let state = self.state(asset_id)?;
        let entry = match entry_id {
            Some(id) => self.entry(asset_id, id)?,
            None => state.current_entry,
        };
        FilePreparation::for_recipe(&state.asset, &entry.snapshot.recipe)
    }

    /// A repeated import can reuse the one verified immutable decode without a new worker job.
    pub(crate) fn cached_import(&self, path: &Path) -> Result<Option<EditorState>, Error> {
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
    pub fn inspect_source(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<Value, Error> {
        let state = self.state(asset_id)?;
        let entry = match entry_id {
            Some(id) => self.entry(asset_id, id)?,
            None => state.current_entry,
        };
        validate_source_recipe(&state.asset, &entry.snapshot.recipe)?;
        let cached = self.cached_state(asset_id)?.is_some();
        let needs_development = match development_gains(&state.asset, &entry.snapshot.recipe)? {
            Some(gains) => cached && self.raw_development(asset_id, gains)?.is_some(),
            None => false,
        };
        Ok(json!({
            "asset_id": asset_id,
            "entry_id": entry.id,
            "fingerprint": state.asset.fingerprint,
            "width": state.asset.width,
            "height": state.asset.height,
            "source": state.asset.source,
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
    /// queues.
    pub(crate) fn entry_needs(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<PreparationNeeds, Error> {
        let state = self.state(asset_id)?;
        let entry = match entry_id {
            Some(id) => self.entry(asset_id, id)?,
            None => state.current_entry,
        };
        validate_source_recipe(&state.asset, &entry.snapshot.recipe)?;
        self.preparation_needs(Evaluated::exactly(
            &state.asset,
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
    /// already hold them, and when the cache holds no RAW source of this asset, whose preparation
    /// develops it.
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
        if gains == raw.gains && raw.linear.is_some() {
            return Ok(None);
        }
        Ok(Some(RawDevelopment {
            asset_id: asset_id.clone(),
            signature: cached.signature.clone(),
            fingerprint: state.asset.fingerprint,
            sensor: raw.sensor.clone(),
            gains,
            file_name: state
                .asset
                .locator
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
        }))
    }

    /// Release the cache's float planes before the sole source worker allocates another development.
    /// Preview jobs may still hold the old Arc; the worker's memory gate waits for those to finish.
    pub(crate) fn evict_development(&self) {
        if let Some(cached) = self.source_cache.borrow_mut().as_mut()
            && let PreparedSource::Raw(raw) = &mut cached.source
        {
            raw.linear.take();
        }
    }

    pub(crate) fn install_development(
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
        let PreparedSource::Raw(previous) = &cached.source else {
            return Err(Error::incompatible(
                "RAW development targeted a JPEG source",
            ));
        };
        if !Arc::ptr_eq(&previous.sensor, &request.sensor) {
            return Err(Error::conflict("RAW mosaic changed during development"));
        }
        cached.source = PreparedSource::Raw(developed);
        Ok(state)
    }

    pub(crate) fn cached_state(&self, asset_id: &AssetId) -> Result<Option<EditorState>, Error> {
        let state = self.state(asset_id)?;
        let signature = original_signature(&state.asset)?;
        let cache = self.source_cache.borrow();
        Ok(cache
            .as_ref()
            .filter(|cached| cached.asset_id == state.asset.id && cached.signature == signature)
            .map(|_| state))
    }

    /// Direct service clients may prepare synchronously; the API owner never calls this path.
    pub fn import(&mut self, path: &Path) -> Result<EditorState, Error> {
        let target = self.known_file_preparation(path)?;
        let prepared = Self::prepare_file_cancel(path, target.as_ref(), &AtomicBool::new(false))?;
        self.import_prepared(prepared).map(|(state, _)| state)
    }

    /// Complete an import only after a worker has verified and decoded its exact source bytes.
    /// The bool says whether a new asset was inserted for event publication.
    pub(crate) fn import_prepared(
        &mut self,
        prepared: PreparedFile,
    ) -> Result<(EditorState, bool), Error> {
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
            self.source_cache.replace(Some(CachedSource {
                asset_id: state.asset.id.clone(),
                signature,
                source,
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
        if matches!(asset.source, SourceKind::Raw { .. }) {
            snapshot = snapshot.with_layer_inserted(
                0,
                crate::RawPayload::for_as_shot(
                    source.metadata().expect("RAW metadata").as_shot_gains,
                    source.metadata().expect("RAW metadata").cam_xyz,
                )?
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
        let byte_len = i64::try_from(asset.byte_len)
            .map_err(|_| Error::resource_limit("source length exceeds catalog range"))?;
        let artifact_root = &self.artifact_root;
        write(&mut self.connection, |tx| {
            tx.execute(
                "INSERT INTO assets VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    asset.id.as_str(),
                    asset.source_root.to_string_lossy(),
                    asset.locator.to_string_lossy(),
                    canonical_text,
                    asset.file_identity,
                    asset.fingerprint,
                    byte_len,
                    i64::from(asset.width),
                    i64::from(asset.height),
                    encode(&asset.source)?,
                ],
            )?;
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

    pub(super) fn verified_prepared(&self, asset: &AssetRecord) -> Result<PreparedSource, Error> {
        let signature = original_signature(asset)?;
        let raw_source = matches!(&asset.source, SourceKind::Raw { .. });
        let max_source_bytes = if raw_source {
            lightwell_raw::MAX_SOURCE_BYTES as u64
        } else {
            crate::source::MAX_JPEG_BYTES as u64
        };
        if signature.byte_len > max_source_bytes {
            return Err(Error::source_unavailable(
                "original source fingerprint changed",
            ));
        }
        if let Some(cached) = self.source_cache.borrow().as_ref()
            && cached.asset_id == asset.id
            && cached.signature == signature
        {
            return Ok(cached.source.clone());
        }
        if !self.allow_sync_source {
            return Err(Error::preparation_required("source preparation required"));
        }
        let prepared = Self::prepare_file(&asset.locator)?;
        if prepared.signature != signature || prepared.fingerprint != asset.fingerprint {
            return Err(Error::source_unavailable(
                "original source fingerprint changed",
            ));
        }
        match (&asset.source, &prepared.source) {
            (SourceKind::Jpeg, PreparedSource::Jpeg(_)) => {}
            (SourceKind::Raw { metadata }, PreparedSource::Raw(raw))
                if **metadata == *raw.sensor.metadata() => {}
            _ => {
                return Err(Error::incompatible(
                    "original source interpretation changed",
                ));
            }
        }
        self.source_cache.replace(Some(CachedSource {
            asset_id: asset.id.clone(),
            signature,
            source: prepared.source.clone(),
        }));
        Ok(prepared.source)
    }
}

pub(super) fn validate_source_recipe(
    asset: &AssetRecord,
    recipe: &crate::Recipe,
) -> Result<(), Error> {
    match asset.source {
        SourceKind::Jpeg => {
            if recipe
                .layers
                .iter()
                .any(|layer| layer.effect_id == crate::RAW_EFFECT)
            {
                return Err(Error::incompatible(
                    "JPEG recipe contains a RAW source layer",
                ));
            }
        }
        SourceKind::Raw { ref metadata } => {
            let payload = raw_payload(recipe)?;
            if payload.as_shot_gains != metadata.as_shot_gains
                || payload.cam_xyz != metadata.cam_xyz
            {
                return Err(Error::incompatible(
                    "RAW source layer calibration differs from original",
                ));
            }
        }
    }
    Ok(())
}

/// Whether an evaluation may approximate a RAW white balance the developed planes do not hold.
///
/// [`Self::DraftPreview`] is taken in exactly one place, [`EditorService::preview_job`] for an
/// open draft. Every other evaluation — a committed or historical preview, `render_entry` and so
/// every export, `sample_entry` and `sample_draft` and so the readout and `render.sample`,
/// `analysis_plan`, and every pixel a plan or query samples from its stage context — is
/// [`Self::Strict`].
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
    let gains = match payload.wb_mode {
        crate::WhiteBalanceMode::AsShot => as_shot_gains,
        crate::WhiteBalanceMode::Custom => payload.gains,
    };
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
    Ok(crate::LinearSettings {
        exposure_ev: payload.exposure_ev,
        white_balance,
    })
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
    let SourceKind::Raw { metadata } = &asset.source else {
        return Ok(None);
    };
    let payload = raw_payload(recipe)?;
    Ok(Some(match payload.wb_mode {
        crate::WhiteBalanceMode::AsShot => metadata.as_shot_gains,
        crate::WhiteBalanceMode::Custom => payload.gains,
    }))
}

fn raw_payload(recipe: &crate::Recipe) -> Result<crate::RawPayload, Error> {
    let Some(layer) = recipe.layers.first() else {
        return Err(Error::incompatible(
            "RAW recipe is missing its required source layer",
        ));
    };
    if layer.effect_id != crate::RAW_EFFECT
        || recipe
            .layers
            .iter()
            .skip(1)
            .any(|layer| layer.effect_id == crate::RAW_EFFECT)
    {
        return Err(Error::incompatible(
            "RAW recipe needs exactly one source layer at index zero",
        ));
    }
    crate::RawPayload::from_layer(layer)
}

pub(crate) fn source_signature(path: &Path, metadata: &Metadata) -> SourceSignature {
    SourceSignature {
        byte_len: metadata.len(),
        modified: metadata.modified().ok(),
        file_identity: file_identity(metadata, path),
        change_marker: metadata_change_marker(metadata),
    }
}

#[cfg(unix)]
fn metadata_change_marker(metadata: &Metadata) -> Option<(i128, i128)> {
    use std::os::unix::fs::MetadataExt;
    Some((
        i128::from(metadata.ctime()),
        i128::from(metadata.ctime_nsec()),
    ))
}

#[cfg(windows)]
fn metadata_change_marker(metadata: &Metadata) -> Option<(i128, i128)> {
    use std::os::windows::fs::MetadataExt;
    Some((i128::from(metadata.last_write_time()), 0))
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
fn file_identity(metadata: &Metadata, canonical: &Path) -> String {
    use std::os::windows::fs::MetadataExt;
    match (metadata.volume_serial_number(), metadata.file_index()) {
        (Some(volume), Some(index)) => format!("windows:{volume}:{index}"),
        _ => format!("path:{}", canonical.to_string_lossy().to_lowercase()),
    }
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_: &Metadata, canonical: &Path) -> String {
    format!("path:{}", canonical.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{
        AnalysisSelection, MutationOutcome,
        test_support::{fixture, mutation, temp},
    };
    use crate::{Draft, PreviewSource, render::testing::render};
    use serde_json::Map;

    #[test]
    fn known_file_preparation_checks_bytes_and_source_kind() {
        let path = fixture();
        let prepared = EditorService::prepare_file(&path).unwrap();
        let target = FilePreparation {
            fingerprint: prepared.fingerprint.clone(),
            raw: None,
        };
        EditorService::prepare_file_cancel(&path, Some(&target), &AtomicBool::new(false))
            .expect("matching JPEG target");
        let wrong = FilePreparation {
            fingerprint: "different bytes".into(),
            ..target
        };
        assert_eq!(
            EditorService::prepare_file_cancel(&path, Some(&wrong), &AtomicBool::new(false))
                .unwrap_err()
                .kind,
            ErrorKind::SourceUnavailable
        );
    }

    #[test]
    fn raw_interpretation_json_roundtrip_is_strict_at_native_precision() {
        use lightwell_raw::{RawMetadata, RawMode, RawRect};
        let rect = RawRect {
            x: 0,
            y: 0,
            width: 32,
            height: 32,
        };
        let metadata = RawMetadata {
            make: "Test".into(),
            model: "Camera".into(),
            mode: RawMode::NikonZ6Lossless14,
            sensor_width: 32,
            sensor_height: 32,
            active_area: rect,
            default_crop: rect,
            cfa_width: 2,
            cfa_height: 2,
            cfa: vec![0, 1, 1, 2],
            black_cfa: vec![0, 1, 3, 2],
            black_base: 12.125,
            black_channels: [0.1, 0.2, 0.3, 0.4],
            black_repeat_width: 1,
            black_repeat_height: 1,
            black_repeat: vec![0.12345678],
            sensor_white: 16383.0,
            as_shot_gains: [1.2345678, 1.0, 1.8765432],
            libraw_flip: 0,
            rgb_cam: [[0.12345678; 4]; 3],
            cam_xyz: [[0.12345678; 3]; 4],
            backend: "pinned backend".into(),
            exif_orientation: 1,
            libraw_inset: Some(rect),
            format_identity: "test-format".into(),
            warnings: vec![],
            dng_corrections: None,
        };
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
        dng.mode = RawMode::DjiAir2sDng16;
        dng.dng_corrections = Some(lightwell_raw::DngCorrectionMetadata {
            interpretation: "test-stage3-v1".into(),
            applied: [9_u32, 1]
                .map(|id| lightwell_raw::DngOpcodeProvenance {
                    list: 51022,
                    id,
                    version: 0x0103_0000,
                    flags: 0,
                    payload_sha256: format!("{id:064x}"),
                })
                .to_vec(),
            skipped_optional: vec![],
            calibration: lightwell_raw::DngCalibrationMetadata {
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
        validate_source_recipe(&asset, &snapshot.recipe).unwrap();
        for calibration in ["as_shot_gains", "cam_xyz"] {
            let mut corrupted = payload.clone();
            if calibration == "as_shot_gains" {
                corrupted.as_shot_gains[0] += 0.1;
            } else {
                corrupted.cam_xyz[0][0] += 0.1;
            }
            let mut recipe = snapshot.recipe.clone();
            recipe.layers[0] = corrupted.layer(layer_id.clone());
            assert_eq!(
                validate_source_recipe(&asset, &recipe).unwrap_err().kind,
                ErrorKind::Incompatible,
                "{calibration}"
            );
        }
    }

    /// A camera matrix with rows summing to one and strong cross terms, as a real `rgb_cam` has.
    const RGB_CAM: [[f32; 4]; 3] = [
        [1.72, -0.61, -0.11, 0.0],
        [-0.18, 1.49, -0.31, 0.0],
        [0.04, -0.52, 1.48, 0.0],
    ];

    fn custom(gains: [f32; 3], exposure_ev: f64) -> crate::RawPayload {
        crate::RawPayload {
            exposure_ev,
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
                &custom(developed, 0.4),
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
                    exposure_ev: 0.4,
                    white_balance: None,
                },
                "{mode:?}: planes that hold the white balance need no approximation"
            );
            // As shot resolves to the camera's gains, which these planes hold.
            let as_shot = crate::RawPayload {
                wb_mode: crate::WhiteBalanceMode::AsShot,
                ..custom(target, 0.0)
            };
            assert_eq!(
                resolve_raw_settings(&as_shot, developed, RGB_CAM, developed, true, mode)
                    .unwrap()
                    .white_balance,
                None
            );
            let missing = resolve_raw_settings(
                &custom(developed, 0.0),
                developed,
                RGB_CAM,
                developed,
                false,
                mode,
            )
            .unwrap_err();
            assert_eq!(missing.kind, ErrorKind::PreparationRequired, "{mode:?}");
        }

        let strict = resolve_raw_settings(
            &custom(target, 0.4),
            developed,
            RGB_CAM,
            developed,
            true,
            Strict,
        )
        .unwrap_err();
        assert_eq!(strict.kind, ErrorKind::PreparationRequired);

        let drafted = resolve_raw_settings(
            &custom(target, 0.4),
            developed,
            RGB_CAM,
            developed,
            true,
            DraftPreview,
        )
        .unwrap();
        assert_eq!(drafted.exposure_ev, 0.4);
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
            &custom(target, 0.0),
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
    /// LIGHTWELL_RAW_FIXTURE pointing to a private qualified NEF, RAF or DNG.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_raw_draft_preview_approximates_and_every_strict_path_refuses() {
        let path = PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE").expect("fixture path"));
        let catalog = temp("raw-approximate.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let state = service.import(&path).unwrap();
        let asset = state.asset.id.clone();
        let is_required = |error: Error| error.kind == ErrorKind::PreparationRequired;

        let committed = service.preview_job(&asset, None, None, None, None).unwrap();
        assert!(!committed.source.approximate_white_balance());

        let mut draft = Draft::new("set-raw-temperature", asset.clone(), state.revision);
        draft.merge(Map::from_iter([("kelvin".to_owned(), json!(3200.0))]));
        let drafted = service
            .preview_job(&asset, None, None, Some(&draft), None)
            .expect("a drafted white balance previews");
        assert!(drafted.source.approximate_white_balance());
        let PreviewSource::Raw { settings, .. } = &drafted.source else {
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
            .source
            .render(
                &service.registry,
                drafted.entry.snapshot.id.clone(),
                &drafted.recipe,
            )
            .expect("the approximate frame renders");
        let exact = committed
            .source
            .render(
                &service.registry,
                committed.entry.snapshot.id.clone(),
                &committed.recipe,
            )
            .unwrap();
        assert_ne!(
            rendered.rgba, exact.rgba,
            "3200 K is not the as-shot picture"
        );

        // The drafted value's numbers are strict.
        assert!(is_required(
            service.sample_draft(&asset, &draft, 10, 10).unwrap_err()
        ));
        assert!(is_required(
            service
                .analysis_plan(&asset, AnalysisSelection::Draft(&draft))
                .unwrap_err()
        ));
        // A drafted exposure over planes that hold the white balance is exact.
        let mut exposure = Draft::new("set-raw-exposure", asset.clone(), state.revision);
        exposure.merge(Map::from_iter([("ev".to_owned(), json!(0.5))]));
        assert!(
            !service
                .preview_job(&asset, None, None, Some(&exposure), None)
                .unwrap()
                .source
                .approximate_white_balance()
        );

        // Committed, the same white balance is strict everywhere until it is redeveloped.
        let result = service
            .apply_action(
                &asset,
                mutation(state.revision, "temperature"),
                "set-raw-temperature",
                json!({"kelvin": 3200.0}),
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

    /// Run with LIGHTWELL_RAW_FIXTURE pointing to a private qualified NEF or RAF.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn raw_import_wb_history_redevelopment_and_reopen_preserve_original() {
        let path = PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE").expect("fixture path"));
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
        assert!(matches!(original.source, PreviewSource::Raw { .. }));
        let as_shot = raw_payload(&initial.current_entry.snapshot.recipe).unwrap();
        assert_eq!(as_shot.as_shot_gains, as_shot.gains);
        let exposure = service
            .apply_action(
                &initial.asset.id,
                mutation(0, "raw-exposure"),
                "set-raw-exposure",
                json!({"ev":1.5}),
            )
            .unwrap();
        let exposed = service.state(&initial.asset.id).unwrap();
        assert_eq!(
            exposed.current_entry.snapshot.recipe.layers[0].id,
            source_layer
        );
        assert_eq!(
            raw_payload(&exposed.current_entry.snapshot.recipe)
                .unwrap()
                .exposure_ev,
            1.5
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
        let request = service
            .raw_development(&initial.asset.id, payload.gains)
            .unwrap()
            .unwrap();
        let developed = RawPrepared::develop(
            request.sensor.clone(),
            request.fingerprint.clone(),
            request.gains,
            &AtomicBool::new(false),
        )
        .unwrap();
        service.install_development(&request, developed).unwrap();
        assert!(matches!(
            service
                .preview_job(&initial.asset.id, None, None, None, None)
                .unwrap()
                .source,
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
    /// LIGHTWELL_RAW_FIXTURE pointing to a private qualified NEF, RAF or DNG.
    #[test]
    #[ignore = "requires a photo-sized RAW fixture; run explicitly on the owner's Mac"]
    fn a_reopened_raw_interpretation_equals_the_decoders_again() {
        let path = PathBuf::from(std::env::var("LIGHTWELL_RAW_FIXTURE").expect("fixture path"));
        let catalog = temp("raw-reinterpretation.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let imported = service.import(&path).unwrap();
        drop(service);

        let mut service = EditorService::open(&catalog).unwrap();
        let again = service.import(&path).expect("the same interpretation");
        assert_eq!(again.asset, imported.asset);
        drop(service);

        let service = EditorService::open(&catalog).unwrap();
        let state = service.state(&imported.asset.id).unwrap();
        assert_eq!(state.asset.source, imported.asset.source);
        service
            .verified_prepared(&state.asset)
            .expect("a preparation after reopen accepts the decoded interpretation");
        drop(service);
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
            .assets()
            .unwrap()
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
            (&first.source, &second.source)
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
            first.entry.snapshot.id.clone(),
            &first.entry.snapshot.recipe,
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
        assert_eq!(
            service
                .preview_job(&asset, None, None, None, None)
                .unwrap_err()
                .kind,
            ErrorKind::SourceUnavailable
        );
        drop(service);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
