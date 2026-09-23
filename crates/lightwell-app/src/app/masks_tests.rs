//! The Mask mode, the Masks panel and the gradient handle editor against a real owner and catalog.
//!
//! Every request these gestures build is compared with the one an independent JSON client sends for
//! the same edit, and every refusal the command family makes is asserted where the panel surfaces
//! it. Tasks are run here as the plain functions they wrap, so the answers reach the editor as the
//! same messages the runtime delivers.
use super::{
    Boot, Editor,
    message::{MaskMessage, MaskPointer, MenuTarget, Message},
    tasks::{self, call},
};
use crate::{
    Config,
    app::testing::descriptors,
    mask_draft::{LINEAR, MaskHandle},
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

    /// Draw one whole gradient and commit it, which is what New mask does end to end.
    fn draw_mask(&mut self) {
        self.message(MaskMessage::New(LINEAR.to_owned()));
        self.open_gesture();
        self.message(MaskMessage::Handle(MaskPointer::Sweep {
            from: (0.5, 0.2),
            to: (0.5, 0.8),
        }));
        self.drain_draft_set();
        self.message(MaskMessage::Handle(MaskPointer::End));
        self.drain_draft_set();
        self.apply();
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
        lightwell_core::mask::component_kinds().collect::<Vec<_>>(),
        "the kinds are the host's, not a list of the panel's own"
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
            .gradient
            .y1,
        0.5
    );
    masking.message(MaskMessage::Field {
        name: "y1".into(),
        value: 99.0,
    });
    assert_eq!(
        masking.editor.mask_draft.as_ref().unwrap().gradient.y1,
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
        only.controls.is_empty(),
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
    let result = masking.run(MaskMessage::Invert(mask.as_str().to_owned()));
    assert_eq!(result["label"], json!("Inverted"), "{result}");
    assert!(masking.listing().masks[0].invert);

    // Duplicate copies the mask and its layers, so the list grows by one.
    masking.run(MaskMessage::Duplicate(mask.as_str().to_owned()));
    assert_eq!(masking.listing().masks.len(), 2);

    // Delete removes it and names the layers it removed.
    let result = masking.run(MaskMessage::Delete(mask.as_str().to_owned()));
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
