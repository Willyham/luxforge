//! The Select grid's decoded previews ([catalog design](../../../../docs/design/catalog.md#architecture),
//! "Desktop"): each wanted cell's grid preview, read through `preview.read`, decoded off the update
//! loop at the size its cell needs, and held as one image handle, made once and lent to the grid
//! while its cell may be shown. **Lane D (views and desktop)** owns this module; the Select seam
//! (`select.rs`) drives it.
//!
//! - **What is wanted.** After every message the Select seam hands over the grid as it is
//!   ([`GridWindow`]): the files of the cells on screen, then of those within one screen of it,
//!   nearest first, with the view's revision and the pixels a cell's photograph box needs
//!   ([`Wanted`]). A row the grid has not read yet, or a photograph's, wants nothing.
//! - **Reads.** `preview.read {item: {kind: file, file_id}, tier: grid, priority}` for every wanted
//!   file not yet asked under this revision — `visible` on screen, `background` in the margin — at
//!   most [`READ_BATCH`] in one owner task and one task at a time, never for a row whose grid state
//!   is `unavailable`. `ready` names the tier; `queued` names the job making it and the best preview
//!   cached meanwhile (the thumbnail stage), which is drawn until the tier replaces it; a refusal is
//!   remembered for the revision and not asked again (`resource-limit` waits for the owner's next
//!   wake). A new revision asks every wanted file once more, so a file that changed shows its new
//!   preview, and an unchanged key decodes nothing again.
//! - **Waking.** The owner wakes this client ([`OwnerHandle::watch_previews`]) when a preview it
//!   waits on is written or its task ends. The wake, on the owner thread, only raises a flag and
//!   posts this module's signal; on the update loop the files still queued among those wanted are
//!   read again, and so are those a batch in flight at the wake then answers `queued`. Nothing
//!   polls.
//! - **Decoding.** One [`Latest`] worker, started with the first decode and blocked while idle,
//!   takes the newest plan — every wanted file whose answer names a preview not held at the size
//!   the cell needs, cells on screen first, at most [`MAX_PLAN`] — and decodes them one by one
//!   through [`decode_preview`], handing each over as it lands, at most [`DECODED_WAITING`] waiting.
//!   A plan with a decode the running one lacks supersedes it after the decode it is making, so
//!   the cells scrolled away are never decoded; a plan that only shrank lets the running one go on.
//! - **Handles.** A decoded preview becomes its handle here, once, on the update loop, while it is
//!   still the preview the file's newest answer names (its `key`): the one `Handle::from_rgba` in
//!   the desktop. The handle keeps its id while it is held, so its texture is uploaded once; the
//!   grid borrows it ([`GridImages`]). A better stage — the embedded preview after the thumbnail —
//!   replaces the held one when it is decoded, so the cell never goes blank between the two.
//! - **Budget.** A held preview is charged its RGBA bytes against [`DECODED_BUDGET_BYTES`], and at
//!   most [`MAX_HANDLES`] are held. Past either, the least recently wanted go first; a cell on
//!   screen is never evicted, and a margin cell only for a cell on screen. A decoded preview that
//!   cannot fit so is dropped, and not decoded again until the wanted cells change.
//! - **Dropping.** [`SelectPreviews::release`] forgets everything — handles, answers and the plan —
//!   when Select is left and the worker blocks idle; dropping the cache (the catalog closing with
//!   the window) ends the worker. A view's new revision drops nothing it holds: its previews stay,
//!   the least recently wanted evicted first.
use crate::app::{
    tasks::{call_detailed, owner_work},
    waker::Signal,
};
use crate::state::select::RowCache;
use iced::{Subscription, Task, widget::image::Handle};
use luxforge_core::{
    ClientId, DecodedPreview, ErrorKind, OwnerHandle,
    catalog_types::{
        FileId, PreviewAnswer, PreviewInfo, PreviewItem, PreviewPriority, PreviewState,
        PreviewTier, RowItem,
    },
    decode_preview,
    latest::{Latest, Running},
};
use luxforge_ui::GridLayout;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

