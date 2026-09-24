use super::{
    AssetRecord, CachedSource, EditorService, EditorState, PreparedFile, RawDevelopment,
    SourceKind, SourceSignature,
    catalog::{catalog_error, encode, insert_entry, json_error, now_ms},
};
use crate::{
    AssetId, EntryId, Error, ErrorKind, HistoryEntry, LayerId, Snapshot, open_source_bytes,
    read_bounded_file,
    source::{PreparedSource, RawPrepared},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

impl EditorService {
    /// The catalog owner disables synchronous source misses; workers prepare these separately.
    pub(crate) fn disable_sync_source(&mut self) {
        self.allow_sync_source = false;
    }

    pub(crate) fn request_signature(path: &Path) -> Result<(PathBuf, SourceSignature), Error> {
        let canonical = path.canonicalize().map_err(|e| {
            Error::new(
                ErrorKind::FileAccess,
                format!("cannot resolve source: {}", e.kind()),
            )
        })?;
        let metadata = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        if !metadata.is_file() {
            return Err(Error::new(
                ErrorKind::UnsupportedInput,
                "expected a regular file",
            ));
        }
        Ok((canonical.clone(), source_signature(&canonical, &metadata)))
    }

    /// Read, hash and decode from the same bounded, stable read-only file handle on a worker.
    pub(crate) fn prepare_file(path: &Path) -> Result<PreparedFile, Error> {
        Self::prepare_file_cancel(path, &AtomicBool::new(false))
    }

    pub(crate) fn prepare_file_cancel(
        path: &Path,
        cancel: &AtomicBool,
    ) -> Result<PreparedFile, Error> {
        let canonical = path.canonicalize().map_err(|e| {
            Error::new(
                ErrorKind::FileAccess,
                format!("cannot resolve source: {}", e.kind()),
            )
        })?;
        let mut file = File::open(&canonical)
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        let handle_before = file
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        let path_before = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        let signature = source_signature(&canonical, &handle_before);
        if signature != source_signature(&canonical, &path_before) {
            return Err(Error::new(
                ErrorKind::Conflict,
                "source changed before preparation",
            ));
        }
        let bytes = read_bounded_file(&mut file)?;
        let (source, fingerprint) = if bytes.starts_with(&[0xff, 0xd8]) {
            let image = open_source_bytes(bytes)?;
            let fingerprint = image.fingerprint.clone();
            (PreparedSource::Jpeg(image), fingerprint)
        } else {
            let fingerprint = format!("{:x}", Sha256::digest(&bytes));
            let raw = RawPrepared::decode(bytes, fingerprint.clone(), cancel)?;
            (PreparedSource::Raw(raw), fingerprint)
        };
        let handle_after = file
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        let path_after = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        if signature != source_signature(&canonical, &handle_after)
            || signature != source_signature(&canonical, &path_after)
        {
            return Err(Error::new(
                ErrorKind::Conflict,
                "source changed during preparation",
            ));
        }
        Ok(PreparedFile {
            canonical,
            signature,
            source,
            fingerprint,
        })
    }

    pub(crate) fn known_fingerprint(&self, path: &Path) -> Result<Option<String>, Error> {
        let (canonical, signature) = Self::request_signature(path)?;
        self.connection.query_row(
            "SELECT fingerprint FROM assets WHERE canonical_locator=?1 OR file_identity=?2 LIMIT 1",
            params![canonical.to_string_lossy(), signature.file_identity],
            |row| row.get(0),
        ).optional().map_err(catalog_error)
    }

    /// A repeated import can reuse the one verified immutable decode without a new worker job.
    pub(crate) fn cached_import(&self, path: &Path) -> Result<Option<EditorState>, Error> {
        let canonical = path.canonicalize().map_err(|e| {
            Error::new(
                ErrorKind::FileAccess,
                format!("cannot resolve source: {}", e.kind()),
            )
        })?;
        let metadata = canonical
            .metadata()
            .map_err(|e| Error::new(ErrorKind::FileAccess, e.kind().to_string()))?;
        let signature = source_signature(&canonical, &metadata);
        let id = self
            .connection
            .query_row(
                "SELECT id FROM assets WHERE canonical_locator=?1 OR file_identity=?2 LIMIT 1",
                params![canonical.to_string_lossy(), signature.file_identity],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(catalog_error)?;
        let Some(id) = id else { return Ok(None) };
        let state = self.state(&AssetId::parse(id)?)?;
        if state.asset.file_identity != signature.file_identity
            || state.asset.byte_len != signature.byte_len
        {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
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
        let needs_development =
            cached && self.raw_development(asset_id, Some(&entry.id))?.is_some();
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

    pub(crate) fn raw_development(
        &self,
        asset_id: &AssetId,
        entry_id: Option<&EntryId>,
    ) -> Result<Option<RawDevelopment>, Error> {
        let state = self.state(asset_id)?;
        let entry = match entry_id {
            Some(id) => self.entry(asset_id, id)?,
            None => state.current_entry,
        };
        validate_source_recipe(&state.asset, &entry.snapshot.recipe)?;
        let cache = self.source_cache.borrow();
        let Some(cached) = cache.as_ref().filter(|cached| cached.asset_id == *asset_id) else {
            return Ok(None);
        };
        let PreparedSource::Raw(raw) = &cached.source else {
            return Ok(None);
        };
        let payload = raw_payload(&entry.snapshot.recipe)?;
        let gains = match payload.wb_mode {
            crate::WhiteBalanceMode::AsShot => raw.sensor.metadata().as_shot_gains,
            crate::WhiteBalanceMode::Custom => payload.gains,
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
            return Err(Error::new(
                ErrorKind::Conflict,
                "RAW development identity changed",
            ));
        }
        let current = Self::request_signature(&state.asset.locator)?.1;
        if current != request.signature {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
                "original source changed during RAW development",
            ));
        }
        let mut cache = self.source_cache.borrow_mut();
        let Some(cached) = cache.as_mut().filter(|cached| {
            cached.asset_id == request.asset_id && cached.signature == request.signature
        }) else {
            return Err(Error::new(
                ErrorKind::Conflict,
                "RAW source cache was replaced during development",
            ));
        };
        let PreparedSource::Raw(previous) = &cached.source else {
            return Err(Error::new(
                ErrorKind::Incompatible,
                "RAW development targeted a JPEG source",
            ));
        };
        if !Arc::ptr_eq(&previous.sensor, &request.sensor) {
            return Err(Error::new(
                ErrorKind::Conflict,
                "RAW mosaic changed during development",
            ));
        }
        cached.source = PreparedSource::Raw(developed);
        Ok(state)
    }

    pub(crate) fn cached_state(&self, asset_id: &AssetId) -> Result<Option<EditorState>, Error> {
        let state = self.state(asset_id)?;
        let metadata = state.asset.locator.metadata().map_err(|_| {
            Error::new(
                ErrorKind::SourceUnavailable,
                "original source is unavailable",
            )
        })?;
        let signature = source_signature(&state.asset.locator, &metadata);
        if signature.file_identity != state.asset.file_identity
            || signature.byte_len != state.asset.byte_len
        {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
                "original source fingerprint changed",
            ));
        }
        let cache = self.source_cache.borrow();
        Ok(cache
            .as_ref()
            .filter(|cached| cached.asset_id == state.asset.id && cached.signature == signature)
            .map(|_| state))
    }

    /// Direct service clients may prepare synchronously; the API owner never calls this path.
    pub fn import(&mut self, path: &Path) -> Result<EditorState, Error> {
        self.import_prepared(Self::prepare_file(path)?)
            .map(|(state, _)| state)
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
            .map_err(|e| Error::new(ErrorKind::SourceUnavailable, e.kind().to_string()))?;
        if source_signature(&canonical, &current) != signature {
            return Err(Error::new(
                ErrorKind::Conflict,
                "source changed before import completion",
            ));
        }
        let identity = signature.file_identity.clone();
        let canonical_text = canonical.to_string_lossy().into_owned();
        if let Some(existing) = self
            .connection
            .query_row(
                "SELECT id FROM assets WHERE canonical_locator=?1 OR file_identity=?2 LIMIT 1",
                params![canonical_text, identity],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(catalog_error)?
        {
            let state = self.state(&AssetId::parse(existing)?)?;
            if state.asset.fingerprint != fingerprint {
                return Err(Error::new(
                    ErrorKind::SourceUnavailable,
                    "original source fingerprint changed",
                ));
            }
            let interpretation = match source.metadata() {
                None => SourceKind::Jpeg,
                Some(metadata) => SourceKind::Raw {
                    metadata: serde_json::to_value(metadata)
                        .map_err(|e| json_error("RAW metadata", e))?,
                },
            };
            if !source_kinds_equal(&state.asset.source, &interpretation)? {
                return Err(Error::new(
                    ErrorKind::Incompatible,
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
        let source_kind = match source.metadata() {
            None => SourceKind::Jpeg,
            Some(metadata) => SourceKind::Raw {
                metadata: serde_json::to_value(metadata)
                    .map_err(|e| json_error("RAW metadata", e))?,
            },
        };
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
        let entry = HistoryEntry {
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
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(catalog_error)?;
        tx.execute(
            "INSERT INTO assets VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                asset.id.as_str(),
                asset.source_root.to_string_lossy(),
                asset.locator.to_string_lossy(),
                canonical_text,
                asset.file_identity,
                asset.fingerprint,
                i64::try_from(asset.byte_len).map_err(|_| Error::new(
                    ErrorKind::ResourceLimit,
                    "source length exceeds catalog range"
                ))?,
                i64::from(asset.width),
                i64::from(asset.height),
                encode(&asset.source)?,
            ],
        )
        .map_err(catalog_error)?;
        insert_entry(&self.registry, &tx, &self.artifact_root, &entry)?;
        tx.execute(
            "INSERT INTO asset_state VALUES (?1,?2,0,'[]')",
            params![asset.id.as_str(), entry.id.as_str()],
        )
        .map_err(catalog_error)?;
        tx.commit().map_err(catalog_error)?;
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
        let before = asset.locator.metadata().map_err(|_| {
            Error::new(
                ErrorKind::SourceUnavailable,
                "original source is unavailable",
            )
        })?;
        let signature = source_signature(&asset.locator, &before);
        let raw_source = matches!(&asset.source, SourceKind::Raw { .. });
        let max_source_bytes = if raw_source {
            lightwell_raw::MAX_SOURCE_BYTES as u64
        } else {
            crate::source::MAX_JPEG_BYTES as u64
        };
        if signature.byte_len != asset.byte_len
            || signature.byte_len > max_source_bytes
            || signature.file_identity != asset.file_identity
        {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
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
            return Err(Error::new(
                ErrorKind::PreparationRequired,
                "source preparation required",
            ));
        }
        let prepared = Self::prepare_file(&asset.locator)?;
        if prepared.signature != signature || prepared.fingerprint != asset.fingerprint {
            return Err(Error::new(
                ErrorKind::SourceUnavailable,
                "original source fingerprint changed",
            ));
        }
        match (&asset.source, &prepared.source) {
            (SourceKind::Jpeg, PreparedSource::Jpeg(_)) => {}
            (SourceKind::Raw { .. }, PreparedSource::Raw(raw))
                if source_kinds_equal(
                    &asset.source,
                    &SourceKind::Raw {
                        metadata: serde_json::to_value(raw.sensor.metadata())
                            .map_err(|e| json_error("RAW metadata", e))?,
                    },
                )? => {}
            _ => {
                return Err(Error::new(
                    ErrorKind::Incompatible,
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
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    "JPEG recipe contains a RAW source layer",
                ));
            }
        }
        SourceKind::Raw { ref metadata } => {
            let payload = raw_payload(recipe)?;
            let stored = parse_raw_interpretation(metadata, "RAW interpretation")?;
            if payload.as_shot_gains != stored.as_shot_gains || payload.cam_xyz != stored.cam_xyz {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    "RAW source layer calibration differs from original",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn parse_raw_interpretation(
    metadata: &Value,
    context: &str,
) -> Result<lightwell_raw::RawMetadata, Error> {
    let parsed: lightwell_raw::RawMetadata =
        serde_json::from_value(metadata.clone()).map_err(|e| json_error(context, e))?;
    let is_dng = parsed.mode.requires_dng_corrections();
    if (is_dng
        && (!metadata
            .as_object()
            .is_some_and(|fields| fields.contains_key("dng_corrections"))
            || parsed.dng_corrections.is_none()))
        || (!is_dng && parsed.dng_corrections.is_some())
    {
        return Err(Error::new(
            ErrorKind::Incompatible,
            format!("{context}: RAW correction record differs from mode"),
        ));
    }
    Ok(parsed)
}

/// Compare typed camera interpretation after JSON roundtrip. A stored f32 has a shortest decimal
/// encoding while a fresh serde_json::Value can hold its exact f64 widening; comparing those
/// Values directly can reject an unchanged original on reopen (seen on the Fuji corpus file).
/// Parsing back to RawMetadata preserves strict field equality at the native f32 precision.
fn source_kinds_equal(left: &SourceKind, right: &SourceKind) -> Result<bool, Error> {
    match (left, right) {
        (SourceKind::Jpeg, SourceKind::Jpeg) => Ok(true),
        (SourceKind::Raw { metadata: a }, SourceKind::Raw { metadata: b }) => {
            let a = parse_raw_interpretation(a, "stored RAW interpretation")?;
            let b = parse_raw_interpretation(b, "RAW interpretation")?;
            Ok(
                serde_json::to_value(a).map_err(|e| json_error("stored RAW interpretation", e))?
                    == serde_json::to_value(b).map_err(|e| json_error("RAW interpretation", e))?,
            )
        }
        _ => Ok(false),
    }
}

/// Whether an evaluation may approximate a RAW white balance the developed planes do not hold.
///
/// [`Self::DraftPreview`] is taken in exactly one place, [`EditorService::preview_job`] for an
/// open draft. Every other evaluation — a committed or historical preview, `render_entry` and so
/// every export, `sample_entry` and `sample_draft` and so the readout and `render.sample`,
/// `analysis_plan`, and the stage context every plan and query is answered from — is
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
    let required = |detail: String| Error::new(ErrorKind::PreparationRequired, detail);
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

fn raw_payload(recipe: &crate::Recipe) -> Result<crate::RawPayload, Error> {
    let Some(layer) = recipe.layers.first() else {
        return Err(Error::new(
            ErrorKind::Incompatible,
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
        return Err(Error::new(
            ErrorKind::Incompatible,
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
    use crate::{Draft, PreviewSource, render};
    use serde_json::Map;

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
        let fresh = SourceKind::Raw {
            metadata: serde_json::to_value(&metadata).unwrap(),
        };
        let stored: SourceKind =
            serde_json::from_str(&serde_json::to_string(&fresh).unwrap()).unwrap();
        assert!(source_kinds_equal(&stored, &fresh).unwrap());
        for field in ["backend", "default_crop", "cam_xyz"] {
            let mut changed = fresh.clone();
            if let SourceKind::Raw { metadata } = &mut changed {
                match field {
                    "backend" => metadata["backend"] = json!("other backend"),
                    "default_crop" => metadata["default_crop"]["x"] = json!(1),
                    "cam_xyz" => metadata["cam_xyz"][0][0] = json!(0.5),
                    _ => unreachable!(),
                }
            }
            assert!(!source_kinds_equal(&stored, &changed).unwrap(), "{field}");
        }
        let mut unknown = fresh;
        if let SourceKind::Raw { metadata } = &mut unknown {
            metadata["unexpected"] = json!(true);
        }
        assert!(source_kinds_equal(&stored, &unknown).is_err());
        if let SourceKind::Raw { metadata } = &stored {
            assert!(metadata.get("dng_corrections").is_none());
        }
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
        let dng = SourceKind::Raw {
            metadata: serde_json::to_value(dng).unwrap(),
        };
        let dng_stored: SourceKind =
            serde_json::from_str(&serde_json::to_string(&dng).unwrap()).unwrap();
        assert!(source_kinds_equal(&dng_stored, &dng).unwrap());
        let mut changed_correction = dng.clone();
        if let SourceKind::Raw { metadata } = &mut changed_correction {
            metadata["dng_corrections"]["applied"][0]["payload_sha256"] = json!("changed");
        }
        assert!(!source_kinds_equal(&dng_stored, &changed_correction).unwrap());
        let mut missing_correction = dng.clone();
        if let SourceKind::Raw { metadata } = &mut missing_correction {
            metadata["dng_corrections"] = Value::Null;
        }
        assert_eq!(
            source_kinds_equal(&missing_correction, &dng)
                .unwrap_err()
                .kind,
            ErrorKind::Incompatible
        );
        let mut absent_correction = dng.clone();
        if let SourceKind::Raw { metadata } = &mut absent_correction {
            metadata.as_object_mut().unwrap().remove("dng_corrections");
        }
        assert_eq!(
            source_kinds_equal(&absent_correction, &dng)
                .unwrap_err()
                .kind,
            ErrorKind::Incompatible
        );
        assert!(!source_kinds_equal(&stored, &SourceKind::Jpeg).unwrap());

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
        let mut incompatible_asset = asset.clone();
        if let (
            SourceKind::Raw { metadata },
            SourceKind::Raw {
                metadata: dng_metadata,
            },
        ) = (&mut incompatible_asset.source, &dng)
        {
            metadata["dng_corrections"] = dng_metadata["dng_corrections"].clone();
        }
        assert_eq!(
            validate_source_recipe(&incompatible_asset, &snapshot.recipe)
                .unwrap_err()
                .kind,
            ErrorKind::Incompatible
        );
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
    /// the stage context an action is planned in — stays `preparation-required` until the mosaic is
    /// redeveloped. Run with LIGHTWELL_RAW_FIXTURE pointing to a private qualified NEF, RAF or DNG.
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
            SourceKind::Raw { metadata } => {
                parse_raw_interpretation(metadata, "RAW interpretation").unwrap()
            }
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
        // Planning a pixel-stage module's action answers from the stage context, which is strict
        // too. (A RAW source action plans without pixels, so it would not reach it.)
        assert!(is_required(
            service
                .apply_action(
                    &asset,
                    mutation(result.revision, "basic"),
                    "set-basic",
                    json!({"exposure": 0.5}),
                )
                .unwrap_err()
        ));
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
        assert_eq!(
            service
                .preview_job(&initial.asset.id, None, None, None, None)
                .unwrap_err()
                .kind,
            ErrorKind::PreparationRequired
        );
        let request = service
            .raw_development(&initial.asset.id, None)
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
