//! Locate: point one photograph at a file a person chose for its original, once the file is proven
//! to be that original (`docs/specs/source-recovery.md`, "Locate missing original"). It is the one
//! relocation write: a Locate, a relink a check makes on its own ([`super::availability`]) and
//! their undo and redo all write the photograph's `asset-source` item through the journal.
//!
//! 1. **Before reading anything**, on the owner ([`plan`]): the path must be absolute and name a
//!    regular file of the original's length that no other photograph names — by path or by file
//!    identity — or the request is refused, the last naming that photograph.
//! 2. **Verify**, on the lane's worker ([`verify`]): the file's SHA-256 is streamed one bounded
//!    chunk at a time, cancellable between chunks, from one open handle whose signature is taken
//!    before and after, so a file modified while it is read is refused; the digest must be the
//!    fingerprint the catalog stores. A mismatch is reported however similar the file looks.
//! 3. **Commit**, on the owner ([`commit`]): the file must still have the signature it was verified
//!    with ([`unchanged`]) and still be unclaimed; then, in one transaction, its volume is recorded,
//!    the photograph's `asset-source` item becomes the file (one library change, undone with
//!    `library.undo` like any other) and its original is recorded available.
//!
//! A cancel, a mismatch, an unplugged volume or a failed commit leaves the photograph as it was.
//! History, the fingerprint and the interpretation are never touched, and nor is any file: the
//! original is only read.
use super::{
    availability,
    journal::{self, Desired, Outcome, Request},
};
use crate::{
    AssetId, EditorService, Error,
    atomic_file::file_error,
    catalog_types::{
        AssetSourceValue, AvailabilityRow, FileAvailability, LibraryChangeRow, LibraryItem, Volume,
        VolumeId,
    },
    editor::{SourceSignature, source_signature, source_signature_for_handle, upsert_volume},
    index::volume_of,
    jobs::JobControl,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{ErrorKind as IoErrorKind, Read},
    path::{Path, PathBuf},
};

/// The bytes one read of a verification takes: the one buffer a verification holds, whatever the
/// file's size, and how often it looks for a cancel.
pub(crate) const CHUNK: usize = 1024 * 1024;

/// Where a verification has got to, for a test that holds it there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// About to read the next chunk.
    Hashing,
    /// Every byte read, before the signature is taken again.
    Hashed,
    /// Verified, before the owner commits.
    Verified,
    /// A batch job is about to take its next photograph (`super::batch`).
    NextPhotograph,
}

/// A chosen file that passed every check made before reading it.
#[derive(Clone, Debug)]
pub(crate) struct Candidate {
    pub asset_id: AssetId,
    /// Its canonical path.
    pub path: PathBuf,
    /// Its signature when it was chosen.
    pub signature: SourceSignature,
    /// The fingerprint the catalog stores for the photograph's original.
    pub fingerprint: String,
}

/// A chosen file whose bytes are the photograph's original.
#[derive(Clone, Debug)]
pub(crate) struct Verified {
    pub asset_id: AssetId,
    pub path: PathBuf,
    /// Its signature while it was verified.
    pub signature: SourceSignature,
    /// The volume it is on, seen as it was verified.
    pub volume: Volume,
}

