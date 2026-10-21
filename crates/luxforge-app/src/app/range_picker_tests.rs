//! The range picker against a real owner and catalog: arming it, what a click, a Shift-click and a
//! drag on the photograph send, what puts it down, and what it leaves alone while it is armed.
//!
//! The photograph is the masks harness's, and every pick a test makes is the request the picker sent,
//! run against the owner as the runtime's task runs it. The pointer is driven through the messages
//! the canvas publishes, in output-stage pixels, so what is proved is the editor's decision and the
//! request it builds, not a simulated widget.
use super::*;
use crate::app::message::mask::PickPointer;
use crate::range_pick::{COLOUR_HINT, TONE_HINT};
use iced::keyboard::key::Named;

/// The pick command a kind's eyedropper sends.
fn pick_method(kind: &str) -> &'static str {
    luxforge_core::mask::commands::pick(kind)
        .expect("the kind is picked from the photograph")
        .method
}

/// One named key through the single keymap table.
fn press_key(masking: &mut Masking, named: Named) {
    let event = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
        key: Key::Named(named),
        modified_key: Key::Named(named),
        physical_key: iced::keyboard::key::Physical::Unidentified(
            iced::keyboard::key::NativeCode::Unidentified,
        ),
        location: iced::keyboard::Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat: false,
    });
    if let Some(message) = crate::app::keymap::keymap(
        &event,
        iced::event::Status::Ignored,
        &masking.editor.key_context(),
    ) {
        let _ = masking.editor.update(message);
    }
}

/// Answer the armed picker's `render.transform`, as the runtime's task does.
fn answer_map(masking: &mut Masking) {
    let id = masking
        .editor
        .mask_panel
        .picker
        .as_ref()
        .expect("the picker is armed")
        .request
        .expect("the picker asked for its map");
    let (transform, _) = call(
        &masking.owner(),
        masking.editor.client,
        "render.transform",
        json!({"asset_id": masking.asset}),
    )
    .unwrap();
    masking.message(MaskMessage::Transform(
        id,
        Ok(serde_json::from_value(transform).unwrap()),
    ));
    assert!(
        masking
            .editor
            .mask_panel
            .picker
            .as_ref()
            .is_some_and(|picker| picker.map.is_some() && picker.request.is_none()),
        "the picker holds its map"
    );
}

/// The output stage the picker's map was drawn for, in pixels.
fn output(masking: &Masking) -> (f64, f64) {
    masking
        .editor
        .mask_panel
        .picker
        .as_ref()
        .and_then(|picker| picker.map.as_ref())
        .expect("the picker holds its map")
        .output()
}

fn pointer(masking: &mut Masking, pointer: PickPointer) {
    masking.message(MaskMessage::PickPointer(pointer));
}

/// The canvas's own point for an output-stage pixel, at one point a pixel.
fn view((x, y): (f64, f64)) -> (f32, f32) {
    (x as f32, y as f32)
}

/// A press and its release at `at`, with Shift held at the release or not.
fn click(masking: &mut Masking, at: (f64, f64), shift: bool) {
    pointer(masking, PickPointer::Press { view: view(at), at });
    pointer(
        masking,
        PickPointer::Release {
            view: view(at),
            at,
            shift,
        },
    );
}

/// The pick the picker sent, run against the owner as its task does, and the photograph read back
/// with its new map answered. Returns the pick's method and parameters.
fn settle_pick(masking: &mut Masking) -> (String, Value) {
    let (method, params) = masking
        .editor
        .mask_panel
        .last_request
        .take()
        .expect("the pick sent its command");
    call(
        &masking.owner(),
        masking.editor.client,
        &method,
        params.clone(),
    )
    .unwrap_or_else(|error| panic!("{method} was refused: {error}"));
    masking.editor.busy = false;
    masking.refresh();
    if masking.editor.mask_panel.picker.is_some() {
        answer_map(masking);
    }
    (method, params)
}

