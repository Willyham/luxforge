//! Where developed photographs' originals are, as last observed (`assets.availability` and
//! `checked_ms`): available, offline, missing or changed (`docs/design/catalog.md`, "Missing
//! originals").
//!
//! Availability is an observation, not a library change: it is written outside the journal, is
//! never undone, and the next look replaces it. Three things observe it:
//!
//! - **A check** (`source.check`): the owner reads each photograph's locator, length, identity and
//!   volume ([`plan`]); a worker looks at the disk ([`observe`]); the owner records what it saw
//!   ([`record`]). A volume whose mount point is gone, or is another volume now, makes all of its
//!   photographs offline with one look ([`mounted`]); otherwise each locator is looked at: the
//!   recorded file (its length and file identity) is available, another file there is changed, no
//!   file is missing. A photograph whose file moved within its volume is found again by its file
//!   identity in the index and confirmed by its fingerprint ([`Found`]), and one whose own file
//!   is still at its locator under another identity (a volume mounted again under another device
//!   number) is confirmed by its fingerprint too; each is relinked by the owner as one library
//!   change by the `system` actor ([`relinks`]); nothing else is relinked without being asked.
//! - **A refusal**: a preparation, an evaluation or an export whose original is not there is
//!   refused naming why ([`refusal`]), and records what it found.
//! - **A preparation** that read and verified the original records it available.
use super::{
    journal::Desired,
    locate::{self, Phase},
};
use crate::{
    AssetId, AssetRecord, Error, ErrorKind,
    catalog_types::{
        AssetRowId, AssetSourceValue, AvailabilityRow, FileAvailability, FileIdentity,
        LibraryChangeRow, LibraryItem, Volume, VolumeId,
    },
    editor::{SourceSignature, library_rows, now_ms, source_signature},
    index::volume_of,
    jobs::JobControl,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::json;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Who a relink the check makes on its own is attributed to: no client, so no client's undo takes
/// it back.
pub(crate) const SYSTEM_ACTOR: &str = "system";

/// How many photographs a check looks at between two progress reports.
const PROGRESS_EVERY: usize = 256;

/// One photograph a check looks at, as the catalog records it.
#[derive(Clone, Debug)]
pub(crate) struct Subject {
    pub asset_id: AssetId,
    /// Where the catalog looks for its original.
    pub source: AssetSourceValue,
    pub byte_len: u64,
    pub fingerprint: String,
    /// The availability the catalog holds for it.
    pub stored: FileAvailability,
}

/// The photographs of one volume a check looks at, with the volume as the catalog records it when
/// it does.
#[derive(Debug)]
pub(crate) struct VolumeGroup {
    pub volume: Option<Volume>,
    pub subjects: Vec<Subject>,
}

/// What a check looks at: the photographs it names, by volume, in the order their volumes were
/// first named.
#[derive(Debug, Default)]
pub(crate) struct Plan {
    pub groups: Vec<VolumeGroup>,
    pub count: usize,
}

/// One photograph as a check found it at its locator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Observation {
    pub asset_id: AssetId,
    pub availability: FileAvailability,
    /// What the catalog held before, so the owner knows whether anything changed.
    pub stored: FileAvailability,
}

/// A photograph whose file a check found again elsewhere on its volume, by its file identity,
/// with the fingerprint the catalog stores.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    pub asset_id: AssetId,
    /// Where the catalog looked for it when the check read it.
    pub was: AssetSourceValue,
    /// Where it is now.
    pub now: AssetSourceValue,
    /// The file's signature when its fingerprint was verified.
    pub signature: SourceSignature,
}

/// What a check saw, for the owner to record.
#[derive(Debug)]
pub(crate) struct Observed {
    pub checked_ms: i64,
    /// Each photograph in the order named.
    pub rows: Vec<Observation>,
    pub found: Vec<Found>,
}

