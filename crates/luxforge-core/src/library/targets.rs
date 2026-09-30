//! What a library method's `targets` name ([`Targets`]): files by path, by index row or by the
//! caller's selection, or photographs by identity, path, index row or selection. Each is resolved
//! once, in the order named, each item once, before the change's transaction; an item that names
//! nothing is refused by name.
use crate::{
    AssetId, EditorService, Error,
    catalog_types::{
        AssetRowId, FileId, FileIdentity, FileSignature, MAX_LIBRARY_BATCH, Targets, ViewItem,
        VolumeId,
    },
};
use rusqlite::{Connection, OptionalExtension};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// A file a target list names: its path, the key every pick uses, and what the index or the file
/// itself says of it. A file that is neither listed nor readable carries its path alone, which is
/// enough to clear its pick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NamedFile {
    pub path: PathBuf,
    pub file_id: Option<FileId>,
    pub signature: Option<FileSignature>,
    /// The volume the index lists it on.
    pub volume_id: Option<VolumeId>,
}

/// The files `targets` names. `selected` answers the items selected in the caller's view; a
/// selected photograph, or a photograph named by identity, is refused, since only a file is picked.
pub(crate) fn files(
    service: &EditorService,
    targets: &Targets,
    selected: impl FnOnce() -> Result<Vec<ViewItem>, Error>,
) -> Result<Vec<NamedFile>, Error> {
    targets.check()?;
    let index = service.index()?;
    let index = index.connection();
    let mut named = HashSet::new();
    let mut files = Vec::new();
    let mut add = |file: NamedFile| {
        if named.insert(file.path.clone()) {
            files.push(file);
        }
    };
    match targets {
        Targets::Paths { paths } => {
            for path in paths {
                add(file_at(index, path)?);
            }
        }
        Targets::Files { file_ids } => {
            for &id in file_ids {
                add(listed(index, id)?);
            }
        }
        Targets::Selection => {
            let items = selected()?;
            if items.len() > MAX_LIBRARY_BATCH {
                return Err(over_limit(items.len()));
            }
            for item in items {
                match item {
                    ViewItem::File(id) => add(listed(index, id)?),
                    ViewItem::Photo(_) => {
                        return Err(Error::validation(
                            "the selection holds photographs, and only files are picked",
                        ));
                    }
                }
            }
        }
        Targets::Assets { .. } => {
            return Err(Error::validation(
                "targets name photographs, and only files are picked",
            ));
        }
    }
    Ok(files)
}

/// The photographs `targets` names, each once, in the order named: by identity; by the path of
/// their original, or the index row of that file; or the photographs selected in the caller's view.
/// One that names no photograph in the catalog is refused by name.
#[allow(
    dead_code,
    reason = "asset.move and collection.add resolve their targets here"
)]
pub(crate) fn assets(
    service: &EditorService,
    targets: &Targets,
    selected: impl FnOnce() -> Result<Vec<ViewItem>, Error>,
) -> Result<Vec<(AssetId, AssetRowId)>, Error> {
    targets.check()?;
    let catalog = &service.connection;
    let mut named = HashSet::new();
    let mut assets = Vec::new();
    let mut add = |asset: (AssetId, AssetRowId)| {
        if named.insert(asset.1) {
            assets.push(asset);
        }
    };
    match targets {
        Targets::Assets { asset_ids } => {
            let mut statement =
                catalog.prepare_cached("SELECT row_id FROM assets WHERE id = ?1")?;
            for asset in asset_ids {
                let row: Option<i64> = statement
                    .query_row([asset.as_str()], |row| row.get(0))
                    .optional()?;
                let row = row.ok_or_else(|| Error::validation(format!("unknown asset {asset}")))?;
                add((asset.clone(), AssetRowId(row)));
            }
        }
        Targets::Paths { paths } => {
            for path in paths {
                add(asset_at(catalog, path)?);
            }
        }
        Targets::Files { file_ids } => {
            let index = service.index()?;
            for &id in file_ids {
                let file = listed(index.connection(), id)?;
                add(asset_at(catalog, &file.path)?);
            }
        }
        Targets::Selection => {
            let items = selected()?;
            if items.len() > MAX_LIBRARY_BATCH {
                return Err(over_limit(items.len()));
            }
            let mut statement =
                catalog.prepare_cached("SELECT id FROM assets WHERE row_id = ?1")?;
            for item in items {
                match item {
                    ViewItem::Photo(row) => {
                        let id: Option<String> = statement
                            .query_row([row.0], |found| found.get(0))
                            .optional()?;
                        let id = id.ok_or_else(|| {
                            Error::conflict("a selected photograph is no longer in the catalog")
                        })?;
                        add((AssetId::parse(id)?, row));
                    }
                    ViewItem::File(_) => {
                        return Err(Error::validation(
                            "the selection holds files, and this names photographs",
                        ));
                    }
                }
            }
        }
    }
    Ok(assets)
}

