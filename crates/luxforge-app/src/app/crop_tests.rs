use super::*;
use crate::app::{
    draft::Round,
    message::{
        Message, control::ControlMessage, draft::DraftMessage, pointer::PointerMessage,
        sync::SyncMessage, view::ViewMessage,
    },
    tasks::SyncResult,
    testing::{
        CROP_ASPECTS, CROP_SOURCE, answer_commit, core_draft, crop_layer, described_at, entry,
        events, finish, open_crop, opened, refresh_for,
    },
};
use crate::crop_draft::{Corner, Handle};
use crate::state::tools::{ControlModel, NumberControlStyle, SliderControl};
use luxforge_core::{ClientSession, HistoryEntry, HistorySelection};

/// The crop angle's declared step: one press of its − or + button.
const STEP: f64 = 0.5;

/// One message of the angle's stepper, naming the crop action's declared angle as the widget
/// does.
fn angle_message(
    editor: &Editor,
    message: impl FnOnce(String, String) -> ControlMessage,
) -> Message {
    let frame = crop_frame(&editor.modules).expect("a crop frame");
    Message::Control(message(frame.action.to_owned(), frame.angle.to_owned()))
}

/// Type `text` into the angle's box and press Enter.
fn submit_angle(editor: &mut Editor, text: &str) {
    let typed = angle_message(editor, |action, parameter| ControlMessage::Field {
        action,
        parameter,
        text: text.to_owned(),
    });
    let _ = editor.update(typed);
    let enter = angle_message(editor, |action, parameter| ControlMessage::Submit {
        action,
        parameter: Some(parameter),
    });
    let _ = editor.update(enter);
}

/// One press of the angle's − (`-1`) or + (`1`) button.
fn step_angle(editor: &mut Editor, direction: i8) {
    let press = angle_message(editor, |action, parameter| ControlMessage::Step {
        action,
        parameter,
        direction,
    });
    let _ = editor.update(press);
}

/// A drag on the angle's rail through `fractions`, not yet released.
fn drag_angle(editor: &mut Editor, fractions: &[f64]) {
    for &fraction in fractions {
        let moved = angle_message(editor, |action, parameter| ControlMessage::Fraction {
            action,
            parameter,
            fraction,
        });
        let _ = editor.update(moved);
    }
}

/// The end of a drag on the angle's rail.
fn release_angle(editor: &mut Editor) {
    let released = angle_message(editor, |action, parameter| ControlMessage::Released {
        action,
        parameter,
    });
    let _ = editor.update(released);
}

/// The crop section's angle control as the panel draws it.
fn angle_control(editor: &Editor) -> SliderControl {
    editor
        .workspace
        .tools
        .all()
        .flat_map(|section| section.controls.iter())
        .find_map(|control| match control {
            ControlModel::CropFrame(model) => model.angle.clone(),
            _ => None,
        })
        .expect("the crop section's angle")
}

/// The angle the crop section's box shows, as a captured frame records it.
fn section_angle(editor: &Editor) -> Value {
    editor.snapshot()["crop"]["section"]["angle"].clone()
}

#[test]
fn crop_focus_restores_all_disclosures_on_cancel_noop_and_failed_stage() {
    for exit in ["cancel", "noop", "failed-stage"] {
        let (mut editor, catalog, _, _) = opened(Vec::new(), 2);
        editor
            .controls
            .expanded
            .insert("luxforge.basic".into(), true);
        editor
            .controls
            .expanded
            .insert("luxforge.crop".into(), false);
        editor
            .controls
            .expanded
            .insert("luxforge.controls".into(), true);
        let before = editor.controls.expanded.clone();
        let _ = editor.update(Message::Crop(CropMessage::Start));
        assert!(
            editor
                .workspace
                .tools
                .all()
                .all(|section| { section.expanded == (section.module_id == "luxforge.crop") })
        );
        // A second Start must not replace the saved layout with the focused one. Changes to
        // disclosures during crop are temporary too; implicit defaults must remain implicit.
        let _ = editor.update(Message::Crop(CropMessage::Start));
        let _ = editor.update(Message::Control(ControlMessage::ToggleSection(
            "luxforge.basic".into(),
        )));
        match exit {
            "cancel" => draft_message(&mut editor, DraftMessage::Cancel),
            "noop" => {
                draft_message(&mut editor, DraftMessage::Commit);
                answer_commit(&mut editor, Ok(None));
            }
            _ => {
                let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
                    StagePlan::Open,
                    Err("source unavailable".into()),
                )));
            }
        }
        assert!(editor.crop().is_none(), "{exit}");
        assert_eq!(editor.controls.expanded, before, "{exit}");
        assert!(editor.crop_section.previous_expanded.is_none());
        finish(editor, catalog);
    }
}

#[test]
fn straighten_is_one_shot_and_the_next_drag_uses_crop_handles() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 2);
    let _ = editor.update(Message::Crop(CropMessage::Guide(true)));
    assert!(editor.crop_section.guide);
    for pointer in [
        CropPointer::Begin {
            handle: Handle::Guide,
            x: 100.0,
            y: 100.0,
        },
        CropPointer::Drag {
            x: 300.0,
            y: 120.0,
            option: false,
        },
        CropPointer::End,
    ] {
        let _ = editor.update(Message::Crop(CropMessage::Pointer(pointer)));
    }
    assert!(!editor.crop_section.guide);
    assert_eq!(
        editor.workspace.canvas.surface_mode,
        crate::state::canvas::SurfaceMode::Frame
    );
    let angle = editor.crop().expect("a crop draft").stage.angle;
    assert!(angle.abs() > 1.0);
    for pointer in [
        CropPointer::Begin {
            handle: Handle::Move,
            x: 200.0,
            y: 150.0,
        },
        CropPointer::Drag {
            x: 210.0,
            y: 150.0,
            option: false,
        },
        CropPointer::End,
    ] {
        let _ = editor.update(Message::Crop(CropMessage::Pointer(pointer)));
    }
    assert_eq!(editor.crop().expect("a crop draft").stage.angle, angle);
    assert_eq!(editor.document.state.as_ref().expect("a state").revision, 2);
    finish(editor, catalog);
}

/// The stage the rows of [`opened`] give a crop with no geometry ahead of it.
fn stage() -> CropStage {
    CropStage {
        width: CROP_SOURCE.0,
        height: CROP_SOURCE.1,
        angle: 0.0,
    }
}

fn draft_message(editor: &mut Editor, message: DraftMessage) {
    let _ = editor.update(Message::Draft(message));
}

/// Another client's commit of `newer`, read back with the rows the owner describes for it.
fn committed_elsewhere(editor: &mut Editor, asset: &luxforge_core::AssetId, newer: &HistoryEntry) {
    let mut refresh = refresh_for(asset, newer, vec![newer.clone()], &[newer], false);
    refresh.recipe = described_at(newer, CROP_SOURCE);
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(SyncResult::changed(
        refresh,
    )))));
}

/// The frame opens in the update that starts the draft, from the current entry's row for the
/// stack's first crop layer: its layer, its index, the stage it receives and the rectangle and
/// angle its values hold, exactly as committed. The stage's pixels and the core draft's
/// `draft.begin` are still on their way.
#[test]
fn a_draft_opens_at_once_on_the_existing_crop_layers_row() {
    let payload = CropPayload {
        angle: 7.0,
        x: 0.2,
        y: 0.25,
        width: 0.4,
        height: 0.3,
    };
    let earlier = luxforge_core::Layer::pixel(0, 0, [1, 2, 3]);
    let crop = crop_layer(payload);
    let (mut editor, catalog, _, _) = opened(
        vec![earlier, crop.clone(), crop_layer(CropPayload::NEUTRAL)],
        4,
    );
    // Only the first crop layer is the one being edited.
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let draft = editor.crop().expect("the frame opened with the start");
    assert_eq!(draft.layer, Some(crop.id.clone()));
    assert_eq!(draft.layer_index, 1, "the stage is the pixel edit's output");
    assert_eq!((draft.stage.width, draft.stage.height), CROP_SOURCE);
    assert_eq!(draft.stage.angle, 7.0);
    let reopened = CropStage {
        angle: 7.0,
        ..stage()
    };
    assert_eq!(
        draft.output().expect("a valid draft"),
        payload.output_rect(&reopened).expect("a valid payload"),
        "reopening shows exactly the rectangle the payload committed"
    );
    assert_eq!(section_angle(&editor), json!("7.0"));
    assert_eq!(
        editor.crop_stage(),
        Some(StageView::Rendering {
            reapply: false,
            base_revision: 4
        }),
        "the stage's pixels are on their way"
    );
    assert!(
        core_draft(&editor).is_some_and(CoreDraft::drained),
        "the draft opened and took the frame's fields in the start's own update"
    );
    assert_eq!(editor.snapshot()["crop"]["layer_index"], json!(1));
    open_crop(&mut editor);
    assert_eq!(editor.crop_stage(), Some(StageView::Shown));
    assert_eq!(core_draft(&editor).expect("a core draft").base_revision, 4);
    finish(editor, catalog);
}