/// The photographs `assets` names, with what the catalog records of each and its volume, grouped
/// by volume. On the owner: one indexed read per photograph.
pub(crate) fn plan(
    connection: &Connection,
    assets: &[(AssetId, AssetRowId)],
) -> Result<Plan, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT a.locator, a.source_folder, a.volume_id, a.file_identity, a.byte_len,
             a.fingerprint, a.availability, v.mount_point, v.label, v.removable, v.platform_id,
             v.last_seen_ms
         FROM assets a LEFT JOIN volumes v ON v.id = a.volume_id WHERE a.row_id = ?1",
    )?;
    let mut plan = Plan::default();
    let mut groups: HashMap<VolumeId, usize> = HashMap::new();
    for (asset_id, row) in assets {
        let read = statement
            .query_row([row.0], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<bool>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                ))
            })
            .optional()?
            .ok_or_else(|| Error::validation(format!("unknown asset {asset_id}")))?;
        let (
            locator,
            source_folder,
            volume_id,
            file_identity,
            byte_len,
            fingerprint,
            stored,
            mount_point,
            label,
            removable,
            platform_id,
            last_seen_ms,
        ) = read;
        let volume_id = VolumeId::parse(volume_id)?;
        let volume = match (mount_point, label, removable, last_seen_ms) {
            (Some(mount_point), Some(label), Some(removable), Some(last_seen_ms)) => Some(Volume {
                id: volume_id.clone(),
                mount_point: mount_point.into(),
                label,
                removable,
                platform_id,
                last_seen_ms,
            }),
            _ => None,
        };
        let subject = Subject {
            asset_id: asset_id.clone(),
            source: AssetSourceValue {
                locator: locator.into(),
                source_folder: source_folder.into(),
                volume_id: volume_id.clone(),
                file_identity,
            },
            byte_len: byte_len as u64,
            fingerprint,
            stored: FileAvailability::parse(&stored).unwrap_or_default(),
        };
        let group = *groups.entry(volume_id).or_insert_with(|| {
            plan.groups.push(VolumeGroup {
                volume,
                subjects: Vec::new(),
            });
            plan.groups.len() - 1
        });
        plan.groups[group].subjects.push(subject);
        plan.count += 1;
    }
    Ok(plan)
}

/// Whether `volume` is mounted where the catalog last saw it: its mount point is there, and is that
/// volume now rather than a bare folder or another volume mounted in its place. One look at the
/// mount point, whatever the volume holds.
pub(crate) fn mounted(volume: &Volume) -> bool {
    volume_of(&volume.mount_point, 0).is_ok_and(|now| now.id == volume.id)
}

/// Look at each photograph of `plan` on the disk, off the owner: a volume that is not mounted makes
/// all of its photographs offline at once; otherwise each locator is looked at, and a missing or
/// replaced original is looked for by its file identity in `index` (the index's `files`, which
/// lists moved files under their new paths) and confirmed by a streamed fingerprint, one file at a
/// time. Stops at the next photograph, or the next chunk of a file, once `control` is cancelled.
pub(crate) fn observe(
    plan: Plan,
    index: Option<&Connection>,
    control: &JobControl,
    pause: &dyn Fn(Phase),
) -> Result<Observed, Error> {
    let checked_ms = now_ms();
    let total = plan.count;
    let mut rows = Vec::with_capacity(total);
    let mut found = Vec::new();
    let report = |done: usize| {
        control.set_progress(
            Some(done as f64 / total.max(1) as f64),
            &format!("{done} of {total} photographs"),
        );
    };
    for group in plan.groups {
        control.checkpoint()?;
        let offline = group.volume.as_ref().is_some_and(|volume| !mounted(volume));
        for subject in group.subjects {
            let availability = if offline {
                FileAvailability::Offline
            } else {
                control.checkpoint()?;
                let (availability, moved) = look(&subject);
                if moved {
                    let again = match renumbered(&subject, control, pause)? {
                        Some(again) => Some(again),
                        None => match index {
                            Some(index) => find_by_identity(index, &subject, control, pause)?,
                            None => None,
                        },
                    };
                    found.extend(again);
                }
                availability
            };
            rows.push(Observation {
                asset_id: subject.asset_id,
                availability,
                stored: subject.stored,
            });
            if rows.len() % PROGRESS_EVERY == 0 {
                report(rows.len());
            }
        }
        report(rows.len());
    }
    Ok(Observed {
        checked_ms,
        rows,
        found,
    })
}

/// What is at a photograph's locator, and whether its file may have moved: nothing there, or
/// another file (another identity) in its place.
fn look(subject: &Subject) -> (FileAvailability, bool) {
    let locator = &subject.source.locator;
    match locator.metadata() {
        Ok(metadata) if metadata.is_file() => {
            let signature = source_signature(locator, &metadata);
            if signature.file_identity() != subject.source.file_identity {
                (FileAvailability::Changed, true)
            } else if signature.byte_len() != subject.byte_len {
                (FileAvailability::Changed, false)
            } else {
                (FileAvailability::Available, false)
            }
        }
        Ok(_) => (FileAvailability::Changed, true),
        Err(_) => (FileAvailability::Missing, true),
    }
}

