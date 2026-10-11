//! Remove, the Corrections band's fill mode (`docs/design/ai-editing.md`, "Desktop surface"): the
//! controller of the selection a press paints, the requests its controls send and the job and
//! preview reads that follow them.
//!
//! Every control is one request an API client sends: Generate and Regenerate are the fill tier's
//! `task.fill-…`, Cancel is `job.cancel`, the strip reads each candidate's preview with
//! `job.read-candidate` and Accept is `edit.accept-correction-patch`. Each is built once, by the
//! builder the Copy as JSON menu reads too, so what is copied is what is sent. The selection, the
//! seed it starts from, the chosen candidate and the prompt as typed are this window's own view
//! state. A job is followed by its own event-driven reader (`job.wait`); each preview is read and
//! decoded off the update loop and turned into one image handle, held until its candidate leaves
//! the strip, so the renderer uploads it once.
use super::{
    Before, Editor,
    decoded_handles::image_handle,
    gesture::Starting,
    job_reads::{self, Pass, Reader, Watch},
    message::{
        Message,
        capability::CapabilityMessage,
        remove::{RemoveMessage, RemoveRequest},
        settings::SettingsMessage,
    },
    tasks::{self, CallError, owner_task},
};
use crate::{
    brush::{BrushGesture, Modifiers, Moved, Released, ViewPoint},
    state::{
        MenuTarget,
        capabilities::Operation,
        remove::{
            FillJob, FillTier, RemoveNotice, RemoveSelection, chosen_tier, fill_module,
        },
        settings::SettingsTab,
    },
};
use iced::{
    Subscription, Task,
    futures::stream::BoxStream,
    widget::image::Handle,
};
use luxforge_core::{
    AssetId, CandidateState, ClientId, DecodedPreview, ErrorKind, OwnerHandle,
    jobs::{JOB_CANCEL, JOB_READ, JobRecord, JobStatus},
    redact_params,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};

/// `job.read-candidate`, which reads a candidate's preview in chunks.
pub(crate) const READ_CANDIDATE: &str = "job.read-candidate";
/// `edit.accept-correction-patch`, which commits one candidate as a frozen patch.
pub(crate) const ACCEPT: &str = "edit.accept-correction-patch";
/// The most candidate previews held decoded: the strip's.
const HELD: usize = crate::state::remove::STRIP_MAX;
/// Seeds are drawn below this, so a selection's Regenerates stay inside the request's seed range.
const SEED_RANGE: u64 = 1 << 40;

/// One candidate, by its job and number.
type CandidateKey = (String, u8);

/// What a start in flight was for: the selection it fills, from which seed, with which prompt.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Start {
    pub(crate) asset: AssetId,
    pub(crate) serial: u64,
    pub(crate) seed: u64,
    pub(crate) prompt: Option<String>,
}

/// What an evidence step on Remove waits for before its frame is captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoveWait {
    /// The generation it started shows a candidate's progress, or has ended.
    Progress,
    /// Nothing of Remove's is in flight: no start, no generation on the photograph and every
    /// strip candidate's preview read.
    Done,
}

/// The mode's own bookkeeping beside its view state ([`crate::state::remove::RemovePanel`]).
#[derive(Debug, Default)]
pub(crate) struct Remove {
    /// The stroke being painted, and its path as posted at the selection's radius so far.
    painting: Option<(BrushGesture, Vec<[f64; 2]>)>,
    /// Where the last selection stroke ended, for `⇧`-click.
    last_end: Option<[f64; 2]>,
    /// The start in flight.
    pub(crate) starting: Option<Start>,
    /// Decoded previews, oldest first: one handle per candidate, kept while it is in the strip.
    pub(crate) previews: Vec<(CandidateKey, Handle)>,
    /// Preview reads in flight, and those that failed, which are not asked again.
    pub(crate) reading: BTreeSet<CandidateKey>,
    failed: BTreeSet<CandidateKey>,
    /// The revision the strip's candidate states were last read at: another client's accept
    /// changes them.
    read_at: Option<u64>,
    /// The tiers were asked for on entering Remove.
    asked_tiers: bool,
    /// The last request Remove sent, as `(method, params)`: what an API client sends for the
    /// same gesture.
    pub(crate) last_request: Option<(String, Value)>,
    /// The last request its menu copied.
    pub(crate) last_copied: Option<Value>,
    /// What the running evidence step waits for.
    pub(crate) wait: Option<RemoveWait>,
}

/// The candidate previews the strip and the canvas draw, borrowed so each handle keeps its id and
/// uploads once.
#[derive(Clone, Copy)]
pub(crate) struct RemoveImages<'a> {
    previews: &'a [(CandidateKey, Handle)],
}

impl<'a> RemoveImages<'a> {
    pub(crate) fn image(&self, job: &str, candidate: u8) -> Option<&'a Handle> {
        let previews: &'a [(CandidateKey, Handle)] = self.previews;
        previews
            .iter()
            .find(|((held, number), _)| held == job && *number == candidate)
            .map(|(_, handle)| handle)
    }
}

