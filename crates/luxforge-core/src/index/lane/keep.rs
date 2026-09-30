//! Keeping the index current from the platform's change notifications
//! (`docs/design/catalog.md`, "Keeping up with the disk"). The coordinator owns one
//! [`luxforge_watch::Watcher`], watches every indexed folder with it (never a card or a folder only
//! browsed), and applies what it reports on its own thread, in the order it arrives:
//!
//! - **Changed paths are hints.** Each is looked at: a file is reconciled by signature, as a listing
//!   reconciles it (read again when its signature changed, carried by its file identity when it
//!   moved within its volume, read when new); a folder the index knows nothing under is listed with
//!   everything under it; a path that is gone, a link, on another volume or one the index skips
//!   (hidden, a package, another application's cache, a system folder, Luxforge's own) has its row
//!   and every row under it dropped. A rename can arrive as two reports, its old path in one and
//!   its new path in the next, so a path gone waits [`GONE_AFTER`] for the path it may have moved
//!   to, which carries its row by file identity, before it is looked at again and its rows go.
//! - **Rescans are listings, and jobs.** A subtree whose changes were not all reported is listed
//!   again and reconciled by signature; the whole root when the root itself is named. A root that
//!   changed (moved, removed, its volume gone) is dropped from the watcher, listed again if it is
//!   there and watched anew, or else recorded offline or missing, as a listing records it. Each
//!   such listing is an `index-refresh` job no request started ([`Run::own_job`]), which a client
//!   reads and cancels as any listing.
//! - **Stale roots.** A root whose listing was cancelled or failed, or that a unit which failed
//!   touched, is stale, on its row too: it records no cursor, and it is listed in full before its
//!   next change is applied, as the catalog next opens (watched without a cursor, so the watcher
//!   asks for the listing) and when its volume comes back.
//! - **Cursors.** On macOS each event may carry where the root's notifications resume. The newest is
//!   recorded on the root's row in the transaction that writes the last change of the unit it came
//!   in, after every header read of that unit, so a restart replays from there; a unit that fails
//!   records none, and its roots are listed again before any later cursor of theirs is recorded.
//! - **Volumes.** A volume mounted brings its folders back: their roots are online again and
//!   watched from their cursors; one taken out takes its roots offline and out of the watcher. The
//!   owner hears both, to survey the volumes again and to list a card.
//! - **Not watched.** A folder on a network volume is not watched, since its changes are not
//!   reported: it is listed again when a client refreshes it. A folder the watcher cannot follow, or
//!   no longer follows all of, is reported to the owner with the reason.
use super::{Input, LaneEvent, Listed, Post, RootPlan, Run, Write};
use crate::{
    Error, ErrorKind,
    catalog_types::{FileSignature, RootKind, VolumeId},
    index::{
        database,
        exclude::{hidden, kind_of},
        reconcile::Reconciler,
        volumes::{PlatformMount, volume_in},
        walk::{ListedFile, ListedFolder, WalkLimits},
    },
};
use luxforge_watch::{RescanReason, Resume, VolumeEvent, WatchEvent, WatchRoot, Watcher};
use rusqlite::Connection;
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::mpsc::SyncSender,
    time::{Duration, Instant},
};

/// Why a folder on a network volume is not watched.
const ON_NETWORK: &str =
    "it is on a network volume, whose changes are not reported: refresh it to list it again";
/// Why a folder whose volume was taken out is not watched.
const OFFLINE: &str = "it is offline: its volume is not connected";
/// How long a path reported gone waits, before its rows go, for the path it may have moved to:
/// longer than the platform takes between the reports of a rename's two halves.
pub(crate) const GONE_AFTER: Duration = Duration::from_millis(500);
/// Paths reported gone that may wait at once; past it the oldest go at once.
pub(crate) const GONE_WAITING: usize = luxforge_watch::MAX_PATHS;

/// The watcher and the roots it follows.
#[derive(Default)]
pub(super) struct Keeper {
    watcher: Option<Watcher>,
    /// Why there is no watcher, when there is none.
    unavailable: Option<String>,
    roots: Vec<Kept>,
    next_id: u64,
    /// The files the running unit has applied, with the signature it applied: a path reported
    /// again in the same unit with the same signature is not read twice, since its row is not
    /// committed yet for the reconciler to find.
    applied: HashMap<PathBuf, FileSignature>,
    /// Paths reported gone, each with its root's id and when its rows go ([`GONE_AFTER`]).
    gone: VecDeque<(u64, PathBuf, Instant)>,
}