/// The angle's rail is a continuous gesture on the draft: every move follows the rail on its
/// step and refits the rectangle, only the release logs a draft change, and nothing commits.
#[test]
fn a_drag_on_the_angle_rail_drafts_the_angle_and_logs_once_on_release() {
    let (mut editor, catalog, _, _) = opened(vec![luxforge_core::Layer::pixel(0, 0, [9, 9, 9])], 2);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    let log = crate::app::testing::attach_log(&mut editor);
    // 2.4° is 47.4 of the rail's 90°; a fraction a hair off it snaps to the angle's declared
    // 0.05° fine step.
    drag_angle(&mut editor, &[0.6, 0.5 + 2.4 / 90.0 + 1e-4]);
    let draft = editor.crop().expect("the draft stays open");
    assert_eq!(draft.stage.angle, 2.4);
    let rect = draft.rect;
    // The generic stepper with its rail, reading 2.4°, its handle live for the open draft.
    let control = angle_control(&editor);
    assert_eq!(control.style, NumberControlStyle::Stepper { rail: true });
    assert_eq!(
        (control.display.as_str(), control.unit.as_deref()),
        ("2.4", Some("\u{b0}"))
    );
    assert_eq!((control.value, control.dragging), (2.4, true));
    assert_eq!((control.spec.step, control.spec.fine_step), (0.5, 0.05));
    release_angle(&mut editor);
    assert_eq!(editor.crop().expect("still drafting").rect, rect);
    assert_eq!(editor.document.state.as_ref().expect("a state").revision, 2);
    let changes: Vec<Value> = crate::app::testing::logged(&mut editor, &log)
        .into_iter()
        .filter(|record| record["event"] == "crop_draft_changed")
        .collect();
    assert_eq!(changes.len(), 1, "one draft change, at the release");
    assert_eq!(changes[0]["detail"]["angle"], json!(2.4));
    finish(editor, catalog);
}

/// A double-click on the angle's rail sends the generic `ResetField`, which puts the angle back
/// to its declared default of 0 and sends it, as a button press sends its angle: the frame,
/// the angle's box and the client's core draft all read 0, and exactly one draft change is
/// logged and set on the core draft. Nothing commits.
#[test]
fn a_double_click_on_the_angle_rail_resets_the_angle_as_one_change() {
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-crop-reset-{}-{}.sqlite",
        std::process::id(),
        crate::app::tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let (mut editor, _, _) = crate::app::testing::real_photo(&catalog);
    let (owner, client) = (editor.owner.clone(), editor.client);
    let draft = || {
        crate::app::tasks::call(&owner, client, "session.state", json!({}))
            .expect("a session")
            .0["draft"]
            .clone()
    };
    let revision = editor
        .document
        .state
        .as_ref()
        .expect("a photograph")
        .revision;
    let _ = editor.update(Message::Crop(CropMessage::Start));
    step_angle(&mut editor, 1);
    assert_eq!(editor.crop().expect("a frame").stage.angle, STEP);
    let before = draft();
    assert_eq!(before["fields"]["angle"], json!(STEP));

    let log = crate::app::testing::attach_log(&mut editor);
    let reset = angle_message(&editor, |action, parameter| ControlMessage::ResetField {
        action,
        parameter,
    });
    let _ = editor.update(reset);
    assert_eq!(editor.crop().expect("still drafting").stage.angle, 0.0);
    assert_eq!(section_angle(&editor), json!("0.0"));
    let after = draft();
    assert_eq!(after["fields"]["angle"], json!(0.0));
    assert_eq!(
        after["draft_revision"].as_u64(),
        before["draft_revision"]
            .as_u64()
            .map(|revision| revision + 1),
        "one change reached the core draft"
    );
    let changes: Vec<Value> = crate::app::testing::logged(&mut editor, &log)
        .into_iter()
        .filter(|record| record["event"] == "crop_draft_changed")
        .collect();
    assert_eq!(changes.len(), 1, "one draft change");
    assert_eq!(changes[0]["detail"]["angle"], json!(0.0));
    assert_eq!(
        editor
            .document
            .state
            .as_ref()
            .expect("a photograph")
            .revision,
        revision,
        "nothing committed"
    );
    finish(editor, catalog);
}

/// The angle's box shows the angle with its unit until it is pressed; a submitted number closes
/// it again, and text that is not a number keeps it open for correcting.
#[test]
fn the_angle_box_opens_for_typing_and_closes_on_a_submitted_number() {
    let (mut editor, catalog, _, _) = opened(vec![luxforge_core::Layer::pixel(0, 0, [9, 9, 9])], 2);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    let frame = crop_frame(&editor.modules).expect("a crop frame");
    let key = (frame.action.to_owned(), frame.angle.to_owned());
    assert!(!editor.editing_angle());
    let _ = editor.update(Message::Control(ControlMessage::EditValue {
        action: key.0.clone(),
        parameter: key.1.clone(),
    }));
    assert!(editor.editing_angle());
    submit_angle(&mut editor, "two");
    assert!(editor.editing_angle(), "invalid text stays open");
    // The box says why, as every number field does, and so does the status line.
    assert_eq!(
        angle_control(&editor).invalid.as_deref(),
        Some("angle must be a number from -45 to 45")
    );
    assert_eq!(editor.status.text, "angle must be a number from -45 to 45");
    submit_angle(&mut editor, "46");
    assert!(editor.editing_angle(), "an angle out of range stays open");
    assert_eq!(editor.crop().expect("a draft").stage.angle, 0.0);
    submit_angle(&mut editor, "3.5");
    assert!(!editor.editing_angle());
    assert_eq!(editor.crop().expect("a draft").stage.angle, 3.5);
    finish(editor, catalog);
}

#[test]
fn a_stack_without_a_crop_layer_drafts_a_neutral_crop_at_the_end() {
    let (mut editor, catalog, _, _) = opened(vec![luxforge_core::Layer::pixel(0, 0, [9, 9, 9])], 2);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let draft = editor.crop().expect("an opened draft");
    assert!(draft.layer.is_none());
    assert_eq!(draft.layer_index, 1, "the whole stack is the input stage");
    assert_eq!(
        (draft.stage.width, draft.stage.height),
        CROP_SOURCE,
        "the stack's output stage, which no row reports"
    );
    assert_eq!(draft.payload(), CropPayload::NEUTRAL);
    finish(editor, catalog);
}

/// A crop layer whose row carries no readable values is never silently replaced by a neutral
/// crop: the start is refused before any draft begins, and says why.
#[test]
fn an_unreadable_crop_payload_refuses_the_draft_and_keeps_the_layer() {
    let mut broken = crop_layer(CropPayload::NEUTRAL);
    broken.payload = json!({"angle":"sideways"});
    let (mut editor, catalog, _, _) = opened(vec![broken], 1);
    let task = editor.dispatch(Message::Crop(CropMessage::Start));
    assert_eq!(task.units(), 0, "nothing was sent");
    assert!(editor.gesture.is_none(), "no draft began");
    assert!(
        editor.crop().is_none(),
        "no neutral crop replaced the layer"
    );
    assert_eq!(editor.sync.mode, None, "the mode did not change");
    assert!(
        editor.status.text.contains("cannot be read"),
        "{}",
        editor.status.text
    );
    finish(editor, catalog);
}

#[test]
fn every_draft_change_is_reachable_as_a_message() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 3);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    let start = editor.crop().expect("a draft").rect;

    // A pointer gesture: begin, drag, end. Nothing changes until the drag arrives.
    let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::Begin {
        handle: Handle::Corner(Corner::TopLeft),
        x: 0.0,
        y: 0.0,
    })));
    assert_eq!(editor.crop().expect("a draft").rect, start);
    let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::Drag {
        x: 80.0,
        y: 60.0,
        option: false,
    })));
    let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::End)));
    let dragged = editor.crop().expect("a draft").rect;
    assert_eq!((dragged.x, dragged.y), (80.0, 60.0));

    // The angle's stepper: its box, its buttons and the arrow keys, each held key a live move
    // that its release makes one change. Shift moves ten steps and Option the fine step.
    submit_angle(&mut editor, "11.5");
    assert_eq!(editor.crop().expect("a draft").stage.angle, 11.5);
    step_angle(&mut editor, -1);
    assert_eq!(editor.crop().expect("a draft").stage.angle, 11.0);
    assert_eq!(section_angle(&editor), json!("11.0"));
    for (shift, option, expected) in [(true, false, 6.0), (false, true, 5.95)] {
        let key = angle_message(&editor, |action, parameter| ControlMessage::KeyNudge {
            action,
            parameter,
            direction: -1,
            shift,
            option,
        });
        let _ = editor.update(key);
        release_angle(&mut editor);
        let angle = editor.crop().expect("a draft").stage.angle;
        assert!((angle - expected).abs() < 1e-9, "{angle} is not {expected}");
    }
    assert_eq!(section_angle(&editor), json!("5.95"));
    submit_angle(&mut editor, "sideways");
    assert!((editor.crop().expect("a draft").stage.angle - 5.95).abs() < 1e-9);
    assert!(
        editor.status.text.contains("angle must be a number"),
        "{}",
        editor.status.text
    );
    submit_angle(&mut editor, "0");

    // Ratio presets, swap, lock and the custom extents, all by index into the declared list.
    let _ = editor.update(Message::Crop(CropMessage::Preset(option("16:9"))));
    let draft = editor.crop().expect("a draft");
    assert_eq!(draft.preset, "16:9");
    assert_eq!(draft.aspect.ratio(), Some(16.0 / 9.0));
    let _ = editor.update(Message::Crop(CropMessage::Swap));
    assert_eq!(
        editor.crop().expect("a draft").aspect.ratio(),
        Some(9.0 / 16.0)
    );
    let _ = editor.update(Message::Crop(CropMessage::Lock));
    assert_eq!(editor.crop().expect("a draft").aspect.ratio(), None);
    let _ = editor.update(Message::Crop(CropMessage::CustomWidth("5".into())));
    let _ = editor.update(Message::Crop(CropMessage::CustomHeight("4".into())));
    let _ = editor.update(Message::Crop(CropMessage::Preset(option("custom"))));
    assert_eq!(editor.crop().expect("a draft").aspect.ratio(), Some(1.25));
    let _ = editor.update(Message::Crop(CropMessage::CustomHeight("none".into())));
    let _ = editor.update(Message::Crop(CropMessage::Preset(option("1:1"))));
    let _ = editor.update(Message::Crop(CropMessage::Preset(option("custom"))));
    assert_eq!(
        editor.crop().expect("a draft").aspect.ratio(),
        Some(1.0),
        "an unreadable custom extent changes nothing"
    );

    // The modifier and guide state the canvas reads is app state, reachable by message.
    for (message, read) in [
        (CropMessage::Option(true), true),
        (CropMessage::Option(false), false),
    ] {
        let _ = editor.update(Message::Crop(message));
        assert_eq!(editor.crop_section.option, read);
    }
    let _ = editor.update(Message::Crop(CropMessage::Space(true)));
    assert!(editor.crop_section.space);
    let _ = editor.update(Message::Crop(CropMessage::Guide(true)));
    assert!(editor.crop_section.guide);

    // Cancel is the one draft lifecycle's, as it is for every gesture.
    draft_message(&mut editor, DraftMessage::Cancel);
    assert!(editor.crop().is_none());
    assert!(editor.presentation.presenter.stage().is_none());
    assert!(
        !editor.crop_section.guide,
        "cancelling leaves no guide mode on"
    );
    let summary = editor.snapshot()["crop"].clone();
    assert_eq!(summary["drafting"], json!(false));
    // The idle section is back, reading the stack the draft never committed to.
    assert_eq!(
        summary["section"],
        json!({"drafting":false,"enabled":true,"chosen":"Free","locked":false,"can_swap":false,"angle":"0.0","rail":0.0,"guide":false})
    );
    finish(editor, catalog);
}

