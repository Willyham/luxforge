//! Disposable job results: the bounded host store of the candidate patches a task publishes, which
//! a client reads in chunks and either accepts as a durable artifact or releases. A candidate is
//! never an artifact and never JSON pixels: its bytes live under the artifact root's
//! `tmp/candidates/<job_id>/` until it is accepted or released, and only its description reaches
//! `job.read`. See `docs/design/module-capabilities.md#job-candidates` and the frozen contract in
//! `docs/design/corrections.md#candidates`.
//!
//! - **Admission before allocation.** A task reserves a candidate's shape first
//!   ([`Candidates::reserve`]): the job's count ([`MAX_JOB_CANDIDATES`]), the candidate's bytes
//!   ([`MAX_CANDIDATE_BYTES`], patch plus preview) and the bytes held across jobs
//!   ([`MAX_HELD_CANDIDATE_BYTES`]) are charged before the host converts or writes anything. Past
//!   the global bound the oldest finished job's live candidates are released first (`budget`);
//!   when even that cannot make room the reservation is refused `resource-limit`.
//! - **Ownership.** A candidate belongs to its task job, which belongs to the catalog: any client
//!   reads, releases or accepts it, and a client disconnecting releases nothing. Cancelling the job
//!   releases its candidates (`cancelled`), an explicit release releases them (`released`), and a
//!   job the job table forgets takes its candidates with it. Closing the catalog deletes every
//!   file, and opening it sweeps whatever a crash left ([`Candidates::sweep`]).
//! - **Pins.** A candidate being accepted is pinned ([`Candidates::pin`]): no budget release takes
//!   it, an explicit release of it is a `conflict`, and a cancel waits for the accept to settle; a
//!   read in flight ([`Candidates::reader`]) only keeps its files until the read is done.
//! - **Off the owner.** Converting, writing, reading a chunk and publishing an accepted patch run on
//!   a worker; the catalog owner only changes the table and removes released files.
use super::{ArtifactMeta, ArtifactRecord, ArtifactWriter, PreparedArtifact, TEMPORARY, lock};
use crate::{AssetId, Error, JobId, atomic_file::file_error, jobs::JobControl};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// The most candidates one job publishes; their numbers are `0..=3` and are never reused.
pub(crate) const MAX_JOB_CANDIDATES: usize = 4;
/// The most bytes one candidate holds, its patch and its preview together: 64 MiB.
pub(crate) const MAX_CANDIDATE_BYTES: u64 = 64 * 1024 * 1024;
/// The most bytes the live candidates of every job hold together: 1 GiB.
pub(crate) const MAX_HELD_CANDIDATE_BYTES: u64 = 1024 * 1024 * 1024;
/// The longest chunk `job.read-candidate` answers: 768 KiB, about 1 MiB of base64.
pub(crate) const MAX_CANDIDATE_CHUNK: u64 = 768 * 1024;
/// The longest side of a candidate's preview.
pub(crate) const MAX_PREVIEW_SIDE: u32 = 1024;
/// The longest label a candidate carries, in characters.
pub(crate) const MAX_CANDIDATE_LABEL: usize = 32;
/// The most bytes a candidate's provenance takes as compact JSON, the host's fields included.
pub(crate) const MAX_PROVENANCE_BYTES: usize = 2048;
/// The artifact kind an accepted patch is published as.
// The accept seam: the repair layer's accept action calls it; until it lands only tests do.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const PATCH_KIND: &str = "repair-patch";
/// The colour interpretation of an accepted patch's RGB, as its artifact records it.
// The accept seam: the repair layer's accept action calls it; until it lands only tests do.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const PATCH_COLOUR: &str = "linear-srgb";
/// A patch's stored form: `width × height` RGBA IEEE half floats, little-endian, RGB linear sRGB
/// (D65), the repair stage's representation, and A the coverage `0..=1`, not premultiplied.
pub const PATCH_FORMAT: &str = "rgba16f-le";
/// A preview's stored form: `width × height` RGB, 8 bits a channel, sRGB-encoded.
pub const PREVIEW_FORMAT: &str = "rgb8-srgb";
/// Under the artifact root's `tmp/`.
const CANDIDATES: &str = "candidates";
/// The largest finite half float.
const HALF_MAX: f32 = 65504.0;
/// The read size for hashing an accepted candidate's patch.
// The accept seam: the repair layer's accept action calls it; until it lands only tests do.
#[cfg_attr(not(test), allow(dead_code))]
const CHUNK: usize = 64 * 1024;

/// Where a candidate stands, as `job.read` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateState {
    /// Held: its bytes can be read and accepted.
    Live,
    /// Published as an artifact by an accept that committed; its own files are gone.
    Accepted,
    /// Gone, for [`ReleaseReason`].
    Released,
}

/// Why a candidate was released.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReleaseReason {
    /// Past the bytes held across jobs, the oldest finished job's live candidates go first.
    Budget,
    /// A client released it (`job.release-candidates`).
    Released,
    /// Its job was cancelled.
    Cancelled,
    /// The catalog closed.
    Closed,
}

impl ReleaseReason {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::Released => "released",
            Self::Cancelled => "cancelled",
            Self::Closed => "closed",
        }
    }
}

/// Which bytes of a candidate a chunk read reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidatePart {
    /// The sRGB 8-bit preview, [`PREVIEW_FORMAT`].
    Preview,
    /// The stored patch, [`PATCH_FORMAT`].
    Patch,
}

impl CandidatePart {
    fn extension(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Patch => "patch",
        }
    }

    /// The stored form a chunk of this part is in.
    pub fn format(self) -> &'static str {
        match self {
            Self::Preview => PREVIEW_FORMAT,
            Self::Patch => PATCH_FORMAT,
        }
    }
}

/// A preview's dimensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewSize {
    pub width: u32,
    pub height: u32,
}

