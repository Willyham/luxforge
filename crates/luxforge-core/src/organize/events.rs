//! Events (P3): where one event ends and the next begins.
//!
//! Dated frames are taken in capture-time order across every camera and folder. Two things start a
//! new event:
//!
//! - **A jump.** A positioned frame more than `event_distance_km` from the last positioned frame of
//!   the current event: not merely the frame before it, because a camera without GPS interleaves
//!   with a phone that has one, and its frames must not hide the phone's move.
//! - **A gap** longer than `event_gap_ms` between consecutive frames, unless it is a **stay**: a gap
//!   under [`STAY_LIMIT_MS`] whose last positioned frame before it (in the current event) and first
//!   positioned frame after it (before the next gap longer than `event_gap_ms`) are within
//!   `event_distance_km` of each other. A trip is one event across its nights, and a long lunch at
//!   one place does not split an afternoon; without positions on both sides, each outing stays its
//!   own event, because nothing says it did not move.
//!
//! The known limit: a place photographed every day for a week, such as home, is one event while
//! each day's gap stays under a day, since nothing tells home from a trip.
//!
//! Undated frames make one event per folder, by file name, after every dated event.
use super::caseless;
use super::names::Namer;
use crate::catalog_types::{
    EventSet, FolderIndex, FrameFacts, FrameTables, GeoPosition, PlaceNames, Thresholds,
};
use std::{cmp::Ordering, ops::Range};

/// A gap this long or longer always starts a new event, whatever the positions on either side:
/// under it, a gap longer than the event gap may be a stay (a night at one place); at a day or more
/// the same place is a new outing. Fixed, not a view threshold: it is what makes the rule a trip
/// rule, not a tuning.
pub(super) const STAY_LIMIT_MS: u64 = 24 * 60 * 60 * 1000;

pub(super) fn events(
    frames: &[FrameFacts],
    tables: &FrameTables,
    thresholds: &Thresholds,
    places: &dyn PlaceNames,
) -> EventSet {
    let folder_rank = folder_ranks(frames, tables);
    let mut order: Vec<u32> = (0..frames.len() as u32).collect();
    order.sort_unstable_by(|a, b| {
        let (a, b) = (&frames[*a as usize], &frames[*b as usize]);
        match (a.instant_ms, b.instant_ms) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => folder_rank[a.folder.0 as usize].cmp(&folder_rank[b.folder.0 as usize]),
        }
        .then_with(|| caseless(&a.name, &b.name))
        .then_with(|| a.item.cmp(&b.item))
    });
    let dated = order.partition_point(|index| frames[*index as usize].instant_ms.is_some());
    let mut namer = Namer::new(tables, places);
    let mut events = Vec::new();
    for range in dated_events(frames, &order[..dated], thresholds) {
        events.push(namer.event(frames, &order[range.clone()], range.start));
    }
    let mut start = dated;
    while start < order.len() {
        let folder = frames[order[start] as usize].folder;
        let len = order[start..]
            .iter()
            .take_while(|index| frames[**index as usize].folder == folder)
            .count();
        events.push(namer.event(frames, &order[start..start + len], start));
        start += len;
    }
    EventSet { order, events }
}

/// Each folder index's rank by path, so undated frames sort by folder without comparing a path per
/// frame. Empty when every frame is dated.
fn folder_ranks(frames: &[FrameFacts], tables: &FrameTables) -> Vec<u32> {
    if frames.iter().all(|frame| frame.instant_ms.is_some()) {
        return Vec::new();
    }
    let count = frames
        .iter()
        .map(|frame| frame.folder.0 as usize + 1)
        .max()
        .unwrap_or(0);
    let mut by_path: Vec<u32> = (0..count as u32).collect();
    by_path.sort_by(|a, b| {
        tables
            .folder_path(FolderIndex(*a))
            .cmp(tables.folder_path(FolderIndex(*b)))
    });
    let mut rank = vec![0; count];
    for (position, folder) in by_path.into_iter().enumerate() {
        rank[folder as usize] = position as u32;
    }
    rank
}

/// The events of `dated` (frame indices in capture-time order) as ranges of it.
fn dated_events(
    frames: &[FrameFacts],
    dated: &[u32],
    thresholds: &Thresholds,
) -> Vec<Range<usize>> {
    let frame = |at: usize| &frames[dated[at] as usize];
    let instant = |at: usize| frame(at).instant_ms.unwrap_or_default();
    let gap_before = |at: usize| instant(at).abs_diff(instant(at - 1));
    let far = |a: &GeoPosition, b: &GeoPosition| a.distance_km(b) > thresholds.event_distance_km;
    // The first positioned frame from `at` on, before the next gap longer than the event gap.
    let first_position = |at: usize| {
        (at..dated.len())
            .take_while(|&next| next == at || gap_before(next) <= thresholds.event_gap_ms)
            .find_map(|next| frame(next).position)
    };
    let mut events = Vec::new();
    let mut start = 0;
    // The last positioned frame of the current event.
    let mut last_position: Option<GeoPosition> = None;
    for at in 0..dated.len() {
        let position = frame(at).position;
        if at > start {
            let jump = matches!((&last_position, &position), (Some(a), Some(b)) if far(a, b));
            let gap = gap_before(at);
            let parted = gap > thresholds.event_gap_ms
                && !(gap < STAY_LIMIT_MS
                    && last_position.is_some_and(|before| {
                        first_position(at).is_some_and(|after| !far(&before, &after))
                    }));
            if jump || parted {
                events.push(start..at);
                start = at;
                last_position = None;
            }
        }
        if position.is_some() {
            last_position = position;
        }
    }
    if !dated.is_empty() {
        events.push(start..dated.len());
    }
    events
}
