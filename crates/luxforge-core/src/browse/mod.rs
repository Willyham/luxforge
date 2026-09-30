//! Browse views, facets and selection. **Lane D (views and desktop)** owns this module
//! (`docs/design/catalog.md`, "Views on the owner", API `browse.*` and `event.list`).
//!
//! A view is one client's ordered list of files or photographs: a [`ViewQuery`] evaluated over the
//! index (files) or the catalog (photographs) into [`ViewItem`]s, 16 bytes each, which the owner
//! holds for that client (`api/owner/views.rs`) with the [`GroupLayout`] its summary carries.
//! Queries are structured JSON, never SQL text: every statement here is a fixed string with bound
//! parameters.
//!
//! Evaluation ([`evaluate`]) is one pass over compact columns of the source's rows — no query per
//! row — into [`FrameFacts`] with their folders and bodies interned once, then:
//!
//! 1. **Moments without a pick** is decided per frame, over the whole source, before any other
//!    condition: the source's frames are ordered and grouped Day › Camera › Moment with the query's
//!    thresholds, and a frame is left out when it is picked or its moment has a picked frame. So it
//!    is a property of the frame as shot, whatever the other conditions, the sort or the grouping,
//!    and a facet count still equals the view it predicts.
//! 2. Every other condition is a predicate of one item, combined with AND.
//! 3. The capture-time sort orders with `organize::order` under the query's grouping; the other
//!    sorts order by their key and then the declared tie-breaks (capture time earliest first with
//!    undated last, the file name ignoring case, then the path for files and the photograph's row
//!    for photographs); `descending` reverses the key only.
//! 4. `organize::group` lays the ordered view out under
//!    [`ViewQuery::effective_grouping`](crate::catalog_types::ViewQuery::effective_grouping).
//!
//! A view is stamped with the catalog's latest library change and the index revision read before
//! its rows ([`ViewStamp`]), so a change made while it was read marks it stale rather than hiding.
//!
//! Files: `candidates.rs` (reading a source's rows), `filter.rs` (a filter as per-item predicates),
//! `view.rs` (evaluation, ordering and the held item list), `rows.rs` (windows), `facets.rs`,
//! `select.rs` (the selection and its carry-over), `events.rs` (events and `event.list`) and
//! `previews.rs` (the preview-state seam to lane B).

mod candidates;
mod events;
mod facets;
mod filter;
mod previews;
mod rows;
mod select;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
mod view;

pub(crate) use events::{EventCache, event_list};
pub(crate) use facets::facets;
pub(crate) use rows::rows;
pub(crate) use select::{SelectRequest, carry_over, select, selected_items};
pub(crate) use view::{Context, View, evaluate};

use crate::{
    EditorService, Error,
    catalog_types::{BrowseSession, FileId, LocalDay, PlaceNames, ViewSource, ViewStamp},
};
use rusqlite::{Connection, OptionalExtension};
use std::cmp::Ordering;

/// The gazetteer file places and event names come from: the bundled offline one (P4). Its index is
/// built by the first lookup, which the index lane makes from a worker after committing positioned
/// files, so the owner normally finds it built. A developed photograph's place is the one its
/// capture row records.
pub(crate) fn places() -> &'static dyn PlaceNames {
    &crate::organize::Gazetteer
}

/// The index's revision: the integer under `index_meta`'s `revision` key, which the index lane
/// increments in the same transaction as every write batch; 0 when it has never written one.
pub(crate) fn index_revision(index: &Connection) -> Result<u64, Error> {
    let revision: Option<i64> = index
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM index_meta WHERE key = 'revision'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(revision.unwrap_or(0).max(0) as u64)
}

/// The library change and index revision a view evaluated now would be stamped with: one read of
/// each database.
pub(crate) fn current_stamp(service: &EditorService) -> Result<ViewStamp, Error> {
    let library_sequence = service.library_sequence()?;
    let index_revision = index_revision(service.index()?.connection())?;
    Ok(ViewStamp {
        library_sequence,
        index_revision,
    })
}

/// Mark the session's view stale when the catalog or the index has moved on since it was
/// evaluated; a stale view stays stale until it is evaluated again. Answers whether it is stale.
/// A session without a view reads nothing.
pub(crate) fn refresh_stale(
    service: &EditorService,
    browse: &mut BrowseSession,
) -> Result<bool, Error> {
    if let Some(stamp) = browse.evaluated_at
        && !browse.stale
    {
        browse.stale = current_stamp(service)? != stamp;
    }
    Ok(browse.stale)
}

/// The files a source over files covers — an event's across its folders and cards, a folder's
/// (with its subfolders when asked) or a card's — ascending by row, whatever a filter would say.
/// Refused for a source over photographs, and for an event, folder or card the index does not
/// know. An event is resolved under the default thresholds, computing the events afresh.
#[allow(
    dead_code,
    reason = "the seam lane C's picks by source call; nothing in this lane needs it"
)]
pub(crate) fn source_files(
    service: &EditorService,
    source: &ViewSource,
) -> Result<Vec<FileId>, Error> {
    candidates::source_files(service, &mut EventCache::default(), source)
}

/// Two names compared ignoring case, allocating nothing: the order file names sort and tie-break
/// by, as `organize` compares them.
pub(crate) fn caseless(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

/// The camera-local day of a capture stored as its instant and offset (`local = instant + offset`).
pub(crate) fn local_day(instant_ms: i64, offset_minutes: Option<i16>) -> LocalDay {
    let local = instant_ms + i64::from(offset_minutes.unwrap_or(0)) * 60_000;
    LocalDay(local.div_euclid(86_400_000) as i32)
}

/// The half-open range of stored paths strictly inside the directory `path`: from `path` and a
/// separator up to the same with the separator's next byte, so `/a/b` covers `/a/b/c` but never
/// `/a/b-x` or `/a/bc`. Paths are stored as text and compared bytewise, as SQLite's default
/// collation and Rust's `str` both do.
pub(crate) fn subtree_bounds(path: &str) -> (String, String) {
    let separator = std::path::MAIN_SEPARATOR;
    let mut lower = path.to_owned();
    if !lower.ends_with(separator) {
        lower.push(separator);
    }
    let mut upper = lower[..lower.len() - separator.len_utf8()].to_owned();
    upper.push(char::from(separator as u8 + 1));
    (lower, upper)
}

/// Whether the stored path `path` is `directory` or inside it, by the same rule as
/// [`subtree_bounds`].
pub(crate) fn within(path: &str, directory: &str) -> bool {
    let (lower, upper) = subtree_bounds(directory);
    path == directory || (path >= lower.as_str() && path < upper.as_str())
}

/// A JSON array of `values`, bound as one parameter and read back with `json_each`, so a statement
/// that takes a list is one fixed, cached statement.
pub(crate) fn json_list<T: serde::Serialize>(values: &[T]) -> Result<String, Error> {
    serde_json::to_string(values).map_err(|error| Error::internal(error.to_string()))
}
