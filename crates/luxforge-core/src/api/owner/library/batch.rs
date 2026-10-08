//! `batch.apply-preset` and `batch.export` on the owner. Each resolves its photographs and checks
//! its request on the owner, then runs as one job on the lane's worker
//! ([`LibraryLane`](super::LibraryLane)) that takes one photograph at a time through
//! [`JobContext::commit`], so a cancel between photographs keeps every one finished and does nothing
//! more:
//!
//! - **Apply preset**: each photograph on the owner through exactly the path `edit.apply-preset`
//!   takes, the module action against the photograph's current revision, announced as that call
//!   announces it.
//! - **Export**: each photograph's current entry planned on the owner as `export.jpeg` plans it,
//!   then rendered, encoded and published on the worker through the export's own steps
//!   ([`export::write`]), streamed through the owner's tile service with the same reference
//!   fallback, one photograph at a time, into the chosen folder under the export's naming rule;
//!   each written file is announced as a single export's is.
//!
//! A photograph whose stack needs a prepared source is prepared through the owner's one preparation
//! path first, as a client's request would be, and the worker waits for that off the owner. The
//! library side is `crate::library::batch`.
use super::{Call, JobContext, Owner, Task, selected};
use crate::api::owner::SYSTEM_CLIENT;
use crate::{
    AssetId, Error, ErrorKind, JobId, JobStatus, Mutation, MutationOutcome,
    api::{ClientId, Origin, announce_once, methods::value, owner::export},
    catalog_types::{
        BatchWritten,
        api::{BatchApplyPreset, BatchExport, BatchPasteSettings},
        jobs::{BATCH_EXPORT, BATCH_PASTE, BATCH_PRESET, CatalogJob},
    },
    editor::{
        ExportPlan, library_rows,
        pixels::{DeferredRead, PixelAnswer, PixelMemo, Replay},
    },
    library::{
        batch::{self, Applied, Progress, SettingsApply},
        locate::Phase,
        targets,
    },
    modules::PASTE_SETTINGS,
};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{Receiver, sync_channel},
    },
};

/// How often one photograph's source is prepared before it is left out as still needing
/// preparation: another client's preparation may replace it in the editor's one prepared source
/// before the photograph is asked for again.
const PREPARATIONS: usize = 3;

/// `batch.apply-preset`: read the preset from the library once, then start a `batch-preset` job
/// that applies it to each photograph `targets` names as its own history entry, answering a
/// [`BatchReport`](crate::catalog_types::BatchReport).
pub(in crate::api) fn batch_apply_preset(
    owner: &mut Owner,
    call: &Call<'_>,
    params: BatchApplyPreset,
) -> Result<Value, Error> {
    let assets = targets::assets(&owner.service, &params.targets, || {
        selected(owner, call.client)
    })?;
    let (record, _) = owner.service.preset(&params.preset_id)?;
    let preset = Arc::new(SettingsApply::new(
        record,
        params.mutation.request_id,
        params.mutation.actor,
    ));
    queue_settings(owner, call, assets, preset, &BATCH_PRESET)
}

/// Validate the inline set once, before publishing a job or changing any photograph.
pub(in crate::api) fn batch_paste_settings(
    owner: &mut Owner,
    call: &Call<'_>,
    params: BatchPasteSettings,
) -> Result<Value, Error> {
    crate::presets::validate_settings(owner.service.registry(), &params.settings)?;
    let mut parameters = serde_json::json!({"settings": params.settings, "source": params.source});
    if let Some(asset) = params.source_asset_id {
        parameters["source-asset"] = Value::String(asset);
    }
    let (module, action) = owner
        .service
        .registry()
        .action(PASTE_SETTINGS)
        .ok_or_else(|| Error::validation("paste-settings is unavailable"))?;
    module.descriptor().check_available()?;
    let checked = crate::check_parameters(action, &parameters)?;
    let input = module.parse(PASTE_SETTINGS, &checked)?;
    let parameters = Value::Object(input.parameters);
    let source = parameters["source"].as_str().unwrap_or_default().to_owned();
    let assets = targets::assets(&owner.service, &params.targets, || {
        selected(owner, call.client)
    })?;
    let paste = Arc::new(SettingsApply::action(
        PASTE_SETTINGS,
        source,
        parameters,
        params.mutation.request_id,
        params.mutation.actor,
    ));
    queue_settings(owner, call, assets, paste, &BATCH_PASTE)
}

