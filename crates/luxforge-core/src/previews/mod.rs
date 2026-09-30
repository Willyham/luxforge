//! The preview lane and cache. **Lane B (previews)** owns this module (`docs/design/catalog.md`,
//! "The index and previews cache", "Browsing at speed", "Architecture").
//!
//! It will hold a priority queue — the loupe's look-ahead, then visible cells, then the rest of the
//! view ([`PreviewPriority`](crate::catalog_types::PreviewPriority)) — that extracts embedded
//! previews through `luxforge-raw` and makes scaled or cropped JPEG decodes through
//! `luxforge-jpeg`, writing each through the atomic-file rule into `<catalog>.index/previews/`
//! and recording it in the index database's `previews` and `photo_previews` tables; the grid tier
//! kept, the loupe and large tiers under one byte budget with least-recently-used eviction; the
//! 100% region, from the embedded full-size preview or a neutral development one RAW at a time;
//! rendered grid and large previews of developed photographs from their current entry; and the
//! preview-brightness bracket check, a
//! [`BracketProbe`](crate::catalog_types::BracketProbe) over decoded grid previews. It never uses
//! or evicts the editor's one-slot source cache, and publishes its progress on the activity board.
//!
//! Planned files: `cache.rs` (keys, budget and eviction), `lane.rs` (the queue and its workers),
//! `extract.rs` (embedded previews per camera), `region.rs` (the 100% region and the development
//! fallback), `rendered.rs` (previews of developed photographs) and `bracket.rs` (the brightness
//! check).

pub(crate) mod region;
pub(crate) mod rendered;
