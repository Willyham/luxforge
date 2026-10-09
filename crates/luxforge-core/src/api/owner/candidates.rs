//! `job.read-candidate` and `job.release-candidates`: a task job's candidate patches, read in
//! base64 chunks and released, beside `job.read`, which lists them. A candidate belongs to its task
//! job, so the same ownership holds: any client reads or releases a task job's candidates, and a
//! client's disconnect releases nothing. A chunk is read on the index lane's query thread, so the
//! owner reads no candidate bytes itself; it only changes the store and removes released files.
//! See `docs/design/module-capabilities.md#job-candidates`.
use super::{Call, Owner, files};
use crate::{
    CandidatePart, Error, JobId,
    api::{announce_once, methods, params::host_params},
    artifacts::candidates::{MAX_CANDIDATE_CHUNK, base64},
    jobs::{JobKind, JobRecord},
};
use serde_json::{Value, json};

/// `job.read-candidate`.
pub const READ_CANDIDATE: &str = "job.read-candidate";
/// `job.release-candidates`.
pub const RELEASE_CANDIDATES: &str = "job.release-candidates";

host_params! {
    /// `job.read-candidate`.
    pub(in crate::api) struct ReadCandidate {
        job_id: JobId = job(),
        candidate: u8 = integer(0, 3).notes("the candidate's number in its job, as job.read lists it"),
        part: CandidatePart = enumeration(["preview", "patch"]).notes("preview: RGB 8-bit sRGB (rgb8-srgb); patch: RGBA half floats, little-endian, RGB linear sRGB and A the coverage (rgba16f-le)"),
        offset: Option<u64> = integer(0, 64 * 1024 * 1024).notes("the first byte to read; default 0"),
        length: Option<u64> = integer(0, MAX_CANDIDATE_CHUNK as i64).notes("at most 786432 bytes (768 KiB); default as much of that as the part has from offset"),
    }
}

host_params! {
    /// `job.release-candidates`.
    pub(in crate::api) struct ReleaseCandidates {
        job_id: JobId = job(),
        mutation: MutationRequest,
        candidates: Option<Vec<u8>> = json("[candidate, …]: the numbers to release, 0 to 3 each; default every candidate the job holds"),
    }
}

/// The task job `client` may read, as `job.read` reads it; any other kind holds no candidates.
fn task_job(owner: &Owner, job_id: &JobId, call: &Call<'_>) -> Result<JobRecord, Error> {
    let record = owner.jobs.read_for(job_id, call.client)?;
    if record.kind != JobKind::Task {
        return Err(Error::validation(format!(
            "job {job_id} is not a task and holds no candidates"
        )));
    }
    Ok(record)
}

/// `job.read-candidate`: one chunk of a live candidate's preview or patch, base64-encoded, read on
/// the query thread while a read pin keeps its files. A candidate released while the chunk was
/// read is refused as a `conflict`, like one released before.
pub(in crate::api) fn read_candidate(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ReadCandidate,
) -> Result<Value, Error> {
    let ReadCandidate {
        job_id,
        candidate,
        part,
        offset,
        length,
    } = params;
    task_job(owner, &job_id, call)?;
    let pin = owner.service.candidates().reader(&job_id, candidate)?;
    let total = pin.total(part);
    let offset = offset.unwrap_or(0);
    if offset > total {
        return Err(Error::validation(format!(
            "offset {offset} is past the {total} bytes of candidate {candidate}'s {}",
            part.format()
        )));
    }
    let length = length
        .unwrap_or(MAX_CANDIDATE_CHUNK)
        .min(MAX_CANDIDATE_CHUNK)
        .min(total - offset);
    let read = pin.candidate();
    let (width, height) = match part {
        CandidatePart::Patch => (read.width, read.height),
        CandidatePart::Preview => (read.preview.width, read.preview.height),
    };
    files::ask(owner, move || {
        let data = pin.read(part, offset, length).map(|bytes| base64(&bytes));
        drop(pin);
        Box::new(move |owner: &mut Owner| {
            let data = data?;
            owner.service.candidates().live(&job_id, candidate)?;
            Ok(json!({
                "job_id": job_id,
                "candidate": candidate,
                "part": part,
                "width": width,
                "height": height,
                "format": part.format(),
                "offset": offset,
                "length": length,
                "total": total,
                "data": data,
            }))
        })
    })
}

/// `job.release-candidates`: release the named candidates of a task job, or all of them, and
/// answer which were released now beside the job's candidates as `job.read` lists them. A
/// release that releases something is announced.
pub(in crate::api) fn release_candidates(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ReleaseCandidates,
) -> Result<Value, Error> {
    task_job(owner, &params.job_id, call)?;
    if let Some(named) = &params.candidates
        && let Some(beyond) = named.iter().find(|candidate| **candidate > 3)
    {
        return Err(Error::validation(format!(
            "candidate {beyond} is not a number from 0 to 3"
        )));
    }
    let candidates = owner.service.candidates();
    let released = candidates.release(&params.job_id, params.candidates.as_deref())?;
    if !released.is_empty() {
        announce_once(&mut owner.announced, &call.origin);
    }
    let outcome = if released.is_empty() {
        crate::MutationOutcome::NoOp
    } else {
        crate::MutationOutcome::Applied
    };
    let rows = candidates.read_job(&params.job_id);
    methods::value(json!({
        "job_id": params.job_id,
        "outcome": outcome,
        "released": released,
        "candidates": rows,
    }))
}
