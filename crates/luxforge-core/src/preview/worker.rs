//! One preview job on the preview worker: the proxy phase when the job has one, then the exact
//! phase, from one compilation of the job's stack at each stage it renders at.

use super::{
    ExactOutcome, PhaseOutcome, PreviewIntent, PreviewJob, PreviewResult, ProxyOutcome,
    queue::{ExactProgress, PreviewTask},
};
use crate::{
    Cancel, Error, ErrorKind, ProxyCache, ProxyKey, Recipe, Render, RenderOptions,
    activity::{Activity, ActivitySpec, Outcome},
    cancel::{ProgressCounts, RenderProgress},
    latest::Running,
    render,
    render::ProxyStage,
};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// How long an exact phase runs before its progress wakes the consumer. A phase that ends sooner
/// wakes it once, with its result, as it always has; one past this is long enough that the
/// consumer may show how far it has got.
pub const PROGRESS_QUIET: Duration = Duration::from_millis(250);
/// The least time between two progress wakes of one exact phase, so a render of many quick
/// tiles wakes the consumer at most twenty times a second.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

/// The progress meter of one job's exact phase: it reports each finished tile's fraction to the
/// job's activity and, once the phase has run [`PROGRESS_QUIET`], wakes the consumer at most once
/// per [`PROGRESS_INTERVAL`]. It wakes nothing before `phase` is set, when the exact phase starts.
pub(super) fn exact_meter(
    activity: Option<&Activity>,
    waker: Option<crate::latest::Wake>,
    phase: Arc<OnceLock<Instant>>,
) -> RenderProgress {
    let report = activity.map(Activity::reporter);
    // Milliseconds into the phase of the last wake, plus one, so zero means none yet.
    let woken = AtomicU64::new(0);
    RenderProgress::new(move |counts: ProgressCounts| {
        if let (Some(report), Some(fraction)) = (&report, counts.fraction()) {
            report(fraction);
        }
        let Some(started) = phase.get() else {
            return;
        };
        let elapsed = started.elapsed();
        if elapsed < PROGRESS_QUIET {
            return;
        }
        let now = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX - 1) + 1;
        let last = woken.load(Ordering::Relaxed);
        if last != 0 && now.saturating_sub(last) < PROGRESS_INTERVAL.as_millis() as u64 {
            return;
        }
        woken.store(now, Ordering::Relaxed);
        if let Some(waker) = &waker {
            waker();
        }
    })
}

/// What the proxy phase of one job should do. Decided on the worker, which owns the proxy cache,
/// at the start of the job.
enum ProxyStep {
    /// The job asked for no proxy phase.
    Skipped,
    /// The job asked, and this is why it has none.
    Declined(String),
    /// Render the stack's one compilation at the proxy stage against the proxy source this key
    /// names, from the cache or built on a miss.
    Planned(ProxyKey, ProxyStage),
}

/// Whether this job has a proxy phase, and against which source.
///
/// `recipe` is the stack the job renders and `exact` its one compilation at the exact stage: the
/// whole stack, or for a truncated job its layer prefix, which is planned exactly as a whole stack
/// of those layers would be. A crop draft's input stage is such a prefix, and the prefix before a
/// crop holds no crop, so its proxy is the whole proxy stage. The cache key is the source's
/// identity and the plan: it holds downscaled source pixels, never a rendered stack, so a prefix
/// and a whole stack that plan the same proxy share its pixels correctly, and ones that plan
/// different proxies have different keys.
///
/// Cost is `O(layers)`: `proxy_eligible` reads stages, the plan reads the output stage of the job's
/// exact compilation, and the window compiles the stack once at the proxy stage, which the proxy
/// frame then renders, to walk back what its output reads. None of them reads a pixel. It runs on
/// the preview worker, as does building the proxy itself.
fn plan_proxy(job: &PreviewJob, recipe: &Recipe, exact: &Result<Render<'_>, Error>) -> ProxyStep {
    // A proxy is a moving frame's: a draft's tick the GPU does not draw, or a crop draft's input
    // stage. A job at rest renders none: its picture is the GPU's, and the reference frame it hands
    // over is its exact frame reduced to the view ([`view_frame`]).
    if job.intent != PreviewIntent::Interactive {
        return ProxyStep::Skipped;
    }
    let Some(bounds) = job.proxy else {
        return ProxyStep::Skipped;
    };
    let evaluation = &job.evaluation;
    if let Err(error) = evaluation.registry().proxy_eligible(recipe) {
        return ProxyStep::Declined(error.detail);
    }
    let exact = match exact {
        Ok(exact) => exact,
        Err(error) => return ProxyStep::Declined(error.detail.clone()),
    };
    match exact.proxy_plan(bounds) {
        Some(plan) => {
            // A cropped stack's proxy holds only the window of the proxy stage its output reads,
            // so its size follows the display bounds and not the crop's tightness.
            let stage = exact.proxy_window(evaluation.registry(), recipe, plan);
            ProxyStep::Planned(
                ProxyKey {
                    identity: evaluation.source().identity(),
                    plan: stage.plan(),
                },
                stage,
            )
        }
        None => ProxyStep::Declined(
            "the proxy scale is 1: the stage already fits the display bounds".into(),
        ),
    }
}