fn samples(masking: &Masking, component: usize) -> usize {
    masking.listing().masks[0].components[component].payload["samples"]
        .as_array()
        .map_or(0, Vec::len)
}

fn close(a: &Value, b: f64) -> bool {
    a.as_f64().is_some_and(|a| (a - b).abs() < 1e-9)
}

/// **A typed create arms the eyedropper** on the component it made: Mask mode stays, the row's Pick
/// is lit, the pick bar's hint is the kind's, an overlay that is Off shows Tint, and the canvas takes
/// presses once the map has arrived. Escape puts it down and Off is back; Mask mode stays for a
/// second Escape. The row's Pick arms it again, and Return is the bar's Done.
#[test]
fn a_typed_create_arms_the_picker_over_tint_and_escape_puts_it_down() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("colour-range".to_owned()));
    let component = masking.listing().masks[0].components[0].id.clone();
    let picker = masking
        .editor
        .mask_panel
        .picker
        .as_ref()
        .expect("a typed create arms the picker");
    assert_eq!(picker.component, component);
    assert_eq!(picker.action, pick_method("colour-range"));
    assert_eq!(masking.editor.session.workspace.mode, MASK_MODE);
    assert!(masking.editor.workspace.masks.components[0].picking);
    assert_eq!(overlay_state(&masking), ("off".into(), "tint".into(), true));
    let summary = &masking.editor.workspace.masks.summary()["picker"];
    assert_eq!(summary["hint"], json!(COLOUR_HINT));
    assert_eq!(
        masking.editor.workspace.masks.summary()["components"][0]["sample_caption"],
        json!("No colour yet")
    );
    assert_eq!(summary["mapped"], json!(false));
    assert!(
        masking.editor.range_pick_canvas().is_none(),
        "no press is taken before the map arrives"
    );
    answer_map(&mut masking);
    assert!(masking.editor.range_pick_canvas().is_some());

    press_key(&mut masking, Named::Escape);
    assert!(masking.editor.mask_panel.picker.is_none());
    assert!(!masking.editor.workspace.masks.components[0].picking);
    assert_eq!(overlay_state(&masking), ("off".into(), "off".into(), false));
    assert!(masking.editor.mask_mode_active(), "one Escape, one level");

    masking.message(MaskMessage::Pick);
    assert!(
        masking.editor.mask_panel.picker.is_some(),
        "the row's Pick arms it"
    );
    press_key(&mut masking, Named::Enter);
    assert!(masking.editor.mask_panel.picker.is_none(), "Return is Done");
    masking.message(MaskMessage::Pick);
    masking.message(MaskMessage::PickDone);
    assert!(masking.editor.mask_panel.picker.is_none(), "and so is Done");
}

/// **Picking is a sub-state of Mask mode.** The mode, the open mask and the sections' target stay
/// as they were, so an Exposure drag made while the eyedropper is armed edits the open mask's own
/// layer, and the eyedropper is still armed after it, with the map of the entry it made.
#[test]
fn picking_keeps_mask_mode_and_a_slider_edits_the_open_mask() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("luminance-range".to_owned()));
    let mask = masking.listing().masks[0].id.clone();
    assert!(masking.editor.mask_panel.picker.is_some());
    answer_map(&mut masking);
    assert_eq!(masking.editor.section_target(), Some(&mask));

    let _ = testing::slide(&mut masking.editor, "set-basic", "exposure", 0.3);
    assert_eq!(
        masking.editor.draft_target("set-basic").mask.as_ref(),
        Some(&mask),
        "the drag drafts through the open mask"
    );
    assert!(masking.editor.mask_panel.picker.is_some());
    let _ = testing::let_go(&mut masking.editor, "set-basic", "exposure");
    assert!(testing::run_commit(&mut masking.editor));
    assert_eq!(masking.editor.session.workspace.mode, MASK_MODE);
    let described = masking.editor.document.recipe.as_ref().expect("a recipe");
    let masked: Vec<_> = described
        .layers
        .iter()
        .filter(|layer| layer.mask.as_ref() == Some(&mask))
        .collect();
    assert_eq!(masked.len(), 1, "{described:?}");
    assert_eq!(masked[0].values["exposure"], json!(0.3));
    assert!(
        masking.editor.mask_panel.picker.is_some(),
        "an adjustment between picks keeps the eyedropper"
    );
    answer_map(&mut masking);
}

