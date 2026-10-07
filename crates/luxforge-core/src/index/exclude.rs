//! What indexing lists and what it skips (`docs/design/catalog.md`, "Keeping up with the disk"):
//! the supported files by extension, and never macOS packages, other applications' caches and
//! previews, hidden entries, system folders or Luxforge's own directories. The walk, `disk.folders`
//! and `index.add-folder` share these rules, so what can be added is what can be listed.
use crate::{SourceTag, file_metadata::hidden};
use std::{
    ffi::OsStr,
    fs::Metadata,
    path::{Path, PathBuf},
};

/// The files indexing lists, by lowercase extension: JPEG and the RAW containers the camera
/// catalog admits.
pub(crate) const SUPPORTED_EXTENSIONS: &[(&str, SourceTag)] = &[
    ("jpg", SourceTag::Jpeg),
    ("jpeg", SourceTag::Jpeg),
    ("nef", SourceTag::Raw),
    ("arw", SourceTag::Raw),
    ("cr2", SourceTag::Raw),
    ("cr3", SourceTag::Raw),
    ("raf", SourceTag::Raw),
    ("orf", SourceTag::Raw),
    ("rw2", SourceTag::Raw),
    ("pef", SourceTag::Raw),
    ("dng", SourceTag::Raw),
];

/// Directories the Finder shows as one document or application (macOS packages), by lowercase
/// extension: applications and plug-ins, photo and media libraries (Photos, Aperture, iPhoto,
/// Lightroom's cloud library, Capture One catalogs, Final Cut, iMovie, TV, Music), documents and
/// projects. What `NSWorkspace` calls a package also includes any directory with the package bit
/// set, which only a Launch Services query can tell; this list names the kinds a photo folder
/// meets.
pub(crate) const PACKAGE_EXTENSIONS: &[&str] = &[
    "app",
    "appex",
    "bundle",
    "component",
    "framework",
    "kext",
    "plugin",
    "prefpane",
    "qlgenerator",
    "saver",
    "xpc",
    "photoslibrary",
    "photolibrary",
    "migratedphotolibrary",
    "aplibrary",
    "lrlibrary",
    "cocatalog",
    "fcpbundle",
    "imovielibrary",
    "tvlibrary",
    "musiclibrary",
    "theater",
    "band",
    "logicx",
    "key",
    "numbers",
    "pages",
    "rtfd",
    "scptd",
    "sparsebundle",
    "pkg",
    "mpkg",
    "xcodeproj",
    "xcworkspace",
    "playground",
    "docset",
];

/// Other applications' caches and previews, by lowercase directory extension: Lightroom Classic's
/// `Previews.lrdata`, `Smart Previews.lrdata` and `Helper.lrdata`.
pub(crate) const CACHE_EXTENSIONS: &[&str] = &["lrdata"];

/// Other applications' caches and previews, by directory name ignoring case: Capture One's
/// per-folder `CaptureOne` cache and settings, Synology's and QNAP's thumbnail folders, and
/// darktable's and Picasa's caches.
pub(crate) const CACHE_NAMES: &[&str] = &[
    "captureone",
    "@eadir",
    "@__thumb",
    ".dtcache",
    ".picasaoriginals",
];

/// System folders, by name ignoring case, skipped wherever they are.
pub(crate) const SYSTEM_NAMES: &[&str] = &[
    "$recycle.bin",
    "system volume information",
    "lost+found",
    ".spotlight-v100",
    ".trashes",
    ".trash",
    ".fseventsd",
    ".documentrevisions-v100",
    ".temporaryitems",
];

/// System folders, by name ignoring case, skipped at the root of a volume: macOS's, Windows's and
/// Linux's own trees.
pub(crate) const VOLUME_ROOT_SYSTEM_NAMES: &[&str] = &[
    "system",
    "library",
    "applications",
    "private",
    "usr",
    "bin",
    "sbin",
    "cores",
    "dev",
    "etc",
    "opt",
    "var",
    "tmp",
    "volumes",
    "network",
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "recovery",
    "boot",
    "proc",
    "run",
    "snap",
    "sys",
];

/// Why a directory or file is not listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Skip {
    Hidden,
    Package,
    Cache,
    System,
    /// Luxforge's own catalog, index or artifact directory.
    Own,
}

impl Skip {
    /// The reason as a sentence names it.
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Hidden => "a hidden folder",
            Self::Package => "a package (an application or a library the Finder shows as one file)",
            Self::Cache => "another application's cache",
            Self::System => "a system folder",
            Self::Own => "one of Luxforge's own directories",
        }
    }
}

/// The kind of the file named `name`, when indexing lists it.
pub(crate) fn kind_of(name: &OsStr) -> Option<SourceTag> {
    let extension = Path::new(name).extension()?.to_str()?;
    SUPPORTED_EXTENSIONS
        .iter()
        .find(|(supported, _)| extension.eq_ignore_ascii_case(supported))
        .map(|(_, kind)| *kind)
}

fn has_extension(name: &OsStr, extensions: &[&str]) -> bool {
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|listed| extension.eq_ignore_ascii_case(listed))
        })
}

