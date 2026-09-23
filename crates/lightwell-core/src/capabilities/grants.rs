//! Scoped permission grants and denials, outside every catalog, in `<config>/modules/grants.json`.
//! The file follows the settings file's discipline: a `format: 1` marker, an advisory lock on
//! `grants.lock` for writers, a fresh read on every call, a synced temporary file renamed over the
//! old one, and refusal with `incompatible` of any other shape, which is never rewritten. Nothing
//! here ever holds a secret: a scope names a path, a resource version, or a profile, adapter,
//! origin, data class and asset. See `docs/design/module-capabilities.md#capability-and-consent-contract`.
use super::{
    atomic,
    descriptor::{CapabilityKind, DataClass},
};
use crate::{AssetId, Error, ErrorKind};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    fs::File,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// The only grants file format this build reads or writes.
pub const GRANTS_FORMAT: u32 = 1;
/// Grants and denials together, revoked grants included. A new record beyond this prunes the
/// oldest revoked grant or denial; when every record is a live grant, it is refused.
pub const MAX_GRANT_RECORDS: usize = 1024;
/// The largest grants file, read or written. A record is well under 1 KiB.
pub const MAX_GRANTS_BYTES: u64 = 2 * 1024 * 1024;

pub const GRANTS_FILE: &str = "grants.json";
pub const GRANTS_LOCK: &str = "grants.lock";
const GRANTS_TEMPORARY: &str = "grants.json.tmp";

/// The permission methods.
pub const GRANT: &str = "module.permission.grant";
pub const DENY: &str = "module.permission.deny";
pub const REVOKE: &str = "module.permission.revoke";
pub const LIST: &str = "module.permission.list";

/// The longest revocation reason a client may give, in characters.
pub const MAX_REASON: usize = 256;

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn incompatible(path: &Path, reason: impl std::fmt::Display) -> Error {
    Error::new(
        ErrorKind::Incompatible,
        format!("{}: {reason}; the file is kept unchanged", path.display()),
    )
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The capability kind a grant covers, which decides the shape of its scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantKind {
    ReadUserFile,
    DownloadArtifact,
    RemoteImageRequest,
}

impl GrantKind {
    pub fn of(kind: &CapabilityKind) -> Self {
        match kind {
            CapabilityKind::ReadUserFile { .. } => Self::ReadUserFile,
            CapabilityKind::DownloadArtifact { .. } => Self::DownloadArtifact,
            CapabilityKind::RemoteImageRequest { .. } => Self::RemoteImageRequest,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ReadUserFile => "read-user-file",
            Self::DownloadArtifact => "download-artifact",
            Self::RemoteImageRequest => "remote-image-request",
        }
    }
}

/// `read-user-file`: the canonical path a `file` setting holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileScope {
    pub path: PathBuf,
}

/// `download-artifact`: one declared resource version from the origin of its pinned URL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadScope {
    pub resource: String,
    pub version: String,
    pub origin: String,
}

/// `remote-image-request`: one data class of one asset, sent to one profile's endpoint origin
/// through its adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteScope {
    pub profile_id: String,
    pub adapter: String,
    pub origin: String,
    pub data: DataClass,
    pub asset_id: AssetId,
}

/// An exact grant scope. Matching is equality: nothing widens a scope, so a grant for one path,
/// resource version or asset never covers another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GrantScope {
    File(FileScope),
    Download(DownloadScope),
    Remote(RemoteScope),
}

impl GrantScope {
    pub fn kind(&self) -> GrantKind {
        match self {
            Self::File(_) => GrantKind::ReadUserFile,
            Self::Download(_) => GrantKind::DownloadArtifact,
            Self::Remote(_) => GrantKind::RemoteImageRequest,
        }
    }

    /// A client's scope, read strictly as the shape `kind` takes: an unknown or missing field is a
    /// `validation` error naming it.
    pub fn parse(kind: GrantKind, value: &Value) -> Result<Self, Error> {
        fn strict<T: DeserializeOwned>(kind: GrantKind, value: &Value) -> Result<T, Error> {
            serde_json::from_value(value.clone())
                .map_err(|error| validation(format!("a {} scope is invalid: {error}", kind.name())))
        }
        Ok(match kind {
            GrantKind::ReadUserFile => Self::File(strict(kind, value)?),
            GrantKind::DownloadArtifact => Self::Download(strict(kind, value)?),
            GrantKind::RemoteImageRequest => Self::Remote(strict(kind, value)?),
        })
    }
}

