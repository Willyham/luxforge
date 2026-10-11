//! The lab's threads. Nothing here runs on the window's thread.
//!
//! - The **segmentation worker** holds the segmenter and the current embedding. Its inbox has one
//!   slot per kind of request, so a newer request replaces an unanswered one: an encode for the
//!   latest image, one stroke, one click, and the latest hover point (a mailbox, not a queue).
//!   Encoding comes first; strokes, clicks and hovers for an image wait until its embedding is
//!   ready, and requests for a replaced image are dropped.
//! - The **fill worker** holds the fill and runs removals in order: region, fill, display copy.
//! - Loading, the display copy after an undo, and saving each run on a short-lived thread.
//!
//! Every result goes to the lab's channel, followed by a call to the wake function, which is how a
//! window learns there is something to apply.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::image::Rgb8;
use crate::mask::{Logits, Point, RectPx};
use crate::refine;
use crate::segment::{Embedding, LoadTimes, Prompt, Segmenter};
use crate::stroke::{self, Decision, Resolution, Stroke};

use super::log::ms;
use super::pipeline::{self, FillReport, Filler, Target};
use super::raster::{self, Picture, Raster};

/// Called on a worker thread after each result is sent.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Opens the segmenter on the segmentation worker's own thread.
pub type SegmenterOpener =
    Box<dyn FnOnce() -> Result<(Box<dyn Segmenter>, LoadTimes), String> + Send>;

/// Opens the fill on the fill worker's own thread.
pub type FillerOpener = Box<dyn FnOnce() -> Result<(Filler, Option<Duration>), String> + Send>;

#[derive(Clone)]
pub(crate) struct Outbox {
    tx: Sender<Event>,
    wake: Wake,
}

impl Outbox {
    pub(crate) fn new(wake: Wake) -> (Outbox, Receiver<Event>) {
        let (tx, rx) = channel();
        (Outbox { tx, wake }, rx)
    }

    fn send(&self, event: Event) {
        // The lab is gone when this fails; there is nobody left to tell.
        if self.tx.send(event).is_ok() {
            (self.wake)();
        }
    }
}

pub(crate) enum Event {
    Loaded {
        image: Arc<Rgb8>,
        picture: Picture,
        load_ms: f64,
        picture_ms: f64,
    },
    LoadFailed(String),
    SegmenterReady {
        encoder_ms: f64,
        decoder_ms: f64,
    },
    SegmenterFailed(String),
    FillReady {
        fill: &'static str,
        load_ms: Option<f64>,
    },
    FillFailed(String),
    Encoded {
        generation: u64,
        encode_ms: f64,
    },
    EncodeFailed {
        generation: u64,
        error: String,
    },
    Hover(Box<HoverResult>),
    HoverFailed {
        generation: u64,
        seq: u64,
        click: bool,
        error: String,
    },
    Stroke(Box<StrokeResult>),
    StrokeFailed {
        generation: u64,
        error: String,
    },
    Removed(Box<Removed>),
    RemoveFailed {
        job: u64,
        error: String,
    },
    Picture {
        generation: u64,
        picture: Picture,
    },
    Saved {
        path: PathBuf,
        save_ms: f64,
    },
    SaveFailed(String),
}

pub(crate) struct HoverResult {
    pub generation: u64,
    pub seq: u64,
    pub click: bool,
    pub point: Point,
    /// When the pointer event that asked for it was handled.
    pub at: Instant,
    /// Hover points replaced in the mailbox, unanswered, since the previous one taken.
    pub replaced: u64,
    pub decode_ms: f64,
    pub raster_ms: f64,
    pub candidates: usize,
    pub pick: Option<Picked>,
}

pub(crate) struct Picked {
    pub index: usize,
    pub predicted_iou: f32,
    pub logits: Logits,
    pub raster: Option<Raster>,
}

pub(crate) struct StrokeResult {
    pub generation: u64,
    pub prompts: usize,
    pub candidates: usize,
    pub resolution: Resolution,
    /// The decision's objects in its order: the one, or the several numbered from 1.
    pub items: Vec<Item>,
    pub prompts_ms: f64,
    pub decode_ms: f64,
    pub decide_ms: f64,
    pub raster_ms: f64,
}

/// An object a stroke found.
#[derive(Clone, Debug)]
pub struct Item {
    /// Its number on screen, from 1.
    pub number: usize,
    /// Its index in the resolution's `scored`.
    pub scored: usize,
    pub logits: Logits,
    pub raster: Option<Raster>,
    pub pin: Option<Point>,
    pub area: f64,
    pub selected: bool,
}

