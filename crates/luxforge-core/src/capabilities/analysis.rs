//! The analysis cache (`docs/design/ai-editing.md`, "Analysis and the per-photo cache"): what a
//! module's analysis tasks compute from a photograph's repair stage (an embedding, class and subject
//! probabilities, people, depth, and the luminance guide a selection is refined against), held in
//! memory so hover, the picker and the selection tasks reuse it.
//!
//! # Shape
//!
//! One cache per catalog owner, held by the capability host and shared with every module context
//! and query. An entry is an opaque, immutable value a module stored, with the bytes it declared,
//! under an [`AnalysisKey`]: the module, the asset, the source identity, the repair stage's output
//! identity (the prefix hash), what was computed (a model resource or the guide) and its version.
//! A changed source, prefix or model version is another key, so a stale entry is never answered;
//! storing a new entry for the same module, asset and kind drops every older entry of that kind for
//! the asset, so a removal or a model update releases what it replaced at once rather than waiting
//! for eviction. Nothing here is written to a catalog: everything is recomputable.
//!
//! # Bounds
//!
//! At most [`ANALYSIS_CACHE_BYTES`] declared bytes and [`MAX_ANALYSIS_ENTRIES`] entries, least
//! recently used first out. An entry larger than the whole budget is refused `resource-limit`
//! before anything is evicted. Lookups and stores are `O(entries)` under one mutex, never held
//! across a computation: a module computes outside the cache and stores the result. The budget is
//! the one a model's machine minimum counts ([`super::descriptor::machine_floor`]).
use super::descriptor::ANALYSIS_CACHE_BYTES;
use crate::{AssetId, Error};
use serde_json::{Value, json};
use std::{
    any::Any,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

/// The most entries held at once, whatever their size: a few photographs' analyses.
pub(crate) const MAX_ANALYSIS_ENTRIES: usize = 64;

/// What one entry was computed from and what it is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AnalysisKey {
    pub(crate) module: String,
    pub(crate) asset: AssetId,
    /// The source identity (`RepairInput::source`).
    pub(crate) source: String,
    /// The repair stage's output identity (`corrections_selection_prefix`).
    pub(crate) prefix: String,
    /// What was computed: a model resource's identity, or a module's name for a derived input such
    /// as the guide.
    pub(crate) kind: String,
    /// The model resource's version, or the derived input's format.
    pub(crate) version: String,
}

struct Entry {
    key: AnalysisKey,
    value: Arc<dyn Any + Send + Sync>,
    bytes: u64,
    used: u64,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    bytes: u64,
    clock: u64,
    hits: u64,
    misses: u64,
    evicted: u64,
    replaced: u64,
}

/// The analysis cache. See the [module documentation](self).
pub(crate) struct AnalysisCache {
    budget: u64,
    state: Mutex<State>,
}

impl std::fmt::Debug for AnalysisCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        f.debug_struct("AnalysisCache")
            .field("budget", &self.budget)
            .field("bytes", &state.bytes)
            .field("entries", &state.entries.len())
            .finish()
    }
}

impl Default for AnalysisCache {
    fn default() -> Self {
        Self::new(ANALYSIS_CACHE_BYTES)
    }
}

impl AnalysisCache {
    pub(crate) fn new(budget: u64) -> Self {
        Self {
            budget,
            state: Mutex::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The entry under `key`, made the most recently used, when it holds a `T`.
    pub(crate) fn get<T: Any + Send + Sync>(&self, key: &AnalysisKey) -> Option<Arc<T>> {
        let mut state = self.lock();
        state.clock += 1;
        let clock = state.clock;
        let found = state
            .entries
            .iter_mut()
            .find(|entry| &entry.key == key)
            .and_then(|entry| {
                entry.used = clock;
                entry.value.clone().downcast::<T>().ok()
            });
        if found.is_some() {
            state.hits += 1;
        } else {
            state.misses += 1;
        }
        found
    }

    /// Store `value` of `bytes` under `key`, replacing an entry under the same key and dropping
    /// every entry of the same module, asset and kind computed from another source, prefix or
    /// version, then evicting the least recently used until it fits. Refused `resource-limit`, with
    /// nothing changed, when `bytes` exceed the whole budget.
    pub(crate) fn insert<T: Any + Send + Sync>(
        &self,
        key: AnalysisKey,
        value: Arc<T>,
        bytes: u64,
    ) -> Result<(), Error> {
        if bytes > self.budget {
            return Err(Error::resource_limit(format!(
                "an analysis of {bytes} bytes exceeds the {} byte analysis cache",
                self.budget
            ))
            .with_data(json!({"limit": "analysis-cache", "bytes": bytes, "budget": self.budget})));
        }
        let mut state = self.lock();
        let before = state.entries.len();
        let mut released = 0;
        state.entries.retain(|entry| {
            let kept = !(entry.key.module == key.module
                && entry.key.asset == key.asset
                && entry.key.kind == key.kind);
            if !kept {
                released += entry.bytes;
            }
            kept
        });
        let replaced = (before - state.entries.len()) as u64;
        state.replaced += replaced;
        state.bytes -= released;
        while state.bytes + bytes > self.budget || state.entries.len() >= MAX_ANALYSIS_ENTRIES {
            let Some(oldest) = state
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(index, _)| index)
            else {
                break;
            };
            let entry = state.entries.swap_remove(oldest);
            state.bytes -= entry.bytes;
            state.evicted += 1;
        }
        state.clock += 1;
        let used = state.clock;
        state.bytes += bytes;
        state.entries.push(Entry {
            key,
            value,
            bytes,
            used,
        });
        Ok(())
    }

    /// Drop every entry `module` stored: what unloading or removing its models does.
    pub(crate) fn forget_module(&self, module: &str) {
        let mut state = self.lock();
        let mut released = 0;
        state.entries.retain(|entry| {
            let kept = entry.key.module != module;
            if !kept {
                released += entry.bytes;
            }
            kept
        });
        state.bytes -= released;
    }

    /// Bytes held now.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> u64 {
        self.lock().bytes
    }

