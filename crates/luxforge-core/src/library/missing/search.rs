//! A find's disk work, on the lane's worker: one bounded, cancellable walk of the chosen folder
//! with its subfolders, then each candidate's streamed fingerprint, one file at a time.
//!
//! - **The walk** ([`walk`]) lists directory entries and stats only the files whose name a sought
//!   photograph had (ignoring case where the platform's file systems do). It follows no symbolic
//!   link, so it never leaves its folder through one; it enters no folder on another volume; it
//!   skips hidden folders, packages and other applications' caches; and it refuses with
//!   `resource-limit` past [`MAX_SEARCH_FILES`] files or [`MAX_SEARCH_FOLDERS`] folders. Folders
//!   deeper than [`MAX_SEARCH_DEPTH`] are not entered. A folder that cannot be read fails the
//!   search (`read-error`): a photograph there would otherwise read as not found.
//! - **Verification**: a same-name file of the original's length is streamed through
//!   [`locate::digest_file`] in bounded chunks, cancellable between them, each file once whatever
//!   photographs want it; another length differs without being read. Before any file is read the
//!   owner says which photographs already name the candidates, so each photograph's row in the
//!   partial report `job.read` answers is its result as soon as its files are read.
//!
//! Memory is the stack of folders still to visit, the same-name files found and one digest each.
//! An unplugged volume fails the search with `source-unavailable`; nothing is ever written.
use super::{Looked, SameBytes, Sought, settle};
use crate::{
    AssetId, Error, ErrorKind,
    catalog_types::{FindReport, FindResult, FindRow, Volume},
    editor::{now_ms, source_signature},
    file_metadata::hidden,
    index::volume_of,
    jobs::JobControl,
    library::locate::{self, Phase},
};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs::Metadata,
    path::{Path, PathBuf},
};

/// The most files one search walks past; past it the search is refused with `resource-limit`.
pub(crate) const MAX_SEARCH_FILES: usize = 500_000;
/// The most folders one search visits; past it the search is refused with `resource-limit`.
pub(crate) const MAX_SEARCH_FOLDERS: usize = 100_000;
/// How far below the chosen folder a search goes: folders deeper are not entered.
pub(crate) const MAX_SEARCH_DEPTH: usize = 64;
/// How many directory entries the walk reads between checks for its cancellation.
const CHECK_EVERY: usize = 1024;

/// Folders the Finder shows as one document or application, by lowercase extension, and other
/// applications' caches (Lightroom Classic's `.lrdata`): a search never enters them.
const SKIPPED_EXTENSIONS: &[&str] = &[
    "app",
    "bundle",
    "framework",
    "plugin",
    "photoslibrary",
    "photolibrary",
    "migratedphotolibrary",
    "aplibrary",
    "lrlibrary",
    "cocatalog",
    "fcpbundle",
    "imovielibrary",
    "sparsebundle",
    "pkg",
    "lrdata",
];

/// Folders a search never enters, by name ignoring case: system folders a hidden flag does not
/// mark, and other applications' caches.
const SKIPPED_NAMES: &[&str] = &[
    "$recycle.bin",
    "system volume information",
    "lost+found",
    "captureone",
    "@eadir",
    "@__thumb",
];

/// A search's bounds, the named limits above unless a test sets smaller ones.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SearchLimits {
    pub files: usize,
    pub folders: usize,
    pub depth: usize,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            files: MAX_SEARCH_FILES,
            folders: MAX_SEARCH_FOLDERS,
            depth: MAX_SEARCH_DEPTH,
        }
    }
}

/// The owner's answer to which photographs already name each of a list of files, by canonical path
/// and file identity ([`super::claimants`]).
pub(crate) type Claims<'a> =
    dyn Fn(Vec<(PathBuf, String)>) -> Result<Vec<Vec<AssetId>>, Error> + 'a;

/// What a search needs from the job that runs it: its control (its cancel flag, its progress and
/// its partial report), where a test may hold it, and who names the files it is about to read.
pub(crate) struct SearchJob<'a> {
    pub control: &'a JobControl,
    pub pause: &'a dyn Fn(Phase),
    pub claims: &'a Claims<'a>,
}

/// What a search found: the volume it searched, and for each sought photograph, in order, what it
/// found.
#[derive(Debug)]
pub(crate) struct Searched {
    pub volume: Volume,
    pub sought: Vec<Sought>,
    pub looked: Vec<Looked>,
}

/// A file the walk listed under a sought name.
#[derive(Clone, Debug)]
pub(crate) struct Listed {
    pub path: PathBuf,
    pub byte_len: u64,
    pub identity: String,
}

/// What reading one candidate came to.
#[derive(Clone, Debug)]
enum Read {
    Digest(String, crate::editor::SourceSignature),
    /// It changed while it was read.
    Unverifiable,
    /// It is no longer there.
    Gone,
}

/// A file name as a search compares it: ignoring case where the platform's file systems do (macOS
/// and Windows), exactly elsewhere.
pub(crate) fn name_key(name: &str) -> String {
    if cfg!(any(target_os = "macos", windows)) {
        name.to_lowercase()
    } else {
        name.to_owned()
    }
}

