//! Setups through the catalog owner, as a client drives them over an in-memory transport: one
//! combined consent for every missing download, then one install per missing resource in declared
//! order, installed resources skipped and running installs joined, the lane's room checked before
//! anything is queued, a retry deduplicated, a failure reported and retried, and what this host
//! cannot run refused by name. `setups.rs` holds the status derivation's own tests.
use super::{
    descriptor::{
        CapabilityDescriptor, CapabilityKind, Introduction, IntroductionPicture, IntroductionStep,
        ModelKind, ResourceDescriptor, SetupDescriptor, SetupPart,
    },
    grants::{DENY, GRANT},
    host::{HostConfig, INSTALL_SETUP},
    resources::INSTALL,
    secrets::MemorySecretStore,
    testing::{
        LifecycleModule, MODULE, MemoryTransport, Owner, PALETTE, Probe, SWATCH,
        capability_descriptor, sha256_hex, temp,
    },
};
use crate::{ModuleDescriptor, ModuleRegistry};
use luxforge_testbase::Gate;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, sync::Arc};

const SERVED: &str = "https://downloads.example";
const GIB: u64 = 1 << 30;

/// The capabilities test module with the resources `finder` (the palette's bytes), `fill` (the
/// swatch's, needing 8 GiB) and `extra-1` to `extra-4` (the palette's), each installed by
/// `download-<id>`, and two setups: `object`, Object finder then Generative fill, and `masks`,
/// Object finder alone.
fn descriptor() -> ModuleDescriptor {
    let mut module = capability_descriptor();
    module.tasks.clear();
    module.controls.clear();
    let palette = module.resources[0].clone();
    let resource = |id: &str, body: &[u8], minimum: Option<u64>| ResourceDescriptor {
        id: id.into(),
        title: format!("The {id}"),
        url: format!("{SERVED}/{id}"),
        bytes: body.len() as u64,
        sha256: sha256_hex(body),
        min_memory_bytes: minimum,
        redirect_origins: Vec::new(),
        ..palette.clone()
    };
    module.resources = vec![
        resource("finder", PALETTE, None),
        resource("fill", SWATCH, Some(8 * GIB)),
    ];
    for n in 1..=4 {
        module
            .resources
            .push(resource(&format!("extra-{n}"), PALETTE, None));
    }
    module.capabilities = module
        .resources
        .iter()
        .map(|resource| CapabilityDescriptor {
            id: format!("download-{}", resource.id),
            kind: CapabilityKind::DownloadArtifact {
                resource: resource.id.clone(),
            },
            purpose: format!("Install the {}.", resource.id),
        })
        .collect();
    let introduction = Introduction {
        eyebrow: "Test".into(),
        title: "Remove an object by pointing at it".into(),
        lead: "Point at it and it is gone.".into(),
        hero: None,
        steps: vec![IntroductionStep {
            title: "Point at it".into(),
            body: "The object under the pointer is outlined.".into(),
            picture: IntroductionPicture::Hover,
        }],
        promise: "Your photos stay on this computer.".into(),
    };
    let finder = SetupPart {
        name: "Object finder".into(),
        purpose: "Finds the object".into(),
        resources: vec!["finder".into()],
    };
    module.setups = vec![
        SetupDescriptor {
            id: "object".into(),
            title: "Object".into(),
            introduction: introduction.clone(),
            parts: vec![
                finder.clone(),
                SetupPart {
                    name: "Generative fill".into(),
                    purpose: "Fills the space".into(),
                    resources: vec!["fill".into()],
                },
            ],
        },
        SetupDescriptor {
            id: "masks".into(),
            title: "Masks".into(),
            introduction,
            parts: vec![finder],
        },
    ];
    module
}

struct Fixture {
    root: PathBuf,
    descriptor: ModuleDescriptor,
    transport: Arc<MemoryTransport>,
    memory: Option<u64>,
}

