//! The Settings sheet's Models tab against the real owner, the developer models proof and a
//! loopback [`ProofEndpoint`] that serves its two files with HTTP ranges: the tab lists the model
//! with its trade-off, use restriction, requirement and this machine's memory, an interrupted
//! download resumes from its offset over the network transport, Remove and Unload now go through
//! the capability driver as any client's requests, and Copy as JSON request copies exactly those.
use super::{
    Boot, Editor,
    capabilities::run,
    message::{
        Message, capability::CapabilityMessage, settings::SettingsMessage, sync::SyncMessage,
    },
    tasks::{REQUEST_NUMBER, call, request},
};
use crate::{
    Config,
    state::{
        models::{ModelRequest, ModelsModel},
        settings::SettingsTab,
    },
};
use luxforge_core::{
    HostConfig, MODELS_PROOF_MODULE, ModuleDescriptor, OwnerHandle, PROOF_ENCODER_PATH,
    PROOF_MODEL_RESOURCE, PROOF_MODEL_RESTRICTION, PROOF_MODEL_TRADEOFF,
    capabilities::secrets::MemorySecretStore, proof_model_files,
};
use luxforge_testbase::{ProofEndpoint, wait_until};
use luxforge_testkit::proof_protocol;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
};

const MODULE: &str = MODELS_PROOF_MODULE;
const RESOURCE: &str = PROOF_MODEL_RESOURCE;

/// An editor whose owner serves the developer modules and the proofs against a loopback endpoint,
/// with the deterministic fake runtime and an injected machine memory.
struct Models {
    editor: Editor,
    endpoint: ProofEndpoint,
    root: PathBuf,
}

impl Models {
    fn start(machine_memory_bytes: Option<u64>) -> Self {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
        );
        let endpoint = ProofEndpoint::start("models-key", proof_protocol()).unwrap();
        let root = std::env::temp_dir().join(format!("luxforge-desktop-models-{unique}"));
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
            machine_memory_bytes,
            inference: Some(luxforge_onnx::host_runtime(None, true)),
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
        Self {
            editor,
            endpoint,
            root,
        }
    }

    fn send(&mut self, message: CapabilityMessage) {
        let _ = self.editor.update(Message::Capability(message));
    }

    /// Run every round trip the update function started, as its task would, and hand each answer
    /// back; answer what the requests sent.
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

    fn api(&self, method: &str, params: Value) -> Result<Value, String> {
        call(&self.editor.owner, self.editor.client, method, params).map(|(answer, _)| answer)
    }

    /// Open the sheet at the Models tab, which reads the model module's status.
    fn open(&mut self) -> Vec<Value> {
        let _ = self.editor.update(Message::Settings(SettingsMessage::Open(
            SettingsTab::Models,
        )));
        self.answer()
    }

    fn tab(&self) -> &ModelsModel {
        self.editor
            .workspace
            .settings
            .models
            .as_ref()
            .expect("the Models tab is open")
    }

    fn grant(&self) {
        self.api(
            "module.permission.grant",
            json!({"module_id": MODULE, "capability": "model",
                   "scope": {"resource": RESOURCE, "version": "1", "origin": self.endpoint.base_url()},
                   "mutation": request()}),
        )
        .unwrap();
    }

    /// Install through the API and wait for the job to end; answers its record.
    fn install(&self) -> Value {
        let queued = self
            .api(
                "module.resource.install",
                json!({"module_id": MODULE, "resource_id": RESOURCE, "mutation": request()}),
            )
            .unwrap();
        let mut record = Value::Null;
        wait_until("the install ending", || {
            record = self
                .api("job.read", json!({"job_id": queued["job_id"]}))
                .unwrap();
            !matches!(record["status"].as_str(), Some("queued" | "running"))
        });
        record
    }

    fn installed_dir(&self) -> PathBuf {
        self.root
            .join("resources")
            .join(MODULE)
            .join(RESOURCE)
            .join("1")
    }
}

