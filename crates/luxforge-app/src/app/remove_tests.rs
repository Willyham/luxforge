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
    state::{GENERATING, capabilities::Operation, fill_setup::SetupAction, remove::RemoveModel},
};
use iced::keyboard::{Key, Modifiers as Keys, key::Named};
use luxforge_core::{
    AssetId, ClientId, HostConfig, ModuleDescriptor, OwnerHandle,
    capabilities::{descriptor::IntroductionPicture, secrets::MemorySecretStore},
    inference::{
        CompositeInference, InferenceRuntime,
        fake::{FAKE_BACKEND, FAKE_HOST_BACKEND, FakeInference},
    },
    jobs::JobRecord,
};
use luxforge_test_macros::slow;
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
pub(super) struct Filling {
    pub(super) editor: Editor,
    pub(super) root: PathBuf,
    pub(super) asset: AssetId,
    pub(super) agent: ClientId,
    pub(super) fake: Arc<FakeInference>,
}

impl Drop for Filling {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Filling {
    /// The photograph open in Corrections with the experiment on, as the desktop read it, and
    /// Remove chosen.
    pub(super) fn opened() -> Self {
        let mut filling = Self::corrections();
        filling.choose();
        filling
    }

    /// The photograph open in Corrections, with the flags read as the launch reads them.
    pub(super) fn corrections() -> Self {
        Self::corrections_on(&fixture("orientation-1.jpg"))
    }