/// One candidate as `job.read` lists it under its task job: its number, state and, once released,
/// why; its patch's dimensions, the bytes it holds (patch and preview), the rectangle of content
/// pixels it covers, its preview's dimensions, the asset and revision it was generated against,
/// its producer's label and its provenance (`module`, `task` and the producer's own fields).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateRead {
    pub candidate: u8,
    pub state: CandidateState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<ReleaseReason>,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    /// `[x, y, width, height]` in content pixels.
    pub rect: [u32; 4],
    pub preview: PreviewSize,
    pub asset_id: AssetId,
    pub revision: u64,
    pub label: String,
    pub provenance: Map<String, Value>,
    /// The input identity a new top correction of the asset had when the task was queued, which
    /// accepting the candidate as a frozen patch checks ([`crate::RepairInput`]); absent when the
    /// stack could not say, and such a candidate cannot be accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<crate::RepairInput>,
}

/// What a task's candidates were generated against, which the owner records when it queues the
/// task: the asset, its revision, its content dimensions, which bound every candidate's rectangle,
/// and the input identity a new top correction would have ([`crate::RepairInput`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CandidateBasis {
    pub asset_id: AssetId,
    pub revision: u64,
    pub width: u32,
    pub height: u32,
    pub input: Option<crate::RepairInput>,
}

/// The dimensions a candidate is reserved at: its patch's and its preview's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CandidateShape {
    pub width: u32,
    pub height: u32,
    pub preview_width: u32,
    pub preview_height: u32,
}

impl CandidateShape {
    fn patch_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * 8
    }

    fn preview_bytes(&self) -> u64 {
        u64::from(self.preview_width) * u64::from(self.preview_height) * 3
    }

    /// The bytes the shape holds stored, when it is well formed and within `limit`.
    fn check(&self, limit: u64) -> Result<u64, Error> {
        if self.width == 0 || self.height == 0 {
            return Err(Error::validation("a candidate patch is at least one pixel"));
        }
        if self.preview_width == 0
            || self.preview_height == 0
            || self.preview_width.max(self.preview_height) > MAX_PREVIEW_SIDE
        {
            return Err(Error::validation(format!(
                "a candidate preview is 1 to {MAX_PREVIEW_SIDE} pixels on its longest side"
            )));
        }
        let bytes = self.patch_bytes() + self.preview_bytes();
        if bytes > limit {
            return Err(Error::resource_limit(format!(
                "a {}×{} candidate with a {}×{} preview holds {bytes} bytes, more than {limit}",
                self.width, self.height, self.preview_width, self.preview_height
            )));
        }
        Ok(bytes)
    }
}

/// A patch's colour as its producer hands it over, RGB interleaved, one triple a pixel.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PatchColour<'a> {
    /// Linear sRGB (D65) light, the repair stage's representation: finite and within the half
    /// float range, unbounded otherwise.
    LinearSrgb(&'a [f32]),
    /// 8-bit sRGB-encoded codes, decoded to linear light by the host.
    Srgb8(&'a [u8]),
}

/// One candidate as a task publishes it ([`crate::ModuleContext::publish_candidate`]).
#[derive(Clone, Debug)]
pub(crate) struct CandidatePatch<'a> {
    /// `[x, y, width, height]` in the content pixels of the asset the task was generated against.
    pub rect: [u32; 4],
    pub colour: PatchColour<'a>,
    /// One coverage a pixel, `0..=1`.
    pub coverage: &'a [f32],
    /// RGB 8-bit sRGB, `preview_width × preview_height` pixels.
    pub preview: &'a [u8],
    pub preview_width: u32,
    pub preview_height: u32,
    /// The display noun, 1 to 32 characters (`Remove`, the proof's `Patch`).
    pub label: &'a str,
    /// String and number fields the producer declares (model, version, seed); `module` and `task`
    /// are the host's.
    pub provenance: Map<String, Value>,
}

impl CandidatePatch<'_> {
    /// The shape this patch is reserved at.
    pub(crate) fn shape(&self) -> CandidateShape {
        CandidateShape {
            width: self.rect[2],
            height: self.rect[3],
            preview_width: self.preview_width,
            preview_height: self.preview_height,
        }
    }
}

/// The store's bounds; production uses the contract's ([`CandidateLimits::default`]) and a test
/// gives it kilobytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CandidateLimits {
    pub per_job: usize,
    pub candidate_bytes: u64,
    pub held_bytes: u64,
}

impl Default for CandidateLimits {
    fn default() -> Self {
        Self {
            per_job: MAX_JOB_CANDIDATES,
            candidate_bytes: MAX_CANDIDATE_BYTES,
            held_bytes: MAX_HELD_CANDIDATE_BYTES,
        }
    }
}

/// What the owner hands a task that may publish candidates: the store, the job and what it was
/// generated against, and the job's control, whose cancel stops publication and whose change token
/// `job.wait` follows.
pub(crate) struct CandidateSink {
    pub store: Arc<Candidates>,
    pub job_id: JobId,
    pub module_id: String,
    pub task_id: String,
    pub basis: CandidateBasis,
    pub control: Arc<JobControl>,
}

impl std::fmt::Debug for CandidateSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandidateSink")
            .field("job_id", &self.job_id)
            .field("basis", &self.basis)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    /// Admitted and being written; not a candidate yet, and listed nowhere.
    Reserved,
    Live,
    Accepted,
    Released(ReleaseReason),
}

/// What a published candidate says about itself.
#[derive(Clone, Debug)]
struct Published {
    rect: [u32; 4],
    label: String,
    provenance: Map<String, Value>,
    /// Of the stored patch, checked again when it is accepted.
    sha256: String,
}

#[derive(Debug)]
struct Slot {
    state: SlotState,
    shape: CandidateShape,
    bytes: u64,
    published: Option<Published>,
    /// Chunk reads in flight, which keep the files until they finish.
    readers: u32,
    /// Accepts in flight, which keep the candidate live.
    accepting: u32,
    /// A cancel or forget that waits for the accepts in flight to settle.
    pending: Option<ReleaseReason>,
    /// Whether its files may still be on disk.
    files: bool,
}

