//! The catalog lanes on the owner: each lane's owner-side state, the messages its workers post
//! back, its method handlers and its per-client clean-up live in a file of its own beside this one
//! (`files.rs` lane A, `previews.rs` lane B, `library.rs` lane C, `views.rs` lane D), so the lanes
//! grow the owner in parallel without touching the same lines. The owner holds them together as
//! [`CatalogLanes`], hands every [`CatalogMessage`] to [`Owner::catalog_message`], tells them when
//! a client disconnects and stops them when it stops.
//!
//! A lane's module is a child of `owner`, so its handlers take `&mut Owner` and reach the catalog,
//! the sessions, the job table, the activity board and the event announcements as the owner's own
//! handlers do. The domain code stays in the lane's core module (`crate::index`, `crate::previews`,
//! `crate::library`, `crate::browse`); these files are the glue.
#![allow(
    dead_code,
    reason = "catalog contracts: the lanes fill these as they land"
)]

use super::{ClientId, Owner, OwnerMessage, files, library, previews, views};
use crate::{
    JobId,
    activity::ActivityBoard,
    api::Origin,
    catalog_types::LibraryItem,
    jobs::{JobKind, Jobs},
};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc::SyncSender},
};

/// What a catalog lane's worker posts to the owner, which the owner hands to that lane.
pub(super) enum CatalogMessage {
    Files(files::FilesMessage),
    Previews(previews::PreviewsMessage),
    Library(library::LibraryMessage),
}

/// How a lane's worker posts into the owner's own channel: nothing polls, and the owner sleeps
/// until a message arrives.
#[derive(Clone)]
pub(super) struct Poster {
    sender: SyncSender<OwnerMessage>,
}

impl Poster {
    pub(super) fn new(sender: SyncSender<OwnerMessage>) -> Self {
        Self { sender }
    }

    /// Post `message`; a stopped owner has nobody to tell.
    pub(super) fn post(&self, message: CatalogMessage) {
        let _ = self.sender.send(OwnerMessage::Catalog(message));
    }
}

/// Every catalog lane's owner-side state.
pub(super) struct CatalogLanes {
    pub(super) files: files::FilesLane,
    pub(super) previews: previews::PreviewsLane,
    pub(super) library: library::LibraryLane,
    pub(super) views: views::ViewsLane,
}

impl CatalogLanes {
    /// The lanes, each given how to post back to the owner and the board its work publishes to.
    /// Nothing starts here: a lane starts its workers on its first job, so an owner that never
    /// browses runs no thread for it.
    pub(super) fn new(poster: Poster, activity: Arc<ActivityBoard>) -> Self {
        Self {
            files: files::FilesLane::new(poster.clone(), activity.clone()),
            previews: previews::PreviewsLane::new(poster.clone(), activity.clone()),
            library: library::LibraryLane::new(poster, activity),
            views: views::ViewsLane::default(),
        }
    }

    /// Forget what the lanes hold for a client that has gone. A lane that ends a job of its own
    /// for it records that in `jobs`.
    pub(super) fn disconnect(&mut self, client: ClientId, jobs: &mut Jobs) {
        self.files.disconnect(client);
        self.previews.disconnect(client, jobs);
        self.library.disconnect(client);
        self.views.disconnect(client);
    }

    /// Stop every lane's workers as the owner stops.
    pub(super) fn shutdown(self) {
        self.files.shutdown();
        self.previews.shutdown();
        self.library.shutdown();
    }
}

impl Owner {
    /// A job of a catalog lane was cancelled in the job table (`job.cancel`): the lane that runs it
    /// drops it from its queue, and a running one stops at its next checkpoint through its
    /// control. A lane whose job has no one worker to report its end records it in the job table;
    /// lane A announces the end of an `index.refresh` job that was waiting.
    pub(super) fn catalog_cancelled(&mut self, job_id: &JobId, kind: JobKind) {
        match kind {
            JobKind::IndexRefresh => files::cancelled(self, job_id),
            JobKind::PreviewExtract | JobKind::PreviewRegion | JobKind::PreviewRender => {
                self.catalog.previews.cancelled(job_id, &mut self.jobs)
            }
            _ => self.catalog.library.cancelled(job_id),
        }
    }

    /// A library change committed, a change, an undo or a redo alike, under `origin`: each lane
    /// that follows a kind of item it changed hears which, with the owner, so it can schedule work.
    /// Lane A follows the indexed folders it lists, so an undone `index.add-folder` stops its
    /// listing and forgets its rows and a redone one lists it again. A lane that follows another
    /// kind adds its arm here.
    pub(super) fn library_changed(&mut self, origin: &Origin, items: &[LibraryItem]) {
        let folders: Vec<PathBuf> = items
            .iter()
            .filter_map(|item| match item {
                LibraryItem::IndexedFolder { path } => Some(path.clone()),
                _ => None,
            })
            .collect();
        if !folders.is_empty() {
            files::indexed_folders_changed(self, origin, &folders);
        }
        // Lane C: photographs a change sent back (an `asset.send-back`, or the undo of a Develop)
        // are gone from the catalog, and lane B forgets their rendered previews.
        let sent_back: Vec<crate::AssetId> = items
            .iter()
            .filter_map(|item| match item {
                LibraryItem::DevelopedAsset { asset_id } => Some(asset_id),
                _ => None,
            })
            .filter(|asset_id| {
                !crate::editor::library_rows::has_asset(&self.service.connection, asset_id)
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        if !sent_back.is_empty() {
            previews::forget_photographs(self, &sent_back);
        }
    }

    /// Hand one catalog message to the lane whose worker posted it.
    pub(super) fn catalog_message(&mut self, message: CatalogMessage) {
        match message {
            CatalogMessage::Files(message) => files::handle(self, message),
            CatalogMessage::Previews(message) => previews::handle(self, message),
            CatalogMessage::Library(message) => library::handle(self, message),
        }
    }
}
