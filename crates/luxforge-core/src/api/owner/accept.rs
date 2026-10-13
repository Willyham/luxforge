//! A candidate-accepting action ([`crate::ActionDescriptor::accepts_candidate`],
//! `edit.accept-correction-patch`): the catalog owner pins a task's candidate, publishes its bytes
//! as an artifact on the source worker, and only then commits the one ordinary entry that references
//! it (`docs/design/corrections.md`, "Candidates"; `docs/design/module-capabilities.md`, "Job
//! candidates").
//!
//! 1. **Checked first, on the owner.** The request is checked as a commit would check it; a retry
//!    the catalog already answered is answered from its request log without pinning anything. The
//!    session must be on current and `mutation.expected_revision` current; the job must be a task
//!    the caller may read; the candidate must be live, of this photograph, and generated against
//!    exactly the input a new top correction has now — otherwise `conflict` with `data.reasons`.
//! 2. **Published off the owner.** The pinned candidate ([`crate::artifacts::candidates::CandidatePin`])
//!    travels to the source worker as an `artifacts` job, which reads, verifies and writes its
//!    stored patch as a `repair-patch` artifact. The call is parked meanwhile. No budget release
//!    takes a pinned candidate, an explicit release of it is a `conflict` and a cancel of its task
//!    waits for the accept.
//! 3. **Committed on the owner.** The artifact's row is recorded live, then the action is planned and
//!    committed through the ordinary path with the published candidate in hand, which rechecks the
//!    revision and the input. Success settles the pin (`accepted`); anything else drops it, so the
//!    candidate stays live and the published artifact is only an unreferenced object for
//!    `artifact.collect`.
//!
//! The caller leaving — a disconnect stops its accept job — or `job.cancel` of the accept job drops
//! the call and the pin before anything is committed. A caller that may not wait
//! ([`super::OwnerHandle::call_unparked`]) is answered `not-ready` before anything is pinned.
use super::{Call, Owner, OwnerCall, SourceFlightKey, SourceResult, SourceTaskKind, methods};
use crate::{
    AcceptedCandidate, AssetId, Error, JobId, Mutation,
    api::{ApiResponse, Origin, announce_once, params},
    artifacts::{
        ArtifactRecord, ArtifactWriter, PreparedArtifact,
        candidates::{CandidatePin, CandidateRead},
    },
    jobs::JobKind,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// The `not-ready` answer's `data.reason` for an accept sent where it may not wait.
pub(crate) const ACCEPT_PUBLISHES: &str = "accept-publishes";

/// What one accept commits once its candidate is published.
pub(super) struct AcceptRequest {
    asset_id: AssetId,
    mutation: Mutation,
    action_id: String,
    /// The action's own parameters, as the request sent them.
    parameters: Value,
    candidate: CandidateRead,
    origin: Origin,
}

/// An accept whose candidate the source worker is publishing: left by [`begin`] for the owner to
/// park with the call's reply, keyed by its job.
pub(super) struct PendingAccept {
    pub(super) job: JobId,
    pub(super) request: AcceptRequest,
}

/// A parked accept: the call and what it commits.
pub(super) struct ParkedAccept {
    pub(super) call: OwnerCall,
    pub(super) request: AcceptRequest,
}

/// What the source worker runs for an accept: the pinned candidate and where to publish it.
pub(super) struct AcceptWork {
    pub(super) asset_id: AssetId,
    pub(super) pin: CandidatePin,
    pub(super) writer: ArtifactWriter,
}

/// What it hands back: the pin, still held, and the published artifact.
pub(super) struct Published {
    pub(super) pin: CandidatePin,
    pub(super) record: ArtifactRecord,
    pub(super) prepared: Arc<PreparedArtifact>,
}

impl AcceptWork {
    /// Publish the candidate's stored patch through the catalog's artifact writer. Worker-side.
    pub(super) fn run(self, cancel: &std::sync::atomic::AtomicBool) -> Result<Published, Error> {
        let (record, prepared) = self.pin.publish(&self.writer, cancel)?;
        Ok(Published {
            pin: self.pin,
            record,
            prepared,
        })
    }

    pub(super) fn asset_id(&self) -> &AssetId {
        &self.asset_id
    }
}

/// Step 1 ([module](self) docs): answer a retry, refuse what cannot be accepted, or pin the
/// candidate, queue its publication and leave the call for the owner to park. `parks` says whether
/// the caller may wait.
pub(super) fn begin(
    owner: &mut Owner,
    call: &Call<'_>,
    action_id: &str,
    parks: bool,
) -> Result<Value, Error> {
    let mut parameters = params::generated(&call.request.params)?;
    let asset_id: AssetId = params::take(&mut parameters, "asset_id")?;
    let mutation: Mutation = params::take(&mut parameters, "mutation")?;
    let job_id: JobId = parameters
        .get("job_id")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| Error::validation("job_id is a job identity"))?
        .ok_or_else(|| Error::validation("job_id is required"))?;
    let candidate = parameters
        .get("candidate")
        .and_then(Value::as_u64)
        .and_then(|candidate| u8::try_from(candidate).ok())
        .filter(|candidate| *candidate <= 3)
        .ok_or_else(|| Error::validation("candidate is a number from 0 to 3"))?;
    let parameters = Value::Object(parameters);
    // A retry the catalog already answered is that answer; the request is checked as a commit
    // would check it before anything is pinned.
    if let Some(result) =
        owner
            .service
            .action_retry(&asset_id, &mutation, action_id, parameters.clone())?
    {
        return methods::value(result);
    }
    let session = owner.sessions.entry(call.client).or_default();
    methods::require_current(session, &asset_id)?;
    let state = owner.service.state(&asset_id)?;
    if state.revision != mutation.expected_revision {
        return Err(Error::conflict(format!(
            "expected revision {} but current revision is {}",
            mutation.expected_revision, state.revision
        )));
    }
    let record = owner.jobs.read_for(&job_id, call.client)?;
    if record.kind != JobKind::Task {
        return Err(Error::validation(format!(
            "job {job_id} is not a task and holds no candidates"
        )));
    }
    if !parks {
        return Err(Error::not_ready(
            "accepting a candidate publishes it off the catalog owner; send the call where it may \
             wait",
        )
        .with_data(json!({ "reason": ACCEPT_PUBLISHES })));
    }
    let pin = owner.service.candidates().pin(&job_id, candidate)?;
    let read = pin.candidate().clone();
    if read.asset_id != asset_id {
        return Err(Error::validation(format!(
            "candidate {candidate} of job {job_id} belongs to another photograph"
        )));
    }
    let Some(generated) = &read.input else {
        return Err(Error::conflict(format!(
            "candidate {candidate} of job {job_id} does not say what it was generated against"
        )));
    };
    check_input(generated, &owner.service.correction_input(&asset_id)?)?;
    let writer = owner.service.artifact_writer()?;
    let job = owner.sources.submit(
        &mut owner.jobs,
        call.client,
        SourceFlightKey {
            path: format!("accept:{job_id}:{candidate}").into(),
            signature: None,
            expected_fingerprint: None,
            gains_bits: None,
            artifacts: Vec::new(),
        },
        SourceTaskKind::Accept(Box::new(AcceptWork {
            asset_id: asset_id.clone(),
            pin,
            writer,
        })),
        Vec::new(),
        None,
        false,
        None,
    )?;
    owner.accept_parking = Some(PendingAccept {
        job,
        request: AcceptRequest {
            asset_id,
            mutation,
            action_id: action_id.to_owned(),
            parameters,
            candidate: read,
            origin: call.origin.clone(),
        },
    });
    Ok(Value::Null)
}

