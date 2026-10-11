//! The removal lab's state machine, shared by the `removal-lab` window and its headless script
//! mode, and the stages the `remove` command shares with it.
//!
//! [`Lab`] owns the image, its undo stack and what is on screen; the window turns pointer and key
//! events into calls on it and draws what it exposes, and the script runner makes the same calls.
//! All model work happens on the workers in [`workers`]; every call here returns at once.
//!
//! - **Hover** posts the pointer position to the segmentation worker's mailbox. The worker decodes
//!   the latest position, picks with `stroke::pick_hover` and tints the pick; the lab keeps the
//!   raster it already has when the pick's mask is unchanged.
//! - **Click** removes the hovered object: its region without a stroke, then the fill.
//! - **Brush** strokes resolve with `stroke::decide`: one object is removed with the stroke's
//!   footprint; several are offered as numbered pins to choose and confirm, removed together
//!   without the footprint (it also crosses the objects left out); none removes the dilated
//!   footprint.
//! - A removal is a job: the selection stays on screen until the fill succeeds, fails or is
//!   cancelled. Success replaces the image, pushes the previous one on the undo stack and starts
//!   analysing the new one.

pub mod log;
pub mod pipeline;
pub mod raster;
pub mod script;
pub mod workers;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::fill;
use crate::image::Rgb8;
use crate::mask::{Logits, Point};
use crate::refine;
use crate::runtime::Backend;
use crate::segment::{self, Family};
use crate::stroke::{self, Decision, Stroke};

use self::log::{Log, ms, round3};
use self::pipeline::{Filler, Target};
use self::raster::{Picture, Raster};
use self::workers::{
    EncodeRequest, Event, FillWorker, FillerOpener, HoverRequest, Outbox, RemoveRequest, SegWorker,
    SegmenterOpener, StrokeRequest,
};
pub use self::workers::{Item, Wake};

#[derive(Clone, Debug)]
pub struct Config {
    pub image: PathBuf,
    pub family: Family,
    /// The segmenter's encoder and decoder backend.
    pub backend: Backend,
    pub fill_backend: Backend,
    pub fill_model: crate::models::FillModel,
    pub models: PathBuf,
    /// Where saved copies and the log go.
    pub out: PathBuf,
    pub stroke: stroke::Params,
    pub refine: refine::Params,
    pub fill: fill::Params,
}

impl Config {
    pub fn new(image: PathBuf, out: PathBuf) -> Config {
        Config {
            image,
            family: Family::EfficientVitL0,
            backend: Backend::OrtCpu,
            fill_backend: Backend::OrtCpu,
            fill_model: crate::models::FillModel::default(),
            models: crate::models::directory(None),
            out,
            stroke: stroke::Params::default(),
            refine: refine::Params::default(),
            fill: fill::Params::default(),
        }
    }
}

/// Sets one of `stroke::Params`'s fields from `name=value`, for tuning from the command line.
pub fn stroke_param(params: &mut stroke::Params, setting: &str) -> Result<(), String> {
    let (name, value) = setting
        .split_once('=')
        .ok_or_else(|| format!("--param {setting:?}: expected name=value"))?;
    let bad = || format!("--param {setting:?}: cannot read the value");
    let unit = |v: &str| -> Result<f32, String> {
        v.parse::<f32>()
            .ok()
            .filter(|x| (0.0..=1.0).contains(x))
            .ok_or_else(bad)
    };
    match name {
        "sample_points" => params.sample_points = value.parse().map_err(|_| bad())?,
        "min_predicted_iou" => params.min_predicted_iou = unit(value)?,
        "min_whole_iou" => params.min_whole_iou = unit(value)?,
        "min_stability" => params.min_stability = unit(value)?,
        "nms_iou" => params.nms_iou = unit(value)?,
        "one_object_stroke_coverage" => params.one_object_stroke_coverage = unit(value)?,
        "min_object_coverage" => params.min_object_coverage = unit(value)?,
        _ => {
            return Err(format!(
                "--param {name}: expected one of sample_points, min_predicted_iou, min_whole_iou, \
                 min_stability, nms_iou, one_object_stroke_coverage, min_object_coverage"
            ));
        }
    }
    Ok(())
}

/// The brush's radius starts at this share of the image's long side.
const BRUSH_SHARE: f32 = 0.02;
/// `[` and `]` scale the brush by this factor.
const BRUSH_STEP: f32 = 1.25;

