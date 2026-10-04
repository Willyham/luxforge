//! The capability surface against the real owner and the developer proof module: every gesture
//! goes through the update function, the operations it starts run through the same owner methods
//! an independent JSON client calls, and their answers come back through the update function. What
//! the block does not draw — profiles, secrets, a single grant — is set up through those methods
//! directly, as any client sets it. A loopback [`ProofEndpoint`] stands in for the provider, and
//! the secret store is in memory.
use super::{
    Boot, Editor,
    capabilities::{Polled, capability_pass, capability_reads, read_job, run},
    evidence::Settle,
    job_reads::{self, Pass, Reader},
    message::{Message, capability::CapabilityMessage, control::ControlMessage, sync::SyncMessage},
    tasks::{ACTOR, HostAnswer, REQUEST_NUMBER, Scope, call, refresh, request},
    testing::{
        Followed, attach_log, derive_ran, idle_workers, import_and_adopt, logged, mark_no_derive,
    },
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
    AssetId, HostConfig, ModuleDescriptor, OwnerHandle,
    capabilities::secrets::MemorySecretStore,
    jobs::{JobRecord, JobStatus},
};
use luxforge_testbase::{ProofEndpoint, wait_for, wait_until};
use luxforge_testkit::proof_protocol;
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
        let endpoint = ProofEndpoint::start(&key, proof_protocol()).unwrap();
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
        let asset = import_and_adopt(&owner, client, &fixture);
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

    /// Read each tracked live job once, as a reader's pass would.
    fn read_live(&self) -> Vec<(String, String, Result<JobRecord, String>)> {
        self.editor
            .capabilities
            .live_jobs()
            .into_iter()
            .map(|(module, job)| {
                let record = read_job(&self.editor.owner, self.editor.client, &job);
                (module, job, record)
            })
            .collect()
    }

    /// Read the tracked live jobs, as the reader would, until none is left.
    fn finish_jobs(&mut self) {
        wait_until("every tracked job finishing", || {
            if !self.editor.capabilities.live() {
                return true;
            }
            let polled = self.read_live();
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
    // With nothing in flight there is no job reader.
    assert!(proof.editor.capability_reader_subscription().is_none());
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
    assert_eq!(proof.editor.status.text, line);
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
    assert!(proof.editor.capability_reader_subscription().is_some());
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
        let polled = proof.read_live();
        proof.send(CapabilityMessage::Polled(polled));
        None
    });
    assert!(installing.state.starts_with("Installing"), "{installing:?}");
    proof.endpoint.palette().open();
    proof.finish_jobs();
    assert!(proof.editor.capability_reader_subscription().is_none());
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
    assert!(proof.editor.busy, "{}", proof.editor.status.text);
    assert_eq!(proof.editor.status.text, "Running edit.apply-proof-tint…");
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
        proof.editor.status.text,
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

/// A task's job that ends before the job reader reads it is reported by the next status read made
/// for any other reason — the owner's wake when this desktop's own task ended, which reads the
/// module again — and that read tracks the job as ended, which ends the reader. So the run takes
/// its result from that read, and Apply offers it.
#[test]
fn a_task_run_takes_its_result_from_a_status_read_that_sees_its_job_end() {
    let mut proof = Proof::start();
    proof.ready();
    proof.send(CapabilityMessage::RunTask {
        module_id: MODULE.into(),
        task: TASK.into(),
    });
    proof.answer();
    proof.send(CapabilityMessage::Consent(true));
    proof.answer();
    let job = proof.state().jobs.last().cloned().expect("the task's job");
    wait_until("the task's job", || {
        proof.api("job.read", json!({"job_id": job.job_id}))["status"] == "ready"
    });
    // The read the event sync asks for, in place of the job reader.
    let _ = proof.editor.reload_capabilities();
    proof.answer();
    assert!(
        !proof.editor.capabilities.live(),
        "the job is tracked as ended, so nothing reads it"
    );
    assert!(
        matches!(
            proof.task_control().state,
            TaskControlState::Succeeded { apply: Some(_), .. }
        ),
        "{:?}",
        proof.task_control().state
    );
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

/// While a task runs, its reader sends the desktop a record only when it has changed. The
/// reader's first read of a job can answer the record the round trip that started it already
/// tracked, and that read changes nothing: it skips the hooks and the derive. Progress and the
/// job's end are changes: they take the full update and show in the block, against the real owner.
#[test]
fn a_job_read_that_changes_nothing_skips_the_hooks_and_the_derive() {
    let mut proof = Proof::start();
    proof.ready();
    idle_workers(&mut proof.editor);
    proof.endpoint.generation().shut();
    proof.send(CapabilityMessage::RunTask {
        module_id: MODULE.into(),
        task: TASK.into(),
    });
    proof.answer();
    proof.send(CapabilityMessage::Consent(true));
    proof.answer();
    assert!(matches!(
        proof.task_control().state,
        TaskControlState::Running { .. }
    ));
    assert!(proof.editor.capabilities.live());

    // The first read settles what is held, whether or not it differed from the answer's own.
    let polled = proof.read_live();
    assert_eq!(polled.len(), 1);
    proof.send(CapabilityMessage::Polled(polled.clone()));
    proof.answer();

    // The same read again, as a restarted reader's first read would be, changes nothing.
    let updates = proof.editor.full_updates;
    mark_no_derive(&proof.editor);
    proof.send(CapabilityMessage::Polled(polled.clone()));
    assert_eq!(
        proof.editor.full_updates, updates,
        "a read of what is held runs no hooks"
    );
    assert!(!derive_ran(&proof.editor), "and no derive");
    assert_eq!(proof.editor.log.loop_timing.get().last_rederive_ms, 0.0);

    // What counts as unchanged, read against what is held: an ended job, a read that failed, a
    // job not tracked and any difference at all are not.
    let polled_message = |polled: Vec<(String, String, Result<_, String>)>| {
        Message::Capability(CapabilityMessage::Polled(polled))
    };
    let held = polled[0].2.clone().expect("the job's record");
    let with = |record: Result<_, String>, module: &str| {
        polled_message(vec![(module.to_owned(), polled[0].1.clone(), record)])
    };
    assert!(
        proof
            .editor
            .job_read_changes_nothing(&with(Ok(held.clone()), MODULE))
    );
    let mut moved = held.clone();
    moved.progress.fraction = Some(0.5);
    let mut ended = held.clone();
    ended.status = JobStatus::Ready;
    for (case, message) in [
        ("progress", with(Ok(moved.clone()), MODULE)),
        ("an ended job", with(Ok(ended), MODULE)),
        ("a failed read", with(Err("gone".into()), MODULE)),
        (
            "an untracked module",
            with(Ok(held.clone()), "luxforge.other"),
        ),
    ] {
        assert!(!proof.editor.job_read_changes_nothing(&message), "{case}");
    }

    // Progress takes the full update, and the block shows it.
    let updates = proof.editor.full_updates;
    mark_no_derive(&proof.editor);
    proof.send(CapabilityMessage::Polled(vec![(
        MODULE.into(),
        polled[0].1.clone(),
        Ok(moved),
    )]));
    assert_eq!(proof.editor.full_updates, updates + 1);
    assert!(derive_ran(&proof.editor));
    let TaskControlState::Running { text, .. } = proof.task_control().state else {
        panic!("a running task: {:?}", proof.task_control().state);
    };
    assert!(text.contains("50%"), "{text}");

    // The job's end comes from the reader the subscription starts, as a real run delivers it:
    // each message it sends is applied, the end takes the full update and the run takes its
    // outcome, and the reader ends with its job.
    proof.endpoint.generation().open();
    assert!(proof.editor.capability_reader_subscription().is_some());
    let mut reader = Followed::new(capability_reads(&Reader {
        identity: proof.editor.capabilities.live_jobs(),
        owner: proof.editor.owner.clone(),
        client: proof.editor.client,
    }));
    let mut sent = 0;
    let mut ended_seen = false;
    while let Some(message) = reader.next() {
        sent += 1;
        let ended = matches!(
            &message,
            Message::Capability(CapabilityMessage::Polled(polled))
                if polled.iter().any(|(_, _, record)| record
                    .as_ref()
                    .is_ok_and(|record| record.status.is_finished()))
        );
        let updates = proof.editor.full_updates;
        let _ = proof.editor.update(message);
        if ended {
            ended_seen = true;
            assert!(
                proof.editor.full_updates > updates,
                "an ended job is a change"
            );
        }
        proof.answer();
    }
    assert!(
        ended_seen,
        "the reader's last message was the job's end ({sent} sent)"
    );
    assert!(!proof.editor.capabilities.live());
    assert!(
        matches!(
            proof.task_control().state,
            TaskControlState::Succeeded { .. } | TaskControlState::Failed(_)
        ),
        "{:?}",
        proof.task_control().state
    );
    proof.stop();
}

/// A tracked job as the owner would read it.
fn job_at(job: &str, status: &str, fraction: Option<f64>) -> JobRecord {
    serde_json::from_value(json!({
        "job_id": job,
        "kind": "task",
        "status": status,
        "progress": fraction.map_or(json!({}), |fraction| json!({"fraction": fraction})),
        "module_id": MODULE,
    }))
    .unwrap()
}

/// Scripted owner answers by job, and the reads made of each.
#[derive(Default)]
struct Owned {
    answers: std::collections::BTreeMap<String, Result<JobRecord, String>>,
    reads: std::collections::BTreeMap<String, usize>,
}

impl Owned {
    fn answer(&mut self, job: &str, answer: Result<JobRecord, String>) {
        let _ = self.answers.insert(job.to_owned(), answer);
    }

    fn read(&mut self, job: &str) -> Result<JobRecord, String> {
        *self.reads.entry(job.to_owned()).or_default() += 1;
        self.answers[job].clone()
    }
}

fn polled(pass: Pass<Message>) -> (Polled, bool) {
    let (Pass::Send(Message::Capability(CapabilityMessage::Polled(entries)))
    | Pass::Last(Message::Capability(CapabilityMessage::Polled(entries)))) = &pass
    else {
        panic!("a quiet pass");
    };
    (entries.clone(), matches!(pass, Pass::Last(_)))
}

/// A reader of capability jobs sends each job's first read, then only the entries that changed, in
/// one message per pass: a pass in which no job changed sends nothing, and the entries the handler
/// applies one by one are never repeated. A job's end is sent once and that job is read no more;
/// the reader's last message is the end of the last job.
#[test]
fn a_capability_reader_sends_only_the_jobs_that_changed_and_stops_per_job() {
    let owned = Arc::new(std::sync::Mutex::new(Owned::default()));
    let (a, b) = ("job-aaaaaaaaaaaa", "job-bbbbbbbbbbbb");
    {
        let mut owned = owned.lock().unwrap();
        owned.answer(a, Ok(job_at(a, "running", Some(0.1))));
        owned.answer(b, Ok(job_at(b, "queued", None)));
    }
    let read = owned.clone();
    let mut pass = capability_pass(
        vec![(MODULE.into(), a.into()), (MODULE.into(), b.into())],
        move |job| read.lock().unwrap().read(job),
    );

    // The first pass sends both, in the order the desktop tracks them.
    let (entries, last) = polled(pass());
    assert!(!last);
    assert_eq!(
        entries
            .iter()
            .map(|(module, job, record)| (module.as_str(), job.as_str(), record.is_ok()))
            .collect::<Vec<_>>(),
        [(MODULE, a, true), (MODULE, b, true)]
    );

    // Many passes of the same records send nothing.
    for _ in 0..50 {
        assert!(matches!(pass(), Pass::Quiet));
    }

    // Progress of one job sends that job alone.
    owned
        .lock()
        .unwrap()
        .answer(a, Ok(job_at(a, "running", Some(0.4))));
    let (entries, last) = polled(pass());
    assert!(!last);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1, a);
    assert_eq!(
        entries[0].2.as_ref().unwrap().progress.fraction,
        Some(0.4),
        "the moved record"
    );
    assert!(matches!(pass(), Pass::Quiet));

    // A job's end is sent once, alone, and that job is not read again.
    owned
        .lock()
        .unwrap()
        .answer(a, Ok(job_at(a, "ready", Some(1.0))));
    let (entries, last) = polled(pass());
    assert!(!last, "the other job is still live");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1, a);
    let reads_of_a = owned.lock().unwrap().reads[a];
    for _ in 0..10 {
        assert!(matches!(pass(), Pass::Quiet));
    }
    assert_eq!(
        owned.lock().unwrap().reads[a],
        reads_of_a,
        "an ended job is read no more"
    );

    // A read that fails is the end of that job too, sent once; the reader stops with its last job.
    owned.lock().unwrap().answer(b, Err("gone".into()));
    let (entries, last) = polled(pass());
    assert!(last, "no job is left to read");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1, b);
    assert_eq!(entries[0].2, Err("gone".to_owned()));
}