impl Slot {
    fn pinned(&self) -> bool {
        self.readers > 0 || self.accepting > 0
    }
}

/// One task job's candidates.
#[derive(Debug)]
struct Held {
    module_id: String,
    task_id: String,
    basis: CandidateBasis,
    control: Arc<JobControl>,
    /// When the job finished, in the store's own order: the budget releases the oldest first.
    finished: Option<u64>,
    /// No more candidates: the job finished, was cancelled or was forgotten.
    closed: bool,
    /// The job table forgot the job; the entry goes once nothing reads or accepts it.
    forgotten: bool,
    slots: Vec<Option<Slot>>,
}

impl Held {
    fn row(&self, index: usize, slot: &Slot) -> Option<CandidateRead> {
        let published = slot.published.as_ref()?;
        let (state, released) = match slot.state {
            SlotState::Reserved => return None,
            SlotState::Live => (CandidateState::Live, None),
            SlotState::Accepted => (CandidateState::Accepted, None),
            SlotState::Released(reason) => (CandidateState::Released, Some(reason)),
        };
        Some(CandidateRead {
            candidate: index as u8,
            state,
            released,
            width: slot.shape.width,
            height: slot.shape.height,
            bytes: slot.bytes,
            rect: published.rect,
            preview: PreviewSize {
                width: slot.shape.preview_width,
                height: slot.shape.preview_height,
            },
            asset_id: self.basis.asset_id.clone(),
            revision: self.basis.revision,
            label: published.label.clone(),
            provenance: published.provenance.clone(),
            input: self.basis.input.clone(),
        })
    }
}

#[derive(Debug, Default)]
struct Inner {
    jobs: HashMap<JobId, Held>,
    /// The bytes of every reserved and live candidate.
    held: u64,
    tick: u64,
}

/// What a change to the table leaves to do once the lock is released: files and directories to
/// remove, and the jobs whose `job.read` answer changed.
#[derive(Default)]
struct Aftermath {
    files: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    changed: Vec<Arc<JobControl>>,
}

impl Inner {
    /// Release a live candidate now, or, while an accept holds it, once the accept settles.
    fn release(
        &mut self,
        job_id: &JobId,
        index: usize,
        reason: ReleaseReason,
        after: &mut Aftermath,
    ) -> bool {
        let Some(held) = self.jobs.get_mut(job_id) else {
            return false;
        };
        let Some(slot) = held.slots.get_mut(index).and_then(Option::as_mut) else {
            return false;
        };
        if slot.state != SlotState::Live {
            return false;
        }
        if slot.accepting > 0 {
            slot.pending.get_or_insert(reason);
            return false;
        }
        slot.state = SlotState::Released(reason);
        slot.pending = None;
        self.held -= slot.bytes;
        after.changed.push(held.control.clone());
        true
    }

    /// Release every live candidate of a job, as a cancel or a forget does, and close it to more.
    fn release_all(&mut self, job_id: &JobId, reason: ReleaseReason, after: &mut Aftermath) {
        let Some(held) = self.jobs.get_mut(job_id) else {
            return;
        };
        held.closed = true;
        for index in 0..held.slots.len() {
            self.release(job_id, index, reason, after);
        }
    }

    /// Collect the files of candidates that are no longer live and that nothing reads or accepts,
    /// and the entries of forgotten jobs nothing holds any more.
    fn settle(&mut self, directory: &Path, after: &mut Aftermath) {
        let mut gone = Vec::new();
        for (job_id, held) in &mut self.jobs {
            let job_directory = directory.join(job_id.as_str());
            for (index, slot) in held.slots.iter_mut().enumerate() {
                let Some(slot) = slot else { continue };
                let done = matches!(slot.state, SlotState::Accepted | SlotState::Released(_));
                if done && slot.files && !slot.pinned() {
                    slot.files = false;
                    for part in [CandidatePart::Patch, CandidatePart::Preview] {
                        after.files.push(part_path(&job_directory, index, part));
                    }
                }
            }
            let holding = held.slots.iter().flatten().any(|slot| {
                slot.pinned() || slot.state == SlotState::Reserved || slot.state == SlotState::Live
            });
            if held.forgotten && !holding {
                gone.push(job_id.clone());
                after.directories.push(job_directory);
            }
        }
        for job_id in gone {
            self.jobs.remove(&job_id);
        }
    }
}

fn part_path(job_directory: &Path, index: usize, part: CandidatePart) -> PathBuf {
    job_directory.join(format!("{index}.{}", part.extension()))
}

fn unknown(job_id: &JobId, candidate: usize) -> Error {
    Error::validation(format!("job {job_id} holds no candidate {candidate}"))
}

fn not_live(job_id: &JobId, candidate: usize, state: SlotState) -> Error {
    let (name, reason) = match state {
        SlotState::Accepted => ("accepted", None),
        SlotState::Released(reason) => ("released", Some(reason)),
        SlotState::Reserved | SlotState::Live => ("live", None),
    };
    let detail = match reason {
        Some(reason) => format!(
            "candidate {candidate} of job {job_id} was released ({})",
            reason.name()
        ),
        None => format!("candidate {candidate} of job {job_id} is {name}"),
    };
    Error::conflict(detail).with_data(json!({"state": name, "released": reason}))
}

/// The bounded candidate store of one open catalog. Shared by the catalog owner, which reads and
/// releases, and the workers that publish, read chunks and accept; every change is a short critical
/// section over the table, and every file is written, read or hashed outside it.
pub(crate) struct Candidates {
    writer: ArtifactWriter,
    limits: CandidateLimits,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Candidates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = lock(&self.inner);
        f.debug_struct("Candidates")
            .field("jobs", &inner.jobs.len())
            .field("held", &inner.held)
            .field("limits", &self.limits)
            .finish()
    }
}

