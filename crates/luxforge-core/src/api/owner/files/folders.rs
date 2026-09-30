//! `index.add-folder`, `index.remove-folder`, `index.folders` and `index.refresh` on the owner.
use super::{
    super::library::{change, retried},
    Call, Owner, exclusions, indexed_folders, mount_table, start_refresh,
};
use crate::{
    Error, MutationOutcome,
    api::methods::value,
    catalog_types::{
        IndexFolderAnswer, IndexFolders, IndexSource, IndexedFolder, IndexedFolderState,
        LibraryAnswer, LibraryItem, RootKind,
        api::{IndexAddFolder, IndexRefresh, IndexRemoveFolder},
    },
    editor::{library_rows, now_ms, upsert_volume},
    index::{database, lane::RootPlan, volumes::MountTable},
    library::journal::{self, Desired, Request},
};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// `index.add-folder`: add a folder, with its subfolders, to the indexed folders as one library
/// change, and list it. A folder already indexed changes nothing.
pub(in crate::api) fn index_add_folder(
    owner: &mut Owner,
    call: &Call<'_>,
    params: IndexAddFolder,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        let folder = recorded_folder(owner, &answer)?;
        return value(IndexFolderAnswer {
            folder: state(owner, &mount_table(owner), folder)?,
            deduplicated: answer.deduplicated,
            change: answer,
            job_id: None,
        });
    }
    let canonical = addable(owner, &params.path)?;
    let source = IndexSource::IndexedFolder {
        path: canonical.clone(),
    };
    if let Some(folder) = library_rows::indexed_folder(&owner.service.connection, &canonical)? {
        return value(IndexFolderAnswer {
            folder: state(owner, &mount_table(owner), folder)?,
            change: LibraryAnswer {
                outcome: MutationOutcome::NoOp,
                change: None,
                items: 0,
                deduplicated: false,
            },
            job_id: owner.catalog.files.live_job(&source),
            deduplicated: false,
        });
    }
    let table = mount_table(owner);
    let volume = table.volume_of(&canonical)?;
    let folder = IndexedFolder {
        path: canonical.clone(),
        volume_id: volume.id.clone(),
        added_ms: now_ms(),
        actor: params.mutation.actor.clone(),
    };
    let recorded = serde_json::to_value(&folder)
        .map_err(|error| Error::internal(format!("cannot encode an indexed folder: {error}")))?;
    let name = display_name(&canonical);
    let answer = change(owner, &call.origin, |tx| {
        upsert_volume(tx, &volume)?;
        journal::apply(
            tx,
            request,
            vec![(
                LibraryItem::IndexedFolder {
                    path: canonical.clone(),
                },
                Desired::Value(Some(recorded)),
            )],
            |_| format!("Added {name} to indexed folders"),
        )
    })?;
    // The change reached `indexed_folders_changed`, which queued the folder's listing.
    let job_id = owner.catalog.files.live_job(&source);
    value(IndexFolderAnswer {
        folder: state(owner, &table, folder)?,
        deduplicated: answer.deduplicated,
        change: answer,
        job_id,
    })
}

/// `index.remove-folder`: forget an indexed folder as one library change. Its files leave events,
/// its rows are forgotten unless another root lists them, and nothing on disk changes. A folder
/// that is not indexed changes nothing.
pub(in crate::api) fn index_remove_folder(
    owner: &mut Owner,
    call: &Call<'_>,
    params: IndexRemoveFolder,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(answer);
    }
    let path = indexed_path(owner, &params.path)?;
    let name = display_name(&path);
    value(change(owner, &call.origin, |tx| {
        journal::apply(
            tx,
            request,
            vec![(LibraryItem::IndexedFolder { path }, Desired::Value(None))],
            |_| format!("Removed {name} from indexed folders"),
        )
    })?)
}

/// `index.folders`: the indexed folders, whether each is offline, and what its last listing found.
pub(in crate::api) fn index_folders(
    owner: &mut Owner,
    _: &Call<'_>,
    _: crate::api::params::NoParams,
) -> Result<Value, Error> {
    let table = mount_table(owner);
    let folders = indexed_folders(owner)?
        .into_iter()
        .map(|folder| state(owner, &table, folder))
        .collect::<Result<_, _>>()?;
    value(IndexFolders { folders })
}

/// `index.refresh`: list a source again as a job.
pub(in crate::api) fn index_refresh(
    owner: &mut Owner,
    call: &Call<'_>,
    params: IndexRefresh,
) -> Result<Value, Error> {
    let (roots, detail) = roots_of(owner, &params.source)?;
    value(start_refresh(
        owner,
        params.source,
        roots,
        detail,
        &call.origin,
    )?)
}