/// Search `root` (canonical) and its subfolders for each of `sought`, publishing each photograph's
/// row in the job's partial report as it is settled (`checking` until then) and progress on the
/// board: the files looked at while walking, then the photographs checked with the fraction of
/// candidate bytes read. Stops at the next folder, entry batch or chunk once cancelled.
pub(crate) fn search(
    root: PathBuf,
    sought: Vec<Sought>,
    limits: SearchLimits,
    job: &SearchJob<'_>,
) -> Result<Searched, Error> {
    let control = job.control;
    let rows: Vec<FindRow> = sought
        .iter()
        .map(|sought| FindRow {
            asset_id: sought.asset_id.clone(),
            file_name: sought.file_name.clone(),
            result: FindResult::Checking,
        })
        .collect();
    control.update_partial(|partial| *partial = serde_json::json!(FindReport { rows }));
    let root_metadata = root.symlink_metadata().map_err(|_| gone(&root))?;
    let device = device_of(&root_metadata);
    let volume = volume_of(&root, now_ms()).map_err(|_| gone(&root))?;

    control.set_phase("searching");
    let names: HashSet<String> = sought.iter().map(|s| name_key(&s.file_name)).collect();
    let listed = walk(&root, device, &names, limits, control)?;

    // The files worth reading: a same-name file of a sought photograph's length. Which
    // photographs already name them is asked once, before any is read.
    control.set_phase("verifying");
    let mut candidates: Vec<&Listed> = Vec::new();
    let mut seen = HashSet::new();
    for sought in &sought {
        for file in listed
            .get(&name_key(&sought.file_name))
            .into_iter()
            .flatten()
        {
            if file.byte_len == sought.byte_len && seen.insert(&file.path) {
                candidates.push(file);
            }
        }
    }
    let total_bytes: u64 = candidates.iter().map(|file| file.byte_len).sum();
    let claims: HashMap<PathBuf, Vec<AssetId>> = if candidates.is_empty() {
        HashMap::new()
    } else {
        let asked = candidates
            .iter()
            .map(|file| (file.path.clone(), file.identity.clone()))
            .collect();
        candidates
            .iter()
            .map(|file| file.path.clone())
            .zip((job.claims)(asked)?)
            .collect()
    };

    let count = sought.len();
    let mut read_bytes = 0_u64;
    let mut reads: HashMap<PathBuf, Read> = HashMap::new();
    let mut looked_all = Vec::with_capacity(count);
    let report = |done: usize, bytes: u64| {
        let fraction = if total_bytes > 0 {
            bytes as f64 / total_bytes as f64
        } else {
            done as f64 / count.max(1) as f64
        };
        control.set_progress(
            Some(fraction),
            &format!("{done} of {count} photographs checked"),
        );
    };
    report(0, 0);
    for (index, sought) in sought.iter().enumerate() {
        control.checkpoint()?;
        let mut looked = Looked::default();
        for file in listed
            .get(&name_key(&sought.file_name))
            .into_iter()
            .flatten()
        {
            if file.byte_len != sought.byte_len {
                looked.different.get_or_insert_with(|| file.path.clone());
                continue;
            }
            let outcome = match reads.get(&file.path) {
                Some(outcome) => outcome.clone(),
                None => {
                    let done = read_bytes;
                    let outcome = read_candidate(&root, device, file, job, &|bytes, _| {
                        report(index, done + bytes);
                    })?;
                    read_bytes += file.byte_len;
                    reads.insert(file.path.clone(), outcome.clone());
                    outcome
                }
            };
            match outcome {
                Read::Digest(digest, signature) if digest == sought.fingerprint => {
                    looked.same.push(SameBytes {
                        path: file.path.clone(),
                        signature,
                    });
                }
                Read::Digest(..) | Read::Unverifiable => {
                    looked.different.get_or_insert_with(|| file.path.clone());
                }
                Read::Gone => {}
            }
        }
        let mine: Vec<Vec<AssetId>> = looked
            .same
            .iter()
            .map(|file| claims.get(&file.path).cloned().unwrap_or_default())
            .collect();
        let (result, _) = settle(sought, &looked, &mine);
        let row = FindRow {
            asset_id: sought.asset_id.clone(),
            file_name: sought.file_name.clone(),
            result,
        };
        control.update_partial(|partial| partial["rows"][index] = serde_json::json!(row));
        looked_all.push(looked);
        report(index + 1, read_bytes);
    }
    Ok(Searched {
        volume,
        sought,
        looked: looked_all,
    })
}

