//! The Grain section on the desktop (`docs/design/grain.md`, Desktop), against a real owner and a
//! real photograph. The section is generated from the module's descriptor, with no grain-specific
//! widget: after Vignette, collapsed, its one group drawn without a header, Amount, Size and
//! Roughness unipolar at 0, 25 and 50, and the module's reset. Each slider drafts through the core
//! lifecycle and previews while dragging, a release commits once, Escape cancels, a right-click's
//! Copy as JSON request is the request an independent client sends to `edit.set-grain`, and the
//! band's accent dot follows the core's neutrality, which is Amount's alone. The section, its
//! settings group and its palette entries are offered only while the `grain` experiment flag is
//! on, and follow the flag live.
use super::{
    gpu_preview_tests::{catalog, deliver_until},
    keymap,
    message::{
        action::ActionMessage, control::ControlMessage, draft::DraftMessage, sync::SyncMessage,
    },
    tasks,
    testing::{
        attach_log, events, finish, grain_config, let_go, logged, read_flags, real_photo_prepared,
        run_commit, slide, start_owner,
    },
    *,
};
use crate::state::tools::{ControlModel, SectionModel};
use luxforge_core::{AssetId, ClientId, GRAIN_EFFECT};
use luxforge_gpu::GpuStageState;

const MODULE: &str = "luxforge.grain";
const ACTION: &str = "set-grain";

fn section(editor: &Editor) -> &SectionModel {
    editor
        .workspace
        .tools
        .sections
        .iter()
        .find(|section| section.module_id == MODULE)
        .expect("the Grain section is drawn")
}

/// The grain layer's values as `recipe.describe` reports them, every field resolved, and whether
/// the core calls it neutral; `None` with no grain layer.
fn grain_row(editor: &Editor) -> Option<(Value, bool)> {
    editor
        .document
        .current_recipe
        .as_ref()?
        .layers
        .iter()
        .find(|row| row.effect == GRAIN_EFFECT)
        .map(|row| (Value::Object(row.values.clone()), row.neutral))
}

/// The standard deviation of the green channel over a 13 × 13 patch at fractions `(fx, fy)` of the
/// frame on screen: near zero on the fixture's flat quadrant colours, and raised by grain.
fn spread(editor: &Editor, fx: f64, fy: f64) -> f64 {
    let frame = editor.presentation.presenter.photo().expect("a frame");
    let (width, height) = frame.size();
    let pixels = frame.pixels();
    let (cx, cy) = (
        (fx * f64::from(width)) as i64,
        (fy * f64::from(height)) as i64,
    );
    let values: Vec<f64> = (cy - 6..=cy + 6)
        .flat_map(|y| (cx - 6..=cx + 6).map(move |x| (x, y)))
        .map(|(x, y)| f64::from(pixels[(y as usize * width as usize + x as usize) * 4 + 1]))
        .collect();
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

/// The summed [`spread`] of the four quadrants' flat interiors, clear of the centre cross, the dash
/// band and the corners' labels.
fn grain_spread(editor: &Editor) -> f64 {
    [(0.12, 0.2), (0.88, 0.2), (0.12, 0.8), (0.88, 0.8)]
        .into_iter()
        .map(|(fx, fy)| spread(editor, fx, fy))
        .sum()
}

/// Exactly the message the keyboard table raises for Escape while a slider gesture is open.
fn escape(editor: &Editor) -> Message {
    let context = keymap::KeyContext {
        slider_drafting: true,
        ..editor.key_context()
    };
    let key = iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape);
    keymap::keymap(
        &iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: iced::keyboard::key::Physical::Unidentified(
                iced::keyboard::key::NativeCode::Unidentified,
            ),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        }),
        iced::event::Status::Ignored,
        &context,
    )
    .expect("Escape maps to a message while a slider drafts")
}

/// Read the photograph back after another client's edit, as the desktop's event poll does.
fn refresh(editor: &mut Editor, asset: &AssetId) {
    let read = tasks::refresh(
        &editor.owner,
        editor.client,
        asset.clone(),
        tasks::Scope::Elsewhere,
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(read)))));
}

/// One `edit.set-grain` from the independent client, on the revision the desktop shows.
fn agent_sets(editor: &mut Editor, agent: ClientId, asset: &AssetId, fields: Value) {
    let revision = editor.document.state.as_ref().unwrap().revision;
    let mut params = json!({"asset_id": asset, "mutation": tasks::mutation(revision)});
    params
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    tasks::call(&editor.owner, agent, "edit.set-grain", params).expect("the edit commits");
    refresh(editor, asset);
}

