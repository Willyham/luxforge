//! `update.status`, `update.check`, `update.install`, `update.skip` and
//! `update.move-to-applications`: the catalog owner's side of automatic updates ([`crate::update`],
//! `docs/design/auto-update.md`). The owner validates each command and forwards it to the host's
//! [`UpdatePort`]; every port call returns at once, so the owner never waits on the network.
//!
//! - **A check is answered with its outcome.** `update.check` starts one check on the port and parks
//!   the call, as `events.wait` is parked, until the port finishes it ([`crate::update::CheckDone`])
//!   or [`CHECK_WAIT`] passes; the answer is the status then. A call already waiting joins the check
//!   that is running, so at most one check the owner started runs at a time, and at most
//!   [`MAX_HELD_CHECKS`] calls wait for it. A caller that may not wait ([`Owner::call_parks`]) and a
//!   call past the bound are answered at once, with `checking: true`.
//! - **What the port reports is announced.** A port signals its notifier when its state changes;
//!   after each message the owner reads the status and records an event, `update.status` with a
//!   request id `update:<n>`, when anything but download progress moved since the last it
//!   recorded. A client watching the event log reads `update.status` again.
//! - **The preferences reach the port.** The owner applies the policy at its start, and again after
//!   every change to the update preferences, the skipped version or the pre-release flag. It keeps
//!   the policy it applied, so reading the port's signal reads no file; `update.status`,
//!   `update.check` and `update.install` read it again, one small file, and apply a change another
//!   process made.
use super::{Call, Owner};
use crate::{
    Error, JobId,
    api::{
        ApiResponse, Origin, announce_once,
        methods::value,
        params::{NoParams, host_params},
        response::ResponseSender,
    },
    jobs::JobKind,
    update::{
        CheckDone, MAX_VERSION_LEN, Mechanism, UpdateNotifier, UpdatePolicy, UpdatePort,
        UpdateStatus,
    },
};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// The longest `update.check` waits for its check before it is answered with the status as it is,
/// still checking.
pub(super) const CHECK_WAIT: Duration = Duration::from_secs(60);

/// The most `update.check` calls held waiting for a check at once; one more is answered at once.
pub(super) const MAX_HELD_CHECKS: usize = 8;

/// The jobs an update waits for: work a person started that writes what they asked for, which the
/// quit path would cancel. Previews, analysis, source preparation and listings are regenerated and
/// do not hold an update back.
const BLOCKING_JOBS: &[JobKind] = &[
    JobKind::Export,
    JobKind::BatchExport,
    JobKind::HdrMerge,
    JobKind::PanoramaMerge,
    JobKind::DevelopPicks,
    JobKind::BatchSettings,
    JobKind::Install,
    JobKind::Remove,
    JobKind::Task,
];

/// The `data.reason` of a refusal from a Linux installation that cannot replace itself.
const NOTIFY_ONLY: &str = "notify-only";
/// The `data.reason` of an install with nothing to install.
const NO_UPDATE: &str = "no-update";
/// The `data.reason` of a move the port cannot make.
const NOT_SUPPORTED: &str = "not-supported";

/// One `update.check` waiting for its check.
struct HeldCheck {
    id: String,
    generation: u64,
    deadline: Instant,
    response: ResponseSender,
}

/// The owner's update state: the notifier it hands its port, the checks it started and the calls
/// waiting for them, and what it last announced.
pub(super) struct Updates {
    notifier: UpdateNotifier,
    /// The generation the next check takes.
    next: u64,
    /// The check this owner started that has not finished.
    running: Option<u64>,
    /// Set by `update.check` when its call waits: the generation it waits for. Empty between calls.
    pub(super) parking: Option<u64>,
    held: Vec<HeldCheck>,
    /// The status as last announced, without its progress.
    announced: Option<UpdateStatus>,
    /// `update.install` was accepted.
    install_requested: bool,
    /// The policy last applied: what the owner reads its status with between commands, so a
    /// port's signal reads no file.
    policy: UpdatePolicy,
    /// How many changes the port's own events have announced, which names the next.
    events: u64,
}

impl Updates {
    /// The owner's update state, whose notifier calls `wake`, which must never block.
    pub(super) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            notifier: UpdateNotifier::new(wake),
            next: 1,
            running: None,
            parking: None,
            held: Vec::new(),
            announced: None,
            install_requested: false,
            policy: UpdatePolicy::new(true, false, None),
            events: 0,
        }
    }

    /// The soonest held check's deadline: how long the owner may sleep on its channel.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.held.iter().map(|held| held.deadline).min()
    }
}

fn port(owner: &Owner) -> Option<Arc<dyn UpdatePort>> {
    owner.host.updates().cloned()
}

/// The policy the stored preferences give now. A store that cannot be read gives the defaults, as
/// the launch does: the desktop reports that failure when it reads them.
fn read_policy(owner: &Owner) -> UpdatePolicy {
    let preferences = owner.host.preferences.read().unwrap_or_default();
    crate::update::policy(&preferences)
}

