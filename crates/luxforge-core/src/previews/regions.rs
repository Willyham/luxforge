//! The 100% region's worker: one thread, started on the first `preview.region` and blocked on its
//! channel while idle, that answers one region job at a time, apart from the two extraction
//! workers (`lane.rs`), so a development of a second or more never holds up a grid or loupe
//! preview and an extraction worker never runs a region.
//!
//! The owner plans each job from SQL alone — a file's index row, or the catalog's record of a
//! developed photograph's original — and hands it here as a [`RegionWork`]. The worker checks the
//! file is still the one planned, answers the region through [`region::region`] with the job's
//! render token (the embedded full-size preview decoded for that region alone, or the one kept
//! development), encodes it ([`region::encode_region`]), writes the answer's JPEG through a
//! temporary file and a rename, never flushed (it is disposable), and posts the answer back
//! ([`RegionDone`]).
//!
//! **Answers.** `<catalog>.index/previews/regions/<client>-<sequence>.jpg`
//! ([`answer_path`]), valid until the client's next region: the owner removes a client's previous
//! answer once the next is written, and the client's answer when it disconnects. Nothing about a
//! region is a row of the index, and nothing outlives the process: the worker clears the
//! directory when it starts.
use super::region::{self, FrameSize, RegionRequest};
use crate::{
    AssetRecord, Error, JobId, SourceTag,
    atomic_file::file_error,
    catalog_types::{Dimensions, FileSignature, PixelRect, PreviewItem, RegionAnswer},
    jobs::JobControl,
};
use std::{
    fs, io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread,
};

/// The answers' directory inside the preview cache.
pub(crate) const REGIONS_DIR: &str = "regions";

/// Where a region's pixels come from, as the owner planned it.
#[derive(Clone, Debug)]
pub(crate) enum RegionSource {
    /// A file of the index, as its row records it: one whose signature has changed since is
    /// `source-unavailable`, and the index must read it again.
    File {
        path: PathBuf,
        kind: SourceTag,
        signature: FileSignature,
    },
    /// A developed photograph's original, as the catalog records it: the file must still be the
    /// one the catalog recorded ([`AssetRecord::is_original`]), and one that is gone or is another
    /// file is `source-unavailable`.
    Original(Box<AssetRecord>),
}

/// One region job, planned by the owner and answered by the worker.
pub(crate) struct RegionWork {
    pub job_id: JobId,
    /// What the request named, echoed in the answer.
    pub item: PreviewItem,
    pub source: RegionSource,
    /// In the upright full-resolution frame of the source, or in [`Self::frame`] when named.
    pub rect: PixelRect,
    pub frame: Option<Dimensions>,
    /// Where the answer's JPEG is written.
    pub path: PathBuf,
    /// The job's control: its cancel flag and the render token every pass of the region checks.
    pub control: Arc<JobControl>,
    /// Where a test holds the worker before it starts the job.
    #[cfg(test)]
    pub hold: Option<Arc<luxforge_testbase::Gate>>,
}

/// What the worker posts back when a job ends.
#[derive(Debug)]
pub(crate) struct RegionDone {
    pub job_id: JobId,
    pub result: Result<RegionAnswer, Error>,
}

/// How the worker posts into the owner's channel.
pub(crate) type RegionPost = Arc<dyn Fn(RegionDone) + Send + Sync>;

/// The region worker's channel: the owner sends only while the worker is idle, so a send never
/// waits.
pub(crate) struct RegionWorker {
    sender: SyncSender<RegionWork>,
}

impl RegionWorker {
    /// Start the worker, which writes its answers under `previews` and posts each through `post`.
    pub(crate) fn start(previews: &Path, post: RegionPost) -> Result<Self, Error> {
        let (sender, receiver) = sync_channel(1);
        let dir = previews.join(REGIONS_DIR);
        thread::Builder::new()
            .name("luxforge-preview-region".into())
            .spawn(move || work(&dir, &receiver, &post))
            .map_err(|error| Error::internal(format!("cannot start the region worker: {error}")))?;
        Ok(Self { sender })
    }

