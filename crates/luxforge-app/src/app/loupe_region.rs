//! The 100% focus check's region ([catalog design](../../../../docs/design/catalog.md#the-100-region)):
//! the rectangle under the pointer at full resolution, cut by `preview.region` on the preview
//! lane's region worker, decoded off the update loop and drawn in the loupe's inset.
//!
//! - **One in flight, the newest waiting.** The pointer moves faster than regions are cut, so the
//!   region follows the app's one coalescing slot ([`Coalesce`]): at most one region is out — from
//!   `preview.region` through its job's end to its answer decoded — and of the rectangles asked for
//!   meanwhile only the newest waits. The owner cancels a client's previous region when it asks for
//!   the next (latest wins), and a region's answer file is valid until this client's next region is
//!   written; the slot never asks for the next before the last is decoded, so it never reads a
//!   file the lane has removed.
//! - **Waking.** The lane wakes this client when its latest region ends, through the one previews
//!   wake the loupe shares ([`super::loupe::owner_woke`]); only then is the job read (`job.read`),
//!   and a wake that arrives before the job's number, or during a read, is kept for when it can be
//!   used. Nothing polls.
//! - **Identity.** A region names the item it was cut from; the loupe draws it only under that
//!   frame ([`crate::state::loupe::Region`]). Its handle is made once when it lands, in
//!   `loupe_frames.rs`.
use crate::app::{
    loupe_frames::region_handle,
    select::job_now,
    tasks::{call, owner_work},
};
use crate::coalesce::Coalesce;
use crate::state::loupe::Region;
use iced::{Task, widget::image::Handle};
use luxforge_core::{
    Cancel, ClientId, DecodedPreview, OwnerHandle,
    catalog_types::{Dimensions, PixelRect, PreviewItem, RegionAnswer},
    decode_preview,
};
use serde_json::{Value, json};

/// One rectangle of a frame at 100%, as `preview.region` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegionRequest {
    pub(crate) item: PreviewItem,
    pub(crate) rect: PixelRect,
    /// The upright frame `rect` is in.
    pub(crate) frame: Dimensions,
}

/// What this module's owner tasks bring back, carried in the loupe's message.
#[derive(Clone, Debug)]
pub(crate) enum RegionMessage {
    /// `preview.region` answered request `serial` with its job, or refused it.
    Started {
        serial: u64,
        result: Result<String, String>,
    },
    /// `job.read` of request `serial`'s job answered.
    Read {
        serial: u64,
        result: Result<Box<RegionRead>, String>,
    },
}

/// A region job's state as `job.read` found it: still being cut, or ended with its region decoded
/// or the reason it failed.
#[derive(Clone, Debug)]
pub(crate) enum RegionRead {
    Running,
    Ended(Result<(RegionAnswer, DecodedPreview), String>),
}

/// The region out: its number and request, its job once the owner has answered, a `job.read` in
/// flight, and a wake that arrived before either could use it.
#[derive(Debug)]
struct Out {
    serial: u64,
    request: RegionRequest,
    job: Option<String>,
    reading: bool,
    woken: bool,
}

/// The region the inset draws: what it is and its handle.
struct Shown {
    region: Region,
    handle: Handle,
}

/// The focus check's regions. See the [module documentation](self).
#[derive(Default)]
pub(crate) struct FocusCheck {
    slot: Coalesce<RegionRequest>,
    out: Option<Out>,
    serial: u64,
    /// The rectangle last offered, so an unchanged pointer asks for nothing.
    last: Option<RegionRequest>,
    shown: Option<Shown>,
    /// The last region failed, for this item.
    error: Option<(PreviewItem, String)>,
    /// The frame the last region was cut from, for its item.
    frame_of: Option<(PreviewItem, Dimensions)>,
}

impl std::fmt::Debug for FocusCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FocusCheck")
            .field("out", &self.out)
            .field("pending", &self.slot.pending())
            .field("shown", &self.shown.as_ref().map(|shown| &shown.region))
            .finish_non_exhaustive()
    }
}

