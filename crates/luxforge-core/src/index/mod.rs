//! The index: what Luxforge has read from the files it browses, so browsing is instant the second
//! time. **Lane A (files)** owns this module (`docs/design/catalog.md`, "Delivery plan").
//!
//! It will hold the index lane: a bounded, cancellable walk that lists the supported files of an
//! indexed folder, a card or a browsed folder, following no symbolic link out of its roots, crossing
//! no volume and skipping packages, other applications' caches, hidden and system folders and
//! Luxforge's own directories; header reads bounded in bytes that fill a
//! [`HeaderMetadata`](crate::HeaderMetadata) and never read image data; reconciliation by signature
//! (unchanged rows kept, changed ones read again, new ones added, vanished ones dropped, moves
//! within a volume carried by file identity); the platform's change notifications for indexed
//! folders and mount notifications for cards; and progress on the activity board. Two workers on
//! different files write the index database; the catalog owner is not involved.
//!
//! Landed with the contracts:
//! - `database.rs`: the index database — schema, format marker, open, create and discard, and the
//!   file and root row writers ([`IndexDb`], [`INDEX_FORMAT`], [`index_dir`]).
//! - `volume.rs`: which volume a path is on, a placeholder until the platform's volume identity.
//!
//! Planned: `walk.rs` (listing and exclusions), `header.rs` (bounded header reads, with
//! `export/metadata`'s typed accessors), `lane.rs` (the workers, reconciliation and progress),
//! `watch.rs` (FSEvents, inotify and Windows notifications) and `cards.rs` (mounted cards).
mod database;
mod volume;

pub use database::{INDEX_FILE, INDEX_FORMAT, IndexDb, IndexOpened, PREVIEWS_DIR, index_dir};
#[allow(unused_imports, reason = "catalog contracts: used as the lanes land")]
pub(crate) use database::{file, upsert_file, upsert_root};
pub(crate) use volume::volume_of;
