//! The preset library and capture through the real `EditorService` and a real catalog: the record
//! shape and sort order, the name, uniqueness and size rules, update and delete outcomes, the
//! computed `unavailable` list, import and inspect, the catalog format marker, and capture from a
//! JPEG entry's stored payloads.
use super::*;
use crate::{
    ActionDescriptor, ActionInput, ActionPlan, AssetId, Availability, BASIC_EFFECT, BasicModule,
    CropModule, EFFECT_FORMAT, EditorService, EffectDescriptor, EffectStage, EntryId, Error,
    ErrorKind, MAX_PRESET_NAME, MixerModule, ModuleDescriptor, ModuleRegistry, Mutation,
    MutationOutcome, ParameterDescriptor, ParameterKind, PixelModule, PresenceModule, PresetId,
    PresetsModule, Processing, Stage, StageContext, ToolModule, TransformModule, VignetteModule,
    modules::{STAGE_ACTION, StageModule, TestModule},
};
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn catalog(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lightwell-preset-library-{name}-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&path);
    path
}

fn jpeg() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

fn preset_file(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/presets")
            .join(name),
    )
    .expect("a preset fixture")
}

fn set(value: Value) -> Map<String, Value> {
    value.as_object().expect("a settings set").clone()
}

fn mutation(revision: u64, request: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "library-test".into(),
    }
}

fn stored_rows(service: &EditorService) -> i64 {
    service
        .connection
        .query_row("SELECT COUNT(*) FROM presets", [], |row| row.get(0))
        .expect("a row count")
}

fn soft() -> Map<String, Value> {
    set(json!({
        "set-basic": {"exposure": 0.35, "contrast": 12},
        "set-mixer": {"blue-saturation": -20},
    }))
}

fn create(
    service: &mut EditorService,
    name: &str,
    group: Option<&str>,
) -> Result<PresetRecord, Error> {
    service.create_preset(name, group, &soft(), "library-test")
}

fn names(service: &EditorService) -> Vec<(String, String)> {
    service
        .presets()
        .expect("a listing")
        .into_iter()
        .map(|preset| (preset.group, preset.name))
        .collect()
}

fn assert_error(result: Result<impl std::fmt::Debug, Error>, kind: ErrorKind, detail: &str) {
    let error = result.expect_err(detail);
    assert_eq!(error.kind, kind, "{}", error.detail);
    assert!(
        error.detail.contains(detail),
        "{:?} does not contain {detail:?}",
        error.detail
    );
}

// -------------------------------------------------------------------------------------------
// Catalog format.
// -------------------------------------------------------------------------------------------

#[test]
fn a_fresh_catalog_is_marked_with_the_current_format_and_starts_with_an_empty_library() {
    let path = catalog("fresh");
    let service = EditorService::open(&path).expect("a catalog");
    let marker: i64 = service
        .connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("a marker");
    // The merged shape: the mask table and stroke store, the preset library, and the catalog
    // identity with the artifact tables. Two branches each wrote a format 6 holding one half.
    assert_eq!(marker, 7);
    assert!(service.presets().expect("a listing").is_empty());
    drop(service);
    let reopened = EditorService::open(&path).expect("a current-format catalog reopens");
    assert!(reopened.presets().expect("a listing").is_empty());
    drop(reopened);
    std::fs::remove_file(path).expect("the catalog is removed");
}

#[test]
fn a_format_4_catalog_is_refused_by_name_without_rewriting_it() {
    let path = catalog("format-4");
    let mut service = EditorService::open(&path).expect("a catalog");
    service.import(&jpeg()).expect("an import");
    drop(service);
    let connection = Connection::open(&path).expect("the file");
    connection
        .pragma_update(None, "user_version", 4)
        .expect("a marker");
    drop(connection);
    let before = std::fs::read(&path).expect("the bytes");
    let error = EditorService::open(&path).expect_err("format 4 has no library");
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert_eq!(
        error.detail,
        "catalog format 4 is not supported; expected 7; choose a new catalog path"
    );
    assert_eq!(
        std::fs::read(&path).expect("the bytes"),
        before,
        "a refused catalog is left byte for byte as it was"
    );
    std::fs::remove_file(path).expect("the catalog is removed");
}

// -------------------------------------------------------------------------------------------
// Create, list and read.
// -------------------------------------------------------------------------------------------