#[test]
fn the_grain_section_drafts_previews_commits_once_cancels_copies_and_dots_on_amount() {
    let catalog = catalog("grain-section");
    // The reference renderer draws every frame, so the preview's pixels can be read here.
    let (owner, join) = start_owner(&catalog, &grain_config());
    let (mut editor, asset, agent, ()) = real_photo_prepared(
        owner,
        join,
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0/orientation-1.jpg"),
        grain_config(),
        |editor| editor.renderer.stage = Some(GpuStageState::NoAdapter { refused: true }),
    );
    deliver_until(&mut editor, "the opened frame", |editor| {
        editor.presentation.has_picture() && !editor.presentation.queue.is_busy()
    });

    // Generated after Vignette, collapsed, with no dot and nothing built for its body.
    let order: Vec<&str> = editor
        .workspace
        .tools
        .sections
        .iter()
        .map(|section| section.module_id.as_str())
        .collect();
    let at = order.iter().position(|id| *id == MODULE).expect("listed");
    assert_eq!(order[at - 1], "luxforge.vignette", "{order:?}");
    let grain = section(&editor);
    assert_eq!(grain.title, "Grain");
    assert!(!grain.expanded && !grain.active && grain.controls.is_empty());
    assert_eq!(
        grain.reset.as_ref().map(|reset| reset.action.as_str()),
        Some("reset-grain")
    );

    // Expanded: three sliders straight under the band, no group header, each unipolar (its fill
    // grows from the rail's start, so no zero tick) at its declared default.
    let _ = editor.update(Message::Control(ControlMessage::ToggleSection(
        MODULE.into(),
    )));
    let grain = section(&editor);
    assert!(grain.expanded);
    let sliders: Vec<_> = grain
        .controls
        .iter()
        .map(|control| match control {
            ControlModel::Slider(slider) => slider,
            other => panic!("only sliders, no group header: {other:?}"),
        })
        .collect();
    let shown: Vec<_> = sliders
        .iter()
        .map(|slider| {
            let spec = &slider.spec;
            assert_eq!(slider.action, ACTION);
            assert_eq!((spec.min, spec.max), (0.0, 100.0), "{}", slider.label);
            assert_eq!(
                luxforge_ui::geometry::zero_fraction(spec.soft_min, spec.soft_max, Some(spec.zero)),
                None,
                "{} is unipolar",
                slider.label
            );
            (
                slider.label.as_str(),
                slider.parameter.as_str(),
                slider.value,
                slider.default.as_str(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("Amount", "amount", 0.0, "0"),
            ("Size", "size", 25.0, "25"),
            ("Roughness", "roughness", 50.0, "50"),
        ]
    );
    let before = grain_spread(&editor);

    // A drag on Amount drafts and previews: one draft, one set per value, and the newest tick's
    // drafted frame on screen carries grain over the flat quadrants.
    let log = attach_log(&mut editor);
    for value in [20.0, 40.0, 60.0] {
        let _ = slide(&mut editor, ACTION, "amount", value);
    }
    let draft = editor.session.draft.as_ref().expect("a core draft");
    assert_eq!(draft.action, ACTION);
    assert_eq!(draft.fields, *json!({"amount": 60.0}).as_object().unwrap());
    let revision = draft.draft_revision;
    deliver_until(&mut editor, "the drafted frame", |editor| {
        editor.presentation.displayed_draft_revision == Some(revision)
            && !editor.presentation.queue.is_busy()
    });
    let drafted = grain_spread(&editor);
    assert!(
        drafted > before + 2.0,
        "the drafted frame shows grain: {before} -> {drafted}"
    );
    assert_eq!(grain_row(&editor), None, "a draft commits nothing");
    assert!(
        !section(&editor).active,
        "the dot reads the committed recipe"
    );

    // The release commits once, and the band's dot lights.
    let entries = editor.document.history.entries.len();
    let _ = let_go(&mut editor, ACTION, "amount");
    assert!(run_commit(&mut editor));
    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "slider_draft_begin").len(), 1);
    assert_eq!(events(&records, "slider_draft_set").len(), 3);
    assert_eq!(events(&records, "slider_draft_commit").len(), 1);
    assert!(editor.gesture.is_none());
    assert_eq!(editor.document.history.entries.len(), entries + 1);
    assert_eq!(
        editor.document.state.as_ref().unwrap().current_entry.label,
        "Grain amount 60"
    );
    let committed = json!({"amount": 60.0, "size": 25.0, "roughness": 50.0});
    assert_eq!(grain_row(&editor), Some((committed.clone(), false)));
    assert!(section(&editor).active);

    // Escape cancels a Size drag: no commit, and the field is back at the stored value.
    let log = attach_log(&mut editor);
    let revision = editor.document.state.as_ref().unwrap().revision;
    let _ = slide(&mut editor, ACTION, "size", 80.0);
    assert!(editor.slider_gesture().is_some());
    let escape = escape(&editor);
    assert!(matches!(escape, Message::Draft(DraftMessage::Cancel)));
    let _ = editor.update(escape);
    let records = logged(&mut editor, &log);
    assert_eq!(events(&records, "slider_draft_cancelled").len(), 1);
    assert!(events(&records, "slider_draft_commit").is_empty());
    assert!(editor.gesture.is_none());
    assert_eq!(editor.document.state.as_ref().unwrap().revision, revision);
    assert_eq!(editor.controls.fields.get(ACTION, "size"), Some("25"));
    assert_eq!(grain_row(&editor), Some((committed, false)));

    // A right-click's Copy as JSON request on Roughness is the request an independent client
    // builds for `edit.set-grain`, envelope aside, and that request lands where the field reads it.
    editor.controls.fields.set(ACTION, "roughness", "90".into());
    let _ = editor.update(Message::Action(ActionMessage::CopyRequest {
        action: ACTION.into(),
        parameter: Some("roughness".into()),
        preset: None,
    }));
    assert_eq!(editor.status.text, "Copied the edit.set-grain request");
    let copied = editor.request_for(ACTION, Some("roughness")).unwrap();
    assert_eq!(copied["method"], "edit.set-grain");
    let mutation = json!({
        "expected_revision": revision,
        "request_id": "grain-json-client",
        "actor": "grain-json-client",
    });
    let independent = json!({"asset_id": asset, "mutation": mutation, "roughness": 90.0});
    let mut comparable = copied["params"].clone();
    assert_eq!(comparable["mutation"]["expected_revision"], revision);
    comparable["mutation"] = mutation;
    assert_eq!(comparable, independent);
    tasks::call(&editor.owner, agent, "edit.set-grain", independent).expect("the edit commits");
    refresh(&mut editor, &asset);
    assert_eq!(editor.controls.fields.get(ACTION, "roughness"), Some("90"));
    assert_eq!(
        grain_row(&editor),
        Some((
            json!({"amount": 60.0, "size": 25.0, "roughness": 90.0}),
            false
        ))
    );

    // The dot is Amount's alone: back at 0 the layer is kept but neutral, whatever Size holds.
    agent_sets(
        &mut editor,
        agent,
        &asset,
        json!({"amount": 0.0, "size": 80.0}),
    );
    let (values, neutral) = grain_row(&editor).expect("the layer is kept");
    assert_eq!(
        values,
        json!({"amount": 0.0, "size": 80.0, "roughness": 90.0})
    );
    assert!(neutral);
    assert!(!section(&editor).active, "Amount 0 carries no dot");
    agent_sets(&mut editor, agent, &asset, json!({"amount": 5.0}));
    assert!(section(&editor).active, "any non-zero Amount lights it");
    finish(editor, catalog);
}

