//! Events over the files of the indexed folders and mounted cards, and `event.list`.
//!
//! Events are computed on demand by `organize::events` from the index's rows under the roots of
//! kind `indexed` (online or offline) and of kind `card` while mounted, and kept in memory keyed
//! by the index revision and the event thresholds ([`EventCache`]), so listing them again or
//! opening one costs no scan until the index changes. What is cached is each event's summary and
//! its files' rows, never frame facts. Picks change with the library, not the index, so each
//! `event.list` counts them afresh: one read of the picks and one index lookup per pick.

use super::{
    candidates::{FILE_COLUMNS, FileScope, Names, file_row},
    index_revision, places,
};
use crate::{
    EditorService, Error,
    catalog_types::{
        Event, EventId, EventList, FileId, LocalDay, Month, MonthCount, RootKind, Thresholds,
        ViewItem, VolumeId,
    },
    organize,
};
use rusqlite::OptionalExtension;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::PathBuf,
};

/// How many event computations are kept: the default thresholds `event.list` uses and one view's
/// own.
const CACHED: usize = 2;

/// Events computed at one index revision under one pair of event thresholds.
pub(crate) struct Events {
    revision: u64,
    gap_ms: u64,
    distance_km: f64,
    /// Newest first: dated events by start, latest first, then the Undated events by name.
    listed: Vec<CachedEvent>,
    /// Every event's files with the event and month each is counted under, ascending by file.
    by_file: Vec<(FileId, u32, Option<Month>)>,
}

struct CachedEvent {
    /// The event as `event.list` answers it, `picked` aside.
    event: Event,
    /// Its files, ascending.
    files: Vec<FileId>,
    /// How many of its files each of its months holds.
    month_files: Vec<(Month, u32)>,
}

/// The events of the index, per revision and thresholds, most recently used first.
#[derive(Default)]
pub(crate) struct EventCache {
    entries: Vec<Events>,
}

impl EventCache {
    /// The events at `revision` under `thresholds`, computed now unless they are cached.
    pub(crate) fn get(
        &mut self,
        service: &EditorService,
        revision: u64,
        thresholds: &Thresholds,
    ) -> Result<&Events, Error> {
        let found = self.entries.iter().position(|events| {
            events.revision == revision
                && events.gap_ms == thresholds.event_gap_ms
                && events.distance_km == thresholds.event_distance_km
        });
        let events = match found {
            Some(found) => self.entries.remove(found),
            None => compute(service, revision, thresholds)?,
        };
        self.entries.insert(0, events);
        self.entries.truncate(CACHED);
        Ok(&self.entries[0])
    }

    /// The files of the event `id` at `revision` under `thresholds`, ascending; refused when there
    /// is no such event.
    pub(crate) fn files(
        &mut self,
        service: &EditorService,
        revision: u64,
        thresholds: &Thresholds,
        id: &EventId,
    ) -> Result<Vec<FileId>, Error> {
        self.get(service, revision, thresholds)?
            .listed
            .iter()
            .find(|cached| &cached.event.id == id)
            .map(|cached| cached.files.clone())
            .ok_or_else(|| {
                Error::validation(format!(
                    "unknown event {id}: the index or the thresholds changed; list the events again"
                ))
            })
    }
}

