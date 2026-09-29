//! Control variants: the checks the complete registry makes, the one resolver and the superseded
//! fields both derive.
use super::tests::TestModule;
use crate::{
    ActionControl, BasicModule, Control, ControlVariant, ErrorKind, GroupControl, MaskId,
    ModuleDescriptor, ModuleRegistry, NumberControl, PickerControl, RawModule, ResetAction,
    SourceTag, ToolModule, modules::linked_modules, resolve_control, resolve_group_reset,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// An edit of Basic's descriptor a registry is built with once.
type EditOnce = Box<dyn FnOnce(&mut ModuleDescriptor)>;
/// An edit of a descriptor checked on its own.
type Edit = Box<dyn Fn(&mut ModuleDescriptor)>;

/// A registry of the built-in modules with `edit` applied to the descriptor of `module`.
fn registry_with(module: &str, edit: impl FnOnce(&mut ModuleDescriptor)) -> ModuleRegistry {
    let mut registry = ModuleRegistry::new();
    let mut edit = Some(edit);
    for provider in linked_modules(false) {
        let provider: Arc<dyn ToolModule> = if provider.descriptor().id == module {
            let mut descriptor = provider.descriptor().clone();
            (edit.take().expect("one module is edited"))(&mut descriptor);
            TestModule::from_descriptor(descriptor)
        } else {
            provider
        };
        registry
            .register(provider)
            .expect("each descriptor is valid on its own");
    }
    registry
}

/// Basic's White balance group, which carries every built-in variant.
fn white_balance(descriptor: &mut ModuleDescriptor) -> &mut Vec<Control> {
    match &mut descriptor.controls[0] {
        Control::Group(GroupControl { controls, .. }) => controls,
        _ => panic!("Basic's first control is the White balance group"),
    }
}

fn variant_of(control: &mut Control) -> &mut ControlVariant {
    match control {
        Control::Number(NumberControl { variants, .. })
        | Control::Action(ActionControl { variants, .. })
        | Control::Picker(PickerControl { variants, .. })
        | Control::Group(GroupControl { variants, .. }) => &mut variants[0],
        _ => panic!("a control that carries variants"),
    }
}

#[test]
fn the_built_in_variants_validate_against_the_complete_registry() {
    let registry = ModuleRegistry::builtin();
    registry
        .check_complete()
        .expect("Basic's variants are valid");
    // Registered unavailable, a module keeps its descriptor, so the variants still validate.
    let mut disabled = ModuleRegistry::new();
    for provider in linked_modules(false) {
        if provider.descriptor().id == RawModule::new().descriptor().id {
            disabled
                .register_unavailable(provider, "switched off")
                .unwrap();
        } else {
            disabled.register(provider).unwrap();
        }
    }
    disabled
        .check_complete()
        .expect("an unavailable RAW module");
}

#[test]
fn a_variant_the_named_module_cannot_provide_is_refused_once_the_registry_is_complete() {
    let basic = BasicModule::new().descriptor().id.clone();
    let cases: Vec<(&str, EditOnce, &str)> = vec![
        (
            "an unknown action",
            Box::new(|descriptor| {
                if let Some(Control::Number(NumberControl { action, .. })) =
                    variant_of(&mut white_balance(descriptor)[0])
                        .control
                        .as_deref_mut()
                {
                    *action = "set-raw-temperature".into();
                }
            }),
            "references undeclared action set-raw-temperature",
        ),
        (
            "an unknown parameter",
            Box::new(|descriptor| {
                if let Some(Control::Number(NumberControl { parameter, .. })) =
                    variant_of(&mut white_balance(descriptor)[1])
                        .control
                        .as_deref_mut()
                {
                    *parameter = "kelvin".into();
                }
            }),
            "action set-raw has no parameter kelvin",
        ),
        (
            "an unregistered module",
            Box::new(|descriptor| {
                variant_of(&mut white_balance(descriptor)[0]).module = "test.nobody".into();
            }),
            "names unregistered module test.nobody",
        ),
        (
            "a module that does not apply to the kind",
            Box::new(|descriptor| {
                let variant = variant_of(&mut white_balance(descriptor)[2]);
                variant.source = SourceTag::Jpeg;
            }),
            "which does not apply to a JPEG photo",
        ),
        (
            "a reset of an unknown action",
            Box::new(|descriptor| {
                let variant = variant_of(&mut descriptor.controls[0]);
                variant.reset = Some(ResetAction {
                    action: "reset-raw".into(),
                    preset: Default::default(),
                });
            }),
            "references undeclared action reset-raw",
        ),
    ];
    for (case, edit, fragment) in cases {
        let registry = registry_with(&basic, edit);
        let error = registry.check_complete().expect_err(case);
        assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        assert!(error.detail.contains(fragment), "{case}: {}", error.detail);
    }
}

/// What a descriptor can say about its own variants is checked when it registers: a control of
/// another shape, a second variant for one kind, a variant naming its own module, and a group
/// reset variant on a group without a reset.
#[test]
fn a_malformed_variant_is_refused_by_the_descriptor_alone() {
    let base = BasicModule::new().descriptor().clone();
    let cases: Vec<(&str, Edit, &str)> = vec![
        (
            "a control of another shape",
            Box::new(|descriptor| {
                variant_of(&mut white_balance(descriptor)[0]).control =
                    Some(Box::new(Control::picker("Neutral picker").into()));
            }),
            "is a picker control",
        ),
        (
            "a second variant for one kind",
            Box::new(|descriptor| {
                if let Control::Number(NumberControl { variants, .. }) =
                    &mut white_balance(descriptor)[0]
                {
                    variants.push(variants[0].clone());
                }
            }),
            "declares two variants for RAW",
        ),
        (
            "its own module",
            Box::new(|descriptor| {
                let own = descriptor.id.clone();
                variant_of(&mut white_balance(descriptor)[0]).module = own;
            }),
            "must name another module",
        ),
        (
            "a reset on a control",
            Box::new(|descriptor| {
                let variant = variant_of(&mut white_balance(descriptor)[3]);
                variant.reset = variant.control.take().map(|_| ResetAction::default());
            }),
            "needs a control and no reset",
        ),
        (
            "a control on a group",
            Box::new(|descriptor| {
                let variant = variant_of(&mut descriptor.controls[0]);
                variant.control = Some(Box::new(Control::picker("Pick").into()));
            }),
            "needs a reset and no control",
        ),
        (
            "a reset variant without a reset",
            Box::new(|descriptor| {
                if let Control::Group(GroupControl { reset, .. }) = &mut descriptor.controls[0] {
                    *reset = None;
                }
            }),
            "declares a RAW reset variant but no reset",
        ),
        (
            "a variant of a variant",
            Box::new(|descriptor| {
                let variant = variant_of(&mut white_balance(descriptor)[2]);
                let nested = variant.clone();
                if let Some(Control::Picker(PickerControl { variants, .. })) =
                    variant.control.as_deref_mut()
                {
                    variants.push(nested);
                }
            }),
            "declares variants of its own",
        ),
    ];
    for (case, edit, fragment) in cases {
        let mut descriptor = base.clone();
        edit(&mut descriptor);
        let error = descriptor.validate().expect_err(case);
        assert_eq!(error.kind, ErrorKind::Validation, "{case}");
        assert!(error.detail.contains(fragment), "{case}: {}", error.detail);
    }
}

/// The RAW module declares a pick canvas and no controls: valid on its own, and valid in a
/// registry only because Basic's picker variant reaches it.
#[test]
fn a_pick_canvas_without_its_own_picker_needs_a_variant_that_reaches_it() {
    let raw = RawModule::new();
    raw.descriptor().validate().expect("valid on its own");
    let mut alone = ModuleRegistry::new();
    alone.register(Arc::new(RawModule::new())).unwrap();
    let error = alone
        .check_complete()
        .expect_err("nothing reaches its canvas");
    assert_eq!(
        error.detail,
        "module luxforge.raw declares a pick canvas but no picker control reaches it"
    );
    let basic = BasicModule::new().descriptor().id.clone();
    let unreached = registry_with(&basic, |descriptor| {
        if let Control::Picker(PickerControl { variants, .. }) = &mut white_balance(descriptor)[2] {
            variants.clear();
        }
    });
    assert!(unreached.check_complete().is_err());
    // An editor service refuses such a registry before it opens anything.
    let catalog = std::env::temp_dir().join(format!(
        "luxforge-variants-{}-{}.sqlite",
        std::process::id(),
        crate::LayerId::new().as_str()
    ));
    let error = crate::EditorService::open_with(&catalog, Arc::new(alone))
        .map(drop)
        .expect_err("an incomplete registry");
    assert_eq!(error.kind, ErrorKind::Validation);
    let _ = std::fs::remove_file(catalog);
}

/// The one resolver: a variant applies on the global target of a photo of its kind; a mask
/// target, another kind and no photo at all use the base control.
#[test]
fn a_control_resolves_to_its_variant_only_on_the_global_target_of_its_kind() {
    let registry = ModuleRegistry::builtin();
    let basic = registry.module("luxforge.basic").unwrap().descriptor();
    let group = &basic.controls[0];
    let Control::Group(GroupControl { controls, .. }) = group else {
        panic!("the White balance group");
    };
    let mask = MaskId::new();
    for control in controls {
        let base = resolve_control(&basic.id, control, Some(SourceTag::Jpeg), None);
        assert_eq!(
            (base.module, base.control, base.variant),
            ("luxforge.basic", control, false)
        );
        for (kind, target) in [
            (None, None),
            (Some(SourceTag::Raw), Some(&mask)),
            (Some(SourceTag::Jpeg), Some(&mask)),
        ] {
            assert_eq!(resolve_control(&basic.id, control, kind, target), base);
        }
        let raw = resolve_control(&basic.id, control, Some(SourceTag::Raw), None);
        assert!(raw.variant);
        assert_eq!(raw.module, "luxforge.raw");
        assert_eq!(raw.control.kind_name(), control.kind_name());
        assert_eq!(
            raw.control,
            control.variants()[0].control.as_deref().unwrap()
        );
    }
    let requests: Vec<Value> = controls
        .iter()
        .map(|control| {
            let resolved = resolve_control(&basic.id, control, Some(SourceTag::Raw), None);
            serde_json::to_value(resolved.control).unwrap()
        })
        .collect();
    assert_eq!(requests[0]["action"], "set-raw");
    assert_eq!(requests[0]["parameter"], "temperature");
    assert_eq!(requests[1]["parameter"], "tint");
    assert_eq!(
        requests[2],
        json!({"kind": "picker", "label": "Neutral picker"})
    );
    assert_eq!(requests[3]["preset"], json!({"white-balance": "as-shot"}));

    let as_shot = ResetAction {
        action: "set-raw".into(),
        preset: json!({"white-balance": "as-shot"})
            .as_object()
            .unwrap()
            .clone(),
    };
    let on_raw = resolve_group_reset(&basic.id, group, Some(SourceTag::Raw), None).unwrap();
    assert_eq!(
        (on_raw.module, on_raw.reset, on_raw.variant),
        ("luxforge.raw", &as_shot, true)
    );
    for (kind, target) in [
        (Some(SourceTag::Jpeg), None),
        (Some(SourceTag::Raw), Some(&mask)),
        (None, None),
    ] {
        let base = resolve_group_reset(&basic.id, group, kind, target).unwrap();
        assert_eq!(base.module, "luxforge.basic");
        assert_eq!(base.reset.action, "set-basic");
        assert!(!base.variant);
    }
    // A group without a reset, and a control that is not a group, resolve to none.
    let tone = &basic.controls[1];
    assert!(resolve_group_reset(&basic.id, tone, Some(SourceTag::Raw), None).is_some());
    assert!(resolve_group_reset(&basic.id, &controls[0], Some(SourceTag::Raw), None).is_none());
}

/// Superseded fields are derived from the variants: Basic's Temperature and Tint on a RAW photo,
/// and nothing else — not Exposure, and nothing on a JPEG. The refusal is worded from the
/// declarations.
#[test]
fn superseded_fields_are_derived_from_the_variants() {
    let registry = ModuleRegistry::builtin();
    let found: Vec<(SourceTag, &str, &str, String)> = registry
        .superseded()
        .into_iter()
        .map(|field| (field.source, field.action, field.parameter, field.by()))
        .collect();
    assert_eq!(
        found,
        [
            (
                SourceTag::Raw,
                "set-basic",
                "temperature",
                "set-raw.temperature".to_owned()
            ),
            (
                SourceTag::Raw,
                "set-basic",
                "tint",
                "set-raw.tint".to_owned()
            ),
        ]
    );
    assert!(
        registry
            .superseded_field("set-basic", "exposure", SourceTag::Raw)
            .is_none()
    );
    assert!(
        registry
            .superseded_field("set-basic", "temperature", SourceTag::Jpeg)
            .is_none()
    );
    let temperature = registry
        .superseded_field("set-basic", "temperature", SourceTag::Raw)
        .unwrap();
    assert_eq!(
        registry.superseded_refusal(&temperature),
        "on a RAW photo, Temperature is the source development's: set-raw temperature (K)"
    );
    let tint = registry
        .superseded_field("set-basic", "tint", SourceTag::Raw)
        .unwrap();
    assert_eq!(
        registry.superseded_refusal(&tint),
        "on a RAW photo, Tint is the source development's: set-raw tint"
    );
}

/// Variants serialize only where they are declared, so every other built-in descriptor — and the
/// host's — is byte for byte what it was before the vocabulary gained them. The digests are the
/// SHA-256 of each descriptor's compact JSON as it was listed before variants existed; a
/// deliberate change to one of these descriptors updates its digest here.
#[test]
fn every_descriptor_without_variants_serializes_exactly_as_before() {
    let registry = ModuleRegistry::developer();
    let before = [
        (
            "luxforge.presets",
            "c60ad4b917dc0e4c7b0668b964711a835009fedd746bea1fd8510da256a39f7a",
        ),
        (
            "luxforge.pixel",
            "d971294242b2a28ec0a5bc14965689d8ba21bfced2770418b19baf0f6f4f579b",
        ),
        (
            "luxforge.presence",
            "23aa2adfbc18684f6787d5b7c637291cf1b911db0bd956384c0c291261ff82ea",
        ),
        (
            "luxforge.mixer",
            "06c0c14b6a241dbd0affed3bf25f99bceecd55c262208a8d18a6be5cd844b469",
        ),
        (
            "luxforge.transform",
            "d78f86ba56ee33bc84de8a0583fd97592ec80ce23e68e544e2302efbf16d2b38",
        ),
        (
            "luxforge.crop",
            // Its angle declares `step` 0.5 and `fine_step` 0.05, which the stepper reads.
            "7eafaccc88aeb0774e8ea617cfe589d742c5e6475202b34c1c5f9f03e09f0269",
        ),
        (
            "luxforge.vignette",
            "09ae86380463efe753399f7d8a9fb2465b84f73ea0ce2918e72229b37fecafaf",
        ),
        (
            "luxforge.masks",
            "ad8a452ea02d47580d2d9f93595baa308bcbf8ad4976cacebdb3bd2e5ee23d27",
        ),
    ];
    let listed: Vec<&ModuleDescriptor> = registry
        .descriptors()
        .into_iter()
        .chain(registry.host_descriptors())
        .collect();
    // The one deliberate change among them: `apply-preset`'s notes gained the skip rule. With the
    // notes it had before, the presets descriptor is otherwise byte for byte what it was.
    const PRESET_NOTES_BEFORE: &str = "applies a settings set as one history entry labelled \
        `Preset: <name>`. Each key of settings names a field-patch action and its value the fields \
        to send it; the host runs the actions in key order, each against the stack the ones before \
        it produced, exactly as it would run that action alone, and commits the result once. \
        Fields the set does not name keep their values, and a set that changes nothing is a \
        reported no-op. An unknown, non-patch or unavailable action, or a field its action \
        refuses, refuses the whole preset and writes nothing.";
    for (id, digest) in before {
        let mut descriptor = (*listed
            .iter()
            .find(|descriptor| descriptor.id == id)
            .unwrap_or_else(|| panic!("{id} is listed")))
        .clone();
        if id == "luxforge.presets" {
            assert!(
                descriptor.actions[0]
                    .notes
                    .contains("are skipped and listed under skipped")
            );
            descriptor.actions[0].notes = PRESET_NOTES_BEFORE.into();
        }
        // The other: the luminance band gained its one `range` control. Without it the masks
        // descriptor is byte for byte what it was.
        if id == "luxforge.masks" {
            let before = descriptor.controls.len();
            descriptor
                .controls
                .retain(|control| !matches!(control, crate::Control::Range(_)));
            assert_eq!(descriptor.controls.len(), before - 1);
        }
        let json = serde_json::to_string(&descriptor).unwrap();
        assert!(!json.contains("\"variants\""), "{id}");
        assert_eq!(
            format!("{:x}", Sha256::digest(json.as_bytes())),
            digest,
            "{id}"
        );
    }
    // Only Basic declares variants, and every module's JSON round-trips through the parser.
    for descriptor in registry.descriptors() {
        let value = serde_json::to_value(descriptor).unwrap();
        assert_eq!(
            value.to_string().contains("\"variants\""),
            descriptor.id == "luxforge.basic",
            "{}",
            descriptor.id
        );
        assert_eq!(
            &serde_json::from_value::<ModuleDescriptor>(value).unwrap(),
            descriptor
        );
    }
}