impl Candidates {
    /// The store of the catalog `writer` publishes into. Nothing is created until a candidate is
    /// reserved.
    pub(crate) fn new(writer: ArtifactWriter) -> Arc<Self> {
        Self::with_limits(writer, CandidateLimits::default())
    }

    pub(crate) fn with_limits(writer: ArtifactWriter, limits: CandidateLimits) -> Arc<Self> {
        Arc::new(Self {
            writer,
            limits,
            inner: Mutex::new(Inner::default()),
        })
    }

    /// `<artifact root>/tmp/candidates`.
    pub(crate) fn directory(&self) -> PathBuf {
        self.writer.root().join(TEMPORARY).join(CANDIDATES)
    }

    fn job_directory(&self, job_id: &JobId) -> PathBuf {
        self.directory().join(job_id.as_str())
    }

    /// Remove what a change left, and tell the jobs it changed.
    fn finish(&self, after: Aftermath) {
        for path in after.files {
            let _ = fs::remove_file(path);
        }
        for path in after.directories {
            let _ = fs::remove_dir_all(path);
        }
        for control in after.changed {
            control.changed();
        }
    }

    /// Change the table under its lock, settle it, then remove what the change left.
    fn change<T>(&self, change: impl FnOnce(&mut Inner, &mut Aftermath) -> T) -> T {
        let mut after = Aftermath::default();
        let value = {
            let mut inner = lock(&self.inner);
            let value = change(&mut inner, &mut after);
            inner.settle(&self.directory(), &mut after);
            value
        };
        self.finish(after);
        value
    }

    /// The bytes every reserved and live candidate holds.
    #[cfg(test)]
    pub(crate) fn held_bytes(&self) -> u64 {
        lock(&self.inner).held
    }

    // The worker's side: admission and publication.

    /// Admit one candidate of `shape` for the sink's job before anything is converted or written:
    /// the job is still running and holds fewer than four, the candidate fits 64 MiB, and the bytes
    /// held across jobs fit 1 GiB once the oldest finished jobs' live candidates are released
    /// (`budget`). A candidate an accept holds is never released for room. Refused room is
    /// `resource-limit`, a cancelled job `cancelled`, and nothing is released when the room cannot
    /// be made. Worker-side: the first reservation creates the job's directory.
    pub(crate) fn reserve(
        self: &Arc<Self>,
        sink: &CandidateSink,
        shape: CandidateShape,
    ) -> Result<Reservation, Error> {
        sink.control.checkpoint()?;
        let bytes = shape.check(self.limits.candidate_bytes)?;
        self.writer.prepare_root()?;
        let limits = self.limits;
        let index = self.change(|inner, after| {
            sink.control.checkpoint()?;
            let held = inner
                .jobs
                .entry(sink.job_id.clone())
                .or_insert_with(|| Held {
                    module_id: sink.module_id.clone(),
                    task_id: sink.task_id.clone(),
                    basis: sink.basis.clone(),
                    control: sink.control.clone(),
                    finished: None,
                    closed: false,
                    forgotten: false,
                    slots: Vec::new(),
                });
            if held.closed {
                return Err(sink.control.cancelled_error());
            }
            let index = match held.slots.iter().position(Option::is_none) {
                Some(index) => index,
                None if held.slots.len() < limits.per_job => {
                    held.slots.push(None);
                    held.slots.len() - 1
                }
                None => {
                    return Err(Error::resource_limit(format!(
                        "a job publishes at most {} candidates",
                        limits.per_job
                    )));
                }
            };
            if inner.held + bytes > limits.held_bytes {
                let need = inner.held + bytes - limits.held_bytes;
                // The oldest finished jobs' live candidates nothing accepts, oldest first.
                let mut victims: Vec<(u64, JobId, usize, u64)> = inner
                    .jobs
                    .iter()
                    .filter_map(|(job_id, held)| Some((held.finished?, job_id, held)))
                    .flat_map(|(finished, job_id, held)| {
                        held.slots
                            .iter()
                            .enumerate()
                            .filter_map(move |(index, slot)| {
                                let slot = slot.as_ref()?;
                                (slot.state == SlotState::Live && slot.accepting == 0)
                                    .then(|| (finished, job_id.clone(), index, slot.bytes))
                            })
                    })
                    .collect();
                victims.sort();
                let available: u64 = victims.iter().map(|(.., bytes)| bytes).sum();
                if available < need {
                    return Err(Error::resource_limit(format!(
                        "candidates hold {} of {} bytes and {bytes} more do not fit",
                        inner.held, limits.held_bytes
                    )));
                }
                let mut freed = 0;
                for (_, job_id, index, bytes) in victims {
                    if freed >= need {
                        break;
                    }
                    if inner.release(&job_id, index, ReleaseReason::Budget, after) {
                        freed += bytes;
                    }
                }
            }
            let held = inner
                .jobs
                .get_mut(&sink.job_id)
                .expect("the job was just admitted");
            held.slots[index] = Some(Slot {
                state: SlotState::Reserved,
                shape,
                bytes,
                published: None,
                readers: 0,
                accepting: 0,
                pending: None,
                files: false,
            });
            inner.held += bytes;
            Ok(index)
        })?;
        let reservation = Reservation {
            store: self.clone(),
            job_id: sink.job_id.clone(),
            index,
            shape,
            basis: sink.basis.clone(),
            module_id: sink.module_id.clone(),
            task_id: sink.task_id.clone(),
            control: sink.control.clone(),
            written: false,
            done: false,
        };
        fs::create_dir_all(self.job_directory(&sink.job_id))
            .map_err(|error| file_error("cannot create candidate directory", error.kind()))?;
        Ok(reservation)
    }

    // The owner's side: reads, releases and the job's lifecycle.

