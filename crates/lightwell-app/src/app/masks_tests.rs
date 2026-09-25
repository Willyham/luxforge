//! The Mask mode, the Masks panel and the gradient handle editor against a real owner and catalog.
//!
//! Every request these gestures build is compared with the one an independent JSON client sends for
//! the same edit, and every refusal the command family makes is asserted where the panel surfaces
//! it. Tasks are run here as the plain functions they wrap, so the answers reach the editor as the
//! same messages the runtime delivers. A gesture's `draft.set` is no task: the editor sends it and
//! takes its answer up inside the update that produced the geometry.
use super::{
    Boot, Editor,
    draft::Round,
    message::{DraftMessage, MaskMessage, MaskPointer, MenuTarget, Message, PaintTarget, RowEdit},
    tasks::{self, call},
    testing,
};
use crate::{
    Config,
    app::testing::descriptors,
    mask_draft::{BRUSH, LINEAR, MaskDraft, MaskHandle, RADIAL},
};
use iced::keyboard::{Key, Modifiers};
use lightwell_core::{
    AssetId, ClientId, ComponentMode, MASK_MODE, MaskOverlayMode, OwnerHandle, POINTER_MODE,
    mask::commands::MaskListing,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "lightwell-masks-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// An editor with every built-in discovered, one real photograph open, plus a second client of the
/// same owner standing in for an independent JSON client.
struct Masking {
    editor: Editor,
    catalog: PathBuf,
    asset: AssetId,
    agent: ClientId,
}

impl Masking {
    fn opened() -> Self {
        let catalog = scratch("catalog.sqlite");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let agent = owner.register();
        let (queued, _) = call(
            &owner,
            agent,
            "catalog.import",
            json!({"path": fixture("fixtures/s0/orientation-1.jpg"), "mutation": crate::app::tasks::request()}),
        )
        .unwrap();
        let job_id = queued["job_id"].as_str().expect("a source job").to_owned();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (status, _) = call(&owner, agent, "job.status", json!({"job_id": job_id})).unwrap();
            match status["status"].as_str() {
                Some("ready") => break,
                Some("queued" | "running") => {
                    assert!(Instant::now() < deadline, "source preparation: {status}");
                    std::thread::sleep(Duration::from_millis(1));
                }
                other => panic!("source preparation failed {other:?}: {status}"),
            }
        }
        let (adopted, _) = call(&owner, agent, "job.adopt", json!({"job_id": job_id})).unwrap();
        let asset =
            AssetId::parse(adopted["asset"]["asset"]["id"].as_str().expect("an asset")).unwrap();
        let (mut editor, _) = Editor::new(Boot {
            owner: owner.clone(),
            join,
            live_server: None,
            config: Config::default(),
            client: None,
            initial_import: None,
            window: (1440.0, 900.0),
        });
        let _ = editor.update(Message::ModulesLoaded(Ok(descriptors())));
        let mut masking = Self {
            editor,
            catalog,
            asset,
            agent,
        };
        masking.refresh();
        assert!(masking.editor.editable(), "{}", masking.editor.status);
        masking
    }

    fn owner(&self) -> OwnerHandle {
        self.editor.owner.clone()
    }

    /// Read the asset back into the editor, as a command's completion does.
    fn refresh(&mut self) {
        let refreshed = tasks::refresh(
            &self.owner(),
            self.editor.client,
            self.asset.clone(),
            tasks::Scope::Open,
            None,
        )
        .unwrap();
        let _ = self
            .editor
            .update(Message::Refreshed(Ok(Box::new(refreshed))));
    }

    /// Enter Mask mode the way the mode strip, the letter and the palette all do. The message runs
    /// through the update function; its `workspace.set` task is then run here as the plain call it
    /// wraps, exactly as the runtime's executor would.
    fn enter_mask_mode(&mut self) {
        self.set_mode(MASK_MODE);
        assert!(self.editor.mask_mode_active(), "{}", self.editor.status);
    }

    fn set_mode(&mut self, mode: &str) {
        let _ = self.editor.update(Message::SetMode(mode.to_owned()));
        // A refused mode change sends no request, so the session is only asked when one was sent.
        if self.editor.status.starts_with("Apply or Cancel")
            || self.editor.status.starts_with("Finish or discard")
        {
            return;
        }
        let _ = call(
            &self.owner(),
            self.editor.client,
            "workspace.set",
            json!({ "mode": mode }),
        );
        self.adopt_session();
    }

    /// The owner's session, as the `workspace.set` round trip returns it.
    fn adopt_session(&mut self) {
        let (session, _) = call(
            &self.owner(),
            self.editor.client,
            "session.state",
            json!({}),
        )
        .unwrap();
        let session = serde_json::from_value(session).unwrap();
        let _ = self.editor.update(Message::WorkspaceUpdated(Ok(session)));
    }

    fn message(&mut self, message: MaskMessage) {
        let _ = self.editor.update(Message::Mask(message));
    }

    /// One key press through the **single keymap table**, so what a test presses is what a keyboard
    /// presses and nothing here invents a shortcut of its own.
    fn key(&mut self, letter: &str, modifiers: Modifiers) {
        let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: Key::Character(letter.into()),
            modified_key: Key::Character(letter.into()),
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
            let _ = self.editor.update(message);
        }
    }

    /// The modifiers changing, through the same table. The erase modifier is a hold, so this is how
    /// it goes down and how it comes up.
    fn modifiers(&mut self, modifiers: Modifiers) {
        let event = iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers));
        if let Some(message) = super::keymap::keymap(
            &event,
            iced::event::Status::Ignored,
            &self.editor.key_context(),
        ) {
            let _ = self.editor.update(message);
        }
    }

    /// Every entry's label, oldest first, without the import's own.
    fn labels(&self) -> Vec<String> {
        let (listed, _) = call(
            &self.owner(),
            self.editor.client,
            "history.list",
            json!({"asset_id": self.asset, "limit": 100}),
        )
        .expect("history.list answers");
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

    /// One step back or forward through history, as the editor's own command does.
    fn walk(&mut self, method: &str) {
        let revision = self.editor.state.as_ref().unwrap().revision;
        call(
            &self.owner(),
            self.editor.client,
            method,
            json!({"asset_id": self.asset, "mutation": tasks::mutation(revision)}),
        )
        .unwrap_or_else(|error| panic!("{method} was refused: {error}"));
        self.refresh();
    }

    fn undo(&mut self) {
        self.walk("history.undo");
    }

    fn redo(&mut self) {
        self.walk("history.redo");
    }

    /// One panel gesture that sends a `mask.*` command, with the request it built run against the
    /// owner as the runtime's task would run it. The request replayed here is the panel's own,
    /// recorded as it was sent, so this proves the panel's request and nothing reconstructed.
    fn run(&mut self, message: MaskMessage) -> Value {
        self.editor.last_mask_request = None;
        self.message(message);
        let (method, params) = self
            .editor
            .last_mask_request
            .clone()
            .expect("the gesture sent a mask command");
        let (result, _) = call(&self.owner(), self.editor.client, &method, params)
            .unwrap_or_else(|error| panic!("{method} was refused: {error}"));
        self.editor.busy = false;
        self.refresh();
        result
    }

    /// One decision about the open gesture's draft, as Apply, Cancel, Escape and the notice send it.
    fn draft(&mut self, message: DraftMessage) {
        let _ = self.editor.update(Message::Draft(message));
    }

    /// Run the open gesture's `draft.begin` as the runtime's task does. Its answer sends the first
    /// `draft.set` itself, synchronously, in the same update.
    fn open_gesture(&mut self) {
        assert!(self.editor.mask_shape().is_some(), "a gesture is open");
        assert_eq!(
            testing::run_round(&mut self.editor),
            Some(Round::Begin),
            "the gesture was waiting for its draft.begin"
        );
        // `render.transform` answers the affine the handles are mapped through.
        let (transform, _) = call(
            &self.owner(),
            self.editor.client,
            "render.transform",
            json!({"asset_id": self.asset}),
        )
        .unwrap();
        let gesture = testing::gesture_of(&self.editor);
        let _ = self.editor.update(Message::MaskTransform(
            gesture,
            Ok(serde_json::from_value(transform).unwrap()),
        ));
        self.assert_geometry_sent();
    }

    /// The gesture's newest geometry is already in its core draft.
    ///
    /// A mask gesture's `draft.set` runs synchronously on the desktop thread, in the update that
    /// produced the geometry, so nothing is left in flight or queued behind it and the draft the
    /// session reports holds every field the gesture would commit. There is no answer for a test to
    /// deliver: asserting that the update already took it up is the whole of what a runtime task
    /// used to be simulated for.
    fn assert_geometry_sent(&self) {
        let Some(gesture) = self.editor.core_gesture() else {
            return;
        };
        assert!(
            gesture.draft.drained(),
            "a draft.set is still in flight or queued after the update that sent it"
        );
        let Some(draft) = self.editor.mask_shape() else {
            return;
        };
        // An armed brush has painted nothing, so there is nothing to send and nothing was.
        if draft.brush().is_some_and(|stroke| !stroke.drawn()) {
            return;
        }
        let held = self
            .editor
            .session
            .draft
            .as_ref()
            .expect("the core draft the gesture set");
        for (name, value) in draft.fields() {
            assert_eq!(
                held.fields.get(&name),
                Some(&value),
                "the core draft holds the gesture's {name}"
            );
        }
    }

    /// Commit the open gesture, as Apply does.
    fn apply(&mut self) {
        self.draft(DraftMessage::Commit);
        self.commit_open_draft();
    }

    /// Paint one whole stroke: a press, a move per position and the release that commits it.
    ///
    /// The release is what commits, because one stroke is one draft and therefore one history entry;
    /// nothing presses Apply. The gesture re-arms itself on the component the stroke landed on, and
    /// the caller opens that core draft when it wants to paint again.
    fn paint(&mut self, points: &[(f64, f64)]) {
        let (x, y) = points[0];
        self.message(MaskMessage::Handle(MaskPointer::PaintBegin { x, y }));
        self.assert_geometry_sent();
        for &(x, y) in &points[1..] {
            self.message(MaskMessage::Handle(MaskPointer::PaintTo { x, y }));
            self.assert_geometry_sent();
        }
        self.message(MaskMessage::Handle(MaskPointer::PaintEnd));
        self.commit_open_draft();
    }

    /// Answer the `draft.commit` the open gesture asked for, as its task does.
    ///
    /// The desktop reads the answer back, and a mask command answers with what it changed beside
    /// the mutation envelope. Reading it as the bare envelope would refuse the extra fields by name
    /// and turn every committed gesture into a failure, so the task's own reading is what runs here
    /// and a committed entry is what it must find.
    fn commit_open_draft(&mut self) {
        let gesture = self.editor.core_gesture().expect("a gesture is open");
        assert_eq!(
            gesture.draft.in_flight(),
            Some(Round::Commit),
            "the gesture's commit is in flight"
        );
        let draft_id = gesture
            .draft
            .draft_id
            .clone()
            .expect("the core draft is open");
        let revision = gesture.draft.base_revision;
        let refreshed = tasks::draft_commit_now(
            &self.owner(),
            self.editor.client,
            &draft_id,
            self.asset.clone(),
            tasks::mutation(revision),
            None,
        )
        .unwrap_or_else(|error| panic!("the gesture commits: {error}"))
        .expect("the gesture commits an entry, not a no-op");
        testing::answer_commit(&mut self.editor, Ok(Some(refreshed)));
    }

    /// One whole shape drawn in a stroke, as a press and a drag on the photograph do.
    fn sweep(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.message(MaskMessage::Handle(MaskPointer::Sweep { from, to }));
        self.assert_geometry_sent();
        self.message(MaskMessage::Handle(MaskPointer::End));
        self.assert_geometry_sent();
    }

    /// Draw one whole gradient and commit it, which is what New mask does end to end.
    fn draw_mask(&mut self) {
        self.message(MaskMessage::New(LINEAR.to_owned()));
        self.open_gesture();
        self.sweep((0.5, 0.2), (0.5, 0.8));
        self.apply();
    }

    /// Add one component of that kind and mode to the open mask, through the Add row and the canvas
    /// gesture that follows it.
    fn add_component(&mut self, kind: &str, mode: ComponentMode) {
        self.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
            mode,
        )));
        self.message(MaskMessage::Add(kind.to_owned()));
        self.open_gesture();
        self.sweep((0.25, 0.3), (0.7, 0.65));
        self.apply();
        // The Add row is left where every other gesture leaves it, so a later New mask is not
        // refused by a mode this helper chose.
        self.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
            ComponentMode::Add,
        )));
    }

    /// The request one row edit would send, built through the panel's own builder — which is the
    /// same builder its Copy as JSON request reads.
    fn request_for(&mut self, edit: &RowEdit) -> Value {
        let (_, target, fields) = self
            .editor
            .row_command(edit)
            .expect("the row names a declared command");
        identified(
            self.editor
                .mask_request(&target, &fields)
                .expect("a photograph is open"),
        )
    }

    /// The request the panel last sent, with its deduplication id replaced by a marker.
    fn sent(&self) -> Option<Value> {
        self.editor
            .last_mask_request
            .as_ref()
            .map(|(_, request)| identified(request.clone()))
    }

    /// Every declared field the panel shows for the open gesture is the value the draft holds, to
    /// the bit. That is what "the handles match their number fields" means.
    fn assert_fields_match_the_draft(&self, what: &str) {
        let draft = self.editor.mask_shape().expect("a gesture is open");
        let shown = self
            .editor
            .workspace
            .masks
            .draft
            .as_ref()
            .expect("the panel shows the gesture");
        assert_eq!(shown.fields.len(), draft.values().len(), "{what}");
        for ((name, value), field) in draft.values().into_iter().zip(shown.fields.iter()) {
            assert_eq!(field.name, name, "{what}");
            assert_eq!(field.value, value, "{what}: {name}");
        }
    }

    /// One more component on that mask, posted the way an independent client posts one. Used where
    /// a test needs a long list rather than a drawn one.
    fn add_component_through_the_api(&mut self, mask: &lightwell_core::MaskId) {
        let revision = self.editor.state.as_ref().unwrap().revision;
        call(
            &self.owner(),
            self.editor.client,
            "mask.add-linear",
            json!({"asset_id": self.asset, "mutation": tasks::mutation(revision), "mask": mask,
                   "mode": "add", "x0": 0.1, "y0": 0.1, "x1": 0.9, "y1": 0.9}),
        )
        .expect("the component is added");
        self.refresh();
    }

    /// One more mask, posted the same way.
    fn create_mask_through_the_api(&mut self) {
        let revision = self.editor.state.as_ref().unwrap().revision;
        call(
            &self.owner(),
            self.editor.client,
            "mask.create-linear",
            json!({"asset_id": self.asset, "mutation": tasks::mutation(revision),
                   "x0": 0.1, "y0": 0.1, "x1": 0.9, "y1": 0.9}),
        )
        .expect("the mask is created");
        self.refresh();
    }

    /// The masks the owner holds, read the way an independent client reads them.
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
}

