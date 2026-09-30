//! Planning a Develop, on the owner: which event each file is in and which moment, the catalog
//! folder each event is proposed for, which files are on removable media and which have a copy,
//! and which are offline. `pick.plan` answers it; `pick.develop` plans exactly the same way and
//! then takes each event's folder from its `into`.
//!
//! Events and moments are `crate::organize`'s over every file the index lists in the planned
//! files' folders, not over the planned files alone, so an event has the span and identity it has
//! among its own files and a burst or bracket its own frames: a later Develop from the same event
//! finds the folder this one made, and a bracket's developed frames record the moment they share.
//! A file the index does not list is an undated frame of its folder.
use super::frames::{self, Frames, unlisted_item};
use crate::{
    Error,
    catalog_types::{
        CatalogFolderId, DevelopInto, DevelopPlan, EventId, EventSpan, FolderChoice, FolderValue,
        Grouping, LocalDay, MAX_LIBRARY_NAME, MomentId, NoProbe, PlannedEvent, RemovablePicks,
        Thresholds, ViewItem, Volume, VolumeId,
    },
    editor::library_rows,
    index::volume_of,
    library::{availability::mounted, folders, targets::NamedFile, tree},
    organize::{self, Gazetteer},
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
};

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// What developing some files would do, per event.
#[derive(Debug)]
pub(crate) struct Plan {
    /// The files, event by event, each event's in its order.
    pub files: Vec<PlannedFile>,
    /// The events that hold them, in [`organize::events`]'s order: chronological, undated last.
    pub events: Vec<PlanEvent>,
}

/// One file of a plan.
#[derive(Clone, Debug)]
pub(crate) struct PlannedFile {
    /// Its path, as a pick names it.
    pub path: PathBuf,
    /// The event it is in: an index into [`Plan::events`].
    pub event: usize,
    /// The burst or bracket it is a frame of, which its photograph records.
    pub moment: Option<MomentId>,
    /// The removable volume it is on, when it is on one.
    pub removable: Option<VolumeId>,
    /// Files of its name and length in indexed folders on other, fixed volumes: the copies a card's
    /// pick may be developed from, once one proves to hold its bytes. Only for a removable file.
    pub copies: Vec<PathBuf>,
    /// The label of its volume, when its file is not there because that volume is not connected.
    pub offline: Option<String>,
}

/// One event of a plan and the folder proposed for it.
#[derive(Clone, Debug)]
pub(crate) struct PlanEvent {
    pub id: EventId,
    pub name: String,
    /// Its first and last capture instants and identity; none for an Undated event.
    pub span: Option<EventSpan>,
    /// Its files: a range of [`Plan::files`].
    pub files: Range<usize>,
    /// An existing folder made from it, or a new one named after it.
    pub proposal: FolderChoice,
    /// The proposed existing folder's name.
    pub folder_name: Option<String>,
}

impl Plan {
    /// What `pick.plan` answers.
    pub(crate) fn answer(&self, volumes: &HashMap<VolumeId, Volume>) -> DevelopPlan {
        let events = self
            .events
            .iter()
            .map(|event| {
                let mut removable: Vec<RemovablePicks> = Vec::new();
                for file in &self.files[event.files.clone()] {
                    let (Some(volume), None) = (&file.removable, &file.offline) else {
                        continue;
                    };
                    let at = match removable
                        .iter()
                        .position(|group| group.volume_id == *volume)
                    {
                        Some(at) => at,
                        None => {
                            removable.push(RemovablePicks {
                                volume_id: volume.clone(),
                                label: volumes.get(volume).map_or_else(
                                    || volume.to_string(),
                                    |known| known.label.clone(),
                                ),
                                count: 0,
                                with_copy: 0,
                            });
                            removable.len() - 1
                        }
                    };
                    removable[at].count += 1;
                    removable[at].with_copy += u32::from(!file.copies.is_empty());
                }
                PlannedEvent {
                    event_id: Some(event.id.clone()),
                    name: event.name.clone(),
                    count: event.files.len() as u32,
                    folder: event.proposal.clone(),
                    folder_name: event.folder_name.clone(),
                    removable,
                }
            })
            .collect();
        DevelopPlan {
            events,
            count: self.files.len() as u32,
            offline: self
                .files
                .iter()
                .filter(|file| file.offline.is_some())
                .count() as u32,
        }
    }
}

