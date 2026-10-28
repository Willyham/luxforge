//! Masks › Object against a real owner serving the developer `luxforge.ai` module, its fake
//! segmenter and the deterministic fake runtime.
//!
//! Every control goes through the update function as the menu, the canvas, the keyboard and the
//! row send it. The owner work it starts is run here as the plain calls its tasks wrap, and each
//! answer is handed back as the task's message. Each request is checked against the one Copy as
//! JSON copies and the one an independent JSON client sends, and each pick as exactly one history
//! entry with its label.
use super::{
    Editor,
    message::{
        Message,
        capability::CapabilityMessage,
        mask::{KindMenu, MaskMessage, ObjectRequest},
        object_pick::{ObjectPickMessage, ObjectPointer},
        sync::SyncMessage,
        view::ViewMessage,
    },
    object_pick::{SetupId, SetupReady, pick, queue_analysis},
    tasks::{self, call, request},
    testing,
};
use crate::{
    Config,
    object_pick::{self as shared, Decision, Found, FoundCandidate, SelectFor},
    state::{capabilities::Operation, fill_setup::SetupAction},
};
use iced::keyboard::{Key, Modifiers, key::Named};
use luxforge_core::{
    AssetId, ClientId, HostConfig, MASK_MODE, ModuleDescriptor, OwnerHandle,
    capabilities::secrets::MemorySecretStore,
    inference::{
        CompositeInference, InferenceRuntime,
        fake::{FAKE_BACKEND, FAKE_HOST_BACKEND, FakeInference},
    },
    mask::commands::MaskListing,
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
};

static NEXT: AtomicU64 = AtomicU64::new(1);

const AI: &str = "luxforge.ai";
const ORIGIN: &str = "https://developer.luxforge.invalid";
const ENCODER: &str = "fake-segmenter-encoder";
const DECODER: &str = "fake-segmenter-decoder";
/// The top-left quadrant of the four-quadrant fixture, which the fake decoder finds as one object.
const LEFT: [f64; 2] = [0.25, 0.25];
const RIGHT: [f64; 2] = [0.75, 0.25];

/// The desktop on one photograph, in Mask mode, with the developer AI module beside the product
/// modules and the experiment on.
struct Picking {
    editor: Editor,
    root: PathBuf,
    asset: AssetId,
    agent: ClientId,
    _fake: Arc<FakeInference>,
}

