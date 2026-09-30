//! Developing picks (`docs/design/catalog.md`, "Developing picks", P8, P9, P14, P15): bringing
//! files into the catalog as photographs, into the catalog folder chosen for each event, on the
//! develop lane — lane C's worker, off the owner and off the source worker.
//!
//! 1. **Plan** on the owner (`plan.rs`): the files by event and moment (`crate::organize`, over
//!    what the index lists in their folders, `frames.rs`), each event's proposed folder, and which
//!    files are on removable media, have a copy in an indexed folder, or are offline. `pick.plan`
//!    answers it; `pick.develop` plans the same way and takes each event's folder from its `into`
//!    ([`plan::destinations`]).
//! 2. **Read** on the worker, one file at a time (`read.rs`): one bounded read streaming the
//!    fingerprint, the header from the same bytes and the interpretation without developing; a
//!    card's pick from its verified copy when asked ([`develop_file`]).
//! 3. **Commit** on the owner, in batches (`commit.rs`): the first batch one file, so Develop can
//!    show it at once, then up to [`BATCH_FILES`] files or [`BATCH_TIME`], never across an event.
//!    Each batch is one library change, a part of the Develop's request
//!    ([`journal::apply_part`](super::journal::apply_part)): the folder it needs, its new
//!    photographs (`developed-asset`, written by the lane and recorded as written), relinked
//!    originals (`asset-source`) and the committed picks cleared. A file is linked to the
//!    photograph that already has its bytes, relinks a photograph whose original is not there when
//!    its name, length and fingerprint match, or becomes a new photograph.
//! 4. **Send back** (`send_back.rs`): a photograph with nothing but its Original leaves the
//!    catalog and its file is picked again; the `developed-asset` item going absent does it, so a
//!    Develop's undo sends its photographs back too.
//!
//! A cancel stops between files: committed batches stay, whole, and nothing of the batch being
//! gathered is written. A file that fails is reported and stays picked. Originals are only read.
mod commit;
mod frames;
mod plan;
mod read;
pub(crate) mod send_back;

#[cfg(test)]
pub(crate) mod develop_picks_tests;

pub(crate) use commit::{Decided, Refused, decide, failure, write};
#[allow(
    unused_imports,
    reason = "the views lane organizes the files it views from the same rows"
)]
pub(crate) use frames::{Frames, frames};
pub(crate) use plan::{Destination, Plan, PlannedFile, destinations, plan};
pub(crate) use read::{ReadFile, from_copy, read};

use super::{availability::SYSTEM_ACTOR, journal::Request, locate::Phase};
use crate::{
    EditorService, EditorState, Error, JobId,
    catalog_types::{
        CatalogFolderId, DevelopOutcome, DevelopReport, DevelopedPick, FolderValue,
        LibraryChangeDetail, LibraryItem, MomentId,
    },
    editor::now_ms,
    jobs::JobControl,
};
use rusqlite::OptionalExtension;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

