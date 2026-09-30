//! The loupe's decoded frames ([catalog design](../../../../docs/design/catalog.md#browsing-at-speed),
//! "Look-ahead"): the active frame, compare's frames and the look-ahead, each read through
//! `preview.read` at the loupe's tier, decoded off the update loop at the size the screen needs, and
//! held as one image handle, made once and lent to the view while the frame may be shown.
//!
//! - **What is wanted.** After every message the loupe hands over what it wants, most wanted
//!   first ([`Want`], from `state::loupe::wanted`): the active frame and compare's frames, which are
//!   on screen, then the look-ahead in the direction of travel and the next moment's first frame.
//! - **Reads.** `preview.read {item, tier, priority: look-ahead}` — a file's `loupe` tier, a
//!   developed photograph's `large` one — for every wanted frame not yet asked, most wanted first,
//!   at most [`READ_BATCH`] in one owner task and one task at a time. The lane hands its look-ahead
//!   out oldest first, so the frame asked first is read first. `ready` names the tier; `queued`
//!   names the job making it and the best preview cached meanwhile (a file's grid tier or
//!   thumbnail, a photograph's camera preview), which is drawn, and said to be a stand-in, until
//!   the tier replaces it. A frame that leaves the wanted frames while its read is still queued
//!   has its job cancelled with the next batch (`job.cancel`), so a held arrow never leaves the
//!   lane working through frames already passed ahead of the one on screen.
//! - **Waking.** The owner wakes this client when a preview it waits on is written or its task
//!   ends; the Select grid registers the one wake a client has
//!   ([`OwnerHandle::watch_previews`](luxforge_core::OwnerHandle::watch_previews)) and passes it on
//!   to the loupe ([`super::loupe::owner_woke`]). On the update loop the frames still queued are
//!   read again. Nothing polls.
//! - **Decoding.** One [`Latest`] worker, started with the first decode and blocked while idle,
//!   takes the newest plan — the wanted frames whose answer names a preview not held at the size
//!   they need, most wanted first, whose decoded bytes together fit the budget, at most
//!   [`MAX_PLAN`] — and decodes them one by one through [`decode_preview`], handing each over as it
//!   lands. A newer plan starts after the decode running now when that decode is still wanted; when
//!   it is not, the running decode is abandoned at its next strip. So a held arrow never queues more
//!   decodes than the budget holds, and a frame passed is not decoded.
//! - **Handles.** A decoded frame becomes its handle here, once, on the update loop, while it is
//!   still the preview the frame's newest answer names (its `key`): with the Select grid's, one of
//!   the desktop's two homes of `Handle::from_rgba`. The 100% region's handle is made here too
//!   ([`region_handle`]). A better stage — the tier after its stand-in — replaces the held one when
//!   it lands, so the frame never goes blank between the two.
//! - **Budget.** A held frame is charged its RGBA bytes against [`DECODED_BUDGET_BYTES`], and at
//!   most [`MAX_HANDLES`] are held. Past either, the least recently wanted go first; a frame on
//!   screen is never evicted, and one the look-ahead still wants only for one on screen. A decoded
//!   frame that cannot fit so is dropped, and not decoded again until the wanted frames change.
//! - **Releasing.** [`LoupeFrames::release`] forgets everything when the loupe closes or Select is
//!   left, cancelling the reads still queued; dropping the cache ends the worker.
use crate::app::tasks::call_detailed;
use crate::state::loupe::{Held as HeldFrame, Picture};
use iced::widget::image::Handle;
use luxforge_core::{
    ClientId, DecodedPreview, ErrorKind, OwnerHandle,
    catalog_types::{
        PreviewAnswer, PreviewInfo, PreviewItem, PreviewOrigin, PreviewPriority, PreviewTier,
    },
    decode_preview,
    jobs::JOB_CANCEL,
    latest::{Latest, Running},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

/// The decoded loupe frames the desktop holds, in RGBA8 bytes: the catalog design's provisional
/// desktop budget (Performance, "Memory"). A frame fitted to a 2,600 × 1,500-pixel loupe on the
/// M4's display is about 13 MiB, so it holds the frame on screen, the look-ahead and a dozen frames
/// behind.
pub(crate) const DECODED_BUDGET_BYTES: usize = 256 << 20;

/// The most frames held, whatever their size.
pub(crate) const MAX_HANDLES: usize = 48;

/// `preview.read`s in one owner task: the frame on screen, compare's and the look-ahead.
pub(crate) const READ_BATCH: usize = 8;

/// The decodes one plan holds: compare's four frames and the look-ahead's four, and room.
pub(crate) const MAX_PLAN: usize = 12;

/// Decoded frames waiting for the update loop; the worker waits for room past it. Each is at most
/// the budget.
const DECODED_WAITING: usize = 2;

/// What this module's owner tasks bring back to the update loop, carried in the loupe's message.
#[derive(Clone, Debug)]
pub(crate) enum LoupeFramesMessage {
    Read(ReadAnswers),
}

/// One owner task: the jobs of frames no longer wanted to cancel, then the reads.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReadBatch {
    pub(crate) serial: u64,
    pub(crate) cancel: Vec<String>,
    pub(crate) reads: Vec<PreviewItem>,
}