impl Drop for Masking {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.catalog);
    }
}

/// Mask is a canvas-takeover mode in the strip, entered by the strip, by `M` and by the palette;
/// leaving it with an open gesture is refused with a reason and discards nothing.
#[test]
fn mask_is_a_canvas_mode_and_leaving_it_with_an_open_gesture_is_refused() {
    let mut masking = Masking::opened();
    // The strip offers it whatever modules are registered, because no module declares it.
    let strip = &masking.editor.workspace.canvas.modes;
    let entry = strip
        .iter()
        .find(|mode| mode.id == MASK_MODE)
        .expect("the strip offers Mask");
    assert_eq!(entry.label, "Mask");
    assert_eq!(entry.shortcut.as_deref(), Some("M"));

    masking.enter_mask_mode();
    assert!(masking.editor.workspace.canvas.masking);
    // The tools panel shows the Masks panel in place of the module list.
    assert_eq!(
        masking.editor.workspace.masks.caption.as_deref(),
        Some("No masks yet · New mask draws one on the photograph")
    );
    // New mask names the kinds it can create, from the host's own table.
    let kinds: Vec<&str> = masking
        .editor
        .workspace
        .masks
        .kinds
        .iter()
        .map(|kind| kind.kind.as_str())
        .collect();
    assert!(kinds.contains(&"linear"), "{kinds:?}");
    assert_eq!(
        kinds,
        lightwell_core::mask::declared_geometry_kinds().collect::<Vec<_>>(),
        "the kinds are the host's, not a list of the panel's own"
    );
    // And they are the kinds that can be created, not every kind the build can evaluate. The brush
    // is parsed, evaluated and retained but its geometry is drawn, so it declares no parameters and
    // generates no `mask.create-brush`; offering it here would be a button with no command behind
    // it. Strokes reach a mask through `mask.add-stroke`, not through this row.
    for kind in &kinds {
        assert!(
            !lightwell_core::mask::component_geometry_is_drawn(kind),
            "the Add row offers {kind}, whose geometry is drawn and has no create command"
        );
        // And the command is really there, for each of the two routes a row can take: a kind with
        // handles is drawn and a kind whose geometry is entirely defaulted is typed, and either way
        // the button sends a generated method rather than nothing.
        for op in [
            lightwell_core::mask::commands::GeometryOp::Create,
            lightwell_core::mask::commands::GeometryOp::Add,
        ] {
            assert!(
                lightwell_core::mask::commands::geometry(op, kind).is_some(),
                "the Add row offers {kind} with no generated {op:?} command behind it"
            );
        }
    }
    // Every registered kind is reachable: the four on this row, and the brush through
    // `mask.add-stroke`. A kind that were neither would be a kind a person could never make.
    let panel: Vec<&str> = masking
        .editor
        .workspace
        .masks
        .kinds
        .iter()
        .filter(|kind| kind.drawable || kind.typed)
        .map(|kind| kind.kind.as_str())
        .collect();
    assert_eq!(panel, kinds, "every offered kind is drawn or typed");
    for kind in lightwell_core::mask::component_kinds() {
        assert!(
            kinds.contains(&kind) || kind == lightwell_core::mask::BRUSH,
            "{kind} is registered but reachable from nowhere"
        );
    }
    assert!(
        lightwell_core::mask::component_kinds()
            .any(|kind| { lightwell_core::mask::component_geometry_is_drawn(kind) }),
        "a drawn kind is registered, or this test proves nothing"
    );

    // With a gesture open, every route out of the mode is refused and says why.
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.set_mode(POINTER_MODE);
    assert!(
        masking.editor.status.starts_with("Apply or Cancel"),
        "{}",
        masking.editor.status
    );
    assert!(
        masking.editor.mask_shape().is_some(),
        "the gesture was discarded by a refused mode change"
    );
    // Compare during a gesture is refused for the same reason.
    let _ = masking.editor.update(Message::CompareBegin);
    assert!(
        masking.editor.status.contains("mask gesture"),
        "{}",
        masking.editor.status
    );
    assert!(masking.editor.mask_shape().is_some());
    // Cancel ends it, and then the mode can be left.
    masking.draft(DraftMessage::Cancel);
    assert!(masking.editor.mask_shape().is_none());
    masking.set_mode(POINTER_MODE);
    assert!(!masking.editor.mask_mode_active());
}

/// A gradient drags as one draft and commits once, and its exact values are editable as numbers.
#[test]
fn a_gradient_drags_as_one_draft_commits_once_and_is_editable_as_numbers() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    let before = masking.editor.history.entries.len();

    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    // One press, several moves and a release: the whole drag is one draft, so nothing is committed
    // until Apply and no entry is written per move.
    masking.message(MaskMessage::Handle(MaskPointer::Begin {
        handle: MaskHandle::End,
        x: 0.5,
        y: 0.75,
    }));
    for y in [0.6, 0.7, 0.9] {
        masking.message(MaskMessage::Handle(MaskPointer::Drag { x: 0.5, y }));
        masking.assert_geometry_sent();
    }
    masking.message(MaskMessage::Handle(MaskPointer::End));
    masking.assert_geometry_sent();
    assert_eq!(
        masking.editor.history.entries.len(),
        before,
        "a drag committed something before Apply"
    );
    // The number fields show the exact values the drag produced, to the declared precision.
    let readout = masking
        .editor
        .workspace
        .masks
        .draft
        .as_ref()
        .expect("the draft bar reads out the gesture")
        .readout
        .clone();
    assert_eq!(readout.len(), 4, "{readout:?}");
    assert_eq!(readout[3].0, "y1");
    assert!(readout[3].1.starts_with("0.9"), "{readout:?}");

    masking.apply();
    assert_eq!(
        masking.editor.history.entries.len(),
        before + 1,
        "the whole gesture is exactly one history entry"
    );
    let listing = masking.listing();
    assert_eq!(listing.masks.len(), 1);
    let mask = &listing.masks[0];
    assert_eq!(mask.components.len(), 1);
    assert_eq!(mask.components[0].kind, "linear");
    assert_eq!(mask.components[0].mode, ComponentMode::Add);
    assert_eq!(mask.components[0].payload["y1"], json!(0.9));
    // The gesture that created the mask opens it, so the adjustments are already bound to it.
    assert_eq!(masking.editor.selected_mask.as_ref(), Some(&mask.id));

    // A typed field is the same edit as a drag: the draft accepts it and refuses what the declared
    // range refuses, so no gesture is reachable only by pointer.
    masking.message(MaskMessage::EditShape(
        mask.components[0].id.as_str().to_owned(),
    ));
    masking.open_gesture();
    masking.message(MaskMessage::Field {
        name: "y1".into(),
        value: 0.5,
    });
    masking.assert_geometry_sent();
    assert_eq!(
        masking
            .editor
            .mask_shape()
            .expect("the gesture is open")
            .linear()
            .expect("a gradient")
            .y1,
        0.5
    );
    masking.message(MaskMessage::Field {
        name: "y1".into(),
        value: 99.0,
    });
    assert_eq!(
        masking
            .editor
            .mask_shape()
            .unwrap()
            .linear()
            .expect("a gradient")
            .y1,
        0.5,
        "a value outside the declared range changes nothing"
    );
    masking.apply();
    assert_eq!(
        masking.listing().masks[0].components[0].payload["y1"],
        json!(0.5)
    );
}

/// Selecting a mask shows its component list and, beneath it, the generated sections of the
/// maskable modules bound to that mask; a Copy as JSON request from one of those controls includes
/// the mask and is exactly the request that was sent.
#[test]
fn a_masked_control_sends_and_copies_the_request_an_independent_client_sends() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.clone();

    // The panel shows the mask's one component, and only the maskable modules' sections below it.
    let panel = &masking.editor.workspace.masks;
    assert_eq!(panel.selected.as_ref(), Some(&mask));
    assert_eq!(panel.components.len(), 1);
    let sections: Vec<&str> = masking
        .editor
        .workspace
        .tools
        .sections
        .iter()
        .map(|section| section.module_id.as_str())
        .collect();
    assert!(sections.contains(&"lightwell.basic"), "{sections:?}");
    assert!(
        !sections.contains(&"lightwell.crop"),
        "a module with no maskable effect has nothing to offer a mask: {sections:?}"
    );

    // The request one generated Basic control sends carries the host's own `mask` field.
    let copied = masking
        .editor
        .request_for_preset("set-basic", Some("exposure"), None)
        .expect("a request");
    assert_eq!(copied["method"], json!("edit.set-basic"));
    assert_eq!(copied["params"]["mask"], json!(mask));

    // Run it, and compare the stack with the one an independent client's identical request makes.
    masking
        .editor
        .set_control_field_value("set-basic", "exposure", &json!(0.4));
    let _ = masking.editor.update(Message::RunAction {
        action: "set-basic".into(),
        preset: serde_json::Map::new(),
    });
    let revision = masking.editor.state.as_ref().unwrap().revision;
    let (result, _) = call(
        &masking.owner(),
        masking.editor.client,
        "edit.set-basic",
        json!({"asset_id": masking.asset, "mutation": tasks::mutation(revision), "mask": mask, "exposure": 0.4}),
    )
    .unwrap();
    assert_ne!(result["outcome"], json!("no-op"), "{result}");
    masking.refresh();

    // The masked Basic layer exists, is bound to that mask, and the global layer was not touched.
    let listing = masking.listing();
    assert_eq!(
        listing.masks[0].layers.len(),
        1,
        "the mask holds exactly the Basic layer it was edited through"
    );
    let described = masking.editor.recipe.as_ref().expect("a described recipe");
    let masked: Vec<_> = described
        .layers
        .iter()
        .filter(|layer| layer.mask.as_ref() == Some(&mask))
        .collect();
    assert_eq!(masked.len(), 1, "{described:?}");
    assert_eq!(masked[0].values["exposure"], json!(0.4));
    assert!(
        described
            .layers
            .iter()
            .all(|layer| layer.mask.is_some() || layer.module.as_deref() != Some("lightwell.basic")),
        "the global Basic layer was created by a masked edit"
    );

    // The recipe panel groups the masked layer under its mask's name while keeping the durable
    // processing order visible: the rows stay in order and each masked row names its mask.
    let rows = &masking.editor.workspace.panel.recipe;
    let row = rows
        .iter()
        .find(|row| row.mask.is_some())
        .expect("a masked row");
    let group = row.mask.as_ref().unwrap();
    assert_eq!(group.id, mask);
    assert_eq!(group.name, listing.masks[0].name);
    assert_eq!(group.index, Some(0));
    assert!(group.heading, "the first row of a mask carries its heading");
    assert_eq!(
        rows.iter().map(|row| &row.layer_id).collect::<Vec<_>>(),
        described
            .layers
            .iter()
            .map(|layer| &layer.id)
            .collect::<Vec<_>>(),
        "the rows are the recipe's own durable order"
    );

    // Leaving Mask mode binds the sections back to the global layer, so a field never shows a
    // value the control in front of it would not edit.
    masking.set_mode(POINTER_MODE);
    assert!(masking.editor.section_target().is_none());
    let global = masking
        .editor
        .request_for_preset("set-basic", Some("exposure"), None)
        .expect("a request");
    assert!(
        global["params"].get("mask").is_none(),
        "a global edit carries no target: {global}"
    );
}