impl Drop for Models {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn the_models_tab_is_listed_only_with_a_model_and_shows_it_against_this_machine() {
    let mut models = Models::start(Some(16 << 30));
    assert!(
        models.editor.workspace.settings.tabs.is_empty(),
        "a closed sheet derives nothing"
    );
    let sent = models.open();
    assert_eq!(
        sent.iter()
            .filter(|request| request["method"] == "module.status")
            .count(),
        1,
        "opening the tab reads the one model module's status: {sent:?}"
    );
    let tabs = &models.editor.workspace.settings.tabs;
    assert!(tabs.contains(&SettingsTab::Models), "{tabs:?}");
    let tab = models.tab();
    assert!(!tab.loading);
    let [row] = tab.rows.as_slice() else {
        panic!("one model: {:?}", tab.rows)
    };
    assert_eq!(row.title, "Proof segmenter");
    assert_eq!(row.detail, "v1 · 8.8 KB · 2 files");
    assert_eq!(row.state, "Not installed");
    assert_eq!(row.tradeoff.as_deref(), Some(PROOF_MODEL_TRADEOFF));
    assert_eq!(
        row.use_restriction.as_deref(),
        Some(PROOF_MODEL_RESTRICTION)
    );
    assert_eq!(
        row.requirement.as_deref(),
        Some("Needs 2 GB of memory · 64.0 MB while loaded · fake · fp32")
    );
    assert_eq!(row.fits, Some(true));
    assert!(!row.removable);
    assert_eq!(
        tab.machine.as_deref(),
        Some("This machine has 16 GB of memory. Proof segmenter needs 2 GB.")
    );
    assert!(tab.resident.is_empty());
    // What a frame records is the tab as drawn beside the status it was drawn from.
    let summary = models.editor.settings_summary();
    assert_eq!(summary["open"], "models");
    assert_eq!(summary["models"]["rows"][0]["state"], "Not installed");
    let status = &summary["models"]["status"][MODULE];
    assert_eq!(status["machine"]["memory_bytes"], 16u64 << 30);
    assert_eq!(status["resources"][0]["state"], "not-installed");
    assert_eq!(summary["models"]["tiers"], json!([]));
}

#[test]
fn a_machine_below_the_minimum_sees_the_requirement_and_cannot_install() {
    let mut models = Models::start(Some(1 << 30));
    models.open();
    let row = &models.tab().rows[0];
    assert_eq!(row.fits, Some(false));
    assert_eq!(
        models.tab().machine.as_deref(),
        Some("This machine has 1 GB of memory. Proof segmenter needs 2 GB, more than it has.")
    );
    models.grant();
    let refused = models
        .api(
            "module.resource.install",
            json!({"module_id": MODULE, "resource_id": RESOURCE, "mutation": request()}),
        )
        .unwrap_err();
    assert!(
        refused.contains("Proof segmenter needs a machine with at least 2 GB of memory"),
        "{refused}"
    );
    assert!(models.endpoint.requests().is_empty(), "nothing was fetched");
}

#[test]
fn an_interrupted_download_resumes_over_loopback_and_installs_both_files_together() {
    let mut models = Models::start(Some(16 << 30));
    models.grant();
    models.endpoint.cut_next_model(1000);
    let failed = models.install();
    assert_eq!(failed["status"], "failed", "{failed}");
    assert_eq!(failed["error"]["code"], "read-error", "{failed}");
    assert!(!models.installed_dir().exists(), "nothing is installed");
    models.open();
    let row = &models.tab().rows[0];
    assert_eq!(
        row.state,
        "Failed: the response from 127.0.0.1 ended early · 1000 B of 8.8 KB downloaded, resumes \
         on the next download"
    );
    assert!(row.removable, "a kept partial download can be removed");

    let installed = models.install();
    assert_eq!(installed["status"], "ready", "{installed}");
    let encoder: Vec<_> = models
        .endpoint
        .requests()
        .into_iter()
        .filter(|request| request.path == PROOF_ENCODER_PATH)
        .collect();
    assert_eq!(encoder.len(), 2);
    assert_eq!(encoder[0].header("range"), None);
    assert_eq!(encoder[1].header("range"), Some("bytes=1000-"));
    assert_eq!(encoder[1].status, 206);
    let files = proof_model_files();
    let dir = models.installed_dir();
    assert_eq!(std::fs::read(dir.join(RESOURCE)).unwrap(), files[0].1);
    assert_eq!(std::fs::read(dir.join("decoder")).unwrap(), files[1].1);
    let marker: Value =
        serde_json::from_slice(&std::fs::read(dir.join("installed.json")).unwrap()).unwrap();
    assert_eq!(marker["files"][0]["name"], "decoder", "{marker}");

    let _ = models.editor.reload_capabilities();
    models.answer();
    let row = &models.tab().rows[0];
    assert_eq!(row.state, "Installed");
    assert!(row.removable);
}

#[test]
fn unload_now_and_remove_go_through_the_tab_and_copy_the_requests_they_send() {
    let mut models = Models::start(Some(16 << 30));
    models.grant();
    assert_eq!(models.install()["status"], "ready");
    let load = models
        .api(
            "module.runtime.load",
            json!({"module_id": MODULE, "resource_id": RESOURCE}),
        )
        .unwrap();
    if load["state"] == "loading" {
        wait_until("the load", || {
            models
                .api("job.read", json!({"job_id": load["job_id"]}))
                .unwrap()["status"]
                == "ready"
        });
    }
    models.open();
    let tab = models.tab();
    assert_eq!(tab.rows[0].state, "Loaded");
    let [resident] = tab.resident.as_slice() else {
        panic!("one loaded model: {:?}", tab.resident)
    };
    assert_eq!(resident.title, "Proof segmenter");
    // The placement is the session's own: the composite hands the fake its own `cpu`.
    assert_eq!(resident.detail, "Loaded · 64.0 MB · cpu · fp32");
    assert_eq!(
        tab.resident_line.as_deref(),
        Some("64.0 MB of 16 GB held by loaded models")
    );

    // Copy as JSON request copies exactly what Unload now sends.
    let _ = models
        .editor
        .update(Message::Capability(CapabilityMessage::CopyModelRequest {
            module_id: MODULE.into(),
            resource: RESOURCE.into(),
            request: ModelRequest::Unload,
        }));
    assert_eq!(
        models.editor.status.text,
        "Copied the module.runtime.unload request"
    );
    models.send(CapabilityMessage::Unload {
        module_id: MODULE.into(),
        resource: RESOURCE.into(),
    });
    let sent = models.answer();
    assert_eq!(
        sent[0],
        json!({"method": "module.runtime.unload", "params": {"module_id": MODULE, "resource_id": RESOURCE}})
    );
    assert_eq!(
        sent[0],
        ModelRequest::Unload.request(MODULE, RESOURCE, Value::Null),
        "the copied request is the one sent"
    );
    assert!(models.tab().resident.is_empty());
    assert_eq!(models.tab().rows[0].state, "Installed");

    let _ = models
        .editor
        .update(Message::Capability(CapabilityMessage::CopyModelRequest {
            module_id: MODULE.into(),
            resource: RESOURCE.into(),
            request: ModelRequest::Remove,
        }));
    assert_eq!(
        models.editor.status.text,
        "Copied the module.resource.remove request"
    );
    models.send(CapabilityMessage::Remove {
        module_id: MODULE.into(),
        resource: RESOURCE.into(),
    });
    let sent = models.answer();
    assert_eq!(sent[0]["method"], "module.resource.remove");
    assert_eq!(
        sent[0]["params"]["resource_id"], RESOURCE,
        "the same request Copy as JSON request copies, with its own request id"
    );
    wait_until("the removal", || !models.installed_dir().exists());
    let _ = models.editor.reload_capabilities();
    models.answer();
    assert_eq!(models.tab().rows[0].state, "Not installed");
    assert!(!models.tab().rows[0].removable);
}
