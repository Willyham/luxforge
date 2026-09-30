//! `pick.plan`, `pick.develop` and `asset.send-back` on the owner. A plan and the start of a
//! Develop are made here; the Develop's files are read on the lane's worker
//! ([`LibraryLane`](super::LibraryLane)), one at a time, and committed back here in batches
//! ([`JobContext::commit`]), each one library change announced as one event. The library side is
//! `crate::library::develop`.
use super::{
    Call, ClientId, Commit, JobContext, LibraryLane, Owner, Task, change, retried, selected,
};
use crate::{
    Error, ErrorKind, JobId, MutationRequest,
    api::{Origin, methods::value},
    catalog_types::{
        DevelopReport, FileId, Targets, ViewItem, Volume, VolumeId,
        api::{AssetTargets, PickDevelop, PickPlan},
        jobs::DEVELOP_PICKS,
    },
    editor::{library_rows, now_ms, upsert_volume},
    library::{
        develop::{
            self, BATCH_FILES, BATCH_TIME, Decided, Destination, Developed, Plan, PlannedFile,
            send_back,
        },
        folders::counted,
        journal::{self, Request},
        targets::{self, NamedFile},
    },
};
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, time::Instant};

/// `pick.plan`: what developing the files `targets` names — or, with none, every pick in the
/// caller's view — would do, answering a [`DevelopPlan`](crate::catalog_types::DevelopPlan).
pub(in crate::api) fn pick_plan(
    owner: &mut Owner,
    call: &Call<'_>,
    params: PickPlan,
) -> Result<Value, Error> {
    let files = files(owner, call.client, params.targets.as_ref())?;
    let mut volumes = HashMap::new();
    let plan = planned(owner, files, &mut volumes)?;
    value(plan.answer(&volumes))
}

/// `pick.develop`: plan as `pick.plan` does, take each event's folder from `into`, refuse picks on
/// removable media that nothing covers, and start a `develop-picks` job that reads the files on
/// the lane's worker and commits them in batches, answering a
/// [`DevelopReport`](crate::catalog_types::DevelopReport). A retry of a Develop that committed is
/// answered, after a restart too, as a finished job with the report its recorded changes give.
pub(in crate::api) fn pick_develop(
    owner: &mut Owner,
    call: &Call<'_>,
    params: PickDevelop,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    let parts = journal::parts(&owner.service.connection, request)?;
    if !parts.is_empty() {
        return value(LibraryLane::answered(
            &mut owner.jobs,
            &DEVELOP_PICKS,
            None,
            call.origin.clone(),
            encode(&develop::recorded_report(&parts))?,
        ));
    }
    let files = files(owner, call.client, params.targets.as_ref())?;
    if files.is_empty() {
        return Err(Error::validation("there are no picks to develop"));
    }
    let mut volumes = HashMap::new();
    let plan = planned(owner, files, &mut volumes)?;
    let (destinations, of_event) =
        develop::destinations(&owner.service.connection, &plan, &params.into, now_ms())?;
    let use_copies = params.use_copies.unwrap_or(false);
    let confirm_removable = params.confirm_removable.unwrap_or(false);
    if !confirm_removable {
        uncovered(&plan, &volumes, use_copies)?;
    }
    let count = plan.files.len();
    let origin = call.origin.clone();
    let job = DevelopJob {
        files: plan.files,
        of_event,
        destinations,
        use_copies,
        confirm_removable,
        method: call.request.method.clone(),
        mutation: params.mutation,
        origin: origin.clone(),
    };
    let task = Task {
        job_id: JobId::new(),
        job: &DEVELOP_PICKS,
        asset_id: None,
        detail: Some(counted(count, "file", "files")),
        origin,
        work: Box::new(move |context| job.run(context)),
    };
    value(owner.catalog.library.queue(&mut owner.jobs, task)?)
}