/// `event.list`: the events at the index's current revision under the default thresholds, newest
/// first, those `month` names when given and those `query` matches (places, dates and cameras,
/// ignoring case) when given; and every month the matching events are listed under, newest first,
/// with its events, files and picks.
pub(crate) fn event_list(
    service: &EditorService,
    cache: &mut EventCache,
    month: Option<Month>,
    query: Option<&str>,
) -> Result<EventList, Error> {
    let revision = index_revision(service.index()?.connection())?;
    let events = cache.get(service, revision, &Thresholds::default())?;
    let picks = picked(service, events)?;
    let needle = query.map(str::to_lowercase);
    let matching: Vec<usize> = (0..events.listed.len())
        .filter(|index| {
            needle
                .as_deref()
                .is_none_or(|needle| matches(&events.listed[*index].event, needle))
        })
        .collect();
    let mut months: BTreeMap<Month, MonthCount> = BTreeMap::new();
    for index in &matching {
        for (month, files) in &events.listed[*index].month_files {
            let count = months.entry(*month).or_insert(MonthCount {
                month: *month,
                events: 0,
                files: 0,
                picked: 0,
            });
            count.events += 1;
            count.files += files;
        }
    }
    let matching_set: HashSet<usize> = matching.iter().copied().collect();
    let mut picked_per_event = vec![0u32; events.listed.len()];
    for (event, month) in &picks {
        picked_per_event[*event as usize] += 1;
        if matching_set.contains(&(*event as usize))
            && let Some(count) = month.and_then(|month| months.get_mut(&month))
        {
            count.picked += 1;
        }
    }
    let listed = matching
        .into_iter()
        .filter(|index| {
            month.is_none_or(|month| events.listed[*index].event.months.contains(&month))
        })
        .map(|index| Event {
            picked: picked_per_event[index],
            ..events.listed[index].event.clone()
        })
        .collect();
    Ok(EventList {
        events: listed,
        months: months.into_values().rev().collect(),
    })
}

/// Whether `needle` (lowercased) is in the event's name, place, cameras or dates, or names a day
/// it spans.
pub(super) fn matches(event: &Event, needle: &str) -> bool {
    let has = |value: &str| value.to_lowercase().contains(needle);
    has(&event.name)
        || event.place.as_deref().is_some_and(has)
        || event.cameras.iter().any(|camera| has(camera))
        || event.months.iter().any(|month| has(&month.to_string()))
        || [event.first_day, event.last_day]
            .into_iter()
            .flatten()
            .any(|day| has(&day.to_string()))
        || serde_json::from_value::<LocalDay>(serde_json::Value::String(needle.to_owned()))
            .is_ok_and(|day| {
                event
                    .first_day
                    .zip(event.last_day)
                    .is_some_and(|(first, last)| first <= day && day <= last)
            })
}

/// The event and month of every picked file in an event: the picks read once, each found in the
/// index by its path.
fn picked(service: &EditorService, events: &Events) -> Result<Vec<(u32, Option<Month>)>, Error> {
    let mut picks = service
        .connection
        .prepare_cached("SELECT path FROM picks")?;
    let paths = picks
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let index = service.index()?;
    let mut lookup = index
        .connection()
        .prepare_cached("SELECT id FROM files WHERE path = ?1")?;
    let mut found = Vec::new();
    for path in paths {
        let Some(id) = lookup
            .query_row([&path], |row| row.get::<_, i64>(0))
            .optional()?
        else {
            continue;
        };
        if let Ok(at) = events
            .by_file
            .binary_search_by_key(&FileId(id), |(file, _, _)| *file)
        {
            let (_, event, month) = events.by_file[at];
            found.push((event, month));
        }
    }
    Ok(found)
}

/// One root the events are computed over.
struct Root {
    path: String,
    volume_id: VolumeId,
    offline: bool,
}