fn over_limit(count: usize) -> Error {
    Error::resource_limit(format!(
        "{count} targets exceed the {MAX_LIBRARY_BATCH} a library change covers"
    ))
}

/// The index columns [`read_listed`] maps.
const LISTED: &str = "id, path, byte_len, modified_ns, device, inode, volume_id";

/// A file as the index lists it, from a row of [`LISTED`].
fn read_listed(row: &rusqlite::Row<'_>) -> rusqlite::Result<NamedFile> {
    let volume: String = row.get(6)?;
    Ok(NamedFile {
        path: row.get::<_, String>(1)?.into(),
        file_id: Some(FileId(row.get(0)?)),
        signature: Some(FileSignature {
            len: row.get::<_, i64>(2)? as u64,
            modified_ns: row.get(3)?,
            identity: FileIdentity::from_columns(row.get(4)?, row.get(5)?),
        }),
        volume_id: Some(VolumeId::parse(volume).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?),
    })
}

/// The file the index lists as `id`.
fn listed(index: &Connection, id: FileId) -> Result<NamedFile, Error> {
    index
        .prepare_cached(&format!("SELECT {LISTED} FROM files WHERE id = ?1"))?
        .query_row([id.0], read_listed)
        .optional()?
        .ok_or_else(|| Error::validation(format!("unknown file {}", id.0)))
}

/// The file at `path`: the index's row for it when it has one, otherwise what the file says now.
/// The path is taken in its canonical spelling when the file can be resolved, as the index lists
/// it.
fn file_at(index: &Connection, path: &Path) -> Result<NamedFile, Error> {
    if !path.is_absolute() {
        return Err(Error::validation(format!(
            "{} is not an absolute path",
            path.display()
        )));
    }
    let canonical = path.canonicalize().ok();
    let key = canonical.as_deref().unwrap_or(path);
    let listed = index
        .prepare_cached(&format!("SELECT {LISTED} FROM files WHERE path = ?1"))?
        .query_row([key.to_string_lossy()], read_listed)
        .optional()?;
    if let Some(listed) = listed {
        return Ok(listed);
    }
    let signature = match &canonical {
        Some(canonical) => {
            let metadata = canonical.metadata().map_err(|error| {
                Error::file_access(format!("cannot read {}: {}", path.display(), error.kind()))
            })?;
            if !metadata.is_file() {
                return Err(Error::validation(format!(
                    "{} is not a file",
                    path.display()
                )));
            }
            Some(FileSignature::of(&metadata))
        }
        None => None,
    };
    Ok(NamedFile {
        path: key.to_path_buf(),
        file_id: None,
        signature,
        volume_id: None,
    })
}

/// The photograph whose original is at `path`.
fn asset_at(catalog: &Connection, path: &Path) -> Result<(AssetId, AssetRowId), Error> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let found: Option<(String, i64)> = catalog
        .prepare_cached("SELECT id, row_id FROM assets WHERE canonical_locator = ?1")?
        .query_row([canonical.to_string_lossy()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let (id, row) = found.ok_or_else(|| {
        Error::validation(format!(
            "no photograph in the catalog has its original at {}",
            path.display()
        ))
    })?;
    Ok((AssetId::parse(id)?, AssetRowId(row)))
}
