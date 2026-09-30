//! Organization: events by time and place, and the day, camera and moment boundaries of a view.
//! **Lane A (files)** owns this module (`docs/design/catalog.md`, "Events", "Grouping inside a
//! view").
//!
//! Pure functions of [`FrameFacts`] (header metadata, compact), the gazetteer ([`PlaceNames`]) and
//! the view's [`Thresholds`]; nothing is stored, so changing a threshold regroups at once. The
//! preview-brightness bracket check is lane B's and plugs in through [`BracketProbe`].
//!
//! The signatures below are the contract lane D's views call. **Placeholder bodies** (catalog
//! contracts), which lane A replaces: [`events`] makes one event per camera-local day and one
//! Undated event per folder, named by date; [`group`] finds days and cameras exactly and no moments,
//! so every frame is a single. [`order`] and [`metadata_steps`] are the rules as the design states
//! them, which lane A may refine.
//!
//! Planned files: `events.rs` (the P3 event rule and naming), `moments.rs` (runs, bursts and
//! brackets, P5), `gazetteer.rs` (the bundled offline place names, P4).
#![allow(dead_code, reason = "catalog contracts: called as lane D's views land")]

use crate::catalog_types::{
    BracketProbe, CameraGroup, DayGroup, EventGroup, EventId, EventSet, Exposure, FrameFacts,
    FrameTables, GroupLayout, Grouping, PlaceNames, Thresholds,
};
use std::{cmp::Ordering, collections::BTreeMap};

pub(crate) mod gazetteer;

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
    let ascending = |a: &FrameFacts, b: &FrameFacts| -> Ordering {
        let body_rank = |frame: &FrameFacts| {
            frame
                .local_day
                .and_then(|day| first_of_body.get(&(day, frame.body)).copied())
        };
        let by_day = match grouping {
            Grouping::None => Ordering::Equal,
            Grouping::Day | Grouping::DayCameraMoment => a.local_day.cmp(&b.local_day),
        };
        let by_body = match grouping {
            Grouping::DayCameraMoment => body_rank(a)
                .cmp(&body_rank(b))
                .then_with(|| a.body.cmp(&b.body)),
            Grouping::Day | Grouping::None => Ordering::Equal,
        };
        by_day
            .then(by_body)
            .then_with(|| a.instant_ms.cmp(&b.instant_ms))
            .then_with(|| caseless(&a.name, &b.name))
            .then_with(|| a.item.cmp(&b.item))
    };
    frames.sort_by(|a, b| {
        let dated = |frame: &FrameFacts| frame.instant_ms.is_some();
        match (dated(a), dated(b)) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => tables
                .folder_path(a.folder)
                .cmp(tables.folder_path(b.folder))
                .then_with(|| caseless(&a.name, &b.name))
                .then_with(|| a.item.cmp(&b.item)),
            (true, true) if descending => ascending(b, a),
            (true, true) => ascending(a, b),
        }
    });
}

/// Two names compared ignoring case, allocating nothing.
fn caseless(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

/// The group layout of `frames`, already in [`order`]: their days, the camera groups of days with
/// more than one body (under [`Grouping::DayCameraMoment`]), and their bursts and brackets, a
/// bracket from metadata ([`metadata_steps`]) or, when the metadata cannot say, from `probe`.
///
/// **Placeholder**: days and cameras are exact; no moments are found, so every frame is a single.
pub(crate) fn group(
    frames: &[FrameFacts],
    tables: &FrameTables,
    grouping: Grouping,
    _thresholds: &Thresholds,
    _probe: &dyn BracketProbe,
) -> GroupLayout {
    let mut layout = GroupLayout::default();
    if grouping == Grouping::None || frames.is_empty() {
        return layout;
    }
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
        });
        let day_frames = &frames[start..start + len];
        let several = day_frames
            .iter()
            .any(|frame| frame.body != day_frames[0].body);
        if grouping == Grouping::DayCameraMoment && several {
            let mut run = 0;
            while run < len {
                let body = day_frames[run].body;
                let run_len = day_frames[run..]
                    .iter()
                    .take_while(|frame| frame.body == body)
                    .count();
                layout.cameras.push(CameraGroup {
                    body: tables.body_key(body),
                    label: tables.body_label(body),
                    start: (start + run) as u32,
                    len: run_len as u32,
                });
                run += run_len;
            }
        }
        start += len;
    }
    layout
}