/// The decoded grid previews the desktop holds, in RGBA8 bytes, whatever the view's size: the
/// catalog design's provisional desktop budget (Performance, "Memory"). A Select cell's photograph
/// at scale 2 is about 240 × 160 pixels, 150 KiB, so it holds about 1,300 previews — the cells on
/// screen and a screen either side of the largest window several times over — while the
/// photographs Develop decodes keep their own limits.
pub(crate) const DECODED_BUDGET_BYTES: usize = 192 << 20;

/// The most previews held, whatever their size: tiny previews would otherwise let the bookkeeping
/// grow within the byte budget.
pub(crate) const MAX_HANDLES: usize = 4096;

/// `preview.read`s in one owner task. Each is one query and one stat on the owner; the smallest
/// cells put about 100 on a large screen, so a screen and its margin take a few batches, one at a
/// time, each a few milliseconds of the owner's time.
pub(crate) const READ_BATCH: usize = 64;

/// The decodes one plan holds: more than the cells on screen and a screen either side at the
/// smallest cell size.
pub(crate) const MAX_PLAN: usize = 512;

/// Decoded previews waiting for the update loop; the worker waits for room past it. Each is at
/// most the grid tier's 512 px square, 1 MiB.
pub(crate) const DECODED_WAITING: usize = 8;

/// The longest side a preview is decoded at: twice the grid tier's 512 px, which a decode never
/// enlarges past anyway.
pub(crate) const MAX_SIDE: u32 = 1024;

/// What this module's owner tasks and its signal bring back to the update loop. The Select seam
/// carries it in one variant of its own message and hands it to [`SelectPreviews::update`].
#[derive(Clone, Debug)]
pub(crate) enum SelectPreviewMessage {
    /// One owner task's `preview.read` answers.
    Read(ReadAnswers),
    /// This module's signal: the worker handed over a decoded preview, or the owner wrote a preview
    /// this client waits on.
    Woken,
}

/// One owner task's reads: each file with the priority it is asked at, under the view revision
/// the cells were read at.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReadBatch {
    pub(crate) serial: u64,
    pub(crate) revision: u64,
    pub(crate) reads: Vec<(FileId, PreviewPriority)>,
}

/// What the owner answered one [`ReadBatch`], file by file.
#[derive(Clone, Debug)]
pub(crate) struct ReadAnswers {
    pub(crate) serial: u64,
    pub(crate) revision: u64,
    pub(crate) answers: Vec<(FileId, Result<PreviewAnswer, Refusal>)>,
}

/// Why the owner refused a file's preview: its error code and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) code: String,
    pub(crate) message: String,
}

/// What the grid wants now: the cells on screen before those near it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Wanted {
    /// The view revision the rows were read at.
    pub(crate) revision: u64,
    /// The files of the cells on screen, in reading order, each with its row's grid state.
    pub(crate) visible: Vec<(FileId, PreviewState)>,
    /// The files of the cells within one screen of them, nearest first.
    pub(crate) margin: Vec<(FileId, PreviewState)>,
    /// The longest side of a cell's photograph box, in pixels.
    pub(crate) side: u32,
}

/// The grid as the Select seam holds it: its layout, the rows read so far, its scroll offset and
/// height, and the display's scale.
#[derive(Clone, Copy)]
pub(crate) struct GridWindow<'a> {
    pub(crate) layout: &'a GridLayout,
    pub(crate) rows: &'a RowCache,
    pub(crate) scroll: f32,
    pub(crate) height: f32,
    pub(crate) scale_factor: f32,
}

impl Wanted {
    /// The files `window` shows and those within one screen of it: the rows window's own reach.
    pub(crate) fn of(window: GridWindow<'_>) -> Self {
        let GridWindow {
            layout,
            rows,
            scroll,
            height,
            scale_factor,
        } = window;
        let file = |cell: u32| {
            let row = rows.row(layout.cell(cell).item)?;
            match row.item {
                RowItem::File { file_id } => Some((file_id, row.preview)),
                RowItem::Photo { .. } => None,
            }
        };
        let on_screen = layout.visible_cells(scroll, height, 0.0);
        let near = layout.visible_cells(scroll, height, height);
        let below: Vec<u32> = (on_screen.end..near.end).collect();
        let above: Vec<u32> = (near.start..on_screen.start).rev().collect();
        // Nearest the screen first, the cells after it before those before it.
        let mut margin = Vec::with_capacity(below.len() + above.len());
        for at in 0..below.len().max(above.len()) {
            margin.extend(below.get(at).copied().and_then(file));
            margin.extend(above.get(at).copied().and_then(file));
        }
        let most = layout.metrics().image_max;
        Self {
            revision: rows.revision(),
            visible: on_screen.filter_map(file).collect(),
            margin,
            side: side(most.width.max(most.height), scale_factor),
        }
    }
}

