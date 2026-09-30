//! `source.missing`, `source.find` and `source.relink` on the owner. `source.missing` reads the
//! catalog; `source.find` plans on the owner, walks and verifies on the lane's worker
//! ([`LibraryLane`](super::LibraryLane)) and remembers what it verified on the owner when it ends;
//! `source.relink` commits what a find verified as one library change. The library side is
//! `crate::library::missing`.
use super::{Call, Owner, Task, change, retried, selected};
use crate::{
    AssetId, Error, JobId,
    api::methods::value,
    catalog_types::{
        api::{SourceFind, SourceMissing, SourceRelink},
        jobs::SOURCE_FIND,
    },
    editor::now_ms,
    library::{
        journal::Request,
        missing::{self, SearchJob, SearchLimits, Searched},
        targets,
    },
};
use serde_json::Value;
use std::path::PathBuf;

/// `source.missing`: the photographs whose originals were last recorded offline, missing or
/// changed, grouped by the folder on disk each was developed from, with each group's reason.
pub(in crate::api) fn source_missing(
    owner: &mut Owner,
    _: &Call<'_>,
    params: SourceMissing,
) -> Result<Value, Error> {
    // Grouping by source folder is the one grouping there is.
    let _ = params.grouping.unwrap_or_default();
    value(missing::missing(&owner.service.connection)?)
}

/// `source.find`: check the folder to search and the photographs to look for, then start a
/// `source-find` job that searches it on the lane's worker and answers a
/// [`FindReport`](crate::catalog_types::FindReport), changing nothing. The files it verified are
/// remembered for `source.relink` when it ends.
pub(in crate::api) fn source_find(
    owner: &mut Owner,
    call: &Call<'_>,
    params: SourceFind,
) -> Result<Value, Error> {
    let sought = match (&params.targets, &params.source_folder) {
        (Some(targets), None) => {
            let assets = targets::assets(&owner.service, targets, || selected(owner, call.client))?;
            missing::sought(&owner.service.connection, &assets)?
        }
        (None, Some(folder)) => missing::sought_from(&owner.service.connection, folder)?,
        _ => {
            return Err(Error::validation(
                "name exactly one of targets and source_folder: the photographs to look for",
            ));
        }
    };
    let root = missing::search_root(&params.search_root)?;
    let detail = match sought.len() {
        1 => format!("{} in {}", sought[0].file_name, root.display()),
        count => format!("{count} photographs in {}", root.display()),
    };
    let asset_id = (sought.len() == 1).then(|| sought[0].asset_id.clone());
    let task = Task {
        job_id: JobId::new(),
        job: &SOURCE_FIND,
        asset_id,
        detail: Some(detail),
        origin: call.origin.clone(),
        work: Box::new(move |job| {
            let claims = |files: Vec<(PathBuf, String)>| -> Result<Vec<Vec<AssetId>>, Error> {
                let answer = job.commit(Box::new(move |owner: &mut Owner| {
                    value(missing::claimants(&owner.service.connection, &files)?)
                }))?;
                serde_json::from_value(answer)
                    .map_err(|error| Error::internal(format!("cannot read the claims: {error}")))
            };
            let searched = missing::search(
                root,
                sought,
                SearchLimits::default(),
                &SearchJob {
                    control: job.control,
                    pause: &|phase| job.pause(phase),
                    claims: &claims,
                },
            );
            Box::new(move |owner: &mut Owner| found(owner, searched))
        }),
    };
    value(owner.catalog.library.queue(&mut owner.jobs, task)?)
}

/// Settle a finished search against the photographs that name each file now, remember what it
/// verified for each photograph (replacing what an earlier find did), and answer its report.
fn found(owner: &mut Owner, searched: Result<Searched, Error>) -> Result<Value, Error> {
    let searched = searched?;
    let reported = missing::report(&owner.service.connection, &searched)?;
    for (asset_id, files) in reported.verified {
        owner.catalog.library.verified.remember(asset_id, files);
    }
    value(reported.report)
}

/// `source.relink`: commit every pair a find verified, in one transaction as one library change. A
/// retry is answered with the change its first attempt recorded, before anything is checked again.
pub(in crate::api) fn source_relink(
    owner: &mut Owner,
    call: &Call<'_>,
    params: SourceRelink,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(answer);
    }
    let relinks = missing::plan_relink(
        &owner.service.connection,
        &mut owner.catalog.library.verified,
        &params.pairs,
    )?;
    let checked_ms = now_ms();
    value(change(owner, &call.origin, |tx| {
        missing::commit_relink(tx, request, &relinks, checked_ms)
    })?)
}