/// One preview job, on the preview worker: an interactive job's proxy phase, handed over as soon
/// as it is rendered and ending the job, or else the exact phase — with its frame reduced to the
/// view's bounds when the job names them ([`view_frame`]) — returned as the job's last result. An
/// interactive job whose proxy declines takes the exact phase instead.
///
/// The proxy phase reads the job's `abandoned` token and the exact phase its `superseded` one, so
/// a drag keeps presenting proxy frames while the full-resolution renders behind them are
/// abandoned.
pub(super) fn run(
    cache: &mut ProxyCache,
    progress: &ExactProgress,
    task: PreviewTask,
    running: &Running<'_, PreviewTask, PreviewResult>,
) -> Option<PreviewResult> {
    if task.job.intent == PreviewIntent::Reduce {
        return Some(run_reduce(task, running));
    }
    let PreviewTask {
        job,
        board,
        requested_at,
    } = task;
    let queue_wait_ms = requested_at.map(|requested| requested.elapsed().as_secs_f64() * 1000.0);
    let generation = running.generation();
    let evaluation = &job.evaluation;
    // One activity spans both phases. A job abandoned mid-way, its results stale before its exact
    // phase could be handed over, drops the guard, which records it as cancelled.
    let activity = board.map(|board| {
        board.begin(ActivitySpec {
            kind: "preview.render",
            label: "Rendering preview",
            detail: None,
            asset_id: Some(evaluation.entry().asset_id.clone()),
            job_id: None,
        })
    });
    let (proxy_cancel, exact_cancel) = (running.abandoned(), running.superseded());
    // The exact phase reports its spatial tiles on its own token. An interactive job has no exact
    // phase, and its full-resolution compilation reads the proxy phase's token.
    let phase = Arc::new(OnceLock::new());
    let meter = (job.intent != PreviewIntent::Interactive)
        .then(|| exact_meter(activity.as_ref(), running.waker(), phase.clone()));
    let metered: Cancel;
    let full_cancel = match &meter {
        None => proxy_cancel,
        Some(meter) => {
            metered = exact_cancel.with_progress(meter);
            &metered
        }
    };
    let entry_id = evaluation.entry().id.clone();
    let draft_revision = evaluation.draft_revision();
    let snapshot_id = evaluation.entry().snapshot.id.clone();
    // Both phases of a job share its source, so both are approximate or neither is. An approximate
    // frame is never reduced, which is the rule on `PreviewJob::analyse`.
    let approximate_white_balance = evaluation.source().approximate_white_balance();
    let analyse = job.analyse && !approximate_white_balance;
    // A truncated job copies the layer prefix only; the whole stack is rendered in place.
    let whole = evaluation.recipe();
    let prefix = job.layer_count.map(|count| Recipe {
        format: whole.format,
        layers: whole.layers.iter().take(count).cloned().collect(),
        // The mask table belongs to the recipe, not to the prefix: a truncated stack keeps it so a
        // masked layer inside the prefix still finds the mask it names.
        masks: whole.masks.clone(),
        strokes: whole.strokes.clone(),
        artifacts: whole.artifacts.clone(),
    });
    let recipe = prefix.as_ref().unwrap_or(whole);

    // The job's one compilation at the exact stage: the one its evaluation made on the catalog
    // owner, which the whole stack renders from, or the layer prefix's own for a truncated job.
    // The proxy plan reads its output stage and the exact phase renders it, so neither compiles
    // the stack again. It is charged to the first phase that hands over a frame.
    let compile_started = Instant::now();
    let exact = match &prefix {
        Some(prefix) => render(
            evaluation.registry(),
            evaluation.source().input(),
            prefix,
            RenderOptions::exact(full_cancel),
            evaluation.context(),
        ),
        None => evaluation.exact(full_cancel),
    };
    let mut compile_ms = Some(milliseconds_since(compile_started));

    // Nothing in the proxy phase is fatal. A plan, a build or a render that fails — including a
    // cancel — records its reason on the exact result and the exact phase runs as it always does,
    // so a job never loses its frame because the shortcut did not work out. Nothing is logged.
    // The proxy phase's own clock: the plan, the build when this job builds, then the render.
    let started = Instant::now();
    let declined = match plan_proxy(&job, recipe, &exact) {
        ProxyStep::Skipped => None,
        ProxyStep::Declined(reason) => Some(reason),
        ProxyStep::Planned(key, stage) => {
            if let Some(activity) = &activity {
                activity.phase("proxy");
            }
            // The cache holds pixels; the settings a RAW development layer asks for come from this
            // job's recipe, so a drafted exposure renders against the cached planes. The proxy this
            // job builds belongs to the worker whether or not its frame is still wanted: the next
            // job at the same bounds is a hit either way.
            let built = cache.source_for(&key, evaluation.source(), || {
                evaluation
                    .source()
                    .proxy_cancellable(key.plan, proxy_cancel)
            });
            match built {
                Err(error) => Some(error.detail),
                Ok((source, fresh)) => {
                    // The source this frame is rendered against: the proxy stage's window when the
                    // plan has one, and the whole proxy stage otherwise.
                    let dimensions = source.dimensions();
                    // The proxy stage's one compilation, the one the plan made: the frame and the
                    // reason it is approximate both come from it, so what is reported and what is
                    // drawn cannot disagree. A windowed one is cut from it, and asks the job's
                    // exact compilation for any spatial estimate the window cannot reduce.
                    let proxied = exact.as_ref().map_err(Clone::clone).and_then(|exact| {
                        exact.render_proxy(
                            source.input(),
                            stage,
                            proxy_cancel,
                            evaluation.context(),
                        )
                    });
                    let rendered = proxied.as_ref().map_err(Clone::clone).and_then(|proxy| {
                        Ok((proxy.frame(snapshot_id.clone())?, proxy.approximation()))
                    });
                    match rendered {
                        Err(error) => Some(error.detail),
                        Ok((raster, proxy_approximation)) => {
                            let proxy = PreviewResult {
                                generation,
                                entry_id: entry_id.clone(),
                                identity: job.identity.clone(),
                                draft_revision,
                                intent: job.intent,
                                // A proxy raster is never reduced: every number the histogram and
                                // the clipping counters report is the exact phase's (performance
                                // rule 11).
                                outcome: PhaseOutcome::Proxy(ProxyOutcome {
                                    raster,
                                    dimensions,
                                    built: fresh,
                                    // Read from the compilation at exactly the dimensions this
                                    // frame was rendered against, because whether a mask draws a
                                    // feature the proxy's pixel grid can resolve is a fact about
                                    // that grid.
                                    approximation: proxy_approximation,
                                }),
                                approximate_white_balance,
                                render_ms: compile_ms.take().unwrap_or(0.0)
                                    + milliseconds_since(started),
                                queue_wait_ms,
                            };
                            // The proxy is an interactive job's one phase. One nobody will ever
                            // see — the queue was cancelled or dropped — leaves the activity
                            // cancelled.
                            if running.send(proxy)
                                && let Some(activity) = activity
                            {
                                activity.finish(Outcome::Completed);
                            }
                            return None;
                        }
                    }
                }
            }
        }
    };

    if let Some(activity) = &activity {
        activity.phase("exact");
    }
    // The exact phase's own clock starts here, after the proxy phase has handed over its frame, so
    // the two phases' times never overlap and neither includes the other.
    let started = Instant::now();
    let _published = meter.as_ref().map(|meter| {
        let _ = phase.set(started);
        progress.begin(generation, started, meter)
    });
    let rendered = exact
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|exact| exact.frame(snapshot_id));
    // The histogram is reduced from the frame this worker just produced, in place and without a
    // second render or a copy. A failed reduction leaves no report rather than reporting zeroes; a
    // cancelled one means the job was superseded mid-reduce, and the frame it describes is as stale
    // as the reduction, so the phase answers cancelled rather than a frame nothing will adopt.
    let (result, report) = match rendered {
        Ok(raster) if analyse => {
            match crate::analysis::reduce(&raster.rgba, raster.width, raster.height, full_cancel) {
                Ok(report) => (Ok(raster), Some(report)),
                Err(error) if error.kind == ErrorKind::Cancelled => (Err(error), None),
                Err(_) => (Ok(raster), None),
            }
        }
        rendered => (rendered, None),
    };
    let display = view_frame(&job, &result, full_cancel);
    let (result, report, display) = match display {
        Ok(display) => (result, report, display),
        Err(error) => (Err(error), None, None),
    };
    let render_ms = compile_ms.unwrap_or(0.0) + milliseconds_since(started);
    drop(exact);
    // A superseded or abandoned exact phase answers `Cancelled`, so its activity ends cancelled.
    if let Some(activity) = activity {
        activity.finish(Outcome::of(&result));
    }
    let exact_result = PreviewResult {
        generation,
        entry_id,
        identity: job.identity,
        draft_revision,
        intent: job.intent,
        outcome: PhaseOutcome::Exact(Box::new(ExactOutcome {
            display,
            result,
            report,
            proxy_declined: declined,
        })),
        approximate_white_balance,
        render_ms,
        queue_wait_ms,
    };
    Some(exact_result)
}

