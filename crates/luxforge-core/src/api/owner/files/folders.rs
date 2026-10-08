//! `index.add-folder`, `index.remove-folder`, `index.folders` and `index.refresh` on the owner. A
//! path a client names is resolved on the index lane's query thread — canonicalized, checked to
//! be a folder that may be indexed, its volume read — and the call is answered on the owner once
//! it has been; an indexed folder named exactly as it was added, every indexed folder, and a card
//! the survey knows need no disk at all.
use super::{
    super::library::{change, retried},
    Call, Owner, absolute, indexed_folders, own_dirs,
    queries::{self, ask},
    start_refresh,
};
use crate::{
    Error, MutationOutcome, MutationRequest,
    api::{Origin, methods::value},
    catalog_types::{
        IndexFolderAnswer, IndexFolders, IndexSource, IndexedFolder, IndexedFolderState,
        LibraryAnswer, LibraryItem, RootKind, Volume, VolumeId,
        api::{IndexAddFolder, IndexRefresh, IndexRemoveFolder},
    },
    editor::{library_rows, now_ms, upsert_volume},
    index::{
        database,
        exclude::OwnDirs,
        lane::RootPlan,
        query::existing_folder,
        volumes::{MountSource, card_of, volume_in},
    },
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
    // The answer says whether the folder is offline, which the survey knows.
    if !queries::learned(owner) {
        return queries::wait_for_survey(owner);
    }
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(recorded(owner, answer)?);
    }
    absolute(&params.path)?;
    let (own, mounts) = (own_dirs(owner), owner.catalog.files.mounts.clone());
    let (method, origin) = (call.request.method.clone(), call.origin.clone());
    ask(owner, move || {
        let found = browsable(&params.path, &own, &mounts).and_then(|canonical| {
            let volume = volume_in(&mounts.list(), &canonical, now_ms())?;
            Ok((canonical, volume))
        });
        Box::new(move |owner| add(owner, &method, &origin, &params.mutation, found))
    })
}