/// Two jobs that end in the same pass are sent in the same message, so the handler refreshes their
/// module once, as it did when it was handed every job's read together.
#[test]
fn a_capability_reader_sends_jobs_that_end_together_in_one_message() {
    let owned = Arc::new(std::sync::Mutex::new(Owned::default()));
    let (a, b) = ("job-aaaaaaaaaaaa", "job-bbbbbbbbbbbb");
    {
        let mut owned = owned.lock().unwrap();
        owned.answer(a, Ok(job_at(a, "running", None)));
        owned.answer(b, Ok(job_at(b, "running", None)));
    }
    let read = owned.clone();
    let mut pass = capability_pass(
        vec![(MODULE.into(), a.into()), (MODULE.into(), b.into())],
        move |job| read.lock().unwrap().read(job),
    );
    let _ = polled(pass());
    {
        let mut owned = owned.lock().unwrap();
        owned.answer(a, Ok(job_at(a, "ready", Some(1.0))));
        owned.answer(b, Ok(job_at(b, "cancelled", None)));
    }
    let (entries, last) = polled(pass());
    assert!(last);
    assert_eq!(entries.len(), 2);
}

/// The whole stream of a reader following two jobs: the update loop gets a message for each pass
/// in which something changed and for no other. The first carries both jobs, the next only the job
/// that moved, a failed read ends that job alone and the last job's end ends the stream, with
/// each job read exactly as long as it was live.
#[test]
fn a_capability_reader_yields_a_message_only_for_a_pass_in_which_a_job_changed() {
    let (a, b) = ("job-aaaaaaaaaaaa", "job-bbbbbbbbbbbb");
    let reads = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
        String,
        usize,
    >::new()));
    let counted = reads.clone();
    // Job a runs unchanged for ten passes and is ready on the eleventh. Job b is queued for three,
    // running for three and then cannot be read.
    let mut stream = Followed::new(job_reads::reads(
        std::time::Duration::from_micros(200),
        capability_pass(
            vec![(MODULE.into(), a.into()), (MODULE.into(), b.into())],
            move |job| {
                let mut counts = counted.lock().unwrap();
                let pass = counts.entry(job.to_owned()).or_default();
                *pass += 1;
                match (job == a, *pass) {
                    (true, 1..=10) => Ok(job_at(a, "running", Some(0.1))),
                    (true, _) => Ok(job_at(a, "ready", Some(1.0))),
                    (false, 1..=3) => Ok(job_at(b, "queued", None)),
                    (false, 4..=6) => Ok(job_at(b, "running", None)),
                    (false, _) => Err("gone".to_owned()),
                }
            },
        ),
    ));
    let mut sent = Vec::new();
    while let Some(message) = stream.next() {
        let Message::Capability(CapabilityMessage::Polled(entries)) = message else {
            panic!("the reader sent {message:?}");
        };
        sent.push(
            entries
                .into_iter()
                .map(|(_, job, record)| {
                    let state = match record {
                        Ok(record) => format!("{:?}", record.status),
                        Err(error) => error,
                    };
                    (job, state)
                })
                .collect::<Vec<_>>(),
        );
    }
    let jobs = |entries: &[(&str, &str)]| -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(job, state)| ((*job).to_owned(), (*state).to_owned()))
            .collect()
    };
    assert_eq!(
        sent,
        [
            jobs(&[(a, "Running"), (b, "Queued")]),
            jobs(&[(b, "Running")]),
            jobs(&[(b, "gone")]),
            jobs(&[(a, "Ready")]),
        ]
    );
    let counts = reads.lock().unwrap();
    assert_eq!(counts[a], 11, "a is read until its end");
    assert_eq!(counts[b], 7, "b is read until its read failed");
}

