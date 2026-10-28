//! The desktop's derivation of the person's module layout, over the built-in registry and variants
//! of it: letters, switches, moves and the words each region draws.
use super::*;
use crate::state::{host_commands::action_shortcuts, testing::descriptors, tools::mode_shortcuts};
use luxforge_core::{
    CanvasInteraction, Chord,
    module_layout::{LayoutView, ToolsetOrder},
};

const CROP: &str = "luxforge.crop";
const CORRECTIONS: &str = "luxforge.corrections";
const VIGNETTE: &str = "luxforge.vignette";
const BASIC: &str = "luxforge.basic";

fn ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

/// A layout as `module.layout.read` would answer it for `modules`: `tools` and `adjustments` in
/// the given orders, and `disabled` off.
fn view(adjustments: &[&str], tools: &[&str], disabled: &[&str]) -> LayoutView {
    LayoutView {
        toolsets: vec![
            ToolsetOrder {
                toolset: Toolset::Adjustments,
                order: ids(adjustments),
                default: ids(adjustments),
            },
            ToolsetOrder {
                toolset: Toolset::Tools,
                order: ids(tools),
                default: ids(tools),
            },
        ],
        disabled: ids(disabled),
        unrecognized: Vec::new(),
    }
}

fn module<'a>(modules: &'a mut [ModuleDescriptor], id: &str) -> &'a mut ModuleDescriptor {
    modules
        .iter_mut()
        .find(|module| module.id == id)
        .unwrap_or_else(|| panic!("{id} is registered"))
}

fn set_shortcut(module: &mut ModuleDescriptor, letter: &str) {
    match module.canvas.as_mut().expect("a canvas") {
        CanvasInteraction::CropFrame { shortcut, .. }
        | CanvasInteraction::BrushMask { shortcut, .. }
        | CanvasInteraction::PointPick { shortcut, .. }
        | CanvasInteraction::SampleApply { shortcut, .. } => *shortcut = Some(letter.into()),
        CanvasInteraction::AreaPick { .. } => panic!("an area pick declares no letter"),
    }
}

#[test]
fn module_layout_moves_step_past_the_drawn_neighbour_or_reach_either_end() {
    let order = ids(&["a", "b", "off", "c", "d"]);
    // `off` is in the order but not drawn: moving `c` up passes `b`, not the module that is off.
    let shown = ["a", "b", "c", "d"];
    assert_eq!(
        moved(&order, &shown, "c", Move::Up),
        Some(ids(&["a", "c", "b", "off", "d"]))
    );
    assert_eq!(
        moved(&order, &shown, "b", Move::Down),
        Some(ids(&["a", "off", "c", "b", "d"]))
    );
    assert_eq!(
        moved(&order, &shown, "c", Move::Top),
        Some(ids(&["c", "a", "b", "off", "d"]))
    );
    assert_eq!(
        moved(&order, &shown, "b", Move::Bottom),
        Some(ids(&["a", "off", "c", "d", "b"]))
    );
    // A move that changes nothing sends nothing, and an id the order does not hold moves nowhere.
    assert_eq!(moved(&order, &shown, "a", Move::Up), None);
    assert_eq!(moved(&order, &shown, "a", Move::Top), None);
    assert_eq!(moved(&order, &shown, "d", Move::Down), None);
    assert_eq!(moved(&order, &shown, "d", Move::Bottom), None);
    assert_eq!(moved(&order, &shown, "gone", Move::Top), None);
}

