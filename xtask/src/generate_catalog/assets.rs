//! The catalog: developed photographs written through the core's seeding API
//! (`luxforge_core::seed`), which owns all the SQL. [`Library`] holds one new catalog: its three
//! volumes, a catalog folder per event it develops (named like "Konstanz · Sep 2026", those at the
//! lake nested in "Bodensee" and those abroad in "Travel"), its photographs in batches, then its
//! collections — "Portfolio", a group holding "Landscapes" and "Nights", the plain "Print order"
//! and the smart "Drone", every photograph by the drone — with their members, the pending picks and
//! the indexed folders. [`history`] fills it with the developed photographs of older trips.
use super::clock::DAY;
use super::disk::{self, Disk, File};
use super::plan::{BODIES, DRONE, Event, Frame, MomentKind, Parent};
use super::random::Random;
use crate::Result;
use luxforge_core::catalog_types::{
    AssetRowId, BodyKey, CatalogFolder, CatalogFolderId, Collection, CollectionId, CollectionKind,
    EventId, EventSpan, FileAvailability, MomentId, Pick, ViewFilter, ViewQuery, ViewSource,
};
use luxforge_core::seed::{CatalogSeeder, SeedAsset, SeedKind};
use luxforge_core::{AssetId, SourceTag};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;

/// Photographs written in one transaction.
const BATCH: usize = 10_000;
/// The plain collections, by their place in [`collections`], and the most members of each:
/// every 37th photograph in "Landscapes", the night frames in "Nights" and every 11th in "Print
/// order".
const LANDSCAPES: (usize, u32) = (1, 2_000);
const NIGHTS: (usize, u32) = (2, 500);
const PRINTS: (usize, u32) = (3, 24);

/// The catalog's collections, parents first: a group, its two collections, a plain collection and
/// a smart one.
fn collections() -> [Collection; 5] {
    let collection = |id: &str, name: &str, parent: Option<&str>, kind, days_ago: i64| Collection {
        id: CollectionId::parse(id).expect("a collection identity"),
        name: name.into(),
        parent_id: parent.map(|id| CollectionId::parse(id).expect("a collection identity")),
        kind,
        query: None,
        created_ms: disk::now_ms() - days_ago * DAY,
        count: None,
    };
    let drone = BODIES[DRONE].key();
    [
        collection(
            "collection-generated-portfolio",
            "Portfolio",
            None,
            CollectionKind::Group,
            400,
        ),
        collection(
            "collection-generated-landscapes",
            "Landscapes",
            Some("collection-generated-portfolio"),
            CollectionKind::Collection,
            399,
        ),
        collection(
            "collection-generated-nights",
            "Nights",
            Some("collection-generated-portfolio"),
            CollectionKind::Collection,
            200,
        ),
        collection(
            "collection-generated-print-order",
            "Print order",
            None,
            CollectionKind::Collection,
            12,
        ),
        Collection {
            query: Some(ViewQuery {
                filter: ViewFilter {
                    cameras: vec![BodyKey(drone)],
                    ..ViewFilter::default()
                },
                ..ViewQuery::of(ViewSource::AllPhotographs)
            }),
            ..collection(
                "collection-generated-drone",
                "Drone",
                None,
                CollectionKind::Smart,
                90,
            )
        },
    ]
}

/// When a photograph was developed, and whether its original is where it was: available,
/// offline or missing ([`Library::add`] marks some available ones changed).
pub struct Developed {
    pub developed_ms: i64,
    pub availability: FileAvailability,
    /// The moment it was developed from, for a bracket's frames.
    pub moment: Option<MomentId>,
}

/// What the catalog holds, for the command's summary and the tests.
#[derive(Default, Debug)]
pub struct Counts {
    pub photographs: u64,
    pub folders: u32,
    pub removed: u64,
    pub offline: u64,
    pub missing: u64,
    pub changed: u64,
    pub picks: usize,
}