/// The reader is identified by the set of live jobs: the same set, however often the desktop
/// rebuilds its subscriptions, is the same reader, and a different set is another.
#[test]
fn a_capability_reader_is_identified_by_the_set_of_live_jobs() {
    use iced::advanced::subscription::{Hasher, into_recipes};
    use std::hash::Hasher as _;
    let mut proof = Proof::start();
    proof.ready();
    assert!(proof.editor.capability_reader_subscription().is_none());
    proof.endpoint.generation().shut();
    proof.send(CapabilityMessage::RunTask {
        module_id: MODULE.into(),
        task: TASK.into(),
    });
    proof.answer();
    proof.send(CapabilityMessage::Consent(true));
    proof.answer();
    let identity = |proof: &Proof| {
        let mut recipes = into_recipes(
            proof
                .editor
                .capability_reader_subscription()
                .expect("a live job"),
        );
        assert_eq!(recipes.len(), 1);
        let mut hasher = Hasher::default();
        recipes.remove(0).hash(&mut hasher);
        hasher.finish()
    };
    let first = identity(&proof);
    assert_eq!(identity(&proof), first, "rebuilt, it is the same reader");
    let polled = proof.read_live();
    let mut moved = polled[0].2.clone().unwrap();
    moved.progress.fraction = Some(0.5);
    proof.send(CapabilityMessage::Polled(vec![(
        MODULE.into(),
        polled[0].1.clone(),
        Ok(moved),
    )]));
    assert_eq!(identity(&proof), first, "a changed record is the same job");
    proof.endpoint.generation().open();
    proof.finish_jobs();
    assert!(proof.editor.capability_reader_subscription().is_none());
    proof.stop();
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
        proof
            .editor
            .status
            .text
            .contains("no consent notice is open"),
        "{}",
        proof.editor.status.text
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
    assert!(!capability_event("pick.develop"));
}