/// The pixels a photograph box of `points` needs at `scale_factor`, within `1..=MAX_SIDE`.
fn side(points: f32, scale_factor: f32) -> u32 {
    let pixels = (points * scale_factor).ceil();
    if pixels.is_finite() {
        (pixels as u32).clamp(1, MAX_SIDE)
    } else {
        MAX_SIDE
    }
}

/// Where a file's `preview.read` stands under the current revision.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Read {
    /// Not asked under this revision.
    Unasked,
    /// In the batch in flight.
    Asking,
    /// The tier is cached: nothing more to ask.
    Ready,
    /// The lane is making it; asked again once the owner wakes this client.
    Queued,
    /// The owner woke this client while it was queued: asked again at the next plan.
    Recheck,
    /// Refused: not asked again under this revision.
    Refused(Refusal),
}

/// The preview a file's newest answer names.
#[derive(Clone, Debug)]
struct Source {
    key: String,
    path: PathBuf,
    /// Its longest side, which a decode never passes.
    long_edge: u32,
}

impl From<PreviewInfo> for Source {
    fn from(info: PreviewInfo) -> Self {
        Self {
            key: info.key,
            path: info.path,
            long_edge: info.width.max(info.height).max(1),
        }
    }
}

/// A decoded preview's image handle, made once.
struct Held {
    key: String,
    handle: Handle,
    /// The side it was decoded to fit.
    side: u32,
    bytes: usize,
}

/// One file's reads, its preview and its handle.
struct Entry {
    read: Read,
    source: Option<Source>,
    held: Option<Held>,
    /// A key whose decode failed: not decoded again until an answer names another.
    failed: Option<String>,
    /// A key whose decode did not fit the budget: not decoded again until the wanted cells change.
    over_budget: Option<String>,
    /// Its row says there is nothing to draw (`unavailable`): it is not asked for.
    unavailable: bool,
    /// The last plan that wanted it; the current one is [`SelectPreviews::tick`].
    wanted_at: u64,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            read: Read::Unasked,
            source: None,
            held: None,
            failed: None,
            over_budget: None,
            unavailable: false,
            wanted_at: 0,
        }
    }
}

/// One decode: the preview an answer named, fitted within `side`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Decode {
    pub(crate) file: FileId,
    pub(crate) key: String,
    pub(crate) path: PathBuf,
    pub(crate) side: u32,
}

/// One decode's pixels, or why it failed.
pub(crate) struct Decoded {
    pub(crate) decode: Decode,
    pub(crate) result: Result<DecodedPreview, String>,
}

/// The decodes wanted now, cells on screen first.
struct Plan(Vec<Decode>);

/// The decode worker: the newest plan, one decode at a time, each handed over as it lands.
struct DecodeWorker {
    worker: Latest<Plan, ()>,
    decoded: mpsc::Receiver<Decoded>,
}

impl DecodeWorker {
    fn start() -> Self {
        let (sender, decoded) = mpsc::sync_channel(DECODED_WAITING);
        let worker = Latest::new(
            "luxforge-select-previews",
            move |plan: Plan, running: &Running<'_, Plan, ()>| {
                for decode in plan.0 {
                    let result =
                        match decode_preview(&decode.path, decode.side, running.superseded()) {
                            // A newer plan wants other cells first, or nobody wants any: it runs next.
                            Err(error) if error.kind == ErrorKind::Cancelled => return None,
                            result => result.map_err(|error| error.to_string()),
                        };
                    // Waits while `DECODED_WAITING` wait; fails once the cache is gone.
                    if sender.send(Decoded { decode, result }).is_err() {
                        return None;
                    }
                    signal().post();
                }
                None
            },
        );
        Self { worker, decoded }
    }
}

