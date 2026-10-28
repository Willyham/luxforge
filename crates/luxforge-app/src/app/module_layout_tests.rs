//! The person's module layout against a real owner: discovery reads `module.list` and
//! `module.layout.read`, a band's menu, the strip's ⋯ menu and the palette send the method the
//! design names and adopt its answer, and another client's change is followed through its
//! `module.layout` event while this desktop's own is skipped.
use super::{
    Editor,
    message::{
        Message,
        modules::{Discovery, ModulesMessage, Written},
        palette::PaletteMessage,
        sync::SyncMessage,
        view::ViewMessage,
    },
    settings_tests::{finish, launch},
    tasks::{SyncResult, call, call_own, sync_now},
};
use crate::state::{MenuTarget, module_layout::Move};
use luxforge_core::{AssetId, module_layout::LayoutView};
use serde_json::{Value, json};

const VIGNETTE: &str = "luxforge.vignette";
const BASIC: &str = "luxforge.basic";

/// Answer discovery as its task would: `module.list`, then `module.layout.read`.
fn discover(editor: &mut Editor) {
    let (mut listed, _) = call(&editor.owner, editor.client, "module.list", json!({})).unwrap();
    let modules = serde_json::from_value(listed["modules"].take()).unwrap();
    let layout = read_layout(editor);
    let _ = editor.update(Message::Modules(ModulesMessage::Discovered(Ok(Box::new(
        Discovery { modules, layout },
    )))));
}

/// The layout the owner holds, read as this desktop's own read does.
fn read_layout(editor: &Editor) -> LayoutView {
    let (layout, _) = call(
        &editor.owner,
        editor.client,
        "module.layout.read",
        json!({}),
    )
    .unwrap();
    serde_json::from_value(layout).unwrap()
}

/// Answer the one layout write in flight as its task would, and say what it sent.
fn answer_write(editor: &mut Editor) -> (String, Value) {
    assert!(editor.layout.pending, "a write in flight");
    let (method, params, status) = editor.layout_started.last().cloned().expect("a write");
    let answer =
        call_own(&editor.owner, editor.client, &method, params.clone()).map(|(layout, request)| {
            Box::new(Written {
                layout: serde_json::from_value(layout).unwrap(),
                request,
                status,
            })
        });
    let _ = editor.update(Message::Modules(ModulesMessage::Written(answer)));
    assert!(!editor.layout.pending);
    (method, params)
}

/// Poll the event log as the event sync does and hand the answer to the desktop.
fn poll(editor: &mut Editor) -> SyncResult {
    let own: Vec<String> = editor.sync.own_requests.iter().cloned().collect();
    let polled = sync_now(
        &editor.owner,
        editor.client,
        Some((AssetId::new(), 0)),
        editor.sync.sequence,
        &own,
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled.clone()))));
    polled
}

/// The sections the panel draws, in its order.
fn drawn(editor: &Editor) -> Vec<String> {
    editor
        .workspace
        .tools
        .sections
        .iter()
        .map(|section| section.module_id.clone())
        .collect()
}

