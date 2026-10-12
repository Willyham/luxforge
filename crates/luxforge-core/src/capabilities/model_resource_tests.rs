//! Model resources through the catalog owner, as a client drives them: a resource of several files
//! installed whole or not at all and removed together; an interrupted transfer resumed from its
//! offset with an HTTP `Range` request and hashed whole at the end, refused when the whole file
//! misses its pin; a server that ignores the range, and range answers that do not continue the
//! file; stale staging; the trade-off, use restriction and requirements a row and the disclosure
//! carry; the machine minimum; and the runtime preparing an installed model and forgetting it. The transport answers
//! in process and honours `Range` as a server does; `luxforge-net`'s tests send a range over a
//! loopback socket.
use super::{
    descriptor::{
        CapabilityDescriptor, CapabilityKind, ModelKind, ResourceDescriptor, ResourceFile,
    },
    grants::GRANT,
    host::{HostConfig, STATUS},
    resources::{INSTALL, INSTALLED_FILE, PARTIAL_FILE, REMOVE, RESOURCE_LIST, STAGING_DIR},
    secrets::MemorySecretStore,
    testing::{
        Answer, Exchange, LifecycleModule, MODULE, MemoryTransport, Owner, Probe,
        lifecycle_descriptor, sha256_hex, temp,
    },
};
use crate::{Error, ModuleRegistry, inference::Precision};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

const SERVED: &str = "https://models.example";
const MODEL: &str = "segmenter";
const LARGE: &str = "segmenter-large";
const VERSION: &str = "2.1";
/// The model's two files: its own, the encoder, and the decoder beside it.
const ENCODER: &[u8] = b"encoder weights: thirty-two byte";
const DECODER: &[u8] = b"decoder weights, a little longer than the encoder's";
const GIB: u64 = 1 << 30;
const TRADEOFF: &str = "Finds smaller objects; slower to start and uses more memory";
const RESTRICTION: &str = "Non-commercial use only";

fn served(path: &str) -> String {
    format!("{SERVED}{path}")
}

/// What the server does with the next requests.
#[derive(Default)]
struct Serving {
    /// Answer this path with this many bytes of its body, then fail as a dropped connection does.
    cut: Option<(&'static str, usize)>,
    /// Answer a range request with the whole file, as a server without range support does.
    ignore_range: bool,
    /// Answer a range request with this status and `Content-Range`, and the rest of the file.
    range_answer: Option<(u16, Option<String>)>,
    /// Serve these bytes for a path in place of its pinned ones.
    replaced: BTreeMap<&'static str, Vec<u8>>,
    /// Every request's path and `Range` header, in order.
    seen: Vec<(String, Option<String>)>,
}

struct Server {
    state: Arc<Mutex<Serving>>,
    transport: Arc<MemoryTransport>,
}

impl Server {
    fn new() -> Self {
        let state = Arc::new(Mutex::new(Serving::default()));
        let held = state.clone();
        let transport = MemoryTransport::new(move |exchange: &Exchange, answer: &Answer| {
            let range = exchange
                .headers
                .iter()
                .find(|(name, _)| name == "range")
                .map(|(_, value)| value.clone());
            let mut serving = held.lock().unwrap();
            serving
                .seen
                .push((exchange.path().to_owned(), range.clone()));
            let path = exchange.path();
            let pinned: &[u8] = match path {
                "/segmenter/encoder.onnx" | "/large/encoder.onnx" => ENCODER,
                "/segmenter/decoder.onnx" => DECODER,
                _ => return answer.whole(404, b""),
            };
            let body = serving
                .replaced
                .get(path)
                .cloned()
                .unwrap_or_else(|| pinned.to_vec());
            let offset = range
                .as_deref()
                .and_then(|range| range.strip_prefix("bytes="))
                .and_then(|range| range.strip_suffix('-'))
                .and_then(|offset| offset.parse::<usize>().ok())
                .filter(|_| !serving.ignore_range);
            let cut = serving.cut.take_if(|(at, _)| *at == path);
            let scripted = serving.range_answer.clone().filter(|_| offset.is_some());
            drop(serving);
            let rest = match (offset, scripted) {
                (Some(offset), Some((status, content_range))) => {
                    answer.status(status);
                    if let Some(content_range) = content_range {
                        answer.header("Content-Range", &content_range);
                    }
                    &body[offset..]
                }
                (Some(offset), None) => {
                    answer.status(206);
                    answer.header(
                        "Content-Range",
                        &format!("bytes {offset}-{}/{}", body.len() - 1, body.len()),
                    );
                    &body[offset..]
                }
                (None, _) => {
                    answer.status(200);
                    &body[..]
                }
            };
            match cut {
                Some((_, after)) => {
                    answer.bytes(&rest[..after]);
                    answer.fail(Error::file_access("the connection was reset"));
                }
                None => answer.bytes(rest),
            }
        });
        Self { state, transport }
    }