/// Plan developing `files`, reading the catalog, the index and one stat per file: the files by
/// event and moment, each event's proposed folder, and each file's volume, removability, copies
/// and whether it is offline. The volumes it saw are kept in `volumes`, by identity.
pub(crate) fn plan(
    catalog: &Connection,
    index: &Connection,
    files: Vec<NamedFile>,
    volumes: &mut HashMap<VolumeId, Volume>,
) -> Result<Plan, Error> {
    // Every file the index lists in the planned files' folders, and the unlisted ones themselves.
    let mut folders = Vec::new();
    let mut unlisted = Vec::new();
    let mut items = Vec::with_capacity(files.len());
    for file in &files {
        match file.file_id {
            Some(id) => {
                folders.push(parent(&file.path).to_path_buf());
                items.push(ViewItem::File(id));
            }
            None => {
                items.push(unlisted_item(unlisted.len()));
                unlisted.push(file.path.clone());
            }
        }
    }
    let mut frames = frames::frames(index, &folders, &unlisted)?;
    // A listed file its folder's rows did not hold (its row moved since) is an undated frame too.
    let listed: HashSet<ViewItem> = frames.frames.iter().map(|frame| frame.item).collect();
    let missing: Vec<usize> = (0..files.len())
        .filter(|&at| !listed.contains(&items[at]))
        .collect();
    if !missing.is_empty() {
        for at in missing {
            items[at] = unlisted_item(unlisted.len());
            unlisted.push(files[at].path.clone());
        }
        frames = frames::frames(index, &folders, &unlisted)?;
    }

    let set = organize::events(
        &frames.frames,
        &frames.tables,
        &Thresholds::default(),
        &Gazetteer,
    );
    let moments = moments(&frames);
    // Each frame's event and its position in the events' order.
    let mut placed = vec![(usize::MAX, usize::MAX); frames.frames.len()];
    for (event, group) in set.events.iter().enumerate() {
        let start = group.start as usize;
        for (at, &frame) in set.order[start..start + group.len as usize]
            .iter()
            .enumerate()
        {
            placed[frame as usize] = (event, start + at);
        }
    }
    let frame_of: HashMap<ViewItem, usize> = frames
        .frames
        .iter()
        .enumerate()
        .map(|(at, frame)| (frame.item, at))
        .collect();
    let mut order: Vec<(usize, usize, usize)> = items
        .iter()
        .enumerate()
        .map(|(file, item)| {
            let (event, position) = placed[frame_of[item]];
            (event, position, file)
        })
        .collect();
    order.sort_unstable();

    let indexed: Vec<PathBuf> = catalog
        .prepare_cached("SELECT path FROM indexed_folders")?
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|path| path.map(PathBuf::from))
        .collect::<Result<_, _>>()?;
    let mut resolver = Resolver {
        catalog,
        volumes,
        folders: HashMap::new(),
    };
    let mut taken = top_level_names(catalog)?;
    let mut plan = Plan {
        files: Vec::with_capacity(files.len()),
        events: Vec::new(),
    };
    let mut named: Vec<Option<NamedFile>> = files.into_iter().map(Some).collect();
    for (event, _, file) in order {
        let group = &set.events[event];
        if plan.events.last().is_none_or(|last| last.id != group.id) {
            let start = plan.files.len();
            let first_folder = frames.tables.folder_path(group.folders[0]).clone();
            let (proposal, folder_name) = propose(catalog, group, &first_folder, &mut taken)?;
            plan.events.push(PlanEvent {
                id: group.id.clone(),
                name: group.name.clone(),
                span: group
                    .start_ms
                    .zip(group.end_ms)
                    .map(|(start_ms, end_ms)| EventSpan {
                        start_ms,
                        end_ms,
                        event_id: Some(group.id.clone()),
                    }),
                files: start..start,
                proposal,
                folder_name,
            });
        }
        let named = named[file].take().expect("each file is planned once");
        let planned = resolver.file(index, &indexed, named, plan.events.len() - 1)?;
        let moment = moments.get(&items[file]).cloned();
        plan.files.push(PlannedFile { moment, ..planned });
        plan.events.last_mut().expect("its event").files.end += 1;
    }
    Ok(plan)
}

