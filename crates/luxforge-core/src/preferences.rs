//! Small user preferences outside the catalog, shared by the desktop and command clients.
//! The existing bounded, format-marked and locked JSON writer preserves unsupported documents.
use crate::{Error, capabilities::document::JsonDocument};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

const MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(crate) performance_expanded: bool,
    /// Whether an edit that sets the same control as the entry before it collapses that entry in
    /// history ([`crate::EditorService::set_auto_collapse`]). On unless the person turned it off.
    #[serde(default = "enabled")]
    pub(crate) auto_collapse_history: bool,
    /// The feature flags the person chose, by identity, exactly as stored: a value no flag claims,
    /// or one that does not fit its flag, is kept for [`crate::flags`] to report.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) flags: BTreeMap<String, Value>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            performance_expanded: true,
            auto_collapse_history: true,
            flags: BTreeMap::new(),
        }
    }
}

fn enabled() -> bool {
    true
}

/// The preferences one `preferences.set` changes; `None` leaves one as it is.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PreferenceChange {
    pub(crate) performance_expanded: Option<bool>,
    pub(crate) auto_collapse_history: Option<bool>,
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

    /// Store the preferences `change` names, leaving the rest as they were, and answer them all.
    pub(crate) fn set(&self, change: PreferenceChange) -> Result<Preferences, Error> {
        self.0
            .as_ref()
            .ok_or_else(|| Error::not_ready("no user preference directory is configured"))?
            .transact(|preferences| {
                if let Some(expanded) = change.performance_expanded {
                    preferences.performance_expanded = expanded;
                }
                if let Some(collapse) = change.auto_collapse_history {
                    preferences.auto_collapse_history = collapse;
                }
                Ok(preferences.clone())
            })
    }

    /// Store one flag's value, or remove it for `None`, leaving every other stored flag as it
    /// was. Returns whether the stored value changed; an unchanged one writes nothing.
    pub(crate) fn set_flag(&self, id: &str, value: Option<Value>) -> Result<bool, Error> {
        self.0
            .as_ref()
            .ok_or_else(|| Error::not_ready("no user preference directory is configured"))?
            .transact(|preferences| {
                let before = preferences.flags.get(id).cloned();
                match &value {
                    Some(value) => preferences.flags.insert(id.to_owned(), value.clone()),
                    None => preferences.flags.remove(id),
                };
                Ok(before != value)
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
        let defaults = store.read().unwrap();
        assert!(defaults.performance_expanded);
        assert!(
            defaults.auto_collapse_history,
            "history collapses by default"
        );
        assert!(!root.exists(), "reading defaults creates nothing");
        store
            .set(PreferenceChange {
                performance_expanded: Some(false),
                ..PreferenceChange::default()
            })
            .unwrap();
        let reopened = PreferenceStore::new(Some(root.clone())).read().unwrap();
        assert!(!reopened.performance_expanded);
        assert!(
            reopened.auto_collapse_history,
            "one preference's write leaves the others as they were"
        );
        store
            .set(PreferenceChange {
                auto_collapse_history: Some(false),
                ..PreferenceChange::default()
            })
            .unwrap();
        let reopened = PreferenceStore::new(Some(root.clone())).read().unwrap();
        assert!(!reopened.performance_expanded && !reopened.auto_collapse_history);
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
            assert!(store.set(PreferenceChange::default()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