/// What the owner answered one [`ReadBatch`], frame by frame.
#[derive(Clone, Debug)]
pub(crate) struct ReadAnswers {
    pub(crate) serial: u64,
    pub(crate) answers: Vec<(PreviewItem, Result<PreviewAnswer, Refusal>)>,
}

/// Why the owner refused a frame's preview: its error code and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) code: String,
    pub(crate) message: String,
}

/// One frame the loupe wants: its item, the pixels the area it is drawn in needs, and whether it
/// is on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Want {
    pub(crate) item: PreviewItem,
    pub(crate) pixels: (u32, u32),
    pub(crate) shown: bool,
}

/// The tier the loupe reads for an item: a file's largest embedded preview, a developed
/// photograph's large render.
pub(crate) fn tier(item: &PreviewItem) -> PreviewTier {
    match item {
        PreviewItem::File { .. } => PreviewTier::Loupe,
        PreviewItem::Photo { .. } => PreviewTier::Large,
    }
}

/// Whether a preview carries an approximation it must be labelled with.
///
/// HOOK (lane B, in flight): `PreviewInfo.approximate` is being added to the core beside this
/// task. Until it is on this branch no preview says it is approximate; once it is, this reads it.
fn approximate(_info: &PreviewInfo) -> bool {
    false
}

/// `side` fitted to `pixels`: the longest side a `width` × `height` preview is decoded at to fill
/// the area without being enlarged.
fn side(width: u32, height: u32, pixels: (u32, u32)) -> u32 {
    let (w, h) = (width.max(1) as f64, height.max(1) as f64);
    let scale = (pixels.0 as f64 / w).min(pixels.1 as f64 / h).min(1.0);
    (w.max(h) * scale).ceil().max(1.0) as u32
}

/// The RGBA bytes a decode fitted within `side` of a `width` × `height` preview holds, rounded up.
fn estimate(width: u32, height: u32, side: u32) -> usize {
    let long = width.max(height).max(1) as f64;
    let scale = (side as f64 / long).min(1.0);
    let fw = (width as f64 * scale).ceil() as usize;
    let fh = (height as f64 * scale).ceil() as usize;
    fw.max(1) * fh.max(1) * 4
}

/// Where a frame's read stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Read {
    Unasked,
    Asking,
    Ready,
    Queued,
    /// The owner woke this client while it was queued: asked again at the next plan.
    Recheck,
    Refused(Refusal),
}

/// The preview a frame's newest answer names.
#[derive(Clone, Debug)]
struct Source {
    key: String,
    path: PathBuf,
    width: u32,
    height: u32,
    origin: PreviewOrigin,
    stand_in: bool,
    approximate: bool,
}

impl Source {
    fn of(info: PreviewInfo, stand_in: bool) -> Self {
        Self {
            approximate: approximate(&info),
            key: info.key,
            path: info.path,
            width: info.width.max(1),
            height: info.height.max(1),
            origin: info.origin,
            stand_in,
        }
    }
}

/// A decoded frame's handle, made once, and what it is.
struct Held {
    handle: Handle,
    picture: Picture,
    /// The side it was decoded to fit.
    side: u32,
    bytes: usize,
}