/// The folder a Develop proposes for `event`: the newest catalog folder made from it — whose
/// stored key is its identity, else whose stored span overlaps its span — or, for an Undated
/// event, which has no span, the folder the newest photograph developed from its folder on disk is
/// in; otherwise a
/// new top-level folder named after it, unique among the top-level folders and the plan's other
/// new ones.
fn propose(
    catalog: &Connection,
    event: &crate::catalog_types::EventGroup,
    folder: &Path,
    taken: &mut Vec<String>,
) -> Result<(FolderChoice, Option<String>), Error> {
    let existing: Option<(String, String)> = match event.start_ms.zip(event.end_ms) {
        Some((start, end)) => {
            let by_key = catalog
                .prepare_cached(
                    "SELECT id, name FROM catalog_folders WHERE event_key = ?1
                     ORDER BY created_ms DESC, id DESC LIMIT 1",
                )?
                .query_row([event.id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            match by_key {
                Some(found) => Some(found),
                None => catalog
                    .prepare_cached(
                        "SELECT id, name FROM catalog_folders
                         WHERE event_start_ms <= ?2 AND event_end_ms >= ?1
                         ORDER BY created_ms DESC, id DESC LIMIT 1",
                    )?
                    .query_row([start, end], |row| Ok((row.get(0)?, row.get(1)?)))
                    .optional()?,
            }
        }
        // Every availability, so the lookup reads `assets_by_availability` four times rather
        // than every photograph.
        None => catalog
            .prepare_cached(
                "SELECT f.id, f.name FROM assets a
                 JOIN catalog_folders f ON f.id = a.catalog_folder_id
                 WHERE a.availability IN ('available', 'offline', 'missing', 'changed')
                     AND a.source_folder = ?1 AND a.removed_ms IS NULL
                 ORDER BY a.developed_ms DESC, a.row_id DESC LIMIT 1",
            )?
            .query_row([folder.to_string_lossy()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?,
    };
    if let Some((id, name)) = existing {
        return Ok((
            FolderChoice::Existing {
                folder_id: CatalogFolderId::parse(id)?,
            },
            Some(name),
        ));
    }
    let name = unique(&folder_name(event, folder), taken);
    taken.push(name.clone());
    Ok((
        FolderChoice::New {
            name,
            parent_id: None,
        },
        None,
    ))
}

/// The name a new folder made from `event` is proposed with: its place with the month and year of
/// its first day ("Konstanz · Sep 2026"), or else the event's own name ("Lake · 14 Sep", "16 Sep ·
/// LEICA Q3"), as a catalog name keeps it. An Undated event's folder is named after the folder on
/// disk its files are in ("From Anna"), as a single opened file's has always been.
pub(crate) fn folder_name(event: &crate::catalog_types::EventGroup, disk_folder: &Path) -> String {
    let name = match (&event.place, event.first_day) {
        (Some(place), Some(day)) => format!("{place} · {}", month_and_year(day)),
        _ if event.undated() => disk_folder.file_name().map_or_else(
            || event.name.clone(),
            |name| name.to_string_lossy().into_owned(),
        ),
        _ => event.name.clone(),
    };
    let name: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let name: String = name.trim().chars().take(MAX_LIBRARY_NAME).collect();
    match name.trim() {
        "" => "Developed".to_owned(),
        name => name.to_owned(),
    }
}

/// "Sep 2026".
fn month_and_year(day: LocalDay) -> String {
    let (year, month, _) = day.ymd();
    format!("{} {year}", MONTHS[month as usize - 1])
}

/// `name`, or `name 2`, `name 3`… when a name in `taken` is already it, ignoring case.
fn unique(name: &str, taken: &[String]) -> String {
    let free = |candidate: &str| !taken.iter().any(|name| tree::same_name(name, candidate));
    if free(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| {
            let suffix = format!(" {n}");
            let keep = MAX_LIBRARY_NAME - suffix.chars().count();
            format!("{}{suffix}", name.chars().take(keep).collect::<String>())
        })
        .find(|candidate| free(candidate))
        .expect("some suffix is free")
}

fn top_level_names(catalog: &Connection) -> Result<Vec<String>, Error> {
    Ok(catalog
        .prepare_cached("SELECT name FROM catalog_folders WHERE parent_id IS NULL")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

/// Every burst's and bracket's identity by the items of its frames, over `frames` grouped as a
/// view groups them (day, camera, moment) with the recorded thresholds: a moment is known by its
/// first frame's path and capture instant.
fn moments(frames: &Frames) -> HashMap<ViewItem, MomentId> {
    let mut ordered = frames.frames.clone();
    organize::order(
        &mut ordered,
        &frames.tables,
        Grouping::DayCameraMoment,
        false,
    );
    let layout = organize::group(
        &ordered,
        &frames.tables,
        Grouping::DayCameraMoment,
        &Thresholds::default(),
        &NoProbe,
    );
    let mut of = HashMap::new();
    for moment in layout.moments {
        let members = &ordered[moment.start as usize..(moment.start + moment.len) as usize];
        let first = &members[0];
        let id = MomentId::of(&frames.path(first), first.instant_ms.unwrap_or(0));
        for frame in members {
            of.insert(frame.item, id.clone());
        }
    }
    of
}

/// The parent folder of a path.
pub(crate) fn parent(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new(""))
}

/// What a plan reads of volumes, each once: the catalog's record of a volume by identity, and
/// the volume a folder is on now.
struct Resolver<'a> {
    catalog: &'a Connection,
    volumes: &'a mut HashMap<VolumeId, Volume>,
    folders: HashMap<PathBuf, Option<Volume>>,
}

impl Resolver<'_> {
    /// The catalog's record of the volume `id`.
    fn recorded(&mut self, id: &VolumeId) -> Result<Option<Volume>, Error> {
        if let Some(volume) = self.volumes.get(id) {
            return Ok(Some(volume.clone()));
        }
        let volume = self
            .catalog
            .prepare_cached(
                "SELECT mount_point, label, removable, platform_id, last_seen_ms FROM volumes
                 WHERE id = ?1",
            )?
            .query_row([id.as_str()], |row| {
                Ok(Volume {
                    id: id.clone(),
                    mount_point: row.get::<_, String>(0)?.into(),
                    label: row.get(1)?,
                    removable: row.get(2)?,
                    platform_id: row.get(3)?,
                    last_seen_ms: row.get(4)?,
                })
            })
            .optional()?;
        if let Some(volume) = &volume {
            self.volumes.insert(id.clone(), volume.clone());
        }
        Ok(volume)
    }

    /// The volume the folder `folder` is on now, when it is there.
    fn mounted_at(&mut self, folder: &Path) -> Option<Volume> {
        let volume = self
            .folders
            .entry(folder.to_path_buf())
            .or_insert_with(|| volume_of(folder, crate::editor::now_ms()).ok())
            .clone();
        if let Some(volume) = &volume {
            self.volumes
                .entry(volume.id.clone())
                .or_insert_with(|| volume.clone());
        }
        volume
    }

    /// One file of the plan in event `event`: its volume, as the index or its pick records it or
    /// as it is mounted now, the catalog's record deciding whether it is removable; whether it is
    /// offline (not there, on a recorded volume that is not mounted); and its copies when it is
    /// on removable media.
    fn file(
        &mut self,
        index: &Connection,
        indexed: &[PathBuf],
        file: NamedFile,
        event: usize,
    ) -> Result<PlannedFile, Error> {
        let present = file
            .path
            .metadata()
            .is_ok_and(|metadata| metadata.is_file());
        let recorded_id = match &file.volume_id {
            Some(id) => Some(id.clone()),
            None => library_rows::pick_at(self.catalog, &file.path)?.map(|pick| pick.volume_id),
        };
        let recorded = match &recorded_id {
            Some(id) => self.recorded(id)?,
            None => None,
        };
        let volume = match recorded {
            Some(volume) => Some(volume),
            None if present => self.mounted_at(parent(&file.path)),
            None => None,
        };
        let offline = match &volume {
            Some(volume) if !present && !mounted(volume) => Some(volume.label.clone()),
            _ => None,
        };
        let removable = volume
            .as_ref()
            .filter(|volume| volume.removable && offline.is_none())
            .map(|volume| volume.id.clone());
        let copies = match (&removable, &file.signature) {
            (Some(volume), Some(signature)) => {
                self.copies(index, indexed, &file.path, signature.len, volume)?
            }
            _ => Vec::new(),
        };
        Ok(PlannedFile {
            path: file.path,
            event,
            moment: None,
            removable,
            copies,
            offline,
        })
    }

    /// The files the index lists with the name and length of the file at `path`, in an indexed
    /// folder, on a volume other than `volume` that is not removable: a card's copies. One indexed
    /// query (`files_by_name_and_length`).
    fn copies(
        &mut self,
        index: &Connection,
        indexed: &[PathBuf],
        path: &Path,
        len: u64,
        volume: &VolumeId,
    ) -> Result<Vec<PathBuf>, Error> {
        let Some(name) = path.file_name() else {
            return Ok(Vec::new());
        };
        let found: Vec<(String, String)> = index
            .prepare_cached(
                "SELECT path, volume_id FROM files WHERE name = ?1 AND byte_len = ?2 ORDER BY path",
            )?
            .query_map(
                rusqlite::params![name.to_string_lossy(), len as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
            .collect::<Result<_, _>>()?;
        let mut copies = Vec::new();
        for (copy, on) in found {
            let copy = PathBuf::from(copy);
            let Ok(on) = VolumeId::parse(on) else {
                continue;
            };
            if copy == path
                || on == *volume
                || !indexed.iter().any(|folder| copy.starts_with(folder))
                || self.recorded(&on)?.is_some_and(|known| known.removable)
            {
                continue;
            }
            copies.push(copy);
        }
        Ok(copies)
    }
}

/// Where each event of `plan` goes: its own `into` entry (by `event_id`), else the entry that
/// names no event, else the plan's proposal. An existing folder must be in the catalog; a new
/// folder's name follows a folder's rules and must be free among its siblings, and events given
/// the same new folder (the same name, ignoring case, under the same parent) share it. A new
/// folder records the span of the events it is made from, and their identity when it is made
/// from one. Answers the folders and, per event, the index of its folder among them.
pub(crate) fn destinations(
    catalog: &Connection,
    plan: &Plan,
    into: &[DevelopInto],
    now_ms: i64,
) -> Result<(Vec<Destination>, Vec<usize>), Error> {
    let mut by_event: HashMap<&EventId, &FolderChoice> = HashMap::new();
    let mut default = None;
    for entry in into {
        match &entry.event_id {
            None if default.is_some() => {
                return Err(Error::validation(
                    "into names at most one folder with no event_id",
                ));
            }
            None => default = Some(&entry.folder),
            Some(event) => {
                if by_event.insert(event, &entry.folder).is_some() {
                    return Err(Error::validation(format!("into names event {event} twice")));
                }
                if !plan.events.iter().any(|planned| planned.id == *event) {
                    return Err(Error::conflict(format!(
                        "event {event} is not one of these picks' events; plan them again"
                    ))
                    .with_data(json!({"event_id": event})));
                }
            }
        }
    }
    let mut folders: Vec<Destination> = Vec::new();
    let mut of_event = Vec::with_capacity(plan.events.len());
    for event in &plan.events {
        let choice = by_event
            .get(&event.id)
            .copied()
            .or(default)
            .unwrap_or(&event.proposal);
        let at = match choice {
            FolderChoice::Existing { folder_id } => {
                folders::folder(catalog, folder_id)?;
                match folders
                    .iter()
                    .position(|known| matches!(known, Destination::Existing(id) if id == folder_id))
                {
                    Some(at) => at,
                    None => {
                        folders.push(Destination::Existing(folder_id.clone()));
                        folders.len() - 1
                    }
                }
            }
            FolderChoice::New { name, parent_id } => {
                let name = tree::checked_name(name)?;
                let shared = folders.iter().position(|known| match known {
                    Destination::New { folder, .. } => {
                        folder.parent_id == *parent_id && tree::same_name(&folder.name, &name)
                    }
                    Destination::Existing(_) => false,
                });
                match shared {
                    Some(at) => at,
                    None => {
                        let (folder, _) =
                            folders::create(catalog, &name, parent_id.as_ref(), now_ms)?;
                        folders.push(Destination::New {
                            folder,
                            events: Vec::new(),
                        });
                        folders.len() - 1
                    }
                }
            }
        };
        if let Destination::New { events, .. } = &mut folders[at] {
            events.push(event.span.clone());
        }
        of_event.push(at);
    }
    for destination in &mut folders {
        if let Destination::New { folder, events } = destination {
            folder.event = made_from(events);
        }
    }
    Ok((folders, of_event))
}

/// The catalog folder a Develop brings an event's photographs into.
#[derive(Clone, Debug)]
pub(crate) enum Destination {
    Existing(CatalogFolderId),
    /// A folder the Develop makes, as its first batch into it commits, with the spans of the
    /// events it is made from (none for an Undated one).
    New {
        folder: FolderValue,
        events: Vec<Option<EventSpan>>,
    },
}

impl Destination {
    pub(crate) fn id(&self) -> &CatalogFolderId {
        match self {
            Self::Existing(id) => id,
            Self::New { folder, .. } => &folder.id,
        }
    }
}

/// The span a new folder records: every dated event's it is made from, with the event's identity
/// when it is made from exactly one event.
fn made_from(events: &[Option<EventSpan>]) -> Option<EventSpan> {
    let dated: Vec<&EventSpan> = events.iter().flatten().collect();
    let start_ms = dated.iter().map(|span| span.start_ms).min()?;
    let end_ms = dated.iter().map(|span| span.end_ms).max()?;
    let event_id = match (events.len(), dated.as_slice()) {
        (1, [only]) => only.event_id.clone(),
        _ => None,
    };
    Some(EventSpan {
        start_ms,
        end_ms,
        event_id,
    })
}