    fn with(&self, change: impl FnOnce(&mut Serving)) {
        change(&mut self.state.lock().unwrap());
    }

    /// The requests since the last call, by path and range.
    fn take(&self) -> Vec<(String, Option<String>)> {
        std::mem::take(&mut self.state.lock().unwrap().seen)
    }
}

/// The lifecycle module with two models: `segmenter`, an encoder and a decoder held by a
/// `local-runtime` capability, with a trade-off, a use restriction and a 2 GiB minimum, and
/// `segmenter-large`, which needs 64 GiB.
fn descriptor() -> crate::ModuleDescriptor {
    let mut descriptor = lifecycle_descriptor(&served("/palette"), &served("/swatch"));
    let model = |id: &str, path: &str, minimum: u64| ResourceDescriptor {
        id: id.into(),
        title: "Object finder".into(),
        version: VERSION.into(),
        url: served(path),
        bytes: ENCODER.len() as u64,
        sha256: sha256_hex(ENCODER),
        format: "onnx".into(),
        license: "Apache-2.0".into(),
        provenance: "Generated for tests".into(),
        min_memory_bytes: Some(minimum),
        ..ResourceDescriptor::default()
    };
    descriptor.resources.push(ResourceDescriptor {
        files: vec![ResourceFile {
            name: "decoder.onnx".into(),
            url: served("/segmenter/decoder.onnx"),
            bytes: DECODER.len() as u64,
            sha256: sha256_hex(DECODER),
        }],
        tradeoff: Some(TRADEOFF.into()),
        use_restriction: Some(RESTRICTION.into()),
        ..model(MODEL, "/segmenter/encoder.onnx", 2 * GIB)
    });
    descriptor
        .resources
        .push(model(LARGE, "/large/encoder.onnx", 64 * GIB));
    for (id, resource) in [("segmenter-download", MODEL), ("large-download", LARGE)] {
        descriptor.capabilities.push(CapabilityDescriptor {
            id: id.into(),
            kind: CapabilityKind::DownloadArtifact {
                resource: resource.into(),
            },
            purpose: "Install the object finder.".into(),
        });
    }
    descriptor.capabilities.push(CapabilityDescriptor {
        id: "segment".into(),
        kind: CapabilityKind::LocalRuntime {
            resource: MODEL.into(),
            model: ModelKind::Session,
            resident_bytes: 512 << 20,
            backends: vec!["coreml".into(), "cpu".into()],
            precision: Precision::Fp32,
            with: Vec::new(),
        },
        purpose: "Find objects.".into(),
    });
    descriptor
}

struct Fixture {
    root: PathBuf,
    server: Server,
    descriptor: crate::ModuleDescriptor,
    machine: Option<u64>,
    inference: Option<Arc<dyn crate::inference::InferenceRuntime>>,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = temp(name);
        fs::create_dir_all(&root).unwrap();
        Self {
            root,
            server: Server::new(),
            descriptor: descriptor(),
            machine: Some(32 * GIB),
            inference: None,
        }
    }

    fn resources(&self) -> PathBuf {
        self.root.join("data").join("resources")
    }

    fn version_dir(&self, resource: &str) -> PathBuf {
        let version = &self.descriptor.resource(resource).unwrap().version;
        self.resources().join(MODULE).join(resource).join(version)
    }

    fn staging_dir(&self, resource: &str) -> PathBuf {
        let version = &self.descriptor.resource(resource).unwrap().version;
        self.resources()
            .join(STAGING_DIR)
            .join(MODULE)
            .join(resource)
            .join(version)
    }

