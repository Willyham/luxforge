//! `volume.list`, `card.list` and `disk.folders` on the owner. The volume and card lists answer at
//! once from what the index lane's survey learned, matched against the mount table read now, which
//! waits on no file system; a folder's subfolders are listed on the lane's query thread. Nothing is
//! read below the folder asked about.
use super::{
    Call, Owner, absolute, own_dirs,
    queries::{self, ask},
};
use crate::{
    Error,
    api::{methods::value, params::NoParams},
    catalog_types::{
        Card, Cards, DiskFolders, Volume, VolumeState, Volumes, api::DiskFoldersParams,
    },
    editor::{folder_rows, now_ms},
    index::{
        database, disk, exclude::OwnDirs, query::existing_folder, volumes::MountSource,
        volumes::volume_in,
    },
};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The most camera bodies `card.list` names per card.
pub(crate) const MAX_CARD_CAMERAS: usize = 16;

/// `volume.list`: the mounted volumes, the startup disk first, then the volumes the catalog knows
/// that are not mounted, offline.
pub(in crate::api) fn volume_list(
    owner: &mut Owner,
    _: &Call<'_>,
    _: NoParams,
) -> Result<Value, Error> {
    if !queries::learned(owner) {
        return queries::wait_for_survey(owner);
    }
    let mounts = queries::mounted(owner);
    let now = now_ms();
    let mut volumes: Vec<VolumeState> = mounts
        .volumes()
        .map(|mounted| VolumeState {
            volume: Volume {
                last_seen_ms: now,
                ..mounted.volume.clone()
            },
            offline: false,
            card: mounted.card_folder().is_some(),
            startup: mounted.startup,
        })
        .collect();
    volumes.sort_by_key(|state| !state.startup);
    for known in folder_rows::volumes(&owner.service.connection)? {
        if !mounts.is_mounted(&known.id) {
            volumes.push(VolumeState {
                volume: known,
                offline: true,
                card: false,
                startup: false,
            });
        }
    }
    value(Volumes { volumes })
}

/// `card.list`: the mounted volumes with a `DCIM` folder, with what the index has listed of each.
pub(in crate::api) fn card_list(
    owner: &mut Owner,
    _: &Call<'_>,
    _: NoParams,
) -> Result<Value, Error> {
    if !queries::learned(owner) {
        return queries::wait_for_survey(owner);
    }
    let now = now_ms();
    let found: Vec<_> = queries::mounted(owner)
        .volumes()
        .filter_map(|mounted| {
            mounted.card_folder().map(|dcim| {
                let volume = Volume {
                    last_seen_ms: now,
                    ..mounted.volume.clone()
                };
                (volume, dcim)
            })
        })
        .collect();
    let mut cards = Vec::with_capacity(found.len());
    for (volume, dcim) in found {
        let (files, cameras) = match queries::index(owner)? {
            Some(index) => {
                let connection = index.connection();
                (
                    database::root(connection, &dcim)?.and_then(|root| root.file_count),
                    database::cameras_under(connection, &dcim, MAX_CARD_CAMERAS)?
                        .iter()
                        .map(|camera| camera.label())
                        .collect(),
                )
            }
            None => (None, Vec::new()),
        };
        cards.push(Card {
            volume,
            dcim,
            files,
            cameras,
            events: None,
        });
    }
    value(Cards { cards })
}

/// `disk.folders`: a folder's immediate subfolders without what indexing skips, bounded, listed on
/// the index lane's query thread.
pub(in crate::api) fn disk_folders(
    owner: &mut Owner,
    _: &Call<'_>,
    params: DiskFoldersParams,
) -> Result<Value, Error> {
    absolute(&params.path)?;
    let own = own_dirs(owner);
    let mounts = owner.catalog.files.mounts.clone();
    ask(owner, move || {
        let listed = subfolders(&params.path, &own, &mounts);
        Box::new(move |_| value(listed?))
    })
}

/// The subfolders of the folder at `path` (absolute), off the owner: it must be a folder that is
/// not a package, another application's cache or one of Luxforge's own directories.
fn subfolders(path: &Path, own: &OwnDirs, mounts: &MountSource) -> Result<DiskFolders, Error> {
    let canonical: PathBuf = existing_folder(path)?;
    let exclusions = own.exclusions();
    if let Some(skip) = exclusions.refuse_root(&canonical) {
        return Err(Error::validation(format!(
            "{} is {}",
            path.display(),
            skip.describe()
        )));
    }
    let volume = volume_in(&mounts.list(), &canonical, now_ms())?;
    disk::subfolders(&canonical, &volume.mount_point, &exclusions)
}