/// One frame's reads, its preview and its handle.
struct Entry {
    read: Read,
    source: Option<Source>,
    held: Option<Held>,
    /// A key whose decode failed: not decoded again until an answer names another.
    failed: Option<String>,
    /// A key whose decode did not fit the budget: not decoded again until the wanted frames change.
    over_budget: Option<String>,
    /// The last plan that wanted it; the current one is [`LoupeFrames::tick`].
    wanted_at: u64,
    /// The job the lane answered it `queued` with: cancelled when the frame is no longer wanted,
    /// and a later answer with another job means the lane ended this one without the tier.
    job: Option<String>,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            read: Read::Unasked,
            source: None,
            held: None,
            failed: None,
            over_budget: None,
            wanted_at: 0,
            job: None,
        }
    }
}

/// One decode: the preview an answer named, fitted within `side`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Decode {
    pub(crate) item: PreviewItem,
    pub(crate) key: String,
    pub(crate) path: PathBuf,
    pub(crate) side: u32,
    /// Its decoded bytes at most.
    pub(crate) bytes: usize,
}

/// One decode's pixels, or why it failed.
pub(crate) struct Decoded {
    pub(crate) decode: Decode,
    pub(crate) result: Result<DecodedPreview, String>,
}

/// The decodes wanted now, most wanted first, under the plan's number.
struct Plan {
    number: u64,
    decodes: Vec<Decode>,
}

/// The decode worker: the newest plan, one decode at a time, each handed over as it lands.
struct DecodeWorker {
    worker: Latest<Plan, ()>,
    decoded: mpsc::Receiver<Decoded>,
    /// The newest plan's number: a plan that is no longer the newest stops before its next decode.
    newest: Arc<AtomicU64>,
    /// The decode running now, if one is.
    running: Arc<Mutex<Option<Decode>>>,
}

impl DecodeWorker {
    fn start() -> Self {
        let (sender, decoded) = mpsc::sync_channel(DECODED_WAITING);
        let newest = Arc::new(AtomicU64::new(0));
        let running = Arc::new(Mutex::new(None::<Decode>));
        let (plan_newest, now) = (newest.clone(), running.clone());
        // The decode the last plan finished, which a newer plan that still wants it skips: a plan
        // requested while it ran already counted on it.
        let mut finished: Option<Decode> = None;
        let worker = Latest::new(
            "luxforge-loupe-frames",
            move |plan: Plan, job: &Running<'_, Plan, ()>| {
                for decode in plan.decodes {
                    if plan_newest.load(Ordering::Acquire) != plan.number {
                        return None;
                    }
                    if finished.as_ref() == Some(&decode) {
                        continue;
                    }
                    *now.lock().unwrap_or_else(PoisonError::into_inner) = Some(decode.clone());
                    let result = decode_preview(&decode.path, decode.side, job.abandoned());
                    *now.lock().unwrap_or_else(PoisonError::into_inner) = None;
                    let result = match result {
                        // Nothing wants it any more.
                        Err(error) if error.kind == ErrorKind::Cancelled => return None,
                        result => result.map_err(|error| error.to_string()),
                    };
                    finished = Some(decode.clone());
                    // Waits while `DECODED_WAITING` wait; fails once the cache is gone.
                    if sender.send(Decoded { decode, result }).is_err() {
                        return None;
                    }
                    super::loupe::post();
                }
                None
            },
        );
        Self {
            worker,
            decoded,
            newest,
            running,
        }
    }

    fn running(&self) -> Option<Decode> {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The loupe's decoded frames: what each wanted frame's answer names, the handles held under the
/// byte budget, the one read batch in flight and the decode worker. See the
/// [module documentation](self).
pub(crate) struct LoupeFrames {
    entries: HashMap<PreviewItem, Entry>,
    wanted: Vec<Want>,
    /// The frames on screen, never evicted.
    shown: HashSet<PreviewItem>,
    /// Raised by every change of what is wanted; an entry wanted now was stamped with it.
    tick: u64,
    budget: usize,
    bytes: usize,
    handles: usize,
    /// The read batch in flight: one owner task at a time.
    reading: Option<u64>,
    serial: u64,
    /// The owner woke this client while a batch was in flight: what it answers `queued` may already
    /// be written, so it is read again at once.
    woken_while_reading: bool,
    /// Jobs of frames no longer wanted, cancelled with the next batch.
    cancel: Vec<String>,
    decoder: Option<DecodeWorker>,
    /// The decodes last handed to the worker, and its number.
    planned: Vec<Decode>,
    plan_number: u64,
    /// Decoded frames dropped because they could not fit, for evidence.
    dropped: u64,
    /// Tests plan decodes without starting the worker, and hand the decodes in themselves.
    #[cfg(test)]
    pub(crate) paused: bool,
}

impl Default for LoupeFrames {
    fn default() -> Self {
        Self::with_budget(DECODED_BUDGET_BYTES)
    }
}

impl fmt::Debug for LoupeFrames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoupeFrames")
            .field("entries", &self.entries.len())
            .field("handles", &self.handles)
            .field("bytes", &self.bytes)
            .field("reading", &self.reading)
            .finish_non_exhaustive()
    }
}

