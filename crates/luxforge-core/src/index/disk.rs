//! A folder's immediate subfolders, as `disk.folders` answers them for On disk: the directories in
//! it, not following links, without what indexing skips (hidden and system folders, packages, other
//! applications' caches, Luxforge's own directories), in name order, bounded. Nothing is read below
//! the folder.
use super::exclude::Exclusions;
use crate::{
    Error,
    atomic_file::file_error,
    catalog_types::{DiskFolder, DiskFolders},
};
use std::path::Path;

/// The most subfolders one answer names; past it the answer says it was cut short.
pub(crate) const MAX_DISK_FOLDERS: usize = 2_000;
/// The most directory entries one answer reads; past it the answer says it was cut short.
pub(crate) const MAX_DISK_ENTRIES: usize = 50_000;

/// The subfolders of the folder at `path` (canonical), on the volume mounted at `mount_point`.
pub(crate) fn subfolders(
    path: &Path,
    mount_point: &Path,
    exclusions: &Exclusions,
) -> Result<DiskFolders, Error> {
    let entries = std::fs::read_dir(path)
        .map_err(|error| file_error("cannot read the folder", error.kind()))?;
    let at_volume_root = path == mount_point;
    let mut folders = Vec::new();
    let mut truncated = false;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_DISK_ENTRIES || folders.len() >= MAX_DISK_FOLDERS {
            truncated = true;
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let name = entry.file_name();
        let child = entry.path();
        if exclusions
            .directory(&child, &name, &metadata, at_volume_root)
            .is_some()
        {
            continue;
        }
        folders.push(DiskFolder {
            name: name.to_string_lossy().into_owned(),
            path: child,
        });
    }
    folders.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(DiskFolders {
        path: path.to_path_buf(),
        folders,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn subfolders_are_listed_by_name_without_the_excluded() {
        let dir = luxforge_testbase::paths::temp_dir("index-disk")
            .canonicalize()
            .unwrap();
        for name in [
            "b trip",
            "A shoot",
            ".hidden",
            "Photos Library.photoslibrary",
            "Previews.lrdata",
            "catalog.index",
            "Library",
        ] {
            std::fs::create_dir_all(dir.join(name).join("inner")).unwrap();
        }
        std::fs::write(dir.join("c.jpg"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("b trip"), dir.join("link")).unwrap();
        let exclusions = Exclusions::new(vec![dir.join("catalog.index")]);
        let names = |listing: DiskFolders| -> Vec<String> {
            listing
                .folders
                .into_iter()
                .map(|folder| folder.name)
                .collect()
        };
        let listing = subfolders(&dir, Path::new("/"), &exclusions).unwrap();
        assert!(!listing.truncated);
        assert_eq!(listing.folders[0].path, dir.join("A shoot"));
        assert_eq!(names(listing), ["A shoot", "b trip", "Library"]);
        let at_root = subfolders(&dir, &dir, &exclusions).unwrap();
        assert_eq!(
            names(at_root),
            ["A shoot", "b trip"],
            "a system folder at a volume root"
        );
        assert!(subfolders(&dir.join("c.jpg"), Path::new("/"), &exclusions).is_err());
        assert!(
            subfolders(
                &PathBuf::from("/no/such/folder"),
                Path::new("/"),
                &exclusions
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
