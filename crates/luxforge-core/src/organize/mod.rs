//! Organization: events by time and place, and the day, camera and moment boundaries of a view.
//! **Lane A (files)** owns this module (`docs/design/catalog.md`, "Events", "Grouping inside a
//! view").
//!
//! Pure functions of [`FrameFacts`] (header metadata, compact), the gazetteer ([`PlaceNames`]) and
//! the view's [`Thresholds`]; nothing is stored, so changing a threshold regroups at once. The
//! preview-brightness bracket check is lane B's and plugs in through [`BracketProbe`].
//!
//! - [`order`] sorts a view's frames; [`group`] finds its days, cameras and moments in that order
//!   (`moments.rs`: runs, bursts and brackets, P5); [`events`] splits every frame into events and
//!   names them (`events.rs`, the P3 rule; `names.rs`, P4's names and [`Gazetteer`]).
//! - `gazetteer.rs` is the bundled offline place table and its index.
//!
//! The cost is a sort and a pass: `events` allocates its order vector and then per event, `group`
//! per moment, and neither holds a string per frame.

use crate::catalog_types::{
    BracketProbe, CameraGroup, DayGroup, EventSet, Exposure, FrameFacts, FrameTables, GroupLayout,
    Grouping, PlaceNames, Thresholds,
};
use std::{cmp::Ordering, collections::BTreeMap};

mod events;
pub(crate) mod gazetteer;
mod moments;
mod names;
#[cfg(test)]
mod tests;

pub(crate) use names::Gazetteer;

/// Sort `frames` into the order a view shows them under `grouping`, earliest first, or the reverse
/// when `descending`; undated frames come last either way, by folder and file name.
///
/// Under [`Grouping::DayCameraMoment`] a day's frames are ordered body by body, each body in the
/// order its first frame of the day was taken, and each body's frames by capture time, so a body's
/// moments are consecutive. Under the other groupings frames are ordered by capture time. Ties
/// fall to the file name and then the item, so the order is total.
pub(crate) fn order(
    frames: &mut [FrameFacts],
    tables: &FrameTables,
    grouping: Grouping,
    descending: bool,
) {
    // The rank of each body within each day: the order of its first frame that day.
    let mut first_of_body = BTreeMap::new();
    if grouping == Grouping::DayCameraMoment {
        for frame in frames.iter() {
            if let (Some(day), Some(instant)) = (frame.local_day, frame.instant_ms) {
                first_of_body
                    .entry((day, frame.body))
                    .and_modify(|first: &mut i64| *first = (*first).min(instant))
                    .or_insert(instant);
            }
        }
    }
    // Each frame's key, worked out once, so sorting compares a few words and moves 40 bytes; the
    // names and folders are read only for frames the key cannot tell apart.
    let keys: Vec<OrderKey> = frames
        .iter()
        .enumerate()
        .map(|(at, frame)| OrderKey {
            dated: frame.instant_ms.is_some(),
            day: match grouping {
                Grouping::None => None,
                Grouping::Day | Grouping::DayCameraMoment => frame.local_day,
            },
            rank: match grouping {
                Grouping::DayCameraMoment => frame
                    .local_day
                    .and_then(|day| first_of_body.get(&(day, frame.body)).copied()),
                Grouping::Day | Grouping::None => None,
            },
            body: match grouping {
                Grouping::DayCameraMoment => frame.body.0,
                Grouping::Day | Grouping::None => 0,
            },
            instant: frame.instant_ms,
            at: at as u32,
        })
        .collect();
    let ascending = |a: &OrderKey, b: &OrderKey| -> Ordering {
        let (x, y) = (&frames[a.at as usize], &frames[b.at as usize]);
        a.day
            .cmp(&b.day)
            .then(a.rank.cmp(&b.rank))
            .then(a.body.cmp(&b.body))
            .then(a.instant.cmp(&b.instant))
            .then_with(|| caseless(&x.name, &y.name))
            .then_with(|| x.item.cmp(&y.item))
    };
    let mut keys = keys;
    keys.sort_unstable_by(|a, b| match (a.dated, b.dated) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => {
            let (x, y) = (&frames[a.at as usize], &frames[b.at as usize]);
            tables
                .folder_path(x.folder)
                .cmp(tables.folder_path(y.folder))
                .then_with(|| caseless(&x.name, &y.name))
                .then_with(|| x.item.cmp(&y.item))
        }
        (true, true) if descending => ascending(b, a),
        (true, true) => ascending(a, b),
    });
    // The order is total — every tie falls to the item — so the unstable sort gives the one order.
    let order: Vec<usize> = keys.iter().map(|key| key.at as usize).collect();
    permute(frames, &order);
}

