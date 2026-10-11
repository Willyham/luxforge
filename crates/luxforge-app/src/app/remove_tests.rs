//! Remove, the Corrections band's generative fill mode, against a real owner serving the developer
//! `luxforge.ai` module, its fake models and the deterministic fake runtime.
//!
//! Every control goes through the update function as the band, the canvas and the keyboard send
//! it. The owner work it starts is run here as the plain calls its tasks wrap, and each answer is
//! handed back as the task's message. Each request is checked against the one Copy as JSON copies,
//! a generation against the same request an independent JSON client sends, and an accept as
//! exactly one history entry with its label.
use super::{
    Editor,
    corrections::answer_reads,
    gesture::Starting,
    message::{
        Message,
        capability::CapabilityMessage,
        corrections::{CanvasHit, CorrectionMessage},
        remove::{RemoveMessage, RemoveRequest},
        sync::SyncMessage,
        view::ViewMessage,
    },
    remove::{read_preview, start_fill},
    tasks::{self, call, request},
    testing,
};
use crate::{
    Config,
    brush::Modifiers,
    state::{
        GENERATING,
        capabilities::Operation,
        remove::{RemoveModel, TierAction},
    },
};
use iced::keyboard::{Key, Modifiers as Keys, key::Named};
use luxforge_core::{
    AssetId, ClientId, HostConfig, ModuleDescriptor, OwnerHandle,
    capabilities::secrets::MemorySecretStore,
    inference::{
        CompositeInference, InferenceRuntime,
        fake::{FAKE_BACKEND, FAKE_HOST_BACKEND, FakeInference},
    },
    jobs::JobRecord,
};
use luxforge_testbase::wait_until;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(1);

const AI: &str = "luxforge.ai";
const CORRECTIONS: &str = "luxforge.corrections";
const GENERATOR: &str = "fake-generator";
const ENCODER: &str = "fake-text-encoder";
const ORIGIN: &str = "https://developer.luxforge.invalid";
const TASK: &str = "task.fill-generative";

fn fixture(name: &str) -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0")
        .join(name)
}

/// The desktop on one photograph, in Corrections, with the developer AI module beside the product
/// modules.
struct Filling {
    editor: Editor,
    root: PathBuf,
    asset: AssetId,
    agent: ClientId,
    fake: Arc<FakeInference>,
}

impl Drop for Filling {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Filling {
    fn opened() -> Self {
        let root = std::env::temp_dir().join(format!(
            "luxforge-remove-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let fake = Arc::new(FakeInference::new());
        let runtime: Arc<dyn InferenceRuntime> = fake.clone();
        let host = HostConfig {
            preferences_dir: Some(root.join("config")),
            config_dir: Some(root.join("config").join("modules")),
            resource_dir: Some(root.join("resources")),
            secrets: Arc::new(MemorySecretStore::new()),
            // The developer models install from memory; nothing else is fetched.
            transport: luxforge_core::DeveloperModels::for_run(
                true,
                Arc::new(luxforge_net::HttpTransport::system()),
            ),
            inference: Some(
                CompositeInference::new()
                    .with_alias(FAKE_HOST_BACKEND, runtime, FAKE_BACKEND)
                    .unwrap()
                    .shared(),
            ),
            ..HostConfig::unconfigured()
        };
        let (owner, join) = OwnerHandle::start_with_host(
            &root.join("catalog.sqlite"),
            Arc::new(luxforge_core::ModuleRegistry::developer()),
            host,
        )
        .unwrap();
        let (editor, asset, agent, ()) = testing::real_photo_prepared(
            owner,
            join,
            &fixture("orientation-1.jpg"),
            Config {
                developer: true,
                ..Config::default()
            },
            |editor| {
                let (mut listed, _) =
                    call(&editor.owner, editor.client, "module.list", json!({})).unwrap();
                let modules: Vec<ModuleDescriptor> =
                    serde_json::from_value(listed["modules"].take()).unwrap();
                let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(modules))));
            },
        );
        let mut filling = Self {
            editor,
            root,
            asset,
            agent,
            fake,
        };
        filling.enter();
        filling
    }