/// A masked slider drafts through the mask, which is what makes it follow the drag the way a
/// global one does, and it commits exactly one entry.
#[test]
fn a_masked_slider_drafts_through_its_mask_and_commits_one_entry() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.clone();
    let before = masking.editor.history.entries.len();

    // The first move opens the draft. Its target is the open mask, so the previewed stack is the
    // masked layer the release will commit rather than the global one.
    let _ = masking.editor.update(Message::SliderMoved {
        action: "set-basic".into(),
        parameter: "exposure".into(),
        value: 0.3,
    });
    let target = masking.editor.draft_target("set-basic");
    assert_eq!(target.mask.as_ref(), Some(&mask));
    assert!(
        target.component.is_none(),
        "a module edits through the whole mask"
    );
    assert_eq!(testing::run_round(&mut masking.editor), Some(Round::Begin));
    let draft = masking.editor.session.draft.clone().unwrap_or_else(|| {
        panic!(
            "a maskable action drafts through a mask: {}",
            masking.editor.status
        )
    });
    assert_eq!(
        draft
            .target
            .as_ref()
            .and_then(|target| target.mask.as_ref()),
        Some(&mask)
    );
    assert_eq!(
        draft.fields.get("exposure"),
        Some(&json!(0.3)),
        "the drafted preview is the masked stack at the dragged value"
    );

    // Committing it writes exactly one masked layer.
    let _ = masking.editor.update(Message::SliderReleased {
        action: "set-basic".into(),
        parameter: "exposure".into(),
    });
    assert_eq!(testing::run_round(&mut masking.editor), Some(Round::Commit));
    assert!(
        masking.editor.gesture.is_none(),
        "{}",
        masking.editor.status
    );
    assert_eq!(
        masking.editor.history.entries.len(),
        before + 1,
        "a masked slider gesture is one entry"
    );
    let described = masking.editor.recipe.as_ref().expect("a recipe");
    let masked: Vec<_> = described
        .layers
        .iter()
        .filter(|layer| layer.mask.as_ref() == Some(&mask))
        .collect();
    assert_eq!(masked.len(), 1);
    assert_eq!(masked[0].values["exposure"], json!(0.3));
}

/// The panel surfaces the refusals the command family already makes rather than offering buttons
/// that would be refused.
#[test]
fn the_panel_shows_the_familys_refusals_instead_of_offering_them() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].clone();

    // A mask's only component cannot be deleted; the panel offers Delete mask in its place.
    let panel = &masking.editor.workspace.masks;
    let only = &panel.components[0];
    let reason = only
        .delete_reason
        .as_ref()
        .expect("the only component names why it cannot be deleted");
    assert!(reason.contains("only component"), "{reason}");
    assert!(reason.contains("delete the mask instead"), "{reason}");
    // And the host agrees, which is why the panel does not offer it.
    let revision = masking.editor.state.as_ref().unwrap().revision;
    let refused = call(
        &masking.owner(),
        masking.editor.client,
        "mask.delete-component",
        json!({"asset_id": masking.asset, "mutation": tasks::mutation(revision), "mask": mask.id, "component": only.id}),
    )
    .expect_err("the host refuses it too");
    assert!(refused.contains("delete the mask"), "{refused}");

    // A mask's first component is always add, so its mode is not offered.
    assert!(
        only.mode_options.is_empty(),
        "the first component has no mode control"
    );
    let mode_reason = only
        .mode_reason
        .as_ref()
        .expect("the first component names why its mode is fixed");
    assert!(mode_reason.contains("always add"), "{mode_reason}");

    // Add a subtract component; moving it to the front would leave a non-add leading, so the panel
    // refuses the move with the family's own reason.
    masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
        ComponentMode::Subtract,
    )));
    masking.message(MaskMessage::Add(LINEAR.to_owned()));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::Sweep {
        from: (0.2, 0.5),
        to: (0.8, 0.5),
    }));
    masking.assert_geometry_sent();
    masking.message(MaskMessage::Handle(MaskPointer::End));
    masking.assert_geometry_sent();
    masking.apply();
    let panel = &masking.editor.workspace.masks;
    assert_eq!(panel.components.len(), 2);
    assert_eq!(panel.components[1].mode, ComponentMode::Subtract.as_str());
    let up = panel.components[1]
        .up_reason
        .as_ref()
        .expect("moving a subtract component to the front is refused");
    assert!(up.contains("always add"), "{up}");
    assert!(!panel.components[1].can_move_up());
    // The first component may not move down for the same reason, and both may now be deleted.
    assert!(panel.components[0].down_reason.is_some());
    assert!(panel.components[0].delete_reason.is_none());
    assert!(panel.components[1].delete_reason.is_none());
}

/// The overlay is per-client view state: Shift+M toggles it, `O` keeps meaning thirds, and the
/// grid the canvas draws is asked for beside the frame rather than by a second render.
#[test]
fn shift_m_toggles_the_overlay_and_o_still_means_thirds() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    assert_eq!(
        masking.editor.session.workspace.mask_overlay,
        MaskOverlayMode::Off
    );

    masking.message(MaskMessage::ToggleOverlay);
    assert_eq!(
        masking.editor.session.workspace.mask_overlay,
        MaskOverlayMode::Tint
    );
    // With the overlay on and a mask open, a preview job asks for that mask's coverage grid.
    let request = masking
        .editor
        .mask_overlay_request()
        .expect("the overlay names the mask whose grid it wants");
    assert_eq!(Some(&request.mask), masking.editor.selected_mask.as_ref());
    assert!(request.cells_w > 0 && request.cells_h > 0);

    // The eye hides one mask's overlay without changing what it does to the picture.
    let mask = masking.listing().masks[0].id.clone();
    masking.message(MaskMessage::ToggleVisible(mask.as_str().to_owned()));
    assert!(masking.editor.mask_overlay_request().is_none());
    assert_eq!(
        masking.listing().masks[0].components.len(),
        1,
        "hiding an overlay changed the recipe"
    );
    masking.message(MaskMessage::ToggleVisible(mask.as_str().to_owned()));
    assert!(masking.editor.mask_overlay_request().is_some());

    masking.message(MaskMessage::ToggleOverlay);
    assert_eq!(
        masking.editor.session.workspace.mask_overlay,
        MaskOverlayMode::Off
    );
    assert!(
        masking.editor.mask_overlay_request().is_none(),
        "an overlay that is off asks for no grid at all"
    );

    // `O` still means thirds, in Mask mode as everywhere else.
    let thirds = masking.editor.session.workspace.thirds;
    let _ = masking.editor.update(Message::ToggleThirds);
    let _ = call(
        &masking.owner(),
        masking.editor.client,
        "workspace.set",
        json!({ "thirds": !thirds }),
    );
    masking.adopt_session();
    assert_eq!(
        masking.editor.session.workspace.thirds, !thirds,
        "O still means thirds in Mask mode"
    );
    assert!(
        masking.editor.mask_mode_active(),
        "toggling thirds did not leave Mask mode"
    );
}

/// Every generated mask control's message produces the request an independent JSON client sends,
/// and a drag re-derives only the section that changed.
#[test]
fn a_mask_controls_request_matches_json_and_a_drag_rederives_one_section() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.clone();

    // The whole-mask amount is a generated control of `mask.set-amount`; the request it copies is
    // the method with the identity in its envelope, exactly as a JSON client sends it.
    masking
        .editor
        .set_control_field_value("mask.set-amount", "amount", &json!(60.0));
    let copied = masking
        .editor
        .request_for_preset("mask.set-amount", Some("amount"), None)
        .expect("a request");
    assert_eq!(copied["method"], json!("mask.set-amount"));
    assert_eq!(copied["params"]["mask"], json!(mask));
    assert_eq!(copied["params"]["amount"], json!(60.0));
    assert!(
        copied["params"].get("component").is_none(),
        "a whole-mask command takes no component: {copied}"
    );

    // Sending exactly that request through the API changes the stack the same way.
    let revision = masking.editor.state.as_ref().unwrap().revision;
    let mut params = copied["params"].clone();
    params["mutation"] = json!(tasks::mutation(revision));
    let (result, _) = call(
        &masking.owner(),
        masking.agent,
        copied["method"].as_str().unwrap(),
        params,
    )
    .unwrap_or_else(|error| panic!("the copied request is a valid one: {error}"));
    assert_ne!(result["outcome"], json!("no-op"), "{result}");
    masking.refresh();
    assert_eq!(masking.listing().masks[0].amount, 60.0);

    // A component's own geometry field copies its kind's patch method with both identities.
    let component = masking.listing().masks[0].components[0].id.clone();
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));
    let copied = masking
        .editor
        .request_for_preset("mask.set-linear", Some("x0"), None)
        .expect("a request");
    assert_eq!(copied["method"], json!("mask.set-linear"));
    assert_eq!(copied["params"]["mask"], json!(mask));
    assert_eq!(copied["params"]["component"], json!(component));

    // A drag re-derives only the section that changed: every other section keeps its version.
    let versions = |editor: &Editor| -> Vec<(String, u64)> {
        editor
            .workspace
            .tools
            .all()
            .map(|section| (section.module_id.clone(), section.version))
            .collect()
    };
    let before = versions(&masking.editor);
    let _ = masking.editor.update(Message::SliderMoved {
        action: "set-basic".into(),
        parameter: "exposure".into(),
        value: 0.2,
    });
    let after = versions(&masking.editor);
    assert_eq!(before.len(), after.len());
    let moved: Vec<&str> = before
        .iter()
        .zip(after.iter())
        .filter(|(before, after)| before.1 != after.1)
        .map(|(before, _)| before.0.as_str())
        .collect();
    assert_eq!(
        moved,
        vec!["lightwell.basic"],
        "a drag re-derived more than the section it changed"
    );
}

/// A generated `mask.*` control copies **the request it sends**, byte for byte.
///
/// The row controls already prove this, because one function builds both. A generated control reaches
/// the two through different functions — `request_for_preset` for the copy and `run_mask_action` for
/// the send — which share `mask_request`, `action_params` and `draft_target` but are distinct paths.
/// An argument that two paths agree is not the claim the panel makes; this is. It is checked for the
/// whole-mask amount and for a component's own geometry field, because the second carries one more
/// identity than the first.
#[test]
fn a_generated_mask_control_copies_the_request_it_sends() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let component = masking.listing().masks[0].components[0].id.clone();
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));

    for (action, parameter, value) in [
        ("mask.set-amount", "amount", json!(60.0)),
        ("mask.set-linear", "x0", json!(0.4)),
    ] {
        masking
            .editor
            .set_control_field_value(action, parameter, &value);
        let copied = masking
            .editor
            .request_for_preset(action, Some(parameter), None)
            .unwrap_or_else(|| panic!("{action} built no request"));
        assert_eq!(copied["method"], json!(action));

        // The preset is derived exactly as the field's own submit derives it, so the two paths are
        // given the same input and any difference in the request is theirs.
        let preset = super::submit_preset(
            &masking.editor.modules,
            action,
            Some(parameter),
            &masking.editor.fields,
        )
        .unwrap_or_else(|error| panic!("{action} refused its own field: {error}"));
        masking.editor.last_mask_request = None;
        let _ = masking.editor.update(Message::RunAction {
            action: action.to_owned(),
            preset,
        });
        let (method, sent) = masking
            .editor
            .last_mask_request
            .clone()
            .unwrap_or_else(|| panic!("{action} sent nothing"));
        assert_eq!(method, action, "the sent method is not the copied one");
        assert_eq!(
            identified(sent),
            identified(copied["params"].clone()),
            "the copied {action} request is not the request that was sent"
        );
        // The send is answered so the next iteration is not refused as busy.
        masking.editor.busy = false;
        masking.refresh();
    }
}

/// A mask's row menu carries the lifecycle the design names, and each item is one host command.
#[test]
fn the_row_menu_duplicates_inverts_and_deletes_through_the_host() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.clone();

    // The menu opens on the row and is per-client state.
    let _ = masking.editor.update(Message::OpenMenu(MenuTarget::Mask(
        mask.as_str().to_owned(),
    )));
    assert_eq!(
        masking.editor.menu,
        Some(MenuTarget::Mask(mask.as_str().to_owned()))
    );

    // Invert is one command, and the listing shows it afterwards.
    let result = masking.run(MaskMessage::Row(RowEdit::InvertMask {
        mask: mask.as_str().to_owned(),
        invert: true,
    }));
    assert_eq!(result["label"], json!("Inverted"), "{result}");
    assert!(masking.listing().masks[0].invert);

    // Duplicate copies the mask and its layers, so the list grows by one.
    masking.run(MaskMessage::Row(RowEdit::DuplicateMask(
        mask.as_str().to_owned(),
    )));
    assert_eq!(masking.listing().masks.len(), 2);

    // Delete removes it and names the layers it removed.
    let result = masking.run(MaskMessage::Row(RowEdit::DeleteMask(
        mask.as_str().to_owned(),
    )));
    assert!(
        result["label"]
            .as_str()
            .is_some_and(|label| label.contains("Delete")),
        "{result}"
    );
    let listing = masking.listing();
    assert_eq!(listing.masks.len(), 1);
    assert_ne!(listing.masks[0].id, mask);
    // The selection followed the stack rather than pointing at a mask that is gone.
    assert!(
        masking
            .editor
            .workspace
            .masks
            .selected
            .as_ref()
            .is_none_or(|selected| selected != &mask)
    );
}