#[test]
fn module_layout_letters_go_to_host_entries_first_then_registry_order() {
    let mut modules = descriptors();
    // A variant registry: a second crop frame after Crop declares Crop's R, and Corrections
    // declares the host's M. Registration refuses both between modules; the desktop's rule is what
    // a later kind of module meets.
    let mut frame = module(&mut modules, CROP).clone();
    frame.id = "test.frame".into();
    frame.locked = None;
    modules.push(frame);
    set_shortcut(module(&mut modules, CORRECTIONS), "M");
    let mut layout = EffectiveLayout::default();
    layout.adopt_descriptors(&modules);

    assert_eq!(layout.letter(CROP), Some(&LetterClaim::Holds('R')));
    let crop_title = module(&mut modules, CROP)
        .canvas
        .as_ref()
        .unwrap()
        .title()
        .to_owned();
    let taken = layout.letter("test.frame").expect("a claim");
    assert_eq!(
        taken,
        &LetterClaim::Taken {
            letter: 'R',
            owner: crop_title.clone()
        }
    );
    assert_eq!(taken.note(), Some(format!("R is {crop_title}'s")));
    assert_eq!(
        layout.letter(CORRECTIONS).and_then(LetterClaim::note),
        Some("M is Mask's".to_owned()),
        "the host's letters come before every module's"
    );
    // The keyboard binds only the letters held.
    let bound = mode_shortcuts(&modules, None, None, &layout);
    assert!(bound.contains(&('R', CROP.to_owned())), "{bound:?}");
    assert!(
        bound
            .iter()
            .all(|(_, mode)| mode != "test.frame" && mode != CORRECTIONS),
        "{bound:?}"
    );
}

#[test]
fn module_layout_an_off_module_gives_up_its_letter_and_its_chords() {
    let mut modules = descriptors();
    // Basic is locked in the built-in registry; a variant unlocks it so it can be turned off and
    // its declared chord followed.
    module(&mut modules, BASIC).locked = None;
    let mut layout = EffectiveLayout::default();
    layout.adopt_descriptors(&modules);
    let on = mode_shortcuts(&modules, None, None, &layout);
    assert!(
        on.iter()
            .any(|(letter, mode)| *letter == 'Q' && mode == CORRECTIONS)
    );
    assert_eq!(
        action_shortcuts(&modules, &layout),
        [(Chord::command('U'), "auto-tone".to_owned())]
    );

    layout.adopt_view(
        view(&[], &[CORRECTIONS, CROP], &[CORRECTIONS, BASIC]),
        &modules,
    );
    assert!(!layout.enabled(CORRECTIONS) && !layout.enabled(BASIC));
    let off = mode_shortcuts(&modules, None, None, &layout);
    assert!(off.iter().all(|(letter, _)| *letter != 'Q'), "{off:?}");
    assert!(off.iter().any(|(letter, _)| *letter == 'R'), "{off:?}");
    assert!(action_shortcuts(&modules, &layout).is_empty());
    // Nothing else takes the letter while its holder is off.
    assert_eq!(layout.letter(CORRECTIONS), Some(&LetterClaim::Holds('Q')));
}

#[test]
fn module_layout_a_locked_module_is_always_on_whatever_a_read_says() {
    let modules = descriptors();
    let mut layout = EffectiveLayout::default();
    layout.adopt_view(view(&[], &[], &[CROP, VIGNETTE]), &modules);
    assert!(layout.enabled(CROP), "Crop is locked");
    assert!(layout.lock(CROP).is_some());
    assert!(!layout.enabled(VIGNETTE));
    assert_eq!(
        layout
            .off(&modules)
            .map(|module| module.id.as_str())
            .collect::<Vec<_>>(),
        [VIGNETTE]
    );
}

#[test]
fn module_layout_sorting_follows_the_persons_order_and_waits_for_the_read() {
    let modules = descriptors();
    let mut layout = EffectiveLayout::default();
    layout.adopt_descriptors(&modules);
    let mut items = vec!["a", "b", "c"];
    layout.sort(Toolset::Adjustments, &mut items, |item| item);
    assert_eq!(
        items,
        ["a", "b", "c"],
        "nothing moves before the layout is read"
    );
    layout.adopt_view(view(&["c", "a"], &[], &[]), &modules);
    layout.sort(Toolset::Adjustments, &mut items, |item| item);
    assert_eq!(
        items,
        ["c", "a", "b"],
        "an id the order omits keeps its place after"
    );
}