impl Drop for Picking {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl Picking {
    fn opened() -> Self {
        let root = std::env::temp_dir().join(format!(
            "luxforge-mask-object-{}-{}",
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
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0/orientation-1.jpg");
        let (editor, asset, agent, ()) = testing::real_photo_prepared(
            owner,
            join,
            &fixture,
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
        let mut picking = Self {
            editor,
            root,
            asset,
            agent,
            _fake: fake,
        };
        picking.api("flags.set", json!({"flag": "ai.analysis", "value": true}));
        testing::read_flags(&mut picking.editor);
        picking.enter_mask_mode();
        picking.read_status();
        picking
    }

    fn owner(&self) -> OwnerHandle {
        self.editor.owner.clone()
    }

    fn update(&mut self, message: Message) {
        let _ = self.editor.update(message);
    }

    fn mask(&mut self, message: MaskMessage) {
        self.update(Message::Mask(message));
    }

    fn object(&mut self, message: ObjectPickMessage) {
        self.update(Message::ObjectPick(message));
    }

    fn api(&self, method: &str, params: Value) -> Value {
        call(&self.owner(), self.editor.client, method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
            .0
    }

    fn enter_mask_mode(&mut self) {
        self.update(Message::View(ViewMessage::SetMode(MASK_MODE.to_owned())));
        let _ = call(
            &self.owner(),
            self.editor.client,
            "workspace.set",
            json!({ "mode": MASK_MODE }),
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
            self.editor.mask_mode_active(),
            "{}",
            self.editor.status.text
        );
    }

    /// Run the capability operations the update function started, as the driver's tasks would,
    /// and hand each answer back.
    fn answer_capabilities(&mut self) {
        loop {
            let started = std::mem::take(&mut self.editor.capability_started);
            if started.is_empty() {
                return;
            }
            for (module, op) in started {
                let answer =
                    super::capabilities::run(&self.editor.owner, self.editor.client, module, op);
                self.update(Message::Capability(CapabilityMessage::Answered(Box::new(
                    answer,
                ))));
            }
        }
    }

    /// Read the AI module's status, as an install's end does.
    fn read_status(&mut self) {
        let _ = self.editor.capability_op(AI, Operation::Refresh);
        self.answer_capabilities();
    }

    fn finished(&self, job: &str) -> Value {
        let mut record = Value::Null;
        wait_until("the job ending", || {
            record = self.api("job.read", json!({"job_id": job}));
            !matches!(record["status"].as_str(), Some("queued" | "running"))
        });
        record
    }

    /// Grant and install the object finder through the API, as any client does, and read the
    /// status again.
    fn install(&mut self) {
        for resource in [ENCODER, DECODER] {
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
        self.read_status();
    }

    /// Answer the session's entry: each model load, the analysis and the content map.
    fn answer_entry(&mut self) {
        let session = self.editor.object_pick.as_ref().expect("picking");
        for resource in session.loads.clone() {
            let result = tasks::call_detailed(
                &self.owner(),
                self.editor.client,
                shared::RUNTIME_LOAD,
                json!({"module_id": AI, "resource_id": resource}),
            )
            .map_err(|error| error.message);
            self.object(ObjectPickMessage::Loaded { resource, result });
        }
        let session = self.editor.object_pick.as_ref().unwrap();
        if let Some((asset, revision)) = session.analysing.clone() {
            let job = queue_analysis(&self.owner(), self.editor.client, &asset).unwrap();
            assert_eq!(self.finished(&job)["status"], "ready");
            self.object(ObjectPickMessage::Analysed {
                asset,
                revision,
                result: Ok(()),
            });
        }
        self.answer_map();
    }

    /// Answer the session's `render.transform`, when it asked for one.
    fn answer_map(&mut self) {
        let Some(gesture) = self
            .editor
            .object_pick
            .as_ref()
            .and_then(|session| session.request)
        else {
            return;
        };
        let (transform, _) = call(
            &self.owner(),
            self.editor.client,
            "render.transform",
            json!({"asset_id": self.asset}),
        )
        .unwrap();
        self.object(ObjectPickMessage::Transform(
            gesture,
            Ok(serde_json::from_value(transform).unwrap()),
        ));
    }

    /// Choose Object… from a kind menu with the object finder installed, and answer the entry.
    fn choose(&mut self, menu: KindMenu) {
        self.mask(MaskMessage::ChooseObject(menu));
        assert!(
            self.editor.object_pick.is_some(),
            "{}",
            self.editor.status.text
        );
        self.answer_entry();
    }

    /// Answer the hover pick in flight, as its task would.
    fn answer_hover(&mut self) {
        let session = self.editor.object_pick.as_ref().unwrap();
        let point = session.asking.expect("a hover pick is in flight");
        let params = session.last_request.clone().unwrap();
        assert_eq!(params, shared::point_query(&self.asset, point));
        let revision = self.editor.document.state.as_ref().unwrap().revision;
        let result = pick(&self.owner(), self.editor.client, params, false);
        self.object(ObjectPickMessage::Hovered {
            asset: self.asset.clone(),
            revision,
            point,
            result,
        });
    }

    /// Hover at `point` and answer the picks it waits on: one already in flight (latest wins, so
    /// `point` is sent once it answers), then its own.
    fn hover(&mut self, point: [f64; 2]) {
        self.object(ObjectPickMessage::Hover(Some(point)));
        while self
            .editor
            .object_pick
            .as_ref()
            .is_some_and(|session| session.asking.is_some_and(|asking| asking != point))
        {
            self.answer_hover();
        }
        assert_eq!(
            self.editor.object_pick.as_ref().unwrap().asking,
            Some(point)
        );
        self.answer_hover();
    }

    fn click(&mut self, at: [f64; 2]) {
        let view = ((at[0] * 1000.0) as f32, (at[1] * 1000.0) as f32);
        self.object(ObjectPickMessage::Pointer(ObjectPointer::Press {
            view,
            at,
            modifiers: crate::brush::Modifiers::default(),
        }));
        self.object(ObjectPickMessage::Pointer(ObjectPointer::Release {
            view,
            at,
        }));
    }

    /// Run the selection the desktop is running, as its task would, and hand back its answer.
    fn select(&mut self) -> Value {
        let (purpose, params) = self
            .editor
            .object_selects
            .running
            .clone()
            .expect("a selection is running");
        let queued = loop {
            match tasks::call_detailed(
                &self.owner(),
                self.editor.client,
                shared::SELECT,
                params.clone(),
            ) {
                Ok(queued) => break queued,
                Err(error) if error.code == "preparation-required" => {
                    let job = error.job_id.unwrap();
                    assert_eq!(self.finished(&job)["status"], "ready");
                }
                Err(error) => panic!("task.select-object: {}", error.message),
            }
        };
        let record = self.finished(queued["job_id"].as_str().unwrap());
        assert_eq!(record["status"], "ready", "{record}");
        let selection = record["result"]["result"].clone();
        self.object(ObjectPickMessage::Selected {
            purpose,
            result: Ok(selection.clone()),
        });
        selection
    }

    /// Run the mask command the panel sent, as its task would, and read the asset back: its
    /// method and params.
    fn commit(&mut self) -> (String, Value) {
        let (method, params) = self
            .editor
            .mask_panel
            .last_request
            .clone()
            .expect("the selection committed a mask command");
        call(&self.owner(), self.editor.client, &method, params.clone())
            .unwrap_or_else(|error| panic!("{method} was refused: {error}"));
        self.editor.busy = false;
        self.refresh();
        (method, params)
    }

    fn refresh(&mut self) {
        let refreshed = tasks::refresh(
            &self.owner(),
            self.editor.client,
            self.asset.clone(),
            tasks::Scope::Open,
            None,
        )
        .unwrap();
        self.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
            refreshed,
        )))));
    }

