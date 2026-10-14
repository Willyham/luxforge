//! Protected consent in the person's preferences, the once-only startup claim and the core of the
//! `usage.*` commands ([design](../../../../docs/design/usage-statistics.md#consent-and-first-launch)).
//!
//! The record is `usage_statistics` in `preferences.json`, written only through [`Usage`]: never
//! by `preferences.set`, its `null` or any reset. An absent record means the notice was never
//! shown and sharing is off. An enabled record holds a random installation ID and a random image
//! key, created together on the saved disabled-to-enabled transition and removed together by a
//! saved opt-out; the image key is never answered by any read.
//!
//! One [`Usage`] serves a whole process, whatever catalogs it opens. Its [`Gate`] is what producers
//! check. Turning off ends the generation before anything is written, so off is immediate even when
//! the write fails; turning on writes first and takes effect only once the write succeeded. A read
//! failure, an unusable record, an ineligible launch or another disclosure version collects
//! nothing. There is no preferences watch or polling: the statistics worker confirms the stored
//! record with [`Usage::confirm_stored`] before detaching a window and before every upload. Every
//! change of the effective state calls the one watcher a statistics supervisor registers
//! ([`Usage::watch`]), a coalesced wake rather than a poll.
use super::{
    DISCLOSURE_VERSION, Gate, Generation,
    eligibility::{Eligibility, LaunchMode, Reason},
    encode::{Release, ReportId},
    event::Diagnostics,
    image_id::ImageKey,
    registry::Trigger,
};
use crate::{
    ClientAuthority, Error,
    preferences::{PreferenceStore, Preferences},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fmt,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};
use uuid::Uuid;
use zeroize::Zeroizing;

/// The protected record's key in `preferences.json`.
pub const RECORD_KEY: &str = "usage_statistics";

/// How many `usage.set` request IDs one process remembers, so a retry of an enable that a later
/// opt-out revoked is answered rather than applied again, even after the owner's own request table
/// has forgotten it or the catalog changed.
const REMEMBERED_REQUESTS: usize = 64;

/// The random reporting ID of one consent lifetime: a canonical lowercase UUIDv4. Its `Debug` is
/// redacted so it never reaches a log by accident.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct InstallationId(Uuid);

impl InstallationId {
    fn random() -> Self {
        Self(Uuid::new_v4())
    }

    fn parse(text: &str) -> Option<Self> {
        let uuid = Uuid::parse_str(text).ok()?;
        (uuid.get_version() == Some(uuid::Version::Random)
            && uuid.get_variant() == uuid::Variant::RFC4122
            && uuid.hyphenated().to_string() == text)
            .then_some(Self(uuid))
    }
}

impl InstallationId {
    /// The ID as the wire encoder carries it.
    pub fn report_id(&self) -> ReportId {
        ReportId::from_uuid(self.0).expect("an installation ID is a UUIDv4")
    }
}

impl fmt::Display for InstallationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.hyphenated().fmt(f)
    }
}

impl fmt::Debug for InstallationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InstallationId(..)")
    }
}

impl Serialize for InstallationId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for InstallationId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| {
            de::Error::custom("installation_id must be a canonical lowercase UUIDv4")
        })
    }
}

/// 32 bytes of the operating system's cryptographic randomness: a fresh image key.
fn random_image_key() -> Result<ImageKey, Error> {
    let mut bytes = Zeroizing::new([0; 32]);
    getrandom::fill(bytes.as_mut_slice())
        .map_err(|error| Error::internal(format!("no system randomness: {error}")))?;
    Ok(ImageKey::from_bytes(*bytes))
}

/// The image key as the record stores it: 64 lowercase hexadecimal characters, never any other
/// spelling.
mod image_key_hex {
    use super::ImageKey;
    use serde::{Deserialize, Deserializer, Serializer, de};
    use zeroize::Zeroizing;

