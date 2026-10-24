//! Candidate drafts (`docs/design/ai-editing.md`, "Candidate drafts"): a client's draft of a
//! candidate-accepting action ([`crate::ActionDescriptor::accepts_candidate`]), which shows a live
//! candidate in the photograph through the whole stack before it is accepted.
//!
//! - **`draft.set`** naming a candidate is checked as an accept is checked — the job a task the
//!   caller may read, the candidate live, of this photograph and generated against the input a new
//!   top correction has now — with the accept's own codes and data, and a refusal leaves the draft
//!   as it was. A candidate the draft does not hold yet is read and verified on the source worker
//!   ([`CandidatePin::prepare`]) while the call is parked; the call then runs again from its
//!   request with the prepared patch in hand, so every check is made again on the owner's state of
//!   that moment. A caller that may not wait is answered as a planning read's is, `not-ready` with
//!   [`super::PIXEL_READ_REQUIRED`], having changed nothing.
//! - **Held in the draft.** The prepared patch lives in the session's one draft
//!   ([`crate::DraftCandidate`]), so a session holds at most one; it goes when the draft names
//!   another candidate, is cancelled or closes.
//! - **Superseded.** A later `draft.set`, `draft.commit` or `draft.cancel` of the same client,
//!   whether it parks or is answered at once, supersedes a `draft.set` still waiting for its
//!   preparation: that one is answered `cancelled` and its read stopped, so it never runs again
//!   over the later call's draft.
//! - **`draft.commit`** is the accept ([`super::accept`]), with the draft's request at its base
//!   revision, publishing the patch the draft holds rather than reading the candidate again; the
//!   draft closes when the entry commits and stays when the accept is refused.
//! - **The lock.** A fill's lock on the photograph refuses neither the draft's begin nor its
//!   reapply, since a candidate draft commits nothing; its commit is refused as an accept is.
use super::{
    Call, Owner, OwnerCall, PIXEL_READ_REQUIRED, SourceFlightKey, SourceResult, SourceTaskKind,
    accept, methods, requests::RequestKey,
};
use crate::{
    AcceptedCandidate, AssetId, ClientId, DraftCandidate, Error, JobId,
    api::{
        ApiRequest, ClientSession,
        methods::{DraftCommit, DraftSet},
        params,
    },
    artifacts::{
        PreparedArtifact,
        candidates::{CandidatePin, CandidateRead},
    },
    draft::named_candidate,
    jobs::{JobKind, Release},
};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::AtomicBool};

/// What the source worker runs for a candidate draft: the candidate, kept from release while it
/// is read, and the photograph it belongs to.
pub(super) struct PrepareWork {
    asset_id: AssetId,
    pin: CandidatePin,
}

/// What it hands back: the candidate's patch, read and verified, and the read's pin, which the
/// owner lets go.
pub(super) struct PreparedPatch {
    job_id: JobId,
    candidate: u8,
    artifact: Arc<PreparedArtifact>,
    _pin: CandidatePin,
}

impl PrepareWork {
    /// Read and verify the stored patch. Worker-side.
    pub(super) fn run(self, cancel: &AtomicBool) -> Result<PreparedPatch, Error> {
        let artifact = self.pin.prepare(cancel)?;
        Ok(PreparedPatch {
            job_id: self.pin.job_id().clone(),
            candidate: self.pin.candidate().candidate,
            artifact,
            _pin: self.pin,
        })
    }

    pub(super) fn asset_id(&self) -> &AssetId {
        &self.asset_id
    }

    /// The activity a preparation shows.
    pub(super) fn detail(&self) -> Option<String> {
        let read = self.pin.candidate();
        Some(format!("{} {}", read.label, read.candidate))
    }
}

/// Whether `session`'s draft is a candidate draft: a draft of a candidate-accepting action.
fn candidate_draft(owner: &Owner, session: Option<&ClientSession>) -> bool {
    session
        .and_then(|session| session.draft.as_ref())
        .and_then(|draft| owner.service.registry().resolve_action(&draft.action))
        .is_some_and(|action| action.descriptor().accepts_candidate)
}

/// Whether `request` is a `draft.set` or `draft.commit` of `client`'s candidate draft, which the
/// owner answers here rather than through the editor's draft handlers.
pub(super) fn handles(owner: &Owner, client: ClientId, request: &ApiRequest) -> bool {
    matches!(request.method.as_str(), "draft.set" | "draft.commit")
        && candidate_draft(owner, owner.sessions.get(&client))
}

/// Whether a fill's lock on the photograph lets `request` through: a candidate draft's begin and
/// reapply commit nothing.
pub(super) fn unlocked(owner: &Owner, client: ClientId, request: &ApiRequest) -> bool {
    match request.method.as_str() {
        "draft.begin" => request.params["action"]
            .as_str()
            .and_then(|action| owner.service.registry().resolve_action(action))
            .is_some_and(|action| action.descriptor().accepts_candidate),
        "draft.reapply" => candidate_draft(owner, owner.sessions.get(&client)),
        _ => false,
    }
}

