//! The command palette runs exactly what the panels run.
use super::{
    message::{action::ActionMessage, palette::PaletteMessage},
    testing::{descriptors, finish, opened_with_modules},
    *,
};
use crate::state::palette::PaletteAction;

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
    assert!(editor.busy, "{}", editor.status.text);
    assert!(
        editor.status.text.starts_with("Running edit.transform"),
        "{}",
        editor.status.text
    );
    let palette_status = editor.status.text.clone();

    // The exact message the generated button's own click raises produces the identical request.
    editor.busy = false;
    let _ = editor.update(Message::Action(ActionMessage::Run { action, preset }));
    assert_eq!(editor.status.text, palette_status);
    finish(editor, catalog);
}

/// The palette's entries for `query`, as the open palette lists them.
fn entries_for(editor: &mut Editor, query: &str) -> Vec<crate::state::palette::PaletteEntry> {
    let _ = editor.update(Message::Palette(PaletteMessage::Open));
    let _ = editor.update(Message::Palette(PaletteMessage::Query(query.into())));
    editor.workspace.palette.entries.clone()
}

/// A query naming a control opens its section and nothing else: the first entry for "clarity" is
/// Presence's Clarity slider, and running it expands Presence, collapses every other section and
/// marks that slider, until the mark's own timeout clears it and an older one cannot.
#[test]
fn a_control_query_opens_its_section_collapses_the_rest_and_marks_the_control() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 3);
    let entries = entries_for(&mut editor, "clarity");
    let first = entries.first().expect("an entry for clarity");
    assert_eq!(first.label, "Presence \u{b7} Clarity");
    assert_eq!(first.detail, "edit.set-presence");
    let PaletteAction::Reveal(target) = first.action.clone() else {
        panic!("expected a reveal first, got {:?}", first.action);
    };
    assert_eq!(target.module_id, "luxforge.presence");

    let _ = editor.update(Message::Palette(PaletteMessage::Run));
    assert!(
        !editor.workspace.palette.open,
        "running a reveal closes the palette"
    );
    let sections: Vec<_> = editor.workspace.tools.all().collect();
    let presence = sections
        .iter()
        .find(|section| section.module_id == "luxforge.presence")
        .expect("the Presence section");
    assert!(
        presence.expanded,
        "Presence is declared collapsed and the reveal opens it"
    );
    assert!(
        presence.controls.iter().any(|control| control.reveal_key()
            == Some(crate::state::tools::RevealKey::Field((
                "set-presence".into(),
                "clarity".into()
            )))),
        "the open section draws the revealed control"
    );
    assert_eq!(
        presence.mark,
        Some(crate::state::tools::SectionMark::Control(
            crate::state::tools::RevealKey::Field(("set-presence".into(), "clarity".into()))
        ))
    );
    for section in sections
        .iter()
        .filter(|section| section.module_id != presence.module_id)
    {
        assert!(!section.expanded, "{} stays open", section.module_id);
        assert!(section.mark.is_none(), "{} is marked", section.module_id);
    }

    let sequence = editor.palette.revealed.as_ref().unwrap().sequence;
    let _ = editor.update(Message::Palette(PaletteMessage::Unmark(sequence - 1)));
    assert!(
        editor.palette.revealed.is_some(),
        "an older timeout leaves the mark"
    );
    let _ = editor.update(Message::Palette(PaletteMessage::Unmark(sequence)));
    assert!(editor.palette.revealed.is_none());
    let presence = editor
        .workspace
        .tools
        .all()
        .find(|section| section.module_id == "luxforge.presence")
        .unwrap();
    assert!(presence.mark.is_none(), "the mark times out");
    assert!(presence.expanded, "the section stays open after the mark");
    finish(editor, catalog);
}

/// A query naming a module opens its section first, ahead of its controls and its reset, and a
/// reveal of a control inside a closed group opens the group too.
#[test]
fn a_module_query_opens_the_section_ahead_of_its_controls_and_reset() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 3);
    let entries = entries_for(&mut editor, "detail");
    assert_eq!(entries[0].label, "Detail");
    assert_eq!(
        entries[0].action,
        PaletteAction::Reveal(crate::state::palette::RevealTarget {
            module_id: "luxforge.detail".into(),
            control: None,
        })
    );
    let reset = entries
        .iter()
        .position(|entry| entry.label == "Detail \u{b7} Reset")
        .expect("Detail's reset is still offered");
    assert!(
        entries[..reset]
            .iter()
            .all(|entry| matches!(entry.action, PaletteAction::Reveal(_))),
        "every reveal comes before the reset: {:?}",
        entries.iter().map(|entry| &entry.label).collect::<Vec<_>>()
    );
    let _ = editor.update(Message::Palette(PaletteMessage::Run));
    for section in editor.workspace.tools.all() {
        assert_eq!(
            section.expanded,
            section.module_id == "luxforge.detail",
            "{}",
            section.module_id
        );
    }
    assert_eq!(
        editor
            .workspace
            .tools
            .all()
            .find(|section| section.module_id == "luxforge.detail")
            .unwrap()
            .mark,
        Some(crate::state::tools::SectionMark::Section)
    );

    // A group holding a revealed control is opened with it, even one the person closed.
    let entries = entries_for(&mut editor, "exposure");
    let PaletteAction::Reveal(target) = entries[0].action.clone() else {
        panic!("expected a reveal first, got {:?}", entries[0]);
    };
    let (path, _) = target.control.clone().expect("a control");
    assert!(path.len() > 1, "{} sits in a group", entries[0].label);
    let group = crate::state::tools::group_key(&target.module_id, &path[..1]);
    editor
        .controls
        .ui
        .group_expanded
        .insert(group.clone(), false);
    let _ = editor.update(Message::Palette(PaletteMessage::Run));
    assert_eq!(editor.controls.ui.group_expanded.get(&group), Some(&true));
    finish(editor, catalog);
}

/// The queries the evidence scripts run still pick the command they always did: an exact label,
/// and a command no section or control is named for, rank ahead of every reveal.
#[test]
fn the_scripted_queries_still_run_their_commands() {
    let (mut editor, catalog) = opened_with_modules(descriptors(), 3);
    editor.developer = true;
    for query in [
        "gpu preview",
        "Fit",
        "As shot",
        "settings general",
        "experiments",
        "Crop, transform, straighten \u{b7} Rotate 90\u{b0} right",
    ] {
        let entries = entries_for(&mut editor, query);
        let first = entries
            .first()
            .unwrap_or_else(|| panic!("no entry for {query:?}"));
        assert!(
            !matches!(first.action, PaletteAction::Reveal(_)),
            "{query:?} now reveals {:?}",
            first.label
        );
    }
    finish(editor, catalog);
}
