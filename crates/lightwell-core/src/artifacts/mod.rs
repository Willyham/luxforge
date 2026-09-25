//! Immutable derived artifacts: content-addressed bytes that committed recipes reference by an
//! opaque identity, stored beside the catalog. See `docs/design/module-capabilities.md`.
//!
//! This module owns what needs no catalog: the identity, the metadata a module declares, the
//! verified bytes evaluation binds, the table a recipe carries them in, the bounded cache the
//! catalog owner keeps them in and the on-disk store (`store.rs`). The catalog rows, references,
//! root and the binding step live with the editor service.
mod store;

pub use store::ArtifactWriter;
pub(crate) use store::{
    ArtifactRead, Collected, Collection, MANIFEST, RootState, VerifiedArtifact, collect_files,
    object_path, read_verified, root_state,
};
#[cfg(test)]
pub(crate) mod testing;

use crate::{Error, ErrorKind, editor::SourceSignature, modules::valid_identity};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

const PREFIX: &str = "artifact-";

/// The largest artifact the host publishes, reads or verifies: 256 MiB.
pub const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
/// The most artifacts one layer may reference.
pub const MAX_LAYER_ARTIFACTS: usize = 16;
/// The verified bytes the catalog owner keeps ready for evaluation, in total: 256 MiB.
pub const PREPARED_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
/// The most artifacts the catalog owner keeps ready at once, however small they are.
pub const PREPARED_ARTIFACT_ENTRIES: usize = 4096;
/// The longest kind or colour description an artifact may declare.
const MAX_META_TEXT: usize = 64;

/// One artifact's identity, derived by the host from the SHA-256 of its bytes. Clients treat it as
/// opaque; the host relies on it naming exactly one content, so publishing the same bytes twice
/// yields the same artifact.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArtifactId(String);

impl ArtifactId {
    /// The identity of the bytes whose lowercase hexadecimal SHA-256 is `sha256`.
    pub fn for_hash(sha256: &str) -> Result<Self, Error> {
        Self::parse(format!("{PREFIX}{sha256}"))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        let valid = value.strip_prefix(PREFIX).is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if valid {
            Ok(Self(value))
        } else {
            Err(Error::new(ErrorKind::Validation, "invalid ArtifactId"))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The lowercase hexadecimal SHA-256 of the artifact's bytes.
    pub fn sha256(&self) -> &str {
        &self.0[PREFIX.len()..]
    }
}

impl std::fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for ArtifactId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ArtifactId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// What a module says about the bytes it publishes: a kind of its own choosing, optional pixel
/// dimensions and an optional colour interpretation. The host stores it with the artifact and
/// hands it back beside the bytes; it never interprets the bytes itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMeta {
    /// Lowercase ASCII letters, digits, `-` and `.`, at most 64 characters, e.g. `tint` or `mask`.
    pub kind: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// Printable ASCII, at most 64 characters, e.g. `linear-srgb`.
    #[serde(default)]
    pub colour: Option<String>,
}

impl ArtifactMeta {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let kind_valid = !self.kind.is_empty()
            && self.kind.len() <= MAX_META_TEXT
            && self.kind.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'.'
            });
        if !kind_valid {
            return Err(Error::new(
                ErrorKind::Validation,
                format!(
                    "artifact kind must be 1..={MAX_META_TEXT} lowercase letters, digits, - or ."
                ),
            ));
        }
        if self.width == Some(0) || self.height == Some(0) {
            return Err(Error::new(
                ErrorKind::Validation,
                "artifact dimensions must be positive",
            ));
        }
        if let Some(colour) = &self.colour
            && (colour.is_empty()
                || colour.len() > MAX_META_TEXT
                || !colour.bytes().all(|byte| byte.is_ascii_graphic()))
        {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("artifact colour must be 1..={MAX_META_TEXT} printable characters"),
            ));
        }
        Ok(())
    }
}

