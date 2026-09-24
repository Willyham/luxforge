//! The capability surface against the real owner and the developer proof module: every gesture
//! goes through the update function, the operations it starts run through the same owner methods
//! an independent JSON client calls, and their answers come back through the update function. A
//! loopback [`ProofEndpoint`] stands in for the provider, and the secret store is in memory.
use super::{
    Boot, Editor,
    capabilities::{poll, run},
    evidence::{CapabilityAction, Step, parse_script},
    message::{CapabilityMessage, Message},
    tasks::{REQUEST_NUMBER, call, refresh},
    testing::{attach_log, logged},
};
use crate::{
    Config,
    state::{
        canvas::NoticeAction,
        capabilities::{
            CapabilityView, FieldKindModel, Operation, SecretText, TaskControlState, TaskPhase,
        },
        tools::ControlModel,
    },
};
use lightwell_core::{
    AssetId, HostConfig, ModuleDescriptor, OwnerHandle, ProofEndpoint,
    capabilities::{jobs::JobStatus, secrets::MemorySecretStore},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

const MODULE: &str = "lightwell.capabilities";
const TASK: &str = "generate-proof-tint";

/// An editor whose owner serves the proof module against a loopback endpoint, with one photograph
/// imported and shown.
struct Proof {
    editor: Editor,
    endpoint: ProofEndpoint,
    root: PathBuf,
    key: String,
    asset: AssetId,
}

impl Proof {
    fn start() -> Self {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
        );
        let key = format!("desktop-sentinel-{unique}");
        let endpoint = ProofEndpoint::start(&key).unwrap();
        let root = std::env::temp_dir().join(format!("lightwell-desktop-capabilities-{unique}"));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let registry = super::registry(&[], true, Some(&endpoint.base_url())).unwrap();
        let host = HostConfig {
            config_dir: Some(root.join("config")),
            resource_dir: Some(root.join("resources")),
            secrets: Arc::new(MemorySecretStore::new()),
            ..HostConfig::unconfigured()
        };
        let (owner, join) =
            OwnerHandle::start_with_host(&root.join("catalog.sqlite"), Arc::new(registry), host)
                .unwrap();
        let (mut editor, _) = Editor::new(Boot {
            owner: owner.clone(),
            join,
            live_server: None,
            config: Config {
                developer: true,
                ..Config::default()
            },
            window: (1440.0, 900.0),
        });
        let (mut listed, _) = call(&owner, editor.client, "module.list", json!({})).unwrap();
        let modules: Vec<ModuleDescriptor> =
            serde_json::from_value(listed["modules"].take()).unwrap();
        let _ = editor.update(Message::ModulesLoaded(Ok(modules)));
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0/orientation-1.jpg")
            .canonicalize()
            .unwrap();
        let client = editor.client;
        let (imported, _) = call(
            &owner,
            client,
            "catalog.import",
            json!({"path": fixture, "mutation": crate::app::tasks::request()}),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let asset: AssetId = loop {
            let (status, _) = call(
                &owner,
                client,
                "job.status",
                json!({"job_id": imported["job_id"]}),
            )
            .unwrap();
            if status["status"] == "ready" {
                break serde_json::from_value(status["asset"]["asset"]["id"].clone()).unwrap();
            }
            assert!(Instant::now() < deadline, "{status}");
            std::thread::sleep(Duration::from_millis(2));
        };
        call(
            &owner,
            client,
            "job.adopt",
            json!({"job_id": imported["job_id"]}),
        )
        .unwrap();
        let shown = refresh(&owner, client, asset.clone(), true, 0, None).unwrap();
        let _ = editor.update(Message::Refreshed(Ok(Box::new(shown))));
        Self {
            editor,
            endpoint,
            root,
            key,
            asset,
        }
    }

    fn send(&mut self, message: CapabilityMessage) {
        let _ = self.editor.update(Message::Capability(message));
    }

    /// Run every operation the update function started, exactly as the task would, and hand each
    /// answer back until nothing is left to run.
    fn answer(&mut self) -> Vec<Value> {
        let mut sent = Vec::new();
        loop {
            let started = std::mem::take(&mut self.editor.capability_started);
            if started.is_empty() {
                return sent;
            }
            for (module, op) in started {
                let answer = run(&self.editor.owner, self.editor.client, module, op);
                sent.extend(answer.sent.clone());
                self.send(CapabilityMessage::Answered(Box::new(answer)));
            }
        }
    }

    /// Poll the tracked live jobs, as the timer would, until none is left.
    fn finish_jobs(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.editor.capabilities.live() {
            assert!(Instant::now() < deadline, "a job never finished");
            let polled = poll(
                &self.editor.owner,
                self.editor.client,
                self.editor.capabilities.live_jobs(),
            );
            self.send(CapabilityMessage::Polled(polled));
            self.answer();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn section(&self) -> &crate::state::tools::SectionModel {
        self.editor
            .workspace
            .tools
            .all()
            .find(|section| section.module_id == MODULE)
            .expect("the proof section is listed in developer mode")
    }

    fn task_control(&self) -> crate::state::capabilities::TaskControl {
        self.section()
            .controls
            .iter()
            .find_map(|control| match control {
                ControlModel::Task(task) => Some(task.clone()),
                _ => None,
            })
            .expect("the proof declares a task control")
    }

    fn state(&self) -> &crate::state::capabilities::ModuleCapabilities {
        &self.editor.capabilities.modules[MODULE]
    }

    fn type_and_commit(&mut self, profile: Option<String>, field: &str, text: &str) {
        self.send(CapabilityMessage::FieldText {
            module_id: MODULE.into(),
            profile: profile.clone(),
            field: field.into(),
            text: text.into(),
        });
        self.send(CapabilityMessage::FieldCommit {
            module_id: MODULE.into(),
            profile,
            field: field.into(),
        });
    }

    /// Expand the section, fill every setting a task needs and install and activate, allowing each
    /// consent the core asks for.
    fn ready(&mut self) -> String {
        let _ = self.editor.update(Message::ToggleSection(MODULE.into()));
        self.answer();
        self.send(CapabilityMessage::ProfileLabel {
            module_id: MODULE.into(),
            label: "Local".into(),
        });
        self.send(CapabilityMessage::ProfileCreate(MODULE.into()));
        self.answer();
        let profile = self.state().profile_at(0).expect("a profile");
        let generate = self.endpoint.generate_url();
        self.type_and_commit(Some(profile.clone()), "endpoint", &generate);
        self.answer();
        self.send(CapabilityMessage::SecretEdit {
            module_id: MODULE.into(),
            profile: Some(profile.clone()),
            field: "api-key".into(),
        });
        self.send(CapabilityMessage::SecretText {
            module_id: MODULE.into(),
            text: SecretText::new(self.key.clone()),
        });
        self.send(CapabilityMessage::SecretCommit(MODULE.into()));
        self.answer();
        self.send(CapabilityMessage::Install {
            module_id: MODULE.into(),
            resource: "proof-palette".into(),
        });
        self.answer();
        self.send(CapabilityMessage::Consent(true));
        self.answer();
        self.finish_jobs();
        self.send(CapabilityMessage::Activate {
            module_id: MODULE.into(),
            on: true,
        });
        self.answer();
        self.finish_jobs();
        profile
    }

    fn stop(mut self) {
        self.editor.owner.stop();
        self.editor.owner_join.take().unwrap().join().unwrap();
        drop(self.editor);
        drop(self.endpoint);
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn a_section_reads_its_settings_and_status_once_it_is_expanded() {
    let mut proof = Proof::start();
    // Discovery and a collapsed developer section read nothing.
    assert!(proof.editor.capability_started.is_empty());
    assert!(
        proof
            .section()
            .capability
            .as_ref()
            .is_some_and(|model| model.loading)
    );
    let _ = proof.editor.update(Message::ToggleSection(MODULE.into()));
    assert_eq!(
        proof.editor.capability_started,
        vec![(MODULE.to_owned(), Operation::Load)],
        "expanding asks once, and only for this module"
    );
    let sent = proof.answer();
    let methods: Vec<&str> = sent
        .iter()
        .map(|request| request["method"].as_str().unwrap())
        .collect();
    assert_eq!(methods, ["module.settings.read", "module.status"]);
    let model = proof
        .section()
        .capability
        .clone()
        .expect("a capability block");
    assert!(!model.loading && model.enabled);
    assert_eq!(
        model.activation.map(|row| row.text),
        Some("Inactive".into())
    );
    assert_eq!(model.resources[0].state, "Not installed");
    assert_eq!(model.resources[0].detail, "v1 · 20 B");
    assert_eq!(model.permissions.summary, "0 permissions");
    // Collapsing and expanding again reads nothing more.
    let _ = proof.editor.update(Message::ToggleSection(MODULE.into()));
    let _ = proof.editor.update(Message::ToggleSection(MODULE.into()));
    assert!(proof.editor.capability_started.is_empty());
    // With nothing in flight there is no poll timer.
    assert!(proof.editor.capability_poll_subscription().is_none());
    proof.stop();
}

#[test]
fn settings_writes_send_the_envelope_and_show_refusals_and_conflicts_at_the_field() {
    let mut proof = Proof::start();
    let _ = proof.editor.update(Message::ToggleSection(MODULE.into()));
    proof.answer();
    proof.send(CapabilityMessage::Show {
        module_id: MODULE.into(),
        view: CapabilityView::Settings,
    });
    proof.type_and_commit(None, "strength", "0.8");
    assert_eq!(
        proof.editor.capability_started[0].1,
        Operation::Set {
            profile: None,
            field: "strength".into(),
            value: json!(0.8),
            revision: 0,
        }
    );
    let sent = proof.answer();
    assert_eq!(sent[0]["method"], "module.settings.set");
    assert_eq!(sent[0]["params"]["values"], json!({"strength": 0.8}));
    assert_eq!(sent[0]["params"]["mutation"]["expected_revision"], 0);
    assert!(
        sent[0]["params"]["mutation"]["request_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("desktop-"))
    );
    assert_eq!(proof.state().revision(), Some(1));
    let settings = proof
        .section()
        .capability
        .clone()
        .and_then(|model| model.settings)
        .expect("the settings sub-view");
    let strength = &settings.fields[0];
    assert!(
        matches!(&strength.kind, FieldKindModel::Number { display, typing: None, .. } if display == "0.80"),
        "{strength:?}"
    );
    // The core refuses a value outside the declared range, and says so under the field.
    proof.type_and_commit(None, "strength", "7");
    proof.answer();
    assert!(
        proof.state().errors[&(None, "strength".to_owned())].contains("0..=1"),
        "{:?}",
        proof.state().errors
    );
    // Another client writes first: the desktop's write is a conflict, the settings are read
    // again and the field says it changed elsewhere.
    let (_, _) = call(
        &proof.editor.owner,
        proof.editor.client,
        "module.settings.set",
        json!({"module_id": MODULE, "values": {"strength": 0.3},
            "mutation": {"expected_revision": 1, "request_id": "agent", "actor": "agent"}}),
    )
    .unwrap();
    proof.type_and_commit(None, "strength", "0.6");
    proof.answer();
    assert!(
        proof
            .state()
            .conflicts
            .contains(&(None, "strength".to_owned()))
    );
    assert_eq!(proof.state().revision(), Some(2), "the conflict re-read");
    let settings = proof
        .section()
        .capability
        .clone()
        .and_then(|model| model.settings)
        .unwrap();
    assert!(settings.fields[0].conflict);
    assert!(
        matches!(&settings.fields[0].kind, FieldKindModel::Number { display, .. } if display == "0.30")
    );
    proof.stop();
}

#[test]
fn the_whole_journey_goes_through_consent_jobs_and_apply_with_no_secret_anywhere() {
    let mut proof = Proof::start();
    let log = attach_log(&mut proof.editor);
    let _ = proof.editor.update(Message::ToggleSection(MODULE.into()));
    proof.answer();
    proof.send(CapabilityMessage::ProfileLabel {
        module_id: MODULE.into(),
        label: "Local".into(),
    });
    proof.send(CapabilityMessage::ProfileCreate(MODULE.into()));
    proof.answer();
    let profile = proof.state().profile_at(0).expect("a profile");
    let generate = proof.endpoint.generate_url();
    proof.type_and_commit(Some(profile.clone()), "endpoint", &generate);
    proof.answer();
    proof.send(CapabilityMessage::Show {
        module_id: MODULE.into(),
        view: CapabilityView::Settings,
    });
    let endpoint_field = proof
        .section()
        .capability
        .clone()
        .and_then(|model| model.settings)
        .map(|settings| settings.profiles[0].fields[0].kind.clone());
    assert!(
        matches!(&endpoint_field, Some(FieldKindModel::Text { class: Some(class), .. }) if class == "loopback"),
        "{endpoint_field:?}"
    );
    // The secret goes out once, redacted in everything the desktop records.
    proof.send(CapabilityMessage::SecretEdit {
        module_id: MODULE.into(),
        profile: Some(profile.clone()),
        field: "api-key".into(),
    });
    proof.send(CapabilityMessage::SecretText {
        module_id: MODULE.into(),
        text: SecretText::new(proof.key.clone()),
    });
    assert!(
        !proof.editor.snapshot().to_string().contains(&proof.key),
        "a typed secret is not in the captured state"
    );
    proof.send(CapabilityMessage::SecretCommit(MODULE.into()));
    let sent = proof.answer();
    assert_eq!(sent[0]["method"], "module.settings.set-secret");
    assert_eq!(sent[0]["params"]["value"], "<redacted>");
    assert!(proof.state().secret.is_none(), "the masked input closed");
    let summary = proof.editor.snapshot()["capabilities"][MODULE].clone();
    assert_eq!(
        summary["settings"]["profiles"][0]["fields"]["api-key"],
        "set"
    );
    assert_eq!(summary["settings"]["profiles"][0]["status"], "ready");
    // Install is refused for consent: the notice names what would happen.
    proof.send(CapabilityMessage::Install {
        module_id: MODULE.into(),
        resource: "proof-palette".into(),
    });
    proof.answer();
    let notice = proof.editor.workspace.canvas.notices[0].clone();
    assert_eq!(
        notice.title,
        "Allow Capabilities proof to download a resource?"
    );
    assert!(
        notice.body.contains("Proof palette 1 · 20 B"),
        "{}",
        notice.body
    );
    assert!(!notice.body.contains("declined"));
    assert_eq!(
        notice.actions,
        vec![
            ("Allow".to_owned(), NoticeAction::AllowConsent),
            ("Don't allow".to_owned(), NoticeAction::DenyConsent),
        ]
    );
    // Don't allow records the denial and leaves the module usable.
    proof.send(CapabilityMessage::Consent(false));
    let sent = proof.answer();
    assert_eq!(sent[0]["method"], "module.permission.deny");
    assert!(proof.editor.workspace.canvas.notices.is_empty());
    assert!(
        proof
            .section()
            .capability
            .as_ref()
            .is_some_and(|model| model.permissions.summary == "0 permissions · 1 declined")
    );
    proof.send(CapabilityMessage::Install {
        module_id: MODULE.into(),
        resource: "proof-palette".into(),
    });
    proof.answer();
    assert!(
        proof.editor.workspace.canvas.notices[0]
            .body
            .ends_with("You declined this before.")
    );
    // Allow grants exactly the refused scope and retries the install once.
    proof.endpoint.set_palette_delay(Duration::from_secs(10));
    proof.send(CapabilityMessage::Consent(true));
    let sent = proof.answer();
    assert_eq!(sent[0]["method"], "module.permission.grant");
    assert_eq!(
        sent[0]["params"]["scope"],
        json!({"resource": "proof-palette", "version": "1", "origin": proof.endpoint.base_url()})
    );
    assert_eq!(sent[1]["method"], "module.resource.install");
    assert!(proof.editor.capabilities.live(), "the install is followed");
    assert!(proof.editor.capability_poll_subscription().is_some());
    // The transfer lane reports its first progress asynchronously; poll briefly rather than
    // assuming it has already done so by the time the status round trip above answered.
    let deadline = Instant::now() + Duration::from_secs(5);
    let installing = loop {
        let installing = proof.section().capability.clone().unwrap().resources[0].clone();
        if installing
            .progress
            .is_some_and(|fraction| (0.0..=1.0).contains(&fraction))
        {
            break installing;
        }
        assert!(
            Instant::now() < deadline,
            "the install never reported progress: {installing:?}"
        );
        let polled = poll(
            &proof.editor.owner,
            proof.editor.client,
            proof.editor.capabilities.live_jobs(),
        );
        proof.send(CapabilityMessage::Polled(polled));
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(installing.state.starts_with("Installing"), "{installing:?}");
    proof.endpoint.set_palette_delay(Duration::ZERO);
    proof.finish_jobs();
    assert!(proof.editor.capability_poll_subscription().is_none());
    assert_eq!(
        proof.section().capability.clone().unwrap().resources[0].state,
        "Installed"
    );
    proof.send(CapabilityMessage::Activate {
        module_id: MODULE.into(),
        on: true,
    });
    proof.answer();
    proof.finish_jobs();
    assert_eq!(
        proof
            .section()
            .capability
            .clone()
            .unwrap()
            .activation
            .unwrap()
            .text,
        "Active"
    );
    // The task sends the open asset and the only ready profile, asks for the photo-data consent
    // and, once allowed, runs to a result Apply can commit.
    assert_eq!(
        proof.task_control().profile.as_deref(),
        Some(profile.as_str())
    );
    proof.send(CapabilityMessage::RunTask {
        module_id: MODULE.into(),
        task: TASK.into(),
    });
    assert_eq!(
        proof.editor.capability_started[0].1,
        Operation::RunTask {
            task: TASK.into(),
            asset: Some(proof.asset.clone()),
            profile: Some(profile.clone()),
        }
    );
    proof.answer();
    assert_eq!(proof.task_control().state, TaskControlState::Consent);
    assert_eq!(
        proof.editor.workspace.canvas.notices[0].title,
        "Allow Capabilities proof to send photo data?"
    );
    proof.send(CapabilityMessage::Consent(true));
    proof.answer();
    proof.finish_jobs();
    let control = proof.task_control();
    let TaskControlState::Succeeded {
        apply: Some((action, preset)),
        ..
    } = &control.state
    else {
        panic!("the task succeeded with an apply: {:?}", control.state);
    };
    assert_eq!(action, "apply-proof-tint");
    let artifact = preset["artifact"].as_str().unwrap().to_owned();
    assert!(artifact.starts_with("artifact-"));
    let summary = proof.editor.snapshot()["capabilities"][MODULE].clone();
    assert_eq!(summary["tasks"][TASK]["status"], "ready");
    assert_eq!(summary["tasks"][TASK]["artifact"], artifact.as_str());
    assert_eq!(summary["tasks"][TASK]["apply_available"], true);
    assert_eq!(summary["permissions"]["live"], 2);
    // Apply is the ordinary edit path: one command with the task's artifact, the request an
    // independent client sends.
    let request = proof
        .editor
        .request_for_preset(action, None, Some(preset))
        .expect("the apply request");
    assert_eq!(request["method"], "edit.apply-proof-tint");
    assert_eq!(request["params"]["asset_id"], json!(proof.asset));
    assert_eq!(request["params"]["artifact"], artifact.as_str());
    proof.send(CapabilityMessage::Apply {
        module_id: MODULE.into(),
        task: TASK.into(),
    });
    assert!(proof.editor.busy, "{}", proof.editor.status);
    assert_eq!(proof.editor.status, "Running edit.apply-proof-tint…");
    // Once the commit is read back, the control says the result is applied and offers no Apply.
    let client = proof.editor.client;
    let owner = proof.editor.owner.clone();
    let (applied, sequence) = call(
        &owner,
        client,
        "edit.apply-proof-tint",
        request["params"].clone(),
    )
    .unwrap();
    assert_eq!(applied["outcome"], "applied");
    let shown = refresh(&owner, client, proof.asset.clone(), false, sequence, None).unwrap();
    let _ = proof.editor.update(Message::Refreshed(Ok(Box::new(shown))));
    assert!(matches!(
        &proof.task_control().state,
        TaskControlState::Succeeded { summary, apply: None } if summary.starts_with("Applied")
    ));
    assert_eq!(
        proof.editor.snapshot()["capabilities"][MODULE]["tasks"][TASK]["applied"],
        true
    );
    // A revocation is shown in the permissions list.
    let grant = proof.state().grants()[1].grant_id.clone();
    proof.send(CapabilityMessage::Revoke {
        module_id: MODULE.into(),
        grant,
    });
    proof.answer();
    let summary = proof.editor.snapshot()["capabilities"][MODULE].clone();
    assert_eq!(summary["permissions"]["revoked"], 1);
    // Nothing the desktop recorded holds the key.
    let events = logged(&mut proof.editor, &log);
    let recorded = serde_json::to_string(&events).unwrap();
    assert!(
        !recorded.contains(&proof.key),
        "the event log holds the key"
    );
    assert!(
        recorded.contains("\"<redacted>\""),
        "the secret's request is logged redacted"
    );
    assert!(!proof.editor.snapshot().to_string().contains(&proof.key));
    proof.stop();
}

#[test]
fn a_task_result_belongs_to_its_asset_and_a_wrong_key_fails_the_job() {
    let mut proof = Proof::start();
    let profile = proof.ready();
    // A wrong key reaches the endpoint, which answers 401: the job fails with its error.
    proof.send(CapabilityMessage::SecretEdit {
        module_id: MODULE.into(),
        profile: Some(profile),
        field: "api-key".into(),
    });
    proof.send(CapabilityMessage::SecretText {
        module_id: MODULE.into(),
        text: SecretText::new("wrong-key".into()),
    });
    proof.send(CapabilityMessage::SecretCommit(MODULE.into()));
    proof.answer();
    proof.send(CapabilityMessage::RunTask {
        module_id: MODULE.into(),
        task: TASK.into(),
    });
    proof.answer();
    proof.send(CapabilityMessage::Consent(true));
    proof.answer();
    proof.finish_jobs();
    assert_eq!(
        proof.task_control().state,
        TaskControlState::Failed("read-error: proof-echo answered 401".into())
    );
    let job = proof.state().jobs.last().cloned().expect("the task's job");
    assert_eq!(job.status, JobStatus::Failed);
    // Another asset clears the run.
    proof.editor.capabilities_asset_changed(&AssetId::new());
    assert!(proof.state().tasks.is_empty());
    proof.stop();
}

#[test]
fn cancel_and_deactivate_go_through_the_job_and_activation_methods() {
    let mut proof = Proof::start();
    proof.ready();
    proof.endpoint.set_delay(Duration::from_secs(10));
    proof.send(CapabilityMessage::RunTask {
        module_id: MODULE.into(),
        task: TASK.into(),
    });
    proof.answer();
    proof.send(CapabilityMessage::Consent(true));
    proof.answer();
    let job = proof
        .state()
        .newest_live()
        .map(|job| job.job_id.as_str().to_owned())
        .expect("a running task");
    assert!(matches!(
        proof.task_control().state,
        TaskControlState::Running { .. }
    ));
    proof.send(CapabilityMessage::Cancel {
        module_id: MODULE.into(),
        job: job.clone(),
    });
    let sent = proof.answer();
    assert_eq!(sent[0]["method"], "module.job.cancel");
    assert_eq!(sent[0]["params"]["job_id"], json!(job));
    assert_eq!(sent[0]["params"]["mutation"]["actor"], "desktop");
    proof.endpoint.set_delay(Duration::ZERO);
    proof.finish_jobs();
    assert!(matches!(
        &proof.state().tasks[TASK].phase,
        TaskPhase::Failed { code, .. } if code == "cancelled"
    ));
    proof.send(CapabilityMessage::Activate {
        module_id: MODULE.into(),
        on: false,
    });
    let sent = proof.answer();
    assert_eq!(sent[0]["method"], "module.deactivate");
    proof.finish_jobs();
    assert_eq!(
        proof
            .section()
            .capability
            .clone()
            .unwrap()
            .activation
            .unwrap()
            .text,
        "Inactive"
    );
    proof.stop();
}

#[test]
fn capability_steps_parse_strictly_and_record_a_secret_as_redacted() {
    let steps = parse_script(
        &json!([
            {"capability": {"module": MODULE, "section": "settings"}},
            {"capability": {"module": MODULE, "set": {"field": "strength", "value": 0.8}}},
            {"capability": {"module": MODULE, "set": {"field": "endpoint", "value": "http://127.0.0.1:1/generate", "profile": 0}}},
            {"capability": {"module": MODULE, "secret": {"field": "api-key", "value": "script-sentinel", "profile": 0}}},
            {"capability": {"module": MODULE, "profile": {"create": {"adapter": "proof-echo", "label": "Local"}}}},
            {"capability": {"module": MODULE, "profile": {"remove": 0}}},
            {"capability": {"module": MODULE, "install": {"resource": "proof-palette"}}},
            {"capability": {"module": MODULE, "remove": {"resource": "proof-palette"}}},
            {"capability": {"module": MODULE, "activate": false}},
            {"capability": {"module": MODULE, "task": {"task": TASK}}},
            {"capability": {"module": MODULE, "consent": "allow", "wait": false}},
            {"capability": {"module": MODULE, "apply": true}},
            {"capability": {"module": MODULE, "cancel": true}},
            {"capability": {"module": MODULE, "revoke": 2}},
            {"capability": {"module": MODULE, "settle": true}}
        ])
        .to_string(),
    )
    .expect("a valid script");
    assert_eq!(steps.len(), 15);
    let Step::Capability(secret) = &steps[3] else {
        panic!("a capability step");
    };
    assert!(matches!(
        &secret.action,
        CapabilityAction::Secret { value, profile: Some(0), .. } if value.expose() == "script-sentinel"
    ));
    let recorded = steps[3].record().to_string();
    assert!(!recorded.contains("script-sentinel"), "{recorded}");
    assert_eq!(
        steps[3].record(),
        json!({"capability": {"module": MODULE, "secret": {"field": "api-key", "value": "<redacted>", "profile": 0}}})
    );
    assert!(!format!("{:?}", steps[3]).contains("script-sentinel"));
    // A raw API step that stores a secret is recorded redacted too.
    let raw = parse_script(
        &json!([{"api": {"method": "module.settings.set-secret", "params": {"module_id": MODULE, "setting": "api-key", "value": "script-sentinel"}}}])
            .to_string(),
    )
    .expect("a valid script");
    assert_eq!(raw[0].record()["api"]["params"]["value"], "<redacted>");
    assert_eq!(
        steps[10].record(),
        json!({"capability": {"module": MODULE, "consent": "allow", "wait": false}})
    );
    for (script, expected) in [
        (json!({"capability": {"module": MODULE}}), "exactly one of"),
        (
            json!({"capability": {"module": MODULE, "apply": true, "cancel": true}}),
            "exactly one of",
        ),
        (
            json!({"capability": {"section": "status"}}),
            "needs a module",
        ),
        (
            json!({"capability": {"module": MODULE, "section": "elsewhere"}}),
            "\"status\" or \"settings\"",
        ),
        (
            json!({"capability": {"module": MODULE, "consent": "maybe"}}),
            "\"allow\" or \"deny\"",
        ),
        (
            json!({"capability": {"module": MODULE, "revoke": -1}}),
            "non-negative index",
        ),
        (
            json!({"capability": {"module": MODULE, "wait": "no", "apply": true}}),
            "true or false",
        ),
        (
            json!({"capability": {"module": MODULE, "nowhere": 1}}),
            "unknown capability field",
        ),
        (
            json!({"capability": {"module": MODULE, "secret": {"field": "api-key", "value": "leaky", "extra": 1}}}),
            "unknown capability secret field",
        ),
    ] {
        let error = parse_script(&json!([script]).to_string()).expect_err(expected);
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("leaky"), "a refusal never echoes a value");
    }
}

#[test]
fn a_scripted_gesture_is_captured_only_once_its_round_trip_has_answered() {
    let mut proof = Proof::start();
    let _ = proof.editor.update(Message::ToggleSection(MODULE.into()));
    proof.answer();
    proof.editor.evidence = Some(crate::app::testing::scripted_evidence(
        r#"[{"capability":{"module":"lightwell.capabilities","set":{"field":"strength","value":0.8}}},
            {"capability":{"module":"lightwell.capabilities","settle":true}}]"#,
    ));
    let _ = proof.editor.next_step();
    // Typing and Enter are one gesture: nothing is captured between them or before the answer.
    let evidence = proof.editor.evidence.as_ref().unwrap();
    assert!(
        !evidence.capture_pending,
        "captured before the write answered"
    );
    assert_eq!(evidence.awaiting, Some(super::evidence::Settle::Capability));
    assert_eq!(proof.state().view, CapabilityView::Settings);
    proof.answer();
    let evidence = proof.editor.evidence.as_ref().unwrap();
    assert!(evidence.capture_pending && evidence.capability_wait.is_none());
    assert_eq!(proof.state().revision(), Some(1));
    // A settle with nothing in flight captures at once.
    proof.editor.evidence.as_mut().unwrap().capture_pending = false;
    let _ = proof.editor.next_step();
    assert!(proof.editor.evidence.as_ref().unwrap().capture_pending);
    proof.stop();
}

#[test]
fn a_scripted_step_that_sends_nothing_is_recorded_and_captured() {
    let mut proof = Proof::start();
    proof.editor.evidence = Some(crate::app::testing::scripted_evidence(
        r#"[{"capability":{"module":"lightwell.capabilities","consent":"allow"}}]"#,
    ));
    let _ = proof.editor.next_step();
    let evidence = proof.editor.evidence.as_ref().unwrap();
    assert!(evidence.had_errors && evidence.capture_pending);
    assert!(evidence.capability_wait.is_none());
    assert!(
        proof.editor.status.contains("no consent notice is open"),
        "{}",
        proof.editor.status
    );
    proof.stop();
}

#[test]
fn only_capability_events_ask_for_capability_reads() {
    use super::tasks::capability_event;
    assert!(capability_event("module.settings.set"));
    assert!(capability_event("module.permission.revoke"));
    assert!(capability_event("task.generate-proof-tint"));
    assert!(!capability_event("edit.apply-proof-tint"));
    assert!(!capability_event("catalog.import"));
}