/// The status as the port reports it now, with the policy last applied.
fn status(owner: &Owner) -> UpdateStatus {
    let port = port(owner).map(|port| port.status());
    UpdateStatus::compose(
        port.as_ref(),
        &owner.updates.policy,
        owner.updates.install_requested,
    )
}

/// Read the policy again, as each `update.*` command does, and apply it if another process changed
/// what it is read from: one small file read.
fn refresh(owner: &mut Owner) {
    let policy = read_policy(owner);
    if policy != owner.updates.policy {
        apply(owner, policy);
    }
}

fn apply(owner: &mut Owner, policy: UpdatePolicy) {
    if let Some(port) = port(owner) {
        port.apply(&policy);
    }
    owner.updates.policy = policy;
}

/// Hand the port the owner's notifier and the person's choices, once as the owner starts.
pub(super) fn launch(owner: &mut Owner) {
    if let Some(port) = port(owner) {
        port.attach(owner.updates.notifier.clone());
    }
    applied(owner);
}

/// Read the policy and apply it to the port after a change the calling method announces itself;
/// the status then is the one announced from now on.
pub(super) fn applied(owner: &mut Owner) {
    let policy = read_policy(owner);
    apply(owner, policy);
    owner.updates.announced = Some(status(owner).settled());
}

/// After each message: announce what the port changed, and answer every held check that is over.
pub(super) fn serve(owner: &mut Owner) {
    if owner.updates.notifier.take() {
        let settled = status(owner).settled();
        if owner.updates.announced.as_ref() != Some(&settled) {
            owner.updates.announced = Some(settled);
            owner.updates.events += 1;
            let origin = Origin::new("update.status", &format!("update:{}", owner.updates.events));
            announce_once(&mut owner.announced, &origin);
            owner.record_announced();
        }
    }
    answer_held(owner);
}

/// Answer every held check whose check ended, whose deadline passed or whose caller left. Also
/// called before the owner sleeps, so a deadline is kept however quiet the owner is.
pub(super) fn answer_held(owner: &mut Owner) {
    if let Some(running) = owner.updates.running
        && owner.updates.notifier.finished(running)
    {
        owner.updates.running = None;
    }
    if owner.updates.held.is_empty() {
        return;
    }
    let now = Instant::now();
    let notifier = owner.updates.notifier.clone();
    let (over, held): (Vec<_>, Vec<_>) = std::mem::take(&mut owner.updates.held)
        .into_iter()
        .partition(|held| {
            notifier.finished(held.generation) || held.deadline <= now || held.response.closed()
        });
    owner.updates.held = held;
    if over.is_empty() {
        return;
    }
    let answer = value(status(owner));
    for held in over {
        let response = match &answer {
            Ok(value) => ApiResponse::value(held.id, owner.log.sequence, value.clone()),
            Err(error) => ApiResponse::failure(held.id, owner.log.sequence, error.clone()),
        };
        // A caller that has gone has nobody to answer.
        let _ = held.response.send(response);
    }
}

/// Hold one `update.check` until its check ends or [`CHECK_WAIT`] passes.
pub(super) fn park(owner: &mut Owner, id: String, response: ResponseSender, generation: u64) {
    owner.updates.held.push(HeldCheck {
        id,
        generation,
        deadline: Instant::now() + CHECK_WAIT,
        response,
    });
    // A check that finished as it started is answered at once.
    serve(owner);
}

/// A `not-ready` refusal naming the mechanism's reason.
fn unavailable(status: &UpdateStatus, doing: &str) -> Option<Error> {
    match status.mechanism {
        Mechanism::Unavailable => {
            let reason = status.reason.map_or("headless", |reason| reason.as_str());
            Some(
                Error::not_ready(format!("{doing} is unavailable: {reason}"))
                    .with_data(json!({ "reason": reason })),
            )
        }
        _ => None,
    }
}

