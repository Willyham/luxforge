//! The module layout's rules on their own: placements derived from the built-in descriptors, the
//! merge of a stored order with what this run registers, every refusal, and what each write keeps.
//! The methods, their events and the unchanged renders are the owner's tests.
use super::*;
use crate::{Control, ErrorKind, ModuleLock, ModuleRegistry, ToggleControl};

/// A module that draws a tools-panel section and nothing else.
fn section(id: &str) -> ModuleDescriptor {
    ModuleDescriptor {
        id: id.to_owned(),
        title: id.to_owned(),
        controls: vec![Control::Toggle(ToggleControl {
            action: "set".into(),
            parameter: "on".into(),
            label: "On".into(),
        })],
        ..ModuleDescriptor::default()
    }
}

fn ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

fn adjustments(view: &LayoutView) -> Vec<&str> {
    view.order(Toolset::Adjustments)
        .unwrap()
        .iter()
        .map(String::as_str)
        .collect()
}

/// Each built-in's home, lock and placements, as the design lists them: Crop is a section and the
/// strip's R, Corrections a strip tool, AI editing a Corrections and a mask kind, the RAW
/// development processing only, and every other module a section.
#[test]
fn module_layout_built_ins_declare_their_home_and_derive_their_placements() {
    let registry = ModuleRegistry::builtin();
    let listed: Vec<(&str, Toolset, bool, Vec<Toolset>)> = registry
        .descriptors()
        .into_iter()
        .map(|module| {
            (
                module.id.as_str(),
                module.toolset,
                module.locked.is_some(),
                placements(module)
                    .into_iter()
                    .map(|placement| placement.toolset)
                    .collect(),
            )
        })
        .collect();
    use Toolset::{Adjustments as A, Corrections as C, Masking as M, Processing as P, Tools as T};
    assert_eq!(
        listed,
        [
            ("luxforge.crop", A, true, vec![A, T]),
            ("luxforge.presets", A, false, vec![A]),
            ("luxforge.corrections", T, false, vec![T]),
            ("luxforge.raw", P, true, vec![]),
            ("luxforge.look", A, false, vec![A]),
            ("luxforge.basic", A, true, vec![A]),
            ("luxforge.presence", A, false, vec![A]),
            ("luxforge.curve", A, false, vec![A]),
            ("luxforge.detail", A, false, vec![A]),
            ("luxforge.mixer", A, false, vec![A]),
            ("luxforge.lens", A, false, vec![A]),
            ("luxforge.perspective", A, false, vec![A]),
            ("luxforge.vignette", A, false, vec![A]),
            ("luxforge.grain", A, false, vec![A]),
            ("luxforge.ai", C, false, vec![C, M]),
        ]
    );
    // The host owns Mask's lock: its descriptor is the masking toolset, always on.
    let host = registry.host_descriptors();
    assert!(
        host.iter()
            .all(|host| host.toolset == Toolset::Masking && host.locked.is_some())
    );
}

/// A layout nobody chose follows the default order, registry order with the strip's brush first,
/// with every module on, and stores nothing.
#[test]
fn module_layout_defaults_follow_the_default_order_with_every_module_on() {
    let registry = ModuleRegistry::builtin();
    let arrangement = Arrangement::new(registry.descriptors());
    let view = arrangement.view(None);
    let toolsets: Vec<Toolset> = view.toolsets.iter().map(|entry| entry.toolset).collect();
    assert_eq!(toolsets, Toolset::ORDERABLE);
    for entry in &view.toolsets {
        assert_eq!(entry.order, entry.default, "{:?}", entry.toolset);
    }
    assert_eq!(
        view.order(Toolset::Tools).unwrap(),
        ids(&["luxforge.corrections", "luxforge.crop"])
    );
    assert!(view.disabled.is_empty() && view.unrecognized.is_empty());
    assert!(
        registry
            .descriptors()
            .into_iter()
            .all(|module| arrangement.enabled(None, module))
    );
    assert_eq!(normalized(StoredLayout::default()), None);
}

/// A stored order keeps an id this run does not register and lists it as unrecognized, and a
/// module registered since takes its default place after the registry neighbour before it.
#[test]
fn module_layout_merges_a_stored_order_with_what_this_run_registers() {
    let modules = ["a.one", "a.two", "a.new", "a.three"].map(section);
    let arrangement = Arrangement::new(modules.iter().collect());
    let stored = StoredLayout {
        disabled: ids(&["gone.module"]),
        order: StoredOrders {
            adjustments: ids(&["a.three", "gone.other", "a.one", "a.two"]),
            tools: Vec::new(),
        },
    };
    let view = arrangement.view(Some(&stored));
    assert_eq!(
        adjustments(&view),
        ["a.three", "a.one", "a.two", "a.new"],
        "a.new follows a.two, its registry neighbour"
    );
    assert_eq!(view.unrecognized, ids(&["gone.module", "gone.other"]));
    assert!(
        view.disabled.is_empty(),
        "an unregistered id is not listed off"
    );
    // A new first module goes to the top.
    let modules = ["a.first", "a.one", "a.two"].map(section);
    let arrangement = Arrangement::new(modules.iter().collect());
    let stored = StoredLayout {
        order: StoredOrders {
            adjustments: ids(&["a.two", "a.one"]),
            tools: Vec::new(),
        },
        ..StoredLayout::default()
    };
    assert_eq!(
        adjustments(&arrangement.view(Some(&stored))),
        ["a.first", "a.two", "a.one"]
    );
}

