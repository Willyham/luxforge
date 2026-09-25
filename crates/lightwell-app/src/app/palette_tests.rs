//! The command palette runs exactly what the panels run.
use super::{
    message::{ActionMessage, PaletteAction, PaletteMessage},
    testing::{descriptors, finish, opened_with_modules},
    *,
};

/// The palette runs an entry through the exact message a click on its control raises, so a
/// palette hit for `edit.transform` and the generated button produce the identical request.
#[test]
fn a_palette_entry_for_transform_runs_the_same_request_as_its_button() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 3);
    let _ = editor.update(Message::Palette(PaletteMessage::Open));
    let _ = editor.update(Message::Palette(PaletteMessage::Query(
        "Rotate right".into(),
    )));
    let entry = editor
        .workspace
        .palette
        .entries
        .first()
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "no palette entry matched: {:?}",
                editor.workspace.palette.entries
            )
        });
    let PaletteAction::Run { action, preset } = entry.action.clone() else {
        panic!("expected a runnable action entry, got {:?}", entry.action);
    };
    assert_eq!(action, "transform");
    let _ = editor.update(Message::Palette(PaletteMessage::Run));
    assert!(
        !editor.workspace.palette.open,
        "running an entry closes the palette"
    );
    assert!(editor.busy, "{}", editor.status);
    assert!(
        editor.status.starts_with("Running edit.transform"),
        "{}",
        editor.status
    );
    let palette_status = editor.status.clone();

    // The exact message the generated button's own click raises produces the identical request.
    editor.busy = false;
    let _ = editor.update(Message::Action(ActionMessage::Run { action, preset }));
    assert_eq!(editor.status, palette_status);
    finish(editor, catalog);
}
