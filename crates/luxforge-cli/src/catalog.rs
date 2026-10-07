use crate::Paths;
use std::path::{Path, PathBuf};

/// The file the default catalog's folder holds, as a chosen catalog folder does.
pub const CATALOG_FILE: &str = "catalog.sqlite";

/// The catalog an ordinary launch opens, and the one `luxforge-ctl` attaches to: the desktop and
/// the live client decide it with this one rule, so their defaults cannot drift
/// ([preferences](../../../docs/design/preferences.md#behaviour)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSelection {
    /// The catalog file selected.
    pub path: PathBuf,
    /// The stored catalog whose folder did not exist, so the default was selected instead. The
    /// stored location is kept for the next launch.
    pub missing: Option<PathBuf>,
}

impl CatalogSelection {
    /// The explicit catalog, then the stored location when its folder exists, then the default. A
    /// stored catalog whose folder is missing, as with an unplugged drive, selects the default and
    /// is kept as [`Self::missing`]; a folder without a catalog is still selected, since the
    /// desktop creates one there. `None` when nothing names a catalog and there is no default.
    /// Selecting creates nothing.
    pub fn select(
        explicit: Option<PathBuf>,
        default: Option<PathBuf>,
        stored: Option<PathBuf>,
    ) -> Option<Self> {
        if let Some(path) = explicit {
            return Some(Self {
                path,
                missing: None,
            });
        }
        let (path, missing) = match stored {
            Some(stored) if stored.parent().is_some_and(Path::is_dir) => (stored, None),
            Some(stored) => (default?, Some(stored)),
            None => (default?, None),
        };
        Some(Self { path, missing })
    }
}

impl Paths {
    /// The catalog an ordinary launch opens with no location stored: [`CATALOG_FILE`] in the
    /// configuration directory.
    pub fn default_catalog(&self) -> PathBuf {
        self.config.join(CATALOG_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_is_the_explicit_one_the_stored_location_or_the_default() {
        let root = luxforge_testbase::paths::temp_path("catalog-selection");
        let paths = Paths::resolve(Some(&root)).unwrap();
        let default = paths.default_catalog();
        assert_eq!(default, root.join("config").join(CATALOG_FILE));
        let present = root.join("Photos").join(CATALOG_FILE);
        let missing = root.join("Unplugged").join(CATALOG_FILE);
        let select = |explicit: Option<&Path>, stored: Option<&Path>| {
            CatalogSelection::select(
                explicit.map(Path::to_path_buf),
                Some(default.clone()),
                stored.map(Path::to_path_buf),
            )
        };
        // Nothing exists yet: the stored folder is missing, so the default is selected.
        assert_eq!(
            select(None, Some(&present)),
            Some(CatalogSelection {
                path: default.clone(),
                missing: Some(present.clone())
            })
        );
        assert!(!root.exists(), "selecting creates nothing");
        std::fs::create_dir_all(present.parent().unwrap()).unwrap();
        // A stored location whose folder exists is selected, though it holds no catalog yet.
        assert_eq!(select(None, Some(&present)).unwrap().path, present);
        let named = root.join("named.sqlite");
        assert_eq!(select(Some(&named), Some(&present)).unwrap().path, named);
        assert_eq!(
            select(None, Some(&missing)).unwrap(),
            CatalogSelection {
                path: default.clone(),
                missing: Some(missing)
            }
        );
        assert_eq!(select(None, None).unwrap().path, default);
        // Nowhere to keep the default and nothing naming a catalog.
        assert_eq!(CatalogSelection::select(None, None, None), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