#[derive(Clone, Debug, PartialEq)]
enum Readiness {
    Loading,
    Ready,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Analysis {
    Waiting,
    Running { generation: u64 },
    Ready { generation: u64 },
    Failed(String),
}

/// The hovered object on screen.
#[derive(Clone, Debug)]
pub struct Hover {
    pub seq: u64,
    pub point: Point,
    pub predicted_iou: f32,
    pub logits: Logits,
    pub raster: Option<Raster>,
    /// The hover request that made the raster on screen, and when its pointer event was handled:
    /// an unchanged later result keeps both.
    pub raster_seq: u64,
    pub raster_at: Instant,
}

/// Several objects found under a stroke, waiting for the person to choose.
#[derive(Clone, Debug)]
pub struct Choice {
    pub stroke: Stroke,
    pub items: Vec<Item>,
}

/// A removal in progress.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: u64,
    pub started: Instant,
    /// The selection being removed, kept on screen.
    pub rasters: Vec<Raster>,
    pub stroke: Option<Stroke>,
    pub what: String,
}

pub struct Lab {
    config: Config,
    log: Arc<Log>,
    rx: Receiver<Event>,
    out: Outbox,
    // Joined when the lab drops; what they send after the channel closes is dropped.
    seg: SegWorker,
    fill: FillWorker,
    image: Option<Arc<Rgb8>>,
    picture: Option<Picture>,
    generation: u64,
    undo: Vec<Arc<Rgb8>>,
    load_error: Option<String>,
    segmenter: Readiness,
    filler: Readiness,
    fill_name: &'static str,
    analysis: Analysis,
    pointer: Option<Point>,
    hover_seq: u64,
    /// Hover results at or below this are no longer wanted (the pointer left, or the mode changed).
    hover_floor: u64,
    /// The newest hover request answered or given up on.
    hover_answered: u64,
    hover: Option<Hover>,
    pending_click: Option<u64>,
    brush_toggled: bool,
    shift: bool,
    radius: f32,
    painting: Option<Stroke>,
    resolving: Option<Stroke>,
    choice: Option<Choice>,
    job: Option<Job>,
    next_job: u64,
    saving: usize,
    unsaved: bool,
    removals: usize,
    last_decision: Option<&'static str>,
    message: Option<String>,
}

impl Lab {
    /// Starts loading the image, the segmenter and the fill, each on its own thread.
    pub fn start(config: Config, log: Arc<Log>, wake: Wake) -> Lab {
        let (family, backend, models) = (config.family, config.backend, config.models.clone());
        let segmenter: SegmenterOpener =
            Box::new(move || segment::open_timed(family, backend, backend, &models));
        let (fill_model, fill_backend, models, params) = (
            config.fill_model,
            config.fill_backend,
            config.models.clone(),
            config.fill.clone(),
        );
        // LaMa loads once, on the fill worker, while the image loads and is analysed.
        let filler: FillerOpener = Box::new(move || {
            Filler::open(fill_model, fill_backend, &models, params).map(|(f, load)| (f, Some(load)))
        });
        Lab::start_with(config, log, wake, segmenter, filler)
    }

    /// `start` with the segmenter and fill supplied, for tests.
    pub fn start_with(
        config: Config,
        log: Arc<Log>,
        wake: Wake,
        segmenter: SegmenterOpener,
        filler: FillerOpener,
    ) -> Lab {
        let (out, rx) = Outbox::new(wake);
        log.event(
            "start",
            json!({
                "image": config.image.display().to_string(),
                "family": config.family.name(),
                "backend": config.backend.name(),
                "fill_backend": config.fill_backend.name(),
                "out": config.out.display().to_string(),
                "log": log.path().map(|p| p.display().to_string()),
            }),
        );
        let seg = SegWorker::spawn(segmenter, config.stroke.clone(), out.clone());
        let fill = FillWorker::spawn(filler, out.clone());
        workers::load(config.image.clone(), out.clone());
        Lab {
            fill_name: "lama",
            config,
            log,
            rx,
            out,
            seg,
            fill,
            image: None,
            picture: None,
            generation: 0,
            undo: Vec::new(),
            load_error: None,
            segmenter: Readiness::Loading,
            filler: Readiness::Loading,
            analysis: Analysis::Waiting,
            pointer: None,
            hover_seq: 0,
            hover_floor: 0,
            hover_answered: 0,
            hover: None,
            pending_click: None,
            brush_toggled: false,
            shift: false,
            radius: 0.0,
            painting: None,
            resolving: None,
            choice: None,
            job: None,
            next_job: 0,
            saving: 0,
            unsaved: false,
            removals: 0,
            last_decision: None,
            message: None,
        }
    }