    fn start(&self) -> Owner {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(LifecycleModule::shared(
                self.descriptor.clone(),
                Arc::new(Probe::default()),
            ))
            .unwrap();
        Owner::start(
            &self.root.join("catalog.sqlite"),
            Arc::new(registry),
            HostConfig {
                config_dir: Some(self.root.join("config")),
                resource_dir: Some(self.resources()),
                secrets: Arc::new(MemorySecretStore::new()),
                transport: self.server.transport.clone(),
                resource_quota_bytes: 1 << 20,
                machine_memory_bytes: self.machine,
                inference: self.inference.clone(),
                ..HostConfig::unconfigured()
            },
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn grant(owner: &Owner, capability: &str, resource: &str) {
    owner.ok_as(
        owner.admin,
        GRANT,
        json!({"module_id": MODULE, "capability": capability,
               "scope": {"resource": resource, "version": VERSION, "origin": SERVED}}),
    );
}

fn install(owner: &Owner, resource: &str) -> Value {
    let queued = owner.ok(
        INSTALL,
        json!({"module_id": MODULE, "resource_id": resource}),
    );
    owner.finished(&queued["job_id"])
}

fn row(owner: &Owner, resource: &str) -> Value {
    owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}))["resources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == resource)
        .cloned()
        .expect("a declared resource")
}

fn range(path: &str, range: Option<&str>) -> (String, Option<String>) {
    (path.to_owned(), range.map(str::to_owned))
}

#[test]
fn a_resource_of_several_files_installs_together_with_its_labels_and_is_removed_together() {
    let fixture = Fixture::new("model-files");
    let owner = fixture.start();
    // The consent asked before the download states both files' size together, where they go,
    // the trade-off, the use restriction and what the model needs, and no training-data note.
    let refused = owner.fail(INSTALL, json!({"module_id": MODULE, "resource_id": MODEL}));
    assert_eq!(refused.code, "consent-required");
    let disclosure = &refused.data.as_ref().unwrap()["consent"]["disclosure"];
    let total = (ENCODER.len() + DECODER.len()) as u64;
    assert_eq!(disclosure["bytes"], json!(total));
    assert_eq!(disclosure["data"], json!("Object finder 2.1 (2 files)"));
    assert_eq!(disclosure["tradeoff"], json!(TRADEOFF));
    assert_eq!(disclosure["use_restriction"], json!(RESTRICTION));
    assert_eq!(
        disclosure["requirements"],
        json!({"model": "session", "backends": ["coreml", "cpu"], "precision": "fp32",
               "resident_bytes": 512 << 20, "min_memory_bytes": 2 * GIB})
    );
    assert!(
        disclosure["storage"]
            .as_str()
            .unwrap()
            .ends_with(&format!("{MODEL}/{VERSION}"))
    );
    assert!(!disclosure.to_string().to_lowercase().contains("training"));

    grant(&owner, "segmenter-download", MODEL);
    let done = install(&owner, MODEL);
    assert_eq!(done["status"], json!("ready"), "{done}");
    assert_eq!(done["result"]["bytes"], json!(total));
    assert_eq!(
        done["result"]["files"],
        json!([
            {"name": MODEL, "bytes": ENCODER.len(), "sha256": sha256_hex(ENCODER)},
            {"name": "decoder.onnx", "bytes": DECODER.len(), "sha256": sha256_hex(DECODER)},
        ])
    );
    let version = fixture.version_dir(MODEL);
    assert_eq!(fs::read(version.join(MODEL)).unwrap(), ENCODER);
    assert_eq!(fs::read(version.join("decoder.onnx")).unwrap(), DECODER);
    let marker: Value =
        serde_json::from_slice(&fs::read(version.join(INSTALLED_FILE)).unwrap()).unwrap();
    assert_eq!(
        marker["files"],
        json!([{"name": "decoder.onnx", "url": served("/segmenter/decoder.onnx"),
                "sha256": sha256_hex(DECODER), "bytes": DECODER.len()}])
    );
    let mut names: Vec<String> = fs::read_dir(&version)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["decoder.onnx", INSTALLED_FILE, MODEL]);
    assert!(!fixture.resources().join(STAGING_DIR).exists());

