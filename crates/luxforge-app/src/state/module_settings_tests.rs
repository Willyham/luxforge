//! Settings › Modules over the built-in registry: grouping by home, the person's order, switches,
//! locks and followed switches, the detail pane, moves, drops, resets and their Undo writes.
use super::*;
use luxforge_core::{
    CanvasInteraction, ModuleRegistry,
    module_layout::{Placement, placements},
};

/// What `module.list` and `module.layout.read` answer for the built-in registry with `disabled`
/// off and every order the default, built from the core's own placement rule.
fn read(disabled: &[&str]) -> ModulesRead {
    let registry = ModuleRegistry::builtin();
    let descriptors: Vec<ModuleDescriptor> = registry.descriptors().into_iter().cloned().collect();
    let modules: Vec<ListedModule> = descriptors
        .iter()
        .map(|descriptor| ListedModule {
            descriptor: descriptor.clone(),
            origin: ModuleOrigin::BuiltIn,
            placements: placements(descriptor),
            enabled: !disabled.contains(&descriptor.id.as_str()),
        })
        .collect();
    let placed = |toolset: Toolset| -> Vec<String> {
        let (brushes, others): (Vec<&ListedModule>, Vec<&ListedModule>) = modules
            .iter()
            .filter(|module| module.placements.contains(&Placement { toolset }))
            .partition(|module| {
                toolset == Toolset::Tools
                    && matches!(
                        module.descriptor.canvas,
                        Some(CanvasInteraction::BrushMask { .. })
                    )
            });
        brushes
            .into_iter()
            .chain(others)
            .map(|module| module.descriptor.id.clone())
            .collect()
    };
    let layout = LayoutView {
        toolsets: Toolset::ORDERABLE
            .into_iter()
            .map(|toolset| ToolsetOrder {
                toolset,
                order: placed(toolset),
                default: placed(toolset),
            })
            .collect(),
        disabled: disabled.iter().map(|id| (*id).to_owned()).collect(),
        unrecognized: Vec::new(),
    };
    ModulesRead {
        modules,
        host: registry.host_descriptors().into_iter().cloned().collect(),
        layout,
        packages: crate::state::external_modules::PackageList::default(),
    }
}

fn tab(disabled: &[&str]) -> ModulesTab {
    ModulesTab {
        read: Some(read(disabled)),
        ..ModulesTab::default()
    }
}

fn row(toolset: Toolset, module: &str) -> RowKey {
    RowKey {
        toolset,
        module: module.into(),
    }
}

fn group(model: &ModulesModel, toolset: Toolset) -> &ModuleGroup {
    model
        .groups
        .iter()
        .find(|group| group.toolset == toolset)
        .expect("the group is listed")
}

fn ids(group: &ModuleGroup) -> Vec<&str> {
    group
        .rows
        .iter()
        .map(|row| row.key.module.as_str())
        .collect()
}

fn adjustments(tab: &ModulesTab) -> Vec<String> {
    tab.layout().unwrap().toolsets[0].order.clone()
}

