//! Permissions, capability jobs, task requirements and resources driven through the catalog owner
//! exactly as a client drives them, against an in-memory transport that counts every request,
//! isolated directories and an in-memory secret store. Nothing here opens a socket. These are the
//! one owner-level test of each capability behaviour; `proof_tests.rs` keeps the journey the proof
//! module adds to them.
use super::{
    descriptor::CapabilityKind,
    grants::{DENY, GRANT, GRANTS_FILE, LIST, REVOKE},
    host::{HostConfig, STATUS, TASK_PREFIX},
    resources::{INSTALL, INSTALLED_FILE, REMOVE, RESOURCE_LIST, STAGING_DIR, faults},
    secrets::MemorySecretStore,
    settings::{CLEAR_SECRET, CREATE_PROFILE, READ, REMOVE_PROFILE, RESET, SET, SET_SECRET},
    testing::{
        LifecycleModule, MODULE, MemoryTransport, Owner, PALETTE, Probe, SWATCH, TASK,
        lifecycle_descriptor, sha256_hex, temp,
    },
};
use crate::{
    ApiResponse, ClientId, EditorService, Error, LocalServer, ModuleDescriptor, ModuleRegistry,
    jobs::{JOB_CANCEL, JOB_READ},
};
use luxforge_testbase::Gate;
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::Ordering},
};

/// A catalog, a settings directory and a resource directory under one temporary root, and the
/// registry the owner is started with.
struct Fixture {
    root: PathBuf,
    secrets: Arc<MemorySecretStore>,
    transport: Arc<MemoryTransport>,
    probe: Arc<Probe>,
    descriptor: ModuleDescriptor,
    quota: u64,
}

impl Fixture {
    fn new(name: &str, transport: &Arc<MemoryTransport>) -> Self {
        let root = temp(name);
        fs::create_dir_all(&root).unwrap();
        Self {
            root,
            secrets: Arc::new(MemorySecretStore::new()),
            transport: transport.clone(),
            probe: Arc::new(Probe::default()),
            descriptor: lifecycle_descriptor(&served("/palette"), &served("/swatch")),
            quota: 1 << 20,
        }
    }

    fn catalog(&self) -> PathBuf {
        self.root.join("catalog.sqlite")
    }

    fn config(&self) -> PathBuf {
        self.root.join("config").join("modules")
    }

    fn resources(&self) -> PathBuf {
        self.root.join("data").join("modules").join("resources")
    }

    fn version_dir(&self, resource: &str) -> PathBuf {
        let version = &self
            .descriptor
            .resource(resource)
            .expect("a declared resource")
            .version;
        self.resources().join(MODULE).join(resource).join(version)
    }

    fn registry(&self) -> Arc<ModuleRegistry> {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(LifecycleModule::shared(
                self.descriptor.clone(),
                self.probe.clone(),
            ))
            .unwrap();
        Arc::new(registry)
    }

    fn host(&self) -> HostConfig {
        HostConfig {
            config_dir: Some(self.config()),
            resource_dir: Some(self.resources()),
            secrets: self.secrets.clone(),
            transport: self.transport.clone(),
            resource_quota_bytes: self.quota,
        }
    }

    fn start(&self) -> Owner {
        Owner::start(&self.catalog(), self.registry(), self.host())
    }

    /// Import the fixture photo before any owner runs, so a remote grant can name an asset.
    fn import_asset(&self) -> String {
        let photo =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
        let mut service = EditorService::open(&self.catalog()).unwrap();
        service.import(&photo).unwrap().asset.id.to_string()
    }