fn queue_settings(
    owner: &mut Owner,
    call: &Call<'_>,
    assets: Vec<(AssetId, crate::catalog_types::AssetRowId)>,
    preset: Arc<SettingsApply>,
    kind: &'static CatalogJob,
) -> Result<Value, Error> {
    let (client, origin) = (call.client, call.origin.clone());
    let tiles = Arc::clone(&owner.tiles);
    let task = Task {
        job_id: JobId::new(),
        job: kind,
        asset_id: only(&assets),
        detail: Some(format!("{} · {}", preset.name, photographs(assets.len()))),
        origin: call.origin.clone(),
        work: Box::new(move |job| {
            let mut progress = Progress::new(job.control, assets.len());
            for (asset, _) in assets {
                job.pause(Phase::NextPhotograph);
                let (preset, origin, target) = (preset.clone(), origin.clone(), asset.clone());
                let applied = apply_on_worker(job, client, &tiles, origin, target, preset);
                match applied {
                    Ok(Ok(Applied::Done(settings))) => progress.done(asset, None, settings),
                    Ok(Ok(Applied::Skipped(skip))) => progress.skip(skip),
                    Ok(Err(refused)) => progress.skip(batch::refused(&asset, &refused)),
                    Err(stopped) => return Box::new(move |_: &mut Owner| Err(stopped)),
                }
            }
            let report = progress.finish();
            Box::new(move |_: &mut Owner| value(report))
        }),
    };
    value(owner.catalog.library.queue(&mut owner.jobs, task)?)
}

/// One pass of a photograph's apply on the owner: its answer, or the read it deferred, with the
/// pass's replay and this photograph's memo as the owner left them.
enum ApplyPass {
    Done(Applied),
    Read {
        read: Box<DeferredRead>,
        replay: Replay,
        memo: PixelMemo,
    },
}

/// Apply the settings to one photograph through the owner's deferred-read replay
/// ([`Replay`]), driven from this lane's worker: each pass runs on the owner through the pass the
/// owner's own calls take ([`Owner::pixel_pass`]), after the owner has checked that what the last
/// read was read from is still current; each read is submitted to the same tile service, under
/// this job's cancellation and for no client, so only a cancel of the job stops it and a
/// disconnect never does, and the worker waits for it off the owner; and the replay stops at the
/// same bound. The photograph's memo is its own, so the caller's draft and session memo are
/// neither read nor changed, and JobContext's commit gate keeps a cancelled batch from committing
/// late.
fn apply_on_worker(
    job: &JobContext<'_>,
    client: ClientId,
    tiles: &Arc<dyn crate::tiles::TileService>,
    origin: Origin,
    asset: AssetId,
    preset: Arc<SettingsApply>,
) -> Result<Result<Applied, Error>, Error> {
    let mut memo = PixelMemo::default();
    // The last read and its number among this photograph's reads.
    let mut answered: Option<(usize, PixelAnswer)> = None;
    loop {
        job.control.checkpoint()?;
        let (origin, asset, preset, held, last) = (
            origin.clone(),
            asset.clone(),
            preset.clone(),
            memo.clone(),
            answered.take(),
        );
        let pass = ask_owner(job, client, move |owner| {
            let mut memo = held.clone();
            let replay = match &last {
                None => Replay::First,
                Some((reads, answer)) => {
                    let current = owner.service.pixel_key_current(&answer.key, None)?;
                    Replay::answered(*reads, answer.clone(), current, &mut memo)
                }
            };
            let (result, deferred) = owner.pixel_pass(None, memo.clone(), |owner| {
                apply(owner, client, &origin, &asset, &preset)
            });
            match deferred {
                Some(read) => Ok(ApplyPass::Read {
                    read: Box::new(read),
                    replay,
                    memo,
                }),
                None => result.map(ApplyPass::Done),
            }
        })?;
        let (read, replay, held) = match pass {
            Ok(ApplyPass::Done(applied)) => return Ok(Ok(applied)),
            Err(refused) => return Ok(Err(refused)),
            Ok(ApplyPass::Read { read, replay, memo }) => (read, replay, memo),
        };
        let reads = match replay.park() {
            Ok(reads) => reads,
            Err(refused) => return Ok(Err(refused)),
        };
        memo = held;
        let (send, receive) = sync_channel(1);
        tiles.submit(read.tile_call(
            SYSTEM_CLIENT,
            job.control.render_cancel().clone(),
            move |result| {
                let _ = send.send(result);
            },
        ));
        let answer = receive.recv().map_err(|_| job.control.cancelled_error())?;
        job.control.checkpoint()?;
        match answer {
            Ok(answer) => answered = Some((reads, answer)),
            Err(error) => return Ok(Err(error)),
        }
    }
}