/// The Select grid's decoded previews: what each wanted file's answer names, the handles held under
/// the byte budget, the one read batch in flight and the decode worker. See the
/// [module documentation](self).
pub(crate) struct SelectPreviews {
    entries: HashMap<FileId, Entry>,
    wanted: Wanted,
    /// The files of the cells on screen, never evicted.
    on_screen: HashSet<FileId>,
    /// Raised by every change of what is wanted; an entry wanted now was stamped with it.
    tick: u64,
    budget: usize,
    bytes: usize,
    handles: usize,
    /// The read batch in flight: one owner task at a time.
    reading: Option<u64>,
    serial: u64,
    /// Raised by the owner's wake on its thread, taken on the update loop.
    woken: Arc<AtomicBool>,
    /// The owner woke this client while a batch was in flight: what it answers `queued` may already
    /// be written, so it is read again at once.
    woken_while_reading: bool,
    watching: bool,
    decoder: Option<DecodeWorker>,
    /// The decodes last handed to the worker.
    planned: HashSet<Decode>,
    /// Decoded previews dropped because they could not fit, for evidence.
    dropped: u64,
    /// Tests plan decodes without starting the worker, and hand the decodes in themselves.
    #[cfg(test)]
    pub(crate) paused: bool,
    #[cfg(test)]
    pub(crate) last_plan: Vec<Decode>,
}

impl Default for SelectPreviews {
    fn default() -> Self {
        Self::with_budget(DECODED_BUDGET_BYTES)
    }
}

impl fmt::Debug for SelectPreviews {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SelectPreviews")
            .field("entries", &self.entries.len())
            .field("handles", &self.handles)
            .field("bytes", &self.bytes)
            .field("reading", &self.reading)
            .finish_non_exhaustive()
    }
}

/// This module's signal: the worker's and the owner's wakes, carried in as
/// [`SelectPreviewMessage::Woken`].
fn signal() -> &'static Signal<SelectPreviewMessage> {
    static SIGNAL: OnceLock<Signal<SelectPreviewMessage>> = OnceLock::new();
    SIGNAL.get_or_init(|| Signal::new(|| SelectPreviewMessage::Woken))
}

/// One [`SelectPreviewMessage::Woken`] per signal. The Select seam gates it on Select being shown:
/// a signal posted meanwhile is buffered and delivered when it is shown again.
pub(crate) fn subscription() -> Subscription<SelectPreviewMessage> {
    Subscription::run(|| signal().stream())
}

/// `preview.read` of each file of `batch`, as the API reads it, answered file by file.
pub(crate) fn read(owner: &OwnerHandle, client: ClientId, batch: ReadBatch) -> ReadAnswers {
    let answers = batch
        .reads
        .into_iter()
        .map(|(file, priority)| {
            let params = json!({
                "item": PreviewItem::File { file_id: file },
                "tier": PreviewTier::Grid,
                "priority": priority,
            });
            let answer = call_detailed(owner, client, "preview.read", params)
                .map_err(|error| Refusal {
                    code: error.code,
                    message: error.message,
                })
                .and_then(|answer| {
                    serde_json::from_value::<PreviewAnswer>(answer).map_err(|error| Refusal {
                        code: "protocol".into(),
                        message: error.to_string(),
                    })
                });
            (file, answer)
        })
        .collect();
    ReadAnswers {
        serial: batch.serial,
        revision: batch.revision,
        answers,
    }
}