    // The row: the size of both files, each file, the labels and the requirements, met here.
    let listed = owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}));
    assert_eq!(listed["machine"], json!({"memory_bytes": 32 * GIB}));
    assert_eq!(listed["storage"]["used_bytes"], json!(total));
    let installed = row(&owner, MODEL);
    assert_eq!(installed["state"], json!("installed"));
    assert_eq!(installed["bytes"], json!(total));
    assert_eq!(
        installed["files"],
        json!([{"name": MODEL, "bytes": ENCODER.len()},
               {"name": "decoder.onnx", "bytes": DECODER.len()}])
    );
    assert_eq!(installed["tradeoff"], json!(TRADEOFF));
    assert_eq!(installed["use_restriction"], json!(RESTRICTION));
    assert_eq!(installed["requirements"], disclosure["requirements"]);
    assert_eq!(installed["fits"], json!(true));
    assert!(installed.get("staged_bytes").is_none());
    // module.status carries the same rows and the machine.
    let status = owner.status(MODULE);
    assert_eq!(status["machine"], json!({"memory_bytes": 32 * GIB}));
    assert_eq!(
        status["resources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == MODEL),
        Some(&installed)
    );

    // A file gone from the directory makes the whole resource not installed.
    fs::rename(version.join("decoder.onnx"), fixture.root.join("aside")).unwrap();
    assert_eq!(row(&owner, MODEL)["state"], json!("not-installed"));
    fs::rename(fixture.root.join("aside"), version.join("decoder.onnx")).unwrap();
    assert_eq!(row(&owner, MODEL)["state"], json!("installed"));

    // Removed together.
    let removed = owner.ok(REMOVE, json!({"module_id": MODULE, "resource_id": MODEL}));
    assert_eq!(owner.finished(&removed["job_id"])["status"], json!("ready"));
    assert!(!version.exists());
    assert!(!fixture.resources().join(MODULE).join(MODEL).exists());
    assert_eq!(row(&owner, MODEL)["state"], json!("not-installed"));
    owner.stop();
}

/// A further file may come from another remote origin, as a model's weights from their publisher
/// and a file the project computed from its own releases: the consent names that origin, and the
/// grant for the resource's own origin and pinned version installs both.
#[test]
fn a_further_file_from_another_origin_is_disclosed_and_installed_with_the_resource() {
    const RELEASES: &str = "https://releases.example";
    let mut fixture = Fixture::new("model-origins");
    let model = fixture
        .descriptor
        .resources
        .iter_mut()
        .find(|resource| resource.id == MODEL)
        .unwrap();
    model.files[0].url = format!("{RELEASES}/segmenter/decoder.onnx");
    let owner = fixture.start();
    let refused = owner.fail(INSTALL, json!({"module_id": MODULE, "resource_id": MODEL}));
    assert_eq!(refused.code, "consent-required");
    let disclosure = &refused.data.as_ref().unwrap()["consent"]["disclosure"];
    assert_eq!(
        disclosure["further_origins"],
        json!([RELEASES]),
        "{disclosure}"
    );
    grant(&owner, "segmenter-download", MODEL);
    assert_eq!(install(&owner, MODEL)["status"], json!("ready"));
    assert_eq!(
        fs::read(fixture.version_dir(MODEL).join("decoder.onnx")).unwrap(),
        DECODER
    );
    // One origin's files disclose none.
    let refused = owner.fail(INSTALL, json!({"module_id": MODULE, "resource_id": LARGE}));
    let disclosure = &refused.data.as_ref().unwrap()["consent"]["disclosure"];
    assert!(disclosure.get("further_origins").is_none(), "{disclosure}");
    owner.stop();
}

