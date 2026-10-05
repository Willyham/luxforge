//! `source.check` and `source.locate` on the owner: each plans on the owner, runs its disk work on
//! the lane's worker ([`LibraryLane`](super::LibraryLane)) and commits on the owner when the worker
//! posts back. The library side is `crate::library::availability` and `crate::library::locate`.
use super::{Call, Owner, Task, change, retried, selected};
use crate::{
    Error, JobId,
    api::{Origin, announce_once, methods::value},
    catalog_types::{
        AvailabilityReport, AvailabilityRow, FileAvailability,
        api::{SourceCheck, SourceLocate},
        jobs::{SOURCE_CHECK, SOURCE_LOCATE},
    },
    editor::now_ms,
    library::{
        availability::{self, Observed, SYSTEM_ACTOR},
        journal::{self, Outcome, Request},
        locate::{self, Verified},
    },
};
use serde_json::Value;
use std::collections::HashSet;

/// `source.check`: start a `source-check` job that looks at where the photographs `targets` names
/// are, records each one's availability with the time it looked, relinks each one found again on
/// its volume by its file identity and fingerprint, and answers an
/// [`AvailabilityReport`](crate::catalog_types::AvailabilityReport).
pub(in crate::api) fn source_check(
    owner: &mut Owner,
    call: &Call<'_>,
    params: SourceCheck,
) -> Result<Value, Error> {
    let assets = crate::library::targets::assets(&owner.service, &params.targets, || {
        selected(owner, call.client)
    })?;
    let plan = availability::plan(&owner.service.connection, &assets)?;
    // The index is a cache the worker reads with its own connection. A catalog that has indexed
    // nothing gets none made here; without one, nothing moved is found again, and everything else
    // is still checked.
    let indexed = owner
        .service
        .index_dir()
        .join(crate::index::INDEX_FILE)
        .exists();
    let index = indexed
        .then(|| owner.service.index().and_then(|index| index.connect()).ok())
        .flatten();
    let job_id = JobId::new();
    let origin = call.origin.clone();
    let task = Task {
        job_id: job_id.clone(),
        job: &SOURCE_CHECK,
        asset_id: None,
        detail: Some(match plan.count {
            1 => "1 photograph".to_owned(),
            count => format!("{count} photographs"),
        }),
        origin: origin.clone(),
        work: Box::new(move |job| {
            let observed =
                availability::observe(plan, index.as_ref(), job.control, &|phase| job.pause(phase));
            Box::new(move |owner: &mut Owner| checked(owner, &origin, &job_id, observed))
        }),
    };
    value(owner.catalog.library.queue(&mut owner.jobs, task)?)
}

/// Record what a check observed: every photograph's availability and the time it was looked at,
/// outside the journal, and, in the same transaction, the photographs it found again as one library
/// change by the `system` actor under the check's job. Announced once: naming the change when there
/// is one, and otherwise when any availability changed.
fn checked(
    owner: &mut Owner,
    origin: &Origin,
    job_id: &JobId,
    observed: Result<Observed, Error>,
) -> Result<Value, Error> {
    let Observed {
        checked_ms,
        rows: observations,
        found,
    } = observed?;
    // A request of its own, the check's job: however the check was asked for, its relinks are
    // never taken for another request's retry.
    let request_id = job_id.to_string();
    let request = Request {
        method: &origin.method,
        actor: SYSTEM_ACTOR,
        request_id: &request_id,
    };
    let mut rows = Vec::with_capacity(observations.len());
    let mut changed = false;
    let answer = change(owner, origin, |tx| {
        let relinks = availability::relinks(tx, &found)?;
        let relinked: HashSet<_> = relinks.iter().map(|found| &found.asset_id).collect();
        for observation in &observations {
            let availability = if relinked.contains(&observation.asset_id) {
                FileAvailability::Available
            } else {
                observation.availability
            };
            changed |= availability != observation.stored;
            rows.push(AvailabilityRow {
                asset_id: observation.asset_id.clone(),
                availability,
                checked_ms,
            });
        }
        availability::record(tx, &rows)?;
        if relinks.is_empty() {
            return Ok(Outcome::NoOp);
        }
        journal::apply(
            tx,
            request,
            availability::relink_changes(&relinks)?,
            availability::relink_label,
        )
    })?;
    if answer.change.is_none() && changed {
        announce_once(&mut owner.announced, origin);
    }
    value(AvailabilityReport { rows })
}

/// `source.locate`: check the chosen file before reading anything, then start a `source-locate`
/// job that verifies its fingerprint on the lane's worker and commits the relocation as one library
/// change ([`locate`]), answering a [`LibraryAnswer`](crate::catalog_types::LibraryAnswer). A retry
/// of a Locate that committed is answered with its change, as a finished job, reading nothing.
pub(in crate::api) fn source_locate(
    owner: &mut Owner,
    call: &Call<'_>,
    params: SourceLocate,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(super::LibraryLane::answered(
            &mut owner.jobs,
            &SOURCE_LOCATE,
            Some(params.asset_id),
            call.origin.clone(),
            serde_json::to_value(answer)
                .map_err(|error| Error::internal(format!("cannot encode an answer: {error}")))?,
        ));
    }
    let candidate = locate::plan(&owner.service.connection, &params.asset_id, &params.path)?;
    let origin = call.origin.clone();
    let method = call.request.method.clone();
    let mutation = params.mutation;
    let task = Task {
        job_id: JobId::new(),
        job: &SOURCE_LOCATE,
        asset_id: Some(params.asset_id),
        detail: candidate
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
        origin: origin.clone(),
        work: Box::new(move |job| {
            let verified = locate::verify(&candidate, job.control, &|phase| job.pause(phase));
            Box::new(move |owner: &mut Owner| {
                let request = Request::new(&method, &mutation);
                located(owner, &origin, request, verified)
            })
        }),
    };
    value(owner.catalog.library.queue(&mut owner.jobs, task)?)
}

/// Commit a verified Locate: the file must still be the one verified, and then the relocation is
/// one library change, announced as one event naming it.
fn located(
    owner: &mut Owner,
    origin: &Origin,
    request: Request<'_>,
    verified: Result<Verified, Error>,
) -> Result<Value, Error> {
    let verified = verified?;
    if !locate::unchanged(&verified.path, &verified.signature) {
        return Err(Error::conflict(format!(
            "{} changed after it was verified",
            verified.path.display()
        )));
    }
    value(change(owner, origin, |tx| {
        locate::commit(tx, request, &verified, now_ms())
    })?)
}