/// `preview.region` of `request`: its job's number.
pub(crate) fn start_now(
    owner: &OwnerHandle,
    client: ClientId,
    request: &RegionRequest,
) -> Result<String, String> {
    let (started, _) = call(
        owner,
        client,
        "preview.region",
        json!({"item": request.item, "rect": request.rect, "frame": request.frame}),
    )?;
    started["job_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("preview.region answered no job: {started}"))
}

/// `job.read` of a region job and, once it has ended with a region, that region's JPEG decoded
/// whole, on this task's thread.
pub(crate) fn read_now(
    owner: &OwnerHandle,
    client: ClientId,
    job: &str,
) -> Result<RegionRead, String> {
    let record = job_now(owner, client, job)?;
    match record["status"].as_str() {
        Some("queued" | "running") => Ok(RegionRead::Running),
        Some("ready") => {
            let answer: RegionAnswer = serde_json::from_value(record["result"].clone())
                .map_err(|error| format!("the region's answer: {error}"))?;
            let side = answer.width.max(answer.height).max(1);
            let decoded = decode_preview(&answer.path, side, &Cancel::never())
                .map_err(|error| error.to_string());
            Ok(RegionRead::Ended(decoded.map(|decoded| (answer, decoded))))
        }
        _ => {
            let code = record["error"]["code"].as_str().unwrap_or("failed");
            let message = record["error"]["message"].as_str().unwrap_or("");
            Ok(RegionRead::Ended(Err(format!("{code}: {message}"))))
        }
    }
}

/// What the focus check asks the owner for next.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// `preview.region` of this request, numbered.
    Start(u64, RegionRequest),
    /// `job.read` of this job, for request `serial`.
    Read(u64, String),
}

impl FocusCheck {
    /// The focus check wants `request` now, or nothing (it is off): offer it to the slot unless it
    /// is the one already asked for, and send the next request when nothing is out.
    pub(crate) fn want(&mut self, request: Option<RegionRequest>) -> Option<Next> {
        match request {
            None => {
                self.slot.drop_pending();
                self.last = None;
            }
            Some(request) => {
                if self.last.as_ref() != Some(&request) {
                    self.last = Some(request.clone());
                    self.slot.offer(request);
                }
            }
        }
        self.start()
    }

    /// Send the waiting request when nothing is out.
    fn start(&mut self) -> Option<Next> {
        if self.out.is_some() {
            return None;
        }
        let request = self.slot.start()?;
        self.serial += 1;
        self.out = Some(Out {
            serial: self.serial,
            request: request.clone(),
            job: None,
            reading: false,
            woken: false,
        });
        Some(Next::Start(self.serial, request))
    }

    /// The owner woke this client: read the region's job when it is known and no read is out;
    /// otherwise keep the wake for when it can be used.
    pub(crate) fn woken(&mut self, owner: bool) -> Option<Next> {
        if !owner {
            return None;
        }
        let out = self.out.as_mut()?;
        match &out.job {
            Some(job) if !out.reading => {
                out.reading = true;
                Some(Next::Read(out.serial, job.clone()))
            }
            _ => {
                out.woken = true;
                None
            }
        }
    }

    /// One of this module's answers.
    pub(crate) fn update(&mut self, message: RegionMessage) -> Option<Next> {
        match message {
            RegionMessage::Started { serial, result } => {
                let out = self.out.as_mut().filter(|out| out.serial == serial)?;
                match result {
                    Ok(job) => {
                        out.job = Some(job.clone());
                        if std::mem::take(&mut out.woken) {
                            out.reading = true;
                            return Some(Next::Read(serial, job));
                        }
                        None
                    }
                    Err(error) => {
                        let request = self.out.take().map(|out| out.request)?;
                        self.error = Some((request.item, error));
                        self.slot.answered();
                        self.start()
                    }
                }
            }
            RegionMessage::Read { serial, result } => {
                let out = self.out.as_mut().filter(|out| out.serial == serial)?;
                out.reading = false;
                let ended = match result.map(|read| *read) {
                    Ok(RegionRead::Running) => {
                        // Still being cut: read again on the wake its end brings, or now for a
                        // wake that arrived during this read.
                        if std::mem::take(&mut out.woken) {
                            out.reading = true;
                            return out.job.clone().map(|job| Next::Read(serial, job));
                        }
                        return None;
                    }
                    Ok(RegionRead::Ended(ended)) => ended,
                    Err(error) => Err(error),
                };
                let request = self.out.take().map(|out| out.request)?;
                match ended {
                    Ok((answer, decoded)) => self.landed(serial, request, answer, decoded),
                    Err(error) => self.error = Some((request.item, error)),
                }
                self.slot.answered();
                self.start()
            }
        }
    }

