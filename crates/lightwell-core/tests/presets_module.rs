//! The `lightwell.presets` module and the host's composite plans end to end, through the real
//! `EditorService`: `apply-preset` commits one entry whose stack equals the same fields sent through
//! the individual `edit.set-*` actions, leaves every field it does not name alone, is a no-op when
//! it changes nothing, undoes to the stack from before it, refuses every bad step with nothing
//! written, and drafts exactly what it commits.
//!
//! The descriptor, the generic `string` and `settings` checks, the `presets` control's registration
//! rules and the module's own parse, plan and label are proved in-crate next to their code.

use lightwell_core::{
    ActionDescriptor, ActionInput, ActionPlan, AssetId, Availability, Draft, EditorService, Error,
    ErrorKind, Layer, ModuleDescriptor, ModuleRegistry, Mutation, MutationOutcome,
    ParameterDescriptor, ParameterKind, Processing, Recipe, Stage, StageContext, ToolModule,
};
use serde_json::{Map, Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

// -------------------------------------------------------------------------------------------
// Shared helpers.
// -------------------------------------------------------------------------------------------

static NEXT: AtomicU64 = AtomicU64::new(1);

fn jpeg() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
}

fn catalog(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "lightwell-presets-module-{name}-{}-{}.sqlite",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_file(&path);
    path
}

fn mutation(revision: u64, request: &str) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request.into(),
        actor: "presets-module-test".into(),
    }
}

/// A service over a fresh catalog with the JPEG fixture imported.
fn opened(name: &str, registry: Option<ModuleRegistry>) -> (EditorService, AssetId, PathBuf) {
    let path = catalog(name);
    let mut service = match registry {
        Some(registry) => EditorService::open_with(&path, Arc::new(registry)),
        None => EditorService::open(&path),
    }
    .expect("a catalog");
    let asset = service.import(&jpeg()).expect("an import").asset.id;
    (service, asset, path)
}

fn revision(service: &EditorService, asset: &AssetId) -> u64 {
    service.state(asset).expect("state").revision
}

fn entries(service: &EditorService, asset: &AssetId) -> usize {
    service
        .history(asset, None, 100)
        .expect("history")
        .entries
        .len()
}

fn recipe(service: &EditorService, asset: &AssetId) -> Recipe {
    service
        .state(asset)
        .expect("state")
        .current_entry
        .snapshot
        .recipe
}

/// A stack as its effects, formats and payloads in order: what two stacks built by different
/// requests share, since every commit mints fresh layer identities.
fn contents(layers: &[Layer]) -> Vec<(String, u32, Value)> {
    layers
        .iter()
        .map(|layer| {
            (
                layer.effect_id.clone(),
                layer.effect_format,
                layer.payload.clone(),
            )
        })
        .collect()
}

fn apply_preset(
    service: &mut EditorService,
    asset: &AssetId,
    request: &str,
    parameters: Value,
) -> Result<lightwell_core::MutationResult, Error> {
    let current = revision(service, asset);
    service.apply_action(
        asset,
        mutation(current, request),
        "apply-preset",
        parameters,
    )
}

/// A settings set over all four presettable photo modules.
fn soft_film() -> Value {
    json!({
        "set-basic": {"exposure": 0.35, "contrast": 12, "temperature": 5, "tint": -3, "vibrance": 10},
        "set-presence": {"texture": 20, "clarity": -10, "dehaze": 5},
        "set-mixer": {"blue-saturation": -20, "red-hue": 10},
        "set-vignette": {"amount": -18, "midpoint": 40, "roundness": 0, "feather": 50},
    })
}

/// The refusal every rejected request must leave behind: nothing written, so the revision and the
/// entry count are where they were and the request identity is still free.
fn assert_refused(
    service: &mut EditorService,
    asset: &AssetId,
    case: &str,
    settings: Value,
    kind: ErrorKind,
    detail: &str,
) {
    let (before, count) = (revision(service, asset), entries(service, asset));
    let stack = recipe(service, asset);
    let error = apply_preset(
        service,
        asset,
        "refused",
        json!({"settings": settings, "name": "Refused"}),
    )
    .expect_err(case);
    assert_eq!(error.kind, kind, "{case}: {error}");
    assert_eq!(error.detail, detail, "{case}");
    assert_eq!(revision(service, asset), before, "{case}: revision");
    assert_eq!(entries(service, asset), count, "{case}: entries");
    assert_eq!(recipe(service, asset), stack, "{case}: stack");
}