/// One published artifact as the catalog records it. A writer returns it before the catalog knows
/// about the artifact; the owner records it with [`crate::EditorService::register_artifact`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecord {
    pub id: ArtifactId,
    pub sha256: String,
    pub bytes: u64,
    pub meta: ArtifactMeta,
    /// The module whose task published it.
    pub module_id: String,
    pub created_ms: i64,
}

impl ArtifactRecord {
    /// A record is only accepted whole: its hash is its identity, its module a valid identity and
    /// its metadata within bounds.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.id.sha256() != self.sha256 {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("artifact {} does not match its hash", self.id),
            ));
        }
        if self.bytes > MAX_ARTIFACT_BYTES {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "artifact {} holds {} bytes, more than {MAX_ARTIFACT_BYTES}",
                    self.id, self.bytes
                ),
            ));
        }
        if !valid_identity(&self.module_id) {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("invalid module identity {}", self.module_id),
            ));
        }
        self.meta.validate()
    }
}

/// An artifact's verified bytes with the metadata its row records: what
/// [`crate::CapabilityModule::compile_bound`] receives. Immutable and shared: a recipe bound with it
/// holds it in its [`ArtifactTable`], so an eviction from the owner's cache never breaks an
/// evaluation of that recipe.
pub struct PreparedArtifact {
    pub id: ArtifactId,
    pub kind: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub colour: Option<String>,
    /// Bytes whose SHA-256 the host checked against `id`.
    pub bytes: Arc<[u8]>,
}

impl PreparedArtifact {
    pub(crate) fn new(id: ArtifactId, meta: &ArtifactMeta, bytes: Arc<[u8]>) -> Self {
        Self {
            id,
            kind: meta.kind.clone(),
            width: meta.width,
            height: meta.height,
            colour: meta.colour.clone(),
            bytes,
        }
    }
}

/// Up to 256 MiB of bytes are never printed; the length stands in for them.
impl std::fmt::Debug for PreparedArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedArtifact")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("colour", &self.colour)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The verified bytes of the artifacts one recipe's layers list, by identity: what compilation
/// hands [`crate::CapabilityModule::compile_bound`]. It is [`crate::Recipe::artifacts`], filled by the
/// catalog owner's binding step ([`crate::EditorService::bind_artifacts`]) where a recipe enters
/// evaluation or admission. It is never stored, and a recipe that lists no artifact — every recipe
/// without a module-published layer — carries an empty one, which costs eight bytes and no
/// allocation.
///
/// **Shared, never copied.** Cloning a recipe, which every plan, draft, preview and job does,
/// clones one pointer. The bytes live as long as some recipe bound with them, so a job owns what it
/// evaluates by owning its recipe, and an eviction from the owner's cache never breaks it.
#[derive(Clone, Debug, Default)]
pub struct ArtifactTable(Option<Arc<BTreeMap<ArtifactId, Arc<PreparedArtifact>>>>);

impl ArtifactTable {
    /// The verified bytes bound under this identity, if the recipe was bound with them.
    pub fn get(&self, id: &ArtifactId) -> Option<&Arc<PreparedArtifact>> {
        self.0.as_ref().and_then(|held| held.get(id))
    }

    pub fn is_empty(&self) -> bool {
        self.0.as_ref().is_none_or(|held| held.is_empty())
    }

    pub fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |held| held.len())
    }

    /// Every bound artifact, in identity order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<PreparedArtifact>> {
        self.0.iter().flat_map(|held| held.values())
    }

    /// Whether two tables are one shared table rather than two copies.
    #[cfg(test)]
    pub(crate) fn shares(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(held), Some(other)) => Arc::ptr_eq(held, other),
            _ => false,
        }
    }
}

/// A table of exactly these artifacts, each under its own identity.
impl FromIterator<Arc<PreparedArtifact>> for ArtifactTable {
    fn from_iter<I: IntoIterator<Item = Arc<PreparedArtifact>>>(artifacts: I) -> Self {
        let held: BTreeMap<_, _> = artifacts
            .into_iter()
            .map(|artifact| (artifact.id.clone(), artifact))
            .collect();
        Self((!held.is_empty()).then(|| Arc::new(held)))
    }
}