/// One indexed folder the lane keeps current.
struct Kept {
    /// Its name in the watcher's events.
    id: u64,
    /// Its canonical path, as its root row names it.
    path: PathBuf,
    volume_id: VolumeId,
    /// Whether the watcher follows it now.
    active: bool,
    /// The newest cursor the running unit applied everything before, recorded as it commits.
    cursor: Option<Resume>,
    /// Whether the running unit applied an event of it.
    touched: bool,
    /// A unit that touched it failed: it is listed again before its next cursor is recorded.
    stale: bool,
}

impl Keeper {
    /// Start the watcher, its events arriving on `inputs`: `Err` with why, when the platform
    /// offers none.
    pub(super) fn start(&mut self, inputs: SyncSender<Input>) -> Result<(), String> {
        match Watcher::start(inputs) {
            Ok(watcher) => {
                self.watcher = Some(watcher);
                Ok(())
            }
            Err(error) => {
                let reason = format!("changes are not followed on this system: {error}");
                self.unavailable = Some(reason.clone());
                Err(reason)
            }
        }
    }

    /// Stop the watcher: every stream stops and its sender drops.
    pub(super) fn stop(&mut self) {
        self.watcher = None;
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.roots.iter().position(|root| root.id == id)
    }

    fn index_at(&self, path: &Path) -> Option<usize> {
        self.roots.iter().position(|root| root.path == path)
    }

    /// Keep the indexed folder `path`, on `volume_id`, if it is not kept yet: its index.
    fn keep(&mut self, path: &Path, volume_id: &VolumeId) -> usize {
        if let Some(index) = self.index_at(path) {
            return index;
        }
        self.next_id += 1;
        self.roots.push(Kept {
            id: self.next_id,
            path: path.to_path_buf(),
            volume_id: volume_id.clone(),
            active: false,
            cursor: None,
            touched: false,
            stale: false,
        });
        self.roots.len() - 1
    }