/// Every change's end sets the payload's declared fields on the core draft, which `session.state`
/// reports; Apply commits that draft against the revision it is based on, and Copy as JSON
/// request offers the `edit.crop` request an independent client would send for the same frame.
#[test]
fn apply_commits_the_core_draft_the_frame_has_set() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 6);
    let log = crate::app::testing::attach_log(&mut editor);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::Begin {
        handle: Handle::Corner(Corner::TopLeft),
        x: 0.0,
        y: 0.0,
    })));
    let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::Drag {
        x: 48.0,
        y: 32.0,
        option: false,
    })));
    let set = editor
        .session
        .draft
        .clone()
        .expect("the session holds the draft");
    let _ = editor.update(Message::Crop(CropMessage::Pointer(CropPointer::End)));
    let payload = editor.crop().expect("a draft").payload();
    let held = editor
        .session
        .draft
        .clone()
        .expect("the session holds the draft");
    assert_eq!(
        held.draft_revision,
        set.draft_revision + 1,
        "a drag is one draft.set, at its end"
    );
    assert_eq!(held.action, "crop");
    assert_eq!(held.fields["angle"], json!(payload.angle));
    assert_eq!(held.fields["x"], json!(payload.x));
    assert_eq!(held.fields["width"], json!(payload.width));
    assert!(
        held.fields.get("aspect").is_none(),
        "only the declared five"
    );
    assert_eq!(editor.snapshot()["draft"]["fields"], json!(held.fields));

    let (method, request) = editor
        .crop_copy_request()
        .expect("a request")
        .expect("a valid draft");
    assert_eq!(method, "edit.crop");
    assert_eq!(request["asset_id"], json!(asset));
    assert_eq!(request["mutation"]["expected_revision"], json!(6));
    assert_eq!(request["angle"], json!(payload.angle));

    draft_message(&mut editor, DraftMessage::Commit);
    assert_eq!(
        core_draft(&editor).and_then(CoreDraft::in_flight),
        Some(Round::Commit)
    );
    let records = crate::app::testing::logged(&mut editor, &log);
    let commits = events(&records, "crop_draft_commit");
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0]["expected_revision"], json!(6));
    assert_eq!(
        events(&records, "crop_draft_set").len(),
        2,
        "the opened frame and the drag"
    );
    finish(editor, catalog);
}

/// Another client's commit conflicts the draft, which the one Changed elsewhere notice says;
/// the shared Reapply rebases the frame at once, onto the stage the new rows report, and the
/// core draft through `draft.reapply`, in the same update.
#[test]
fn an_external_commit_marks_the_draft_conflicted_and_reapply_rebases_it() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 1);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    submit_angle(&mut editor, "6");
    let composed = editor.crop().expect("a draft").rect;

    // Somebody else committed: the draft survives and says so, and Apply is refused.
    committed_elsewhere(&mut editor, &asset, &entry(&asset, 9, None));
    assert!(core_draft(&editor).expect("the draft is kept").conflicted);
    assert_eq!(
        editor.crop().expect("the draft is kept").rect,
        composed,
        "the composition is untouched"
    );
    assert_eq!(
        editor.release_refusal().as_deref(),
        Some("Changed elsewhere: discard the crop draft or reapply it"),
        "Apply is refused"
    );
    let snapshot = editor.snapshot();
    assert_eq!(snapshot["crop"]["conflicted"], json!(true));
    assert_eq!(
        snapshot["notices"],
        json!(["Changed elsewhere"]),
        "the captured frame records the chrome it drew"
    );
    assert_eq!(snapshot["render_error"], json!(null));
    assert_eq!(snapshot["compare"], json!(false));

    // Reapply re-reads the rows, rebases the frame onto the new input stage and the core draft
    // onto the new revision, sends the rebased frame's fields, all in this update, and asks
    // for the new stage's pixels.
    editor.busy = false;
    let log = crate::app::testing::attach_log(&mut editor);
    draft_message(&mut editor, DraftMessage::Reapply);
    assert_eq!(
        editor.crop_stage(),
        Some(StageView::Rendering {
            reapply: true,
            base_revision: 9
        })
    );
    let draft = core_draft(&editor).expect("the rebased draft");
    assert!(!draft.conflicted && draft.drained());
    assert_eq!(draft.base_revision, 9);
    let payload = editor.crop().expect("a frame").payload();
    let records = crate::app::testing::logged(&mut editor, &log);
    let sets = events(&records, "crop_draft_set");
    assert_eq!(
        sets.len(),
        1,
        "one draft.set, of the rebased frame, follows the rebase: {sets:?}"
    );
    assert_eq!(sets[0]["fields"]["width"], json!(payload.width));
    assert_eq!(
        editor.crop().expect("a frame").stage.angle,
        6.0,
        "the angle survives a rebase"
    );
    assert!(
        editor.release_refusal().is_none(),
        "Apply is possible again"
    );
    finish(editor, catalog);
}

#[test]
fn crop_reapply_after_external_geometry_edit_rebinds_mapping() {
    let catalog = luxforge_testbase::paths::temp_path("crop-warp-reapply.sqlite");
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/geometry/z6-24-70-35mm-grid.jpg");
    let (mut editor, asset, agent) = crate::app::testing::real_photo_at(&catalog, &source);
    let owner = editor.owner.clone();
    let rows = luxforge_testbase::wait_for("the offline lens index", || {
        let response = owner
            .call(
                agent,
                luxforge_core::ApiRequest {
                    id: "crop-lens-query".into(),
                    method: "query.lens-profiles".into(),
                    params: json!({"asset_id":asset,"assume-uncorrected":true}),
                    token: None,
                },
            )
            .unwrap();
        match response.error {
            Some(error) if error.code == "not-ready" => None,
            Some(error) => panic!("{error:?}"),
            None => response.result,
        }
    });
    // The detected profile, offered in the answer's status rather than as a row.
    let row = &rows["status"]["suggestion"];
    assert!(
        row["match"] == "lens-model" && row["eligible"] == true,
        "{rows}"
    );
    crate::app::tasks::call(
        &owner,
        agent,
        "edit.select-lens-profile",
        json!({"asset_id":asset,"profile":row["key"],"assume-uncorrected":true,
            "mutation":{"expected_revision":0,"request_id":"crop-lens","actor":"test"}}),
    )
    .unwrap();
    let refresh = crate::app::tasks::refresh(
        &owner,
        editor.client,
        asset.clone(),
        crate::app::tasks::Scope::Elsewhere,
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(SyncResult::changed(
        refresh,
    )))));
    let _ = editor.update(Message::Crop(CropMessage::Start));
    submit_angle(&mut editor, "2.5");
    let before = editor.crop().unwrap().payload();
    let count = editor.crop().unwrap().layer_index;
    assert_eq!(count, 1);
    let old_job = owner
        .preview_job(luxforge_core::PreviewRequest::new(editor.client, asset.clone()).layers(count))
        .unwrap();
    let old_map = luxforge_core::stage_transform(
        old_job.evaluation.registry(),
        600,
        400,
        old_job.evaluation.recipe(),
    )
    .unwrap();
    assert!(matches!(
        old_map.mapping,
        luxforge_core::MappingShape::Warp { .. }
    ));

    crate::app::tasks::call(
        &owner,
        agent,
        "edit.set-perspective",
        json!({"asset_id":asset,"horizontal":40,"vertical":-25,
            "mutation":{"expected_revision":1,"request_id":"crop-perspective","actor":"agent"}}),
    )
    .unwrap();
    let refresh = crate::app::tasks::refresh(
        &owner,
        editor.client,
        asset.clone(),
        crate::app::tasks::Scope::Elsewhere,
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(SyncResult::changed(
        refresh,
    )))));
    assert!(editor.gesture_conflicted());
    assert_eq!(editor.crop().unwrap().payload(), before);
    editor.busy = false;
    draft_message(&mut editor, DraftMessage::Reapply);
    let draft = editor.crop().unwrap();
    assert_eq!(draft.layer_index, 2);
    assert_eq!((draft.stage.width, draft.stage.height), (600, 400));
    assert_eq!(
        draft.payload(),
        before,
        "fixed canvas retains the frame's composition"
    );
    assert_eq!(core_draft(&editor).unwrap().base_revision, 2);
    assert!(!editor.gesture_conflicted());
    let new_job = owner
        .preview_job(
            luxforge_core::PreviewRequest::new(editor.client, asset.clone())
                .layers(draft.layer_index),
        )
        .unwrap();
    let new_map = luxforge_core::stage_transform(
        new_job.evaluation.registry(),
        600,
        400,
        new_job.evaluation.recipe(),
    )
    .unwrap();
    assert_ne!(new_map.sha256(), old_map.sha256());
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
        StagePlan::Open,
        Ok(Box::new(new_job)),
    )));
    luxforge_testbase::wait_until("the rebound crop stage", || {
        let _ = editor.update(Message::Preview(
            crate::app::message::preview::PreviewMessage::Poll,
        ));
        editor.crop_stage() == Some(StageView::Shown)
    });
    assert_eq!(editor.crop().unwrap().payload(), before);
    draft_message(&mut editor, DraftMessage::Cancel);
    let state = crate::app::tasks::call(&owner, agent, "asset.state", json!({"asset_id":asset}))
        .unwrap()
        .0;
    assert_eq!(
        state["revision"],
        json!(2),
        "reapply and cancellation commit nothing"
    );
    finish(editor, catalog);
}

