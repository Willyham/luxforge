//! The render worker: one thread of its own, started on a photograph's first render request and
//! blocked on its channel while idle, apart from the extraction workers and the region worker,
//! that renders developed photographs' grid and large tiers one render at a time
//! (`docs/design/catalog.md`, "Rendered previews").
//!
//! The owner plans each render in `O(layers)` ([`rendered::plan_render`]) and hands it here as a
//! [`RenderWork`] only while the worker is idle, so a send never waits. The worker renders it
//! ([`rendered::render`]: the original read and verified by the render itself, off the editor's
//! source cache and source worker, every tier the job wants from one preparation), writes each
//! tier and its row (`photos.rs`), collects the photograph's stale rows and files once the new
//! tiers are written when it rendered the entry that was current ([`rendered::stale_rows`]), keeps
//! the large tier within the byte budget it shares with files' loupe tiers ([`Store::evict`]), and
//! posts the outcome back ([`RenderDone`]). One render at a time is the design's one RAW at a time
//! off the editor's cache: the worker holds at most one preparation.
//!
//! **Renderer generation.** When the worker starts, a second, short-lived thread discards the
//! rows of other renderer generations with their files, a page at a time
//! ([`photos::discard_other_generations`]), and ends; nothing on the owner's request path waits
//! for it, and a row it has not reached yet is never served, since its key is not current.
use super::{
    cache::Store,
    lane::now_ms,
    photos::{self, NewTier},
    rendered::{self, RENDERER_GENERATION, RenderRequest, RenderedTier},
};
use crate::{
    EntryId, Error, ErrorKind,
    catalog_types::{PreviewInfo, PreviewOrigin, PreviewTier},
    jobs::JobControl,
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread,
};

/// The most renders waiting for the render worker. A grid screen at its smallest cells shows about
/// 200 photographs and Develop's filmstrip a handful of neighbours, so this is ten screens of
/// visible cells ahead of one worker that takes a tenth of a second to a few seconds a render; a
/// client that scrolls further cancels the renders it no longer shows.
pub(crate) const RENDER_QUEUE_CAPACITY: usize = 2_000;

/// One render, planned by the owner and made by the worker.
pub(crate) struct RenderWork {
    /// The owner's number for it, echoed in [`RenderDone`].
    pub id: u64,
    pub request: RenderRequest,
    /// The photograph's current entry when the render was planned: its stale rows are collected
    /// only when the render is of this entry, never for a named earlier one.
    pub current: EntryId,
    /// The render's cancel flag and render token, which a cancel sets.
    pub control: Arc<JobControl>,
    /// The bytes the loupe and large tiers may take together.
    pub budget: u64,
    /// Where a test holds the worker before it starts the render.
    #[cfg(test)]
    pub hold: Option<Arc<luxforge_testbase::Gate>>,
}

/// What the worker posts back when a render ends: every tier it wrote, or why it wrote none.
#[derive(Debug)]
pub(crate) struct RenderDone {
    pub id: u64,
    pub result: Result<Vec<PreviewInfo>, Error>,
}

/// How the worker posts into the owner's channel.
pub(crate) type RenderPost = Arc<dyn Fn(RenderDone) + Send + Sync>;

/// The render worker's channel: the owner sends only while the worker is idle, so a send never
/// waits.
pub(crate) struct RenderWorker {
    sender: SyncSender<RenderWork>,
    /// Stops the discard of other generations when the lane stops.
    discard: Arc<JobControl>,
}

impl RenderWorker {
    /// Start the worker, which writes through `store` and posts each outcome through `post`, and
    /// the discard of other generations' rows through `discard`, a second connection.
    pub(crate) fn start(store: Store, discard: Store, post: RenderPost) -> Result<Self, Error> {
        let (sender, receiver) = sync_channel(1);
        thread::Builder::new()
            .name("luxforge-preview-render".into())
            .spawn(move || work(store, &receiver, &post))
            .map_err(|error| Error::internal(format!("cannot start the render worker: {error}")))?;
        let stop = JobControl::new();
        let control = stop.clone();
        let mut discard = discard;
        // A discard that fails leaves its rows for the next process: none of them is served.
        let _ = thread::Builder::new()
            .name("luxforge-preview-discard".into())
            .spawn(move || photos::discard_other_generations(&mut discard, &control));
        Ok(Self {
            sender,
            discard: stop,
        })
    }

    /// Hand `work` to the idle worker: `internal` when the worker has gone.
    pub(crate) fn send(&self, work: RenderWork) -> Result<(), Error> {
        self.sender
            .send(work)
            .map_err(|_| Error::internal("the render worker has stopped"))
    }
}

impl Drop for RenderWorker {
    fn drop(&mut self) {
        self.discard.cancel("the preview lane stopped");
    }
}

/// The worker: one render at a time until the lane stops, when its channel closes.
fn work(mut store: Store, renders: &Receiver<RenderWork>, post: &RenderPost) {
    while let Ok(work) = renders.recv() {
        #[cfg(test)]
        if let Some(hold) = &work.hold {
            hold.pass();
        }
        // A panic is that render's failure, never a worker the owner waits on forever.
        let result = catch_unwind(AssertUnwindSafe(|| run(&mut store, &work)))
            .unwrap_or_else(|_| Err(Error::internal("the render worker failed")));
        post(RenderDone {
            id: work.id,
            result,
        });
    }
}

/// Make one render: its tiers rendered from one preparation, each written with its row, the
/// photograph's stale rows collected when the render is of its current entry, and the budget kept
/// when it wrote a large tier. A cancel stops it at its next checkpoint and it writes nothing
/// after.
pub(crate) fn run(store: &mut Store, work: &RenderWork) -> Result<Vec<PreviewInfo>, Error> {
    let control = &work.control;
    control.checkpoint()?;
    let tiers = rendered::render(&work.request, control.render_cancel()).map_err(|error| {
        if error.kind == ErrorKind::Cancelled {
            control.cancelled_error()
        } else {
            error
        }
    })?;
    control.checkpoint()?;
    let now = now_ms();
    let mut written = Vec::with_capacity(tiers.len());
    for tier in &tiers {
        written.push(write(store, tier, now, control)?);
    }
    let asset_id = work.request.asset_id();
    if work.request.entry_id() == &work.current {
        let stale = rendered::stale_rows(store.connection(), asset_id, &work.current)?;
        photos::collect(store, &stale)?;
    }
    if let Some(large) = written.iter().find(|info| info.tier == PreviewTier::Large) {
        store.evict(work.budget, &large.path)?;
    }
    Ok(written)
}

/// Write one rendered tier and its row, labelled approximate when its proxy render is; none once
/// `control` is cancelled.
fn write(
    store: &mut Store,
    tier: &RenderedTier,
    now_ms: i64,
    control: &JobControl,
) -> Result<PreviewInfo, Error> {
    let name = tier.key.file_name();
    let row = photos::write(
        store,
        &NewTier {
            asset_id: &tier.key.asset_id,
            entry_id: &tier.key.entry_id,
            tier: tier.key.tier,
            renderer: i64::from(RENDERER_GENERATION),
            origin: PreviewOrigin::Rendered,
            approximate: tier.approximate(),
            name: &name,
            jpeg: &tier.jpeg,
            width: tier.width,
            height: tier.height,
            now_ms,
            control,
        },
    )?
    .ok_or_else(|| Error::internal("a rendered tier was refused"))?;
    Ok(tier.info(row.path))
}