#[test]
fn created_presets_list_by_group_then_name_ignoring_case_and_read_back_whole() {
    let path = catalog("create");
    let mut service = EditorService::open(&path).expect("a catalog");
    let warm = create(&mut service, "  Warm  ", None).expect("a preset");
    create(&mut service, "cool", Some("User presets")).expect("a preset");
    create(&mut service, "Zeta", Some("Looks")).expect("a preset");
    create(&mut service, "alpha", Some("looks")).expect("a preset");
    create(&mut service, "Mono", Some(" Black and white ")).expect("a preset");
    assert_eq!(
        names(&service),
        [
            ("Black and white", "Mono"),
            ("looks", "alpha"),
            ("Looks", "Zeta"),
            ("User presets", "cool"),
            ("User presets", "Warm"),
        ]
        .map(|(group, name)| (group.to_owned(), name.to_owned())),
        "sorted by group, then name, ignoring case; names and groups are trimmed"
    );

    // The record has exactly the design's fields, and a preset created here has no report.
    let shape = serde_json::to_value(&warm).expect("a record");
    assert_eq!(
        shape
            .as_object()
            .expect("an object")
            .keys()
            .collect::<Vec<_>>(),
        [
            "actor",
            "created_ms",
            "group",
            "id",
            "name",
            "origin",
            "report",
            "settings",
            "unavailable",
            "updated_ms"
        ]
    );
    assert!(warm.id.as_str().starts_with("preset-"));
    assert_eq!(warm.name, "Warm");
    assert_eq!(warm.group, USER_PRESET_GROUP);
    assert_eq!(warm.settings, soft());
    assert_eq!(shape["origin"], json!({"kind": "lightwell"}));
    assert_eq!(shape["report"], Value::Null);
    assert_eq!(warm.actor, "library-test");
    assert_eq!(warm.created_ms, warm.updated_ms);
    assert!(warm.unavailable.is_empty());

    // Read returns the stored record, and no source text for a preset created here.
    let (read, source_text) = service.preset(&warm.id).expect("the preset");
    assert_eq!(read, warm);
    assert_eq!(source_text, None);
    let listed = service
        .presets()
        .expect("a listing")
        .into_iter()
        .find(|preset| preset.id == warm.id)
        .expect("the listed preset");
    assert_eq!(listed, warm.clone().summary());

    // An unknown or malformed identity is a validation error, as for entries and assets.
    let missing = PresetId::new();
    assert_error(
        service.preset(&missing),
        ErrorKind::Validation,
        &format!("unknown preset {missing}"),
    );
    assert_error(
        PresetId::parse("entry-0123456789abcdef"),
        ErrorKind::Validation,
        "invalid PresetId",
    );

    drop(service);
    let reopened = EditorService::open(&path).expect("the catalog reopens");
    assert_eq!(reopened.preset(&warm.id).expect("the preset").0, warm);
    assert_eq!(reopened.presets().expect("a listing").len(), 5);
    drop(reopened);
    std::fs::remove_file(path).expect("the catalog is removed");
}