    /// The candidates `job.read` lists for a task job, in number order: none for a job that
    /// published none.
    pub(crate) fn read_job(&self, job_id: &JobId) -> Vec<CandidateRead> {
        let inner = lock(&self.inner);
        let Some(held) = inner.jobs.get(job_id) else {
            return Vec::new();
        };
        held.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| held.row(index, slot.as_ref()?))
            .collect()
    }

    /// `Ok` while the candidate is live; its `conflict` naming why once it is not.
    pub(crate) fn live(&self, job_id: &JobId, candidate: u8) -> Result<(), Error> {
        let index = usize::from(candidate);
        let inner = lock(&self.inner);
        let slot = inner
            .jobs
            .get(job_id)
            .and_then(|held| held.slots.get(index)?.as_ref())
            .filter(|slot| slot.state != SlotState::Reserved)
            .ok_or_else(|| unknown(job_id, index))?;
        match slot.state {
            SlotState::Live => Ok(()),
            state => Err(not_live(job_id, index, state)),
        }
    }

    /// `job.release-candidates`: release the named candidates, or every one the job holds. A
    /// candidate already released or accepted stays as it is; one an accept holds refuses the whole
    /// request with `conflict`, and a number the job never published with `validation`, releasing
    /// nothing. Returns the numbers released now.
    pub(crate) fn release(
        &self,
        job_id: &JobId,
        candidates: Option<&[u8]>,
    ) -> Result<Vec<u8>, Error> {
        self.change(|inner, after| {
            let Some(held) = inner.jobs.get(job_id) else {
                return match candidates.and_then(|named| named.first()) {
                    Some(first) => Err(unknown(job_id, usize::from(*first))),
                    None => Ok(Vec::new()),
                };
            };
            let named: Vec<usize> = match candidates {
                Some(named) => named.iter().map(|n| usize::from(*n)).collect(),
                None => (0..held.slots.len()).collect(),
            };
            for &index in &named {
                let slot = held
                    .slots
                    .get(index)
                    .and_then(Option::as_ref)
                    .filter(|slot| slot.state != SlotState::Reserved);
                match slot {
                    None if candidates.is_some() => return Err(unknown(job_id, index)),
                    Some(slot) if slot.state == SlotState::Live && slot.accepting > 0 => {
                        return Err(Error::conflict(format!(
                            "candidate {index} of job {job_id} is being accepted"
                        )));
                    }
                    _ => {}
                }
            }
            let mut released = Vec::new();
            for index in named {
                if inner.release(job_id, index, ReleaseReason::Released, after) {
                    released.push(index as u8);
                }
            }
            released.sort_unstable();
            released.dedup();
            Ok(released)
        })
    }

    /// The job was cancelled: it publishes nothing more and its live candidates are released
    /// (`cancelled`), a candidate an accept holds once that accept settles without committing. A
    /// finished job's cancel releases them too.
    pub(crate) fn cancel(&self, job_id: &JobId) {
        self.change(|inner, after| inner.release_all(job_id, ReleaseReason::Cancelled, after));
    }

    /// The job finished: it publishes nothing more, and its candidates stay live until released.
    /// A cancelled job's are released ([`Self::cancel`]); a failed job keeps what it published.
    pub(crate) fn finished(&self, job_id: &JobId, cancelled: bool) {
        self.change(|inner, after| {
            inner.tick += 1;
            let tick = inner.tick;
            if let Some(held) = inner.jobs.get_mut(job_id) {
                held.closed = true;
                held.finished.get_or_insert(tick);
            }
            if cancelled {
                inner.release_all(job_id, ReleaseReason::Cancelled, after);
            }
        });
    }

    /// Forget every job `keep` refuses, as the job table forgets its record: its candidates go
    /// with it, an accepted or read one once its accept or read settles.
    pub(crate) fn retain(&self, keep: impl Fn(&JobId) -> bool) {
        self.change(|inner, after| {
            let forgotten: Vec<JobId> = inner
                .jobs
                .iter()
                .filter(|(job_id, held)| !held.forgotten && !keep(job_id))
                .map(|(job_id, _)| job_id.clone())
                .collect();
            for job_id in forgotten {
                inner.release_all(&job_id, ReleaseReason::Budget, after);
                if let Some(held) = inner.jobs.get_mut(&job_id) {
                    held.forgotten = true;
                }
            }
        });
    }

    /// Pin a live candidate for an accept: no budget release takes it, an explicit release of it is
    /// a `conflict` and a cancel waits until the accept settles. The pin is the accept's: it
    /// publishes the stored bytes ([`CandidatePin::publish`]) and is settled by
    /// [`CandidatePin::accepted`] once the commit holds, or by dropping it when the accept fails.
    pub(crate) fn pin(
        self: &Arc<Self>,
        job_id: &JobId,
        candidate: u8,
    ) -> Result<CandidatePin, Error> {
        self.pin_as(job_id, candidate, PinKind::Accept)
    }

    /// Keep a live candidate's files for one chunk read, which a release does not wait for.
    pub(crate) fn reader(
        self: &Arc<Self>,
        job_id: &JobId,
        candidate: u8,
    ) -> Result<CandidatePin, Error> {
        self.pin_as(job_id, candidate, PinKind::Read)
    }

    fn pin_as(
        self: &Arc<Self>,
        job_id: &JobId,
        candidate: u8,
        kind: PinKind,
    ) -> Result<CandidatePin, Error> {
        let index = usize::from(candidate);
        let mut inner = lock(&self.inner);
        let held = inner
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| unknown(job_id, index))?;
        let (module_id, task_id) = (held.module_id.clone(), held.task_id.clone());
        let slot = held
            .slots
            .get(index)
            .and_then(Option::as_ref)
            .filter(|slot| slot.state != SlotState::Reserved)
            .ok_or_else(|| unknown(job_id, index))?;
        if slot.state != SlotState::Live {
            return Err(not_live(job_id, index, slot.state));
        }
        let read = held
            .row(index, slot)
            .expect("a live candidate is published");
        let sha256 = slot
            .published
            .as_ref()
            .map(|published| published.sha256.clone())
            .unwrap_or_default();
        let (patch_bytes, preview_bytes) = (slot.shape.patch_bytes(), slot.shape.preview_bytes());
        let slot = held.slots[index].as_mut().expect("the slot was just read");
        match kind {
            PinKind::Read => slot.readers += 1,
            PinKind::Accept => slot.accepting += 1,
        }
        drop(inner);
        Ok(CandidatePin {
            store: self.clone(),
            job_id: job_id.clone(),
            index,
            kind,
            directory: self.job_directory(job_id),
            read,
            module_id,
            task_id,
            sha256,
            patch_bytes,
            preview_bytes,
            settled: false,
        })
    }

    /// Settle one pin: an accept that committed makes the candidate `accepted`; otherwise a release
    /// that waited for the last accept happens now.
    fn unpin(&self, job_id: &JobId, index: usize, kind: PinKind, accepted: bool) {
        self.change(|inner, after| {
            let Some(held) = inner.jobs.get_mut(job_id) else {
                return;
            };
            let control = held.control.clone();
            let Some(slot) = held.slots.get_mut(index).and_then(Option::as_mut) else {
                return;
            };
            match kind {
                PinKind::Read => slot.readers = slot.readers.saturating_sub(1),
                PinKind::Accept => slot.accepting = slot.accepting.saturating_sub(1),
            }
            let mut freed = None;
            let mut pending = None;
            if accepted && slot.state == SlotState::Live {
                slot.state = SlotState::Accepted;
                slot.pending = None;
                freed = Some(slot.bytes);
            } else if slot.accepting == 0 {
                pending = slot.pending;
            }
            if let Some(bytes) = freed {
                inner.held -= bytes;
                after.changed.push(control);
            }
            if let Some(reason) = pending {
                inner.release(job_id, index, reason, after);
            }
        });
    }

    /// Remove every job directory under `tmp/candidates/` the store does not hold: what a process
    /// that ended or crashed left, swept when the catalog opens and on `artifact.collect`. Returns
    /// how many entries it removed. Worker-side.
    pub(crate) fn sweep(&self, cancel: &AtomicBool) -> Result<usize, Error> {
        let directory = self.directory();
        let entries = match fs::read_dir(&directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(file_error("cannot read candidate directory", error.kind())),
            Ok(entries) => entries,
        };
        let listed: Vec<fs::DirEntry> = entries
            .collect::<Result<_, _>>()
            .map_err(|error| file_error("cannot read candidate directory", error.kind()))?;
        // A directory is created after its job is in the table, so one listed before this snapshot
        // whose job is not in it belongs to no job of this process.
        let held: Vec<String> = lock(&self.inner)
            .jobs
            .keys()
            .map(|job_id| job_id.as_str().to_owned())
            .collect();
        let mut removed = 0;
        for entry in listed {
            if held.iter().any(|name| entry.file_name() == name.as_str()) {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::cancelled("artifact job cancelled"));
            }
            let path = entry.path();
            let result = match entry.file_type() {
                Ok(kind) if kind.is_dir() => fs::remove_dir_all(&path),
                _ => fs::remove_file(&path),
            };
            match result {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(file_error("cannot remove stale candidates", error.kind()));
                }
            }
        }
        Ok(removed)
    }
}