/// The artifacts published while one editor service is open: by its writers, from any thread, and
/// through [`crate::EditorService::register_artifact`]. No collection removes one, because the
/// catalog may be about to record it or already has. Shared by the service, its writers and its
/// collections; reopening the catalog starts an empty set.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiveArtifacts(Arc<Mutex<HashSet<ArtifactId>>>);

impl LiveArtifacts {
    pub(crate) fn mark(&self, id: &ArtifactId) {
        lock(&self.0).insert(id.clone());
    }

    pub(crate) fn contains(&self, id: &ArtifactId) -> bool {
        lock(&self.0).contains(id)
    }
}

/// The verified bytes the catalog owner keeps ready, bounded to [`PREPARED_ARTIFACT_BYTES`] and
/// [`PREPARED_ARTIFACT_ENTRIES`], least recently used first out. Each entry remembers the file
/// signature its bytes were verified against, so a changed file is a miss and never a stale hit.
#[derive(Debug, Default)]
pub(crate) struct PreparedArtifacts {
    entries: HashMap<ArtifactId, Cached>,
    /// Use order: the smallest tick is the least recently used entry.
    order: BTreeMap<u64, ArtifactId>,
    tick: u64,
    bytes: u64,
}

#[derive(Debug)]
struct Cached {
    signature: SourceSignature,
    artifact: Arc<PreparedArtifact>,
    used: u64,
}

impl PreparedArtifacts {
    /// The cached bytes when they were verified against exactly this file signature. An entry
    /// verified against another signature is dropped: the file changed since.
    pub(crate) fn get(
        &mut self,
        id: &ArtifactId,
        signature: &SourceSignature,
    ) -> Option<Arc<PreparedArtifact>> {
        let current = self.entries.get(id)?;
        if &current.signature != signature {
            self.remove(id);
            return None;
        }
        self.tick += 1;
        let entry = self.entries.get_mut(id).expect("the entry was just found");
        self.order.remove(&entry.used);
        entry.used = self.tick;
        self.order.insert(self.tick, id.clone());
        Some(entry.artifact.clone())
    }

    /// Keep these verified bytes, evicting the least recently used entries until both bounds hold.
    pub(crate) fn insert(&mut self, signature: SourceSignature, artifact: Arc<PreparedArtifact>) {
        let id = artifact.id.clone();
        self.remove(&id);
        let length = artifact.bytes.len() as u64;
        while !self.order.is_empty()
            && (self.bytes + length > PREPARED_ARTIFACT_BYTES
                || self.entries.len() >= PREPARED_ARTIFACT_ENTRIES)
        {
            let (_, oldest) = self.order.pop_first().expect("the order is not empty");
            let evicted = self.entries.remove(&oldest).expect("ordered entries exist");
            self.bytes -= evicted.artifact.bytes.len() as u64;
        }
        self.tick += 1;
        self.order.insert(self.tick, id.clone());
        self.bytes += length;
        self.entries.insert(
            id,
            Cached {
                signature,
                artifact,
                used: self.tick,
            },
        );
    }

    pub(crate) fn remove(&mut self, id: &ArtifactId) {
        if let Some(removed) = self.entries.remove(id) {
            self.order.remove(&removed.used);
            self.bytes -= removed.artifact.bytes.len() as u64;
        }
    }

    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::EditorService;

    #[test]
    fn an_artifact_identity_is_the_prefix_and_a_lowercase_sha256() {
        let hash = "a".repeat(64);
        let id = ArtifactId::for_hash(&hash).unwrap();
        assert_eq!(id.as_str(), format!("artifact-{hash}"));
        assert_eq!(id.sha256(), hash);
        for bad in [
            "artifact-".to_owned(),
            format!("artifact-{}", "A".repeat(64)),
            format!("artifact-{}", "a".repeat(63)),
            format!("layer-{}", "a".repeat(64)),
            format!("artifact-{}g", "a".repeat(63)),
        ] {
            assert!(ArtifactId::parse(bad.clone()).is_err(), "{bad}");
        }
        let json = serde_json::to_value(&id).unwrap();
        assert_eq!(serde_json::from_value::<ArtifactId>(json).unwrap(), id);
    }