/// The component list is the recorded improvement over Lightroom, so this is the part that has to
/// be right: each row carries its **own** mode, inversion, order and delete, every one of them is a
/// declared command, and the request a row sends is the request its Copy as JSON request produces.
#[test]
fn each_component_row_carries_its_own_mode_invert_order_and_delete() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.add_component(LINEAR, ComponentMode::Subtract);
    masking.add_component(RADIAL, ComponentMode::Subtract);
    let listed = masking.listing().masks[0].clone();
    assert_eq!(listed.components.len(), 3);
    let second = listed.components[1].id.as_str().to_owned();
    let third = listed.components[2].id.as_str().to_owned();

    // Every row names its kind and shows its own mode, and the options come from the host's own
    // declaration rather than a vocabulary the panel made up.
    let panel = &masking.editor.workspace.masks;
    let declared = lightwell_core::mask::commands::find("mask.set-component-mode")
        .and_then(|command| command.action.parameter("mode"))
        .map(|declared| match &declared.kind {
            lightwell_core::ParameterKind::Enum { options } => options.clone(),
            other => panic!("mode is declared as {other:?}"),
        })
        .expect("the host declares the mode parameter");
    assert_eq!(panel.components[1].mode_options, declared);
    assert_eq!(panel.components[2].mode_options, declared);
    assert_eq!(panel.components[2].kind_title, "Radial");
    assert_eq!(
        panel.components[1].mode_options[panel.components[1].mode_selected],
        ComponentMode::Subtract.as_str()
    );

    // Select the first component, then change the *second* row's mode. The row edits its own
    // component: a list whose controls all addressed the selection would be no list at all.
    masking.message(MaskMessage::SelectComponent(
        listed.components[0].id.as_str().to_owned(),
    ));
    let edit = RowEdit::ComponentMode {
        component: second.clone(),
        mode: ComponentMode::Intersect.as_str().to_owned(),
    };
    // What Copy as JSON request would copy, built before the gesture runs, from the same builder.
    let copied = masking.request_for(&edit);
    let result = masking.run(MaskMessage::Row(edit));
    assert_eq!(
        masking.sent(),
        Some(copied),
        "the copied request is not the request that was sent"
    );
    assert!(
        result["label"]
            .as_str()
            .is_some_and(|label| label.contains("intersect")),
        "{result}"
    );
    let listed = masking.listing().masks[0].clone();
    assert_eq!(listed.components[1].mode, ComponentMode::Intersect);
    assert_eq!(
        listed.components[2].mode,
        ComponentMode::Subtract,
        "one row's mode control changed another row's component"
    );
    // The selection survives a row edit: the canvas keeps editing what it was editing.
    assert_eq!(
        masking
            .editor
            .selected_component
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        Some(listed.components[0].id.as_str().to_owned())
    );

    // Inverting is the row's own too, and the request matches the builder the copy reads.
    let edit = RowEdit::ComponentInvert {
        component: third.clone(),
        invert: true,
    };
    let copied = masking.request_for(&edit);
    masking.run(MaskMessage::Row(edit));
    assert_eq!(masking.sent(), Some(copied));
    let listed = masking.listing().masks[0].clone();
    assert!(listed.components[2].invert);
    assert!(!listed.components[1].invert);

    // And so is the order. Moving the third row up swaps it with the second, and the selection is
    // still the first component.
    let edit = RowEdit::MoveComponent {
        component: third.clone(),
        index: 1,
    };
    let copied = masking.request_for(&edit);
    masking.run(MaskMessage::Row(edit));
    assert_eq!(masking.sent(), Some(copied));
    let listed = masking.listing().masks[0].clone();
    assert_eq!(listed.components[1].id.as_str(), third);
    assert_eq!(listed.components[2].id.as_str(), second);
    assert_eq!(
        masking
            .editor
            .selected_component
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        Some(listed.components[0].id.as_str().to_owned())
    );

    // Delete is a row command like the rest, and the panel offers it because the host would accept
    // it: two components remain afterwards.
    let edit = RowEdit::DeleteComponent(second.clone());
    let copied = masking.request_for(&edit);
    masking.run(MaskMessage::Row(edit));
    assert_eq!(masking.sent(), Some(copied));
    assert_eq!(masking.listing().masks[0].components.len(), 2);

    // Every method a row sends is one the host declares, and every one of them mutates.
    for method in [
        "mask.set-component-mode",
        "mask.set-component-invert",
        "mask.reorder-component",
        "mask.delete-component",
        "mask.delete",
        "mask.duplicate",
        "mask.set-invert",
        "mask.reorder",
    ] {
        let command = lightwell_core::mask::commands::find(method)
            .unwrap_or_else(|| panic!("{method} is declared"));
        assert!(command.mutates, "{method}");
    }
}

/// Hovering a component row shows that component's own contribution in the overlay, and leaving the
/// row restores the composed mask. It is view state: no selection changes and nothing commits.
#[test]
fn hovering_a_row_shows_that_components_contribution_and_leaving_restores_the_mask() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.add_component(LINEAR, ComponentMode::Subtract);
    masking.message(MaskMessage::ToggleOverlay);
    let listed = masking.listing().masks[0].clone();
    let second = listed.components[1].id.clone();

    // With nothing hovered or selected, the overlay asks for the whole composed mask.
    let request = masking
        .editor
        .mask_overlay_request()
        .expect("the overlay is on and a mask is open");
    assert_eq!(request.mask, listed.id);
    assert_eq!(
        request.component, None,
        "the composed mask, not a component"
    );

    // Hovering the second row asks for that component's own grid instead.
    masking.message(MaskMessage::Hover(Some(second.as_str().to_owned())));
    let request = masking.editor.mask_overlay_request().expect("an overlay");
    assert_eq!(request.component.as_ref(), Some(&second));
    assert!(
        masking.editor.workspace.masks.components[1].hovered,
        "the row says the overlay is showing it"
    );
    // Nothing was selected and nothing was committed by pointing at a row.
    assert_eq!(masking.editor.selected_component, None);
    assert_eq!(masking.editor.last_mask_request, None);

    // Leaving the row restores the composed overlay.
    masking.message(MaskMessage::Hover(None));
    let request = masking.editor.mask_overlay_request().expect("an overlay");
    assert_eq!(request.component, None);
    assert!(!masking.editor.workspace.masks.components[1].hovered);

    // The overlay follows the pointer and nothing else. A selected component opens that row's own
    // numbers; it does not pin the overlay to that component, or leaving the list would never show
    // the composition again — which is the comparison the component list exists to make.
    let first = listed.components[0].id.clone();
    masking.message(MaskMessage::SelectComponent(first.as_str().to_owned()));
    assert_eq!(
        masking.editor.mask_overlay_request().unwrap().component,
        None
    );
    masking.message(MaskMessage::Hover(Some(second.as_str().to_owned())));
    assert_eq!(
        masking
            .editor
            .mask_overlay_request()
            .unwrap()
            .component
            .as_ref(),
        Some(&second),
        "the pointer wins over the selection"
    );
    masking.message(MaskMessage::Hover(None));
    assert_eq!(
        masking.editor.mask_overlay_request().unwrap().component,
        None
    );
    assert_eq!(
        masking.editor.selected_component.as_ref(),
        Some(&first),
        "pointing at a row never changes what is selected"
    );
}

/// A **typed** kind is created by its button and not by a gesture: no draft opens, the request is
/// the generated `mask.create-<kind>` with no geometry in it, and the component that lands carries
/// the payload the host's own declarations describe.
///
/// This is what makes a range selection reachable at all. It has nothing to drag — its geometry is
/// a band on the histogram's axis and a list of sampled colours — so routing it through the handle
/// gesture would open a draft with no shape and no preview. The panel names neither kind: it reads
/// that every declared field carries a default and creates it directly, so a kind registered later
/// with defaulted geometry arrives the same way.
#[test]
fn a_typed_kind_is_created_by_its_button_with_no_gesture() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();

    let created = masking.run(MaskMessage::New("luminance-range".to_owned()));
    assert_eq!(created["label"], json!("Add luminance range"));
    assert!(
        masking.editor.mask_shape().is_none(),
        "a typed kind opens no gesture"
    );
    // The request carried the envelope and nothing else: the four numbers came from the host's own
    // declarations, read once, rather than from a copy of them in the panel.
    let sent = masking.sent().expect("the panel sent a request");
    let keys: Vec<&String> = sent.as_object().expect("an object").keys().collect();
    assert_eq!(keys, ["asset_id", "mutation"], "{sent}");

    let listing = masking.listing();
    let component = &listing.masks[0].components[0];
    assert_eq!(component.kind, "luminance-range");
    assert!(component.available);
    // The whole tonal range with soft shoulders: a new band selects the picture and is narrowed,
    // the way a crop starts at the whole frame.
    assert_eq!(component.payload["low"], json!(0.0));
    assert_eq!(component.payload["high"], json!(100.0));
    assert_eq!(component.payload["low_feather"], json!(5.0));
    assert_eq!(component.payload["high_feather"], json!(5.0));

    // The other range kind added to the same mask, with its mode chosen up front exactly as a drawn
    // kind's is.
    masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
        ComponentMode::Intersect,
    )));
    let added = masking.run(MaskMessage::Add("colour-range".to_owned()));
    assert_eq!(added["label"], json!("Add intersect colour range"));
    assert!(masking.editor.mask_shape().is_none());
    let listing = masking.listing();
    let component = &listing.masks[0].components[1];
    assert_eq!(component.kind, "colour-range");
    assert_eq!(component.mode, ComponentMode::Intersect);
    assert_eq!(component.payload["refine"], json!(50.0));
    // An unsampled colour range holds no swatches and selects nothing until one is picked.
    assert_eq!(component.payload["samples"], json!([]));
}

/// **A create opens what it made**, for a typed kind exactly as for a drawn one.
///
/// This is not a nicety. The generated sections under the component list are bound to the *open* mask,
/// so a New mask button that leaves the previous mask open puts the next slider on a mask the person
/// was not looking at — and with a range selection that is especially easy to miss, because creating
/// one changes no pixel until it is narrowed. A drafted create already opens its own mask on commit;
/// a typed kind is created by its button and never reaches that path, so the rule is applied where
/// every `mask.*` answer arrives instead.
///
/// The converse is checked too: a command that adds a *component* gains no mask and must leave the
/// open one alone.
#[test]
fn a_create_opens_the_mask_it_made_and_nothing_else_moves_the_selection() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let first = masking.listing().masks[0].id.clone();
    assert_eq!(masking.editor.selected_mask.as_ref(), Some(&first));

    // A typed kind's own button, with another mask already open.
    masking.run(MaskMessage::New("luminance-range".to_owned()));
    let listing = masking.listing();
    assert_eq!(listing.masks.len(), 2, "the button made a second mask");
    let second = listing.masks[1].id.clone();
    assert_eq!(
        masking.editor.selected_mask.as_ref(),
        Some(&second),
        "a create opens the mask it made, so the adjustments are bound to it"
    );
    assert_eq!(
        masking.editor.workspace.masks.name, listing.masks[1].name,
        "the rename field follows the mask that is now open"
    );
    assert!(
        masking.editor.selected_component.is_none(),
        "a newly opened mask has no row selected"
    );

    // Adding a component gains no mask, so the open one stays open.
    masking.run(MaskMessage::Add("colour-range".to_owned()));
    assert_eq!(
        masking.editor.selected_mask.as_ref(),
        Some(&second),
        "adding a component to the open mask does not move the selection"
    );
    assert_eq!(masking.listing().masks.len(), 2);
}

/// The Add row offers each kind with its mode chosen up front, and the gesture that follows creates
/// exactly that component — not one whose role was guessed from a modifier key afterwards.
#[test]
fn the_add_row_chooses_the_mode_before_the_gesture() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();

    // Every registered kind is offered, and both of this build's kinds are drawable.
    let panel = &masking.editor.workspace.masks;
    let kinds: Vec<String> = panel.kinds.iter().map(|kind| kind.kind.clone()).collect();
    assert!(
        kinds.contains(&LINEAR.to_owned()) && kinds.contains(&RADIAL.to_owned()),
        "{kinds:?}"
    );
    // Each kind reaches the panel one of the two ways the host's own declarations allow: a
    // gradient is drawn with handles, a range selection is typed and created from its defaults.
    for kind in &panel.kinds {
        assert!(
            kind.drawable || kind.typed,
            "{} is neither drawn nor typed, so its button would do nothing",
            kind.kind
        );
    }
    let typed: Vec<&str> = panel
        .kinds
        .iter()
        .filter(|kind| !kind.drawable && kind.typed)
        .map(|kind| kind.kind.as_str())
        .collect();
    assert_eq!(typed, ["luminance-range", "colour-range"]);

    for (mode, kind) in [
        (ComponentMode::Subtract, LINEAR),
        (ComponentMode::Intersect, RADIAL),
    ] {
        masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
            mode,
        )));
        assert_eq!(
            masking.editor.workspace.masks.modes[masking.editor.workspace.masks.add_mode],
            mode.as_str(),
            "the panel shows the mode the next gesture will use"
        );
        masking.message(MaskMessage::Add(kind.to_owned()));
        // The gesture already knows its mode: it is in the request the release will send.
        let fields = masking
            .editor
            .mask_shape()
            .expect("a gesture is open")
            .fields();
        assert_eq!(fields["mode"], json!(mode.as_str()));
        masking.open_gesture();
        masking.sweep((0.3, 0.3), (0.7, 0.7));
        masking.apply();
        let listed = masking.listing().masks[0].clone();
        let added = listed.components.last().expect("the component was added");
        assert_eq!(added.mode, mode, "the gesture created a {mode:?} component");
        assert_eq!(added.kind, kind);
    }
}

