//! The capability surface against the real owner and the developer proof module: every gesture
//! goes through the update function, the operations it starts run through the same owner methods
//! an independent JSON client calls, and their answers come back through the update function. What
//! the block does not draw — profiles, secrets, a single grant — is set up through those methods
//! directly, as any client sets it. A loopback [`ProofEndpoint`] stands in for the provider, and
//! the secret store is in memory.
use super::{
    Boot, Editor,
    capabilities::{poll, run},
    evidence::{Settle, Step, parse_script, record},
    message::{CapabilityMessage, ControlMessage, Message, SyncMessage},
    tasks::{ACTOR, HostAnswer, REQUEST_NUMBER, Scope, call, refresh, request},
    testing::{attach_log, logged},
};
use crate::{
    Config,
    state::{
        canvas::NoticeAction,
        capabilities::{FieldKindModel, Operation, TaskControlState, TaskPhase},
        tools::ControlModel,
    },
};
use luxforge_core::{
    AssetId, HostConfig, ModuleDescriptor, OwnerHandle, capabilities::secrets::MemorySecretStore,
    jobs::JobStatus,
};
use luxforge_testbase::{wait_for, wait_until};
use luxforge_testkit::ProofEndpoint;
use serde_json::{Map, Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
};

const MODULE: &str = "luxforge.capabilities";
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
        let root = std::env::temp_dir().join(format!("luxforge-desktop-capabilities-{unique}"));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let registry = luxforge_core::ModuleRegistry::assemble(&luxforge_core::RegistryOptions {
            developer: true,
            proof_endpoint: Some(&endpoint.base_url()),
            ..luxforge_core::RegistryOptions::default()
        })
        .unwrap();
        let host = HostConfig {
            config_dir: Some(root.join("config")),
            resource_dir: Some(root.join("resources")),
            secrets: Arc::new(MemorySecretStore::new()),
            transport: Arc::new(luxforge_net::HttpTransport::system()),
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
            client: None,
            initial_import: None,
            window: (1440.0, 900.0),
        });
        let (mut listed, _) = call(&owner, editor.client, "module.list", json!({})).unwrap();
        let modules: Vec<ModuleDescriptor> =
            serde_json::from_value(listed["modules"].take()).unwrap();
        let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(modules))));
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0/orientation-1.jpg")
            .canonicalize()
            .unwrap();
        let client = editor.client;
        let (imported, _) = call(
            &owner,
            client,
            "catalog.import",
            json!({"path": fixture, "mutation": request()}),
        )
        .unwrap();
        let asset: AssetId = wait_for("the import", || {
            let (status, _) = call(
                &owner,
                client,
                "job.read",
                json!({"job_id": imported["job_id"]}),
            )
            .unwrap();
            (status["status"] == "ready")
                .then(|| serde_json::from_value(status["result"]["asset"]["id"].clone()).unwrap())
        });
        call(
            &owner,
            client,
            "job.adopt",
            json!({"job_id": imported["job_id"]}),
        )
        .unwrap();
        let shown = refresh(&owner, client, asset.clone(), Scope::Open, None).unwrap();
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(shown)))));
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
        wait_until("every tracked job finishing", || {
            if !self.editor.capabilities.live() {
                return true;
            }
            let polled = poll(
                &self.editor.owner,
                self.editor.client,
                self.editor.capabilities.live_jobs(),
            );
            self.send(CapabilityMessage::Polled(polled));
            self.answer();
            false
        });
    }

    /// Expand the proof section, which reads its settings and status once.
    fn expand(&mut self) {
        let _ = self
            .editor
            .update(Message::Control(ControlMessage::ToggleSection(
                MODULE.into(),
            )));
        self.answer();
    }

    /// One owner request of this desktop's client, as any client sends it.
    fn api(&self, method: &str, params: Value) -> Value {
        call(&self.editor.owner, self.editor.client, method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
            .0
    }

    /// The settings envelope another client sends: the revision it has just read.
    fn settings_mutation(&self) -> Value {
        let read = self.api("module.settings.read", json!({"module_id": MODULE}));
        json!({"expected_revision": read["revision"], "request_id": format!("agent-{}", REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)), "actor": "agent"})
    }

    /// A profile with the endpoint and `key`, created through the API, and the block read again.
    fn profile(&mut self, key: &str) -> String {
        let created = self.api(
            "module.profile.create",
            json!({"module_id": MODULE, "adapter": "proof-echo", "label": "Local", "mutation": self.settings_mutation()}),
        );
        let profile = created["profile"]["id"].as_str().unwrap().to_owned();
        self.api(
            "module.settings.set",
            json!({"module_id": MODULE, "profile_id": profile, "values": {"endpoint": self.endpoint.generate_url()}, "mutation": self.settings_mutation()}),
        );
        self.set_key(&profile, key);
        profile
    }

    fn set_key(&mut self, profile: &str, key: &str) {
        self.api(
            "module.settings.set-secret",
            json!({"module_id": MODULE, "profile_id": profile, "setting": "api-key", "value": key, "mutation": self.settings_mutation()}),
        );
        let _ = self.editor.reload_capabilities();
        self.answer();
    }

    /// Expand the section, give it a ready profile and install its resource through the API.
    fn ready(&mut self) -> String {
        self.expand();
        let profile = self.profile(&self.key.clone());
        self.api(
            "module.permission.grant",
            json!({"module_id": MODULE, "capability": "palette", "scope": {"resource": "proof-palette", "version": "1", "origin": self.endpoint.base_url()}, "mutation": request()}),
        );
        let install = self.api(
            "module.resource.install",
            json!({"module_id": MODULE, "resource_id": "proof-palette", "mutation": request()}),
        );
        wait_until("the install", || {
            self.api("job.read", json!({"job_id": install["job_id"]}))["status"] == "ready"
        });
        let _ = self.editor.reload_capabilities();
        self.answer();
        profile
    }

    fn section(&self) -> &crate::state::tools::SectionModel {
        self.editor
            .workspace
            .tools
            .all()
            .find(|section| section.module_id == MODULE)
            .expect("the proof section is listed in developer mode")
    }

    fn block(&self) -> crate::state::capabilities::CapabilityModel {
        self.section()
            .capability
            .clone()
            .expect("a capability block")
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
        self.editor.capabilities.module(MODULE)
    }

    fn type_and_commit(&mut self, field: &str, text: &str) {
        self.send(CapabilityMessage::FieldText {
            module_id: MODULE.into(),
            field: field.into(),
            text: text.into(),
        });
        self.send(CapabilityMessage::FieldCommit {
            module_id: MODULE.into(),
            field: field.into(),
        });
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
    assert!(proof.block().loading);
    let _ = proof
        .editor
        .update(Message::Control(ControlMessage::ToggleSection(
            MODULE.into(),
        )));
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
    let model = proof.block();
    assert!(!model.loading && model.enabled);
    assert_eq!(model.resources[0].state, "Not installed");
    assert_eq!(model.resources[0].detail, "v1 · 20 B");
    assert_eq!(model.permissions, "0 permissions");
    assert!(!model.revoke_all, "nothing to revoke");
    // The form holds the module-level setting, drawn as a number field; the profile block's
    // endpoint and secret are set through the API.
    assert_eq!(model.fields.len(), 1);
    assert_eq!(model.fields[0].label, "Strength");
    assert!(
        matches!(&model.fields[0].kind, FieldKindModel::Number { display, typing: None } if display == "0.50"),
        "{:?}",
        model.fields[0]
    );
    // The section's own controls are drawn under the block.
    assert!(proof.section().shows_controls());
    // Collapsing and expanding again reads nothing more.
    for _ in 0..2 {
        let _ = proof
            .editor
            .update(Message::Control(ControlMessage::ToggleSection(
                MODULE.into(),
            )));
    }
    assert!(proof.editor.capability_started.is_empty());
    // With nothing in flight there is no poll timer.
    assert!(proof.editor.capability_poll_subscription().is_none());
    proof.stop();
}