/// Why and when a grant stopped applying.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revocation {
    pub ms: u64,
    pub reason: String,
}

/// One grant. A revoked grant is kept, with its revocation, until it is pruned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// `grant-<uuid>`, generated by the host.
    pub grant_id: String,
    pub module_id: String,
    /// The declared capability's identity.
    pub capability: String,
    pub kind: GrantKind,
    pub scope: GrantScope,
    /// The authority of the client that granted it.
    pub actor: String,
    /// The grant request's identity, by which a retry is recognised.
    pub request_id: String,
    pub created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<Revocation>,
}

impl Grant {
    pub fn is_live(&self) -> bool {
        self.revoked.is_none()
    }

    fn covers(&self, module_id: &str, capability: &str, scope: &GrantScope) -> bool {
        self.module_id == module_id && self.capability == capability && &self.scope == scope
    }
}

/// A recorded "Don't allow" for one exact scope. A later consent request for that scope reports
/// `denied: true`; a grant of the scope clears it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Denial {
    pub module_id: String,
    pub capability: String,
    pub kind: GrantKind,
    pub scope: GrantScope,
    pub actor: String,
    pub ms: u64,
}

impl Denial {
    fn covers(&self, module_id: &str, capability: &str, scope: &GrantScope) -> bool {
        self.module_id == module_id && self.capability == capability && &self.scope == scope
    }
}

/// `module.permission.list`: grants, revoked ones included, and denials.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantList {
    pub grants: Vec<Grant>,
    pub denials: Vec<Denial>,
}

/// `{format: 1, grants: [...], denials: [...]}`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: u32,
    #[serde(default)]
    grants: Vec<Grant>,
    #[serde(default)]
    denials: Vec<Denial>,
}

impl Document {
    fn empty() -> Self {
        Self {
            format: GRANTS_FORMAT,
            grants: Vec::new(),
            denials: Vec::new(),
        }
    }

    fn records(&self) -> usize {
        self.grants.len() + self.denials.len()
    }

