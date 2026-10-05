//! A new import's first-open actions ([`crate::ToolModule::first_open`]): one ordinary `system`
//! entry after the Original, through the blocking service and the catalog owner alike; never again
//! for that asset; a refusal reported on the import job; and the readiness wait made on the source
//! worker, never on the owner.
use super::*;
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, CompileStage, EditorService, Layer, LayerReport,
    ModuleDescriptor, Orientation, PERSPECTIVE_EFFECT, Processing, Stage, StageContext, ToolModule,
    editor::mutation,
};
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};
use serde_json::Map;
use std::{fs, sync::Mutex, thread::ThreadId};

#[derive(Clone, Copy)]
enum Proposal {
    Perspective,
    Refuse,
}

/// Perspective as linked, with a first-open action: how any module that corrects a new photo
/// looks to the host. It records the threads it was asked and awaited on.
struct Probe {
    inner: Arc<dyn ToolModule>,
    proposal: Proposal,
    asked: Mutex<Vec<ThreadId>>,
    awaited: Mutex<Vec<ThreadId>>,
}

impl ToolModule for Probe {
    fn descriptor(&self) -> &ModuleDescriptor {
        self.inner.descriptor()
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        self.inner.parse(action_id, parameters)
    }
    fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error> {
        self.inner.plan(input, stage)
    }
    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error> {
        self.inner.validate_payload(effect_id, format, payload)
    }
    fn describe(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<LayerReport, Error> {
        self.inner.describe(effect_id, format, payload)
    }
    fn label(&self, action: &ActionDescriptor, input: &ActionInput) -> String {
        self.inner.label(action, input)
    }
    fn planned_label(&self, input: &ActionInput, layers: &[Layer], fallback: &str) -> String {
        self.inner.planned_label(input, layers, fallback)
    }
    fn settings(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<Map<String, Value>, Error> {
        self.inner.settings(effect_id, format, payload)
    }
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        at: CompileStage,
    ) -> Result<Processing, Error> {
        self.inner.compile(effect_id, format, payload, at)
    }
    fn carry(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        input: Stage,
        orientation: Orientation,
    ) -> Result<Option<Value>, Error> {
        self.inner
            .carry(effect_id, format, payload, input, orientation)
    }
    fn first_open(&self, context: &StageContext<'_>) -> Result<Option<ActionInput>, Error> {
        self.asked.lock().unwrap().push(std::thread::current().id());
        // A module decides from metadata only: the source is prepared, so optics answer.
        context.optics()?;
        match self.proposal {
            Proposal::Perspective if context.own_layer(PERSPECTIVE_EFFECT)?.is_some() => Ok(None),
            Proposal::Perspective => Ok(Some(ActionInput {
                action_id: "set-perspective".into(),
                parameters: Map::from_iter([("horizontal".into(), json!(20))]),
            })),
            Proposal::Refuse => Err(Error::not_ready("probe resource unavailable")),
        }
    }
    fn await_first_open(&self) {
        self.awaited
            .lock()
            .unwrap()
            .push(std::thread::current().id());
    }
}

fn registry(proposal: Proposal) -> (Arc<Probe>, Arc<ModuleRegistry>) {
    wrapping("luxforge.perspective", proposal)
}

/// The linked modules with the one named `id` wrapped in a probe.
fn wrapping(id: &str, proposal: Proposal) -> (Arc<Probe>, Arc<ModuleRegistry>) {
    let mut registry = ModuleRegistry::new();
    let mut probe = None;
    for module in crate::modules::linked_modules(false) {
        if module.descriptor().id == id {
            let wrapped = Arc::new(Probe {
                inner: module,
                proposal,
                asked: Mutex::new(Vec::new()),
                awaited: Mutex::new(Vec::new()),
            });
            probe = Some(wrapped.clone());
            registry.register(wrapped).unwrap();
        } else {
            registry.register(module).unwrap();
        }
    }
    (probe.unwrap(), Arc::new(registry))
}

fn perspective(recipe: &crate::Recipe) -> Option<&Layer> {
    recipe
        .layers
        .iter()
        .find(|layer| layer.effect_id == PERSPECTIVE_EFFECT)
}

#[test]
fn first_open_commits_one_system_entry_after_the_original_and_never_again() {
    let dir = temp_dir("first-open-service").canonicalize().unwrap();
    let source = dir.join("photo.jpg");
    fs::copy(fixture(), &source).unwrap();
    let original_bytes = fs::read(&source).unwrap();
    let catalog = dir.join("catalog.sqlite");
    let (probe, registry) = registry(Proposal::Perspective);
    let mut service = EditorService::open_with(&catalog, registry.clone()).unwrap();
    let state = service.import(&source).unwrap();
    let asset = state.asset.id.clone();
    let entry = &state.current_entry;
    assert_eq!(state.revision, 1);
    assert_eq!(
        (
            entry.sequence,
            entry.action_id.as_str(),
            entry.actor.as_str()
        ),
        (1, "set-perspective", "system")
    );
    assert_eq!(
        perspective(&entry.snapshot.recipe).unwrap().payload["horizontal"],
        20
    );
    let rows = service.history(&asset, None, 10).unwrap().entries;
    assert_eq!(rows.len(), 2);
    let original = rows.iter().find(|row| row.sequence == 0).unwrap();
    assert_eq!(original.action_id, "original");
    assert_eq!(entry.undo_parent.as_ref(), Some(&original.id));
    // The Original is the photo as imported; Undo returns to it.
    let stored = service.entry(&asset, &original.id).unwrap();
    assert!(perspective(&stored.snapshot.recipe).is_none());
    let undone = service.undo(&asset, mutation(1, "undo")).unwrap();
    let after_undo = service.state(&asset).unwrap();
    assert_eq!(after_undo.revision, undone.revision);
    assert!(perspective(&after_undo.current_entry.snapshot.recipe).is_none());
    // The same file imported again, and the catalog reopened, never ask again.
    let again = service.import(&source).unwrap();
    assert_eq!(again.asset.id, asset);
    assert_eq!(again.revision, after_undo.revision);
    drop(service);
    let mut reopened = EditorService::open_with(&catalog, registry).unwrap();
    reopened
        .prepare(&reopened.entry_needs(&asset, None).unwrap())
        .unwrap();
    assert_eq!(
        reopened.state(&asset).unwrap().revision,
        after_undo.revision
    );
    assert_eq!(probe.asked.lock().unwrap().len(), 1);
    // The blocking helper waits on its caller's thread, as the source worker does on its own.
    assert_eq!(
        *probe.awaited.lock().unwrap(),
        [std::thread::current().id()]
    );
    assert_eq!(fs::read(&source).unwrap(), original_bytes);
    drop(reopened);
    fs::remove_dir_all(dir).unwrap();
}

static NEXT: AtomicU64 = AtomicU64::new(1);

fn import_job(owner: &OwnerHandle, client: ClientId, path: &std::path::Path) -> Value {
    let call = |method: &str, params: Value| {
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: format!("{method}-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered");
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    };
    let queued = call(
        "catalog.import",
        json!({"path": path, "mutation": {
            "request_id": format!("import-{}", NEXT.fetch_add(1, Ordering::Relaxed)),
            "actor": "test",
        }}),
    );
    let job = queued["job_id"].clone();
    let record = luxforge_testbase::wait_for("the import job to settle", || {
        let status = call("job.read", json!({"job_id": job}));
        (!matches!(status["status"].as_str(), Some("queued" | "running"))).then_some(status)
    });
    assert_eq!(record["status"], "ready", "{record}");
    let adopted = call("job.adopt", json!({"job_id": job}));
    json!({"record": record, "adopted": adopted["asset"]})
}

#[test]
fn an_owner_import_reports_its_first_open_entry_and_waits_off_the_owner() {
    let dir = temp_dir("first-open-owner").canonicalize().unwrap();
    let source = dir.join("photo.jpg");
    fs::copy(fixture(), &source).unwrap();
    let (probe, registry) = registry(Proposal::Perspective);
    let (owner, join) = OwnerHandle::start_with(&dir.join("catalog.sqlite"), registry).unwrap();
    let client = owner.register();
    let imported = import_job(&owner, client, &source);
    let record = &imported["record"];
    let result = &record["result"];
    assert_eq!(result["revision"], 1);
    assert_eq!(result["current_entry"]["actor"], "system");
    assert_eq!(
        record["first_open"],
        json!([{
            "module_id": "luxforge.perspective",
            "action_id": "set-perspective",
            "entry_id": result["current_entry"]["id"],
            "label": result["current_entry"]["label"],
        }])
    );
    // job.adopt hands back the same state, with the first-open entry current.
    assert_eq!(imported["adopted"], *result);
    // The owner asked; the source worker, a different thread, awaited readiness first.
    let asked = probe.asked.lock().unwrap().clone();
    let awaited = probe.awaited.lock().unwrap().clone();
    assert_eq!((asked.len(), awaited.len()), (1, 1));
    assert_ne!(asked[0], awaited[0]);
    assert_ne!(asked[0], std::thread::current().id());
    // Importing the same file again creates nothing and reports nothing.
    let again = import_job(&owner, client, &source);
    assert!(again["record"].get("first_open").is_none());
    assert_eq!(again["record"]["result"]["revision"], 1);
    assert_eq!(probe.asked.lock().unwrap().len(), 1);
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_first_open_refusal_is_reported_and_the_import_still_completes() {
    let dir = temp_dir("first-open-refused").canonicalize().unwrap();
    let source = dir.join("photo.jpg");
    fs::copy(fixture(), &source).unwrap();
    let (_, registry) = registry(Proposal::Refuse);
    let (owner, join) = OwnerHandle::start_with(&dir.join("catalog.sqlite"), registry).unwrap();
    let client = owner.register();
    let imported = import_job(&owner, client, &source);
    let record = &imported["record"];
    assert_eq!(record["result"]["revision"], 0);
    assert_eq!(record["result"]["current_entry"]["action_id"], "original");
    assert_eq!(
        record["first_open"],
        json!([{
            "module_id": "luxforge.perspective",
            "error": {"code": "not-ready", "message": "probe resource unavailable"},
        }])
    );
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

/// The lens module, asked on first open, answers with a refusal, so whether it was asked shows in
/// the probe and on the import job alike.
fn lens_probe() -> (Arc<Probe>, Arc<ModuleRegistry>) {
    wrapping(crate::modules::LENS_MODULE, Proposal::Refuse)
}

#[test]
fn first_open_skips_the_lens_module_while_the_switch_is_off_and_asks_it_when_on() {
    let dir = temp_dir("first-open-lens-switch").canonicalize().unwrap();
    let source = dir.join("photo.jpg");
    fs::copy(fixture(), &source).unwrap();
    for enabled in [false, true] {
        let (probe, registry) = lens_probe();
        let catalog = dir.join(format!("catalog-{enabled}.sqlite"));
        let mut service = EditorService::open_with(&catalog, registry).unwrap();
        assert!(
            service.auto_lens_profile(),
            "on until the host says otherwise"
        );
        service.set_auto_lens_profile(enabled);
        let state = service.import(&source).unwrap();
        assert_eq!(state.revision, 0, "the probe's refusal commits nothing");
        assert_eq!(probe.asked.lock().unwrap().len(), usize::from(enabled));
        drop(service);
    }
    fs::remove_dir_all(dir).unwrap();
}

/// The owner sets the lens switch from the stored preference when it starts and again on each
/// `preferences.set`, and the switch applies to the next import.
#[test]
fn an_owner_import_follows_the_lens_preference_at_start_and_after_a_change() {
    let dir = temp_dir("first-open-lens-preference")
        .canonicalize()
        .unwrap();
    let config = dir.join("config");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("preferences.json"),
        br#"{"format":1,"auto_lens_profile":false}"#,
    )
    .unwrap();
    let (probe, registry) = lens_probe();
    let (owner, join) = OwnerHandle::start_with_host(
        &dir.join("catalog.sqlite"),
        registry,
        crate::HostConfig {
            preferences_dir: Some(config),
            ..crate::HostConfig::unconfigured()
        },
    )
    .unwrap();
    let client = owner.register();
    let first = dir.join("first.jpg");
    fs::copy(fixture(), &first).unwrap();
    let off = import_job(&owner, client, &first);
    assert!(off["record"].get("first_open").is_none(), "{off}");
    assert_eq!(off["record"]["result"]["revision"], 0);
    assert!(probe.asked.lock().unwrap().is_empty());

    let response = owner
        .call(
            client,
            ApiRequest {
                id: "lens-on".into(),
                method: "preferences.set".into(),
                params: json!({"auto_lens_profile": null}),
                token: None,
            },
        )
        .unwrap();
    assert_eq!(response.result.unwrap()["auto_lens_profile"], true);
    let second = dir.join("second.jpg");
    fs::copy(fixture(), &second).unwrap();
    let on = import_job(&owner, client, &second);
    assert_eq!(
        on["record"]["first_open"],
        json!([{
            "module_id": crate::modules::LENS_MODULE,
            "error": {"code": "not-ready", "message": "probe resource unavailable"},
        }])
    );
    assert_eq!(probe.asked.lock().unwrap().len(), 1);
    owner.stop();
    join.join().unwrap();
    fs::remove_dir_all(dir).unwrap();
}