/// One new catalog being written.
pub struct Library {
    seeder: CatalogSeeder,
    seed: u64,
    /// The names already used under each parent, in lower case: sibling names are unique.
    names: HashSet<(Option<&'static str>, String)>,
    folders: Vec<CatalogFolder>,
    photographs: Vec<SeedAsset>,
    /// Which plain collections each waiting photograph joins, by index into [`collections`].
    joins: Vec<Vec<usize>>,
    members: Vec<(CollectionId, AssetRowId, i64)>,
    joined: [u32; 5],
    picks: Vec<Pick>,
    pub counts: Counts,
}

impl Library {
    /// A new catalog at `path` with its volumes and the parent folders.
    pub fn create(path: &Path, catalog_id: &str, seed: u64) -> Result<Self> {
        let mut seeder = CatalogSeeder::create(path, catalog_id)?;
        seeder.volumes(&Disk::ALL.map(Disk::volume))?;
        let parents = Parent::ALL.map(|parent| CatalogFolder {
            id: parent_id(parent),
            name: parent.name().into(),
            parent_id: None,
            created_ms: disk::now_ms() - 3000 * DAY,
            event: None,
            count: 0,
            year: None,
        });
        seeder.folders(&parents)?;
        let mut names = HashSet::new();
        for parent in Parent::ALL {
            names.insert((None, parent.name().to_lowercase()));
        }
        Ok(Library {
            seeder,
            seed,
            names,
            folders: Vec::new(),
            photographs: Vec::with_capacity(BATCH),
            joins: Vec::with_capacity(BATCH),
            members: Vec::new(),
            joined: [0; 5],
            picks: Vec::new(),
            counts: Counts::default(),
        })
    }

    /// A new catalog folder for `event`, whose frames are `files`: named after its title and first
    /// month ("Konstanz · Sep 2026"), nested under its place's parent, and carrying its span.
    pub fn folder(&mut self, event: &Event, files: &[File], created_ms: i64) -> CatalogFolderId {
        self.counts.folders += 1;
        let id = CatalogFolderId::parse(format!("folder-generated-{:06}", self.counts.folders))
            .expect("a folder identity");
        let parent = event.place.and_then(|place| place.parent);
        let base = match event.place {
            Some(_) => format!(
                "{} · {} {}",
                event.title,
                event.start.month_name(),
                event.start.year
            ),
            None => event.title.clone(),
        };
        let parent_name = parent.map(Parent::name);
        let mut name = base.clone();
        let mut copy = 1;
        while !self.names.insert((parent_name, name.to_lowercase())) {
            copy += 1;
            name = format!("{base} ({copy})");
        }
        let span = event
            .frames
            .iter()
            .zip(files)
            .filter_map(|(frame, file)| Some((frame.utc()?, &file.path)))
            .min_by_key(|(utc, _)| *utc)
            .map(|(start_ms, first)| EventSpan {
                start_ms,
                end_ms: event
                    .frames
                    .iter()
                    .filter_map(Frame::utc)
                    .max()
                    .unwrap_or(start_ms),
                event_id: Some(EventId::of(first, start_ms)),
            });
        self.folders.push(CatalogFolder {
            id: id.clone(),
            name,
            parent_id: parent.map(parent_id),
            created_ms,
            event: span,
            count: 0,
            year: None,
        });
        id
    }

    /// Adds one developed photograph: `frame` of `event`, kept as `file`, in `folder`. Its place
    /// is the event's when it carries a position. One photograph in 97 has been removed since, and
    /// one available original in 331 is at its place but changed.
    pub fn add(
        &mut self,
        event: &Event,
        frame: &Frame,
        file: &File,
        folder: &CatalogFolderId,
        developed: Developed,
    ) -> Result {
        self.counts.photographs += 1;
        let number = self.counts.photographs;
        let body = frame.body();
        let header = disk::header(frame, false, 0)?;
        let fingerprint = Sha256::new()
            .chain_update(self.seed.to_le_bytes())
            .chain_update(file.path.as_os_str().as_encoded_bytes())
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let removed_ms = (number % 97 == 13).then(|| {
            (developed.developed_ms + (number as i64 % 50 + 1) * DAY).min(disk::now_ms() - DAY)
        });
        let availability = match developed.availability {
            FileAvailability::Available if number % 331 == 17 => FileAvailability::Changed,
            availability => availability,
        };
        self.counts.removed += u64::from(removed_ms.is_some());
        match availability {
            FileAvailability::Offline => self.counts.offline += 1,
            FileAvailability::Missing => self.counts.missing += 1,
            FileAvailability::Changed => self.counts.changed += 1,
            FileAvailability::Available => {}
        }
        let mut joins = Vec::new();
        for ((collection, most), eligible) in [
            (LANDSCAPES, number % 37 == 1),
            (NIGHTS, frame.scene.night),
            (PRINTS, number % 11 == 3),
        ] {
            if eligible && self.joined[collection] < most {
                self.joined[collection] += 1;
                joins.push(collection);
            }
        }
        self.joins.push(joins);
        self.photographs.push(SeedAsset {
            id: AssetId::parse(format!("asset-{number:032x}"))?,
            kind: match file.kind {
                SourceTag::Jpeg => SeedKind::Jpeg,
                SourceTag::Raw => SeedKind::Raw,
            },
            locator: file.path.clone(),
            fingerprint,
            file_identity: file.identity_text(),
            byte_len: file.signature.len,
            width: body.size.0,
            height: body.size.1,
            catalog_folder_id: folder.clone(),
            volume_id: file.disk.id(),
            developed_ms: developed.developed_ms,
            removed_ms,
            availability,
            checked_ms: disk::now_ms() - (number as i64 % 360) * 60_000,
            develop_moment: developed.moment,
            header,
            place: frame
                .gps
                .and(event.place)
                .map(|place| place.name.to_owned()),
        });
        if self.photographs.len() >= BATCH {
            self.flush()?;
        }
        Ok(())
    }

