//! The field-patch rules one module's provider answers in process, with no catalog: which stored
//! payloads it reads, how a patch and a reset plan against the stack they find, the history label
//! a request earns, and how a layer describes itself and reports its values. They are the shared
//! implementation's (`modules/field_patch.rs`), so they are proved here for every module in the
//! field-patch shape rather than in each module's own tests; a module keeps only its declaration,
//! its equations and its own neutrality rule.
use super::{
    Checked, ensure,
    shape::{Field, FieldPatch, Label},
};
use luxforge_core::{
    ActionInput, ActionPlan, Error, ErrorKind, Layer, ModuleRegistry, Orientation, Provider, Stage,
    StageContext, StageQuestions, check_parameters,
};
use serde_json::{Map, Value, json};

/// Stage answers for planning: every prefix receives `stage` and every point reads black. A
/// field-patch plan reads neither, and a module that started to would still be answered.
struct Fixed(Stage);

impl StageQuestions for Fixed {
    fn stage_before(&self, _: usize) -> Result<Stage, Error> {
        Ok(self.0)
    }

    fn sample_before(&self, _: usize, _: u32, _: u32) -> Result<Option<[u8; 4]>, Error> {
        Ok(Some([0, 0, 0, 255]))
    }
}

/// One module's provider and declarations, as the checks below ask them.
struct Rules<'a> {
    registry: &'a ModuleRegistry,
    module: &'a FieldPatch,
    provider: Provider<'a>,
    stage: Stage,
}

impl Rules<'_> {
    /// Plan one request the way the host does: the generic parameter check, the module's parse,
    /// then its plan against `layers` for the global target.
    fn plan(&self, action: &str, parameters: Value, layers: &[Layer]) -> Result<ActionPlan, Error> {
        let declared = self
            .module
            .descriptor
            .action(action)
            .ok_or_else(|| Error::validation(format!("{action} is undeclared")))?;
        let checked = check_parameters(declared, &parameters)?;
        let input = self.provider.parse(action, &checked)?;
        let questions = Fixed(self.stage);
        self.provider.plan(
            &input,
            &StageContext {
                layers,
                registry: self.registry,
                target: None,
                kind: luxforge_core::SourceTag::Jpeg,
                masks: &[],
                questions: &questions,
            },
        )
    }

    /// A plan that must succeed.
    fn planned(&self, action: &str, parameters: Value, layers: &[Layer]) -> Checked<ActionPlan> {
        self.plan(action, parameters.clone(), layers)
            .map_err(|error| format!("{action} {parameters} did not plan: {error}"))
    }

    fn no_op(&self, action: &str, parameters: Value, layers: &[Layer], what: &str) -> Checked {
        let plan = self.planned(action, parameters, layers)?;
        ensure(
            plan == ActionPlan::NoOp,
            format!("{what} planned {plan:?}, not a no-op"),
        )
    }

    /// A plan that must update `layer` in place, answering the payload it stores.
    fn updated(
        &self,
        action: &str,
        parameters: Value,
        layers: &[Layer],
        layer: &Layer,
        what: &str,
    ) -> Checked<Value> {
        match self.planned(action, parameters, layers)? {
            ActionPlan::Update(update) => {
                ensure(
                    update.id == layer.id,
                    format!("{what} updated another layer than its own"),
                )?;
                Ok(update.payload)
            }
            other => Err(format!("{what} planned {other:?}, not an update in place")),
        }
    }

    fn layer(&self, payload: Value) -> Layer {
        Layer {
            effect_format: self.module.effect.format,
            ..Layer::new(self.module.effect.id.clone(), payload)
        }
    }

    /// The label the host stores for this request: the module's, with the requested action's
    /// title as its default.
    fn label(&self, action: &str, parameters: &Map<String, Value>) -> Checked<String> {
        let declared = self
            .provider
            .descriptor()
            .action(action)
            .ok_or_else(|| format!("{action} is not declared"))?;
        Ok(self.provider.label(
            declared,
            &ActionInput {
                action_id: action.to_owned(),
                parameters: parameters.clone(),
            },
        ))
    }

    fn validate(&self, format: u32, payload: &Value) -> Result<(), Error> {
        self.provider
            .validate_payload(&self.module.effect.id, format, payload)
    }
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