/// What the canvas draws of Remove: the selection, the stroke being painted, a running
/// generation's ring and the chosen candidate over the region it fills.
#[derive(Clone, Copy)]
pub(crate) struct RemoveCanvas<'a> {
    pub(crate) selection: Option<&'a [[f64; 2]]>,
    pub(crate) live: Option<&'a [[f64; 2]]>,
    pub(crate) radius: f64,
    /// A generation runs: its fraction, when it reports one.
    pub(crate) progress: Option<Option<f64>>,
    /// The chosen candidate's preview and the content pixels it covers.
    pub(crate) preview: Option<(&'a Handle, [u32; 4])>,
}

/// A standard base64 string (with padding) as its bytes.
pub(crate) fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    fn value(byte: u8) -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err("base64 text is not a whole number of quanta".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (index, quantum) in bytes.chunks_exact(4).enumerate() {
        let last = index + 1 == bytes.len() / 4;
        let padding = quantum.iter().rev().take_while(|byte| **byte == b'=').count();
        if padding > 2 || (padding > 0 && !last) {
            return Err("base64 padding is misplaced".into());
        }
        let mut word = 0u32;
        for byte in &quantum[..4 - padding] {
            word = (word << 6) | value(*byte).ok_or("base64 text has a byte outside its alphabet")?;
        }
        word <<= 6 * padding as u32;
        let decoded = [(word >> 16) as u8, (word >> 8) as u8, word as u8];
        out.extend_from_slice(&decoded[..3 - padding]);
    }
    Ok(out)
}

