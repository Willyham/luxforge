//! One preview job on the preview worker: the proxy phase when the job has one, then the exact
//! phase, from one compilation of the job's stack at each stage it renders at.

use super::{
    ExactOutcome, MaskOverlayOutcome, MaskOverlayRequest, PhaseOutcome, PreviewJob, PreviewResult,
    ProxyOutcome, job::one_component, queue::PreviewTask,
};
use crate::{
    Cancel, Error, ErrorKind, ModuleRegistry, ProxyCache, ProxyKey, Recipe, Render, RenderContext,
    RenderOptions,
    activity::{ActivitySpec, Outcome},
    analysis::{MaskOverlay, MaskPixels},
    latest::Running,
    mask::CompiledMask,
    modules::Stage,
    render,
};
use std::time::Instant;

/// What the proxy phase of one job should do. Decided on the worker, which owns the proxy cache,
/// at the start of the job.
enum ProxyStep {
    /// The job asked for no proxy phase.
    Skipped,
    /// The job asked, and this is why it has none.
    Declined(String),
    /// Render against the proxy source this key names, from the cache or built on a miss.
    Planned(ProxyKey),
}

/// Whether this job has a proxy phase, and against which source.
///
/// Cost is `O(layers)`: `proxy_eligible` reads stages, the plan reads the output stage of the job's
/// exact compilation, and the window compiles the stack once at the proxy stage to walk back what
/// its output reads. None of them reads a pixel. It runs on the preview worker, as does building
/// the proxy itself.
fn plan_proxy(job: &PreviewJob, exact: &Result<Render<'_>, Error>) -> ProxyStep {
    let Some(bounds) = job.proxy else {
        return ProxyStep::Skipped;
    };
    if job.layer_count.is_some() {
        // A truncated job renders a layer prefix, and the plan describes the whole stack's output
        // stage, so the prefix has no proxy phase at all.
        return ProxyStep::Declined(
            "a truncated preview renders a layer prefix, which has no proxy phase".into(),
        );
    }
    if let Err(error) = job.registry.proxy_eligible(&job.recipe) {
        return ProxyStep::Declined(error.detail);
    }
    match exact.as_ref().map(|exact| exact.proxy_plan(bounds)) {
        Ok(Some(plan)) => ProxyStep::Planned(ProxyKey {
            identity: job.source.identity(),
            // A cropped stack's proxy holds only the window of the proxy stage its output reads,
            // so its size follows the display bounds and not the crop's tightness.
            plan: exact
                .as_ref()
                .map(|exact| exact.proxy_window(&job.registry, &job.recipe, plan))
                .unwrap_or(plan),
        }),
        Ok(None) => ProxyStep::Declined(
            "the proxy scale is 1: the stage already fits the display bounds".into(),
        ),
        Err(error) => ProxyStep::Declined(error.detail.clone()),
    }
}