/// A `conflict` naming each component of the candidate's input that differs from `now`.
fn check_input(generated: &crate::RepairInput, now: &crate::RepairInput) -> Result<(), Error> {
    let reasons = generated.reasons(now);
    if reasons.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = reasons.iter().map(|reason| reason.as_str()).collect();
    let said: Vec<&str> = reasons.iter().map(|reason| reason.sentence()).collect();
    Err(Error::conflict(format!(
        "the candidate was generated against another input: {}",
        said.join(", ")
    ))
    .with_data(json!({ "reasons": names })))
}

impl Owner {
    /// Park the call that [`begin`] left an accept for, until its job completes.
    pub(super) fn park_accept(&mut self, call: OwnerCall, pending: PendingAccept) {
        self.parked_accepts.insert(
            pending.job,
            ParkedAccept {
                call,
                request: pending.request,
            },
        );
    }

    /// Drop every accept `client` parked: it left, and its accept jobs are stopped with it.
    pub(super) fn drop_parked_accepts(&mut self, client: crate::ClientId) {
        self.parked_accepts
            .retain(|_, parked| parked.call.client != client);
    }

    /// Step 3 ([module](self) docs): the source worker finished the accept job `job`. Answers the
    /// parked call, if it is still waiting, with the commit's answer or the refusal; the pin is
    /// settled or dropped either way. Returns what the job records.
    pub(super) fn accept_published(
        &mut self,
        job: &JobId,
        result: Result<SourceResult, Error>,
    ) -> Result<Value, Error> {
        let parked = self.parked_accepts.remove(job);
        let published = match result {
            Ok(SourceResult::Accepted(published)) => published,
            Ok(_) => return Err(Error::internal("an accept job published nothing")),
            Err(error) => {
                if let Some(parked) = parked {
                    self.answer_accept(parked.call, Err(error.clone()));
                }
                return Err(error);
            }
        };
        let Published {
            pin,
            record,
            prepared,
        } = *published;
        let artifact = record.id.clone();
        let Some(parked) = parked else {
            // Nobody waits any more: the pin drops and the artifact, if recorded, stays unreferenced.
            return Err(Error::cancelled("the accept was cancelled"));
        };
        let committed = self.commit_accept(&parked.request, record, prepared);
        match &committed {
            Ok(_) => pin.accepted(),
            Err(_) => drop(pin),
        }
        self.answer_accept(parked.call, committed);
        Ok(json!({ "artifact": artifact }))
    }