/// Turning a module off stores its id once; turning it on removes it. A locked module and an
/// unregistered id are refused by name, and an unregistered id stored off survives both.
#[test]
fn module_layout_switches_store_only_modules_turned_off() {
    let mut locked = section("a.locked");
    locked.locked = Some(ModuleLock::new("Every photo needs it"));
    let modules = [section("a.one"), locked];
    let arrangement = Arrangement::new(modules.iter().collect());
    let mut stored = StoredLayout {
        disabled: ids(&["gone.module"]),
        ..StoredLayout::default()
    };
    assert!(
        arrangement
            .set_enabled(&mut stored, "a.one", false)
            .unwrap()
    );
    assert!(
        !arrangement
            .set_enabled(&mut stored, "a.one", false)
            .unwrap()
    );
    assert_eq!(stored.disabled, ids(&["gone.module", "a.one"]));
    assert!(!arrangement.enabled(Some(&stored), &modules[0]));
    assert_eq!(arrangement.view(Some(&stored)).disabled, ids(&["a.one"]));
    assert!(arrangement.set_enabled(&mut stored, "a.one", true).unwrap());
    assert!(!arrangement.set_enabled(&mut stored, "a.one", true).unwrap());
    assert_eq!(stored.disabled, ids(&["gone.module"]));
    for (id, enabled, words) in [
        (
            "a.locked",
            false,
            "module a.locked is locked and always on: Every photo needs it",
        ),
        (
            "a.locked",
            true,
            "module a.locked is locked and always on: Every photo needs it",
        ),
        ("gone.module", true, "module gone.module is not registered"),
        ("a.missing", false, "module a.missing is not registered"),
    ] {
        let error = arrangement
            .set_enabled(&mut stored, id, enabled)
            .unwrap_err();
        assert_eq!(
            (error.kind, error.detail.as_str()),
            (ErrorKind::Validation, words)
        );
    }
    assert_eq!(
        stored.disabled,
        ids(&["gone.module"]),
        "a refusal stores nothing"
    );
    // A locked module is on even when a stored layout names it.
    stored.disabled.push("a.locked".into());
    assert!(arrangement.enabled(Some(&stored), &modules[1]));
}

/// `module.order.set`'s rule: only a permutation of an orderable toolset's placements, refused by
/// name otherwise; the default order stores nothing; unrecognized ids keep their stored places.
#[test]
fn module_layout_orders_are_whole_permutations_that_keep_unrecognized_ids() {
    let modules = ["a.one", "a.two", "a.three"].map(section);
    let arrangement = Arrangement::new(modules.iter().collect());
    let mut stored = StoredLayout::default();
    for (toolset, order, words) in [
        (
            Toolset::Corrections,
            ids(&[]),
            "toolset corrections cannot be reordered; the orderable toolsets are adjustments and tools",
        ),
        (
            Toolset::Adjustments,
            ids(&["a.one", "a.two"]),
            "order leaves out a.three: it names every adjustments placement exactly once",
        ),
        (
            Toolset::Adjustments,
            ids(&["a.one", "a.two", "a.two"]),
            "order names a.two twice",
        ),
        (
            Toolset::Adjustments,
            ids(&["a.one", "a.two", "a.three", "a.four"]),
            "order names a.four, which has no adjustments placement",
        ),
        (
            Toolset::Tools,
            ids(&["a.one"]),
            "order names a.one, which has no tools placement",
        ),
    ] {
        let error = arrangement
            .set_order(&mut stored, toolset, &order)
            .unwrap_err();
        assert_eq!(
            (error.kind, error.detail.as_str()),
            (ErrorKind::Validation, words)
        );
    }
    assert_eq!(stored, StoredLayout::default(), "a refusal stores nothing");
    let reversed = ids(&["a.three", "a.two", "a.one"]);
    assert!(
        arrangement
            .set_order(&mut stored, Toolset::Adjustments, &reversed)
            .unwrap()
    );
    assert!(
        !arrangement
            .set_order(&mut stored, Toolset::Adjustments, &reversed)
            .unwrap()
    );
    assert_eq!(stored.order.adjustments, reversed);
    let default = ids(&["a.one", "a.two", "a.three"]);
    assert!(
        arrangement
            .set_order(&mut stored, Toolset::Adjustments, &default)
            .unwrap()
    );
    assert_eq!(
        stored,
        StoredLayout::default(),
        "the default order is no choice"
    );
    // Unrecognized ids keep their stored positions through a new order, and alone through the
    // default one.
    stored.order.adjustments = ids(&["a.one", "gone.x", "a.two", "a.three", "gone.y"]);
    arrangement
        .set_order(&mut stored, Toolset::Adjustments, &reversed)
        .unwrap();
    assert_eq!(
        stored.order.adjustments,
        ids(&["a.three", "gone.x", "a.two", "a.one", "gone.y"])
    );
    arrangement
        .set_order(&mut stored, Toolset::Adjustments, &default)
        .unwrap();
    assert_eq!(stored.order.adjustments, ids(&["gone.x", "gone.y"]));
    assert_eq!(adjustments(&arrangement.view(Some(&stored))), default);
}