pub(crate) struct Removed {
    pub job: u64,
    pub image: Arc<Rgb8>,
    pub picture: Picture,
    pub region: Option<RectPx>,
    pub region_area: u64,
    pub region_ms: f64,
    pub fill_ms: f64,
    pub picture_ms: f64,
    pub fill: FillReport,
}

pub(crate) struct EncodeRequest {
    pub generation: u64,
    pub image: Arc<Rgb8>,
    /// Source pixels per display pixel, the finest an overlay needs to be.
    pub min_step: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct HoverRequest {
    pub generation: u64,
    pub seq: u64,
    pub point: Point,
    pub at: Instant,
}

pub(crate) struct StrokeRequest {
    pub generation: u64,
    pub stroke: Stroke,
}

#[derive(Default)]
struct Inbox {
    encode: Option<EncodeRequest>,
    stroke: Option<StrokeRequest>,
    click: Option<HoverRequest>,
    hover: Option<HoverRequest>,
    replaced: u64,
    quit: bool,
}

type Shared = Arc<(Mutex<Inbox>, Condvar)>;

pub(crate) struct SegWorker {
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}

impl SegWorker {
    pub(crate) fn spawn(open: SegmenterOpener, params: stroke::Params, out: Outbox) -> SegWorker {
        let shared: Shared = Arc::default();
        let inbox = shared.clone();
        let thread = std::thread::Builder::new()
            .name("lab-segment".into())
            .spawn(move || segment_loop(open, params, inbox, out))
            .expect("spawn the segmentation worker");
        SegWorker {
            shared,
            thread: Some(thread),
        }
    }

    fn post(&self, f: impl FnOnce(&mut Inbox)) {
        let (lock, cvar) = &*self.shared;
        if let Ok(mut inbox) = lock.lock() {
            f(&mut inbox);
            cvar.notify_one();
        }
    }

    pub(crate) fn encode(&self, request: EncodeRequest) {
        self.post(|i| i.encode = Some(request));
    }

    /// The latest pointer position; an unanswered earlier one is dropped.
    pub(crate) fn hover(&self, request: HoverRequest) {
        self.post(|i| {
            if i.hover.replace(request).is_some() {
                i.replaced += 1;
            }
        });
    }

    pub(crate) fn click(&self, request: HoverRequest) {
        self.post(|i| i.click = Some(request));
    }