/// Against a real owner: the open crop draft is this client's core draft, so `session.state`
/// reports it with the payload's declared fields as each change ends; another client can
/// neither see nor commit it; Apply commits it through `draft.commit` as one entry on the crop
/// layer, and Cancel ends it with nothing committed.
#[test]
fn the_crop_draft_is_the_clients_core_draft_in_its_session() {
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-crop-{}-{}.sqlite",
        std::process::id(),
        crate::app::tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let (mut editor, _, agent) = crate::app::testing::real_photo(&catalog);
    let owner = editor.owner.clone();
    let client = editor.client;
    let session = |client| {
        crate::app::tasks::call(&owner, client, "session.state", json!({}))
            .expect("a session")
            .0
    };
    let revision = editor
        .document
        .state
        .as_ref()
        .expect("a photograph")
        .revision;

    let _ = editor.update(Message::Crop(CropMessage::Start));
    step_angle(&mut editor, 1);
    let payload = editor.crop().expect("an open frame").payload();
    let held = session(client);
    assert_eq!(held["draft"]["action"], json!("crop"));
    assert_eq!(held["draft"]["base_revision"], json!(revision));
    assert_eq!(held["draft"]["conflicted"], json!(false));
    assert_eq!(held["draft"]["fields"]["angle"], json!(payload.angle));
    assert_eq!(held["draft"]["fields"]["width"], json!(payload.width));
    assert_eq!(
        held["draft"]["draft_revision"],
        json!(2),
        "the opened frame and the nudge"
    );
    assert_eq!(
        session(agent)["draft"],
        Value::Null,
        "a draft is one client's"
    );
    let foreign = crate::app::tasks::call(
        &owner,
        agent,
        "draft.commit",
        json!({"draft_id": held["draft"]["draft_id"], "mutation": mutation(revision)}),
    );
    assert!(
        foreign.is_err_and(|error| error.contains("unknown draft")),
        "another client cannot commit it"
    );

    draft_message(&mut editor, DraftMessage::Commit);
    assert!(crate::app::testing::run_commit(&mut editor));
    assert!(editor.gesture.is_none() && editor.crop().is_none());
    assert_eq!(session(client)["draft"], Value::Null);
    let state = editor.document.state.as_ref().expect("a photograph");
    assert_eq!(state.revision, revision + 1, "one entry");
    let layer = state
        .current_entry
        .snapshot
        .recipe
        .layers
        .iter()
        .find(|layer| layer.effect_id == luxforge_core::CROP_EFFECT)
        .expect("the crop layer");
    assert_eq!(
        serde_json::from_value::<CropPayload>(layer.payload.clone()).expect("a payload"),
        payload
    );

    // A second draft, opened on the committed crop's row and discarded: nothing commits and
    // the session holds no draft afterwards.
    let _ = editor.update(Message::Crop(CropMessage::Start));
    assert_eq!(
        editor.crop().expect("a second frame").payload(),
        payload,
        "the frame reopens at what was committed"
    );
    assert_eq!(session(client)["draft"]["action"], json!("crop"));
    draft_message(&mut editor, DraftMessage::Cancel);
    assert!(editor.gesture.is_none());
    assert_eq!(session(client)["draft"], Value::Null);
    assert_eq!(
        editor
            .document
            .state
            .as_ref()
            .expect("a photograph")
            .revision,
        revision + 1
    );
    finish(editor, catalog);
}

/// A crop's input stage is everything ahead of it, the transforms included: a draft on a stack
/// turned after it was cropped opens on the turned stage the crop's row reports, after the
/// orientation layer the host keeps ahead of the crop, and remembers the turn the row says it
/// opened behind.
#[test]
fn a_draft_opens_behind_every_transform_ahead_of_its_crop() {
    let right = Orientation::NEUTRAL.then(luxforge_core::Transform::RotateRight);
    let crop = crop_layer(CropPayload {
        angle: 0.0,
        x: 0.25,
        y: 0.0,
        width: 0.5,
        height: 1.0,
    });
    let (mut editor, catalog, _, _) = opened(
        vec![
            luxforge_core::Layer::pixel(0, 0, [1, 2, 3]),
            luxforge_core::Layer::orientation(right),
            crop.clone(),
        ],
        3,
    );
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let draft = editor.crop().expect("an opened draft");
    assert_eq!(draft.layer, Some(crop.id));
    assert_eq!(
        draft.layer_index, 2,
        "the pixel edit and the turn are shown"
    );
    assert_eq!(draft.ahead, right);
    assert_eq!(
        (draft.stage.width, draft.stage.height),
        (CROP_SOURCE.1, CROP_SOURCE.0)
    );
    finish(editor, catalog);
}

/// Without a crop layer the draft shows the stage the host would give a new one, which is
/// before any finish layer: a post-crop effect acts on the crop's output, not on its input.
#[test]
fn a_first_draft_stops_before_a_finish_layer() {
    let finish_layer = luxforge_core::Layer {
        id: LayerId::new(),
        effect_id: luxforge_core::VIGNETTE_EFFECT.into(),
        effect_format: 1,
        payload: json!({"amount": -40}),
        artifacts: Vec::new(),
        mask: None,
    };
    let (mut editor, catalog, _, _) = opened(
        vec![luxforge_core::Layer::pixel(0, 0, [1, 2, 3]), finish_layer],
        2,
    );
    let finishing = luxforge_core::ModuleDescriptor {
        id: "luxforge.vignette".into(),
        effects: vec![luxforge_core::EffectDescriptor {
            id: luxforge_core::VIGNETTE_EFFECT.into(),
            format: 1,
            stage: luxforge_core::EffectStage::Finish,
            order: 0,
            maskable: false,
            artifacts: false,
            single: false,
            sources: Vec::new(),
        }],
        ..luxforge_core::ModuleDescriptor::default()
    };
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(vec![
        crate::app::testing::crop_descriptor(),
        finishing,
    ]))));
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let draft = editor.crop().expect("an opened draft");
    assert_eq!(draft.layer, None);
    assert_eq!(
        draft.layer_index, 1,
        "the vignette is not part of the input"
    );
    finish(editor, catalog);
}

/// A turn committed while drafting, here as another client would commit it, goes ahead of the
/// crop and turns its input stage. Reapply carries the draft through the turn the rows report,
/// so the frame keeps selecting what it did instead of landing on whatever the same box numbers
/// now show.
#[test]
fn reapply_after_a_turn_carries_the_draft_with_the_photograph() {
    let crop = crop_layer(CropPayload {
        angle: 0.0,
        x: 0.0,
        y: 0.0,
        width: 0.5,
        height: 0.5,
    });
    let (mut editor, catalog, asset, _) = opened(vec![crop.clone()], 4);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    let before = editor.crop().expect("a draft").rect;
    assert_eq!(
        (before.x, before.y, before.width, before.height),
        (0.0, 0.0, 240.0, 160.0)
    );

    let right = Orientation::NEUTRAL.then(luxforge_core::Transform::RotateRight);
    let mut newer = entry(&asset, 5, None);
    for layer in [
        luxforge_core::Layer::orientation(right),
        luxforge_core::Layer {
            payload: serde_json::to_value(
                serde_json::from_value::<CropPayload>(crop.payload.clone())
                    .unwrap()
                    .carried(CROP_SOURCE, right)
                    .unwrap(),
            )
            .unwrap(),
            ..crop.clone()
        },
    ] {
        newer.snapshot = newer.snapshot.append(layer).expect("a valid stack");
    }
    committed_elsewhere(&mut editor, &asset, &newer);
    assert!(core_draft(&editor).expect("the draft is kept").conflicted);

    editor.busy = false;
    draft_message(&mut editor, DraftMessage::Reapply);
    let draft = editor.crop().expect("the rebased frame");
    assert_eq!((draft.layer_index, draft.ahead), (1, right));
    // The top-left quarter of the photograph, turned clockwise, is its top-right quarter.
    assert_eq!(
        (
            draft.rect.x,
            draft.rect.y,
            draft.rect.width,
            draft.rect.height
        ),
        (160.0, 0.0, 160.0, 240.0)
    );
    assert!(!core_draft(&editor).expect("the rebased draft").conflicted);
    finish(editor, catalog);
}