/// Every in-process rule for one module.
pub fn rules(registry: &ModuleRegistry, module: &FieldPatch, stage: Stage) -> Checked<Value> {
    let provider = registry
        .module(&module.id)
        .ok_or("the module is not registered")?;
    let rules = Rules {
        registry,
        module,
        provider,
        stage,
    };
    let payloads = stored_payloads(&rules)?;
    let plans = plans(&rules)?;
    let labels = labels(&rules)?;
    let described = described(&rules)?;
    Ok(json!({"payloads": payloads, "plans": plans, "labels": labels, "described": described}))
}

/// Every field reads at the edges of its declaration — both ends of a range and a whole maximum
/// as a JSON integer, both booleans, every option, black and white, a moved curve — while a value
/// its declaration refuses (outside the range, of the wrong type, an undeclared option, a channel
/// past 255, too many curve points), a payload that is not an object, another effect and an
/// undeclared format are refused by name.
fn stored_payloads(rules: &Rules<'_>) -> Checked<Value> {
    let effect = &rules.module.effect;
    let mut accepted = Vec::new();
    for field in &rules.module.fields {
        for value in field.accepted() {
            let payload = json!({field.name.clone(): value});
            rules
                .validate(effect.format, &payload)
                .map_err(|error| format!("{payload} was refused: {error}"))?;
            // A field patch addresses no coordinates of its input stage, so a turn leaves it as it
            // is: the carry hook's default, which no geometry module's own answer is checked here.
            let turned = Orientation {
                turns: 1,
                mirror: true,
            };
            let carried = rules
                .provider
                .carry(&effect.id, effect.format, &payload, rules.stage, turned)
                .map_err(|error| format!("{payload} was refused a carry: {error}"))?;
            ensure(
                carried.is_none(),
                format!("{payload} was rewritten by a turn: {carried:?}"),
            )?;
            accepted.push(payload);
        }
        for (value, reason) in field.refused() {
            let payload = json!({field.name.clone(): value});
            let error = rules
                .validate(effect.format, &payload)
                .err()
                .ok_or_else(|| format!("{payload}, which its declaration refuses, was accepted"))?;
            ensure(
                error.kind == ErrorKind::Validation
                    && error.detail.contains(&field.name)
                    && error.detail.contains(&reason),
                format!("{payload} was refused with {error}, not by its field and {reason:?}"),
            )?;
        }
    }
    for payload in [json!([1.0]), json!(1.0)] {
        let error = rules
            .validate(effect.format, &payload)
            .err()
            .ok_or_else(|| format!("{payload} was accepted"))?;
        ensure(
            error.kind == ErrorKind::Validation && error.detail.contains("must be a JSON object"),
            format!("{payload} was refused with {error}"),
        )?;
    }
    let other = rules
        .provider
        .validate_payload("conformance.other", effect.format, &json!({}))
        .err()
        .ok_or("a payload of another effect was accepted")?;
    ensure(
        other.kind == ErrorKind::Incompatible,
        format!("a payload of another effect was refused with {other}"),
    )?;
    let future = effect.format + 1;
    for (what, refused) in [
        ("validated", rules.validate(future, &json!({})).err()),
        (
            "described",
            rules
                .provider
                .describe(&effect.id, future, &json!({}))
                .err(),
        ),
    ] {
        let error = refused.ok_or_else(|| format!("an undeclared format was {what}"))?;
        ensure(
            error.kind == ErrorKind::Incompatible
                && error
                    .detail
                    .contains(&format!("unsupported effect format {future}")),
            format!("an undeclared format {what} was refused with {error}"),
        )?;
    }
    Ok(json!({"accepted": accepted}))
}

/// The field a first set of which commits a layer: the first one that is not neutral on its own by
/// the module's rule. The vignette's shape fields do nothing at amount 0.
fn lead(rules: &Rules<'_>) -> Checked<usize> {
    rules
        .module
        .fields
        .iter()
        .position(|field| {
            !rules
                .registry
                .layer_report(&rules.layer(json!({field.name.clone(): field.high()})))
                .is_ok_and(|report| report.neutral)
        })
        .ok_or_else(|| "no single field changes the image".to_owned())
}