/// Closing the catalog releases every candidate (`closed`) and removes its files; nothing pins one
/// any more, because every pin and reservation holds the store.
impl Drop for Candidates {
    fn drop(&mut self) {
        let directory = self.directory();
        let inner = self
            .inner
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        for (job_id, held) in &mut inner.jobs {
            for slot in held.slots.iter_mut().flatten() {
                if slot.state == SlotState::Live {
                    slot.state = SlotState::Released(ReleaseReason::Closed);
                }
            }
            let _ = fs::remove_dir_all(directory.join(job_id.as_str()));
        }
        inner.held = 0;
    }
}

/// One admitted candidate, before it is published. Dropping it unpublished returns its bytes to
/// the budget and removes whatever it wrote.
pub(crate) struct Reservation {
    store: Arc<Candidates>,
    job_id: JobId,
    index: usize,
    shape: CandidateShape,
    basis: CandidateBasis,
    module_id: String,
    task_id: String,
    control: Arc<JobControl>,
    written: bool,
    done: bool,
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reservation")
            .field("job_id", &self.job_id)
            .field("candidate", &self.index)
            .field("shape", &self.shape)
            .finish_non_exhaustive()
    }
}

impl Reservation {
    /// Validate the patch, convert it to the stored form and write it with its preview, then make
    /// it live. The rectangle must lie in the asset's content pixels and match the reserved shape;
    /// every colour must be finite and within the half float range once linear, every coverage in
    /// `0..=1`; the label is 1 to 32 characters and the provenance string and number fields within
    /// 2 KiB. A job cancelled meanwhile publishes nothing. Returns the candidate's number.
    pub(crate) fn publish(mut self, patch: CandidatePatch<'_>) -> Result<u8, Error> {
        self.control.checkpoint()?;
        if patch.shape() != self.shape {
            return Err(Error::validation(format!(
                "candidate {} was reserved at {:?}, not {:?}",
                self.index,
                self.shape,
                patch.shape()
            )));
        }
        let (label, provenance) = describe(&patch, &self.basis, &self.module_id, &self.task_id)?;
        let stored = encode(&patch)?;
        let sha256 = format!("{:x}", Sha256::digest(&stored));
        self.control.checkpoint()?;
        let directory = self.store.job_directory(&self.job_id);
        self.written = true;
        let write = |part, bytes: &[u8]| {
            fs::write(part_path(&directory, self.index, part), bytes)
                .map_err(|error| file_error("cannot write candidate", error.kind()))
        };
        write(CandidatePart::Patch, &stored)?;
        drop(stored);
        write(CandidatePart::Preview, patch.preview)?;
        let control = self.control.clone();
        let published = Published {
            rect: patch.rect,
            label,
            provenance,
            sha256,
        };
        let (job_id, index) = (self.job_id.clone(), self.index);
        self.store.change(|inner, _| {
            let held = inner.jobs.get_mut(&job_id);
            let slot = held
                .filter(|held| !held.closed && !control.is_cancelled())
                .and_then(|held| held.slots.get_mut(index)?.as_mut())
                .filter(|slot| slot.state == SlotState::Reserved);
            match slot {
                Some(slot) => {
                    slot.state = SlotState::Live;
                    slot.files = true;
                    slot.published = Some(published);
                    Ok(())
                }
                None => Err(control.cancelled_error()),
            }
        })?;
        self.done = true;
        self.control.changed();
        Ok(index as u8)
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let (job_id, index) = (self.job_id.clone(), self.index);
        self.store.change(|inner, _| {
            let Some(held) = inner.jobs.get_mut(&job_id) else {
                return;
            };
            if let Some(entry) = held.slots.get_mut(index)
                && entry
                    .as_ref()
                    .is_some_and(|slot| slot.state == SlotState::Reserved)
            {
                let slot = entry.take().expect("a reserved slot");
                inner.held -= slot.bytes;
            }
        });
        if self.written {
            let directory = self.store.job_directory(&self.job_id);
            for part in [CandidatePart::Patch, CandidatePart::Preview] {
                let _ = fs::remove_file(part_path(&directory, self.index, part));
            }
        }
    }
}