#[test]
fn a_settings_conflict_reads_the_settings_again_and_says_so_on_one_status_line() {
    let mut proof = Proof::start();
    proof.expand();
    proof.type_and_commit("strength", "0.8");
    assert_eq!(
        proof.editor.capability_started[0].1,
        Operation::Set {
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
    assert!(
        matches!(&proof.block().fields[0].kind, FieldKindModel::Number { display, typing: None } if display == "0.80")
    );
    assert_eq!(proof.block().status_line, None);
    // A value outside the declared range is read back against the setting's parameter exactly as
    // a module control's text is: nothing is sent, and the status line says why.
    proof.type_and_commit("strength", "7");
    assert!(proof.answer().is_empty(), "a refused value is not sent");
    assert_eq!(
        proof.block().status_line.as_deref(),
        Some("strength must be a number from 0 to 1")
    );
    // Another client writes first: the desktop's write is a conflict, the settings are read
    // again, the form shows the other client's value and one status line says what happened.
    proof.api(
        "module.settings.set",
        json!({"module_id": MODULE, "values": {"strength": 0.3}, "mutation": proof.settings_mutation()}),
    );
    proof.type_and_commit("strength", "0.6");
    let sent = proof.answer();
    let methods: Vec<&str> = sent
        .iter()
        .map(|request| request["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "module.settings.set",
            "module.settings.read",
            "module.status"
        ]
    );
    assert_eq!(proof.state().revision(), Some(2), "the conflict re-read");
    let block = proof.block();
    assert!(
        matches!(&block.fields[0].kind, FieldKindModel::Number { display, typing: None } if display == "0.30"),
        "{:?}",
        block.fields[0]
    );
    let line = block.status_line.expect("one status line");
    assert!(
        line.starts_with("Changed elsewhere, so the settings were read again: "),
        "{line}"
    );
    assert_eq!(proof.editor.status, line);
    assert_eq!(
        proof.editor.snapshot()["capabilities"][MODULE]["status_line"],
        line.as_str()
    );
    // The next write is made against the re-read revision and clears the line.
    proof.type_and_commit("strength", "0.6");
    proof.answer();
    assert_eq!(proof.state().revision(), Some(3));
    assert_eq!(proof.block().status_line, None);
    proof.stop();
}

#[test]
fn consent_install_task_and_apply_go_through_the_notice_and_revoke_all_withdraws_every_grant() {
    let mut proof = Proof::start();
    let log = attach_log(&mut proof.editor);
    proof.expand();
    let profile = proof.profile(&proof.key.clone());
    let summary = proof.editor.snapshot()["capabilities"][MODULE].clone();
    assert_eq!(
        summary["settings"]["profiles"][0]["fields"]["api-key"],
        json!({"secret_present": true, "valid": true}),
        "the summary is the core's own read: a secret only as whether it is set"
    );
    assert_eq!(summary["settings"]["profiles"][0]["status"], "ready");
    // Download is refused for consent: the notice names what would happen.
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
    assert_eq!(proof.block().permissions, "0 permissions · 1 declined");
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
    proof.endpoint.palette().shut();
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
    // The transfer lane reports its first progress asynchronously, and the download stays held at
    // the endpoint's palette gate until the test opens it, so the install is observed running.
    let installing = wait_for("the install reporting progress", || {
        let installing = proof.block().resources[0].clone();
        if installing
            .progress
            .is_some_and(|fraction| (0.0..=1.0).contains(&fraction))
        {
            return Some(installing);
        }
        let polled = poll(
            &proof.editor.owner,
            proof.editor.client,
            proof.editor.capabilities.live_jobs(),
        );
        proof.send(CapabilityMessage::Polled(polled));
        None
    });
    assert!(installing.state.starts_with("Installing"), "{installing:?}");
    proof.endpoint.palette().open();
    proof.finish_jobs();
    assert!(proof.editor.capability_poll_subscription().is_none());
    assert_eq!(proof.block().resources[0].state, "Installed");
    // The task sends the open asset and the ready profile, asks for the photo-data consent and,
    // once allowed, runs to a result Apply can commit.
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
    let sent = proof.answer();
    let task = sent
        .iter()
        .find(|request| request["method"] == format!("task.{TASK}"))
        .expect("the task request");
    // It carries the request envelope, so a retry of it starts no second job.
    assert!(
        task["params"]["mutation"]["request_id"].is_string(),
        "{task}"
    );
    assert_eq!(task["params"]["mutation"]["actor"], ACTOR);
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
    let (applied, _) = call(
        &proof.editor.owner,
        proof.editor.client,
        "edit.apply-proof-tint",
        request["params"].clone(),
    )
    .unwrap();
    assert_eq!(applied["outcome"], "applied");
    let scope = Scope::after("edit.apply-proof-tint", &applied);
    let shown = refresh(
        &proof.editor.owner,
        proof.editor.client,
        proof.asset.clone(),
        scope,
        None,
    )
    .unwrap();
    let _ = proof
        .editor
        .update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(shown)))));
    assert!(matches!(
        &proof.task_control().state,
        TaskControlState::Succeeded { summary, apply: None } if summary.starts_with("Applied")
    ));
    assert_eq!(
        proof.editor.snapshot()["capabilities"][MODULE]["tasks"][TASK]["applied"],
        true
    );
    // Revoke all lists the module's grants and withdraws each live one through the one revoke
    // method, and the counts read after it say so.
    let block = proof.block();
    assert_eq!(
        block.permissions, "2 permissions",
        "the grant cleared the denial"
    );
    assert!(block.revoke_all);
    proof.send(CapabilityMessage::RevokeAll(MODULE.into()));
    assert_eq!(proof.editor.capability_started[0].1, Operation::RevokeAll);
    let sent = proof.answer();
    let methods: Vec<&str> = sent
        .iter()
        .map(|request| request["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "module.permission.list",
            "module.permission.revoke",
            "module.permission.revoke",
            "module.status"
        ]
    );
    assert!(sent[1]["params"]["mutation"]["request_id"].is_string());
    let block = proof.block();
    assert_eq!(block.permissions, "0 permissions · 2 revoked");
    assert!(!block.revoke_all, "nothing is left to revoke");
    assert_eq!(
        proof.editor.status,
        "Revoked 2 permission(s) of luxforge.capabilities"
    );
    let listed = proof.api("module.permission.list", json!({"module_id": MODULE}));
    assert!(
        listed["grants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|grant| !grant["revoked"].is_null()),
        "{listed}"
    );
    // Nothing the desktop recorded holds the key.
    let events = logged(&mut proof.editor, &log);
    assert!(!serde_json::to_string(&events).unwrap().contains(&proof.key));
    assert!(!proof.editor.snapshot().to_string().contains(&proof.key));
    proof.stop();
}