    pub(super) fn serialize<S: Serializer>(
        key: &Option<ImageKey>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let Some(key) = key else {
            return serializer.serialize_none();
        };
        let mut text = Zeroizing::new(String::with_capacity(64));
        for byte in key.as_bytes() {
            text.push(HEX[usize::from(byte >> 4)] as char);
            text.push(HEX[usize::from(byte & 0xf)] as char);
        }
        serializer.serialize_str(&text)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ImageKey>, D::Error> {
        let Some(text) = Option::<String>::deserialize(deserializer)?.map(Zeroizing::new) else {
            return Ok(None);
        };
        let refused = || de::Error::custom("image_key must be 64 lowercase hexadecimal characters");
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        if text.len() != 64 {
            return Err(refused());
        }
        let mut bytes = Zeroizing::new([0; 32]);
        for (index, pair) in text.as_bytes().chunks_exact(2).enumerate() {
            let (Some(high), Some(low)) = (digit(pair[0]), digit(pair[1])) else {
                return Err(refused());
            };
            bytes[index] = high << 4 | low;
        }
        Ok(Some(ImageKey::from_bytes(*bytes)))
    }
}

/// The protected record as stored. The only supported shape is this one: anything else makes the
/// preferences file unreadable, so it is refused unchanged and statistics stay off.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageRecord {
    /// The startup notice was claimed, or a choice was saved: no automatic notice again.
    pub(crate) prompt_shown: bool,
    pub(crate) enabled: bool,
    /// The disclosure the record was written under.
    pub(crate) disclosure_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) installation_id: Option<InstallationId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "image_key_hex"
    )]
    pub(crate) image_key: Option<ImageKey>,
}

impl UsageRecord {
    /// What the record's type cannot say: a positive disclosure version, and an installation ID
    /// and image key exactly while it is enabled.
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.disclosure_version == 0 {
            return Err(format!(
                "{RECORD_KEY}.disclosure_version must be a positive integer"
            ));
        }
        match (self.enabled, &self.installation_id, &self.image_key) {
            (true, Some(_), Some(_)) | (false, None, None) => Ok(()),
            (true, ..) => Err(format!(
                "{RECORD_KEY}: an enabled record holds an installation_id and an image_key"
            )),
            (false, ..) => Err(format!(
                "{RECORD_KEY}: a disabled record holds no installation_id or image_key"
            )),
        }
    }

    /// The stored identity when the record is consent to the current disclosure.
    fn current(&self) -> Option<(InstallationId, ImageKey)> {
        match (&self.installation_id, &self.image_key) {
            (Some(id), Some(key))
                if self.enabled && self.disclosure_version == DISCLOSURE_VERSION =>
            {
                Some((*id, key.clone()))
            }
            _ => None,
        }
    }
}

/// The record as `preferences.read` reports it: never the image key. An absent record reads as
/// not shown and off.
pub(crate) fn public_record(record: Option<&UsageRecord>) -> Value {
    match record {
        None => json!({"prompt_shown": false, "enabled": false}),
        Some(record) => {
            let mut value = json!({
                "prompt_shown": record.prompt_shown,
                "enabled": record.enabled,
                "disclosure_version": record.disclosure_version,
            });
            if let Some(id) = record.installation_id {
                value["installation_id"] = json!(id);
            }
            value
        }
    }
}

/// The delivery contract `usage.read` publishes. The statistics worker is held to these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Limits {
    /// The most events one window, and so one batch, holds.
    pub window_events: u32,
    /// The largest request body, in bytes.
    pub batch_bytes: u32,
    /// The shortest time between two upload attempts, in seconds.
    pub attempt_interval_s: u32,
    /// How long after a fresh window's first event it is flushed, in seconds, before jitter.
    pub flush_after_s: u32,
    /// How old a window's oldest contribution may grow before it is discarded, in seconds.
    pub window_age_s: u32,
}

impl Limits {
    /// The design's delivery policy.
    pub const CONTRACT: Self = Self {
        window_events: 512,
        batch_bytes: 64 * 1024,
        attempt_interval_s: 60,
        flush_after_s: 120,
        window_age_s: 30 * 60,
    };
}