impl Fixture {
    fn new(name: &str, transport: Arc<MemoryTransport>) -> Self {
        let root = temp(name);
        fs::create_dir_all(&root).unwrap();
        Self {
            root,
            descriptor: descriptor(),
            transport,
            memory: Some(16 * GIB),
        }
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
                resource_dir: Some(self.root.join("resources")),
                secrets: Arc::new(MemorySecretStore::new()),
                transport: self.transport.clone(),
                resource_quota_bytes: 1 << 20,
                machine_memory_bytes: self.memory,
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

/// A transport that answers every resource with its pinned bytes, `fill` with the swatch's.
fn serving() -> Arc<MemoryTransport> {
    MemoryTransport::new(|exchange, answer| match exchange.path() {
        "/fill" => answer.whole(200, SWATCH),
        _ => answer.whole(200, PALETTE),
    })
}

/// A transport whose answers wait at the returned gate, shut, after their status.
fn stalling() -> (Arc<MemoryTransport>, Arc<Gate>) {
    let gate = Arc::new(Gate::new());
    gate.shut();
    let held = gate.clone();
    let transport = MemoryTransport::new(move |exchange, answer| {
        answer.status(200);
        held.pass();
        answer.bytes(if exchange.path() == "/fill" {
            SWATCH
        } else {
            PALETTE
        });
    });
    (transport, gate)
}

fn setup_install(owner: &Owner, setup: &str) -> Result<Value, crate::ApiFailure> {
    let response = owner.call(
        owner.edit,
        INSTALL_SETUP,
        json!({"module_id": MODULE, "setup": setup}),
    );
    match response.error {
        Some(error) => Err(error),
        None => Ok(response.result.expect("a result")),
    }
}

/// The setup's row in `module.status`.
fn setup(owner: &Owner, id: &str) -> Value {
    owner.status(MODULE)["setups"]
        .as_array()
        .expect("the module lists its setups")
        .iter()
        .find(|row| row["id"] == id)
        .cloned()
        .expect("the setup is listed")
}

/// Grant each scope a consent failure lists, as the permission client.
fn grant_all(owner: &Owner, consent: &Value) {
    for grant in consent["grants"].as_array().unwrap() {
        owner.ok_as(
            owner.admin,
            GRANT,
            json!({"module_id": MODULE, "capability": grant["capability"], "scope": grant["scope"]}),
        );
    }
}

/// The resource each answered job installs, in order.
fn resources_of(answer: &Value) -> Vec<String> {
    answer["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["resource_id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn one_consent_lists_every_missing_grant_then_the_missing_installs_queue_in_order() {
    let transport = serving();
    let fixture = Fixture::new("setup-consent", transport.clone());
    let owner = fixture.start();
    let missing = setup(&owner, "object");
    assert_eq!(missing["state"], "missing");
    assert_eq!(
        missing["missing_bytes"],
        json!(PALETTE.len() + SWATCH.len())
    );
    assert_eq!(missing["minimum_memory"], json!(8 * GIB));
    assert_eq!(missing["fits"], json!(true));

    // A declined scope is reported as declined.
    owner.ok(
        DENY,
        json!({"module_id": MODULE, "capability": "download-fill",
               "scope": {"resource": "fill", "version": "1.0.0", "origin": SERVED}}),
    );
    let refused = setup_install(&owner, "object").unwrap_err();
    assert_eq!(refused.code, "consent-required");
    let consent = &refused.data.as_ref().unwrap()["consent"];
    assert_eq!(consent["module_id"], MODULE);
    assert_eq!(consent["setup"], "object");
    assert_eq!(
        consent["grants"],
        json!([
            {"capability": "download-finder", "kind": "download-artifact", "denied": false,
             "scope": {"resource": "finder", "version": "1.0.0", "origin": SERVED}},
            {"capability": "download-fill", "kind": "download-artifact", "denied": true,
             "scope": {"resource": "fill", "version": "1.0.0", "origin": SERVED}},
        ])
    );
    assert_eq!(consent["denied"], true);
    let disclosure = &consent["disclosure"];
    assert_eq!(disclosure["title"], "Object");
    assert_eq!(
        disclosure["introduction"]["title"],
        "Remove an object by pointing at it"
    );
    assert_eq!(disclosure["bytes"], json!(PALETTE.len() + SWATCH.len()));
    assert_eq!(disclosure["origins"], json!([SERVED]));
    assert_eq!(disclosure["licenses"], json!(["CC0-1.0"]));
    assert_eq!(
        (
            &disclosure["minimum_memory"],
            &disclosure["machine_memory_bytes"],
            &disclosure["fits"]
        ),
        (&json!(8 * GIB), &json!(16 * GIB), &json!(true))
    );
    assert_eq!(
        disclosure["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| (part["name"].clone(), part["installed"].clone()))
            .collect::<Vec<_>>(),
        [
            (json!("Object finder"), json!(false)),
            (json!("Generative fill"), json!(false))
        ]
    );
    assert_eq!(disclosure["downloads"].as_array().unwrap().len(), 2);
    assert_eq!(transport.sends(), 0, "nothing is fetched before consent");

    // Granting one scope leaves the other asked for.
    let first = &consent["grants"][0];
    owner.ok_as(
        owner.admin,
        GRANT,
        json!({"module_id": MODULE, "capability": first["capability"], "scope": first["scope"]}),
    );
    let refused = setup_install(&owner, "object").unwrap_err();
    let consent = refused.data.unwrap()["consent"].clone();
    assert_eq!(consent["grants"].as_array().unwrap().len(), 1);
    assert_eq!(consent["grants"][0]["capability"], "download-fill");
    grant_all(&owner, &consent);

    // Every scope granted: one install per missing resource, in declared order; a retry of the
    // same request is answered from the first.
    let request = json!({"module_id": MODULE, "setup": "object",
                         "mutation": {"request_id": "set-up-object", "actor": "test"}});
    let queued = owner.ok(INSTALL_SETUP, request.clone());
    assert_eq!(queued["setup"], "object");
    assert_eq!(resources_of(&queued), ["finder", "fill"]);
    let retried = owner.ok(INSTALL_SETUP, request);
    assert_eq!(retried["deduplicated"], true);
    assert_eq!(retried["jobs"], queued["jobs"]);
    for job in queued["jobs"].as_array().unwrap() {
        assert_eq!(owner.finished(&job["job_id"])["status"], "ready");
    }
    let installed = setup(&owner, "object");
    assert_eq!(
        (&installed["state"], &installed["missing_bytes"]),
        (&json!("installed"), &json!(0))
    );
    assert_eq!(setup(&owner, "masks")["state"], "installed");
    // Nothing missing: nothing queued, nothing asked.
    assert_eq!(
        resources_of(&setup_install(&owner, "object").unwrap()),
        Vec::<String>::new()
    );
    assert_eq!(transport.sends(), 2);
}

#[test]
fn an_installed_resource_is_skipped_and_a_running_install_joined() {
    let (transport, gate) = stalling();
    let fixture = Fixture::new("setup-join", transport);
    let owner = fixture.start();
    let consent = setup_install(&owner, "object").unwrap_err().data.unwrap()["consent"].clone();
    grant_all(&owner, &consent);

    // Mask Object's finder is installing: Object joins it and queues only the fill.
    let masks = setup_install(&owner, "masks").unwrap();
    assert_eq!(resources_of(&masks), ["finder"]);
    let installing = setup(&owner, "masks");
    assert_eq!(installing["state"], "installing");
    assert_eq!(installing["jobs"][0]["job_id"], masks["jobs"][0]["job_id"]);
    let object = setup_install(&owner, "object").unwrap();
    assert_eq!(resources_of(&object), ["finder", "fill"]);
    assert_eq!(object["jobs"][0]["job_id"], masks["jobs"][0]["job_id"]);
    assert_eq!(setup(&owner, "object")["state"], "installing");
    gate.open();
    for job in object["jobs"].as_array().unwrap() {
        assert_eq!(owner.finished(&job["job_id"])["status"], "ready");
    }

    // A finder removed again: Object queues the finder alone, the fill being installed.
    let removed = owner.ok(
        super::resources::REMOVE,
        json!({"module_id": MODULE, "resource_id": "finder"}),
    );
    owner.finished(&removed["job_id"]);
    let again = setup_install(&owner, "object").unwrap();
    assert_eq!(resources_of(&again), ["finder"]);
    owner.finished(&again["jobs"][0]["job_id"]);
}

#[test]
fn a_setup_the_lane_has_no_room_for_queues_nothing() {
    let (transport, gate) = stalling();
    let fixture = Fixture::new("setup-room", transport);
    let owner = fixture.start();
    let consent = setup_install(&owner, "object").unwrap_err().data.unwrap()["consent"].clone();
    grant_all(&owner, &consent);
    // Four other installs: one running and three queued leave room for one.
    let mut held = Vec::new();
    for n in 1..=4 {
        let id = format!("extra-{n}");
        owner.ok_as(
            owner.admin,
            GRANT,
            json!({"module_id": MODULE, "capability": format!("download-{id}"),
                   "scope": {"resource": id, "version": "1.0.0", "origin": SERVED}}),
        );
        held.push(
            owner.ok(INSTALL, json!({"module_id": MODULE, "resource_id": id}))["job_id"].clone(),
        );
    }
    let jobs = owner.status(MODULE)["jobs"].as_array().unwrap().len();
    let refused = setup_install(&owner, "object").unwrap_err();
    assert_eq!(refused.code, "resource-limit", "{}", refused.message);
    assert_eq!(refused.data.as_ref().unwrap()["room"], 1);
    assert_eq!(owner.status(MODULE)["jobs"].as_array().unwrap().len(), jobs);
    assert_eq!(setup(&owner, "object")["state"], "missing");
    gate.open();
    for job in held {
        owner.finished(&job);
    }
    let queued = setup_install(&owner, "object").unwrap();
    assert_eq!(resources_of(&queued), ["finder", "fill"]);
    for job in queued["jobs"].as_array().unwrap() {
        owner.finished(&job["job_id"]);
    }
}

#[test]
fn a_failed_part_reports_its_error_and_trying_again_installs_only_what_is_missing() {
    let transport = MemoryTransport::new(|exchange, answer| match exchange.path() {
        "/fill" => answer.whole(404, b""),
        _ => answer.whole(200, PALETTE),
    });
    let fixture = Fixture::new("setup-failed", transport);
    let owner = fixture.start();
    let consent = setup_install(&owner, "object").unwrap_err().data.unwrap()["consent"].clone();
    grant_all(&owner, &consent);
    let queued = setup_install(&owner, "object").unwrap();
    for job in queued["jobs"].as_array().unwrap() {
        owner.finished(&job["job_id"]);
    }
    let failed = setup(&owner, "object");
    assert_eq!(failed["state"], "failed");
    assert!(failed["error"]["message"].is_string(), "{failed}");
    assert_eq!(failed["parts"][0]["state"], "installed");
    assert_eq!(failed["parts"][1]["state"], "failed");
    assert_eq!(failed["missing_bytes"], json!(SWATCH.len()));
    let again = setup_install(&owner, "object").unwrap();
    assert_eq!(resources_of(&again), ["fill"]);
    owner.finished(&again["jobs"][0]["job_id"]);
}

#[test]
fn a_setup_this_host_cannot_run_is_refused_not_ready_by_name() {
    // Below the minimum: cannot-run, and refused once allowed.
    let mut fixture = Fixture::new("setup-small", serving());
    fixture.memory = Some(4 * GIB);
    let owner = fixture.start();
    let small = setup(&owner, "object");
    assert_eq!(
        (&small["state"], &small["reason"], &small["fits"]),
        (&json!("cannot-run"), &json!("memory"), &json!(false))
    );
    assert_eq!(setup(&owner, "masks")["state"], "missing");
    let consent = setup_install(&owner, "object").unwrap_err().data.unwrap()["consent"].clone();
    assert_eq!(
        consent["disclosure"]["fits"], false,
        "consent is asked first"
    );
    grant_all(&owner, &consent);
    let refused = setup_install(&owner, "object").unwrap_err();
    assert_eq!(refused.code, "not-ready");
    assert_eq!(
        refused.message,
        "Object needs a computer with at least 8 GB of memory; this one has 4 GB"
    );
    assert_eq!(
        refused.data.as_ref().unwrap()["requirements"][0]["kind"],
        "memory"
    );
    assert_eq!(fixture.transport.sends(), 0);
    drop(owner);

    // A fill whose model runs on a backend this host has no runtime for: unavailable.
    let mut fixture = Fixture::new("setup-runtime", serving());
    fixture.descriptor.capabilities.push(CapabilityDescriptor {
        id: "run-fill".into(),
        kind: CapabilityKind::LocalRuntime {
            resource: "fill".into(),
            model: ModelKind::Generator,
            resident_bytes: GIB,
            backends: vec!["metal".into()],
            precision: crate::inference::Precision::Fp32,
            with: Vec::new(),
        },
        purpose: "Fill.".into(),
    });
    let owner = fixture.start();
    let unavailable = setup(&owner, "object");
    assert_eq!(
        (&unavailable["state"], &unavailable["reason"]),
        (&json!("unavailable"), &json!("no-runtime"))
    );
    assert_eq!(unavailable["parts"][1]["state"], "unavailable");
    let consent = setup_install(&owner, "object").unwrap_err().data.unwrap()["consent"].clone();
    grant_all(&owner, &consent);
    let refused = setup_install(&owner, "object").unwrap_err();
    assert_eq!(refused.code, "not-ready");
    assert_eq!(
        refused.data.as_ref().unwrap()["requirements"][0],
        json!({"kind": "runtime", "id": "fill", "part": "Generative fill", "state": "unavailable"})
    );
    assert_eq!(fixture.transport.sends(), 0);
    // The finder alone needs no runtime.
    let masks = setup_install(&owner, "masks").unwrap();
    assert_eq!(resources_of(&masks), ["finder"]);
    owner.finished(&masks["jobs"][0]["job_id"]);
}

#[test]
fn an_unknown_setup_is_refused_by_name() {
    let fixture = Fixture::new("setup-unknown", serving());
    let owner = fixture.start();
    let refused = setup_install(&owner, "sky").unwrap_err();
    assert_eq!(refused.code, "validation");
    assert_eq!(
        refused.message,
        "module test.capabilities declares no setup sky"
    );
}