/// **A click replaces, Shift-click adds, a drag sends its rectangle.** Each is one
/// `mask.pick-colour-range` with the clicked position normalized by the content stage; a drag sends
/// the rectangle's top-left corner and size, and the canvas draws it while it lasts. A Shift-click
/// made while the last pick's new map is on its way waits for it rather than being placed on the
/// old one.
#[test]
fn a_click_replaces_a_shift_click_adds_and_a_drag_sends_its_bounding_box() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("colour-range".to_owned()));
    let report = masking.listing().masks[0].clone();
    let component = report.components[0].id.clone();
    answer_map(&mut masking);
    let (width, height) = output(&masking);
    let method = pick_method("colour-range");

    masking.editor.mask_panel.last_request = None;
    click(&mut masking, (width * 0.25, height * 0.25), false);
    let (sent, params) = masking
        .editor
        .mask_panel
        .last_request
        .take()
        .expect("the click sent its pick");
    assert_eq!(sent, method);
    assert_eq!(params["mask"], json!(report.id.as_str()));
    assert_eq!(params["component"], json!(component.as_str()));
    assert!(
        close(&params["x"], 0.25) && close(&params["y"], 0.25),
        "{params}"
    );
    assert_eq!(params["add"], json!(false), "a plain click replaces");
    assert!(params.get("width").is_none());
    call(&masking.owner(), masking.editor.client, &sent, params)
        .unwrap_or_else(|error| panic!("{sent} was refused: {error}"));
    masking.editor.busy = false;
    masking.refresh();
    assert_eq!(samples(&masking, 0), 1);
    assert_eq!(
        masking.labels().last().map(String::as_str),
        Some("Pick Colour range 1")
    );

    // The pick made a new entry, so the map is asked for again; a Shift-click meanwhile waits.
    assert!(
        masking
            .editor
            .mask_panel
            .picker
            .as_ref()
            .unwrap()
            .request
            .is_some(),
        "the new entry's map is asked for"
    );
    click(&mut masking, (width * 0.75, height * 0.5), true);
    assert!(
        masking.editor.mask_panel.last_request.is_none(),
        "the pick waits for the map of the stack on screen"
    );
    assert_eq!(
        masking.editor.workspace.masks.summary()["picker"]["waiting"],
        json!(true)
    );
    answer_map(&mut masking);
    let (_, params) = settle_pick(&mut masking);
    assert!(
        close(&params["x"], 0.75) && close(&params["y"], 0.5),
        "{params}"
    );
    assert_eq!(params["add"], json!(true), "Shift-click adds");
    assert_eq!(samples(&masking, 0), 2);
    assert_eq!(
        masking.editor.workspace.masks.summary()["components"][0]["sample_caption"],
        json!("2 of 5")
    );

    // A drag, drawn while it lasts and sent as its rectangle.
    let (from, to) = ((width * 0.1, height * 0.2), (width * 0.5, height * 0.6));
    pointer(
        &mut masking,
        PickPointer::Press {
            view: view(from),
            at: from,
        },
    );
    pointer(
        &mut masking,
        PickPointer::Move {
            view: view(to),
            at: to,
        },
    );
    assert_eq!(
        masking.editor.range_pick_canvas().unwrap().rectangle,
        Some([from.0, from.1, to.0, to.1])
    );
    pointer(
        &mut masking,
        PickPointer::Release {
            view: view(to),
            at: to,
            shift: false,
        },
    );
    let (_, params) = settle_pick(&mut masking);
    assert!(
        close(&params["x"], 0.1) && close(&params["y"], 0.2),
        "{params}"
    );
    assert!(
        close(&params["width"], 0.4) && close(&params["height"], 0.4),
        "{params}"
    );
    assert_eq!(params["add"], json!(false));
    assert_eq!(samples(&masking, 0), 1, "a plain drag replaces too");
    assert!(
        masking.editor.mask_panel.picker.is_some(),
        "it stays for the next pick"
    );
}