    /// No version of `resource` is installed and nothing is staged.
    fn assert_clean(&self, resource: &str, case: &str) {
        let version = self.version_dir(resource);
        assert!(
            !version.join(INSTALLED_FILE).exists(),
            "{case}: nothing is installed"
        );
        assert!(!version.exists(), "{case}: no version directory is left");
        assert!(
            !self.resources().join(STAGING_DIR).exists(),
            "{case}: no staging directory is left"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        faults::clear(&self.resources());
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// What the lifecycle tests ask of the owner besides the shared harness.
trait Lifecycle {
    /// Commit module-level settings at the current revision.
    fn set(&self, values: Value) -> Value;
    /// Grant a download of one resource as the permission client.
    fn grant_download(&self, fixture: &Fixture, capability: &str, resource: &str) -> Value;
    /// Grant the download of one resource, install it from its pinned URL on the fixture's
    /// transport, and wait.
    fn install_downloaded(&self, fixture: &Fixture, resource: &str) -> Value;
}

impl Lifecycle for Owner {
    fn set(&self, values: Value) -> Value {
        self.ok(
            SET,
            json!({"module_id": MODULE, "values": values, "mutation": self.mutation(MODULE)}),
        )
    }

    fn grant_download(&self, fixture: &Fixture, capability: &str, resource: &str) -> Value {
        self.ok_as(
            self.admin,
            GRANT,
            json!({
                "module_id": MODULE,
                "capability": capability,
                "scope": download_scope(fixture, resource),
            }),
        )
    }

    fn install_downloaded(&self, fixture: &Fixture, resource: &str) -> Value {
        let capability = fixture
            .descriptor
            .capabilities
            .iter()
            .find(|capability| {
                matches!(&capability.kind, CapabilityKind::DownloadArtifact { resource: id } if id == resource)
            })
            .expect("a download capability for the resource")
            .id
            .clone();
        self.grant_download(fixture, &capability, resource);
        let queued = self.ok(
            INSTALL,
            json!({"module_id": MODULE, "resource_id": resource}),
        );
        self.finished(&queued["job_id"])
    }
}

fn download_scope(fixture: &Fixture, resource: &str) -> Value {
    let declared = fixture.descriptor.resource(resource).unwrap();
    let origin = url::Url::parse(&declared.url)
        .unwrap()
        .origin()
        .ascii_serialization();
    json!({"resource": resource, "version": declared.version, "origin": origin})
}

/// The origin the fixture's resources are declared at. Only the in-memory transport answers it.
const SERVED: &str = "https://downloads.example";

/// The URL of `path` at [`SERVED`].
fn served(path: &str) -> String {
    format!("{SERVED}{path}")
}

/// A transport that answers `/palette` and `/swatch` with their pinned bytes.
fn serving() -> Arc<MemoryTransport> {
    MemoryTransport::new(|exchange, answer| match exchange.path() {
        "/palette" => answer.whole(200, PALETTE),
        "/swatch" => answer.whole(200, SWATCH),
        _ => answer.whole(404, b""),
    })
}

/// A transport whose `/palette` answers its status and four bytes, then waits at the returned gate,
/// shut, before the rest; `/swatch` answers at once.
fn stalling() -> (Arc<MemoryTransport>, Arc<Gate>) {
    let gate = Arc::new(Gate::new());
    gate.shut();
    let held = gate.clone();
    let transport = MemoryTransport::new(move |exchange, answer| match exchange.path() {
        "/palette" => {
            answer.status(200);
            answer.bytes(&PALETTE[..4]);
            held.pass();
            answer.bytes(&PALETTE[4..]);
        }
        _ => answer.whole(200, SWATCH),
    });
    (transport, gate)
}

#[test]
fn only_a_client_registered_with_permission_authority_can_grant() {
    let transport = serving();
    let fixture = Fixture::new("authority", &transport);
    let owner = fixture.start();
    assert_eq!(
        owner.ok_as(owner.edit, "session.state", json!({}))["authority"],
        json!("edit")
    );
    assert_eq!(
        owner.ok_as(owner.admin, "session.state", json!({}))["authority"],
        json!("permissions")
    );
    // Each call below is a new request: the helper gives each one a fresh request identity.
    let request = json!({
        "module_id": MODULE,
        "capability": "palette",
        "scope": download_scope(&fixture, "palette"),
    });
    let refused = owner.fail(GRANT, request.clone());
    assert_eq!(refused.code, "forbidden");
    assert_eq!(
        refused.message,
        "granting a permission needs permission authority"
    );
    // Every request is parsed before its handler runs, so a malformed one is refused as malformed
    // whoever sends it; it grants nothing either way.
    assert_eq!(owner.fail(GRANT, json!({})).code, "validation");
    let granted = owner.ok_as(owner.admin, GRANT, request.clone());
    assert_eq!(granted["outcome"], json!("committed"));
    assert_eq!(granted["deduplicated"], json!(false));
    assert_eq!(
        granted["grant"]["actor"],
        json!("test"),
        "the envelope's actor"
    );
    assert_eq!(granted["grant"]["kind"], json!("download-artifact"));
    assert!(
        granted["grant"]["grant_id"]
            .as_str()
            .unwrap()
            .starts_with("grant-")
    );
    // Denying and revoking reduce privilege, so an edit client may.
    owner.ok(
        DENY,
        json!({"module_id": MODULE, "capability": "swatches", "scope": download_scope(&fixture, "swatch")}),
    );
    let revoked = owner.ok(
        REVOKE,
        json!({"grant_id": granted["grant"]["grant_id"], "reason": "changed my mind"}),
    );
    assert_eq!(revoked["outcome"], json!("committed"));
    assert_eq!(
        revoked["grant"]["revoked"]["reason"],
        json!("changed my mind")
    );
    // Authority is forgotten on disconnect: the same identity is an edit client afterwards.
    owner.handle.disconnect(owner.admin);
    assert_eq!(
        owner.fail_as(owner.admin, GRANT, request.clone()).code,
        "forbidden"
    );
    // A loopback live-session client never has permission authority.
    let session_file = fixture.root.join("live-session.json");
    match LocalServer::start(owner.handle.clone(), &session_file) {
        Ok(live) => {
            let mut stream = TcpStream::connect(live.info().address).unwrap();
            let mut params = request;
            params["mutation"] = json!({"request_id": "live-grant", "actor": "live"});
            let line = json!({
                "id": "live-grant",
                "method": GRANT,
                "params": params,
                "token": live.info().token,
            });
            writeln!(stream, "{line}").unwrap();
            let mut answer = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut answer)
                .unwrap();
            let response: ApiResponse = serde_json::from_str(&answer).unwrap();
            assert_eq!(response.error.unwrap().code, "forbidden");
        }
        Err(error) if error.detail.contains("Operation not permitted") => {}
        Err(error) => panic!("cannot start the live session: {error}"),
    }
    assert_eq!(
        owner.methods(),
        [GRANT, DENY, REVOKE],
        "each change is announced once"
    );
    owner.stop();
}

#[test]
fn a_grant_scope_must_be_one_the_module_can_use_now_and_a_retry_returns_the_same_grant() {
    let transport = serving();
    let fixture = Fixture::new("scopes", &transport);
    let asset = fixture.import_asset();
    let owner = fixture.start();
    let grant = |capability: &str, scope: Value, request_id: &str| {
        owner.call(
            owner.admin,
            GRANT,
            json!({"module_id": MODULE, "capability": capability, "scope": scope, "mutation": {"request_id": request_id, "actor": "test"}}),
        )
    };
    let refusal = |response: ApiResponse| {
        let error = response.error.expect("refused");
        assert_eq!(error.code, "validation", "{}", error.message);
        error.message
    };
    // A download grant names the declared version from the origin of the pinned URL.
    let mut scope = download_scope(&fixture, "palette");
    scope["version"] = json!("2.0.0");
    assert!(refusal(grant("palette", scope, "download-1")).contains("version 1.0.0"));
    let mut scope = download_scope(&fixture, "palette");
    scope["origin"] = json!("https://example.com");
    assert!(refusal(grant("palette", scope, "download-2")).contains("origin of its pinned URL"));
    // A remote grant names an existing profile of the adapter, its endpoint's origin, the data
    // class and an asset of this catalog.
    let created = owner.ok(
        CREATE_PROFILE,
        json!({"module_id": MODULE, "adapter": "echo-adapter", "label": "Echo", "mutation": owner.mutation(MODULE)}),
    );
    let profile = created["profile"]["id"].as_str().unwrap().to_owned();
    owner.ok(
        SET,
        json!({"module_id": MODULE, "profile_id": profile, "values": {"endpoint": "http://127.0.0.1:9/echo"}, "mutation": owner.mutation(MODULE)}),
    );
    let remote = |profile: &str, origin: &str, asset: &str| json!({"profile_id": profile, "adapter": "echo-adapter", "origin": origin, "data": "sample-grid-8", "asset_id": asset});
    let granted = grant(
        "echo",
        remote(&profile, "http://127.0.0.1:9", &asset),
        "remote-1",
    );
    assert!(granted.error.is_none(), "{:?}", granted.error);
    assert!(
        refusal(grant(
            "echo",
            remote(&profile, "http://127.0.0.1:8", &asset),
            "remote-2"
        ))
        .contains("sends to http://127.0.0.1:9")
    );
    assert!(
        refusal(grant(
            "echo",
            remote("profile-missing", "http://127.0.0.1:9", &asset),
            "remote-3"
        ))
        .contains("unknown profile")
    );
    let stranger = crate::AssetId::new().to_string();
    assert!(
        refusal(grant(
            "echo",
            remote(&profile, "http://127.0.0.1:9", &stranger),
            "remote-4"
        ))
        .contains("is not in this catalog")
    );
    let mut wrong = remote(&profile, "http://127.0.0.1:9", &asset);
    wrong["adapter"] = json!("other-adapter");
    assert!(refusal(grant("echo", wrong, "remote-5")).contains("through adapter echo-adapter"));
    assert!(
        refusal(grant("missing", json!({}), "unknown")).contains("declares no capability missing")
    );
    let unknown = owner
        .call(
            owner.admin,
            GRANT,
            json!({"module_id": "test.missing", "capability": "echo", "scope": {}, "mutation": {"request_id": "x", "actor": "test"}}),
        )
        .error
        .unwrap();
    assert_eq!(unknown.message, "unknown module test.missing");
    let listed = owner.ok(LIST, json!({"module_id": MODULE}));
    assert_eq!(listed["grants"].as_array().unwrap().len(), 1);
    owner.stop();
}

#[test]
fn a_denial_is_reported_in_the_next_consent_error_and_a_grant_clears_it() {
    let transport = serving();
    let fixture = Fixture::new("denial", &transport);
    let owner = fixture.start();
    let install = json!({"module_id": MODULE, "resource_id": "palette"});
    let first = owner.fail(INSTALL, install.clone());
    assert_eq!(first.code, "consent-required");
    let consent = first.data.as_ref().unwrap()["consent"].clone();
    assert_eq!(consent["module_id"], json!(MODULE));
    assert_eq!(consent["capability"], json!("palette"));
    assert_eq!(consent["kind"], json!("download-artifact"));
    assert_eq!(consent["scope"], download_scope(&fixture, "palette"));
    assert_eq!(consent["denied"], json!(false));
    let disclosure = &consent["disclosure"];
    assert_eq!(disclosure["module"], json!("Capabilities test"));
    assert_eq!(disclosure["purpose"], json!("Install the tint palette."));
    assert_eq!(disclosure["destination"], json!(served("/palette")));
    assert_eq!(disclosure["data"], json!("Tint palette 1.0.0"));
    assert_eq!(disclosure["bytes"], json!(12));
    assert_eq!(
        disclosure["storage"],
        json!(fixture.version_dir("palette").display().to_string())
    );
    assert_eq!(disclosure["license"], json!("CC0-1.0"));
    assert_eq!(disclosure["cost"], json!("free"));
    owner.ok(
        DENY,
        json!({"module_id": MODULE, "capability": "palette", "scope": consent["scope"]}),
    );
    let again = owner.fail(INSTALL, install.clone());
    assert_eq!(again.code, "consent-required");
    assert_eq!(again.data.unwrap()["consent"]["denied"], json!(true));
    assert_eq!(
        owner.status(MODULE)["permissions"],
        json!({"live": 0, "revoked": 0, "denials": 1})
    );
    assert_eq!(
        transport.sends(),
        0,
        "nothing was downloaded without a grant"
    );
    assert_eq!(owner.handle.lane_threads(), 0, "nothing was queued");
    owner.ok_as(
        owner.admin,
        GRANT,
        json!({"module_id": MODULE, "capability": "palette", "scope": consent["scope"], "mutation": {"request_id": "allow", "actor": "test"}}),
    );
    assert!(
        owner.ok(LIST, json!({}))["denials"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the grant cleared the denial"
    );
    let queued = owner.ok(INSTALL, install);
    assert_eq!(owner.finished(&queued["job_id"])["status"], json!("ready"));
    owner.stop();
}

#[test]
fn revoking_a_grant_cancels_its_queued_and_running_jobs_and_nothing_else() {
    let (transport, release) = stalling();
    let fixture = Fixture::new("revoke", &transport);
    let owner = fixture.start();
    let palette = owner.grant_download(&fixture, "palette", "palette");
    let swatch = owner.grant_download(&fixture, "swatches", "swatch");
    let running = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    let queued = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "swatch"}),
    );
    assert_eq!(queued["status"], json!("queued"));
    owner.until(&running["job_id"], |job| {
        job["progress"]["fraction"]
            .as_f64()
            .is_some_and(|fraction| fraction > 0.0)
    });
    let revision = owner.revision(MODULE);
    // Revoking the queued job's grant cancels it at once.
    let revoked = owner.ok(REVOKE, json!({"grant_id": swatch["grant"]["grant_id"]}));
    assert_eq!(revoked["cancelled_jobs"], json!([queued["job_id"]]));
    let cancelled = owner.job(&queued["job_id"]);
    assert_eq!(cancelled["status"], json!("cancelled"));
    assert_eq!(
        cancelled["error"],
        json!({"code": "cancelled", "message": "permission revoked"})
    );
    // Revoking the running job's grant stops it at its next checkpoint, mid-transfer.
    let revoked = owner.ok(REVOKE, json!({"grant_id": palette["grant"]["grant_id"]}));
    assert_eq!(revoked["cancelled_jobs"], json!([running["job_id"]]));
    let stopped = owner.finished(&running["job_id"]);
    assert_eq!(stopped["status"], json!("cancelled"));
    assert_eq!(stopped["error"]["message"], json!("permission revoked"));
    release.open();
    fixture.assert_clean("palette", "revoked while running");
    fixture.assert_clean("swatch", "revoked while queued");
    // Everything else is as it was: the settings, and no grant besides these two.
    assert_eq!(owner.revision(MODULE), revision);
    let listed = owner.ok(LIST, json!({"module_id": MODULE}));
    let live: Vec<&Value> = listed["grants"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|grant| grant.get("revoked").is_none())
        .collect();
    assert!(live.is_empty(), "{listed}");
    // A revoked grant cannot be used: the next install asks for consent again.
    assert_eq!(
        owner
            .fail(
                INSTALL,
                json!({"module_id": MODULE, "resource_id": "palette"})
            )
            .code,
        "consent-required"
    );
    owner.stop();
}