    fn owner(&self) -> OwnerHandle {
        self.editor.owner.clone()
    }

    fn update(&mut self, message: Message) {
        let _ = self.editor.update(message);
        answer_reads(&mut self.editor);
    }

    fn remove(&mut self, message: RemoveMessage) {
        self.update(Message::Remove(message));
    }

    fn api(&self, method: &str, params: Value) -> Value {
        call(&self.owner(), self.editor.client, method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
            .0
    }

    /// Enter Corrections with its letter and choose Remove; the tiers are read.
    fn enter(&mut self) {
        self.key(Key::Character("q".into()), Keys::empty());
        let _ = call(
            &self.owner(),
            self.editor.client,
            "workspace.set",
            json!({ "mode": CORRECTIONS }),
        );
        let (session, _) = call(
            &self.owner(),
            self.editor.client,
            "session.state",
            json!({}),
        )
        .unwrap();
        let session = serde_json::from_value(session).unwrap();
        self.update(Message::View(ViewMessage::WorkspaceUpdated(Ok(session))));
        assert!(self.editor.corrections_active(), "{}", self.editor.status.text);
        self.remove(RemoveMessage::Choose);
        self.answer_capabilities();
        assert!(self.editor.remove_active());
    }

    fn key(&mut self, key: Key, modifiers: Keys) {
        let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        });
        if let Some(message) = super::keymap::keymap(
            &event,
            iced::event::Status::Ignored,
            &self.editor.key_context(),
        ) {
            self.update(message);
        }
    }

    /// Run the capability operations the update function started, as the driver's tasks would,
    /// and hand each answer back; answer the requests they sent.
    fn answer_capabilities(&mut self) -> Vec<Value> {
        let mut sent = Vec::new();
        loop {
            let started = std::mem::take(&mut self.editor.capability_started);
            if started.is_empty() {
                return sent;
            }
            for (module, op) in started {
                let answer =
                    super::capabilities::run(&self.editor.owner, self.editor.client, module, op);
                sent.extend(answer.sent.clone());
                self.update(Message::Capability(CapabilityMessage::Answered(Box::new(
                    answer,
                ))));
            }
        }
    }

    /// Read the AI module's status again, as a flag change or an install's end does.
    fn refresh_tiers(&mut self) {
        let _ = self.editor.remove_refresh_tiers();
        self.answer_capabilities();
    }

    fn model(&self) -> &RemoveModel {
        self.editor
            .workspace
            .corrections
            .remove
            .as_ref()
            .expect("the Remove model")
    }

    fn flag(&self) {
        self.api(
            "flags.set",
            json!({"flag": "ai.generative", "value": true}),
        );
    }

    /// Grant and install one developer model through the API, as any client does.
    fn install(&self, resource: &str) {
        self.api(
            "module.permission.grant",
            json!({"module_id": AI, "capability": format!("download-{resource}"),
                   "scope": {"resource": resource, "version": "1", "origin": ORIGIN},
                   "mutation": request()}),
        );
        let queued = self.api(
            "module.resource.install",
            json!({"module_id": AI, "resource_id": resource, "mutation": request()}),
        );
        let record = self.finished(queued["job_id"].as_str().unwrap());
        assert_eq!(record["status"], "ready", "{record}");
    }

    /// The flag on and the generator installed: the Generative tier is offered.
    fn ready(&mut self) {
        self.flag();
        self.install(GENERATOR);
        self.refresh_tiers();
        assert_eq!(self.model().tiers.len(), 1, "{:?}", self.model());
    }

    fn finished(&self, job: &str) -> Value {
        let mut record = Value::Null;
        wait_until("the job ending", || {
            record = self.api("job.read", json!({"job_id": job}));
            !matches!(record["status"].as_str(), Some("queued" | "running"))
        });
        record
    }

    fn content_press(&mut self, at: [f64; 2]) {
        self.update(Message::Corrections(CorrectionMessage::Press {
            view: ((at[0] * 1000.0) as f32, (at[1] * 1000.0) as f32),
            content: at,
            modifiers: Modifiers::default(),
            hit: CanvasHit::Add,
        }));
    }

    /// Paint a horizontal stroke across the middle of the photograph: the selection.
    fn paint(&mut self) {
        self.content_press([0.4, 0.5]);
        for step in 1..=10 {
            let x = 0.4 + 0.02 * f64::from(step);
            self.update(Message::Corrections(CorrectionMessage::Drag {
                view: ((x * 1000.0) as f32, 500.0),
                content: [x, 0.5],
            }));
        }
        self.update(Message::Corrections(CorrectionMessage::Release {
            view: (600.0, 500.0),
            content: [0.6, 0.5],
        }));
        assert!(self.model().selection, "{}", self.editor.status.text);
    }

    /// The request the last control sent, taken.
    fn sent(&mut self) -> (String, Value) {
        self.editor
            .remove_reads
            .last_request
            .take()
            .expect("a request was sent")
    }

    /// Copy one request as JSON: what the menu line copies.
    fn copy(&mut self, request: RemoveRequest) -> Value {
        self.remove(RemoveMessage::Copy(request));
        self.editor
            .remove_reads
            .last_copied
            .take()
            .unwrap_or_else(|| panic!("nothing copied: {}", self.editor.status.text))
    }

    /// Run the start Generate or Regenerate sent, as its task would: its job's id.
    fn start(&mut self) -> String {
        let (method, params) = self.sent();
        assert_eq!(method, TASK);
        let answer = start_fill(&self.owner(), self.editor.client, &method, params);
        self.remove(RemoveMessage::Started(answer.clone()));
        answer.expect("the fill is queued")["job_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// One read of `job`, handed back as its reader's pass would.
    fn poll(&mut self, job: &str) -> Value {
        let record = self.api("job.read", json!({"job_id": job}));
        let parsed: JobRecord = serde_json::from_value(record.clone()).unwrap();
        self.remove(RemoveMessage::Polled {
            job_id: job.to_owned(),
            result: Ok(Box::new(parsed)),
        });
        record
    }

    /// Run the preview reads the strip started until none is left.
    fn read_previews(&mut self) {
        loop {
            let reading: Vec<_> = self.editor.remove_reads.reading.iter().cloned().collect();
            if reading.is_empty() {
                return;
            }
            for (job, candidate) in reading {
                let result = read_preview(&self.owner(), self.editor.client, &job, candidate)
                    .map(Arc::new);
                self.remove(RemoveMessage::Preview {
                    job_id: job,
                    candidate,
                    result,
                });
            }
        }
    }

    /// Generate (or Regenerate) to its end, its candidates' previews read: its job.
    fn generate(&mut self, regenerate: bool) -> String {
        self.remove(if regenerate {
            RemoveMessage::Regenerate
        } else {
            RemoveMessage::Generate
        });
        let job = self.start();
        let record = self.finished(&job);
        assert_eq!(record["status"], "ready", "{record}");
        self.poll(&job);
        self.read_previews();
        job
    }

    /// Run the accept command the band sent, as its task would.
    fn run_accept(&mut self) {
        let (method, params) = self.sent();
        let refreshed = tasks::command_now(
            &self.owner(),
            self.editor.client,
            self.asset.clone(),
            &method,
            params,
            None,
        );
        self.update(Message::Sync(SyncMessage::Refreshed(
            refreshed.map(Box::new),
        )));
    }

    /// Run the cancel the band (or Escape) sent, as its task would.
    fn run_cancel(&mut self) {
        let (method, params) = self.sent();
        assert_eq!(method, "job.cancel");
        let job_id = params["job_id"].as_str().unwrap().to_owned();
        let result = call(&self.owner(), self.editor.client, &method, params).map(|_| ());
        self.remove(RemoveMessage::Cancelled { job_id, result });
    }

    fn history(&self) -> Vec<String> {
        let (page, _) = call(
            &self.owner(),
            self.agent,
            "history.list",
            json!({"asset_id": self.asset, "before_sequence": null, "limit": 100}),
        )
        .unwrap();
        page["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["label"].as_str().unwrap().to_owned())
            .collect()
    }

    /// A candidate's preview bytes as an independent client reads them.
    fn agent_preview(&self, job: &str, candidate: u8) -> Vec<u8> {
        read_preview(&self.owner(), self.agent, job, candidate)
            .unwrap()
            .rgba
    }
}

/// A request as sent, without the request id each send draws afresh.
fn without_request_id(mut request: Value) -> Value {
    if let Some(mutation) = request["params"]["mutation"].as_object_mut() {
        mutation.remove("request_id");
    }
    request
}

/// What Copy as JSON copies is `{method, params}` of the request the control then sends.
fn same_request(copied: Value, sent: &(String, Value)) {
    assert_eq!(
        without_request_id(copied),
        without_request_id(json!({"method": sent.0, "params": sent.1})),
        "Copy as JSON copies the request the control sends"
    );
}

/// Remove joins Heal and Clone, and with nothing set up names why no tier is offered with its one
/// action: the experiment's flag in Settings, then the model's download through the capability
/// driver; once both are done the Generative tier is listed.
#[test]
fn remove_names_why_no_tier_is_offered_and_offers_its_one_action() {
    let mut filling = Filling::opened();
    let band = &filling.editor.workspace.corrections;
    assert!(band.active);
    let model = filling.model();
    assert!(model.chosen);
    assert!(!model.reading, "the tiers were read on choosing Remove");
    assert!(model.tiers.is_empty());
    let unoffered = model.unoffered.clone().expect("why no tier is offered");
    assert_eq!(unoffered.reason, "Generative fill is an experiment that is off");
    assert!(matches!(
        unoffered.action,
        Some((ref label, TierAction::Experiments)) if label == "Settings \u{203a} Experiments"
    ));
    assert_eq!(
        model.generate_reason.as_deref(),
        Some("Generative fill is an experiment that is off")
    );

    filling.flag();
    filling.refresh_tiers();
    let unoffered = filling.model().unoffered.clone().expect("the model is missing");
    assert_eq!(
        unoffered.reason,
        "Generative fill needs its model installed \u{b7} download it here or in Settings \u{203a} \
         Models"
    );
    let Some((label, TierAction::Download { module_id, resource })) = unoffered.action else {
        panic!("a Download action: {unoffered:?}");
    };
    assert_eq!(label, "Download");
    assert_eq!((module_id.as_str(), resource.as_str()), (AI, GENERATOR));
    // Download goes through the capability driver, which asks for consent before anything.
    filling.remove(RemoveMessage::Download {
        module_id: module_id.clone(),
        resource: resource.clone(),
    });
    let started = filling.editor.capability_started.clone();
    assert_eq!(
        started,
        vec![(
            AI.to_owned(),
            Operation::Install {
                resource: GENERATOR.into()
            }
        )]
    );
    filling.answer_capabilities();
    let notice = &filling.editor.workspace.canvas.notices[0];
    assert!(notice.title.contains("download"), "{}", notice.title);

    filling.install(GENERATOR);
    filling.refresh_tiers();
    let model = filling.model();
    assert_eq!(model.unoffered, None);
    assert_eq!(
        model
            .tiers
            .iter()
            .map(|tier| tier.title.as_str())
            .collect::<Vec<_>>(),
        ["Generative"]
    );
    assert_eq!(model.action, "Generate");
    assert_eq!(
        model.generate_reason.as_deref(),
        Some("Paint over what to remove first")
    );
}

/// A painted selection and Generate send the tier's fill request, the one Copy as JSON copies; the
/// ring shows while it runs and three candidates fill the strip, the first chosen and previewed on
/// the canvas. An independent client sending the same request gets the same pixels.
#[test]
fn generate_sends_the_copied_request_and_fills_the_strip_with_three_candidates() {
    let mut filling = Filling::opened();
    filling.ready();
    filling.paint();
    let model = filling.model();
    assert_eq!(model.generate_reason, None, "{model:?}");
    assert!(model.hint.starts_with("Generate fills the selection"));
    let canvas = filling.editor.remove_canvas().expect("Remove draws");
    assert!(canvas.selection.is_some_and(|points| points.len() >= 2));
    assert!(canvas.preview.is_none(), "a draft selection is tinted");

    let copied = filling.copy(RemoveRequest::Generate);
    assert_eq!(filling.editor.status.text, format!("Copied the {TASK} request"));
    filling.remove(RemoveMessage::Generate);
    let sent = filling
        .editor
        .remove_reads
        .last_request
        .clone()
        .expect("Generate sent its request");
    same_request(copied, &sent);
    assert_eq!(sent.1["model"], GENERATOR);
    assert_eq!(sent.1["selection-radius"], json!(filling.editor.remove_radius()));
    assert!(sent.1.get("prompt").is_none());
    assert_eq!(filling.editor.status.text, "Generating\u{2026}");

    let job = filling.start();
    assert!(filling.editor.generation_lock().is_some());
    assert!(
        filling
            .model()
            .running
            .as_ref()
            .is_some_and(|running| running.sentence.starts_with("Generative fill \u{b7} ")),
        "{:?}",
        filling.model().running
    );
    assert!(filling.editor.remove_canvas().unwrap().progress.is_some(), "the ring");
    assert_eq!(filling.finished(&job)["status"], "ready");
    filling.poll(&job);
    filling.read_previews();

    let model = filling.model();
    assert_eq!(model.running, None);
    assert_eq!(filling.editor.generation_lock(), None);
    assert_eq!(model.strip.len(), 3, "{:?}", model.strip);
    assert!(model.strip.iter().all(|cell| cell.state == "live"));
    assert!(model.strip[0].chosen, "the first candidate is chosen");
    assert!(model.can_accept && model.can_regenerate);
    assert_eq!(filling.editor.status.text, "3 candidates \u{b7} choose one and Accept");
    for cell in &model.strip {
        assert!(
            filling
                .editor
                .remove_images()
                .image(&cell.job_id, cell.candidate)
                .is_some(),
            "each preview is held as one handle"
        );
    }
    assert!(filling.editor.remove_canvas().unwrap().preview.is_some());

    // Parity: the same request from an independent client makes the same candidates.
    let mut params = sent.1.clone();
    params["mutation"] = json!({"request_id": "agent-fill", "actor": "agent"});
    let (agent_job, _) = call(&filling.owner(), filling.agent, TASK, params).unwrap();
    let agent_job = agent_job["job_id"].as_str().unwrap().to_owned();
    assert_eq!(filling.finished(&agent_job)["status"], "ready");
    for candidate in 0..3u8 {
        assert_eq!(
            filling.agent_preview(&agent_job, candidate),
            filling.agent_preview(&job, candidate),
            "candidate {candidate} is the same for the same request"
        );
    }
}

/// Regenerate asks again from the newest job's next seed and adds three more to the strip; the
/// earlier candidates stay. Choosing one previews it on the canvas and Accept commits exactly it:
/// one history entry, and the selection and strip are cleared.
#[test]
fn regenerate_adds_three_and_accept_commits_the_chosen_candidate_as_one_entry() {
    let mut filling = Filling::opened();
    filling.ready();
    filling.paint();
    let first = filling.generate(false);
    let next_seed = filling.editor.remove.job(&first).unwrap().next_seed.unwrap();

    let copied = filling.copy(RemoveRequest::Regenerate);
    filling.remove(RemoveMessage::Regenerate);
    let sent = filling.editor.remove_reads.last_request.clone().unwrap();
    same_request(copied, &sent);
    assert_eq!(sent.1["seed"], next_seed, "Regenerate continues from the next seed");
    let second = filling.start();
    assert_eq!(filling.finished(&second)["status"], "ready");
    filling.poll(&second);
    filling.read_previews();
    let model = filling.model();
    assert_eq!(model.strip.len(), 6, "{:?}", model.strip);
    assert_eq!(model.strip[3].job_id, second);

    // Choose the second job's last candidate: the canvas previews it.
    filling.remove(RemoveMessage::Candidate {
        job_id: second.clone(),
        candidate: 2,
    });
    assert!(filling.model().strip[5].chosen);
    let chosen = filling
        .editor
        .remove_images()
        .image(&second, 2)
        .unwrap()
        .id();
    let (preview, _) = filling.editor.remove_canvas().unwrap().preview.unwrap();
    assert_eq!(preview.id(), chosen, "the chosen candidate's own handle is drawn");
    let copied = filling.copy(RemoveRequest::Preview);
    assert_eq!(
        copied,
        json!({"method": "job.read-candidate", "params": {"job_id": second, "candidate": 2, "part": "preview"}})
    );

    let before = filling.history().len();
    let copied = filling.copy(RemoveRequest::Accept);
    filling.remove(RemoveMessage::Accept);
    let sent = filling.editor.remove_reads.last_request.clone().unwrap();
    same_request(copied, &sent);
    assert_eq!(sent.0, "edit.accept-correction-patch");
    assert_eq!(sent.1["job_id"], second);
    assert_eq!(sent.1["candidate"], 2);
    assert!(filling.model().accepting);
    filling.run_accept();
    let history = filling.history();
    assert_eq!(history.len(), before + 1, "{history:?}");
    assert_eq!(history[0], "Add Generative fill 1");
    let model = filling.model();
    assert!(!model.selection && model.strip.is_empty(), "{model:?}");
    assert_eq!(filling.editor.status.text, "Accepted \u{b7} \u{2318}Z undoes it");
    assert!(filling.editor.remove_canvas().unwrap().preview.is_none());
    // The candidate accepted is the one the agent reads as accepted.
    let record = filling.api("job.read", json!({"job_id": second}));
    assert_eq!(record["candidates"][2]["state"], "accepted", "{record}");
}

/// A prompt makes the action Replace. Without the text encoder the field offers its download and
/// Generate sends nothing; with it the prompt is sent and the entry is labelled with it. The
/// field holds at most the declared 256 characters.
#[test]
fn a_prompt_replaces_and_without_the_encoder_generates_nothing() {
    let mut filling = Filling::opened();
    filling.ready();
    filling.paint();
    filling.remove(RemoveMessage::Prompt("x".repeat(300)));
    assert_eq!(filling.model().prompt.chars().count(), 256);
    assert_eq!(filling.model().prompt_max, 256);
    filling.remove(RemoveMessage::Prompt("a red  bicycle ".into()));
    let model = filling.model();
    assert_eq!(model.action, "Replace");
    let encoder = model.encoder.clone().expect("the encoder's download");
    assert_eq!(encoder.resource, ENCODER);
    assert_eq!(
        model.generate_reason.as_deref(),
        Some("Replace needs the text encoder installed")
    );
    filling.remove(RemoveMessage::Generate);
    assert!(
        filling.editor.remove_reads.last_request.is_none(),
        "nothing is sent without the encoder"
    );
    assert_eq!(
        filling.editor.status.text,
        "Replace needs the text encoder installed"
    );

    filling.install(ENCODER);
    filling.refresh_tiers();
    assert_eq!(filling.model().encoder, None);
    let copied = filling.copy(RemoveRequest::Generate);
    filling.remove(RemoveMessage::Generate);
    let sent = filling.editor.remove_reads.last_request.clone().unwrap();
    same_request(copied, &sent);
    assert_eq!(sent.1["prompt"], "a red bicycle", "the prompt as an API client sends it");
    let job = filling.start();
    assert!(
        filling
            .model()
            .running
            .as_ref()
            .is_some_and(|running| running.sentence.starts_with("Replace \u{201c}a red bicycle\u{201d}"))
    );
    assert_eq!(filling.finished(&job)["status"], "ready");
    filling.poll(&job);
    filling.read_previews();
    assert_eq!(filling.model().strip[0].label, "Replace \"a red bicycle\"");
    filling.remove(RemoveMessage::Accept);
    filling.run_accept();
    assert_eq!(filling.history()[0], "Add Replace \"a red bicycle\" 1");
}

/// While a generation runs its photograph's edit controls say so and the desktop sends nothing the
/// owner would refuse; another photograph stays editable. Escape cancels the generation first,
/// through the same `job.cancel` Copy as JSON copies, which releases the lock at once; the next
/// Escape clears the selection.
#[test]
fn a_running_generation_locks_its_photograph_and_escape_cancels_it_then_clears() {
    let mut filling = Filling::opened();
    filling.ready();
    filling.fake.controls().set_step_delay(Duration::from_millis(200));
    let other = testing::import_and_adopt(
        &filling.owner(),
        filling.agent,
        &fixture("jpeg-scan-444-interleaved.jpg"),
    );
    filling.paint();
    filling.remove(RemoveMessage::Generate);
    let job = filling.start();
    wait_until("the generation stepping", || {
        let record = filling.poll(&job);
        record["progress"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("step"))
    });

    assert_eq!(filling.editor.generation_lock().as_deref(), Some(GENERATING));
    assert_eq!(
        filling.editor.gesture_refusal(Starting::Action).as_deref(),
        Some(GENERATING)
    );
    assert_eq!(
        filling.editor.gesture_refusal(Starting::Correction).as_deref(),
        Some(GENERATING)
    );
    let model = filling.model();
    assert!(!model.enabled);
    assert_eq!(model.generate_reason.as_deref(), Some(GENERATING));
    assert!(model.hint.ends_with("Esc cancels"), "{}", model.hint);
    // Nothing that would be refused is sent: no second fill, no new selection.
    filling.remove(RemoveMessage::Generate);
    assert!(filling.editor.remove_reads.last_request.is_none());
    filling.content_press([0.2, 0.2]);
    assert_eq!(filling.editor.status.text, GENERATING);
    // The owner refuses this photograph's edits, and takes the other photograph's.
    let exposure = |asset: &AssetId, revision: u64| {
        json!({"asset_id": asset, "exposure": 0.5, "mutation": tasks::mutation(revision)})
    };
    let revision = |asset: &AssetId| {
        filling.api("asset.state", json!({"asset_id": asset}))["revision"]
            .as_u64()
            .unwrap()
    };
    let refused = call(
        &filling.owner(),
        filling.agent,
        "edit.set-basic",
        exposure(&filling.asset, revision(&filling.asset)),
    )
    .unwrap_err();
    assert!(refused.contains("locked") || refused.contains("conflict"), "{refused}");
    assert!(filling.editor.remove.running_on(&other).is_none());
    call(
        &filling.owner(),
        filling.agent,
        "edit.set-basic",
        exposure(&other, revision(&other)),
    )
    .expect("another photograph stays editable");

    let copied = filling.copy(RemoveRequest::Cancel);
    filling.key(Key::Named(Named::Escape), Keys::empty());
    let sent = filling.editor.remove_reads.last_request.clone().unwrap();
    same_request(copied, &sent);
    assert!(filling.model().running.as_ref().unwrap().cancelling);
    filling.run_cancel();
    assert_eq!(filling.editor.generation_lock(), None, "the lock is released at once");
    assert_eq!(filling.editor.gesture_refusal(Starting::Action), None);
    assert_eq!(filling.editor.status.text, "Generation cancelled");
    assert_eq!(filling.finished(&job)["status"], "cancelled");
    assert!(filling.model().selection, "the first Escape keeps the selection");

    filling.key(Key::Named(Named::Escape), Keys::empty());
    let model = filling.model();
    assert!(!model.selection && model.strip.is_empty());
    assert_eq!(filling.editor.status.text, "Selection cleared");
    assert!(
        !filling.history().iter().any(|label| label.contains("Generative")),
        "a cancelled generation commits nothing"
    );
}