/// What `usage.read` answers. Never the image key, buffered events or file contents.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsageState {
    /// Whether the startup notice was claimed or a choice saved; `null` when the preferences
    /// cannot be read.
    pub prompt_shown: Option<bool>,
    /// The stored choice; `null` when the preferences cannot be read.
    pub enabled: Option<bool>,
    /// Whether this process is collecting now.
    pub effective: bool,
    /// The disclosure this build describes, which an enable must name.
    pub disclosure_version: u32,
    /// Where an eligible launch sends: the production destination, hosted test tenant or test
    /// receiver.
    pub destination: String,
    /// The release a batch reports, `<version>+<12 hex>` (synthetic for unrevisioned developer
    /// builds), or `null` when unavailable.
    pub release: Option<String>,
    /// The stored installation ID while the stored choice is on.
    pub installation_id: Option<InstallationId>,
    /// The current session's ID; `null` until the statistics worker starts a session.
    pub session_id: Option<String>,
    /// Why this process is not collecting; `null` while it is.
    pub reason: Option<Reason>,
    pub limits: Limits,
    /// The statistics worker's bounded local counts, all zero until a worker runs.
    #[serde(serialize_with = "diagnostics")]
    pub diagnostics: Diagnostics,
}

/// [`Diagnostics`] as `usage.read` answers them:
/// `{recorded, dropped, sent, attempts, accepted, delivery: {state, reason}}`.
fn diagnostics<S: Serializer>(counts: &Diagnostics, serializer: S) -> Result<S::Ok, S::Error> {
    json!({
        "recorded": counts.recorded,
        "dropped": counts.dropped,
        "sent": counts.sent,
        "attempts": counts.attempts,
        "accepted": counts.accepted,
        "delivery": {"state": counts.delivery.state(), "reason": counts.delivery.reason()},
    })
    .serialize(serializer)
}

/// The stored consent as the statistics worker confirms it: whether the record is consent to the
/// current disclosure, and its installation ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredConsent {
    pub enabled: bool,
    pub installation_id: Option<InstallationId>,
}

/// An off-owner reader of the stored record, for the statistics worker. It reads the file again on
/// every call and never writes it.
pub struct ConsentReader(PreferenceStore);

impl ConsentReader {
    /// A reader of `preferences.json` under `preferences_dir`, the same directory the host's
    /// preferences use ([`crate::HostConfig::preferences_dir`]).
    pub fn new(preferences_dir: Option<PathBuf>) -> Self {
        Self(PreferenceStore::new(preferences_dir))
    }

    /// Reread the stored record: whether it is consent to the current disclosure, and the
    /// installation ID it holds. A file that cannot be read is an error, which a caller treats as
    /// an opt-out.
    pub fn stored(&self) -> Result<StoredConsent, Error> {
        let current = self
            .0
            .read()?
            .usage_statistics
            .as_ref()
            .and_then(UsageRecord::current);
        Ok(StoredConsent {
            enabled: current.is_some(),
            installation_id: current.map(|(id, _)| id),
        })
    }
}

/// The identity collection runs under while the gate is on: its generation, the installation ID
/// ([`InstallationId::report_id`] for the encoder) and the image key, from the record that enabled
/// it, and whether the launch read or an opt-in turned it on. The key is never serialized and its
/// `Debug` is redacted; the session's collector takes it as it is.
#[derive(Clone)]
pub struct Active {
    pub generation: Generation,
    pub installation_id: InstallationId,
    pub image_key: ImageKey,
    /// [`Trigger::Launch`] for the launch read of a stored opt-in, [`Trigger::OptIn`] for an
    /// enable in this process: the first session's `session.start` trigger.
    pub trigger: Trigger,
}