#[test]
fn changing_what_a_grant_names_revokes_it() {
    let transport = serving();
    let fixture = Fixture::new("scope-change", &transport);
    let asset = fixture.import_asset();
    let owner = fixture.start();
    let grant = |capability: &str, scope: Value| {
        owner.ok_as(
            owner.admin,
            GRANT,
            json!({"module_id": MODULE, "capability": capability, "scope": scope}),
        )["grant"]["grant_id"]
            .clone()
    };
    let reason = |grant_id: &Value| {
        owner.ok(LIST, json!({}))["grants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|grant| &grant["grant_id"] == grant_id)
            .unwrap()["revoked"]["reason"]
            .clone()
    };
    let download = grant("palette", download_scope(&fixture, "palette"));
    // Changing another field revokes nothing.
    assert!(
        owner
            .set(json!({"strength": 0.75}))
            .get("revoked")
            .is_none()
    );
    let created = owner.ok(
        CREATE_PROFILE,
        json!({"module_id": MODULE, "adapter": "echo-adapter", "label": "Echo", "mutation": owner.mutation(MODULE)}),
    );
    let profile = created["profile"]["id"].as_str().unwrap().to_owned();
    let endpoint = |url: &str| {
        owner.ok(
            SET,
            json!({"module_id": MODULE, "profile_id": profile, "values": {"endpoint": url}, "mutation": owner.mutation(MODULE)}),
        )
    };
    endpoint("http://127.0.0.1:9/echo");
    let remote_scope = |origin: &str| json!({"profile_id": profile, "adapter": "echo-adapter", "origin": origin, "data": "sample-grid-8", "asset_id": asset});
    let first = grant("echo", remote_scope("http://127.0.0.1:9"));
    assert_eq!(
        endpoint("http://127.0.0.1:10/echo")["revoked"],
        json!([first])
    );
    assert_eq!(reason(&first), json!("endpoint changed"));
    let second = grant("echo", remote_scope("http://127.0.0.1:10"));
    let removed = owner.ok(
        REMOVE_PROFILE,
        json!({"module_id": MODULE, "profile_id": profile, "mutation": owner.mutation(MODULE)}),
    );
    assert_eq!(removed["revoked"], json!([second]));
    assert_eq!(reason(&second), json!("profile removed"));
    let created = owner.ok(
        CREATE_PROFILE,
        json!({"module_id": MODULE, "adapter": "echo-adapter", "label": "Echo 2", "mutation": owner.mutation(MODULE)}),
    );
    let profile2 = created["profile"]["id"].as_str().unwrap().to_owned();
    owner.ok(
        SET,
        json!({"module_id": MODULE, "profile_id": profile2, "values": {"endpoint": "http://127.0.0.1:9/echo"}, "mutation": owner.mutation(MODULE)}),
    );
    let other = grant(
        "echo",
        json!({"profile_id": profile2, "adapter": "echo-adapter", "origin": "http://127.0.0.1:9", "data": "sample-grid-8", "asset_id": asset}),
    );
    let reset = owner.ok(
        RESET,
        json!({"module_id": MODULE, "mutation": owner.mutation(MODULE)}),
    );
    assert_eq!(reset["revoked"], json!([other]));
    assert_eq!(reason(&other), json!("settings reset"));
    assert_eq!(
        reason(&download),
        Value::Null,
        "a download grant names a pinned resource, not a setting"
    );
    owner.stop();
}

#[test]
fn a_grants_file_of_another_format_is_refused_and_kept() {
    let transport = serving();
    let fixture = Fixture::new("grants-format", &transport);
    fs::create_dir_all(fixture.config()).unwrap();
    let contents = json!({"format": 2, "grants": [], "denials": []}).to_string();
    fs::write(fixture.config().join(GRANTS_FILE), &contents).unwrap();
    let owner = fixture.start();
    assert_eq!(owner.fail(LIST, json!({})).code, "incompatible");
    assert_eq!(
        owner
            .fail(
                INSTALL,
                json!({"module_id": MODULE, "resource_id": "palette"})
            )
            .code,
        "incompatible",
        "consent cannot be decided, so nothing is downloaded"
    );
    let refused = owner.fail_as(
        owner.admin,
        GRANT,
        json!({"module_id": MODULE, "capability": "palette", "scope": download_scope(&fixture, "palette"), "mutation": {"request_id": "g", "actor": "test"}}),
    );
    assert_eq!(refused.code, "incompatible");
    let status = owner.status(MODULE);
    assert_eq!(
        status["permissions"]["error"]["code"],
        json!("incompatible")
    );
    assert_eq!(
        fs::read_to_string(fixture.config().join(GRANTS_FILE)).unwrap(),
        contents
    );
    assert_eq!(transport.sends(), 0);
    owner.stop();
}

#[test]
fn a_task_lists_every_missing_requirement_before_anything_is_queued() {
    let transport = serving();
    let fixture = Fixture::new("requirements", &transport);
    let asset = fixture.import_asset();
    let owner = fixture.start();
    let created = owner.ok(
        CREATE_PROFILE,
        json!({"module_id": MODULE, "adapter": "echo-adapter", "label": "Echo", "mutation": owner.mutation(MODULE)}),
    );
    let profile = created["profile"]["id"].as_str().unwrap().to_owned();
    let refused = owner.fail(
        &format!("{TASK_PREFIX}{TASK}"),
        json!({"asset_id": asset, "profile_id": profile}),
    );
    assert_eq!(refused.code, "not-ready");
    assert_eq!(
        refused.data.unwrap()["requirements"],
        json!([
            {"kind": "profile", "id": profile, "state": "incomplete"},
            {"kind": "resource", "id": "palette", "state": "not-installed"},
        ])
    );
    assert!(
        refused.message.contains(&format!(
            "profile {profile} is incomplete, resource palette is not-installed"
        )),
        "{}",
        refused.message
    );
    assert_eq!(owner.handle.lane_threads(), 0, "nothing was queued");
    assert_eq!(transport.sends(), 0);
    owner.stop();
}

#[test]
fn status_reports_settings_resources_permissions_and_jobs() {
    let transport = serving();
    let fixture = Fixture::new("status", &transport);
    let owner = fixture.start();
    owner.set(json!({"label": "tint"}));
    let grant = owner.grant_download(&fixture, "palette", "palette");
    let installed = owner.finished(
        &owner.ok(
            INSTALL,
            json!({"module_id": MODULE, "resource_id": "palette"}),
        )["job_id"],
    );
    assert_eq!(installed["status"], json!("ready"));
    assert_eq!(
        installed["request_id"],
        json!(queued_request(&owner, INSTALL))
    );
    let status = owner.status(MODULE);
    assert_eq!(status["module_id"], json!(MODULE));
    assert!(status.get("activation").is_none());
    assert_eq!(status["settings"]["state"], json!("ready"));
    assert_eq!(status["settings"]["missing"], json!([]));
    assert_eq!(status["resources"][0]["state"], json!("installed"));
    assert_eq!(status["resources"][1]["state"], json!("not-installed"));
    // Status counts the module's grants and denials; the records are read through the list.
    assert_eq!(
        status["permissions"],
        json!({"live": 1, "revoked": 0, "denials": 0})
    );
    assert_eq!(
        owner.ok(LIST, json!({"module_id": MODULE}))["grants"][0]["grant_id"],
        grant["grant"]["grant_id"]
    );
    let jobs: Vec<&str> = status["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["kind"].as_str().unwrap())
        .collect();
    assert_eq!(jobs, ["install"]);
    // Each change a job made is announced under the request that started it.
    let methods: Vec<String> = owner
        .methods()
        .into_iter()
        .filter(|method| !method.starts_with("module.settings") && method != GRANT)
        .collect();
    assert_eq!(methods, [INSTALL]);
    owner.stop();
}

/// The request identity of the last request with this method, as the owner recorded it.
fn queued_request(owner: &Owner, method: &str) -> String {
    owner
        .events()
        .into_iter()
        .rev()
        .find(|(announced, _)| announced == method)
        .map(|(_, request_id)| request_id)
        .expect("the change was announced")
}

#[test]
fn a_download_installs_the_pinned_bytes_with_their_record() {
    let transport = serving();
    let fixture = Fixture::new("download", &transport);
    let owner = fixture.start();
    owner.grant_download(&fixture, "palette", "palette");
    let listed = owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}));
    assert_eq!(listed["resources"][0]["state"], json!("not-installed"));
    assert_eq!(listed["storage"]["used_bytes"], json!(0));
    assert_eq!(listed["storage"]["quota_bytes"], json!(fixture.quota));
    let queued = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    assert_eq!(queued["state"], json!("installing"));
    let joined = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    assert!(
        joined["job_id"] == queued["job_id"] || joined["state"] == json!("installed"),
        "a second install joins the first: {joined}"
    );
    let done = owner.finished(&queued["job_id"]);
    assert_eq!(done["status"], json!("ready"));
    assert_eq!(done["kind"], json!("install"));
    assert_eq!(done["resource_id"], json!("palette"));
    assert_eq!(done["progress"]["fraction"], json!(1.0));
    let path = fixture.version_dir("palette").join("palette");
    assert_eq!(done["result"]["path"], json!(path));
    assert_eq!(done["result"]["bytes"], json!(12));
    assert_eq!(done["result"]["sha256"], json!(sha256_hex(PALETTE)));
    assert_eq!(fs::read(&path).unwrap(), PALETTE);
    let marker: Value = serde_json::from_slice(
        &fs::read(fixture.version_dir("palette").join(INSTALLED_FILE)).unwrap(),
    )
    .unwrap();
    let installed_ms = marker["installed_ms"].clone();
    assert!(installed_ms.is_u64());
    assert_eq!(
        marker,
        json!({
            "format": 1, "module_id": MODULE, "resource_id": "palette", "version": "1.0.0",
            "url": served("/palette"), "sha256": sha256_hex(PALETTE), "bytes": 12,
            "license": "CC0-1.0", "provenance": "Generated for tests",
            "actor": "test", "installed_ms": installed_ms,
        })
    );
    let listed = owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}));
    let row = &listed["resources"][0];
    assert_eq!(row["state"], json!("installed"));
    assert_eq!(row["path"], json!(path));
    assert_eq!(row["installed_ms"], installed_ms);
    assert_eq!(listed["storage"]["used_bytes"], json!(12));
    assert_eq!(listed["storage"]["root"], json!(fixture.resources()));
    assert_eq!(
        owner.ok(
            INSTALL,
            json!({"module_id": MODULE, "resource_id": "palette"})
        ),
        json!({"module_id": MODULE, "resource_id": "palette", "state": "installed", "deduplicated": false}),
        "an installed resource answers at once"
    );
    assert_eq!(transport.sends(), 1, "the resource was downloaded once");
    assert!(!fixture.resources().join(STAGING_DIR).exists());
    owner.stop();
}