/// A patch commits only a layer that is not neutral, stores the canonical payload (only the fields
/// away from their defaults), merges over the stored layer and updates it in place; the same values
/// in any spelling and an empty patch are no-ops; a reset keeps the layer's identity and stores
/// `{}`; a stack holding two layers for the target is refused by name.
fn plans(rules: &Rules<'_>) -> Checked<Value> {
    let module = rules.module;
    let (set, reset) = (module.set.as_str(), module.reset.as_str());
    let lead_index = lead(rules)?;
    let lead = &module.fields[lead_index];
    let other = module
        .fields
        .iter()
        .find(|field| field.name != lead.name)
        .ok_or("the module declares one field, so a merge cannot be shown")?;

    let declared = module
        .descriptor
        .action(set)
        .ok_or("the patch is undeclared")?;
    let checked = check_parameters(declared, &json!({}))
        .map_err(|error| format!("an empty patch failed the generic check: {error}"))?;
    ensure(
        checked.is_empty(),
        format!("the generic check filled {checked:?} into an empty patch"),
    )?;
    let checked = check_parameters(declared, &json!({lead.name.clone(): lead.high()}))
        .map_err(|error| format!("a one-field patch failed the generic check: {error}"))?;
    ensure(
        checked.len() == 1,
        format!("the generic check filled defaults into a one-field patch: {checked:?}"),
    )?;

    rules.no_op(set, json!({}), &[], "an empty first set")?;
    rules.no_op(
        set,
        json!({lead.name.clone(): lead.default.clone()}),
        &[],
        "a first set at the default",
    )?;
    rules.no_op(reset, json!({}), &[], "a reset without a layer")?;

    let high = json!({lead.name.clone(): lead.high()});
    match rules.planned(set, high.clone(), &[])? {
        ActionPlan::Commit(new) => {
            ensure(
                new.effect_id == module.effect.id && new.payload == high,
                format!("a first set of {high} committed {new:?}"),
            )?;
        }
        plan => {
            return Err(format!(
                "a first set of {high} planned {plan:?}, not a commit"
            ));
        }
    }

    let existing = rules.layer(high.clone());
    let stack = std::slice::from_ref(&existing);
    let merged = rules.updated(
        set,
        json!({other.name.clone(): other.high()}),
        stack,
        &existing,
        "a set of another field",
    )?;
    let expected = json!({lead.name.clone(): lead.high(), other.name.clone(): other.high()});
    ensure(
        merged == expected,
        format!("a set of another field over {high} stored {merged}, not {expected}"),
    )?;
    rules.no_op(set, high.clone(), stack, "the stored value set again")?;
    if let Some(respelled) = lead.respelled(&lead.high()) {
        rules.no_op(
            set,
            json!({lead.name.clone(): respelled}),
            stack,
            "the stored value in another spelling",
        )?;
    }
    rules.no_op(set, json!({}), stack, "an empty patch over a stored layer")?;

    let both = rules.layer(expected);
    let cleared = rules.updated(
        set,
        json!({lead.name.clone(): lead.default.clone()}),
        std::slice::from_ref(&both),
        &both,
        "a field set back to its default",
    )?;
    let kept = json!({other.name.clone(): other.high()});
    ensure(
        cleared == kept,
        format!("a field set back to its default stored {cleared}, not {kept}"),
    )?;

    for stored in [json!({}), json!({lead.name.clone(): lead.default.clone()})] {
        let layer = rules.layer(stored.clone());
        let stack = std::slice::from_ref(&layer);
        rules.no_op(
            set,
            json!({lead.name.clone(): lead.default.clone()}),
            stack,
            &format!("the default set on {stored}"),
        )?;
        rules.no_op(reset, json!({}), stack, &format!("a reset of {stored}"))?;
    }
    let reset_payload = rules.updated(reset, json!({}), stack, &existing, "a reset")?;
    ensure(
        reset_payload == json!({}),
        format!("a reset stored {reset_payload}, not {{}}"),
    )?;

    let expected = format!("ambiguous {} layers", module.title);
    for action in [set, reset] {
        let error = rules
            .plan(
                action,
                json!({}),
                &[existing.clone(), rules.layer(high.clone())],
            )
            .err()
            .ok_or_else(|| format!("{action} planned against two layers for one target"))?;
        ensure(
            error.kind == ErrorKind::Validation && error.detail == expected,
            format!("{action} against two layers was refused with {error}, not {expected}"),
        )?;
    }
    let fields = every_field(rules)?;
    Ok(json!({
        "lead": lead.name,
        "other": other.name,
        "merged": merged,
        "cleared": cleared,
        "fields": fields,
    }))
}