/// A radial drags as one draft, commits once, and its number fields agree with the drag at every
/// point of it — which is what makes the handles and the fields two views of one geometry.
#[test]
fn a_radial_drags_as_one_draft_commits_once_and_matches_its_number_fields() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    let before = masking.editor.history.entries.len();
    masking.message(MaskMessage::New(RADIAL.to_owned()));
    masking.open_gesture();
    // The gesture knows the content stage's aspect from `render.transform`, which is the only thing
    // it needs about the stage to place an ellipse.
    let aspect = masking.editor.mask_shape().expect("a gesture").aspect();
    assert!(aspect > 0.0 && aspect.is_finite(), "{aspect}");

    masking.sweep((0.5, 0.5), (0.75, 0.8));
    // Every handle is where the panel's own fields say it is, at every step of the drag.
    let handles: Vec<_> = masking.editor.mask_shape().expect("a gesture").handles();
    assert_eq!(
        handles.len(),
        7,
        "four radii, a centre, a rotation grip and the ring"
    );
    for (handle, _) in &handles {
        let from = masking
            .editor
            .mask_shape()
            .unwrap()
            .handles()
            .into_iter()
            .find(|(known, _)| known == handle)
            .map(|(_, point)| point)
            .expect("the handle is drawn");
        masking.message(MaskMessage::Handle(MaskPointer::Begin {
            handle: *handle,
            x: from.0,
            y: from.1,
        }));
        for step in [(0.03, 0.02), (-0.04, 0.05)] {
            masking.message(MaskMessage::Handle(MaskPointer::Drag {
                x: from.0 + step.0,
                y: from.1 + step.1,
            }));
            masking.assert_geometry_sent();
            masking.assert_fields_match_the_draft(&format!("{handle:?} {step:?}"));
        }
        masking.message(MaskMessage::Handle(MaskPointer::End));
        masking.assert_geometry_sent();
    }
    // One drag of seven handles is still one draft, and the commit is one entry.
    let drawn: Vec<(String, f64)> = masking
        .editor
        .mask_shape()
        .unwrap()
        .values()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    masking.apply();
    assert_eq!(
        masking.editor.history.entries.len(),
        before + 1,
        "a shape gesture is one history entry"
    );
    let listed = masking.listing().masks[0].clone();
    assert_eq!(listed.components.len(), 1);
    assert_eq!(listed.components[0].kind, RADIAL);
    // The committed payload is the geometry the fields showed, to the last bit.
    for (name, value) in drawn {
        assert_eq!(
            listed.components[0].payload[&name],
            json!(value),
            "{name} was committed as something other than what the field showed"
        );
    }

    // Selecting the row puts that component's stored geometry in its own number fields, so the
    // numbers under a row are the geometry its handles draw and not a declared minimum.
    masking.message(MaskMessage::SelectComponent(
        listed.components[0].id.as_str().to_owned(),
    ));
    let fields = &masking.editor.workspace.masks.components[0].fields;
    assert!(!fields.is_empty(), "the selected row shows its own fields");
    let patch = lightwell_core::mask::commands::find("mask.set-radial").expect("the patch method");
    for name in ["x", "y", "radius_x", "radius_y", "angle", "feather"] {
        let declared = patch.action.parameter(name).expect("a declared parameter");
        let stored = &listed.components[0].payload[name];
        let shown = masking
            .editor
            .fields
            .get("mask.set-radial", name)
            .unwrap_or_else(|| panic!("{name} is a field"));
        assert_eq!(
            shown,
            crate::app::fields::value_text(declared, stored).expect("the stored value"),
            "{name} reads {shown} and is stored as {stored}"
        );
    }

    // Reopening the component starts from exactly the stored payload rather than a reconstruction.
    masking.message(MaskMessage::EditShape(
        listed.components[0].id.as_str().to_owned(),
    ));
    masking.open_gesture();
    let reopened = masking.editor.mask_shape().expect("a gesture");
    for (name, value) in reopened.values() {
        assert_eq!(listed.components[0].payload[name], json!(value), "{name}");
    }
    masking.draft(DraftMessage::Cancel);
}

/// The panel refuses rather than silently coercing, and says why each time: a first component that
/// is not an add, and a list that has reached its declared limit.
#[test]
fn the_panel_refuses_a_first_component_that_is_not_add_and_a_list_at_its_limit() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();

    // With the next component set to subtract, New mask would have to create an add: it is refused
    // with its reason rather than quietly creating one.
    masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
        ComponentMode::Subtract,
    )));
    let reason = masking
        .editor
        .workspace
        .masks
        .create_reason
        .clone()
        .expect("New mask names why it cannot run");
    assert!(
        reason.contains("always add") && reason.contains("subtract"),
        "{reason}"
    );
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    assert!(
        masking.editor.mask_shape().is_none(),
        "a gesture opened anyway"
    );
    assert_eq!(masking.editor.status, reason);
    // Setting it back to add clears the refusal, so nothing is permanently blocked.
    masking.message(MaskMessage::SetAddMode(crate::state::masks::mode_index(
        ComponentMode::Add,
    )));
    assert_eq!(masking.editor.workspace.masks.create_reason, None);

    // A mask at its component limit says so rather than offering an Add the host would refuse.
    let mask = masking.listing().masks[0].id.clone();
    while masking.listing().masks[0].components.len() < lightwell_core::COMPONENTS_PER_MASK {
        masking.add_component_through_the_api(&mask);
    }
    masking.refresh();
    let add_reason = masking
        .editor
        .workspace
        .masks
        .add_reason
        .clone()
        .expect("the Add row names why it cannot run");
    assert!(
        add_reason.contains(&lightwell_core::COMPONENTS_PER_MASK.to_string()),
        "{add_reason}"
    );
    // And the host refuses one more, with a reason of the same shape.
    let revision = masking.editor.state.as_ref().unwrap().revision;
    let refused = call(
        &masking.owner(),
        masking.editor.client,
        "mask.add-linear",
        json!({"asset_id": masking.asset, "mutation": tasks::mutation(revision), "mask": mask,
               "mode": "add", "x0": 0.1, "y0": 0.1, "x1": 0.9, "y1": 0.9}),
    )
    .expect_err("the host refuses a component past the limit");
    assert!(
        refused.contains(&lightwell_core::COMPONENTS_PER_MASK.to_string()),
        "{refused}"
    );

    // A recipe at its mask limit says so in the same place.
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    for _ in 0..lightwell_core::MASKS_PER_RECIPE {
        masking.create_mask_through_the_api();
    }
    masking.refresh();
    let create_reason = masking
        .editor
        .workspace
        .masks
        .create_reason
        .clone()
        .expect("New mask names the limit");
    assert!(
        create_reason.contains(&lightwell_core::MASKS_PER_RECIPE.to_string()),
        "{create_reason}"
    );
}

/// The generated Amount slider shows the amount the open mask actually holds, at every value.
///
/// It is the host's own `mask.set-amount` control, read from the same field store a module's
/// controls read, so nothing seeds it unless the panel does: an unseeded number control falls back
/// to its declared minimum, which here is zero, and the panel would then read `0` beside a row
/// reading `100%` — one mask, two numbers, and the one the pointer can grab is the wrong one.
#[test]
fn the_amount_slider_reads_the_amount_the_mask_holds() {
    use crate::state::tools::ControlModel;

    let amount = |masking: &Masking| -> (f64, String, String) {
        let panel = &masking.editor.workspace.masks;
        let slider = panel
            .controls
            .iter()
            .find_map(|control| match control {
                ControlModel::Slider(slider) if slider.action == "mask.set-amount" => Some(slider),
                _ => None,
            })
            .expect("the panel generates the whole-mask Amount control");
        (
            slider.value,
            slider.display.clone(),
            panel.masks[0].amount.clone(),
        )
    };

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    assert_eq!(
        amount(&masking),
        (100.0, "100".to_owned(), "100%".to_owned()),
        "a freshly drawn mask is at full amount in both places"
    );

    // And after an amount this desktop did not choose: the control follows the stack, so a mask
    // another client turned down is read correctly here the moment the refresh lands.
    let mask = masking.listing().masks[0].id.clone();
    let revision = masking.editor.state.as_ref().unwrap().revision;
    call(
        &masking.owner(),
        masking.agent,
        "mask.set-amount",
        json!({"asset_id":masking.asset,"mutation":tasks::mutation(revision),
               "mask":mask,"amount":40.0}),
    )
    .expect("the amount is set");
    masking.refresh();
    assert_eq!(
        amount(&masking),
        (40.0, "40".to_owned(), "40%".to_owned()),
        "the slider follows the amount the stack holds"
    );
}

/// A `mask.*` command the host refuses ends the script step that sent it.
///
/// The panel states the rules it knows on the controls themselves rather than offering a button the
/// host would reject, so this is the one refusal that can only arrive from the host: an arbitrary
/// reorder index, which no button offers, that would leave a component that is not an `add` at the
/// front of the list. It changes nothing and renders nothing, so the step waiting for its pixels
/// would otherwise wait out the whole run's deadline on a request that was answered a round trip
/// ago.
#[test]
fn a_refused_mask_command_ends_the_step_that_sent_it() {
    use crate::app::{
        evidence::Settle,
        testing::{attach_script, evidence},
    };

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.add_component(LINEAR, ComponentMode::Subtract);
    let listed = masking.listing().masks[0].clone();
    assert_eq!(listed.components.len(), 2);
    assert_eq!(listed.components[1].mode, ComponentMode::Subtract);

    // The panel already refuses this move on the row itself, which is why a script has to ask for
    // it by position to reach the host's own refusal at all.
    assert!(
        masking.editor.workspace.masks.components[1]
            .up_reason
            .is_some(),
        "the row states the rule rather than offering the move"
    );

    attach_script(
        &mut masking.editor,
        r#"[{"mask":{"row":{"component":1,"index":0}}}]"#,
    );
    let _ = masking.editor.next_step();
    assert_eq!(
        evidence(&masking.editor).awaiting,
        Some(Settle::Preview),
        "the command went out and the step is waiting for its pixels"
    );
    assert!(!evidence(&masking.editor).capture_pending);

    // The host's answer, as the runtime delivers it.
    let (method, params) = masking
        .editor
        .last_mask_request
        .clone()
        .expect("the step sent the row's own command");
    let error = call(&masking.owner(), masking.editor.client, &method, params)
        .expect_err("the host refuses a move that would leave a subtract leading");
    let _ = masking
        .editor
        .update(Message::Refreshed(Err(error.to_string())));

    let run = evidence(&masking.editor);
    assert_eq!(run.awaiting, None, "the refusal ended the wait");
    assert!(
        run.capture_pending,
        "and the frame on screen is captured as the evidence of it"
    );
    assert!(run.had_errors, "the run records the refusal");
    let step = run.current.clone().expect("the step's own record");
    assert_eq!(step["status"], json!("failed"));
    assert!(
        step["reason"]
            .as_str()
            .is_some_and(|reason| reason.to_lowercase().contains("add")),
        "the refusal's own reason is what is recorded: {step}"
    );
    // Nothing moved.
    let after = masking.listing().masks[0].clone();
    assert_eq!(
        after
            .components
            .iter()
            .map(|component| component.id.clone())
            .collect::<Vec<_>>(),
        listed
            .components
            .iter()
            .map(|component| component.id.clone())
            .collect::<Vec<_>>(),
    );
}