#[test]
fn the_drafts_own_apply_ends_it_and_a_failed_apply_keeps_it() {
    let (mut editor, catalog, asset, _) = opened(Vec::new(), 2);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    // A stale revision comes back as a conflict: the draft is kept and marked.
    draft_message(&mut editor, DraftMessage::Commit);
    answer_commit(&mut editor, Err("conflict: stale revision".into()));
    assert!(editor.crop().is_some(), "the draft is kept");
    assert!(core_draft(&editor).expect("the draft is kept").conflicted);
    assert!(editor.release_refusal().is_some());

    // The draft's own successful Apply ends it and drops the extra texture.
    editor.core_gesture_mut().expect("a draft").draft.conflicted = false;
    draft_message(&mut editor, DraftMessage::Commit);
    let newer = entry(&asset, 5, None);
    let refresh = refresh_for(&asset, &newer, vec![newer.clone()], &[&newer], false);
    answer_commit(&mut editor, Ok(Some(refresh)));
    assert!(editor.crop().is_none() && editor.gesture.is_none());
    assert!(editor.session.draft.is_none());
    assert!(editor.presentation.presenter.stage().is_none());
    assert!(
        editor.status.text.contains("Crop applied"),
        "{}",
        editor.status.text
    );
    finish(editor, catalog);
}

#[test]
fn starting_or_ending_a_draft_by_any_route_asks_the_session_to_follow_it() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 1);
    let crop_id = crop_frame(&editor.modules)
        .expect("a declared crop frame")
        .module
        .id
        .to_owned();

    // The section's own "Crop & straighten" button, `R` and a scripted `draft.start` all send
    // this message directly, never through `ViewMessage::SetMode`; starting still asks the session
    // to enter the crop mode.
    let _ = editor.dispatch(Message::Crop(CropMessage::Start));
    assert_eq!(
        editor.sync.mode.as_deref(),
        Some(crop_id.as_str()),
        "starting a draft by any route queues the session's own mode change"
    );
    // The public entry point folds that into the returned task and consumes the flag.
    editor.session.workspace.mode = crop_id.clone();
    let _ = editor.update(Message::Pointer(PointerMessage::Moved(None)));
    assert_eq!(editor.sync.mode, None, "the wrapper always consumes it");

    open_crop(&mut editor);
    assert!(editor.workspace.canvas.modes[0].id == POINTER_MODE);
    assert!(
        !editor.workspace.canvas.modes[0].selected,
        "pointer is not selected while the session reports the crop mode"
    );

    // Ending it, by Cancel here (Apply and a scripted cancel go through the same `end_crop_view`),
    // returns the session to pointer.
    let _ = editor.dispatch(Message::Draft(DraftMessage::Cancel));
    assert_eq!(
        editor.sync.mode.as_deref(),
        Some(POINTER_MODE),
        "ending a draft by any route queues the session's return to pointer"
    );
    finish(editor, catalog);
}

/// The mode strip's Crop entry enters the crop mode only by starting the draft: a start that is
/// refused sends no `workspace.set`, so the session never reports a crop mode the desktop is not
/// in. A start that goes ahead asks for the mode once, through the update's own catch-up.
#[test]
fn a_refused_crop_start_sends_no_workspace_change() {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 1);
    let crop_id = crop_frame(&editor.modules)
        .expect("a declared crop frame")
        .module
        .id
        .to_owned();
    let mode = editor.session.workspace.mode.clone();
    let refused = |editor: &mut Editor, case: &str| {
        let task = editor.dispatch(Message::View(ViewMessage::SetMode(crop_id.clone())));
        assert_eq!(task.units(), 0, "{case}: nothing is sent");
        assert_eq!(editor.sync.mode, None, "{case}: no mode change is queued");
        assert!(editor.crop().is_none());
        assert_eq!(editor.session.workspace.mode, mode, "{case}");
    };

    // A historical preview cannot be edited.
    let current = editor.shown_selection();
    crate::state::testing::show(
        &mut editor.session,
        editor.document.state.as_ref(),
        HistorySelection::Entry(entry_id),
    );
    refused(&mut editor, "a historical preview");
    crate::state::testing::show(&mut editor.session, editor.document.state.as_ref(), current);

    // Nothing refuses it: the draft starts and asks the session for the mode once.
    let task = editor.dispatch(Message::View(ViewMessage::SetMode(crop_id.clone())));
    assert_eq!(
        task.units(),
        2,
        "the input stage's preview and the panel scroll: the draft's begin answered in this update"
    );
    assert!(editor.crop().is_some());
    assert_eq!(editor.sync.mode.as_deref(), Some(crop_id.as_str()));
    finish(editor, catalog);
}

#[test]
fn a_history_preview_pauses_the_draft_without_discarding_it() {
    let (mut editor, catalog, _, entry_id) = opened(Vec::new(), 1);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    let composed = editor.crop().expect("a draft").rect;
    let mut session = ClientSession {
        revision: 3,
        ..ClientSession::default()
    };
    crate::state::testing::show(
        &mut session,
        editor.document.state.as_ref(),
        HistorySelection::Entry(entry_id),
    );
    let _ = editor.update(Message::View(ViewMessage::SessionUpdated(Ok(session))));
    assert!(!editor.at_current());
    assert!(
        editor.crop().is_some(),
        "selecting a historical state keeps the draft"
    );
    assert!(!editor.drafting(), "the plain historical preview is shown");
    assert_eq!(editor.snapshot()["crop"]["paused"], json!(true));
    // Nothing can be applied or started while previewing history.
    draft_message(&mut editor, DraftMessage::Commit);
    assert!(editor.crop().is_some());
    assert_eq!(editor.status.text, "Return to the current state to apply");
    assert_eq!(
        core_draft(&editor).and_then(CoreDraft::in_flight),
        None,
        "nothing was committed"
    );
    assert_eq!(editor.crop().expect("a draft").rect, composed);
    finish(editor, catalog);
}

/// Apply reads enabled exactly when the app would commit: the draft bar and the crop section
/// both show the app's one release refusal, whatever it is — none, another request in flight,
/// a historical preview on screen or a conflicted draft — so neither can read enabled while
/// Enter or the button's own press would be refused.
#[test]
fn apply_reads_enabled_exactly_when_the_app_would_commit() {
    let (mut editor, catalog, asset, entry_id) = opened(Vec::new(), 1);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    open_crop(&mut editor);
    let agree = |editor: &mut Editor, case: &str| -> Option<String> {
        let _ = editor.update(Message::Pointer(PointerMessage::Moved(None)));
        let refusal = editor.release_refusal();
        let bar = editor
            .workspace
            .canvas
            .draft_bar
            .clone()
            .expect("the draft bar");
        assert_eq!(bar.apply_reason, refusal, "{case}: the bar's reason");
        assert_eq!(bar.can_apply, refusal.is_none(), "{case}: the bar's Apply");
        let section = editor
            .workspace
            .tools
            .all()
            .flat_map(|section| section.controls.iter())
            .find_map(|control| match control {
                crate::state::tools::ControlModel::CropFrame(model) => Some(model.can_apply),
                _ => None,
            })
            .expect("the crop section");
        assert_eq!(section, refusal.is_none(), "{case}: the section's Apply");
        refusal
    };
    assert_eq!(agree(&mut editor, "a fresh draft"), None);

    editor.busy = true;
    assert_eq!(
        agree(&mut editor, "a request in flight").as_deref(),
        Some("Waiting for the last request")
    );
    draft_message(&mut editor, DraftMessage::Commit);
    assert_eq!(
        core_draft(&editor).and_then(CoreDraft::in_flight),
        None,
        "the refused Apply sent nothing"
    );
    editor.busy = false;

    let current = editor.shown_selection();
    crate::state::testing::show(
        &mut editor.session,
        editor.document.state.as_ref(),
        HistorySelection::Entry(entry_id),
    );
    assert_eq!(
        agree(&mut editor, "a historical preview").as_deref(),
        Some("Return to the current state to apply")
    );
    crate::state::testing::show(&mut editor.session, editor.document.state.as_ref(), current);

    committed_elsewhere(&mut editor, &asset, &entry(&asset, 4, None));
    assert_eq!(
        agree(&mut editor, "a conflicted draft").as_deref(),
        Some("Changed elsewhere: discard the crop draft or reapply it")
    );
    finish(editor, catalog);
}

/// The committed 16:9 crop the idle tests start from, fitted on [`stage`] by the core's own
/// geometry exactly as `crop-fit` or the 16:9 chip would fit it.
fn committed_wide() -> CropPayload {
    let stage = stage();
    let whole = luxforge_core::BoxRect {
        x: 0.0,
        y: 0.0,
        width: 480.0,
        height: 320.0,
    };
    stage
        .fit_about_center(luxforge_core::largest_with_ratio_inside(whole, 16.0 / 9.0))
        .normalized(&stage)
}

fn option(option: &str) -> usize {
    CROP_ASPECTS
        .iter()
        .position(|candidate| *candidate == option)
        .expect("a declared option")
}

/// A chip pressed in the idle section opens the draft seeded from the committed crop, as Start
/// does, and applies itself at once: the canvas enters crop mode with the frame at the new
/// ratio, the log shows the seeded draft and then the one change, and nothing commits.
#[test]
fn an_idle_change_opens_the_draft_seeded_from_the_committed_crop_and_applies_it() {
    let crop = crop_layer(committed_wide());
    let (mut editor, catalog, _, _) = opened(vec![crop.clone()], 5);
    let log = crate::app::testing::attach_log(&mut editor);
    let _ = editor.dispatch(Message::Crop(CropMessage::Preset(option("1:1"))));
    let draft = editor.crop().expect("the change opened a draft");
    assert_eq!(draft.layer, Some(crop.id));
    assert_eq!(draft.preset, "1:1");
    assert_eq!(draft.aspect.ratio(), Some(1.0));
    assert_eq!(draft.rect.width, draft.rect.height);
    assert_eq!(
        editor.sync.mode.as_deref(),
        Some("luxforge.crop"),
        "the canvas enters crop mode"
    );
    assert_eq!(
        editor.document.state.as_ref().expect("a state").revision,
        5,
        "nothing commits until Apply"
    );
    let records = crate::app::testing::logged(&mut editor, &log);
    let started = events(&records, "crop_draft_started");
    assert_eq!(started.len(), 1);
    assert_eq!(
        started[0]["preset"],
        json!("16:9"),
        "the draft seeds the ratio the committed crop reads as"
    );
    let changed = events(&records, "crop_draft_changed");
    assert_eq!(changed.len(), 1, "the one idle change: {changed:?}");
    assert_eq!(changed[0]["preset"], json!("1:1"));
    finish(editor, catalog);
}