#[test]
fn every_failed_install_leaves_nothing_installed_and_nothing_staged() {
    let mode = Arc::new(Mutex::new("hash"));
    let serving_mode = mode.clone();
    let transport = MemoryTransport::new(move |_, answer| {
        let current = *serving_mode.lock().unwrap();
        match current {
            "hash" => answer.whole(200, b"twelve BYTES"),
            "short" => answer.whole(200, b"twelve"),
            "oversized" => answer.whole(200, b"twelve bytes and more"),
            // The transport's own refusal of a redirect to an origin the resource does not list;
            // the redirect policy itself is the transport's to test, in `luxforge-net`.
            "redirect" => answer.fail(Error::validation(
                "redirect refused: https://elsewhere.example is not an allowed origin",
            )),
            "error" => answer.whole(500, b""),
            _ => answer.whole(200, PALETTE),
        }
    });
    let fixture = Fixture::new("failures", &transport);
    let owner = fixture.start();
    let install = json!({"module_id": MODULE, "resource_id": "palette"});
    assert_eq!(
        owner.fail(INSTALL, install.clone()).code,
        "consent-required"
    );
    fixture.assert_clean("palette", "without a grant");
    owner.grant_download(&fixture, "palette", "palette");
    let root = fixture.resources();
    for (case, code, message) in [
        (
            "hash",
            "validation",
            "palette does not match its pinned hash",
        ),
        (
            "short",
            "validation",
            "palette is 6 bytes; it is pinned at 12 bytes",
        ),
        ("oversized", "resource-limit", "larger than"),
        ("redirect", "validation", "redirect refused"),
        ("error", "read-error", "answered HTTP 500 for palette"),
        ("refused", "validation", "is not a palette"),
        ("disk-full", "resource-limit", "disk full"),
        ("write", "read-error", "cannot write"),
    ] {
        *mode.lock().unwrap() = case;
        fixture
            .probe
            .refuse
            .store(case == "refused", Ordering::SeqCst);
        match case {
            "disk-full" => faults::inject(&root, 4, io::ErrorKind::StorageFull),
            "write" => faults::inject(&root, 0, io::ErrorKind::PermissionDenied),
            _ => {}
        }
        let queued = owner.ok(INSTALL, install.clone());
        let failed = owner.finished(&queued["job_id"]);
        faults::clear(&root);
        assert_eq!(failed["status"], json!("failed"), "{case}: {failed}");
        assert_eq!(failed["error"]["code"], json!(code), "{case}: {failed}");
        assert!(
            failed["error"]["message"]
                .as_str()
                .unwrap()
                .contains(message),
            "{case}: {failed}"
        );
        fixture.assert_clean("palette", case);
        let row = &owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}))["resources"][0];
        assert_eq!(row["state"], json!("failed"), "{case}");
        assert_eq!(row["error"]["code"], json!(code), "{case}");
    }
    // The failures announced nothing; only the grant was.
    assert_eq!(owner.methods(), [GRANT]);
    // The same resource installs once the transport sends the pinned bytes.
    *mode.lock().unwrap() = "good";
    fixture.probe.refuse.store(false, Ordering::SeqCst);
    let queued = owner.ok(INSTALL, install);
    assert_eq!(owner.finished(&queued["job_id"])["status"], json!("ready"));
    owner.stop();
}