/// A coverage grid the host refuses ends the step that was waiting for its texture, with the host's
/// own reason on it.
///
/// This is the refusal one step further out than
/// [`a_refused_mask_command_ends_the_step_that_sent_it`]: the request is accepted, the frame is
/// rendered, and it is the **grid** that is refused, on the worker, a round trip after the step
/// returned. The mask here is drawn and **no layer is bound to it**, and it holds a component whose
/// coverage depends on the pixel the masked operation receives — so there is no operation to read
/// that pixel from and no grid at all (proposal P16 of `docs/design/range-study.md`, decided and
/// built). A step that asked for the overlay would otherwise wait out the run's whole deadline for a
/// texture nothing will fill.
#[test]
fn a_refused_coverage_grid_ends_the_step_waiting_for_it() {
    use crate::app::{
        evidence::Settle,
        testing::{attach_script, evidence},
    };
    use lightwell_core::{MaskOverlayRequest, PreviewRequest};

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    let mask = masking.listing().masks[0].id.clone();

    // One luminance-range component, through the generated command a person's own button sends, is
    // what makes this mask read pixels.
    let revision = masking.editor.state.as_ref().unwrap().revision;
    call(
        &masking.owner(),
        masking.editor.client,
        "mask.add-luminance-range",
        json!({"asset_id": masking.asset, "mutation": tasks::mutation(revision), "mask": mask,
               "mode": "intersect", "low": 20.0, "low_feather": 5.0, "high": 80.0,
               "high_feather": 5.0}),
    )
    .expect("the range component is added");
    masking.refresh();
    masking.message(MaskMessage::ToggleOverlay);
    assert_eq!(
        masking.editor.session.workspace.mask_overlay,
        MaskOverlayMode::Tint
    );

    // A step that asks for the overlay waits for the grid's own texture, not for the frame.
    // The pointer off the list, so the overlay is asked for the **composition**: hovering one row
    // would ask for that component alone, and the gradient on its own has a perfectly good grid.
    attach_script(&mut masking.editor, r#"[{"mask":{"hover":null}}]"#);
    let _ = masking.editor.next_step();
    assert_eq!(
        evidence(&masking.editor).awaiting,
        Some(Settle::MaskOverlay),
        "the step is waiting for the coverage grid"
    );

    // The job the panel's own refresh would send, run through the editor's real queue and the real
    // worker, so what ends the step is the host's answer and not a message this test wrote.
    let request = masking
        .editor
        .mask_overlay_request()
        .expect("the overlay names the mask whose grid it wants");
    let job = masking
        .owner()
        .preview_job(
            PreviewRequest::new(masking.editor.client, masking.asset.clone()).mask_overlay(
                MaskOverlayRequest {
                    mask: mask.clone(),
                    component: None,
                    cells_w: request.cells_w,
                    cells_h: request.cells_h,
                },
            ),
        )
        .expect("a preview job");
    masking.editor.request_preview(job);
    let deadline = Instant::now() + Duration::from_secs(60);
    while evidence(&masking.editor).awaiting.is_some() {
        assert!(Instant::now() < deadline, "the frame never arrived");
        let _ = masking.editor.update(Message::Poll);
        std::thread::sleep(Duration::from_millis(1));
    }

    let run = evidence(&masking.editor);
    assert!(
        run.capture_pending,
        "the frame on screen is captured as the evidence of the refusal"
    );
    assert!(run.had_errors, "the run records the refusal");
    let step = run.current.clone().expect("the step's own record");
    assert_eq!(step["status"], json!("failed"));
    assert!(
        step["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("depends on the pixel it reads")
                && reason.contains("no layer is bound to mask")),
        "the host's own reason is what is recorded, and it says which half is missing: {step}"
    );
    assert!(
        masking.editor.mask_overlay_surface().is_none(),
        "nothing is drawn over the photograph for a mask that has no grid"
    );
}

/// One request with its deduplication id replaced by a marker.
///
/// Two sends of the same edit are two requests and must carry two ids, so the id is the one field a
/// copy cannot be expected to reproduce — and the one field that must be present in both.
fn identified(mut request: Value) -> Value {
    let mutation = request["mutation"]
        .as_object_mut()
        .expect("a mutation envelope");
    assert!(
        mutation
            .insert("request_id".into(), json!("<fresh>"))
            .is_some_and(|id| id.as_str().is_some_and(|id| !id.is_empty())),
        "a mutation carries its own request id"
    );
    request
}

/// The brush gesture end to end: one entry a stroke, the labels the granularity table states, the
/// brush's own keys, the held erase modifier, and a stroke deleted as a forward edit.
///
/// Every step here is a message the panel or the keymap sends, so the whole gesture is reachable
/// without a pointer, and the requests that reach the owner are the panel's own.
#[test]
fn painting_commits_one_entry_a_stroke_and_the_brush_keys_size_it() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();

    // The Add row offers only the kinds that declare their geometry as numbers, because that is what
    // generates a `mask.create-<kind>`. A brush declares none, so it is not there — and the panel
    // does not put up a button with no command behind it.
    assert!(
        !masking
            .editor
            .workspace
            .masks
            .kinds
            .iter()
            .any(|kind| kind.kind == BRUSH),
        "the Add row must not offer a brush: there is no mask.create-brush to run"
    );

    // The bracket keys move the brush by its command's own declared step, so a key and the panel's
    // nudge can never disagree. `[` and `]` size it; shifted, they feather it.
    let declared = |name: &str| {
        lightwell_core::mask::commands::find("mask.add-stroke")
            .and_then(|command| command.action.parameter(name))
            .and_then(|parameter| parameter.step)
            .expect("the command declares a step")
    };
    let before = masking.editor.brush;
    masking.key("]", Modifiers::default());
    assert_eq!(
        masking.editor.brush.size,
        before.size + declared("size"),
        "] grows the brush by its declared step"
    );
    masking.key("[", Modifiers::default());
    assert_eq!(masking.editor.brush.size, before.size, "[ shrinks it back");
    masking.key("]", Modifiers::SHIFT);
    assert_eq!(
        masking.editor.brush.feather,
        (before.feather + declared("feather")).min(100.0),
        "Shift+] feathers it"
    );
    masking.key("[", Modifiers::SHIFT);
    assert_eq!(masking.editor.brush.feather, before.feather);

    // The first stroke on nothing: a mask, a brush component and the stroke, as one entry.
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    assert_eq!(
        masking.editor.mask_shape().and_then(MaskDraft::method),
        Some("mask.add-stroke"),
        "every stroke commits through the one command that carries a path"
    );
    masking.paint(&[(0.3, 0.3), (0.45, 0.4), (0.6, 0.35)]);
    let listing = masking.listing();
    assert_eq!(listing.masks.len(), 1, "one mask");
    let component = &listing.masks[0].components[0];
    assert_eq!(component.kind, BRUSH);
    assert_eq!(component.name, "Brush 1");
    // No coordinate is ever written into a component payload: it holds addresses and nothing else.
    assert_eq!(
        component
            .payload
            .as_object()
            .expect("an object payload")
            .keys()
            .collect::<Vec<_>>(),
        ["strokes"],
        "a brush payload holds the reserved strokes field and nothing else"
    );

    // The gesture re-arms itself on the component that stroke landed on, so painting carries on
    // without a second gesture — and every later stroke is one more entry on that component.
    masking.open_gesture();
    assert_eq!(
        masking
            .editor
            .mask_shape()
            .and_then(|draft| draft.component.clone()),
        Some(component.id.clone()),
        "the brush is armed on the component the last stroke landed on"
    );
    masking.paint(&[(0.5, 0.6), (0.65, 0.65)]);

    // Option erases while it is held, and the flag is frozen for the stroke's whole life: letting
    // the key go halfway along a path must not turn an erase into an add.
    masking.open_gesture();
    masking.modifiers(Modifiers::ALT);
    assert!(masking.editor.painting_brush().erase, "Option erases");
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.4,
        y: 0.45,
    }));
    masking.assert_geometry_sent();
    masking.modifiers(Modifiers::default());
    assert!(
        masking
            .editor
            .mask_shape()
            .and_then(MaskDraft::brush)
            .is_some_and(|stroke| stroke.brush.erase),
        "a stroke already down keeps the flag it started with"
    );
    masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.5, y: 0.5 }));
    masking.assert_geometry_sent();
    masking.message(MaskMessage::Handle(MaskPointer::PaintEnd));
    masking.commit_open_draft();

    // One entry a stroke, named by the design's granularity table, with undo walking back one
    // stroke at a time and redo returning them in order.
    let labels = masking.labels();
    assert_eq!(
        labels,
        ["Add brush", "Update Brush 1", "Update Brush 1"],
        "one entry a stroke, named for what that stroke did"
    );
    let strokes = |masking: &Masking| {
        masking.listing().masks[0].components[0].payload["strokes"]
            .as_array()
            .expect("a stroke list")
            .len()
    };
    assert_eq!(strokes(&masking), 3);
    masking.undo();
    assert_eq!(strokes(&masking), 2, "undo walks back one stroke");
    masking.undo();
    assert_eq!(strokes(&masking), 1);
    masking.redo();
    masking.redo();
    assert_eq!(strokes(&masking), 3, "redo returns them in order");

    // A press that painted no position is not an edit: it commits nothing and writes no entry.
    masking.open_gesture();
    let before = masking.labels().len();
    masking.message(MaskMessage::Handle(MaskPointer::PaintEnd));
    assert_eq!(
        masking.labels().len(),
        before,
        "a stroke that committed nothing produces no entry"
    );

    // Escape cancels the armed gesture with nothing committed.
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.2,
        y: 0.2,
    }));
    masking.assert_geometry_sent();
    masking.draft(DraftMessage::Cancel);
    assert!(masking.editor.mask_shape().is_none(), "Escape ends it");
    assert_eq!(
        masking.labels().len(),
        before,
        "a cancelled stroke commits nothing"
    );
}

/// `mask.delete-stroke` is a forward edit and is presented as one: it appends an entry, removes only
/// the stroke it names, and leaves every entry after that stroke exactly where it is.
#[test]
fn deleting_a_stroke_is_a_forward_edit_the_panel_names_as_its_own() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.paint(&[(0.2, 0.2), (0.3, 0.3)]);
    for path in [[(0.4, 0.4), (0.5, 0.5)], [(0.6, 0.6), (0.7, 0.7)]] {
        masking.open_gesture();
        masking.paint(&path);
    }
    let component = masking.listing().masks[0].components[0].id.clone();
    let held: Vec<String> = masking.listing().masks[0].components[0].payload["strokes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|address| address.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(held.len(), 3);

    // The row lists its strokes while it is selected, each with a delete of its own, and the request
    // that delete sends is the one an independent client would send.
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));
    let rows = masking.editor.workspace.masks.components[0].strokes.clone();
    assert_eq!(
        rows.iter()
            .map(|row| row.stroke.clone())
            .collect::<Vec<_>>(),
        held,
        "the row lists the component's own strokes, in the order they compose"
    );
    let edit = RowEdit::DeleteStroke {
        component: component.as_str().to_owned(),
        stroke: held[1].clone(),
    };
    let request = masking.request_for(&edit);
    assert_eq!(request["stroke"], json!(held[1]));
    assert_eq!(request["component"], json!(component.as_str()));

    let before = masking.labels().len();
    masking.run(MaskMessage::Row(edit));
    masking.refresh();
    let after: Vec<String> = masking.listing().masks[0].components[0].payload["strokes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|address| address.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        after,
        [held[0].clone(), held[2].clone()],
        "only the named stroke goes and the rest keep their order"
    );
    let labels = masking.labels();
    assert_eq!(labels.len(), before + 1, "a delete appends an entry");
    assert_eq!(
        labels.last().map(String::as_str),
        Some("Delete a stroke from Brush 1"),
        "it is named as its own edit and not as an undo"
    );
    // A component's last stroke is not deletable: a component with no stroke covers nothing, so the
    // panel says so on the row rather than offering a delete the host would refuse.
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));
    masking.run(MaskMessage::Row(RowEdit::DeleteStroke {
        component: component.as_str().to_owned(),
        stroke: after[0].clone(),
    }));
    masking.refresh();
    masking.message(MaskMessage::SelectComponent(component.as_str().to_owned()));
    let rows = masking.editor.workspace.masks.components[0].strokes.clone();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0]
            .delete_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("only stroke")),
        "the row says why the last stroke cannot go: {rows:?}"
    );
}

/// A commit sends the **core** draft's fields, so every position the gesture has produced must be
/// in that draft before the commit goes out.
///
/// When the gesture's `draft.set` was a runtime task, a hand moving faster than its round trip left
/// positions queued, and committing there wrote the path as it was one step ago: a background
/// capture found a six-position stroke stored as one position. The set runs synchronously in the
/// update that produced each position, so a release finds nothing queued and can only commit the
/// whole path. That is pinned here rather than left to timing.
#[test]
fn a_release_commits_the_whole_path_the_pointer_drew() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    let brush = masking.editor.brush;

    // Every position reaches the core draft before the next one is handled, however fast they come:
    // nothing is ever in flight or queued between two messages.
    let drawn = [[0.3, 0.3], [0.4, 0.35], [0.5, 0.4], [0.6, 0.42]];
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: drawn[0][0],
        y: drawn[0][1],
    }));
    masking.assert_geometry_sent();
    for [x, y] in drawn[1..].iter().copied() {
        masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x, y }));
        masking.assert_geometry_sent();
    }

    // So the release commits at once, with no commit held back waiting for geometry.
    masking.message(MaskMessage::Handle(MaskPointer::PaintEnd));
    assert_eq!(
        masking
            .editor
            .core_gesture()
            .and_then(|gesture| gesture.draft.in_flight()),
        Some(Round::Commit),
        "nothing was left to send, so what is in flight is the commit"
    );
    masking.commit_open_draft();
    let held = masking.listing().masks[0].components[0].payload["strokes"]
        .as_array()
        .expect("a stroke list")
        .clone();
    assert_eq!(held.len(), 1, "one stroke");
    // A stroke is named by the hash of its contents, so asserting the address asserts the path.
    let expected =
        lightwell_core::path::Stroke::capture(&drawn, brush.size, brush.feather, brush.flow, false)
            .expect("a capturable stroke")
            .id();
    assert_eq!(
        held[0].as_str(),
        Some(expected.as_str()),
        "the committed stroke is the whole path the pointer drew"
    );
}