/// The label and the provenance with the host's `module` and `task` first, checked.
fn describe(
    patch: &CandidatePatch<'_>,
    basis: &CandidateBasis,
    module_id: &str,
    task_id: &str,
) -> Result<(String, Map<String, Value>), Error> {
    let [x, y, width, height] = patch.rect;
    if u64::from(x) + u64::from(width) > u64::from(basis.width)
        || u64::from(y) + u64::from(height) > u64::from(basis.height)
    {
        return Err(Error::validation(format!(
            "candidate rectangle [{x}, {y}, {width}, {height}] leaves the {}×{} photograph",
            basis.width, basis.height
        )));
    }
    let label = patch.label.trim();
    let characters = label.chars().count();
    if characters == 0 || characters > MAX_CANDIDATE_LABEL || label.chars().any(char::is_control) {
        return Err(Error::validation(format!(
            "a candidate label is 1 to {MAX_CANDIDATE_LABEL} printable characters"
        )));
    }
    let mut provenance = Map::new();
    provenance.insert("module".into(), json!(module_id));
    provenance.insert("task".into(), json!(task_id));
    for (key, value) in &patch.provenance {
        if provenance.contains_key(key) {
            return Err(Error::validation(format!(
                "provenance field {key} is the host's"
            )));
        }
        if key.is_empty() || key.len() > 64 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(Error::validation(format!(
                "provenance field {key:?} is not a name of 1 to 64 printable characters"
            )));
        }
        if !(value.is_string() || value.is_number()) {
            return Err(Error::validation(format!(
                "provenance field {key} is a string or a number"
            )));
        }
        provenance.insert(key.clone(), value.clone());
    }
    let length = serde_json::to_vec(&provenance)
        .map_err(|error| Error::internal(error.to_string()))?
        .len();
    if length > MAX_PROVENANCE_BYTES {
        return Err(Error::validation(format!(
            "a candidate's provenance is at most {MAX_PROVENANCE_BYTES} bytes, not {length}"
        )));
    }
    Ok((label.to_owned(), provenance))
}