#[test]
fn a_download_cancelled_mid_transfer_stops_promptly_and_leaves_nothing() {
    let (transport, release) = stalling();
    let fixture = Fixture::new("cancel-download", &transport);
    let owner = fixture.start();
    owner.grant_download(&fixture, "palette", "palette");
    let queued = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    let running = owner.until(&queued["job_id"], |job| {
        job["progress"]["fraction"]
            .as_f64()
            .is_some_and(|fraction| fraction > 0.0)
    });
    assert_eq!(running["status"], json!("running"));
    assert_eq!(running["progress"]["message"], json!("downloading"));
    // The transport still holds the rest of the body at its gate, so the job ended mid-transfer at
    // its cancellation, not when the transfer could finish.
    owner.ok(JOB_CANCEL, json!({"job_id": queued["job_id"]}));
    let cancelled = owner.finished(&queued["job_id"]);
    assert!(release.holding(), "the rest of the body is still held");
    release.open();
    assert_eq!(cancelled["status"], json!("cancelled"));
    assert_eq!(
        cancelled["error"],
        json!({"code": "cancelled", "message": "the job was cancelled"})
    );
    fixture.assert_clean("palette", "cancelled");
    assert_eq!(
        owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}))["resources"][0]["state"],
        json!("not-installed")
    );
    owner.stop();
}