/// Apply the preset to one photograph as `edit.apply-preset` would, on the owner: left out while it
/// is in Removed, or while the caller's session holds a draft on it or previews its history;
/// answered as its first attempt was when this batch's request already applied it, before a
/// restart; otherwise the module action against its current revision under the request identity
/// derived for it, and a new entry announced naming the photograph and its revision.
fn apply(
    owner: &mut Owner,
    client: ClientId,
    origin: &Origin,
    asset: &AssetId,
    preset: &SettingsApply,
) -> Result<Applied, Error> {
    if let Some(skip) = batch::removed(&owner.service.connection, asset)? {
        return Ok(Applied::Skipped(skip));
    }
    if let Some(session) = owner.sessions.get(&client) {
        if let Some(draft) = session
            .draft
            .as_ref()
            .filter(|draft| &draft.asset_id == asset)
        {
            return Ok(Applied::Skipped(batch::skip(
                asset,
                batch::DRAFT_OPEN,
                format!(
                    "an unapplied {} draft is open on it; apply or cancel it first",
                    draft.action
                ),
            )));
        }
        if !session.preview.can_edit(asset) {
            return Ok(Applied::Skipped(batch::skip(
                asset,
                batch::HISTORY_SELECTED,
                "its history is being previewed: return to current or restore the selected \
                 history entry before editing",
            )));
        }
    }
    let request_id = batch::request_id(&preset.request_id, asset);
    if let Some(first) = owner.service.recorded_request(asset, &request_id)? {
        return Ok(preset.outcome(asset, first));
    }
    let mutation = Mutation {
        expected_revision: owner.service.revision(asset)?,
        request_id,
        actor: preset.actor.clone(),
    };
    let result =
        owner
            .service
            .run_action(asset, mutation, preset.action, preset.parameters.clone())?;
    if result.mutation.outcome != MutationOutcome::NoOp && !result.mutation.deduplicated {
        let changed = origin
            .clone()
            .changed(asset.clone(), Some(result.mutation.revision));
        announce_once(&mut owner.announced, &changed);
    }
    Ok(preset.outcome(asset, result))
}