/// Every event of `frames` (the files of the indexed folders and mounted cards), by the design's
/// event rule: sorted by capture time across every camera and folder, a new event at a gap over
/// `thresholds.event_gap_ms`, or where consecutive positioned frames are further apart than
/// `thresholds.event_distance_km`; a calendar day is never split below that; frames with no capture
/// time form an Undated event per folder. Events are named by their place (`places`), a user-named
/// folder that holds most of them, or their dates and cameras.
///
/// **Placeholder**: one event per camera-local day and one Undated event per folder, each named by
/// its date or folder; no place is looked up.
pub(crate) fn events(
    frames: &[FrameFacts],
    tables: &FrameTables,
    _thresholds: &Thresholds,
    _places: &dyn PlaceNames,
) -> EventSet {
    let mut order: Vec<u32> = (0..frames.len() as u32).collect();
    let key = |index: &u32| {
        let frame = &frames[*index as usize];
        (
            frame.instant_ms.is_none(),
            frame.local_day,
            frame.instant_ms,
            frame
                .instant_ms
                .is_none()
                .then(|| tables.folder_path(frame.folder).clone()),
            frame.name.to_lowercase(),
            frame.item,
        )
    };
    order.sort_by_cached_key(key);
    let mut set = EventSet {
        order,
        events: Vec::new(),
    };
    let mut start = 0;
    while start < set.order.len() {
        let first = &frames[set.order[start] as usize];
        let same = |index: &u32| {
            let frame = &frames[*index as usize];
            match first.instant_ms {
                Some(_) => frame.local_day == first.local_day && frame.instant_ms.is_some(),
                None => frame.instant_ms.is_none() && frame.folder == first.folder,
            }
        };
        let len = set.order[start..]
            .iter()
            .take_while(|index| same(index))
            .count();
        let members = &set.order[start..start + len];
        let last = &frames[members[len - 1] as usize];
        let mut bodies: Vec<_> = members
            .iter()
            .map(|index| frames[*index as usize].body)
            .collect();
        bodies.sort();
        bodies.dedup();
        let mut folders: Vec<_> = members
            .iter()
            .map(|index| frames[*index as usize].folder)
            .collect();
        folders.sort();
        folders.dedup();
        let folder = tables.folder_path(first.folder);
        let name = match first.local_day {
            Some(day) => day.to_string(),
            None => format!(
                "Undated · {}",
                folder
                    .file_name()
                    .map_or_else(|| folder.to_string_lossy(), |name| name.to_string_lossy())
            ),
        };
        set.events.push(EventGroup {
            id: EventId::of(
                &folder.join(&*first.name),
                first.instant_ms.unwrap_or_default(),
            ),
            name,
            place: None,
            start_ms: first.instant_ms,
            end_ms: last.instant_ms,
            first_day: first.local_day,
            last_day: last.local_day,
            bodies,
            folders,
            start: start as u32,
            len: len as u32,
        });
        start += len;
    }
    set
}