/// Discard ends a mask gesture on screen and in the owner, whatever is still on its way back.
///
/// The gesture's `draft.set` is synchronous, so no set is in flight when Discard runs, but the
/// drafted frames it queued can be, and so can a `draft.reapply`. Here the queue is still rendering
/// the drag when Discard comes, and a `draft.set` answer and a `draft.reapply` answer, both
/// produced by the owner before it, arrive after it. None of them may present a frame, and none may
/// leave a draft on the desktop or in the owner.
#[test]
fn an_answer_that_arrives_after_discard_presents_no_frame_and_leaves_no_draft() {
    use crate::app::testing::{attach_log, logged};

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    // The committed frame is on screen and nothing else is queued, so every frame the queue holds
    // from here on is one the gesture asked for.
    let deadline = Instant::now() + Duration::from_secs(60);
    while masking.editor.preview_queue.is_busy() {
        assert!(Instant::now() < deadline, "the opening frame never arrived");
        let _ = masking.editor.update(Message::Poll);
        std::thread::sleep(Duration::from_millis(1));
    }
    let presented = masking.editor.presented_generation;
    assert_eq!(masking.editor.displayed_draft_revision, None);
    let log = attach_log(&mut masking.editor);

    // A drag whose drafted frames are still being rendered: nothing polls the queue until Discard.
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::Sweep {
        from: (0.5, 0.2),
        to: (0.5, 0.8),
    }));
    masking.assert_geometry_sent();
    assert!(
        masking.editor.preview_queue.is_busy(),
        "the drag's drafted frames are queued"
    );
    let draft_id = masking
        .editor
        .core_gesture()
        .and_then(|gesture| gesture.draft.draft_id.clone())
        .expect("the core draft is open");

    // Two answers the owner produces before Discard and the desktop receives after it: a
    // `draft.set` with its drafted preview job, exactly as the gesture's own helper returns them,
    // and the `draft.reapply` of a Reapply pressed just before Discard.
    let fields = Value::Object(masking.editor.mask_shape().unwrap().fields());
    let late_set = tasks::draft_set_now(
        &masking.owner(),
        masking.editor.client,
        draft_id.clone(),
        masking.asset.clone(),
        fields,
        None,
    );
    assert!(late_set.is_ok(), "the owner accepts the geometry");
    masking.draft(DraftMessage::Reapply);
    assert_eq!(
        testing::core_draft(&masking.editor).and_then(|draft| draft.in_flight()),
        Some(Round::Reapply),
        "the reapply is in flight"
    );
    let rebased = tasks::draft_reapply_now(&masking.owner(), masking.editor.client, &draft_id);

    // Discard, as Escape and the Changed elsewhere notice both send it. The gesture leaves the
    // screen at once; its draft closes once the reapply in flight has answered, so no cancel races
    // it.
    masking.draft(DraftMessage::Cancel);
    assert!(
        masking.editor.mask_shape().is_none(),
        "Discard ends the gesture"
    );
    assert!(masking.editor.gesture_closing(), "its draft is closing");
    let asked = masking.editor.preview_generation;

    // Both late answers arrive.
    let _ = masking.editor.draft_set(late_set);
    let _ = masking
        .editor
        .update(Message::Draft(DraftMessage::Reapplied {
            gesture: testing::gesture_of(&masking.editor),
            draft: draft_id.clone(),
            result: rebased.map(Box::new),
        }));
    assert_eq!(
        masking.editor.preview_generation, asked,
        "a late answer asks for no frame"
    );
    assert!(
        masking.editor.session.draft.is_none(),
        "a late answer leaves no draft on the desktop"
    );
    assert_eq!(masking.editor.snapshot()["draft"], json!(null));
    assert_eq!(
        testing::core_draft(&masking.editor).and_then(|draft| draft.in_flight()),
        Some(Round::Cancel),
        "the reapply's answer is followed by the cancel, and nothing else"
    );

    // The drag's drafted jobs run to their end through the editor's real queue and worker, and not
    // one of their frames is presented.
    let deadline = Instant::now() + Duration::from_secs(60);
    while masking.editor.preview_queue.is_busy() {
        assert!(Instant::now() < deadline, "the drafted jobs never ended");
        let _ = masking.editor.update(Message::Poll);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        masking.editor.presented_generation, presented,
        "no frame was presented after Discard"
    );
    assert_eq!(masking.editor.displayed_draft_revision, None);

    // The cancel answers, with the committed frame read after it.
    assert_eq!(testing::run_round(&mut masking.editor), Some(Round::Cancel));
    assert!(masking.editor.gesture.is_none(), "the slot is free again");
    let (session, _) = call(
        &masking.owner(),
        masking.editor.client,
        "session.state",
        json!({}),
    )
    .unwrap();
    assert_eq!(session["draft"], json!(null), "nor in the owner");
    assert!(masking.editor.session.draft.is_none());

    let records = logged(&mut masking.editor, &log);
    let events: Vec<&str> = records
        .iter()
        .filter_map(|record| record["event"].as_str())
        .collect();
    let cancelled = events
        .iter()
        .position(|event| *event == "mask_draft_cancelled")
        .expect("Discard is recorded");
    assert!(
        !events[cancelled..].contains(&"mask_draft_preview"),
        "no drafted frame was queued after Discard: {events:?}"
    );
    assert!(
        !events[cancelled..].contains(&"mask_draft_set"),
        "and no geometry was sent to the discarded draft: {events:?}"
    );
    assert_eq!(
        events[cancelled..]
            .iter()
            .filter(|event| **event == "draft_set_dropped")
            .count(),
        1,
        "the late set answer is dropped, and says so: {events:?}"
    );
}

/// A slider gesture started with the brush in hand takes the draft the brush was holding.
///
/// At most one core draft exists per client, so the adjustments under the component list — which are
/// exactly where a hand goes after painting — could not reach one while the brush held it. Every
/// other gesture and every `mask.*` command already give an **armed** brush's draft up, because a
/// brush that has painted nothing has nothing to Apply and nothing to lose; this is that same rule on
/// the slider path, where its absence left the gesture with a `draft.begin` the host refused a round
/// trip later and a picture that never followed the drag.
#[test]
fn a_slider_gesture_takes_an_armed_brushs_draft() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();

    // One stroke painted, and the brush left armed on the component it landed on, which is where the
    // gesture leaves itself after every stroke.
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.paint(&[(0.3, 0.3), (0.5, 0.35)]);
    masking.open_gesture();
    assert!(masking.editor.armed_brush(), "the brush is armed");

    let _ =
        masking
            .editor
            .control_moved("set-presence".to_owned(), "dehaze".to_owned(), json!(30.0));
    assert!(
        masking.editor.slider_gesture().is_some(),
        "the slider gesture opened: {}",
        masking.editor.status
    );
    assert!(
        masking.editor.mask_shape().is_none(),
        "the armed brush gave up the draft it was holding"
    );
}

/// The other case: a gesture that has drawn something has an Apply to answer, so a slider started
/// over it is refused with that gesture's own reason rather than discarding a half-drawn gradient.
#[test]
fn a_slider_gesture_is_refused_while_a_drawn_mask_gesture_is_open() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.2, 0.2), (0.8, 0.8));
    let before = masking.editor.mask_shape().cloned();
    assert!(before.is_some() && !masking.editor.armed_brush());
    let _ =
        masking
            .editor
            .control_moved("set-presence".to_owned(), "dehaze".to_owned(), json!(20.0));
    assert!(
        masking
            .editor
            .status
            .contains("Apply or Cancel the mask gesture"),
        "{}",
        masking.editor.status
    );
    assert_eq!(
        masking.editor.mask_shape().cloned(),
        before,
        "the refused slider left the drawn gesture exactly as it was"
    );
}

// ---- The draft races the one driver closes. Each of these failed against the two drivers it
// replaced; the base versions are recorded with the change that introduced the driver.

fn event_names(records: &[Value]) -> Vec<String> {
    records
        .iter()
        .filter_map(|record| record["event"].as_str().map(str::to_owned))
        .collect()
}

/// Wait for the preview queue to finish what it holds, taking each result up as the runtime does.
fn drain_queue(masking: &mut Masking) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while masking.editor.preview_queue.is_busy() {
        assert!(Instant::now() < deadline, "the preview queue never drained");
        let _ = masking.editor.update(Message::Poll);
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// (a) Apply pressed before `draft.begin` has answered is not lost: the draft commits, with every
/// field the gesture drew, as soon as it exists.
#[test]
fn race_a_apply_before_the_draft_opens_still_commits() {
    use crate::app::testing::{attach_log, logged};
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    let log = attach_log(&mut masking.editor);
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.message(MaskMessage::Handle(MaskPointer::Sweep {
        from: (0.5, 0.2),
        to: (0.5, 0.8),
    }));
    masking.message(MaskMessage::Handle(MaskPointer::End));
    // Apply while `draft.begin` is still on its way.
    masking.draft(DraftMessage::Commit);
    assert_eq!(
        testing::core_draft(&masking.editor).and_then(|draft| draft.in_flight()),
        Some(Round::Begin)
    );
    assert_eq!(testing::run_round(&mut masking.editor), Some(Round::Begin));
    let events = event_names(&logged(&mut masking.editor, &log));
    let set = events.iter().position(|event| event == "mask_draft_set");
    let commit = events.iter().position(|event| event == "mask_draft_commit");
    assert!(
        set.is_some() && commit > set,
        "the geometry goes out first, then the commit: {events:?}"
    );
    masking.commit_open_draft();
    let listing = masking.listing();
    assert_eq!(listing.masks.len(), 1, "the Apply made its entry");
    assert_eq!(
        listing.masks[0].components[0].payload["y1"],
        json!(0.8),
        "with the geometry drawn before the draft opened"
    );
}

/// (b) A `draft.begin` answer that arrives after Discard is never adopted by a newer gesture: the
/// discarded gesture holds the slot until that answer names the draft to cancel, and an answer for
/// a gesture that is gone is dropped, its draft cancelled. The begin is in flight from the moment
/// the gesture opens.
#[test]
fn race_b_a_late_begin_answer_is_never_adopted_by_a_newer_gesture() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    let first = testing::gesture_of(&masking.editor);
    assert_eq!(
        testing::core_draft(&masking.editor).and_then(|draft| draft.in_flight()),
        Some(Round::Begin),
        "the opening gesture's draft.begin is in flight"
    );
    masking.draft(DraftMessage::Cancel);
    assert!(
        masking.editor.mask_shape().is_none(),
        "Discard ends it on screen"
    );

    // A newer gesture cannot take the slot while the discarded one's draft may still open.
    masking.message(MaskMessage::New(RADIAL.to_owned()));
    assert!(masking.editor.mask_shape().is_none());
    assert!(
        masking
            .editor
            .status
            .starts_with("Wait for the discarded draft to close"),
        "{}",
        masking.editor.status
    );

    // The first gesture's begin reaches the owner and answers: its draft is cancelled, not adopted.
    let (begun, _) = call(
        &masking.owner(),
        masking.editor.client,
        "draft.begin",
        tasks::draft_begin_params(
            masking.asset.clone(),
            "mask.create-linear",
            Default::default(),
        ),
    )
    .unwrap();
    let _ = masking.editor.update(Message::Draft(DraftMessage::Begun {
        gesture: first,
        result: Ok(Box::new(serde_json::from_value(begun).unwrap())),
    }));
    assert_eq!(testing::run_round(&mut masking.editor), Some(Round::Cancel));
    assert!(masking.editor.gesture.is_none(), "the slot is free again");

    // Now the newer gesture opens, and a stray answer naming the old one is dropped.
    masking.message(MaskMessage::New(RADIAL.to_owned()));
    assert!(
        masking.editor.mask_shape().is_some(),
        "{}",
        masking.editor.status
    );
    let stray = lightwell_core::Draft::new("mask.create-linear", masking.asset.clone(), 0);
    let _ = masking.editor.update(Message::Draft(DraftMessage::Begun {
        gesture: first,
        result: Ok(Box::new(stray.clone())),
    }));
    let newer = testing::core_draft(&masking.editor).expect("the newer gesture");
    assert_ne!(newer.gesture, first);
    assert_eq!(newer.draft_id, None, "the stray answer was not adopted");
    assert_eq!(
        newer.in_flight(),
        Some(Round::Begin),
        "its own begin is still awaited"
    );
    assert_eq!(testing::run_round(&mut masking.editor), Some(Round::Begin));
    assert!(
        testing::core_draft(&masking.editor)
            .and_then(|draft| draft.draft_id.as_ref())
            .is_some_and(|id| *id != stray.draft_id),
        "{}",
        masking.editor.status
    );
}

/// (c) Discard during an in-flight commit sends no racing `draft.cancel`: the commit decides, and
/// its entry is what the person sees, with no refusal in the status line.
#[test]
fn race_c_discard_during_a_commit_sends_no_racing_cancel() {
    use crate::app::testing::{attach_log, logged};
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    let log = attach_log(&mut masking.editor);
    masking.draft(DraftMessage::Commit);
    // Discard while the commit is on its way.
    masking.draft(DraftMessage::Cancel);
    assert!(
        masking.editor.mask_shape().is_some(),
        "the gesture waits for its commit to decide"
    );
    masking.commit_open_draft();
    assert!(masking.editor.gesture.is_none());
    let events = event_names(&logged(&mut masking.editor, &log));
    assert!(
        events.iter().any(|event| event == "mask_draft_commit")
            && !events.iter().any(|event| event == "mask_draft_cancelled"),
        "Discard sent a draft.cancel racing the commit: {events:?}"
    );
    assert_eq!(masking.editor.status, "Mask committed");
    assert_eq!(masking.listing().masks.len(), 1);
}

/// (d) Every answer names the gesture and the draft it belongs to, so a failed reapply answer for
/// a discarded gesture never clears a newer gesture's commit in flight.
#[test]
fn race_d_a_late_failed_reapply_leaves_a_newer_commit_in_flight() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    let first = testing::core_draft(&masking.editor)
        .cloned()
        .expect("a gesture");
    masking.editor.gesture_revision(first.base_revision + 1);
    masking.draft(DraftMessage::Reapply);
    masking.draft(DraftMessage::Cancel);
    // The reapply answers, then the cancel it was waiting for goes out and answers.
    assert_eq!(
        testing::run_round(&mut masking.editor),
        Some(Round::Reapply)
    );
    assert_eq!(testing::run_round(&mut masking.editor), Some(Round::Cancel));

    // A newer gesture opens, drafts and commits; its commit is in flight.
    masking.message(MaskMessage::New(RADIAL.to_owned()));
    masking.open_gesture();
    masking.sweep((0.3, 0.3), (0.6, 0.6));
    masking.draft(DraftMessage::Commit);
    assert_eq!(
        testing::core_draft(&masking.editor).and_then(|draft| draft.in_flight()),
        Some(Round::Commit),
        "the newer commit is in flight"
    );
    // A second, late answer for the discarded gesture's reapply arrives, refused.
    let _ = masking
        .editor
        .update(Message::Draft(DraftMessage::Reapplied {
            gesture: first.gesture,
            draft: first.draft_id.clone().expect("the first draft"),
            result: Err("not-found: no such draft".into()),
        }));
    assert_eq!(
        testing::core_draft(&masking.editor).and_then(|draft| draft.in_flight()),
        Some(Round::Commit),
        "the stale reapply answer left the newer gesture's commit in flight"
    );
    masking.commit_open_draft();
    assert_eq!(masking.listing().masks.len(), 1);
}