/// `job.cancel` of each of `batch`'s jobs, then `preview.read` of each frame at the look-ahead's
/// priority, as the API reads them, answered frame by frame. A cancel that fails (the job ended
/// meanwhile) changes nothing.
pub(crate) fn read(owner: &OwnerHandle, client: ClientId, batch: ReadBatch) -> ReadAnswers {
    for job in batch.cancel {
        let _ = call_detailed(owner, client, JOB_CANCEL, json!({ "job_id": job }));
    }
    let answers = batch
        .reads
        .into_iter()
        .map(|item| {
            let params = json!({
                "item": item,
                "tier": tier(&item),
                "priority": PreviewPriority::LookAhead,
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
            (item, answer)
        })
        .collect();
    ReadAnswers {
        serial: batch.serial,
        answers,
    }
}

/// A 100% region's pixels as the handle the inset draws, made once when the region lands: the
/// loupe's pictures are all made in this module.
pub(crate) fn region_handle(decoded: DecodedPreview) -> Handle {
    let DecodedPreview {
        width,
        height,
        rgba,
    } = decoded;
    Handle::from_rgba(width, height, rgba)
}

impl LoupeFrames {
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            wanted: Vec::new(),
            shown: HashSet::new(),
            tick: 0,
            budget,
            bytes: 0,
            handles: 0,
            reading: None,
            serial: 0,
            woken_while_reading: false,
            cancel: Vec::new(),
            decoder: None,
            planned: Vec::new(),
            plan_number: 0,
            dropped: 0,
            #[cfg(test)]
            paused: false,
        }
    }

    /// Take in what the loupe wants now: the read batch to send, if any, and the decodes.
    pub(crate) fn want(&mut self, wanted: Vec<Want>) -> Option<ReadBatch> {
        self.drain();
        if wanted != self.wanted {
            self.tick += 1;
            let tick = self.tick;
            for (item, entry) in &mut self.entries {
                entry.over_budget = None;
                let still = wanted.iter().any(|want| &want.item == item);
                // Its read waits in the lane's queue for a frame the loupe has left: cancelled, so
                // the frames wanted now are read first.
                if !still && matches!(entry.read, Read::Queued | Read::Recheck) {
                    self.cancel.extend(entry.job.take());
                    entry.read = Read::Unasked;
                }
            }
            for want in &wanted {
                self.entries.entry(want.item.clone()).or_default().wanted_at = tick;
            }
            self.shown = wanted
                .iter()
                .filter(|want| want.shown)
                .map(|want| want.item.clone())
                .collect();
            self.wanted = wanted;
        }
        self.plan()
    }

    /// The owner answered a batch: take each answer of a frame still asked, then plan.
    pub(crate) fn answered(&mut self, answers: ReadAnswers) -> Option<ReadBatch> {
        if self.reading == Some(answers.serial) {
            self.reading = None;
        }
        let queued = if std::mem::take(&mut self.woken_while_reading) {
            Read::Recheck
        } else {
            Read::Queued
        };
        for (item, answer) in answers.answers {
            let Some(entry) = self.entries.get_mut(&item) else {
                continue;
            };
            if entry.read != Read::Asking {
                continue;
            }
            let (read, source) = match answer {
                Ok(PreviewAnswer::Ready { preview }) => {
                    entry.job = None;
                    (Read::Ready, Some(Source::of(preview, false)))
                }
                Ok(PreviewAnswer::Queued { job_id, fallback }) => {
                    let job = job_id.as_str().to_owned();
                    let ended = entry.job.as_ref().is_some_and(|earlier| *earlier != job);
                    entry.job = Some(job);
                    let fallback = fallback.map(|info| Source::of(info, true));
                    if ended {
                        let refusal = Refusal {
                            code: "not-read".into(),
                            message: "the preview lane ended its read of this frame without it"
                                .into(),
                        };
                        (Read::Refused(refusal), fallback)
                    } else {
                        (queued.clone(), fallback)
                    }
                }
                // The lane's queue is full: asked again once it has written something.
                Err(refusal) if refusal.code == "resource-limit" => {
                    entry.read = queued.clone();
                    continue;
                }
                Err(refusal) => (Read::Refused(refusal), None),
            };
            entry.read = read;
            match source {
                // The held picture stays until the one named is decoded.
                Some(source) => entry.source = Some(source),
                // Nothing valid is cached for the frame: what is held is not its picture.
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

    /// The loupe's signal: take what the worker decoded and, when the owner woke this client, read
    /// again the frames still queued. Then plan.
    pub(crate) fn woken(&mut self, owner: bool) -> Option<ReadBatch> {
        if owner {
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

    /// Forget every handle, answer and plan: the loupe closed or Select was left. The worker drops
    /// its plan and blocks idle; the reads still queued are cancelled by the batch this returns.
    pub(crate) fn release(&mut self) -> Option<ReadBatch> {
        if let Some(decoder) = &mut self.decoder {
            decoder.newest.store(u64::MAX, Ordering::Release);
            decoder.worker.cancel();
            while decoder.decoded.try_recv().is_ok() {}
        }
        for entry in self.entries.values_mut() {
            if matches!(entry.read, Read::Queued | Read::Recheck) {
                self.cancel.extend(entry.job.take());
            }
        }
        self.entries.clear();
        self.wanted.clear();
        self.shown.clear();
        self.tick += 1;
        self.bytes = 0;
        self.handles = 0;
        self.planned.clear();
        self.woken_while_reading = false;
        self.plan_reads()
    }

    /// The handle held for `picture`: the frame's own, at exactly that preview.
    pub(crate) fn handle(&self, picture: &Picture) -> Option<&Handle> {
        let held = self.entries.get(&picture.item)?.held.as_ref()?;
        (held.picture.key == picture.key).then_some(&held.handle)
    }

    /// What is held for `item`, as the model reads it.
    pub(crate) fn held(&self, item: &PreviewItem) -> HeldFrame {
        let entry = self.entries.get(item);
        HeldFrame {
            item: item.clone(),
            picture: entry
                .and_then(|entry| entry.held.as_ref())
                .map(|held| held.picture.clone()),
            unavailable: entry.and_then(|entry| match &entry.read {
                Read::Refused(refusal) if entry.source.is_none() => Some(refusal.code.clone()),
                _ => None,
            }),
        }
    }

    /// The frames on screen of `wanted` — what the loupe wants now, which may be newer than what
    /// it last handed over — have what they will draw: each the tier decoded at the size it needs,
    /// or nothing more to wait for (refused, or its decode failed or could not fit), with no read in
    /// flight. An evidence run waits for it before a capture.
    pub(crate) fn settled(&self, wanted: &[Want]) -> bool {
        self.reading.is_none()
            && wanted
                .iter()
                .filter(|want| want.shown)
                .all(|want| self.frame_settled(want))
    }

    fn frame_settled(&self, want: &Want) -> bool {
        let Some(entry) = self.entries.get(&want.item) else {
            return false;
        };
        match &entry.read {
            Read::Unasked | Read::Asking | Read::Queued | Read::Recheck => false,
            Read::Refused(_) | Read::Ready => match &entry.source {
                None => true,
                Some(source) => {
                    entry.failed.as_ref() == Some(&source.key)
                        || entry.over_budget.as_ref() == Some(&source.key)
                        || entry.held.as_ref().is_some_and(|held| {
                            held.picture.key == source.key
                                && held.side >= side(source.width, source.height, want.pixels)
                        })
                }
            },
        }
    }

    /// How many look-ahead frames are wanted, and how many are decoded.
    pub(crate) fn ahead(&self) -> (u32, u32) {
        let ahead: Vec<&Want> = self.wanted.iter().filter(|want| !want.shown).collect();
        let ready = ahead
            .iter()
            .filter(|want| {
                self.entries.get(&want.item).is_some_and(|entry| {
                    entry
                        .held
                        .as_ref()
                        .is_some_and(|held| !held.picture.stand_in)
                })
            })
            .count();
        (ahead.len() as u32, ready as u32)
    }

    /// What the cache holds, for correlated evidence.
    pub(crate) fn summary(&self) -> Value {
        let (ahead, ready) = self.ahead();
        json!({
            "handles": self.handles,
            "bytes": self.bytes,
            "budget": self.budget,
            "wanted": self.wanted.len(),
            "shown": self.shown.len(),
            "ahead": ahead,
            "ahead_ready": ready,
            "reading": self.reading.is_some(),
            "planned": self.planned.len(),
            "plan_bytes": self.plan_bytes(),
            "decoding": self.decoder.as_ref().is_some_and(|decoder| decoder.worker.is_busy()),
            "dropped": self.dropped,
            "settled": self.settled(&self.wanted),
        })
    }

    /// The bytes the plan handed to the worker decodes at most.
    pub(crate) fn plan_bytes(&self) -> usize {
        self.planned.iter().map(|decode| decode.bytes).sum()
    }

    /// Adopt every decoded frame waiting.
    fn drain(&mut self) {
        let Some(decoder) = &self.decoder else {
            return;
        };
        let landed: Vec<Decoded> = decoder.decoded.try_iter().collect();
        for decoded in landed {
            self.adopt(decoded);
        }
    }

    /// Make the handle of a decoded frame that is still the preview its frame's newest answer
    /// names, charging its bytes and making room for them; drop anything else.
    pub(crate) fn adopt(&mut self, decoded: Decoded) {
        let Decoded { decode, result } = decoded;
        let Some(entry) = self.entries.get_mut(&decode.item) else {
            return;
        };
        // An older stage, or the preview of a frame that changed since: a newer answer replaced it.
        let Some(source) = entry
            .source
            .as_ref()
            .filter(|source| source.key == decode.key)
        else {
            return;
        };
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
            .is_some_and(|held| held.picture.key == decode.key && held.side >= decode.side)
        {
            return;
        }
        let picture = Picture {
            item: decode.item.clone(),
            key: decode.key.clone(),
            origin: source.origin,
            width: source.width,
            height: source.height,
            stand_in: source.stand_in,
            approximate: source.approximate,
        };
        let replaced = entry.held.as_ref().map(|held| held.bytes);
        let bytes = preview.rgba.len();
        if !self.make_room(&decode.item, bytes, replaced) {
            self.dropped += 1;
            if let Some(entry) = self.entries.get_mut(&decode.item) {
                entry.over_budget = Some(decode.key);
            }
            return;
        }
        let Some(entry) = self.entries.get_mut(&decode.item) else {
            return;
        };
        let DecodedPreview {
            width,
            height,
            rgba,
        } = preview;
        let held = Held {
            handle: Handle::from_rgba(width, height, rgba),
            picture,
            side: decode.side,
            bytes,
        };
        match entry.held.replace(held) {
            Some(old) => self.bytes -= old.bytes,
            None => self.handles += 1,
        }
        self.bytes += bytes;
    }

    /// Evict until `incoming` bytes fit — replacing `replaced` bytes of `item`'s own — within the
    /// budget and the handle count: the least recently wanted first, never a frame on screen, and a
    /// frame still wanted only for one on screen. Evicts nothing and answers `false` when they
    /// cannot fit so.
    fn make_room(&mut self, item: &PreviewItem, incoming: usize, replaced: Option<usize>) -> bool {
        let mut bytes = self.bytes - replaced.unwrap_or(0) + incoming;
        let mut handles = self.handles + usize::from(replaced.is_none());
        let over = |bytes: usize, handles: usize| bytes > self.budget || handles > MAX_HANDLES;
        if !over(bytes, handles) {
            return true;
        }
        let for_screen = self.shown.contains(item);
        let mut candidates: Vec<(u64, PreviewItem, usize)> = self
            .entries
            .iter()
            .filter(|(held, entry)| {
                *held != item
                    && !self.shown.contains(*held)
                    && (for_screen || entry.wanted_at != self.tick)
            })
            .filter_map(|(held, entry)| {
                Some((entry.wanted_at, held.clone(), entry.held.as_ref()?.bytes))
            })
            .collect();
        candidates.sort_unstable_by(|a, b| a.0.cmp(&b.0));
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
            self.evict(evicted);
        }
        true
    }

    /// Drop `item`'s handle, and its entry when nothing wants it.
    fn evict(&mut self, item: &PreviewItem) {
        let Some(entry) = self.entries.get_mut(item) else {
            return;
        };
        if let Some(held) = entry.held.take() {
            self.bytes -= held.bytes;
            self.handles -= 1;
        }
        if entry.wanted_at != self.tick && entry.job.is_none() {
            self.entries.remove(item);
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

    /// The next batch: the cancels waiting, and the wanted frames not asked yet or woken while
    /// queued, most wanted first; none while one is in flight.
    fn plan_reads(&mut self) -> Option<ReadBatch> {
        if self.reading.is_some() {
            return None;
        }
        let mut reads = Vec::new();
        for want in &self.wanted {
            if reads.len() >= READ_BATCH {
                break;
            }
            let Some(entry) = self.entries.get_mut(&want.item) else {
                continue;
            };
            if matches!(entry.read, Read::Unasked | Read::Recheck) {
                entry.read = Read::Asking;
                reads.push(want.item.clone());
            }
        }
        if reads.is_empty() && self.cancel.is_empty() {
            return None;
        }
        self.serial += 1;
        self.reading = Some(self.serial);
        Some(ReadBatch {
            serial: self.serial,
            cancel: std::mem::take(&mut self.cancel),
            reads,
        })
    }

    /// The decodes wanted now: each wanted frame whose answer names a preview not held at the size
    /// it needs, most wanted first, while their bytes together fit the budget.
    fn decodes(&self) -> Vec<Decode> {
        let mut plan = Vec::new();
        let mut bytes = 0;
        for want in &self.wanted {
            if plan.len() >= MAX_PLAN {
                break;
            }
            let Some(entry) = self.entries.get(&want.item) else {
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
            let side = side(source.width, source.height, want.pixels);
            if entry
                .held
                .as_ref()
                .is_some_and(|held| held.picture.key == source.key && held.side >= side)
            {
                continue;
            }
            let size = estimate(source.width, source.height, side);
            if bytes + size > self.budget {
                break;
            }
            bytes += size;
            plan.push(Decode {
                item: want.item.clone(),
                key: source.key.clone(),
                path: source.path.clone(),
                side,
                bytes: size,
            });
        }
        plan
    }

    /// Hand the worker the decodes wanted now. The running plan goes on when it already covers them
    /// all; a newer plan starts after the running decode when that decode is still wanted, and the
    /// running decode is abandoned when it is not.
    fn plan_decodes(&mut self) {
        let plan = self.decodes();
        if plan == self.planned {
            return;
        }
        let busy = self
            .decoder
            .as_ref()
            .is_some_and(|decoder| decoder.worker.is_busy());
        if busy && !plan.is_empty() && plan.iter().all(|decode| self.planned.contains(decode)) {
            // Only shrank: the running plan decodes the rest, whose landing is still wanted.
            return;
        }
        self.planned = plan.clone();
        #[cfg(test)]
        if self.paused {
            return;
        }
        if plan.is_empty() {
            if busy && let Some(decoder) = &mut self.decoder {
                decoder.newest.store(u64::MAX, Ordering::Release);
                decoder.worker.cancel();
            }
            return;
        }
        let decoder = self.decoder.get_or_insert_with(DecodeWorker::start);
        self.plan_number += 1;
        let running = decoder.running();
        decoder.newest.store(self.plan_number, Ordering::Release);
        if running.is_some_and(|running| !plan.contains(&running)) {
            decoder.worker.cancel();
        }
        let _ = decoder.worker.request(Plan {
            number: self.plan_number,
            decodes: plan,
        });
    }

    /// The plan the worker was last handed, for tests.
    #[cfg(test)]
    pub(crate) fn planned(&self) -> &[Decode] {
        &self.planned
    }

    /// The bytes held and the handles, for tests.
    #[cfg(test)]
    pub(crate) fn held_bytes(&self) -> (usize, usize) {
        (self.bytes, self.handles)
    }
}

#[cfg(test)]
#[path = "loupe_frames_tests.rs"]
mod tests;
