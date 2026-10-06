//! `--files N`: the index (`<catalog>.index/index.sqlite`) of N files with no image files, written
//! through the core's seeding API. The files are the [plan](super::plan::Plan::files)'s: the
//! September card, dumps and user-named folders, and older trip folders, the older half of them on
//! the external drive that is not connected. Headers read as the image folders write them; one
//! file in two hundred is still pending and two could not be read. Some moments were picked and
//! some developed already: a quarter of the brackets (all frames, linked by their moment), one
//! burst in seven (one frame) and one single in twenty are photographs in the catalog, and a few
//! more are picked and waiting.
use super::Run;
use super::assets::{Developed, Library, bracket_moment};
use super::clock::DAY;
use super::disk::{self, Disk, File};
use super::plan::{Frame, MomentKind, Plan};
use super::random::Random;
use crate::Result;
use luxforge_core::catalog_types::{FileAvailability, FileRecord, HeaderState, Pick};
use luxforge_core::seed::IndexSeeder;

/// Files written in one transaction.
const BATCH: usize = 10_000;

/// What became of one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    Browsed,
    Picked,
    Developed,
}

/// What `write` made.
pub struct Written {
    pub files: u64,
    pub events: u64,
    pub developed: u64,
}

/// Writes the index of `plan`'s `total` files beside the run's catalog, developing into `library`
/// those the plan's moments developed, at most `most_developed`.
pub fn write(
    run: &Run,
    plan: Plan,
    total: u32,
    library: &mut Library,
    most_developed: Option<u64>,
    inodes: &mut u64,
) -> Result<Written> {
    let seed = run.seed;
    let mut index = IndexSeeder::create(run.catalog, run.catalog_id)?;
    let mut batch = Vec::with_capacity(BATCH);
    let mut per_disk = [0u32; Disk::ALL.len()];
    let unreadable = [u64::from(total) / 3, u64::from(total) * 2 / 3];
    let mut written = Written {
        files: 0,
        events: 0,
        developed: 0,
    };
    for (event_index, event) in plan.enumerate() {
        written.events += 1;
        let mut random = Random::stream(seed, 0x4649_4c45_5300_0000 + event_index as u64);
        let files: Vec<File> = event
            .frames
            .iter()
            .map(|frame| {
                *inodes += 1;
                disk::locate(&event, frame, *inodes)
            })
            .collect();
        let fates = fates(&event.frames, &mut random);
        let end = event.frames.iter().filter_map(Frame::utc).max();
        let developed_at = match end {
            Some(end) => disk::after(&mut random, end, 20),
            None => disk::now_ms() - 5 * DAY,
        };
        let mut folder = None;
        let mut first = None;
        for (offset, ((frame, file), fate)) in
            event.frames.iter().zip(&files).zip(fates).enumerate()
        {
            if frame.index == 0 {
                first = Some((file, frame));
            }
            let number = written.files;
            written.files += 1;
            per_disk[file.disk as usize] += 1;
            let header = if unreadable.contains(&number) {
                HeaderState::Unreadable("the TIFF structure ends before its first IFD".into())
            } else if number % 199 == 57 {
                HeaderState::Pending
            } else {
                HeaderState::Ok(Box::new(disk::header(frame, true, file.inode())?))
            };
            let readable = matches!(header, HeaderState::Ok(_));
            let last_seen_ms = file.disk.volume().last_seen_ms;
            if readable && fate == Fate::Picked {
                library.pick(Pick {
                    path: file.path.clone(),
                    signature: file.signature,
                    volume_id: file.disk.id(),
                    actor: if offset % 4 == 0 {
                        "agent:claude"
                    } else {
                        "desktop"
                    }
                    .into(),
                    request_id: format!("generated-pick-{number:07}"),
                    picked_ms: disk::after(&mut random, end.unwrap_or(developed_at), 3),
                    file_id: None,
                });
            }
            let room = most_developed.is_none_or(|most| written.developed < most);
            if readable && fate == Fate::Developed && room {
                let folder = folder
                    .get_or_insert_with(|| library.folder(&event, &files, developed_at))
                    .clone();
                let developed_ms = developed_at + offset as i64 * 2_000;
                let availability = if file.disk.online() {
                    FileAvailability::Available
                } else {
                    FileAvailability::Offline
                };
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
                written.developed += 1;
            }
            batch.push(FileRecord {
                path: file.path.clone(),
                folder: file.folder().to_path_buf(),
                name: file.name(),
                volume_id: file.disk.id(),
                signature: file.signature,
                kind: file.kind,
                header,
                last_seen_ms,
            });
            if batch.len() >= BATCH {
                index.files(&batch)?;
                batch.clear();
            }
        }
    }
    index.files(&batch)?;
    let roots: Vec<_> = Disk::ALL
        .iter()
        .zip(per_disk)
        .map(|(disk, files)| disk.index_root(files))
        .collect();
    index.roots(&roots)?;
    index.finish()?;
    Ok(written)
}

/// What became of each frame of an event: a quarter of the brackets developed whole and a few more
/// picked whole; one burst in seven with one frame developed and a few with one picked; one single
/// in twenty developed and one in fifty picked.
fn fates(frames: &[Frame], random: &mut Random) -> Vec<Fate> {
    let mut fates = vec![Fate::Browsed; frames.len()];
    let mut start = 0;
    while start < frames.len() {
        let moment = frames[start].moment;
        let end = frames[start..]
            .iter()
            .position(|frame| frame.moment != moment)
            .map_or(frames.len(), |length| start + length);
        let roll = random.unit();
        let (developed, picked) = match frames[start].kind {
            MomentKind::Bracket(_) => (0.25, 0.32),
            MomentKind::Burst => (0.15, 0.22),
            MomentKind::Single => (0.05, 0.07),
        };
        let fate = if roll < developed {
            Fate::Developed
        } else if roll < picked {
            Fate::Picked
        } else {
            Fate::Browsed
        };
        if frames[start].kind == MomentKind::Burst {
            let one = random.between(start as i64, end as i64 - 1) as usize;
            fates[one] = fate;
        } else {
            fates[start..end].fill(fate);
        }
        start = end;
    }
    fates
}