impl fmt::Debug for Active {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Active")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// What one `usage.set` did.
#[derive(Debug)]
pub(crate) struct SetOutcome {
    /// Whether the stored record changed.
    pub(crate) changed: bool,
    pub(crate) state: UsageState,
}

#[derive(Default)]
struct Runtime {
    /// Whether the launch read of the stored record has happened: once per process, so a later
    /// owner (another catalog) never re-enables what this process turned off.
    started: bool,
    active: Option<Active>,
    /// The `usage.set` request IDs this process applied, with their `enabled`, oldest first.
    applied: VecDeque<(String, bool)>,
}

/// The process-wide consent runtime: eligibility, the collection gate and the identity collection
/// runs under. Every catalog owner of the process shares one ([`crate::HostConfig::usage_consent`]).
pub struct Usage {
    eligibility: Eligibility,
    gate: Gate,
    runtime: Mutex<Runtime>,
    /// Held across a whole `usage.set`, so two owners' changes never interleave a write and its
    /// effect.
    writer: Mutex<()>,
    /// Counts revocations, so an enable whose write finished after a revocation began does not turn
    /// the gate back on.
    revocations: AtomicU64,
    diagnostics: Mutex<Diagnostics>,
    /// The statistics worker's current session ID, which `usage.read` answers while collecting.
    session_id: Mutex<Option<ReportId>>,
    /// The release the statistics supervisor's batches report, when it was given one other than the
    /// build's ([`Usage::set_release`]).
    reported_release: Mutex<Option<Release>>,
    /// Called after every change of the effective state, outside every lock of this runtime.
    watcher: Mutex<Option<Watcher>>,
}

/// What [`Usage::watch`] registers: a coalesced wake, which must not block or call back into the
/// runtime's writers.
pub type Watcher = Arc<dyn Fn() + Send + Sync>;

impl fmt::Debug for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Usage")
            .field("eligibility", &self.eligibility)
            .field("generation", &self.gate.current())
            .finish_non_exhaustive()
    }
}

impl Default for Usage {
    /// [`Usage::headless`].
    fn default() -> Self {
        Self::headless()
    }
}

impl Usage {
    /// A runtime with this eligibility, off until the launch read of a stored opt-in or an
    /// explicit enable.
    pub fn new(eligibility: Eligibility) -> Self {
        Self {
            eligibility,
            gate: Gate::new(),
            runtime: Mutex::new(Runtime::default()),
            writer: Mutex::new(()),
            revocations: AtomicU64::new(0),
            diagnostics: Mutex::new(Diagnostics::default()),
            session_id: Mutex::new(None),
            reported_release: Mutex::new(None),
            watcher: Mutex::new(None),
        }
    }

    /// Register the one watcher (the statistics supervisor's wake), replacing any earlier one. It
    /// is called after each change of the effective state: the launch read or an enable turning
    /// collection on, an opt-out (before its write is attempted) and [`Usage::confirm_stored`]
    /// ending a generation. There is no preferences watch or polling behind it.
    pub fn watch(&self, watcher: Option<Watcher>) {
        *self
            .watcher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = watcher;
    }

    /// Tell the watcher the effective state changed. Never called while a lock of this runtime is
    /// held, so the watcher may read [`Usage::active`].
    fn changed(&self) {
        let watcher = self
            .watcher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(watcher) = watcher {
            watcher();
        }
    }

    /// The launch read for a host that has no catalog owner to make it: read the stored record
    /// under `preferences_dir` once, as an owner does when it starts. It never writes and never
    /// grants consent that is not stored.
    pub fn launch(&self, preferences_dir: Option<PathBuf>) {
        self.start(PreferenceStore::new(preferences_dir).read().as_ref());
    }

    /// Record the statistics worker's current session, which `usage.read` answers as its
    /// `session_id` while collection is effective.
    pub fn set_session(&self, session: Option<ReportId>) {
        *self
            .session_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = session;
    }