#[test]
fn an_install_beyond_the_quota_is_refused_before_it_is_queued() {
    let transport = serving();
    let mut fixture = Fixture::new("quota", &transport);
    fixture.quota = PALETTE.len() as u64 - 1;
    let owner = fixture.start();
    owner.grant_download(&fixture, "palette", "palette");
    let refused = owner.fail(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    assert_eq!(refused.code, "resource-limit");
    assert!(
        refused.message.contains("would exceed the resource quota"),
        "{}",
        refused.message
    );
    assert_eq!(owner.handle.lane_threads(), 0, "nothing was queued");
    assert_eq!(transport.sends(), 0);
    fixture.assert_clean("palette", "over quota");
    owner.stop();
}

#[test]
fn what_a_crash_leaves_is_not_installed_and_the_next_install_removes_it() {
    let transport = serving();
    let fixture = Fixture::new("crash", &transport);
    let stale = fixture.resources().join(STAGING_DIR).join("job-stale");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("palette"), &PALETTE[..6]).unwrap();
    // A crash between the move into place and the marker leaves a version without installed.json.
    fs::create_dir_all(fixture.version_dir("palette")).unwrap();
    fs::write(fixture.version_dir("palette").join("palette"), PALETTE).unwrap();
    let owner = fixture.start();
    assert_eq!(
        owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}))["resources"][0]["state"],
        json!("not-installed")
    );
    let done = owner.install_downloaded(&fixture, "palette");
    assert_eq!(done["status"], json!("ready"));
    assert!(!stale.exists(), "the stale staging directory was removed");
    assert!(!fixture.resources().join(STAGING_DIR).exists());
    assert!(fixture.version_dir("palette").join(INSTALLED_FILE).exists());
    owner.stop();
}