/// The checks an accept makes of a candidate before it pins it, made here of a candidate a draft
/// is to show, with the accept's codes and data: a job `client` may not read or that is not a
/// task, a candidate that is not live, of another photograph, or generated against another input.
/// Answers the candidate held for one read.
fn checked(
    owner: &Owner,
    client: ClientId,
    asset_id: &AssetId,
    job_id: &JobId,
    candidate: u8,
) -> Result<CandidatePin, Error> {
    task_job(owner, client, job_id)?;
    let pin = owner.service.candidates().reader(job_id, candidate)?;
    of_this_input(owner, pin.candidate(), asset_id, job_id)?;
    Ok(pin)
}

/// The job is a task `client` may read.
pub(super) fn task_job(owner: &Owner, client: ClientId, job_id: &JobId) -> Result<(), Error> {
    let record = owner.jobs.read_for(job_id, client)?;
    if record.kind != JobKind::Task {
        return Err(Error::validation(format!(
            "job {job_id} is not a task and holds no candidates"
        )));
    }
    Ok(())
}

/// The candidate is of `asset_id` and was generated against the input a new top correction of it
/// has now.
pub(super) fn of_this_input(
    owner: &Owner,
    read: &CandidateRead,
    asset_id: &AssetId,
    job_id: &JobId,
) -> Result<(), Error> {
    let candidate = read.candidate;
    if read.asset_id != *asset_id {
        return Err(Error::validation(format!(
            "candidate {candidate} of job {job_id} belongs to another photograph"
        )));
    }
    let Some(generated) = &read.input else {
        return Err(Error::conflict(format!(
            "candidate {candidate} of job {job_id} does not say what it was generated against"
        )));
    };
    accept::check_input(generated, &owner.service.correction_input(asset_id)?)
}

/// `draft.set` of a candidate draft ([module](self) docs). Answers the draft, or leaves the call
/// for the owner to park while the named candidate's patch is prepared.
pub(super) fn set(owner: &mut Owner, call: &Call<'_>) -> Result<Value, Error> {
    let p: DraftSet = params::parse(&call.request.params)?;
    let client = call.client;
    let session = owner.sessions.entry(client).or_default();
    let complete = methods::checked_set(&owner.service, session, &p.draft_id, &p.fields)?;
    let mut next = session.held_draft(&p.draft_id)?.merged(p.fields);
    let named = complete.then(|| named_candidate(&next.fields)).flatten();
    match named {
        // A partial draft names no candidate yet, and holds none.
        None => next.candidate = None,
        Some(_)
            if next
                .candidate
                .as_ref()
                .is_some_and(|held| held.names(&next.fields)) => {}
        Some((job_id, candidate)) => {
            let pin = checked(owner, client, &next.asset_id, &job_id, candidate)?;
            let in_hand = owner
                .draft_patch_in_hand
                .as_ref()
                .filter(|patch| patch.job_id == job_id && patch.candidate == candidate)
                .map(|patch| patch.artifact.clone());
            match in_hand {
                Some(artifact) => {
                    next.candidate = Some(Arc::new(drafted(
                        pin.candidate(),
                        job_id,
                        candidate,
                        artifact,
                    )?));
                }
                None if !owner.call_parks => {
                    return Err(Error::not_ready(
                        "a candidate draft reads the candidate's patch off the catalog owner; \
                         send the call where it may wait",
                    )
                    .with_data(json!({ "reason": PIXEL_READ_REQUIRED })));
                }
                None => return prepare(owner, client, next.asset_id.clone(), pin),
            }
        }
    }
    // Planned now, so a refusal of the drafted accept is this set's and the draft stays as it was.
    if complete {
        owner.service.draft_recipe(&next.asset_id, &next)?;
    }
    let session = owner.sessions.entry(client).or_default();
    session.draft = Some(next);
    session.touch();
    methods::draft_value(&owner.service, session)
}

/// What the draft holds of a candidate whose patch is `artifact`: what its accept would commit,
/// with the identity publishing gives the patch.
fn drafted(
    read: &CandidateRead,
    job_id: JobId,
    candidate: u8,
    artifact: Arc<PreparedArtifact>,
) -> Result<DraftCandidate, Error> {
    let input = read
        .input
        .clone()
        .ok_or_else(|| Error::conflict("the candidate holds no input identity"))?;
    Ok(DraftCandidate {
        job_id,
        candidate,
        accepted: Arc::new(AcceptedCandidate {
            artifact: artifact.id.clone(),
            rect: read.rect,
            label: read.label.clone(),
            provenance: crate::artifacts::candidates::stored_provenance(&read.provenance),
            input,
        }),
        artifact,
    })
}