/// **Escape mid-drag** cancels that drag: its release sends nothing and the eyedropper stays armed.
/// The next Escape puts it down.
#[test]
fn escape_mid_drag_sends_nothing_and_keeps_the_picker() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("colour-range".to_owned()));
    answer_map(&mut masking);
    masking.editor.mask_panel.last_request = None;
    let (from, to) = ((40.0, 40.0), (200.0, 120.0));
    pointer(
        &mut masking,
        PickPointer::Press {
            view: view(from),
            at: from,
        },
    );
    pointer(
        &mut masking,
        PickPointer::Move {
            view: view(to),
            at: to,
        },
    );
    press_key(&mut masking, Named::Escape);
    assert_eq!(masking.editor.status.text, "Pick cancelled");
    let picker = masking
        .editor
        .mask_panel
        .picker
        .as_ref()
        .expect("still armed");
    assert!(picker.drag.is_none() && picker.rectangle().is_none());
    pointer(
        &mut masking,
        PickPointer::Release {
            view: view(to),
            at: to,
            shift: false,
        },
    );
    assert!(
        masking.editor.mask_panel.last_request.is_none(),
        "nothing is sent"
    );
    assert!(
        masking
            .editor
            .mask_panel
            .picker
            .as_ref()
            .unwrap()
            .waiting
            .is_none()
    );
    assert_eq!(samples(&masking, 0), 0);
    press_key(&mut masking, Named::Escape);
    assert!(masking.editor.mask_panel.picker.is_none());
}

/// **One pick is in flight at a time.** A pick made while another's command is out waits, and a
/// newer one replaces it; the newest is sent once the first is answered and the map is fresh.
#[test]
fn one_pick_is_in_flight_and_the_newest_waiting_one_replaces_the_older() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("colour-range".to_owned()));
    answer_map(&mut masking);
    let (width, height) = output(&masking);
    let log = testing::attach_log(&mut masking.editor);
    click(&mut masking, (width * 0.25, height * 0.25), false);
    let (method, first) = masking
        .editor
        .mask_panel
        .last_request
        .take()
        .expect("the first pick is sent");
    assert!(masking.editor.busy);
    click(&mut masking, (width * 0.5, height * 0.5), true);
    click(&mut masking, (width * 0.75, height * 0.75), true);
    assert!(
        masking.editor.mask_panel.last_request.is_none(),
        "both wait"
    );
    assert_eq!(
        testing::events(
            &testing::logged(&mut masking.editor, &log),
            "range_pick_replaced"
        )
        .len(),
        1
    );

    call(&masking.owner(), masking.editor.client, &method, first).unwrap();
    masking.editor.busy = false;
    masking.refresh();
    answer_map(&mut masking);
    let (_, params) = settle_pick(&mut masking);
    assert!(
        close(&params["x"], 0.75) && close(&params["y"], 0.75),
        "{params}"
    );
    assert_eq!(params["add"], json!(true));
    assert_eq!(samples(&masking, 0), 2, "the replaced pick was never sent");
}