    /// Have the watcher follow the root at `index`, resuming from `resume`, and tell the owner
    /// whether it does. A root on a network volume is not watched.
    fn add(&mut self, index: usize, resume: Option<Resume>, mounts: &[PlatformMount], post: &Post) {
        let root = &mut self.roots[index];
        if root.active {
            return;
        }
        let watching = if on_network(mounts, &root.path) {
            Err(ON_NETWORK.to_owned())
        } else if let Some(watcher) = &mut self.watcher {
            watcher
                .add_root(WatchRoot {
                    id: root.id,
                    path: root.path.clone(),
                    resume,
                })
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => {
                        "it is not there: its volume is not connected, or it moved".to_owned()
                    }
                    _ => format!("its changes cannot be followed: {error}"),
                })
        } else {
            Err(self
                .unavailable
                .clone()
                .unwrap_or_else(|| "changes are not followed on this system".into()))
        };
        root.active = watching.is_ok();
        post(LaneEvent::Watching {
            path: root.path.clone(),
            watching,
        });
    }

    /// Stop following the root at `index`.
    fn remove(&mut self, index: usize) {
        let root = &mut self.roots[index];
        if root.active
            && let Some(watcher) = &mut self.watcher
        {
            watcher.remove_root(root.id);
        }
        root.active = false;
        root.cursor = None;
    }

    /// Watch the indexed folders `roots` as the catalog opens, each from the cursor its root row
    /// keeps: what changed while Luxforge was closed is replayed (macOS), and a root without one, or
    /// on a platform that keeps no history, is listed again when the watcher says so. A root left
    /// stale is watched without its cursor, so it is listed again now.
    pub(super) fn watch(
        &mut self,
        connection: &Connection,
        roots: &[RootPlan],
        mounts: &[PlatformMount],
        post: &Post,
    ) {
        for plan in roots {
            let Some(volume_id) = &plan.volume_id else {
                continue;
            };
            let index = self.keep(&plan.path, volume_id);
            self.roots[index].stale |=
                database::root_stale(connection, &plan.path).unwrap_or(false);
            let resume = self.resume(connection, index);
            self.add(index, resume, mounts, post);
        }
    }

    /// Where the root at `index` resumes when the watcher follows it again: the cursor its row
    /// keeps, or none while it is stale, so the watcher asks for the listing that makes it current.
    fn resume(&self, connection: &Connection, index: usize) -> Option<Resume> {
        let root = &self.roots[index];
        if root.stale {
            return None;
        }
        database::root_cursor(connection, &root.path).ok().flatten()
    }

    /// A job listed `listed` in full: each is current again, and an indexed folder not watched yet
    /// is watched from the cursor read before its walk, recorded on its row first, so what changed
    /// during the walk replays.
    pub(super) fn listed(
        &mut self,
        connection: &mut Connection,
        listed: &[Listed],
        mounts: &[PlatformMount],
        post: &Post,
    ) {
        for root in listed.iter().filter(|root| root.kind == RootKind::Indexed) {
            let index = self.keep(&root.path, &root.volume_id);
            self.roots[index].stale = false;
            if self.roots[index].active {
                continue;
            }
            if let Some(cursor) = root.cursor {
                let recorded = (|| -> Result<(), Error> {
                    let tx = connection.transaction()?;
                    database::set_root_cursor(&tx, &root.path, Some(cursor))?;
                    Ok(tx.commit()?)
                })();
                if recorded.is_err() {
                    // Without its cursor recorded it is listed again when the watcher says so.
                    self.add(index, None, mounts, post);
                    continue;
                }
            }
            self.add(index, root.cursor, mounts, post);
        }
    }

    /// Stop watching the indexed folder at `path`, which is no longer one.
    pub(super) fn forget(&mut self, path: &Path) {
        if let Some(index) = self.index_at(path) {
            self.remove(index);
            let id = self.roots.remove(index).id;
            self.gone.retain(|(root, ..)| *root != id);
        }
    }

    /// When the next path reported gone is due to be looked at again, if one waits: how long the
    /// coordinator may sleep on its channel.
    pub(super) fn next_gone(&self) -> Option<Instant> {
        self.gone.front().map(|(.., due)| *due)
    }

    /// Look again at each path reported gone whose wait is over, after
    /// writing what the unit wrote so far, so a row a move carried is not dropped: a path still
    /// gone, or one the listing now skips, has its row and every row under it dropped.
    pub(super) fn drop_gone(
        &mut self,
        run: &mut Run<'_>,
        mounts: &[PlatformMount],
    ) -> Result<(), Error> {
        let now = Instant::now();
        let mut due = Vec::new();
        while let Some((_, _, when)) = self.gone.front() {
            if *when > now {
                break;
            }
            let (root, path, _) = self.gone.pop_front().expect("the front was looked at");
            due.push((root, path));
        }
        if due.is_empty() {
            return Ok(());
        }
        run.batch.commit(run.connection, &run.config.post)?;
        for (id, path) in due {
            let Some(index) = self.index_of(id) else {
                continue;
            };
            let root = self.roots[index].path.clone();
            let still_gone = match volume_in(mounts, &root, run.stamp) {
                Ok(volume) => match root.symlink_metadata() {
                    Ok(metadata) => {
                        let mut look = Look::new(&root, &metadata, &volume.mount_point, run);
                        matches!(look.at(&path), Seen::Gone)
                    }
                    Err(_) => false,
                },
                // The root is not there: its root change lists it.
                Err(_) => false,
            };
            if still_gone {
                drop_under(run, &[path])?;
            }
        }
        Ok(())
    }

    /// Record each root's newest cursor in the run's batch, its last writes. A stale root records
    /// none until it has been listed again.
    pub(super) fn record_cursors(&mut self, run: &mut Run<'_>) {
        let gone = &self.gone;
        for root in &mut self.roots {
            // A root whose gone paths still wait keeps its cursor until they have been applied.
            if gone.iter().any(|(id, ..)| *id == root.id) {
                continue;
            }
            if let Some(cursor) = root.cursor.take()
                && !root.stale
            {
                run.batch.push(Write::Cursor {
                    root: root.path.clone(),
                    cursor: Some(cursor),
                });
            }
        }
    }

    /// The unit committed, its cursors with it.
    pub(super) fn recorded(&mut self) {
        self.applied.clear();
        for root in &mut self.roots {
            root.touched = false;
        }
    }

    /// The unit failed: its cursors are not recorded, and every root it touched is stale, listed
    /// again before a later cursor of its own is. Answers the roots it made stale, for their rows.
    pub(super) fn unrecorded(&mut self) -> Vec<PathBuf> {
        self.applied.clear();
        let mut stale = Vec::new();
        for root in &mut self.roots {
            root.cursor = None;
            if std::mem::take(&mut root.touched) && !root.stale {
                root.stale = true;
                stale.push(root.path.clone());
            }
        }
        stale
    }

    /// Apply one event in the run of a unit.
    pub(super) fn apply(
        &mut self,
        run: &mut Run<'_>,
        event: WatchEvent,
        mounts: &[PlatformMount],
        last_stamp: &mut i64,
    ) -> Result<(), Error> {
        if let Some(index) = event.root().and_then(|root| self.index_of(root)) {
            self.roots[index].touched = true;
            // A stale root is listed in full before its next change is applied, so what it missed
            // is caught up; a rescan of the whole root, or of a root that changed, is that listing.
            let changes = match &event {
                WatchEvent::Changed { .. } => true,
                WatchEvent::Rescan {
                    subtree, reason, ..
                } => *reason != RescanReason::RootChanged && *subtree != self.roots[index].path,
                _ => false,
            };
            if self.roots[index].stale && changes {
                let path = self.roots[index].path.clone();
                self.rescan(run, index, &path, mounts, last_stamp)?;
            }
        }
        match event {
            WatchEvent::Changed {
                root,
                paths,
                cursor,
            } => {
                let Some(index) = self.index_of(root) else {
                    return Ok(());
                };
                self.changed(run, index, &paths, mounts, last_stamp)?;
                if cursor.is_some() {
                    self.roots[index].cursor = cursor;
                }
            }
            WatchEvent::Rescan {
                root,
                subtree,
                reason,
            } => {
                let Some(index) = self.index_of(root) else {
                    return Ok(());
                };
                match reason {
                    RescanReason::RootChanged => {
                        self.root_changed(run, index, mounts, last_stamp)?
                    }
                    RescanReason::Wrapped => {
                        // Every cursor of the history is void: this root's is forgotten until
                        // the next one arrives.
                        self.roots[index].cursor = None;
                        run.batch.push(Write::Cursor {
                            root: self.roots[index].path.clone(),
                            cursor: None,
                        });
                        self.rescan(run, index, &subtree, mounts, last_stamp)?;
                    }
                    RescanReason::Overflow | RescanReason::Dropped | RescanReason::NoReplay => {
                        self.rescan(run, index, &subtree, mounts, last_stamp)?;
                    }
                }
            }
            WatchEvent::CaughtUp { root, cursor } => {
                if let Some(index) = self.index_of(root)
                    && cursor.is_some()
                {
                    self.roots[index].cursor = cursor;
                }
            }
            WatchEvent::Unwatched {
                root,
                subtree,
                error,
            } => {
                if let Some(index) = self.index_of(root) {
                    let path = self.roots[index].path.clone();
                    let reason = if subtree == path {
                        format!("its changes are no longer followed: {error}")
                    } else {
                        format!(
                            "changes under {} are no longer followed: {error}",
                            subtree.display()
                        )
                    };
                    (run.config.post)(LaneEvent::Watching {
                        path,
                        watching: Err(reason),
                    });
                }
            }
            WatchEvent::Volume(event) => self.volume(run, event, mounts)?,
        }
        Ok(())
    }

    /// Look at each changed path under the root at `index`, and write what it means.
    fn changed(
        &mut self,
        run: &mut Run<'_>,
        index: usize,
        paths: &[PathBuf],
        mounts: &[PlatformMount],
        last_stamp: &mut i64,
    ) -> Result<(), Error> {
        let root = self.roots[index].path.clone();
        let id = self.roots[index].id;
        // A root that is not there is reported as a root change, which lists it.
        let Ok(volume) = volume_in(mounts, &root, run.stamp) else {
            return Ok(());
        };
        let Ok(root_metadata) = root.symlink_metadata() else {
            return Ok(());
        };
        let mut look = Look::new(&root, &root_metadata, &volume.mount_point, run);
        let mut files: BTreeMap<PathBuf, Vec<ListedFile>> = BTreeMap::new();
        let mut folders = Vec::new();
        let mut gone = Vec::new();
        for path in paths {
            if !path.starts_with(&root) || path == &root {
                // The root's own change needs nothing: what changed in it is reported apart.
                continue;
            }
            match look.at(path) {
                Seen::File(folder, file) => {
                    if self.applied.insert(path.clone(), file.signature) == Some(file.signature) {
                        continue;
                    }
                    files.entry(folder).or_default().push(file);
                }
                Seen::Folder => folders.push(path.clone()),
                Seen::Gone => gone.push(path.clone()),
            }
        }
        // Files, by folder, reconciled by signature: a moved file's row is carried by its identity.
        let mut reconciler = Reconciler::new(volume.id.clone());
        for (path, mut listed) in files {
            listed.sort_by(|a, b| a.name.cmp(&b.name));
            let folder = ListedFolder {
                path,
                files: listed,
            };
            let decisions = reconciler.folder(run.connection, &folder)?;
            run.apply_folder(&folder, &decisions, &volume.id)?;
            run.drain_ready()?;
        }
        // Folders the index knows nothing under, with everything under them; one inside another
        // is listed with it.
        folders.sort();
        let mut listed: Vec<PathBuf> = Vec::new();
        for folder in folders {
            if listed.iter().any(|outer| folder.starts_with(outer))
                || !database::files_under(run.connection, &folder)?.is_empty()
            {
                continue;
            }
            run.batch.commit(run.connection, &run.config.post)?;
            run.stamp = next_stamp(last_stamp);
            run.list(&folder, &volume, None)?;
            listed.push(folder);
        }
        // What is gone waits for the path it may have moved to.
        let due = Instant::now() + GONE_AFTER;
        for path in gone {
            if !self
                .gone
                .iter()
                .any(|(root, waiting, _)| *root == id && *waiting == path)
            {
                self.gone.push_back((id, path, due));
            }
        }
        if self.gone.len() > GONE_WAITING {
            let over = self.gone.len() - GONE_WAITING;
            let oldest: Vec<_> = self.gone.drain(..over).collect();
            run.batch.commit(run.connection, &run.config.post)?;
            for (_, path, _) in oldest {
                drop_under(run, &[path])?;
            }
        }
        Ok(())
    }

    /// List `subtree` of the root at `index` again, reconciling by signature — the whole root when
    /// it names the root — as a job of the lane's own ([`Run::own_job`]).
    fn rescan(
        &mut self,
        run: &mut Run<'_>,
        index: usize,
        subtree: &Path,
        mounts: &[PlatformMount],
        last_stamp: &mut i64,
    ) -> Result<(), Error> {
        let root = self.roots[index].path.clone();
        let whole = subtree == root;
        let plan = RootPlan {
            path: root.clone(),
            kind: RootKind::Indexed,
            volume_id: Some(self.roots[index].volume_id.clone()),
        };
        // The job commits what the unit wrote so far before the listing reconciles against it.
        run.stamp = next_stamp(last_stamp);
        let listed = run.own_job(&root, subtree, |run| {
            if whole {
                let listed = run.root(&plan, mounts, false);
                run.listed.clear();
                listed
            } else if subtree.starts_with(&root) {
                match volume_in(mounts, &root, run.stamp) {
                    Ok(volume) if subtree.symlink_metadata().is_ok_and(|m| m.is_dir()) => {
                        let walked = run.list(subtree, &volume, None)?;
                        run.report.roots.push(subtree.to_path_buf());
                        run.report.files += u32::try_from(walked.files).unwrap_or(u32::MAX);
                        run.report.unreadable_folders +=
                            u32::try_from(walked.unreadable_folders).unwrap_or(u32::MAX);
                        Ok(())
                    }
                    Ok(_) => drop_under(run, &[subtree.to_path_buf()]),
                    // The root is not there: its root change lists it.
                    Err(_) => Ok(()),
                }
            } else {
                Ok(())
            }
        });
        self.listed_own(run, index, whole, listed)
    }

    /// How a listing of the lane's own of the root at `index` ended, `whole` when it listed all of
    /// it: the root is current once the whole of it was listed, and stale when the listing did not
    /// complete. A cancelled listing ends there and the unit goes on with its other changes, unless
    /// the lane is stopping; one that failed fails the unit.
    fn listed_own(
        &mut self,
        run: &Run<'_>,
        index: usize,
        whole: bool,
        listed: Result<(), Error>,
    ) -> Result<(), Error> {
        match listed {
            Ok(()) => {
                if whole {
                    self.roots[index].stale = false;
                }
                Ok(())
            }
            Err(error) => {
                self.roots[index].stale = true;
                if error.kind == ErrorKind::Cancelled && !run.stop.is_cancelled() {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }

    /// The root at `index` moved, was removed or its volume went away: stop following it, list it
    /// again as a job of the lane's own (which records it offline or missing when it is not
    /// there), and follow it anew from the cursor read before that listing when it is there. One
    /// whose listing was cancelled is followed from now: it is stale, so its next change lists it
    /// (where the platform keeps no history, following it asks for that listing at once).
    fn root_changed(
        &mut self,
        run: &mut Run<'_>,
        index: usize,
        mounts: &[PlatformMount],
        last_stamp: &mut i64,
    ) -> Result<(), Error> {
        self.remove(index);
        let path = self.roots[index].path.clone();
        let there = path.canonicalize().is_ok_and(|canonical| canonical == path)
            && path
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.is_dir());
        run.stamp = next_stamp(last_stamp);
        let plan = RootPlan {
            path: path.clone(),
            kind: RootKind::Indexed,
            volume_id: Some(self.roots[index].volume_id.clone()),
        };
        let listed = run.own_job(&path, &path, |run| run.root(&plan, mounts, false));
        let completed = listed.is_ok();
        self.listed_own(run, index, true, listed)?;
        let cursor = match run.listed.drain(..).find_map(|listed| listed.cursor) {
            None if !completed => luxforge_watch::current_cursor(&path),
            cursor => cursor,
        };
        if there {
            self.roots[index].cursor = cursor;
            self.add(index, cursor, mounts, &run.config.post);
        } else {
            (run.config.post)(LaneEvent::Watching {
                path,
                watching: Err("it is not there: its volume is not connected, or it moved".into()),
            });
        }
        Ok(())
    }

    /// A volume was mounted or taken out: its roots go offline or come back, the owner hears of
    /// it, and the watcher follows the indexed folders on it again or no longer.
    fn volume(
        &mut self,
        run: &mut Run<'_>,
        event: VolumeEvent,
        mounts: &[PlatformMount],
    ) -> Result<(), Error> {
        match &event {
            VolumeEvent::Mounted { mount } => {
                run.batch.push(Write::Offline {
                    mount_point: mount.mount_point.clone(),
                    offline: false,
                });
                let back: Vec<usize> = (0..self.roots.len())
                    .filter(|&index| {
                        let root = &self.roots[index];
                        !root.active && root.path.starts_with(&mount.mount_point)
                    })
                    .collect();
                for index in back {
                    // A stale root comes back without its cursor, so it is listed again.
                    let resume = self.resume(run.connection, index);
                    self.add(index, resume, mounts, &run.config.post);
                }
            }
            VolumeEvent::Unmounted { mount_point } => {
                run.batch.push(Write::Offline {
                    mount_point: mount_point.clone(),
                    offline: true,
                });
                for index in 0..self.roots.len() {
                    if self.roots[index].path.starts_with(mount_point) {
                        self.remove(index);
                        (run.config.post)(LaneEvent::Watching {
                            path: self.roots[index].path.clone(),
                            watching: Err(OFFLINE.into()),
                        });
                    }
                }
            }
        }
        (run.config.post)(LaneEvent::Volume(event));
        Ok(())
    }
}

/// The next listing's time, later than every earlier one's.
fn next_stamp(last: &mut i64) -> i64 {
    *last = crate::editor::now_ms().max(*last + 1);
    *last
}

/// Drop the rows at each of `paths` and under it.
fn drop_under(run: &mut Run<'_>, paths: &[PathBuf]) -> Result<(), Error> {
    for path in paths {
        run.batch.push(Write::Gone(path.clone()));
        let under = database::files_under(run.connection, path)?;
        if !under.is_empty() {
            run.batch.push(Write::Vanished(under));
        }
    }
    Ok(())
}

/// Whether `path` is on a network volume: the mount it is under, by its mount point, is not
/// local. Read from the mount table alone, touching no file system.
fn on_network(mounts: &[PlatformMount], path: &Path) -> bool {
    mounts
        .iter()
        .filter(|mount| path.starts_with(&mount.mount_point))
        .max_by_key(|mount| mount.mount_point.as_os_str().len())
        .is_some_and(|mount| !mount.local)
}

/// What a changed path is now, as a listing would see it.
enum Seen {
    /// A supported file the listing would take, in its folder.
    File(PathBuf, ListedFile),
    /// A folder the listing would enter.
    Folder,
    /// Gone, or something the listing skips: its row and every row under it go.
    Gone,
}

/// Looks at changed paths under one root with a listing's rules: no link followed, no other volume
/// entered, nothing deeper than the listing goes, and nothing the exclusions skip, in the path or
/// any folder above it up to the root. What it learns of each folder above is kept for the
/// event's other paths.
struct Look<'a> {
    root: &'a Path,
    device: Option<u64>,
    mount_point: &'a Path,
    exclusions: &'a crate::index::exclude::Exclusions,
    limits: WalkLimits,
    /// Whether the listing enters each folder looked at so far.
    folders: HashMap<PathBuf, bool>,
}