/// Compute the events at `revision` under `thresholds` from the index.
fn compute(
    service: &EditorService,
    revision: u64,
    thresholds: &Thresholds,
) -> Result<Events, Error> {
    let index = service.index()?;
    let connection = index.connection();
    let roots: Vec<Root> = connection
        .prepare_cached(
            "SELECT path, kind, volume_id, offline FROM roots
             WHERE kind = ?1 OR (kind = ?2 AND offline = 0) ORDER BY path",
        )?
        .query_map(
            [RootKind::Indexed.as_str(), RootKind::Card.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            },
        )?
        .map(|row| {
            let (path, volume, offline) = row?;
            Ok(Root {
                path,
                volume_id: VolumeId::parse(volume)?,
                offline,
            })
        })
        .collect::<Result<_, Error>>()?;
    let places = places();
    let mut names = Names::default();
    let mut frames = Vec::new();
    let mut root_of: Vec<u32> = Vec::new();
    let mut seen = HashSet::new();
    for (at, root) in roots.iter().enumerate() {
        let scope = FileScope::Folder {
            path: &root.path,
            subfolders: true,
        };
        let (sql, parameters) = scope.statement(FILE_COLUMNS);
        let mut statement = connection.prepare_cached(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(parameters.iter()))?;
        while let Some(row) = rows.next()? {
            let file = file_row(row, &mut names, places)?;
            if seen.insert(file.fact.item) {
                frames.push(file.fact);
                root_of.push(at as u32);
            }
        }
    }
    drop(index);
    let set = organize::events(&frames, &names.tables, thresholds, places);
    let file_of = |at: &u32| match frames[*at as usize].item {
        ViewItem::File(file) => file,
        ViewItem::Photo(_) => unreachable!("events are over files"),
    };
    // Every file with the event (as computed) and month it is counted under.
    let mut by_file: Vec<(FileId, u32, Option<Month>)> = Vec::with_capacity(frames.len());
    let mut listed = Vec::with_capacity(set.events.len());
    for (computed, group) in set.events.iter().enumerate() {
        let members = &set.order[group.start as usize..(group.start + group.len) as usize];
        let mut files: Vec<FileId> = members.iter().map(file_of).collect();
        files.sort_unstable();
        by_file.extend(members.iter().map(|at| {
            (
                file_of(at),
                computed as u32,
                frames[*at as usize].local_day.map(LocalDay::month),
            )
        }));
        let event_roots: BTreeSet<u32> = members.iter().map(|at| root_of[*at as usize]).collect();
        let mut months: BTreeMap<Month, u32> = BTreeMap::new();
        for at in members {
            if let Some(day) = frames[*at as usize].local_day {
                *months.entry(day.month()).or_default() += 1;
            }
        }
        let mut cameras: Vec<String> = Vec::new();
        for body in &group.bodies {
            if names.tables.camera(*body).is_some() {
                let label = names.tables.body_label(*body);
                if !cameras.contains(&label) {
                    cameras.push(label);
                }
            }
        }
        let volumes: BTreeSet<&VolumeId> = event_roots
            .iter()
            .map(|root| &roots[*root as usize].volume_id)
            .collect();
        listed.push(CachedEvent {
            event: Event {
                id: group.id.clone(),
                name: group.name.clone(),
                label: group.label.clone(),
                place: group.place.clone(),
                first_day: group.first_day,
                last_day: group.last_day,
                months: months.keys().copied().collect(),
                cameras,
                count: group.len,
                picked: 0,
                offline: members
                    .iter()
                    .filter(|at| roots[root_of[**at as usize] as usize].offline)
                    .count() as u32,
                roots: event_roots
                    .iter()
                    .map(|root| PathBuf::from(&roots[*root as usize].path))
                    .collect(),
                volumes: volumes.into_iter().cloned().collect(),
                undated: group.undated(),
            },
            files,
            month_files: months.into_iter().collect(),
        });
    }
    // Newest first: dated events latest start first, then the Undated ones by name.
    let starts: Vec<Option<i64>> = set.events.iter().map(|group| group.start_ms).collect();
    let mut order: Vec<usize> = (0..listed.len()).collect();
    order.sort_by(|a, b| match (starts[*a], starts[*b]) {
        (Some(x), Some(y)) => y
            .cmp(&x)
            .then_with(|| listed[*a].event.id.cmp(&listed[*b].event.id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => listed[*a]
            .event
            .name
            .cmp(&listed[*b].event.name)
            .then_with(|| listed[*a].event.id.cmp(&listed[*b].event.id)),
    });
    let mut listed_at = vec![0u32; order.len()];
    for (at, computed) in order.iter().enumerate() {
        listed_at[*computed] = at as u32;
    }
    let mut slots: Vec<Option<CachedEvent>> = listed.into_iter().map(Some).collect();
    let listed: Vec<CachedEvent> = order
        .into_iter()
        .map(|at| slots[at].take().expect("each event once"))
        .collect();
    for entry in &mut by_file {
        entry.1 = listed_at[entry.1 as usize];
    }
    by_file.sort_unstable_by_key(|(file, _, _)| *file);
    Ok(Events {
        revision,
        gap_ms: thresholds.event_gap_ms,
        distance_km: thresholds.event_distance_km,
        listed,
        by_file,
    })
}