/// Check a chosen file before reading any of it: refuse a relative path, a folder or anything but
/// a regular file, a file of another length than the original, and a file another photograph
/// already names (`conflict`, naming it in `data.asset_id`). On the owner: a few stats and one
/// indexed read.
pub(crate) fn plan(
    connection: &Connection,
    asset_id: &AssetId,
    path: &Path,
) -> Result<Candidate, Error> {
    if !path.is_absolute() {
        return Err(Error::validation(format!(
            "the chosen file must be an absolute path, not {}",
            path.display()
        )));
    }
    let (byte_len, fingerprint): (i64, String) = connection
        .prepare_cached("SELECT byte_len, fingerprint FROM assets WHERE id = ?1")?
        .query_row([asset_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?
        .ok_or_else(|| Error::validation(format!("unknown asset {asset_id}")))?;
    let metadata = path
        .metadata()
        .map_err(|error| file_error(format!("cannot read {}", path.display()), error.kind()))?;
    if metadata.is_dir() {
        return Err(Error::validation(format!(
            "{} is a folder; choose the original's file",
            path.display()
        )));
    }
    let (canonical, signature) = EditorService::request_signature(path)
        .map_err(|_| Error::validation(format!("{} is not a regular file", path.display())))?;
    if signature.byte_len() != byte_len as u64 {
        return Err(not_the_original(
            &canonical,
            &format!(
                "it is {} bytes and the original {byte_len}",
                signature.byte_len()
            ),
        ));
    }
    if let Some(other) = claimed_by(connection, asset_id, &canonical, signature.file_identity())? {
        return Err(claimed(&canonical, &other));
    }
    Ok(Candidate {
        asset_id: asset_id.clone(),
        path: canonical,
        signature,
        fingerprint,
    })
}

/// Verify a candidate on the lane's worker ([`verify_file`]), reporting how much of it has been
/// read, and see which volume it is on.
pub(crate) fn verify(
    candidate: &Candidate,
    control: &JobControl,
    pause: &dyn Fn(Phase),
) -> Result<Verified, Error> {
    let signature = verify_file(
        &candidate.path,
        &candidate.signature,
        &candidate.fingerprint,
        control,
        pause,
        &|read, len| {
            control.set_progress(Some(read as f64 / len.max(1) as f64), "verifying");
        },
    )?;
    let volume = volume_of(&candidate.path, crate::editor::now_ms())?;
    pause(Phase::Verified);
    Ok(Verified {
        asset_id: candidate.asset_id.clone(),
        path: candidate.path.clone(),
        signature,
        volume,
    })
}

/// Stream the SHA-256 of the file at `path`, which must still have `expected`, from one open
/// handle in [`CHUNK`]s, and answer its signature when the digest is `fingerprint` and the file
/// kept that signature throughout ([`digest_file`]); one that is not the original is refused
/// (`source-unavailable`).
pub(crate) fn verify_file(
    path: &Path,
    expected: &SourceSignature,
    fingerprint: &str,
    control: &JobControl,
    pause: &dyn Fn(Phase),
    progress: &dyn Fn(u64, u64),
) -> Result<SourceSignature, Error> {
    let (digest, signature) = digest_file(path, expected, control, pause, progress)?;
    if digest != fingerprint {
        return Err(not_the_original(path, "its bytes differ"));
    }
    Ok(signature)
}

/// Stream the SHA-256 of the file at `path`, which must still have `expected`, from one open
/// handle in [`CHUNK`]s, and answer the digest (lowercase hex, as fingerprints are stored) with the
/// file's signature, when the file kept that signature throughout: its handle's and its path's are
/// taken before the first read and after the last, so a file replaced, rewritten or touched while
/// it is read is refused (`conflict`). Cancellable between chunks; `progress` hears the bytes read
/// so far and the length. Reads nothing but this file.
pub(crate) fn digest_file(
    path: &Path,
    expected: &SourceSignature,
    control: &JobControl,
    pause: &dyn Fn(Phase),
    progress: &dyn Fn(u64, u64),
) -> Result<(String, SourceSignature), Error> {
    let unreadable =
        |error: std::io::Error| file_error(format!("cannot read {}", path.display()), error.kind());
    let mut file = File::open(path).map_err(unreadable)?;
    let before = held_signature(path, &file)?;
    if before != *expected {
        return Err(changed(path, "before it was verified"));
    }
    let len = before.byte_len();
    let mut buffer = vec![0_u8; CHUNK.min(usize::try_from(len).unwrap_or(CHUNK)).max(1)];
    let mut hash = Sha256::new();
    let mut read = 0_u64;
    loop {
        pause(Phase::Hashing);
        control.checkpoint()?;
        let count = match file.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == IoErrorKind::Interrupted => continue,
            Err(error) => return Err(unreadable(error)),
        };
        if count == 0 {
            break;
        }
        read += count as u64;
        if read > len {
            return Err(changed(path, "while it was being verified"));
        }
        hash.update(&buffer[..count]);
        progress(read, len);
    }
    pause(Phase::Hashed);
    let after = held_signature(path, &file)?;
    if after != before || read != len {
        return Err(changed(path, "while it was being verified"));
    }
    Ok((format!("{:x}", hash.finalize()), before))
}

/// The signature of the file open at `path`, which must be the same as the file at `path` now.
fn held_signature(path: &Path, file: &File) -> Result<SourceSignature, Error> {
    let unreadable =
        |error: std::io::Error| file_error(format!("cannot read {}", path.display()), error.kind());
    let held = file.metadata().map_err(unreadable)?;
    let at_path = path.metadata().map_err(unreadable)?;
    let signature = source_signature_for_handle(path, file, &held);
    if signature != source_signature(path, &at_path) {
        return Err(changed(path, "while it was being verified"));
    }
    Ok(signature)
}

/// Whether the file at `path` still has the signature it was verified with: one stat, on the owner,
/// right before a commit.
pub(crate) fn unchanged(path: &Path, signature: &SourceSignature) -> bool {
    path.metadata()
        .is_ok_and(|metadata| source_signature(path, &metadata) == *signature)
}

/// The photograph other than `asset` that names the file at `path` or with `identity`, if any.
pub(crate) fn claimed_by(
    connection: &Connection,
    asset: &AssetId,
    path: &Path,
    identity: &str,
) -> Result<Option<AssetId>, Error> {
    connection
        .prepare_cached(
            "SELECT id FROM assets WHERE (canonical_locator = ?1 OR file_identity = ?2) AND id <> ?3
             LIMIT 1",
        )?
        .query_row(
            params![path.to_string_lossy(), identity, asset.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(AssetId::parse)
        .transpose()
}

/// Where the catalog looks for an original at the canonical `path`: its folder is the source folder.
pub(crate) fn source_value(path: &Path, volume_id: VolumeId, identity: &str) -> AssetSourceValue {
    AssetSourceValue {
        locator: path.to_path_buf(),
        source_folder: path.parent().unwrap_or(Path::new("")).to_path_buf(),
        volume_id,
        file_identity: identity.to_owned(),
    }
}

/// An `asset-source` value as the journal stores it.
pub(crate) fn encode_source(source: &AssetSourceValue) -> Result<Value, Error> {
    serde_json::to_value(source)
        .map_err(|error| Error::internal(format!("cannot encode a source: {error}")))
}

/// The file name a change row's new source names, for its label.
pub(crate) fn file_name_of(row: &LibraryChangeRow) -> String {
    row.after
        .as_ref()
        .and_then(|after| after.get("locator"))
        .and_then(Value::as_str)
        .map(Path::new)
        .and_then(Path::file_name)
        .map_or_else(
            || "the original".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
}

/// Commit a verified Locate in the owner's transaction: refuse a file another photograph has named
/// since it was chosen, record its volume, point the photograph's `asset-source` item at it as one
/// library change labelled "Located <file>", and record the original available. The caller has
/// checked the file is unchanged since it was verified ([`unchanged`]).
pub(crate) fn commit(
    tx: &Transaction<'_>,
    request: Request<'_>,
    verified: &Verified,
    checked_ms: i64,
) -> Result<Outcome, Error> {
    let identity = verified.signature.file_identity();
    if let Some(other) = claimed_by(tx, &verified.asset_id, &verified.path, identity)? {
        return Err(claimed(&verified.path, &other));
    }
    upsert_volume(tx, &verified.volume)?;
    let source = source_value(&verified.path, verified.volume.id.clone(), identity);
    let outcome = journal::apply(
        tx,
        request,
        vec![(
            LibraryItem::AssetSource {
                asset_id: verified.asset_id.clone(),
            },
            Desired::Value(Some(encode_source(&source)?)),
        )],
        |rows| {
            format!(
                "Located {}",
                rows.first().map_or_else(String::new, file_name_of)
            )
        },
    )?;
    availability::record(
        tx,
        &[AvailabilityRow {
            asset_id: verified.asset_id.clone(),
            availability: FileAvailability::Available,
            checked_ms,
        }],
    )?;
    Ok(outcome)
}

/// The refusal of a file that is not the photograph's original, saying why.
fn not_the_original(path: &Path, why: &str) -> Error {
    Error::source_unavailable(format!(
        "{} is not this photograph's original: {why}",
        path.display()
    ))
}

/// The refusal of a file another photograph names: Locate never merges two photographs.
fn claimed(path: &Path, other: &AssetId) -> Error {
    Error::conflict(format!(
        "{} is already the original of photograph {other}",
        path.display()
    ))
    .with_data(json!({"asset_id": other, "path": path}))
}

/// The refusal of a file that changed around its verification.
fn changed(path: &Path, when: &str) -> Error {
    Error::conflict(format!("{} changed {when}", path.display()))
}

/// Locate `path` for `asset_id` on the caller's thread, blocking — the owner's plan, the worker's
/// verification and the owner's commit, in turn — as the actor `test`. For tests that need the
/// relocation write without the owner.
#[cfg(test)]
pub(crate) fn locate_now(
    service: &mut EditorService,
    asset_id: &AssetId,
    path: &Path,
) -> Result<Outcome, Error> {
    let candidate = plan(&service.connection, asset_id, path)?;
    let verified = verify(&candidate, &JobControl::new(), &|_| {})?;
    if !unchanged(&verified.path, &verified.signature) {
        return Err(changed(&verified.path, "after it was verified"));
    }
    let request_id = crate::JobId::new().to_string();
    let request = Request {
        method: "source.locate",
        actor: "test",
        request_id: &request_id,
    };
    service.library_write(|tx| commit(tx, request, &verified, crate::editor::now_ms()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn signature(path: &Path) -> SourceSignature {
        EditorService::request_signature(path).unwrap().1
    }

    /// A file of several chunks is streamed chunk by chunk to the SHA-256 the catalog stores for
    /// it, reporting each; a digest that differs, a cancel between chunks and a file touched while
    /// it is read are each refused.
    #[test]
    fn locate_streams_a_bounded_fingerprint_and_refuses_what_changes() {
        let dir = luxforge_testbase::paths::temp_dir("locate-stream");
        let path = dir.join("large.bin");
        let bytes: Vec<u8> = (0..(CHUNK * 2 + CHUNK / 2))
            .map(|i| (i * 7 % 251) as u8)
            .collect();
        std::fs::write(&path, &bytes).unwrap();
        let path = path.canonicalize().unwrap();
        let fingerprint = format!("{:x}", Sha256::digest(&bytes));
        let reads = Mutex::new(Vec::new());
        let verified = verify_file(
            &path,
            &signature(&path),
            &fingerprint,
            &JobControl::new(),
            &|_| {},
            &|read, len| reads.lock().unwrap().push((read, len)),
        )
        .unwrap();
        assert_eq!(verified, signature(&path));
        let len = bytes.len() as u64;
        assert_eq!(
            *reads.lock().unwrap(),
            [(CHUNK as u64, len), (2 * CHUNK as u64, len), (len, len)]
        );

        let other = verify_file(
            &path,
            &signature(&path),
            &"0".repeat(64),
            &JobControl::new(),
            &|_| {},
            &|_, _| {},
        )
        .unwrap_err();
        assert_eq!(other.kind, crate::ErrorKind::SourceUnavailable, "{other:?}");

        // Cancelled after the first chunk.
        let control = JobControl::new();
        let hashing = Mutex::new(0);
        let cancelled = verify_file(
            &path,
            &signature(&path),
            &fingerprint,
            &control,
            &|phase| {
                if phase == Phase::Hashing {
                    let mut count = hashing.lock().unwrap();
                    *count += 1;
                    if *count == 2 {
                        control.cancel("stopped");
                    }
                }
            },
            &|_, _| {},
        )
        .unwrap_err();
        assert_eq!(cancelled.kind, crate::ErrorKind::Cancelled);
        assert_eq!(*hashing.lock().unwrap(), 2, "it stopped at the next chunk");

        // Touched once every byte is read: its signature is no longer the one it started with.
        let touched = verify_file(
            &path,
            &signature(&path),
            &fingerprint,
            &JobControl::new(),
            &|phase| {
                if phase == Phase::Hashed {
                    let file = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&path)
                        .unwrap();
                    file.set_len(len - 1).unwrap();
                    file.set_len(len).unwrap();
                }
            },
            &|_, _| {},
        )
        .unwrap_err();
        assert_eq!(touched.kind, crate::ErrorKind::Conflict, "{touched:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