/// The draft is live before its stage's pixels arrive: changes made then — a ratio, a nudge and
/// a rail drag — apply to the frame at once, each one draft change, with no queue between
/// them and the frame; the stage then arrives under the frame they made.
#[test]
fn a_change_made_before_the_stage_arrives_applies_at_once() {
    let (mut editor, catalog, _, _) = opened(vec![crop_layer(committed_wide())], 5);
    let log = crate::app::testing::attach_log(&mut editor);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    assert_eq!(section_angle(&editor), json!("0.0"), "the committed angle");
    let _ = editor.update(Message::Crop(CropMessage::Preset(option("4:3"))));
    step_angle(&mut editor, 1);
    assert_eq!(editor.crop().expect("an open frame").stage.angle, 0.5);
    assert_eq!(section_angle(&editor), json!("0.5"));
    drag_angle(&mut editor, &[0.52, 0.55, 0.5 + 2.4 / 90.0 + 1e-4]);
    release_angle(&mut editor);
    let draft = editor.crop().expect("an open frame");
    assert_eq!(draft.preset, "4:3");
    assert_eq!(draft.stage.angle, 2.4, "the rail's last position");
    assert_eq!(section_angle(&editor), json!("2.4"));
    assert!(
        matches!(editor.crop_stage(), Some(StageView::Rendering { .. })),
        "the stage is still on its way"
    );
    open_crop(&mut editor);
    assert_eq!(editor.crop().expect("an open frame").stage.angle, 2.4);
    assert_eq!(editor.document.state.as_ref().expect("a state").revision, 5);
    let records = crate::app::testing::logged(&mut editor, &log);
    assert_eq!(
        events(&records, "crop_draft_changed").len(),
        3,
        "the ratio, the nudge and the rail's release"
    );
    finish(editor, catalog);
}

/// Cancel on a draft whose stage has not yet rendered cancels it outright: the frame leaves,
/// the stage's job is stopped, the session returns to pointer, and the core draft is cancelled,
/// all in the update of the press.
#[test]
fn cancel_while_the_stage_is_rendering_cancels_it_outright() {
    let (mut editor, catalog, _, _) = opened(Vec::new(), 2);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    assert!(matches!(
        editor.crop_stage(),
        Some(StageView::Rendering { .. })
    ));
    let _ = editor.dispatch(Message::Draft(DraftMessage::Cancel));
    assert!(editor.crop().is_none(), "the frame left at once");
    assert!(editor.gesture.is_none(), "and its core draft with it");
    assert_eq!(editor.draft_generation(), None);
    assert_eq!(editor.sync.mode.as_deref(), Some(POINTER_MODE));
    assert_eq!(editor.status.text, "Crop draft discarded");
    assert_eq!(editor.document.state.as_ref().expect("a state").revision, 2);
    finish(editor, catalog);
}

/// What an idle control does that is not a change: the committed angle submitted again only
/// closes the box, text that is not a number stays open with its reason, a rail release with no
/// drag does nothing, and pressing the chip already chosen opens the draft without refitting
/// the committed rectangle to it. An open slider gesture refuses the draft, and a start whose
/// input stage fails ends with the change it made.
#[test]
fn an_idle_control_that_changes_nothing_opens_no_draft_or_leaves_the_crop_as_it_is() {
    let (mut editor, catalog, _, _) = opened(vec![crop_layer(committed_wide())], 5);
    let frame = crop_frame(&editor.modules).expect("a crop frame");
    let key = (frame.action.to_owned(), frame.angle.to_owned());
    editor.controls.fields.set(&key.0, &key.1, "31".into());
    let _ = editor.update(Message::Control(ControlMessage::EditValue {
        action: key.0.clone(),
        parameter: key.1.clone(),
    }));
    assert_eq!(
        editor.controls.fields.get(&key.0, &key.1),
        Some("0.0"),
        "the idle box opens at the committed angle"
    );
    let _ = editor.update(Message::Control(ControlMessage::Submit {
        action: key.0.clone(),
        parameter: Some(key.1.clone()),
    }));
    assert!(editor.gesture.is_none() && !editor.editing_angle());
    let _ = editor.update(Message::Control(ControlMessage::EditValue {
        action: key.0.clone(),
        parameter: key.1.clone(),
    }));
    submit_angle(&mut editor, "level");
    assert!(editor.gesture.is_none() && editor.editing_angle());
    assert!(
        editor.status.text.contains("angle must be a number"),
        "{}",
        editor.status.text
    );
    // A release with no drag, and a double-click reset of an angle already at its default of
    // 0, change nothing, so nothing opens: the reset needs no case of its own.
    release_angle(&mut editor);
    let _ = editor.update(Message::Control(ControlMessage::ResetField {
        action: key.0,
        parameter: key.1,
    }));
    let _ = editor.update(Message::Crop(CropMessage::Guide(false)));
    assert!(editor.gesture.is_none());

    // A slider gesture is finished deliberately, never displaced by the crop draft.
    crate::app::testing::hold_slider(&mut editor, "set-basic", "exposure");
    let _ = editor.update(Message::Crop(CropMessage::Lock));
    assert!(editor.crop().is_none());
    assert!(
        editor.status.text.contains("slider draft"),
        "{}",
        editor.status.text
    );
    editor.gesture = None;

    // A start whose input stage cannot be prepared ends, taking the change it made with it.
    let _ = editor.update(Message::Crop(CropMessage::Swap));
    assert!(editor.crop().is_some(), "the swap opened a draft");
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
        StagePlan::Open,
        Err("the source is gone".into()),
    )));
    assert!(editor.crop().is_none(), "nothing is left open");
    assert_eq!(editor.status.text, "the source is gone");
    assert!(
        editor.gesture.is_none(),
        "its core draft is cancelled in the same update"
    );

    // The chip already chosen opens the draft and leaves the committed rectangle exactly.
    let _ = editor.update(Message::Crop(CropMessage::Preset(option("16:9"))));
    let draft = editor.crop().expect("an opened draft");
    assert_eq!(draft.preset, "16:9");
    assert_eq!(draft.payload(), committed_wide());
    finish(editor, catalog);
}

/// At Fit a crop draft's input stage the GPU does not draw is the reference's frame of the layer
/// prefix reduced to the display bounds, one frame with no proxy phase. A percentage zoom that
/// draws the stage at its own size asks for the exact stage once, and a zoom back to Fit hands the
/// held reduced frame over again without a render, as the photograph's frames behave.
#[test]
fn the_input_stage_is_reduced_at_fit_and_exact_only_at_a_percentage_zoom() {
    use luxforge_core::Zoom;
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-crop-stage-proxy-{}-{}.sqlite",
        std::process::id(),
        crate::app::tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let (mut editor, asset, _) = crate::app::testing::real_photo(&catalog);
    // A photo surface smaller than the 480 × 320 photograph, so Fit draws it smaller than it is.
    editor.session.workspace.state_panel = false;
    editor.session.workspace.tools_panel = false;
    editor.view_state.window = (360.0, 300.0);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let count = editor.crop().expect("a frame").layer_index;
    let input = editor.crop().expect("a frame").stage;
    let job = editor
        .owner
        .preview_job(luxforge_core::PreviewRequest::new(editor.client, asset.clone()).layers(count))
        .expect("the input stage's job");
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
        StagePlan::Open,
        Ok(Box::new(job)),
    )));
    let shown = |editor: &mut Editor, phase: &str| -> Value {
        luxforge_testbase::wait_until(&format!("the {phase} stage"), || {
            let _ = editor.update(Message::Preview(
                crate::app::message::preview::PreviewMessage::Poll,
            ));
            editor.snapshot()["crop"]["input_stage_frame"]["phase"] == json!(phase)
        });
        editor.snapshot()["crop"]["input_stage_frame"].clone()
    };

    let fit = shown(&mut editor, "reduced");
    assert_eq!(editor.crop_stage(), Some(StageView::Shown));
    let bounds = editor.crop_stage_bounds().expect("Fit bounds the stage");
    let (width, height) = (
        fit["size"][0].as_u64().unwrap() as u32,
        fit["size"][1].as_u64().unwrap() as u32,
    );
    assert!(
        width < input.width && height < input.height,
        "a {width}x{height} proxy of the {}x{} stage",
        input.width,
        input.height
    );
    assert!(width <= bounds.width && height <= bounds.height);
    assert_eq!(fit["held_exact"], json!(false), "no exact phase at Fit");
    assert_eq!(
        editor.draft_generation(),
        None,
        "nothing more is on its way"
    );

    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    let plan = editor.zoom_changed(&Zoom::Fit);
    assert_eq!(plan.units(), 1, "the stage is planned again");
    let _ = editor.update(stage_replanned(&editor, &asset, count).0);
    assert!(
        editor.draft_generation().is_some(),
        "the exact stage is asked for"
    );
    let exact = shown(&mut editor, "exact");
    assert_eq!(exact["size"], json!([input.width, input.height]));
    assert_eq!(editor.draft_generation(), None);

    editor.session.preview.view.zoom = Zoom::Fit;
    let plan = editor.zoom_changed(&Zoom::Percent { value: 100.0 });
    assert_eq!(plan.units(), 0, "nothing is planned");
    let back = editor.snapshot()["crop"]["input_stage_frame"].clone();
    assert_eq!(back["phase"], json!("reduced"), "the held proxy, at once");
    assert_eq!(back["size"], fit["size"]);
    assert_eq!(editor.draft_generation(), None, "nothing is rendered");
    finish(editor, catalog);
}