/// Each field on its own, whatever its kind: a first set of a moved value commits exactly that
/// field unless the module's rule calls it neutral, setting it again in any spelling is a no-op,
/// a second moved value updates the layer in place, and setting the default in any spelling stores
/// `{}` again.
fn every_field(rules: &Rules<'_>) -> Checked<Value> {
    let (module, set) = (rules.module, rules.module.set.as_str());
    let mut shown = Vec::new();
    for field in &module.fields {
        let named = |value: Value| json!({field.name.clone(): value});
        let high = named(field.high());
        let neutral = rules
            .registry
            .layer_report(&rules.layer(high.clone()))
            .is_ok_and(|report| report.neutral);
        match rules.planned(set, high.clone(), &[])? {
            ActionPlan::NoOp if neutral => {}
            ActionPlan::Commit(new) if !neutral && new.payload == high => {}
            plan => {
                return Err(format!(
                    "a first set of {high}, {} by the module's rule, planned {plan:?}",
                    if neutral { "neutral" } else { "not neutral" }
                ));
            }
        }
        let stored = rules.layer(high.clone());
        let stack = std::slice::from_ref(&stored);
        rules.no_op(set, high.clone(), stack, &format!("{high} set again"))?;
        if let Some(respelled) = field.respelled(&field.high()) {
            rules.no_op(
                set,
                named(respelled),
                stack,
                &format!("{high} set again in another spelling"),
            )?;
        }
        if !field.same(Some(&field.low()), &field.high()) {
            let low = named(field.low());
            let moved = rules.updated(set, low.clone(), stack, &stored, &format!("{low}"))?;
            ensure(moved == low, format!("{low} over {high} stored {moved}"))?;
        }
        let defaults =
            std::iter::once(field.default.clone()).chain(field.respelled(&field.default));
        for default in defaults {
            let default = named(default);
            let cleared =
                rules.updated(set, default.clone(), stack, &stored, &format!("{default}"))?;
            ensure(
                cleared == json!({}),
                format!("{default} over {high} stored {cleared}, not {{}}"),
            )?;
        }
        shown.push(json!({"field": field.name, "high": field.high(), "neutral": neutral}));
    }
    Ok(json!(shown))
}

/// Labels by the declared rules: one field by its value (at its default too), each group's reset
/// preset as `Reset <group>`, the module reset as `Reset <title>`, a larger patch by its field
/// count, and no label for an empty patch.
fn labels(rules: &Rules<'_>) -> Checked<Value> {
    let module = rules.module;
    let mut patches: Vec<Map<String, Value>> = Vec::new();
    for field in &module.fields {
        for value in [field.high(), field.low(), field.default.clone()] {
            patches.push(object(json!({field.name.clone(): value})));
        }
    }
    patches.extend(module.groups.iter().map(|group| group.preset.clone()));
    let lead = &module.fields[lead(rules)?];
    if let Some(other) = module.fields.iter().find(|field| field.name != lead.name) {
        patches.push(object(
            json!({lead.name.clone(): lead.high(), other.name.clone(): other.high()}),
        ));
    }
    let mut shown = Vec::new();
    for patch in patches {
        let expected = module.expected_label(&patch)?;
        let label = rules.label(&module.set, &patch)?;
        ensure(
            expected.matches(&label),
            format!("{patch:?} is labelled {label:?}, not {expected:?}"),
        )?;
        shown.push(label);
    }
    let reset = rules.label(&module.reset, &Map::new())?;
    ensure(
        reset == format!("Reset {}", module.title),
        format!("the module reset is labelled {reset:?}"),
    )?;
    let empty = rules.label(&module.set, &Map::new())?;
    let title = &rules
        .provider
        .descriptor()
        .action(&module.set)
        .ok_or("the set action is not declared")?
        .title;
    ensure(
        empty == *title,
        format!("an empty patch is labelled {empty:?}, not the action's title"),
    )?;
    Ok(json!(shown))
}

