//! The library: picks and the journal of library changes, catalog folders and collections,
//! developing picks, availability and Locate, resolving missing originals, removal and batch jobs.
//! The library lane runs its jobs, developing picks among them (`docs/design/catalog.md`,
//! "Picking", "Developing picks", "The catalog", "Library changes and undo", "Missing originals",
//! "Removing").
//!
//! Every change here is one library change through [`journal::apply`]: a journal row and its
//! per-item rows in the same transaction as the change itself (`library_changes`,
//! `library_change_rows`), at most [`MAX_LIBRARY_BATCH`](crate::catalog_types::MAX_LIBRARY_BATCH)
//! items, announced as one event naming its sequence, undone by appending its inverse for the
//! calling actor's own changes and refused, naming the items, when a later change touched them. A
//! method only says what each item is to become; [`items`] reads and writes every kind of item, so
//! undo and redo revert any change the same way. The item rows' SQL is in
//! `editor/catalog_rows.rs` (`library_rows`); the owner-side handlers are in
//! `api/owner/library.rs`.
//!
//! - `journal.rs`: changes, undo and redo, the journal's pages and a change's rows.
//! - `items.rs`: each item's value, read and written.
//! - `targets.rs`: the files or photographs a method's `targets` name.
//! - `picks.rs`: picking and clearing, and the pick pages.
//! - `folders.rs`, `collections.rs` and `tree.rs`: catalog folders and moving photographs;
//!   collections, smart collections and groups; what the two trees share.
//! - `availability.rs`, `locate.rs` and `worker.rs`: where originals are, Locate, and the lane's
//!   worker thread that checks and verifies off the owner.
//! - `missing.rs` (with `missing/search.rs`): missing originals by source folder, the bounded
//!   search, and relinking what it verified.
//!
//! - `develop.rs` (with `develop/`): planning a Develop by event, the develop lane that reads each
//!   file once off the owner and commits in batches, linking and relinking, card copies, and
//!   sending back.
//! - `batch.rs`: applying a preset to, and exporting, many photographs as one job.
//! - `remove.rs`: removing photographs to Removed, putting them back, and emptying Removed.
pub(crate) mod items;
pub(crate) mod journal;
pub(crate) mod picks;
pub(crate) mod targets;

// Catalog folders and collections.
#[cfg(test)]
mod catalog_folder_tests;
/// Collections, smart collections and groups, and their members.
pub(crate) mod collections;
/// Catalog folders, and moving photographs between them.
pub(crate) mod folders;
/// What folders and collections share as named trees: names, clashes, order, a planned change.
pub(crate) mod tree;

// Availability and Locate.
pub(crate) mod availability;
pub(crate) mod locate;
/// The one scratch disk image the offline-volume tests attach at a time.
#[cfg(all(test, target_os = "macos"))]
pub(crate) mod test_disk;
pub(crate) mod worker;

// Developing picks.
/// Developing picks: planning, the develop lane's reads, committing in batches, sending back.
pub(crate) mod develop;

// Resolving missing originals.
/// What is missing, finding it in a chosen folder, and relinking what was verified.
pub(crate) mod missing;

// Removing.
/// Removing photographs, putting them back, and emptying Removed.
pub(crate) mod remove;
#[cfg(test)]
mod remove_tests;

// Batch preset and export.
/// Batch preset and export: what both share, the naming, and the report as it grows.
pub(crate) mod batch;