    pub fn log(&self) -> &Arc<Log> {
        &self.log
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    // ---- Results ----

    /// Applies every result the workers have sent. True when anything was applied.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok(event) = self.rx.try_recv() {
            self.apply(event);
            any = true;
        }
        any
    }

    /// Waits up to `timeout` for a result, then applies it and any others waiting.
    pub fn wait(&mut self, timeout: Duration) -> bool {
        match self.rx.recv_timeout(timeout) {
            Ok(event) => {
                self.apply(event);
                self.poll();
                true
            }
            Err(_) => false,
        }
    }

    /// Whether anything the person would wait for is still running: loading, analysis, a hover
    /// or click in flight, a stroke being resolved, a removal or a save.
    pub fn busy(&self) -> bool {
        let loading = self.image.is_none() && self.load_error.is_none();
        let segmenting = self.segmenter == Readiness::Loading
            || matches!(self.analysis, Analysis::Running { .. });
        let seg_ok = !matches!(self.segmenter, Readiness::Failed(_))
            && !matches!(self.analysis, Analysis::Failed(_));
        loading
            || segmenting
            || (seg_ok && self.hover_seq > self.hover_answered)
            || self.pending_click.is_some()
            || self.resolving.is_some()
            || self.job.is_some()
            || self.saving > 0
    }

    fn apply(&mut self, event: Event) {
        match event {
            Event::Loaded {
                image,
                picture,
                load_ms,
                picture_ms,
            } => {
                self.log.event(
                    "loaded",
                    json!({"source": [image.width, image.height], "display": [picture.size.0, picture.size.1],
                           "load_ms": load_ms, "display_ms": picture_ms}),
                );
                self.radius = (image.width.max(image.height) as f32 * BRUSH_SHARE)
                    .round()
                    .max(2.0);
                self.image = Some(image);
                self.picture = Some(picture);
                self.generation = 1;
                self.analyse();
            }
            Event::LoadFailed(e) => {
                self.log.event("load_failed", json!({"error": e}));
                self.message = Some(format!("Could not open the image: {e}"));
                self.load_error = Some(e);
            }
            Event::SegmenterReady {
                encoder_ms,
                decoder_ms,
            } => {
                self.log.event(
                    "segmenter_ready",
                    json!({"family": self.config.family.name(), "backend": self.config.backend.name(),
                           "encoder_load_ms": encoder_ms, "decoder_load_ms": decoder_ms}),
                );
                self.segmenter = Readiness::Ready;
            }
            Event::SegmenterFailed(e) => {
                self.log.event("segmenter_failed", json!({"error": e}));
                self.message = Some(format!("The segmenter did not load: {e}"));
                self.segmenter = Readiness::Failed(e);
            }
            Event::FillReady { fill, load_ms } => {
                self.log
                    .event("fill_ready", json!({"fill": fill, "load_ms": load_ms}));
                self.fill_name = fill;
                self.filler = Readiness::Ready;
            }
            Event::FillFailed(e) => {
                self.log.event("fill_failed", json!({"error": e}));
                self.message = Some(format!("The fill did not load: {e}"));
                self.filler = Readiness::Failed(e);
            }
            Event::Encoded {
                generation,
                encode_ms,
            } => {
                self.log.event(
                    "analysed",
                    json!({"generation": generation, "encode_ms": encode_ms,
                           "current": generation == self.generation}),
                );
                if generation == self.generation {
                    self.analysis = Analysis::Ready { generation };
                }
            }
            Event::EncodeFailed { generation, error } => {
                self.log.event(
                    "analysis_failed",
                    json!({"generation": generation, "error": error}),
                );
                if generation == self.generation {
                    self.message = Some(format!("Analysis failed: {error}"));
                    self.analysis = Analysis::Failed(error);
                }
            }
            Event::Hover(r) => self.apply_hover(*r),
            Event::HoverFailed {
                generation,
                seq,
                click,
                error,
            } => {
                self.log.event(
                    "hover_failed",
                    json!({"seq": seq, "click": click, "error": error}),
                );
                if generation == self.generation {
                    self.hover_answered = self.hover_answered.max(seq);
                    if click && self.pending_click == Some(seq) {
                        self.pending_click = None;
                    }
                    self.message = Some(format!("Decoding failed: {error}"));
                }
            }
            Event::Stroke(r) => self.apply_stroke(*r),
            Event::StrokeFailed { generation, error } => {
                self.log.event("stroke_failed", json!({"error": error}));
                if generation == self.generation && self.resolving.take().is_some() {
                    self.message = Some(format!("Could not resolve the stroke: {error}"));
                }
            }
            Event::Removed(r) => self.apply_removed(*r),
            Event::RemoveFailed { job, error } => {
                let current = self.job.as_ref().is_some_and(|j| j.id == job);
                self.log.event(
                    "remove_failed",
                    json!({"job": job, "error": error, "current": current}),
                );
                if current {
                    self.job = None;
                    self.message = Some(format!("Removal failed; the image is unchanged: {error}"));
                }
            }
            Event::Picture {
                generation,
                picture,
            } => {
                if generation == self.generation {
                    self.picture = Some(picture);
                }
            }
            Event::Saved { path, save_ms } => {
                self.saving -= 1;
                self.unsaved = false;
                self.log.event(
                    "saved",
                    json!({"path": path.display().to_string(), "save_ms": save_ms}),
                );
                self.message = Some(format!("Saved {}", path.display()));
            }
            Event::SaveFailed(e) => {
                self.saving -= 1;
                self.log.event("save_failed", json!({"error": e}));
                self.message = Some(format!("Save failed: {e}"));
            }
        }
    }

    fn apply_hover(&mut self, r: workers::HoverResult) {
        if r.generation != self.generation {
            return;
        }
        self.hover_answered = self.hover_answered.max(r.seq);
        let pointer_to_result_ms = ms(r.at.elapsed());
        let mut fields = json!({
            "seq": r.seq, "click": r.click, "point": [r.point.x, r.point.y],
            "replaced": r.replaced, "decode_ms": r.decode_ms, "raster_ms": r.raster_ms,
            "candidates": r.candidates,
            "pick": r.pick.as_ref().map(|p| p.index),
            "predicted_iou": r.pick.as_ref().map(|p| round3(p.predicted_iou as f64)),
            "pointer_to_result_ms": pointer_to_result_ms,
        });
        if r.click {
            let wanted = self.pending_click == Some(r.seq);
            fields["applied"] = Value::from(wanted);
            self.log.event("hover", fields);
            if !wanted {
                return;
            }
            self.pending_click = None;
            match r.pick {
                Some(p) => self.remove_object(p.logits, p.raster),
                None => self.message = Some("Nothing under the pointer to remove".into()),
            }
            return;
        }
        let newer = self.hover.as_ref().is_none_or(|h| r.seq > h.seq);
        let wanted = r.seq > self.hover_floor && newer && self.hover_allowed();
        fields["applied"] = Value::from(wanted);
        if !wanted {
            self.log.event("hover", fields);
            return;
        }
        let at = r.at;
        match r.pick {
            None => {
                fields["changed"] = Value::from(self.hover.is_some());
                self.hover = None;
            }
            Some(p) => {
                let same = match (&self.hover, &p.raster) {
                    (Some(h), Some(new)) => {
                        h.raster.as_ref().map(|r| r.signature) == Some(new.signature)
                    }
                    _ => false,
                };
                fields["changed"] = Value::from(!same);
                let previous = self.hover.take();
                let (raster, raster_seq, raster_at) = match previous {
                    Some(h) if same => (h.raster, h.raster_seq, h.raster_at),
                    _ => (p.raster, r.seq, at),
                };
                self.hover = Some(Hover {
                    seq: r.seq,
                    point: r.point,
                    predicted_iou: p.predicted_iou,
                    logits: p.logits,
                    raster,
                    raster_seq,
                    raster_at,
                });
            }
        }
        self.log.event("hover", fields);
    }

    fn apply_stroke(&mut self, r: workers::StrokeResult) {
        if r.generation != self.generation {
            return;
        }
        let Some(stroke) = self.resolving.take() else {
            self.log
                .event("stroke_discarded", json!({"reason": "cancelled"}));
            return;
        };
        let kind = match r.resolution.decision {
            Decision::One(_) => "one",
            Decision::Several(_) => "several",
            Decision::Nothing => "nothing",
        };
        self.last_decision = Some(kind);
        let scored: Vec<Value> = r
            .resolution
            .scored
            .iter()
            .enumerate()
            .map(|(i, s)| {
                json!({"index": i, "prompt": s.prompt, "candidate": s.candidate,
                       "predicted_iou": round3(s.predicted_iou as f64), "stability": round3(s.stability as f64),
                       "stroke_coverage": round3(s.stroke_coverage as f64),
                       "object_coverage": round3(s.object_coverage as f64), "area_px": s.area.round()})
            })
            .collect();
        self.log.event(
            "decision",
            json!({
                "decision": kind,
                "chosen": r.items.iter().map(|i| i.scored).collect::<Vec<_>>(),
                "stroke_points": stroke.points.len(), "radius": stroke.radius,
                "prompts": r.prompts, "candidates": r.candidates, "scored": scored,
                "prompts_ms": r.prompts_ms, "decode_ms": r.decode_ms, "decide_ms": r.decide_ms,
                "raster_ms": r.raster_ms,
            }),
        );
        let mut items = r.items;
        match r.resolution.decision {
            Decision::One(_) => {
                let item = items.remove(0);
                let what = "the object under the stroke".to_string();
                self.start_removal(
                    vec![Target::Object {
                        logits: item.logits,
                        stroke: Some(stroke.clone()),
                    }],
                    item.raster.into_iter().collect(),
                    Some(stroke),
                    what,
                );
            }
            Decision::Several(_) => {
                self.message = None;
                self.choice = Some(Choice { stroke, items });
            }
            Decision::Nothing => {
                self.start_removal(
                    vec![Target::Footprint(stroke.clone())],
                    Vec::new(),
                    Some(stroke),
                    "the painted area (no object found)".into(),
                );
            }
        }
    }

    fn apply_removed(&mut self, r: workers::Removed) {
        let current = self.job.as_ref().is_some_and(|j| j.id == r.job);
        let total_ms = self
            .job
            .as_ref()
            .filter(|_| current)
            .map(|j| ms(j.started.elapsed()));
        self.log.event(
            "removed",
            json!({
                "job": r.job, "applied": current,
                "region": r.region.map(|b| [b.x, b.y, b.width, b.height]), "region_area_px": r.region_area,
                "region_ms": r.region_ms, "fill_ms": r.fill_ms, "display_ms": r.picture_ms,
                "fill": r.fill, "total_ms": total_ms,
            }),
        );
        if !current {
            return;
        }
        let job = self.job.take().expect("checked");
        if let Some(previous) = self.image.replace(r.image) {
            self.undo.push(previous);
        }
        self.picture = Some(r.picture);
        self.removals += 1;
        self.unsaved = true;
        self.message = Some(format!(
            "Removed {} in {:.0} ms",
            job.what,
            job.started.elapsed().as_secs_f64() * 1e3,
        ));
        self.new_generation();
    }

    /// The image changed: forget what belonged to the old one and analyse the new one.
    fn new_generation(&mut self) {
        self.generation += 1;
        self.hover = None;
        self.hover_floor = self.hover_seq;
        self.hover_answered = self.hover_seq;
        self.pending_click = None;
        self.resolving = None;
        self.choice = None;
        self.analyse();
    }

    fn analyse(&mut self) {
        let (Some(image), Some(picture)) = (&self.image, &self.picture) else {
            return;
        };
        let min_step =
            image.width.max(image.height) as f32 / picture.size.0.max(picture.size.1) as f32;
        self.analysis = Analysis::Running {
            generation: self.generation,
        };
        self.seg.encode(EncodeRequest {
            generation: self.generation,
            image: image.clone(),
            min_step,
        });
    }

    // ---- Input ----

    pub fn brush_active(&self) -> bool {
        self.brush_toggled || self.shift
    }

    pub fn toggle_brush(&mut self) {
        self.brush_toggled = !self.brush_toggled;
        self.mode_changed();
    }

    pub fn set_shift(&mut self, held: bool) {
        if self.shift != held {
            self.shift = held;
            self.mode_changed();
        }
    }

    fn mode_changed(&mut self) {
        if self.brush_active() {
            self.hover = None;
            self.hover_floor = self.hover_seq;
            self.hover_answered = self.hover_seq;
        } else if self.painting.is_none() {
            // Back to hover: highlight what is under the pointer without waiting for it to move.
            if let Some(p) = self.pointer {
                self.pointer_moved(p, Instant::now());
            }
        }
    }

    pub fn brush_radius(&self) -> f32 {
        self.radius
    }

    pub fn set_brush_radius(&mut self, radius: f32) {
        let long = self
            .image
            .as_ref()
            .map_or(f32::MAX, |i| i.width.max(i.height) as f32);
        self.radius = radius.clamp(2.0, (long / 4.0).max(2.0));
    }

    pub fn grow_brush(&mut self) {
        self.set_brush_radius(self.radius * BRUSH_STEP);
    }

    pub fn shrink_brush(&mut self) {
        self.set_brush_radius(self.radius / BRUSH_STEP);
    }

    fn hover_allowed(&self) -> bool {
        self.image.is_some()
            && !self.brush_active()
            && self.painting.is_none()
            && self.resolving.is_none()
            && self.choice.is_none()
            && self.job.is_none()
            && !matches!(self.segmenter, Readiness::Failed(_))
    }

    fn clamp(&self, p: Point) -> Point {
        match &self.image {
            Some(i) => Point::new(
                p.x.clamp(0.0, i.width as f32),
                p.y.clamp(0.0, i.height as f32),
            ),
            None => p,
        }
    }

    /// The pointer moved to `p` (source pixels) in an event handled at `at`.
    pub fn pointer_moved(&mut self, p: Point, at: Instant) {
        let p = self.clamp(p);
        self.pointer = Some(p);
        if let Some(stroke) = self.painting.as_mut() {
            let last = *stroke.points.last().expect("a stroke starts with a point");
            if (p.x - last.x).hypot(p.y - last.y) >= 1.0 {
                stroke.points.push(p);
            }
            return;
        }
        if !self.hover_allowed() {
            return;
        }
        self.hover_seq += 1;
        self.seg.hover(HoverRequest {
            generation: self.generation,
            seq: self.hover_seq,
            point: p,
            at,
        });
    }

    /// The pointer left the image.
    pub fn pointer_left(&mut self) {
        self.pointer = None;
        if self.painting.is_none() {
            self.hover = None;
            self.hover_floor = self.hover_seq;
        }
    }

    /// The primary button went down at `p`. `tolerance` is how far from a pin, in source pixels,
    /// still hits it.
    pub fn press(&mut self, p: Point, at: Instant, tolerance: f32) {
        if self.image.is_none() {
            return;
        }
        let p = self.clamp(p);
        if self.job.is_some() {
            self.message = Some("Still removing; Esc cancels".into());
            return;
        }
        if self.choice.is_some() {
            self.toggle_at(p, tolerance);
            return;
        }
        if self.resolving.is_some() {
            return;
        }
        if self.brush_active() {
            self.hover = None;
            self.hover_floor = self.hover_seq;
            self.painting = Some(Stroke {
                points: vec![p],
                radius: self.radius,
            });
            return;
        }
        if let Some(e) = self.segmentation_error() {
            self.message = Some(e);
            return;
        }
        let hovered = self
            .hover
            .as_ref()
            .is_some_and(|h| h.point == p && h.seq > self.hover_floor);
        if hovered {
            let h = self.hover.take().expect("checked");
            self.log.event(
                "click",
                json!({"point": [p.x, p.y], "hovered": true, "seq": h.seq}),
            );
            self.remove_object(h.logits, h.raster);
            return;
        }
        // Not hovered yet (or the hover result is still on its way): decode the click itself.
        self.hover_seq += 1;
        self.pending_click = Some(self.hover_seq);
        self.log.event(
            "click",
            json!({"point": [p.x, p.y], "hovered": false, "seq": self.hover_seq}),
        );
        self.seg.click(HoverRequest {
            generation: self.generation,
            seq: self.hover_seq,
            point: p,
            at,
        });
    }

    /// The primary button went up: a painted stroke is resolved.
    pub fn release(&mut self) {
        let Some(stroke) = self.painting.take() else {
            return;
        };
        if let Some(e) = self.segmentation_error() {
            self.message = Some(e);
            return;
        }
        self.log.event(
            "stroke",
            json!({"points": stroke.points.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(),
                   "radius": stroke.radius, "analysed": matches!(self.analysis, Analysis::Ready { .. })}),
        );
        self.message = None;
        self.resolving = Some(stroke.clone());
        self.seg.stroke(StrokeRequest {
            generation: self.generation,
            stroke,
        });
    }

    fn segmentation_error(&self) -> Option<String> {
        if let Readiness::Failed(e) = &self.segmenter {
            return Some(format!("The segmenter did not load: {e}"));
        }
        if let Analysis::Failed(e) = &self.analysis {
            return Some(format!("Analysis failed: {e}"));
        }
        None
    }

    fn toggle_at(&mut self, p: Point, tolerance: f32) {
        let Some(choice) = &self.choice else { return };
        let pinned = choice
            .items
            .iter()
            .filter_map(|i| i.pin.map(|q| (i.number, (q.x - p.x).hypot(q.y - p.y))))
            .filter(|&(_, d)| d <= tolerance)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| n);
        // Otherwise the smallest object under the point.
        let under = || {
            choice
                .items
                .iter()
                .filter(|i| i.logits.sample(p) > 0.0)
                .min_by(|a, b| a.area.total_cmp(&b.area))
                .map(|i| i.number)
        };
        if let Some(n) = pinned.or_else(under) {
            let _ = self.toggle_pin(n);
        }
    }

    /// Toggles the object numbered `number` in the open choice.
    pub fn toggle_pin(&mut self, number: usize) -> Result<(), String> {
        let choice = self.choice.as_mut().ok_or("no choice is open")?;
        let item = choice
            .items
            .iter_mut()
            .find(|i| i.number == number)
            .ok_or_else(|| format!("no object numbered {number}"))?;
        item.selected = !item.selected;
        let selected: Vec<usize> = choice
            .items
            .iter()
            .filter(|i| i.selected)
            .map(|i| i.number)
            .collect();
        self.log.event(
            "choice_toggled",
            json!({"number": number, "selected": selected}),
        );
        Ok(())
    }

    /// Enter: remove the chosen objects as one region.
    pub fn confirm(&mut self) {
        let Some(choice) = &self.choice else { return };
        let chosen: Vec<Item> = choice
            .items
            .iter()
            .filter(|i| i.selected)
            .cloned()
            .collect();
        if chosen.is_empty() {
            self.message = Some("Choose at least one object by its pin, or press Esc".into());
            return;
        }
        self.choice = None;
        let what = format!(
            "object{} {}",
            if chosen.len() == 1 { "" } else { "s" },
            chosen
                .iter()
                .map(|i| i.number.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let rasters = chosen.iter().filter_map(|i| i.raster.clone()).collect();
        let targets = chosen
            .into_iter()
            .map(|i| Target::Object {
                logits: i.logits,
                stroke: None,
            })
            .collect();
        self.start_removal(targets, rasters, None, what);
    }

    /// Escape: cancel the removal, the choice or the stroke in progress.
    pub fn cancel(&mut self) {
        if let Some(job) = self.job.take() {
            self.log.event("remove_cancelled", json!({"job": job.id}));
            self.message = Some("Removal cancelled; the image is unchanged".into());
        } else if self.choice.take().is_some() {
            self.log.event("choice_cancelled", json!({}));
            self.message = Some("Choice cancelled".into());
        } else if self.painting.take().is_some() || self.resolving.take().is_some() {
            self.message = Some("Stroke cancelled".into());
        } else {
            return;
        }
        self.mode_changed();
    }

    /// ⌘Z: back to the image before the last removal.
    pub fn undo(&mut self) {
        if self.job.is_some() || self.resolving.is_some() || self.painting.is_some() {
            self.message = Some("Busy; undo when it finishes, or Esc first".into());
            return;
        }
        let Some(previous) = self.undo.pop() else {
            self.message = Some("Nothing to undo".into());
            return;
        };
        self.image = Some(previous.clone());
        self.unsaved = true;
        self.log
            .event("undo", json!({"remaining": self.undo.len()}));
        self.message = Some("Undone".into());
        self.new_generation();
        workers::picture(self.generation, previous, self.out.clone());
    }

    /// ⌘S: write a PNG copy into the output directory.
    pub fn save(&mut self) {
        let Some(image) = self.image.clone() else {
            return;
        };
        let stem = self
            .config
            .image
            .file_stem()
            .map_or("image".into(), |s| s.to_string_lossy().into_owned());
        self.saving += 1;
        workers::save(image, self.config.out.clone(), stem, self.out.clone());
    }

    fn remove_object(&mut self, logits: Logits, raster: Option<Raster>) {
        self.start_removal(
            vec![Target::Object {
                logits,
                stroke: None,
            }],
            raster.into_iter().collect(),
            None,
            "the hovered object".into(),
        );
    }

    fn start_removal(
        &mut self,
        targets: Vec<Target>,
        rasters: Vec<Raster>,
        stroke: Option<Stroke>,
        what: String,
    ) {
        if let Readiness::Failed(e) = &self.filler {
            self.message = Some(format!("Cannot remove: the fill did not load: {e}"));
            return;
        }
        let Some(image) = self.image.clone() else {
            return;
        };
        self.next_job += 1;
        let id = self.next_job;
        self.log.event(
            "remove_started",
            json!({"job": id, "what": what, "targets": targets.len(), "fill": self.fill_name}),
        );
        self.fill.remove(RemoveRequest {
            job: id,
            image,
            targets,
            refine: self.config.refine.clone(),
        });
        self.hover = None;
        self.hover_floor = self.hover_seq;
        self.hover_answered = self.hover_seq;
        self.message = None;
        self.job = Some(Job {
            id,
            started: Instant::now(),
            rasters,
            stroke,
            what,
        });
    }

    // ---- What the window draws ----

    pub fn picture(&self) -> Option<&Picture> {
        self.picture.as_ref()
    }

    pub fn source_size(&self) -> Option<(u32, u32)> {
        self.image.as_ref().map(|i| (i.width, i.height))
    }

    pub fn hover(&self) -> Option<&Hover> {
        self.hover.as_ref()
    }

    pub fn choice(&self) -> Option<&Choice> {
        self.choice.as_ref()
    }

    pub fn job(&self) -> Option<&Job> {
        self.job.as_ref()
    }

    /// The stroke being painted, or the one being resolved.
    pub fn stroke(&self) -> Option<&Stroke> {
        self.painting.as_ref().or(self.resolving.as_ref())
    }

    pub fn painting(&self) -> bool {
        self.painting.is_some()
    }

    pub fn last_decision(&self) -> Option<&'static str> {
        self.last_decision
    }

    pub fn removals(&self) -> usize {
        self.removals
    }

    pub fn unsaved(&self) -> bool {
        self.unsaved
    }

    pub fn image(&self) -> Option<&Arc<Rgb8>> {
        self.image.as_ref()
    }

    /// Every raster the window may draw, so it can take new pixels and make their handles.
    pub fn rasters_mut(&mut self) -> impl Iterator<Item = &mut Raster> {
        let hover = self.hover.iter_mut().filter_map(|h| h.raster.as_mut());
        let choice = self
            .choice
            .iter_mut()
            .flat_map(|c| c.items.iter_mut().filter_map(|i| i.raster.as_mut()));
        let job = self.job.iter_mut().flat_map(|j| j.rasters.iter_mut());
        hover.chain(choice).chain(job)
    }

    pub fn picture_mut(&mut self) -> Option<&mut Picture> {
        self.picture.as_mut()
    }

    /// One line saying what the lab is doing and what the person can do.
    pub fn status(&self) -> String {
        let state = self.state_line();
        match &self.message {
            Some(m) => format!("{state}  ·  {m}"),
            None => state,
        }
    }

    fn state_line(&self) -> String {
        if let Some(e) = &self.load_error {
            return format!("Could not open {}: {e}", self.config.image.display());
        }
        if self.image.is_none() {
            return format!("Loading {}…", self.config.image.display());
        }
        if let Some(job) = &self.job {
            return format!("Removing {}… (Esc cancels)", job.what);
        }
        if let Some(choice) = &self.choice {
            let chosen = choice.items.iter().filter(|i| i.selected).count();
            return format!(
                "{} objects under the stroke: click to choose ({chosen} chosen), Enter removes, Esc cancels",
                choice.items.len()
            );
        }
        let analysing = match (&self.segmenter, &self.analysis) {
            (Readiness::Failed(e), _) => return format!("The segmenter did not load: {e}"),
            (_, Analysis::Failed(e)) => return format!("Analysis failed: {e}"),
            (Readiness::Loading, _) => Some("Loading the segmenter…"),
            (_, Analysis::Running { .. } | Analysis::Waiting) => Some("Analysing…"),
            (_, Analysis::Ready { .. }) => None,
        };
        if self.resolving.is_some() {
            return match analysing {
                Some(a) => format!("{a} the stroke is resolved when it finishes"),
                None => "Finding objects under the stroke…".into(),
            };
        }
        if self.brush_active() {
            let a = analysing.map_or(String::new(), |a| format!("{a} "));
            return format!(
                "{a}Brush {:.0} px: paint over what should go; [ and ] resize, B returns to hover",
                self.radius
            );
        }
        match analysing {
            Some(a) => format!("{a} hover waits for it; the brush (B or Shift) paints now"),
            None => {
                "Hover to highlight, click to remove · B or Shift: brush · ⌘Z undo · ⌘S save".into()
            }
        }
    }
}

#[cfg(test)]
mod tests;