/// The photograph's own file at its locator when only its file identity changed, confirmed by its
/// fingerprint: a volume mounted again after another disk may be given another device number,
/// which renumbers the identity of every file on it. Anything else at the locator is not it, and
/// only a cancel stops the check.
fn renumbered(
    subject: &Subject,
    control: &JobControl,
    pause: &dyn Fn(Phase),
) -> Result<Option<Found>, Error> {
    let locator = &subject.source.locator;
    let Ok(metadata) = locator.metadata() else {
        return Ok(None);
    };
    let signature = source_signature(locator, &metadata);
    if !metadata.is_file()
        || signature.byte_len() != subject.byte_len
        || signature.file_identity() == subject.source.file_identity
    {
        return Ok(None);
    }
    match locate::verify_file(
        locator,
        &signature,
        &subject.fingerprint,
        control,
        pause,
        &|_, _| {},
    ) {
        Ok(signature) => Ok(Some(Found {
            asset_id: subject.asset_id.clone(),
            was: subject.source.clone(),
            now: locate::source_value(
                locator,
                subject.source.volume_id.clone(),
                signature.file_identity(),
            ),
            signature,
        })),
        Err(error) if error.kind == ErrorKind::Cancelled => Err(error),
        Err(_) => Ok(None),
    }
}

/// The device and inode a catalog file identity names (`unix:<device>:<inode>`), as the index
/// stores them; none for another platform's identity.
fn identity_columns(identity: &str) -> Option<(i64, i64)> {
    let (device, inode) = identity.strip_prefix("unix:")?.split_once(':')?;
    Some(
        FileIdentity {
            device: device.parse().ok()?,
            inode: inode.parse().ok()?,
        }
        .to_columns(),
    )
}

/// The file the index lists with the photograph's file identity, when it is still that file, of
/// the original's length, and its bytes are the original's. The index is a cache: a stale row, an
/// unreadable file or a mismatch is simply not found; only a cancel stops the check.
fn find_by_identity(
    index: &Connection,
    subject: &Subject,
    control: &JobControl,
    pause: &dyn Fn(Phase),
) -> Result<Option<Found>, Error> {
    let Some((device, inode)) = identity_columns(&subject.source.file_identity) else {
        return Ok(None);
    };
    let listed: Vec<String> = index
        .prepare_cached("SELECT path FROM files WHERE device = ?1 AND inode = ?2")
        .and_then(|mut statement| {
            statement
                .query_map(params![device, inode], |row| row.get(0))?
                .collect()
        })
        .unwrap_or_default();
    for path in listed {
        let Ok(path) = PathBuf::from(path).canonicalize() else {
            continue;
        };
        if path == subject.source.locator {
            continue;
        }
        let Ok(metadata) = path.metadata() else {
            continue;
        };
        let signature = source_signature(&path, &metadata);
        if !metadata.is_file()
            || signature.file_identity() != subject.source.file_identity
            || signature.byte_len() != subject.byte_len
        {
            continue;
        }
        match locate::verify_file(
            &path,
            &signature,
            &subject.fingerprint,
            control,
            pause,
            &|_, _| {},
        ) {
            Ok(signature) => {
                return Ok(Some(Found {
                    asset_id: subject.asset_id.clone(),
                    was: subject.source.clone(),
                    now: locate::source_value(
                        &path,
                        subject.source.volume_id.clone(),
                        signature.file_identity(),
                    ),
                    signature,
                }));
            }
            Err(error) if error.kind == ErrorKind::Cancelled => return Err(error),
            Err(_) => {}
        }
    }
    Ok(None)
}

/// The relinks the owner may commit of what a check found: each photograph still pointing where
/// the check read it, at a file that is still the one verified and that no other photograph names.
/// Any other is left as the check observed it.
pub(crate) fn relinks<'a>(
    tx: &Transaction<'_>,
    found: &'a [Found],
) -> Result<Vec<&'a Found>, Error> {
    let mut relinks = Vec::with_capacity(found.len());
    for found in found {
        if library_rows::asset_source(tx, &found.asset_id)?.as_ref() != Some(&found.was)
            || !locate::unchanged(&found.now.locator, &found.signature)
            || locate::claimed_by(
                tx,
                &found.asset_id,
                &found.now.locator,
                &found.now.file_identity,
            )?
            .is_some()
        {
            continue;
        }
        relinks.push(found);
    }
    Ok(relinks)
}