/// A run's per-frame exposure steps from its metadata, in stops relative to its metered frame
/// (brighter positive), or none when the metadata cannot say.
///
/// The settings decide first ([`Exposure::ev`]): when the time, f-number and ISO are recorded on
/// every frame and differ, the steps are theirs. Otherwise the recorded bias decides when it is on
/// every frame and differs, since some drones and phones write a bracket's bias but not a changed
/// shutter time. Adding the two would count a step twice, because a camera applies its bias through
/// its settings. The metered frame is the one with no bias, or the median frame. Whether the steps
/// make a bracket is the caller's rule (at least ⅓ EV between frames, 2 to 9 frames).
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
    let metered = bias
        .as_ref()
        .and_then(|bias| bias.iter().position(|value| value.abs() <= SAME))
        .unwrap_or_else(|| {
            let mut ranked: Vec<usize> = (0..values.len()).collect();
            ranked.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
            ranked[(ranked.len() - 1) / 2]
        });
    let reference = values[metered];
    Some(values.iter().map(|value| value - reference).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_types::{CameraBody, FileId, LocalDay, NoPlaces, NoProbe, ViewItem};

    fn frame(
        tables: &mut FrameTables,
        id: i64,
        folder: &str,
        instant: Option<i64>,
        body: Option<&CameraBody>,
    ) -> FrameFacts {
        FrameFacts {
            item: ViewItem::File(FileId(id)),
            folder: tables.folder(folder.into()),
            name: format!("F{id:04}.JPG").into(),
            instant_ms: instant,
            local_day: instant.map(|instant| LocalDay(instant.div_euclid(86_400_000) as i32)),
            position: None,
            body: tables.body(body),
            exposure: Exposure::default(),
        }
    }

    /// Days and cameras are exact in the placeholder too: a day with two bodies is split by body,
    /// each body in the order of its first frame, and undated frames come last.
    #[test]
    fn organize_orders_and_groups_days_and_cameras() {
        let mut tables = FrameTables::default();
        let z8 = CameraBody {
            make: "NIKON CORPORATION".into(),
            model: "NIKON Z 8".into(),
            serial: Some("1".into()),
        };
        let q3 = CameraBody {
            make: "LEICA CAMERA AG".into(),
            model: "LEICA Q3".into(),
            serial: None,
        };
        let day = 86_400_000;
        let mut frames = vec![
            frame(&mut tables, 1, "/card", Some(10 * day + 5_000), Some(&z8)),
            frame(&mut tables, 2, "/dump", Some(10 * day + 1_000), Some(&q3)),
            frame(&mut tables, 3, "/card", Some(10 * day + 2_000), Some(&z8)),
            frame(&mut tables, 4, "/dump", Some(10 * day + 9_000), Some(&q3)),
            frame(&mut tables, 5, "/card", None, Some(&z8)),
            frame(&mut tables, 6, "/card", Some(11 * day), Some(&z8)),
        ];
        order(&mut frames, &tables, Grouping::DayCameraMoment, false);
        let ids: Vec<_> = frames.iter().map(|frame| frame.item).collect();
        let expected: Vec<_> = [2, 4, 3, 1, 6, 5]
            .map(|id| ViewItem::File(FileId(id)))
            .to_vec();
        assert_eq!(ids, expected);
        let layout = group(
            &frames,
            &tables,
            Grouping::DayCameraMoment,
            &Thresholds::default(),
            &NoProbe,
        );
        assert_eq!(
            layout
                .days
                .iter()
                .map(|day| (day.start, day.len))
                .collect::<Vec<_>>(),
            [(0, 4), (4, 1), (5, 1)]
        );
        assert_eq!(layout.days[2].day, None, "undated last");
        assert_eq!(
            layout
                .cameras
                .iter()
                .map(|camera| (camera.label.as_str(), camera.start, camera.len))
                .collect::<Vec<_>>(),
            [("LEICA Q3", 0, 2), ("NIKON Z 8", 2, 2)]
        );
        assert!(
            layout.moments.is_empty(),
            "the placeholder finds no moments"
        );
        let set = events(&frames, &tables, &Thresholds::default(), &NoPlaces);
        assert_eq!(set.events.len(), 3, "two days and one undated folder");
        assert!(set.events[2].undated());
        assert_eq!(set.order.len(), frames.len());
    }

    #[test]
    fn organize_metadata_steps_follow_settings_then_bias() {
        let base = Exposure {
            time_s: Some(1.0 / 60.0),
            f_number: Some(8.0),
            iso: Some(100),
            bias_ev: Some(0.0),
            ..Exposure::default()
        };
        let at = |time: f32, bias: f32| Exposure {
            time_s: Some(time),
            bias_ev: Some(bias),
            ..base
        };
        // A shutter bracket with its bias recorded: steps from the settings, around the 0 frame.
        let steps = metadata_steps(&[at(1.0 / 240.0, -2.0), base, at(1.0 / 15.0, 2.0)]).unwrap();
        for (step, expected) in steps.iter().zip([-2.0, 0.0, 2.0]) {
            assert!((step - expected).abs() < 1e-3, "{steps:?}");
        }
        // Settings unchanged, bias written: the bias says.
        let drone = [
            at(1.0 / 60.0, -1.0),
            at(1.0 / 60.0, 0.0),
            at(1.0 / 60.0, 1.0),
        ];
        assert_eq!(metadata_steps(&drone), Some(vec![-1.0, 0.0, 1.0]));
        // Nothing changes: the metadata cannot say.
        assert_eq!(metadata_steps(&[base, base, base]), None);
        assert_eq!(metadata_steps(&[base]), None);
    }
}