/// The roots a source names, and what the activity board says is being indexed.
fn roots_of(owner: &Owner, source: &IndexSource) -> Result<(Vec<RootPlan>, String), Error> {
    let plan = |folder: IndexedFolder| RootPlan {
        path: folder.path,
        kind: RootKind::Indexed,
        volume_id: Some(folder.volume_id),
    };
    match source {
        IndexSource::IndexedFolder { path } => {
            let path = indexed_path(owner, path)?;
            let folder = library_rows::indexed_folder(&owner.service.connection, &path)?
                .ok_or_else(|| {
                    Error::validation(format!("{} is not an indexed folder", path.display()))
                })?;
            Ok((vec![plan(folder)], path.display().to_string()))
        }
        IndexSource::AllIndexed => {
            let folders = indexed_folders(owner)?;
            let detail = match folders.len() {
                1 => folders[0].path.display().to_string(),
                count => format!("{count} indexed folders"),
            };
            Ok((folders.into_iter().map(plan).collect(), detail))
        }
        IndexSource::Card { volume_id } => {
            let table = mount_table(owner);
            let (volume, dcim) = table
                .get(volume_id)
                .and_then(|mounted| {
                    mounted
                        .card_folder()
                        .map(|dcim| (mounted.volume.clone(), dcim))
                })
                .ok_or_else(|| {
                    Error::source_unavailable(format!("no card {volume_id} is connected"))
                })?;
            Ok((
                vec![RootPlan {
                    path: dcim,
                    kind: RootKind::Card,
                    volume_id: Some(volume.id),
                }],
                format!("the {} card", volume.label),
            ))
        }
        IndexSource::Folder { path } => {
            let canonical = browsable(owner, path)?;
            // A folder already listed as an indexed folder or a card stays one.
            let kind = database::root(owner.service.index()?.connection(), &canonical)?
                .map_or(RootKind::Browsed, |root| root.kind);
            let detail = canonical.display().to_string();
            Ok((
                vec![RootPlan {
                    path: canonical,
                    kind,
                    volume_id: None,
                }],
                detail,
            ))
        }
    }
}

/// An existing folder a person may browse or add: absolute, a directory, and not a package,
/// another application's cache or one of Luxforge's own directories. Answers its canonical path.
fn browsable(owner: &Owner, path: &Path) -> Result<PathBuf, Error> {
    if !path.is_absolute() {
        return Err(Error::validation(format!(
            "{} is not an absolute path",
            path.display()
        )));
    }
    let canonical = path.canonicalize().map_err(|error| {
        crate::atomic_file::file_error(format!("cannot find {}", path.display()), error.kind())
    })?;
    if !canonical.is_dir() {
        return Err(Error::validation(format!(
            "{} is a file, not a folder",
            path.display()
        )));
    }
    if let Some(skip) = exclusions(owner).refuse_root(&canonical) {
        return Err(Error::validation(format!(
            "{} is {} and cannot be indexed",
            path.display(),
            skip.describe()
        )));
    }
    Ok(canonical)
}

/// A folder that may be added to the indexed folders: browsable, and neither inside nor around an
/// indexed folder, which `conflict` names.
fn addable(owner: &Owner, path: &Path) -> Result<PathBuf, Error> {
    let canonical = browsable(owner, path)?;
    for folder in indexed_folders(owner)? {
        if folder.path == canonical {
            continue;
        }
        let relation = if canonical.starts_with(&folder.path) {
            "inside"
        } else if folder.path.starts_with(&canonical) {
            "around"
        } else {
            continue;
        };
        return Err(Error::conflict(format!(
            "{} is {relation} the indexed folder {}",
            canonical.display(),
            folder.path.display()
        ))
        .with_data(serde_json::json!({ "folder": folder.path })));
    }
    Ok(canonical)
}

/// The indexed folder `path` names: its canonical path when it exists and is indexed under it,
/// otherwise the path as given (an offline folder cannot be resolved).
fn indexed_path(owner: &Owner, path: &Path) -> Result<PathBuf, Error> {
    if !path.is_absolute() {
        return Err(Error::validation(format!(
            "{} is not an absolute path",
            path.display()
        )));
    }
    if let Ok(canonical) = path.canonicalize()
        && library_rows::indexed_folder(&owner.service.connection, &canonical)?.is_some()
    {
        return Ok(canonical);
    }
    Ok(path.to_path_buf())
}

/// An indexed folder as `index.folders` answers it: offline when it is not there and its volume is
/// not mounted, and what the index's last listing of it found.
fn state(
    owner: &Owner,
    table: &MountTable,
    folder: IndexedFolder,
) -> Result<IndexedFolderState, Error> {
    let root = if owner.service.index_dir().join(crate::INDEX_FILE).exists() {
        database::root(owner.service.index()?.connection(), &folder.path)?
    } else {
        None
    };
    Ok(IndexedFolderState {
        offline: table.offline(&folder.path, &folder.volume_id),
        files: root.as_ref().and_then(|root| root.file_count),
        listed_ms: root.and_then(|root| root.listed_ms),
        folder,
    })
}

/// The indexed folder a retried `index.add-folder` recorded, read from its journal row.
fn recorded_folder(owner: &Owner, answer: &LibraryAnswer) -> Result<IndexedFolder, Error> {
    let sequence = answer
        .change
        .ok_or_else(|| Error::internal("a recorded change without a sequence"))?;
    journal::inspect(&owner.service.connection, sequence.0)?
        .rows
        .into_iter()
        .find_map(|row| row.after)
        .and_then(|after| serde_json::from_value(after).ok())
        .ok_or_else(|| Error::internal("the recorded change names no indexed folder"))
}

/// A folder's name as a change's label says it.
fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}