#[test]
fn settings_modules_groups_every_module_at_home_in_the_persons_order() {
    let model = derive(&tab(&[]));
    assert_eq!(
        model
            .groups
            .iter()
            .map(|group| group.toolset)
            .collect::<Vec<_>>(),
        Toolset::ALL,
        "Adjustments, Corrections, Tools, Masking, then Processing only"
    );
    let panel = group(&model, Toolset::Adjustments);
    assert_eq!(
        ids(panel),
        [
            "luxforge.crop",
            "luxforge.presets",
            "luxforge.look",
            "luxforge.basic",
            "luxforge.presence",
            "luxforge.curve",
            "luxforge.detail",
            "luxforge.mixer",
            "luxforge.lens",
            "luxforge.perspective",
            "luxforge.vignette",
            "luxforge.grain",
        ]
    );
    assert!(panel.reorderable && !panel.can_reset);
    assert!(panel.rows.iter().all(|row| row.grip));
    let look = &panel.rows[2];
    assert_eq!(look.qualifier.as_deref(), Some("RAW photos only"));
    assert_eq!(look.control, RowControl::Switch(true));
    assert!(
        matches!(&panel.rows[0].control, RowControl::Lock(reason) if reason == "Every photo's geometry")
    );
    assert!(
        matches!(&panel.rows[3].control, RowControl::Lock(_)),
        "Basic is locked"
    );

    let strip = group(&model, Toolset::Tools);
    assert_eq!(
        ids(strip),
        ["luxforge.masks", "luxforge.corrections", "luxforge.crop"]
    );
    let (mask, corrections, crop) = (&strip.rows[0], &strip.rows[1], &strip.rows[2]);
    assert_eq!(
        (mask.title.as_str(), mask.letter.as_deref()),
        ("Mask", Some("M"))
    );
    assert!(
        !mask.grip && matches!(mask.control, RowControl::Lock(_)),
        "the strip fixes Mask"
    );
    assert_eq!(corrections.letter.as_deref(), Some("Q"));
    assert_eq!(
        corrections.control,
        RowControl::Switch(true),
        "Corrections' home is Tools"
    );
    assert_eq!(
        (
            crop.title.as_str(),
            crop.letter.as_deref(),
            crop.qualifier.as_deref()
        ),
        ("Crop", Some("R"), Some("of Crop, transform, straighten"))
    );
    assert!(crop.grip, "a locked module can still be moved");

    let corrections = group(&model, Toolset::Corrections);
    assert!(!corrections.reorderable && corrections.rows.iter().all(|row| !row.grip));
    assert_eq!(ids(corrections), ["luxforge.ai"]);
    assert_eq!(corrections.rows[0].control, RowControl::Switch(true));
    let masking = group(&model, Toolset::Masking);
    assert_eq!(ids(masking), ["luxforge.masks", "luxforge.ai"]);
    assert_eq!(
        masking.rows[1].control,
        RowControl::Follows(true),
        "AI editing's object kind follows its switch in Corrections"
    );
    let processing = group(&model, Toolset::Processing);
    assert_eq!(ids(processing), ["luxforge.raw"]);
    assert!(matches!(processing.rows[0].control, RowControl::Lock(_)));
}

#[test]
fn settings_modules_draws_an_off_module_dimmed_in_place_and_a_changed_order_with_reset() {
    let mut tab = tab(&["luxforge.vignette"]);
    let order = &mut tab.read.as_mut().unwrap().layout.toolsets[0].order;
    let curve = order.iter().position(|id| id == "luxforge.curve").unwrap();
    let presence = order.remove(curve - 1);
    order.insert(curve, presence);
    let model = derive(&tab);
    let panel = group(&model, Toolset::Adjustments);
    assert_eq!(ids(panel)[4..6], ["luxforge.curve", "luxforge.presence"]);
    assert!(panel.can_reset && model.can_reset_all);
    let vignette = panel
        .rows
        .iter()
        .find(|row| row.key.module == "luxforge.vignette")
        .unwrap();
    assert!(vignette.off);
    assert_eq!(vignette.control, RowControl::Switch(false));
    assert!(!group(&model, Toolset::Tools).can_reset);
}

#[test]
fn settings_modules_switch_writes_once_shows_at_once_and_undo_writes_back() {
    let mut tab = tab(&[]);
    let (write, toast) = tab.toggled("luxforge.vignette").unwrap().unwrap();
    assert_eq!(write, LayoutWrite::Disable("luxforge.vignette".into()));
    assert_eq!(
        (write.method(), write.params()),
        ("module.disable", json!({"module": "luxforge.vignette"}))
    );
    assert_eq!(toast.text, "Turned Vignette off");
    assert_eq!(
        toast.undo,
        [LayoutWrite::Enable("luxforge.vignette".into())]
    );
    tab.writing = Some(write);
    let model = derive(&tab);
    assert!(
        group(&model, Toolset::Adjustments)
            .rows
            .iter()
            .any(|row| row.key.module == "luxforge.vignette" && row.off),
        "the change shows before its answer lands"
    );
    assert!(!tab.idle());
    assert_eq!(
        tab.switched("luxforge.vignette", false).unwrap(),
        None,
        "a switch already where it is asked to be writes nothing"
    );
    assert_eq!(
        tab.toggled("luxforge.basic").unwrap_err(),
        "Basic is always on: Carries the RAW development's white balance and exposure"
    );
}