/// Off by default, the Grain section is not drawn, its settings group is not offered and the
/// palette lists none of its entries; another client turning the flag on shows all three as soon as
/// the desktop reads the flags again, as its event sync does for a `flags.set`, and turning it off
/// hides them again, with no relaunch.
#[test]
fn the_grain_section_follows_the_live_flag() {
    let (mut editor, root) = super::settings_tests::launch();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(
        testing::descriptors(),
    ))));
    read_flags(&mut editor);
    let offered = |editor: &Editor| {
        let section = editor
            .workspace
            .tools
            .sections
            .iter()
            .any(|section| section.module_id == MODULE);
        let group = crate::state::presets::settings_groups(
            &editor.modules,
            editor.developer,
            editor.settings.flags.as_ref(),
        )
        .groups
        .iter()
        .any(|group| group.module == MODULE);
        let palette = editor
            .workspace
            .palette
            .entries
            .iter()
            .any(|entry| entry.label.starts_with("Grain"));
        (section, group, palette)
    };
    let _ = editor.update(Message::Palette(
        crate::app::message::palette::PaletteMessage::Open,
    ));
    assert_eq!(offered(&editor), (false, false, false), "off by default");

    let agent = editor.owner.register();
    for on in [true, false] {
        tasks::call(
            &editor.owner,
            agent,
            "flags.set",
            json!({"flag": luxforge_core::flags::GRAIN, "value": on}),
        )
        .unwrap();
        read_flags(&mut editor);
        assert_eq!(offered(&editor), (on, on, on), "the flag {on}");
    }
    super::settings_tests::finish(editor, root);
}