/// The stored patch: RGBA half floats, little-endian, RGB linear and A the coverage. Checks every
/// length, value and coverage as it goes, so a refused patch allocates nothing past its own size.
fn encode(patch: &CandidatePatch<'_>) -> Result<Vec<u8>, Error> {
    let shape = patch.shape();
    let pixels = shape.width as usize * shape.height as usize;
    let preview = shape.preview_width as usize * shape.preview_height as usize * 3;
    if patch.preview.len() != preview {
        return Err(Error::validation(format!(
            "a {}×{} preview holds {preview} bytes, not {}",
            shape.preview_width,
            shape.preview_height,
            patch.preview.len()
        )));
    }
    if patch.coverage.len() != pixels {
        return Err(Error::validation(format!(
            "a {}×{} patch has {pixels} coverages, not {}",
            shape.width,
            shape.height,
            patch.coverage.len()
        )));
    }
    let channels = match patch.colour {
        PatchColour::LinearSrgb(values) => values.len(),
        PatchColour::Srgb8(codes) => codes.len(),
    };
    if channels != pixels * 3 {
        return Err(Error::validation(format!(
            "a {}×{} patch has {} RGB values, not {channels}",
            shape.width,
            shape.height,
            pixels * 3
        )));
    }
    let decoded: Option<[f32; 256]> = match patch.colour {
        PatchColour::Srgb8(_) => {
            let mut table = [0.0; 256];
            for (code, slot) in table.iter_mut().enumerate() {
                *slot = crate::colour::srgb::decode_u8(code as u8) as f32;
            }
            Some(table)
        }
        PatchColour::LinearSrgb(_) => None,
    };
    let mut stored = Vec::with_capacity(pixels * 8);
    for (pixel, &coverage) in patch.coverage.iter().enumerate() {
        for channel in 0..3 {
            let value = match (patch.colour, &decoded) {
                (PatchColour::LinearSrgb(values), _) => values[pixel * 3 + channel],
                (PatchColour::Srgb8(codes), Some(table)) => {
                    table[usize::from(codes[pixel * 3 + channel])]
                }
                (PatchColour::Srgb8(_), None) => unreachable!("an sRGB patch has its table"),
            };
            if !value.is_finite() || value.abs() > HALF_MAX {
                return Err(Error::validation(format!(
                    "patch value {value} at pixel {pixel} is not a finite half float"
                )));
            }
            stored.extend_from_slice(&half::f16::from_f32(value).to_le_bytes());
        }
        if !(0.0..=1.0).contains(&coverage) {
            return Err(Error::validation(format!(
                "coverage {coverage} at pixel {pixel} is outside 0 to 1"
            )));
        }
        stored.extend_from_slice(&half::f16::from_f32(coverage).to_le_bytes());
    }
    Ok(stored)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PinKind {
    Read,
    Accept,
}

/// A live candidate held for a read or an accept, with what it says about itself. Dropping it
/// settles it: an accept that did not commit leaves the candidate live, or releases it if a cancel
/// waited for the accept.
pub(crate) struct CandidatePin {
    store: Arc<Candidates>,
    job_id: JobId,
    index: usize,
    kind: PinKind,
    directory: PathBuf,
    read: CandidateRead,
    module_id: String,
    #[cfg_attr(not(test), allow(dead_code))]
    task_id: String,
    sha256: String,
    patch_bytes: u64,
    preview_bytes: u64,
    settled: bool,
}

impl std::fmt::Debug for CandidatePin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandidatePin")
            .field("job_id", &self.job_id)
            .field("candidate", &self.index)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl CandidatePin {
    /// The candidate as `job.read` lists it, when it was pinned: its asset and revision, its
    /// rectangle, dimensions, label and provenance.
    pub(crate) fn candidate(&self) -> &CandidateRead {
        &self.read
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn job_id(&self) -> &JobId {
        &self.job_id
    }

    /// The module whose task published it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn module_id(&self) -> &str {
        &self.module_id
    }

    /// The task that published it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn task_id(&self) -> &str {
        &self.task_id
    }

    /// How many bytes a part holds stored.
    pub(crate) fn total(&self, part: CandidatePart) -> u64 {
        match part {
            CandidatePart::Patch => self.patch_bytes,
            CandidatePart::Preview => self.preview_bytes,
        }
    }

    /// Open a part, checking it still holds its stored length.
    fn open(&self, part: CandidatePart) -> Result<File, Error> {
        let corrupt = || {
            Error::source_unavailable(format!(
                "candidate {} of job {} is missing or corrupt",
                self.index, self.job_id
            ))
        };
        let file = match File::open(part_path(&self.directory, self.index, part)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(corrupt()),
            Err(error) => return Err(file_error("cannot read candidate", error.kind())),
            Ok(file) => file,
        };
        let length = file
            .metadata()
            .map_err(|error| file_error("cannot read candidate", error.kind()))?
            .len();
        if length != self.total(part) {
            return Err(corrupt());
        }
        Ok(file)
    }

    /// Read `length` bytes of a part from `offset`, which the caller has bounded to the part and to
    /// one chunk. Worker-side.
    pub(crate) fn read(
        &self,
        part: CandidatePart,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>, Error> {
        if offset.saturating_add(length) > self.total(part) || length > MAX_CANDIDATE_CHUNK {
            return Err(Error::validation(format!(
                "{length} bytes from {offset} is not a chunk of the candidate's {} bytes",
                self.total(part)
            )));
        }
        let mut file = self.open(part)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| file_error("cannot read candidate", error.kind()))?;
        let mut bytes = vec![0; length as usize];
        file.read_exact(&mut bytes)
            .map_err(|error| file_error("cannot read candidate", error.kind()))?;
        Ok(bytes)
    }

    /// Publish the stored patch as a `repair-patch` artifact through the catalog's artifact writer,
    /// on the worker that calls it: the file is read whole (at most 64 MiB), its length and hash
    /// checked against what was stored, then written and synced as the artifact. The owner records
    /// the returned row ([`crate::EditorService::register_artifact`]) and commits the edit that
    /// references it, then settles the pin ([`Self::accepted`]); a commit that is refused leaves
    /// only an unreferenced artifact, which `artifact.collect` removes once this process no
    /// longer holds it live.
    pub(crate) fn publish(
        &self,
        writer: &ArtifactWriter,
        cancel: &AtomicBool,
    ) -> Result<(ArtifactRecord, Arc<PreparedArtifact>), Error> {
        let cancelled = || {
            cancel
                .load(Ordering::Relaxed)
                .then(|| Error::cancelled("the accept was cancelled"))
        };
        if let Some(error) = cancelled() {
            return Err(error);
        }
        let mut file = self.open(CandidatePart::Patch)?;
        let mut bytes = Vec::with_capacity(self.patch_bytes as usize);
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; CHUNK];
        loop {
            let read = match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(file_error("cannot read candidate", error.kind())),
            };
            hasher.update(&buffer[..read]);
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.len() as u64 > self.patch_bytes {
                break;
            }
        }
        if bytes.len() as u64 != self.patch_bytes
            || format!("{:x}", hasher.finalize()) != self.sha256
        {
            return Err(Error::source_unavailable(format!(
                "candidate {} of job {} is corrupt",
                self.index, self.job_id
            )));
        }
        if let Some(error) = cancelled() {
            return Err(error);
        }
        writer.write(
            &bytes,
            ArtifactMeta {
                kind: PATCH_KIND.into(),
                width: Some(self.read.width),
                height: Some(self.read.height),
                colour: Some(PATCH_COLOUR.into()),
            },
            &self.module_id,
        )
    }

    /// The commit that references the published patch holds: the candidate is `accepted`, its
    /// files go once no read holds them, and its bytes leave the budget.
    pub(crate) fn accepted(mut self) {
        self.settled = true;
        self.store.unpin(
            &self.job_id,
            self.index,
            self.kind,
            self.kind == PinKind::Accept,
        );
    }
}

impl Drop for CandidatePin {
    fn drop(&mut self) {
        if !self.settled {
            self.store.unpin(&self.job_id, self.index, self.kind, false);
        }
    }
}

/// Standard base64 with padding, which `job.read-candidate` answers its chunks in.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let word = (u32::from(group[0]) << 16)
            | (u32::from(*group.get(1).unwrap_or(&0)) << 8)
            | u32::from(*group.get(2).unwrap_or(&0));
        for position in 0..4 {
            if position <= group.len() {
                text.push(char::from(
                    ALPHABET[(word >> (18 - 6 * position)) as usize & 63],
                ));
            } else {
                text.push('=');
            }
        }
    }
    text
}