#[test]
fn a_later_file_that_misses_its_pin_installs_nothing_of_the_resource() {
    let fixture = Fixture::new("model-atomic");
    let owner = fixture.start();
    grant(&owner, "segmenter-download", MODEL);
    let mut wrong = DECODER.to_vec();
    wrong[3] ^= 1;
    fixture.server.with(|serving| {
        serving
            .replaced
            .insert("/segmenter/decoder.onnx", wrong.clone());
    });
    let failed = install(&owner, MODEL);
    assert_eq!(failed["status"], json!("failed"));
    assert_eq!(
        failed["error"],
        json!({"code": "validation", "message": "decoder.onnx does not match its pinned hash"})
    );
    // The encoder, verified, is not installed alone, and nothing is kept to resume.
    assert!(!fixture.version_dir(MODEL).exists());
    assert!(!fixture.resources().join(STAGING_DIR).exists());
    let failed_row = row(&owner, MODEL);
    assert_eq!(failed_row["state"], json!("failed"));
    assert!(failed_row.get("staged_bytes").is_none());
    // Served right, both files are fetched again from their start and installed.
    fixture.server.with(|serving| serving.replaced.clear());
    fixture.server.take();
    assert_eq!(install(&owner, MODEL)["status"], json!("ready"));
    assert_eq!(
        fixture.server.take(),
        [
            range("/segmenter/encoder.onnx", None),
            range("/segmenter/decoder.onnx", None)
        ]
    );
    owner.stop();
}

#[test]
fn an_interrupted_transfer_resumes_from_its_offset_and_is_hashed_whole() {
    let fixture = Fixture::new("model-resume");
    let owner = fixture.start();
    grant(&owner, "segmenter-download", MODEL);
    fixture
        .server
        .with(|serving| serving.cut = Some(("/segmenter/decoder.onnx", 20)));
    let interrupted = install(&owner, MODEL);
    assert_eq!(interrupted["status"], json!("failed"));
    assert_eq!(interrupted["error"]["code"], json!("read-error"));
    // Nothing is installed; what was staged stays, under the pins it was fetched with.
    assert!(!fixture.version_dir(MODEL).exists());
    let staging = fixture.staging_dir(MODEL);
    assert_eq!(fs::read(staging.join(MODEL)).unwrap(), ENCODER);
    assert_eq!(
        fs::read(staging.join("decoder.onnx")).unwrap(),
        &DECODER[..20]
    );
    assert!(staging.join(PARTIAL_FILE).is_file());
    let partial = row(&owner, MODEL);
    assert_eq!(partial["state"], json!("failed"));
    assert_eq!(partial["staged_bytes"], json!(ENCODER.len() + 20));
    assert_eq!(
        fixture.server.take(),
        [
            range("/segmenter/encoder.onnx", None),
            range("/segmenter/decoder.onnx", None)
        ]
    );
    // The next install asks only for the rest of the decoder, and hashes each file whole.
    let resumed = install(&owner, MODEL);
    assert_eq!(resumed["status"], json!("ready"), "{resumed}");
    assert_eq!(
        fixture.server.take(),
        [range("/segmenter/decoder.onnx", Some("bytes=20-"))],
        "the staged encoder is complete, so it is not asked for again"
    );
    assert_eq!(
        resumed["result"]["files"][1]["sha256"],
        json!(sha256_hex(DECODER))
    );
    let version = fixture.version_dir(MODEL);
    assert_eq!(fs::read(version.join("decoder.onnx")).unwrap(), DECODER);
    assert!(
        !version.join(PARTIAL_FILE).exists(),
        "the staging record is not installed"
    );
    assert!(!fixture.resources().join(STAGING_DIR).exists());
    assert_eq!(row(&owner, MODEL)["state"], json!("installed"));
    owner.stop();
}

#[test]
fn a_resumed_file_that_misses_its_pin_is_refused_and_the_next_install_starts_again() {
    let fixture = Fixture::new("model-resume-mismatch");
    let owner = fixture.start();
    grant(&owner, "segmenter-download", MODEL);
    fixture
        .server
        .with(|serving| serving.cut = Some(("/segmenter/decoder.onnx", 20)));
    install(&owner, MODEL);
    // The server's file changed under the same pin: the rest it sends does not complete the pinned
    // bytes, and only the hash of the whole file can tell.
    let mut changed = DECODER.to_vec();
    changed[40] ^= 1;
    fixture.server.with(|serving| {
        serving
            .replaced
            .insert("/segmenter/decoder.onnx", changed.clone());
    });
    fixture.server.take();
    let refused = install(&owner, MODEL);
    assert_eq!(refused["status"], json!("failed"));
    assert_eq!(
        refused["error"],
        json!({"code": "validation", "message":
            "decoder.onnx does not match its pinned hash after resuming at byte 20; its partial transfer is discarded"})
    );
    assert_eq!(
        fixture.server.take(),
        [range("/segmenter/decoder.onnx", Some("bytes=20-"))]
    );
    assert!(!fixture.version_dir(MODEL).exists());
    assert!(!fixture.resources().join(STAGING_DIR).exists());
    assert!(row(&owner, MODEL).get("staged_bytes").is_none());
    // Served right again, the whole resource is fetched from the start.
    fixture.server.with(|serving| serving.replaced.clear());
    assert_eq!(install(&owner, MODEL)["status"], json!("ready"));
    assert_eq!(
        fixture.server.take(),
        [
            range("/segmenter/encoder.onnx", None),
            range("/segmenter/decoder.onnx", None)
        ]
    );
    owner.stop();
}

