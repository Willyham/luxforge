//! The Mask mode, the Masks panel and the gradient handle editor against a real owner and catalog.
//!
//! Every request these gestures build is compared with the one an independent JSON client sends for
//! the same edit, and every refusal the command family makes is asserted where the panel surfaces
//! it. Tasks are run here as the plain functions they wrap, so the answers reach the editor as the
//! same messages the runtime delivers.
use super::{
    Boot, Editor,
    message::{MaskMessage, MaskPointer, MenuTarget, Message, RowEdit},
    tasks::{self, call},
};
use crate::{
    Config,
    app::testing::descriptors,
    mask_draft::{LINEAR, MaskHandle, RADIAL},
};
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
            json!({"path": fixture("fixtures/s0/orientation-1.jpg")}),
        )
        .unwrap();
        let job_id = queued["job_id"].as_str().expect("a source job").to_owned();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (status, _) = call(&owner, agent, "job.status", json!({"job_id": job_id})).unwrap();
            match status["state"].as_str() {
                Some("ready") => break,
                Some("queued" | "preparing") => {
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
            true,
            self.editor.api_sequence,
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
        let (session, sequence) = call(
            &self.owner(),
            self.editor.client,
            "session.state",
            json!({}),
        )
        .unwrap();
        let _ = self.editor.update(Message::WorkspaceUpdated(Ok((
            serde_json::from_value(session).unwrap(),
            sequence,
        ))));
    }

    fn message(&mut self, message: MaskMessage) {
        let _ = self.editor.update(Message::Mask(message));
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

    /// Run the open gesture's `draft.begin`, then its `draft.set`, as the runtime's tasks do.
    fn open_gesture(&mut self) {
        let draft = self.editor.mask_draft.as_ref().expect("a gesture is open");
        let method = draft.method().expect("a generated method");
        let target = lightwell_core::mask::commands::MaskTarget {
            mask: draft.mask.clone(),
            component: draft.component.clone(),
            name: None,
        };
        let (begun, _) = call(
            &self.owner(),
            self.editor.client,
            "draft.begin",
            tasks::draft_begin_params(self.asset.clone(), method, target),
        )
        .unwrap();
        let _ = self.editor.update(Message::MaskDraftBegun(Ok(Box::new(
            serde_json::from_value(begun).unwrap(),
        ))));
        // `render.transform` answers the affine the handles are mapped through.
        let (transform, _) = call(
            &self.owner(),
            self.editor.client,
            "render.transform",
            json!({"asset_id": self.asset}),
        )
        .unwrap();
        let _ = self
            .editor
            .update(Message::MaskTransform(Ok(serde_json::from_value(
                transform,
            )
            .unwrap())));
        self.drain_draft_set();
    }

    /// Answer whatever `draft.set` the gesture has outstanding, as its task does.
    fn drain_draft_set(&mut self) {
        for _ in 0..8 {
            if !self.editor.mask_draft_in_flight {
                break;
            }
            let (draft_id, fields) = {
                let id = self
                    .editor
                    .mask_draft_id
                    .clone()
                    .expect("the core draft is open");
                let draft = self.editor.mask_draft.as_ref().expect("a gesture");
                (id, Value::Object(draft.fields()))
            };
            let (set, _) = call(
                &self.owner(),
                self.editor.client,
                "draft.set",
                json!({"draft_id": draft_id, "fields": fields}),
            )
            .unwrap();
            let job = self
                .owner()
                .preview_job(
                    lightwell_core::PreviewRequest::new(self.editor.client, self.asset.clone())
                        .draft(draft_id),
                )
                .unwrap();
            let _ = self.editor.update(Message::MaskDraftSet(Ok(Box::new((
                serde_json::from_value(set).unwrap(),
                job,
            )))));
        }
    }

    /// Commit the open gesture, as Apply does.
    fn apply(&mut self) {
        self.message(MaskMessage::Apply);
        let draft_id = self
            .editor
            .mask_draft_id
            .clone()
            .expect("the core draft is open");
        let revision = self.editor.state.as_ref().unwrap().revision;
        let (committed, sequence) = call(
            &self.owner(),
            self.editor.client,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": tasks::mutation(revision)}),
        )
        .unwrap_or_else(|error| panic!("the gesture commits: {error}"));
        assert_ne!(committed["outcome"], json!("no-op"), "{committed}");
        // The desktop reads this answer back, and a mask command answers with what it changed
        // beside the mutation envelope. Reading it as the bare envelope refuses the extra fields by
        // name and turns every committed gesture into a failure, so the shape is pinned here.
        serde_json::from_value::<lightwell_core::mask::commands::MaskCommandResult>(
            committed.clone(),
        )
        .unwrap_or_else(|error| {
            panic!("the desktop reads the commit's own answer: {error}: {committed}")
        });
        let refreshed = tasks::refresh(
            &self.owner(),
            self.editor.client,
            self.asset.clone(),
            true,
            sequence,
            None,
        )
        .unwrap();
        let _ = self
            .editor
            .update(Message::MaskDraftCommitted(Ok(Some(Box::new(refreshed)))));
    }

    /// One whole shape drawn in a stroke, as a press and a drag on the photograph do.
    fn sweep(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.message(MaskMessage::Handle(MaskPointer::Sweep { from, to }));
        self.drain_draft_set();
        self.message(MaskMessage::Handle(MaskPointer::End));
        self.drain_draft_set();
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
        let draft = self.editor.mask_draft.as_ref().expect("a gesture is open");
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
        masking.editor.mask_draft.is_some(),
        "the gesture was discarded by a refused mode change"
    );
    // Compare during a gesture is refused for the same reason.
    let _ = masking.editor.update(Message::CompareBegin);
    assert!(
        masking.editor.status.contains("mask gesture"),
        "{}",
        masking.editor.status
    );
    assert!(masking.editor.mask_draft.is_some());
    // Cancel ends it, and then the mode can be left.
    masking.message(MaskMessage::Cancel);
    assert!(masking.editor.mask_draft.is_none());
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
        masking.drain_draft_set();
    }
    masking.message(MaskMessage::Handle(MaskPointer::End));
    masking.drain_draft_set();
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
    masking.drain_draft_set();
    assert_eq!(
        masking
            .editor
            .mask_draft
            .as_ref()
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
            .mask_draft
            .as_ref()
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
    let (begun, _) = call(
        &masking.owner(),
        masking.editor.client,
        "draft.begin",
        tasks::draft_begin_params(masking.asset.clone(), "set-basic", target),
    )
    .unwrap_or_else(|error| panic!("a maskable action drafts through a mask: {error}"));
    let draft: lightwell_core::Draft = serde_json::from_value(begun).unwrap();
    assert_eq!(
        draft
            .target
            .as_ref()
            .and_then(|target| target.mask.as_ref()),
        Some(&mask)
    );
    let _ = masking
        .editor
        .update(Message::SliderDraftBegun(Ok(Box::new(draft.clone()))));

    // The drafted preview is the masked stack: committing it writes exactly one masked layer.
    let (_, sequence) = call(
        &masking.owner(),
        masking.editor.client,
        "draft.set",
        json!({"draft_id": draft.draft_id, "fields": {"exposure": 0.3}}),
    )
    .unwrap();
    let revision = masking.editor.state.as_ref().unwrap().revision;
    let (committed, sequence) = call(
        &masking.owner(),
        masking.editor.client,
        "draft.commit",
        json!({"draft_id": draft.draft_id, "mutation": tasks::mutation(revision)}),
    )
    .map(|(value, seen)| (value, seen.max(sequence)))
    .unwrap_or_else(|error| panic!("a masked gesture commits: {error}"));
    assert_ne!(committed["outcome"], json!("no-op"), "{committed}");
    let refreshed = tasks::refresh(
        &masking.owner(),
        masking.editor.client,
        masking.asset.clone(),
        true,
        sequence,
        None,
    )
    .unwrap();
    let _ = masking
        .editor
        .update(Message::SliderDraftCommitted(Ok(Some(Box::new(refreshed)))));
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
    masking.drain_draft_set();
    masking.message(MaskMessage::Handle(MaskPointer::End));
    masking.drain_draft_set();
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
    for kind in &panel.kinds {
        assert!(kind.drawable, "{} has no handles", kind.kind);
    }

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
            .mask_draft
            .as_ref()
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
    let aspect = masking
        .editor
        .mask_draft
        .as_ref()
        .expect("a gesture")
        .aspect();
    assert!(aspect > 0.0 && aspect.is_finite(), "{aspect}");

    masking.sweep((0.5, 0.5), (0.75, 0.8));
    // Every handle is where the panel's own fields say it is, at every step of the drag.
    let handles: Vec<_> = masking
        .editor
        .mask_draft
        .as_ref()
        .expect("a gesture")
        .handles();
    assert_eq!(
        handles.len(),
        7,
        "four radii, a centre, a rotation grip and the ring"
    );
    for (handle, _) in &handles {
        let from = masking
            .editor
            .mask_draft
            .as_ref()
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
            masking.drain_draft_set();
            masking.assert_fields_match_the_draft(&format!("{handle:?} {step:?}"));
        }
        masking.message(MaskMessage::Handle(MaskPointer::End));
        masking.drain_draft_set();
    }
    // One drag of seven handles is still one draft, and the commit is one entry.
    let drawn: Vec<(String, f64)> = masking
        .editor
        .mask_draft
        .as_ref()
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
    let reopened = masking.editor.mask_draft.as_ref().expect("a gesture");
    for (name, value) in reopened.values() {
        assert_eq!(listed.components[0].payload[name], json!(value), "{name}");
    }
    masking.message(MaskMessage::Cancel);
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
        masking.editor.mask_draft.is_none(),
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