    /// Region `serial` landed: its handle made once, drawn under the item it was cut from.
    fn landed(
        &mut self,
        serial: u64,
        request: RegionRequest,
        answer: RegionAnswer,
        decoded: DecodedPreview,
    ) {
        self.error = None;
        self.frame_of = Some((request.item.clone(), answer.frame));
        self.shown = Some(Shown {
            region: Region {
                serial,
                item: request.item,
                rect: answer.rect,
                frame: answer.frame,
                origin: answer.origin,
            },
            handle: region_handle(decoded),
        });
    }

    /// Forget every region: the focus check is off or the loupe closed. An answer still out is
    /// dropped when it arrives, and the next region the owner is asked for cancels it.
    pub(crate) fn release(&mut self) {
        *self = Self {
            serial: self.serial,
            ..Self::default()
        };
    }

    /// The region held, as the model reads it.
    pub(crate) fn region(&self) -> Option<Region> {
        self.shown.as_ref().map(|shown| shown.region.clone())
    }

    /// The handle of the region the model names.
    pub(crate) fn handle(&self, region: &Region) -> Option<&Handle> {
        self.shown
            .as_ref()
            .filter(|shown| shown.region == *region)
            .map(|shown| &shown.handle)
    }

    /// A region is out or waits.
    pub(crate) fn pending(&self) -> bool {
        self.out.is_some() || self.slot.pending().is_some()
    }

    /// Why the last region of `item` failed.
    pub(crate) fn error(&self, item: &PreviewItem) -> Option<String> {
        self.error
            .as_ref()
            .filter(|(of, _)| of == item)
            .map(|(_, error)| error.clone())
    }

    /// The frame the last region of an item was cut from.
    pub(crate) fn frame_of(&self) -> Option<(PreviewItem, Dimensions)> {
        self.frame_of.clone()
    }

    /// `request` is the rectangle last asked for, nothing is out or waiting, and a region of its
    /// frame has landed or its last region failed: an evidence step captures it.
    pub(crate) fn settled(&self, request: &RegionRequest) -> bool {
        self.last.as_ref() == Some(request)
            && !self.pending()
            && (self
                .shown
                .as_ref()
                .is_some_and(|shown| shown.region.item == request.item)
                || self.error(&request.item).is_some())
    }

    /// What the focus check holds, for correlated evidence.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "out": self.out.as_ref().map(|out| json!({
                "serial": out.serial,
                "rect": out.request.rect,
                "frame": out.request.frame,
                "job": out.job.is_some(),
            })),
            "waiting": self.slot.pending().map(|request| request.rect),
            "requests": self.serial,
            "error": self.error.as_ref().map(|(_, error)| error),
        })
    }
}

/// The owner task `next` asks for, its answer carried back as `wrap` makes it.
pub(crate) fn task<M: Send + 'static>(
    owner: &OwnerHandle,
    client: ClientId,
    next: Option<Next>,
    wrap: fn(RegionMessage) -> M,
) -> Task<M> {
    let Some(next) = next else {
        return Task::none();
    };
    let owner = owner.clone();
    match next {
        Next::Start(serial, request) => owner_work(move || start_now(&owner, client, &request))
            .map(move |result| wrap(RegionMessage::Started { serial, result })),
        Next::Read(serial, job) => owner_work(move || read_now(&owner, client, &job).map(Box::new))
            .map(move |result| wrap(RegionMessage::Read { serial, result })),
    }
}

#[cfg(test)]
#[path = "loupe_region_tests.rs"]
mod tests;