/// Wall-clock milliseconds since `started`, as [`PreviewResult::render_ms`] reports them.
fn milliseconds_since(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// The exact phase's whole frame reduced to the view's bounds ([`crate::render::reduce_to_view`]):
/// the frame the reference renderer draws where the view draws the stage smaller than it is, for
/// every whole-stack job that names its view; none for a layer prefix, an interactive job, an
/// approximate white balance, or a stage that already fits the bounds.
pub(super) fn view_frame(
    job: &PreviewJob,
    result: &Result<crate::Raster, Error>,
    cancel: &crate::Cancel,
) -> Result<Option<crate::Raster>, Error> {
    if job.layer_count.is_some()
        || job.intent == PreviewIntent::Interactive
        || job.evaluation.source().approximate_white_balance()
    {
        return Ok(None);
    }
    let (Some(bounds), Ok(raster)) = (job.proxy, result) else {
        return Ok(None);
    };
    crate::render::reduce_to_view(raster, bounds, cancel)
}

/// A resized Fit reuses the immutable exact allocation and performs only bounded reduction.
fn run_reduce(
    task: PreviewTask,
    running: &Running<'_, PreviewTask, PreviewResult>,
) -> PreviewResult {
    let started = Instant::now();
    let queue_wait_ms = task
        .requested_at
        .map(|at| at.elapsed().as_secs_f64() * 1000.0);
    let job = task.job;
    let display = job
        .reduce
        .as_ref()
        .ok_or_else(|| Error::validation("reduce-only job needs an exact raster"))
        .and_then(|raster| {
            if job.layer_count.is_some()
                || job.evaluation.source().approximate_white_balance()
                || raster.snapshot_id != job.evaluation.entry().snapshot.id
                || raster.source_fingerprint != job.evaluation.source().fingerprint()
                || (raster.width, raster.height) != (job.identity.width, job.identity.height)
            {
                return Err(Error::validation(
                    "reduce-only pixels do not match the whole evaluated image",
                ));
            }
            let bounds = job
                .proxy
                .ok_or_else(|| Error::validation("reduce-only job needs Fit bounds"))?;
            crate::render::reduce_to_view(raster, bounds, running.superseded())
        });
    let (result, display) = match display {
        Ok(display) => (
            Ok((**job.reduce.as_ref().expect("validated exact raster")).clone()),
            display,
        ),
        Err(error) => (Err(error), None),
    };
    PreviewResult {
        generation: running.generation(),
        entry_id: job.evaluation.entry().id.clone(),
        identity: job.identity,
        draft_revision: job.evaluation.draft_revision(),
        intent: job.intent,
        approximate_white_balance: job.evaluation.source().approximate_white_balance(),
        outcome: PhaseOutcome::Exact(Box::new(ExactOutcome {
            display,
            result,
            report: None,
            proxy_declined: None,
        })),
        render_ms: milliseconds_since(started),
        queue_wait_ms,
    }
}