/// Read one candidate's digest, taking its signature just before: a file that is no longer there
/// is gone, one that changes while it is read cannot be verified, and one of another length now
/// differs. A cancel stops the search; so does an unplugged volume (`source-unavailable`) and a
/// file that cannot be read (`read-error`, naming it).
fn read_candidate(
    root: &Path,
    device: Option<u64>,
    file: &Listed,
    job: &SearchJob<'_>,
    progress: &dyn Fn(u64, u64),
) -> Result<Read, Error> {
    let metadata = match file.path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() => metadata,
        _ if !present(root, device) => return Err(gone(root)),
        _ => return Ok(Read::Gone),
    };
    if metadata.len() != file.byte_len {
        return Ok(Read::Unverifiable);
    }
    let signature = source_signature(&file.path, &metadata);
    match locate::digest_file(&file.path, &signature, job.control, job.pause, progress) {
        Ok((digest, signature)) => Ok(Read::Digest(digest, signature)),
        Err(error) if error.kind == ErrorKind::Cancelled => Err(error),
        Err(_) if !present(root, device) => Err(gone(root)),
        Err(error) if error.kind == ErrorKind::Conflict => Ok(Read::Unverifiable),
        Err(_) if file.path.symlink_metadata().is_err() => Ok(Read::Gone),
        Err(error) => Err(error),
    }
}

/// Walk `root` and its subfolders, depth first in name order, answering the files whose name key
/// is in `names`, by key, each list in path order.
pub(crate) fn walk(
    root: &Path,
    device: Option<u64>,
    names: &HashSet<String>,
    limits: SearchLimits,
    control: &JobControl,
) -> Result<HashMap<String, Vec<Listed>>, Error> {
    let mut stack = vec![(root.to_path_buf(), 0_usize)];
    let (mut files, mut folders) = (0_usize, 0_usize);
    let mut listed: HashMap<String, Vec<Listed>> = HashMap::new();
    while let Some((folder, depth)) = stack.pop() {
        control.checkpoint()?;
        folders += 1;
        if folders > limits.folders {
            return Err(Error::resource_limit(format!(
                "{} holds more than {} folders; choose a smaller folder to search",
                root.display(),
                limits.folders
            )));
        }
        let entries = std::fs::read_dir(&folder)
            .map_err(|error| unreadable(root, device, &folder, &error))?;
        let mut subfolders = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index % CHECK_EVERY == CHECK_EVERY - 1 {
                control.checkpoint()?;
            }
            let entry = entry.map_err(|error| unreadable(root, device, &folder, &error))?;
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            if kind.is_dir() {
                if depth + 1 > limits.depth || skipped(&name) {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if hidden(&name, &metadata) || device_of(&metadata) != device {
                    continue;
                }
                subfolders.push(entry.path());
            } else if kind.is_file() {
                files += 1;
                if files > limits.files {
                    return Err(Error::resource_limit(format!(
                        "{} holds more than {} files; choose a smaller folder to search",
                        root.display(),
                        limits.files
                    )));
                }
                let key = name_key(&name.to_string_lossy());
                if !names.contains(&key) {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if hidden(&name, &metadata) {
                    continue;
                }
                let path = entry.path();
                let identity = source_signature(&path, &metadata)
                    .file_identity()
                    .to_owned();
                listed.entry(key).or_default().push(Listed {
                    path,
                    byte_len: metadata.len(),
                    identity,
                });
            }
            // A symbolic link, or anything that is neither a file nor a folder, is never listed
            // or followed.
        }
        control.set_progress(None, &format!("{files} files looked at"));
        subfolders.sort();
        stack.extend(subfolders.into_iter().rev().map(|path| (path, depth + 1)));
    }
    for files in listed.values_mut() {
        files.sort_by(|a, b| a.path.cmp(&b.path));
    }
    Ok(listed)
}

/// Whether the walk never enters the folder named `name`.
fn skipped(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    SKIPPED_NAMES
        .iter()
        .any(|skipped| name.eq_ignore_ascii_case(skipped))
        || Path::new(name)
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| {
                SKIPPED_EXTENSIONS
                    .iter()
                    .any(|skipped| extension.eq_ignore_ascii_case(skipped))
            })
}

/// The device a folder or file is on, where the platform says; a walk enters no other.
#[cfg(unix)]
pub(crate) fn device_of(metadata: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev())
}

#[cfg(not(unix))]
pub(crate) fn device_of(_: &Metadata) -> Option<u64> {
    None
}

/// Whether the searched folder is still there, on the same device.
fn present(root: &Path, device: Option<u64>) -> bool {
    root.symlink_metadata()
        .is_ok_and(|metadata| device_of(&metadata) == device)
}

/// The failure of a search whose folder went away while it ran: its drive was disconnected, or
/// the folder was moved.
fn gone(root: &Path) -> Error {
    Error::source_unavailable(format!(
        "{} is no longer there: its drive was disconnected or the folder moved while it was \
         searched",
        root.display()
    ))
}

/// The failure of a folder the walk cannot read: its drive went away, or it cannot be read.
fn unreadable(root: &Path, device: Option<u64>, folder: &Path, error: &std::io::Error) -> Error {
    if present(root, device) {
        crate::atomic_file::file_error(format!("cannot read {}", folder.display()), error.kind())
    } else {
        gone(root)
    }
}