impl SelectPreviews {
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            wanted: Wanted::default(),
            on_screen: HashSet::new(),
            tick: 0,
            budget,
            bytes: 0,
            handles: 0,
            reading: None,
            serial: 0,
            woken: Arc::new(AtomicBool::new(false)),
            woken_while_reading: false,
            watching: false,
            decoder: None,
            planned: HashSet::new(),
            dropped: 0,
            #[cfg(test)]
            paused: false,
            #[cfg(test)]
            last_plan: Vec::new(),
        }
    }

    /// After every message while Select is shown: take in what `window` shows, and read and decode
    /// what it lacks.
    pub(crate) fn want(
        &mut self,
        owner: &OwnerHandle,
        client: ClientId,
        window: GridWindow<'_>,
    ) -> Task<SelectPreviewMessage> {
        self.watch(owner, client);
        let batch = self.plan_for(Wanted::of(window));
        read_task(owner, client, batch)
    }

    /// One of this module's messages.
    pub(crate) fn update(
        &mut self,
        owner: &OwnerHandle,
        client: ClientId,
        message: SelectPreviewMessage,
    ) -> Task<SelectPreviewMessage> {
        let batch = match message {
            SelectPreviewMessage::Read(answers) => self.answered(answers),
            SelectPreviewMessage::Woken => self.woken(),
        };
        read_task(owner, client, batch)
    }

    /// Forget every handle, answer and plan: Select was left. The worker drops its plan and blocks
    /// idle; a batch in flight answers nothing any more.
    pub(crate) fn release(&mut self) {
        if let Some(decoder) = &mut self.decoder {
            decoder.worker.cancel();
            while decoder.decoded.try_recv().is_ok() {}
        }
        self.entries.clear();
        self.wanted = Wanted::default();
        self.on_screen.clear();
        self.tick += 1;
        self.bytes = 0;
        self.handles = 0;
        self.planned.clear();
        self.woken.store(false, Ordering::Relaxed);
        self.woken_while_reading = false;
    }

    /// The handle to draw for `file`, borrowed so it keeps its id.
    pub(crate) fn handle(&self, file: FileId) -> Option<&Handle> {
        self.entries
            .get(&file)?
            .held
            .as_ref()
            .map(|held| &held.handle)
    }

    /// The size of the handle held for `file`.
    #[cfg(test)]
    pub(crate) fn size(&self, file: FileId) -> Option<(u32, u32)> {
        match self.handle(file)? {
            Handle::Rgba { width, height, .. } => Some((*width, *height)),
            Handle::Path(..) | Handle::Bytes(..) => None,
        }
    }

    /// `file` is wanted and has nothing to draw yet, and was neither refused nor reported
    /// unavailable: its cell shows the loading placeholder.
    pub(crate) fn loading(&self, file: FileId) -> bool {
        self.entries.get(&file).is_some_and(|entry| {
            entry.wanted_at == self.tick
                && entry.held.is_none()
                && !entry.unavailable
                && !matches!(entry.read, Read::Refused(_))
        })
    }

    /// `file` has nothing to draw and never will as it is: its row reported it `unavailable`, or
    /// the owner found no usable preview in it. Its cell is Unreadable.
    pub(crate) fn unreadable(&self, file: FileId) -> bool {
        self.entries.get(&file).is_some_and(|entry| {
            entry.held.is_none()
                && (entry.unavailable
                    || matches!(&entry.read, Read::Refused(refusal) if refusal.code == "unsupported-input"))
        })
    }

    /// What the grid's cells borrow, looked up by view position through `rows`.
    pub(crate) fn grid<'a>(&'a self, rows: &'a RowCache) -> GridImages<'a> {
        GridImages {
            previews: self,
            rows,
        }
    }

    /// What the cache holds, for correlated evidence.
    pub(crate) fn summary(&self) -> Value {
        let count = |read: fn(&Read) -> bool| {
            self.entries
                .values()
                .filter(|entry| read(&entry.read))
                .count()
        };
        json!({
            "handles": self.handles,
            "bytes": self.bytes,
            "budget": self.budget,
            "side": self.wanted.side,
            "visible": self.wanted.visible.len(),
            "margin": self.wanted.margin.len(),
            "loading": self.entries.keys().filter(|file| self.loading(**file)).count(),
            "reading": self.reading.is_some(),
            "queued": count(|read| matches!(read, Read::Queued | Read::Recheck)),
            "refused": count(|read| matches!(read, Read::Refused(_))),
            "failed": self.entries.values().filter(|entry| entry.failed.is_some()).count(),
            "decoding": self.decoder.as_ref().is_some_and(|decoder| decoder.worker.is_busy()),
            "dropped": self.dropped,
        })
    }

    /// Ask the owner to wake this client when a preview it waits on is written, once.
    fn watch(&mut self, owner: &OwnerHandle, client: ClientId) {
        if std::mem::replace(&mut self.watching, true) {
            return;
        }
        let woken = self.woken.clone();
        owner.watch_previews(
            client,
            Arc::new(move || {
                woken.store(true, Ordering::Release);
                signal().post();
            }),
        );
    }

    /// Take in what is wanted now, and plan: the read batch to send, if any, and the decodes.
    pub(crate) fn plan_for(&mut self, wanted: Wanted) -> Option<ReadBatch> {
        self.drain();
        if wanted != self.wanted {
            if wanted.revision != self.wanted.revision {
                for entry in self.entries.values_mut() {
                    if entry.read != Read::Asking {
                        entry.read = Read::Unasked;
                    }
                    entry.failed = None;
                }
            }
            self.tick += 1;
            for entry in self.entries.values_mut() {
                entry.over_budget = None;
            }
            for (file, state) in wanted.visible.iter().chain(&wanted.margin) {
                let entry = self.entries.entry(*file).or_default();
                entry.wanted_at = self.tick;
                entry.unavailable = *state == PreviewState::Unavailable;
            }
            self.on_screen = wanted.visible.iter().map(|(file, _)| *file).collect();
            self.wanted = wanted;
        }
        self.plan()
    }

    /// The owner answered a batch: take each answer that is still for this revision, then plan.
    pub(crate) fn answered(&mut self, answers: ReadAnswers) -> Option<ReadBatch> {
        if self.reading == Some(answers.serial) {
            self.reading = None;
        }
        let queued = if std::mem::take(&mut self.woken_while_reading) {
            Read::Recheck
        } else {
            Read::Queued
        };
        let current = answers.revision == self.wanted.revision;
        for (file, answer) in answers.answers {
            let Some(entry) = self.entries.get_mut(&file) else {
                continue;
            };
            if entry.read != Read::Asking {
                continue;
            }
            if !current {
                entry.read = Read::Unasked;
                continue;
            }
            let (read, source) = match answer {
                Ok(PreviewAnswer::Ready { preview }) => (Read::Ready, Some(preview)),
                Ok(PreviewAnswer::Queued { fallback, .. }) => (queued.clone(), fallback),
                // The lane's queue is full: asked again once it has written something.
                Err(refusal) if refusal.code == "resource-limit" => {
                    entry.read = queued.clone();
                    continue;
                }
                Err(refusal) => (Read::Refused(refusal), None),
            };
            entry.read = read;
            match source {
                // The held preview stays until the one named is decoded.
                Some(info) => entry.source = Some(Source::from(info)),
                // Nothing valid is cached for the file: what is held is not its preview.
                None => {
                    entry.source = None;
                    if let Some(held) = entry.held.take() {
                        self.bytes -= held.bytes;
                        self.handles -= 1;
                    }
                }
            }
        }
        self.drain();
        self.plan()
    }

    /// This module's signal: take what the worker decoded and, when the owner woke this client,
    /// read again the files still queued. Then plan.
    pub(crate) fn woken(&mut self) -> Option<ReadBatch> {
        if self.woken.swap(false, Ordering::AcqRel) {
            self.woken_while_reading = self.reading.is_some();
            for entry in self.entries.values_mut() {
                if entry.read == Read::Queued {
                    entry.read = Read::Recheck;
                }
            }
        }
        self.drain();
        self.plan()
    }

    /// Adopt every decoded preview waiting.
    fn drain(&mut self) {
        let Some(decoder) = &self.decoder else {
            return;
        };
        let landed: Vec<Decoded> = decoder.decoded.try_iter().collect();
        for decoded in landed {
            self.adopt(decoded);
        }
    }

    /// Make the handle of a decoded preview that is still the one its file's newest answer names,
    /// charging its bytes and making room for them; drop anything else.
    pub(crate) fn adopt(&mut self, decoded: Decoded) {
        let Decoded { decode, result } = decoded;
        let Some(entry) = self.entries.get_mut(&decode.file) else {
            return;
        };
        // An older stage, or the preview of a file that changed since: a newer answer replaced it.
        if entry
            .source
            .as_ref()
            .is_none_or(|source| source.key != decode.key)
        {
            return;
        }
        let preview = match result {
            Ok(preview) => preview,
            Err(_) => {
                entry.failed = Some(decode.key);
                return;
            }
        };
        if entry
            .held
            .as_ref()
            .is_some_and(|held| held.key == decode.key && held.side >= decode.side)
        {
            return;
        }
        let replaced = entry.held.as_ref().map(|held| held.bytes);
        let bytes = preview.rgba.len();
        if !self.make_room(decode.file, bytes, replaced) {
            self.dropped += 1;
            if let Some(entry) = self.entries.get_mut(&decode.file) {
                entry.over_budget = Some(decode.key);
            }
            return;
        }
        let Some(entry) = self.entries.get_mut(&decode.file) else {
            return;
        };
        let DecodedPreview {
            width,
            height,
            rgba,
        } = preview;
        let held = Held {
            key: decode.key,
            handle: Handle::from_rgba(width, height, rgba),
            side: decode.side,
            bytes,
        };
        match entry.held.replace(held) {
            Some(old) => self.bytes -= old.bytes,
            None => self.handles += 1,
        }
        self.bytes += bytes;
    }

    /// Evict until `incoming` bytes fit — replacing `replaced` bytes of `file`'s own — within the
    /// budget and the handle count: the least recently wanted first, never a cell on screen, and a
    /// margin cell only for a cell on screen. Evicts nothing and answers `false` when they cannot
    /// fit so.
    fn make_room(&mut self, file: FileId, incoming: usize, replaced: Option<usize>) -> bool {
        let mut bytes = self.bytes - replaced.unwrap_or(0) + incoming;
        let mut handles = self.handles + usize::from(replaced.is_none());
        let over = |bytes: usize, handles: usize| bytes > self.budget || handles > MAX_HANDLES;
        if !over(bytes, handles) {
            return true;
        }
        let for_screen = self.on_screen.contains(&file);
        let mut candidates: Vec<(u64, FileId, usize)> = self
            .entries
            .iter()
            .filter(|(held_file, entry)| {
                **held_file != file
                    && !self.on_screen.contains(held_file)
                    && (for_screen || entry.wanted_at != self.tick)
            })
            .filter_map(|(held_file, entry)| {
                Some((entry.wanted_at, *held_file, entry.held.as_ref()?.bytes))
            })
            .collect();
        candidates.sort_unstable();
        let mut evict = 0;
        for (_, _, held) in &candidates {
            if !over(bytes, handles) {
                break;
            }
            bytes -= held;
            handles -= 1;
            evict += 1;
        }
        if over(bytes, handles) {
            return false;
        }
        for (_, evicted, _) in &candidates[..evict] {
            self.evict(*evicted);
        }
        true
    }

    /// Drop `file`'s handle, and its entry when nothing wants it.
    fn evict(&mut self, file: FileId) {
        let Some(entry) = self.entries.get_mut(&file) else {
            return;
        };
        if let Some(held) = entry.held.take() {
            self.bytes -= held.bytes;
            self.handles -= 1;
        }
        if entry.wanted_at != self.tick {
            self.entries.remove(&file);
        }
    }

    /// Forget what nothing wants and holds nothing, send the next reads and hand the worker the
    /// decodes wanted now.
    fn plan(&mut self) -> Option<ReadBatch> {
        let tick = self.tick;
        self.entries
            .retain(|_, entry| entry.wanted_at == tick || entry.held.is_some());
        let batch = self.plan_reads();
        self.plan_decodes();
        batch
    }

    /// The next batch: the wanted files not asked under this revision, or woken while queued, cells
    /// on screen first; none while one is in flight.
    fn plan_reads(&mut self) -> Option<ReadBatch> {
        if self.reading.is_some() {
            return None;
        }
        let mut visible = Vec::new();
        let mut margin = Vec::new();
        let wanted = self
            .wanted
            .visible
            .iter()
            .map(|file| (file, true))
            .chain(self.wanted.margin.iter().map(|file| (file, false)));
        for ((file, state), on_screen) in wanted {
            if visible.len() + margin.len() >= READ_BATCH {
                break;
            }
            // The row says there is nothing to draw; the grid shows it Unreadable.
            if *state == PreviewState::Unavailable {
                continue;
            }
            let Some(entry) = self.entries.get_mut(file) else {
                continue;
            };
            if !matches!(entry.read, Read::Unasked | Read::Recheck) {
                continue;
            }
            entry.read = Read::Asking;
            if on_screen {
                visible.push(*file);
            } else {
                margin.push(*file);
            }
        }
        if visible.is_empty() && margin.is_empty() {
            return None;
        }
        self.serial += 1;
        self.reading = Some(self.serial);
        // The lane hands out the newest visible task first: the screen's last cell is asked first,
        // so its first is read first.
        let reads = visible
            .into_iter()
            .rev()
            .map(|file| (file, PreviewPriority::Visible))
            .chain(
                margin
                    .into_iter()
                    .map(|file| (file, PreviewPriority::Background)),
            )
            .collect();
        Some(ReadBatch {
            serial: self.serial,
            revision: self.wanted.revision,
            reads,
        })
    }

    /// Hand the worker every wanted file whose answer names a preview not held at the size its cell
    /// needs, cells on screen first. The running plan goes on when it already covers them all.
    fn plan_decodes(&mut self) {
        let mut plan = Vec::new();
        for (file, _) in self.wanted.visible.iter().chain(&self.wanted.margin) {
            if plan.len() >= MAX_PLAN {
                break;
            }
            let Some(entry) = self.entries.get(file) else {
                continue;
            };
            let Some(source) = &entry.source else {
                continue;
            };
            if entry.failed.as_ref() == Some(&source.key)
                || entry.over_budget.as_ref() == Some(&source.key)
            {
                continue;
            }
            let side = self.wanted.side.min(source.long_edge).max(1);
            if entry
                .held
                .as_ref()
                .is_some_and(|held| held.key == source.key && held.side >= side)
            {
                continue;
            }
            plan.push(Decode {
                file: *file,
                key: source.key.clone(),
                path: source.path.clone(),
                side,
            });
        }
        #[cfg(test)]
        {
            self.last_plan = plan.clone();
        }
        let busy = self
            .decoder
            .as_ref()
            .is_some_and(|decoder| decoder.worker.is_busy());
        if plan.is_empty() {
            if busy && let Some(decoder) = &mut self.decoder {
                decoder.worker.cancel();
            }
            self.planned.clear();
            return;
        }
        if busy && plan.iter().all(|decode| self.planned.contains(decode)) {
            return;
        }
        self.planned = plan.iter().cloned().collect();
        #[cfg(test)]
        if self.paused {
            return;
        }
        let decoder = self.decoder.get_or_insert_with(DecodeWorker::start);
        let _ = decoder.worker.request(Plan(plan));
    }
}

