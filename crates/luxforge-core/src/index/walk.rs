//! The walk: a bounded, cancellable listing of one root, a folder at a time.
//!
//! It lists the supported files ([`kind_of`](super::exclude::kind_of)) with their signatures, from
//! directory entries and one `lstat` per supported file and per directory, and reads nothing else.
//! It follows no symbolic link, to a file or a directory, so it never leaves its root through one
//! and a link loop is never entered; it crosses into no other volume (a directory on another device
//! is skipped); it skips what [`Exclusions`] skips; and it refuses with `resource-limit` past
//! [`MAX_INDEX_FILES`] files or [`MAX_WALK_FOLDERS`] folders. Folders deeper than
//! [`MAX_WALK_DEPTH`] below the root are not entered. A subfolder that cannot be read is counted and
//! skipped; the root itself must be readable.
//!
//! Memory is one folder's supported files and the stack of folders still to visit.
use super::exclude::{Exclusions, kind_of};
use crate::file_metadata::hidden;
use crate::{Error, SourceTag, atomic_file::file_error, catalog_types::FileSignature};
use std::{
    fs::Metadata,
    path::{Path, PathBuf},
};

/// The most supported files one listing takes; past it the listing is refused with
/// `resource-limit`. The design's first index of a large folder is 200,000 files.
pub(crate) const MAX_INDEX_FILES: usize = 500_000;
/// The most folders one listing visits; past it the listing is refused with `resource-limit`.
pub(crate) const MAX_WALK_FOLDERS: usize = 100_000;
/// How far below its root a listing goes: folders deeper are not entered.
pub(crate) const MAX_WALK_DEPTH: usize = 64;
/// How many directory entries the walk reads between checks for its cancellation.
const CHECK_EVERY: usize = 1024;

/// One listed supported file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ListedFile {
    pub name: String,
    pub kind: SourceTag,
    pub signature: FileSignature,
}

/// One folder of the walk, with its supported files in name order.
#[derive(Debug)]
pub(crate) struct ListedFolder {
    pub path: PathBuf,
    pub files: Vec<ListedFile>,
}

/// The walk's bounds, the named limits above unless a test sets smaller ones.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WalkLimits {
    pub files: usize,
    pub folders: usize,
    pub depth: usize,
}

impl Default for WalkLimits {
    fn default() -> Self {
        Self {
            files: MAX_INDEX_FILES,
            folders: MAX_WALK_FOLDERS,
            depth: MAX_WALK_DEPTH,
        }
    }
}

/// A walk of one root, folder by folder, depth first in name order.
pub(crate) struct Walk<'a> {
    root: PathBuf,
    device: Option<u64>,
    mount_point: PathBuf,
    exclusions: &'a Exclusions,
    limits: WalkLimits,
    /// Folders still to visit, with their depth below the root; the next is last.
    stack: Vec<(PathBuf, usize)>,
    files: usize,
    folders: usize,
    unreadable_folders: usize,
}

impl<'a> Walk<'a> {
    /// A walk of the directory `root` (canonical), on the volume mounted at `mount_point`.
    pub(crate) fn new(
        root: &Path,
        mount_point: &Path,
        exclusions: &'a Exclusions,
        limits: WalkLimits,
    ) -> Result<Self, Error> {
        let metadata = root
            .symlink_metadata()
            .map_err(|error| file_error("cannot read the folder", error.kind()))?;
        if !metadata.is_dir() {
            return Err(Error::validation(format!(
                "{} is not a folder",
                root.display()
            )));
        }
        Ok(Self {
            root: root.to_path_buf(),
            device: device(&metadata),
            mount_point: mount_point.to_path_buf(),
            exclusions,
            limits,
            stack: vec![(root.to_path_buf(), 0)],
            files: 0,
            folders: 0,
            unreadable_folders: 0,
        })
    }

    /// Supported files listed so far.
    pub(crate) fn files(&self) -> usize {
        self.files
    }

    /// Subfolders that could not be read and were skipped.
    pub(crate) fn unreadable_folders(&self) -> usize {
        self.unreadable_folders
    }

    /// The next folder with its supported files, or `None` once every folder has been visited.
    /// `checkpoint` is asked between folders and every [`CHECK_EVERY`] entries, and stops the walk
    /// with its error.
    pub(crate) fn next_folder(
        &mut self,
        checkpoint: &dyn Fn() -> Result<(), Error>,
    ) -> Result<Option<ListedFolder>, Error> {
        while let Some((path, depth)) = self.stack.pop() {
            checkpoint()?;
            self.folders += 1;
            if self.folders > self.limits.folders {
                return Err(Error::resource_limit(format!(
                    "{} holds more than {} folders; indexing stops there",
                    self.root.display(),
                    self.limits.folders
                )));
            }
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(error) if path == self.root => {
                    return Err(file_error("cannot read the folder", error.kind()));
                }
                Err(_) => {
                    self.unreadable_folders += 1;
                    continue;
                }
            };
            let at_volume_root = path == self.mount_point;
            let mut files = Vec::new();
            let mut folders = Vec::new();
            for (index, entry) in entries.enumerate() {
                if index % CHECK_EVERY == CHECK_EVERY - 1 {
                    checkpoint()?;
                }
                let Ok(entry) = entry else { continue };
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let name = entry.file_name();
                if file_type.is_dir() {
                    if depth + 1 > self.limits.depth {
                        continue;
                    }
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };
                    let child = entry.path();
                    if device(&metadata) != self.device
                        || self
                            .exclusions
                            .directory(&child, &name, &metadata, at_volume_root)
                            .is_some()
                    {
                        continue;
                    }
                    folders.push(child);
                } else if file_type.is_file() {
                    let Some(kind) = kind_of(&name) else { continue };
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };
                    if hidden(&name, &metadata) {
                        continue;
                    }
                    files.push(ListedFile {
                        name: name.to_string_lossy().into_owned(),
                        kind,
                        signature: FileSignature::of(&metadata),
                    });
                }
                // Symbolic links, and anything that is neither a file nor a directory, are never
                // listed or followed.
            }
            self.files += files.len();
            if self.files > self.limits.files {
                return Err(Error::resource_limit(format!(
                    "{} holds more than {} supported files; indexing stops there",
                    self.root.display(),
                    self.limits.files
                )));
            }
            files.sort_by(|a, b| a.name.cmp(&b.name));
            folders.sort();
            self.stack
                .extend(folders.into_iter().rev().map(|folder| (folder, depth + 1)));
            return Ok(Some(ListedFolder { path, files }));
        }
        Ok(None)
    }
}

