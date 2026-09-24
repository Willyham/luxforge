//! Permissions, activation, capability jobs and resources driven through the catalog owner exactly
//! as a client drives them, against loopback servers, isolated directories, an in-memory secret
//! store and a network that counts every lookup and connection. Nothing here leaves the machine.
use super::{
    grants::{DENY, GRANT, GRANTS_FILE, LIST, REVOKE},
    host::{ACTIVATE, DEACTIVATE, HostConfig, STATUS},
    jobs::{JOB_CANCEL, JOB_READ, LANE_QUEUE},
    resources::{INSTALL, INSTALLED_FILE, REMOVE, RESOURCE_LIST, STAGING_DIR, faults},
    secrets::MemorySecretStore,
    settings::{CREATE_PROFILE, READ, REMOVE_PROFILE, RESET, SET, SET_SECRET},
    testing::{
        CountingNet, LifecycleModule, MODULE, PALETTE, Probe, SWATCH, Server, enveloped,
        lane_descriptor, lifecycle_descriptor, reply, sha256_hex, temp,
    },
    transport::{TlsTrust, TransportConfig},
};
use crate::{
    ApiFailure, ApiRequest, ApiResponse, ClientAuthority, ClientId, EditorService, LocalServer,
    ModuleDescriptor, ModuleRegistry, OwnerHandle,
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    fs,
    io::{self, BufRead, BufReader, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// How many lane-test modules each fixture registers: enough to fill the module lane and one more.
const LANES: usize = LANE_QUEUE + 2;

/// A catalog, a settings directory and a resource directory under one temporary root, and the
/// registry the owner is started with.
struct Fixture {
    root: PathBuf,
    secrets: Arc<MemorySecretStore>,
    net: Arc<CountingNet>,
    probe: Arc<Probe>,
    lanes: Vec<Arc<Probe>>,
    descriptor: ModuleDescriptor,
    quota: u64,
}

impl Fixture {
    fn new(name: &str, server: &Server) -> Self {
        let root = temp(name);
        fs::create_dir_all(&root).unwrap();
        Self {
            root,
            secrets: Arc::new(MemorySecretStore::new()),
            net: Arc::new(CountingNet::default()),
            probe: Arc::new(Probe::default()),
            lanes: (0..LANES).map(|_| Arc::new(Probe::default())).collect(),
            descriptor: lifecycle_descriptor(&server.url("/palette"), &server.url("/swatch")),
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
        for (index, probe) in self.lanes.iter().enumerate() {
            registry
                .register(LifecycleModule::shared(
                    lane_descriptor(index),
                    probe.clone(),
                ))
                .unwrap();
        }
        Arc::new(registry)
    }

    fn host(&self) -> HostConfig {
        HostConfig {
            config_dir: Some(self.config()),
            resource_dir: Some(self.resources()),
            secrets: self.secrets.clone(),
            transport: TransportConfig {
                trust: TlsTrust::Roots(Vec::new()),
                resolver: self.net.clone(),
                connector: self.net.clone(),
            },
            resource_quota_bytes: self.quota,
        }
    }

    fn start(&self) -> Owner {
        let (handle, join) =
            OwnerHandle::start_with_host(&self.catalog(), self.registry(), self.host()).unwrap();
        let edit = handle.register();
        let admin = handle.register_with(ClientAuthority::Permissions);
        Owner {
            handle,
            join: Some(join),
            edit,
            admin,
            next: Cell::new(0),
            observed: RefCell::new(Vec::new()),
        }
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

/// A running owner with an edit client and a permission client, recording every response.
struct Owner {
    handle: OwnerHandle,
    join: Option<JoinHandle<()>>,
    edit: ClientId,
    admin: ClientId,
    next: Cell<u64>,
    observed: RefCell<Vec<String>>,
}

impl Owner {
    fn call(&self, client: ClientId, method: &str, params: Value) -> ApiResponse {
        let id = format!("request-{}", self.next.get());
        self.next.set(self.next.get() + 1);
        let params = enveloped(method, params, &id);
        let response = self
            .handle
            .call(
                client,
                ApiRequest {
                    id,
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered");
        self.observed
            .borrow_mut()
            .push(serde_json::to_string(&response).unwrap());
        response
    }

    fn ok_as(&self, client: ClientId, method: &str, params: Value) -> Value {
        let response = self.call(client, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        self.ok_as(self.edit, method, params)
    }

    fn fail_as(&self, client: ClientId, method: &str, params: Value) -> ApiFailure {
        self.call(client, method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to fail"))
    }

    fn fail(&self, method: &str, params: Value) -> ApiFailure {
        self.fail_as(self.edit, method, params)
    }

    fn revision(&self) -> u64 {
        self.ok(READ, json!({"module_id": MODULE}))["revision"]
            .as_u64()
            .unwrap()
    }

    /// Commit module-level settings at the current revision.
    fn set(&self, values: Value) -> Value {
        let revision = self.revision();
        self.ok(
            SET,
            json!({"module_id": MODULE, "values": values, "mutation": mutation(revision)}),
        )
    }

    fn job(&self, job_id: &Value) -> Value {
        self.ok(JOB_READ, json!({"job_id": job_id}))
    }

    /// Wait for a capability job to finish and return its record.
    fn finished(&self, job_id: &Value) -> Value {
        self.until(job_id, |job| {
            !matches!(job["status"].as_str(), Some("queued" | "running"))
        })
    }

    fn until(&self, job_id: &Value, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let job = self.job(job_id);
            if done(&job) {
                return job;
            }
            assert!(Instant::now() < deadline, "job never got there: {job}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn status(&self, module_id: &str) -> Value {
        self.ok(STATUS, json!({"module_id": module_id}))
    }

    fn events(&self) -> Vec<(String, String)> {
        self.ok("events.since", json!({"after": 0}))["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                (
                    event["method"].as_str().unwrap().to_owned(),
                    event["request_id"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// Grant a download of one resource as the permission client.
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

    /// Install one resource from a local file holding exactly its pinned bytes, and wait.
    fn install_from_file(&self, fixture: &Fixture, resource: &str, bytes: &[u8]) -> Value {
        let path = fixture.root.join(format!("{resource}.local"));
        fs::write(&path, bytes).unwrap();
        let queued = self.ok(
            INSTALL,
            json!({"module_id": MODULE, "resource_id": resource, "source": {"kind": "file", "path": path}}),
        );
        self.finished(&queued["job_id"])
    }

    fn stop(mut self) {
        self.handle.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            self.handle.stop();
            let _ = join.join();
        }
    }
}

fn mutation(revision: u64) -> Value {
    json!({
        "expected_revision": revision,
        "request_id": format!("settings-{}", uuid::Uuid::new_v4().simple()),
        "actor": "test",
    })
}

fn download_scope(fixture: &Fixture, resource: &str) -> Value {
    let declared = fixture.descriptor.resource(resource).unwrap();
    let origin = url::Url::parse(&declared.url)
        .unwrap()
        .origin()
        .ascii_serialization();
    json!({"resource": resource, "version": declared.version, "origin": origin})
}

/// A server that answers `/palette` and `/swatch` with their pinned bytes.
fn serving() -> Server {
    Server::start(|path, stream| match path {
        "/palette" => reply(stream, "200 OK", "", PALETTE),
        "/swatch" => reply(stream, "200 OK", "", SWATCH),
        _ => reply(stream, "404 Not Found", "", b""),
    })
}

/// A server whose `/palette` sends its head and four bytes, then waits for `release` before the
/// rest; `/swatch` answers at once.
fn stalling(release: Arc<AtomicBool>) -> Server {
    Server::start(move |path, stream| match path {
        "/palette" => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                PALETTE.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&PALETTE[..4]);
            let _ = stream.flush();
            let deadline = Instant::now() + Duration::from_secs(20);
            while !release.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            let _ = stream.write_all(&PALETTE[4..]);
        }
        _ => reply(stream, "200 OK", "", SWATCH),
    })
}

/// The latest release job in a module status. The status names it as the activation's job only
/// while it runs, and a release is quick, so the job list is where a test finds it.
fn release_job(status: &Value) -> Value {
    status["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|job| job["kind"] == json!("deactivate"))
        .expect("a release job")["job_id"]
        .clone()
}

/// Wait until `condition` holds, for a probe flag set on a worker.
fn eventually(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "{what} never happened");
        thread::sleep(Duration::from_millis(2));
    }
}

/// Every file under `root`, read whole.
fn files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(bytes) = fs::read(&path) {
                found.push((path, bytes));
            }
        }
    }
    found
}

#[test]
fn only_a_client_registered_with_permission_authority_can_grant() {
    let server = serving();
    let fixture = Fixture::new("authority", &server);
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
    let methods: Vec<String> = owner
        .events()
        .into_iter()
        .map(|(method, _)| method)
        .collect();
    assert_eq!(
        methods,
        [GRANT, DENY, REVOKE],
        "each change is announced once"
    );
    owner.stop();
}

#[test]
fn a_grant_scope_must_be_one_the_module_can_use_now_and_a_retry_returns_the_same_grant() {
    let server = serving();
    let fixture = Fixture::new("scopes", &server);
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
        json!({"module_id": MODULE, "adapter": "echo-adapter", "label": "Echo", "mutation": mutation(owner.revision())}),
    );
    let profile = created["profile"]["id"].as_str().unwrap().to_owned();
    owner.ok(
        SET,
        json!({"module_id": MODULE, "profile_id": profile, "values": {"endpoint": "http://127.0.0.1:9/echo"}, "mutation": mutation(owner.revision())}),
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
    let server = serving();
    let fixture = Fixture::new("denial", &server);
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
    assert_eq!(disclosure["destination"], json!(server.url("/palette")));
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
    assert_eq!(server.hits(), 0, "nothing was downloaded without a grant");
    assert_eq!(owner.handle.capability_threads(), 0, "nothing was queued");
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
    assert_eq!(
        owner.finished(&queued["job_id"])["status"],
        json!("succeeded")
    );
    owner.stop();
}

#[test]
fn revoking_a_grant_cancels_its_queued_and_running_jobs_and_nothing_else() {
    let release = Arc::new(AtomicBool::new(false));
    let server = stalling(release.clone());
    let fixture = Fixture::new("revoke", &server);
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
    let revision = owner.revision();
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
    release.store(true, Ordering::SeqCst);
    fixture.assert_clean("palette", "revoked while running");
    fixture.assert_clean("swatch", "revoked while queued");
    // Everything else is as it was: the settings, and no grant besides these two.
    assert_eq!(owner.revision(), revision);
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
    let server = serving();
    let fixture = Fixture::new("scope-change", &server);
    let asset = fixture.import_asset();
    let owner = fixture.start();
    let grant = |capability: &str, scope: Value| {
        owner.ok_as(
            owner.admin,
            GRANT,
            json!({"module_id": MODULE, "capability": capability, "scope": scope, "mutation": {"request_id": uuid::Uuid::new_v4().to_string(), "actor": "test"}}),
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
        json!({"module_id": MODULE, "adapter": "echo-adapter", "label": "Echo", "mutation": mutation(owner.revision())}),
    );
    let profile = created["profile"]["id"].as_str().unwrap().to_owned();
    let endpoint = |url: &str| {
        owner.ok(
            SET,
            json!({"module_id": MODULE, "profile_id": profile, "values": {"endpoint": url}, "mutation": mutation(owner.revision())}),
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
        json!({"module_id": MODULE, "profile_id": profile, "mutation": mutation(owner.revision())}),
    );
    assert_eq!(removed["revoked"], json!([second]));
    assert_eq!(reason(&second), json!("profile removed"));
    let created = owner.ok(
        CREATE_PROFILE,
        json!({"module_id": MODULE, "adapter": "echo-adapter", "label": "Echo 2", "mutation": mutation(owner.revision())}),
    );
    let profile2 = created["profile"]["id"].as_str().unwrap().to_owned();
    owner.ok(
        SET,
        json!({"module_id": MODULE, "profile_id": profile2, "values": {"endpoint": "http://127.0.0.1:9/echo"}, "mutation": mutation(owner.revision())}),
    );
    let other = grant(
        "echo",
        json!({"profile_id": profile2, "adapter": "echo-adapter", "origin": "http://127.0.0.1:9", "data": "sample-grid-8", "asset_id": asset}),
    );
    let reset = owner.ok(
        RESET,
        json!({"module_id": MODULE, "mutation": mutation(owner.revision())}),
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
    let server = serving();
    let fixture = Fixture::new("grants-format", &server);
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
    assert_eq!(server.hits(), 0);
    owner.stop();
}

#[test]
fn the_module_lane_runs_one_holds_four_and_refuses_the_sixth() {
    let server = serving();
    let fixture = Fixture::new("lane", &server);
    for probe in &fixture.lanes {
        probe.hold.store(true, Ordering::SeqCst);
    }
    let owner = fixture.start();
    let activate = |index: usize| {
        owner.call(
            owner.edit,
            ACTIVATE,
            json!({"module_id": format!("test.lane{index}")}),
        )
    };
    let first = activate(0).result.unwrap();
    assert_eq!(first["activation"], json!("activating"));
    eventually("the first activation runs", || {
        fixture.lanes[0].running.load(Ordering::SeqCst)
    });
    let waiting: Vec<Value> = (1..=LANE_QUEUE)
        .map(|index| activate(index).result.unwrap())
        .collect();
    assert!(waiting.iter().all(|job| job["status"] == json!("queued")));
    let refused = activate(LANE_QUEUE + 1).error.unwrap();
    assert_eq!(refused.code, "resource-limit");
    assert_eq!(refused.message, "the module lane is full");
    assert_eq!(
        owner.status(&format!("test.lane{}", LANE_QUEUE + 1))["activation"]["state"],
        json!("inactive"),
        "a refused activation changes nothing"
    );
    // A second request joins the waiting activation.
    let joined = activate(1).result.unwrap();
    assert_eq!(joined["job_id"], waiting[0]["job_id"]);
    // Cancelling a waiting activation removes it; deactivating one supersedes it.
    let cancelled = owner.ok(JOB_CANCEL, json!({"job_id": waiting[1]["job_id"]}));
    assert_eq!(cancelled["status"], json!("cancelled"));
    assert_eq!(
        owner.status("test.lane2")["activation"]["state"],
        json!("inactive")
    );
    let deactivated = owner.ok(DEACTIVATE, json!({"module_id": "test.lane3"}));
    assert_eq!(deactivated["activation"], json!("inactive"));
    assert_eq!(deactivated["status"], json!("superseded"));
    assert_eq!(
        owner.job(&waiting[2]["job_id"])["status"],
        json!("superseded")
    );
    // The lane has room again.
    let admitted = activate(LANE_QUEUE + 1).result.unwrap();
    assert_eq!(admitted["status"], json!("queued"));
    for probe in &fixture.lanes {
        probe.hold.store(false, Ordering::SeqCst);
    }
    for job in [&first, &waiting[0], &waiting[3], &admitted] {
        assert_eq!(owner.finished(&job["job_id"])["status"], json!("succeeded"));
    }
    for (index, probe) in fixture.lanes.iter().enumerate() {
        let expected = match index {
            2 | 3 => 0,
            _ => 1,
        };
        assert_eq!(
            probe.activations.load(Ordering::SeqCst),
            expected,
            "lane {index}"
        );
    }
    assert_eq!(
        owner.handle.capability_threads(),
        1,
        "only the module lane ran"
    );
    owner.stop();
}

#[test]
fn a_running_activation_stops_promptly_and_releases_what_it_loaded() {
    let server = serving();
    let fixture = Fixture::new("running-cancel", &server);
    fixture.probe.hold.store(true, Ordering::SeqCst);
    let owner = fixture.start();
    owner.set(json!({"label": "tint"}));
    owner.install_from_file(&fixture, "palette", PALETTE);
    let queued = owner.ok(ACTIVATE, json!({"module_id": MODULE}));
    owner.until(&queued["job_id"], |job| {
        job["progress"]["fraction"] == json!(0.5)
    });
    assert_eq!(
        fixture.probe.loaded.lock().unwrap().as_deref(),
        Some(PALETTE),
        "the activation loaded the palette and holds it"
    );
    // The owner answers while the activation runs: the worker never holds it.
    let started = Instant::now();
    let status = owner.status(MODULE);
    let job = owner.job(&queued["job_id"]);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the owner answered in {:?}",
        started.elapsed()
    );
    assert_eq!(status["activation"]["state"], json!("activating"));
    assert_eq!(status["activation"]["job_id"], queued["job_id"]);
    assert_eq!(job["status"], json!("running"));
    assert_eq!(
        job["progress"],
        json!({"fraction": 0.5, "message": "loaded"})
    );
    let started = Instant::now();
    let cancelling = owner.ok(JOB_CANCEL, json!({"job_id": queued["job_id"]}));
    assert_eq!(
        cancelling["status"],
        json!("running"),
        "it stops at its checkpoint"
    );
    let stopped = owner.finished(&queued["job_id"]);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the cancel took {:?}",
        started.elapsed()
    );
    assert_eq!(stopped["status"], json!("cancelled"));
    assert_eq!(stopped["error"]["code"], json!("cancelled"));
    assert_eq!(
        fixture.probe.loaded.lock().unwrap().as_deref(),
        None,
        "the host released what the activation had loaded"
    );
    assert_eq!(fixture.probe.deactivations.load(Ordering::SeqCst), 1);
    assert_eq!(
        owner.status(MODULE)["activation"]["state"],
        json!("inactive")
    );
    owner.stop();
}

#[test]
fn activation_lists_every_missing_requirement_before_anything_is_queued() {
    let server = serving();
    let fixture = Fixture::new("requirements", &server);
    let owner = fixture.start();
    let refused = owner.fail(ACTIVATE, json!({"module_id": MODULE}));
    assert_eq!(refused.code, "not-ready");
    assert_eq!(
        refused.data.unwrap()["requirements"],
        json!([
            {"kind": "setting", "id": "label", "state": "missing"},
            {"kind": "resource", "id": "palette", "state": "not-installed"},
        ])
    );
    assert!(
        refused
            .message
            .contains("setting label is missing, resource palette is not-installed")
    );
    assert_eq!(owner.handle.capability_threads(), 0, "nothing was queued");
    // A module that declares no activation, or an unknown one, is refused outright.
    let error = owner.fail(ACTIVATE, json!({"module_id": "lightwell.basic"}));
    assert_eq!(
        (error.code.as_str(), error.message.as_str()),
        (
            "validation",
            "module lightwell.basic declares no activation"
        )
    );
    assert_eq!(
        owner
            .fail(ACTIVATE, json!({"module_id": "test.missing"}))
            .message,
        "unknown module test.missing"
    );
    assert_eq!(fixture.probe.activations.load(Ordering::SeqCst), 0);
    owner.stop();
}

#[test]
fn an_activation_goes_active_then_inactive_and_status_reports_each_step() {
    let server = serving();
    let fixture = Fixture::new("activate", &server);
    let owner = fixture.start();
    owner.set(json!({"label": "tint"}));
    owner.ok(
        SET_SECRET,
        json!({"module_id": MODULE, "setting": "token", "value": "worker-only", "mutation": mutation(owner.revision())}),
    );
    let grant = owner.grant_download(&fixture, "palette", "palette");
    let installed = owner.finished(
        &owner.ok(
            INSTALL,
            json!({"module_id": MODULE, "resource_id": "palette"}),
        )["job_id"],
    );
    assert_eq!(installed["status"], json!("succeeded"));
    let queued = owner.ok(ACTIVATE, json!({"module_id": MODULE}));
    assert_eq!(queued["activation"], json!("activating"));
    let activated = owner.finished(&queued["job_id"]);
    assert_eq!(activated["status"], json!("succeeded"));
    assert_eq!(activated["result"], json!({"activation": "active"}));
    assert_eq!(
        activated["request_id"],
        json!(queued_request(&owner, ACTIVATE))
    );
    assert_eq!(
        fixture.probe.loaded.lock().unwrap().as_deref(),
        Some(PALETTE),
        "the module loaded the installed resource through its context"
    );
    assert!(
        fixture.probe.secret_read.load(Ordering::SeqCst),
        "the worker read the declared secret"
    );
    let status = owner.status(MODULE);
    assert_eq!(status["module_id"], json!(MODULE));
    assert_eq!(status["activation"], json!({"state": "active"}));
    assert_eq!(status["settings"]["state"], json!("ready"));
    assert_eq!(status["settings"]["missing"], json!([]));
    assert_eq!(status["resources"][0]["state"], json!("installed"));
    assert_eq!(status["resources"][1]["state"], json!("not-installed"));
    assert_eq!(
        status["permissions"]["grants"][0]["grant_id"],
        grant["grant"]["grant_id"]
    );
    let jobs: Vec<&str> = status["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["kind"].as_str().unwrap())
        .collect();
    assert_eq!(jobs, ["install", "activate"]);
    // Activating an active module changes nothing.
    assert_eq!(
        owner.ok(ACTIVATE, json!({"module_id": MODULE})),
        json!({"module_id": MODULE, "activation": "active", "deduplicated": false})
    );
    let deactivated = owner.ok(DEACTIVATE, json!({"module_id": MODULE}));
    assert_eq!(deactivated["activation"], json!("inactive"));
    let released = owner.finished(&deactivated["job_id"]);
    assert_eq!(released["kind"], json!("deactivate"));
    assert_eq!(released["status"], json!("succeeded"));
    assert_eq!(fixture.probe.loaded.lock().unwrap().as_deref(), None);
    assert_eq!(
        owner.status(MODULE)["activation"],
        json!({"state": "inactive"})
    );
    assert!(
        fixture.version_dir("palette").join(INSTALLED_FILE).exists(),
        "deactivation deletes no resource"
    );
    assert_eq!(
        owner.ok(DEACTIVATE, json!({"module_id": MODULE})),
        json!({"module_id": MODULE, "activation": "inactive", "deduplicated": false}),
        "deactivating an inactive module changes nothing"
    );
    // Each change a job made is announced under the request that started it.
    let methods: Vec<String> = owner
        .events()
        .into_iter()
        .map(|(method, _)| method)
        .filter(|method| !method.starts_with("module.settings") && method != GRANT)
        .collect();
    assert_eq!(methods, [INSTALL, ACTIVATE, DEACTIVATE]);
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
fn a_failed_activation_reads_failed_and_releases_its_partial_state() {
    let server = serving();
    let fixture = Fixture::new("failed", &server);
    fixture.probe.fail.store(true, Ordering::SeqCst);
    let owner = fixture.start();
    owner.set(json!({"label": "tint"}));
    owner.install_from_file(&fixture, "palette", PALETTE);
    let queued = owner.ok(ACTIVATE, json!({"module_id": MODULE}));
    let failed = owner.finished(&queued["job_id"]);
    assert_eq!(failed["status"], json!("failed"));
    assert_eq!(
        failed["error"],
        json!({"code": "invalid-input", "message": "the palette is corrupt"})
    );
    let status = owner.status(MODULE);
    assert_eq!(status["activation"]["state"], json!("failed"));
    assert_eq!(
        status["activation"]["error"]["message"],
        json!("the palette is corrupt")
    );
    assert_eq!(fixture.probe.loaded.lock().unwrap().as_deref(), None);
    assert_eq!(fixture.probe.deactivations.load(Ordering::SeqCst), 1);
    // A failed module can be activated again.
    fixture.probe.fail.store(false, Ordering::SeqCst);
    let retried = owner.ok(ACTIVATE, json!({"module_id": MODULE}));
    assert_eq!(
        owner.finished(&retried["job_id"])["status"],
        json!("succeeded")
    );
    assert_eq!(owner.status(MODULE)["activation"]["state"], json!("active"));
    owner.stop();
}

#[test]
fn a_settings_change_that_invalidates_activation_deactivates_the_module() {
    let server = serving();
    let fixture = Fixture::new("invalidates", &server);
    let owner = fixture.start();
    owner.set(json!({"label": "tint"}));
    owner.install_from_file(&fixture, "palette", PALETTE);
    let queued = owner.ok(ACTIVATE, json!({"module_id": MODULE}));
    owner.finished(&queued["job_id"]);
    // A field that does not invalidate activation leaves the module active.
    owner.set(json!({"strength": 0.25}));
    assert_eq!(owner.status(MODULE)["activation"]["state"], json!("active"));
    let changed = owner.set(json!({"label": "other tint"}));
    assert_eq!(changed["invalidates_activation"], json!(true));
    let status = owner.status(MODULE);
    assert_eq!(status["activation"]["state"], json!("inactive"));
    assert_eq!(status["activation"]["reason"], json!("settings changed"));
    assert_eq!(
        owner.finished(&release_job(&status))["status"],
        json!("succeeded")
    );
    assert_eq!(fixture.probe.loaded.lock().unwrap().as_deref(), None);
    assert_eq!(fixture.probe.deactivations.load(Ordering::SeqCst), 1);
    owner.stop();
}

#[test]
fn a_download_installs_the_pinned_bytes_with_their_record() {
    let server = serving();
    let fixture = Fixture::new("download", &server);
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
    assert_eq!(done["status"], json!("succeeded"));
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
            "url": server.url("/palette"), "sha256": sha256_hex(PALETTE), "bytes": 12,
            "license": "CC0-1.0", "provenance": "Generated for tests", "source": "download",
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
    assert_eq!(server.hits(), 1, "the resource was downloaded once");
    assert!(!fixture.resources().join(STAGING_DIR).exists());
    owner.stop();
}

#[test]
fn every_failed_install_leaves_nothing_installed_and_nothing_staged() {
    let mode = Arc::new(Mutex::new("hash"));
    let serving_mode = mode.clone();
    let server = Server::start(move |_, stream| {
        let current = *serving_mode.lock().unwrap();
        match current {
            "hash" => reply(stream, "200 OK", "", b"twelve BYTES"),
            "short" => reply(stream, "200 OK", "", b"twelve"),
            "oversized" => reply(stream, "200 OK", "", b"twelve bytes and more"),
            "redirect" => reply(
                stream,
                "302 Found",
                "Location: http://127.0.0.1:1/palette\r\n",
                b"",
            ),
            "error" => reply(stream, "500 Internal Server Error", "", b""),
            _ => reply(stream, "200 OK", "", PALETTE),
        }
    });
    let fixture = Fixture::new("failures", &server);
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
    let methods: Vec<String> = owner
        .events()
        .into_iter()
        .map(|(method, _)| method)
        .collect();
    assert_eq!(methods, [GRANT]);
    // The same resource installs once the server sends the pinned bytes.
    *mode.lock().unwrap() = "good";
    fixture.probe.refuse.store(false, Ordering::SeqCst);
    let queued = owner.ok(INSTALL, install);
    assert_eq!(
        owner.finished(&queued["job_id"])["status"],
        json!("succeeded")
    );
    owner.stop();
}

#[test]
fn a_download_cancelled_mid_transfer_stops_promptly_and_leaves_nothing() {
    let release = Arc::new(AtomicBool::new(false));
    let server = stalling(release.clone());
    let fixture = Fixture::new("cancel-download", &server);
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
    let started = Instant::now();
    owner.ok(JOB_CANCEL, json!({"job_id": queued["job_id"]}));
    let cancelled = owner.finished(&queued["job_id"]);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the cancel took {:?}",
        started.elapsed()
    );
    release.store(true, Ordering::SeqCst);
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
    let server = serving();
    let mut fixture = Fixture::new("quota", &server);
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
    let path = fixture.root.join("palette.local");
    fs::write(&path, PALETTE).unwrap();
    let refused = owner.fail(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette", "source": {"kind": "file", "path": path}}),
    );
    assert_eq!(refused.code, "resource-limit");
    assert_eq!(owner.handle.capability_threads(), 0, "nothing was queued");
    assert_eq!(server.hits(), 0);
    fixture.assert_clean("palette", "over quota");
    owner.stop();
}

#[test]
fn what_a_crash_leaves_is_not_installed_and_the_next_install_removes_it() {
    let server = serving();
    let fixture = Fixture::new("crash", &server);
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
    owner.set(json!({"label": "tint"}));
    let refused = owner.fail(ACTIVATE, json!({"module_id": MODULE}));
    assert_eq!(
        refused.data.unwrap()["requirements"],
        json!([{"kind": "resource", "id": "palette", "state": "not-installed"}])
    );
    let done = owner.install_from_file(&fixture, "palette", PALETTE);
    assert_eq!(done["status"], json!("succeeded"));
    assert!(!stale.exists(), "the stale staging directory was removed");
    assert!(!fixture.resources().join(STAGING_DIR).exists());
    assert!(fixture.version_dir("palette").join(INSTALLED_FILE).exists());
    owner.stop();
}

#[test]
fn a_local_file_installs_without_a_grant_only_when_its_bytes_match() {
    let server = serving();
    let fixture = Fixture::new("local", &server);
    let owner = fixture.start();
    let done = owner.install_from_file(&fixture, "palette", PALETTE);
    assert_eq!(done["status"], json!("succeeded"), "{done}");
    let marker: Value = serde_json::from_slice(
        &fs::read(fixture.version_dir("palette").join(INSTALLED_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(marker["source"], json!("file"));
    assert_eq!(marker["url"], json!(server.url("/palette")));
    let mismatch = owner.install_from_file(&fixture, "swatch", b"sixteen bytes!!!");
    assert_eq!(mismatch["status"], json!("failed"));
    assert_eq!(
        mismatch["error"]["message"],
        json!("swatch does not match its pinned hash")
    );
    fixture.assert_clean("swatch", "mismatched local file");
    let longer = owner.install_from_file(&fixture, "swatch", b"sixteen byte set and more");
    assert_eq!(longer["error"]["code"], json!("resource-limit"));
    fixture.assert_clean("swatch", "longer local file");
    let missing = owner.fail(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "swatch", "source": {"kind": "file", "path": fixture.root.join("absent")}}),
    );
    assert_eq!(missing.code, "validation");
    assert_eq!(fixture.net.total(), 0, "a local install touches no network");
    assert_eq!(server.hits(), 0);
    owner.stop();
}

#[test]
fn removing_a_required_resource_deactivates_the_module_and_deletes_only_the_resource() {
    let server = serving();
    let fixture = Fixture::new("remove", &server);
    let owner = fixture.start();
    owner.set(json!({"label": "tint"}));
    owner.install_from_file(&fixture, "palette", PALETTE);
    owner.install_from_file(&fixture, "swatch", SWATCH);
    let queued = owner.ok(ACTIVATE, json!({"module_id": MODULE}));
    owner.finished(&queued["job_id"]);
    let catalog = fs::read(fixture.catalog()).unwrap();
    // Removing a resource the activation does not require leaves the module active.
    let swatch = owner.ok(
        REMOVE,
        json!({"module_id": MODULE, "resource_id": "swatch"}),
    );
    assert_eq!(
        owner.finished(&swatch["job_id"])["status"],
        json!("succeeded")
    );
    assert_eq!(owner.status(MODULE)["activation"]["state"], json!("active"));
    assert!(!fixture.version_dir("swatch").exists());
    let removal = owner.ok(
        REMOVE,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    let status = owner.status(MODULE);
    assert_eq!(status["activation"]["state"], json!("inactive"));
    assert_eq!(status["activation"]["reason"], json!("resource removed"));
    let removed = owner.finished(&removal["job_id"]);
    assert_eq!(removed["status"], json!("succeeded"));
    assert_eq!(
        removed["result"],
        json!({"resource_id": "palette", "version": "1.0.0", "removed": true})
    );
    owner.finished(&release_job(&status));
    assert_eq!(fixture.probe.loaded.lock().unwrap().as_deref(), None);
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
        .events()
        .into_iter()
        .map(|(method, _)| method)
        .filter(|method| method != SET)
        .collect();
    // The palette's removal is announced twice: when it deactivated the module and when the
    // resource was gone.
    assert_eq!(
        methods,
        [INSTALL, INSTALL, ACTIVATE, REMOVE, REMOVE, REMOVE]
    );
    owner.stop();
}

#[test]
fn discovery_status_and_reopen_start_no_lane_reach_no_network_and_create_nothing() {
    let server = serving();
    let fixture = Fixture::new("inert", &server);
    for round in 0..2 {
        let owner = fixture.start();
        let asked = fixture.secrets.calls().total();
        owner.ok("module.list", json!({}));
        let schema = owner.ok("schema.list", json!({}));
        for method in [
            GRANT,
            DENY,
            REVOKE,
            LIST,
            ACTIVATE,
            DEACTIVATE,
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
        assert_eq!(
            fixture.secrets.calls().total(),
            asked,
            "round {round}: discovery never asks the secret store"
        );
        let status = owner.status(MODULE);
        assert_eq!(status["activation"], json!({"state": "inactive"}));
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
        owner.ok(DEACTIVATE, json!({"module_id": MODULE}));
        assert_eq!(owner.handle.capability_threads(), 0, "round {round}");
        owner.stop();
    }
    assert_eq!(fixture.net.total(), 0, "no lookup or connection was made");
    assert_eq!(server.hits(), 0);
    assert!(
        !fixture.root.join("config").exists(),
        "no settings directory"
    );
    assert!(!fixture.root.join("data").exists(), "no resource directory");
    assert_eq!(fixture.probe.activations.load(Ordering::SeqCst), 0);
}

#[test]
fn a_sentinel_secret_reaches_the_worker_and_no_observable_surface() {
    let server = serving();
    let fixture = Fixture::new("sentinel", &server);
    let sentinel = format!("SENTINEL-{}", uuid::Uuid::new_v4().simple());
    let owner = fixture.start();
    owner.set(json!({"label": "tint"}));
    owner.ok(
        SET_SECRET,
        json!({"module_id": MODULE, "setting": "token", "value": sentinel, "mutation": mutation(owner.revision())}),
    );
    let consent = owner.fail(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    assert_eq!(consent.code, "consent-required");
    owner.grant_download(&fixture, "palette", "palette");
    let installed = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": "palette"}),
    );
    owner.finished(&installed["job_id"]);
    let queued = owner.ok(ACTIVATE, json!({"module_id": MODULE}));
    owner.finished(&queued["job_id"]);
    assert!(fixture.probe.secret_read.load(Ordering::SeqCst));
    owner.status(MODULE);
    owner.ok(LIST, json!({}));
    owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}));
    let deactivated = owner.ok(DEACTIVATE, json!({"module_id": MODULE}));
    owner.finished(&deactivated["job_id"]);
    owner.events();
    let observed = owner.observed.borrow().clone();
    owner.stop();
    assert!(observed.len() > 15);
    for text in &observed {
        assert!(
            !text.contains(&sentinel),
            "a response carries the secret: {text}"
        );
    }
    for (path, bytes) in files(&fixture.root) {
        assert!(
            !String::from_utf8_lossy(&bytes).contains(&sentinel),
            "{} holds the secret",
            path.display()
        );
    }
}

/// Every capability family that carries the `{request_id, actor}` envelope — permissions,
/// activation, resources and capability jobs — answers a retry from the owner's request table: the
/// first answer comes back marked `deduplicated`, nothing runs or is queued again and no event is
/// recorded; the same `request_id` with other input is a conflict. Each request's own work is let
/// finish before its retry, so an event its job announces is never mistaken for the retry's.
#[test]
fn a_retry_of_every_capability_family_returns_the_first_answer_and_records_no_event() {
    let server = serving();
    let fixture = Fixture::new("retries", &server);
    let owner = fixture.start();
    owner.set(json!({"label": "tint"}));
    owner.ok(
        SET_SECRET,
        json!({"module_id": MODULE, "setting": "token", "value": "worker-only", "mutation": mutation(owner.revision())}),
    );
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
    assert_eq!(
        owner.job(&installed["job_id"])["status"],
        json!("succeeded")
    );
    assert_eq!(jobs("install"), 1, "the retry queued no second install");
    let activating = twice(
        owner.edit,
        ACTIVATE,
        json!({"module_id": MODULE, "mutation": envelope("activate-1")}),
    );
    assert_eq!(activating["activation"], json!("activating"));
    assert_eq!(owner.status(MODULE)["activation"]["state"], json!("active"));
    assert_eq!(jobs("activate"), 1, "the retry queued no second activation");
    twice(
        owner.edit,
        DEACTIVATE,
        json!({"module_id": MODULE, "mutation": envelope("deactivate-1")}),
    );
    assert_eq!(jobs("deactivate"), 1);
    twice(
        owner.edit,
        JOB_CANCEL,
        json!({"job_id": installed["job_id"], "mutation": envelope("cancel-1")}),
    );
    let removed = twice(
        owner.edit,
        REMOVE,
        json!({"module_id": MODULE, "resource_id": "palette", "mutation": envelope("remove-1")}),
    );
    assert_eq!(owner.job(&removed["job_id"])["status"], json!("succeeded"));
    assert_eq!(jobs("remove"), 1, "the retry queued no second removal");
    let revoked = twice(
        owner.edit,
        REVOKE,
        json!({"grant_id": granted["grant"]["grant_id"], "mutation": envelope("revoke-1")}),
    );
    assert_eq!(revoked["outcome"], json!("committed"));
    owner.stop();
}
