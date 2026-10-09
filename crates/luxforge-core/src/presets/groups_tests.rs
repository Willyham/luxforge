//! The settings groups (`preset.groups`): identities, defaults and per-kind captures derived from
//! the built-in descriptors, and each group's state on a real JPEG entry, captured exactly as
//! `preset.capture` reads it.
use super::*;
use crate::{
    AssetId, BASIC_EFFECT, Control, EditorService, EffectStage, EntryId, ErrorKind,
    ModuleDescriptor, ModuleRegistry, ParameterDescriptor, SourceTag,
    editor::mutation,
    modules::{STAGE_ACTION, StageModule},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

fn builtin() -> SettingsGroups {
    settings_groups(ModuleRegistry::builtin().descriptors())
}

fn ids(groups: &SettingsGroups) -> Vec<&str> {
    groups
        .groups
        .iter()
        .map(|group| group.id.as_str())
        .collect()
}

/// Every presettable field-patch group of the built-in modules, in registry order, under an
/// identity made of its module's and its own label, with the white-balance group the one per-photo
/// group and Auto tone the one analysis step, overwriting Tone and Colour.
#[test]
fn the_builtin_groups_have_stable_ids_in_registry_order_and_white_balance_is_per_photo() {
    let groups = builtin();
    assert_eq!(
        ids(&groups),
        [
            "luxforge.basic/white-balance",
            "luxforge.basic/tone",
            "luxforge.basic/colour",
            "luxforge.curve/tone-curve",
            "luxforge.detail/sharpening",
            "luxforge.detail/noise-reduction",
            "luxforge.presence/presence",
            "luxforge.mixer/hsl",
            "luxforge.mixer/grading",
            "luxforge.vignette/vignette",
        ]
    );
    let titles: Vec<&str> = groups.groups.iter().map(|g| g.title.as_str()).collect();
    assert_eq!(titles[0], "Basic \u{00b7} White balance");
    assert_eq!(titles[1], "Basic \u{00b7} Tone");
    let unchecked: Vec<&str> = groups
        .groups
        .iter()
        .filter(|group| !group.default_checked)
        .map(|group| group.id.as_str())
        .collect();
    assert_eq!(unchecked, ["luxforge.basic/white-balance"]);
    assert!(groups.groups[0].per_photo);
    let tone = groups.group("luxforge.basic/tone").expect("Tone");
    assert_eq!(
        serde_json::to_value(&tone.fields).unwrap(),
        json!({"set-basic": ["exposure", "contrast", "highlights", "shadows", "whites", "blacks"]})
    );
    assert!(
        groups
            .groups
            .iter()
            .all(|group| group.unavailable.is_none())
    );
    let [auto] = groups.analysis.as_slice() else {
        panic!("one analysis step: {:?}", groups.analysis);
    };
    assert_eq!(auto.id, "auto-tone");
    assert_eq!(auto.title, "Basic \u{00b7} Auto tone");
    assert_eq!(
        auto.overwrites,
        ["luxforge.basic/tone", "luxforge.basic/colour"]
    );
    assert_eq!(tone.overwritten_by, ["auto-tone"]);
    assert!(groups.groups[0].overwritten_by.is_empty());
    // Without a photograph nothing is per-entry.
    assert!(groups.photo.is_none());
    assert!(
        groups
            .groups
            .iter()
            .all(|g| g.state.is_none() && g.reason.is_none())
    );
}

/// White balance is each kind's own: a JPEG's relative pair, which a RAW photo's development
/// supersedes, and the RAW development, which does not apply to a JPEG. Every other group captures
/// the same fields on both kinds and applies on both.
#[test]
fn per_kind_captures_and_skips_predict_the_white_balance_notice() {
    let groups = builtin();
    let white = &groups.groups[0];
    let jpeg = white.on(SourceTag::Jpeg).expect("JPEG");
    let raw = white.on(SourceTag::Raw).expect("RAW");
    assert_eq!(
        Value::Object(jpeg.captures.clone()),
        json!({"set-basic": ["temperature", "tint"]})
    );
    assert_eq!(
        Value::Object(raw.captures.clone()),
        json!({"set-raw": true})
    );
    assert!(jpeg.refused.is_none() && raw.refused.is_none());
    let [on_raw] = jpeg.skipped.as_slice() else {
        panic!("a JPEG's pair is skipped on RAW: {:?}", jpeg.skipped);
    };
    assert_eq!(on_raw.kind, SourceTag::Raw);
    assert!(on_raw.all);
    assert!(
        on_raw.reasons[0].starts_with("on a RAW photo, Temperature is"),
        "{:?}",
        on_raw.reasons
    );
    let [on_jpeg] = raw.skipped.as_slice() else {
        panic!("the development is skipped on a JPEG: {:?}", raw.skipped);
    };
    assert_eq!(
        (on_jpeg.kind, on_jpeg.all, on_jpeg.reasons.as_slice()),
        (
            SourceTag::Jpeg,
            true,
            ["RAW does not apply to a JPEG photo".to_owned()].as_slice()
        )
    );
    for group in &groups.groups[1..] {
        for on in &group.kinds {
            assert!(on.skipped.is_empty() && on.refused.is_none(), "{group:?}");
            assert_eq!(
                serde_json::to_value(&on.captures).unwrap(),
                serde_json::to_value(&group.fields).unwrap()
            );
        }
    }
}

/// Groups come from the descriptors, not from module or field names: a module's loose patch
/// controls are one group named for the module; a group declared per photo is unchecked whatever
/// its fields are called; a group of request inputs is no group; an unavailable module's groups
/// are listed with the refusal capture gives them.
#[test]
fn groups_come_from_descriptors_not_from_names() {
    let number = |name: &str| {
        ParameterDescriptor::number(name, -1.0, 1.0)
            .default(0.0)
            .notes("n")
    };
    let module = ModuleDescriptor {
        id: "fixture.look".into(),
        title: "Look".into(),
        actions: vec![
            crate::ActionDescriptor {
                patch: true,
                parameters: vec![number("amount"), number("tint"), number("glow")],
                ..crate::ActionDescriptor::new("set-look", "Set look", "patch")
            },
            crate::ActionDescriptor {
                parameters: vec![number("x")],
                ..crate::ActionDescriptor::new("place", "Place", "request")
            },
        ],
        controls: vec![
            Control::number("set-look", "amount", "Amount").into(),
            Control::group(
                "Cast",
                vec![Control::number("set-look", "tint", "Tint").into()],
            )
            .into(),
            Control::group("Where", vec![Control::number("place", "x", "X").into()]).into(),
            Control::group(
                "Glow & light",
                vec![Control::number("set-look", "glow", "Glow").into()],
            )
            .per_photo(true)
            .into(),
        ],
        ..ModuleDescriptor::default()
    };
    module.validate().expect("a valid fixture descriptor");
    let groups = settings_groups([&module]);
    assert_eq!(
        ids(&groups),
        [
            "fixture.look",
            "fixture.look/cast",
            "fixture.look/glow-light"
        ]
    );
    assert_eq!(groups.groups[0].title, "Look");
    assert_eq!(groups.groups[1].title, "Look \u{00b7} Cast");
    assert!(
        groups.groups[1].default_checked,
        "a tint field is not per photo by name"
    );
    assert!(!groups.groups[2].default_checked);
    let unavailable = ModuleDescriptor {
        availability: crate::Availability::Unavailable {
            reason: "test".into(),
        },
        ..module
    };
    let groups = settings_groups([&unavailable]);
    assert_eq!(
        groups.groups[0].unavailable.as_deref(),
        Some("incompatible: unavailable module fixture.look")
    );
    assert_eq!(
        groups.groups[0].kinds[0].refused.as_deref(),
        Some("incompatible: unavailable module fixture.look")
    );
}

#[test]
fn capture_fields_merge_groups_and_name_analysis_steps() {
    let groups = builtin();
    assert_eq!(
        Value::Object(
            groups
                .capture_fields(&[
                    "luxforge.basic/white-balance",
                    "luxforge.basic/tone",
                    "luxforge.vignette/vignette"
                ])
                .unwrap()
        ),
        json!({
            "set-basic": ["temperature", "tint", "exposure", "contrast", "highlights", "shadows", "whites", "blacks"],
            "set-vignette": serde_json::to_value(&groups.group("luxforge.vignette/vignette").unwrap().fields["set-vignette"]).unwrap(),
        })
    );
    assert_eq!(
        Value::Object(
            groups
                .capture_fields(&["auto-tone", "luxforge.basic/white-balance"])
                .unwrap()
        ),
        json!({"auto-tone": true, "set-basic": ["temperature", "tint"]})
    );
    for (ids, detail) in [
        (
            vec!["luxforge.basic/nowhere"],
            "unknown settings group luxforge.basic/nowhere",
        ),
        (
            vec!["luxforge.basic/tone", "luxforge.basic/tone"],
            "names luxforge.basic/tone twice",
        ),
        (vec![], "groups names 1 to 64 groups"),
    ] {
        let error = groups.capture_fields(&ids).expect_err(detail);
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains(detail), "{}", error.detail);
    }
}