#[test]
fn an_install_takes_only_the_pinned_url_and_refuses_a_caller_path() {
    let transport = serving();
    let fixture = Fixture::new("pinned-only", &transport);
    let owner = fixture.start();
    // A file holding exactly the pinned bytes is still not a source: no client names a path the
    // owner reads.
    let path = fixture.root.join("palette.local");
    fs::write(&path, PALETTE).unwrap();
    for client in [owner.edit, owner.admin] {
        let refused = owner.fail_as(
            client,
            INSTALL,
            json!({"module_id": MODULE, "resource_id": "palette", "source": {"kind": "file", "path": path}}),
        );
        assert_eq!(refused.code, "validation", "{}", refused.message);
        assert!(
            refused.message.contains("unknown variant `file`"),
            "{}",
            refused.message
        );
    }
    let refused = owner.fail(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette", "source": {"kind": "download", "path": path}}),
    );
    assert_eq!(refused.code, "validation", "{}", refused.message);
    assert!(
        refused.message.contains("unknown field `path`"),
        "{}",
        refused.message
    );
    assert_eq!(owner.handle.lane_threads(), 0, "nothing was queued");
    fixture.assert_clean("palette", "a caller path");
    // The declared URL, named or by default, is the one source, under its grant.
    assert_eq!(
        owner
            .fail(
                INSTALL,
                json!({"module_id": MODULE, "resource_id": "palette", "source": {"kind": "download"}}),
            )
            .code,
        "consent-required"
    );
    owner.grant_download(&fixture, "palette", "palette");
    let queued = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette", "source": {"kind": "download"}}),
    );
    assert_eq!(owner.finished(&queued["job_id"])["status"], json!("ready"));
    assert_eq!(transport.sends(), 1);
    owner.stop();
}

#[test]
fn removing_a_resource_deletes_only_the_resource() {
    let transport = serving();
    let fixture = Fixture::new("remove", &transport);
    let owner = fixture.start();
    owner.install_downloaded(&fixture, "palette");
    owner.install_downloaded(&fixture, "swatch");
    let catalog = fs::read(fixture.catalog()).unwrap();
    // Removing one resource leaves the other installed.
    let swatch = owner.ok(
        REMOVE,
        json!({"module_id": MODULE, "resource_id": "swatch"}),
    );
    assert_eq!(owner.finished(&swatch["job_id"])["status"], json!("ready"));
    assert!(!fixture.version_dir("swatch").exists());
    assert!(fixture.version_dir("palette").join(INSTALLED_FILE).exists());
    let removal = owner.ok(
        REMOVE,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    let removed = owner.finished(&removal["job_id"]);
    assert_eq!(removed["status"], json!("ready"));
    assert_eq!(
        removed["result"],
        json!({"resource_id": "palette", "version": "1.0.0", "removed": true})
    );
    assert!(!fixture.version_dir("palette").exists());
    assert!(
        !fixture.resources().join(MODULE).exists(),
        "emptied parents are removed"
    );
    assert_eq!(
        owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}))["resources"][0]["state"],
        json!("not-installed")
    );
    assert_eq!(
        owner.ok(
            REMOVE,
            json!({"module_id": MODULE, "resource_id": "palette"})
        ),
        json!({"module_id": MODULE, "resource_id": "palette", "state": "not-installed", "deduplicated": false}),
        "removing what is not installed queues nothing"
    );
    assert_eq!(
        fs::read(fixture.catalog()).unwrap(),
        catalog,
        "the catalog is untouched"
    );
    let methods: Vec<String> = owner
        .methods()
        .into_iter()
        .filter(|method| method != GRANT)
        .collect();
    // Each removal is announced once, when the resource is gone.
    assert_eq!(methods, [INSTALL, INSTALL, REMOVE, REMOVE]);
    owner.stop();
}