    /// What `module.status` reports of the cache: its budget, what it holds, and what it has
    /// answered, evicted and replaced since the owner started.
    pub(crate) fn status(&self) -> Value {
        let state = self.lock();
        json!({
            "budget_bytes": self.budget,
            "bytes": state.bytes,
            "entries": state.entries.len(),
            "max_entries": MAX_ANALYSIS_ENTRIES,
            "hits": state.hits,
            "misses": state.misses,
            "evicted": state.evicted,
            "replaced": state.replaced,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key of the asset a test names `asset`: one asset per name on each test's thread.
    fn key(asset: &str, prefix: &str, kind: &str) -> AnalysisKey {
        thread_local! {
            static ASSETS: std::cell::RefCell<std::collections::HashMap<String, AssetId>> =
                std::cell::RefCell::default();
        }
        let asset = ASSETS.with(|assets| {
            assets
                .borrow_mut()
                .entry(asset.to_owned())
                .or_insert_with(AssetId::new)
                .clone()
        });
        AnalysisKey {
            module: "m".into(),
            asset,
            source: "s".into(),
            prefix: prefix.into(),
            kind: kind.into(),
            version: "1".into(),
        }
    }

    #[test]
    fn a_stored_analysis_is_answered_under_its_key_only() {
        let cache = AnalysisCache::new(100);
        cache
            .insert(key("a", "p", "k"), Arc::new(7_u32), 10)
            .unwrap();
        assert_eq!(cache.get::<u32>(&key("a", "p", "k")).as_deref(), Some(&7));
        assert!(
            cache.get::<u32>(&key("a", "q", "k")).is_none(),
            "another prefix"
        );
        assert!(
            cache.get::<u64>(&key("a", "p", "k")).is_none(),
            "another type"
        );
        let version = AnalysisKey {
            version: "2".into(),
            ..key("a", "p", "k")
        };
        assert!(
            cache.get::<u32>(&version).is_none(),
            "another model version"
        );
        assert_eq!(cache.status()["hits"], 1);
        assert_eq!(cache.status()["misses"], 3);
    }

    #[test]
    fn a_new_analysis_replaces_the_photographs_older_one_of_its_kind() {
        let cache = AnalysisCache::new(100);
        cache
            .insert(key("a", "p", "k"), Arc::new(1_u32), 30)
            .unwrap();
        cache
            .insert(key("a", "p", "guide"), Arc::new(2_u32), 30)
            .unwrap();
        cache
            .insert(key("b", "p", "k"), Arc::new(3_u32), 30)
            .unwrap();
        // A removal changed photograph a's prefix: its old analysis of kind k goes at once.
        cache
            .insert(key("a", "q", "k"), Arc::new(4_u32), 10)
            .unwrap();
        assert!(cache.get::<u32>(&key("a", "p", "k")).is_none());
        assert!(cache.get::<u32>(&key("a", "p", "guide")).is_some());
        assert!(cache.get::<u32>(&key("b", "p", "k")).is_some());
        assert_eq!(cache.bytes(), 70);
        assert_eq!(cache.status()["replaced"], 1);
    }

    #[test]
    fn the_least_recently_used_leave_first_and_the_budget_is_never_exceeded() {
        let cache = AnalysisCache::new(100);
        for (asset, bytes) in [("a", 40), ("b", 40)] {
            cache
                .insert(key(asset, "p", "k"), Arc::new(0_u8), bytes)
                .unwrap();
        }
        // Using a makes b the least recently used.
        assert!(cache.get::<u8>(&key("a", "p", "k")).is_some());
        cache
            .insert(key("c", "p", "k"), Arc::new(0_u8), 40)
            .unwrap();
        assert!(cache.get::<u8>(&key("b", "p", "k")).is_none());
        assert!(cache.get::<u8>(&key("a", "p", "k")).is_some());
        assert_eq!(cache.bytes(), 80);
        let refused = cache
            .insert(key("d", "p", "k"), Arc::new(0_u8), 101)
            .unwrap_err();
        assert_eq!(refused.kind, crate::ErrorKind::ResourceLimit);
        assert_eq!(cache.bytes(), 80, "a refused entry evicts nothing");
        for index in 0..MAX_ANALYSIS_ENTRIES + 3 {
            cache
                .insert(key(&format!("e{index}"), "p", "k"), Arc::new(0_u8), 0)
                .unwrap();
        }
        assert_eq!(cache.status()["entries"], MAX_ANALYSIS_ENTRIES);
        cache.forget_module("m");
        assert_eq!(cache.bytes(), 0);
    }
}
