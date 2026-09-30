//! The preview lane and cache. **Lane B (previews)** owns this module (`docs/design/catalog.md`,
//! "The index and previews cache", "Browsing at speed", "Architecture").
//!
//! Built:
//! - `cache.rs`: the previews cache under `<catalog>.index/previews/` and its rows in the index's
//!   `previews` table — validity by the file's signature, names from what made each file, writes
//!   through a temporary file and a rename, the loupe and large tiers' shared byte budget with
//!   least-recently-used eviction — and the reads other lanes use: [`grid_states`] (lane D's
//!   `browse.rows`) and [`cache_bytes`] (lane C's `catalog.info`).
//! - `extract.rs`: a file's grid tier, in two stages (its thumbnail, then its embedded preview), and
//!   its loupe tier, from a JPEG original or a RAW's embedded images through `luxforge-raw`, with
//!   scaled decodes through `luxforge-jpeg`, every tier upright; and the seam where a file with no
//!   usable preview is developed instead (`develop_instead`, TASK-009's).
//! - `lane.rs`: the priority queue — the loupe's look-ahead, then visible cells, then the rest of
//!   the view — deduplicated by (file, tier) and bounded, the failures it remembers, and at most two
//!   worker threads, each blocked on its channel while idle.
//!
//! The owner's side — each request's job, each client's view job and its progress on the activity
//! board, waking clients, `preview.read` — is `api/owner/previews.rs`. The lane never uses or
//! evicts the editor's one-slot source cache.
//!
//! To come in this lane: `region.rs` (the 100% region and the development fallback, TASK-009),
//! `rendered.rs` (previews of developed photographs, TASK-010) and `bracket.rs` (the brightness
//! check, a [`BracketProbe`](crate::catalog_types::BracketProbe) over decoded grid previews).
mod cache;
mod extract;
mod lane;

#[allow(
    unused_imports,
    reason = "the reads lanes C and D call as they land: grid states for browse.rows, cache bytes for catalog.info"
)]
pub(crate) use cache::{CacheBytes, cache_bytes, grid_states};
pub(crate) use cache::{Store, file_tiers, grid_rows, grids_wanted, intact, touch};
pub(crate) use lane::{
    Failures, Outcome, PREVIEW_WORKERS, Post, Queue, Task, TaskKey, WorkerEvent, Workers,
};

/// A file's grid tier's long edge at most, in pixels, as a developed photograph's
/// ([`PHOTO_GRID_SIDE`](crate::catalog_types::PHOTO_GRID_SIDE)).
pub(crate) const FILE_GRID_SIDE: u32 = 512;

#[cfg(test)]
#[path = "tests.rs"]
pub(crate) mod preview_cache;