    /// Make room for one more record by removing the oldest revoked grant or denial, by the time it
    /// stopped applying; refuse when every record is a live grant.
    fn make_room(&mut self) -> Result<(), Error> {
        if self.records() < MAX_GRANT_RECORDS {
            return Ok(());
        }
        let revoked = self
            .grants
            .iter()
            .enumerate()
            .filter_map(|(index, grant)| grant.revoked.as_ref().map(|at| (at.ms, index)))
            .min();
        let denied = self
            .denials
            .iter()
            .enumerate()
            .map(|(index, denial)| (denial.ms, index))
            .min();
        match (revoked, denied) {
            (Some(grant), Some(denial)) if grant <= denial => {
                self.grants.remove(grant.1);
            }
            (_, Some(denial)) => {
                self.denials.remove(denial.1);
            }
            (Some(grant), None) => {
                self.grants.remove(grant.1);
            }
            (None, None) => {
                return Err(Error::new(
                    ErrorKind::ResourceLimit,
                    format!(
                        "{MAX_GRANT_RECORDS} live grants are recorded; revoke one before granting another"
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// What a grant request did: a new grant, the grant a retry names, or the live grant that already
/// covers the scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GrantOutcome {
    pub grant: Grant,
    /// `committed` when anything was written, `no-op` when the scope was already granted.
    pub outcome: super::settings::WriteOutcome,
    /// The request was a retry of an earlier one, answered with its grant.
    pub deduplicated: bool,
}

/// The fields of a new grant a caller supplies; the host adds the identity and the time.
pub(crate) struct NewGrant<'a> {
    pub module_id: &'a str,
    pub capability: &'a str,
    pub scope: GrantScope,
    pub actor: &'a str,
    pub request_id: &'a str,
}

/// The grants file of one user, in one directory. Holds no state of its own: every call reads the
/// file again.
#[derive(Clone, Debug)]
pub struct GrantsStore {
    dir: PathBuf,
}

impl GrantsStore {
    /// A store over `<dir>/grants.json`. Nothing is created until the first write.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(GRANTS_FILE)
    }

    /// Every grant and denial, optionally of one module.
    pub fn list(&self, module_id: Option<&str>) -> Result<GrantList, Error> {
        let document = self.load()?;
        let wanted = |id: &str| module_id.is_none_or(|module_id| module_id == id);
        Ok(GrantList {
            grants: document
                .grants
                .into_iter()
                .filter(|grant| wanted(&grant.module_id))
                .collect(),
            denials: document
                .denials
                .into_iter()
                .filter(|denial| wanted(&denial.module_id))
                .collect(),
        })
    }

    /// The grant one module's grant request made, by the request's identity.
    pub fn request(&self, module_id: &str, request_id: &str) -> Result<Option<Grant>, Error> {
        Ok(self
            .load()?
            .grants
            .into_iter()
            .find(|grant| grant.module_id == module_id && grant.request_id == request_id))
    }

    /// The live grant covering exactly this scope, and whether a denial of it is recorded.
    pub fn consent(
        &self,
        module_id: &str,
        capability: &str,
        scope: &GrantScope,
    ) -> Result<(Option<Grant>, bool), Error> {
        let document = self.load()?;
        let grant = document
            .grants
            .into_iter()
            .find(|grant| grant.is_live() && grant.covers(module_id, capability, scope));
        let denied = document
            .denials
            .iter()
            .any(|denial| denial.covers(module_id, capability, scope));
        Ok((grant, denied))
    }

    /// Record a grant. A retry of the same request returns the grant it made, whatever became of it
    /// since; the same request identity with another scope is a `conflict`. A live grant of the same
    /// scope is returned instead of a duplicate. A matching denial is cleared either way.
    pub(crate) fn grant(&self, request: NewGrant<'_>) -> Result<GrantOutcome, Error> {
        let _lock = self.lock()?;
        let mut document = self.load()?;
        let NewGrant {
            module_id,
            capability,
            scope,
            actor,
            request_id,
        } = request;
        if let Some(previous) = document
            .grants
            .iter()
            .find(|grant| grant.module_id == module_id && grant.request_id == request_id)
        {
            if !previous.covers(module_id, capability, &scope) {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    "request_id was already used with different input",
                ));
            }
            return Ok(GrantOutcome {
                grant: previous.clone(),
                outcome: super::settings::WriteOutcome::NoOp,
                deduplicated: true,
            });
        }
        let denials = document.denials.len();
        document
            .denials
            .retain(|denial| !denial.covers(module_id, capability, &scope));
        let cleared = document.denials.len() != denials;
        if let Some(live) = document
            .grants
            .iter()
            .find(|grant| grant.is_live() && grant.covers(module_id, capability, &scope))
            .cloned()
        {
            if cleared {
                self.save(&document)?;
            }
            return Ok(GrantOutcome {
                grant: live,
                outcome: if cleared {
                    super::settings::WriteOutcome::Committed
                } else {
                    super::settings::WriteOutcome::NoOp
                },
                deduplicated: false,
            });
        }
        document.make_room()?;
        let grant = Grant {
            grant_id: format!("grant-{}", uuid::Uuid::new_v4().simple()),
            module_id: module_id.to_owned(),
            capability: capability.to_owned(),
            kind: scope.kind(),
            scope,
            actor: actor.to_owned(),
            request_id: request_id.to_owned(),
            created_ms: now_ms(),
            last_used_ms: None,
            revoked: None,
        };
        document.grants.push(grant.clone());
        self.save(&document)?;
        Ok(GrantOutcome {
            grant,
            outcome: super::settings::WriteOutcome::Committed,
            deduplicated: false,
        })
    }

    /// Record a denial of one exact scope, or refresh the one already recorded.
    pub(crate) fn deny(
        &self,
        module_id: &str,
        capability: &str,
        scope: GrantScope,
        actor: &str,
    ) -> Result<Denial, Error> {
        let _lock = self.lock()?;
        let mut document = self.load()?;
        document
            .denials
            .retain(|denial| !denial.covers(module_id, capability, &scope));
        document.make_room()?;
        let denial = Denial {
            module_id: module_id.to_owned(),
            capability: capability.to_owned(),
            kind: scope.kind(),
            scope,
            actor: actor.to_owned(),
            ms: now_ms(),
        };
        document.denials.push(denial.clone());
        self.save(&document)?;
        Ok(denial)
    }

    /// Revoke one grant. Revoking a revoked grant changes nothing and returns it with `false`.
    pub(crate) fn revoke(&self, grant_id: &str, reason: &str) -> Result<(Grant, bool), Error> {
        let _lock = self.lock()?;
        let mut document = self.load()?;
        let grant = document
            .grants
            .iter_mut()
            .find(|grant| grant.grant_id == grant_id)
            .ok_or_else(|| validation(format!("unknown grant {grant_id}")))?;
        if !grant.is_live() {
            return Ok((grant.clone(), false));
        }
        grant.revoked = Some(Revocation {
            ms: now_ms(),
            reason: reason.to_owned(),
        });
        let revoked = grant.clone();
        self.save(&document)?;
        Ok((revoked, true))
    }

    /// Revoke every live grant `matches` selects and return them. A file with nothing to revoke is
    /// neither locked nor written, so a settings write on a fresh install creates no grants file.
    pub(crate) fn revoke_matching(
        &self,
        matches: impl Fn(&Grant) -> bool,
        reason: &str,
    ) -> Result<Vec<Grant>, Error> {
        let selected = |document: &Document| {
            document
                .grants
                .iter()
                .any(|grant| grant.is_live() && matches(grant))
        };
        if !selected(&self.load()?) {
            return Ok(Vec::new());
        }
        let _lock = self.lock()?;
        let mut document = self.load()?;
        let ms = now_ms();
        let mut revoked = Vec::new();
        for grant in &mut document.grants {
            if grant.is_live() && matches(grant) {
                grant.revoked = Some(Revocation {
                    ms,
                    reason: reason.to_owned(),
                });
                revoked.push(grant.clone());
            }
        }
        if !revoked.is_empty() {
            self.save(&document)?;
        }
        Ok(revoked)
    }

    /// Record that live work was admitted under these grants.
    pub(crate) fn touch(&self, grant_ids: &[String]) -> Result<(), Error> {
        if grant_ids.is_empty() {
            return Ok(());
        }
        let _lock = self.lock()?;
        let mut document = self.load()?;
        let ms = now_ms();
        for grant in &mut document.grants {
            if grant_ids.contains(&grant.grant_id) {
                grant.last_used_ms = Some(ms);
            }
        }
        self.save(&document)
    }

    fn lock(&self) -> Result<File, Error> {
        atomic::lock(&self.dir, GRANTS_LOCK)
    }

    /// The whole file, bounded and checked. An absent file is an empty document; a file of another
    /// format, or one that is not the current shape, is refused and left exactly as it is.
    fn load(&self) -> Result<Document, Error> {
        let path = self.path();
        let Some(bytes) = atomic::read(&path, MAX_GRANTS_BYTES)? else {
            return Ok(Document::empty());
        };
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| incompatible(&path, format!("not valid JSON ({error})")))?;
        match value.get("format").and_then(Value::as_u64) {
            Some(format) if format == u64::from(GRANTS_FORMAT) => {}
            Some(format) => {
                return Err(incompatible(
                    &path,
                    format!("grants format {format} is not supported"),
                ));
            }
            None => return Err(incompatible(&path, "no grants format marker")),
        }
        let document: Document = serde_json::from_value(value)
            .map_err(|error| incompatible(&path, format!("not a grants file ({error})")))?;
        let mismatched = document
            .grants
            .iter()
            .map(|grant| (grant.kind, grant.scope.kind()))
            .chain(
                document
                    .denials
                    .iter()
                    .map(|denial| (denial.kind, denial.scope.kind())),
            )
            .any(|(kind, scope)| kind != scope);
        if mismatched {
            return Err(incompatible(
                &path,
                "a record's scope does not match its kind",
            ));
        }
        if document.records() > MAX_GRANT_RECORDS {
            return Err(incompatible(
                &path,
                format!("it holds more than {MAX_GRANT_RECORDS} records"),
            ));
        }
        Ok(document)
    }

    fn save(&self, document: &Document) -> Result<(), Error> {
        let bytes = serde_json::to_vec_pretty(document)
            .map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))?;
        if bytes.len() as u64 > MAX_GRANTS_BYTES {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "grants would be {} bytes; the file is at most {MAX_GRANTS_BYTES}",
                    bytes.len()
                ),
            ));
        }
        atomic::replace(&self.dir, GRANTS_FILE, GRANTS_TEMPORARY, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::testing::{MODULE, temp};
    use serde_json::json;
    use std::fs;

    struct Fixture {
        root: PathBuf,
        store: GrantsStore,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = temp(name);
            Self {
                store: GrantsStore::new(root.join("modules")),
                root,
            }
        }

        fn file(&self) -> PathBuf {
            self.root.join("modules").join(GRANTS_FILE)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn file_scope(path: &str) -> GrantScope {
        GrantScope::File(FileScope { path: path.into() })
    }

    fn new_grant<'a>(scope: GrantScope, request_id: &'a str) -> NewGrant<'a> {
        NewGrant {
            module_id: MODULE,
            capability: "input",
            scope,
            actor: "permissions",
            request_id,
        }
    }

    #[test]
    fn scopes_parse_strictly_as_their_kind() {
        assert_eq!(
            GrantScope::parse(GrantKind::ReadUserFile, &json!({"path": "/a"})).unwrap(),
            file_scope("/a")
        );
        let download = GrantScope::parse(
            GrantKind::DownloadArtifact,
            &json!({"resource": "palette", "version": "1", "origin": "https://example.com"}),
        )
        .unwrap();
        assert_eq!(download.kind(), GrantKind::DownloadArtifact);
        for (kind, value, expected) in [
            (
                GrantKind::ReadUserFile,
                json!({"path": "/a", "extra": 1}),
                "unknown field `extra`",
            ),
            (
                GrantKind::DownloadArtifact,
                json!({"path": "/a"}),
                "a download-artifact scope is invalid",
            ),
            (
                GrantKind::RemoteImageRequest,
                json!({"profile_id": "p", "adapter": "a", "origin": "https://x", "data": "sample-grid-8", "asset_id": "nope"}),
                "invalid AssetId",
            ),
        ] {
            let error = GrantScope::parse(kind, &value).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Validation);
            assert!(error.detail.contains(expected), "{}", error.detail);
        }
    }

    #[test]
    fn a_retry_returns_its_grant_and_a_denial_is_cleared_by_a_grant() {
        let fixture = Fixture::new("grants-retry");
        assert_eq!(fixture.store.list(None).unwrap(), GrantList::default());
        assert!(!fixture.file().exists(), "reading creates nothing");
        let denial = fixture
            .store
            .deny(MODULE, "input", file_scope("/a"), "edit")
            .unwrap();
        assert_eq!(denial.kind, GrantKind::ReadUserFile);
        assert_eq!(
            fixture
                .store
                .consent(MODULE, "input", &file_scope("/a"))
                .unwrap(),
            (None, true)
        );
        let first = fixture
            .store
            .grant(new_grant(file_scope("/a"), "grant-1"))
            .unwrap();
        assert_eq!(
            first.outcome,
            super::super::settings::WriteOutcome::Committed
        );
        assert!(!first.deduplicated);
        assert!(first.grant.grant_id.starts_with("grant-"));
        assert_eq!(first.grant.actor, "permissions");
        let (live, denied) = fixture
            .store
            .consent(MODULE, "input", &file_scope("/a"))
            .unwrap();
        assert_eq!(live.as_ref(), Some(&first.grant));
        assert!(!denied, "the grant cleared the denial");
        let retry = fixture
            .store
            .grant(new_grant(file_scope("/a"), "grant-1"))
            .unwrap();
        assert!(retry.deduplicated);
        assert_eq!(retry.grant, first.grant);
        let error = fixture
            .store
            .grant(new_grant(file_scope("/b"), "grant-1"))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Conflict);
        // Another request for a scope already granted returns the live grant instead of a copy.
        let again = fixture
            .store
            .grant(new_grant(file_scope("/a"), "grant-2"))
            .unwrap();
        assert_eq!(again.outcome, super::super::settings::WriteOutcome::NoOp);
        assert_eq!(again.grant.grant_id, first.grant.grant_id);
        let (revoked, changed) = fixture
            .store
            .revoke(&first.grant.grant_id, "revoked by test")
            .unwrap();
        assert!(changed);
        assert_eq!(revoked.revoked.as_ref().unwrap().reason, "revoked by test");
        assert!(
            !fixture
                .store
                .revoke(&first.grant.grant_id, "again")
                .unwrap()
                .1
        );
        // A retry of the original request still names the grant it made, revoked as it now is.
        let retry = fixture
            .store
            .grant(new_grant(file_scope("/a"), "grant-1"))
            .unwrap();
        assert!(retry.deduplicated);
        assert!(!retry.grant.is_live());
        let listed = fixture.store.list(Some(MODULE)).unwrap();
        assert_eq!(listed.grants.len(), 1);
        assert!(
            fixture
                .store
                .list(Some("test.other"))
                .unwrap()
                .grants
                .is_empty()
        );
    }

    #[test]
    fn records_are_bounded_and_the_oldest_revoked_or_denied_is_pruned_first() {
        let fixture = Fixture::new("grants-bounds");
        let mut document = Document::empty();
        let live = |index: usize| Grant {
            grant_id: format!("grant-{index}"),
            module_id: MODULE.into(),
            capability: "input".into(),
            kind: GrantKind::ReadUserFile,
            scope: file_scope(&format!("/{index}")),
            actor: "permissions".into(),
            request_id: format!("request-{index}"),
            created_ms: 1,
            last_used_ms: None,
            revoked: None,
        };
        document.grants = (0..MAX_GRANT_RECORDS).map(live).collect();
        fs::create_dir_all(fixture.root.join("modules")).unwrap();
        fixture.store.save(&document).unwrap();
        let error = fixture
            .store
            .grant(new_grant(file_scope("/new"), "new"))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        let error = fixture
            .store
            .deny(MODULE, "input", file_scope("/new"), "edit")
            .unwrap_err();
        assert_eq!(
            error.kind,
            ErrorKind::ResourceLimit,
            "a denial is a record too"
        );
        // Two revoked grants: the one revoked first goes first.
        document.grants[10].revoked = Some(Revocation {
            ms: 20,
            reason: "later".into(),
        });
        document.grants[20].revoked = Some(Revocation {
            ms: 10,
            reason: "earlier".into(),
        });
        fixture.store.save(&document).unwrap();
        fixture
            .store
            .grant(new_grant(file_scope("/new"), "new"))
            .unwrap();
        let grants = fixture.store.list(None).unwrap().grants;
        assert_eq!(grants.len(), MAX_GRANT_RECORDS);
        assert!(grants.iter().all(|grant| grant.grant_id != "grant-20"));
        assert!(grants.iter().any(|grant| grant.grant_id == "grant-10"));
    }

    #[test]
    fn another_format_or_shape_is_refused_and_kept_unchanged() {
        let fixture = Fixture::new("grants-format");
        fs::create_dir_all(fixture.root.join("modules")).unwrap();
        for (name, contents) in [
            (
                "format",
                json!({"format": 2, "grants": [], "denials": []}).to_string(),
            ),
            ("marker", json!({"grants": []}).to_string()),
            ("json", "{not json".to_owned()),
            (
                "shape",
                json!({"format": 1, "grants": [], "denials": [], "extra": true}).to_string(),
            ),
            (
                "kind",
                json!({"format": 1, "grants": [], "denials": [{
                    "module_id": MODULE, "capability": "input", "kind": "download-artifact",
                    "scope": {"path": "/a"}, "actor": "edit", "ms": 1,
                }]})
                .to_string(),
            ),
        ] {
            fs::write(fixture.file(), &contents).unwrap();
            let error = fixture.store.list(None).unwrap_err();
            assert_eq!(
                error.kind,
                ErrorKind::Incompatible,
                "{name}: {}",
                error.detail
            );
            let error = fixture
                .store
                .grant(new_grant(file_scope("/a"), "a"))
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Incompatible, "{name}");
            let error = fixture
                .store
                .revoke_matching(|_| true, "reset")
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Incompatible, "{name}");
            assert_eq!(
                fs::read_to_string(fixture.file()).unwrap(),
                contents,
                "{name}"
            );
        }
    }

    #[test]
    fn revoking_nothing_creates_no_file_and_touch_records_use() {
        let fixture = Fixture::new("grants-touch");
        assert!(
            fixture
                .store
                .revoke_matching(|_| true, "reset")
                .unwrap()
                .is_empty()
        );
        assert!(!fixture.root.exists(), "nothing was created");
        let granted = fixture
            .store
            .grant(new_grant(file_scope("/a"), "a"))
            .unwrap()
            .grant;
        assert!(granted.last_used_ms.is_none());
        fixture
            .store
            .touch(std::slice::from_ref(&granted.grant_id))
            .unwrap();
        let listed = fixture.store.list(None).unwrap();
        assert!(listed.grants[0].last_used_ms.is_some());
        let revoked = fixture
            .store
            .revoke_matching(|grant| grant.capability == "input", "selection changed")
            .unwrap();
        assert_eq!(revoked.len(), 1);
        assert_eq!(
            revoked[0].revoked.as_ref().unwrap().reason,
            "selection changed"
        );
    }
}