/// Queue the read of the candidate `pin` holds on the source worker, and leave the call for the
/// owner to park until it is prepared.
fn prepare(
    owner: &mut Owner,
    client: ClientId,
    asset_id: AssetId,
    pin: CandidatePin,
) -> Result<Value, Error> {
    let (job_id, candidate) = (pin.job_id().clone(), pin.candidate().candidate);
    let job = owner.sources.submit(
        &mut owner.jobs,
        client,
        SourceFlightKey {
            path: format!("draft-candidate:{job_id}:{candidate}").into(),
            signature: None,
            expected_fingerprint: None,
            gains_bits: None,
            artifacts: Vec::new(),
        },
        SourceTaskKind::DraftPatch(Box::new(PrepareWork { asset_id, pin })),
        Vec::new(),
        None,
        false,
        None,
    )?;
    owner.draft_patch_parking = Some(job);
    Ok(Value::Null)
}

/// `draft.commit` of a candidate draft: the accept of the candidate it shows, with the draft's
/// request at its base revision. `key` is the commit's own request-table key, which records its
/// answer once the entry commits.
pub(super) fn commit(
    owner: &mut Owner,
    call: &Call<'_>,
    key: Option<RequestKey>,
) -> Result<Value, Error> {
    let p: DraftCommit = params::parse(&call.request.params)?;
    let session = owner.sessions.entry(call.client).or_default();
    let draft = methods::committable(&owner.service, session, &p)?;
    let action = draft.action.clone();
    let mut parameters = draft.request();
    parameters.insert("asset_id".into(), json!(draft.asset_id));
    parameters.insert("mutation".into(), methods::value(&p.mutation)?);
    // The patch the draft shows was read and verified when it was drafted: the accept publishes
    // those bytes rather than reading the candidate again.
    let patch = draft
        .candidate
        .as_ref()
        .filter(|held| held.names(&draft.fields))
        .map(|held| held.artifact.clone());
    accept::start(
        owner,
        call,
        &action,
        parameters,
        owner.call_parks,
        Some(accept::DraftClose {
            draft_id: p.draft_id,
            key,
            patch,
        }),
    )
}

/// Whether `request` changes or ends its client's draft, and so supersedes a `draft.set` of that
/// client still waiting for its candidate's patch ([`Owner::supersede_draft_patches`]).
pub(super) fn supersedes(request: &ApiRequest) -> bool {
    matches!(
        request.method.as_str(),
        "draft.set" | "draft.commit" | "draft.cancel"
    )
}

impl Owner {
    /// Park the `draft.set` that [`prepare`] left a job for, until the job completes. The call
    /// that parked it already superseded any other of its client's.
    pub(super) fn park_draft_patch(&mut self, call: OwnerCall, job: JobId) {
        self.parked_draft_patches.insert(job, call);
    }

    /// `client` sent a later `draft.set`, `draft.commit` or `draft.cancel` ([`supersedes`]): a
    /// `draft.set` of its still waiting for its candidate's patch is answered `cancelled` now, so
    /// it never runs again over what the later call left, and its job is stopped, so the read ends
    /// and leaves the source queue.
    pub(super) fn supersede_draft_patches(&mut self, client: ClientId) {
        let superseded: Vec<JobId> = self
            .parked_draft_patches
            .iter()
            .filter(|(_, parked)| parked.client == client)
            .map(|(job, _)| job.clone())
            .collect();
        for job in superseded {
            if let Some(parked) = self.parked_draft_patches.remove(&job) {
                self.refuse(
                    parked,
                    Error::cancelled("superseded: this client changed its draft again"),
                );
            }
            if self.jobs.release(&job, client).ok() == Some(Release::Stopped) {
                self.stop(&job, JobKind::Artifacts);
            }
        }
    }

    /// Drop every `draft.set` `client` parked: it left, and its jobs are stopped with it.
    pub(super) fn drop_parked_draft_patches(&mut self, client: ClientId) {
        self.parked_draft_patches
            .retain(|_, parked| parked.client != client);
    }

    /// The source worker finished preparing a candidate's patch for the `draft.set` parked on
    /// `job`: the call runs again from its request with the patch in hand, or is answered with the
    /// worker's refusal. Returns what the job records.
    pub(super) fn draft_patch_prepared(
        &mut self,
        job: &JobId,
        result: Result<SourceResult, Error>,
    ) -> Result<Value, Error> {
        let parked = self.parked_draft_patches.remove(job);
        let patch = match result {
            Ok(SourceResult::DraftPatch(patch)) => patch,
            Ok(_) => return Err(Error::internal("a candidate draft's job prepared nothing")),
            Err(error) => {
                if let Some(parked) = parked {
                    self.refuse(parked, error.clone());
                }
                return Err(error);
            }
        };
        let artifact = patch.artifact.id.clone();
        let Some(parked) = parked else {
            return Err(Error::cancelled(
                "the draft no longer waits for the candidate",
            ));
        };
        self.draft_patch_in_hand = Some(*patch);
        self.call(parked);
        self.draft_patch_in_hand = None;
        Ok(json!({ "artifact": artifact }))
    }
}