/// `batch.export`: check the folder, then start a `batch-export` job that exports each photograph
/// `targets` names into it as `export.jpeg` would, answering a
/// [`BatchReport`](crate::catalog_types::BatchReport) with the files it wrote.
pub(in crate::api) fn batch_export(
    owner: &mut Owner,
    call: &Call<'_>,
    params: BatchExport,
) -> Result<Value, Error> {
    let folder = batch::export_folder(&params.destination)?;
    let assets = targets::assets(&owner.service, &params.targets, || {
        selected(owner, call.client)
    })?;
    let options = export::Options {
        keep_metadata: params.keep_metadata.unwrap_or(false),
        pixels_per_inch: None,
        reference: false,
    };
    // Each photograph streams through the owner's tile service as `export.jpeg`'s job does.
    let tiles = Arc::clone(&owner.tiles);
    let (client, origin) = (call.client, call.origin.clone());
    #[cfg(test)]
    let hold = owner.export_hold.clone();
    let detail = match folder.file_name() {
        Some(name) => format!(
            "{} to {}",
            photographs(assets.len()),
            name.to_string_lossy()
        ),
        None => photographs(assets.len()),
    };
    let task = Task {
        job_id: JobId::new(),
        job: &BATCH_EXPORT,
        asset_id: only(&assets),
        detail: Some(detail),
        origin: call.origin.clone(),
        work: Box::new(move |job| {
            let mut progress = Progress::new(job.control, assets.len());
            // Each phase of a photograph's export is on the board, and a cancel stops it there.
            let phase = |phase: &'static str| {
                job.control.set_phase(phase);
                #[cfg(test)]
                if let Some(hold) = &hold {
                    hold(phase);
                }
                job.control.checkpoint()
            };
            for (asset, _) in assets {
                job.pause(Phase::NextPhotograph);
                let target = asset.clone();
                let planned =
                    match ask_owner(job, client, move |owner: &mut Owner| plan(owner, &target)) {
                        Ok(planned) => planned,
                        Err(stopped) => return Box::new(move |_: &mut Owner| Err(stopped)),
                    };
                let (plan, original) = match planned {
                    Ok(Planned::Export { plan, original }) => (plan, original),
                    Ok(Planned::Skipped(skip)) => {
                        progress.skip(skip);
                        continue;
                    }
                    Err(refused) => {
                        progress.skip(batch::refused(&asset, &refused));
                        continue;
                    }
                };
                let written = batch::destination(&folder, &original).and_then(|destination| {
                    let written = export::write(
                        *plan,
                        &destination,
                        options,
                        tiles.as_ref(),
                        job.control,
                        &phase,
                        &mut |fraction| progress.within(fraction),
                    )?;
                    Ok(BatchWritten {
                        asset_id: asset.clone(),
                        path: destination.path().to_path_buf(),
                        renderer: written.renderer.into(),
                    })
                });
                match written {
                    Ok(written) => {
                        // Recorded as a single export's is, under the request that asked for it.
                        let origin = origin.clone();
                        let announced = job.commit(Box::new(move |owner: &mut Owner| {
                            announce_once(&mut owner.announced, &origin);
                            Ok(Value::Null)
                        }));
                        if let Err(stopped) = announced {
                            return Box::new(move |_: &mut Owner| Err(stopped));
                        }
                        progress.done(asset, Some(written), Vec::new());
                    }
                    Err(stopped) if stopped.kind == ErrorKind::Cancelled => {
                        return Box::new(move |_: &mut Owner| Err(stopped));
                    }
                    Err(refused) => progress.skip(batch::refused(&asset, &refused)),
                }
            }
            let report = progress.finish();
            Box::new(move |_: &mut Owner| value(report))
        }),
    };
    value(owner.catalog.library.queue(&mut owner.jobs, task)?)
}

/// One photograph's export as the owner planned it.
enum Planned {
    /// Its current entry, frozen as `export.jpeg` freezes it, and where its original is, whose
    /// name the exported file is named from.
    Export {
        plan: Box<ExportPlan>,
        original: PathBuf,
    },
    Skipped(crate::catalog_types::BatchSkip),
}

/// Plan one photograph's export on the owner, as `export.jpeg` plans its current entry: left out
/// while it is in Removed; refused as the single export would be, a missing or offline original
/// with the refusal availability names and an unprepared source with what it needs.
fn plan(owner: &mut Owner, asset: &AssetId) -> Result<Planned, Error> {
    if let Some(skip) = batch::removed(&owner.service.connection, asset)? {
        return Ok(Planned::Skipped(skip));
    }
    let plan = owner.service.export_plan(asset, None)?;
    let original = library_rows::asset_source(&owner.service.connection, asset)?
        .ok_or_else(|| Error::validation(format!("unknown asset {asset}")))?
        .locator;
    Ok(Planned::Export {
        plan: Box::new(plan),
        original,
    })
}