/// One preview job, on the preview worker: the proxy phase when the job has one, handed over as
/// soon as it is rendered, then the exact phase, returned as the job's last result.
///
/// The proxy phase reads the job's `abandoned` token and the exact phase its `superseded` one, so
/// a drag keeps presenting proxy frames while the full-resolution renders behind them are
/// abandoned.
pub(super) fn run(
    cache: &mut ProxyCache,
    task: PreviewTask,
    running: &Running<'_, PreviewTask, PreviewResult>,
) -> Option<PreviewResult> {
    let PreviewTask {
        job,
        board,
        requested_at,
    } = task;
    let queue_wait_ms = requested_at.map(|requested| requested.elapsed().as_secs_f64() * 1000.0);
    let generation = running.generation();
    let (proxy_cancel, exact_cancel) = (running.abandoned(), running.superseded());
    // One activity spans both phases. A job abandoned mid-way, its results stale before its exact
    // phase could be handed over, drops the guard, which records it as cancelled.
    let activity = board.map(|board| {
        board.begin(ActivitySpec {
            kind: "preview.render",
            label: "Rendering preview",
            detail: None,
            asset_id: Some(job.entry.asset_id.clone()),
            job_id: None,
        })
    });
    let entry_id = job.entry.id.clone();
    let draft_revision = job.draft_revision;
    let snapshot_id = job.entry.snapshot.id.clone();
    // Both phases of a job share its source, so both are approximate or neither is. An approximate
    // frame is never reduced, which is the rule on `PreviewJob::analyse`.
    let approximate_white_balance = job.source.approximate_white_balance();
    let analyse = job.analyse && !approximate_white_balance;
    // A truncated job copies the layer prefix only; the whole stack is rendered in place.
    let prefix = job.layer_count.map(|count| Recipe {
        format: job.recipe.format,
        layers: job.recipe.layers.iter().take(count).cloned().collect(),
        // The mask table belongs to the recipe, not to the prefix: a truncated stack keeps it so a
        // masked layer inside the prefix still finds the mask it names.
        masks: job.recipe.masks.clone(),
        strokes: job.recipe.strokes.clone(),
        artifacts: job.recipe.artifacts.clone(),
    });
    let recipe = prefix.as_ref().unwrap_or(&job.recipe);

    // The job's one compilation at the exact stage. The proxy plan reads its output stage, the
    // exact phase renders it and the coverage grid composes its geometry tail, so none of them
    // compiles the stack again. It is charged to the first phase that hands over a frame.
    //
    // The coverage grid reads no pixel of the exact frame — the geometry tail of this compilation,
    // and for a mask that reads pixels, point queries into the input of its first bound layer — so
    // it rides the first frame the job hands over: the proxy when there is one, whose phase it is
    // filled in under the proxy's own token, and otherwise the exact frame, as the job's one phase.
    // Either way it is this one function over this one compilation, so the grid is the same bytes
    // whichever phase carries it.
    let compile_started = Instant::now();
    let exact = render(
        &job.registry,
        job.source.input(),
        recipe,
        RenderOptions::exact(exact_cancel),
        &job.context,
    );
    let mut compile_ms = Some(milliseconds_since(compile_started));

    // Nothing in the proxy phase is fatal. A plan, a build or a render that fails — including a
    // cancel — records its reason on the exact result and the exact phase runs as it always does,
    // so a job never loses its frame because the shortcut did not work out. Nothing is logged.
    // The proxy phase's own clock: the plan, the build when this job builds, then the render.
    let started = Instant::now();
    // Whether a proxy frame has already carried the job's coverage grid, so the exact phase does
    // not fill it a second time.
    let mut overlay_delivered = false;
    let declined = match plan_proxy(&job, &exact) {
        ProxyStep::Skipped => None,
        ProxyStep::Declined(reason) => Some(reason),
        ProxyStep::Planned(key) => {
            if let Some(activity) = &activity {
                activity.phase("proxy");
            }
            // The cache holds pixels; the settings a RAW development layer asks for come from this
            // job's recipe, so a drafted exposure renders against the cached planes.
            let built = match cache.get(&key) {
                Some(cached) => Ok((cached.with_settings_of(&job.source), false)),
                None => job.source.proxy(key.plan).map(|source| (source, true)),
            };
            match built {
                Err(error) => Some(error.detail),
                Ok((source, fresh)) => {
                    // The source this frame is rendered against: the proxy stage's window when the
                    // plan has one, and the whole proxy stage otherwise.
                    let dimensions = source.dimensions();
                    // The proxy stage's one compilation: the frame and the reason it is
                    // approximate both come from it, so what is reported and what is drawn cannot
                    // disagree. A windowed one is cut from it, and asks the job's exact
                    // compilation for any spatial estimate the window cannot reduce.
                    let rendered = exact
                        .as_ref()
                        .map_err(Clone::clone)
                        .and_then(|exact| {
                            exact.render_proxy(
                                &job.registry,
                                source.input(),
                                &job.recipe,
                                key.plan,
                                proxy_cancel,
                                &job.context,
                            )
                        })
                        .and_then(|proxy| {
                            Ok((proxy.frame(snapshot_id.clone())?, proxy.approximation()))
                        });
                    // The proxy this job built belongs to the worker whether or not its frame is
                    // still wanted: the next job at the same bounds is a hit either way.
                    if fresh {
                        cache.insert(key, source);
                    }
                    match rendered {
                        Err(error) => Some(error.detail),
                        Ok((raster, proxy_approximation)) => {
                            let mask_overlay = match (&exact, &job.mask_overlay) {
                                (Ok(exact), Some(request)) => {
                                    overlay_delivered = true;
                                    mask_overlay_for(
                                        &job.registry,
                                        exact,
                                        recipe,
                                        request,
                                        proxy_cancel,
                                        &job.context,
                                    )
                                }
                                _ => MaskOverlayOutcome::default(),
                            };
                            let proxy = PreviewResult {
                                generation,
                                entry_id: entry_id.clone(),
                                identity: job.identity.clone(),
                                draft_revision,
                                // A proxy raster is never reduced: every number the histogram and
                                // the clipping counters report is the exact phase's (performance
                                // rule 11). The mask's coverage grid is not such a number: it is
                                // a function of position over the exact output stage, which this
                                // job's exact compilation already knows, so it arrives here.
                                outcome: PhaseOutcome::Proxy(ProxyOutcome {
                                    raster,
                                    dimensions,
                                    built: fresh,
                                    // Read from the compilation at exactly the dimensions this
                                    // frame was rendered against, because whether a mask draws a
                                    // feature the proxy's pixel grid can resolve is a fact about
                                    // that grid.
                                    approximation: proxy_approximation,
                                    mask_overlay,
                                }),
                                approximate_white_balance,
                                render_ms: compile_ms.take().unwrap_or(0.0)
                                    + milliseconds_since(started),
                                queue_wait_ms,
                            };
                            // A proxy nobody will ever see — the queue was cancelled or dropped —
                            // means the exact phase is not wanted either.
                            if !running.send(proxy) {
                                return None;
                            }
                            None
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
            match crate::analysis::reduce_raster_cancellable(&raster, exact_cancel) {
                Ok(report) => (Ok(raster), Some(report)),
                Err(error) if error.kind == ErrorKind::Cancelled => (Err(error), None),
                Err(_) => (Ok(raster), None),
            }
        }
        rendered => (rendered, None),
    };
    // A job with no proxy frame fills its coverage grid here, beside the frame it describes, so the
    // two travel together under one generation. It reads no pixel of that frame and allocates one
    // byte per display cell; a mask that reads pixels reads them from the input of its own first
    // bound layer instead, one point query per cell.
    let mask_overlay = match (&result, &exact, &job.mask_overlay) {
        (Ok(_), Ok(exact), Some(request)) if !overlay_delivered => mask_overlay_for(
            &job.registry,
            exact,
            recipe,
            request,
            exact_cancel,
            &job.context,
        ),
        _ => MaskOverlayOutcome::default(),
    };
    let render_ms = compile_ms.unwrap_or(0.0) + milliseconds_since(started);
    // A superseded or abandoned exact phase answers `Cancelled`, so its activity ends cancelled.
    if let Some(activity) = activity {
        activity.finish(Outcome::of(&result));
    }
    Some(PreviewResult {
        generation,
        entry_id,
        identity: job.identity,
        draft_revision,
        outcome: PhaseOutcome::Exact(Box::new(ExactOutcome {
            result,
            report,
            mask_overlay,
            proxy_declined: declined,
        })),
        approximate_white_balance,
        render_ms,
        queue_wait_ms,
    })
}

/// One mask's coverage grid over the exact output stage of `frame`, the job's one compilation of
/// `recipe` at the exact stage.
///
/// `recipe` is the stack that is rendered — a truncated job's prefix, when it had one — because the
/// grid describes the frame it arrives with and a prefix has its own geometry tail. The mask table
/// travels with a prefix, so a mask is still found there. It reads no pixel of the exact frame, so
/// it is the same grid whether the proxy phase carries it or the exact one does.
///
/// Every reason there is no grid is a reason there is none to draw, never a silently empty one, and
/// the reason travels with the frame in [`MaskOverlayOutcome::absent`] — the host's own words, for a
/// client that asked for an overlay and would otherwise wait for a texture nothing will fill. The
/// mask or component the request named was validated against this stack when the job was planned,
/// and the stack compiled and rendered a frame, so compiling it cannot fail here for a reason the
/// frame did not already fail for. Two absences carry **no** reason on purpose: a mask with nothing
/// to describe, which [`crate::analysis::coverage_grid`] decides in closed form and which a grid of
/// zeros would misreport, and a cancel, where a newer request is already on its way with its own
/// grid and waiting for it is correct.
pub(super) fn mask_overlay_for(
    registry: &ModuleRegistry,
    frame: &Render<'_>,
    recipe: &Recipe,
    request: &MaskOverlayRequest,
    cancel: &Cancel,
    context: &RenderContext,
) -> MaskOverlayOutcome {
    let refused = |error: Error| MaskOverlayOutcome {
        grid: None,
        absent: match error.kind {
            ErrorKind::Cancelled => None,
            _ => Some(error.detail),
        },
    };
    let absent = |reason: String| MaskOverlayOutcome {
        grid: None,
        absent: Some(reason),
    };
    let Some(held) = recipe.masks.iter().find(|mask| mask.id == request.mask) else {
        return absent(format!(
            "mask {} is not in the stack this frame was rendered from",
            request.mask
        ));
    };
    let derived;
    let mask = match &request.component {
        None => held,
        Some(component) => match one_component(held, component) {
            Some(one) => {
                derived = one;
                &derived
            }
            None => {
                return absent(format!("mask {} holds no component {component}", held.name));
            }
        },
    };
    // `O(layers)`: it composes the geometry tail of the stack already compiled for this frame and
    // reads no pixel.
    let transform = match frame.transform() {
        Ok(transform) => transform,
        Err(error) => return refused(error),
    };
    let stage = Stage {
        width: transform.content.width,
        height: transform.content.height,
    };
    let compiled = match CompiledMask::new(mask, stage, &recipe.strokes) {
        Ok(compiled) => compiled,
        Err(error) => return refused(error),
    };
    // A value-based component is answered on the pixel the masked operation receives, which is the
    // input of the mask's **first bound layer** — the rule `mask::commands::input_layer_index`
    // states once for everything that reads a pixel through a mask, and which the colour-constrained
    // brush's seed and `mask.sample-input` already read, so the overlay and the seed cannot disagree
    // about which pixel a mask reads. The prefix is compiled once and asked once per cell.
    let input;
    let unavailable;
    let pixels = if !compiled.reads_pixels() {
        // Position-only: no operation is needed and none is looked for, so a geometric grid costs
        // exactly what it did before a value-based component existed.
        MaskPixels::Unavailable("this mask reads no pixel")
    } else {
        match crate::mask::commands::input_layer_index(recipe, &request.mask).and_then(|layer| {
            crate::render::layer_input(registry, frame.source(), recipe, layer, context)
        }) {
            // Two different stages would be two different coverage fields, and `coverage_grid`
            // refuses that mismatch for the frame; it is refused here for the operation, in the same
            // voice, rather than read at coordinates of another stage.
            Ok(prefix) if prefix.stage() != stage => {
                unavailable = format!(
                    "the masked operation receives a {}x{} stage and this mask is compiled against \
                     {}x{}",
                    prefix.stage().width,
                    prefix.stage().height,
                    stage.width,
                    stage.height
                );
                MaskPixels::Unavailable(&unavailable)
            }
            Ok(prefix) => {
                input = prefix;
                MaskPixels::Input(&input)
            }
            // No layer is bound to this mask, or its prefix holds a spatial layer, or it does not
            // compile: in every case there is no operation whose input this grid can read, and the
            // refusal's own sentence says which and what to do about it.
            Err(error) => {
                unavailable = error.detail;
                MaskPixels::Unavailable(&unavailable)
            }
        }
    };
    let coverage = match crate::analysis::coverage_grid(
        &compiled,
        &transform,
        request.cells_w,
        request.cells_h,
        pixels,
        cancel,
    ) {
        Ok(Some(coverage)) => coverage,
        Ok(None) => return MaskOverlayOutcome::default(),
        Err(error) => return refused(error),
    };
    MaskOverlayOutcome {
        grid: Some(MaskOverlay {
            mask: request.mask.clone(),
            component: request.component.clone(),
            cells_w: request.cells_w,
            cells_h: request.cells_h,
            coverage,
        }),
        absent: None,
    }
}

/// Wall-clock milliseconds since `started`, as [`PreviewResult::render_ms`] reports them.
fn milliseconds_since(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}
