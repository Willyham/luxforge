//! Small user preferences outside the catalog, shared by the desktop and command clients.
//! The existing bounded, format-marked and locked JSON writer preserves unsupported documents.
use crate::{Error, capabilities::document::JsonDocument};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_BYTES: u64 = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(crate) performance_expanded: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            performance_expanded: true,
        }
    }
}

pub(crate) struct PreferenceStore(Option<JsonDocument<Preferences>>);

impl PreferenceStore {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self(dir.map(|dir| JsonDocument::new(dir, "preferences.json", MAX_BYTES, 1)))
    }

    pub(crate) fn read(&self) -> Result<Preferences, Error> {
        match &self.0 {
            Some(document) => Ok(document.read()?.unwrap_or_default()),
            None => Ok(Preferences::default()),
        }
    }

    pub(crate) fn set(&self, performance_expanded: bool) -> Result<Preferences, Error> {
        self.0
            .as_ref()
            .ok_or_else(|| Error::not_ready("no user preference directory is configured"))?
            .transact(|preferences| {
                preferences.performance_expanded = performance_expanded;
                Ok(preferences.clone())
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_testbase::paths::temp_path;

    #[test]
    fn preferences_survive_reopen_and_refuse_unsupported_data_without_rewriting_it() {
        let root = temp_path("preferences");
        let store = PreferenceStore::new(Some(root.clone()));
        assert!(store.read().unwrap().performance_expanded);
        assert!(!root.exists(), "reading defaults creates nothing");
        store.set(false).unwrap();
        assert!(
            !PreferenceStore::new(Some(root.clone()))
                .read()
                .unwrap()
                .performance_expanded
        );
        let path = root.join("preferences.json");
        for bytes in [
            b"{\"format\":2,\"performance_expanded\":true}".as_slice(),
            b"{\"format\":1,\"performance_expanded\":false,\"unknown\":1}",
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(
                store.read().unwrap_err().kind,
                crate::ErrorKind::Incompatible
            );
            assert!(store.set(true).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