/// (e) A slider's Discard holds back the drafted frames still queued, exactly as a mask gesture's
/// does: one path for both.
#[test]
fn race_e_a_slider_discard_presents_no_queued_drafted_frame() {
    let mut masking = Masking::opened();
    drain_queue(&mut masking);
    let presented = masking.editor.presented_generation;
    let _ = masking.editor.update(Message::SliderMoved {
        action: "set-basic".into(),
        parameter: "exposure".into(),
        value: 0.3,
    });
    assert_eq!(testing::run_round(&mut masking.editor), Some(Round::Begin));
    assert!(
        masking.editor.preview_queue.is_busy(),
        "the drafted frame is queued"
    );
    masking.draft(DraftMessage::Cancel);
    drain_queue(&mut masking);
    assert_eq!(
        masking.editor.presented_generation, presented,
        "a drafted frame was presented after Discard"
    );
    // The committed frame the cancel reads back is the next one on screen.
    assert_eq!(testing::run_round(&mut masking.editor), Some(Round::Cancel));
    drain_queue(&mut masking);
    assert_eq!(masking.editor.displayed_draft_revision, None);
}

/// (f) The session a Discard reads back is read after the cancel, in the same task, so the
/// desktop never adopts one that still holds the ended draft.
#[test]
fn race_f_a_discard_never_adopts_a_session_that_still_holds_the_draft() {
    let mut masking = Masking::opened();
    let _ = masking.editor.update(Message::SliderMoved {
        action: "set-basic".into(),
        parameter: "exposure".into(),
        value: 0.3,
    });
    assert_eq!(testing::run_round(&mut masking.editor), Some(Round::Begin));
    masking.draft(DraftMessage::Cancel);
    let draft_id = testing::core_draft(&masking.editor)
        .and_then(|draft| draft.draft_id.clone())
        .expect("the closing draft");
    let reseed = (
        masking.asset.clone(),
        masking.editor.displayed_entry(),
        None,
    );
    let (cancelled, reseed) = tasks::draft_cancel_now(
        &masking.owner(),
        masking.editor.client,
        &draft_id,
        Some(reseed),
    );
    assert!(cancelled.is_ok());
    let payload = reseed
        .expect("the committed frame is read back")
        .expect("and answers");
    assert!(
        payload.session.draft.is_none(),
        "the session read after the cancel holds no draft"
    );
    let _ = masking
        .editor
        .update(Message::Draft(DraftMessage::Cancelled {
            draft: draft_id,
            cancelled,
            reseed: Some(Ok(payload)),
        }));
    assert!(masking.editor.gesture.is_none());
    assert_eq!(
        masking.editor.snapshot()["draft"],
        json!(null),
        "the desktop adopted a session that still holds the ended draft"
    );
}

/// (g) The proxy refit waits for a mask gesture exactly as it waits for a slider gesture: the one
/// refusal answers for both.
#[test]
fn race_g_a_proxy_refit_waits_for_a_mask_gesture() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    masking.editor.window = (1440.0, 900.0);
    masking.editor.dimensions = Some((4000, 3000));
    masking.editor.session.preview.view.zoom = lightwell_core::Zoom::Fit;
    masking.editor.scale_factor = 1.0;
    masking.editor.presented_generation = masking.editor.preview_generation;
    masking.editor.presented_proxy = true;
    masking.editor.presented_bounds = masking.editor.proxy_bounds();
    masking.editor.refit_pending = false;
    let _ = masking.editor.update(Message::ScaleFactor(2.0));
    assert!(
        !masking.editor.refit_pending,
        "a refit displaced the mask gesture's drafted frame"
    );
}

/// Every start the one-draft rule governs answers to the one refusal, with the open gesture's own
/// reason: a slider, a crop, a pick, a mode change, Compare, a preset and the gallery.
#[test]
fn every_start_answers_to_the_one_refusal() {
    use crate::app::gesture::Starting;
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.5, 0.2), (0.5, 0.8));
    let open = masking.editor.mask_shape().cloned();

    let _ = masking
        .editor
        .control_moved("set-basic".to_owned(), "exposure".to_owned(), json!(0.2));
    assert_eq!(
        masking.editor.status,
        "Apply or Cancel the mask gesture before editing a slider"
    );
    let _ = masking.editor.update(Message::CompareBegin);
    assert_eq!(
        masking.editor.status,
        "Apply or Cancel the mask gesture before comparing with the original"
    );
    let _ = masking
        .editor
        .update(Message::SetMode(lightwell_core::POINTER_MODE.to_owned()));
    assert_eq!(
        masking.editor.status,
        "Apply or Cancel the new mask gesture before leaving Mask mode"
    );
    let _ = masking
        .editor
        .update(Message::Crop(crate::app::message::CropMessage::Start));
    assert_eq!(
        masking.editor.status,
        "Apply or Cancel the mask gesture before cropping"
    );
    assert_eq!(
        masking.editor.pick_refusal().as_deref(),
        Some("Apply or Cancel the mask gesture before picking from the photograph")
    );
    assert_eq!(
        masking.editor.gesture_refusal(Starting::Preset).as_deref(),
        Some("Apply or Cancel the mask gesture before applying a preset")
    );
    masking.editor.developer = true;
    let _ = masking.editor.update(Message::Gallery(Some(0)));
    assert!(!masking.editor.workspace.title.can_open_gallery);
    assert!(masking.editor.gallery_page().is_none());
    assert_eq!(
        masking.editor.mask_shape().cloned(),
        open,
        "nothing displaced the open gesture"
    );
}

/// A scripted release that ends a sweep where the sweep left it asks for no frame, so the step
/// captures the next redraw instead of waiting for pixels nothing will render.
///
/// The draft driver sends only geometry the core draft does not already hold. The release after a
/// sweep changes no geometry, so it sends no `draft.set` and no preview job; the step's evidence is
/// the gesture no longer dragging, drawn on the next frame. Waiting for a preview there ran the
/// `mask-linear` and `mask-combine` scenarios to their deadline.
#[test]
fn a_release_that_changes_no_geometry_captures_the_next_redraw() {
    use crate::app::testing::{attach_log, attach_script, evidence, logged};

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::Sweep {
        from: (0.5, 0.3),
        to: (0.5, 0.7),
    }));
    masking.assert_geometry_sent();
    let asked = masking.editor.preview_generation;

    attach_script(&mut masking.editor, r#"[{"mask":{"release":true}}]"#);
    let log = attach_log(&mut masking.editor);
    let _ = masking.editor.next_step();
    assert_eq!(
        masking.editor.preview_generation, asked,
        "the release asked for no frame"
    );
    let run = evidence(&masking.editor);
    assert_eq!(run.awaiting, None, "nothing is waited for");
    assert!(run.capture_pending, "the next redraw is the step's frame");
    assert_eq!(
        masking.editor.mask_draft_summary()["dragging"],
        json!(false),
        "and it shows the gesture released, still open for Apply"
    );
    let events = event_names(&logged(&mut masking.editor, &log));
    assert!(
        !events.iter().any(|event| event == "mask_draft_set"),
        "the release re-sent geometry the core draft already holds: {events:?}"
    );
}

/// A released stroke's step settles on the committed frame while the brush re-arms.
///
/// The brush re-arms on the component its stroke landed on as soon as the commit answers, and its
/// `draft.begin` is in flight when the committed frame arrives. That begin sends no geometry — an
/// armed brush has painted nothing — so it brings no frame, and the committed frame is the step's
/// evidence whether or not the begin has answered. Waiting for the begin as though it would bring a
/// frame ran the `mask-brush` scenario to its deadline whenever the frame won the race.
#[test]
fn a_stroke_settles_on_its_committed_frame_while_the_brush_re_arms() {
    use crate::app::{
        evidence::Settle,
        testing::{attach_script, evidence},
    };

    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    drain_queue(&mut masking);
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    attach_script(&mut masking.editor, r#"[{"wait":{"ms":1}}]"#);
    masking.editor.await_step(Settle::Preview);
    masking.paint(&[(0.3, 0.3), (0.5, 0.35)]);
    assert_eq!(
        testing::core_draft(&masking.editor).and_then(|draft| draft.in_flight()),
        Some(Round::Begin),
        "the brush is re-arming and its begin is on its way"
    );
    let committed = masking.editor.preview_generation;
    drain_queue(&mut masking);
    assert_eq!(masking.editor.presented_generation, committed);
    assert_eq!(
        evidence(&masking.editor).awaiting,
        None,
        "the committed frame settled the step"
    );
}

/// A generated `mask.*` control submitted while a drawn gesture is open is refused exactly as the
/// panel's own commands are: nothing is sent, the gesture is untouched, and the status bar says
/// what it needs. Once the gesture is cancelled the same submit goes out.
#[test]
fn a_generated_mask_command_is_refused_while_a_gesture_is_open() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.draw_mask();
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    masking.open_gesture();
    masking.sweep((0.2, 0.2), (0.8, 0.8));
    let before = masking.editor.mask_shape().cloned();
    let submit = |masking: &mut Masking| {
        masking.editor.last_mask_request = None;
        let _ = masking.editor.update(Message::RunAction {
            action: "mask.set-amount".into(),
            preset: serde_json::Map::from_iter([("amount".to_owned(), json!(40.0))]),
        });
    };
    submit(&mut masking);
    assert_eq!(masking.editor.last_mask_request, None, "nothing was sent");
    assert!(!masking.editor.busy);
    assert_eq!(
        masking.editor.status,
        "Apply or Cancel the mask gesture before editing a mask"
    );
    assert_eq!(masking.editor.mask_shape().cloned(), before);
    assert!(!masking.editor.gesture_conflicted());

    masking.draft(DraftMessage::Cancel);
    testing::run_round(&mut masking.editor);
    assert!(masking.editor.gesture.is_none(), "the gesture closed");
    submit(&mut masking);
    assert!(
        masking.editor.last_mask_request.is_some(),
        "without a gesture the submit goes out: {}",
        masking.editor.status
    );
}

/// Another client commits while the brush is armed. It has painted nothing and sent nothing, so
/// there is nothing to discard or reapply: no Changed elsewhere notice appears, its draft is
/// rebased onto the new revision with one `draft.reapply`, and the next stroke commits on it.
#[test]
fn an_armed_brush_changed_elsewhere_rebases_without_a_notice() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.paint(&[(0.3, 0.3), (0.5, 0.35)]);
    masking.open_gesture();
    assert!(masking.editor.armed_brush(), "the brush is armed");

    agent_commits(&mut masking);
    let revision = masking.editor.state.as_ref().unwrap().revision;
    assert!(masking.editor.armed_brush(), "the brush stays in hand");
    assert!(!masking.editor.gesture_conflicted(), "no notice");
    assert!(
        !masking
            .editor
            .workspace
            .canvas
            .notices
            .iter()
            .any(|notice| notice.title == "Changed elsewhere"),
        "no Changed elsewhere card"
    );
    assert!(
        !masking.editor.status.starts_with("Changed elsewhere"),
        "{}",
        masking.editor.status
    );
    assert_eq!(
        testing::run_round(&mut masking.editor),
        Some(Round::Reapply),
        "the brush's draft is rebased"
    );
    let draft = testing::core_draft(&masking.editor).expect("the brush's draft");
    assert!(!draft.conflicted && draft.drained());
    assert_eq!(draft.base_revision, revision);
    assert_eq!(masking.editor.armed_rebase, None);
    assert_eq!(testing::run_round(&mut masking.editor), None, "one reapply");

    masking.paint(&[(0.5, 0.6), (0.65, 0.65)]);
    assert_eq!(masking.labels(), ["Add brush", "Update Brush 1"]);
}

/// A brush that has painted holds a stroke of its own, so a commit elsewhere during it shows the
/// Changed elsewhere notice with Discard and Reapply, and nothing is rebased behind its back.
#[test]
fn a_painted_brush_changed_elsewhere_shows_the_notice() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.message(MaskMessage::Paint(PaintTarget::NewMask));
    masking.open_gesture();
    masking.message(MaskMessage::Handle(MaskPointer::PaintBegin {
        x: 0.3,
        y: 0.3,
    }));
    masking.message(MaskMessage::Handle(MaskPointer::PaintTo { x: 0.5, y: 0.4 }));
    masking.assert_geometry_sent();
    assert!(!masking.editor.armed_brush(), "the brush has painted");

    agent_commits(&mut masking);
    assert!(masking.editor.gesture_conflicted());
    assert_eq!(
        masking.editor.status,
        "Changed elsewhere: discard the mask gesture or reapply it"
    );
    assert!(
        masking
            .editor
            .workspace
            .canvas
            .notices
            .iter()
            .any(|notice| notice.title == "Changed elsewhere"),
        "the notice offers Discard and Reapply"
    );
    assert_eq!(masking.editor.armed_rebase, None);
    assert_eq!(
        testing::run_round(&mut masking.editor),
        None,
        "nothing is rebased until the person chooses"
    );
}

/// An independent client commits an edit, and the desktop reads it back as its poll would.
fn agent_commits(masking: &mut Masking) {
    let revision = masking.editor.state.as_ref().unwrap().revision;
    call(
        &masking.owner(),
        masking.agent,
        "edit.set-basic",
        json!({"asset_id": masking.asset, "exposure": 0.5,
            "mutation": {"expected_revision": revision, "request_id": format!("agent-{revision}"), "actor": "agent"}}),
    )
    .unwrap();
    masking.refresh();
}