/// Read one candidate's whole preview through `job.read-candidate`, chunk by chunk, and decode it
/// to RGBA rows. Runs on the blocking pool, never on the update loop.
pub(crate) fn read_preview(
    owner: &OwnerHandle,
    client: ClientId,
    job: &str,
    candidate: u8,
) -> Result<DecodedPreview, String> {
    let mut rgb = Vec::new();
    let (mut width, mut height) = (0, 0);
    let mut total;
    loop {
        let (answer, _) = tasks::call(
            owner,
            client,
            READ_CANDIDATE,
            json!({"job_id": job, "candidate": candidate, "part": "preview", "offset": rgb.len()}),
        )?;
        width = answer["width"].as_u64().unwrap_or(width);
        height = answer["height"].as_u64().unwrap_or(height);
        let whole = answer["total"]
            .as_u64()
            .ok_or("job.read-candidate omitted its total")?;
        total = whole;
        let chunk = decode_base64(answer["data"].as_str().unwrap_or_default())?;
        if chunk.is_empty() && (rgb.len() as u64) < whole {
            return Err("job.read-candidate answered an empty chunk".into());
        }
        rgb.extend_from_slice(&chunk);
        if rgb.len() as u64 >= whole {
            break;
        }
    }
    if total != width * height * 3 || rgb.len() as u64 != total {
        return Err(format!(
            "the preview's {} bytes are not {width} x {height} RGB",
            rgb.len()
        ));
    }
    let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
    for pixel in rgb.chunks_exact(3) {
        rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
    }
    Ok(DecodedPreview {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}

/// How often a start waits through a preparation and asks again before it reports the refusal.
const PREPARATION_RETRIES: usize = 4;

/// The fill request, waiting for the source and asking again, under its one request id, when the
/// owner answers `preparation-required`, as any client does. Runs on the blocking pool.
pub(crate) fn start_fill(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<Value, CallError> {
    let mut attempt = 0;
    loop {
        match tasks::call_detailed(owner, client, method, params.clone()) {
            Err(error)
                if error.code == ErrorKind::PreparationRequired.code()
                    && attempt < PREPARATION_RETRIES =>
            {
                attempt += 1;
                let job = error
                    .job_id
                    .clone()
                    .unwrap_or_else(|| error.message.clone());
                tasks::wait_source_job(owner, client, &job)?;
            }
            other => return other,
        }
    }
}

/// One fill job's reader: its first record, each change and its end, through `job.wait`.
pub(crate) fn fill_reads(reader: &Reader<String>) -> BoxStream<'static, Message> {
    let (owner, client) = (reader.owner.clone(), reader.client);
    let job = reader.identity.clone();
    let visible = reader.presentation_visible;
    let state = Arc::new(std::sync::Mutex::new((None, Watch::default())));
    job_reads::reads(move || {
        let (owner, job) = (owner.clone(), job.clone());
        let (previous, mut watch) = std::mem::take(&mut *state.lock().expect("fill reader"));
        let state = state.clone();
        async move {
            let result = job_reads::wait(&owner, client, &job, previous).await;
            let (next, result) = match result {
                Ok((change, value)) => (Some(change), Ok(value)),
                Err(error) => (previous, Err(error)),
            };
            let verdict = watch.observe_filtered(
                &result,
                |record| !matches!(record["status"].as_str(), Some("queued" | "running")),
                |one, two| {
                    if visible {
                        one == two
                    } else {
                        job_reads::same_without_progress(one, two)
                    }
                },
            );
            *state.lock().expect("fill reader") = (next, watch);
            Pass::of(verdict, || {
                Message::Remove(RemoveMessage::Polled {
                    job_id: job,
                    result: result.and_then(|value| {
                        serde_json::from_value(value)
                            .map(Box::new)
                            .map_err(|error| error.to_string())
                    }),
                })
            })
        }
    })
}

/// A seed for a new selection: different each time, and far enough below the request's range that
/// every Regenerate of it fits.
fn draw_seed(serial: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as u64);
    (nanos ^ serial.wrapping_mul(0x9E37_79B9_7F4A_7C15)) % SEED_RANGE
}

impl Editor {
    /// The fill module's tiers as its status last listed them.
    fn fill_tiers(&self) -> Option<(String, &[FillTier])> {
        let module = fill_module(&self.modules)?;
        let tiers = self
            .capabilities
            .module(&module.id)
            .status
            .as_ref()
            .map_or(&[][..], |status| status.fill_tiers.as_slice());
        Some((module.id.clone(), tiers))
    }

    /// The Corrections mode is on screen with Remove chosen.
    pub(crate) fn remove_active(&self) -> bool {
        self.remove.chosen && self.corrections_active() && fill_module(&self.modules).is_some()
    }

    /// Why the photograph on screen cannot be edited: a generation this desktop started is running
    /// on it, and the owner refuses every edit of it until the job ends. Another photograph stays
    /// editable.
    pub(crate) fn generation_lock(&self) -> Option<String> {
        let asset = &self.document.state.as_ref()?.asset.id;
        self.remove
            .running_on(asset)
            .map(|_| crate::state::GENERATING.to_owned())
    }

    /// The fill request Generate (or Regenerate) sends now, and what it was for.
    fn fill_request(&self, regenerate: bool) -> Result<(String, Value, Start), String> {
        let state = self
            .document
            .state
            .as_ref()
            .ok_or(crate::state::NO_PHOTOGRAPH)?;
        let asset = state.asset.id.clone();
        let (_, tiers) = self.fill_tiers().ok_or("No module fills a selection")?;
        let tier = chosen_tier(tiers, self.remove.tier.as_deref())
            .ok_or("No fill tier is offered")?;
        let model = tier
            .installed_model()
            .ok_or("The tier's model is not installed")?;
        let selection = self
            .remove
            .selection_on(&asset)
            .ok_or("Paint over what to remove first")?;
        let seed = if regenerate {
            self.remove
                .newest(&asset)
                .and_then(|job| job.next_seed)
                .ok_or("Generate before regenerating")?
        } else {
            selection.seed
        };
        let prompt = Some(self.remove.prompt.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|prompt| !prompt.is_empty());
        let mut params = json!({
            "asset_id": asset,
            "selection-points": selection.points,
            "selection-radius": selection.radius,
            "model": model.model,
            "seed": seed,
            "mutation": tasks::request(),
        });
        if let Some(prompt) = &prompt {
            params["prompt"] = json!(prompt);
        }
        Ok((
            tier.task.clone(),
            params,
            Start {
                asset,
                serial: selection.serial,
                seed,
                prompt,
            },
        ))
    }

    /// The chosen candidate's accept, against the revision on screen.
    fn accept_request(&self) -> Result<(String, Value, CandidateKey, u64), String> {
        let state = self
            .document
            .state
            .as_ref()
            .ok_or(crate::state::NO_PHOTOGRAPH)?;
        let (job, row) = self
            .remove
            .chosen_on(&state.asset.id)
            .ok_or("Choose a candidate first")?;
        if row.state != CandidateState::Live {
            return Err("That candidate is no longer held".into());
        }
        Ok((
            ACCEPT.into(),
            json!({
                "asset_id": state.asset.id,
                "job_id": job.job_id,
                "candidate": row.candidate,
                "mutation": tasks::mutation(state.revision),
            }),
            (job.job_id.clone(), row.candidate),
            state.revision,
        ))
    }

    /// Cancel of the generation running on the photograph on screen.
    fn remove_cancel_request(&self) -> Result<(String, Value), String> {
        let asset = &self
            .document
            .state
            .as_ref()
            .ok_or(crate::state::NO_PHOTOGRAPH)?
            .asset
            .id;
        let job = self
            .remove
            .running_on(asset)
            .ok_or("No generation is running")?;
        Ok((JOB_CANCEL.into(), json!({"job_id": job.job_id})))
    }

    /// The chosen candidate's preview read.
    fn preview_request(&self) -> Result<(String, Value), String> {
        let asset = &self
            .document
            .state
            .as_ref()
            .ok_or(crate::state::NO_PHOTOGRAPH)?
            .asset
            .id;
        let (job, row) = self
            .remove
            .chosen_on(asset)
            .ok_or("Choose a candidate first")?;
        Ok((
            READ_CANDIDATE.into(),
            json!({"job_id": job.job_id, "candidate": row.candidate, "part": "preview"}),
        ))
    }

    /// The request one Copy as JSON line copies: the one its control sends.
    pub(crate) fn remove_copy_request(&self, request: RemoveRequest) -> Result<Value, String> {
        let (method, params) = match request {
            RemoveRequest::Generate => self
                .fill_request(false)
                .map(|(method, params, _)| (method, params))?,
            RemoveRequest::Regenerate => self
                .fill_request(true)
                .map(|(method, params, _)| (method, params))?,
            RemoveRequest::Accept => self
                .accept_request()
                .map(|(method, params, _, _)| (method, params))?,
            RemoveRequest::Cancel => self.remove_cancel_request()?,
            RemoveRequest::Preview => self.preview_request()?,
        };
        let params = redact_params(&method, &params);
        Ok(json!({"method": method, "params": params}))
    }

    /// Read the fill module's status, for its tiers, unless a read is already out.
    pub(crate) fn remove_refresh_tiers(&mut self) -> Task<Message> {
        let Some(module) = fill_module(&self.modules).map(|module| module.id.clone()) else {
            return Task::none();
        };
        if self.capabilities.module(&module).pending > 0 {
            return Task::none();
        }
        self.capability_op(&module, Operation::Refresh)
    }

    /// One Remove message.
    pub(crate) fn remove_message(&mut self, message: RemoveMessage) -> Task<Message> {
        match message {
            RemoveMessage::Choose => {
                self.view_state.menu = None;
                self.remove.chosen = true;
                self.correction_reads.line = None;
                self.corrections.selected = None;
                return self.remove_refresh_tiers();
            }
            RemoveMessage::Tier(tier) => self.remove.tier = Some(tier),
            RemoveMessage::Prompt(text) => {
                let max = self.workspace.corrections.remove.as_ref().map_or(256, |model| model.prompt_max);
                self.remove.prompt = text.chars().take(max).collect();
            }
            RemoveMessage::Generate => return self.remove_generate(false),
            RemoveMessage::Regenerate => return self.remove_generate(true),
            RemoveMessage::Cancel => return self.remove_cancel(),
            RemoveMessage::Candidate { job_id, candidate } => {
                self.remove.candidate = Some((job_id.clone(), candidate));
                let key = (job_id, candidate);
                // A preview that left the held set is read again when it is chosen.
                self.remove_reads.failed.remove(&key);
            }
            RemoveMessage::Accept => return self.remove_accept(),
            RemoveMessage::Download {
                module_id,
                resource,
            } => {
                return self.dispatch(Message::Capability(CapabilityMessage::Install {
                    module_id,
                    resource,
                }));
            }
            RemoveMessage::Experiments => {
                return self.dispatch(Message::Settings(SettingsMessage::Open(
                    SettingsTab::Experiments,
                )));
            }
            RemoveMessage::Menu(open) => {
                self.view_state.menu = open.then_some(MenuTarget::Remove);
            }
            RemoveMessage::Copy(request) => {
                self.view_state.menu = None;
                return match self.remove_copy_request(request) {
                    Ok(copied) => {
                        self.status.text = format!(
                            "Copied the {} request",
                            copied["method"].as_str().unwrap_or_default()
                        );
                        let text = serde_json::to_string_pretty(&copied).unwrap_or_default();
                        self.remove_reads.last_copied = Some(copied);
                        iced::clipboard::write(text)
                    }
                    Err(reason) => {
                        self.status.text = reason;
                        Task::none()
                    }
                };
            }
            RemoveMessage::Started(result) => self.remove_started(result),
            RemoveMessage::Cancelled { job_id, result } => {
                self.remove.pending = self.remove.pending.saturating_sub(1);
                match result {
                    // The owner releases the lock at once; the reader's last read confirms it.
                    Ok(()) => {
                        if let Some(job) = self.remove.job_mut(&job_id)
                            && job.running()
                        {
                            job.status = JobStatus::Cancelled;
                            job.cancelling = false;
                        }
                        self.status.text = "Generation cancelled".into();
                    }
                    Err(error) => {
                        if let Some(job) = self.remove.job_mut(&job_id) {
                            job.cancelling = false;
                        }
                        self.status.text = error;
                    }
                }
            }
            RemoveMessage::Polled { job_id, result } => self.remove_polled(&job_id, result),
            RemoveMessage::Preview {
                job_id,
                candidate,
                result,
            } => {
                let key = (job_id, candidate);
                self.remove_reads.reading.remove(&key);
                match result {
                    Ok(preview) => {
                        let preview =
                            Arc::try_unwrap(preview).unwrap_or_else(|shared| (*shared).clone());
                        self.remove_reads.previews.retain(|(held, _)| held != &key);
                        self.remove_reads
                            .previews
                            .push((key, image_handle(preview)));
                        let excess = self.remove_reads.previews.len().saturating_sub(HELD);
                        self.remove_reads.previews.drain(..excess);
                    }
                    Err(error) => {
                        self.event("remove_preview_failed", || json!({"job_id": key.0, "candidate": key.1, "error": error}));
                        self.remove_reads.failed.insert(key);
                    }
                }
            }
        }
        Task::none()
    }

    /// Generate, or Regenerate from the newest job's next seed: one fill request.
    fn remove_generate(&mut self, regenerate: bool) -> Task<Message> {
        if self.remove.pending > 0 || self.remove_reads.starting.is_some() {
            self.status.text = crate::state::IN_FLIGHT.into();
            return Task::none();
        }
        if let Some(reason) = self
            .gesture_refusal(Starting::Action)
            .or_else(|| self.generation_lock())
        {
            self.status.text = reason;
            return Task::none();
        }
        let (method, params, start) = match self.fill_request(regenerate) {
            Ok(request) => request,
            Err(reason) => {
                self.status.text = reason;
                return Task::none();
            }
        };
        if self
            .workspace
            .corrections
            .remove
            .as_ref()
            .is_some_and(|model| model.encoder.is_some())
        {
            self.status.text = "Replace needs the text encoder installed".into();
            return Task::none();
        }
        self.event(
            "remove_generate",
            || json!({"method": method, "params": params, "regenerate": regenerate}),
        );
        self.remove.pending += 1;
        self.remove.notice = None;
        self.remove_reads.starting = Some(start);
        self.remove_reads.last_request = Some((method.clone(), params.clone()));
        self.status.text = if regenerate {
            "Regenerating\u{2026}".into()
        } else {
            "Generating\u{2026}".into()
        };
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || start_fill(&owner, client, &method, params),
            |result| Message::Remove(RemoveMessage::Started(result)),
        )
    }

    /// The start answered: follow its job, or show why it was refused.
    fn remove_started(&mut self, result: Result<Value, CallError>) {
        self.remove.pending = self.remove.pending.saturating_sub(1);
        let Some(start) = self.remove_reads.starting.take() else {
            return;
        };
        match result {
            Ok(answer) => {
                let Some(job_id) = answer["job_id"].as_str().map(str::to_owned) else {
                    self.status.text = "The fill task named no job".into();
                    return;
                };
                self.event("remove_started", || json!({"job_id": job_id, "seed": start.seed}));
                self.remove.jobs.push(FillJob {
                    job_id,
                    asset: start.asset,
                    serial: start.serial,
                    prompt: start.prompt,
                    seed: start.seed,
                    status: JobStatus::Queued,
                    fraction: None,
                    message: None,
                    candidates: Vec::new(),
                    next_seed: None,
                    error: None,
                    cancelling: false,
                });
            }
            Err(error) => {
                self.event("remove_refused", || json!({"code": error.code, "message": error.message, "data": error.data}));
                let text = match error.data.as_ref().and_then(|data| data.get("requirements")) {
                    Some(_) if error.code == "not-ready" => {
                        format!("{} \u{b7} install what it needs first", error.message)
                    }
                    _ => error.message.clone(),
                };
                self.status.text = format!("{text} \u{b7} {}", error.code);
                self.remove.notice = Some(RemoveNotice {
                    text,
                    code: error.code,
                });
            }
        }
    }

    /// Cancel the generation running on the photograph on screen.
    fn remove_cancel(&mut self) -> Task<Message> {
        let (method, params) = match self.remove_cancel_request() {
            Ok(request) => request,
            Err(reason) => {
                self.status.text = reason;
                return Task::none();
            }
        };
        let job_id = params["job_id"].as_str().unwrap_or_default().to_owned();
        if let Some(job) = self.remove.job_mut(&job_id) {
            if job.cancelling {
                return Task::none();
            }
            job.cancelling = true;
        }
        self.event("remove_cancel", || json!({"job_id": job_id}));
        self.remove.pending += 1;
        self.remove_reads.last_request = Some((method.clone(), params.clone()));
        self.status.text = "Cancelling the generation\u{2026}".into();
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || tasks::call(&owner, client, &method, params).map(|_| ()),
            move |result| Message::Remove(RemoveMessage::Cancelled { job_id, result }),
        )
    }

    /// Accept the chosen candidate: one edit, through the same command path every edit takes.
    fn remove_accept(&mut self) -> Task<Message> {
        if let Some(reason) = self
            .gesture_refusal(Starting::Action)
            .or_else(|| self.generation_lock())
        {
            self.status.text = reason;
            return Task::none();
        }
        let (method, params, key, revision) = match self.accept_request() {
            Ok(request) => request,
            Err(reason) => {
                self.status.text = reason;
                return Task::none();
            }
        };
        self.event("remove_accept", || json!({"method": method, "params": params}));
        self.remove.accepting = Some((key.0, key.1, revision));
        self.remove_reads.last_request = Some((method.clone(), params.clone()));
        self.command(method, params)
    }

    /// One read of a fill job: its status, progress, candidates and result.
    fn remove_polled(&mut self, job_id: &str, result: Result<Box<JobRecord>, String>) {
        let Some(job) = self.remove.job_mut(job_id) else {
            return;
        };
        let record = match result {
            Ok(record) => record,
            Err(error) => {
                // A job the owner no longer knows has ended for the desktop too.
                if job.running() {
                    job.status = JobStatus::Failed;
                }
                job.error = Some(error);
                return;
            }
        };
        let first = job.candidates.is_empty();
        job.status = record.status;
        job.fraction = record.progress.fraction;
        job.message = record.progress.message.clone();
        if let Some(candidates) = record.candidates {
            job.candidates = candidates;
        }
        if let Some(seed) = record
            .result
            .as_ref()
            .and_then(|result| result["result"]["next_seed"].as_u64())
        {
            job.next_seed = Some(seed);
        }
        job.error = record.error.as_ref().map(|error| error.message.clone());
        let (status, error, serial, asset) = (
            job.status,
            job.error.clone(),
            job.serial,
            job.asset.clone(),
        );
        // A new job's first candidate is previewed as soon as it is published.
        let newest = self.remove.newest(&asset).map(|job| job.job_id.as_str()) == Some(job_id);
        let published = self
            .remove
            .job(job_id)
            .and_then(|job| job.candidates.first())
            .map(|row| row.candidate);
        let chosen_here = self
            .remove
            .candidate
            .as_ref()
            .is_some_and(|(job, _)| job == job_id);
        if first
            && newest
            && !chosen_here
            && let Some(candidate) = published
            && self.remove.selection_on(&asset).map(|s| s.serial) == Some(serial)
        {
            self.remove.candidate = Some((job_id.to_owned(), candidate));
        }
        match status {
            JobStatus::Ready => {
                let count = self.remove.job(job_id).map_or(0, |job| job.candidates.len());
                self.status.text = format!(
                    "{count} {} \u{b7} choose one and Accept",
                    if count == 1 { "candidate" } else { "candidates" }
                );
            }
            JobStatus::Failed => {
                let text = error.unwrap_or_else(|| "The generation failed".into());
                self.status.text = text.clone();
                self.remove.notice = Some(RemoveNotice {
                    text,
                    code: record
                        .error
                        .as_ref()
                        .map_or("internal".into(), |error| error.code.clone()),
                });
            }
            JobStatus::Cancelled | JobStatus::Superseded => {
                self.status.text = "Generation cancelled".into();
            }
            JobStatus::Queued | JobStatus::Running => {}
        }
    }

    /// A press in Remove: paint the selection. `⇧` draws a straight stroke from where the last one
    /// ended; `⌥` and `⌘` have no meaning here.
    pub(crate) fn remove_press(
        &mut self,
        view: ViewPoint,
        content: [f64; 2],
        modifiers: Modifiers,
    ) -> Task<Message> {
        if let Some(reason) = self.generation_lock() {
            self.status.text = reason;
            return Task::none();
        }
        if let Some(reason) = self.gesture_refusal(Starting::Correction) {
            self.status.text = reason;
            return Task::none();
        }
        let modifiers = Modifiers {
            shift: modifiers.shift,
            ..Modifiers::default()
        };
        let press = BrushGesture::press(view, content, modifiers, self.remove_reads.last_end);
        let points = self.remove_points(press.path());
        self.remove_reads.painting = Some((press, points));
        Task::none()
    }

    /// The stroke at the brush's radius as posted, or nothing when it cannot be.
    fn remove_points(&self, path: &crate::brush::StrokeCapture) -> Vec<[f64; 2]> {
        path.posted(self.remove_radius())
            .map(|points| points.to_vec())
            .unwrap_or_default()
    }

    /// The radius a selection is painted at: the band's Size.
    pub(crate) fn remove_radius(&self) -> f64 {
        self.corrections_binding()
            .map(|(_, binding)| self.corrections.settings_or(&binding).radius)
            .unwrap_or(crate::state::corrections::DEFAULT_RADIUS)
    }

    pub(crate) fn remove_drag(&mut self, view: ViewPoint, content: [f64; 2]) -> Task<Message> {
        let Some((press, _)) = self.remove_reads.painting.as_mut() else {
            return Task::none();
        };
        if press.moved(view, content) == Moved::Path {
            let path = press.path().clone();
            let points = self.remove_points(&path);
            if let Some((_, live)) = self.remove_reads.painting.as_mut() {
                *live = points;
            }
        }
        Task::none()
    }

    /// The stroke is finished: it is the selection, which starts a new strip.
    pub(crate) fn remove_release(&mut self, view: ViewPoint, content: [f64; 2]) -> Task<Message> {
        let Some((press, _)) = self.remove_reads.painting.take() else {
            return Task::none();
        };
        let Released::Add { path, .. } = press.release(view, content) else {
            return Task::none();
        };
        let Some(asset) = self.document.state.as_ref().map(|state| state.asset.id.clone()) else {
            return Task::none();
        };
        let radius = self.remove_radius();
        let points = match path.posted(radius) {
            Ok(points) => points.to_vec(),
            Err(error) => {
                self.status.text = error.to_string();
                return Task::none();
            }
        };
        if points.is_empty() {
            return Task::none();
        }
        self.remove_reads.last_end = path.last();
        self.remove.clear_draft();
        self.remove.serial += 1;
        let serial = self.remove.serial;
        self.event("remove_selection", || json!({"points": points.len(), "radius": radius}));
        self.remove.selection = Some(RemoveSelection {
            asset,
            points,
            radius,
            seed: draw_seed(serial),
            serial,
        });
        let action = if self.remove.prompt.trim().is_empty() {
            "Generate"
        } else {
            "Replace"
        };
        self.status.text = format!("Selection painted \u{b7} {action} fills it \u{b7} Esc clears it");
        Task::none()
    }

    /// Escape in Remove: a stroke being painted goes, then a running generation is cancelled, then
    /// the selection and strip are cleared. `None` when Remove has nothing to put down, so the
    /// workspace's own Escape order follows.
    pub(crate) fn remove_escape(&mut self) -> Option<Task<Message>> {
        if !self.remove_active() {
            return None;
        }
        if self.remove_reads.painting.take().is_some() {
            self.status.text = "Stroke cancelled".into();
            return Some(Task::none());
        }
        if self.generation_lock().is_some() {
            return Some(self.remove_cancel());
        }
        let asset = self.document.state.as_ref()?.asset.id.clone();
        let drafted = self.remove.selection_on(&asset).is_some()
            || self.remove.notice.is_some()
            || self.remove.strip_jobs(&asset).next().is_some();
        if drafted {
            self.remove.clear_draft();
            self.status.text = "Selection cleared".into();
            return Some(Task::none());
        }
        None
    }

    /// The status bar's sentence while Remove is chosen.
    pub(crate) fn remove_status(&self) -> Option<String> {
        if !self.remove_active() {
            return None;
        }
        self.workspace
            .corrections
            .remove
            .as_ref()
            .map(|model| model.hint.clone())
    }

    /// What the canvas draws of Remove, while it is chosen.
    pub(crate) fn remove_canvas(&self) -> Option<RemoveCanvas<'_>> {
        if !self.remove_active() {
            return None;
        }
        let asset = &self.document.state.as_ref()?.asset.id;
        let selection = self.remove.selection_on(asset);
        let preview = self.remove.chosen_on(asset).and_then(|(job, row)| {
            let handle = self.remove_images().image(&job.job_id, row.candidate)?;
            Some((handle, row.rect))
        });
        Some(RemoveCanvas {
            selection: selection.map(|selection| selection.points.as_slice()),
            live: self
                .remove_reads
                .painting
                .as_ref()
                .map(|(_, points)| points.as_slice()),
            radius: selection.map_or_else(|| self.remove_radius(), |selection| selection.radius),
            progress: self.remove.running_on(asset).map(|job| job.fraction),
            preview,
        })
    }

    /// The previews the band's strip and the canvas borrow.
    pub(crate) fn remove_images(&self) -> RemoveImages<'_> {
        RemoveImages {
            previews: &self.remove_reads.previews,
        }
    }

    /// Whether Remove has finished what the running evidence step started.
    pub(crate) fn remove_settled(&self) -> bool {
        if self.remove.pending > 0
            || self.remove_reads.starting.is_some()
            || self.remove.accepting.is_some()
            || !self.remove_reads.reading.is_empty()
        {
            return false;
        }
        let Some(asset) = self.document.state.as_ref().map(|state| &state.asset.id) else {
            return true;
        };
        match self.remove_reads.wait {
            None => true,
            Some(RemoveWait::Progress) => self.remove.running_on(asset).is_none_or(|job| {
                job.message
                    .as_deref()
                    .is_some_and(|message| message.contains("step"))
            }),
            Some(RemoveWait::Done) => {
                self.remove.running_on(asset).is_none()
                    && self.remove.strip(asset).iter().all(|(job, row)| {
                        row.state != CandidateState::Live || {
                            let key = (job.job_id.clone(), row.candidate);
                            self.remove_images().image(&key.0, key.1).is_some()
                                || self.remove_reads.failed.contains(&key)
                        }
                    })
            }
        }
    }

    /// What a captured frame records of Remove beyond its band model.
    pub(crate) fn remove_summary(&self) -> Value {
        json!({
            "active": self.remove_active(),
            "jobs": self.remove.jobs.iter().map(|job| json!({
                "job_id": job.job_id,
                "status": job.status,
                "seed": job.seed,
                "next_seed": job.next_seed,
                "prompt": job.prompt,
                "message": job.message,
                "fraction": job.fraction,
                "candidates": job.candidates.iter().map(|row| json!({"candidate": row.candidate, "state": row.state, "label": row.label, "rect": row.rect})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "selection": self.remove.selection.as_ref().map(|selection| json!({"points": selection.points.len(), "radius": selection.radius, "seed": selection.seed})),
            "candidate": self.remove.candidate,
            "previews": self.remove_reads.previews.iter().map(|(key, _)| json!(key)).collect::<Vec<_>>(),
            "painting": self.remove_reads.painting.is_some(),
            "lock": self.generation_lock(),
            "last_request": self.remove_reads.last_request.as_ref().map(|(method, params)| json!({"method": method, "params": redact_params(method, params)})),
        })
    }
}

/// After every message: in Remove, read each strip candidate's preview once, drop the previews
/// that left it, read the tiers on entering, and finish an accept once its command has answered.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let mut tasks = Vec::new();
    let active = editor.remove_active();
    if active && !editor.remove_reads.asked_tiers {
        editor.remove_reads.asked_tiers = true;
        tasks.push(editor.remove_refresh_tiers());
    } else if !active {
        editor.remove_reads.asked_tiers = false;
        editor.remove_reads.painting = None;
    }
    // An accept finishes with its command: a new revision is the committed entry, which clears the
    // selection and the strip; anything else left the strip as it was, the status bar saying why.
    if let Some((_, _, revision)) = editor.remove.accepting
        && !editor.busy
    {
        editor.remove.accepting = None;
        let committed = editor
            .document
            .state
            .as_ref()
            .is_some_and(|state| state.revision > revision);
        if committed {
            editor.remove.clear_draft();
            editor.status.text = "Accepted \u{b7} \u{2318}Z undoes it".into();
        }
    }
    let Some((asset, revision)) = editor
        .document
        .state
        .as_ref()
        .map(|state| (state.asset.id.clone(), state.revision))
    else {
        return Task::batch(tasks);
    };
    let strip: Vec<CandidateKey> = editor
        .remove
        .strip(&asset)
        .iter()
        .filter(|(_, row)| row.state == CandidateState::Live)
        .map(|(job, row)| (job.job_id.clone(), row.candidate))
        .collect();
    let chosen = editor.remove.candidate.clone();
    editor
        .remove_reads
        .previews
        .retain(|(key, _)| strip.contains(key) || chosen.as_ref() == Some(key));
    let (owner, client) = (editor.owner.clone(), editor.client);
    for key in strip {
        let held = editor.remove_images().image(&key.0, key.1).is_some();
        if held || editor.remove_reads.reading.contains(&key) || editor.remove_reads.failed.contains(&key) {
            continue;
        }
        editor.remove_reads.reading.insert(key.clone());
        let owner = owner.clone();
        let (job, candidate) = key;
        tasks.push(owner_task(
            {
                let job = job.clone();
                move || read_preview(&owner, client, &job, candidate).map(Arc::new)
            },
            move |result| {
                Message::Remove(RemoveMessage::Preview {
                    job_id: job,
                    candidate,
                    result,
                })
            },
        ));
    }
    // Another client's accept (or an undo) changes a candidate's state: read the strip's finished
    // jobs again when the photograph's revision moves.
    if editor.remove_reads.read_at != Some(revision) {
        editor.remove_reads.read_at = Some(revision);
        let finished: Vec<String> = editor
            .remove
            .strip_jobs(&asset)
            .filter(|job| !job.running())
            .map(|job| job.job_id.clone())
            .collect();
        for job_id in finished {
            let owner = owner.clone();
            tasks.push(owner_task(
                {
                    let job_id = job_id.clone();
                    move || {
                        tasks::call(&owner, client, JOB_READ, json!({"job_id": job_id}))
                            .and_then(|(value, _)| {
                                serde_json::from_value::<JobRecord>(value)
                                    .map(Box::new)
                                    .map_err(|error| error.to_string())
                            })
                    }
                },
                move |result| Message::Remove(RemoveMessage::Polled { job_id, result }),
            ));
        }
    }
    Task::batch(tasks)
}

/// One reader per fill job of this desktop's still queued or running.
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    let running: Vec<String> = editor
        .remove
        .jobs
        .iter()
        .filter(|job| job.running())
        .map(|job| job.job_id.clone())
        .collect();
    if running.is_empty() {
        return Subscription::none();
    }
    Subscription::batch(running.into_iter().map(|identity| {
        Subscription::run_with(
            Reader {
                identity,
                owner: editor.owner.clone(),
                client: editor.client,
                presentation_visible: editor.visibility.sampling_allowed(),
            },
            fill_reads,
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::decode_base64;

    #[test]
    fn base64_decodes_the_standard_alphabet_with_padding() {
        assert_eq!(decode_base64("").unwrap(), b"");
        assert_eq!(decode_base64("Zg==").unwrap(), b"f");
        assert_eq!(decode_base64("Zm8=").unwrap(), b"fo");
        assert_eq!(decode_base64("Zm9v").unwrap(), b"foo");
        assert_eq!(decode_base64("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(decode_base64("+/8=").unwrap(), [0xfb, 0xff]);
        assert!(decode_base64("Zm9").is_err());
        assert!(decode_base64("Zg==Zm9v").is_err());
        assert!(decode_base64("Z!==").is_err());
    }
}