/// `asset.send-back`: send the photographs `targets` names back as one library change — each
/// one's record deleted and its file picked again — refused, changing nothing, when any has more
/// than its Original or its original is not where the catalog looks for it.
pub(in crate::api) fn asset_send_back(
    owner: &mut Owner,
    call: &Call<'_>,
    params: AssetTargets,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(answer);
    }
    let assets = targets::assets(&owner.service, &params.targets, || {
        selected(owner, call.client)
    })?;
    let planned = send_back::plan(&owner.service.connection, &assets, request, now_ms())?;
    value(change(owner, &call.origin, |tx| {
        for volume in &planned.volumes {
            upsert_volume(tx, volume)?;
        }
        journal::apply(tx, request, planned.changes, send_back::label)
    })?)
}

/// The files a plan or a Develop covers: those `targets` names, picked or not, or with none, the
/// picked files among the items of the caller's view.
fn files(
    owner: &Owner,
    client: ClientId,
    targets: Option<&Targets>,
) -> Result<Vec<NamedFile>, Error> {
    if let Some(targets) = targets {
        return targets::files(&owner.service, targets, || selected(owner, client));
    }
    let file_ids: Vec<FileId> = owner
        .catalog
        .views
        .items(client)?
        .into_iter()
        .filter_map(|item| match item {
            ViewItem::File(id) => Some(id),
            ViewItem::Photo(_) => None,
        })
        .collect();
    if file_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut picked = Vec::new();
    for file in targets::files(&owner.service, &Targets::Files { file_ids }, || {
        selected(owner, client)
    })? {
        if library_rows::pick_at(&owner.service.connection, &file.path)?.is_some() {
            picked.push(file);
        }
    }
    Ok(picked)
}

/// The plan of `files`, reading the catalog and the index.
fn planned(
    owner: &Owner,
    files: Vec<NamedFile>,
    volumes: &mut HashMap<VolumeId, Volume>,
) -> Result<Plan, Error> {
    let index = owner.service.index()?;
    develop::plan(
        &owner.service.connection,
        index.connection(),
        files,
        volumes,
    )
}

/// Refuse a Develop with picks on a removable volume that nothing covers: not offline, and with no
/// copy to verify when `use_copies` is set. `conflict`, naming the first such volume and how many
/// picks it holds, and what would develop them.
fn uncovered(
    plan: &Plan,
    volumes: &HashMap<VolumeId, Volume>,
    use_copies: bool,
) -> Result<(), Error> {
    let mut on: Vec<(&VolumeId, u32, u32)> = Vec::new();
    for file in &plan.files {
        let Some(volume) = &file.removable else {
            continue;
        };
        if use_copies && !file.copies.is_empty() {
            continue;
        }
        let copied = u32::from(!file.copies.is_empty());
        match on.iter_mut().find(|(known, ..)| *known == volume) {
            Some((_, count, with_copy)) => {
                *count += 1;
                *with_copy += copied;
            }
            None => on.push((volume, 1, copied)),
        }
    }
    let Some((volume, count, with_copy)) = on.first() else {
        return Ok(());
    };
    let label = volumes
        .get(*volume)
        .map_or_else(|| volume.to_string(), |known| known.label.clone());
    let picks = counted(*count as usize, "pick is", "picks are");
    let remedy = if *with_copy > 0 {
        "use_copies develops them from their copies in indexed folders, confirm_removable from the \
         volume itself"
    } else {
        "they have no copy in an indexed folder; confirm_removable develops them from the volume \
         itself"
    };
    Err(
        Error::conflict(format!("{picks} on the removable volume {label}: {remedy}")).with_data(
            json!({"volume_id": volume, "label": label, "count": count, "with_copy": with_copy}),
        ),
    )
}

/// A Develop's work on the lane's worker: its planned files, event by event, each event's folder,
/// and how to treat removable media.
struct DevelopJob {
    files: Vec<PlannedFile>,
    /// Each event's folder: an index into `destinations`.
    of_event: Vec<usize>,
    destinations: Vec<Destination>,
    use_copies: bool,
    confirm_removable: bool,
    method: String,
    mutation: MutationRequest,
    origin: Origin,
}

impl DevelopJob {
    /// Run on the worker, answering the owner's half: the report, or the cancel.
    fn run(self, job: &JobContext<'_>) -> Commit {
        match self.develop(job) {
            Ok(report) => Box::new(move |_| value(report)),
            Err(error) => Box::new(move |_| Err(error)),
        }
    }