    /// Every mask entry's label, oldest first.
    fn labels(&self) -> Vec<String> {
        let listed = self.api(
            "history.list",
            json!({"asset_id": self.asset, "limit": 100}),
        );
        let mut labels: Vec<String> = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| {
                entry["action_id"]
                    .as_str()
                    .is_some_and(|action| action.starts_with("mask."))
            })
            .map(|entry| entry["label"].as_str().unwrap_or_default().to_owned())
            .collect();
        labels.reverse();
        labels
    }

    fn listing(&self) -> MaskListing {
        let (listed, _) = call(
            &self.owner(),
            self.agent,
            "mask.list",
            json!({"asset_id": self.asset}),
        )
        .unwrap();
        serde_json::from_value(listed).unwrap()
    }

    fn key(&mut self, key: Key) {
        let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
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

    /// One pick into the open sub-state at `point`: hover, click, the selection and its commit.
    fn pick_at(&mut self, point: [f64; 2]) -> (String, Value) {
        self.hover(point);
        self.click(point);
        self.select();
        self.commit()
    }
}

/// The request with its mutation's request id replaced by a marker, so two builds of one request
/// compare equal.
fn identified(mut request: Value) -> Value {
    request["mutation"]["request_id"] = json!("<fresh>");
    request
}

/// Object… is the Select group's entry, listed only while the run offers `task.select-object`
/// with the experiment on, and marked for setup until the object finder is installed.
#[test]
#[slow]
fn mask_object_is_listed_in_the_select_group_while_the_experiment_is_on() {
    let mut picking = Picking::opened();
    let select = &picking.editor.workspace.masks.select;
    assert_eq!(select.len(), 1, "{select:?}");
    assert_eq!(select[0].label, "Object\u{2026}");
    assert!(select[0].needs_setup);
    picking.install();
    assert!(!picking.editor.workspace.masks.select[0].needs_setup);
    picking.api("flags.set", json!({"flag": "ai.analysis", "value": false}));
    testing::read_flags(&mut picking.editor);
    assert!(picking.editor.workspace.masks.select.is_empty());
}

/// Without the object finder, Object… sends what its setup line's Set up sends,
/// `module.setup.install` of `mask-object`, through the setup flow, whose refusal for want of
/// consent opens the setup sheet.
#[test]
#[slow]
fn mask_object_without_the_object_finder_opens_the_setup_sheet() {
    let mut picking = Picking::opened();
    let SetupReady::Missing(line) = picking.editor.setup_ready(SetupId::MaskObject) else {
        panic!("the object finder is missing");
    };
    assert_eq!(
        line.action.map(|(_, action)| action),
        Some(SetupAction::Install {
            module_id: AI.into(),
            setup: "mask-object".into()
        })
    );
    assert_eq!(
        picking.editor.workspace.masks.select[0].tag,
        "set up\u{2026}"
    );
    picking.mask(MaskMessage::ChooseObject(KindMenu::New));
    assert!(picking.editor.object_pick.is_none(), "nothing picks yet");
    assert_eq!(
        picking.editor.capability_started,
        vec![(
            AI.to_owned(),
            Operation::InstallSetup {
                setup: "mask-object".into()
            }
        )]
    );
    picking.answer_capabilities();
    let sheet = picking
        .editor
        .workspace
        .setup_sheet
        .clone()
        .expect("the setup sheet");
    assert!(!sheet.title.is_empty());
}

/// Entering loads both models and analyses the photograph at once, saying Finding objects…
/// until the analysis answers; the bar names the gesture.
#[test]
#[slow]
fn mask_object_entering_loads_the_models_and_analyses_the_photograph() {
    let mut picking = Picking::opened();
    picking.install();
    picking.mask(MaskMessage::ChooseObject(KindMenu::New));
    let session = picking.editor.object_pick.as_ref().expect("picking");
    assert_eq!(session.loads, vec![ENCODER.to_owned(), DECODER.to_owned()]);
    assert!(session.finding_objects());
    assert_eq!(session.analyses, 1);
    let model = &picking.editor.workspace.masks;
    assert!(model.object.as_ref().is_some_and(|object| object.finding));
    assert!(
        picking
            .editor
            .workspace
            .canvas
            .notices
            .iter()
            .any(|notice| notice.title == shared::FINDING_OBJECTS)
    );
    let bar = picking.editor.workspace.canvas.draft_bar.as_ref().unwrap();
    assert_eq!(bar.title, "New mask");
    assert_eq!(bar.subject.as_deref(), Some("Object"));
    assert_eq!(bar.readout, crate::state::canvas::OBJECT_HINT);
    assert!(bar.cancel_only);
    picking.answer_entry();
    let session = picking.editor.object_pick.as_ref().unwrap();
    assert!(!session.finding_objects());
    assert!(session.loads.is_empty() && session.map.is_some());
}

/// A click on the outlined object sends the selection an API client sends and commits one
/// `mask.create-model` entry, `Add object (model)`; the sub-state stays armed, and the next pick
/// is one `mask.add-model` into the same mask in the Add row's mode.
#[test]
#[slow]
fn mask_object_each_pick_commits_one_entry_into_the_same_mask() {
    let mut picking = Picking::opened();
    picking.install();
    picking.choose(KindMenu::New);
    picking.hover(LEFT);
    assert!(
        picking.editor.object_overlay().is_some(),
        "the object is outlined"
    );
    picking.click(LEFT);
    let (_, sent) = picking.editor.object_selects.running.clone().unwrap();
    assert_eq!(
        identified(sent),
        identified(shared::select_request(
            &picking.asset,
            &shared::click_prompt(LEFT),
            json!(request())
        ))
    );
    let selection = picking.select();
    let (method, params) = picking.commit();
    assert_eq!(method, "mask.create-model");
    for field in super::mask_object::SELECTION_FIELDS {
        assert_eq!(params[field], selection[field], "{field}");
    }
    assert_eq!(picking.labels(), vec!["Add object (model)"]);
    let listing = picking.listing();
    let mask = listing.masks[0].id.clone();
    assert_eq!(picking.editor.mask_panel.selected_mask, Some(mask.clone()));
    assert!(picking.editor.object_pick.is_some(), "still armed");
    assert_eq!(
        picking.editor.mask_panel.object.as_ref().unwrap().mask,
        Some(mask.clone())
    );

    picking.mask(MaskMessage::SetAddMode(1));
    let (method, params) = picking.pick_at(RIGHT);
    assert_eq!(method, "mask.add-model");
    assert_eq!(params["mask"], json!(mask));
    assert_eq!(params["mode"], json!("subtract"));
    assert_eq!(
        picking.labels(),
        vec!["Add object (model)", "Add subtract object (model)"]
    );
    assert_eq!(picking.listing().masks.len(), 1);
}

/// A click with nothing outlined selects nothing and says so.
#[test]
#[slow]
fn mask_object_a_click_with_nothing_outlined_says_so() {
    let mut picking = Picking::opened();
    picking.install();
    picking.choose(KindMenu::New);
    picking.click(LEFT);
    assert_eq!(picking.editor.status.text, shared::NOTHING_TO_SELECT);
    assert!(picking.editor.object_selects.idle());
}

/// A loose drag asks the picker about its stroke with every preview; Several puts up numbered
/// pins, a pin toggles, and Use selects each chosen candidate by its position; Escape dismisses
/// pins first and then leaves picking.
#[test]
#[slow]
fn mask_object_a_loose_drag_with_several_objects_puts_up_pins() {
    let mut picking = Picking::opened();
    picking.install();
    picking.choose(KindMenu::New);
    let stroke = [[0.2, 0.25], [0.5, 0.25], [0.8, 0.25]];
    let view = |at: [f64; 2]| ((at[0] * 1000.0) as f32, (at[1] * 1000.0) as f32);
    picking.object(ObjectPickMessage::Pointer(ObjectPointer::Press {
        view: view(stroke[0]),
        at: stroke[0],
        modifiers: crate::brush::Modifiers::default(),
    }));
    for at in &stroke[1..] {
        picking.object(ObjectPickMessage::Pointer(ObjectPointer::Move {
            view: view(*at),
            at: *at,
        }));
    }
    picking.object(ObjectPickMessage::Pointer(ObjectPointer::Release {
        view: view(stroke[2]),
        at: stroke[2],
    }));
    let session = picking.editor.object_pick.as_ref().unwrap();
    let (sent, _) = session.finding.clone().expect("the stroke is being found");
    assert_eq!(
        session.last_request,
        Some(shared::stroke_query(
            &picking.asset,
            &sent,
            super::mask_object::STROKE_RADIUS
        ))
    );
    // The picker's Several, as its answer decodes.
    let revision = picking.editor.document.state.as_ref().unwrap().revision;
    let found = Found {
        decision: Decision::Several,
        chosen: vec![0, 2],
        candidates: vec![
            FoundCandidate {
                position: 0,
                bounds: [0.0, 0.0, 0.5, 0.5],
                image: None,
                tinted: None,
            },
            FoundCandidate {
                position: 2,
                bounds: [0.5, 0.0, 1.0, 0.5],
                image: None,
                tinted: None,
            },
        ],
    };
    picking.object(ObjectPickMessage::Found {
        asset: picking.asset.clone(),
        revision,
        stroke: sent.clone(),
        result: Ok(Box::new(found)),
    });
    assert_eq!(
        picking.editor.workspace.masks.object.as_ref().unwrap().pins,
        Some((2, 0))
    );
    picking.object(ObjectPickMessage::Pin(1));
    assert_eq!(
        picking.editor.workspace.masks.object.as_ref().unwrap().pins,
        Some((2, 1))
    );
    picking.key(Key::Named(Named::Enter));
    let (_, params) = picking.editor.object_selects.running.clone().unwrap();
    assert_eq!(params["candidate"], json!(2));
    assert_eq!(params["stroke"], json!(sent));

    // Escape: no pins up now, so it leaves picking.
    picking.key(Key::Named(Named::Escape));
    assert!(picking.editor.object_pick.is_none());
    assert!(picking.editor.mask_panel.object.is_none());
}

/// A model component's row says what picked it, how and with what; a removal under it stales it
/// with the reason in plain words; Keep hides the note and sends nothing; Recompute selects its
/// stored prompt again and replaces it with one `mask.set-model` entry.
#[test]
#[slow]
fn mask_object_a_model_row_goes_stale_and_recompute_and_keep_answer_it() {
    let mut picking = Picking::opened();
    picking.install();
    picking.choose(KindMenu::New);
    picking.pick_at(LEFT);
    picking.key(Key::Named(Named::Escape));
    assert!(picking.editor.object_pick.is_none());
    let row = picking.editor.workspace.masks.components[0].clone();
    let model = row.model.clone().expect("a model row");
    assert_eq!(model.class, "Object");
    assert_eq!(model.picked, Some("picked from a point"));
    assert_eq!(model.part, "Object finder, version 1");
    assert!(model.computed.is_some());
    assert_eq!(model.stale, None);

    // An agent removes something under it: the repair prefix moves.
    let revision = picking.editor.document.state.as_ref().unwrap().revision;
    call(
        &picking.owner(),
        picking.agent,
        "edit.add-correction",
        json!({"asset_id": picking.asset, "points": [[0.3, 0.3]], "radius": 0.02,
               "mutation": {"expected_revision": revision, "request_id": "stale", "actor": "agent"}}),
    )
    .unwrap();
    picking.refresh();
    let model = picking.editor.workspace.masks.components[0]
        .model
        .clone()
        .unwrap();
    assert_eq!(
        model.stale.as_deref(),
        Some("Needs recompute \u{b7} an object was removed after this was picked")
    );

    // Keep: nothing sent, the note hidden until the reasons change.
    picking.editor.mask_panel.last_request = None;
    picking.mask(MaskMessage::Keep(row.id.as_str().to_owned()));
    assert!(picking.editor.mask_panel.last_request.is_none());
    assert!(picking.editor.object_selects.idle());
    let kept = picking.editor.workspace.masks.components[0]
        .model
        .clone()
        .unwrap();
    assert_eq!(kept.stale, None);
    assert_eq!(kept.reasons, model.reasons);

    // Recompute: the stored prompt selected again, then one mask.set-model.
    let before = picking.labels().len();
    picking.mask(MaskMessage::Recompute(row.id.as_str().to_owned()));
    let (purpose, params) = picking.editor.object_selects.running.clone().unwrap();
    assert!(matches!(purpose, SelectFor::Recompute { .. }));
    assert_eq!(params["point"], json!([LEFT]));
    picking.select();
    let (method, params) = picking.commit();
    assert_eq!(method, "mask.set-model");
    assert_eq!(params["component"], json!(row.id));
    assert_eq!(picking.labels().len(), before + 1);
    let model = picking.editor.workspace.masks.components[0]
        .model
        .clone()
        .unwrap();
    assert_eq!(model.stale, None);
    assert!(
        model.reasons.is_empty(),
        "recomputed against the photograph now"
    );
}

/// A model component's menu copies its pick, select and commit exactly as Recompute sends them.
#[test]
#[slow]
fn mask_object_a_model_component_copies_its_pick_select_and_commit() {
    let mut picking = Picking::opened();
    picking.install();
    picking.choose(KindMenu::New);
    picking.pick_at(LEFT);
    let component = picking.listing().masks[0].components[0].clone();
    let id = component.id.as_str();
    let prompt = shared::click_prompt(LEFT);
    assert_eq!(
        picking
            .editor
            .object_request(id, ObjectRequest::Pick)
            .unwrap(),
        json!({"method": shared::PICK, "params": shared::point_query(&picking.asset, LEFT)})
    );
    let select = picking
        .editor
        .object_request(id, ObjectRequest::Select)
        .unwrap();
    assert_eq!(select["method"], shared::SELECT);
    assert_eq!(
        identified(select["params"].clone()),
        identified(shared::select_request(
            &picking.asset,
            &prompt,
            json!(request())
        ))
    );
    let commit = picking
        .editor
        .object_request(id, ObjectRequest::Commit)
        .unwrap();
    assert_eq!(commit["method"], "mask.set-model");
    assert_eq!(commit["params"]["artifact"], component.payload["artifact"]);
    assert_eq!(commit["params"]["prompt"], json!({"point": [LEFT]}));
    // An independent client may send the copied commit as it stands.
    call(
        &picking.owner(),
        picking.agent,
        "mask.set-model",
        commit["params"].clone(),
    )
    .unwrap();
}

/// Turning the experiment off while picking leaves the sub-state as Escape does, and a model row
/// offers no Recompute.
#[test]
#[slow]
fn mask_object_turning_the_experiment_off_leaves_picking() {
    let mut picking = Picking::opened();
    picking.install();
    picking.choose(KindMenu::New);
    picking.pick_at(LEFT);
    picking.api("flags.set", json!({"flag": "ai.analysis", "value": false}));
    testing::read_flags(&mut picking.editor);
    assert!(picking.editor.object_pick.is_none());
    assert!(picking.editor.mask_panel.object.is_none());
    let model = picking.editor.workspace.masks.components[0]
        .model
        .clone()
        .unwrap();
    assert!(model.recompute_reason.is_some());
}

/// A hover is asked once: the same position on the same revision sends nothing again, so its
/// answer does not ask again, while a new position does.
#[test]
#[slow]
fn mask_object_a_hover_is_asked_once_per_position_and_revision() {
    let mut picking = Picking::opened();
    picking.install();
    picking.choose(KindMenu::New);
    picking.hover(LEFT);
    let session = picking.editor.object_pick.as_ref().unwrap();
    assert_eq!((session.asking, session.answered), (None, 1));
    picking.object(ObjectPickMessage::Hover(Some(LEFT)));
    assert_eq!(picking.editor.object_pick.as_ref().unwrap().asking, None);
    picking.object(ObjectPickMessage::Hover(Some(RIGHT)));
    assert_eq!(
        picking.editor.object_pick.as_ref().unwrap().asking,
        Some(RIGHT)
    );
}