/// A layer reports every field's value with the defaults filled, and describes itself by each
/// field away from its default, in declared order, as its history label names it.
fn described(rules: &Rules<'_>) -> Checked<Value> {
    let module = rules.module;
    let effect = &module.effect;
    let lead = &module.fields[lead(rules)?];
    let Some(other) = module.fields.iter().find(|field| field.name != lead.name) else {
        return Ok(Value::Null);
    };
    let payload = json!({lead.name.clone(): lead.high(), other.name.clone(): other.high()});
    let report = rules
        .provider
        .describe(&effect.id, effect.format, &payload)
        .map_err(|error| format!("{payload} was not described: {error}"))?;
    let values = report.values;
    ensure(
        values.len() == module.fields.len(),
        format!("{payload} reported {} values", values.len()),
    )?;
    for field in &module.fields {
        let expected = if field.name == lead.name {
            lead.high()
        } else if field.name == other.name {
            other.high()
        } else {
            field.default.clone()
        };
        ensure(
            field.same(values.get(&field.name), &expected),
            format!(
                "{payload} reports {} as {:?}",
                field.name,
                values.get(&field.name)
            ),
        )?;
    }
    let described = report.summary;
    let mut moved = [(lead, lead.high()), (other, other.high())];
    moved.sort_by_key(|(field, _)| {
        module
            .fields
            .iter()
            .position(|declared| declared.name == field.name)
    });
    let parts: Vec<&str> = described.split(", ").collect();
    ensure(
        parts.len() == 2
            && moved.iter().zip(&parts).all(|((field, value), part)| {
                Label::Ending(format!(" {}", field.shown(value))).matches(part)
            }),
        format!("{payload} is described as {described:?}"),
    )?;
    Ok(json!({"payload": payload, "described": described}))
}

/// The rules every `patch: true` action of every registered module keeps, field patch or not
/// (`set-raw` merges itself under its own rule): the generic check of an empty patch fills in
/// nothing, each declared default alone is exactly that one field, and a value a field's
/// declaration refuses is refused naming the field.
pub fn patch_actions(registry: &ModuleRegistry) -> Checked<Value> {
    let mut checked = Vec::new();
    for descriptor in registry.descriptors() {
        for action in descriptor.actions.iter().filter(|action| action.patch) {
            let what = format!("{} {}", descriptor.id, action.id);
            let empty = check_parameters(action, &json!({})).map_err(|error| {
                format!("{what}: an empty patch failed the generic check: {error}")
            })?;
            ensure(
                empty.is_empty(),
                format!("{what}: the generic check filled {empty:?} into an empty patch"),
            )?;
            for parameter in &action.parameters {
                let Some(default) = &parameter.default else {
                    continue;
                };
                let sent = json!({parameter.name.clone(): default});
                let one = check_parameters(action, &sent)
                    .map_err(|error| format!("{what}: {sent} was refused: {error}"))?;
                ensure(
                    Value::Object(one.clone()) == sent,
                    format!("{what}: the generic check turned {sent} into {one:?}"),
                )?;
                let Some(field) = Field::of(parameter, default.clone()) else {
                    continue;
                };
                for (value, reason) in field.refused() {
                    let sent = json!({parameter.name.clone(): value});
                    let error = check_parameters(action, &sent)
                        .err()
                        .ok_or_else(|| format!("{what}: {sent} was accepted"))?;
                    ensure(
                        error.kind == ErrorKind::Validation
                            && error.detail.contains(&parameter.name)
                            && error.detail.contains(&reason),
                        format!("{what}: {sent} was refused with {error}, not {reason:?}"),
                    )?;
                }
            }
            checked.push(json!(what));
        }
    }
    Ok(json!(checked))
}