/// The one test of inert discovery: starting or reopening an owner, listing and describing the
/// module, and reading its status, lists and jobs start no lane, send nothing, ask the secret store
/// nothing until a status reads a secret's presence, and create no directory.
#[test]
fn discovery_status_and_reopen_start_no_lane_reach_no_network_and_create_nothing() {
    let transport = serving();
    let fixture = Fixture::new("inert", &transport);
    let declared = serde_json::to_value(&fixture.descriptor).unwrap();
    let find = |modules: &Value| {
        modules
            .as_array()
            .unwrap()
            .iter()
            .find(|module| module["id"] == json!(MODULE))
            .expect("the module is listed")
            .clone()
    };
    for round in 0..2 {
        let asked = fixture.secrets.calls().total();
        let owner = fixture.start();
        let listed = owner.ok("module.list", json!({}));
        assert_eq!(
            find(&listed["modules"]),
            declared,
            "exactly the declarations"
        );
        let schema = owner.ok("schema.list", json!({}));
        assert_eq!(find(&schema["modules"]), declared);
        for method in [
            READ,
            SET,
            SET_SECRET,
            CLEAR_SECRET,
            RESET,
            CREATE_PROFILE,
            REMOVE_PROFILE,
            GRANT,
            DENY,
            REVOKE,
            LIST,
            STATUS,
            RESOURCE_LIST,
            INSTALL,
            REMOVE,
            JOB_READ,
            JOB_CANCEL,
        ] {
            assert!(
                schema["methods"].get(method).is_some(),
                "{method} is discoverable"
            );
        }
        let task = &schema["methods"][&format!("{TASK_PREFIX}{TASK}")];
        assert_eq!(task["mutates"], json!(true), "the request queues a job");
        assert_eq!(task["mutation"], json!("request"));
        assert_eq!(
            task["required"],
            json!(["mutation", "asset_id", "profile_id"])
        );
        assert_eq!(task["optional"], json!({"gain": "test"}));
        assert!(
            task["notes"]
                .as_str()
                .unwrap()
                .contains("{job_id, status, deduplicated}")
        );
        assert_eq!(
            fixture.secrets.calls().total(),
            asked,
            "round {round}: opening and discovery never ask the secret store"
        );
        let status = owner.status(MODULE);
        assert_eq!(status["settings"]["state"], json!("incomplete"));
        assert_eq!(status["settings"]["missing"], json!(["label"]));
        assert_eq!(status["jobs"], json!([]));
        owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}));
        owner.ok(LIST, json!({}));
        assert_eq!(
            owner
                .fail(JOB_READ, json!({"job_id": crate::JobId::new()}))
                .code,
            "validation"
        );
        assert_eq!(owner.handle.lane_threads(), 0, "round {round}");
        owner.stop();
    }
    assert_eq!(transport.sends(), 0, "nothing was sent");
    assert!(
        !fixture.root.join("config").exists(),
        "no settings directory"
    );
    assert!(!fixture.root.join("data").exists(), "no resource directory");
}

/// Every capability family that carries the `{request_id, actor}` envelope — permissions,
/// resources and capability jobs — answers a retry from the owner's request table: the
/// first answer comes back marked `deduplicated`, nothing runs or is queued again and no event is
/// recorded; the same `request_id` with other input is a conflict. Each request's own work is let
/// finish before its retry, so an event its job announces is never mistaken for the retry's.
#[test]
fn a_retry_of_every_capability_family_returns_the_first_answer_and_records_no_event() {
    let transport = serving();
    let fixture = Fixture::new("retries", &transport);
    let owner = fixture.start();
    let envelope = |request_id: &str| json!({"request_id": request_id, "actor": "test"});
    let twice = |client: ClientId, method: &str, params: Value| -> Value {
        let first = owner.ok_as(client, method, params.clone());
        if let Some(job_id) = first.get("job_id") {
            owner.finished(job_id);
        }
        let announced = owner.events().len();
        let retry = owner.ok_as(client, method, params);
        assert_eq!(first["deduplicated"], json!(false), "{method}");
        assert_eq!(retry["deduplicated"], json!(true), "{method}");
        let mut original = retry.clone();
        original["deduplicated"] = json!(false);
        assert_eq!(original, first, "{method}: the retry is the first answer");
        assert_eq!(
            owner.events().len(),
            announced,
            "{method}: the retry records no event"
        );
        first
    };
    let jobs = |kind: &str| {
        owner.status(MODULE)["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|job| job["kind"] == json!(kind))
            .count()
    };

    let granted = twice(
        owner.admin,
        GRANT,
        json!({"module_id": MODULE, "capability": "palette", "scope": download_scope(&fixture, "palette"), "mutation": envelope("grant-1")}),
    );
    assert_eq!(granted["outcome"], json!("committed"));
    assert_eq!(
        owner.ok(LIST, json!({"module_id": MODULE}))["grants"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "one grant"
    );
    let conflict = owner.fail_as(
        owner.admin,
        GRANT,
        json!({"module_id": MODULE, "capability": "swatches", "scope": download_scope(&fixture, "swatch"), "mutation": envelope("grant-1")}),
    );
    assert_eq!(conflict.code, "conflict");
    twice(
        owner.edit,
        DENY,
        json!({"module_id": MODULE, "capability": "swatches", "scope": download_scope(&fixture, "swatch"), "mutation": envelope("deny-1")}),
    );
    let installed = twice(
        owner.edit,
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette", "mutation": envelope("install-1")}),
    );
    assert_eq!(owner.job(&installed["job_id"])["status"], json!("ready"));
    assert_eq!(jobs("install"), 1, "the retry queued no second install");
    // A cancel carries no envelope because it converges: sent again, it answers the same job and
    // records nothing.
    let announced = owner.events().len();
    let cancelled = owner.ok(JOB_CANCEL, json!({"job_id": installed["job_id"]}));
    assert_eq!(
        cancelled["status"],
        json!("ready"),
        "a finished job is unchanged"
    );
    assert_eq!(
        owner.ok(JOB_CANCEL, json!({"job_id": installed["job_id"]})),
        cancelled,
        "a repeated cancel answers the same job"
    );
    assert_eq!(
        owner.events().len(),
        announced,
        "a cancel of a finished job records nothing"
    );
    let removed = twice(
        owner.edit,
        REMOVE,
        json!({"module_id": MODULE, "resource_id": "palette", "mutation": envelope("remove-1")}),
    );
    assert_eq!(owner.job(&removed["job_id"])["status"], json!("ready"));
    assert_eq!(jobs("remove"), 1, "the retry queued no second removal");
    let revoked = twice(
        owner.edit,
        REVOKE,
        json!({"grant_id": granted["grant"]["grant_id"], "mutation": envelope("revoke-1")}),
    );
    assert_eq!(revoked["outcome"], json!("committed"));
    owner.stop();
}