    /// Hand `work` to the idle worker: `internal` when the worker has gone.
    pub(crate) fn send(&self, work: RegionWork) -> Result<(), Error> {
        self.sender
            .send(work)
            .map_err(|_| Error::internal("the region worker has stopped"))
    }
}

/// The worker: clear what a previous process left, then answer one job at a time until the lane
/// stops, when its channel closes.
fn work(dir: &Path, jobs: &Receiver<RegionWork>, post: &RegionPost) {
    clear(dir);
    while let Ok(work) = jobs.recv() {
        #[cfg(test)]
        if let Some(hold) = &work.hold {
            hold.pass();
        }
        // A panic is that job's failure, never a worker the owner waits on forever.
        let result = catch_unwind(AssertUnwindSafe(|| answer(&work)))
            .unwrap_or_else(|_| Err(Error::internal("the region worker failed")));
        post(RegionDone {
            job_id: work.job_id,
            result,
        });
    }
}

/// Remove every file in the answers' directory: none is valid once the clients told of it are
/// gone, as every client of an earlier process is.
fn clear(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        remove(&entry.path());
    }
}

/// Where `client`'s region number `sequence` is answered, under the preview cache `previews`.
pub(crate) fn answer_path(previews: &Path, client: u64, sequence: u64) -> PathBuf {
    previews
        .join(REGIONS_DIR)
        .join(format!("{client}-{sequence}.jpg"))
}

/// Remove an answer's file. One already gone is what was wanted, and one that cannot be removed
/// costs only its bytes until the worker next starts.
pub(crate) fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Answer one job: the file checked, the region cut, encoded and written. Every step checks the
/// job's cancellation; a cancelled job writes nothing.
pub(crate) fn answer(work: &RegionWork) -> Result<RegionAnswer, Error> {
    let control = &work.control;
    control.checkpoint()?;
    let request = request(work)?;
    let image = region::region(&request, control.render_cancel())?;
    control.checkpoint()?;
    let jpeg = region::encode_region(&image)?;
    control.checkpoint()?;
    write(&work.path, &jpeg)?;
    Ok(RegionAnswer {
        item: work.item.clone(),
        rect: image.rect,
        frame: Dimensions {
            width: image.frame.width,
            height: image.frame.height,
        },
        path: work.path.clone(),
        width: image.rect.width,
        height: image.rect.height,
        origin: image.origin,
    })
}

/// The domain's request for `work`: a file's path, kind and indexed signature as planned, or a
/// photograph's original checked against the catalog's record and taken at the signature it has
/// now, which the region's open then holds the file to.
fn request(work: &RegionWork) -> Result<RegionRequest, Error> {
    let (path, kind, signature) = match &work.source {
        RegionSource::File {
            path,
            kind,
            signature,
        } => (path.clone(), *kind, *signature),
        RegionSource::Original(asset) => {
            let metadata = fs::metadata(&asset.locator).map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => {
                    Error::source_unavailable("the photograph's original is gone")
                }
                kind => file_error(asset.locator.display(), kind),
            })?;
            if !metadata.is_file() || !asset.is_original(&metadata) {
                return Err(Error::source_unavailable(
                    "the photograph's original is not the file it was developed from",
                ));
            }
            (
                asset.locator.clone(),
                asset.source.tag(),
                FileSignature::of(&metadata),
            )
        }
    };
    Ok(RegionRequest {
        path,
        kind,
        signature,
        rect: work.rect,
        frame: work.frame.map(|frame| FrameSize {
            width: frame.width,
            height: frame.height,
        }),
    })
}

/// Write an answer's JPEG to `path` through a temporary file beside it and a rename, so a reader
/// sees the whole file or none. Never flushed: an answer is disposable.
fn write(path: &Path, jpeg: &[u8]) -> Result<(), Error> {
    let folder = path.parent().expect("an answer's path has a folder");
    fs::create_dir_all(folder).map_err(|error| file_error(folder.display(), error.kind()))?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    if let Err(error) = fs::write(&temporary, jpeg) {
        remove(&temporary);
        return Err(file_error(temporary.display(), error.kind()));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        remove(&temporary);
        return Err(file_error(path.display(), error.kind()));
    }
    Ok(())
}