/// A frame's place in [`order`]'s sort: whether it is dated, then, as the grouping asks, its day,
/// its body's rank that day and the body, then its capture time; `at` is its index.
struct OrderKey {
    dated: bool,
    day: Option<crate::catalog_types::LocalDay>,
    rank: Option<i64>,
    body: u32,
    instant: Option<i64>,
    at: u32,
}

/// Put `items` in the order `order` names: `items[i]` becomes the old `items[order[i]]`, moving
/// each item once along its cycle.
fn permute<T>(items: &mut [T], order: &[usize]) {
    let mut done = vec![false; items.len()];
    for start in 0..items.len() {
        if done[start] {
            continue;
        }
        let mut at = start;
        loop {
            done[at] = true;
            let from = order[at];
            if from == start {
                break;
            }
            items.swap(at, from);
            at = from;
        }
    }
}

/// Two names compared ignoring case, allocating nothing.
fn caseless(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

/// The group layout of `frames`, already in [`order`]: their days, the camera groups of days with
/// more than one body, and the bursts and brackets of each body's frames of a day (both under
/// [`Grouping::DayCameraMoment`] only), a bracket from metadata ([`metadata_steps`]) or, when the
/// metadata cannot say, from `probe`.
///
/// Undated frames are one group with no day, and have no camera groups or moments: they are ordered
/// by folder and name, not by body. The moment rules (P5) are in `moments.rs`: runs of one body by
/// time and settings; a run whose exposure steps in its metadata or its previews is a bracket, and
/// a longer run that repeats one bracket's pattern is several; any other run of two or more frames
/// is a burst. Frames may be in either direction ([`order`]'s `descending`).
pub(crate) fn group(
    frames: &[FrameFacts],
    tables: &FrameTables,
    grouping: Grouping,
    thresholds: &Thresholds,
    probe: &dyn BracketProbe,
) -> GroupLayout {
    let mut layout = GroupLayout::default();
    if grouping == Grouping::None || frames.is_empty() {
        return layout;
    }
    let mut moments = moments::Finder::new(thresholds, probe);
    let mut start = 0;
    while start < frames.len() {
        let day = frames[start].local_day;
        let len = frames[start..]
            .iter()
            .take_while(|frame| frame.local_day == day)
            .count();
        layout.days.push(DayGroup {
            day,
            start: start as u32,
            len: len as u32,
            picked: 0,
        });
        let day_frames = &frames[start..start + len];
        if grouping == Grouping::DayCameraMoment && day.is_some() {
            let several = day_frames
                .iter()
                .any(|frame| frame.body != day_frames[0].body);
            let mut run = 0;
            while run < len {
                let body = day_frames[run].body;
                let run_len = day_frames[run..]
                    .iter()
                    .take_while(|frame| frame.body == body)
                    .count();
                if several {
                    layout.cameras.push(CameraGroup {
                        body: tables.body_key(body),
                        label: tables.body_label(body),
                        start: (start + run) as u32,
                        len: run_len as u32,
                    });
                }
                moments.find(
                    &day_frames[run..run + run_len],
                    start + run,
                    &mut layout.moments,
                );
                run += run_len;
            }
        }
        start += len;
    }
    layout
}

/// Every event of `frames` (the files of the indexed folders and mounted cards), by the design's
/// event rule (P3, `events.rs`), in [`EventSet`]'s order:
///
/// - Dated frames are sorted by capture time across every camera and folder (ties by name, then
///   item). A new event starts before a positioned frame more than `event_distance_km` from the
///   last positioned frame of the current event (a **jump**), and at a gap longer than
///   `event_gap_ms` between consecutive frames, unless the gap is a **stay**: under 24 hours, with
///   the last positioned frame before it and the first positioned frame after it (before the next
///   such gap) within `event_distance_km` of each other. So a trip stays one event across its
///   nights, and without positions on both sides each outing is its own event.
/// - Undated frames form one Undated event per folder, by file name, after every dated event.
///
/// An event is named (`names.rs`, P4) by the place `places` names at its positioned frames' median
/// position, else by a user-named folder holding more than half of its frames, else by its dates
/// and cameras: "Konstanz · 12–13 Sep", "Lake · 14 Sep", "16 Sep · LEICA Q3, Apple iPhone 15 Pro",
/// "Undated · From Anna". [`Gazetteer`] is the bundled gazetteer; its first query builds its index,
/// which the owner thread should not pay for ([`Gazetteer::warm`]).
pub(crate) fn events(
    frames: &[FrameFacts],
    tables: &FrameTables,
    thresholds: &Thresholds,
    places: &dyn PlaceNames,
) -> EventSet {
    events::events(frames, tables, thresholds, places)
}

/// A run's per-frame exposure steps from its metadata, in stops relative to its metered frame
/// (brighter positive), or none when the metadata cannot say.
///
/// The settings decide first ([`Exposure::ev`]): when the time, f-number and ISO are recorded on
/// every frame and differ, the steps are theirs. Otherwise the recorded bias decides when it is on
/// every frame and differs, since some drones and phones write a bracket's bias but not a changed
/// shutter time. Adding the two would count a step twice, because a camera applies its bias through
/// its settings. The metered frame is the one frame with no bias; when no frame or several have
/// none (a bracket of shutter times alone records 0 on every frame), it is the median frame.
/// Whether the steps make a bracket is the caller's rule (at least ⅓ EV between frames, 2 to 9
/// frames).
pub(crate) fn metadata_steps(run: &[Exposure]) -> Option<Vec<f32>> {
    const SAME: f32 = 1e-3;
    fn varies(values: &[f32]) -> bool {
        values.iter().any(|value| (value - values[0]).abs() > SAME)
    }
    if run.len() < 2 {
        return None;
    }
    let brightness: Option<Vec<f32>> = run.iter().map(|frame| frame.ev().map(|ev| -ev)).collect();
    let bias: Option<Vec<f32>> = run.iter().map(|frame| frame.bias_ev).collect();
    let values = match (brightness, &bias) {
        (Some(brightness), _) if varies(&brightness) => brightness,
        (_, Some(bias)) if varies(bias) => bias.clone(),
        _ => return None,
    };
    let unbiased = |value: &f32| value.abs() <= SAME;
    let metered = bias
        .as_ref()
        .filter(|bias| bias.iter().filter(|value| unbiased(value)).count() == 1)
        .and_then(|bias| bias.iter().position(unbiased))
        .unwrap_or_else(|| middle_ranked(&values));
    let reference = values[metered];
    Some(values.iter().map(|value| value - reference).collect())
}

/// The index of the median-ranked value (the lower of the two middle ones for an even count, the
/// earlier of equal values): the metered frame of a run that records none.
fn middle_ranked(values: &[f32]) -> usize {
    let mut ranked: Vec<usize> = (0..values.len()).collect();
    ranked.sort_by(|a, b| values[*a].total_cmp(&values[*b]).then(a.cmp(b)));
    ranked[(ranked.len() - 1) / 2]
}