/// The owner task's answer to the plan a zoom asked for ([`Editor::present_crop_stage`]): the
/// stage on screen planned again from its entry, over a new allocation of its pixels, and a
/// handle that says whether anything still holds them.
fn stage_replanned(
    editor: &Editor,
    asset: &luxforge_core::AssetId,
    count: usize,
) -> (Message, std::sync::Weak<Vec<u8>>) {
    let crop = editor.crop_gesture().expect("a draft");
    let entry = crop
        .frames
        .planned
        .as_ref()
        .expect("a planned stage")
        .entry_id
        .clone();
    let job = crate::app::tasks::crop_preview(
        &editor.owner,
        editor.client,
        asset.clone(),
        Some(entry.clone()),
        count,
        editor.crop_stage_ask(),
    )
    .expect("the stage planned again");
    let (evaluation, pixels) = crate::app::testing::fresh_stack(&job.evaluation);
    let job = luxforge_core::PreviewJob { evaluation, ..job };
    (
        Message::Crop(CropMessage::PreviewReady(
            StagePlan::Zoom(entry),
            Ok(Box::new(job)),
        )),
        pixels,
    )
}

/// An open crop draft holds frames only. Once its input stage is delivered nothing of the
/// planned stack is left on the desktop: a RAW development's planes would hold the source
/// worker's memory gate, so a development the owner needs would wait on the draft. A zoom that
/// needs a phase no held frame serves plans the stage again, once, from the entry it was
/// planned from; its answer is requested only while the view still wants that phase, and an
/// answer to no plan the draft is waiting for is dropped.
#[test]
fn an_open_crop_draft_keeps_no_stack_once_its_stage_is_delivered() {
    use crate::app::message::preview::PreviewMessage;
    use luxforge_core::Zoom;
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-crop-stage-stack-{}-{}.sqlite",
        std::process::id(),
        crate::app::tasks::REQUEST_NUMBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let (mut editor, asset, _) = crate::app::testing::real_photo(&catalog);
    editor.session.workspace.state_panel = false;
    editor.session.workspace.tools_panel = false;
    editor.view_state.window = (360.0, 300.0);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let count = editor.crop().expect("a frame").layer_index;
    let job = editor
        .owner
        .preview_job(luxforge_core::PreviewRequest::new(editor.client, asset.clone()).layers(count))
        .expect("the input stage's job");
    let (evaluation, pixels) = crate::app::testing::fresh_stack(&job.evaluation);
    let job = Box::new(luxforge_core::PreviewJob { evaluation, ..job });
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
        StagePlan::Open,
        Ok(job),
    )));
    let phase = |editor: &Editor| editor.snapshot()["crop"]["input_stage_frame"]["phase"].clone();
    let settled = |editor: &mut Editor, wanted: &str| {
        luxforge_testbase::wait_until(&format!("the {wanted} stage"), || {
            let _ = editor.update(Message::Preview(PreviewMessage::Poll));
            phase(editor) == json!(wanted) && !editor.presentation.queue.is_busy()
        });
    };
    settled(&mut editor, "reduced");
    assert_eq!(
        pixels.strong_count(),
        0,
        "the draft keeps its stage's frame, not its stack"
    );

    // A zoom that needs the exact stage plans it again, once, and asks for no render yet.
    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    assert_eq!(editor.zoom_changed(&Zoom::Fit).units(), 1, "one plan");
    assert!(editor.crop_gesture().expect("a draft").frames.replanning);
    editor.session.preview.view.zoom = Zoom::Percent { value: 200.0 };
    let again = editor.zoom_changed(&Zoom::Percent { value: 100.0 });
    assert_eq!(again.units(), 0, "a plan is already on its way");
    assert_eq!(editor.draft_generation(), None);

    // The zoom moved back to Fit before the answer: the held proxy serves, and the answer is
    // dropped without a render.
    editor.session.preview.view.zoom = Zoom::Fit;
    assert_eq!(
        editor.zoom_changed(&Zoom::Percent { value: 200.0 }).units(),
        0
    );
    let (answer, pixels) = stage_replanned(&editor, &asset, count);
    let _ = editor.update(answer);
    assert_eq!(editor.draft_generation(), None, "nothing is rendered");
    assert!(!editor.crop_gesture().expect("a draft").frames.replanning);
    assert_eq!(phase(&editor), json!("reduced"));
    assert_eq!(pixels.strong_count(), 0, "a dropped answer is not kept");

    // An answer to another entry's plan is not this draft's; one for this entry that is not
    // the stage on screen is said and not rendered.
    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    assert_eq!(editor.zoom_changed(&Zoom::Fit).units(), 1);
    let (answer, _) = stage_replanned(&editor, &asset, count);
    let Message::Crop(CropMessage::PreviewReady(StagePlan::Zoom(entry), Ok(job))) = answer else {
        unreachable!()
    };
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
        StagePlan::Zoom(luxforge_core::EntryId::new()),
        Ok(job.clone()),
    )));
    assert!(
        editor.crop_gesture().expect("a draft").frames.replanning,
        "still waiting for its own plan"
    );
    let other = luxforge_core::PreviewJob {
        layer_count: Some(count + 1),
        ..*job
    };
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
        StagePlan::Zoom(entry),
        Ok(Box::new(other)),
    )));
    assert_eq!(editor.draft_generation(), None, "nothing is rendered");
    assert!(!editor.crop_gesture().expect("a draft").frames.replanning);
    assert_eq!(
        editor.status.text,
        "The crop's input stage planned again is not the one on screen"
    );

    // The next zoom plans it again, and its answer is requested as a start's is and shown.
    editor.session.preview.view.zoom = Zoom::Percent { value: 200.0 };
    assert_eq!(
        editor.zoom_changed(&Zoom::Percent { value: 100.0 }).units(),
        1
    );
    let (answer, pixels) = stage_replanned(&editor, &asset, count);
    let _ = editor.update(answer);
    assert!(
        editor.draft_generation().is_some(),
        "the exact stage is asked for"
    );
    settled(&mut editor, "exact");
    // The worker releases the stack with its job. With no layer ahead of the crop the exact
    // stage is the source's own pixels, shared by the frame the draft holds and the one on
    // screen, and by nothing else; a frame holds no plane lease.
    let exact = editor.crop_gesture().expect("a draft").frames.exact.clone();
    let exact = exact.expect("the exact stage is held");
    let shared = usize::from(std::ptr::eq(
        std::sync::Arc::as_ptr(&exact.rgba),
        pixels.as_ptr(),
    ));
    drop(exact);
    assert_eq!(pixels.strong_count(), 2 * shared, "only the frames hold it");

    // Once the draft has ended its frames are gone, and an answer finds nothing to show.
    editor.session.preview.view.zoom = Zoom::Fit;
    assert_eq!(
        editor.zoom_changed(&Zoom::Percent { value: 200.0 }).units(),
        0
    );
    let (answer, dropped) = stage_replanned(&editor, &asset, count);
    draft_message(&mut editor, DraftMessage::Cancel);
    assert!(editor.crop_gesture().is_none());
    assert_eq!(pixels.strong_count(), 0, "the frames end with the draft");
    let _ = editor.update(answer);
    assert_eq!(editor.draft_generation(), None, "nothing is rendered");
    assert!(editor.presentation.presenter.stage().is_none());
    assert_eq!(dropped.strong_count(), 0, "a dropped answer is not kept");
    finish(editor, catalog);
}