/// What the owner answered for one photograph: its answer, or the source job to wait for before
/// asking again (any source job, when the source queue was full).
enum Asked<T> {
    Answered(Result<T, Error>),
    Wait {
        job: Option<JobId>,
        ended: Receiver<()>,
    },
}

/// Ask the owner for one photograph, between two of its messages, and answer what `ask` answered:
/// its value, or the refusal the single call would have answered. A refusal for want of a prepared
/// source queues that preparation through the owner's one path, for the calling client, as its own
/// request would; the worker waits for it to end, blocked on a channel the owner answers, and asks
/// again, at most [`PREPARATIONS`] times. `Err` only when the job stops: cancelled, or its owner
/// gone. A cancel while a preparation runs takes effect when that preparation ends.
fn ask_owner<T: Send + 'static>(
    job: &JobContext<'_>,
    client: ClientId,
    ask: impl Fn(&mut Owner) -> Result<T, Error> + Clone + Send + 'static,
) -> Result<Result<T, Error>, Error> {
    let mut waited = None;
    for _ in 0..=PREPARATIONS {
        let (answer, answered) = sync_channel(1);
        let (ask, prepared) = (ask.clone(), waited.take());
        job.commit(Box::new(move |owner: &mut Owner| {
            let _ = answer.send(asked(owner, client, prepared, ask));
            Ok(Value::Null)
        }))?;
        match answered.recv() {
            Ok(Asked::Answered(answer)) => return Ok(answer),
            Ok(Asked::Wait { job: source, ended }) => {
                // The owner answers when the job ends, or drops the wait as it stops.
                let _ = ended.recv();
                job.control.checkpoint()?;
                waited = source;
            }
            Err(_) => return Err(job.control.cancelled_error()),
        }
    }
    Ok(Err(Error::preparation_required(format!(
        "its original was prepared {PREPARATIONS} times and each time replaced before it was used"
    ))))
}

/// The owner's half of [`ask_owner`]: the refusal of a preparation this photograph waited for, which
/// asking again would only queue again; otherwise `ask`'s answer, or the preparation it needs,
/// queued, with a wait for it.
fn asked<T>(
    owner: &mut Owner,
    client: ClientId,
    prepared: Option<JobId>,
    ask: impl FnOnce(&mut Owner) -> Result<T, Error>,
) -> Asked<T> {
    if let Some(job) = &prepared
        && let Ok((JobStatus::Failed, _, Some(error))) = owner.jobs.outcome_for(job, client)
    {
        return Asked::Answered(Err(error.clone()));
    }
    match ask(owner) {
        Err(refused) if refused.needs().is_some() => {
            let refused = owner.prepare(client, refused);
            let (reply, ended) = sync_channel(1);
            match refused.preparation_job().cloned() {
                Some(job) => {
                    owner.await_source(client, Some(job.clone()), reply);
                    Asked::Wait {
                        job: Some(job),
                        ended,
                    }
                }
                None if refused.retries_after_source_job() => {
                    owner.await_source(client, None, reply);
                    Asked::Wait { job: None, ended }
                }
                None => Asked::Answered(Err(refused)),
            }
        }
        answer => Asked::Answered(answer),
    }
}

/// The photograph a batch of one names, which the activity board shows it for.
fn only(assets: &[(AssetId, crate::catalog_types::AssetRowId)]) -> Option<AssetId> {
    match assets {
        [(asset, _)] => Some(asset.clone()),
        _ => None,
    }
}

fn photographs(count: usize) -> String {
    match count {
        1 => "1 photograph".to_owned(),
        count => format!("{count} photographs"),
    }
}