/// The owner's half of `index.add-folder`, with the folder the query thread found (its canonical
/// path and volume) or why it cannot be added.
fn add(
    owner: &mut Owner,
    method: &str,
    origin: &Origin,
    mutation: &MutationRequest,
    found: Result<(PathBuf, Volume), Error>,
) -> Result<Value, Error> {
    let request = Request::new(method, mutation);
    // A retry asked while this call waited finds the change its first attempt recorded.
    if let Some(answer) = retried(owner, request)? {
        return value(recorded(owner, answer)?);
    }
    let (canonical, volume) = found?;
    let source = IndexSource::IndexedFolder {
        path: canonical.clone(),
    };
    if let Some(folder) = library_rows::indexed_folder(&owner.service.connection, &canonical)? {
        return value(IndexFolderAnswer {
            folder: state(owner, folder, false)?,
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
    unnested(owner, &canonical)?;
    let folder = IndexedFolder {
        path: canonical.clone(),
        volume_id: volume.id.clone(),
        added_ms: now_ms(),
        actor: mutation.actor.clone(),
    };
    let recorded = serde_json::to_value(&folder)
        .map_err(|error| Error::internal(format!("cannot encode an indexed folder: {error}")))?;
    let name = display_name(&canonical);
    let answer = change(owner, origin, |tx| {
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
        folder: state(owner, folder, false)?,
        deduplicated: answer.deduplicated,
        change: answer,
        job_id,
    })
}

/// The answer to a retried `index.add-folder`: the change its first attempt recorded, with the
/// folder it added.
fn recorded(owner: &Owner, answer: LibraryAnswer) -> Result<IndexFolderAnswer, Error> {
    let folder = recorded_folder(owner, &answer)?;
    let offline = queries::mounted(owner).offline(&folder.path, &folder.volume_id);
    Ok(IndexFolderAnswer {
        folder: state(owner, folder, offline)?,
        deduplicated: answer.deduplicated,
        change: answer,
        job_id: None,
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
    absolute(&params.path)?;
    if library_rows::indexed_folder(&owner.service.connection, &params.path)?.is_some() {
        return remove(owner, request, &call.origin, params.path);
    }
    // Not indexed as named: it may name one through a link, which only the disk can tell.
    let (method, origin) = (call.request.method.clone(), call.origin.clone());
    ask(owner, move || {
        let canonical = params.path.canonicalize().ok();
        Box::new(move |owner| {
            let request = Request::new(&method, &params.mutation);
            if let Some(answer) = retried(owner, request)? {
                return value(answer);
            }
            let path = indexed_as(owner, canonical)?.unwrap_or(params.path);
            remove(owner, request, &origin, path)
        })
    })
}

/// Remove the indexed folder `path` as one library change.
fn remove(
    owner: &mut Owner,
    request: Request<'_>,
    origin: &Origin,
    path: PathBuf,
) -> Result<Value, Error> {
    let name = display_name(&path);
    value(change(owner, origin, |tx| {
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
    if !queries::learned(owner) {
        return queries::wait_for_survey(owner);
    }
    let mounts = queries::mounted(owner);
    let folders = indexed_folders(owner)?
        .into_iter()
        .map(|folder| {
            let offline = mounts.offline(&folder.path, &folder.volume_id);
            state(owner, folder, offline)
        })
        .collect::<Result<_, _>>()?;
    value(IndexFolders { folders })
}

/// `index.refresh`: list a source again as a job.
pub(in crate::api) fn index_refresh(
    owner: &mut Owner,
    call: &Call<'_>,
    params: IndexRefresh,
) -> Result<Value, Error> {
    let origin = call.origin.clone();
    match params.source {
        IndexSource::AllIndexed => {
            let folders = indexed_folders(owner)?;
            let detail = match folders.len() {
                1 => folders[0].path.display().to_string(),
                count => format!("{count} indexed folders"),
            };
            let roots = folders.into_iter().map(indexed_root).collect();
            value(start_refresh(
                owner,
                IndexSource::AllIndexed,
                roots,
                detail,
                &origin,
            )?)
        }
        IndexSource::IndexedFolder { path } => {
            absolute(&path)?;
            if let Some(folder) = library_rows::indexed_folder(&owner.service.connection, &path)? {
                return refresh_indexed(owner, path, folder, &origin);
            }
            // Not indexed as named: it may name one through a link.
            ask(owner, move || {
                let canonical = path.canonicalize().ok();
                Box::new(move |owner| {
                    let named = indexed_as(owner, canonical)?.unwrap_or_else(|| path.clone());
                    let folder = library_rows::indexed_folder(&owner.service.connection, &named)?
                        .ok_or_else(|| {
                        Error::validation(format!("{} is not an indexed folder", named.display()))
                    })?;
                    refresh_indexed(owner, path, folder, &origin)
                })
            })
        }
        IndexSource::Card { volume_id } => {
            let known = queries::mounted(owner).get(&volume_id).and_then(|mounted| {
                mounted
                    .card_folder()
                    .map(|dcim| (mounted.volume.clone(), dcim))
            });
            if let Some((volume, dcim)) = known {
                return refresh_card(owner, volume_id, volume, dcim, &origin);
            }
            // Not a card the survey knows: perhaps one just inserted, which only the disk can tell.
            let mounts = owner.catalog.files.mounts.clone();
            ask(owner, move || {
                let card = card_of(&mounts.list(), &volume_id, now_ms());
                Box::new(move |owner| {
                    let (volume, dcim) = card.ok_or_else(|| {
                        Error::source_unavailable(format!("no card {volume_id} is connected"))
                    })?;
                    refresh_card(owner, volume_id, volume, dcim, &origin)
                })
            })
        }
        IndexSource::Folder { path } => {
            absolute(&path)?;
            let own = own_dirs(owner);
            let mounts = owner.catalog.files.mounts.clone();
            ask(owner, move || {
                let found = browsable(&path, &own, &mounts);
                Box::new(move |owner| {
                    let canonical = found?;
                    let detail = canonical.display().to_string();
                    // The lane keeps the kind of a folder it already lists as an indexed folder or
                    // a card.
                    let root = RootPlan {
                        path: canonical,
                        kind: RootKind::Browsed,
                        volume_id: None,
                    };
                    value(start_refresh(
                        owner,
                        IndexSource::Folder { path },
                        vec![root],
                        detail,
                        &origin,
                    )?)
                })
            })
        }
    }
}

/// List the indexed folder `folder`, named `path` in the request.
fn refresh_indexed(
    owner: &mut Owner,
    path: PathBuf,
    folder: IndexedFolder,
    origin: &Origin,
) -> Result<Value, Error> {
    let detail = folder.path.display().to_string();
    value(start_refresh(
        owner,
        IndexSource::IndexedFolder { path },
        vec![indexed_root(folder)],
        detail,
        origin,
    )?)
}

/// List the card `volume`, whose `DCIM` folder is `dcim`: the card's folder alone.
fn refresh_card(
    owner: &mut Owner,
    volume_id: VolumeId,
    volume: Volume,
    dcim: PathBuf,
    origin: &Origin,
) -> Result<Value, Error> {
    let detail = format!("the {} card", volume.label);
    value(start_refresh(
        owner,
        IndexSource::Card { volume_id },
        vec![RootPlan {
            path: dcim,
            kind: RootKind::Card,
            volume_id: Some(volume.id),
        }],
        detail,
        origin,
    )?)
}

fn indexed_root(folder: IndexedFolder) -> RootPlan {
    RootPlan {
        path: folder.path,
        kind: RootKind::Indexed,
        volume_id: Some(folder.volume_id),
    }
}

/// An existing folder a person may browse or add, off the owner: a directory that is not a
/// package, another application's cache or one of Luxforge's own directories. Answers its
/// canonical path.
fn browsable(path: &Path, own: &OwnDirs, mounts: &MountSource) -> Result<PathBuf, Error> {
    let canonical = existing_folder(path)?;
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    let home = home.map(|home| home.canonicalize().unwrap_or(home));
    if crate::catalog_types::disk::broad_folder(&canonical, home.as_deref())
        || volume_in(&mounts.list(), &canonical, now_ms())?.mount_point == canonical
    {
        return Err(Error::validation(
            "This folder is too broad to scan; open it for navigation and choose a specific subfolder",
        ));
    }
    if let Some(skip) = own.exclusions().refuse_root(&canonical) {
        return Err(Error::validation(format!(
            "{} is {} and cannot be indexed",
            path.display(),
            skip.describe()
        )));
    }
    Ok(canonical)
}

/// Refuse a folder inside or around an indexed folder, naming it with `conflict`.
fn unnested(owner: &Owner, canonical: &Path) -> Result<(), Error> {
    for folder in indexed_folders(owner)? {
        if folder.path == canonical {
            continue;
        }
        let relation = if canonical.starts_with(&folder.path) {
            "inside"
        } else if folder.path.starts_with(canonical) {
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
    Ok(())
}

/// The canonical path a request named, when the query thread could resolve it and it is indexed
/// under it.
fn indexed_as(owner: &Owner, canonical: Option<PathBuf>) -> Result<Option<PathBuf>, Error> {
    match canonical {
        Some(canonical)
            if library_rows::indexed_folder(&owner.service.connection, &canonical)?.is_some() =>
        {
            Ok(Some(canonical))
        }
        _ => Ok(None),
    }
}

/// An indexed folder as `index.folders` answers it: whether it is `offline`, what the index's last
/// listing of it found, and whether a listing the lane ran on its own left it stale.
fn state(owner: &Owner, folder: IndexedFolder, offline: bool) -> Result<IndexedFolderState, Error> {
    let (root, stale) = match queries::index(owner)? {
        Some(index) => (
            database::root(index.connection(), &folder.path)?,
            database::root_stale(index.connection(), &folder.path)?,
        ),
        None => (None, false),
    };
    let (watching, unwatched) = super::watching(owner, &folder.path);
    Ok(IndexedFolderState {
        offline,
        files: root.as_ref().and_then(|root| root.file_count),
        listed_ms: root.and_then(|root| root.listed_ms),
        watching,
        unwatched,
        stale,
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