// -------------------------------------------------------------------------------------------
// One entry whose stack equals the individual actions.
// -------------------------------------------------------------------------------------------

/// A preset over Basic, Presence, the mixer and the vignette commits exactly one entry, stored
/// under its own identity, label and parameters, and its stack and pixels equal the same fields
/// sent through the four `edit.set-*` actions one by one on a fresh asset.
#[test]
fn a_preset_commits_one_entry_whose_stack_equals_the_individual_actions() {
    let (mut service, asset, path) = opened("one-entry", None);
    let sent = json!({"settings": soft_film(), "name": "Soft film", "preset-id": "preset-1"});
    let (before, count) = (revision(&service, &asset), entries(&service, &asset));
    let result = apply_preset(&mut service, &asset, "apply", sent.clone()).expect("applied");
    assert_eq!(result.outcome, MutationOutcome::Applied);
    assert_eq!(result.revision, before + 1, "one revision");
    assert_eq!(entries(&service, &asset), count + 1, "one entry");
    let entry = service.state(&asset).expect("state").current_entry;
    assert_eq!(Some(&entry.id), result.created_entry_id.as_ref());
    assert_eq!(entry.action_id, "apply-preset");
    assert_eq!(entry.label, "Preset: Soft film");
    assert_eq!(
        entry.parameters, sent,
        "the entry stores the request as sent"
    );

    // A retried request returns the original result and writes nothing more.
    let retried = service
        .apply_action(
            &asset,
            mutation(before, "apply"),
            "apply-preset",
            sent.clone(),
        )
        .expect("a retry");
    assert!(retried.deduplicated);
    assert_eq!(retried.created_entry_id, result.created_entry_id);
    assert_eq!(entries(&service, &asset), count + 1);

    // The same fields through the individual actions, one entry each, on a fresh asset.
    let (mut stepwise, other, other_path) = opened("one-entry-stepwise", None);
    for (index, (action, fields)) in soft_film()
        .as_object()
        .expect("a settings set")
        .iter()
        .enumerate()
    {
        let current = revision(&stepwise, &other);
        stepwise
            .apply_action(
                &other,
                mutation(current, &format!("step-{index}")),
                action,
                fields.clone(),
            )
            .unwrap_or_else(|error| panic!("{action}: {error}"));
    }
    let preset = recipe(&service, &asset);
    let reference = recipe(&stepwise, &other);
    assert_eq!(
        contents(&preset.layers),
        contents(&reference.layers),
        "the same effects, formats, payloads and order"
    );
    assert_eq!(
        preset
            .layers
            .iter()
            .map(|layer| layer.effect_id.as_str())
            .collect::<Vec<_>>(),
        [
            lightwell_core::BASIC_EFFECT,
            lightwell_core::MIXER_EFFECT,
            lightwell_core::PRESENCE_EFFECT,
            lightwell_core::VIGNETTE_EFFECT,
        ],
        "each layer at the place its stage and order name"
    );
    let rendered = service.render_current(&asset).expect("the preset renders");
    let expected = stepwise
        .render_current(&other)
        .expect("the stepwise stack renders");
    assert_eq!(
        (rendered.width, rendered.height),
        (expected.width, expected.height)
    );
    assert!(
        rendered.rgba[..] == expected.rgba[..],
        "the preset's pixels equal the stepwise stack's"
    );

    drop((service, stepwise));
    fs::remove_file(path).expect("the catalog is removed");
    fs::remove_file(other_path).expect("the catalog is removed");
}