fn opened(name: &str, registry: Option<Arc<ModuleRegistry>>) -> (EditorService, AssetId, PathBuf) {
    let path = luxforge_testbase::paths::temp_catalog(&format!("preset-groups-{name}"));
    let mut service = match registry {
        Some(registry) => EditorService::open_with(&path, registry),
        None => EditorService::open(&path),
    }
    .expect("a catalog");
    let asset = service
        .import(&luxforge_testbase::paths::jpeg())
        .expect("an import")
        .asset
        .id;
    (service, asset, path)
}

fn current(service: &EditorService, asset: &AssetId) -> EntryId {
    service.state(asset).expect("state").current_entry.id
}

fn states(groups: &SettingsGroups) -> Vec<(&str, GroupState)> {
    groups
        .groups
        .iter()
        .map(|group| (group.id.as_str(), group.state.expect("a state")))
        .collect()
}

/// On a photograph's entry each group is Custom exactly when a field capture reads there differs
/// from its declared default, read from the named entry; and capture by group identities equals
/// capture by the fields those groups name.
#[test]
fn a_photos_groups_are_custom_or_original_against_the_named_entry() {
    let (mut service, asset, path) = opened("states", None);
    let original = current(&service, &asset);
    let at_original = service
        .preset_groups(Some((&asset, &original)))
        .expect("groups");
    assert!(
        states(&at_original)
            .iter()
            .all(|(_, state)| *state == GroupState::Original)
    );
    assert_eq!(
        at_original.photo,
        Some(SettingsPhoto {
            asset_id: asset.clone(),
            entry_id: original.clone(),
            kind: SourceTag::Jpeg
        })
    );
    assert!(at_original.analysis[0].reason.is_none());
    service
        .apply_action(
            &asset,
            mutation(0, "tone"),
            "set-basic",
            json!({"exposure": 0.5}),
        )
        .expect("a Basic edit");
    service
        .apply_action(
            &asset,
            mutation(1, "mixer"),
            "set-mixer",
            json!({"red-hue": 10}),
        )
        .expect("a mixer edit");
    let edited = current(&service, &asset);
    let groups = service
        .preset_groups(Some((&asset, &edited)))
        .expect("groups");
    let custom: Vec<&str> = states(&groups)
        .into_iter()
        .filter(|(_, state)| *state == GroupState::Custom)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(custom, ["luxforge.basic/tone", "luxforge.mixer/hsl"]);
    // The earlier entry still reads as it was.
    assert_eq!(
        service
            .preset_groups(Some((&asset, &original)))
            .unwrap()
            .groups,
        at_original.groups
    );
    // Capture by groups is capture by their fields.
    let named = ["luxforge.basic/tone", "luxforge.mixer/hsl"];
    let fields = groups.capture_fields(&named).unwrap();
    assert_eq!(
        service.capture_preset(&asset, &edited, &fields).unwrap()["set-basic"]["exposure"],
        json!(0.5)
    );
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

/// The mixer is two capture groups: HSL gathers its Hue, Saturation and Luminance groups, and
/// Grading holds its fourteen fields once each, in the order its views first show them, however
/// many views draw a field.
#[test]
fn grading_views_capture_all_four_wheels_once_and_keep_hsl_separate() {
    let groups = builtin();
    let hsl = groups.group("luxforge.mixer/hsl").unwrap();
    let grading = groups.group("luxforge.mixer/grading").unwrap();
    assert_eq!(hsl.fields["set-mixer"].len(), 24);
    assert_eq!(
        serde_json::to_value(&grading.fields).unwrap(),
        json!({"set-mixer": [
            "grade-midtones-hue", "grade-midtones-saturation", "grade-midtones-luminance",
            "grade-shadows-hue", "grade-shadows-saturation", "grade-shadows-luminance",
            "grade-highlights-hue", "grade-highlights-saturation", "grade-highlights-luminance",
            "grade-blending", "grade-balance",
            "grade-global-hue", "grade-global-saturation", "grade-global-luminance"
        ]})
    );
    assert!(hsl.default_checked && grading.default_checked);
}

/// Grading captured from one photograph, dormant values included (a hue at zero saturation,
/// Blending and Balance), reproduces that grade over another photograph's different grade and
/// leaves the target's HSL as it was.
#[test]
fn a_captured_grade_applies_over_another_grade_and_keeps_hsl() {
    let (mut service, source, path) = opened("grade-capture", None);
    let target = service
        .import(&luxforge_testbase::paths::fixture("s0/orientation-6.jpg"))
        .expect("a second import")
        .asset
        .id;
    let grade = json!({
        "grade-shadows-hue": 220, "grade-shadows-saturation": 30,
        "grade-midtones-hue": 40, "grade-blending": 70, "grade-balance": -25
    });
    service
        .apply_action(&source, mutation(0, "grade"), "set-mixer", grade)
        .expect("the source's grade");
    service
        .apply_action(
            &target,
            mutation(0, "target"),
            "set-mixer",
            json!({
                "red-hue": 15, "blue-saturation": -20,
                "grade-midtones-saturation": 25, "grade-highlights-hue": 60,
                "grade-highlights-saturation": 10, "grade-blending": 30
            }),
        )
        .expect("the target's HSL and grade");
    let groups = service.preset_groups(None).expect("groups");
    let grading = groups.capture_fields(&["luxforge.mixer/grading"]).unwrap();
    let hsl = groups.capture_fields(&["luxforge.mixer/hsl"]).unwrap();
    // Every field as a number, so a stored integer and a default compare alike.
    let mixer = |settings: &Map<String, Value>| -> BTreeMap<String, f64> {
        settings["set-mixer"]
            .as_object()
            .expect("mixer fields")
            .iter()
            .map(|(field, value)| (field.clone(), value.as_f64().expect("a number")))
            .collect()
    };
    let captured = service
        .capture_preset(&source, &current(&service, &source), &grading)
        .unwrap();
    let mut expected: BTreeMap<String, f64> = grading["set-mixer"]
        .as_array()
        .expect("grading's fields")
        .iter()
        .map(|field| (field.as_str().unwrap().to_owned(), 0.0))
        .collect();
    expected.extend(
        [
            ("grade-shadows-hue", 220.0),
            ("grade-shadows-saturation", 30.0),
            ("grade-midtones-hue", 40.0),
            ("grade-blending", 70.0),
            ("grade-balance", -25.0),
        ]
        .map(|(field, value)| (field.to_owned(), value)),
    );
    assert_eq!(expected.len(), 14);
    assert_eq!(mixer(&captured), expected);
    service
        .run_action(
            &target,
            mutation(1, "apply"),
            "apply-settings",
            json!({"settings": captured, "origin": {"kind": "preset", "name": "Grade"}}),
        )
        .expect("the grade applies");
    let applied = current(&service, &target);
    assert_eq!(
        mixer(&service.capture_preset(&target, &applied, &grading).unwrap()),
        expected
    );
    let kept = mixer(&service.capture_preset(&target, &applied, &hsl).unwrap());
    assert_eq!((kept["red-hue"], kept["blue-saturation"]), (15.0, -20.0));
    assert!(
        kept.iter().all(|(field, value)| *value == 0.0
            || ["red-hue", "blue-saturation"].contains(&field.as_str())),
        "{kept:?}"
    );
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

/// Capture scopes follow which groups own fields, not layout: a top-level group is a scope; a
/// group that owns no fields directly gathers every group nested in it, whether its children are
/// tabs or stacked, per photo when any of them is; a group nested in one that owns fields is a
/// scope of its own.
#[test]
fn capture_scopes_follow_field_ownership_not_layout() {
    let number = |name: &str| {
        ParameterDescriptor::number(name, -1.0, 1.0)
            .default(0.0)
            .notes("n")
    };
    let value = |name: &str| Control::number("set-look", name, name).into();
    let module = ModuleDescriptor {
        id: "fixture.look".into(),
        title: "Look".into(),
        actions: vec![crate::ActionDescriptor {
            patch: true,
            parameters: ["a", "b", "c", "d", "e", "f"].map(number).into(),
            ..crate::ActionDescriptor::new("set-look", "Set look", "patch")
        }],
        controls: vec![
            Control::group(
                "Stacked",
                vec![
                    Control::group("One", vec![value("a")]).into(),
                    Control::group("Two", vec![value("b")])
                        .per_photo(true)
                        .into(),
                ],
            )
            .into(),
            Control::group(
                "Owner",
                vec![value("c"), Control::group("Inner", vec![value("d")]).into()],
            )
            .into(),
            Control::group(
                "Tabbed",
                vec![
                    Control::group("Three", vec![value("e")]).into(),
                    Control::group("Four", vec![value("f")]).into(),
                ],
            )
            .group_layout(crate::ModuleLayout::Tabs)
            .into(),
        ],
        ..ModuleDescriptor::default()
    };
    module.validate().expect("a valid fixture descriptor");
    let groups = settings_groups([&module]);
    let scopes: Vec<(&str, Value, bool)> = groups
        .groups
        .iter()
        .map(|group| {
            (
                group.id.as_str(),
                serde_json::to_value(&group.fields).unwrap(),
                group.per_photo,
            )
        })
        .collect();
    assert_eq!(
        scopes,
        [
            (
                "fixture.look/stacked",
                json!({"set-look": ["a", "b"]}),
                true
            ),
            ("fixture.look/owner", json!({"set-look": ["c"]}), false),
            ("fixture.look/inner", json!({"set-look": ["d"]}), false),
            (
                "fixture.look/tabbed",
                json!({"set-look": ["e", "f"]}),
                false
            ),
        ]
    );
}

/// A group capture refuses on this entry is Refused with capture's own reason, and the others are
/// unaffected: two Basic layers make Basic's groups ambiguous, and a module registered unavailable
/// refuses its group.
#[test]
fn a_group_capture_refuses_is_refused_with_its_reason() {
    let mut twin = ModuleRegistry::new();
    twin.register(StageModule::shared(
        "test.twin",
        BASIC_EFFECT,
        STAGE_ACTION,
        EffectStage::Color,
        0,
    ))
    .expect("a stand-in Basic provider");
    let (mut service, asset, path) = opened("refused", Some(Arc::new(twin)));
    for (revision, request) in [(0, "one"), (1, "two")] {
        service
            .apply_action(&asset, mutation(revision, request), STAGE_ACTION, json!({}))
            .expect("a Basic-effect layer");
    }
    drop(service);
    let mut registry = ModuleRegistry::new();
    for module in crate::modules::linked_modules(false) {
        match module.descriptor().id.as_str() {
            "luxforge.presence" => registry.register_unavailable(module, "disabled"),
            _ => registry.register(module),
        }
        .expect("a registered module");
    }
    let service = EditorService::open_with(&path, Arc::new(registry)).expect("a catalog");
    let entry = current(&service, &asset);
    let groups = service
        .preset_groups(Some((&asset, &entry)))
        .expect("groups");
    for group in &groups.groups {
        let refused = match group.module.as_str() {
            "luxforge.basic" => Some("validation: ambiguous Basic layers"),
            "luxforge.presence" => Some("incompatible: unavailable module luxforge.presence"),
            _ => None,
        };
        assert_eq!(group.reason.as_deref(), refused, "{}", group.id);
        assert_eq!(
            group.state == Some(GroupState::Refused),
            refused.is_some(),
            "{}",
            group.id
        );
    }
    let presence = groups.group("luxforge.presence/presence").unwrap();
    assert_eq!(
        presence.unavailable.as_deref(),
        Some("incompatible: unavailable module luxforge.presence")
    );
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}