#[test]
fn module_layout_band_menus_offer_turn_off_only_for_an_unlocked_module_that_is_on() {
    let modules = descriptors();
    let mut layout = EffectiveLayout::default();
    let order = [CROP, BASIC, VIGNETTE];
    layout.adopt_view(view(&order, &[], &[]), &modules);
    let find = |id: &str| modules.iter().find(|module| module.id == id).unwrap();

    let basic = band_menu(&layout, find(BASIC), &order);
    assert_eq!(basic.turn_off, None, "Basic is locked");
    assert_eq!(basic.enable, None);
    assert_eq!(
        basic.moves,
        Move::ALL
            .into_iter()
            .map(|to| (to, true))
            .collect::<Vec<_>>()
    );
    let vignette = band_menu(&layout, find(VIGNETTE), &order);
    assert_eq!(vignette.turn_off.as_deref(), Some("Turn off Vignette"));
    assert_eq!(
        vignette.moves,
        [
            (Move::Up, true),
            (Move::Down, false),
            (Move::Top, true),
            (Move::Bottom, false)
        ]
    );
    // Its Copy as JSON request lists every request the menu would send.
    let requests = band_requests(&layout, find(VIGNETTE), &order);
    let methods: Vec<&str> = requests
        .as_array()
        .unwrap()
        .iter()
        .map(|request| request["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        ["module.order.set", "module.order.set", "module.disable"]
    );
    assert_eq!(
        requests[0]["params"],
        json!({"toolset": "adjustments", "order": [CROP, VIGNETTE, BASIC]})
    );

    layout.adopt_view(view(&order, &[], &[VIGNETTE]), &modules);
    let kept = band_menu(&layout, find(VIGNETTE), &order);
    assert_eq!(kept.turn_off, None);
    assert_eq!(kept.enable.as_deref(), Some("Enable Vignette"));
    layout.pending = true;
    assert!(!band_menu(&layout, find(VIGNETTE), &order).enabled);
}

#[test]
fn module_layout_the_strip_menu_lists_every_tool_with_its_check_or_lock() {
    let modules = descriptors();
    let mut layout = EffectiveLayout::default();
    layout.adopt_view(view(&[], &[CROP, CORRECTIONS], &[CORRECTIONS]), &modules);
    let menu = strip_menu(&layout, &modules, false, true);
    let rows: Vec<(Option<&str>, Option<char>, bool, bool)> = menu
        .tools
        .iter()
        .map(|tool| (tool.module_id.as_deref(), tool.letter, tool.on, tool.locked))
        .collect();
    assert_eq!(
        rows,
        [
            (None, Some('M'), true, true),
            (Some(CROP), Some('R'), true, true),
            // Off tools stay listed, unchecked, so they can always be found.
            (Some(CORRECTIONS), Some('Q'), false, false),
        ]
    );
    assert!(menu.open && menu.enabled);
}

#[test]
fn module_layout_status_and_footer_words() {
    assert_eq!(off_footer(&[]), None);
    assert_eq!(
        off_footer(&["Perspective", "Vignette"]).as_deref(),
        Some("2 modules off: Perspective, Vignette")
    );
    assert_eq!(
        off_footer(&["Perspective"]).as_deref(),
        Some("1 module off: Perspective")
    );
    assert_eq!(
        switched_status("Vignette", false),
        "Turned off Vignette \u{b7} rendering unchanged"
    );
    let titles: BTreeMap<&str, &str> = [("curve", "Tone curve"), ("presence", "Presence")]
        .into_iter()
        .collect();
    let order = ids(&["basic", "curve", "presence"]);
    assert_eq!(
        moved_status(
            &titles,
            &order,
            &["basic", "curve", "presence"],
            "curve",
            "panel"
        ),
        "Moved Tone curve above Presence in your panel \u{b7} rendering unchanged"
    );
}

#[test]
fn module_layout_kept_values_read_as_their_controls_show_them() {
    let modules = descriptors();
    let vignette = modules.iter().find(|module| module.id == VIGNETTE).unwrap();
    let row = luxforge_core::LayerDescription {
        id: luxforge_core::LayerId::new(),
        effect: luxforge_core::VIGNETTE_EFFECT.into(),
        module: Some(VIGNETTE.into()),
        title: Some("Vignette".into()),
        summary: String::new(),
        values: serde_json::from_value(json!({"amount": -35.0})).unwrap(),
        available: true,
        mask: None,
        artifacts: Vec::new(),
        neutral: false,
        input_stage: None,
        input_orientation: None,
        stale: Vec::new(),
    };
    let values = kept_values(vignette, &[&row]);
    assert_eq!(values.len(), 1, "{values:?}");
    let (label, value) = &values[0];
    assert_eq!(label, "Amount");
    assert!(value.starts_with("-35"), "{value}");
}