#[test]
fn settings_modules_moves_and_drops_write_the_whole_order() {
    let tab = tab(&[]);
    let curve = row(Toolset::Adjustments, "luxforge.curve");
    let (write, toast) = tab.moved(&curve, Move::Up).unwrap();
    let LayoutWrite::Order { toolset, order } = &write else {
        panic!("an order write")
    };
    assert_eq!(*toolset, Toolset::Adjustments);
    let mut expected = adjustments(&tab);
    expected.swap(4, 5);
    assert_eq!(order, &expected);
    assert_eq!(toast.text, "Moved Tone curve above Presence");
    assert_eq!(
        toast.undo,
        [LayoutWrite::Order {
            toolset: Toolset::Adjustments,
            order: adjustments(&tab),
        }]
    );
    assert_eq!(
        tab.moved(&curve, Move::Down).unwrap().1.text,
        "Moved Tone curve below Detail"
    );
    let (top, toast) = tab.moved(&curve, Move::Top).unwrap();
    assert!(matches!(top, LayoutWrite::Order { order, .. } if order[0] == "luxforge.curve"));
    assert_eq!(toast.text, "Moved Tone curve to the top");
    let crop = row(Toolset::Adjustments, "luxforge.crop");
    assert_eq!(
        tab.moved(&crop, Move::Up),
        None,
        "the top row cannot move up"
    );
    assert!(
        tab.moved(&crop, Move::Bottom).is_some(),
        "a locked module moves"
    );
    let mask = row(Toolset::Tools, "luxforge.masks");
    assert!(!tab.movable(&mask), "the strip fixes Mask");
    assert!(!tab.movable(&row(Toolset::Masking, "luxforge.ai")));

    let (dropped, toast) = tab.dropped(&curve, "luxforge.presence").unwrap();
    assert_eq!(dropped, tab.moved(&curve, Move::Up).unwrap().0);
    assert_eq!(toast.text, "Moved Tone curve above Presence");
    assert_eq!(
        tab.dropped(&curve, "luxforge.curve"),
        None,
        "a drop in place writes nothing"
    );
    let tools = row(Toolset::Tools, "luxforge.crop");
    let (strip, toast) = tab.dropped(&tools, "luxforge.corrections").unwrap();
    assert_eq!(
        strip,
        LayoutWrite::Order {
            toolset: Toolset::Tools,
            order: vec!["luxforge.crop".into(), "luxforge.corrections".into()],
        }
    );
    assert_eq!(toast.text, "Moved Crop to the top");
}

#[test]
fn settings_modules_draws_a_drag_inside_its_group_only() {
    let mut tab = tab(&[]);
    tab.drag = Some(ModuleDrag {
        row: row(Toolset::Adjustments, "luxforge.curve"),
        over: Some("luxforge.presence".into()),
    });
    let model = derive(&tab);
    assert!(model.dragging);
    let panel = group(&model, Toolset::Adjustments);
    let at = |id: &str| panel.rows.iter().find(|row| row.key.module == id).unwrap();
    assert!(at("luxforge.curve").dragged);
    assert_eq!(at("luxforge.presence").drop, Some(DropSide::Above));
    assert!(panel.rows.iter().all(|row| row.drop_target));
    assert!(
        group(&model, Toolset::Tools)
            .rows
            .iter()
            .all(|row| !row.drop_target),
        "a row cannot be dropped among the strip's tools"
    );
}

#[test]
fn settings_modules_resets_say_what_they_restore_and_undo_writes_it_back() {
    let mut tab = tab(&["luxforge.vignette", "luxforge.mixer"]);
    assert_eq!(
        tab.reset(Some(Toolset::Adjustments)),
        None,
        "a default order resets nothing"
    );
    let changed = tab
        .moved(&row(Toolset::Adjustments, "luxforge.curve"), Move::Up)
        .unwrap()
        .0;
    tab.waiting.push_back(changed.clone());
    let (write, toast) = tab.reset(Some(Toolset::Adjustments)).unwrap();
    assert_eq!(write, LayoutWrite::Reset(Some(Toolset::Adjustments)));
    assert_eq!(write.params(), json!({"toolset": "adjustments"}));
    assert_eq!(toast.text, "Reset the Adjustments order");
    assert_eq!(toast.undo, std::slice::from_ref(&changed));
    let (all, toast) = tab.reset(None).unwrap();
    assert_eq!(
        (all.method(), all.params()),
        ("module.layout.reset", json!({}))
    );
    assert_eq!(toast.text, "Reset every module");
    assert_eq!(
        toast.undo,
        [
            changed,
            LayoutWrite::Disable("luxforge.vignette".into()),
            LayoutWrite::Disable("luxforge.mixer".into()),
        ]
    );
    tab.waiting.push_back(all);
    let model = derive(&tab);
    assert!(
        !model.can_reset_all,
        "every module shows on, in its default place"
    );
    assert!(
        group(&model, Toolset::Adjustments)
            .rows
            .iter()
            .all(|row| !row.off)
    );
}