#[test]
fn a_server_that_ignores_the_range_starts_the_file_again_in_the_same_request() {
    let fixture = Fixture::new("model-no-range");
    let owner = fixture.start();
    grant(&owner, "segmenter-download", MODEL);
    fixture
        .server
        .with(|serving| serving.cut = Some(("/segmenter/decoder.onnx", 20)));
    install(&owner, MODEL);
    fixture.server.with(|serving| serving.ignore_range = true);
    fixture.server.take();
    let done = install(&owner, MODEL);
    assert_eq!(done["status"], json!("ready"), "{done}");
    assert_eq!(
        fixture.server.take(),
        [range("/segmenter/decoder.onnx", Some("bytes=20-"))],
        "the range was asked for, and the whole file answered it"
    );
    assert_eq!(
        fs::read(fixture.version_dir(MODEL).join("decoder.onnx")).unwrap(),
        DECODER
    );
    owner.stop();
}

#[test]
fn a_range_answer_that_does_not_continue_the_file_is_refused_and_its_staging_discarded() {
    let fixture = Fixture::new("model-range-refused");
    let owner = fixture.start();
    grant(&owner, "segmenter-download", MODEL);
    let length = DECODER.len();
    for (status, content_range, message) in [
        (
            206,
            Some(format!("bytes 0-{}/{length}", length - 1)),
            format!(
                "https://models.example answered the range bytes 0-{}/{length} for decoder.onnx; resuming at byte 20 needs bytes 20-{}/{length}",
                length - 1,
                length - 1
            ),
        ),
        (
            206,
            None,
            format!(
                "https://models.example answered the range none for decoder.onnx; resuming at byte 20 needs bytes 20-{}/{length}",
                length - 1
            ),
        ),
        (
            416,
            None,
            "https://models.example cannot resume decoder.onnx at byte 20; its partial transfer is discarded"
                .to_owned(),
        ),
    ] {
        fixture.server.with(|serving| {
            serving.range_answer = None;
            serving.cut = Some(("/segmenter/decoder.onnx", 20));
        });
        assert_eq!(install(&owner, MODEL)["status"], json!("failed"));
        assert_eq!(row(&owner, MODEL)["staged_bytes"], json!(ENCODER.len() + 20));
        fixture.server.with(|serving| {
            serving.range_answer = Some((status, content_range.clone()));
        });
        fixture.server.take();
        let refused = install(&owner, MODEL);
        assert_eq!(refused["status"], json!("failed"));
        assert_eq!(
            refused["error"],
            json!({"code": "validation", "message": message}),
            "{status} {content_range:?}"
        );
        assert_eq!(
            fixture.server.take(),
            [range("/segmenter/decoder.onnx", Some("bytes=20-"))]
        );
        // Nothing of the refused transfer is written or kept: the next install starts again.
        assert!(!fixture.version_dir(MODEL).exists());
        assert!(!fixture.resources().join(STAGING_DIR).exists());
        assert!(row(&owner, MODEL).get("staged_bytes").is_none());
    }
    owner.stop();
}