/// **What puts it down**: selecting another component, undoing the component it picks into away,
/// and leaving Mask mode. Undoing a pick keeps it, as the component is still there.
#[test]
fn the_picker_is_put_down_by_a_selection_change_undo_and_leaving_the_mode() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("colour-range".to_owned()));
    masking.run(MaskMessage::Add("luminance-range".to_owned()));
    let components = masking.listing().masks[0].components.clone();
    assert_eq!(
        masking
            .editor
            .mask_panel
            .picker
            .as_ref()
            .map(|picker| &picker.component),
        Some(&components[1].id),
        "a typed add arms the eyedropper on the component it added"
    );

    masking.message(MaskMessage::SelectComponent(
        components[0].id.as_str().to_owned(),
    ));
    assert!(
        masking.editor.mask_panel.picker.is_none(),
        "a selection change"
    );

    masking.message(MaskMessage::Pick);
    answer_map(&mut masking);
    let (width, height) = output(&masking);
    click(&mut masking, (width * 0.5, height * 0.5), false);
    settle_pick(&mut masking);
    masking.undo();
    assert!(
        masking.editor.mask_panel.picker.is_some(),
        "undoing a pick leaves the component, and the eyedropper"
    );

    masking.message(MaskMessage::SelectComponent(
        components[1].id.as_str().to_owned(),
    ));
    masking.message(MaskMessage::Pick);
    assert!(masking.editor.mask_panel.picker.is_some());
    masking.undo();
    assert!(
        masking.editor.mask_panel.picker.is_none(),
        "undoing the add takes the component, and the eyedropper with it"
    );

    masking.message(MaskMessage::SelectComponent(
        components[0].id.as_str().to_owned(),
    ));
    masking.message(MaskMessage::Pick);
    assert!(masking.editor.mask_panel.picker.is_some());
    masking.set_mode(POINTER_MODE);
    assert!(
        masking.editor.mask_panel.picker.is_none(),
        "leaving Mask mode"
    );
    masking.message(MaskMessage::Pick);
    assert!(
        masking.editor.mask_panel.picker.is_none(),
        "outside Mask mode Pick does not arm: {}",
        masking.editor.status.text
    );
}

/// **Another mask gesture, and Compare, put it down** too: a new gradient owns the next press, and
/// the before view is not the stack a pick reads.
#[test]
fn another_mask_gesture_and_compare_put_the_picker_down() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("colour-range".to_owned()));
    assert!(masking.editor.mask_panel.picker.is_some());
    masking.message(MaskMessage::New(LINEAR.to_owned()));
    assert!(masking.editor.mask_shape().is_some());
    assert!(
        masking.editor.mask_panel.picker.is_none(),
        "a new gradient puts the eyedropper down"
    );
    masking.draft(DraftMessage::Cancel);

    let colour = masking.listing().masks[0].components[0].id.clone();
    masking.message(MaskMessage::SelectComponent(colour.as_str().to_owned()));
    if masking.editor.mask_panel.picker.is_none() {
        masking.message(MaskMessage::Pick);
    }
    assert!(masking.editor.mask_panel.picker.is_some());
    // The before view held, as Compare's own answer leaves it; the next update sees it.
    masking.editor.document.compare_return = Some(luxforge_core::HistorySelection::Current);
    masking.message(MaskMessage::Hover(None));
    assert!(
        masking.editor.mask_panel.picker.is_none(),
        "Compare puts it down"
    );
}