#[cfg(unix)]
fn device(metadata: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev())
}

#[cfg(not(unix))]
fn device(_: &Metadata) -> Option<u64> {
    // Without a device number every directory counts as on the root's volume; a mount point
    // inside a folder is a Unix idea.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk_all(
        root: &Path,
        exclusions: &Exclusions,
        limits: WalkLimits,
    ) -> Result<Vec<PathBuf>, Error> {
        let mut walk = Walk::new(root, Path::new("/"), exclusions, limits)?;
        let mut listed = Vec::new();
        while let Some(folder) = walk.next_folder(&|| Ok(()))? {
            listed.extend(folder.files.iter().map(|file| folder.path.join(&file.name)));
        }
        Ok(listed)
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    /// Every supported file in name order, depth first; links, hidden entries, packages, caches,
    /// system and own folders, other files and a link loop are skipped; the depth limit stops
    /// descent and the file limit refuses.
    #[test]
    fn a_walk_lists_supported_files_and_skips_the_rest() {
        let dir = luxforge_testbase::paths::temp_dir("index-walk")
            .canonicalize()
            .unwrap();
        let root = dir.join("root");
        let outside = dir.join("outside");
        for name in [
            "a.jpg",
            "b.NEF",
            "notes.txt",
            ".hidden.jpg",
            "sub/c.cr3",
            "sub/deeper/d.dng",
            ".cache/e.jpg",
            "Photos Library.photoslibrary/originals/f.jpg",
            "Catalog Previews.lrdata/g.jpg",
            "shoot/CaptureOne/Cache/h.jpg",
            "shoot/i.RAF",
            "lost+found/j.jpg",
            "catalog.index/previews/k.jpg",
        ] {
            touch(&root.join(name));
        }
        touch(&outside.join("l.jpg"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&outside, root.join("link-out")).unwrap();
            symlink(outside.join("l.jpg"), root.join("linked.jpg")).unwrap();
            symlink(&root, root.join("sub/loop")).unwrap();
        }
        let exclusions = Exclusions::new(vec![root.join("catalog.index")]);
        let listed = walk_all(&root, &exclusions, WalkLimits::default()).unwrap();
        let relative: Vec<_> = listed
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            relative,
            [
                "a.jpg",
                "b.NEF",
                "shoot/i.RAF",
                "sub/c.cr3",
                "sub/deeper/d.dng"
            ]
        );

        let shallow = WalkLimits {
            depth: 1,
            ..WalkLimits::default()
        };
        assert_eq!(walk_all(&root, &exclusions, shallow).unwrap().len(), 4);

        let few = WalkLimits {
            files: 4,
            ..WalkLimits::default()
        };
        let refused = walk_all(&root, &exclusions, few).unwrap_err();
        assert_eq!(refused.kind, crate::ErrorKind::ResourceLimit);

        let cancelled = Walk::new(&root, Path::new("/"), &exclusions, WalkLimits::default())
            .unwrap()
            .next_folder(&|| Err(Error::cancelled("stop")))
            .unwrap_err();
        assert_eq!(cancelled.kind, crate::ErrorKind::Cancelled);
        assert!(
            Walk::new(
                &root.join("a.jpg"),
                Path::new("/"),
                &exclusions,
                WalkLimits::default()
            )
            .is_err()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// At a volume's root the system's own folders are skipped; elsewhere a folder of that name is
    /// a person's.
    #[test]
    fn system_folders_are_skipped_at_a_volume_root_only() {
        let dir = luxforge_testbase::paths::temp_dir("index-walk-system")
            .canonicalize()
            .unwrap();
        touch(&dir.join("Library/a.jpg"));
        touch(&dir.join("trip/Library/b.jpg"));
        let exclusions = Exclusions::default();
        let mut walk = Walk::new(&dir, &dir, &exclusions, WalkLimits::default()).unwrap();
        let mut listed = Vec::new();
        while let Some(folder) = walk.next_folder(&|| Ok(())).unwrap() {
            listed.extend(folder.files.iter().map(|file| folder.path.join(&file.name)));
        }
        assert_eq!(listed, [dir.join("trip/Library/b.jpg")]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