/// With the real owner and a real RAW, while a crop draft is open over its input stage — the
/// proxy at Fit, the exact stage planned again for 100%, and the proxy again at Fit — another
/// client's white balance and another photograph each prepare their development within a
/// bounded wait. Each runs on a thread of its own, so a development that waits on planes the
/// draft holds fails the test instead of hanging it.
///
/// `LUXFORGE_RAW_FIXTURE=/path/to/file.NEF cargo test --release -p luxforge-app --bin luxforge \
///   a_raw_develops_again_while_a_crop_draft_is_open -- --ignored --nocapture`
#[test]
#[ignore = "requires a private RAW fixture: set LUXFORGE_RAW_FIXTURE"]
fn a_raw_develops_again_while_a_crop_draft_is_open() {
    use crate::app::{
        message::preview::PreviewMessage,
        tasks::{self, Scope},
    };
    use luxforge_core::Zoom;
    use std::time::{Duration, Instant};
    /// Far above a release development of any supported camera, far below a hang.
    const DEADLINE: Duration = Duration::from_secs(60);
    let raw = std::path::PathBuf::from(
        std::env::var("LUXFORGE_RAW_FIXTURE").expect("LUXFORGE_RAW_FIXTURE"),
    );
    let catalog =
        std::env::temp_dir().join(format!("luxforge-crop-gate-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&catalog);
    let (mut editor, asset, other) = crate::app::testing::real_photo_at(&catalog, &raw);
    let (owner, client) = (editor.owner.clone(), editor.client);
    fn bounded<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
        let started = Instant::now();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        let done = receiver
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|_| panic!("{what}: the development did not finish in {DEADLINE:?}"));
        eprintln!("{what}: {:?}", started.elapsed());
        done
    }
    let phase = |editor: &Editor| editor.snapshot()["crop"]["input_stage_frame"]["phase"].clone();
    let settled = |editor: &mut Editor, wanted: &str| {
        luxforge_testbase::wait_until(&format!("the {wanted} stage"), || {
            let _ = editor.update(Message::Preview(PreviewMessage::Poll));
            phase(editor) == json!(wanted) && !editor.presentation.queue.is_busy()
        });
    };
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let count = editor.crop().expect("a frame").layer_index;
    let plan = |entry: Option<luxforge_core::EntryId>| {
        let (owner, asset) = (owner.clone(), asset.clone());
        bounded("the input stage's plan", move || {
            tasks::crop_preview(&owner, client, asset, entry, count, Default::default())
        })
        .map(Box::new)
    };
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
        StagePlan::Open,
        plan(None),
    )));
    settled(&mut editor, "proxy");
    editor.session.preview.view.zoom = Zoom::Percent { value: 100.0 };
    assert_eq!(editor.zoom_changed(&Zoom::Fit).units(), 1, "planned again");
    let entry = editor
        .crop_gesture()
        .expect("a draft")
        .frames
        .planned
        .as_ref();
    let entry = entry.expect("a planned stage").entry_id.clone();
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
        StagePlan::Zoom(entry.clone()),
        plan(Some(entry)),
    )));
    settled(&mut editor, "exact");
    editor.session.preview.view.zoom = Zoom::Fit;
    assert_eq!(
        editor.zoom_changed(&Zoom::Percent { value: 100.0 }).units(),
        0
    );
    assert_eq!(phase(&editor), json!("proxy"));

    // Another client's white balance, read back as the desktop reads it: its refresh plans the
    // new entry's preview and waits for the development it needs.
    let revision = editor
        .document
        .state
        .as_ref()
        .expect("an open photo")
        .revision;
    tasks::call(
        &owner,
        other,
        "edit.set-raw",
        json!({"asset_id": asset, "temperature": 3500.0,
               "mutation": tasks::mutation(revision)}),
    )
    .expect("another client's white balance");
    let refresh = {
        let (owner, asset) = (owner.clone(), asset.clone());
        bounded("another client's white balance", move || {
            tasks::refresh(&owner, client, asset, Scope::Elsewhere, None)
        })
        .expect("the refresh")
    };
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(refresh)))));
    assert!(editor.crop().is_some(), "the draft is kept");

    // Another photograph: its original's preparation retains no development of this one.
    let jpeg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0/orientation-1.jpg");
    let (_, job) = crate::app::testing::develop_and_prepare(&owner, client, &jpeg);
    let import_owner = owner.clone();
    bounded("another photograph", move || {
        tasks::wait_source_job(&import_owner, client, &job)
    })
    .expect("another photograph prepares");
    assert!(editor.crop().is_some(), "the draft is still open");
    finish(editor, catalog);
}

/// A crop draft's input stage asked for with the GPU is planned with its prefix's picture at rest:
/// the layers before the crop, from the source, their tiles reduced to the stage's display bounds,
/// covering the prefix's whole output stage. A prefix the GPU cannot draw — a developer's pixel
/// replacement — names why in the plan and its tiles, and the job's own frame is the reference's.
#[test]
fn a_crop_drafts_input_stage_is_planned_as_its_prefixs_picture_at_rest() {
    use luxforge_core::{GpuAnswer, GpuFallback, PreviewRequest, ProxyBounds};
    use luxforge_testkit::client::{call, mutation, request_id, revision};
    let catalog = luxforge_testbase::paths::temp_path("crop-stage-rest.sqlite");
    let (owner, join) = luxforge_core::OwnerHandle::start_with_host(
        &catalog,
        std::sync::Arc::new(luxforge_core::ModuleRegistry::developer()),
        luxforge_core::HostConfig::unconfigured(),
    )
    .unwrap();
    let client = owner.register();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0/orientation-1.jpg");
    let asset = crate::app::testing::import_and_adopt(&owner, client, &fixture);
    let id = json!(asset.as_str());
    let commit = |method: &str, mut params: Value| {
        params["asset_id"] = id.clone();
        params["mutation"] = mutation(
            revision(&owner, client, &id).unwrap(),
            &request_id("crop-stage"),
            "agent",
        );
        call(&owner, client, method, params).expect("the edit");
    };
    commit("edit.set-basic", json!({"exposure": 0.4}));
    commit(
        "edit.crop",
        json!({"angle": 7.0, "x": 0.2, "y": 0.2, "width": 0.5, "height": 0.5}),
    );
    // The prefix: every layer before the crop's.
    let crop_at = || {
        luxforge_testkit::client::recipe(&owner, client, &id)
            .unwrap()
            .layers
            .iter()
            .position(|layer| layer.effect_id == luxforge_core::CROP_EFFECT)
            .expect("the crop layer")
    };
    let bounds = ProxyBounds {
        width: 240,
        height: 160,
    };
    let stage = |count| {
        crate::app::tasks::ready_preview_job(
            &owner,
            PreviewRequest::new(client, asset.clone())
                .layers(count)
                .proxy(bounds)
                .gpu(),
        )
        .expect("the input stage's job")
    };
    // The prefix holds Basic, over the whole 480 × 320 source.
    let crop = crop_at();
    let job = stage(crop);
    let rest = job
        .gpu_rest
        .as_deref()
        .expect("the prefix's picture at rest");
    let plan = rest.view.answer.plan().expect("a plan from the source");
    assert_eq!(plan.content.len(), 1, "Basic");
    // The view plan, a drag's frame over it, is the prefix at the reduced stage of the bounds:
    // the whole uncropped source fitted to them.
    let output = plan.geometry.output();
    assert_eq!(
        (output.width, output.height),
        (240, 160),
        "no crop in the prefix: the source's stage fitted to the bounds"
    );
    let tiles = match rest.tiles.as_ref().expect("tiles") {
        Ok(tiles) => tiles,
        Err(reason) => panic!("tiles refused: {reason}"),
    };
    assert_eq!((tiles.output.width, tiles.output.height), (480, 320));
    let covered: u64 = tiles
        .tiles
        .iter()
        .map(|tile| u64::from(tile.rect.width) * u64::from(tile.rect.height))
        .sum();
    assert_eq!(covered, 480 * 320, "every pixel of the prefix once");
    assert_eq!(
        tiles.reduction.as_ref().map(|reduction| reduction.view),
        Some((240, 160)),
        "reduced to the stage's display bounds"
    );
    // Without the GPU asked for, nothing is planned.
    let plain = crate::app::tasks::ready_preview_job(
        &owner,
        PreviewRequest::new(client, asset.clone())
            .layers(crop)
            .proxy(bounds),
    )
    .unwrap();
    assert!(plain.gpu_rest.is_none());
    // A pixel replacement in the prefix: the plan names it, and so do the tiles.
    commit("edit.set-pixel", json!({"x": 3, "y": 2, "rgb": [9, 9, 9]}));
    let job = stage(crop_at());
    let rest = job.gpu_rest.as_deref().expect("the prefix's plan");
    assert!(
        matches!(
            rest.view.answer,
            GpuAnswer::Fallback(GpuFallback::PixelStage { .. })
        ),
        "{:?}",
        rest.view.answer.fallback()
    );
    assert!(matches!(
        rest.tiles,
        Some(Err(GpuFallback::PixelStage { .. }))
    ));
    owner.stop();
    let _ = join.join();
    let _ = std::fs::remove_file(&catalog);
}

/// At Fit a crop draft's input stage asked for with the GPU is the GPU's picture of the layer
/// prefix: the job carries the prefix's picture at rest, the GPU takes it, and nothing is queued
/// for the reference; the surfaces are handed it under the frame, and the stage is on screen once
/// the surface has drawn its last tile, which no surface does here.
#[test]
fn the_input_stage_at_fit_is_the_gpus_picture_of_the_prefix() {
    let catalog = luxforge_testbase::paths::temp_path("crop-stage-gpu.sqlite");
    let (mut editor, asset, _) = crate::app::testing::real_photo(&catalog);
    editor.session.workspace.state_panel = false;
    editor.session.workspace.tools_panel = false;
    editor.view_state.window = (360.0, 300.0);
    let _ = editor.update(Message::Crop(CropMessage::Start));
    let count = editor.crop().expect("a frame").layer_index;
    let ask = editor.crop_stage_ask();
    assert!(
        ask.bounds.is_some() && ask.gpu,
        "Fit asks for the GPU's stage: {ask:?}"
    );
    let job = crate::app::tasks::crop_preview(
        &editor.owner,
        editor.client,
        asset.clone(),
        None,
        count,
        ask,
    )
    .expect("the input stage's job");
    assert!(job.gpu_rest.is_some(), "the prefix's picture at rest");
    let log = crate::app::testing::attach_log(&mut editor);
    let _ = editor.update(Message::Crop(CropMessage::PreviewReady(
        StagePlan::Open,
        Ok(Box::new(job)),
    )));
    let records = crate::app::testing::logged(&mut editor, &log);
    assert_eq!(
        crate::app::testing::events(&records, "crop_stage_gpu").len(),
        1,
        "{records:?}"
    );
    assert_eq!(editor.draft_generation(), None, "no reference job");
    assert_eq!(
        editor.snapshot()["crop"]["input_stage_frame"]["phase"],
        json!("gpu")
    );
    assert!(
        editor.surfaces().stage_rest.is_some(),
        "handed to the surfaces"
    );
    assert!(
        matches!(editor.crop_stage(), Some(StageView::Rendering { .. })),
        "on screen once the surface has drawn it"
    );
    let _ = editor.update(Message::Draft(DraftMessage::Cancel));
    assert!(
        editor.surfaces().stage_rest.is_none(),
        "let go with the draft"
    );
    finish(editor, catalog);
}