/// A reset restores one toolset's order, or every order and switch, keeping what this run does
/// not register.
#[test]
fn module_layout_resets_keep_unrecognized_ids() {
    let modules = ["a.one", "a.two"].map(section);
    let arrangement = Arrangement::new(modules.iter().collect());
    let chosen = StoredLayout {
        disabled: ids(&["a.one", "gone.module"]),
        order: StoredOrders {
            adjustments: ids(&["a.two", "gone.x", "a.one"]),
            tools: ids(&["gone.tool"]),
        },
    };
    let mut stored = chosen.clone();
    assert!(
        arrangement
            .reset(&mut stored, Some(Toolset::Adjustments))
            .unwrap()
    );
    assert_eq!(stored.order.adjustments, ids(&["gone.x"]));
    assert_eq!(
        stored.disabled, chosen.disabled,
        "a toolset's reset keeps the switches"
    );
    assert!(
        !arrangement
            .reset(&mut stored, Some(Toolset::Tools))
            .unwrap(),
        "nothing registered was stored in tools"
    );
    let error = arrangement
        .reset(&mut stored, Some(Toolset::Processing))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
    let mut stored = chosen.clone();
    assert!(arrangement.reset(&mut stored, None).unwrap());
    assert_eq!(
        stored,
        StoredLayout {
            disabled: ids(&["gone.module"]),
            order: StoredOrders {
                adjustments: ids(&["gone.x"]),
                tools: ids(&["gone.tool"]),
            },
        }
    );
    assert!(!arrangement.reset(&mut stored, None).unwrap());
}

/// A stored layout this build cannot use is refused by the list's name.
#[test]
fn module_layout_stored_lists_are_bounded_identities_without_repeats() {
    let refused = |layout: StoredLayout| layout.check().unwrap_err();
    assert!(
        refused(StoredLayout {
            disabled: ids(&["Not An Id"]),
            ..StoredLayout::default()
        })
        .starts_with("module_layout.disabled holds")
    );
    assert_eq!(
        refused(StoredLayout {
            order: StoredOrders {
                adjustments: Vec::new(),
                tools: ids(&["a.one", "a.one"]),
            },
            ..StoredLayout::default()
        }),
        "module_layout.order.tools names a.one twice"
    );
    let many = (0..=MAX_LAYOUT_IDS).map(|at| format!("a.m{at}")).collect();
    assert!(
        refused(StoredLayout {
            disabled: many,
            ..StoredLayout::default()
        })
        .contains("more than")
    );
    let long = format!("a.{}", "x".repeat(MAX_LAYOUT_ID));
    assert!(
        refused(StoredLayout {
            disabled: vec![long],
            ..StoredLayout::default()
        })
        .contains("at most")
    );
}

/// What `module.list` writes reads back as [`ListedModule`]: every built-in descriptor unchanged,
/// with its origin, placements and switch beside it.
#[test]
fn module_layout_listed_modules_read_back_with_their_descriptors() {
    let registry = ModuleRegistry::developer();
    let arrangement = Arrangement::new(registry.descriptors());
    let stored = StoredLayout {
        disabled: ids(&["luxforge.vignette"]),
        ..StoredLayout::default()
    };
    let written =
        serde_json::to_value(arrangement.listing(Some(&stored), registry.descriptors(), |_| None))
            .unwrap();
    let read: Vec<ListedModule> = serde_json::from_value(written).unwrap();
    assert_eq!(read.len(), registry.descriptors().len());
    for (listed, descriptor) in read.iter().zip(registry.descriptors()) {
        assert_eq!(&listed.descriptor, descriptor);
        assert_eq!(listed.origin, ModuleOrigin::BuiltIn);
        assert_eq!(listed.placements, placements(descriptor));
        assert_eq!(listed.enabled, descriptor.id != "luxforge.vignette");
    }
}