    /// A pending pick of a file of the index.
    pub fn pick(&mut self, pick: Pick) {
        self.picks.push(pick);
    }

    /// Writes the waiting folders, then the waiting photographs, noting their collections.
    fn flush(&mut self) -> Result {
        if !self.folders.is_empty() {
            self.seeder.folders(&self.folders)?;
            self.folders.clear();
        }
        if self.photographs.is_empty() {
            return Ok(());
        }
        let rows = self.seeder.assets(&self.photographs)?;
        let all = collections();
        for (row, joins) in rows.into_iter().zip(self.joins.drain(..)) {
            for collection in joins {
                self.members
                    .push((all[collection].id.clone(), row, disk::now_ms()));
            }
        }
        self.photographs.clear();
        Ok(())
    }

    /// Writes what is still waiting, the collections and their members, the picks and, when the
    /// catalog has an index, its indexed folders; and closes the catalog.
    pub fn finish(mut self, indexed: bool) -> Result<Counts> {
        self.flush()?;
        self.seeder.collections(&collections())?;
        self.seeder.members(&self.members)?;
        self.counts.picks = self.picks.len();
        self.seeder.picks(&self.picks)?;
        if indexed {
            let folders: Vec<_> = Disk::ALL
                .iter()
                .filter_map(|d| d.indexed_folder())
                .collect();
            self.seeder.indexed_folders(&folders)?;
        }
        self.seeder.finish()?;
        Ok(self.counts)
    }
}

fn parent_id(parent: Parent) -> CatalogFolderId {
    CatalogFolderId::parse(format!("folder-generated-{}", parent.name().to_lowercase()))
        .expect("a folder identity")
}

/// The moment a bracket frame was developed from: its first frame's path and instant.
pub fn bracket_moment(frame: &Frame, first: Option<(&File, &Frame)>) -> Option<MomentId> {
    match (frame.kind, first) {
        (MomentKind::Bracket(_), Some((file, first))) => {
            Some(MomentId::of(&file.path, first.utc()?))
        }
        _ => None,
    }
}

/// The photographs developed from older trips, every frame of each: those on the external drive
/// offline, and every fiftieth trip on the internal disk from the second missing (its folder
/// gone). Answers how many it added.
pub fn history(
    library: &mut Library,
    seed: u64,
    plan: super::plan::Plan,
    inodes: &mut u64,
) -> Result<u64> {
    let mut added = 0;
    let mut internal = 0;
    for (index, event) in plan.enumerate() {
        let mut random = Random::stream(seed, 0x4153_5345_5400_0000 + index as u64);
        let files: Vec<File> = event
            .frames
            .iter()
            .map(|frame| {
                *inodes += 1;
                disk::locate(&event, frame, *inodes)
            })
            .collect();
        let end = event.frames.iter().filter_map(Frame::utc).max();
        let developed = match end {
            Some(end) => disk::after(&mut random, end, 30),
            None => disk::now_ms() - 40 * DAY,
        };
        let gone = files
            .first()
            .is_some_and(|file| file.disk == Disk::Internal)
            && {
                internal += 1;
                internal % 50 == 2
            };
        let folder = library.folder(&event, &files, developed);
        let mut first = None;
        for (offset, (frame, file)) in event.frames.iter().zip(&files).enumerate() {
            if frame.index == 0 {
                first = Some((file, frame));
            }
            let availability = if !file.disk.online() {
                FileAvailability::Offline
            } else if gone {
                FileAvailability::Missing
            } else {
                FileAvailability::Available
            };
            let developed_ms = developed + offset as i64 * 1_500;
            library.add(
                &event,
                frame,
                file,
                &folder,
                Developed {
                    developed_ms,
                    availability,
                    moment: bracket_moment(frame, first),
                },
            )?;
            added += 1;
        }
    }
    Ok(added)
}