#[test]
fn a_task_result_belongs_to_its_asset_and_a_wrong_key_fails_the_job() {
    let mut proof = Proof::start();
    let profile = proof.ready();
    // Don't allow on the photo-data notice ends the run it was asked for.
    proof.send(CapabilityMessage::RunTask {
        module_id: MODULE.into(),
        task: TASK.into(),
    });
    proof.answer();
    assert_eq!(proof.task_control().state, TaskControlState::Consent);
    proof.send(CapabilityMessage::Consent(false));
    assert_eq!(
        proof.answer()[0]["method"],
        "module.permission.deny",
        "the answer is sent"
    );
    assert_eq!(
        proof.task_control().state,
        TaskControlState::Failed("consent-required: echo was not allowed".into())
    );
    assert!(proof.task_control().runnable, "and it may be asked again");
    // A wrong key reaches the endpoint, which answers 401: the job fails with its error.
    proof.set_key(&profile, "wrong-key");
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
fn a_cancel_goes_through_the_job_method() {
    let mut proof = Proof::start();
    proof.ready();
    proof.endpoint.generation().shut();
    proof.send(CapabilityMessage::RunTask {
        module_id: MODULE.into(),
        task: TASK.into(),
    });
    proof.answer();
    proof.send(CapabilityMessage::Consent(true));
    proof.answer();
    let TaskControlState::Running { job, .. } = proof.task_control().state else {
        panic!("a running task: {:?}", proof.task_control().state);
    };
    proof.send(CapabilityMessage::Cancel {
        module_id: MODULE.into(),
        job: job.clone(),
    });
    let sent = proof.answer();
    assert_eq!(sent[0]["method"], "job.cancel");
    assert_eq!(sent[0]["params"]["job_id"], json!(job));
    assert!(
        sent[0]["params"].get("mutation").is_none(),
        "a cancel converges, so it carries no envelope"
    );
    proof.endpoint.generation().open();
    proof.finish_jobs();
    assert!(matches!(
        &proof.state().tasks[TASK].phase,
        TaskPhase::Failed { code, .. } if code == "cancelled"
    ));
    proof.stop();
}

#[test]
fn capability_steps_parse_and_an_api_step_that_stores_a_secret_is_recorded_redacted() {
    let steps = parse_script(
        &json!([
            {"capability": {"module": MODULE, "task": {"task": TASK}}},
            {"capability": {"module": MODULE, "consent": "allow", "wait": false}},
            {"capability": {"module": MODULE, "apply": true}},
            {"capability": {"module": MODULE, "settle": true}},
            {"api": {"method": "module.settings.set-secret", "params": {"module_id": MODULE, "setting": "api-key", "value": "script-sentinel"}}}
        ])
        .to_string(),
    )
    .expect("a valid script");
    assert_eq!(steps.len(), 5);
    assert!(matches!(steps[0], Step::Capability(_)));
    let recorded = record(&steps[4]);
    assert_eq!(recorded["api"]["params"]["value"], "<redacted>");
    assert!(!recorded.to_string().contains("script-sentinel"));
    assert_eq!(
        record(&steps[1]),
        json!({"capability": {"module": MODULE, "consent": "allow", "wait": false}})
    );
}

#[test]
fn an_api_settings_step_carries_the_held_revision_and_is_captured_once_the_block_is_read_again() {
    let mut proof = Proof::start();
    proof.expand();
    let profile = proof.profile("a-key");
    proof.editor.evidence = Some(crate::app::testing::scripted_evidence(
        r#"[{"api":{"method":"module.settings.set","params":{"module_id":"luxforge.capabilities","values":{"strength":0.8}}}}]"#,
    ));
    // The desktop fills the envelope with the settings revision it holds, as the form's own write
    // does, and names a profile by its label as the identity the host assigned it.
    let revision = proof.state().revision().expect("the settings were read");
    let params: Map<String, Value> = serde_json::from_value(json!({"module_id": MODULE})).unwrap();
    let envelope = proof
        .editor
        .settings_envelope("module.settings.set", &params)
        .unwrap();
    assert_eq!(envelope["expected_revision"], revision);
    assert_eq!(envelope["actor"], ACTOR);
    assert_eq!(
        proof.editor.resolve_profile(
            Some(MODULE),
            &crate::app::evidence::Reference::name("Local")
        ),
        Ok(profile.clone())
    );
    assert_eq!(
        proof
            .editor
            .resolve_profile(Some(MODULE), &crate::app::evidence::Reference::Index(0)),
        Ok(profile)
    );
    assert!(
        proof
            .editor
            .resolve_profile(
                Some(MODULE),
                &crate::app::evidence::Reference::name("Other")
            )
            .is_err()
    );
    let _ = proof.editor.next_step();
    let evidence = proof.editor.evidence.as_ref().unwrap();
    assert_eq!(evidence.awaiting, Some(Settle::Host));
    assert_eq!(evidence.capability_wait, Some((MODULE.to_owned(), false)));
    // The host answers; the frame waits for the block to be read again.
    let (result, sequence) = call(
        &proof.editor.owner,
        proof.editor.client,
        "module.settings.set",
        json!({"module_id": MODULE, "values": {"strength": 0.8}, "mutation": envelope}),
    )
    .unwrap();
    let _ = proof.editor.host_answered(Ok(HostAnswer {
        method: "module.settings.set".into(),
        result,
        presets: None,
        sequence,
    }));
    assert_eq!(
        proof.editor.capability_started,
        vec![(MODULE.to_owned(), Operation::Load)]
    );
    let evidence = proof.editor.evidence.as_ref().unwrap();
    assert!(!evidence.capture_pending, "captured before the read");
    assert_eq!(evidence.awaiting, Some(Settle::Capability));
    proof.answer();
    let evidence = proof.editor.evidence.as_ref().unwrap();
    assert!(evidence.capture_pending && evidence.capability_wait.is_none());
    assert_eq!(proof.state().revision(), Some(revision + 1));
    assert!(
        matches!(&proof.block().fields[0].kind, FieldKindModel::Number { display, .. } if display == "0.80")
    );
    proof.stop();
}

#[test]
fn a_scripted_step_that_sends_nothing_is_recorded_and_captured() {
    let mut proof = Proof::start();
    proof.editor.evidence = Some(crate::app::testing::scripted_evidence(
        r#"[{"capability":{"module":"luxforge.capabilities","consent":"allow"}}]"#,
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