fn named(name: &OsStr, names: &[&str]) -> bool {
    name.to_str()
        .is_some_and(|name| names.iter().any(|listed| name.eq_ignore_ascii_case(listed)))
}

/// Whether the directory named `name` is a package.
pub(crate) fn package(name: &OsStr) -> bool {
    has_extension(name, PACKAGE_EXTENSIONS)
}

/// Luxforge's own directories for one catalog as the owner names them, touching no file system:
/// the index directory, and the catalog, whose artifact directory is beside it. The index lane's
/// threads resolve them into [`Exclusions`].
#[derive(Clone, Debug)]
pub(crate) struct OwnDirs {
    pub index_dir: PathBuf,
    /// The catalog database's path, when it has one on disk.
    pub catalog: Option<PathBuf>,
}

impl OwnDirs {
    /// The rules for this catalog: its index and artifact directories, canonical where they
    /// exist. Resolving them reads the file system, so never on the owner.
    pub(crate) fn exclusions(&self) -> Exclusions {
        let mut own = vec![self.index_dir.clone()];
        if let Some(catalog) = &self.catalog {
            own.push(crate::editor::default_artifact_root(catalog));
        }
        Exclusions::new(
            own.into_iter()
                .map(|dir| dir.canonicalize().unwrap_or(dir))
                .collect(),
        )
    }
}

/// The rules one walk, listing or refusal applies: the fixed lists above and the directories of
/// the catalog being served.
#[derive(Clone, Debug, Default)]
pub(crate) struct Exclusions {
    /// Luxforge's own directories: the catalog's index and artifact directories.
    own: Vec<PathBuf>,
}

impl Exclusions {
    /// The rules for the catalog whose own directories are `own` (canonical where they exist).
    pub(crate) fn new(own: Vec<PathBuf>) -> Self {
        Self { own }
    }

    /// Why the directory at `path`, named `name`, is skipped, if it is. `at_volume_root` when its
    /// parent is its volume's mount point, where the system's own trees are.
    pub(crate) fn directory(
        &self,
        path: &Path,
        name: &OsStr,
        metadata: &Metadata,
        at_volume_root: bool,
    ) -> Option<Skip> {
        if named(name, SYSTEM_NAMES) || (at_volume_root && named(name, VOLUME_ROOT_SYSTEM_NAMES)) {
            Some(Skip::System)
        } else if self.is_own(path) {
            Some(Skip::Own)
        } else if package(name) {
            Some(Skip::Package)
        } else if has_extension(name, CACHE_EXTENSIONS) || named(name, CACHE_NAMES) {
            Some(Skip::Cache)
        } else if hidden(name, metadata) {
            Some(Skip::Hidden)
        } else {
            None
        }
    }

    /// Whether `path` is one of Luxforge's own directories or inside one.
    pub(crate) fn is_own(&self, path: &Path) -> bool {
        self.own.iter().any(|own| path.starts_with(own))
    }

    /// Why a folder cannot be added or browsed as a root, if it cannot: it is, or is inside, a
    /// package, another application's cache or one of Luxforge's own directories. Hidden and
    /// system folders may be chosen by hand; only a walk skips them.
    pub(crate) fn refuse_root(&self, canonical: &Path) -> Option<Skip> {
        if self.is_own(canonical) {
            return Some(Skip::Own);
        }
        canonical.components().find_map(|component| {
            let name = component.as_os_str();
            if package(name) {
                Some(Skip::Package)
            } else if has_extension(name, CACHE_EXTENSIONS) || named(name, CACHE_NAMES) {
                Some(Skip::Cache)
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_files_are_named_by_extension_in_any_case() {
        for (name, kind) in [
            ("a.jpg", Some(SourceTag::Jpeg)),
            ("a.JPEG", Some(SourceTag::Jpeg)),
            ("DSC_0001.NEF", Some(SourceTag::Raw)),
            ("x.Cr3", Some(SourceTag::Raw)),
            ("x.dng", Some(SourceTag::Raw)),
            ("x.tif", None),
            ("x.xmp", None),
            ("nef", None),
            ("x.jpg.part", None),
        ] {
            assert_eq!(kind_of(OsStr::new(name)), kind, "{name}");
        }
    }

    #[test]
    fn packages_caches_and_own_directories_are_refused_as_roots() {
        let rules = Exclusions::new(vec![PathBuf::from("/data/catalog.index")]);
        for (path, skip) in [
            ("/p/Photos Library.photoslibrary", Some(Skip::Package)),
            (
                "/p/Photos Library.photoslibrary/originals/0",
                Some(Skip::Package),
            ),
            ("/p/Lightroom/Catalog Previews.lrdata", Some(Skip::Cache)),
            ("/p/shoot/CaptureOne/Cache", Some(Skip::Cache)),
            ("/data/catalog.index/previews", Some(Skip::Own)),
            ("/data/catalog.index", Some(Skip::Own)),
            ("/data", None),
            ("/p/.hidden", None),
            ("/p/2026-09-12 Lake", None),
        ] {
            assert_eq!(rules.refuse_root(Path::new(path)), skip, "{path}");
        }
    }
}