#[test]
fn a_group_and_name_pair_is_unique_ignoring_case() {
    let path = catalog("duplicates");
    let mut service = EditorService::open(&path).expect("a catalog");
    create(&mut service, "Soft", Some("Looks")).expect("a preset");
    create(&mut service, "Été", Some("Looks")).expect("a preset");
    for (name, group) in [
        ("SOFT", "looks"),
        ("soft", "Looks"),
        (" Soft ", "LOOKS"),
        ("éTÉ", "looks"),
    ] {
        let error = create(&mut service, name, Some(group)).expect_err("a duplicate");
        assert_eq!(error.kind, ErrorKind::Conflict, "{name} in {group}");
        assert!(
            error
                .detail
                .starts_with("group \"Looks\" already holds a preset named"),
            "the conflict names the existing group and name: {}",
            error.detail
        );
    }
    let error = create(&mut service, "soft", Some("looks")).expect_err("a duplicate");
    assert_eq!(
        error.detail,
        "group \"Looks\" already holds a preset named \"Soft\"; choose another name or group"
    );
    assert_eq!(stored_rows(&service), 2, "nothing was renamed or stored");
    // The same name in another group, and another name in the same group, are distinct presets.
    create(&mut service, "Soft", Some("Other")).expect("another group");
    create(&mut service, "Softer", Some("Looks")).expect("another name");
    assert_eq!(stored_rows(&service), 4);
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

#[test]
fn names_groups_actors_and_settings_are_validated_before_anything_is_stored() {
    let path = catalog("validation");
    let mut service = EditorService::open(&path).expect("a catalog");
    let long_name = "n".repeat(MAX_PRESET_NAME + 1);
    for name in ["", "   ", "a\tb", "line\nbreak", long_name.as_str()] {
        assert_error(
            create(&mut service, name, None),
            ErrorKind::Validation,
            "preset name must contain 1..=128 printable characters",
        );
    }
    let long_group = "g".repeat(MAX_PRESET_GROUP + 1);
    for group in ["", " ", "a\u{7}b", long_group.as_str()] {
        assert_error(
            create(&mut service, "Name", Some(group)),
            ErrorKind::Validation,
            "preset group must contain 1..=64 printable characters",
        );
    }
    for actor in [String::new(), "a".repeat(129)] {
        assert_error(
            service.create_preset("Name", None, &soft(), &actor),
            ErrorKind::Validation,
            "actor must contain 1..128 characters",
        );
    }
    for (settings, kind, detail) in [
        (json!({}), ErrorKind::Validation, "names 1 to 16 actions"),
        (
            json!({"set-nothing": {"a": 1}}),
            ErrorKind::Validation,
            "unknown action set-nothing",
        ),
        (
            json!({"transform": {"transform": "rotate-left"}}),
            ErrorKind::Validation,
            "transform is not a field-patch action",
        ),
        (
            json!({"set-basic": {"exposure": 9}}),
            ErrorKind::Validation,
            "exposure",
        ),
        (
            json!({"set-basic": {}}),
            ErrorKind::Validation,
            "must be an object of 1 to 64 fields",
        ),
    ] {
        assert_error(
            service.create_preset("Name", None, &set(settings), "library-test"),
            kind,
            detail,
        );
    }
    assert_eq!(stored_rows(&service), 0);
    // The bounds themselves are accepted.
    let name = "n".repeat(MAX_PRESET_NAME);
    let group = "g".repeat(MAX_PRESET_GROUP);
    let preset = create(&mut service, &name, Some(&group)).expect("names at their bounds");
    assert_eq!((preset.name, preset.group), (name, group));
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

#[test]
fn the_library_holds_at_most_1000_presets() {
    let path = catalog("limit");
    let mut service = EditorService::open(&path).expect("a catalog");
    let record = json!({
        "settings": {"set-basic": {"exposure": 0.5}},
        "origin": {"kind": "lightwell"},
        "report": null,
        "actor": "bulk",
        "created_ms": 1,
        "updated_ms": 1,
    })
    .to_string();
    // Fill all but one place in one transaction; a thousand single creates would each sync.
    let tx = service.connection.transaction().expect("a transaction");
    for index in 0..MAX_PRESETS - 1 {
        tx.execute(
            "INSERT INTO presets (id,name,group_name,record_json,source_text) VALUES (?1,?2,'Bulk',?3,NULL)",
            params![PresetId::new().as_str(), format!("Bulk {index:04}"), record],
        )
        .expect("a bulk row");
    }
    tx.commit().expect("the bulk rows");
    assert_eq!(service.presets().expect("a listing").len(), MAX_PRESETS - 1);
    let last = create(&mut service, "Last", None).expect("the thousandth preset");
    assert_error(
        create(&mut service, "One more", None),
        ErrorKind::ResourceLimit,
        "the preset library holds at most 1000 presets",
    );
    assert_error(
        service.import_preset(
            &preset_file("soft-film.lwpreset"),
            None,
            None,
            None,
            "library-test",
        ),
        ErrorKind::ResourceLimit,
        "at most 1000 presets",
    );
    assert_eq!(stored_rows(&service), MAX_PRESETS as i64);
    assert_eq!(
        service.delete_preset(&last.id).expect("a delete"),
        MutationOutcome::Applied
    );
    create(&mut service, "One more", None).expect("room again after a delete");
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

// -------------------------------------------------------------------------------------------
// Update and delete.
// -------------------------------------------------------------------------------------------

#[test]
fn an_update_applies_then_is_a_no_op_then_conflicts_on_a_rename() {
    let path = catalog("update");
    let mut service = EditorService::open(&path).expect("a catalog");
    let soft_preset = create(&mut service, "Soft", Some("Looks")).expect("a preset");
    create(&mut service, "Warm", Some("Looks")).expect("a preset");
    std::thread::sleep(std::time::Duration::from_millis(2));

    let changed = set(json!({"set-vignette": {"amount": -18}}));
    let applied = service
        .update_preset(
            &soft_preset.id,
            "editor",
            Some(" Softer "),
            None,
            Some(&changed),
        )
        .expect("an update");
    assert_eq!(applied.outcome, MutationOutcome::Applied);
    let updated = applied.preset;
    assert_eq!(updated.id, soft_preset.id);
    assert_eq!(updated.name, "Softer");
    assert_eq!(updated.group, "Looks");
    assert_eq!(updated.settings, changed);
    assert_eq!(updated.actor, "editor", "the actor of the last change");
    assert_eq!(updated.created_ms, soft_preset.created_ms);
    assert!(updated.updated_ms > soft_preset.updated_ms);
    assert_eq!(updated.origin, soft_preset.origin);
    assert_eq!(
        service.preset(&soft_preset.id).expect("the preset").0,
        updated
    );

    // The same values again, or no values at all, change nothing and write nothing.
    for (name, group, settings) in [
        (Some("Softer"), Some("Looks"), Some(&changed)),
        (Some("  Softer"), None, None),
        (None, None, None),
    ] {
        let unchanged = service
            .update_preset(&soft_preset.id, "someone else", name, group, settings)
            .expect("a no-op");
        assert_eq!(unchanged.outcome, MutationOutcome::NoOp);
        assert_eq!(
            unchanged.preset, updated,
            "the stored record is returned as it is"
        );
    }
    assert_eq!(
        service.preset(&soft_preset.id).expect("the preset").0,
        updated
    );

    // A rename onto another preset's pair conflicts; the preset's own name may change case.
    assert_error(
        service.update_preset(&soft_preset.id, "editor", Some("warm"), None, None),
        ErrorKind::Conflict,
        "group \"Looks\" already holds a preset named \"Warm\"",
    );
    assert_error(
        service.update_preset(
            &soft_preset.id,
            "editor",
            None,
            Some("Other"),
            Some(&set(json!({"set-basic": {"exposure": 99}}))),
        ),
        ErrorKind::Validation,
        "exposure",
    );
    assert_eq!(
        service.preset(&soft_preset.id).expect("the preset").0,
        updated
    );
    let recased = service
        .update_preset(&soft_preset.id, "editor", Some("SOFTER"), None, None)
        .expect("a change of case");
    assert_eq!(recased.outcome, MutationOutcome::Applied);
    assert_eq!(recased.preset.name, "SOFTER");

    assert_error(
        service.update_preset(&PresetId::new(), "editor", Some("Name"), None, None),
        ErrorKind::Validation,
        "unknown preset",
    );
    assert_error(
        service.update_preset(&soft_preset.id, "", Some("Name"), None, None),
        ErrorKind::Validation,
        "actor must contain 1..128 characters",
    );
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

#[test]
fn a_delete_applies_then_is_a_no_op() {
    let path = catalog("delete");
    let mut service = EditorService::open(&path).expect("a catalog");
    let preset = create(&mut service, "Soft", None).expect("a preset");
    let kept = create(&mut service, "Kept", None).expect("a preset");
    assert_eq!(
        service.delete_preset(&preset.id).expect("a delete"),
        MutationOutcome::Applied
    );
    assert_eq!(
        service.delete_preset(&preset.id).expect("a second delete"),
        MutationOutcome::NoOp
    );
    assert_error(
        service.preset(&preset.id),
        ErrorKind::Validation,
        "unknown preset",
    );
    assert_eq!(
        service
            .presets()
            .expect("a listing")
            .into_iter()
            .map(|preset| preset.id)
            .collect::<Vec<_>>(),
        [kept.id]
    );
    // The name is free again.
    create(&mut service, "Soft", None).expect("the name is free");
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

// -------------------------------------------------------------------------------------------
// Unavailable actions.
// -------------------------------------------------------------------------------------------

/// A registered provider wrapped as unavailable, exactly as the desktop's `--disable-module` does.
struct Disabled {
    inner: Arc<dyn ToolModule>,
    descriptor: ModuleDescriptor,
}

impl Disabled {
    fn wrap(inner: Arc<dyn ToolModule>) -> Arc<dyn ToolModule> {
        let descriptor = ModuleDescriptor {
            availability: Availability::Unavailable {
                reason: "disabled by --disable-module".into(),
            },
            ..inner.descriptor().clone()
        };
        Arc::new(Self { inner, descriptor })
    }
}

impl ToolModule for Disabled {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.descriptor
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        self.inner.parse(action_id, parameters)
    }
    fn plan(&self, input: &ActionInput, stage: &StageContext<'_>) -> Result<ActionPlan, Error> {
        self.inner.plan(input, stage)
    }
    fn validate_payload(&self, effect_id: &str, format: u32, payload: &Value) -> Result<(), Error> {
        self.inner.validate_payload(effect_id, format, payload)
    }
    fn describe_layer(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
    ) -> Result<String, Error> {
        self.inner.describe_layer(effect_id, format, payload)
    }
    fn compile(
        &self,
        effect_id: &str,
        format: u32,
        payload: &Value,
        stage: Stage,
    ) -> Result<Processing, Error> {
        self.inner.compile(effect_id, format, payload, stage)
    }
}

/// The built-in providers with Presence either disabled or not registered at all.
fn without_presence(disabled: bool) -> Arc<ModuleRegistry> {
    let mut registry = ModuleRegistry::new();
    let mut modules: Vec<Arc<dyn ToolModule>> = vec![
        Arc::new(PresetsModule::new()),
        Arc::new(PixelModule::new()),
        Arc::new(BasicModule::new()),
        Arc::new(MixerModule::new()),
        Arc::new(TransformModule::new()),
        Arc::new(CropModule::new()),
        Arc::new(VignetteModule::new()),
    ];
    if disabled {
        modules.push(Disabled::wrap(Arc::new(PresenceModule::new())));
    }
    for module in modules {
        registry.register(module).expect("a registered module");
    }
    Arc::new(registry)
}

#[test]
fn unavailable_is_computed_from_the_registry_the_record_is_read_with() {
    let path = catalog("unavailable");
    let mut service = EditorService::open(&path).expect("a catalog");
    let settings = set(json!({
        "set-basic": {"exposure": 0.5},
        "set-presence": {"clarity": 10},
    }));
    let preset = service
        .create_preset("Clear", None, &settings, "library-test")
        .expect("a preset");
    assert!(preset.unavailable.is_empty());
    drop(service);

    for disabled in [true, false] {
        let mut service =
            EditorService::open_with(&path, without_presence(disabled)).expect("the catalog");
        let listed = service.presets().expect("a listing");
        assert_eq!(
            listed[0].unavailable,
            ["set-presence"],
            "disabled: {disabled}"
        );
        let (read, _) = service.preset(&preset.id).expect("the preset");
        assert_eq!(read.unavailable, ["set-presence"]);
        assert_eq!(
            read.settings, settings,
            "the stored settings are kept whole"
        );
        // The preset still exports whole, so nothing it holds is lost.
        let exported = service.export_preset(&preset.id).expect("an export");
        assert!(exported.content.contains("set-presence"));
        // A new preset naming the unavailable action is refused, as an apply would be.
        let expected = if disabled {
            (
                ErrorKind::Incompatible,
                "unavailable module lightwell.presence",
            )
        } else {
            (ErrorKind::Validation, "unknown action set-presence")
        };
        assert_error(
            service.create_preset("Also clear", None, &settings, "library-test"),
            expected.0,
            expected.1,
        );
        drop(service);
    }
    let service = EditorService::open(&path).expect("the catalog");
    assert!(
        service
            .preset(&preset.id)
            .expect("the preset")
            .0
            .unavailable
            .is_empty(),
        "never stored: the full registry applies it again"
    );
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

// -------------------------------------------------------------------------------------------
// Import, inspect and export.
// -------------------------------------------------------------------------------------------

#[test]
fn an_import_stores_the_mapped_settings_its_report_and_the_text_verbatim() {
    let path = catalog("import");
    let mut service = EditorService::open(&path).expect("a catalog");
    let text = preset_file("develop.xmp");
    let parsed = parse_preset(&text, Some("develop.xmp"), service.registry()).expect("a preset");
    let imported = service
        .import_preset(&text, Some("develop.xmp"), None, None, "importer")
        .expect("an import");
    assert_eq!(imported.name, "Soft Film");
    assert_eq!(imported.group, "Synthetic Looks");
    assert_eq!(imported.settings, parsed.settings);
    assert_eq!(imported.origin, parsed.origin);
    assert!(matches!(
        imported.origin,
        PresetOrigin::LightroomXmp { ref file_name, .. } if file_name.as_deref() == Some("develop.xmp")
    ));
    assert_eq!(imported.report.as_ref(), Some(&parsed.report));
    assert_eq!(imported.actor, "importer");
    let (read, source_text) = service.preset(&imported.id).expect("the preset");
    assert_eq!(read, imported);
    assert_eq!(source_text.as_deref(), Some(text.as_str()), "kept verbatim");
    let listed = &service.presets().expect("a listing")[0];
    assert_eq!(listed.report, Some(parsed.report.counts()));

    // The same file again is a duplicate; the request's name and group resolve it.
    assert_error(
        service.import_preset(&text, None, None, None, "importer"),
        ErrorKind::Conflict,
        "group \"Synthetic Looks\" already holds a preset named \"Soft Film\"",
    );
    let renamed = service
        .import_preset(&text, None, Some(" Soft copy "), Some("Mine"), "importer")
        .expect("an import under another name");
    assert_eq!(
        (renamed.name.as_str(), renamed.group.as_str()),
        ("Soft copy", "Mine")
    );
    assert_error(
        service.import_preset(&text, None, Some("x\ty"), None, "importer"),
        ErrorKind::Validation,
        "preset name must contain",
    );

    // A template names no group, so the import takes the default one.
    let template = service
        .import_preset(
            &preset_file("faded.lrtemplate"),
            Some("faded.lrtemplate"),
            None,
            None,
            "importer",
        )
        .expect("a template");
    assert_eq!(template.group, IMPORTED_PRESET_GROUP);
    assert!(matches!(
        template.origin,
        PresetOrigin::LightroomTemplate { .. }
    ));

    // A file that is refused, or that maps nothing, stores nothing.
    let before = stored_rows(&service);
    assert_error(
        service.import_preset(&preset_file("profile.xmp"), None, None, None, "importer"),
        ErrorKind::UnsupportedInput,
        "Lightroom profiles are not presets",
    );
    assert_error(
        service.import_preset(&preset_file("legacy.xmp"), None, None, None, "importer"),
        ErrorKind::UnsupportedInput,
        "the preset has no setting Lightwell can apply",
    );
    assert_error(
        service.import_preset("not a preset", None, None, None, "importer"),
        ErrorKind::UnsupportedInput,
        "not a Lightwell, Lightroom XMP or .lrtemplate preset",
    );
    assert_error(
        service.import_preset(
            &" ".repeat(MAX_PRESET_BYTES + 1),
            None,
            None,
            None,
            "importer",
        ),
        ErrorKind::ResourceLimit,
        "a preset is at most",
    );
    assert_error(
        service.import_preset(&text, None, Some("Other"), None, ""),
        ErrorKind::Validation,
        "actor must contain 1..128 characters",
    );
    assert_eq!(stored_rows(&service), before);
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

#[test]
fn inspect_returns_what_an_import_would_store_and_stores_nothing() {
    let path = catalog("inspect");
    let service = EditorService::open(&path).expect("a catalog");
    let text = preset_file("develop.xmp");
    let parsed = parse_preset(&text, Some("develop.xmp"), service.registry()).expect("a preset");
    let inspected = service
        .inspect_import(&text, Some("develop.xmp"))
        .expect("a dry run");
    assert_eq!(
        inspected,
        json!({
            "preset": {
                "id": null,
                "name": "Soft Film",
                "group": "Synthetic Looks",
                "settings": parsed.settings,
                "origin": parsed.origin,
                "report": parsed.report,
                "actor": null,
                "created_ms": null,
                "updated_ms": null,
                "unavailable": [],
            },
            "report": parsed.report,
        })
    );
    // A file that maps nothing still returns its report, with empty settings.
    let legacy = service
        .inspect_import(&preset_file("legacy.xmp"), None)
        .expect("a report");
    assert_eq!(legacy["preset"]["settings"], json!({}));
    assert_eq!(legacy["preset"]["group"], json!(IMPORTED_PRESET_GROUP));
    assert!(
        !legacy["report"]["refused"]
            .as_array()
            .expect("refused")
            .is_empty()
    );
    assert_error(
        service.inspect_import(&preset_file("profile.xmp"), None),
        ErrorKind::UnsupportedInput,
        "Lightroom profiles are not presets",
    );
    assert_eq!(stored_rows(&service), 0);
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

#[test]
fn an_exported_preset_imports_back_to_the_same_name_group_and_settings() {
    let path = catalog("export");
    let mut service = EditorService::open(&path).expect("a catalog");
    let preset = create(&mut service, "Soft: film?", Some("Looks")).expect("a preset");
    let exported = service.export_preset(&preset.id).expect("an export");
    assert_eq!(exported.file_name, "Soft_ film_.lwpreset");
    assert_error(
        service.import_preset(
            &exported.content,
            Some(&exported.file_name),
            None,
            None,
            "a",
        ),
        ErrorKind::Conflict,
        "already holds a preset named \"Soft: film?\"",
    );
    service.delete_preset(&preset.id).expect("a delete");
    let imported = service
        .import_preset(
            &exported.content,
            Some(&exported.file_name),
            None,
            None,
            "a",
        )
        .expect("an import");
    assert_eq!(
        (imported.name.as_str(), imported.group.as_str()),
        ("Soft: film?", "Looks")
    );
    assert_eq!(imported.settings, preset.settings);
    assert_eq!(imported.origin, PresetOrigin::Lightwell {});
    assert_eq!(
        imported
            .report
            .as_ref()
            .map(|report| report.format.as_str()),
        Some("lightwell")
    );
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

// -------------------------------------------------------------------------------------------
// Capture.
// -------------------------------------------------------------------------------------------

fn opened(name: &str, registry: Option<Arc<ModuleRegistry>>) -> (EditorService, AssetId, PathBuf) {
    let path = catalog(name);
    let mut service = match registry {
        Some(registry) => EditorService::open_with(&path, registry),
        None => EditorService::open(&path),
    }
    .expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;
    (service, asset, path)
}

fn current(service: &EditorService, asset: &AssetId) -> EntryId {
    service.state(asset).expect("state").current_entry.id
}

fn capture(
    service: &EditorService,
    asset: &AssetId,
    entry: &EntryId,
    fields: Value,
) -> Result<Value, Error> {
    service
        .capture_preset(asset, entry, &set(fields))
        .map(Value::Object)
}

#[test]
fn capture_reads_the_declared_defaults_then_the_stored_values_of_the_named_entry() {
    let (mut service, asset, path) = opened("capture", None);
    let original = current(&service, &asset);
    let basic_defaults = json!({
        "temperature": 0.0, "tint": 0.0, "exposure": 0.0, "contrast": 0.0, "highlights": 0.0,
        "shadows": 0.0, "whites": 0.0, "blacks": 0.0, "vibrance": 0.0, "saturation": 0.0,
    });
    assert_eq!(
        capture(
            &service,
            &asset,
            &original,
            json!({"set-basic": true, "set-mixer": ["red-hue"]})
        )
        .expect("defaults"),
        json!({"set-basic": basic_defaults, "set-mixer": {"red-hue": 0.0}}),
        "with no layer, every field takes its declared default"
    );

    service
        .apply_action(
            &asset,
            mutation(0, "basic"),
            "set-basic",
            json!({"exposure": 0.5, "contrast": 20}),
        )
        .expect("a Basic edit");
    let after_basic = current(&service, &asset);
    service
        .apply_action(
            &asset,
            mutation(1, "mixer"),
            "set-mixer",
            json!({"red-hue": 10, "blue-saturation": -20}),
        )
        .expect("a mixer edit");
    let after_mixer = current(&service, &asset);

    assert_eq!(
        capture(
            &service,
            &asset,
            &after_mixer,
            json!({
                "set-basic": ["exposure", "contrast", "tint"],
                "set-mixer": ["red-hue", "blue-saturation", "green-luminance"],
            })
        )
        .expect("stored values"),
        json!({
            "set-basic": {"exposure": 0.5, "contrast": 20.0, "tint": 0.0},
            "set-mixer": {"red-hue": 10.0, "blue-saturation": -20.0, "green-luminance": 0.0},
        })
    );
    // `true` names every parameter of the action.
    let everything =
        capture(&service, &asset, &after_mixer, json!({"set-basic": true})).expect("every field");
    let mut expected = basic_defaults.clone();
    expected["exposure"] = json!(0.5);
    expected["contrast"] = json!(20.0);
    assert_eq!(everything, json!({"set-basic": expected}));
    // An earlier entry answers with its own stack: the mixer was not set yet.
    assert_eq!(
        capture(
            &service,
            &asset,
            &after_basic,
            json!({"set-basic": ["exposure"], "set-mixer": ["red-hue"]})
        )
        .expect("an earlier entry"),
        json!({"set-basic": {"exposure": 0.5}, "set-mixer": {"red-hue": 0.0}})
    );
    // The capture is a settings set the library stores and the presets module applies.
    let captured = set(everything);
    service
        .create_preset("Captured", None, &captured, "library-test")
        .expect("a stored capture");

    // Capture wrote nothing: the asset is where the two edits left it.
    let state = service.state(&asset).expect("state");
    assert_eq!((state.revision, state.current_entry.id), (2, after_mixer));
    let foreign = EntryId::new();
    assert_error(
        capture(&service, &asset, &foreign, json!({"set-basic": true})),
        ErrorKind::Validation,
        "history entry does not belong to this asset",
    );
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

#[test]
fn capture_refuses_unknown_non_patch_and_unavailable_actions_and_bad_field_lists() {
    let (service, asset, path) = opened("capture-refusals", Some(without_presence(true)));
    let entry = current(&service, &asset);
    for (fields, kind, detail) in [
        (
            json!({}),
            ErrorKind::Validation,
            "fields names 1 to 16 actions",
        ),
        (
            json!({"set-nothing": true}),
            ErrorKind::Validation,
            "unknown action set-nothing",
        ),
        (
            json!({"transform": true}),
            ErrorKind::Validation,
            "transform is not a field-patch action",
        ),
        (
            json!({"reset-basic": true}),
            ErrorKind::Validation,
            "reset-basic is not a field-patch action",
        ),
        (
            json!({"set-presence": ["clarity"]}),
            ErrorKind::Incompatible,
            "unavailable module lightwell.presence",
        ),
        (
            json!({"set-basic": ["exposure", "sharpness"]}),
            ErrorKind::Validation,
            "unknown parameter sharpness for action set-basic",
        ),
        (
            json!({"set-basic": ["exposure", "exposure"]}),
            ErrorKind::Validation,
            "the fields of set-basic name exposure twice",
        ),
        (
            json!({"set-basic": [1]}),
            ErrorKind::Validation,
            "the fields of set-basic must be parameter names",
        ),
        (
            json!({"set-basic": []}),
            ErrorKind::Validation,
            "must be true or an array of 1 to 64 parameter names",
        ),
        (
            json!({"set-basic": false}),
            ErrorKind::Validation,
            "must be true or an array of 1 to 64 parameter names",
        ),
        (
            json!({"set-basic": {"exposure": 1}}),
            ErrorKind::Validation,
            "must be true or an array of 1 to 64 parameter names",
        ),
    ] {
        assert_error(capture(&service, &asset, &entry, fields), kind, detail);
    }
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

/// A stored stack with two Basic layers is ambiguous for capture exactly as it is for planning and
/// rendering: nothing says which layer the preset should read. The stack is written through a
/// registry whose stand-in provider allows two layers of the Basic effect, then read with the
/// built-in one.
#[test]
fn capture_refuses_a_stack_with_two_layers_of_one_module_as_ambiguous() {
    let mut twin = ModuleRegistry::new();
    twin.register(StageModule::shared(
        "test.twin",
        BASIC_EFFECT,
        STAGE_ACTION,
        EffectStage::Color,
        0,
    ))
    .expect("a stand-in Basic provider");
    let (mut service, asset, path) = opened("capture-ambiguous", Some(Arc::new(twin)));
    for (revision, request) in [(0, "one"), (1, "two")] {
        service
            .apply_action(&asset, mutation(revision, request), STAGE_ACTION, json!({}))
            .expect("a Basic-effect layer");
    }
    drop(service);

    let service = EditorService::open(&path).expect("the built-in registry");
    let entry = current(&service, &asset);
    let layers = service
        .entry(&asset, &entry)
        .expect("the entry")
        .snapshot
        .recipe
        .layers;
    assert_eq!(
        layers
            .iter()
            .filter(|layer| layer.effect_id == BASIC_EFFECT)
            .count(),
        2
    );
    assert_error(
        capture(&service, &asset, &entry, json!({"set-basic": ["exposure"]})),
        ErrorKind::Validation,
        "ambiguous Basic layers",
    );
    // Another module's action on the same stack is unaffected.
    assert_eq!(
        capture(&service, &asset, &entry, json!({"set-mixer": ["red-hue"]})).expect("a capture"),
        json!({"set-mixer": {"red-hue": 0.0}})
    );
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}

/// A preset addresses the global layer, so capture reads the global layer and never a masked one:
/// with a global and a masked Basic layer it reads the global values instead of refusing the pair
/// as ambiguous, and with only a masked one it reads the defaults a missing global layer holds.
#[test]
fn capture_reads_the_global_layer_of_a_masked_photo() {
    for global in [true, false] {
        let (mut service, asset, path) = opened("capture-masked", None);
        if global {
            service
                .apply_action(
                    &asset,
                    mutation(0, "global"),
                    "set-basic",
                    json!({"exposure": 0.2, "contrast": 10}),
                )
                .expect("a global Basic layer");
        }
        let revision = service.state(&asset).expect("state").revision;
        let mask = service
            .apply_mask_command(
                &asset,
                mutation(revision, "mask"),
                crate::mask::commands::find("mask.create-linear").expect("a declared command"),
                json!({"x0": 0.5, "y0": 0.2, "x1": 0.5, "y1": 0.8}),
                crate::mask::commands::MaskTarget::default(),
            )
            .expect("a mask")
            .mask
            .expect("the created mask");
        service
            .apply_action(
                &asset,
                mutation(revision + 1, "masked"),
                "set-basic",
                json!({"exposure": 1.0, "contrast": 40, "mask": mask}),
            )
            .expect("a masked Basic layer");
        let entry = current(&service, &asset);
        let expected = if global {
            json!({"set-basic": {"exposure": 0.2, "contrast": 10.0}})
        } else {
            json!({"set-basic": {"exposure": 0.0, "contrast": 0.0}})
        };
        assert_eq!(
            capture(
                &service,
                &asset,
                &entry,
                json!({"set-basic": ["exposure", "contrast"]})
            )
            .unwrap_or_else(|error| panic!("global {global}: {error}")),
            expected,
            "global {global}"
        );
        drop(service);
        std::fs::remove_file(path).expect("the catalog is removed");
    }
}

/// A field with no stored value and no declared default has nothing to capture, so it is refused
/// rather than left out of the set.
#[test]
fn capture_refuses_a_field_with_no_value_and_no_default() {
    let parameter = |name: &str, default: Option<Value>| ParameterDescriptor {
        name: name.into(),
        kind: ParameterKind::Number {
            min: 0.0,
            max: 10.0,
        },
        required: false,
        default,
        unit: None,
        step: None,
        precision: None,
        notes: format!("the {name}"),
        soft_min: None,
        soft_max: None,
        fine_step: None,
        zero: None,
    };
    let sketch = TestModule::from_descriptor(ModuleDescriptor {
        id: "test.sketch".into(),
        title: "Sketch".into(),
        hint: None,
        effects: vec![EffectDescriptor {
            id: "test.sketch.effect".into(),
            format: EFFECT_FORMAT,
            stage: EffectStage::Pixel,
            order: 0,
            maskable: false,
            artifacts: false,
            single: false,
        }],
        actions: vec![ActionDescriptor {
            id: "set-sketch".into(),
            title: "Set sketch".into(),
            notes: "a field patch with one parameter that declares no default".into(),
            summary: None,
            patch: true,
            parameters: vec![
                parameter("weight", None),
                parameter("size", Some(json!(1.0))),
            ],
        }],
        queries: Vec::new(),
        controls: Vec::new(),
        reset: None,
        canvas: None,
        developer: false,
        collapsed: false,
        layout: crate::ModuleLayout::Stacked,
        availability: Availability::Available,
        ..ModuleDescriptor::default()
    });
    let mut registry = ModuleRegistry::builtin();
    registry.register(sketch).expect("the sketch module");
    let (service, asset, path) = opened("capture-no-default", Some(Arc::new(registry)));
    let entry = current(&service, &asset);
    assert_eq!(
        capture(&service, &asset, &entry, json!({"set-sketch": ["size"]})).expect("a default"),
        json!({"set-sketch": {"size": 1.0}})
    );
    for fields in [
        json!({"set-sketch": true}),
        json!({"set-sketch": ["weight"]}),
    ] {
        assert_error(
            capture(&service, &asset, &entry, fields),
            ErrorKind::Validation,
            "parameter weight of set-sketch has no default and the stack does not set it",
        );
    }
    drop(service);
    std::fs::remove_file(path).expect("the catalog is removed");
}