    fn artifact(tag: u8, length: usize) -> Arc<PreparedArtifact> {
        let id = ArtifactId::for_hash(&format!("{tag:02x}{}", "e".repeat(62))).unwrap();
        let meta = ArtifactMeta {
            kind: "test".into(),
            width: None,
            height: None,
            colour: None,
        };
        Arc::new(PreparedArtifact::new(id, &meta, vec![tag; length].into()))
    }

    #[test]
    fn a_table_holds_exactly_what_it_was_bound_with_and_its_clones_share_it() {
        let [first, second] = [0xa1, 0xa2].map(|tag| artifact(tag, 4));
        let table: ArtifactTable = [second.clone(), first.clone()].into_iter().collect();
        assert_eq!(table.len(), 2);
        assert!(Arc::ptr_eq(table.get(&first.id).unwrap(), &first));
        assert!(Arc::ptr_eq(table.get(&second.id).unwrap(), &second));
        assert!(table.get(&artifact(0xa3, 4).id).is_none());
        let ids: Vec<&ArtifactId> = table.iter().map(|artifact| &artifact.id).collect();
        assert_eq!(ids, [&first.id, &second.id], "identity order");
        // A clone is the one table, and it keeps the bytes alive on its own.
        let clone = table.clone();
        assert!(clone.shares(&table));
        let weak = Arc::downgrade(&first);
        drop((table, first));
        assert!(weak.upgrade().is_some(), "the clone holds it");
        drop(clone);
        assert!(weak.upgrade().is_none(), "nothing else held it");
        // An empty table allocates nothing.
        let empty: ArtifactTable = std::iter::empty().collect();
        assert!(empty.is_empty() && empty.0.is_none());
        assert!(ArtifactTable::default().is_empty());
    }

    #[test]
    fn the_prepared_cache_is_bounded_by_bytes_and_evicts_the_least_recently_used() {
        let path = std::env::temp_dir().join(format!(
            "lightwell-prepared-cache-{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, b"signature").unwrap();
        let signature = EditorService::request_signature(&path).unwrap().1;
        let quarter = (PREPARED_ARTIFACT_BYTES / 4) as usize;
        let mut cache = PreparedArtifacts::default();
        let [a, b, c, d, e] = [0xb1, 0xb2, 0xb3, 0xb4, 0xb5].map(|tag| artifact(tag, quarter));
        for held in [&a, &b, &c, &d] {
            cache.insert(signature.clone(), held.clone());
        }
        assert_eq!(cache.bytes(), PREPARED_ARTIFACT_BYTES);
        // Using `a` makes `b` the least recently used, so a fifth quarter evicts `b`.
        assert!(cache.get(&a.id, &signature).is_some());
        cache.insert(signature.clone(), e.clone());
        assert_eq!(cache.len(), 4);
        assert_eq!(cache.bytes(), PREPARED_ARTIFACT_BYTES);
        assert!(cache.get(&b.id, &signature).is_none(), "evicted");
        for kept in [&a, &c, &d, &e] {
            assert!(cache.get(&kept.id, &signature).is_some());
        }
        // Bytes verified against another file signature are never a hit, and the entry is gone.
        std::fs::write(&path, b"a longer rewrite").unwrap();
        let changed = EditorService::request_signature(&path).unwrap().1;
        assert!(cache.get(&a.id, &changed).is_none());
        assert!(cache.get(&a.id, &signature).is_none());
        assert_eq!(cache.len(), 3);
        cache.clear();
        assert_eq!((cache.len(), cache.bytes()), (0, 0));
        std::fs::remove_file(path).unwrap();
    }
}