    /// `photograph` open in Corrections, with the flags read as the launch reads them.
    pub(super) fn corrections_on(photograph: &std::path::Path) -> Self {
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
            // A machine with room for every developer model.
            machine_memory_bytes: Some(16 << 30),
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
            photograph,
            Config {
                developer: true,
                ..Config::default()
            },
            |editor| {
                let (mut listed, _) =
                    call(&editor.owner, editor.client, "module.list", json!({})).unwrap();
                let modules: Vec<ModuleDescriptor> =
                    luxforge_core::module_layout::listed_descriptors(listed["modules"].take())
                        .unwrap();
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
        testing::read_flags(&mut filling.editor);
        filling.enter();
        filling
    }

    pub(super) fn owner(&self) -> OwnerHandle {
        self.editor.owner.clone()
    }

    pub(super) fn update(&mut self, message: Message) {
        let _ = self.editor.update(message);
        answer_reads(&mut self.editor);
    }

    pub(super) fn remove(&mut self, message: RemoveMessage) {
        self.update(Message::Remove(message));
    }

    pub(super) fn api(&self, method: &str, params: Value) -> Value {
        call(&self.owner(), self.editor.client, method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
            .0
    }

    /// Enter Corrections with its letter.
    pub(super) fn enter(&mut self) {
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
        assert!(
            self.editor.corrections_active(),
            "{}",
            self.editor.status.text
        );
    }

    /// Choose Remove, the kind control's last segment; the tiers are read.
    pub(super) fn choose(&mut self) {
        self.remove(RemoveMessage::Choose);
        self.answer_capabilities();
        assert!(self.editor.remove_active());
    }

    pub(super) fn key(&mut self, key: Key, modifiers: Keys) {
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
    pub(super) fn answer_capabilities(&mut self) -> Vec<Value> {
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
    pub(super) fn refresh_tiers(&mut self) {
        let _ = self.editor.remove_refresh_tiers();
        self.answer_capabilities();
    }

    pub(super) fn model(&self) -> &RemoveModel {
        self.editor
            .workspace
            .corrections
            .remove
            .as_ref()
            .expect("the Remove model")
    }

    /// Grant and install one developer model through the API, as any client does.
    pub(super) fn install(&self, resource: &str) {
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

    /// The generator installed: the Generative tier is offered.
    pub(super) fn ready(&mut self) {
        self.install(GENERATOR);
        self.refresh_tiers();
        assert_eq!(self.model().tiers.len(), 1, "{:?}", self.model());
    }

    pub(super) fn finished(&self, job: &str) -> Value {
        let mut record = Value::Null;
        wait_until("the job ending", || {
            record = self.api("job.read", json!({"job_id": job}));
            !matches!(record["status"].as_str(), Some("queued" | "running"))
        });
        record
    }

    pub(super) fn content_press(&mut self, at: [f64; 2]) {
        self.update(Message::Corrections(CorrectionMessage::Press {
            view: ((at[0] * 1000.0) as f32, (at[1] * 1000.0) as f32),
            content: at,
            modifiers: Modifiers::default(),
            hit: CanvasHit::Add,
        }));
    }

    /// Paint a horizontal stroke across the middle of the photograph: the selection.
    pub(super) fn paint(&mut self) {
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
    pub(super) fn sent(&mut self) -> (String, Value) {
        let status = self.editor.status.text.clone();
        self.editor
            .remove_reads
            .last_request
            .take()
            .unwrap_or_else(|| panic!("no request was sent: {status}"))
    }

    /// Copy one request as JSON: what the menu line copies.
    pub(super) fn copy(&mut self, request: RemoveRequest) -> Value {
        self.remove(RemoveMessage::Copy(request));
        self.editor
            .remove_reads
            .last_copied
            .take()
            .unwrap_or_else(|| panic!("nothing copied: {}", self.editor.status.text))
    }

    /// Run the start Generate or Regenerate sent, as its task would: its job's id.
    pub(super) fn start(&mut self) -> String {
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
    pub(super) fn poll(&mut self, job: &str) -> Value {
        let record = self.api("job.read", json!({"job_id": job}));
        let parsed: JobRecord = serde_json::from_value(record.clone()).unwrap();
        self.remove(RemoveMessage::Polled {
            job_id: job.to_owned(),
            result: Ok(Box::new(parsed)),
        });
        record
    }

    /// Run the preview reads the strip started until none is left.
    pub(super) fn read_previews(&mut self) {
        loop {
            let reading: Vec<_> = self.editor.remove_reads.reading.iter().cloned().collect();
            if reading.is_empty() {
                return;
            }
            for (job, candidate) in reading {
                let result =
                    read_preview(&self.owner(), self.editor.client, &job, candidate).map(Arc::new);
                self.remove(RemoveMessage::Preview {
                    job_id: job,
                    candidate,
                    result,
                });
            }
        }
    }

    /// Generate (or Regenerate) to its end, its candidates' previews read and the chosen one
    /// shown: its job.
    pub(super) fn generate(&mut self, regenerate: bool) -> String {
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
        self.show();
        job
    }

    /// Run the candidate draft's sets and rebases that wait for the owner to read a patch, as the
    /// blocking pool would, until none is left.
    pub(super) fn show(&mut self) {
        for _ in 0..8 {
            let sets = std::mem::take(&mut self.editor.reads_waiting.sets);
            let reapplies = std::mem::take(&mut self.editor.reads_waiting.reapplies);
            if sets.is_empty() && reapplies.is_empty() {
                return;
            }
            for set in sets {
                let answer = set.run();
                self.update(answer);
            }
            for reapply in reapplies {
                let answer = reapply.run();
                self.update(answer);
            }
        }
        panic!("the candidate draft kept reading");
    }

    /// The candidate the open candidate draft shows, once its set has been answered.
    pub(super) fn shown(&self) -> Option<(String, u8)> {
        let gesture = self.editor.core_gesture()?;
        let candidate = gesture.candidate()?;
        if !gesture.draft.drafted() {
            return None;
        }
        let session = self.editor.session.draft.as_ref()?;
        assert_eq!(session.action, "accept-correction-patch");
        assert_eq!(session.fields["job_id"], json!(candidate.job_id));
        assert_eq!(session.fields["candidate"], json!(candidate.candidate));
        gesture
            .draft
            .drafted()
            .then(|| (candidate.job_id.clone(), candidate.candidate))
    }

    /// Run the candidate draft's commit Accept sent, as its task would.
    pub(super) fn run_accept(&mut self) {
        assert!(
            testing::run_commit(&mut self.editor),
            "Accept commits the candidate draft: {}",
            self.editor.status.text
        );
        answer_reads(&mut self.editor);
    }

    /// Run the cancel the band (or Escape) sent, as its task would.
    pub(super) fn run_cancel(&mut self) {
        let (method, params) = self.sent();
        assert_eq!(method, "job.cancel");
        let job_id = params["job_id"].as_str().unwrap().to_owned();
        let result = call(&self.owner(), self.editor.client, &method, params).map(|_| ());
        self.remove(RemoveMessage::Cancelled { job_id, result });
    }

    pub(super) fn history(&self) -> Vec<String> {
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
    pub(super) fn agent_preview(&self, job: &str, candidate: u8) -> Vec<u8> {
        read_preview(&self.owner(), self.agent, job, candidate)
            .unwrap()
            .rgba
    }
}

/// A request as sent, without the request id each send draws afresh.
pub(super) fn without_request_id(mut request: Value) -> Value {
    if let Some(mutation) = request["params"]["mutation"].as_object_mut() {
        mutation.remove("request_id");
    }
    request
}

/// What Copy as JSON copies is `{method, params}` of the request the control then sends.
pub(super) fn same_request(copied: Value, sent: &(String, Value)) {
    assert_eq!(
        without_request_id(copied),
        without_request_id(json!({"method": sent.0, "params": sent.1})),
        "Copy as JSON copies the request the control sends"
    );
}

/// With the experiment on, Remove joins Heal and Clone, and with no model installed shows AI
/// editing's setup in place of the Fill setting: what it does, its download and Set up, which
/// installs through the capability driver and its consent notice; once it is installed the
/// Generative tier is listed.
#[test]
fn remove_shows_the_setup_until_the_model_is_installed() {
    let mut filling = Filling::opened();
    let band = &filling.editor.workspace.corrections;
    assert!(band.active);
    let model = filling.model();
    assert!(model.chosen);
    assert!(!model.reading, "the tiers were read on choosing Remove");
    assert!(model.tiers.is_empty());
    let unoffered = filling
        .model()
        .unoffered
        .clone()
        .expect("the model is missing");
    assert_eq!(unoffered.reason, "Set up AI editing first");
    assert_eq!(
        filling.model().generate_reason.as_deref(),
        Some("Set up AI editing first")
    );
    assert_eq!(
        filling.editor.status.text, "Set up AI editing first",
        "the status bar says what to do"
    );
    let line = unoffered.setup.expect("the setup's main line");
    assert_eq!(
        line.text,
        "Remove objects and fill in parts of a photo. AI editing runs on this computer, so your \
         photos stay on it."
    );
    assert_eq!(
        line.detail.as_deref(),
        Some("One-time download \u{b7} 46 B")
    );
    let Some((label, action)) = line.action.clone() else {
        panic!("a Set up action: {line:?}");
    };
    assert_eq!(label, "Set up");
    assert_eq!(
        action,
        SetupAction::Install {
            module_id: AI.into(),
            setup: "remove".into()
        }
    );
    assert!(
        filling.model().needs_setup,
        "the kind control marks Remove with the download glyph"
    );
    // The ⋯ menu copies exactly the request Set up sends.
    let copied = filling.copy(RemoveRequest::Setup);
    assert_eq!(
        without_request_id(copied),
        json!({"method": "module.setup.install",
               "params": {"module_id": AI, "setup": "remove", "mutation": {"actor": "desktop"}}})
    );
    // Set up goes through the capability driver, which asks for consent before anything.
    filling.remove(RemoveMessage::Setup(action));
    let started = filling.editor.capability_started.clone();
    assert_eq!(
        started,
        vec![(
            AI.to_owned(),
            Operation::InstallSetup {
                setup: "remove".into()
            }
        )]
    );
    let sent = filling.answer_capabilities();
    assert_eq!(sent[0]["method"], "module.setup.install");
    // The setup asks through the setup sheet, not a notice: the tool in its own words and three
    // pictured steps, then the facts summed over what is missing and the parts it includes; the
    // model's name, files and storage are left to Settings › Models.
    assert!(filling.editor.workspace.canvas.notices.is_empty());
    let sheet = filling
        .editor
        .workspace
        .setup_sheet
        .clone()
        .expect("the setup sheet");
    assert_eq!(
        (
            sheet.eyebrow.as_str(),
            sheet.title.as_str(),
            sheet.promise.as_str()
        ),
        (
            "AI editing",
            "Remove anything, fill it in naturally",
            "Your photos stay on this computer."
        )
    );
    assert_eq!(sheet.hero, None);
    assert_eq!(
        sheet
            .steps
            .iter()
            .map(|step| (step.title.as_str(), step.picture))
            .collect::<Vec<_>>(),
        [
            ("Paint over it", IntroductionPicture::PaintOver),
            ("Generate", IntroductionPicture::Candidates),
            ("Accept", IntroductionPicture::History),
        ]
    );
    assert_eq!(
        sheet.steps[2].hints,
        [("\u{2318}Z".to_owned(), "undoes it".to_owned())],
        "a step's keys are the keymap's"
    );
    assert_eq!(sheet.facts_label.as_deref(), Some("Before it can start"));
    assert_eq!(
        sheet
            .facts
            .iter()
            .map(|fact| (fact.headline.as_str(), fact.detail.as_str()))
            .collect::<Vec<_>>(),
        [
            ("46 B download", "Once, then it works offline"),
            (
                "From developer.luxforge.invalid",
                "Free \u{b7} GPL-3.0-or-later"
            ),
            (
                "Needs 2 GB of memory",
                &*format!(
                    "This {} has 16 GB \u{2713}",
                    crate::state::fill_setup::this_machine()
                )
            ),
        ]
    );
    assert_eq!(
        sheet
            .includes
            .iter()
            .map(|part| (part.name.as_str(), part.size.as_str(), part.installed))
            .collect::<Vec<_>>(),
        [("Generative fill", "46 B", false)]
    );
    assert_eq!(sheet.setup, "remove");
    assert_eq!(
        sheet.notes,
        [(
            "You can remove it at any time in Settings \u{203a} Models.".to_owned(),
            false
        )]
    );
    assert_eq!(
        (sheet.allow.as_deref(), sheet.dismiss.as_str()),
        (Some("Download 46 B"), "Not now")
    );
    assert!(
        filling.editor.workspace.covers_canvas(),
        "a press on the photograph does not paint behind the sheet"
    );

    filling.install(GENERATOR);
    filling.refresh_tiers();
    let model = filling.model();
    assert_eq!(model.unoffered, None);
    assert!(!model.needs_setup);
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

/// The setup sheet's Not now denies each scope it names and Set up then says so; its Download
/// grants exactly the disclosed scopes and asks for the setup again, and the sheet closes: the
/// download's progress is the Remove group's, where Set up was. Each sends exactly the requests the
/// sheet records.
#[test]
fn the_setup_sheets_download_closes_it_and_starts_the_install() {
    let mut filling = Filling::opened();
    let action = filling
        .model()
        .unoffered
        .clone()
        .and_then(|unoffered| unoffered.setup)
        .and_then(|line| line.action)
        .map(|(_, action)| action)
        .expect("Set up");
    filling.remove(RemoveMessage::Setup(action.clone()));
    filling.answer_capabilities();
    let sheet = filling
        .editor
        .workspace
        .setup_sheet
        .clone()
        .expect("the setup sheet");
    let strip = |requests: &[Value]| -> Vec<Value> {
        requests
            .iter()
            .map(|request| {
                let mut request = request.clone();
                request["params"]
                    .as_object_mut()
                    .unwrap()
                    .remove("mutation");
                request
            })
            .collect()
    };
    let deny = strip(&sheet.dismiss_requests);
    assert_eq!(
        deny,
        [json!({"method": "module.permission.deny", "params": {
            "module_id": AI, "capability": format!("download-{GENERATOR}"),
            "scope": {"resource": GENERATOR, "version": "1", "origin": ORIGIN}}})]
    );
    filling.update(Message::Capability(CapabilityMessage::Consent(false)));
    assert!(filling.editor.workspace.setup_sheet.is_none());
    let sent = filling.answer_capabilities();
    assert_eq!(
        strip(&sent[..1]),
        deny,
        "Not now sends what the sheet records"
    );
    filling.remove(RemoveMessage::Setup(action));
    filling.answer_capabilities();
    let sheet = filling
        .editor
        .workspace
        .setup_sheet
        .clone()
        .expect("the setup sheet again");
    assert_eq!(
        sheet.notes[0].0,
        "You chose Not now before. Nothing has been downloaded."
    );
    assert!(sheet.allow.is_some());
    let allow = strip(&sheet.allow_requests);
    assert_eq!(
        allow
            .iter()
            .map(|request| request["method"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["module.permission.grant", "module.setup.install"]
    );
    // The message the sheet's Download sends.
    filling.update(Message::Capability(CapabilityMessage::Consent(
        sheet.allow.is_some(),
    )));
    assert!(
        filling.editor.workspace.setup_sheet.is_none(),
        "the sheet closes on Download"
    );
    assert_eq!(filling.editor.key_context().setup_sheet, None);
    let sent = filling.answer_capabilities();
    assert_eq!(
        strip(&sent[..2]),
        allow,
        "Download grants the disclosed scope, then asks for the setup again"
    );
    wait_until("the install ending", || {
        filling.api("module.resource.list", json!({"module_id": AI}))["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == GENERATOR && row["state"] == "installed")
    });
}

/// A painted selection and Generate send the tier's fill request, the one Copy as JSON copies; the
/// ring shows while it runs and its one candidate fills the strip, chosen and
/// previewed on the canvas. An independent client sending the same request gets the same pixels.
#[test]
#[slow]
fn generate_sends_the_copied_request_and_fills_the_strip_with_one_candidate() {
    let mut filling = Filling::opened();
    filling.ready();
    filling.paint();
    let model = filling.model();
    assert_eq!(model.generate_reason, None, "{model:?}");
    assert!(model.hint.starts_with("Generate fills the selection"));
    let canvas = filling.editor.remove_canvas().expect("Remove draws");
    assert!(canvas.selection.is_some_and(|points| points.len() >= 2));
    assert!(!canvas.shown, "a draft selection is tinted");

    let copied = filling.copy(RemoveRequest::Generate);
    assert_eq!(
        filling.editor.status.text,
        format!("Copied the {TASK} request")
    );
    filling.remove(RemoveMessage::Generate);
    let sent = filling
        .editor
        .remove_reads
        .last_request
        .clone()
        .expect("Generate sent its request");
    same_request(copied, &sent);
    assert_eq!(sent.1["model"], GENERATOR);
    assert_eq!(
        sent.1["selection-radius"],
        json!(filling.editor.remove_radius())
    );
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
    assert!(
        filling.editor.remove_canvas().unwrap().progress.is_some(),
        "the ring"
    );
    assert_eq!(filling.finished(&job)["status"], "ready");
    filling.poll(&job);
    filling.read_previews();
    // The chosen candidate's draft reads its patch off the owner before it answers.
    assert_eq!(filling.editor.reads_waiting.sets.len(), 1);
    assert!(
        filling.shown().is_none(),
        "not shown before its patch is read"
    );
    let generation = filling.editor.presentation.preview_generation;
    filling.show();

    let model = filling.model();
    assert_eq!(model.running, None);
    assert_eq!(filling.editor.generation_lock(), None);
    assert_eq!(model.strip.len(), 1, "{:?}", model.strip);
    assert!(model.strip.iter().all(|cell| cell.state == "live"));
    assert!(model.strip[0].chosen, "the candidate is chosen");
    assert!(model.can_accept && model.can_regenerate);
    assert_eq!(
        filling.editor.status.text,
        "1 candidate \u{b7} Accept it, or Regenerate for another"
    );
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
    // The chosen candidate is the photograph's own draft, which the photo surface renders; the
    // canvas draws no candidate image and no tint over it.
    assert_eq!(filling.shown(), Some((job.clone(), 0)));
    assert!(filling.editor.remove_canvas().unwrap().shown);
    // Its frame leaves the status bar to what Remove said of it.
    let current = filling
        .editor
        .document
        .state
        .as_ref()
        .unwrap()
        .current_entry
        .id
        .clone();
    assert_eq!(
        filling.editor.displayed_status(&current),
        "1 candidate \u{b7} Accept it, or Regenerate for another"
    );
    assert!(
        filling.editor.presentation.preview_generation > generation
            || filling.editor.drag_frame_waiting(),
        "the drafted frame is asked for, or held for the surface to draw"
    );

    // Parity: the same request from an independent client makes the same candidates.
    let mut params = sent.1.clone();
    params["mutation"] = json!({"request_id": "agent-fill", "actor": "agent"});
    let (agent_job, _) = call(&filling.owner(), filling.agent, TASK, params).unwrap();
    let agent_job = agent_job["job_id"].as_str().unwrap().to_owned();
    assert_eq!(filling.finished(&agent_job)["status"], "ready");
    for candidate in 0..1u8 {
        assert_eq!(
            filling.agent_preview(&agent_job, candidate),
            filling.agent_preview(&job, candidate),
            "candidate {candidate} is the same for the same request"
        );
    }
}

/// The tools panel pins its notice while a generation is in flight: from Generate, before the
/// owner has named the job, while the job runs, and gone once it has ended. The frame records it.
#[test]
fn the_tools_panel_says_a_generation_may_slow_the_editor_while_it_runs() {
    const NOTICE: &str = "Generative fill in progress. The editor may be slow while it runs.";
    let mut filling = Filling::opened();
    filling.ready();
    filling.paint();
    let notice = |filling: &Filling| filling.editor.workspace.tools.notice.clone();
    assert_eq!(notice(&filling), None);

    filling.remove(RemoveMessage::Generate);
    assert_eq!(
        notice(&filling).as_deref(),
        Some(NOTICE),
        "the start is out"
    );
    let job = filling.start();
    assert_eq!(notice(&filling).as_deref(), Some(NOTICE), "the job runs");
    assert_eq!(filling.editor.snapshot()["tools_notice"], json!(NOTICE));

    assert_eq!(filling.finished(&job)["status"], "ready");
    filling.poll(&job);
    assert_eq!(notice(&filling), None, "the job has ended");
    assert_eq!(filling.editor.snapshot()["tools_notice"], Value::Null);
}

/// Regenerate asks again from the newest job's next seed and adds one more to the strip; the
/// earlier candidate stays. Choosing one previews it on the canvas and Accept commits exactly it:
/// one history entry, and the selection and strip are cleared.
#[test]
fn regenerate_adds_one_and_accept_commits_the_chosen_candidate_as_one_entry() {
    let mut filling = Filling::opened();
    filling.ready();
    filling.paint();
    let first = filling.generate(false);
    let next_seed = filling
        .editor
        .remove
        .job(&first)
        .unwrap()
        .next_seed
        .unwrap();

    let copied = filling.copy(RemoveRequest::Regenerate);
    filling.remove(RemoveMessage::Regenerate);
    let sent = filling.editor.remove_reads.last_request.clone().unwrap();
    same_request(copied, &sent);
    assert_eq!(
        sent.1["seed"], next_seed,
        "Regenerate continues from the next seed"
    );
    let second = filling.start();
    assert_eq!(filling.finished(&second)["status"], "ready");
    filling.poll(&second);
    filling.read_previews();
    let model = filling.model();
    assert_eq!(model.strip.len(), 2, "{:?}", model.strip);
    assert_eq!(model.strip[1].job_id, second);

    // The new job's first candidate was chosen and drafted, in the same draft the first job's
    // was. Choose the first job's candidate, then the second's: the draft follows each choice.
    filling.show();
    assert_eq!(filling.shown(), Some((second.clone(), 0)));
    let draft = filling
        .editor
        .session
        .draft
        .as_ref()
        .unwrap()
        .draft_id
        .clone();
    filling.remove(RemoveMessage::Candidate {
        job_id: first.clone(),
        candidate: 0,
    });
    assert!(filling.model().strip[0].chosen);
    filling.show();
    assert_eq!(filling.shown(), Some((first.clone(), 0)));
    filling.remove(RemoveMessage::Candidate {
        job_id: second.clone(),
        candidate: 0,
    });
    assert!(filling.model().strip[1].chosen);
    filling.show();
    assert_eq!(filling.shown(), Some((second.clone(), 0)));
    assert_eq!(
        filling.editor.session.draft.as_ref().unwrap().draft_id,
        draft,
        "one candidate draft follows the choice"
    );
    let copied = filling.copy(RemoveRequest::Preview);
    assert_eq!(
        copied,
        json!({"method": "job.read-candidate", "params": {"job_id": second, "candidate": 0, "part": "preview"}})
    );

    let before = filling.history().len();
    let copied = filling.copy(RemoveRequest::Accept);
    let generation = filling.editor.presentation.preview_generation;
    filling.remove(RemoveMessage::Accept);
    let sent = filling.editor.remove_reads.last_request.clone().unwrap();
    same_request(copied, &sent);
    assert_eq!(sent.0, "edit.accept-correction-patch");
    assert_eq!(sent.1["job_id"], second);
    assert_eq!(sent.1["candidate"], 0);
    assert!(filling.model().accepting);
    // The commit is out: the draft's frame stays on screen until the committed entry's.
    assert_eq!(
        testing::core_draft(&filling.editor).unwrap().in_flight(),
        Some(super::draft::Round::Commit)
    );
    assert_eq!(
        filling.editor.presentation.preview_generation, generation,
        "no frame of the unfilled photograph is asked for"
    );
    filling.run_accept();
    let history = filling.history();
    assert_eq!(history.len(), before + 1, "{history:?}");
    assert_eq!(history[0], "Add Generative fill 1");
    let model = filling.model();
    assert!(!model.selection && model.strip.is_empty(), "{model:?}");
    assert_eq!(
        filling.editor.status.text,
        "Accepted \u{b7} \u{2318}Z undoes it"
    );
    // The committed entry's frame says so too, until a new selection is painted.
    let current = filling
        .editor
        .document
        .state
        .as_ref()
        .unwrap()
        .current_entry
        .id
        .clone();
    assert_eq!(
        filling.editor.displayed_status(&current),
        "Accepted \u{b7} \u{2318}Z undoes it"
    );
    assert!(filling.editor.core_gesture().is_none() && filling.editor.session.draft.is_none());
    assert!(!filling.editor.remove_canvas().unwrap().shown);
    // The candidate accepted is the one the agent reads as accepted.
    let record = filling.api("job.read", json!({"job_id": second}));
    assert_eq!(record["candidates"][0]["state"], "accepted", "{record}");
}

/// A prompt makes the action Replace. Without the Replace add-on the field offers its Add and
/// Generate sends nothing; with it the prompt is sent and the entry is labelled with it. The
/// field holds at most the declared 256 characters.
#[test]
fn a_prompt_replaces_and_without_the_addon_generates_nothing() {
    let mut filling = Filling::opened();
    filling.ready();
    filling.paint();
    filling.remove(RemoveMessage::Prompt("x".repeat(300)));
    assert_eq!(filling.model().prompt.chars().count(), 256);
    assert_eq!(filling.model().prompt_max, 256);
    filling.remove(RemoveMessage::Prompt("a red  bicycle ".into()));
    let model = filling.model();
    assert_eq!(model.action, "Replace");
    let addon = model.addon.clone().expect("the Replace add-on");
    assert_eq!(addon.setup, "replace");
    assert_eq!(addon.text, "Add Replace to fill with what you describe.");
    assert_eq!(addon.action.map(|(label, _)| label).as_deref(), Some("Add"));
    assert_eq!(
        model.generate_reason.as_deref(),
        Some("Add Replace to fill with what you describe")
    );
    filling.remove(RemoveMessage::Generate);
    assert!(
        filling.editor.remove_reads.last_request.is_none(),
        "nothing is sent without the add-on"
    );
    assert_eq!(
        filling.editor.status.text,
        "Add Replace to fill with what you describe"
    );

    filling.install(ENCODER);
    filling.refresh_tiers();
    assert_eq!(filling.model().addon, None);
    let copied = filling.copy(RemoveRequest::Generate);
    filling.remove(RemoveMessage::Generate);
    let sent = filling.editor.remove_reads.last_request.clone().unwrap();
    same_request(copied, &sent);
    assert_eq!(
        sent.1["prompt"], "a red bicycle",
        "the prompt as an API client sends it"
    );
    let job = filling.start();
    assert!(filling.model().running.as_ref().is_some_and(|running| {
        running
            .sentence
            .starts_with("Replace \u{201c}a red bicycle\u{201d}")
    }));
    assert_eq!(filling.finished(&job)["status"], "ready");
    filling.poll(&job);
    filling.read_previews();
    filling.show();
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
#[slow]
fn a_running_generation_locks_its_photograph_and_escape_cancels_it_then_clears() {
    let mut filling = Filling::opened();
    filling.ready();
    filling
        .fake
        .controls()
        .set_step_delay(Duration::from_millis(200));
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

    assert_eq!(
        filling.editor.generation_lock().as_deref(),
        Some(GENERATING)
    );
    assert_eq!(
        filling.editor.gesture_refusal(Starting::Action).as_deref(),
        Some(GENERATING)
    );
    assert_eq!(
        filling
            .editor
            .gesture_refusal(Starting::Correction)
            .as_deref(),
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
    let exposure = |asset: &AssetId, revision: u64| json!({"asset_id": asset, "exposure": 0.5, "mutation": tasks::mutation(revision)});
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
    assert!(
        refused.contains("locked") || refused.contains("conflict"),
        "{refused}"
    );
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
    assert_eq!(
        filling.editor.generation_lock(),
        None,
        "the lock is released at once"
    );
    assert_eq!(filling.editor.gesture_refusal(Starting::Action), None);
    assert_eq!(filling.editor.status.text, "Generation cancelled");
    assert_eq!(filling.finished(&job)["status"], "cancelled");
    assert!(
        filling.model().selection,
        "the first Escape keeps the selection"
    );

    filling.key(Key::Named(Named::Escape), Keys::empty());
    let model = filling.model();
    assert!(!model.selection && model.strip.is_empty());
    assert_eq!(filling.editor.status.text, "Selection cleared");
    assert!(
        !filling
            .history()
            .iter()
            .any(|label| label.contains("Generative")),
        "a cancelled generation commits nothing"
    );
}

/// The candidate draft holds this client's one draft only while Remove wants a candidate shown: a
/// new stroke, Escape, another kind of the band, leaving the mode and opening another photograph
/// each cancel it at the owner, and nothing of it is left in the session.
#[test]
#[slow]
fn a_new_stroke_escape_leaving_remove_and_another_photograph_cancel_the_candidate_draft() {
    let mut filling = Filling::opened();
    filling.ready();
    let gone = |filling: &Filling, what: &str| {
        assert!(filling.editor.core_gesture().is_none(), "{what}");
        assert!(filling.editor.session.draft.is_none(), "{what}");
        let (session, _) = call(
            &filling.owner(),
            filling.editor.client,
            "session.state",
            json!({}),
        )
        .unwrap();
        assert!(session["draft"].is_null(), "{what}: {session}");
        assert!(
            !filling
                .editor
                .remove_canvas()
                .is_some_and(|canvas| canvas.shown)
        );
    };
    let shown = |filling: &mut Filling| {
        filling.paint();
        let job = filling.generate(false);
        assert_eq!(filling.shown(), Some((job, 0)));
    };

    // A new stroke: the press cancels the draft, and its release is the next selection.
    shown(&mut filling);
    filling.content_press([0.2, 0.2]);
    gone(&filling, "a new stroke");
    filling.update(Message::Corrections(CorrectionMessage::Release {
        view: (200.0, 200.0),
        content: [0.2, 0.2],
    }));
    assert!(filling.model().selection);
    let job = filling.generate(false);
    assert_eq!(filling.shown(), Some((job, 0)));
    // Escape after a generation clears the selection and strip, and the draft with them.
    filling.key(Key::Named(Named::Escape), Keys::empty());
    assert_eq!(filling.editor.status.text, "Selection cleared");
    gone(&filling, "Escape");
    // Another kind of the band.
    shown(&mut filling);
    filling.update(Message::Corrections(CorrectionMessage::Kind("heal".into())));
    gone(&filling, "Heal chosen");
    // Leaving the mode. Choosing Remove again shows the candidate still chosen, until the
    // next stroke.
    filling.choose();
    filling.show();
    assert!(filling.shown().is_some());
    shown(&mut filling);
    filling.key(Key::Character("q".into()), Keys::empty());
    gone(&filling, "the mode left");
    // Another photograph.
    filling.enter();
    filling.choose();
    filling.show();
    shown(&mut filling);
    let _ = filling
        .editor
        .open_queued(fixture("jpeg-scan-444-interleaved.jpg"), None);
    gone(&filling, "another photograph");
}

/// A candidate the owner will not draft — here released by an API client after the strip read it
/// live — leaves the photograph unfilled: the draft is cancelled, the band and the status bar name
/// the owner's refusal, Accept sends nothing, and choosing a live candidate shows it again. A
/// Regenerate runs with the chosen candidate still shown.
#[test]
#[slow]
fn a_candidate_the_owner_refuses_leaves_the_photograph_unfilled_and_says_why() {
    let mut filling = Filling::opened();
    filling.ready();
    filling.paint();
    let first = filling.generate(false);
    assert_eq!(filling.shown(), Some((first.clone(), 0)));
    filling.remove(RemoveMessage::Regenerate);
    assert!(
        filling.editor.core_gesture().is_some(),
        "the shown candidate stays while the next generation runs: {}",
        filling.editor.status.text
    );
    let second = filling.start();
    assert_eq!(filling.finished(&second)["status"], "ready");
    filling.poll(&second);
    filling.read_previews();
    filling.show();
    assert_eq!(filling.shown(), Some((second.clone(), 0)));

    filling.api(
        "job.release-candidates",
        json!({"job_id": first, "candidates": [0], "mutation": request()}),
    );
    filling.remove(RemoveMessage::Candidate {
        job_id: first.clone(),
        candidate: 0,
    });
    filling.show();
    assert!(filling.editor.core_gesture().is_none(), "nothing is shown");
    assert!(filling.editor.session.draft.is_none());
    let notice = filling
        .model()
        .notice
        .clone()
        .expect("the refusal is named");
    assert_eq!(notice.code, "conflict", "{notice:?}");
    assert!(
        filling.editor.status.text.starts_with(&notice.text),
        "{}",
        filling.editor.status.text
    );
    assert!(!filling.editor.remove_canvas().unwrap().shown);
    assert_eq!(
        filling
            .editor
            .remove_reads
            .unshown
            .as_ref()
            .map(|(key, _)| key),
        Some(&(first.clone(), 0))
    );
    // Accept has nothing shown to commit.
    filling.remove(RemoveMessage::Accept);
    assert!(filling.editor.remove_reads.last_request.is_none());
    assert_eq!(filling.editor.status.text, notice.text);
    // A live candidate is shown again.
    filling.remove(RemoveMessage::Candidate {
        job_id: second.clone(),
        candidate: 0,
    });
    filling.show();
    assert_eq!(filling.shown(), Some((second, 0)));
    assert_eq!(filling.model().notice, None);
}

/// A selection whose bounds are past half the frame is refused at the start in a plain line: the
/// status bar names what to do, while the refusal's code, words and limit stay in the band's notice
/// and the event log, as an API client receives them.
#[test]
#[slow]
fn a_selection_too_large_to_fill_is_refused_in_a_plain_line() {
    let mut filling = Filling::opened();
    filling.ready();
    filling.content_press([0.05, 0.05]);
    for step in 1..=18 {
        let at = 0.05 + 0.05 * f64::from(step);
        filling.update(Message::Corrections(CorrectionMessage::Drag {
            view: ((at * 1000.0) as f32, (at * 1000.0) as f32),
            content: [at, at],
        }));
    }
    filling.update(Message::Corrections(CorrectionMessage::Release {
        view: (950.0, 950.0),
        content: [0.95, 0.95],
    }));
    filling.remove(RemoveMessage::Generate);
    let (method, params) = filling.sent();
    let answer = start_fill(&filling.owner(), filling.editor.client, &method, params);
    let error = answer
        .clone()
        .expect_err("the region is past half the frame");
    assert_eq!(error.code, "resource-limit");
    assert_eq!(error.data.as_ref().unwrap()["limit"], "half-frame");
    filling.remove(RemoveMessage::Started(answer));
    assert_eq!(filling.editor.status.text, super::remove::TOO_LARGE_PAINTED);
    let notice = filling.model().notice.clone().expect("the refusal is kept");
    assert_eq!(notice.code, "resource-limit");
    assert_eq!(notice.text, error.message);
}

/// A stroke painted after the brush was resized is drawn at the new size, while the selection
/// already painted keeps the size it was painted at.
#[test]
#[slow]
fn a_stroke_over_a_selection_is_drawn_at_the_brush_size_now() {
    let mut filling = Filling::opened();
    filling.ready();
    filling.paint();
    let painted = filling.editor.remove_canvas().unwrap().radius;
    filling.update(Message::Corrections(CorrectionMessage::Nudge {
        feather: false,
        up: false,
    }));
    filling.content_press([0.4, 0.7]);
    filling.update(Message::Corrections(CorrectionMessage::Drag {
        view: (500.0, 700.0),
        content: [0.5, 0.7],
    }));
    let canvas = filling.editor.remove_canvas().unwrap();
    assert!(canvas.live.is_some(), "a stroke is being painted");
    assert_eq!(canvas.radius, painted, "the selection keeps its size");
    assert_eq!(canvas.live_radius, filling.editor.remove_radius());
    assert!(canvas.live_radius < painted, "the brush was made smaller");
}