/// **The luminance row has a Pick** too. Its eyedropper reads no Shift — a tone range holds no
/// colours to add to — so its pick carries no `add`, and Copy as JSON request on the Pick button
/// shows exactly the request its last pick sent.
#[test]
fn the_luminance_rows_pick_sends_a_tone_pick_and_copies_it() {
    let mut masking = Masking::opened();
    masking.enter_mask_mode();
    masking.run(MaskMessage::New("luminance-range".to_owned()));
    let row = &masking.editor.workspace.masks.components[0];
    assert!(row.can_pick && row.picking);
    assert_eq!(
        masking.editor.workspace.masks.summary()["picker"]["hint"],
        json!(TONE_HINT)
    );
    assert_eq!(
        masking.editor.workspace.masks.summary()["components"][0]["sample_caption"],
        Value::Null
    );
    masking.message(MaskMessage::Pick);
    assert!(
        !masking.editor.workspace.masks.components[0].picking,
        "the lit Pick puts it down"
    );
    masking.message(MaskMessage::Pick);
    answer_map(&mut masking);
    let (width, height) = output(&masking);

    click(&mut masking, (width * 0.5, height * 0.25), true);
    let sent = masking.sent().expect("the pick was sent");
    let (method, params) = settle_pick(&mut masking);
    assert_eq!(method, pick_method("luminance-range"));
    assert!(params.get("add").is_none(), "{params}");
    assert!(
        close(&params["x"], 0.5) && close(&params["y"], 0.25),
        "{params}"
    );

    let (copied_method, target, fields) = masking
        .editor
        .copied_pick()
        .expect("the Pick copies a request");
    assert_eq!(copied_method, method);
    let copied = super::identified(
        masking
            .editor
            .mask_request(&target, &fields)
            .expect("a photograph is open"),
    );
    let mut sent_fields = sent.clone();
    let mut copied_fields = copied.clone();
    // The copy is built against the entry it is copied on, which the pick moved on from.
    for request in [&mut sent_fields, &mut copied_fields] {
        request.as_object_mut().unwrap().remove("mutation");
    }
    assert_eq!(copied_fields, sent_fields);

    // Put down, the row still copies the pick it last made; with the component gone, nothing is
    // kept of it.
    masking.message(MaskMessage::Pick);
    assert!(
        masking.editor.mask_panel.picker.is_none(),
        "the picker is down"
    );
    let (_, _, after) = masking
        .editor
        .copied_pick()
        .expect("the Pick copies a request");
    assert!(
        close(&after["x"], 0.5) && close(&after["y"], 0.25),
        "the last pick survives Done: {after:?}"
    );
    assert!(masking.editor.mask_panel.last_pick.is_some());
    masking.run(MaskMessage::Key(crate::app::message::mask::MaskKey::Delete));
    let _ = masking
        .editor
        .update(Message::Mask(MaskMessage::Hover(None)));
    assert!(
        masking.editor.mask_panel.last_pick.is_none(),
        "the pick goes with its component"
    );
}

/// **Picking is never flag-gated.** With `masks.range-selections` turned off after a colour range
/// was made, its row's Pick still arms the eyedropper and a click still commits its pick.
#[test]
fn an_existing_range_is_picked_with_its_experiment_off() {
    let dir = scratch("picker-experiment");
    std::fs::create_dir_all(&dir).unwrap();
    let catalog = dir.join("catalog.sqlite");
    let (owner, join) = OwnerHandle::start_with_host(
        &catalog,
        std::sync::Arc::new(luxforge_core::ModuleRegistry::builtin()),
        luxforge_core::HostConfig {
            preferences_dir: Some(dir.join("config")),
            ..luxforge_core::HostConfig::unconfigured()
        },
    )
    .unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/s0/orientation-1.jpg");
    let (editor, asset, agent) =
        testing::real_photo_on(owner, join, &fixture, crate::Config::default());
    let mut masking = Masking {
        editor,
        catalog,
        asset,
        agent,
    };
    let set_flag = |masking: &mut Masking, value: bool| {
        call(
            &masking.owner(),
            masking.agent,
            "flags.set",
            json!({"flag": luxforge_core::flags::RANGE_MASKS, "value": value}),
        )
        .expect("the flag is stored");
        testing::read_flags(&mut masking.editor);
    };
    masking.enter_mask_mode();
    set_flag(&mut masking, true);
    masking.run(MaskMessage::New("colour-range".to_owned()));
    masking.message(MaskMessage::PickDone);
    set_flag(&mut masking, false);
    assert!(
        !masking
            .editor
            .workspace
            .masks
            .kinds
            .iter()
            .any(|kind| kind.kind == "colour-range"),
        "no new colour range is offered"
    );

    assert!(masking.editor.workspace.masks.components[0].can_pick);
    masking.message(MaskMessage::Pick);
    answer_map(&mut masking);
    let (width, height) = output(&masking);
    click(&mut masking, (width * 0.5, height * 0.5), false);
    let (method, _) = settle_pick(&mut masking);
    assert_eq!(method, pick_method("colour-range"));
    assert_eq!(samples(&masking, 0), 1);
    let _ = std::fs::remove_dir_all(dir.join("config"));
}