impl<'a> Look<'a> {
    /// A look under the root at `root`, whose own metadata is `metadata`, on the volume mounted
    /// at `mount_point`, with the run's exclusions and limits.
    fn new(
        root: &'a Path,
        metadata: &std::fs::Metadata,
        mount_point: &'a Path,
        run: &Run<'a>,
    ) -> Self {
        Self {
            root,
            device: device(metadata),
            mount_point,
            exclusions: run.exclusions,
            limits: run.config.limits,
            folders: HashMap::new(),
        }
    }
}

impl Look<'_> {
    fn at(&mut self, path: &Path) -> Seen {
        let Some(parent) = path.parent() else {
            return Seen::Gone;
        };
        if parent != self.root && !self.enters(parent) {
            return Seen::Gone;
        }
        let Ok(metadata) = path.symlink_metadata() else {
            return Seen::Gone;
        };
        let Some(name) = path.file_name() else {
            return Seen::Gone;
        };
        let depth = self.depth(path);
        if metadata.is_dir() {
            if self.folder(path, &metadata, depth) {
                Seen::Folder
            } else {
                Seen::Gone
            }
        } else if metadata.is_file() && device(&metadata) == self.device {
            match kind_of(name) {
                Some(kind) if !hidden(name, &metadata) && depth <= self.limits.depth + 1 => {
                    Seen::File(
                        parent.to_path_buf(),
                        ListedFile {
                            name: name.to_string_lossy().into_owned(),
                            kind,
                            signature: FileSignature::of(&metadata),
                        },
                    )
                }
                _ => Seen::Gone,
            }
        } else {
            // A link, or anything else that is neither a file nor a folder, is never listed.
            Seen::Gone
        }
    }

    /// How many folders below the root `path` is (a folder right in the root is 1).
    fn depth(&self, path: &Path) -> usize {
        path.strip_prefix(self.root)
            .map_or(usize::MAX, |relative| relative.components().count())
    }

    /// Whether a listing of the root enters the folder `path` and every folder above it.
    fn enters(&mut self, path: &Path) -> bool {
        if path == self.root {
            return true;
        }
        if let Some(&known) = self.folders.get(path) {
            return known;
        }
        let above = path
            .parent()
            .is_some_and(|parent| parent.starts_with(self.root) && self.enters(parent));
        let enters = above
            && path.symlink_metadata().is_ok_and(|metadata| {
                metadata.is_dir() && self.folder(path, &metadata, self.depth(path))
            });
        self.folders.insert(path.to_path_buf(), enters);
        enters
    }

    /// Whether a listing enters the folder `path`, whose own metadata is `metadata`, `depth`
    /// folders below the root, once it has entered the folder above.
    fn folder(&self, path: &Path, metadata: &std::fs::Metadata, depth: usize) -> bool {
        let Some(name) = path.file_name() else {
            return false;
        };
        depth <= self.limits.depth
            && device(metadata) == self.device
            && self
                .exclusions
                .directory(
                    path,
                    name,
                    metadata,
                    path.parent() == Some(self.mount_point),
                )
                .is_none()
    }
}

#[cfg(unix)]
fn device(metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev())
}

#[cfg(not(unix))]
fn device(_: &std::fs::Metadata) -> Option<u64> {
    None
}