/// A `conflict` naming every job an update would interrupt, if any runs.
fn running_jobs(owner: &Owner, doing: &str) -> Result<(), Error> {
    let jobs: Vec<(JobId, JobKind)> = owner.jobs.live_of(BLOCKING_JOBS);
    if jobs.is_empty() {
        return Ok(());
    }
    let kind = |kind: &JobKind| {
        serde_json::to_value(kind)
            .ok()
            .and_then(|kind| kind.as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    let named: Vec<String> = jobs
        .iter()
        .map(|(job, job_kind)| format!("{} {}", kind(job_kind), job.as_str()))
        .collect();
    Err(Error::conflict(format!(
        "{doing} waits until no job runs: {}",
        named.join(", ")
    ))
    .with_data(json!({
        "jobs": jobs
            .iter()
            .map(|(job, job_kind)| json!({"job_id": job.as_str(), "kind": kind(job_kind)}))
            .collect::<Vec<_>>(),
    })))
}

/// `update.status`.
pub(in crate::api) fn read(owner: &mut Owner, _: &Call<'_>, _: NoParams) -> Result<Value, Error> {
    refresh(owner);
    value(status(owner))
}

/// `update.check`: start a check, or join the one this owner started that is still running, and
/// wait for it where the caller may.
pub(in crate::api) fn check(owner: &mut Owner, _: &Call<'_>, _: NoParams) -> Result<Value, Error> {
    refresh(owner);
    let now = status(owner);
    if let Some(refusal) = unavailable(&now, "checking for updates") {
        return Err(refusal);
    }
    let Some(port) = port(owner) else {
        return Err(
            Error::not_ready("checking for updates is unavailable: headless")
                .with_data(json!({ "reason": "headless" })),
        );
    };
    let generation = match owner.updates.running {
        Some(running) if !owner.updates.notifier.finished(running) => running,
        _ => {
            let generation = owner.updates.next;
            owner.updates.next += 1;
            owner.updates.running = Some(generation);
            port.check(CheckDone::new(owner.updates.notifier.clone(), generation));
            generation
        }
    };
    if owner.call_parks
        && owner.updates.held.len() < MAX_HELD_CHECKS
        && !owner.updates.notifier.finished(generation)
    {
        owner.updates.parking = Some(generation);
        return Ok(Value::Null);
    }
    value(status(owner))
}

host_params! {
    pub(in crate::api) struct UpdateRequest {
        mutation: MutationRequest,
    }
}

/// `update.install`: validated here, carried out by the port through the desktop's quit path.
pub(in crate::api) fn install(
    owner: &mut Owner,
    call: &Call<'_>,
    _: UpdateRequest,
) -> Result<Value, Error> {
    refresh(owner);
    let now = status(owner);
    if let Some(refusal) = unavailable(&now, "installing an update") {
        return Err(refusal);
    }
    if now.mechanism == Mechanism::LinuxNotify {
        return Err(Error::not_ready(
            "this installation cannot replace itself; open the release page instead",
        )
        .with_data(json!({ "reason": NOTIFY_ONLY })));
    }
    if now.available.is_none() {
        return Err(
            Error::not_ready("no update is available").with_data(json!({ "reason": NO_UPDATE }))
        );
    }
    running_jobs(owner, "installing an update")?;
    let Some(port) = port(owner) else {
        return Err(
            Error::not_ready("installing an update is unavailable: headless")
                .with_data(json!({ "reason": "headless" })),
        );
    };
    port.request_install()?;
    if !std::mem::replace(&mut owner.updates.install_requested, true) {
        announce_once(&mut owner.announced, &call.origin);
    }
    owner.updates.announced = Some(status(owner).settled());
    Ok(json!({ "outcome": "requested", "status": status(owner) }))
}

host_params! {
    pub(in crate::api) struct UpdateSkip {
        version: String = string(MAX_VERSION_LEN).notes("the version to stop offering, as update.status names it: ASCII letters, digits, '.', '-' and '+'"),
    }
}

/// `update.skip`: one locked write of the skipped version, announced when it changed.
pub(in crate::api) fn skip(
    owner: &mut Owner,
    call: &Call<'_>,
    params: UpdateSkip,
) -> Result<Value, Error> {
    crate::update::check_version(&params.version).map_err(Error::validation)?;
    let changed = owner
        .host
        .preferences
        .set_skipped_version_published(&params.version)?;
    if let Some(port) = port(owner) {
        port.skip(&params.version);
    }
    if changed.value {
        announce_once(&mut owner.announced, &call.origin);
    }
    applied(owner);
    let changed = changed.finish()?;
    Ok(json!({
        "outcome": if changed { "applied" } else { "no-op" },
        "status": status(owner),
    }))
}

host_params! {
    pub(in crate::api) struct UpdateMove {
        mutation: MutationRequest,
        replace: Option<bool> = boolean().default(false).notes("the person confirmed that a Luxforge already in the destination may be moved to the Trash; without it such a move is refused as conflict with data.reason existing-app and data.path"),
    }
}

/// `update.move-to-applications`: validated here, carried out by the port, which relaunches through
/// the same quit path as an install.
pub(in crate::api) fn move_to_applications(
    owner: &mut Owner,
    _: &Call<'_>,
    params: UpdateMove,
) -> Result<Value, Error> {
    let port = port(owner).filter(|port| port.status().can_move_to_applications);
    let Some(port) = port else {
        return Err(
            Error::not_ready("this installation cannot move into Applications")
                .with_data(json!({ "reason": NOT_SUPPORTED })),
        );
    };
    running_jobs(owner, "moving into Applications")?;
    port.move_to_applications(params.replace.unwrap_or(false))?;
    Ok(json!({ "outcome": "requested", "status": status(owner) }))
}