    fn session_id(&self) -> Option<ReportId> {
        *self
            .session_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A host without a desktop, such as `luxforge-json`: never eligible.
    pub fn headless() -> Self {
        Self::new(Eligibility::production(LaunchMode::Headless))
    }

    pub fn eligibility(&self) -> &Eligibility {
        &self.eligibility
    }

    /// The release a batch reports, as the wire encoder carries it: the packaged build's
    /// `<version>+<12 hex>`, or `None` for a build without an embedded clean revision.
    pub fn release(&self) -> Option<Release> {
        self.eligibility
            .release()
            .and_then(|release| Release::parse(&release))
    }

    /// Record the release the statistics supervisor's batches report, so `usage.read` answers the
    /// release a batch carries: the build's, or a test host's own.
    pub fn set_release(&self, release: Option<Release>) {
        *self
            .reported_release
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = release;
    }

    /// The gate every producer checks first: one atomic load.
    pub fn gate(&self) -> &Gate {
        &self.gate
    }

    /// The identity of the current generation, or `None` while collection is off.
    pub fn active(&self) -> Option<Active> {
        let runtime = self.lock();
        runtime
            .active
            .as_ref()
            .filter(|active| self.gate.is_current(active.generation))
            .cloned()
    }

    /// The statistics worker's latest counts.
    pub fn diagnostics(&self) -> Diagnostics {
        *self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Replace the counts `usage.read` reports.
    pub fn set_diagnostics(&self, diagnostics: Diagnostics) {
        *self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = diagnostics;
    }

    /// Confirm, off the owner, that the generation the worker holds is still consented: reread the
    /// stored record and compare it with the identity in memory.
    ///
    /// The statistics worker calls this before detaching a window and again immediately before
    /// every network handoff, and sends nothing when it answers `false`. It answers `false` when
    /// `generation` is no longer current, and ends the generation (the gate turns off and the
    /// identity is dropped) when the stored record is an opt-out, cannot be read, is for another
    /// disclosure, or holds an installation ID different from the one in memory, as when another
    /// process opted out and back in. So another process's revocation is never followed by an
    /// upload, without a preferences watch or polling timer.
    pub fn confirm_stored(&self, reader: &ConsentReader, generation: Generation) -> bool {
        let stored = reader.stored();
        let mut runtime = self.lock();
        let Some(active) = &runtime.active else {
            return false;
        };
        if active.generation != generation || !self.gate.is_current(generation) {
            return false;
        }
        match stored {
            Ok(StoredConsent {
                enabled: true,
                installation_id: Some(id),
            }) if id == active.installation_id => true,
            _ => {
                self.gate.disable();
                runtime.active = None;
                drop(runtime);
                self.changed();
                false
            }
        }
    }

    /// The launch read: once per process, an eligible launch whose stored record is consent to the
    /// current disclosure turns collection on with the stored identity. A read failure, an
    /// unusable record or another disclosure leaves it off and writes nothing.
    pub(crate) fn start(&self, stored: Result<&Preferences, &Error>) {
        let mut runtime = self.lock();
        if std::mem::replace(&mut runtime.started, true) || !self.eligibility.is_eligible() {
            return;
        }
        let Some((installation_id, image_key)) = stored
            .ok()
            .and_then(|preferences| preferences.usage_statistics.as_ref())
            .and_then(UsageRecord::current)
        else {
            return;
        };
        runtime.active = Some(Active {
            generation: self.gate.enable(),
            installation_id,
            image_key,
            trigger: Trigger::Launch,
        });
        drop(runtime);
        self.changed();
    }

    /// `usage.read`: the stored record and this process's state. Reads one small file and makes no
    /// network request.
    pub(crate) fn read(&self, store: &PreferenceStore) -> UsageState {
        let stored = store.read().map(|preferences| preferences.usage_statistics);
        self.state(stored.as_ref())
    }

    fn state(&self, stored: Result<&Option<UsageRecord>, &Error>) -> UsageState {
        let effective = self.active().is_some();
        let (prompt_shown, enabled, installation_id, stored_reason) = match stored {
            Err(_) => (None, None, None, Reason::UnreadableConfiguration),
            Ok(None) => (Some(false), Some(false), None, Reason::Choice),
            Ok(Some(record)) => (
                Some(record.prompt_shown),
                Some(record.enabled),
                record.installation_id.filter(|_| record.enabled),
                if record.enabled && record.disclosure_version != DISCLOSURE_VERSION {
                    Reason::ChangedDisclosure
                } else {
                    Reason::Choice
                },
            ),
        };
        UsageState {
            prompt_shown,
            enabled,
            effective,
            disclosure_version: DISCLOSURE_VERSION,
            destination: self.eligibility.destination().to_owned(),
            release: self
                .reported_release
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .map(|release| release.as_str().to_owned())
                .or_else(|| self.eligibility.release()),
            installation_id,
            session_id: self
                .session_id()
                .filter(|_| effective)
                .map(|id| String::from_utf8_lossy(&id.to_wire()).into_owned()),
            reason: (!effective).then(|| self.eligibility.reason().unwrap_or(stored_reason)),
            limits: Limits::CONTRACT,
            diagnostics: self.diagnostics(),
        }
    }

    /// `usage.prompt.claim`: in an eligible launch, atomically mark the notice shown, with sharing
    /// off, under the preferences lock, and answer `true` to the one claimant that wrote it. A
    /// record that was claimed, answered or enabled answers `false`, as does an ineligible launch,
    /// which writes nothing. A read or write failure is an error and the file is kept as it was.
    #[cfg(test)]
    pub(crate) fn claim(&self, store: &PreferenceStore) -> Result<bool, Error> {
        self.claim_published(store)?.finish()
    }

    pub(crate) fn claim_published(
        &self,
        store: &PreferenceStore,
    ) -> Result<crate::atomic_file::Publication<bool>, Error> {
        if !self.eligibility.is_eligible() {
            return Ok(crate::atomic_file::Publication::confirmed(false));
        }
        store.update_usage_published(|record| {
            if record
                .as_ref()
                .is_some_and(|record| record.prompt_shown || record.enabled)
            {
                return Ok(false);
            }
            *record = Some(UsageRecord {
                prompt_shown: true,
                enabled: false,
                disclosure_version: DISCLOSURE_VERSION,
                installation_id: None,
                image_key: None,
            });
            Ok(true)
        })
    }

    /// `usage.set`. An enable needs permission authority, the current disclosure version and an
    /// eligible launch; it writes first and takes effect only once the write succeeded, creating a
    /// fresh installation ID and image key unless the record already is consent to this disclosure.
    /// A disable is anyone's: the gate turns off before the write is attempted, and a failed write
    /// leaves this process off and reports the stored state as it really is. Either saved choice
    /// also marks the notice as answered. A request ID this process already applied is answered
    /// with the current state and never applied again.
    #[cfg(test)]
    pub(crate) fn set(
        &self,
        store: &PreferenceStore,
        authority: ClientAuthority,
        enabled: bool,
        disclosure_version: Option<u32>,
        request_id: &str,
    ) -> Result<SetOutcome, Error> {
        self.set_published(store, authority, enabled, disclosure_version, request_id)?
            .finish()
    }

    pub(crate) fn set_published(
        &self,
        store: &PreferenceStore,
        authority: ClientAuthority,
        enabled: bool,
        disclosure_version: Option<u32>,
        request_id: &str,
    ) -> Result<crate::atomic_file::Publication<SetOutcome>, Error> {
        if enabled {
            if authority != ClientAuthority::Permissions {
                return Err(Error::forbidden(
                    "enabling usage statistics needs permission authority",
                ));
            }
            match disclosure_version {
                Some(DISCLOSURE_VERSION) => {}
                Some(version) => {
                    return Err(Error::validation(format!(
                        "disclosure_version {version} is not the current disclosure, {DISCLOSURE_VERSION}"
                    )));
                }
                None => {
                    return Err(Error::validation(format!(
                        "enabling usage statistics needs disclosure_version {DISCLOSURE_VERSION}"
                    )));
                }
            }
        } else if disclosure_version.is_some() {
            return Err(Error::validation(
                "disclosure_version is given only when enabling",
            ));
        }
        // A choice made in this process supersedes the launch read, which never runs after it.
        self.lock().started = true;
        if let Some(applied) = self.applied(request_id) {
            if applied != enabled {
                return Err(Error::conflict(
                    "request_id was already used with different input",
                ));
            }
            return Ok(crate::atomic_file::Publication::confirmed(SetOutcome {
                changed: false,
                state: self.read(store),
            }));
        }
        let published = if enabled {
            self.enable(store)?
        } else {
            self.disable(store)?
        };
        let mut runtime = self.lock();
        runtime.applied.push_back((request_id.to_owned(), enabled));
        if runtime.applied.len() > REMEMBERED_REQUESTS {
            runtime.applied.pop_front();
        }
        drop(runtime);
        Ok(published.map(|changed| SetOutcome {
            changed,
            state: self.read(store),
        }))
    }

    fn applied(&self, request_id: &str) -> Option<bool> {
        self.lock()
            .applied
            .iter()
            .find(|(applied, _)| applied == request_id)
            .map(|(_, enabled)| *enabled)
    }

    fn enable(
        &self,
        store: &PreferenceStore,
    ) -> Result<crate::atomic_file::Publication<bool>, Error> {
        if let Some(reason) = self.eligibility.reason() {
            return Err(Error::not_ready(format!(
                "usage statistics cannot be enabled in this launch ({})",
                serde_json::to_value(reason)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default()
            )));
        }
        let _writer = self.lock_writer();
        let revocations = self.revocations.load(Ordering::Acquire);
        let fresh = (InstallationId::random(), random_image_key()?);
        let published = store.update_usage_published(|record| {
            if let Some((id, key)) = record.as_ref().and_then(UsageRecord::current) {
                return Ok((false, id, key));
            }
            *record = Some(UsageRecord {
                prompt_shown: true,
                enabled: true,
                disclosure_version: DISCLOSURE_VERSION,
                installation_id: Some(fresh.0),
                image_key: Some(fresh.1.clone()),
            });
            Ok((true, fresh.0, fresh.1.clone()))
        })?;
        let (changed, installation_id, image_key) = published.value;
        let mut runtime = self.lock();
        // A revocation that began while this was being written wins: it writes after this.
        if self.revocations.load(Ordering::Acquire) != revocations {
            return Ok(crate::atomic_file::Publication {
                value: changed,
                durability_error: published.durability_error,
            });
        }
        let running = runtime.active.as_ref().is_some_and(|active| {
            self.gate.is_current(active.generation) && active.installation_id == installation_id
        });
        if !running {
            // Another identity's generation, if any, ends before this one starts.
            self.gate.disable();
            runtime.active = Some(Active {
                generation: self.gate.enable(),
                installation_id,
                image_key,
                trigger: Trigger::OptIn,
            });
            drop(runtime);
            self.changed();
        }
        Ok(crate::atomic_file::Publication {
            value: changed,
            durability_error: published.durability_error,
        })
    }

    fn disable(
        &self,
        store: &PreferenceStore,
    ) -> Result<crate::atomic_file::Publication<bool>, Error> {
        self.revocations.fetch_add(1, Ordering::AcqRel);
        self.gate.disable();
        self.lock().active = None;
        // The supervisor stops producers and cancels the request in flight before the write.
        self.changed();
        let _writer = self.lock_writer();
        let written = store.update_usage_published(|record| {
            let next = UsageRecord {
                prompt_shown: true,
                enabled: false,
                disclosure_version: DISCLOSURE_VERSION,
                installation_id: None,
                image_key: None,
            };
            let changed = record.as_ref() != Some(&next);
            *record = Some(next);
            Ok(changed)
        });
        written.map_err(|error| {
            let state = self.read(store);
            Error {
                detail: format!(
                    "{}; usage statistics are off in this process, and the stored choice and installation ID were not changed",
                    error.detail
                ),
                ..error
            }
            .with_data(json!({"state": state}))
        })
    }

    fn lock(&self) -> MutexGuard<'_, Runtime> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_writer(&self) -> MutexGuard<'_, ()> {
        self.writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
#[path = "consent_tests.rs"]
mod tests;