/// A preset changes only the fields it names: a Basic field set before it, and not named by it,
/// keeps its value in the same layer, updated in place.
#[test]
fn fields_a_preset_does_not_name_keep_their_values() {
    let (mut service, asset, path) = opened("untouched", None);
    service
        .apply_action(
            &asset,
            mutation(0, "contrast"),
            "set-basic",
            json!({"contrast": 30}),
        )
        .expect("a contrast");
    let before = recipe(&service, &asset);
    apply_preset(
        &mut service,
        &asset,
        "exposure",
        json!({"settings": {"set-basic": {"exposure": 0.5}}, "name": "Brighter"}),
    )
    .expect("applied");
    let after = recipe(&service, &asset);
    assert_eq!(after.layers.len(), 1);
    assert_eq!(after.layers[0].id, before.layers[0].id, "updated in place");
    assert_eq!(
        after.layers[0].payload,
        json!({"contrast": 30.0, "exposure": 0.5}),
        "contrast is untouched"
    );
    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

/// Applying a preset whose fields already hold is a no-op: no entry, no revision, and the request
/// is recorded so a retry is deduplicated.
#[test]
fn applying_the_same_preset_twice_is_a_no_op_the_second_time() {
    let (mut service, asset, path) = opened("twice", None);
    let sent = json!({"settings": soft_film(), "name": "Soft film"});
    apply_preset(&mut service, &asset, "first", sent.clone()).expect("applied");
    let (before, count) = (revision(&service, &asset), entries(&service, &asset));
    let stack = recipe(&service, &asset);
    let second = apply_preset(&mut service, &asset, "second", sent.clone()).expect("a no-op");
    assert_eq!(second.outcome, MutationOutcome::NoOp);
    assert_eq!(second.created_entry_id, None);
    assert_eq!(second.revision, before);
    assert_eq!(revision(&service, &asset), before);
    assert_eq!(entries(&service, &asset), count, "no new entry");
    assert_eq!(recipe(&service, &asset), stack, "the same snapshot");
    let retried = service
        .apply_action(&asset, mutation(before, "second"), "apply-preset", sent)
        .expect("a retried no-op");
    assert!(retried.deduplicated);
    assert_eq!(retried.outcome, MutationOutcome::NoOp);
    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

/// Undo returns to the stack from before the preset, and redo returns to the preset's stack.
#[test]
fn undo_returns_to_the_stack_from_before_the_preset() {
    let (mut service, asset, path) = opened("undo", None);
    service
        .apply_action(
            &asset,
            mutation(0, "exposure"),
            "set-basic",
            json!({"exposure": -1}),
        )
        .expect("an exposure");
    let before = recipe(&service, &asset);
    apply_preset(
        &mut service,
        &asset,
        "apply",
        json!({"settings": soft_film(), "name": "Soft film"}),
    )
    .expect("applied");
    let applied = recipe(&service, &asset);
    assert_ne!(applied, before);
    let current = revision(&service, &asset);
    service
        .undo(&asset, mutation(current, "undo"))
        .expect("an undo");
    assert_eq!(recipe(&service, &asset), before, "the pre-preset stack");
    let current = revision(&service, &asset);
    service
        .redo(&asset, mutation(current, "redo"))
        .expect("a redo");
    assert_eq!(
        recipe(&service, &asset),
        applied,
        "the preset's stack again"
    );
    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

// -------------------------------------------------------------------------------------------
// Refusals: nothing written.
// -------------------------------------------------------------------------------------------

/// Every refused step refuses the whole preset before anything is written, including steps that
/// follow a valid one, so no partial stack is ever committed.
#[test]
fn a_bad_step_refuses_the_whole_preset_and_writes_nothing() {
    let (mut service, asset, path) = opened("refusals", None);
    for (case, settings, kind, detail) in [
        (
            "an unknown action",
            json!({"set-nothing": {"amount": 1}}),
            ErrorKind::Validation,
            "unknown action set-nothing",
        ),
        (
            "a transform, which is not a field patch",
            json!({"transform": {"transform": "rotate-left"}}),
            ErrorKind::Validation,
            "transform is not a field-patch action",
        ),
        (
            "a crop, which is not a field patch",
            json!({"crop": {"angle": 0}}),
            ErrorKind::Validation,
            "crop is not a field-patch action",
        ),
        (
            "a reset, which is not a field patch",
            json!({"reset-basic": {"exposure": 0}}),
            ErrorKind::Validation,
            "reset-basic is not a field-patch action",
        ),
        (
            "a field out of range",
            json!({"set-basic": {"exposure": 9}}),
            ErrorKind::Validation,
            "parameter exposure must be a number within -5..=5",
        ),
        (
            "a field the action does not declare",
            json!({"set-basic": {"brightness": 1}}),
            ErrorKind::Validation,
            "unknown parameter brightness for action set-basic",
        ),
        (
            "a bad step after a valid one",
            json!({"set-basic": {"exposure": 1}, "set-vignette": {"amount": 500}}),
            ErrorKind::Validation,
            "parameter amount must be a number within -100..=100",
        ),
    ] {
        assert_refused(&mut service, &asset, case, settings, kind, detail);
    }
    // Nothing was recorded under the refused request identity, so it is still free to use.
    let result = apply_preset(
        &mut service,
        &asset,
        "refused",
        json!({"settings": {"set-basic": {"exposure": 1}}, "name": "Refused"}),
    )
    .expect("the request identity was never stored");
    assert_eq!(result.outcome, MutationOutcome::Applied);
    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

/// A name that is empty or only whitespace would label an entry with nothing, so it is refused and
/// nothing is written.
#[test]
fn an_empty_or_blank_name_is_refused() {
    let (mut service, asset, path) = opened("names", None);
    for name in ["", "  ", "\t"] {
        let error = apply_preset(
            &mut service,
            &asset,
            "blank",
            json!({"settings": {"set-basic": {"exposure": 1}}, "name": name}),
        )
        .expect_err("a blank name");
        assert_eq!(error.kind, ErrorKind::Validation, "{name:?}");
        let expected = if name == "\t" {
            "parameter name must not contain control characters"
        } else {
            "preset name must not be empty"
        };
        assert_eq!(error.detail, expected, "{name:?}");
    }
    assert_eq!(revision(&service, &asset), 0);
    assert_eq!(entries(&service, &asset), 1, "only the import entry");
    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

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

/// A step whose module is registered but unavailable is refused by name, and nothing is written,
/// even though the steps of available modules in the same preset would have planned.
#[test]
fn a_step_of_an_unavailable_module_is_refused() {
    let mut registry = ModuleRegistry::new();
    for module in [
        Arc::new(lightwell_core::PresetsModule::new()) as Arc<dyn ToolModule>,
        Arc::new(lightwell_core::PixelModule::new()),
        Arc::new(lightwell_core::BasicModule::new()),
        Disabled::wrap(Arc::new(lightwell_core::PresenceModule::new())),
        Arc::new(lightwell_core::TransformModule::new()),
        Arc::new(lightwell_core::CropModule::new()),
    ] {
        registry.register(module).expect("a registered module");
    }
    let (mut service, asset, path) = opened("unavailable", Some(registry));
    assert_refused(
        &mut service,
        &asset,
        "an unavailable module",
        json!({"set-basic": {"exposure": 1}, "set-presence": {"clarity": 10}}),
        ErrorKind::Incompatible,
        "unavailable module lightwell.presence",
    );
    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

/// A test module with no effects whose actions plan composites of their own: `compose-steps`
/// composes `count` Basic exposure steps, and `set-nested` is a field patch whose plan is itself a
/// composite, which the host refuses as a step.
struct Composer(ModuleDescriptor);

impl Composer {
    fn shared() -> Arc<dyn ToolModule> {
        let parameter = |name: &str, kind: ParameterKind| ParameterDescriptor {
            name: name.into(),
            kind,
            required: true,
            default: None,
            unit: None,
            step: None,
            precision: None,
            notes: "test".into(),
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
        };
        Arc::new(Self(ModuleDescriptor {
            id: "test.composer".into(),
            title: "Composer".into(),
            hint: None,
            effects: Vec::new(),
            actions: vec![
                ActionDescriptor {
                    id: "compose-steps".into(),
                    title: "Compose steps".into(),
                    notes: "composes count Basic exposure steps".into(),
                    summary: None,
                    patch: false,
                    parameters: vec![parameter(
                        "count",
                        ParameterKind::Integer { min: 0, max: 64 },
                    )],
                },
                ActionDescriptor {
                    id: "set-nested".into(),
                    title: "Set nested".into(),
                    notes: "a field patch whose plan is a composite".into(),
                    summary: None,
                    patch: true,
                    parameters: vec![parameter(
                        "exposure",
                        ParameterKind::Number {
                            min: -5.0,
                            max: 5.0,
                        },
                    )],
                },
            ],
            queries: Vec::new(),
            controls: Vec::new(),
            reset: None,
            canvas: None,
            developer: false,
            collapsed: false,
            availability: Availability::Available,
        }))
    }

    fn exposure(value: Value) -> ActionInput {
        ActionInput {
            action_id: "set-basic".into(),
            parameters: json!({"exposure": value})
                .as_object()
                .expect("an object")
                .clone(),
        }
    }
}

impl ToolModule for Composer {
    fn descriptor(&self) -> &ModuleDescriptor {
        &self.0
    }
    fn parse(
        &self,
        action_id: &str,
        parameters: &Map<String, Value>,
    ) -> Result<ActionInput, Error> {
        Ok(ActionInput {
            action_id: action_id.into(),
            parameters: parameters.clone(),
        })
    }
    fn plan(&self, input: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
        Ok(ActionPlan::Compose(match input.action_id.as_str() {
            "compose-steps" => {
                let count = input.parameters["count"].as_u64().expect("a count");
                (1..=count)
                    .map(|step| Self::exposure(json!(step as f64 / 100.0)))
                    .collect()
            }
            _ => vec![Self::exposure(input.parameters["exposure"].clone())],
        }))
    }
    fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
        Err(Error::new(ErrorKind::Validation, "no effects"))
    }
    fn describe_layer(&self, _: &str, _: u32, _: &Value) -> Result<String, Error> {
        Err(Error::new(ErrorKind::Validation, "no effects"))
    }
    fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
        Err(Error::new(ErrorKind::Validation, "no effects"))
    }
}

/// A composite step whose own plan is a composite is refused, and so is a composite of more than
/// sixteen steps; sixteen steps commit as one entry. Nothing is written by a refusal.
#[test]
fn composites_do_not_nest_and_hold_at_most_sixteen_steps() {
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(Composer::shared())
        .expect("a module without effects");
    let (mut service, asset, path) = opened("composer", Some(registry));
    assert_refused(
        &mut service,
        &asset,
        "a nested composite",
        json!({"set-nested": {"exposure": 1}}),
        ErrorKind::Validation,
        "composite actions do not nest",
    );

    let (before, count) = (revision(&service, &asset), entries(&service, &asset));
    let error = service
        .apply_action(
            &asset,
            mutation(before, "seventeen"),
            "compose-steps",
            json!({"count": 17}),
        )
        .expect_err("seventeen steps");
    assert_eq!(error.kind, ErrorKind::Validation);
    assert_eq!(
        error.detail,
        "a composite action holds 17 steps, more than 16"
    );
    assert_eq!(revision(&service, &asset), before);
    assert_eq!(entries(&service, &asset), count);

    let result = service
        .apply_action(
            &asset,
            mutation(before, "sixteen"),
            "compose-steps",
            json!({"count": 16}),
        )
        .expect("sixteen steps");
    assert_eq!(result.outcome, MutationOutcome::Applied);
    assert_eq!(entries(&service, &asset), count + 1, "one entry");
    let stack = recipe(&service, &asset);
    assert_eq!(stack.layers.len(), 1, "every step updated the one layer");
    assert_eq!(
        stack.layers[0].payload,
        json!({"exposure": 0.16}),
        "the last step's value"
    );
    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}

// -------------------------------------------------------------------------------------------
// Drafts.
// -------------------------------------------------------------------------------------------

/// A draft of `apply-preset` resolves through the same helper a commit does, so its effective
/// recipe is what committing it produces: the layer it updates keeps its identity in both, and the
/// layers it adds match in everything but their fresh identities.
#[test]
fn a_drafted_preset_resolves_to_exactly_what_it_commits() {
    let (mut service, asset, path) = opened("draft", None);
    service
        .apply_action(
            &asset,
            mutation(0, "exposure"),
            "set-basic",
            json!({"exposure": -1}),
        )
        .expect("an exposure");
    let fields = json!({"settings": soft_film(), "name": "Soft film"});
    let mut draft = Draft::new("apply-preset", asset.clone(), revision(&service, &asset));
    draft.merge(fields.as_object().expect("an object").clone());
    let (drafted, _) = service
        .draft_recipe(&asset, &draft)
        .expect("a draft recipe");
    let unchanged = recipe(&service, &asset);
    assert_ne!(drafted, unchanged, "the draft changes the stack");
    assert_eq!(unchanged.layers.len(), 1, "a draft writes nothing");

    apply_preset(&mut service, &asset, "commit", fields).expect("applied");
    let committed = recipe(&service, &asset);
    assert_eq!(contents(&drafted.layers), contents(&committed.layers));
    assert_eq!(
        drafted.layers[0].id, committed.layers[0].id,
        "the Basic layer is updated in place in both"
    );
    assert_eq!(
        drafted.layers[0], committed.layers[0],
        "the same updated Basic layer"
    );

    // A draft that changes nothing resolves to the current recipe.
    let mut again = Draft::new("apply-preset", asset.clone(), revision(&service, &asset));
    again.merge(
        json!({"settings": soft_film(), "name": "Soft film"})
            .as_object()
            .expect("an object")
            .clone(),
    );
    let (unchanged, _) = service.draft_recipe(&asset, &again).expect("a no-op draft");
    assert_eq!(unchanged, committed);
    drop(service);
    fs::remove_file(path).expect("the catalog is removed");
}
