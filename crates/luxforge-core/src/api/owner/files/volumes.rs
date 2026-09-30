//! `volume.list`, `card.list` and `disk.folders` on the owner: the mount table read without waiting
//! on any file system, a stat per mounted volume, and one bounded directory listing. Nothing is
//! read below the folder asked about.
use super::{Call, Owner, exclusions, mount_table};
use crate::{
    Error,
    api::{methods::value, params::NoParams},
    catalog_types::{Card, Cards, VolumeState, Volumes, api::DiskFoldersParams},
    editor::folder_rows,
    index::{database, disk},
};
use serde_json::Value;

/// The most camera bodies `card.list` names per card.
pub(crate) const MAX_CARD_CAMERAS: usize = 16;

/// `volume.list`: the mounted volumes, the startup disk first, then the volumes the catalog knows
/// that are not mounted, offline.
pub(in crate::api) fn volume_list(
    owner: &mut Owner,
    _: &Call<'_>,
    _: NoParams,
) -> Result<Value, Error> {
    let table = mount_table(owner);
    let mut volumes: Vec<VolumeState> = table
        .volumes()
        .map(|mounted| VolumeState {
            volume: mounted.volume.clone(),
            offline: false,
            card: mounted.card_folder().is_some(),
            startup: mounted.startup,
        })
        .collect();
    volumes.sort_by_key(|state| !state.startup);
    for known in folder_rows::volumes(&owner.service.connection)? {
        if !table.is_mounted(&known.id) {
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
    let table = mount_table(owner);
    let found: Vec<_> = table
        .volumes()
        .filter_map(|mounted| {
            mounted
                .card_folder()
                .map(|dcim| (mounted.volume.clone(), dcim))
        })
        .collect();
    let indexed = owner.service.index_dir().join(crate::INDEX_FILE).exists();
    let mut cards = Vec::with_capacity(found.len());
    for (volume, dcim) in found {
        let (files, cameras) = if indexed {
            let index = owner.service.index()?;
            let connection = index.connection();
            (
                database::root(connection, &dcim)?.and_then(|root| root.file_count),
                database::cameras_under(connection, &dcim, MAX_CARD_CAMERAS)?
                    .iter()
                    .map(|camera| camera.label())
                    .collect(),
            )
        } else {
            (None, Vec::new())
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

/// `disk.folders`: a folder's immediate subfolders without what indexing skips, bounded.
pub(in crate::api) fn disk_folders(
    owner: &mut Owner,
    _: &Call<'_>,
    params: DiskFoldersParams,
) -> Result<Value, Error> {
    let path = &params.path;
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
    let exclusions = exclusions(owner);
    if let Some(skip) = exclusions.refuse_root(&canonical) {
        return Err(Error::validation(format!(
            "{} is {}",
            path.display(),
            skip.describe()
        )));
    }
    let volume = mount_table(owner).volume_of(&canonical)?;
    value(disk::subfolders(
        &canonical,
        &volume.mount_point,
        &exclusions,
    )?)
}