/// The one read batch as an owner task.
fn read_task(
    owner: &OwnerHandle,
    client: ClientId,
    batch: Option<ReadBatch>,
) -> Task<SelectPreviewMessage> {
    let Some(batch) = batch else {
        return Task::none();
    };
    let owner = owner.clone();
    owner_work(move || read(&owner, client, batch)).map(SelectPreviewMessage::Read)
}

/// What the grid's cells borrow: each cell's handle, by view position, through the rows read so
/// far. Plain lookups, so building a cell costs no allocation and no handle is made in `view()`.
#[derive(Clone, Copy)]
pub(crate) struct GridImages<'a> {
    previews: &'a SelectPreviews,
    rows: &'a RowCache,
}

impl<'a> GridImages<'a> {
    fn file(&self, item: u32) -> Option<FileId> {
        match self.rows.row(item)?.item {
            RowItem::File { file_id } => Some(file_id),
            RowItem::Photo { .. } => None,
        }
    }

    /// The handle to draw for the cell at view position `item`.
    pub(crate) fn image(&self, item: u32) -> Option<&'a Handle> {
        let previews: &'a SelectPreviews = self.previews;
        previews.handle(self.file(item)?)
    }

    /// The cell at `item` is Unreadable: the owner found no usable preview in its file.
    pub(crate) fn unreadable(&self, item: u32) -> bool {
        self.file(item)
            .is_some_and(|file| self.previews.unreadable(file))
    }
}

#[cfg(test)]
#[path = "select_previews_tests.rs"]
mod tests;