#[test]
fn staging_no_declaration_names_is_removed_and_a_changed_pin_starts_its_file_again() {
    let mut fixture = Fixture::new("model-stale");
    let staging = fixture.resources().join(STAGING_DIR);
    // An older install's layout, a module this run does not serve, an older version and a stray
    // file; and an interrupted transfer of the current model, kept.
    for stale in [
        staging.join("job-old").join("palette"),
        staging.join("gone.module").join("x").join("1").join("x"),
        staging.join(MODULE).join(MODEL).join("1.0").join(MODEL),
        staging.join(MODULE).join("undeclared").join("1").join("x"),
    ] {
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(&stale, b"stale").unwrap();
    }
    fs::write(staging.join("stray"), b"stale").unwrap();
    {
        let owner = fixture.start();
        grant(&owner, "segmenter-download", MODEL);
        fixture
            .server
            .with(|serving| serving.cut = Some(("/segmenter/decoder.onnx", 20)));
        install(&owner, MODEL);
        owner.stop();
    }
    let mut names: Vec<String> = fs::read_dir(&staging)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, [MODULE], "only the current model's staging is left");
    assert_eq!(
        fs::read_dir(staging.join(MODULE).join(MODEL))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        [VERSION]
    );
    // The decoder is re-pinned at other bytes: its partial is not a prefix of them, so it starts
    // again; the encoder's pin is unchanged, so it is not fetched again.
    let mut repinned = DECODER.to_vec();
    repinned.extend_from_slice(b" and some more");
    let model = fixture
        .descriptor
        .resources
        .iter_mut()
        .find(|resource| resource.id == MODEL)
        .unwrap();
    model.files[0].bytes = repinned.len() as u64;
    model.files[0].sha256 = sha256_hex(&repinned);
    fixture.server.with(|serving| {
        serving
            .replaced
            .insert("/segmenter/decoder.onnx", repinned.clone());
    });
    fixture.server.take();
    let owner = fixture.start();
    grant(&owner, "segmenter-download", MODEL);
    assert_eq!(install(&owner, MODEL)["status"], json!("ready"));
    assert_eq!(
        fixture.server.take(),
        [range("/segmenter/decoder.onnx", None)]
    );
    assert!(!staging.exists());
    owner.stop();
}

#[test]
fn a_machine_below_a_models_minimum_sees_its_requirement_and_cannot_install_it() {
    let mut fixture = Fixture::new("model-machine");
    fixture.machine = Some(8 * GIB);
    let owner = fixture.start();
    let large = row(&owner, LARGE);
    assert_eq!(large["fits"], json!(false));
    assert_eq!(large["requirements"], json!({"min_memory_bytes": 64 * GIB}));
    assert_eq!(row(&owner, MODEL)["fits"], json!(true));
    // Refused by name before consent is asked or anything is sent; a grant changes nothing.
    for granted in [false, true] {
        if granted {
            grant(&owner, "large-download", LARGE);
        }
        let refused = owner.fail(INSTALL, json!({"module_id": MODULE, "resource_id": LARGE}));
        assert_eq!(refused.code, "resource-limit");
        assert_eq!(
            refused.message,
            "Object finder needs a machine with at least 64 GB of memory; this machine has 8 GB"
        );
        assert_eq!(
            refused.data.as_ref().unwrap()["requirements"],
            json!([{"kind": "memory", "id": LARGE, "required_bytes": 64 * GIB,
                    "machine_bytes": 8 * GIB}])
        );
    }
    assert_eq!(fixture.server.transport.sends(), 0);
    assert_eq!(owner.handle.lane_threads(), 0, "nothing was queued");
    owner.stop();

    // A host that cannot tell its memory checks no minimum and says nothing of fitting.
    fixture.machine = None;
    let owner = fixture.start();
    let unknown = row(&owner, LARGE);
    assert!(unknown.get("fits").is_none(), "{unknown}");
    assert_eq!(
        owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}))["machine"],
        json!({"memory_bytes": null})
    );
    assert_eq!(
        owner.status(MODULE)["machine"],
        json!({"memory_bytes": null})
    );
    owner.stop();
}

/// A runtime that derives one file beside each model it prepares, records what it was asked to
/// forget, and can be made to fail its preparation.
#[derive(Default)]
struct Deriving {
    prepared: Mutex<Vec<(PathBuf, String)>>,
    forgot: Mutex<Vec<String>>,
    fail: std::sync::atomic::AtomicBool,
}