/// Discovery adopts the owner's layout, and a band menu's Turn off sends `module.disable`, adopts
/// the answer, drops the section, names it in the footer and leaves a locked module's menu without
/// Turn off; the footer's palette Enable brings it back through `module.enable`.
#[test]
fn module_layout_a_band_menu_turns_a_module_off_and_the_palette_enables_it() {
    let (mut editor, root) = launch();
    discover(&mut editor);
    assert_eq!(editor.layout.view(), Some(&read_layout(&editor)));
    assert!(drawn(&editor).contains(&VIGNETTE.to_owned()));
    assert_eq!(editor.workspace.tools.off_footer, None);

    // A locked module's menu offers no Turn off, and a stray request is refused here.
    let _ = editor.update(Message::View(ViewMessage::OpenMenu(MenuTarget::Band(
        BASIC.into(),
    ))));
    let basic = editor
        .workspace
        .tools
        .sections
        .iter()
        .find(|s| s.module_id == BASIC);
    let menu = basic
        .and_then(|section| section.menu.clone())
        .expect("Basic's menu");
    assert_eq!(menu.turn_off, None);
    let _ = editor.update(Message::Modules(ModulesMessage::SetEnabled {
        module: BASIC.into(),
        enabled: false,
    }));
    assert!(editor.layout_started.is_empty());
    assert!(
        editor.status.text.starts_with("Basic is always on"),
        "{}",
        editor.status.text
    );

    let _ = editor.update(Message::View(ViewMessage::OpenMenu(MenuTarget::Band(
        VIGNETTE.into(),
    ))));
    let _ = editor.update(Message::Modules(ModulesMessage::SetEnabled {
        module: VIGNETTE.into(),
        enabled: false,
    }));
    assert_eq!(editor.view_state.menu, None, "the menu closes");
    let (method, params) = answer_write(&mut editor);
    assert_eq!(
        (method.as_str(), params),
        ("module.disable", json!({ "module": VIGNETTE }))
    );
    assert!(read_layout(&editor).disabled.contains(&VIGNETTE.to_owned()));
    assert!(!drawn(&editor).contains(&VIGNETTE.to_owned()));
    assert_eq!(
        editor.workspace.tools.off_footer.as_deref(),
        Some("1 module off: Vignette")
    );
    assert!(
        editor.status.text.starts_with("Turned off Vignette"),
        "{}",
        editor.status.text
    );

    // This desktop's own event is read and skipped.
    assert!(
        !poll(&mut editor).layout,
        "its own change is not read again"
    );

    let _ = editor.update(Message::Palette(PaletteMessage::Open));
    let _ = editor.update(Message::Palette(PaletteMessage::Query(
        "Enable Vignette".into(),
    )));
    let _ = editor.update(Message::Palette(PaletteMessage::Run));
    let (method, params) = answer_write(&mut editor);
    assert_eq!(
        (method.as_str(), params),
        ("module.enable", json!({ "module": VIGNETTE }))
    );
    assert!(drawn(&editor).contains(&VIGNETTE.to_owned()));
    assert_eq!(editor.workspace.tools.off_footer, None);
    finish(editor, root);
}

/// The band menu's moves and ⌥↓ send `module.order.set` with the whole order, the band stepping
/// past its drawn neighbour, and the panel follows the answer.
#[test]
fn module_layout_a_band_moves_through_the_order_set() {
    let (mut editor, root) = launch();
    discover(&mut editor);
    let before = drawn(&editor);
    let first = before[0].clone();
    let second = before[1].clone();

    let _ = editor.update(Message::Modules(ModulesMessage::Move {
        toolset: luxforge_core::Toolset::Adjustments,
        module: second.clone(),
        to: Move::Top,
    }));
    let (method, params) = answer_write(&mut editor);
    assert_eq!(method, "module.order.set");
    assert_eq!(params["toolset"], "adjustments");
    assert_eq!(params["order"][0], json!(second));
    assert_eq!(drawn(&editor)[..2], [second.clone(), first.clone()]);
    assert_eq!(
        read_layout(&editor)
            .order(luxforge_core::Toolset::Adjustments)
            .unwrap()[0],
        second
    );

    // ⌥↓ while the band's menu is open moves it down one drawn place, and the menu stays open.
    let _ = editor.update(Message::View(ViewMessage::OpenMenu(MenuTarget::Band(
        second.clone(),
    ))));
    let _ = editor.update(Message::Modules(ModulesMessage::MoveOpenBand(Move::Down)));
    let _ = answer_write(&mut editor);
    assert_eq!(drawn(&editor)[..2], [first, second.clone()]);
    assert_eq!(editor.view_state.menu, Some(MenuTarget::Band(second)));
    finish(editor, root);
}

/// Another client's change reaches the desktop through its `module.layout` event: the poll asks
/// for a read, and the strip and the panel follow what it answers.
#[test]
fn module_layout_another_clients_change_is_followed_through_its_event() {
    let (mut editor, root) = launch();
    discover(&mut editor);
    let agent = editor.owner.register();
    call(
        &editor.owner,
        agent,
        "module.disable",
        json!({ "module": VIGNETTE }),
    )
    .unwrap();
    assert!(poll(&mut editor).layout, "another client's change is read");
    let _ = editor.update(Message::Modules(ModulesMessage::Read(Ok(read_layout(
        &editor,
    )))));
    assert!(!editor.layout.enabled(VIGNETTE));
    assert!(!drawn(&editor).contains(&VIGNETTE.to_owned()));

    // The strip's ⋯ menu lists the tools and Customize opens the Settings sheet.
    let _ = editor.update(Message::View(ViewMessage::OpenMenu(MenuTarget::Strip)));
    assert!(editor.workspace.canvas.strip_menu.open);
    let _ = editor.update(Message::Modules(ModulesMessage::Customize {
        toolset: luxforge_core::Toolset::Tools,
        module: None,
    }));
    assert!(editor.settings.open.is_some());
    assert_eq!(editor.view_state.menu, None);
    finish(editor, root);
}