impl EditorService {
    /// Bring the file at `path` into the catalog and prepare it, on the caller's thread, blocking:
    /// a Develop of that one file — the develop lane's read ([`read`]), then the owner's decision
    /// and commit ([`decide`], [`write`]) as one library change by the actor `system` — into the
    /// top-level catalog folder named after its folder on disk, made when there is none, then its
    /// preparation ([`Self::prepare`]). A file whose bytes are already a photograph's is linked to
    /// it, and one that is a missing original's relinks it, as a Develop does. For tests, tools
    /// and the harness; a client sends `pick.develop`, `source.prepare` and `job.adopt`.
    pub fn import(&mut self, path: &Path) -> Result<EditorState, Error> {
        let file = read(path, &JobControl::new(), &|_| {})?;
        let now = now_ms();
        let name = plan::parent(&file.path).file_name().map_or_else(
            || "Imported".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let existing: Option<String> = self
            .connection
            .prepare_cached(
                "SELECT id FROM catalog_folders WHERE parent_id IS NULL AND name = ?1 COLLATE NOCASE",
            )?
            .query_row([&name], |row| row.get(0))
            .optional()?;
        let destination = match existing {
            Some(id) => Destination::Existing(CatalogFolderId::parse(id)?),
            None => Destination::New {
                folder: FolderValue {
                    id: CatalogFolderId::new(),
                    name,
                    parent_id: None,
                    created_ms: now,
                    event: None,
                },
                events: Vec::new(),
            },
        };
        let developed = Developed {
            pick: file.path.clone(),
            used: None,
            file,
            moment: None,
        };
        let (decided, refused) = decide(self, vec![developed], now)?;
        if let Some(refused) = refused.into_iter().next() {
            return Err(refused.error);
        }
        let asset = decided
            .first()
            .map(|decided| decided.reported().asset_id)
            .ok_or_else(|| Error::internal("a developed file was neither committed nor refused"))?;
        let request_id = JobId::new().to_string();
        let request = Request {
            method: "pick.develop",
            actor: SYSTEM_ACTOR,
            request_id: &request_id,
        };
        let artifact_root = self.artifact_root().to_path_buf();
        self.library_write(|tx| write(tx, request, &artifact_root, &destination, &decided, now))?;
        let needs = self.entry_needs(&asset, None)?;
        self.prepare(&needs)
    }
}

/// The most files a batch after the first commits at once.
pub(crate) const BATCH_FILES: usize = 100;
/// The longest a batch after the first gathers files before it commits, so a long Develop's
/// photographs keep appearing and a cancel loses little work.
pub(crate) const BATCH_TIME: Duration = Duration::from_secs(5);

/// One pick read and interpreted on the worker, for the owner to commit.
#[derive(Debug)]
pub(crate) struct Developed {
    /// The pick's path, which the report names and whose pick the commit clears.
    pub pick: PathBuf,
    /// The copy developed in its place, when a card's pick was developed from a verified copy.
    pub used: Option<PathBuf>,
    /// The file the photograph is made of: the pick's, or its copy's.
    pub file: ReadFile,
    pub moment: Option<MomentId>,
}

/// Read one planned file for a Develop, on the worker: refused at once when its volume is not
/// connected; otherwise read and interpreted, and for a card's pick with copies when `use_copies`
/// is set, developed from the first copy that proves to hold its bytes. With no such copy it is
/// developed from the card when `confirm_removable` is set and refused otherwise (`conflict`,
/// naming why). `pause` hears each phase, for a test to hold it there; [`Phase::Verified`] is
/// the file ready to commit.
pub(crate) fn develop_file(
    file: &PlannedFile,
    use_copies: bool,
    confirm_removable: bool,
    control: &JobControl,
    pause: &dyn Fn(Phase),
) -> Result<Developed, Error> {
    if let Some(label) = &file.offline {
        return Err(Error::source_unavailable(format!(
            "the volume {label} is not connected"
        )));
    }
    let card = read(&file.path, control, pause)?;
    let (file_read, used) = match &file.removable {
        Some(_) if use_copies && !file.copies.is_empty() => {
            match from_copy(&card, &file.copies, control, pause)? {
                Some(copy) => {
                    let used = copy.path.clone();
                    (copy, Some(used))
                }
                None if confirm_removable => (card, None),
                None => {
                    return Err(Error::conflict(format!(
                        "no copy of {} in an indexed folder holds its bytes; confirm_removable \
                         develops it from the removable volume",
                        file.path.display()
                    )));
                }
            }
        }
        _ => (card, None),
    };
    pause(Phase::Verified);
    Ok(Developed {
        pick: file.path.clone(),
        used,
        file: file_read,
        moment: file.moment.clone(),
    })
}

/// A Develop's report as its request's recorded changes say it, for a retry answered after a
/// restart ([`journal::parts`](super::journal::parts)): each change, and each file whose photograph
/// a change created (`developed-asset`) or relinked (`asset-source`), with the pick it cleared
/// after it; a copy developed in a pick's place is its `used`. A pick linked to a photograph
/// already in the catalog records no photograph, so it is not listed; nothing failed is either.
pub(crate) fn recorded_report(parts: &[LibraryChangeDetail]) -> DevelopReport {
    let mut report = DevelopReport::default();
    for part in parts {
        report.changes.push(part.change.sequence);
        let mut pending: Option<DevelopedPick> = None;
        for row in &part.rows {
            let located = |key: &str| {
                row.after
                    .as_ref()
                    .and_then(|after| after.get(key))
                    .and_then(|value| value.as_str())
                    .map(PathBuf::from)
            };
            let developed = match &row.item {
                LibraryItem::DevelopedAsset { asset_id } => {
                    located("path").map(|path| (asset_id.clone(), path, DevelopOutcome::Created))
                }
                LibraryItem::AssetSource { asset_id } => located("locator")
                    .map(|path| (asset_id.clone(), path, DevelopOutcome::Relinked)),
                LibraryItem::Pick { path } if row.after.is_none() => {
                    if let Some(mut done) = pending.take() {
                        if done.path != *path {
                            done.used = Some(std::mem::replace(&mut done.path, path.clone()));
                        }
                        report.developed.push(done);
                    }
                    None
                }
                _ => None,
            };
            if let Some((asset_id, path, outcome)) = developed {
                report.developed.extend(pending.take());
                pending = Some(DevelopedPick {
                    path,
                    used: None,
                    asset_id,
                    outcome,
                });
            }
        }
        report.developed.extend(pending);
    }
    report
}