impl crate::inference::InferenceRuntime for Deriving {
    fn identity(&self) -> crate::inference::RuntimeIdentity {
        crate::inference::RuntimeIdentity {
            name: "deriving".into(),
            version: "1".into(),
        }
    }

    fn backends(&self) -> Vec<String> {
        vec!["coreml".into(), "cpu".into()]
    }

    fn prepare(
        &self,
        model: &crate::inference::ModelFile,
        _: &crate::Cancel,
    ) -> Result<Vec<PathBuf>, Error> {
        self.prepared
            .lock()
            .unwrap()
            .push((model.path().to_path_buf(), model.identity().sha256.clone()));
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(Error::validation("not a model this runtime reads"));
        }
        let derived = model.path().with_extension("fixed");
        fs::write(&derived, b"fixed").unwrap();
        Ok(vec![derived])
    }

    fn open_session(
        &self,
        _: &crate::inference::ModelFile,
        _: &crate::inference::OpenOptions,
        _: &crate::Cancel,
    ) -> Result<Box<dyn crate::inference::Session>, Error> {
        Err(Error::validation("the deriving runtime opens nothing"))
    }

    fn forget(&self, sha256: &str) -> usize {
        self.forgot.lock().unwrap().push(sha256.to_owned());
        1
    }
}

#[test]
fn an_installed_model_is_prepared_its_derived_files_recorded_and_forgotten_on_removal() {
    let mut fixture = Fixture::new("model-prepare");
    let runtime = Arc::new(Deriving::default());
    fixture.inference = Some(runtime.clone());
    let owner = fixture.start();
    grant(&owner, "segmenter-download", MODEL);
    let done = install(&owner, MODEL);
    assert_eq!(done["status"], json!("ready"), "{done}");
    let version = fixture.version_dir(MODEL);
    // The runtime prepared the installed model, the resource's own file, before it was marked.
    assert_eq!(
        *runtime.prepared.lock().unwrap(),
        [(version.join(MODEL), sha256_hex(ENCODER))]
    );
    let derived = json!([{"path": format!("{MODEL}.fixed"), "bytes": 5}]);
    assert_eq!(done["result"]["derived"], derived);
    let marker: Value =
        serde_json::from_slice(&fs::read(version.join(INSTALLED_FILE)).unwrap()).unwrap();
    assert_eq!(marker["derived"], derived);
    // What it derived counts against the quota beside the pinned files.
    let total = (ENCODER.len() + DECODER.len()) as u64;
    assert_eq!(
        owner.ok(RESOURCE_LIST, json!({"module_id": MODULE}))["storage"]["used_bytes"],
        json!(total + 5)
    );
    assert_eq!(row(&owner, MODEL)["state"], json!("installed"));

    // Removal takes the derived file with the directory and asks the runtime to forget each file.
    let removed = owner.ok(REMOVE, json!({"module_id": MODULE, "resource_id": MODEL}));
    let finished = owner.finished(&removed["job_id"]);
    assert_eq!(
        finished["result"]["compiled_removed"],
        json!(2),
        "{finished}"
    );
    assert_eq!(
        *runtime.forgot.lock().unwrap(),
        [sha256_hex(ENCODER), sha256_hex(DECODER)]
    );
    assert!(!version.exists());

    // A preparation that fails leaves the model installed, says why, and records nothing derived:
    // the first open derives what it needs and reports its own failure.
    runtime
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let done = install(&owner, MODEL);
    assert_eq!(done["status"], json!("ready"), "{done}");
    assert_eq!(
        done["result"]["prepare_error"],
        json!({"code": "validation", "message": "not a model this runtime reads"})
    );
    let marker: Value =
        serde_json::from_slice(&fs::read(version.join(INSTALLED_FILE)).unwrap()).unwrap();
    assert!(marker.get("derived").is_none(), "{marker}");
    owner.stop();
}

#[test]
fn status_reports_the_machine_a_models_minimum_is_checked_against() {
    let fixture = Fixture::new("model-status");
    let owner = fixture.start();
    assert_eq!(
        owner.ok(STATUS, json!({"module_id": MODULE}))["machine"],
        json!({"memory_bytes": 32 * GIB})
    );
    owner.stop();
}