    /// Record the published artifact and commit the accepting action with it.
    fn commit_accept(
        &mut self,
        request: &AcceptRequest,
        record: ArtifactRecord,
        prepared: Arc<PreparedArtifact>,
    ) -> Result<Value, Error> {
        let artifact = record.id.clone();
        self.service.register_artifact(record, prepared, true)?;
        let generated = request
            .candidate
            .input
            .clone()
            .ok_or_else(|| Error::conflict("the candidate holds no input identity"))?;
        let accepted = Arc::new(AcceptedCandidate {
            artifact,
            rect: request.candidate.rect,
            label: request.candidate.label.clone(),
            provenance: request.candidate.provenance.clone(),
            input: generated,
        });
        let result = self.service.accept_candidate(
            &request.asset_id,
            request.mutation.clone(),
            &request.action_id,
            request.parameters.clone(),
            accepted,
        )?;
        let changed = result.mutation.outcome != crate::MutationOutcome::NoOp
            && !result.mutation.deduplicated;
        if changed {
            let origin = request
                .origin
                .clone()
                .changed(request.asset_id.clone(), Some(result.mutation.revision));
            announce_once(&mut self.announced, &origin);
        }
        self.record_announced();
        methods::value(result)
    }

    /// Answer a parked accept's call with the owner's sequence now.
    fn answer_accept(&mut self, call: OwnerCall, result: Result<Value, Error>) {
        let answer = match result {
            Ok(value) => ApiResponse::value(call.request.id, self.log.sequence, value),
            Err(error) => ApiResponse::failure(call.request.id, self.log.sequence, error),
        };
        let _ = call.response.send(answer);
    }
}

/// The activity an accept job shows.
pub(super) fn activity_detail(work: &AcceptWork) -> Option<String> {
    let read = work.pin.candidate();
    Some(format!("{} {}", read.label, read.candidate))
}
