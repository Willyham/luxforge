//! The preview lane and cache. **Lane B (previews)** owns this module (`docs/design/catalog.md`,
//! "The index and previews cache", "Browsing at speed", "Architecture").
//!
//! Built:
//! - `cache.rs`: the previews cache under `<catalog>.index/previews/` and its rows in the index's
//!   `previews` table — validity by the file's signature, names from what made each file, writes
//!   through a temporary file and a rename, the loupe and large tiers' shared byte budget with
//!   least-recently-used eviction — and the reads other lanes use: [`grid_states`] (lane D's
//!   `browse.rows`) and [`cache_bytes`] (lane C's `catalog.info`).
//! - `extract.rs`: a file's grid tier, in two stages (its thumbnail, then its embedded preview),
//!   and its loupe tier, from a JPEG original or a RAW's embedded images through `luxforge-raw`,
//!   with scaled decodes through `luxforge-jpeg`, every tier upright; and the seam where a RAW
//!   with no usable preview is developed instead (`develop_instead`), for a visible or look-ahead
//!   task.
//! - `display.rs`: [`decode_preview`], a cached preview decoded for a client to draw at the size it
//!   needs, on the client's own worker: the desktop's Select grid's decode (lane D's).
//! - `lane.rs`: the priority queue — the loupe's look-ahead, then visible cells, then the rest of
//!   the view — deduplicated by (file, tier) and bounded, the failures and deferrals it remembers,
//!   and at most two worker threads, each blocked on its channel while idle.
//! - `region.rs`: the 100% region's domain functions, from the embedded full-size preview for that
//!   region alone or from a neutral development, one RAW at a time in the process, off the
//!   editor's cache, and the one development kept.
//! - `regions.rs`: the region worker, one thread apart from the extraction workers, that answers
//!   `preview.region` jobs one at a time and writes each answer's JPEG.
//! - `rendered.rs`: developed photographs' grid and large tiers, planned on the owner and rendered
//!   through the Fit preview's proxy path off the editor's cache, their keys and their stale rows.
//! - `renders.rs`: the render worker, one thread apart from the others, that renders one
//!   photograph at a time, writes its tiers, collects its stale rows and keeps the large tier's
//!   budget; and the discard of other renderer generations' rows.
//! - `photos.rs`: developed photographs' rows in the index's `photo_previews` and their files —
//!   rendered tiers and camera previews — what is served and what stands in meanwhile, writes,
//!   collection and the grid's states.
//! - `camera.rs`: a developed photograph's camera preview, which it shows until its first render,
//!   extracted from its original as a browsed file's tiers are, on an extraction worker.
//! - `bracket.rs`: the brightness check for brackets the metadata cannot show (TASK-008): a
//!   fingerprint of each complete grid tier, kept beside its row, and [`PreviewProbe`], a
//!   [`BracketProbe`](crate::catalog_types::BracketProbe) over the index ([`bracket_probe`]) that
//!   lane D's `browse.view` hands to `organize::group`: it reads a run's fingerprints only when
//!   organizing asks about that run.
//!
//! The owner's side — each request's job, each client's view job and its progress on the activity
//! board, waking clients, `preview.read`, the region jobs of `preview.region` with their queue and
//! answers, and the render jobs with theirs — is `api/owner/previews.rs`,
//! `api/owner/previews/regions.rs` and `api/owner/previews/renders.rs`. The lane never uses or
//! evicts the editor's one-slot source cache.
mod bracket;
mod cache;
mod camera;
mod display;
mod extract;
mod lane;
mod photos;
pub(crate) mod region;
mod regions;
pub(crate) mod rendered;
mod renders;

#[allow(
    unused_imports,
    reason = "lane D's browse.view loads the probe as it lands"
)]
pub(crate) use bracket::{PreviewProbe, bracket_probe};
#[allow(
    unused_imports,
    reason = "lanes C and D read these as they land: browse.rows and catalog.info"
)]
pub(crate) use cache::{CacheBytes, cache_bytes, grid_states};
pub(crate) use cache::{Store, file_tiers, grid_rows, grids_wanted, intact, touch};
pub(crate) use camera::{CameraSource, recorded_signature};
pub use display::{DecodedPreview, decode_preview};
#[cfg(test)]
pub(crate) use lane::DevelopHook;
pub(crate) use lane::{
    Failures, Outcome, PREVIEW_WORKERS, Post, Queue, Task, TaskKey, WorkerEvent, Workers,
};
pub(crate) use photos::{
    fallbacks, grid_rows as photo_grid_rows, rows as photo_rows, touch as touch_photo,
};
pub(crate) use regions::{
    RegionDone, RegionPost, RegionSource, RegionWork, RegionWorker, answer_path,
    remove as remove_answer,
};
pub(crate) use renders::{RENDER_QUEUE_CAPACITY, RenderDone, RenderPost, RenderWork, RenderWorker};

/// A file's grid tier's long edge at most, in pixels, as a developed photograph's
/// ([`PHOTO_GRID_SIDE`](crate::catalog_types::PHOTO_GRID_SIDE)).
pub(crate) const FILE_GRID_SIDE: u32 = 512;

#[cfg(test)]
#[path = "tests.rs"]
pub(crate) mod preview_cache;