/// The library change of `relinks`: each photograph's `asset-source` item pointed at its file.
pub(crate) fn relink_changes(relinks: &[&Found]) -> Result<Vec<(LibraryItem, Desired)>, Error> {
    relinks
        .iter()
        .map(|found| {
            Ok((
                LibraryItem::AssetSource {
                    asset_id: found.asset_id.clone(),
                },
                Desired::Value(Some(locate::encode_source(&found.now)?)),
            ))
        })
        .collect()
}

/// The label of the relinks a check made: "Found DSC_0412.NEF again", "Found 3 originals again".
pub(crate) fn relink_label(rows: &[LibraryChangeRow]) -> String {
    match rows {
        [row] => format!("Found {} again", locate::file_name_of(row)),
        rows => format!("Found {} originals again", rows.len()),
    }
}

/// Record what a check observed, with the time it looked, each photograph's row as it is now.
pub(crate) fn record(tx: &Transaction<'_>, rows: &[AvailabilityRow]) -> Result<(), Error> {
    let mut statement =
        tx.prepare_cached("UPDATE assets SET availability = ?1, checked_ms = ?2 WHERE id = ?3")?;
    for row in rows {
        statement.execute(params![
            row.availability.as_str(),
            row.checked_ms,
            row.asset_id.as_str()
        ])?;
    }
    Ok(())
}

/// Record one observation made on the way — a refusal, a preparation — when it differs from what
/// the catalog holds; an unchanged one writes nothing, so a client that keeps asking for a missing
/// original costs no write per request.
pub(crate) fn observed(
    connection: &Connection,
    asset: &AssetId,
    availability: FileAvailability,
    checked_ms: i64,
) -> Result<(), Error> {
    connection
        .prepare_cached(
            "UPDATE assets SET availability = ?1, checked_ms = ?2
             WHERE id = ?3 AND availability <> ?1",
        )?
        .execute(params![availability.as_str(), checked_ms, asset.as_str()])?;
    Ok(())
}

/// The volume the catalog records a photograph's original on, when it does.
fn volume_of_asset(connection: &Connection, asset: &AssetId) -> Result<Option<Volume>, Error> {
    let read = connection
        .prepare_cached(
            "SELECT v.id, v.mount_point, v.label, v.removable, v.platform_id, v.last_seen_ms
             FROM assets a JOIN volumes v ON v.id = a.volume_id WHERE a.id = ?1",
        )?
        .query_row([asset.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .optional()?;
    read.map(
        |(id, mount_point, label, removable, platform_id, last_seen_ms)| {
            Ok(Volume {
                id: VolumeId::parse(id)?,
                mount_point: mount_point.into(),
                label,
                removable,
                platform_id,
                last_seen_ms,
            })
        },
    )
    .transpose()
}

/// The refusal of an original that is not there, naming why: its volume is not connected, the
/// file is missing from its folder, or another file is at its path (`changed`, when the file at the
/// locator is not the recorded one). `data` carries `{availability, locator, volume?}`, the volume
/// as the catalog records it when the file is not at its locator, so a client can say which drive
/// to connect or offer Locate. What was found is recorded ([`observed`]); failing to record it
/// leaves the refusal as it is.
pub(crate) fn refusal(connection: &Connection, asset: &AssetRecord, changed: bool) -> Error {
    let volume = if changed {
        None
    } else {
        volume_of_asset(connection, &asset.id).ok().flatten()
    };
    let availability = match &volume {
        _ if changed => FileAvailability::Changed,
        Some(volume) if !mounted(volume) => FileAvailability::Offline,
        _ => FileAvailability::Missing,
    };
    let _ = observed(connection, &asset.id, availability, now_ms());
    let locator = &asset.locator;
    let detail = match (availability, &volume) {
        (FileAvailability::Offline, Some(volume)) => {
            format!("the volume {} is not connected", volume.label)
        }
        (FileAvailability::Changed, _) => format!(
            "the original's fingerprint changed: the file at {} is not the one developed",
            locator.display()
        ),
        _ => format!(
            "the original {} is missing from {}",
            name_of(locator),
            locator.parent().unwrap_or(Path::new("")).display()
        ),
    };
    let mut data = json!({"availability": availability, "locator": locator});
    if let Some(volume) = volume {
        data["volume"] = json!(volume);
    }
    Error::source_unavailable(detail).with_data(data)
}

/// A path's file name as a person reads it.
fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}