#[test]
fn settings_modules_detail_says_where_it_lives_renders_and_its_api() {
    let mut tab = tab(&[]);
    tab.selected = Some(row(Toolset::Adjustments, "luxforge.vignette"));
    let detail = derive(&tab).detail.unwrap();
    assert_eq!(
        (detail.title.as_str(), detail.origin),
        ("Vignette", "Built in")
    );
    assert!(detail.hint.is_some() && detail.shown.is_none());
    assert_eq!(detail.lives, ["Adjustments \u{b7} tools panel, 11th"]);
    assert_eq!(
        detail
            .stages
            .iter()
            .filter(|(_, own)| *own)
            .map(|(name, _)| *name)
            .collect::<Vec<_>>(),
        ["Finish"]
    );
    assert_eq!(detail.stages.len(), 7);
    assert!(detail.renders.ends_with(ORDER_NOTE));
    assert!(
        detail
            .photos
            .as_deref()
            .is_some_and(|photos| photos.contains("Vignette"))
    );
    assert!(
        detail
            .api
            .iter()
            .any(|method| method == "edit.set-vignette")
    );

    tab.selected = Some(row(Toolset::Tools, "luxforge.crop"));
    let crop = derive(&tab).detail.unwrap();
    assert_eq!(
        crop.lives,
        [
            "Adjustments \u{b7} tools panel, 1st",
            "Tools \u{b7} mode strip, last"
        ]
    );
    assert_eq!(
        crop.shown.as_deref(),
        Some("Always on: Every photo's geometry. You can still move it.")
    );
    assert_eq!(crop.photos, None, "a locked module is never off");

    tab.selected = Some(row(Toolset::Masking, "luxforge.masks"));
    let masks = derive(&tab).detail.unwrap();
    assert_eq!(masks.origin, "Part of Luxforge");
    assert!(masks.api.iter().any(|method| method == "mask.list"));
    assert_eq!(
        masks.shown.as_deref(),
        Some("Always on: Masks belong to the host.")
    );

    tab.refusal = Some((Some("luxforge.masks".into()), "no".into()));
    assert_eq!(derive(&tab).detail.unwrap().refusal.as_deref(), Some("no"));
}

#[test]
fn settings_modules_selection_steps_through_every_row_and_focus_finds_home() {
    let mut tab = tab(&[]);
    assert_eq!(
        tab.step_selection(1),
        Some(row(Toolset::Adjustments, "luxforge.crop"))
    );
    tab.selected = Some(row(Toolset::Adjustments, "luxforge.grain"));
    assert_eq!(
        tab.step_selection(1),
        Some(row(Toolset::Corrections, "luxforge.ai"))
    );
    assert_eq!(
        tab.step_selection(-1),
        Some(row(Toolset::Adjustments, "luxforge.vignette"))
    );
    tab.selected = Some(row(Toolset::Processing, "luxforge.raw"));
    assert_eq!(
        tab.step_selection(1),
        tab.selected,
        "the last row stays selected"
    );
    assert_eq!(
        tab.resolve(&ModulesFocus::Module("luxforge.crop".into())),
        Some(row(Toolset::Adjustments, "luxforge.crop")),
        "a module's focus is its home row"
    );
    assert_eq!(
        tab.resolve(&ModulesFocus::Module("luxforge.corrections".into())),
        Some(row(Toolset::Tools, "luxforge.corrections"))
    );
    assert_eq!(
        tab.resolve(&ModulesFocus::Toolset(Toolset::Tools)),
        Some(row(Toolset::Tools, "luxforge.masks"))
    );
    assert_eq!(
        derive(&ModulesTab::default()),
        ModulesModel {
            loading: true,
            ..ModulesModel::default()
        }
    );
}
