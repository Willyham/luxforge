//! The index: what Luxforge has read from the files it browses, so browsing is instant the second
//! time. **Lane A (files)** owns this module (`docs/design/catalog.md`, "Delivery plan").
//!
//! - `database.rs`: the index database — schema, format marker, open, create and discard, its
//!   revision, and the file and root rows ([`IndexDb`], [`INDEX_FORMAT`], [`index_dir`]).
//! - `volumes.rs`: the mounted volumes, which volume a path is on, camera cards and offline folders,
//!   from the platform's mount table or a fixed one a test stands in with.
//! - `exclude.rs`: what indexing lists (supported files by extension) and skips (packages, other
//!   applications' caches, hidden and system folders, Luxforge's own directories).
//! - `walk.rs`: the bounded, cancellable listing of one root, a folder at a time, following no link
//!   and crossing no volume.
//! - `read.rs`: one file's header read, bounded in bytes, never image data.
//! - `reconcile.rs`: reconciliation by signature — unchanged rows kept, changed ones read again,
//!   new ones added, vanished ones dropped, moves within a volume carried by file identity.
//! - `lane.rs`: the index lane — a coordinator thread that walks, reconciles and writes in batches
//!   (each advancing the revision and announced as one event), and two header workers on different
//!   files — with progress on the activity board. The catalog owner only schedules it
//!   (`api/owner/files.rs`).
//! - `disk.rs`: a folder's immediate subfolders for `disk.folders`, with the same exclusions.
//!
//! Not built yet: the platform's change notifications for indexed folders and mount notifications
//! for cards (TASK-005). They feed this lane what the owner already hands it: a root to list again
//! ([`lane::Work::Refresh`]) when paths under it change or a card mounts.
pub(crate) mod database;
pub(crate) mod disk;
pub(crate) mod exclude;
pub(crate) mod lane;
mod read;
pub(crate) mod reconcile;
pub(crate) mod volumes;
pub(crate) mod walk;

#[allow(
    unused_imports,
    reason = "the preview and views lanes read file rows through it"
)]
pub(crate) use database::file;
pub use database::{INDEX_FILE, INDEX_FORMAT, IndexDb, IndexOpened, PREVIEWS_DIR, index_dir};
pub(crate) use database::{upsert_file, upsert_root};
pub(crate) use volumes::volume_of;