    /// Read each file and commit them in batches — the first as soon as one file is read, then
    /// up to [`BATCH_FILES`] files or [`BATCH_TIME`], never across an event — reporting progress
    /// after each file. A file that fails is reported and stays picked; a batch the owner refuses
    /// reports each of its files. A cancel stops before the next file, or before a batch commits,
    /// and ends the job cancelled, keeping every batch committed so far.
    fn develop(self, job: &JobContext<'_>) -> Result<DevelopReport, Error> {
        let total = self.files.len();
        let mut report = DevelopReport::default();
        let mut batch: Vec<Developed> = Vec::new();
        let mut gathering = Instant::now();
        let mut first = true;
        for (at, file) in self.files.iter().enumerate() {
            job.control.checkpoint()?;
            if batch.is_empty() {
                gathering = Instant::now();
            }
            match develop::develop_file(
                file,
                self.use_copies,
                self.confirm_removable,
                job.control,
                &|phase| job.pause(phase),
            ) {
                Ok(developed) => batch.push(developed),
                Err(error) if error.kind == ErrorKind::Cancelled => return Err(error),
                Err(error) => report
                    .failed
                    .push(develop::failure(&file.path, None, &error)),
            }
            let done = at + 1;
            job.control.set_progress(
                Some(done as f64 / total.max(1) as f64),
                &format!("{done} of {total}"),
            );
            let event_ends = self
                .files
                .get(done)
                .is_none_or(|next| next.event != file.event);
            let full = first || batch.len() >= BATCH_FILES || gathering.elapsed() >= BATCH_TIME;
            if batch.is_empty() || !(full || event_ends) {
                continue;
            }
            first = false;
            let files = std::mem::take(&mut batch);
            let paths: Vec<PathBuf> = files.iter().map(|file| file.pick.clone()).collect();
            let destination = self.destinations[self.of_event[file.event]].clone();
            let (method, mutation, origin) = (
                self.method.clone(),
                self.mutation.clone(),
                self.origin.clone(),
            );
            let committed = job.commit(Box::new(move |owner: &mut Owner| {
                commit(
                    owner,
                    &origin,
                    Request::new(&method, &mutation),
                    &destination,
                    files,
                )
            }));
            match committed {
                Ok(answer) => {
                    let part: DevelopReport = serde_json::from_value(answer).map_err(|error| {
                        Error::internal(format!("cannot read a batch's report: {error}"))
                    })?;
                    report.developed.extend(part.developed);
                    report.failed.extend(part.failed);
                    report.changes.extend(part.changes);
                }
                Err(error) if error.kind == ErrorKind::Cancelled => return Err(error),
                Err(error) => report.failed.extend(
                    paths
                        .iter()
                        .map(|path| develop::failure(path, None, &error)),
                ),
            }
        }
        Ok(report)
    }
}

/// Commit one batch on the owner, as one library change of the Develop's request: decide what each
/// file becomes, then write them. A change the catalog refuses reports every file of the batch
/// with its refusal and changes nothing. Answers the batch's report.
fn commit(
    owner: &mut Owner,
    origin: &Origin,
    request: Request<'_>,
    destination: &Destination,
    files: Vec<Developed>,
) -> Result<Value, Error> {
    let now = now_ms();
    let (decided, failed) = develop::decide(&owner.service, files, now)?;
    let mut report = DevelopReport {
        failed,
        ..DevelopReport::default()
    };
    if decided.is_empty() {
        return value(report);
    }
    let artifact_root = owner.service.artifact_root().to_path_buf();
    match change(owner, origin, |tx| {
        develop::write(tx, request, &artifact_root, destination, &decided, now)
    }) {
        Ok(answer) => {
            report.developed = decided.iter().map(Decided::reported).collect();
            report.changes.extend(answer.change);
        }
        Err(error) => report.failed.extend(
            decided
                .iter()
                .map(|decided| develop::failure(&decided.developed.pick, None, &error)),
        ),
    }
    value(report)
}

fn encode(report: &DevelopReport) -> Result<Value, Error> {
    serde_json::to_value(report)
        .map_err(|error| Error::internal(format!("cannot encode a report: {error}")))
}