    pub(crate) fn stroke(&self, request: StrokeRequest) {
        self.post(|i| i.stroke = Some(request));
    }
}

impl Drop for SegWorker {
    fn drop(&mut self) {
        self.post(|i| i.quit = true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

enum Work {
    Encode(EncodeRequest),
    Stroke(StrokeRequest),
    Point(HoverRequest, bool, u64),
}

struct Current {
    generation: u64,
    embedding: Embedding,
    source: (u32, u32),
    min_step: f32,
}

fn segment_loop(open: SegmenterOpener, params: stroke::Params, shared: Shared, out: Outbox) {
    let mut segmenter = match open() {
        Ok((s, load)) => {
            out.send(Event::SegmenterReady {
                encoder_ms: ms(load.encoder),
                decoder_ms: ms(load.decoder),
            });
            s
        }
        Err(e) => {
            out.send(Event::SegmenterFailed(e));
            return;
        }
    };
    let mut current: Option<Current> = None;
    let (lock, cvar) = &*shared;
    loop {
        let work = {
            let Ok(mut inbox) = lock.lock() else { return };
            loop {
                if inbox.quit {
                    return;
                }
                if let Some(e) = inbox.encode.take() {
                    break Work::Encode(e);
                }
                let ready = current.as_ref().map(|c| c.generation);
                // A request for an image that has been replaced is dropped; one for an image not yet
                // analysed waits for its embedding.
                let state = |generation: u64| match ready {
                    Some(g) if generation < g => Slot::Stale,
                    Some(g) if generation == g => Slot::Ready,
                    _ => Slot::Waiting,
                };
                if let Some(s) = &inbox.stroke {
                    match state(s.generation) {
                        Slot::Ready => break Work::Stroke(inbox.stroke.take().expect("checked")),
                        Slot::Stale => inbox.stroke = None,
                        Slot::Waiting => {}
                    }
                }
                if let Some(c) = inbox.click {
                    match state(c.generation) {
                        Slot::Ready => {
                            inbox.click = None;
                            break Work::Point(c, true, 0);
                        }
                        Slot::Stale => inbox.click = None,
                        Slot::Waiting => {}
                    }
                }
                if let Some(h) = inbox.hover {
                    match state(h.generation) {
                        Slot::Ready => {
                            inbox.hover = None;
                            let replaced = std::mem::take(&mut inbox.replaced);
                            break Work::Point(h, false, replaced);
                        }
                        Slot::Stale => {
                            inbox.hover = None;
                            inbox.replaced = 0;
                        }
                        Slot::Waiting => {}
                    }
                }
                inbox = match cvar.wait(inbox) {
                    Ok(i) => i,
                    Err(_) => return,
                };
            }
        };
        match work {
            Work::Encode(request) => {
                current = None;
                let started = Instant::now();
                match segmenter.encode(&request.image) {
                    Ok(embedding) => {
                        current = Some(Current {
                            generation: request.generation,
                            embedding,
                            source: (request.image.width, request.image.height),
                            min_step: request.min_step,
                        });
                        out.send(Event::Encoded {
                            generation: request.generation,
                            encode_ms: ms(started.elapsed()),
                        });
                    }
                    Err(error) => out.send(Event::EncodeFailed {
                        generation: request.generation,
                        error,
                    }),
                }
            }
            Work::Stroke(request) => {
                let c = current.as_ref().expect("a ready request has its embedding");
                let generation = request.generation;
                match resolve(segmenter.as_mut(), c, request, &params) {
                    Ok(result) => out.send(Event::Stroke(Box::new(result))),
                    Err(error) => out.send(Event::StrokeFailed { generation, error }),
                }
            }
            Work::Point(request, click, replaced) => {
                let c = current.as_ref().expect("a ready request has its embedding");
                match point(segmenter.as_mut(), c, request, click, replaced, &params) {
                    Ok(result) => out.send(Event::Hover(Box::new(result))),
                    Err(error) => out.send(Event::HoverFailed {
                        generation: request.generation,
                        seq: request.seq,
                        click,
                        error,
                    }),
                }
            }
        }
    }
}

enum Slot {
    Ready,
    Stale,
    Waiting,
}

/// A hover or click point: decode it alone, pick as `stroke::pick_hover` does, and tint the pick.
fn point(
    segmenter: &mut dyn Segmenter,
    c: &Current,
    request: HoverRequest,
    click: bool,
    replaced: u64,
    params: &stroke::Params,
) -> Result<HoverResult, String> {
    let started = Instant::now();
    let prompt = Prompt {
        positive: vec![request.point],
        ..Prompt::default()
    };
    let mut candidates = segmenter.decode(&c.embedding, &prompt)?;
    let decode_ms = ms(started.elapsed());
    let started = Instant::now();
    let count = candidates.len();
    let pick = stroke::pick_hover(&candidates, params).map(|index| {
        let chosen = candidates.swap_remove(index);
        let raster = raster::object_raster(&chosen.logits, c.min_step, raster::HOVER);
        Picked {
            index,
            predicted_iou: chosen.predicted_iou,
            logits: chosen.logits,
            raster,
        }
    });
    Ok(HoverResult {
        generation: request.generation,
        seq: request.seq,
        click,
        point: request.point,
        at: request.at,
        replaced,
        decode_ms,
        raster_ms: ms(started.elapsed()),
        candidates: count,
        pick,
    })
}

/// A stroke: its prompts decoded in one batch, the decision, and a raster and pin per object.
fn resolve(
    segmenter: &mut dyn Segmenter,
    c: &Current,
    request: StrokeRequest,
    params: &stroke::Params,
) -> Result<StrokeResult, String> {
    let started = Instant::now();
    let prompts = stroke::prompts(&request.stroke, c.source, params);
    let prompts_ms = ms(started.elapsed());
    if prompts.is_empty() {
        return Err("an empty stroke".into());
    }
    let started = Instant::now();
    let results = segmenter.decode_many(&c.embedding, &prompts)?;
    let decode_ms = ms(started.elapsed());
    let started = Instant::now();
    let resolution = stroke::decide(&request.stroke, &results, params);
    let decide_ms = ms(started.elapsed());
    let started = Instant::now();
    let (chosen, several) = match &resolution.decision {
        Decision::One(i) => (vec![*i], false),
        Decision::Several(v) => (v.clone(), true),
        Decision::Nothing => (Vec::new(), false),
    };
    let items = chosen
        .iter()
        .enumerate()
        .map(|(k, &i)| {
            let s = &resolution.scored[i];
            let logits = results[s.prompt][s.candidate].logits.clone();
            let colour = if several {
                raster::palette(k)
            } else {
                raster::HOVER
            };
            Item {
                number: k + 1,
                scored: i,
                raster: raster::object_raster(&logits, c.min_step, colour),
                pin: raster::pin(&logits),
                area: s.area,
                logits,
                selected: false,
            }
        })
        .collect();
    Ok(StrokeResult {
        generation: request.generation,
        prompts: prompts.len(),
        candidates: results.iter().map(Vec::len).sum(),
        resolution,
        items,
        prompts_ms,
        decode_ms,
        decide_ms,
        raster_ms: ms(started.elapsed()),
    })
}

pub(crate) struct RemoveRequest {
    pub job: u64,
    pub image: Arc<Rgb8>,
    pub targets: Vec<Target>,
    pub refine: refine::Params,
}

pub(crate) struct FillWorker {
    tx: Option<Sender<RemoveRequest>>,
    thread: Option<JoinHandle<()>>,
}

impl FillWorker {
    pub(crate) fn spawn(open: FillerOpener, out: Outbox) -> FillWorker {
        let (tx, rx) = channel::<RemoveRequest>();
        let thread = std::thread::Builder::new()
            .name("lab-fill".into())
            .spawn(move || {
                let mut filler = match open() {
                    Ok((f, load)) => {
                        out.send(Event::FillReady {
                            fill: f.name(),
                            load_ms: load.map(ms),
                        });
                        Ok(f)
                    }
                    Err(e) => {
                        out.send(Event::FillFailed(e.clone()));
                        Err(e)
                    }
                };
                for request in rx {
                    let job = request.job;
                    let result = match filler.as_mut() {
                        Ok(f) => remove(f, request),
                        Err(e) => Err(format!("the fill did not load: {e}")),
                    };
                    match result {
                        Ok(removed) => out.send(Event::Removed(Box::new(removed))),
                        Err(error) => out.send(Event::RemoveFailed { job, error }),
                    }
                }
            })
            .expect("spawn the fill worker");
        FillWorker {
            tx: Some(tx),
            thread: Some(thread),
        }
    }

    pub(crate) fn remove(&self, request: RemoveRequest) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(request);
        }
    }
}

impl Drop for FillWorker {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn remove(filler: &mut Filler, request: RemoveRequest) -> Result<Removed, String> {
    let started = Instant::now();
    let region = pipeline::region(&request.targets, &request.image, &request.refine);
    let region_ms = ms(started.elapsed());
    let bounds = region.bounds().ok_or("the removal region is empty")?;
    let started = Instant::now();
    let (image, fill) = filler.run(&request.image, &region)?;
    let fill_ms = ms(started.elapsed());
    let started = Instant::now();
    let picture = raster::picture(&image, raster::DISPLAY_LONG);
    Ok(Removed {
        job: request.job,
        image: Arc::new(image),
        picture,
        region: Some(bounds),
        region_area: region.area(),
        region_ms,
        fill_ms,
        picture_ms: ms(started.elapsed()),
        fill,
    })
}

pub(crate) fn load(path: PathBuf, out: Outbox) {
    let _ = std::thread::Builder::new()
        .name("lab-load".into())
        .spawn(move || {
            let started = Instant::now();
            match Rgb8::load(&path) {
                Ok(image) => {
                    let load_ms = ms(started.elapsed());
                    let started = Instant::now();
                    let picture = raster::picture(&image, raster::DISPLAY_LONG);
                    out.send(Event::Loaded {
                        image: Arc::new(image),
                        picture,
                        load_ms,
                        picture_ms: ms(started.elapsed()),
                    });
                }
                Err(e) => out.send(Event::LoadFailed(e)),
            }
        });
}

pub(crate) fn picture(generation: u64, image: Arc<Rgb8>, out: Outbox) {
    let _ = std::thread::Builder::new()
        .name("lab-picture".into())
        .spawn(move || {
            let picture = raster::picture(&image, raster::DISPLAY_LONG);
            out.send(Event::Picture {
                generation,
                picture,
            });
        });
}

/// Writes `image` as the first free `<stem>-lab-NNN.png` in `dir`. The file is created new, so
/// nothing, the input included, is ever overwritten.
pub(crate) fn save(image: Arc<Rgb8>, dir: PathBuf, stem: String, out: Outbox) {
    let _ = std::thread::Builder::new()
        .name("lab-save".into())
        .spawn(move || {
            let started = Instant::now();
            match save_new(&image, &dir, &stem) {
                Ok(path) => out.send(Event::Saved {
                    path,
                    save_ms: ms(started.elapsed()),
                }),
                Err(e) => out.send(Event::SaveFailed(e)),
            }
        });
}

pub(crate) fn save_new(image: &Rgb8, dir: &std::path::Path, stem: &str) -> Result<PathBuf, String> {
    use std::io::{BufWriter, Write};
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for n in 1..10_000 {
        let path = dir.join(format!("{stem}-lab-{n:03}.png"));
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut w = BufWriter::new(file);
        image::RgbImage::from_raw(image.width, image.height, image.data.clone())
            .expect("buffer matches its size")
            .write_to(&mut w, image::ImageFormat::Png)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        w.flush().map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok(path);
    }
    Err(format!("{}: no free name for {stem}", dir.display()))
}
